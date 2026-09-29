use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

mod capture;
pub mod ocr;
mod ocr_service;
mod settings;
pub mod translate;
mod translate_service;

use capture::multi::{locate_in_frames, LiveLocator, Observation};
use capture::overlay::{Decision, OverlayGuard};
#[cfg(target_os = "linux")]
use capture::wayland::WaylandCapture;
use capture::xcap_backend::XCapCapture;
use capture::{
    crop_inside, frame_hash, CaptureEvent, Located, MonitorFrame, NoOpCapture, Rect, ScreenCapture,
    INSET_PX,
};

static FRAME_VISIBLE: AtomicBool = AtomicBool::new(false);
static LIVE_CAPTURE_RUNNING: AtomicBool = AtomicBool::new(false);

fn toggle_frame(app: &AppHandle) {
    let Some(frame) = app.get_webview_window("frame") else {
        return;
    };
    let visible = FRAME_VISIBLE.fetch_xor(true, Ordering::SeqCst);
    if visible {
        let _ = frame.hide();
    } else {
        let _ = frame.show();
        let _ = frame.set_focus();
    }
}

fn show_utility(app: &AppHandle, label: &str, title: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html?view={label}").into()),
    )
    .title(title)
    .inner_size(620.0, 560.0)
    .min_inner_size(480.0, 400.0)
    .resizable(true)
    .build();
}

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Show / Hide frame", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let history = MenuItem::with_id(app, "history", "History", true, None::<&str>)?;
    let save_debug = MenuItem::with_id(
        app,
        "save_debug",
        "Save last capture (dev)",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &settings, &history, &save_debug, &quit])?;
    TrayIconBuilder::new()
        .icon(Image::from_bytes(include_bytes!("../icons/icon.png"))?)
        .menu(&menu)
        .tooltip("LensLate")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => toggle_frame(app),
            "settings" => show_utility(app, "settings", "LensLate Settings"),
            "history" => show_utility(app, "history", "LensLate History"),
            "save_debug" => {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = save_last_capture(app_handle).await;
                });
            }
            "quit" => {
                // Off the UI thread: shutdown joins the capture worker.
                let app = app.clone();
                thread::spawn(move || {
                    shutdown_capture();
                    app.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_frame(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
}

fn create_capture_backend(window_label: String, app: &AppHandle) -> Box<dyn ScreenCapture> {
    #[cfg(target_os = "linux")]
    if is_wayland_session() {
        return Box::new(WaylandCapture::new());
    }
    let mut backend = XCapCapture::new(window_label);
    backend.set_app_handle(app.clone());
    Box::new(backend)
}

struct CaptureState {
    backend: Box<dyn ScreenCapture>,
    last_frames: Vec<MonitorFrame>,
    last_cropped: Option<image::RgbaImage>,
    last_located: Option<Located>,
    capture_thread: Option<thread::JoinHandle<()>>,
}

impl CaptureState {
    fn new() -> Self {
        Self {
            backend: Box::new(NoOpCapture),
            last_frames: Vec::new(),
            last_cropped: None,
            last_located: None,
            capture_thread: None,
        }
    }
}

static CAPTURE_STATE: std::sync::OnceLock<Arc<Mutex<CaptureState>>> = std::sync::OnceLock::new();
/// Boxes our overlay draws inside the frame (see `capture::overlay`).
static OVERLAY_GUARD: Mutex<Option<OverlayGuard>> = Mutex::new(None);

fn get_capture_state() -> Arc<Mutex<CaptureState>> {
    CAPTURE_STATE
        .get_or_init(|| Arc::new(Mutex::new(CaptureState::new())))
        .clone()
}

fn with_guard<T>(f: impl FnOnce(&mut OverlayGuard) -> T) -> T {
    let mut guard = OVERLAY_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(OverlayGuard::new))
}

#[tauri::command]
fn toggle_frame_command(app: AppHandle) {
    toggle_frame(&app);
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    show_utility(&app, "settings", "LensLate Settings");
}

#[tauri::command]
fn log_frontend_error(message: String) {
    eprintln!("[lenslate] {message}");
}

/// Stop live capture and release the stream and portal session.
fn shutdown_capture() {
    LIVE_CAPTURE_RUNNING.store(false, Ordering::SeqCst);
    let state_arc = get_capture_state();
    let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    state.backend.shutdown();
}

fn is_frame_visible(app: &AppHandle) -> bool {
    app.get_webview_window("frame")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// Number of monitors connected, as the windowing system reports them.
fn monitor_count(app: &AppHandle) -> usize {
    app.get_webview_window("frame")
        .and_then(|w| w.available_monitors().ok())
        .map_or(0, |m| m.len())
}

/// The portal shares only the monitors the user picked; true when some
/// connected monitor is not among them (the frame may be there).
fn missing_shared_monitors(app: &AppHandle, backend: &dyn ScreenCapture) -> bool {
    backend
        .shared_monitors()
        .is_some_and(|shared| shared < monitor_count(app))
}

fn log_located(located: &Located, extra: &str) {
    let r = located.rect;
    eprintln!(
        "[lenslate] capture monitor={} frame={},{},{}x{}{extra}",
        located.monitor, r.x, r.y, r.w, r.h
    );
}

#[tauri::command]
async fn capture_once() -> Result<CaptureEvent, String> {
    tauri::async_runtime::spawn_blocking(capture_once_blocking)
        .await
        .map_err(|e| e.to_string())
}

/// Capture every monitor, find the frame and crop inside its border.
fn capture_crop(state: &mut CaptureState) -> Result<(image::RgbaImage, Located), String> {
    let frames = state.backend.capture_monitors().map_err(|e| {
        let msg = e.to_string();
        eprintln!("[lenslate] capture error: {}", msg);
        if msg.contains("Permission") || msg.contains("denied") {
            "Screen capture permission denied".to_string()
        } else {
            msg
        }
    })?;
    let located = locate_in_frames(&frames, state.last_located.as_ref());
    state.last_frames = frames;
    let Some(located) = located else {
        let names: Vec<String> = state.last_frames.iter().map(|f| format!("{f:?}")).collect();
        eprintln!("[lenslate] capture found=no searched={names:?}");
        return Err("Frame not found".into());
    };
    log_located(&located, "");
    let frame = state
        .last_frames
        .iter()
        .find(|f| f.index == located.index)
        .expect("located on one of the frames");
    let cropped = crop_inside(&frame.image, located.rect, INSET_PX);
    state.last_located = Some(located.clone());
    state.last_cropped = Some(cropped.clone());
    Ok((cropped, located))
}

/// `capture_crop`, asking the portal for the screens again once when the
/// frame is on a monitor that is not shared.
fn capture_crop_reselecting(
    app: &AppHandle,
    state: &mut CaptureState,
) -> Result<(image::RgbaImage, Located), String> {
    match capture_crop(state) {
        Err(e)
            if e == "Frame not found" && missing_shared_monitors(app, state.backend.as_ref()) =>
        {
            eprintln!("[lenslate] frame not on the shared monitors; asking again");
            state.backend.reselect();
            state.last_located = None;
            capture_crop(state)
        }
        other => other,
    }
}

fn capture_once_blocking() -> CaptureEvent {
    let state_arc = get_capture_state();
    let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let start = Instant::now();

    match capture_crop(&mut state) {
        Ok((cropped, _)) => CaptureEvent::Frame {
            thumbnail_png_base64: create_thumbnail(&cropped),
            width: cropped.width(),
            height: cropped.height(),
            ms: start.elapsed().as_millis() as u64,
            skipped: 0,
        },
        Err(message) => CaptureEvent::Error { message },
    }
}

/// 文: capture once, crop inside the frame, recognize and translate the text.
#[tauri::command]
async fn ocr_once(app: AppHandle) -> Result<ocr::OcrResultData, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // The frontend hid its overlay; let the compositor repaint first.
        let wait = with_guard(|g| g.settle_left(Instant::now()));
        if !wait.is_zero() {
            thread::sleep(wait);
        }
        let start = Instant::now();
        let (cropped, _) = {
            let state_arc = get_capture_state();
            let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
            capture_crop_reselecting(&app, &mut state)?
        };
        eprintln!(
            "[lenslate] capture ms={} (ocr_once)",
            start.elapsed().as_millis()
        );
        let result = ocr_service::recognize(&app, &cropped, true)?;
        translate_service::submit_now(&result);
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Source script for OCR (Auto / Latin / Arabic); live mode re-reads the
/// current crop with it.
#[tauri::command]
fn set_ocr_script(script: ocr::Script) {
    eprintln!("[lenslate] ocr script={script}");
    ocr::set_ocr_script(script);
    ocr_service::invalidate_live();
}

/// The boxes (crop pixels) the frontend draws over the captured area; empty
/// once it hid them.
#[tauri::command]
fn set_overlay_boxes(boxes: Vec<Rect>) {
    let count = boxes.len();
    with_guard(|g| g.set_boxes(boxes, Instant::now()));
    if count == 0 {
        // Read the page again once it is visible without our drawing.
        ocr_service::invalidate_live();
    }
}

/// Ask the portal for the screens to share again (Wayland).
#[tauri::command]
async fn capture_reselect() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let state_arc = get_capture_state();
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        state.backend.reselect();
        state.last_located = None;
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn capture_backend() -> &'static str {
    let state_arc = get_capture_state();
    let state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    if state.backend.is_wayland() {
        "wayland"
    } else {
        "xcap"
    }
}

/// Hand a crop to OCR unless it shows our own overlay.
fn process_live_crop(app: &AppHandle, cropped: &image::RgbaImage) {
    match with_guard(|g| g.decide(cropped, Instant::now())) {
        Decision::Ocr => {
            ocr_service::offer_live(app, cropped, frame_hash(cropped));
        }
        Decision::Skip => {}
        Decision::Suspend => {
            eprintln!("[lenslate] page changed under the overlay; hiding it to read again");
            let _ = app.emit("overlay://suspend", ());
        }
    }
}

#[tauri::command]
async fn live_start(app: AppHandle) -> Result<(), String> {
    if LIVE_CAPTURE_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let state_arc = get_capture_state();
    let (tx, rx) = mpsc::channel::<MonitorFrame>();

    // Starting may block on the portal dialog, so keep it off the async runtime.
    let start_state = state_arc.clone();
    let started = tauri::async_runtime::spawn_blocking(move || {
        let mut state = start_state.lock().unwrap_or_else(|e| e.into_inner());
        state.backend.start_stream(2, tx).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    if let Err(e) = started {
        LIVE_CAPTURE_RUNNING.store(false, Ordering::SeqCst);
        return Err(e);
    }

    let app_handle = app.clone();
    let state_arc_clone = state_arc.clone();
    let handle = thread::spawn(move || {
        const MAX_MISSING_REUSE: u32 = 3;
        let mut locator = LiveLocator::new();
        let mut skipped = 0u64;
        let mut last_emit = Instant::now();
        let mut last_logged: Option<Located> = None;
        let mut last_hash = 0u64;

        while LIVE_CAPTURE_RUNNING.load(Ordering::SeqCst) {
            if !is_frame_visible(&app_handle) {
                LIVE_CAPTURE_RUNNING.store(false, Ordering::SeqCst);
                let _ = app_handle.emit("capture://stopped", ());
                break;
            }
            let Ok(frame) = rx.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            let start = Instant::now();

            let (located, reused) = match locator.observe(frame) {
                Observation::Found(located) => (located, false),
                Observation::Missing { last, consecutive } if consecutive <= MAX_MISSING_REUSE => {
                    (last, true)
                }
                Observation::Ignored => continue,
                Observation::Missing { .. } | Observation::NotFound => {
                    skipped += 1;
                    if last_emit.elapsed() > Duration::from_secs(1) {
                        let state = state_arc_clone.lock().unwrap_or_else(|e| e.into_inner());
                        let unshared = missing_shared_monitors(&app_handle, state.backend.as_ref());
                        drop(state);
                        eprintln!(
                            "[lenslate] capture found=no ms={} skipped={skipped} unshared_monitors={unshared}",
                            start.elapsed().as_millis()
                        );
                        let _ = app_handle.emit(
                            "capture://error",
                            CaptureEvent::Error {
                                message: "Frame not found".into(),
                            },
                        );
                        last_emit = Instant::now();
                    }
                    continue;
                }
            };
            let Some(frame) = locator.frame(located.index) else {
                continue;
            };
            if !reused && last_logged.as_ref() != Some(&located) {
                log_located(&located, " (live)");
                last_logged = Some(located.clone());
            }
            let cropped = crop_inside(&frame.image, located.rect, INSET_PX);
            process_live_crop(&app_handle, &cropped);

            let hash = frame_hash(&cropped);
            if hash == last_hash {
                skipped += 1;
                continue;
            }
            last_hash = hash;
            {
                let mut state = state_arc_clone.lock().unwrap_or_else(|e| e.into_inner());
                state.last_frames = vec![frame.clone()];
                state.last_located = Some(located.clone());
                state.last_cropped = Some(cropped.clone());
            }
            if last_emit.elapsed() >= Duration::from_secs(1) {
                let _ = app_handle.emit(
                    "capture://frame",
                    CaptureEvent::Frame {
                        thumbnail_png_base64: create_thumbnail(&cropped),
                        width: cropped.width(),
                        height: cropped.height(),
                        ms: start.elapsed().as_millis() as u64,
                        skipped,
                    },
                );
                skipped = 0;
                last_emit = Instant::now();
            } else {
                skipped += 1;
            }
        }

        let mut state = state_arc_clone.lock().unwrap_or_else(|e| e.into_inner());
        state.backend.stop_stream();
    });

    {
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        state.capture_thread = Some(handle);
    }
    Ok(())
}

#[tauri::command]
async fn live_stop() -> Result<(), String> {
    if !LIVE_CAPTURE_RUNNING.swap(false, Ordering::SeqCst) {
        return Ok(());
    }

    let state_arc = get_capture_state();
    {
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        state.backend.stop_stream();
    }

    let handle = {
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        state.capture_thread.take()
    };

    if let Some(handle) = handle {
        let _ = handle.join();
    }

    Ok(())
}

#[tauri::command]
async fn save_last_capture(_app: AppHandle) -> Result<String, String> {
    let state_arc = get_capture_state();
    let state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = dirs::picture_dir()
        .ok_or("No Pictures directory")?
        .join("lenslate-debug");

    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut saved = Vec::new();

    for full in &state.last_frames {
        let path = dir.join(format!("full-{}-{}.png", timestamp, full.index));
        full.image.save(&path).map_err(|e| e.to_string())?;
        saved.push(path.to_string_lossy().to_string());
    }

    if let Some(cropped) = &state.last_cropped {
        let path = dir.join(format!("crop-{}.png", timestamp));
        cropped.save(&path).map_err(|e| e.to_string())?;
        saved.push(path.to_string_lossy().to_string());
    }

    Ok(format!("Saved: {}", saved.join(", ")))
}

fn create_thumbnail(img: &image::RgbaImage) -> String {
    use base64::{engine::general_purpose, Engine as _};
    use image::imageops::FilterType;

    let max_dim = 120u32;
    let (w, h) = (img.width(), img.height());
    let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
    let new_w = (w as f32 * scale) as u32;
    let new_h = (h as f32 * scale) as u32;

    let thumb = image::imageops::resize(img, new_w, new_h, FilterType::Lanczos3);

    let mut png_data = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_data, thumb.width(), thumb.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&thumb).unwrap();
    writer.finish().unwrap();

    general_purpose::STANDARD.encode(&png_data)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    eprintln!(
        "[lenslate] GDK_BACKEND={}, WAYLAND_DISPLAY={}, DISPLAY={}",
        std::env::var("GDK_BACKEND").unwrap_or_else(|_| "<unset>".into()),
        std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "<unset>".into()),
        std::env::var("DISPLAY").unwrap_or_else(|_| "<unset>".into()),
    );
    let shortcut = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyY);
    let shortcut_for_handler = shortcut;
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _| {
            eprintln!("[lenslate] toggle received (argv: {argv:?})");
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || toggle_frame(&handle));
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(move |app, pressed, event| {
            if pressed == &shortcut_for_handler && event.state() == ShortcutState::Pressed { toggle_frame(app); }
        }).build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let frame = app.get_webview_window("frame").ok_or_else(|| {
                tauri::Error::WindowNotFound
            })?;
            let _ = frame.set_title("LensLate");
            let _ = frame.set_shadow(false);
            let _ = app.global_shortcut().register(shortcut);
            if std::env::args().any(|arg| arg == "--toggle") { toggle_frame(app.handle()); }
            if let Err(error) = create_tray(app.handle()) { eprintln!("Could not create tray icon: {error}"); }
            #[cfg(target_os = "linux")]
            eprintln!("XDG GlobalShortcuts portal unavailable through Tauri; use lenslate --toggle with a GNOME custom shortcut.");

            // Initialize capture state with app handle
            let state_arc = get_capture_state();
            {
                let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
                state.backend = create_capture_backend("frame".to_string(), app.handle());
                eprintln!("[lenslate] capture backend wayland={}", state.backend.is_wayland());
            }
            translate_service::init(app.handle());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            toggle_frame_command,
            open_settings,
            log_frontend_error,
            capture_once,
            ocr_once,
            set_ocr_script,
            set_overlay_boxes,
            capture_reselect,
            capture_backend,
            live_start,
            live_stop,
            save_last_capture,
            translate_service::get_settings,
            translate_service::update_settings,
            translate_service::set_api_key,
            translate_service::delete_api_key,
            translate_service::api_key_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running LensLate");
}
