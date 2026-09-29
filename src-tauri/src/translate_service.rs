//! App side of translation: owns the settings, the engine chain (rebuilt when
//! settings or keys change), the cache and the live pipeline, and serves the
//! settings / API key commands.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use tauri::{AppHandle, Emitter};

use crate::ocr::{OcrResultData, Script};
use crate::settings::{self, Settings};
use crate::translate::cache::TranslationCache;
use crate::translate::chain::FallbackChain;
use crate::translate::keys::{self, KeyringStore, SecretStore};
use crate::translate::pipeline::{self, Job, Pipeline, PipelineEvent};
use crate::translate::registry::build_chain;
use crate::translate::{http_client, EngineId, Lang};

struct Service {
    app: AppHandle,
    settings: RwLock<Settings>,
    chain: Arc<RwLock<Arc<FallbackChain>>>,
    secrets: Arc<dyn SecretStore>,
    client: reqwest::Client,
    pipeline: Pipeline,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

fn service() -> Result<&'static Service, String> {
    SERVICE
        .get()
        .ok_or_else(|| "translation is not ready yet".to_string())
}

fn log_event(event: &PipelineEvent) {
    match event {
        PipelineEvent::Result(r) => {
            eprintln!(
                "[lenslate] translate engine={} ms={} cached={} chars={} from={} to={}",
                r.engine,
                r.ms,
                r.cached,
                r.original.chars().count(),
                r.from.as_ref().map_or("auto", Lang::code),
                r.to
            );
            for failure in &r.failures {
                eprintln!("[lenslate] translate fallback: {failure}");
            }
        }
        PipelineEvent::Error(e) => eprintln!("[lenslate] translate error: {}", e.message),
    }
}

/// Load the settings and start the pipeline. Call once from `setup`.
pub fn init(app: &AppHandle) {
    let settings = settings::load(app);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringStore::new());
    let client = http_client();
    let chain = Arc::new(RwLock::new(Arc::new(build_chain(
        &settings.engines,
        secrets.as_ref(),
        &client,
    ))));
    eprintln!(
        "[lenslate] translate target={} chain={:?}",
        settings.target_lang,
        settings.engines.resolved_order()
    );

    let provider = chain.clone();
    let emit_app = app.clone();
    let pipeline = tauri::async_runtime::block_on(async move {
        Pipeline::spawn(
            Arc::new(move || provider.read().unwrap_or_else(|e| e.into_inner()).clone()),
            Arc::new(TranslationCache::default()),
            pipeline::DEBOUNCE,
            Arc::new(move |event: PipelineEvent| {
                log_event(&event);
                let name = event.event_name();
                let _ = match event {
                    PipelineEvent::Result(r) => emit_app.emit(name, r),
                    PipelineEvent::Error(e) => emit_app.emit(name, e),
                };
            }),
        )
    });

    let _ = SERVICE.set(Service {
        app: app.clone(),
        settings: RwLock::new(settings),
        chain,
        secrets,
        client,
        pipeline,
    });
}

impl Service {
    fn settings(&self) -> Settings {
        self.settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn rebuild_chain(&self) {
        let settings = self.settings();
        let chain = build_chain(&settings.engines, self.secrets.as_ref(), &self.client);
        eprintln!("[lenslate] translate chain={:?}", chain.ids());
        *self.chain.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(chain);
    }

    fn job(&self, ocr: &OcrResultData) -> Job {
        // The Arabic OCR model only reads Arabic, so the source is known.
        let from = (ocr.script == Script::Arabic).then(|| Lang::new("ar"));
        Job {
            text: ocr.text.clone(),
            from,
            to: Lang::new(&self.settings().target_lang),
        }
    }
}

/// Translate a live OCR result (debounced, only when the text changed).
pub fn submit_live(ocr: &OcrResultData) {
    if let Ok(s) = service() {
        s.pipeline.submit_live(s.job(ocr));
    }
}

/// Translate a one-shot OCR result right away.
pub fn submit_now(ocr: &OcrResultData) {
    if let Ok(s) = service() {
        s.pipeline.submit_now(s.job(ocr));
    }
}

#[tauri::command]
pub fn get_settings() -> Result<Settings, String> {
    Ok(service()?.settings())
}

#[tauri::command]
pub fn update_settings(settings: Settings) -> Result<Settings, String> {
    let s = service()?;
    let settings = settings.sanitized();
    let old = s.settings();
    settings::save(&s.app, &settings)?;
    *s.settings.write().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    if old.engines != settings.engines {
        s.rebuild_chain();
    }
    if old.engines != settings.engines || old.target_lang != settings.target_lang {
        s.pipeline.refresh(Lang::new(&settings.target_lang));
    }
    Ok(settings)
}

/// Store an engine's API key in the OS keychain. The key is never logged.
#[tauri::command]
pub fn set_api_key(engine: EngineId, key: String) -> Result<HashMap<EngineId, bool>, String> {
    let s = service()?;
    s.secrets
        .set(engine, &key)
        .map_err(|e| format!("Could not save the key in the system keychain: {e}"))?;
    eprintln!("[lenslate] api key saved for {engine}");
    s.rebuild_chain();
    s.pipeline.refresh(Lang::new(&s.settings().target_lang));
    Ok(keys::key_status(s.secrets.as_ref()))
}

#[tauri::command]
pub fn delete_api_key(engine: EngineId) -> Result<HashMap<EngineId, bool>, String> {
    let s = service()?;
    s.secrets
        .delete(engine)
        .map_err(|e| format!("Could not remove the key from the system keychain: {e}"))?;
    eprintln!("[lenslate] api key removed for {engine}");
    s.rebuild_chain();
    Ok(keys::key_status(s.secrets.as_ref()))
}

/// Which engines have a key stored (never the keys themselves).
#[tauri::command]
pub fn api_key_status() -> Result<HashMap<EngineId, bool>, String> {
    Ok(keys::key_status(service()?.secrets.as_ref()))
}
