//! OCR → translation latency with the free Google engine, end to end on the
//! English fixture. Needs the network and the OCR models, so it is ignored
//! by default:
//! `cargo test --release --test ocr_translate_latency -- --ignored --nocapture`

use std::path::Path;
use std::time::Instant;

use lenslate_lib::ocr::engine::PaddleOcrEngine;
use lenslate_lib::ocr::models::{self, StderrReporter};
use lenslate_lib::ocr::{OcrEngine, Script};
use lenslate_lib::translate::cache::TranslationCache;
use lenslate_lib::translate::chain::FallbackChain;
use lenslate_lib::translate::google_free::GoogleFree;
use lenslate_lib::translate::{http_client, Lang, Translator};

#[tokio::test]
#[ignore]
async fn ocr_then_google_free_latency() {
    let dir = std::env::var_os("LENSLATE_MODELS_DIR")
        .map(Into::into)
        .unwrap_or_else(models::default_models_dir);
    models::ensure_models(&dir, &StderrReporter).expect("OCR models");
    let mut engine = PaddleOcrEngine::load(&dir).expect("load OCR engine");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/english.png");
    let img = image::open(fixture).unwrap().to_rgba8();
    // Warm up the OCR session like the app does on its first capture.
    engine.recognize(&img, Script::Latin).unwrap();

    let chain = FallbackChain::new(vec![
        std::sync::Arc::new(GoogleFree::new(http_client())) as std::sync::Arc<dyn Translator>
    ]);
    let cache = TranslationCache::default();
    let to = Lang::new("ar");
    let mut totals = Vec::new();
    for round in 0..3 {
        let start = Instant::now();
        let ocr = engine.recognize(&img, Script::Latin).unwrap();
        let ocr_ms = start.elapsed().as_millis();
        // A fresh cache each round measures the network, not the cache.
        cache.clear();
        let out = chain
            .translate_cached(&cache, &ocr.text, None, &to)
            .await
            .expect("Google free translation");
        let total = start.elapsed().as_millis();
        println!(
            "round {round}: ocr={ocr_ms} ms translate={} ms total={total} ms engine={} text={:?}",
            out.translation.ms, out.translation.engine, out.translation.text
        );
        totals.push(total);
        let cached = chain
            .translate_cached(&cache, &ocr.text, None, &to)
            .await
            .unwrap();
        assert!(cached.cached);
    }
    totals.sort_unstable();
    println!("median OCR→translation: {} ms", totals[1]);
}
