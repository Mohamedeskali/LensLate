# Phase 1d — Fix the frame on Ubuntu Wayland (before Phase 2)

Manual test results from the user (Ubuntu, GNOME, Wayland), with photos:

- OK: tray icon, tray menu, Show/Hide frame, Quit, Ctrl+Alt+Y (GNOME custom shortcut).
- OK (expected for now): Settings and History windows open but are empty placeholders.
- BUG 1: dragging the frame does not move it. The move cursor appears but the frame stays in place.
- BUG 2: the frame's height can be resized but its width cannot.
- BUG 3: the translation bar does not follow the frame. It stays behind, and in one photo it is tiny and stuck inside the frame at the left. It also stays visible when it should move with the frame.
- The frame also opened extremely wide (almost full screen width) after restart, so the restored size is probably wrong.

Fix all of this on your own, without asking me questions, and do not start Phase 2.

1. Find out whether the app runs as native Wayland or through XWayland (`GDK_BACKEND`, `WAYLAND_DISPLAY`, `xwininfo` on the frame). Try forcing XWayland for the app on Linux Wayland sessions (`GDK_BACKEND=x11` set before GTK initialises, only when a Wayland session is detected). Check whether drag, resize from every edge and corner, and `outer_position()` all work correctly that way. Write what you measured in `docs/wayland-notes.md`. We also need the real frame position in Phase 2 for screen capture, so this matters.
2. Use whichever approach makes drag and resize work reliably (XWayland if it works). Use Tauri's native `start_dragging()` and `start_resize_dragging(direction)` from mousedown on the border and edges. Do not use CSS or JS moving.
3. The translation bar must be part of the SAME window as the frame: one window whose top part is the frame and bottom strip is the bar. Remove any separate bar window. Then it always follows the frame and matches its width automatically.
4. Clamp the restored size to a sane range, between the minimum size and 80% of the current monitor. Default to 480x160 when there is no saved size or it is invalid.
5. Run cargo fmt --check, cargo check, cargo clippy -- -D warnings, pnpm lint and pnpm build. Fix everything and commit.

At the end: send the Phase report, then a short manual test list (max 6 steps) covering drag, resize (width and height), bar following the frame, and size restore after restart.
