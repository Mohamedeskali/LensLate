//! End-to-end OCR accuracy on the rendered fixtures in `tests/fixtures/`.
//!
//! Each `<name>.png` with a `<name>.txt` next to it is recognized and scored
//! by character accuracy (`1 - edit distance / expected length`). Latin-script
//! fixtures must reach 95%, Arabic 90%.
//!
//! Models are downloaded into the app data dir on first run. Set
//! `LENSLATE_MODELS_DIR` to load them from another directory (no download).
//! Print the table with `cargo test --release --test ocr_fixtures -- --nocapture`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use lenslate_lib::ocr::engine::PaddleOcrEngine;
use lenslate_lib::ocr::models::{self, StderrReporter};
use lenslate_lib::ocr::{OcrEngine, Script};

const LATIN_MIN: f64 = 0.95;
const ARABIC_MIN: f64 = 0.90;

/// (fixture, script, minimum accuracy)
const FIXTURES: &[(&str, Script, f64)] = &[
    ("english", Script::Latin, LATIN_MIN),
    ("french", Script::Latin, LATIN_MIN),
    ("light_on_dark", Script::Latin, LATIN_MIN),
    ("small_text", Script::Latin, LATIN_MIN),
    ("mixed", Script::Latin, LATIN_MIN),
    ("arabic", Script::Arabic, ARABIC_MIN),
];

fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LENSLATE_MODELS_DIR") {
        return dir.into();
    }
    let dir = models::default_models_dir();
    if let Err(e) = models::ensure_models(&dir, &StderrReporter) {
        panic!(
            "OCR models are missing and could not be downloaded into {}: {e}\n\
             Connect to the internet once, or set LENSLATE_MODELS_DIR.",
            dir.display()
        );
    }
    dir
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Character accuracy of `got` against `expected`, in `0.0..=1.0`.
fn char_accuracy(got: &str, expected: &str) -> f64 {
    let got: Vec<char> = got.chars().collect();
    let expected: Vec<char> = expected.chars().collect();
    let dist = levenshtein(&got, &expected);
    (1.0 - dist as f64 / expected.len().max(1) as f64).max(0.0)
}

/// Trim each line and drop the trailing newline of the fixture file.
fn normalize(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_levenshtein() {
    let c = |s: &str| s.chars().collect::<Vec<_>>();
    assert_eq!(levenshtein(&c("kitten"), &c("sitting")), 3);
    assert_eq!(levenshtein(&c(""), &c("abc")), 3);
    assert_eq!(levenshtein(&c("élève"), &c("eleve")), 2);
    assert_eq!(char_accuracy("Helo", "Hello"), 0.8);
}

#[test]
fn fixtures_reach_accuracy() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut engine = PaddleOcrEngine::load(&models_dir()).expect("load OCR engine");

    let mut failures = Vec::new();
    println!("| fixture | script | got | accuracy | ms |");
    println!("|---|---|---|---|---|");
    for &(name, script, min_accuracy) in FIXTURES {
        let img = image::open(fixtures.join(format!("{name}.png")))
            .unwrap_or_else(|e| panic!("{name}.png: {e}"))
            .to_rgba8();
        let expected = std::fs::read_to_string(fixtures.join(format!("{name}.txt")))
            .unwrap_or_else(|e| panic!("{name}.txt: {e}"));
        let expected = normalize(&expected);

        let start = Instant::now();
        let result = engine
            .recognize(&img, script)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let ms = start.elapsed().as_millis();
        let got = normalize(&result.text);
        let acc = char_accuracy(&got, &expected);
        println!(
            "| {name} | {script} | {} | {:.1}% | {ms} |",
            got.replace('\n', " / "),
            acc * 100.0
        );
        if acc < min_accuracy {
            failures.push(format!(
                "{name}: {:.1}% < {:.0}%\n  got:      {got:?}\n  expected: {expected:?}",
                acc * 100.0,
                min_accuracy * 100.0
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
