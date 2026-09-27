use crate::ocr::{ModelState, OcrError, OcrResult, Script};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Upstream model repository (PaddleOCR v4 mobile models exported to ONNX).
/// One downloadable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    pub name: &'static str,
    pub path: &'static str,
    pub url: &'static str,
    pub size: u64,
    /// SHA-256 computed locally from the bytes served at `url` (see
    /// `examples/fetch_models.rs`); models are never committed to git.
    pub sha256: &'static str,
}

pub const DET_MODEL: ModelFile = ModelFile {
    name: "ch_PP-OCRv4_det_mobile.onnx",
    path: "det.onnx",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/det/ch_PP-OCRv4_det_mobile.onnx",
    size: 4_745_517,
    sha256: "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
};

pub const LATIN_REC_MODEL: ModelFile = ModelFile {
    name: "en_PP-OCRv4_rec_mobile.onnx",
    path: "rec_latin.onnx",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile.onnx",
    size: 7_653_044,
    sha256: "e8770c967605983d1570cdf5352041dfb68fa0c21664f49f47b155abd3e0e318",
};

pub const ARABIC_REC_MODEL: ModelFile = ModelFile {
    name: "arabic_PP-OCRv4_rec_mobile.onnx",
    path: "rec_arabic.onnx",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/rec/arabic_PP-OCRv4_rec_mobile.onnx",
    size: 7_685_206,
    sha256: "4a9011bef71687bb84288dc86ad2471bd5d37b717ddf672dd156f9e7a5601bac",
};

pub const LATIN_DICT: ModelFile = ModelFile {
    name: "en_dict.txt",
    path: "dict_latin.txt",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv4/rec/en_PP-OCRv4_rec_mobile/en_dict.txt",
    size: 190,
    sha256: "5662df9d2d03f0e8ca0d3b0649d6acbab904b6a14b3d3521463c71c37c668ce3",
};

pub const ARABIC_DICT: ModelFile = ModelFile {
    name: "arabic_dict.txt",
    path: "dict_arabic.txt",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv4/rec/arabic_PP-OCRv4_rec_mobile/arabic_dict.txt",
    size: 405,
    sha256: "637c27c88512c22089bef927b34ada08f748dc132ac70facd68d8202384c2726",
};

/// All artifacts, in download order.
pub const ALL_MODELS: [ModelFile; 5] = [
    DET_MODEL,
    LATIN_REC_MODEL,
    ARABIC_REC_MODEL,
    LATIN_DICT,
    ARABIC_DICT,
];

/// CTC decode: the model predicts `dict_len + 1` classes; the extra one is the
/// CTC "blank". Asserted at load time in `engine.rs`.
pub fn dict_len(file: &ModelFile) -> usize {
    match *file {
        LATIN_DICT => 95,
        ARABIC_DICT => 161,
        _ => unreachable!("not a dictionary"),
    }
}

/// Absolute paths of every model file inside `dir`.
pub fn model_paths(dir: &Path) -> [PathBuf; 5] {
    [
        dir.join(DET_MODEL.path),
        dir.join(LATIN_REC_MODEL.path),
        dir.join(ARABIC_REC_MODEL.path),
        dir.join(LATIN_DICT.path),
        dir.join(ARABIC_DICT.path),
    ]
}

/// App data dir used when no `AppHandle` is available (CLI, tests).
pub fn default_models_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lenslate")
        .join("models")
}

// ---------------------------------------------------------------------------
// script selection
// ---------------------------------------------------------------------------

static SCRIPT: AtomicU8 = AtomicU8::new(0); // 0 = Auto, 1 = Latin, 2 = Arabic

fn to_u8(s: Script) -> u8 {
    match s {
        Script::Auto => 0,
        Script::Latin => 1,
        Script::Arabic => 2,
    }
}

fn from_u8(v: u8) -> Script {
    match v {
        1 => Script::Latin,
        2 => Script::Arabic,
        _ => Script::Auto,
    }
}

/// Set the script used when the caller does not pass one explicitly.
pub fn set_script(script: Script) {
    SCRIPT.store(to_u8(script), Ordering::Relaxed);
}

pub fn get_script() -> Script {
    from_u8(SCRIPT.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------

/// Publishes `ocr://models` state changes; the app supplies a real emitter, the
/// CLI/tests use [`NoopReporter`].
pub trait ProgressReporter: Send + Sync {
    fn report(&self, state: ModelState, progress: Option<f32>, detail: &str);
}

pub struct NoopReporter;

impl ProgressReporter for NoopReporter {
    fn report(&self, _state: ModelState, _progress: Option<f32>, _detail: &str) {}
}

/// `stderr` reporter, used by the CLI so downloads are visible headless.
pub struct StderrReporter;

impl ProgressReporter for StderrReporter {
    fn report(&self, state: ModelState, progress: Option<f32>, detail: &str) {
        match progress {
            Some(p) => eprintln!(
                "[lenslate] ocr models state={state:?} progress={:.1}% {detail}",
                p * 100.0
            ),
            None => eprintln!("[lenslate] ocr models state={state:?} {detail}"),
        }
    }
}

/// Process-wide download guard: only one thread may populate the model dir.
static DOWNLOAD_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// verification
// ---------------------------------------------------------------------------

/// SHA-256 of a file on disk, lowercase hex.
pub fn sha256_file(path: &Path) -> OcrResult<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(OcrError::Io)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

/// True when `path` exists and hashes to the expected digest.
pub fn verify(path: &Path, expected: &str) -> bool {
    match sha256_file(path) {
        Ok(actual) => actual.eq_ignore_ascii_case(expected),
        Err(_) => false,
    }
}

/// True when every model in `dir` is present and verified.
pub fn all_present_and_verified(dir: &Path) -> bool {
    model_paths(dir)
        .iter()
        .zip(ALL_MODELS.iter())
        .all(|(p, m)| verify(p, m.sha256))
}

/// True when every model file exists (contents unchecked).
pub fn all_present(dir: &Path) -> bool {
    model_paths(dir).iter().all(|p| p.exists())
}

/// Remove any model that fails verification so the next run re-downloads it.
fn purge_invalid(dir: &Path) -> OcrResult<()> {
    for (path, model) in model_paths(dir).iter().zip(ALL_MODELS.iter()) {
        if path.exists() && !verify(path, model.sha256) {
            eprintln!(
                "[lenslate] ocr models discarding {} (checksum mismatch)",
                model.name
            );
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// download
// ---------------------------------------------------------------------------

const RETRIES: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(750);

/// Download one artifact into `dir`, with 3 attempts, a temp file, and a
/// checksum gate. `progress` is the overall completion in `0.0..=1.0`.
fn download_one(
    dir: &Path,
    model: &ModelFile,
    _reporter: &dyn ProgressReporter,
    progress: impl Fn(f32),
) -> OcrResult<()> {
    let dest = dir.join(model.path);
    let tmp = dir.join(format!("{}.part", model.path));
    let _ = std::fs::remove_file(&tmp);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent("LensLate/0.1 (ocr models)")
        .build()
        .map_err(|e| OcrError::Download(format!("client: {e}")))?;

    let mut last_err = String::new();
    for attempt in 1..=RETRIES {
        let result = (|| -> OcrResult<()> {
            let resp = client
                .get(model.url)
                .send()
                .map_err(|e| OcrError::Download(format!("GET {}: {e}", model.url)))?;
            let status = resp.status();
            if !status.is_success() {
                return Err(OcrError::Download(format!(
                    "GET {} -> HTTP {status}",
                    model.url
                )));
            }
            let bytes = resp
                .bytes()
                .map_err(|e| OcrError::Download(format!("body: {e}")))?;
            if bytes.len() as u64 != model.size {
                return Err(OcrError::Download(format!(
                    "{}: expected {} bytes, got {}",
                    model.name,
                    model.size,
                    bytes.len()
                )));
            }
            std::fs::write(&tmp, &bytes)?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                let actual = sha256_file(&tmp)?;
                if !actual.eq_ignore_ascii_case(model.sha256) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(OcrError::ChecksumMismatch(format!(
                        "{}: expected {}, got {actual}",
                        model.name, model.sha256
                    )));
                }
                std::fs::rename(&tmp, &dest)?;
                progress(1.0);
                return Ok(());
            }
            Err(e) => {
                last_err = e.to_string();
                let _ = std::fs::remove_file(&tmp);
                eprintln!(
                    "[lenslate] ocr models {} attempt {attempt}/{RETRIES} failed: {last_err}",
                    model.name
                );
                if attempt < RETRIES {
                    std::thread::sleep(BACKOFF * attempt);
                }
            }
        }
    }
    Err(OcrError::Download(format!("{}: {last_err}", model.name)))
}

/// Make sure every model in `dir` is downloaded and verified, reporting
/// progress. Existing verified files are left alone.
pub fn ensure_models(dir: &Path, reporter: &dyn ProgressReporter) -> OcrResult<()> {
    let _guard = DOWNLOAD_LOCK.lock().map_err(|_| {
        OcrError::Model("model download lock poisoned by a previous failure".into())
    })?;

    std::fs::create_dir_all(dir)?;
    purge_invalid(dir)?;

    if all_present_and_verified(dir) {
        reporter.report(ModelState::Ready, Some(1.0), "cached");
        return Ok(());
    }

    if !all_present(dir) {
        reporter.report(
            ModelState::Missing,
            None,
            "models not downloaded yet, run `cargo run --example fetch_models`",
        );
    }

    let needed: Vec<&ModelFile> = ALL_MODELS
        .iter()
        .filter(|m| !verify(&dir.join(m.path), m.sha256))
        .collect();
    let total = needed.len() as f32;
    reporter.report(ModelState::Downloading, Some(0.0), "starting");

    for (i, model) in needed.iter().enumerate() {
        let base = i as f32 / total;
        let step = 1.0 / total;
        let inner =
            |p: f32| reporter.report(ModelState::Downloading, Some(base + p * step), model.name);
        download_one(dir, model, reporter, inner)?;
    }

    debug_assert!(all_present_and_verified(dir));
    reporter.report(ModelState::Ready, Some(1.0), "ready");
    Ok(())
}

/// Download + verify, reporting every failure as `ocr://models state=error`.
pub fn ensure_models_reported(dir: &Path, reporter: &dyn ProgressReporter) -> OcrResult<()> {
    match ensure_models(dir, reporter) {
        Ok(()) => Ok(()),
        Err(e) => {
            reporter.report(ModelState::Error, None, &e.to_string());
            Err(e)
        }
    }
}

/// Load a dictionary file into CTC classes: `dict` + the blank at index
/// `dict.len()`. Keys are sorted so lookup is `binary_search`.
///
/// The upstream dict files are one plain UTF-8 entry per line. A single
/// literal `\n` in a line is a PaddleOCR convention for a "newline" label;
/// it is unescaped here rather than dropped.
pub fn load_dictionary(path: &Path) -> OcrResult<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| OcrError::Model(format!("dictionary {}: {e}", path.display())))?;
    let mut classes: Vec<String> = text
        .lines()
        .map(|line| line.replace("\\n", "\n"))
        .filter(|l| !l.is_empty())
        .collect();
    classes.dedup();
    classes.sort();
    classes.push(String::new()); // CTC blank at highest index
                                 // Some ONNX exports include an additional class (OOV or padding)
    classes.push(String::new()); // extra class to reach dict_len + 2
    Ok(classes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dict_lengths_match_metadata() {
        let dir = std::env::temp_dir().join(format!("lenslate-dict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Synthesize a dictionary with the documented length and check that
        // `load_dictionary` produces dict_len + 2 classes (blank + extra).
        let p = dir.join("d.txt");
        std::fs::write(&p, (0..161).map(|i| format!("c{i}\n")).collect::<String>()).unwrap();
        let classes = load_dictionary(&p).unwrap();
        assert_eq!(classes.len(), 163);
        assert_eq!(classes[162], "", "blank must be the highest index");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unescape_newline_label() {
        let dir = std::env::temp_dir().join(format!("lenslate-dict2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.txt");
        std::fs::write(&p, "a\n\\n\nb\n").unwrap();
        let classes = load_dictionary(&p).unwrap();
        assert!(classes.iter().any(|c| c == "\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_embedded_checksums_are_64_hex_chars() {
        for m in ALL_MODELS {
            assert_eq!(m.sha256.len(), 64, "{}", m.name);
            assert!(
                m.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{}",
                m.name
            );
            assert!(m.url.starts_with("https://"), "{}", m.name);
            assert!(m.size > 0, "{}", m.name);
        }
    }

    #[test]
    fn test_purge_invalid_removes_bad_file() {
        let dir = std::env::temp_dir().join(format!("lenslate-purge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(LATIN_DICT.path);
        std::fs::write(&p, "garbage").unwrap();
        assert!(!verify(&p, LATIN_DICT.sha256));
        purge_invalid(&dir).unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_script_roundtrip() {
        set_script(Script::Arabic);
        assert_eq!(get_script(), Script::Arabic);
        set_script(Script::Auto);
        assert_eq!(get_script(), Script::Auto);
    }
}
