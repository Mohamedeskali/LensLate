//! User-ordered fallback over several engines, with a timeout per engine.

use std::sync::Arc;
use std::time::Duration;

use super::cache::{CacheKey, TranslationCache};
use super::{
    EngineId, Lang, TranslateError, TranslateResult, Translation, Translator, ENGINE_TIMEOUT,
};

/// A successful chain run: the translation and the engines that failed first.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainOutcome {
    pub translation: Translation,
    pub failures: Vec<TranslateError>,
    pub cached: bool,
}

pub struct FallbackChain {
    engines: Vec<Arc<dyn Translator>>,
    timeout: Duration,
}

impl FallbackChain {
    pub fn new(engines: Vec<Arc<dyn Translator>>) -> Self {
        Self::with_timeout(engines, ENGINE_TIMEOUT)
    }

    pub fn with_timeout(engines: Vec<Arc<dyn Translator>>, timeout: Duration) -> Self {
        Self { engines, timeout }
    }

    pub fn ids(&self) -> Vec<EngineId> {
        self.engines.iter().map(|e| e.id()).collect()
    }

    /// The engine tried first; the cache is keyed by it.
    pub fn primary(&self) -> Option<EngineId> {
        self.engines.first().map(|e| e.id())
    }

    /// Try each engine in order until one answers.
    pub async fn translate(
        &self,
        text: &str,
        from: Option<&Lang>,
        to: &Lang,
    ) -> TranslateResult<ChainOutcome> {
        if self.engines.is_empty() {
            return Err(TranslateError::NoEngine);
        }
        let mut failures = Vec::new();
        for engine in &self.engines {
            let id = engine.id();
            let result = tokio::time::timeout(self.timeout, engine.translate(text, from, to))
                .await
                .unwrap_or(Err(TranslateError::Timeout(id)));
            match result {
                Ok(translation) => {
                    return Ok(ChainOutcome {
                        translation,
                        failures,
                        cached: false,
                    })
                }
                Err(e) => failures.push(e),
            }
        }
        let summary = failures
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        Err(TranslateError::AllFailed(summary))
    }

    /// Like [`translate`](Self::translate), but answers from `cache` when the
    /// same text was translated before with the same languages and primary engine.
    pub async fn translate_cached(
        &self,
        cache: &TranslationCache,
        text: &str,
        from: Option<&Lang>,
        to: &Lang,
    ) -> TranslateResult<ChainOutcome> {
        let primary = self.primary().ok_or(TranslateError::NoEngine)?;
        let key = CacheKey::new(text, from, to, primary);
        if let Some(translation) = cache.get(&key) {
            return Ok(ChainOutcome {
                translation,
                failures: Vec::new(),
                cached: true,
            });
        }
        let outcome = self.translate(text, from, to).await?;
        cache.put(key, outcome.translation.clone());
        Ok(outcome)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Scripted engine for chain and pipeline tests.
    pub struct Fake {
        pub id: EngineId,
        pub delay: Duration,
        pub fail: Option<TranslateError>,
        pub calls: AtomicUsize,
        pub log: Option<Arc<Mutex<Vec<EngineId>>>>,
    }

    impl Fake {
        pub fn ok(id: EngineId) -> Arc<Fake> {
            Arc::new(Fake {
                id,
                delay: Duration::ZERO,
                fail: None,
                calls: AtomicUsize::new(0),
                log: None,
            })
        }

        pub fn failing(id: EngineId, error: TranslateError) -> Arc<Fake> {
            Arc::new(Fake {
                fail: Some(error),
                ..Arc::into_inner(Fake::ok(id)).unwrap()
            })
        }

        pub fn slow(id: EngineId, delay: Duration) -> Arc<Fake> {
            Arc::new(Fake {
                delay,
                ..Arc::into_inner(Fake::ok(id)).unwrap()
            })
        }

        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Translator for Fake {
        fn id(&self) -> EngineId {
            self.id
        }

        async fn translate(
            &self,
            text: &str,
            _from: Option<&Lang>,
            to: &Lang,
        ) -> TranslateResult<Translation> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(log) = &self.log {
                log.lock().unwrap().push(self.id);
            }
            tokio::time::sleep(self.delay).await;
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            Ok(Translation {
                text: format!("[{to}] {text}"),
                detected_from: None,
                engine: self.id,
                ms: self.delay.as_millis() as u64,
            })
        }
    }

    fn chain(engines: &[Arc<Fake>]) -> FallbackChain {
        FallbackChain::new(
            engines
                .iter()
                .map(|e| e.clone() as Arc<dyn Translator>)
                .collect(),
        )
    }

    #[tokio::test]
    async fn first_working_engine_answers() {
        let a = Fake::failing(EngineId::Deepl, TranslateError::MissingKey(EngineId::Deepl));
        let b = Fake::ok(EngineId::GoogleFree);
        let c = Fake::ok(EngineId::Ollama);
        let out = chain(&[a.clone(), b.clone(), c.clone()])
            .translate("hi", None, &Lang::new("ar"))
            .await
            .unwrap();
        assert_eq!(out.translation.engine, EngineId::GoogleFree);
        assert_eq!(out.translation.text, "[ar] hi");
        assert_eq!(
            out.failures,
            vec![TranslateError::MissingKey(EngineId::Deepl)]
        );
        assert_eq!((a.calls(), b.calls(), c.calls()), (1, 1, 0));
    }

    #[tokio::test]
    async fn order_is_respected() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let make = |id, fail: bool| {
            let fake = Fake {
                log: Some(log.clone()),
                fail: fail.then(|| TranslateError::Network(id, "down".into())),
                ..Arc::into_inner(Fake::ok(id)).unwrap()
            };
            Arc::new(fake)
        };
        let engines = [
            make(EngineId::Claude, true),
            make(EngineId::Openai, true),
            make(EngineId::GoogleFree, false),
        ];
        let out = chain(&engines)
            .translate("x", None, &Lang::new("en"))
            .await
            .unwrap();
        assert_eq!(out.translation.engine, EngineId::GoogleFree);
        assert_eq!(
            *log.lock().unwrap(),
            vec![EngineId::Claude, EngineId::Openai, EngineId::GoogleFree]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_engine_times_out_after_five_seconds() {
        let slow = Fake::slow(EngineId::Openai, Duration::from_secs(30));
        let fast = Fake::ok(EngineId::GoogleFree);
        let start = tokio::time::Instant::now();
        let out = chain(&[slow, fast])
            .translate("x", None, &Lang::new("en"))
            .await
            .unwrap();
        assert_eq!(out.translation.engine, EngineId::GoogleFree);
        assert_eq!(
            out.failures,
            vec![TranslateError::Timeout(EngineId::Openai)]
        );
        assert_eq!(start.elapsed(), ENGINE_TIMEOUT);
    }

    #[tokio::test]
    async fn all_failing_and_empty() {
        let a = Fake::failing(EngineId::Deepl, TranslateError::MissingKey(EngineId::Deepl));
        let b = Fake::failing(
            EngineId::GoogleFree,
            TranslateError::Network(EngineId::GoogleFree, "offline".into()),
        );
        match chain(&[a, b]).translate("x", None, &Lang::new("en")).await {
            Err(TranslateError::AllFailed(msg)) => {
                assert!(msg.contains("deepl: no API key set"));
                assert!(msg.contains("offline"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            chain(&[]).translate("x", None, &Lang::new("en")).await,
            Err(TranslateError::NoEngine)
        );
    }

    #[tokio::test]
    async fn cache_hits_skip_the_engines() {
        let a = Fake::ok(EngineId::GoogleFree);
        let chain = chain(std::slice::from_ref(&a));
        let cache = TranslationCache::default();
        let to = Lang::new("ar");
        let first = chain
            .translate_cached(&cache, "same", None, &to)
            .await
            .unwrap();
        let second = chain
            .translate_cached(&cache, "same", None, &to)
            .await
            .unwrap();
        assert!(!first.cached);
        assert!(second.cached);
        assert_eq!(first.translation, second.translation);
        assert_eq!(a.calls(), 1);
        // Different text or target language goes to the engine again.
        chain
            .translate_cached(&cache, "other", None, &to)
            .await
            .unwrap();
        chain
            .translate_cached(&cache, "same", None, &Lang::new("fr"))
            .await
            .unwrap();
        assert_eq!(a.calls(), 3);
    }

    #[tokio::test]
    async fn failures_are_not_cached() {
        let a = Fake::failing(
            EngineId::GoogleFree,
            TranslateError::Network(EngineId::GoogleFree, "offline".into()),
        );
        let chain = chain(std::slice::from_ref(&a));
        let cache = TranslationCache::default();
        let to = Lang::new("ar");
        assert!(chain
            .translate_cached(&cache, "x", None, &to)
            .await
            .is_err());
        assert!(chain
            .translate_cached(&cache, "x", None, &to)
            .await
            .is_err());
        assert_eq!(a.calls(), 2);
        assert!(cache.is_empty());
    }
}
