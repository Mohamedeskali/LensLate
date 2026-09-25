# LensLate

LensLate is a free desktop screen translator for Linux, Windows, and macOS. A transparent resizable frame lets you select on-screen content, with translations designed to appear alongside it.

LensLate هو مترجم مجاني لسطح المكتب على أنظمة لينكس وويندوز وماك. يتيح لك إطار شفاف قابل لتغيير الحجم تحديد المحتوى الظاهر على الشاشة وعرض ترجمته بجانبه.

## Development

Install the [Tauri Linux prerequisites](https://tauri.app/start/prerequisites/#linux), Rust stable, Node.js, and pnpm, then run:

```sh
pnpm install
pnpm tauri dev
```

The frame is opened from the tray menu or with `Ctrl+Alt+Y` where global shortcuts are supported. On GNOME Wayland, open **Settings > Keyboard > View and Customize Shortcuts > Custom Shortcuts**, add `lenslate --toggle` as the command, and assign `Ctrl+Alt+Y`. The CLI flag is forwarded to the running instance.

On Wayland, the transparent interior is not click-through in this phase. Tauri's cursor-event setting applies to the entire transparent window, so enabling it would also make the border, toolbar, and translation bar unclickable; per-region hit testing requires compositor/native support not available through the current Tauri API. The frame therefore remains interactive as a documented limitation.

Run `pnpm lint`, `pnpm format:check`, `pnpm build`, `cargo fmt --check --manifest-path src-tauri/Cargo.toml`, and `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings` for checks.
