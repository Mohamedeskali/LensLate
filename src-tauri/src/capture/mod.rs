use image::{Rgba, RgbaImage};
use std::sync::mpsc::Sender;
use thiserror::Error;

pub mod locate;
pub mod multi;
pub mod overlay;
#[cfg(target_os = "linux")]
pub mod wayland;
pub mod xcap_backend;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

pub const MARKER_RGB: Rgba<u8> = Rgba([0x19, 0xE6, 0xC1, 0xFF]);
pub const MARKER_BORDER_PX: u32 = 2;
pub const INSET_PX: u32 = 3;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum CaptureError {
    #[error("Portal error: {0}")]
    Portal(String),
    #[error("PipeWire error: {0}")]
    PipeWire(String),
    #[error("Permission denied")]
    PermissionDenied,
    #[error("No suitable monitor found")]
    NoMonitor,
    #[error("Frame not found")]
    FrameNotFound,
    #[error("xcap error: {0}")]
    XCap(String),
    #[error("Image error: {0}")]
    Image(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type CaptureResult<T> = Result<T, CaptureError>;

pub use multi::{Located, MonitorFrame};

pub trait ScreenCapture: Send {
    /// One picture of every monitor the backend can see (the portal: every
    /// shared monitor; xcap: the monitor holding the frame window).
    fn capture_monitors(&mut self) -> CaptureResult<Vec<MonitorFrame>>;
    /// Stream pictures at about `fps` per monitor into `tx`.
    fn start_stream(&mut self, fps: u32, tx: Sender<MonitorFrame>) -> CaptureResult<()>;
    fn stop_stream(&mut self);
    /// Release everything (stream, portal session) before the app quits.
    fn shutdown(&mut self) {
        self.stop_stream();
    }
    /// Forget the chosen screens so the next capture asks again (portal).
    fn reselect(&mut self) {}
    /// Number of monitors being captured, when the backend is limited to a
    /// user selection (the portal); `None` when it follows the window.
    fn shared_monitors(&self) -> Option<usize> {
        None
    }
    fn is_wayland(&self) -> bool;
}

pub fn crop_inside(img: &RgbaImage, rect: Rect, inset: u32) -> RgbaImage {
    locate::crop_inside(img, rect, inset)
}

pub fn frame_hash(img: &RgbaImage) -> u64 {
    locate::frame_hash(img)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum CaptureEvent {
    Frame {
        thumbnail_png_base64: String,
        width: u32,
        height: u32,
        ms: u64,
        skipped: u64,
    },
    Error {
        message: String,
    },
}

// No-op capture backend for initialization before app handle is available
pub struct NoOpCapture;

impl ScreenCapture for NoOpCapture {
    fn capture_monitors(&mut self) -> CaptureResult<Vec<MonitorFrame>> {
        Err(CaptureError::XCap("Not initialized".into()))
    }
    fn start_stream(&mut self, _fps: u32, _tx: Sender<MonitorFrame>) -> CaptureResult<()> {
        Err(CaptureError::XCap("Not initialized".into()))
    }
    fn stop_stream(&mut self) {}
    fn is_wayland(&self) -> bool {
        false
    }
}
