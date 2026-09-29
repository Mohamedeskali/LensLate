//! X11 / Windows / macOS capture with xcap: the monitor holding the frame
//! window is captured and cut to the window area; the caller then finds the
//! marker border inside it.

use crate::capture::multi::{match_monitor, pick_monitor, window_rect_in_image, MonitorInfo};
use crate::capture::{CaptureError, CaptureResult, MonitorFrame, Rect, ScreenCapture};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::Manager;
use xcap::Monitor;

fn xcap_err(e: impl std::fmt::Display) -> CaptureError {
    let msg = e.to_string();
    // macOS reports a missing Screen Recording permission this way.
    if msg.contains("permission") || msg.contains("Permission") || msg.contains("denied") {
        CaptureError::PermissionDenied
    } else {
        CaptureError::XCap(msg)
    }
}

fn xcap_info(m: &Monitor) -> CaptureResult<MonitorInfo> {
    Ok(MonitorInfo {
        name: m.name().map_err(xcap_err)?,
        x: m.x().map_err(xcap_err)?,
        y: m.y().map_err(xcap_err)?,
        width: m.width().map_err(xcap_err)?,
        height: m.height().map_err(xcap_err)?,
        scale: f64::from(m.scale_factor().map_err(xcap_err)?),
    })
}

fn tauri_info(m: &tauri::Monitor) -> MonitorInfo {
    MonitorInfo {
        name: m.name().cloned().unwrap_or_default(),
        x: m.position().x,
        y: m.position().y,
        width: m.size().width,
        height: m.size().height,
        scale: m.scale_factor(),
    }
}

pub struct XCapCapture {
    window_label: String,
    running: Arc<AtomicBool>,
    app_handle: Option<tauri::AppHandle>,
}

impl XCapCapture {
    pub fn new(window_label: String) -> Self {
        Self {
            window_label,
            running: Arc::new(AtomicBool::new(false)),
            app_handle: None,
        }
    }

    pub fn set_app_handle(&mut self, handle: tauri::AppHandle) {
        self.app_handle = Some(handle);
    }

    /// Capture the monitor that holds most of the frame window and cut the
    /// window area out of it.
    fn capture_window_area(
        app: &tauri::AppHandle,
        window_label: &str,
    ) -> CaptureResult<MonitorFrame> {
        let window = app
            .get_webview_window(window_label)
            .ok_or_else(|| CaptureError::XCap("Frame window not found".into()))?;

        // Tauri reports window and monitors in one physical-pixel space.
        let pos = window.outer_position().map_err(xcap_err)?;
        let size = window.outer_size().map_err(xcap_err)?;
        let win = Rect {
            x: pos.x,
            y: pos.y,
            w: size.width,
            h: size.height,
        };
        let tauri_monitors: Vec<MonitorInfo> = window
            .available_monitors()
            .map_err(xcap_err)?
            .iter()
            .map(tauri_info)
            .collect();
        let index = pick_monitor(&tauri_monitors, win).ok_or(CaptureError::NoMonitor)?;
        let target = &tauri_monitors[index];

        // xcap may use another space (logical points on macOS); match by
        // name, origin or size, and fall back to a point lookup.
        let xcap_monitors = Monitor::all().map_err(xcap_err)?;
        let infos = xcap_monitors
            .iter()
            .map(xcap_info)
            .collect::<CaptureResult<Vec<_>>>()?;
        let monitor = match match_monitor(target, &infos) {
            Some(i) => xcap_monitors[i].clone(),
            None => {
                let (cx, cy) = (win.x + win.w as i32 / 2, win.y + win.h as i32 / 2);
                Monitor::from_point(cx, cy).map_err(xcap_err)?
            }
        };
        let image = monitor.capture_image().map_err(xcap_err)?;
        let area = window_rect_in_image(win, target, image.width(), image.height())
            .ok_or(CaptureError::NoMonitor)?;
        let cropped =
            image::imageops::crop_imm(&image, area.x as u32, area.y as u32, area.w, area.h)
                .to_image();
        Ok(MonitorFrame {
            index,
            name: if target.name.is_empty() {
                format!("monitor-{index}")
            } else {
                target.name.clone()
            },
            image: cropped,
        })
    }

    fn is_window_visible(app: &tauri::AppHandle, window_label: &str) -> bool {
        app.get_webview_window(window_label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    }
}

impl ScreenCapture for XCapCapture {
    fn capture_monitors(&mut self) -> CaptureResult<Vec<MonitorFrame>> {
        let app = self
            .app_handle
            .as_ref()
            .ok_or_else(|| CaptureError::XCap("No app handle".into()))?;
        Ok(vec![Self::capture_window_area(app, &self.window_label)?])
    }

    fn start_stream(&mut self, fps: u32, tx: Sender<MonitorFrame>) -> CaptureResult<()> {
        let app = self
            .app_handle
            .as_ref()
            .ok_or_else(|| CaptureError::XCap("No app handle".into()))?;

        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let window_label = self.window_label.clone();
        let interval = Duration::from_millis(1000 / fps.max(1) as u64);
        let app_handle = app.clone();

        thread::spawn(move || {
            while running.load(Ordering::SeqCst)
                && Self::is_window_visible(&app_handle, &window_label)
            {
                let start = std::time::Instant::now();

                match Self::capture_window_area(&app_handle, &window_label) {
                    Ok(frame) => {
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                    Err(e) => eprintln!("[lenslate] capture error: {e}"),
                }

                let elapsed = start.elapsed();
                if elapsed < interval {
                    thread::sleep(interval - elapsed);
                }
            }
        });

        Ok(())
    }

    fn stop_stream(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }

    fn is_wayland(&self) -> bool {
        false
    }
}
