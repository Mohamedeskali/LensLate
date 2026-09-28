# LensLate — Phase 3: read the text under the frame (OCR) — complete phase, parallel work

You are the lead. Split the work across parallel sub-agents as below, integrate, verify, and deliver the whole phase in one go. Do not ask me questions midway. You cannot see the screen: cover logic with automated tests and put GUI checks in the final manual list. Do not start Phase 4 (no translation yet).

## Context (done, do not redo)

Tauri 2 + React + TS + Rust, Ubuntu GNOME native Wayland. Phase 2 works: portal capture → `locate_frame` (#19E6C1 border) → `crop_inside` → hash skip → `capture://frame` event with thumbnail. Commands `capture_once`, `live_start`, `live_stop`. Run the app from GNOME Terminal only.

## Goal

Turn each new crop into text (Arabic, English, French at minimum) and show it in the translation bar, fast enough for live mode.

## Engine decision (fixed)

PaddleOCR PP-OCR models (RapidOCR ONNX exports) run with ONNX Runtime from Rust (`ort` crate, CPU): text detection (DB) + recognition (CTC). Recognition models: Latin/English and Arabic. Models are NOT committed to git: downloaded on first use into the app data dir, verified by SHA-256, cached.

## Step 1 — Lead only, first (commit)

- `src-tauri/src/ocr/mod.rs` contracts: `struct OcrLine { text: String, conf: f32, rect: Rect, rtl: bool }`, `struct OcrResult { lines: Vec<OcrLine>, text: String, ms: u64 }`, `enum Script { Auto, Latin, Arabic }`, `trait OcrEngine { fn recognize(&mut self, img: &RgbaImage, script: Script) -> Result<OcrResult>; }` (stubs).
- Events: `ocr://result { text, lines, ms, script }`, `ocr://models { state: missing|downloading|ready|error, progress }`, `ocr://error { message }`. Command `set_ocr_script(script)`.
- Small Phase 2 fixes: (a) when `locate_frame` returns None, reuse the last good rect for up to 3 frames instead of dropping the frame; (b) do not PNG-encode a thumbnail on every live frame — only on `capture_once` or at most 1 per second.

## Step 2 — Parallel workers (own files only, separate commits)

**A — Engine** (`ocr/engine.rs`): ort sessions (load once, reuse); detection pre/post-processing (resize to multiple of 32, normalize, DB threshold, box unclip, min size); crop + rotate each box; recognition (resize height 48, batch lines, CTC greedy decode with the model's dictionary). Upscale small crops ×2 before detection. Tests with fixture images.
**B — Models** (`ocr/models.rs`): model registry (URLs, sizes, SHA-256) for det + latin rec + arabic rec + dictionaries; download with progress events, resume/retry, checksum, offline error message; never block the UI.
**C — Layout & script** (`ocr/layout.rs`): group boxes into lines and paragraphs, reading order (top-to-bottom; right-to-left for Arabic lines), `Script::Auto` = run Latin rec, if a line has low confidence or Arabic-looking width, retry that line with Arabic and keep the better; convert Arabic output to logical order if the model returns visual order; clean spacing. Unit tests on synthetic boxes.
**D — Frontend** (`src/`): translation bar shows recognized text (selectable, RTL-aware per line, max ~4 lines then scroll) with a Copy button; small source-script selector Auto/Latin/Arabic (calls `set_ocr_script`); model download progress state on first use; keep the debug thumbnail behind a small toggle.
**E — Test fixtures** (`src-tauri/tests/fixtures/`): generate PNGs with a script (rendered text in English, French with accents, Arabic, mixed, small 12px, dark background/light text, light/dark) plus expected text files; used by A and C.

## Step 3 — Lead: integration

Pipeline: new crop (hash changed) → OCR on a background worker (drop stale jobs if a newer crop arrives) → emit `ocr://result`. `文` = capture + OCR once. Live = OCR only when the crop changed. Log `[lenslate] ocr ms=<n> lines=<n> chars=<n> conf=<avg> script=<s>`.
Targets: ~900×250 crop OCR < 400 ms on CPU; idle CPU near 0 when nothing changes.

## Step 4 — Verification

cargo fmt --check, cargo clippy -- -D warnings, cargo test (fixtures: accuracy ≥ 95% chars on the clean English/French fixtures, report Arabic accuracy), pnpm lint, pnpm build, pnpm tauri build --debug. All zero warnings. Commit.

## Final output (only this)

1. Short Phase report with test/accuracy numbers and timings.
2. Manual test list (max 8 steps): first-run model download, 文 over English text, over Arabic text, over French text, live while scrolling, Copy button, what `[lenslate]` lines to send.
