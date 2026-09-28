use crate::ocr::{ModelState, OcrError, OcrResult, Script};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Upstream model repository (PaddleOCR mobile models exported to ONNX by
/// RapidOCR).
/// One downloadable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    pub name: &'static str,
    pub path: &'static str,
    pub url: &'static str,
    pub size: u64,
    /// SHA-256 of the bytes served at `url`; models are never committed to
    /// git.
    pub sha256: &'static str,
}

pub const DET_MODEL: ModelFile = ModelFile {
    name: "ch_PP-OCRv4_det_mobile.onnx",
    path: "det.onnx",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/det/ch_PP-OCRv4_det_mobile.onnx",
    size: 4_745_517,
    sha256: "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
};

/// PaddleOCR's multilingual Latin model (French, Spanish, German, Italian,
/// Portuguese, ... with accents). `en_PP-OCRv4` only knows printable ASCII.
pub const LATIN_REC_MODEL: ModelFile = ModelFile {
    name: "latin_PP-OCRv5_rec_mobile.onnx",
    path: "rec_latin.onnx",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv5/rec/latin_PP-OCRv5_rec_mobile.onnx",
    size: 7_904_513,
    sha256: "b20bd37c168a570f583afbc8cd7925603890efbcdc000a59e22c269d160b5f5a",
};

pub const ARABIC_REC_MODEL: ModelFile = ModelFile {
    name: "arabic_PP-OCRv4_rec_mobile.onnx",
    path: "rec_arabic.onnx",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/onnx/PP-OCRv4/rec/arabic_PP-OCRv4_rec_mobile.onnx",
    size: 7_685_206,
    sha256: "4a9011bef71687bb84288dc86ad2471bd5d37b717ddf672dd156f9e7a5601bac",
};

pub const LATIN_DICT: ModelFile = ModelFile {
    name: "ppocrv5_latin_dict.txt",
    path: "dict_latin.txt",
    url: "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv5/rec/latin_PP-OCRv5_rec_mobile/ppocrv5_latin_dict.txt",
    size: 1_634,
    sha256: "3c0a8a79b612653c25f765271714f71281e4e955962c153e272b7b8c1d2b13ff",
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

/// Number of entries in each upstream dictionary file. The recognizer
/// predicts `dict_len + 2` classes: the CTC blank at index 0, the dictionary
/// lines in file order, then an appended `" "` (see [`ctc_labels`]).
pub fn dict_len(file: &ModelFile) -> usize {
    match *file {
        LATIN_DICT => 502,
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
/// Shown when the model host cannot be reached at all.
pub const OFFLINE_HINT: &str =
    "Cannot download the OCR models: no internet connection. Connect once to download them (about 20 MB); they are kept for offline use";
const BACKOFF: Duration = Duration::from_millis(750);

/// True for errors that mean "no network" rather than a bad server reply.
fn is_offline(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout()
}

/// Download one artifact into `dir` with up to 3 attempts, streaming into a
/// temp file behind a size and checksum gate. `progress` receives this
/// file's completion in `0.0..=1.0`.
fn download_one(dir: &Path, model: &ModelFile, progress: impl Fn(f32)) -> OcrResult<()> {
    use std::io::{Read, Write};

    let dest = dir.join(model.path);
    let tmp = dir.join(format!("{}.part", model.path));
    let _ = std::fs::remove_file(&tmp);

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent("LensLate/0.1 (ocr models)")
        .build()
        .map_err(|e| OcrError::Download(format!("client: {e}")))?;

    let mut last_err = None;
    for attempt in 1..=RETRIES {
        let result = (|| -> OcrResult<()> {
            let mut resp = client.get(model.url).send().map_err(|e| {
                if is_offline(&e) {
                    OcrError::Download(format!("{OFFLINE_HINT} ({e})"))
                } else {
                    OcrError::Download(format!("GET {}: {e}", model.url))
                }
            })?;
            let status = resp.status();
            if !status.is_success() {
                return Err(OcrError::Download(format!(
                    "GET {} -> HTTP {status}",
                    model.url
                )));
            }
            let mut file = std::fs::File::create(&tmp)?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut done = 0u64;
            let mut reported = 0u64;
            loop {
                let n = resp
                    .read(&mut buf)
                    .map_err(|e| OcrError::Download(format!("{}: {e}", model.name)))?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
                done += n as u64;
                // report roughly every 1% of the file
                if done - reported >= model.size / 100 {
                    reported = done;
                    progress((done as f32 / model.size as f32).min(1.0));
                }
            }
            file.flush()?;
            if done != model.size {
                return Err(OcrError::Download(format!(
                    "{}: expected {} bytes, got {done}",
                    model.name, model.size
                )));
            }
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
                let _ = std::fs::remove_file(&tmp);
                eprintln!(
                    "[lenslate] ocr models {} attempt {attempt}/{RETRIES} failed: {e}",
                    model.name
                );
                last_err = Some(e);
                if attempt < RETRIES {
                    std::thread::sleep(BACKOFF * attempt);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| OcrError::Download(model.name.to_string())))
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
        reporter.report(ModelState::Missing, None, "models not downloaded yet");
    }

    let needed: Vec<&ModelFile> = ALL_MODELS
        .iter()
        .filter(|m| !verify(&dir.join(m.path), m.sha256))
        .collect();
    // progress is weighted by bytes: the dictionaries are tiny
    let total: u64 = needed.iter().map(|m| m.size).sum();
    reporter.report(ModelState::Downloading, Some(0.0), "starting");

    let mut done = 0u64;
    for model in needed {
        let inner = |p: f32| {
            let overall = (done as f32 + p * model.size as f32) / total as f32;
            reporter.report(ModelState::Downloading, Some(overall), model.name)
        };
        download_one(dir, model, inner)?;
        done += model.size;
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

/// Load a dictionary file: one label per line, in file order.
///
/// The order is the model's class order and must never change. A single
/// literal `\n` in a line is a PaddleOCR convention for a "newline" label;
/// it is unescaped here rather than dropped.
pub fn load_dictionary(path: &Path) -> OcrResult<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| OcrError::Model(format!("dictionary {}: {e}", path.display())))?;
    Ok(text.lines().map(|line| line.replace("\\n", "\n")).collect())
}

/// CTC class labels in PaddleOCR order: index 0 is the blank (empty label),
/// then the dictionary in file order, then `" "` (PaddleOCR `use_space_char`).
/// Class `i > 0` decodes to `dict[i - 1]`; there are `dict.len() + 2` classes.
pub fn ctc_labels(dict: Vec<String>) -> Vec<String> {
    let mut labels = Vec::with_capacity(dict.len() + 2);
    labels.push(String::new());
    labels.extend(dict);
    labels.push(" ".to_string());
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dictionary_keeps_file_order() {
        let dir = std::env::temp_dir().join(format!("lenslate-dict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Unsorted, with a space entry like some upstream dictionaries.
        let p = dir.join("d.txt");
        std::fs::write(&p, "z\na\nM\n0\n \n").unwrap();
        let dict = load_dictionary(&p).unwrap();
        assert_eq!(dict, ["z", "a", "M", "0", " "]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_ctc_labels_layout() {
        let dict: Vec<String> = (0..161).map(|i| format!("c{i}")).collect();
        let labels = ctc_labels(dict);
        assert_eq!(labels.len(), 161 + 2);
        assert_eq!(labels[0], "", "blank must be index 0");
        assert_eq!(labels[1], "c0", "class i decodes to dict[i - 1]");
        assert_eq!(labels[161], "c160");
        assert_eq!(labels[162], " ", "space is appended last");
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
