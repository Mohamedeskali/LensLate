# LensLate — Phase 2: capture what is under the frame (complete phase, parallel work)

You are the lead of this phase. Split the work across parallel sub-agents as described below, manage them, integrate their work, and deliver the whole phase in one go. Do not ask me questions in the middle. You cannot see the screen: never stop because GUI behaviour is unverified. Instead, cover logic with automated tests and list GUI checks in the final manual test list. Do not start Phase 3.

## Context (already done, do not redo)

- Tauri 2 + React + TS + Rust app, running on Ubuntu GNOME **native Wayland** (forced XWayland breaks drag/resize; keep native).
- Tray, frame window (drag/resize OK, translation bar inside the same window), `lenslate --toggle` bound to Ctrl+Alt+Y via GNOME custom shortcut.
- On GNOME Wayland the app cannot know its global position and cannot keep itself on top; the user sets Alt+Space → "Always on Top" manually for now (a GNOME extension comes in Phase 7).
- Always run the app from GNOME Terminal, never VS Code's terminal (snap env breaks GTK).

## Goal of the phase

Capture the pixels inside the frame (never the border/toolbar), once on demand and live, and show a small debug thumbnail in the translation bar. Approach on Wayland: capture the whole monitor via the xdg-desktop-portal ScreenCast, find the frame by its fixed marker border colour, crop inside it. On X11/Windows/macOS: `outer_position()` + scale factor + `xcap`.

## Step 1 — Lead only, before spawning workers (commit it)

Create the shared contracts so workers never block each other:

- `src-tauri/src/capture/mod.rs`: `trait ScreenCapture { fn capture_monitor(&mut self) -> Result<RgbaImage>; fn start_stream(&mut self, fps: u32, tx: Sender<RgbaImage>) -> Result<()>; fn stop_stream(&mut self); }`, `struct Rect { x, y, w, h }`, `fn locate_frame(img: &RgbaImage, hint: Option<Rect>) -> Option<Rect>`, `fn crop_inside(img, rect, inset: u32) -> RgbaImage`, `fn frame_hash(img) -> u64` (stubs with `todo!()`).
- Frontend event contract (TS types + Rust structs): `capture://frame { thumbnailPngBase64, width, height, ms, skipped }`, `capture://error { message }`; commands `capture_once`, `live_start`, `live_stop`.
- Constant `MARKER_RGB = #19E6C1`, border 2px, inset 3px.

## Step 2 — Parallel workers (each owns only its files, commits separately)

**Worker A — Wayland backend** (`capture/wayland.rs`): portal ScreenCast via `ashpd` + `pipewire`, monitor source, persist `restore_token` in the app data dir so the permission dialog appears only the first time, frame stream into the channel, clean shutdown. Handle the user refusing permission (emit `capture://error`).
**Worker B — Locate / crop / hash** (`capture/locate.rs` + tests): implement `locate_frame` (find the rectangle whose 2px border is MARKER_RGB with a small tolerance for compression; if `hint` is given, search near it first, then full scan), `crop_inside`, `frame_hash`. Unit tests with synthetic images: frame at several positions/sizes, scaled 1x and 2x, noise, partial occlusion → None, marker colour elsewhere as a thin line must not fool it. Target: full-screen 1920x1080 locate < 15 ms in release.
**Worker C — xcap backend** (`capture/xcap_backend.rs`): X11/Windows/macOS using `xcap` + the frame's `outer_position()`/size/scale factor; macOS Screen Recording permission error → `capture://error`.
**Worker D — Frontend** (`src/`): frame border exactly #19E6C1 2px regardless of theme; move the hover toolbar **outside** the capture area (above the top border) so it never appears in captures; 文 button → `capture_once`; ▶/⏸ → `live_start`/`live_stop`; translation bar shows the thumbnail + "W×H px · N ms · skipped K"; show `capture://error` text in the bar.

## Step 3 — Lead: integration (`lib.rs`)

- Backend selection: Wayland session → Worker A, otherwise Worker C.
- Live loop on a background thread (2 fps constant): capture → locate (with last rect as hint) → crop → hash → skip if unchanged → emit. Recapture ~200 ms after the frame stops moving/resizing.
- Stop capturing when the frame is hidden; release PipeWire on Quit.
- Dev-only tray item "Save last capture" → `~/Pictures/lenslate-debug/full-<ts>.png` and `crop-<ts>.png`.
- Log one line per capture: `[lenslate] capture found=<yes/no> rect=<x,y,w,h> ms=<n> skipped=<k>`.

## Step 4 — Verification (lead)

cargo fmt --check, cargo check, cargo clippy -- -D warnings, cargo test (Worker B tests must pass), pnpm lint, pnpm build. Fix everything, final commit. Measure and report CPU usage of the app idle and with live on (if you can run it; otherwise say so).

## Final output (only this)

1. Short Phase report (what works, what is unverified, decisions).
2. One manual test list, max 8 steps, that I will do in one go. Include: run from GNOME Terminal; Alt+Space → Always on Top; first-time portal permission; 文 capture over some text; live mode while scrolling/moving; "Save last capture" and where the PNGs are; what log lines to send you.
