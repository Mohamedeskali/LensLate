# Wayland notes

## Runtime choice

LensLate runs on native Wayland by default and no longer forces
`GDK_BACKEND=x11`. Under XWayland, `start_dragging()` and
`start_resize_dragging(direction)` did not move the frame and only some edges
resized; on native Wayland both work. An explicit `GDK_BACKEND` set by the user
is still honoured, and the value is printed at startup:

```text
[lenslate] GDK_BACKEND=<unset>, WAYLAND_DISPLAY=wayland-0, DISPLAY=:0
```

The translation bar is part of the same `frame` webview window, so it follows
the frame rather than relying on a second window.

## Toggle shortcut

Tauri's global-shortcut plugin cannot grab keys on GNOME Wayland, so the frame
is toggled by a GNOME custom shortcut that runs `lenslate --toggle`. The
single-instance plugin is registered first: a second process claims the
`com.lenslate.app.SingleInstance` D-Bus name, finds it taken, forwards its argv
to the running instance and exits during plugin init, before any window is
created. The running instance logs `[lenslate] toggle received` and toggles the
frame on the main thread.

## Snap-packaged VS Code terminals

The VS Code snap exports `GDK_BACKEND=x11`, `GTK_PATH`, `GIO_MODULE_DIR`,
`GSETTINGS_SCHEMA_DIR` and a snap-prefixed `XDG_DATA_DIRS`. Apps started from
its integrated terminal inherit them: they run on XWayland, and on native
Wayland GTK aborts with `Settings schema
'org.gnome.settings-daemon.plugins.xsettings' does not contain a key named
'antialiasing'`. Run LensLate from GNOME Terminal, or clear those variables.

## Geometry

Saved size is restored with a minimum of `120x60` and clamped to 80% of the
current monitor. When no valid saved size exists, the default is `480x160`.
Position restore is attempted through `outerPosition()`/`setPosition()` and is
best-effort because native Wayland compositors may reject application-set
positions. Move and resize events continue to save the actual geometry for the
next run.
