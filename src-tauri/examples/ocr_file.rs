use std::time::Instant;

use lenslate_lib::ocr::models::{self, StderrReporter};
use lenslate_lib::ocr::{OcrEngine, Script};

const USAGE: &str = "Usage: cargo run --example ocr_file -- <image.png> [auto|latin|arabic]

Models are downloaded on first use into the app data dir. Set
LENSLATE_MODELS_DIR to load them from another directory instead (no download).";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.len() > 3 || args[1] == "-h" || args[1] == "--help" {
        eprintln!("{USAGE}");
        std::process::exit(1);
    }
    let img_path = &args[1];
    let script = match args.get(2) {
        Some(s) => Script::parse(s).unwrap_or_else(|e| {
            eprintln!("invalid script: {e}\n{USAGE}");
            std::process::exit(1);
        }),
        None => Script::Auto,
    };

    let models_dir = match std::env::var_os("LENSLATE_MODELS_DIR") {
        Some(dir) => dir.into(),
        None => {
            let dir = models::default_models_dir();
            if let Err(e) = models::ensure_models(&dir, &StderrReporter) {
                eprintln!("[ocr_file] failed to get models: {e}");
                std::process::exit(1);
            }
            dir
        }
    };
    eprintln!("[ocr_file] models dir: {}", models_dir.display());

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
            for l in &data.lines {
                println!(
                    "  [{},{} {}x{}] conf={:.3} rtl={} {:?}",
                    l.rect.x, l.rect.y, l.rect.w, l.rect.h, l.conf, l.rtl, l.text
                );
            }
            println!("avg_conf: {:.2}", data.avg_conf());
        }
        Err(e) => {
            eprintln!("[ocr_file] OCR failed: {e}");
            std::process::exit(1);
        }
    }
}
