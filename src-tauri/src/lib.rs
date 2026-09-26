use std::sync::atomic::{AtomicBool, Ordering};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

static FRAME_VISIBLE: AtomicBool = AtomicBool::new(false);

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
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &settings, &history, &quit])?;
    TrayIconBuilder::new()
        .icon(Image::from_bytes(include_bytes!("../icons/icon.png"))?)
        .menu(&menu)
        .tooltip("LensLate")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => toggle_frame(app),
            "settings" => show_utility(app, "settings", "LensLate Settings"),
            "history" => show_utility(app, "history", "LensLate History"),
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
        // Must stay first: a second instance forwards its argv over D-Bus and exits
        // during plugin init, before any window is created.
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![toggle_frame_command, open_settings, log_frontend_error])
        .run(tauri::generate_context!())
        .expect("error while running LensLate");
}
