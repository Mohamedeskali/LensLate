# LensLate

LensLate is a free desktop screen translator for Linux, Windows, and macOS. A transparent resizable frame lets you select on-screen content, with translations designed to appear alongside it.

LensLate هو مترجم مجاني لسطح المكتب على أنظمة لينكس وويندوز وماك. يتيح لك إطار شفاف قابل لتغيير الحجم تحديد المحتوى الظاهر على الشاشة وعرض ترجمته بجانبه.

## Development

Install the [Tauri Linux prerequisites](https://tauri.app/start/prerequisites/#linux), Rust stable, Node.js, and pnpm, then run:

```sh
pnpm install
pnpm tauri dev
```

The frame is opened from the tray menu or with `Ctrl+Alt+T` where global shortcuts are supported. On GNOME Wayland, open **Settings > Keyboard > View and Customize Shortcuts > Custom Shortcuts**, add `lenslate --toggle` as the command, and assign a key combination. The CLI flag is forwarded to the running instance.

Run `pnpm lint`, `pnpm format:check`, `pnpm build`, `cargo fmt --check --manifest-path src-tauri/Cargo.toml`, and `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings` for checks.
