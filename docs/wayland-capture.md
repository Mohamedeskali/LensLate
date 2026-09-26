# Phase 2 Step 0 — Wayland capture spike

**Environment checked:** Ubuntu GNOME Wayland, GNOME Shell 50.1 (`XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-0`). The LensLate development process is running with its `frame` window configured as always-on-top in `src-tauri/tauri.conf.json`.

## 1. Always-on-top after focusing another window

**Result: inconclusive (not verified).** I could not perform a reliable focus-switch test from this session. The running frame's Tauri configuration requests `alwaysOnTop`, but that alone does not establish that GNOME keeps it above another window after that window receives focus. I will not report this as a confirmed yes.

## 2. Portal capture and marker-based frame discovery

**Result: portal available; capture reliability not established.** The session D-Bus exposes `org.freedesktop.portal.ScreenCast` (version 5), with monitor/window/virtual source types advertised. This confirms the portal interface exists, not that a PipeWire stream can be authorized, decoded, and used to locate the frame reliably.

A real spike would need to run the portal request flow, handle its interactive permission dialog, consume PipeWire frames, and observe the marker while the frame is moved/resized and other windows are focused. I could not complete that interactive test here. Consequently there is no reliable result for marker scanning, cached-position rescans, or the 3px inset crop.

## 3. Other platforms

Not tested. `outer_position()` plus scale factor and `xcap` remains unvalidated on X11/Windows/macOS.

## Decision

**Stop at Step 0.** The required Wayland path has not been shown to work reliably, and the always-on-top behavior is unverified. Per the phase instructions, implementation should not proceed until a manual/interactive spike verifies those prerequisites. No application code was changed.
