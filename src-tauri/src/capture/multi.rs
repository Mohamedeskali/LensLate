//! Multi-monitor support shared by the capture backends.
//!
//! - [`locate_in_frames`] / [`LiveLocator`]: find the marker frame on any of
//!   several monitor pictures, each in its own pixel space (mixed DPI).
//! - [`pick_monitor`], [`match_monitor`], [`window_rect_in_image`]: for xcap,
//!   choose the monitor that holds the window and map the window rectangle
//!   into that monitor's captured image.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use image::RgbaImage;

use super::{locate::locate_frame, Rect};

/// A picture of one monitor.
#[derive(Clone)]
pub struct MonitorFrame {
    /// Stable index of the monitor within the backend (portal stream index).
    pub index: usize,
    pub name: String,
    pub image: RgbaImage,
}

impl std::fmt::Debug for MonitorFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MonitorFrame({} #{} {}x{})",
            self.name,
            self.index,
            self.image.width(),
            self.image.height()
        )
    }
}

/// Where the frame was found: which monitor, and its rectangle in that
/// monitor's image pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub index: usize,
    pub monitor: String,
    pub rect: Rect,
}

/// Search every monitor, the one of `hint` first (with its rectangle as the
/// search hint), then the others.
pub fn locate_in_frames(frames: &[MonitorFrame], hint: Option<&Located>) -> Option<Located> {
    let mut order: Vec<&MonitorFrame> = frames.iter().collect();
    if let Some(h) = hint {
        order.sort_by_key(|f| f.index != h.index);
    }
    order.into_iter().find_map(|frame| {
        let rect_hint = hint.filter(|h| h.index == frame.index).map(|h| h.rect);
        locate_frame(&frame.image, rect_hint).map(|rect| Located {
            index: frame.index,
            monitor: frame.name.clone(),
            rect,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// The frame is at this place (possibly on a different monitor than before).
    Found(Located),
    /// Not found where it was; `last` is the previous place.
    Missing { last: Located, consecutive: u32 },
    /// Never found yet on any monitor.
    NotFound,
    /// A picture of a monitor the frame is not on; nothing to do.
    Ignored,
}

/// Follows the frame across the live streams of several monitors. Streams
/// deliver pictures independently (and idle monitors rarely), so the latest
/// picture of every monitor is kept and searched when the frame leaves its
/// monitor.
#[derive(Default)]
pub struct LiveLocator {
    frames: BTreeMap<usize, MonitorFrame>,
    last: Option<Located>,
    missing: u32,
}

impl LiveLocator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frame(&self, index: usize) -> Option<&MonitorFrame> {
        self.frames.get(&index)
    }

    /// The latest picture of every monitor seen so far.
    pub fn frames(&self) -> impl Iterator<Item = &MonitorFrame> {
        self.frames.values()
    }

    #[cfg(test)]
    pub fn last(&self) -> Option<&Located> {
        self.last.as_ref()
    }

    pub fn observe(&mut self, frame: MonitorFrame) -> Observation {
        let index = frame.index;
        self.frames.insert(index, frame);
        let image = &self.frames[&index].image;

        match self.last.clone() {
            Some(last) if last.index == index => {
                if let Some(rect) = locate_frame(image, Some(last.rect)) {
                    return self.found(index, rect);
                }
                self.missing += 1;
                // It may have moved to another monitor that is idle.
                if let Some(found) = self.search_others(index) {
                    return self.found(found.index, found.rect);
                }
                Observation::Missing {
                    last,
                    consecutive: self.missing,
                }
            }
            Some(_) => {
                if self.missing == 0 {
                    return Observation::Ignored;
                }
                match locate_frame(image, None) {
                    Some(rect) => self.found(index, rect),
                    None => Observation::Ignored,
                }
            }
            None => {
                if let Some(rect) = locate_frame(image, None) {
                    return self.found(index, rect);
                }
                match self.search_others(index) {
                    Some(found) => self.found(found.index, found.rect),
                    None => Observation::NotFound,
                }
            }
        }
    }

    fn search_others(&self, except: usize) -> Option<Located> {
        let others: Vec<MonitorFrame> = self
            .frames
            .values()
            .filter(|f| f.index != except)
            .cloned()
            .collect();
        locate_in_frames(&others, None)
    }

    fn found(&mut self, index: usize, rect: Rect) -> Observation {
        self.missing = 0;
        let located = Located {
            index,
            monitor: self.frames[&index].name.clone(),
            rect,
        };
        self.last = Some(located.clone());
        Observation::Found(located)
    }
}

/// `LENSLATE_DEBUG_CAPTURE=1` saves the pictures searched when the frame is
/// not found (see [`save_debug_frames`]).
pub fn debug_capture_enabled() -> bool {
    std::env::var("LENSLATE_DEBUG_CAPTURE").is_ok_and(|v| v.trim() == "1")
}

/// `/tmp/lenslate-debug` on Linux and macOS, `%TEMP%\lenslate-debug` on Windows.
pub fn debug_dir() -> PathBuf {
    std::env::temp_dir().join("lenslate-debug")
}

/// Save each picture as `<time>-<index>-<monitor>.png` in `dir`; returns the
/// files written.
pub fn save_debug_frames<'a>(
    frames: impl IntoIterator<Item = &'a MonitorFrame>,
    dir: &Path,
) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.3f");
    let mut saved = Vec::new();
    for frame in frames {
        let name: String = frame
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = dir.join(format!("{stamp}-{}-{name}.png", frame.index));
        frame
            .image
            .save(&path)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        saved.push(path);
    }
    Ok(saved)
}

/// A monitor in some desktop coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl MonitorInfo {
    fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            w: self.width,
            h: self.height,
        }
    }
}

fn overlap(a: Rect, b: Rect) -> u64 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w as i32).min(b.x + b.w as i32);
    let y1 = (a.y + a.h as i32).min(b.y + b.h as i32);
    if x1 <= x0 || y1 <= y0 {
        0
    } else {
        (x1 - x0) as u64 * (y1 - y0) as u64
    }
}

/// The monitor holding most of `window` (same coordinate space); if the
/// window is off every monitor, the nearest one.
pub fn pick_monitor(monitors: &[MonitorInfo], window: Rect) -> Option<usize> {
    let best = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| (i, overlap(m.rect(), window)))
        .max_by_key(|&(_, area)| area)?;
    if best.1 > 0 {
        return Some(best.0);
    }
    let (cx, cy) = (
        window.x as i64 + window.w as i64 / 2,
        window.y as i64 + window.h as i64 / 2,
    );
    monitors
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| {
            let dx = (cx - (m.x as i64 + m.width as i64 / 2)).abs();
            let dy = (cy - (m.y as i64 + m.height as i64 / 2)).abs();
            dx * dx + dy * dy
        })
        .map(|(i, _)| i)
}

/// Find `target` (from Tauri, physical pixels) among the capture library's
/// monitors: by name, else by origin (the library may use logical points,
/// as xcap does on macOS), else by size.
pub fn match_monitor(target: &MonitorInfo, candidates: &[MonitorInfo]) -> Option<usize> {
    if !target.name.is_empty() {
        if let Some(i) = candidates.iter().position(|c| c.name == target.name) {
            return Some(i);
        }
    }
    let same_origin = |c: &MonitorInfo| {
        let physical = |v: i32| (v as f64 * c.scale).round() as i32;
        (c.x == target.x && c.y == target.y)
            || (physical(c.x) == target.x && physical(c.y) == target.y)
    };
    if let Some(i) = candidates.iter().position(same_origin) {
        return Some(i);
    }
    candidates.iter().position(|c| {
        let w = (c.width as f64 * c.scale).round() as u32;
        let h = (c.height as f64 * c.scale).round() as u32;
        (c.width == target.width && c.height == target.height)
            || (w == target.width && h == target.height)
    })
}

/// Map a window rectangle (global, same space as `monitor`) into the pixels
/// of that monitor's captured image, whose resolution may differ from the
/// monitor's reported size (HiDPI). Clamped to the image; `None` if the
/// window is not on it.
pub fn window_rect_in_image(
    window: Rect,
    monitor: &MonitorInfo,
    image_w: u32,
    image_h: u32,
) -> Option<Rect> {
    if monitor.width == 0 || monitor.height == 0 {
        return None;
    }
    let sx = image_w as f64 / monitor.width as f64;
    let sy = image_h as f64 / monitor.height as f64;
    let x0 = ((window.x - monitor.x) as f64 * sx).round() as i64;
    let y0 = ((window.y - monitor.y) as f64 * sy).round() as i64;
    let x1 = ((window.x + window.w as i32 - monitor.x) as f64 * sx).round() as i64;
    let y1 = ((window.y + window.h as i32 - monitor.y) as f64 * sy).round() as i64;
    let (x0, y0) = (x0.clamp(0, image_w as i64), y0.clamp(0, image_h as i64));
    let (x1, y1) = (x1.clamp(0, image_w as i64), y1.clamp(0, image_h as i64));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Rect {
        x: x0 as i32,
        y: y0 as i32,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{MARKER_BORDER_PX, MARKER_RGB};
    use image::Rgba;

    /// A monitor picture with noise and, optionally, the frame drawn with a
    /// border of `MARKER_BORDER_PX * scale` pixels.
    fn screen(w: u32, h: u32, frame: Option<Rect>, scale: u32) -> RgbaImage {
        let mut img = RgbaImage::from_fn(w, h, |x, y| {
            Rgba([(x * 7) as u8, (y * 11) as u8, ((x + y) * 13) as u8, 255])
        });
        if let Some(r) = frame {
            let border = MARKER_BORDER_PX * scale;
            for y in r.y as u32..r.y as u32 + r.h {
                for x in r.x as u32..r.x as u32 + r.w {
                    let edge = x < r.x as u32 + border
                        || y < r.y as u32 + border
                        || x >= r.x as u32 + r.w - border
                        || y >= r.y as u32 + r.h - border;
                    if edge {
                        img.put_pixel(x, y, MARKER_RGB);
                    }
                }
            }
        }
        img
    }

    fn frame(index: usize, image: RgbaImage) -> MonitorFrame {
        MonitorFrame {
            index,
            name: format!("mon{index}"),
            image,
        }
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn finds_frame_on_second_monitor_with_mixed_dpi() {
        // Left: 1920x1080 @1x without the frame; right: 3840x2160 @2x with it.
        let on_hidpi = rect(1000, 600, 800, 400);
        let frames = vec![
            frame(0, screen(1920, 1080, None, 1)),
            frame(1, screen(3840, 2160, Some(on_hidpi), 2)),
        ];
        let found = locate_in_frames(&frames, None).unwrap();
        assert_eq!(found.index, 1);
        assert_eq!(found.monitor, "mon1");
        assert_eq!(found.rect, on_hidpi);
    }

    #[test]
    fn hint_monitor_is_searched_first() {
        // The marker is on both (e.g. a stale picture); the hinted one wins.
        let r0 = rect(10, 10, 100, 60);
        let r1 = rect(50, 40, 120, 80);
        let frames = vec![
            frame(0, screen(400, 300, Some(r0), 1)),
            frame(1, screen(400, 300, Some(r1), 1)),
        ];
        let hint = Located {
            index: 1,
            monitor: "mon1".into(),
            rect: r1,
        };
        assert_eq!(locate_in_frames(&frames, Some(&hint)).unwrap().rect, r1);
        assert_eq!(locate_in_frames(&frames, None).unwrap().index, 0);
        assert_eq!(
            locate_in_frames(&[frame(0, screen(400, 300, None, 1))], None),
            None
        );
    }

    #[test]
    fn live_locator_follows_frame_to_an_idle_monitor() {
        let mut live = LiveLocator::new();
        let a = rect(100, 100, 300, 200);
        let b = rect(200, 150, 600, 400);

        // Frame on monitor 0; monitor 1 (4K, 2x) delivers a picture without it.
        assert!(matches!(
            live.observe(frame(0, screen(1920, 1080, Some(a), 1))),
            Observation::Found(Located { index: 0, .. })
        ));
        assert_eq!(
            live.observe(frame(1, screen(3840, 2160, None, 2))),
            Observation::Ignored
        );

        // The frame is dragged to monitor 1, which then goes idle after one
        // picture that arrives while monitor 0 still shows the frame.
        assert_eq!(
            live.observe(frame(1, screen(3840, 2160, Some(b), 2))),
            Observation::Ignored
        );
        // Monitor 0 now lacks the frame: the cached picture of 1 is searched.
        match live.observe(frame(0, screen(1920, 1080, None, 1))) {
            Observation::Found(l) => {
                assert_eq!(l.index, 1);
                assert_eq!(l.rect, b);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(live.last().unwrap().index, 1);
        assert_eq!(live.frame(1).unwrap().image.width(), 3840);
    }

    #[test]
    fn live_locator_reports_missing_then_recovers() {
        let mut live = LiveLocator::new();
        let a = rect(100, 100, 300, 200);
        assert_eq!(
            live.observe(frame(0, screen(800, 600, None, 1))),
            Observation::NotFound
        );
        assert!(matches!(
            live.observe(frame(0, screen(800, 600, Some(a), 1))),
            Observation::Found(_)
        ));
        for n in 1..=2 {
            match live.observe(frame(0, screen(800, 600, None, 1))) {
                Observation::Missing { last, consecutive } => {
                    assert_eq!(last.rect, a);
                    assert_eq!(consecutive, n);
                }
                other => panic!("{other:?}"),
            }
        }
        // While missing, pictures of other monitors are searched.
        let c = rect(40, 30, 200, 100);
        assert!(matches!(
            live.observe(frame(2, screen(640, 480, Some(c), 1))),
            Observation::Found(Located { index: 2, .. })
        ));
    }

    fn mon(name: &str, x: i32, y: i32, w: u32, h: u32, scale: f64) -> MonitorInfo {
        MonitorInfo {
            name: name.into(),
            x,
            y,
            width: w,
            height: h,
            scale,
        }
    }

    #[test]
    fn debug_frames_are_saved_as_png() {
        let dir = std::env::temp_dir().join(format!("lenslate-debug-test-{}", std::process::id()));
        let frames = [
            MonitorFrame {
                index: 0,
                name: "portal-0@0,0(1920x1080)".into(),
                image: screen(40, 30, None, 1),
            },
            frame(1, screen(20, 10, None, 1)),
        ];
        let saved = save_debug_frames(&frames, &dir).unwrap();
        assert_eq!(saved.len(), 2);
        let first = saved[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(first.ends_with("-0-portal_0_0_0_1920x1080_.png"), "{first}");
        let back = image::open(&saved[1]).unwrap().to_rgba8();
        assert_eq!(back.dimensions(), (20, 10));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(debug_dir().ends_with("lenslate-debug"));
    }

    #[test]
    fn pick_monitor_by_overlap_with_offsets() {
        // Laptop 2880x1800 @2x at the origin, external 1920x1080 on the left
        // (negative x) and one above-right.
        let monitors = vec![
            mon("eDP-1", 0, 0, 2880, 1800, 2.0),
            mon("HDMI-1", -1920, 300, 1920, 1080, 1.0),
            mon("DP-2", 2880, -1080, 1920, 1080, 1.0),
        ];
        assert_eq!(pick_monitor(&monitors, rect(100, 100, 400, 200)), Some(0));
        assert_eq!(pick_monitor(&monitors, rect(-1500, 500, 400, 200)), Some(1));
        // Straddling: most of it on DP-2.
        assert_eq!(pick_monitor(&monitors, rect(2800, -500, 400, 200)), Some(2));
        // Straddling: most of it on the laptop.
        assert_eq!(pick_monitor(&monitors, rect(-100, 400, 400, 200)), Some(0));
        // Off-screen: nearest.
        assert_eq!(pick_monitor(&monitors, rect(-5000, 700, 100, 100)), Some(1));
        assert_eq!(pick_monitor(&[], rect(0, 0, 1, 1)), None);
    }

    #[test]
    fn match_monitor_by_name_origin_or_size() {
        let tauri = mon("DP-2", 2880, 0, 3840, 2160, 2.0);
        // Name match.
        let xcap = vec![
            mon("eDP-1", 0, 0, 1440, 900, 2.0),
            mon("DP-2", 1440, 0, 1920, 1080, 2.0),
        ];
        assert_eq!(match_monitor(&tauri, &xcap), Some(1));
        // macOS-style names differ; xcap reports logical points (1440 * 2 = 2880).
        let xcap = vec![
            mon("Built-in", 0, 0, 1440, 900, 2.0),
            mon("LG UltraFine", 1440, 0, 1920, 1080, 2.0),
        ];
        assert_eq!(match_monitor(&tauri, &xcap), Some(1));
        // Only the size is left to compare.
        let lone = mon("", -99, -99, 3840, 2160, 1.0);
        assert_eq!(
            match_monitor(&tauri, &[mon("x", 5, 5, 100, 100, 1.0), lone]),
            Some(1)
        );
        assert_eq!(
            match_monitor(&tauri, &[mon("x", 5, 5, 100, 100, 1.0)]),
            None
        );
    }

    #[test]
    fn window_rect_maps_into_monitor_image() {
        // Physical coordinates, image at the same resolution (X11/Windows).
        let right = mon("DP-2", 1920, 0, 2560, 1440, 1.0);
        assert_eq!(
            window_rect_in_image(rect(2020, 100, 500, 300), &right, 2560, 1440),
            Some(rect(100, 100, 500, 300))
        );
        // Monitor left of the primary (negative origin).
        let left = mon("HDMI-1", -1920, 0, 1920, 1080, 1.0);
        assert_eq!(
            window_rect_in_image(rect(-1900, 50, 400, 200), &left, 1920, 1080),
            Some(rect(20, 50, 400, 200))
        );
        // Image twice the reported size (logical monitor, Retina capture).
        let retina = mon("Built-in", 0, 0, 1440, 900, 2.0);
        assert_eq!(
            window_rect_in_image(rect(100, 50, 300, 200), &retina, 2880, 1800),
            Some(rect(200, 100, 600, 400))
        );
        // Partly off the monitor: clamped.
        assert_eq!(
            window_rect_in_image(rect(2500, 1300, 200, 300), &right, 2560, 1440),
            Some(rect(580, 1300, 200, 140))
        );
        // Entirely elsewhere.
        assert_eq!(
            window_rect_in_image(rect(0, 0, 100, 100), &right, 2560, 1440),
            None
        );
    }
}
