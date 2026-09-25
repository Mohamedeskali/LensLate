# LensLate - Brief for the implementing agent

LensLate is a free, open-source (MIT) desktop screen translator for Linux, Windows and macOS.

## Fixed technical decisions

- App shell: Tauri 2. Frontend: React + TypeScript + Vite. Core: Rust.
- Package manager: pnpm.
- OCR: PaddleOCR / RapidOCR models run with ONNX Runtime from Rust (`ort` crate).
- Translation: a Rust `TranslationEngine` trait; engines are pluggable.
- Secrets are stored in the OS keyring (`keyring` crate), never in plain config files.
- UI languages: English and Arabic with full RTL support. Colours follow the system theme.
- Tray icon is the app's home; there is no main window.
- Primary dev machine: Ubuntu with GNOME on Wayland.

## Wayland rules

- Apps cannot grab global shortcuts on Wayland. Provide `lenslate --toggle` for a GNOME custom shortcut and try the XDG GlobalShortcuts portal where available.
- Wayland capture must use `xdg-desktop-portal` and PipeWire.
- Apps cannot set their own window position on Wayland; the user drags the frame.

## Working rules

1. Work only on the current phase.
2. Keep the code formatted (`cargo fmt`, `cargo clippy`, `pnpm lint`).
3. Stop and explain if a design change is required.
4. Do not invent features that were not asked for.

## End-of-phase report format

```
## Phase report
- Phase: <number and name>
- Status: done | partly done | blocked
- What works (tested how): ...
- What does not work / known issues: ...
- Decisions I made that were not in the prompt: ...
- Commands to run and test it: ...
- Files changed (main ones): ...
- Questions for the lead: ...
```
