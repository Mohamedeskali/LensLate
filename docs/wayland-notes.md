# Wayland notes

## Session measured

The development session used for Phase 1d reports:

```text
XDG_SESSION_TYPE=wayland
WAYLAND_DISPLAY=wayland-0
DISPLAY=:0
GDK_BACKEND=x11
```

`xwininfo` is installed at `/usr/bin/xwininfo`. Because the frame was not
running during the environment probe, there was no frame window ID to inspect
with `xwininfo`; `DISPLAY=:0` confirms that an X11 display is available for
XWayland inspection.

## Runtime choice

On Linux, when both `WAYLAND_DISPLAY` and `DISPLAY` are present and
`GDK_BACKEND` has not already been selected by the user, LensLate sets
`GDK_BACKEND=x11` before Tauri creates its windows. This requests the XWayland
backend for the transparent frame, while leaving an explicit user choice
untouched.

The frame uses Tauri's native `start_dragging()` and
`start_resize_dragging(direction)` APIs. The translation bar is part of the
same `frame` webview window, so it follows the frame rather than relying on a
second window.

## Geometry

Saved size is restored with a minimum of `120x60` and clamped to 80% of the
current monitor. When no valid saved size exists, the default is `480x160`.
Position restore is attempted through `outerPosition()`/`setPosition()` and is
best-effort because native Wayland compositors may reject application-set
positions. Move and resize events continue to save the actual geometry for the
next run.
