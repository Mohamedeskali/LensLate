# Phase 1 - Project skeleton, tray, shortcut and the frame

Goal: on Ubuntu (Wayland), the user can start LensLate, see a tray icon, press a shortcut, and get a thin transparent frame they can drag and resize. No capture, OCR or translation yet.

## Tasks
1. Create a Tauri 2 + React + TypeScript + Vite project named `lenslate` with pnpm. Add MIT LICENSE, README in English with one Arabic paragraph, `.gitignore`, ESLint, Prettier, Rust formatting/linting, and GitHub Actions for Ubuntu, Windows and macOS.
2. Add single instance and a tray menu with Show/Hide frame, Settings, History and Quit. No main window at startup.
3. Add Ctrl+Alt+T on X11/Windows/macOS, `lenslate --toggle` for Wayland, and document the GNOME shortcut. Try the XDG portal and log fallback.
4. Add a borderless transparent always-on-top resizable draggable frame, hover toolbar, attached translation bar, and persisted size/position where supported.
5. Follow system theme and configure English and Arabic with RTL.

## Acceptance checklist
- `pnpm tauri dev` starts with tray and no window.
- CLI and shortcut show/hide the frame.
- Frame drags/resizes and translation bar follows it.
- Toolbar appears/disappears and close hides the frame.
- System theme changes colours.
- CI passes on all three OSes.
