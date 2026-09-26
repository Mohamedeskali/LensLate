use crate::capture::{CaptureError, CaptureResult, ScreenCapture};
use image::RgbaImage;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::Manager;
use xcap::Monitor;

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

    fn capture_window_area(app: &tauri::AppHandle, window_label: &str) -> CaptureResult<RgbaImage> {
        let window = app
            .get_webview_window(window_label)
            .ok_or_else(|| CaptureError::XCap("Frame window not found".into()))?;

        // outer_position/inner_size are already physical pixels, like xcap's monitor space.
        let pos = window
            .outer_position()
            .map_err(|e| CaptureError::XCap(e.to_string()))?;
        let size = window
            .inner_size()
            .map_err(|e| CaptureError::XCap(e.to_string()))?;

        let monitor =
            Monitor::from_point(pos.x, pos.y).map_err(|e| CaptureError::XCap(e.to_string()))?;
        let mon_x = monitor.x().map_err(|e| CaptureError::XCap(e.to_string()))?;
        let mon_y = monitor.y().map_err(|e| CaptureError::XCap(e.to_string()))?;
        let image = monitor
            .capture_image()
            .map_err(|e| CaptureError::XCap(e.to_string()))?;

        // Crop the captured monitor image to the window area
        let x = (pos.x - mon_x).max(0) as u32;
        let y = (pos.y - mon_y).max(0) as u32;
        let cropped = image::imageops::crop_imm(&image, x, y, size.width, size.height).to_image();
        if cropped.width() == 0 || cropped.height() == 0 {
            return Err(CaptureError::NoMonitor);
        }
        Ok(cropped)
    }
}

impl ScreenCapture for XCapCapture {
    fn capture_monitor(&mut self) -> CaptureResult<RgbaImage> {
        let app = self
            .app_handle
            .as_ref()
            .ok_or_else(|| CaptureError::XCap("No app handle".into()))?;

        Self::capture_window_area(app, &self.window_label)
    }

    fn start_stream(&mut self, fps: u32, tx: Sender<RgbaImage>) -> CaptureResult<()> {
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

                // Send the whole window area; the caller locates the marker and crops.
                if let Ok(image) = Self::capture_window_area(&app_handle, &window_label) {
                    if tx.send(image).is_err() {
                        break;
                    }
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

impl XCapCapture {
    fn is_window_visible(app: &tauri::AppHandle, window_label: &str) -> bool {
        app.get_webview_window(window_label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    }
}
