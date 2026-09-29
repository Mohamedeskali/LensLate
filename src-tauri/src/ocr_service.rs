//! App side of OCR: loads the engine on first use (downloading the models with
//! `ocr://models` progress events), serves `ocr_once`, and runs the live
//! worker thread that recognizes changed crops and emits `ocr://result`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use image::RgbaImage;
use tauri::{AppHandle, Emitter};

use crate::capture::{overlay, Rect};
use crate::ocr::engine::PaddleOcrEngine;
use crate::ocr::models::{self, ProgressReporter};
use crate::ocr::{self as core, ModelState, OcrEngine, OcrEvent, OcrResultData};

/// After a failed model download, live mode waits this long before trying
/// again (a click on 文 always retries).
const LIVE_RETRY_AFTER: Duration = Duration::from_secs(30);

static ENGINE: Mutex<Option<PaddleOcrEngine>> = Mutex::new(None);
static LAST_LOAD_ERROR: Mutex<Option<(Instant, String)>> = Mutex::new(None);

/// True while the live worker is recognizing a crop.
static BUSY: AtomicBool = AtomicBool::new(false);
/// Hash of the last crop handed to the live worker (0 = none).
static LAST_HASH: AtomicU64 = AtomicU64::new(0);
static WORKER: OnceLock<SyncSender<RgbaImage>> = OnceLock::new();

fn emit(app: &AppHandle, event: OcrEvent) {
    let _ = app.emit(event.event_name(), &event);
}

/// Publishes model download state as `ocr://models`.
struct EventReporter(AppHandle);

impl ProgressReporter for EventReporter {
    fn report(&self, state: ModelState, progress: Option<f32>, detail: &str) {
        if state != ModelState::Downloading {
            eprintln!("[lenslate] ocr models state={state:?} {detail}");
        }
        let message = (state == ModelState::Error).then(|| detail.to_string());
        emit(
            &self.0,
            OcrEvent::Models {
                state,
                progress,
                message,
            },
        );
    }
}

fn load_engine(app: &AppHandle) -> Result<PaddleOcrEngine, String> {
    let dir = models::default_models_dir();
    let reporter = EventReporter(app.clone());
    models::ensure_models_reported(&dir, &reporter).map_err(|e| e.to_string())?;
    PaddleOcrEngine::load(&dir).map_err(|e| {
        let message = e.to_string();
        reporter.report(ModelState::Error, None, &message);
        message
    })
}

/// Recognize `crop` with the selected script, loading the engine (and
/// downloading the models) on first use. `retry` = false skips the download
/// for a while after a failure, so live mode does not hammer the network.
pub fn recognize(app: &AppHandle, crop: &RgbaImage, retry: bool) -> Result<OcrResultData, String> {
    let mut engine = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if engine.is_none() {
        let mut last_error = LAST_LOAD_ERROR.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, message)) = last_error.as_ref() {
            if !retry && at.elapsed() < LIVE_RETRY_AFTER {
                return Err(message.clone());
            }
        }
        match load_engine(app) {
            Ok(loaded) => {
                *last_error = None;
                *engine = Some(loaded);
            }
            Err(message) => {
                *last_error = Some((Instant::now(), message.clone()));
                return Err(message);
            }
        }
    }
    let engine = engine.as_mut().expect("engine loaded above");
    let mut result = engine.recognize(crop, core::ocr_script()).map_err(|e| {
        eprintln!("[lenslate] ocr error: {e}");
        e.to_string()
    })?;
    result.width = crop.width();
    result.height = crop.height();
    result.colors = result
        .lines
        .iter()
        .map(|line| {
            let r = line.rect;
            overlay::line_colors(
                crop,
                Rect {
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                },
            )
        })
        .collect();
    eprintln!(
        "[lenslate] ocr ms={} lines={} chars={} conf={:.2} script={}",
        result.ms,
        result.lines.len(),
        result.text.chars().count(),
        result.avg_conf(),
        result.script
    );
    Ok(result)
}

/// Offer a live crop to the OCR worker. It is recognized only when its hash
/// differs from the last crop handed over and the worker is idle; otherwise
/// it is dropped. Returns true when the crop was queued.
pub fn offer_live(app: &AppHandle, crop: &RgbaImage, hash: u64) -> bool {
    if LAST_HASH.load(Ordering::SeqCst) == hash || BUSY.swap(true, Ordering::SeqCst) {
        return false;
    }
    LAST_HASH.store(hash, Ordering::SeqCst);
    let worker = WORKER.get_or_init(|| spawn_worker(app.clone()));
    if worker.try_send(crop.clone()).is_err() {
        BUSY.store(false, Ordering::SeqCst);
        return false;
    }
    true
}

/// Make live mode recognize the next frame even if it did not change (used
/// after the script selection changed).
pub fn invalidate_live() {
    LAST_HASH.store(0, Ordering::SeqCst);
}

fn spawn_worker(app: AppHandle) -> SyncSender<RgbaImage> {
    let (tx, rx) = mpsc::sync_channel::<RgbaImage>(1);
    thread::Builder::new()
        .name("lenslate-ocr".into())
        .spawn(move || {
            for crop in rx {
                match recognize(&app, &crop, false) {
                    Ok(r) => {
                        crate::translate_service::submit_live(&r);
                        emit(&app, OcrEvent::Result(r));
                    }
                    Err(message) => emit(&app, OcrEvent::Error { message }),
                }
                BUSY.store(false, Ordering::SeqCst);
            }
        })
        .expect("spawn OCR worker thread");
    tx
}
