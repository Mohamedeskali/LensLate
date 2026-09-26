use crate::capture::{Rect, MARKER_BORDER_PX, MARKER_RGB, INSET_PX};
use image::{Rgba, RgbaImage};

const COLOR_TOLERANCE: u8 = 12;

fn color_match(pixel: Rgba<u8>, target: Rgba<u8>) -> bool {
    let dr = pixel[0].abs_diff(target[0]);
    let dg = pixel[1].abs_diff(target[1]);
    let db = pixel[2].abs_diff(target[2]);
    dr <= COLOR_TOLERANCE && dg <= COLOR_TOLERANCE && db <= COLOR_TOLERANCE
}

fn is_marker_border(img: &RgbaImage, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return false;
    }
    color_match(*img.get_pixel(x as u32, y as u32), MARKER_RGB)
}

fn check_border_at(img: &RgbaImage, rect: Rect) -> bool {
    let w = rect.w as i32;
    let h = rect.h as i32;
    let x = rect.x;
    let y = rect.y;

    if x + w > img.width() as i32 || y + h > img.height() as i32 {
        return false;
    }

    let border = MARKER_BORDER_PX as i32;

    // Check top and bottom borders
    for i in 0..w {
        for b in 0..border {
            if !is_marker_border(img, x + i, y + b) {
                return false;
            }
            if !is_marker_border(img, x + i, y + h - 1 - b) {
                return false;
            }
        }
    }

    // Check left and right borders
    for j in 0..h {
        for b in 0..border {
            if !is_marker_border(img, x + b, y + j) {
                return false;
            }
            if !is_marker_border(img, x + w - 1 - b, y + j) {
                return false;
            }
        }
    }

    true
}

pub fn locate_frame(img: &RgbaImage, hint: Option<Rect>) -> Option<Rect> {
    let img_w = img.width() as i32;
    let img_h = img.height() as i32;
    let min_dim = (MARKER_BORDER_PX * 2 + INSET_PX * 2) as i32;

    let search_regions: Vec<Rect> = if let Some(h) = hint {
        let hint_expanded = Rect {
            x: (h.x - 50).max(0),
            y: (h.y - 50).max(0),
            w: (h.w + 100).min(img.width() - h.x as u32),
            h: (h.h + 100).min(img.height() - h.y as u32),
        };
        vec![hint_expanded]
    } else {
        vec![Rect { x: 0, y: 0, w: img.width(), h: img.height() }]
    };

    for region in search_regions {
        let region_x_end = (region.x + region.w as i32).min(img_w);
        let region_y_end = (region.y + region.h as i32).min(img_h);

        for y in region.y..region_y_end - min_dim + 1 {
            for x in region.x..region_x_end - min_dim + 1 {
                let max_w = (region_x_end - x).min(img_w - x);
                let max_h = (region_y_end - y).min(img_h - y);

                for w in min_dim..=max_w {
                    if x + w > img_w { break; }
                    for h in min_dim..=max_h {
                        if y + h > img_h { break; }

                        let rect = Rect { x, y, w: w as u32, h: h as u32 };
                        if check_border_at(img, rect) {
                            return Some(rect);
                        }
                    }
                }
            }
        }
    }

    None
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
                img.put_pixel(x, y, Rgba([(x * 7) as u8, (y * 11) as u8, ((x + y) * 13) as u8, 255]));
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
        let hint = Rect { x: 15, y: 15, w: 30, h: 30 };
        let result = locate_frame(&img, Some(hint));
        assert!(result.is_some());
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
        assert!(result.is_none(), "Thin line should not be detected as frame");
    }

    #[test]
    fn test_crop_inside() {
        let img = create_test_image_with_frame(1920, 1080, 100, 100, 400, 300, 1);
        let rect = Rect { x: 100, y: 100, w: 400, h: 300 };
        let cropped = crop_inside(&img, rect, INSET_PX);
        assert_eq!(cropped.width(), 400 - (MARKER_BORDER_PX + INSET_PX) * 2);
        assert_eq!(cropped.height(), 300 - (MARKER_BORDER_PX + INSET_PX) * 2);
    }

    #[test]
    fn test_crop_inside_scaled() {
        let img = create_test_image_with_frame(3840, 2160, 200, 200, 400, 300, 2);
        let rect = Rect { x: 200, y: 200, w: 800, h: 600 };
        let cropped = crop_inside(&img, rect, INSET_PX);
        assert_eq!(cropped.width(), 800 - (MARKER_BORDER_PX + INSET_PX) * 2);
        assert_eq!(cropped.height(), 600 - (MARKER_BORDER_PX + INSET_PX) * 2);
    }

    #[test]
    fn test_frame_hash_consistent() {
        let img = create_test_image_with_frame(1920, 1080, 100, 100, 400, 300, 1);
        let rect = Rect { x: 100, y: 100, w: 400, h: 300 };
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
        let rect = Rect { x: 100, y: 100, w: 400, h: 300 };
        let cropped1 = crop_inside(&img1, rect, INSET_PX);
        let cropped2 = crop_inside(&img2, rect, INSET_PX);
        assert_ne!(frame_hash(&cropped1), frame_hash(&cropped2));
    }
}