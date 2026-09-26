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

use capture::{CaptureEvent, Rect, ScreenCapture, INSET_PX, NoOpCapture};
use capture::locate::{crop_inside, frame_hash, locate_frame};
use capture::wayland::WaylandCapture;
use capture::xcap_backend::XCapCapture;

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
    .inner_size(440.0, 320.0)
    .resizable(false)
    .build();
}

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Show / Hide frame", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let history = MenuItem::with_id(app, "history", "History", true, None::<&str>)?;
    let save_debug = MenuItem::with_id(app, "save_debug", "Save last capture (dev)", true, None::<&str>)?;
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
            "quit" => app.exit(0),
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

fn is_wayland_session() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
}

fn create_capture_backend(window_label: String, app: &AppHandle) -> Box<dyn ScreenCapture> {
    if is_wayland_session() {
        Box::new(WaylandCapture::new())
    } else {
        let mut backend = XCapCapture::new(window_label);
        backend.set_app_handle(app.clone());
        Box::new(backend)
    }
}

struct CaptureState {
    backend: Box<dyn ScreenCapture>,
    last_full_frame: Option<image::RgbaImage>,
    last_cropped: Option<image::RgbaImage>,
    last_rect: Option<Rect>,
    frame_tx: Option<mpsc::Sender<image::RgbaImage>>,
    capture_thread: Option<thread::JoinHandle<()>>,
}

impl CaptureState {
    fn new(_window_label: String) -> Self {
        Self {
            backend: Box::new(NoOpCapture),
            last_full_frame: None,
            last_cropped: None,
            last_rect: None,
            frame_tx: None,
            capture_thread: None,
        }
    }
}

static CAPTURE_STATE: std::sync::OnceLock<Arc<Mutex<CaptureState>>> = std::sync::OnceLock::new();

fn get_capture_state() -> Arc<Mutex<CaptureState>> {
    CAPTURE_STATE.get_or_init(|| Arc::new(Mutex::new(CaptureState::new("frame".to_string())))).clone()
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

#[tauri::command]
async fn capture_once(_app: AppHandle) -> Result<CaptureEvent, String> {
    let state_arc = get_capture_state();
    let mut state = state_arc.lock().unwrap();
    let start = Instant::now();

    match state.backend.capture_monitor() {
        Ok(full_frame) => {
            state.last_full_frame = Some(full_frame.clone());
            let rect = locate_frame(&full_frame, state.last_rect);
            
            match rect {
                Some(r) => {
                    state.last_rect = Some(r);
                    let cropped = crop_inside(&full_frame, r, INSET_PX);
                    state.last_cropped = Some(cropped.clone());
                    
                    let thumbnail = create_thumbnail(&cropped);
                    let ms = start.elapsed().as_millis() as u64;
                    
                    eprintln!("[lenslate] capture found=yes rect={},{},{}x{} ms={} skipped=0", r.x, r.y, r.w, r.h, ms);
                    
                    Ok(CaptureEvent::Frame {
                        thumbnail_png_base64: thumbnail,
                        width: cropped.width(),
                        height: cropped.height(),
                        ms,
                        skipped: 0,
                    })
                }
                None => {
                    eprintln!("[lenslate] capture found=no ms={}", start.elapsed().as_millis());
                    Ok(CaptureEvent::Error { message: "Frame not found".into() })
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            eprintln!("[lenslate] capture error: {}", msg);
            if msg.contains("Permission") || msg.contains("denied") {
                Ok(CaptureEvent::Error { message: "Screen capture permission denied".into() })
            } else {
                Ok(CaptureEvent::Error { message: msg })
            }
        }
    }
}

#[tauri::command]
async fn live_start(app: AppHandle) -> Result<(), String> {
    if LIVE_CAPTURE_RUNNING.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let state_arc = get_capture_state();
    let (tx, rx) = mpsc::channel();
    
    {
        let mut state = state_arc.lock().unwrap();
        state.frame_tx = Some(tx.clone());
        state.backend.start_stream(2, tx).map_err(|e| e.to_string())?;
    }

    let app_handle = app.clone();
    let state_arc_clone = state_arc.clone();
    let handle = thread::spawn(move || {
        let mut skipped = 0u64;
        let mut last_emit = Instant::now();
        let mut last_rect: Option<Rect> = None;

        while LIVE_CAPTURE_RUNNING.load(Ordering::SeqCst) {
            let full_frame = if let Ok(frame) = rx.recv_timeout(Duration::from_millis(100)) {
                frame
            } else {
                continue;
            };

            let start = Instant::now();
            
            {
                let mut state = state_arc_clone.lock().unwrap();
                state.last_full_frame = Some(full_frame.clone());
            }
            
            let rect = locate_frame(&full_frame, last_rect);
            
            match rect {
                Some(r) => {
                    last_rect = Some(r);
                    let cropped = crop_inside(&full_frame, r, INSET_PX);
                    let hash = frame_hash(&cropped);
                    
                    let should_emit = {
                        let mut state = state_arc_clone.lock().unwrap();
                        if state.last_cropped.as_ref().map(frame_hash) == Some(hash) {
                            skipped += 1;
                            false
                        } else {
                            state.last_cropped = Some(cropped.clone());
                            state.last_rect = Some(r);
                            true
                        }
                    };
                    
                    if should_emit {
                        let thumbnail = create_thumbnail(&cropped);
                        let ms = start.elapsed().as_millis() as u64;
                        
                        eprintln!("[lenslate] capture found=yes rect={},{},{}x{} ms={} skipped={}", r.x, r.y, r.w, r.h, ms, skipped);
                        
                        let _ = app_handle.emit("capture://frame", CaptureEvent::Frame {
                            thumbnail_png_base64: thumbnail,
                            width: cropped.width(),
                            height: cropped.height(),
                            ms,
                            skipped,
                        });
                        skipped = 0;
                        last_emit = Instant::now();
                    }
                }
                None => {
                    skipped += 1;
                    if last_emit.elapsed() > Duration::from_secs(1) {
                        eprintln!("[lenslate] capture found=no skipped={}", skipped);
                        let _ = app_handle.emit("capture://error", CaptureEvent::Error { 
                            message: "Frame not found".into() 
                        });
                        last_emit = Instant::now();
                    }
                }
            }
        }

        let mut state = state_arc_clone.lock().unwrap();
        state.backend.stop_stream();
    });

    {
        let mut state = state_arc.lock().unwrap();
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
        let mut state = state_arc.lock().unwrap();
        state.backend.stop_stream();
    }
    
    let handle = {
        let mut state = state_arc.lock().unwrap();
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
    let state = state_arc.lock().unwrap();
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = dirs::picture_dir()
        .ok_or("No Pictures directory")?
        .join("lenslate-debug");
    
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut saved = Vec::new();
    
    if let Some(full) = &state.last_full_frame {
        let path = dir.join(format!("full-{}.png", timestamp));
        full.save(&path).map_err(|e| e.to_string())?;
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
    use base64::{Engine as _, engine::general_purpose};
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
                let mut state = state_arc.lock().unwrap();
                state.backend = create_capture_backend("frame".to_string(), app.handle());
            }
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            toggle_frame_command, 
            open_settings, 
            log_frontend_error,
            capture_once,
            live_start,
            live_stop,
            save_last_capture
        ])
        .run(tauri::generate_context!())
        .expect("error while running LensLate");
}