//! Live translation pipeline: OCR text in, `translate://result` /
//! `translate://error` payloads out.
//!
//! - Live OCR results are debounced (the text must stay the same for
//!   [`DEBOUNCE`]) and translated only when the text changed.
//! - One-shot requests skip the debounce.
//! - Starting a new request aborts the one in flight, so a stale answer never
//!   replaces a newer one.
//! - Text that reads back our own last translation (the overlay seen by the
//!   capture) is ignored.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::cache::TranslationCache;
use super::chain::FallbackChain;
use super::{EngineId, Lang};

pub const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslatedEvent {
    pub original: String,
    pub translated: String,
    /// Detected or requested source language, if known.
    pub from: Option<Lang>,
    pub to: Lang,
    pub engine: EngineId,
    pub ms: u64,
    pub cached: bool,
    /// Engines that failed before `engine` answered (for the log / tooltip).
    pub failures: Vec<String>,
    pub rtl: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateErrorEvent {
    pub original: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PipelineEvent {
    Result(TranslatedEvent),
    Error(TranslateErrorEvent),
}

impl PipelineEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            PipelineEvent::Result(_) => "translate://result",
            PipelineEvent::Error(_) => "translate://error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub text: String,
    pub from: Option<Lang>,
    pub to: Lang,
}

pub type ChainProvider = Arc<dyn Fn() -> Arc<FallbackChain> + Send + Sync>;
pub type Emitter = Arc<dyn Fn(PipelineEvent) + Send + Sync>;

enum Msg {
    Submit {
        job: Job,
        immediate: bool,
    },
    /// Translate the last text again with a new target (settings changed).
    Refresh {
        to: Lang,
    },
    Done {
        generation: u64,
        key: DoneKey,
        translated: Option<String>,
    },
}

/// What was last translated, to skip unchanged text.
#[derive(Debug, Clone, PartialEq)]
struct DoneKey {
    text: String,
    from: Option<Lang>,
    to: Lang,
    primary: Option<EngineId>,
}

#[derive(Clone)]
pub struct Pipeline {
    tx: mpsc::UnboundedSender<Msg>,
}

impl Pipeline {
    /// Spawn the pipeline task on the current tokio runtime.
    pub fn spawn(
        chains: ChainProvider,
        cache: Arc<TranslationCache>,
        debounce: Duration,
        emit: Emitter,
    ) -> Pipeline {
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Worker {
            tx: tx.clone(),
            chains,
            cache,
            debounce,
            emit,
            generation: 0,
            pending: None,
            inflight: None,
            last_job: None,
            last_done: None,
            last_translated: None,
        };
        tokio::spawn(worker.run(rx));
        Pipeline { tx }
    }

    /// A live OCR result: translated after the debounce if it changed.
    pub fn submit_live(&self, job: Job) {
        let _ = self.tx.send(Msg::Submit {
            job,
            immediate: false,
        });
    }

    /// A one-shot OCR result: translated right away.
    pub fn submit_now(&self, job: Job) {
        let _ = self.tx.send(Msg::Submit {
            job,
            immediate: true,
        });
    }

    pub fn refresh(&self, to: Lang) {
        let _ = self.tx.send(Msg::Refresh { to });
    }
}

struct Worker {
    tx: mpsc::UnboundedSender<Msg>,
    chains: ChainProvider,
    cache: Arc<TranslationCache>,
    debounce: Duration,
    emit: Emitter,
    generation: u64,
    pending: Option<(Job, Instant)>,
    inflight: Option<(u64, DoneKey, JoinHandle<()>)>,
    last_job: Option<Job>,
    last_done: Option<DoneKey>,
    last_translated: Option<String>,
}

impl Worker {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        loop {
            let deadline = self.pending.as_ref().map(|(_, at)| *at);
            let msg = tokio::select! {
                msg = rx.recv() => match msg {
                    Some(msg) => msg,
                    None => break,
                },
                _ = sleep_until(deadline), if deadline.is_some() => {
                    if let Some((job, _)) = self.pending.take() {
                        self.start(job);
                    }
                    continue;
                }
            };
            match msg {
                Msg::Submit { job, immediate } => self.submit(job, immediate),
                Msg::Refresh { to } => {
                    if let Some(mut job) = self.last_job.clone() {
                        job.to = to;
                        self.pending = None;
                        self.start(job);
                    }
                }
                Msg::Done {
                    generation,
                    key,
                    translated,
                } => {
                    if self.inflight.as_ref().map(|(g, ..)| *g) == Some(generation) {
                        self.inflight = None;
                        if let Some(translated) = translated {
                            self.last_done = Some(key);
                            self.last_translated = Some(translated);
                        }
                    }
                }
            }
        }
        if let Some((.., handle)) = self.inflight.take() {
            handle.abort();
        }
    }

    fn key(&self, job: &Job) -> DoneKey {
        DoneKey {
            text: job.text.clone(),
            from: job.from.clone(),
            to: job.to.clone(),
            primary: (self.chains)().primary(),
        }
    }

    fn submit(&mut self, job: Job, immediate: bool) {
        if job.text.trim().is_empty() {
            self.pending = None;
            return;
        }
        if self
            .last_translated
            .as_deref()
            .is_some_and(|own| is_own_render(&job.text, own))
        {
            // The capture read our own overlay back; keep the current result.
            self.pending = None;
            return;
        }
        let key = self.key(&job);
        let inflight_same = self.inflight.as_ref().is_some_and(|(_, k, _)| *k == key);
        if inflight_same || self.last_done.as_ref() == Some(&key) {
            // Unchanged: drop any newer pending text, it was replaced by this one.
            self.pending = None;
            return;
        }
        if immediate {
            self.pending = None;
            self.start(job);
        } else {
            self.pending = Some((job, Instant::now() + self.debounce));
        }
    }

    fn start(&mut self, job: Job) {
        if let Some((.., handle)) = self.inflight.take() {
            handle.abort();
        }
        self.generation += 1;
        let generation = self.generation;
        let key = self.key(&job);
        self.last_job = Some(job.clone());

        let chain = (self.chains)();
        let cache = self.cache.clone();
        let emit = self.emit.clone();
        let tx = self.tx.clone();
        let done_key = key.clone();
        let handle = tokio::spawn(async move {
            let result = chain
                .translate_cached(&cache, &job.text, job.from.as_ref(), &job.to)
                .await;
            let translated = match result {
                Ok(outcome) => {
                    let t = outcome.translation;
                    let translated = t.text.clone();
                    emit(PipelineEvent::Result(TranslatedEvent {
                        original: job.text,
                        rtl: job.to.is_rtl(),
                        translated: t.text,
                        from: t.detected_from.or(job.from),
                        to: job.to,
                        engine: t.engine,
                        ms: t.ms,
                        cached: outcome.cached,
                        failures: outcome.failures.iter().map(ToString::to_string).collect(),
                    }));
                    Some(translated)
                }
                Err(e) => {
                    emit(PipelineEvent::Error(TranslateErrorEvent {
                        original: job.text,
                        message: e.to_string(),
                    }));
                    None
                }
            };
            let _ = tx.send(Msg::Done {
                generation,
                key: done_key,
                translated,
            });
        });
        self.inflight = Some((generation, key, handle));
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Letters and digits only, lower case: ignores OCR noise in spacing and
/// punctuation.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// True when OCR `text` is (nearly) the translation we are showing, i.e. the
/// capture saw our own overlay instead of the page underneath.
pub fn is_own_render(text: &str, own: &str) -> bool {
    let (a, b) = (normalize(text), normalize(own));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let longest = a.len().max(b.len());
    // Up to 15% of characters may differ (OCR noise). Cheap reject first.
    if a.len().abs_diff(b.len()) * 100 > longest * 15 {
        return false;
    }
    levenshtein(&a, &b) * 100 <= longest * 15
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::chain::tests::Fake;
    use crate::translate::{TranslateError, Translator};
    use std::sync::Mutex;

    struct Harness {
        pipeline: Pipeline,
        events: Arc<Mutex<Vec<PipelineEvent>>>,
        chain: Arc<Mutex<Arc<FallbackChain>>>,
    }

    fn harness(engine: Arc<Fake>) -> Harness {
        let events = Arc::new(Mutex::new(Vec::new()));
        let chain = Arc::new(Mutex::new(Arc::new(FallbackChain::new(vec![
            engine as Arc<dyn Translator>,
        ]))));
        let provider = chain.clone();
        let sink = events.clone();
        let pipeline = Pipeline::spawn(
            Arc::new(move || provider.lock().unwrap().clone()),
            Arc::new(TranslationCache::default()),
            DEBOUNCE,
            Arc::new(move |e| sink.lock().unwrap().push(e)),
        );
        Harness {
            pipeline,
            events,
            chain,
        }
    }

    impl Harness {
        fn results(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .map(|e| match e {
                    PipelineEvent::Result(r) => r.translated.clone(),
                    PipelineEvent::Error(e) => format!("error: {}", e.message),
                })
                .collect()
        }
    }

    fn job(text: &str) -> Job {
        Job {
            text: text.into(),
            from: None,
            to: Lang::new("ar"),
        }
    }

    async fn advance(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn live_text_is_debounced() {
        let engine = Fake::ok(EngineId::GoogleFree);
        let h = harness(engine.clone());
        for text in ["He", "Hell", "Hello"] {
            h.pipeline.submit_live(job(text));
            advance(100).await;
        }
        assert_eq!(engine.calls(), 0, "still inside the debounce window");
        advance(250).await;
        assert_eq!(engine.calls(), 1);
        assert_eq!(h.results(), vec!["[ar] Hello"]);
        let first = h.events.lock().unwrap()[0].clone();
        match first {
            PipelineEvent::Result(r) => {
                assert_eq!(r.original, "Hello");
                assert_eq!(r.to, Lang::new("ar"));
                assert_eq!(r.engine, EngineId::GoogleFree);
                assert!(r.rtl);
                assert!(!r.cached);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_text_is_not_translated_again() {
        let engine = Fake::ok(EngineId::GoogleFree);
        let h = harness(engine.clone());
        h.pipeline.submit_live(job("Hello"));
        advance(400).await;
        h.pipeline.submit_live(job("Hello"));
        advance(400).await;
        h.pipeline.submit_now(job("Hello"));
        advance(10).await;
        assert_eq!(engine.calls(), 1);
        assert_eq!(h.results().len(), 1);
        // Changed text goes through.
        h.pipeline.submit_live(job("World"));
        advance(400).await;
        assert_eq!(engine.calls(), 2);
        assert_eq!(h.results(), vec!["[ar] Hello", "[ar] World"]);
    }

    #[tokio::test(start_paused = true)]
    async fn one_shot_skips_the_debounce() {
        let engine = Fake::ok(EngineId::GoogleFree);
        let h = harness(engine.clone());
        h.pipeline.submit_now(job("Now"));
        advance(1).await;
        assert_eq!(h.results(), vec!["[ar] Now"]);
    }

    #[tokio::test(start_paused = true)]
    async fn stale_requests_are_cancelled() {
        let engine = Fake::slow(EngineId::Openai, Duration::from_secs(1));
        let h = harness(engine.clone());
        h.pipeline.submit_now(job("first"));
        advance(200).await;
        h.pipeline.submit_now(job("second"));
        advance(2000).await;
        assert_eq!(engine.calls(), 2);
        assert_eq!(h.results(), vec!["[ar] second"], "first answer was dropped");
    }

    #[tokio::test(start_paused = true)]
    async fn going_back_to_the_text_in_flight_cancels_pending() {
        let engine = Fake::slow(EngineId::Openai, Duration::from_millis(500));
        let h = harness(engine.clone());
        h.pipeline.submit_now(job("A"));
        advance(50).await;
        h.pipeline.submit_live(job("B"));
        advance(50).await;
        h.pipeline.submit_live(job("A"));
        advance(1000).await;
        assert_eq!(engine.calls(), 1);
        assert_eq!(h.results(), vec!["[ar] A"]);
    }

    #[tokio::test(start_paused = true)]
    async fn our_own_overlay_is_ignored() {
        let engine = Fake::ok(EngineId::GoogleFree);
        let h = harness(engine.clone());
        h.pipeline.submit_now(job("Hello"));
        advance(10).await;
        // OCR of the overlay, with the usual small OCR differences.
        h.pipeline.submit_live(job("[ar]  hello."));
        advance(400).await;
        assert_eq!(engine.calls(), 1);
        assert_eq!(h.results().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_retranslates_with_new_target_and_engine() {
        let engine = Fake::ok(EngineId::GoogleFree);
        let h = harness(engine.clone());
        h.pipeline.submit_now(job("Hello"));
        advance(10).await;
        h.pipeline.refresh(Lang::new("fr"));
        advance(10).await;
        assert_eq!(h.results(), vec!["[ar] Hello", "[fr] Hello"]);

        // Engine switch: the next identical text is translated by the new engine.
        let deepl = Fake::ok(EngineId::Deepl);
        *h.chain.lock().unwrap() = Arc::new(FallbackChain::new(vec![deepl.clone()]));
        h.pipeline.refresh(Lang::new("fr"));
        advance(10).await;
        assert_eq!(deepl.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn errors_are_emitted_and_blank_text_ignored() {
        let engine = Fake::failing(
            EngineId::GoogleFree,
            TranslateError::Network(EngineId::GoogleFree, "offline".into()),
        );
        let h = harness(engine.clone());
        h.pipeline.submit_now(job("   \n"));
        advance(10).await;
        assert_eq!(engine.calls(), 0);
        h.pipeline.submit_now(job("Hello"));
        advance(10).await;
        let results = h.results();
        assert_eq!(results.len(), 1);
        assert!(results[0].starts_with("error: all engines failed"));
        let name = h.events.lock().unwrap()[0].event_name();
        assert_eq!(name, "translate://error");
        // A failed text is retried the next time it is seen.
        h.pipeline.submit_live(job("Hello"));
        advance(400).await;
        assert_eq!(engine.calls(), 2);
    }

    #[test]
    fn own_render_similarity() {
        assert!(is_own_render("Bonjour le monde", "bonjour le monde!"));
        assert!(is_own_render("مرحبا بالعالم", "مرحبا بالعالم."));
        assert!(is_own_render("Bonjour le rnonde", "Bonjour le monde"));
        assert!(!is_own_render("Hello world", "Bonjour le monde"));
        assert!(!is_own_render("", "x"));
        assert!(!is_own_render("short", "a much longer translation text"));
    }
}
