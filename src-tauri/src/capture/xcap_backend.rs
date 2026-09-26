use crate::capture::{CaptureError, CaptureResult, ScreenCapture, INSET_PX};
use image::{RgbaImage};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;
use tauri::Manager;
use xcap::{Monitor};

pub struct XCapCapture {
    window_label: String,
    running: bool,
    app_handle: Option<tauri::AppHandle>,
}

impl XCapCapture {
    pub fn new(window_label: String) -> Self {
        Self {
            window_label,
            running: false,
            app_handle: None,
        }
    }

    pub fn set_app_handle(&mut self, handle: tauri::AppHandle) {
        self.app_handle = Some(handle);
    }

    fn capture_window_area(app: &tauri::AppHandle, window_label: &str) -> CaptureResult<RgbaImage> {
        let window = app
            .get_webview_window(window_label)
            .ok_or_else(|| CaptureError::XCap("Frame window not found".into()))?;

        let outer_pos = window.outer_position().map_err(|e| CaptureError::XCap(e.to_string()))?;
        let size = window.inner_size().map_err(|e| CaptureError::XCap(e.to_string()))?;
        let scale_factor = window.scale_factor().map_err(|e| CaptureError::XCap(e.to_string()))? as f32;

        let x = (outer_pos.x as f32 * scale_factor) as i32;
        let y = (outer_pos.y as f32 * scale_factor) as i32;
        let w = (size.width as f32 * scale_factor) as u32;
        let h = (size.height as f32 * scale_factor) as u32;

        let monitors = Monitor::all().map_err(|e| CaptureError::XCap(e.to_string()))?;
        let monitor = monitors
            .into_iter()
            .next()
            .ok_or(CaptureError::NoMonitor)?;

        let image = monitor
            .capture_image()
            .map_err(|e| CaptureError::XCap(e.to_string()))?;

        // Crop the captured monitor image to the window area
        let cropped = image::imageops::crop_imm(&image, x as u32, y as u32, w, h).to_image();
        Ok(cropped)
    }
}

impl ScreenCapture for XCapCapture {
    fn capture_monitor(&mut self) -> CaptureResult<RgbaImage> {
        let app = self.app_handle.as_ref()
            .ok_or_else(|| CaptureError::XCap("No app handle".into()))?;

        Self::capture_window_area(app, &self.window_label)
    }

    fn start_stream(&mut self, fps: u32, tx: Sender<RgbaImage>) -> CaptureResult<()> {
        let app = self.app_handle.as_ref()
            .ok_or_else(|| CaptureError::XCap("No app handle".into()))?;

        self.running = true;
        let window_label = self.window_label.clone();
        let interval = Duration::from_millis(1000 / fps.max(1) as u64);
        let app_handle = app.clone();

        thread::spawn(move || {
            while Self::is_window_visible(&app_handle, &window_label) {
                let start = std::time::Instant::now();

                let monitors = match Monitor::all() {
                    Ok(m) => m,
                    Err(_) => {
                        thread::sleep(interval);
                        continue;
                    }
                };
                let monitor = match monitors.into_iter().next() {
                    Some(m) => m,
                    None => {
                        thread::sleep(interval);
                        continue;
                    }
                };

                let window = match app_handle.get_webview_window(&window_label) {
                    Some(w) => w,
                    None => {
                        thread::sleep(interval);
                        continue;
                    }
                };

                let outer_pos = match window.outer_position() {
                    Ok(p) => p,
                    Err(_) => {
                        thread::sleep(interval);
                        continue;
                    }
                };
                let size = match window.inner_size() {
                    Ok(s) => s,
                    Err(_) => {
                        thread::sleep(interval);
                        continue;
                    }
                };
                let scale_factor = match window.scale_factor() {
                    Ok(s) => s as f32,
                    Err(_) => {
                        thread::sleep(interval);
                        continue;
                    }
                };

                let x = (outer_pos.x as f32 * scale_factor) as i32;
                let y = (outer_pos.y as f32 * scale_factor) as i32;
                let w = (size.width as f32 * scale_factor) as u32;
                let h = (size.height as f32 * scale_factor) as u32;

                let image = match monitor.capture_image() {
                    Ok(img) => img,
                    Err(_) => {
                        thread::sleep(interval);
                        continue;
                    }
                };

                let cropped = image::imageops::crop_imm(&image, x as u32, y as u32, w, h).to_image();
                let rgba = cropped;
                let rect = crate::capture::locate::locate_frame(&rgba, None);
                
                if let Some(r) = rect {
                    let cropped = crate::capture::locate::crop_inside(&rgba, r, INSET_PX);
                    let _ = tx.send(cropped);
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
        self.running = false;
    }

    fn is_wayland(&self) -> bool {
        false
    }
}

impl XCapCapture {
    fn is_window_visible(app: &tauri::AppHandle, window_label: &str) -> bool {
        app.get_webview_window(window_label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    }
}