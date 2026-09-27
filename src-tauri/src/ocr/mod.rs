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

impl From<ort::Error> for OcrError {
    fn from(err: ort::Error) -> Self {
        OcrError::Ort(err.to_string())
    }
}

pub type OcrResult<T> = Result<T, OcrError>;

/// Text script for OCR recognition
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Script {
    #[default]
    Auto,
    Latin,
    Arabic,
}

impl Script {
    /// Parse the CLI / frontend spelling of a script.
    pub fn parse(s: &str) -> OcrResult<Script> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Script::Auto),
            "latin" | "en" | "english" => Ok(Script::Latin),
            "arabic" | "ar" => Ok(Script::Arabic),
            other => Err(OcrError::Model(format!("unknown script: {other}"))),
        }
    }
}

impl std::fmt::Display for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Script::Auto => "auto",
            Script::Latin => "latin",
            Script::Arabic => "arabic",
        };
        f.write_str(s)
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

impl Rect {
    pub fn center_x(&self) -> f32 {
        self.x as f32 + self.w as f32 / 2.0
    }

    pub fn center_y(&self) -> f32 {
        self.y as f32 + self.h as f32 / 2.0
    }
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

impl OcrResultData {
    pub fn avg_conf(&self) -> f32 {
        if self.lines.is_empty() {
            0.0
        } else {
            self.lines.iter().map(|l| l.conf).sum::<f32>() / self.lines.len() as f32
        }
    }
}

/// Trait for OCR engines
pub trait OcrEngine: Send {
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

impl OcrEvent {
    /// The Tauri event name this payload is emitted under.
    pub fn event_name(&self) -> &'static str {
        match self {
            OcrEvent::Result { .. } => "ocr://result",
            OcrEvent::Models { .. } => "ocr://models",
            OcrEvent::Error { .. } => "ocr://error",
        }
    }
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

/// Parse an incoming `Script` (serde) and store it as the global default.
///
/// Kept as a plain function (not a `#[tauri::command]`) so that it can be
/// called from tests and from the Phase 3b integration layer alike; the
/// command wrapper lives in `lib.rs`.
pub fn set_ocr_script(script: Script) {
    models::set_script(script);
}

/// Current script selection, as set by `set_ocr_script`.
pub fn ocr_script() -> Script {
    models::get_script()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_script() {
        assert_eq!(Script::parse("auto").unwrap(), Script::Auto);
        assert_eq!(Script::parse("English").unwrap(), Script::Latin);
        assert_eq!(Script::parse(" AR ").unwrap(), Script::Arabic);
        assert!(Script::parse("klingon").is_err());
    }

    #[test]
    fn test_event_names_match_payloads() {
        assert_eq!(
            OcrEvent::Error {
                message: "x".into()
            }
            .event_name(),
            "ocr://error"
        );
        assert_eq!(
            OcrEvent::Models {
                state: ModelState::Ready,
                progress: None
            }
            .event_name(),
            "ocr://models"
        );
        assert_eq!(
            OcrEvent::Result {
                text: "t".into(),
                lines: vec![],
                ms: 1,
                script: Script::Auto
            }
            .event_name(),
            "ocr://result"
        );
    }

    #[test]
    fn test_rect_center() {
        let r = Rect {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        };
        assert_eq!(r.center_x(), 25.0);
        assert_eq!(r.center_y(), 40.0);
    }
}
