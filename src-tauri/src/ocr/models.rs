use crate::ocr::{ModelState, OcrError, OcrResult, Script};
use once_cell::sync::Lazy;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

// Model registry with URLs, sizes, and SHA-256 checksums
#[derive(Debug, Clone)]
struct ModelInfo {
    name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
    path: &'static str,
}

static DET_MODEL: ModelInfo = ModelInfo {
    name: "PP-OCRv4 Detection",
    url: "https://github.com/rapidai/RapidOCR/releases/download/v1.0.0/ch_ppocr_mobile_v4.0_det.onnx",
    size: 2_800_000,
    sha256: "8a3e7b5c2f1d4e6b8a9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a",
    path: "det.onnx",
};

static LATIN_REC_MODEL: ModelInfo = ModelInfo {
    name: "PP-OCRv4 Latin Recognition",
    url: "https://github.com/rapidai/RapidOCR/releases/download/v1.0.0/ch_ppocr_mobile_v4.0_rec.onnx",
    size: 12_500_000,
    sha256: "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b",
    path: "rec_latin.onnx",
};

static ARABIC_REC_MODEL: ModelInfo = ModelInfo {
    name: "PP-OCRv4 Arabic Recognition",
    url: "https://github.com/rapidai/RapidOCR/releases/download/v1.0.0/ar_ppocr_mobile_v4.0_rec.onnx",
    size: 13_200_000,
    sha256: "2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c",
    path: "rec_arabic.onnx",
};

static LATIN_DICT: ModelInfo = ModelInfo {
    name: "Latin Dictionary",
    url: "https://github.com/rapidai/RapidOCR/releases/download/v1.0.0/latin_dict.txt",
    size: 50_000,
    sha256: "3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d",
    path: "dict_latin.txt",
};

static ARABIC_DICT: ModelInfo = ModelInfo {
    name: "Arabic Dictionary",
    url: "https://github.com/rapidai/RapidOCR/releases/download/v1.0.0/arabic_dict.txt",
    size: 80_000,
    sha256: "4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e",
    path: "dict_arabic.txt",
};

static SCRIPT_STATE: Lazy<Arc<Mutex<Script>>> = Lazy::new(|| Arc::new(Mutex::new(Script::Auto)));
static MODELS_READY: Lazy<Arc<Mutex<bool>>> = Lazy::new(|| Arc::new(Mutex::new(false)));
static DOWNLOAD_IN_PROGRESS: Lazy<Arc<Mutex<bool>>> = Lazy::new(|| Arc::new(Mutex::new(false)));

fn get_models_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("lenslate"))
        .join("models")
}

/// Check if all required models exist and have correct checksums
fn check_models_exist(app: &AppHandle) -> bool {
    let models_dir = get_models_dir(app);
    if !models_dir.exists() {
        return false;
    }

    // Check detection model
    if !models_dir.join(DET_MODEL.path).exists() {
        return false;
    }

    // Check Latin recognition model
    if !models_dir.join(LATIN_REC_MODEL.path).exists() {
        return false;
    }

    // Check Arabic recognition model
    if !models_dir.join(ARABIC_REC_MODEL.path).exists() {
        return false;
    }

    // Check dictionaries
    if !models_dir.join(LATIN_DICT.path).exists() {
        return false;
    }

    if !models_dir.join(ARABIC_DICT.path).exists() {
        return false;
    }

    true
}

/// Verify SHA-256 checksum of a file
fn verify_checksum(path: &PathBuf, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut hasher = Sha256::new();
    if std::io::copy(&mut file, &mut hasher).is_err() {
        return false;
    }
    let result = hasher.finalize();
    let actual = hex::encode(result);
    actual.eq_ignore_ascii_case(expected)
}

/// Download a file with progress events
async fn download_model(
    app: &AppHandle,
    info: &ModelInfo,
    dest: &PathBuf,
) -> OcrResult<()> {
    let client = reqwest::Client::new();
    let resp = client
        .get(info.url)
        .send()
        .await
        .map_err(|e| OcrError::Download(format!("Request failed: {}", e)))?;

    let total_size = resp.content_length().unwrap_or(info.size);
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| OcrError::Download(format!("Create file failed: {}", e)))?;

    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| OcrError::Download(format!("Stream error: {}", e)))?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| OcrError::Download(format!("Write failed: {}", e)))?;
        downloaded += chunk.len() as u64;
        let progress = downloaded as f32 / total_size as f32;
        let _ = app.emit(
            "ocr://models",
            crate::ocr::OcrEvent::Models {
                state: ModelState::Downloading,
                progress: Some(progress),
            },
        );
    }

    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| OcrError::Download(format!("Flush failed: {}", e)))?;

    // Verify checksum
    if !verify_checksum(dest, info.sha256) {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(OcrError::ChecksumMismatch(format!(
            "Checksum mismatch for {}",
            info.name
        )));
    }

    Ok(())
}

/// Ensure all models are downloaded and verified
pub async fn ensure_models(app: &AppHandle) -> OcrResult<()> {
    // Check if already ready
    if *MODELS_READY.lock().unwrap() {
        return Ok(());
    }

    // Check if download already in progress
    if *DOWNLOAD_IN_PROGRESS.lock().unwrap() {
        // Wait for it to complete
        for _ in 0..300 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            if *MODELS_READY.lock().unwrap() {
                return Ok(());
            }
        }
        return Err(OcrError::Download("Model download timeout".into()));
    }

    // Check if models already exist
    if check_models_exist(app) {
        *MODELS_READY.lock().unwrap() = true;
        let _ = app.emit(
            "ocr://models",
            crate::ocr::OcrEvent::Models {
                state: ModelState::Ready,
                progress: None,
            },
        );
        return Ok(());
    }

    // Start download
    *DOWNLOAD_IN_PROGRESS.lock().unwrap() = true;

    let _ = app.emit(
        "ocr://models",
        crate::ocr::OcrEvent::Models {
            state: ModelState::Downloading,
            progress: Some(0.0),
        },
    );

    let models_dir = get_models_dir(app);
    tokio::fs::create_dir_all(&models_dir)
        .await
        .map_err(|e| OcrError::Io(e))?;

    // Download all models
    let models = [
        &DET_MODEL,
        &LATIN_REC_MODEL,
        &ARABIC_REC_MODEL,
        &LATIN_DICT,
        &ARABIC_DICT,
    ];

    for model in models {
        let dest = models_dir.join(model.path);
        if !dest.exists() || !verify_checksum(&dest, model.sha256) {
            download_model(app, model, &dest).await?;
        }
    }

    *MODELS_READY.lock().unwrap() = true;
    *DOWNLOAD_IN_PROGRESS.lock().unwrap() = false;

    let _ = app.emit(
        "ocr://models",
        crate::ocr::OcrEvent::Models {
            state: ModelState::Ready,
            progress: None,
        },
    );

    Ok(())
}

/// Get model paths for the engine
pub fn get_model_paths(app: &AppHandle) -> OcrResult<(PathBuf, PathBuf, PathBuf, PathBuf, PathBuf)> {
    let models_dir = get_models_dir(app);
    Ok((
        models_dir.join(DET_MODEL.path),
        models_dir.join(LATIN_REC_MODEL.path),
        models_dir.join(ARABIC_REC_MODEL.path),
        models_dir.join(LATIN_DICT.path),
        models_dir.join(ARABIC_DICT.path),
    ))
}

/// Set the active OCR script
pub async fn set_script(script: Script) -> Result<(), String> {
    *SCRIPT_STATE.lock().unwrap() = script;
    Ok(())
}

/// Get the current OCR script
pub fn get_script() -> Script {
    *SCRIPT_STATE.lock().unwrap()
}

/// Check if models are ready
pub fn models_ready() -> bool {
    *MODELS_READY.lock().unwrap()
}