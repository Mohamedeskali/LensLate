use std::time::Instant;

use lenslate_lib::ocr::OcrEngine;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: cargo run --example ocr_file -- <image.png> [auto|latin|arabic]");
        std::process::exit(1);
    }
    let img_path = &args[1];
    let script_str = args.get(2).map(|s| s.as_str()).unwrap_or("auto");

    let script = lenslate_lib::ocr::Script::parse(script_str).unwrap_or_else(|e| {
        eprintln!("invalid script: {e}");
        std::process::exit(1);
    });

    let models_dir = lenslate_lib::ocr::models::default_models_dir();
    eprintln!("[ocr_file] models dir: {}", models_dir.display());

    let reporter = lenslate_lib::ocr::models::StderrReporter;
    if let Err(e) = lenslate_lib::ocr::models::ensure_models(&models_dir, &reporter) {
        eprintln!("[ocr_file] failed to load models: {e}");
        std::process::exit(1);
    }

    let mut engine = match lenslate_lib::ocr::engine::PaddleOcrEngine::load(&models_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[ocr_file] failed to create engine: {e}");
            std::process::exit(1);
        }
    };

    let img = match image::open(img_path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("[ocr_file] failed to open image: {e}");
            std::process::exit(1);
        }
    };
    let rgba = img.to_rgba8();
    eprintln!("[ocr_file] image size: {}x{}", rgba.width(), rgba.height());

    let start = Instant::now();
    let result = engine.recognize(&rgba, script);
    let elapsed = start.elapsed();

    match result {
        Ok(data) => {
            println!("--- OCR Result ({} ms) ---", elapsed.as_millis());
            println!("script: {}", data.script);
            println!("text:\n{}", data.text);
            println!("lines: {}", data.lines.len());
            println!("avg_conf: {:.2}", data.avg_conf());
        }
        Err(e) => {
            eprintln!("[ocr_file] OCR failed: {e}");
            std::process::exit(1);
        }
    }
}
