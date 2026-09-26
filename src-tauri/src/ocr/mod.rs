use image::RgbaImage;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod engine;
pub mod layout;
pub mod models;

/// OCR error types
#[derive(Debug, Error)]
pub enum OcrError {
    #[error("Model error: {0}")]
    Model(String),
    #[error("Engine error: {0}")]
    Engine(String),
    #[error("Image error: {0}")]
    Image(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ONNX Runtime error: {0}")]
    Ort(String),
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Download error: {0}")]
    Download(String),
    #[error("Checksum mismatch: {0}")]
    ChecksumMismatch(String),
}

pub type OcrResult<T> = Result<T, OcrError>;

/// Text script for OCR recognition
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Script {
    Auto,
    Latin,
    Arabic,
}

impl Default for Script {
    fn default() -> Self {
        Script::Auto
    }
}

/// A single recognized text line with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLine {
    pub text: String,
    pub conf: f32,
    pub rect: Rect,
    pub rtl: bool,
}

/// Rectangle for text bounding box
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Full OCR result with all lines and combined text
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrResultData {
    pub lines: Vec<OcrLine>,
    pub text: String,
    pub ms: u64,
    pub script: Script,
}

/// Trait for OCR engines
pub trait OcrEngine: Send + Sync {
    /// Recognize text in an image with the given script
    fn recognize(&mut self, img: &RgbaImage, script: Script) -> OcrResult<OcrResultData>;
}

/// Events emitted by the OCR system
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OcrEvent {
    /// OCR recognition result
    Result {
        text: String,
        lines: Vec<OcrLine>,
        ms: u64,
        script: Script,
    },
    /// Model download/load state
    Models {
        state: ModelState,
        progress: Option<f32>,
    },
    /// OCR error
    Error { message: String },
}

/// Model loading state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelState {
    Missing,
    Downloading,
    Ready,
    Error,
}

/// Tauri command to set the OCR script
#[tauri::command]
pub async fn set_ocr_script(script: Script) -> Result<(), String> {
    crate::ocr::models::set_script(script).await
}