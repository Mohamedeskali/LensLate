use image::{Rgba, RgbaImage};
use std::sync::mpsc::Sender;
use thiserror::Error;

pub mod locate;
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

pub trait ScreenCapture: Send {
    fn capture_monitor(&mut self) -> CaptureResult<RgbaImage>;
    fn start_stream(&mut self, fps: u32, tx: Sender<RgbaImage>) -> CaptureResult<()>;
    fn stop_stream(&mut self);
    fn is_wayland(&self) -> bool;
}

pub fn locate_frame(img: &RgbaImage, hint: Option<Rect>) -> Option<Rect> {
    locate::locate_frame(img, hint)
}

pub fn crop_inside(img: &RgbaImage, rect: Rect, inset: u32) -> RgbaImage {
    locate::crop_inside(img, rect, inset)
}

pub fn frame_hash(img: &RgbaImage) -> u64 {
    locate::frame_hash(img)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
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
    fn capture_monitor(&mut self) -> CaptureResult<RgbaImage> {
        Err(CaptureError::XCap("Not initialized".into()))
    }
    fn start_stream(&mut self, _fps: u32, _tx: Sender<RgbaImage>) -> CaptureResult<()> {
        Err(CaptureError::XCap("Not initialized".into()))
    }
    fn stop_stream(&mut self) {}
    fn is_wayland(&self) -> bool {
        false
    }
}