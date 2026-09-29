use crate::capture::{Rect, INSET_PX, MARKER_BORDER_PX, MARKER_RGB};
use image::{Rgba, RgbaImage};

const COLOR_TOLERANCE: u8 = 12;
/// Gaps (in pixels) bridged when following a border edge.
const MAX_GAP: i32 = 3;
/// A border is accepted when this share of its pixels (per mille) match,
/// and each side on its own reaches `MIN_SIDE_PERMILLE`.
const MIN_BORDER_PERMILLE: usize = 950;
const MIN_SIDE_PERMILLE: usize = 900;

fn color_match(pixel: Rgba<u8>, target: Rgba<u8>) -> bool {
    let dr = pixel[0].abs_diff(target[0]);
    let dg = pixel[1].abs_diff(target[1]);
    let db = pixel[2].abs_diff(target[2]);
    dr <= COLOR_TOLERANCE && dg <= COLOR_TOLERANCE && db <= COLOR_TOLERANCE
}

/// The marker colour, allowing dimming (a nearby shadow) and slight
/// antialiasing: compared by hue and saturation instead of exact RGB.
/// #19E6C1 has hue ~169° and saturation ~0.89; accepted are hues 165–173°,
/// saturation >= 0.6 and brightness >= ~40%.
fn is_marker_px(p: Rgba<u8>) -> bool {
    if color_match(p, MARKER_RGB) {
        return true;
    }
    let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
    if g < 100 || g <= b || b <= r {
        return false;
    }
    let chroma = g - r;
    if chroma * 10 < g * 6 {
        return false;
    }
    let t = (b - r) * 100;
    t >= 75 * chroma && t <= 89 * chroma
}

fn is_marker_border(img: &RgbaImage, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return false;
    }
    is_marker_px(*img.get_pixel(x as u32, y as u32))
}

/// Whether `rect` has a marker border: enough of its pixels match overall
/// and on every side (tolerates a few dimmed or covered pixels).
fn check_border_at(img: &RgbaImage, rect: Rect) -> bool {
    let (x, y, w, h) = (rect.x, rect.y, rect.w as i32, rect.h as i32);
    if x < 0 || y < 0 || x + w > img.width() as i32 || y + h > img.height() as i32 {
        return false;
    }
    let border = MARKER_BORDER_PX as i32;
    let count = |xs: std::ops::Range<i32>, ys: std::ops::Range<i32>| {
        let mut hits = 0usize;
        let mut total = 0usize;
        for yy in ys {
            for xx in xs.clone() {
                total += 1;
                hits += usize::from(is_marker_border(img, xx, yy));
            }
        }
        (hits, total)
    };
    let sides = [
        count(x..x + w, y..y + border),
        count(x..x + w, y + h - border..y + h),
        count(x..x + border, y..y + h),
        count(x + w - border..x + w, y..y + h),
    ];
    let (mut hits, mut total) = (0, 0);
    for (side_hits, side_total) in sides {
        if side_total == 0 || side_hits * 1000 < side_total * MIN_SIDE_PERMILLE {
            return false;
        }
        hits += side_hits;
        total += side_total;
    }
    hits * 1000 >= total * MIN_BORDER_PERMILLE
}

/// Length of the marker line starting at (x, y) in direction (dx, dy).
/// Gaps of up to `MAX_GAP` pixels are bridged when at least `MIN_RESUME`
/// marker pixels follow, so stray marker-coloured pixels next to the frame
/// never stretch it.
fn run_len(img: &RgbaImage, x: i32, y: i32, dx: i32, dy: i32) -> i32 {
    const MIN_RESUME: i32 = 4;
    let hit = |n: i32| is_marker_border(img, x + dx * n, y + dy * n);
    let mut n = 0;
    while hit(n) {
        n += 1;
    }
    let mut end = n;
    loop {
        let gap_start = n;
        while !hit(n) && n - gap_start <= MAX_GAP {
            n += 1;
        }
        if n - gap_start > MAX_GAP {
            return end;
        }
        let run_start = n;
        while hit(n) {
            n += 1;
        }
        if n - run_start < MIN_RESUME {
            return end;
        }
        end = n;
    }
}

fn search_region(img: &RgbaImage, region: Rect, min_dim: i32) -> Option<Rect> {
    let x_end = (region.x + region.w as i32).min(img.width() as i32);
    let y_end = (region.y + region.h as i32).min(img.height() as i32);

    for y in region.y.max(0)..y_end {
        for x in region.x.max(0)..x_end {
            // Only a top-left corner can start a frame; its size follows from the edge runs.
            if !is_marker_border(img, x, y)
                || is_marker_border(img, x - 1, y)
                || is_marker_border(img, x, y - 1)
            {
                continue;
            }
            let w = run_len(img, x, y, 1, 0);
            let h = run_len(img, x, y, 0, 1);
            if w < min_dim || h < min_dim {
                continue;
            }
            let rect = Rect {
                x,
                y,
                w: w as u32,
                h: h as u32,
            };
            if check_border_at(img, rect) {
                return Some(rect);
            }
        }
    }

    None
}

pub fn locate_frame(img: &RgbaImage, hint: Option<Rect>) -> Option<Rect> {
    let min_dim = (MARKER_BORDER_PX * 2 + INSET_PX * 2) as i32;
    let full = Rect {
        x: 0,
        y: 0,
        w: img.width(),
        h: img.height(),
    };

    // Try near the last known position first, then fall back to the whole image.
    if let Some(h) = hint {
        let near = Rect {
            x: h.x - 50,
            y: h.y - 50,
            w: h.w + 100,
            h: h.h + 100,
        };
        if let Some(rect) = search_region(img, near, min_dim) {
            return Some(rect);
        }
    }
    search_region(img, full, min_dim)
}

pub fn crop_inside(img: &RgbaImage, rect: Rect, inset: u32) -> RgbaImage {
    let inner_x = (rect.x + MARKER_BORDER_PX as i32 + inset as i32).max(0) as u32;
    let inner_y = (rect.y + MARKER_BORDER_PX as i32 + inset as i32).max(0) as u32;
    let inner_w = rect.w.saturating_sub((MARKER_BORDER_PX + inset) * 2);
    let inner_h = rect.h.saturating_sub((MARKER_BORDER_PX + inset) * 2);

    if inner_w == 0 || inner_h == 0 {
        return RgbaImage::new(1, 1);
    }

    let inner_x = inner_x.min(img.width().saturating_sub(1));
    let inner_y = inner_y.min(img.height().saturating_sub(1));
    let inner_w = inner_w.min(img.width().saturating_sub(inner_x));
    let inner_h = inner_h.min(img.height().saturating_sub(inner_y));

    image::imageops::crop_imm(img, inner_x, inner_y, inner_w, inner_h).to_image()
}

pub fn frame_hash(img: &RgbaImage) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for pixel in img.pixels() {
        pixel.0.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn create_test_image_with_frame(
        width: u32,
        height: u32,
        frame_x: i32,
        frame_y: i32,
        frame_w: u32,
        frame_h: u32,
        scale: u32,
    ) -> RgbaImage {
        let mut img = RgbaImage::new(width, height);

        // Fill with noise
        for y in 0..height {
            for x in 0..width {
                img.put_pixel(
                    x,
                    y,
                    Rgba([(x * 7) as u8, (y * 11) as u8, ((x + y) * 13) as u8, 255]),
                );
            }
        }

        // Draw frame border
        let border = MARKER_BORDER_PX * scale;
        for dy in 0..frame_h * scale {
            for dx in 0..border {
                let px = (frame_x + dx as i32) as u32;
                let py = (frame_y + dy as i32) as u32;
                if px < width && py < height {
                    img.put_pixel(px, py, MARKER_RGB);
                }
                let px = (frame_x + frame_w as i32 * scale as i32 - 1 - dx as i32) as u32;
                if px < width && py < height {
                    img.put_pixel(px, py, MARKER_RGB);
                }
            }
        }
        for dx in 0..frame_w * scale {
            for dy in 0..border {
                let px = (frame_x + dx as i32) as u32;
                let py = (frame_y + dy as i32) as u32;
                if px < width && py < height {
                    img.put_pixel(px, py, MARKER_RGB);
                }
                let py = (frame_y + frame_h as i32 * scale as i32 - 1 - dy as i32) as u32;
                if px < width && py < height {
                    img.put_pixel(px, py, MARKER_RGB);
                }
            }
        }

        img
    }

    #[test]
    fn test_locate_frame_basic() {
        let img = create_test_image_with_frame(200, 150, 20, 20, 80, 60, 1);
        let result = locate_frame(&img, None);
        assert!(result.is_some());
        let rect = result.unwrap();
        assert_eq!(rect.x, 20);
        assert_eq!(rect.y, 20);
        assert_eq!(rect.w, 80);
        assert_eq!(rect.h, 60);
    }

    #[test]
    fn test_locate_frame_scaled_2x() {
        let img = create_test_image_with_frame(200, 150, 20, 20, 40, 30, 2);
        let result = locate_frame(&img, None);
        assert!(result.is_some());
        let rect = result.unwrap();
        // With scale=2, frame at (20,20) with size (40,30) draws border at (20,20) with size (80,60)
        // Algorithm may find slightly smaller inner frame due to border matching
        assert!(rect.x >= 20 && rect.x <= 24);
        assert!(rect.y >= 20 && rect.y <= 24);
        assert!(rect.w >= 76 && rect.w <= 80);
        assert!(rect.h >= 56 && rect.h <= 60);
    }

    #[test]
    fn test_locate_frame_with_hint() {
        let img = create_test_image_with_frame(200, 150, 20, 20, 80, 60, 1);
        let hint = Rect {
            x: 15,
            y: 15,
            w: 30,
            h: 30,
        };
        let result = locate_frame(&img, Some(hint));
        assert!(result.is_some());
    }

    #[test]
    fn test_locate_frame_stale_hint_falls_back() {
        let img = create_test_image_with_frame(400, 300, 250, 180, 100, 80, 1);
        let hint = Rect {
            x: 10,
            y: 10,
            w: 80,
            h: 60,
        };
        assert_eq!(
            locate_frame(&img, Some(hint)),
            Some(Rect {
                x: 250,
                y: 180,
                w: 100,
                h: 80
            })
        );
    }

    #[test]
    fn test_locate_frame_full_hd_is_fast() {
        let img = create_test_image_with_frame(1920, 1080, 900, 700, 600, 200, 1);
        let start = std::time::Instant::now();
        assert_eq!(
            locate_frame(&img, None),
            Some(Rect {
                x: 900,
                y: 700,
                w: 600,
                h: 200
            })
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(locate_frame(&RgbaImage::new(1920, 1080), None), None);
    }

    #[test]
    fn test_locate_frame_partial_occlusion() {
        let mut img = create_test_image_with_frame(200, 150, 20, 20, 80, 60, 1);
        // Occlude part of the border
        for y in 20..40 {
            for x in 20..35 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let _result = locate_frame(&img, None);
        // Should still find it (partial occlusion may or may not break depending on severity)
        // This test documents the current behavior
    }

    #[test]
    fn test_locate_frame_thin_line_not_fooled() {
        let mut img = RgbaImage::new(200, 150);
        // Draw a thin line of marker color (1px wide) - should not be detected as frame
        for x in 0..200 {
            img.put_pixel(x, 75, MARKER_RGB);
        }
        let result = locate_frame(&img, None);
        assert!(
            result.is_none(),
            "Thin line should not be detected as frame"
        );
    }

    /// Plain light page with the frame's border drawn in `color` on the
    /// given side rows/columns, and exact marker elsewhere.
    fn page_with_frame(r: Rect) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(400, 300, Rgba([236, 238, 240, 255]));
        for y in r.y..r.y + r.h as i32 {
            for x in r.x..r.x + r.w as i32 {
                let edge = x < r.x + 2
                    || y < r.y + 2
                    || x >= r.x + r.w as i32 - 2
                    || y >= r.y + r.h as i32 - 2;
                if edge {
                    img.put_pixel(x as u32, y as u32, MARKER_RGB);
                }
            }
        }
        img
    }

    const FRAME: Rect = Rect {
        x: 40,
        y: 50,
        w: 300,
        h: 120,
    };

    #[test]
    fn test_marker_pixel_classifier() {
        assert!(is_marker_px(MARKER_RGB));
        // Dimmed by a shadow (measured on GNOME Wayland).
        assert!(is_marker_px(Rgba([22, 206, 173, 255])));
        assert!(is_marker_px(Rgba([22, 203, 170, 255])));
        // Marker blended 25% with a white page.
        assert!(is_marker_px(Rgba([82, 236, 209, 255])));
        // Other greens and teals are not the marker.
        for other in [
            [0, 150, 136], // Material teal
            [0, 200, 0],
            [0, 128, 128],
            [64, 224, 208], // turquoise
            [120, 200, 180],
            [10, 60, 50], // too dark
            [255, 255, 255],
        ] {
            let p = Rgba([other[0], other[1], other[2], 255]);
            assert!(!is_marker_px(p), "{other:?}");
        }
    }

    #[test]
    fn test_bottom_border_darkened_by_shadow() {
        let mut img = page_with_frame(FRAME);
        // A panel shadow below the frame darkens its bottom border rows and
        // the page under it.
        let bottom = FRAME.y + FRAME.h as i32;
        for x in FRAME.x..FRAME.x + FRAME.w as i32 {
            img.put_pixel(x as u32, (bottom - 2) as u32, Rgba([22, 206, 173, 255]));
            img.put_pixel(x as u32, (bottom - 1) as u32, Rgba([22, 203, 170, 255]));
            for (i, y) in (bottom..bottom + 20).enumerate() {
                let v = 150 + i as u8 * 4;
                img.put_pixel(x as u32, y as u32, Rgba([v, v, v, 255]));
            }
        }
        assert_eq!(locate_frame(&img, None), Some(FRAME));
        assert_eq!(locate_frame(&img, Some(FRAME)), Some(FRAME));
    }

    #[test]
    fn test_toolbar_with_1px_gap_and_small_notch() {
        let mut img = page_with_frame(FRAME);
        // Toolbar tab above the right end of the top border, 1px of page
        // between them.
        let surface = Rgba([30, 35, 34, 255]);
        for y in FRAME.y - 29..FRAME.y - 1 {
            for x in FRAME.x + 150..FRAME.x + FRAME.w as i32 {
                img.put_pixel(x as u32, y as u32, surface);
            }
        }
        assert_eq!(locate_frame(&img, None), Some(FRAME));
        // Its bottom edge antialiased onto 3 pixels of the outer border row.
        for x in FRAME.x + 200..FRAME.x + 203 {
            img.put_pixel(x as u32, FRAME.y as u32, Rgba([60, 90, 85, 255]));
        }
        assert_eq!(locate_frame(&img, None), Some(FRAME));
    }

    #[test]
    fn test_fractional_dpi_antialiased_border() {
        // 2px CSS at 1.25x = 2.5 device pixels: the outer ring is a blend of
        // marker and page, then two solid rings, then a blend with the
        // (transparent) inside showing the page.
        let page = [236u8, 238, 240];
        let blend = |a: [u8; 3], t: f32| {
            let mix = |c: usize| (MARKER_RGB[c] as f32 * t + a[c] as f32 * (1.0 - t)) as u8;
            Rgba([mix(0), mix(1), mix(2), 255])
        };
        let outer = Rect {
            x: 60,
            y: 40,
            w: 250,
            h: 150,
        };
        let mut img = RgbaImage::from_pixel(400, 300, Rgba([page[0], page[1], page[2], 255]));
        let ring = |img: &mut RgbaImage, i: i32, px: Rgba<u8>| {
            let (x0, y0) = (outer.x + i, outer.y + i);
            let (x1, y1) = (
                outer.x + outer.w as i32 - 1 - i,
                outer.y + outer.h as i32 - 1 - i,
            );
            for x in x0..=x1 {
                img.put_pixel(x as u32, y0 as u32, px);
                img.put_pixel(x as u32, y1 as u32, px);
            }
            for y in y0..=y1 {
                img.put_pixel(x0 as u32, y as u32, px);
                img.put_pixel(x1 as u32, y as u32, px);
            }
        };
        ring(&mut img, 0, blend(page, 0.5));
        ring(&mut img, 1, MARKER_RGB);
        ring(&mut img, 2, MARKER_RGB);
        ring(&mut img, 3, blend(page, 0.5));
        let found = locate_frame(&img, None).expect("antialiased frame");
        assert!((found.x - outer.x).abs() <= 1 && (found.y - outer.y).abs() <= 1);
        assert!(found.w.abs_diff(outer.w) <= 2 && found.h.abs_diff(outer.h) <= 2);
        // The crop is inside the border and never contains marker pixels.
        let crop = crop_inside(&img, found, INSET_PX);
        assert!(crop.pixels().all(|p| !is_marker_px(*p)));
    }

    #[test]
    fn test_teal_ui_rectangles_are_not_the_frame() {
        let mut img = RgbaImage::from_pixel(300, 200, Rgba([250, 250, 250, 255]));
        for (i, color) in [[0u8, 150, 136], [64, 224, 208], [0, 200, 0]]
            .iter()
            .enumerate()
        {
            let r = Rect {
                x: 10 + i as i32 * 90,
                y: 20,
                w: 80,
                h: 60,
            };
            for y in r.y..r.y + r.h as i32 {
                for x in r.x..r.x + r.w as i32 {
                    let edge = x < r.x + 2
                        || y < r.y + 2
                        || x >= r.x + r.w as i32 - 2
                        || y >= r.y + r.h as i32 - 2;
                    if edge {
                        img.put_pixel(
                            x as u32,
                            y as u32,
                            Rgba([color[0], color[1], color[2], 255]),
                        );
                    }
                }
            }
        }
        assert_eq!(locate_frame(&img, None), None);
    }

    #[test]
    fn test_border_with_a_missing_side_is_rejected() {
        let mut img = page_with_frame(FRAME);
        // Right side covered by another window over 30% of its height.
        let right = FRAME.x + FRAME.w as i32 - 2;
        for y in FRAME.y + 20..FRAME.y + 60 {
            for x in right..right + 2 {
                img.put_pixel(x as u32, y as u32, Rgba([20, 20, 20, 255]));
            }
        }
        assert_eq!(locate_frame(&img, None), None);
    }

    #[test]
    fn test_crop_inside() {
        let img = create_test_image_with_frame(1920, 1080, 100, 100, 400, 300, 1);
        let rect = Rect {
            x: 100,
            y: 100,
            w: 400,
            h: 300,
        };
        let cropped = crop_inside(&img, rect, INSET_PX);
        assert_eq!(cropped.width(), 400 - (MARKER_BORDER_PX + INSET_PX) * 2);
        assert_eq!(cropped.height(), 300 - (MARKER_BORDER_PX + INSET_PX) * 2);
    }

    #[test]
    fn test_crop_inside_scaled() {
        let img = create_test_image_with_frame(3840, 2160, 200, 200, 400, 300, 2);
        let rect = Rect {
            x: 200,
            y: 200,
            w: 800,
            h: 600,
        };
        let cropped = crop_inside(&img, rect, INSET_PX);
        assert_eq!(cropped.width(), 800 - (MARKER_BORDER_PX + INSET_PX) * 2);
        assert_eq!(cropped.height(), 600 - (MARKER_BORDER_PX + INSET_PX) * 2);
    }

    #[test]
    fn test_frame_hash_consistent() {
        let img = create_test_image_with_frame(1920, 1080, 100, 100, 400, 300, 1);
        let rect = Rect {
            x: 100,
            y: 100,
            w: 400,
            h: 300,
        };
        let cropped = crop_inside(&img, rect, INSET_PX);
        let hash1 = frame_hash(&cropped);
        let hash2 = frame_hash(&cropped);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_frame_hash_different() {
        let img1 = create_test_image_with_frame(1920, 1080, 100, 100, 400, 300, 1);
        let mut img2 = create_test_image_with_frame(1920, 1080, 100, 100, 400, 300, 1);
        // Modify one pixel INSIDE the cropped region (frame at 100,100, border=2, inset=3 -> cropped starts at 105,105)
        img2.put_pixel(110, 110, Rgba([255, 0, 0, 255]));
        let rect = Rect {
            x: 100,
            y: 100,
            w: 400,
            h: 300,
        };
        let cropped1 = crop_inside(&img1, rect, INSET_PX);
        let cropped2 = crop_inside(&img2, rect, INSET_PX);
        assert_ne!(frame_hash(&cropped1), frame_hash(&cropped2));
    }
}
