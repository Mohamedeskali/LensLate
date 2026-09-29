//! Keeps LensLate from reading its own output.
//!
//! In the "overlay" display mode (and when the result panel has to sit inside
//! the frame) the frontend draws translated text over the captured area, so
//! the next capture would contain it. The frontend reports the boxes it draws
//! (in crop pixels) and [`OverlayGuard`] decides per captured crop:
//!
//! - pixels outside the boxes unchanged → [`Decision::Skip`]: the page did
//!   not change, only our drawing is new;
//! - they changed → [`Decision::Suspend`]: the frontend hides the overlay,
//!   reports empty boxes, and after [`SETTLE`] the next (clean) crop is OCR'd;
//! - no boxes → [`Decision::Ocr`].
//!
//! The translation pipeline also drops OCR text that equals the shown
//! translation (`translate::pipeline::is_own_render`) as a second line of
//! defence against a crop captured between drawing and reporting.
//!
//! [`line_colors`] samples the page around each OCR line so the overlay can
//! cover it with a matching background.

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use image::RgbaImage;

use super::Rect;
use crate::ocr::LineColors;

/// Time for the compositor to repaint after the overlay was hidden.
pub const SETTLE: Duration = Duration::from_millis(150);
/// Extra pixels masked around each box (antialiasing, shadows).
const MASK_PAD: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Ocr,
    Skip,
    Suspend,
}

#[derive(Debug, Default)]
pub struct OverlayGuard {
    boxes: Vec<Rect>,
    /// Hash of the crop outside the boxes, taken from the first crop after
    /// the boxes were reported.
    baseline: Option<u64>,
    /// Suspend was requested; waiting for the frontend to hide the overlay.
    suspended: bool,
    cleared_at: Option<Instant>,
}

impl OverlayGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// The frontend drew (or hid, with no boxes) its overlay.
    pub fn set_boxes(&mut self, boxes: Vec<Rect>, now: Instant) {
        let had_boxes = !self.boxes.is_empty();
        self.boxes = boxes.into_iter().filter(|b| b.w > 0 && b.h > 0).collect();
        self.baseline = None;
        self.suspended = false;
        if self.boxes.is_empty() && had_boxes {
            self.cleared_at = Some(now);
        }
    }

    #[cfg(test)]
    pub fn has_boxes(&self) -> bool {
        !self.boxes.is_empty()
    }

    /// How long a capture must still wait for the screen to be free of our
    /// overlay (zero when it already is).
    pub fn settle_left(&self, now: Instant) -> Duration {
        self.cleared_at
            .map(|at| SETTLE.saturating_sub(now.saturating_duration_since(at)))
            .unwrap_or_default()
    }

    pub fn decide(&mut self, crop: &RgbaImage, now: Instant) -> Decision {
        if self.boxes.is_empty() {
            return if self.settle_left(now).is_zero() {
                Decision::Ocr
            } else {
                Decision::Skip
            };
        }
        if self.suspended {
            return Decision::Skip;
        }
        let hash = masked_hash(crop, &self.boxes);
        match self.baseline {
            None => {
                self.baseline = Some(hash);
                Decision::Skip
            }
            Some(base) if base == hash => Decision::Skip,
            Some(_) => {
                self.suspended = true;
                Decision::Suspend
            }
        }
    }
}

fn inside_any(boxes: &[Rect], x: i32, y: i32) -> bool {
    boxes.iter().any(|b| {
        x >= b.x - MASK_PAD
            && y >= b.y - MASK_PAD
            && x < b.x + b.w as i32 + MASK_PAD
            && y < b.y + b.h as i32 + MASK_PAD
    })
}

/// Hash of every pixel outside `boxes` (plus the image size).
pub fn masked_hash(img: &RgbaImage, boxes: &[Rect]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    img.dimensions().hash(&mut hasher);
    for (x, y, p) in img.enumerate_pixels() {
        if !inside_any(boxes, x as i32, y as i32) {
            p.0.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn luma(c: [u8; 3]) -> f32 {
    0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32
}

fn median(mut v: Vec<u8>) -> u8 {
    v.sort_unstable();
    v[v.len() / 2]
}

/// Sample the page behind a line: the background is the per-channel median
/// of a ring just outside the box; the text colour is the median of the
/// pixels inside that differ most from it. Falls back to a readable pair
/// when the box is empty.
pub fn line_colors(img: &RgbaImage, rect: Rect) -> LineColors {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let x0 = rect.x.clamp(0, w);
    let y0 = rect.y.clamp(0, h);
    let x1 = (rect.x + rect.w as i32).clamp(0, w);
    let y1 = (rect.y + rect.h as i32).clamp(0, h);
    if x1 <= x0 || y1 <= y0 {
        return LineColors {
            bg: [255, 255, 255],
            fg: [0, 0, 0],
        };
    }

    let ring = 2;
    let mut channels: [Vec<u8>; 3] = Default::default();
    for y in (y0 - ring).max(0)..(y1 + ring).min(h) {
        for x in (x0 - ring).max(0)..(x1 + ring).min(w) {
            let on_ring = x < x0 + 1 || y < y0 + 1 || x >= x1 - 1 || y >= y1 - 1;
            if on_ring {
                let p = img.get_pixel(x as u32, y as u32).0;
                for c in 0..3 {
                    channels[c].push(p[c]);
                }
            }
        }
    }
    let bg = [
        median(std::mem::take(&mut channels[0])),
        median(std::mem::take(&mut channels[1])),
        median(std::mem::take(&mut channels[2])),
    ];

    let dist = |p: [u8; 3]| -> u32 {
        (0..3)
            .map(|c| (p[c] as i32 - bg[c] as i32).unsigned_abs())
            .sum()
    };
    let mut inner: Vec<[u8; 3]> = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .map(|(x, y)| {
            let p = img.get_pixel(x as u32, y as u32).0;
            [p[0], p[1], p[2]]
        })
        .collect();
    inner.sort_by_key(|&p| std::cmp::Reverse(dist(p)));
    let top: Vec<[u8; 3]> = inner
        .iter()
        .take((inner.len() / 10).max(1))
        .copied()
        .collect();
    let mut fg = [
        median(top.iter().map(|p| p[0]).collect()),
        median(top.iter().map(|p| p[1]).collect()),
        median(top.iter().map(|p| p[2]).collect()),
    ];
    // Too little contrast (blank box): pick black or white.
    if (luma(fg) - luma(bg)).abs() < 60.0 {
        fg = if luma(bg) > 128.0 {
            [0, 0, 0]
        } else {
            [255, 255, 255]
        };
    }
    LineColors { bg, fg }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn page() -> RgbaImage {
        // White page with two dark "text" lines.
        let mut img = RgbaImage::from_pixel(300, 120, Rgba([250, 250, 250, 255]));
        for (y0, y1) in [(20, 36), (60, 76)] {
            for y in y0..y1 {
                for x in (20..260).step_by(3) {
                    img.put_pixel(x, y, Rgba([20, 20, 30, 255]));
                }
            }
        }
        img
    }

    fn line_boxes() -> Vec<Rect> {
        vec![
            Rect {
                x: 18,
                y: 18,
                w: 246,
                h: 20,
            },
            Rect {
                x: 18,
                y: 58,
                w: 246,
                h: 20,
            },
        ]
    }

    /// Paint the translated text over the line boxes, like the frontend does.
    fn draw_overlay(img: &mut RgbaImage, boxes: &[Rect], shade: u8) {
        for b in boxes {
            for y in b.y..b.y + b.h as i32 {
                for x in b.x..b.x + b.w as i32 {
                    let text = (x / 4) % 2 == 0 && (y % 5) < 3;
                    let c = if text { shade } else { 250 };
                    img.put_pixel(x as u32, y as u32, Rgba([c, c, c, 255]));
                }
            }
        }
    }

    #[test]
    fn overlay_is_never_read_back() {
        let t0 = Instant::now();
        let mut guard = OverlayGuard::new();
        let clean = page();
        assert_eq!(guard.decide(&clean, t0), Decision::Ocr);

        // The overlay is drawn and reported: frames showing it are skipped,
        // including re-renders of the overlay itself (other font size).
        let boxes = line_boxes();
        guard.set_boxes(boxes.clone(), t0);
        let mut with_overlay = clean.clone();
        draw_overlay(&mut with_overlay, &boxes, 40);
        assert_eq!(guard.decide(&with_overlay, t0), Decision::Skip);
        assert_eq!(guard.decide(&with_overlay, t0), Decision::Skip);
        draw_overlay(&mut with_overlay, &boxes, 90);
        assert_eq!(guard.decide(&with_overlay, t0), Decision::Skip);

        // The page changes outside our boxes: ask to hide the overlay, and
        // skip until the frontend confirms.
        let mut scrolled = with_overlay.clone();
        for x in 20..200 {
            scrolled.put_pixel(x, 100, Rgba([0, 0, 0, 255]));
        }
        assert_eq!(guard.decide(&scrolled, t0), Decision::Suspend);
        assert_eq!(guard.decide(&scrolled, t0), Decision::Skip);

        // Overlay hidden: wait for the compositor, then read the clean page.
        let t1 = t0 + Duration::from_millis(500);
        guard.set_boxes(Vec::new(), t1);
        assert!(!guard.has_boxes());
        assert_eq!(
            guard.decide(&clean, t1 + Duration::from_millis(50)),
            Decision::Skip
        );
        assert_eq!(
            guard.settle_left(t1 + Duration::from_millis(50)),
            Duration::from_millis(100)
        );
        assert_eq!(guard.decide(&clean, t1 + SETTLE), Decision::Ocr);
    }

    #[test]
    fn masked_hash_ignores_only_the_boxes() {
        let img = page();
        let boxes = line_boxes();
        let mut inside = img.clone();
        draw_overlay(&mut inside, &boxes, 10);
        assert_eq!(masked_hash(&img, &boxes), masked_hash(&inside, &boxes));
        let mut outside = img.clone();
        outside.put_pixel(299, 119, Rgba([1, 2, 3, 255]));
        assert_ne!(masked_hash(&img, &boxes), masked_hash(&outside, &boxes));
        // A different crop size (frame resized) is a change too.
        let smaller = image::imageops::crop_imm(&img, 0, 0, 299, 120).to_image();
        assert_ne!(masked_hash(&img, &boxes), masked_hash(&smaller, &boxes));
    }

    #[test]
    fn empty_boxes_are_ignored() {
        let mut guard = OverlayGuard::new();
        guard.set_boxes(
            vec![Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 5,
            }],
            Instant::now(),
        );
        assert!(!guard.has_boxes());
        assert_eq!(guard.decide(&page(), Instant::now()), Decision::Ocr);
    }

    #[test]
    fn samples_line_colors() {
        let img = page();
        let c = line_colors(&img, line_boxes()[0]);
        assert_eq!(c.bg, [250, 250, 250]);
        assert_eq!(c.fg, [20, 20, 30]);

        // Light text on a dark background.
        let mut dark = RgbaImage::from_pixel(100, 40, Rgba([30, 40, 50, 255]));
        for x in (10..90).step_by(2) {
            for y in 12..28 {
                dark.put_pixel(x, y, Rgba([240, 240, 200, 255]));
            }
        }
        let c = line_colors(
            &dark,
            Rect {
                x: 8,
                y: 10,
                w: 84,
                h: 20,
            },
        );
        assert_eq!(c.bg, [30, 40, 50]);
        assert_eq!(c.fg, [240, 240, 200]);

        // Blank box: black on light.
        let blank = RgbaImage::from_pixel(50, 50, Rgba([200, 200, 200, 255]));
        let c = line_colors(
            &blank,
            Rect {
                x: 10,
                y: 10,
                w: 20,
                h: 10,
            },
        );
        assert_eq!(c.fg, [0, 0, 0]);
        let off = line_colors(
            &blank,
            Rect {
                x: 90,
                y: 90,
                w: 20,
                h: 10,
            },
        );
        assert_eq!(off.bg, [255, 255, 255]);
    }
}
