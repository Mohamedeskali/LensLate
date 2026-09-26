use crate::ocr::{OcrLine, Rect, Script};
use image::RgbaImage;
use std::collections::HashMap;

/// Group detection boxes into lines based on vertical overlap
pub fn group_boxes_into_lines(boxes: &[Rect], y_tolerance: f32) -> Vec<Vec<Rect>> {
    if boxes.is_empty() {
        return vec![];
    }

    // Sort boxes by vertical center
    let mut sorted: Vec<_> = boxes.iter().copied().collect();
    sorted.sort_by_key(|b| b.y + (b.h as i32 / 2));

    let mut lines = Vec::new();
    let mut current_line = vec![sorted[0]];

    for box_rect in sorted.iter().skip(1) {
        let last = current_line.last().unwrap();
        let last_center_y = last.y + (last.h as i32 / 2);
        let curr_center_y = box_rect.y + (box_rect.h as i32 / 2);
        let avg_height = ((last.h as f32 + box_rect.h as f32) / 2.0).max(1.0);

        // Check if boxes vertically overlap enough to be on the same line
        if (curr_center_y - last_center_y).abs() as f32 <= y_tolerance * avg_height {
            current_line.push(*box_rect);
        } else {
            lines.push(current_line);
            current_line = vec![*box_rect];
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    // Sort boxes within each line by x (for LTR) or right-to-left (for RTL)
    for line in &mut lines {
        line.sort_by_key(|b| b.x);
    }

    lines
}

/// Determine reading order for lines: top-to-bottom
pub fn sort_lines_reading_order(lines: &mut [Vec<Rect>]) {
    lines.sort_by_key(|line| {
        // Use the minimum y of boxes in the line
        line.iter().map(|b| b.y).min().unwrap_or(0)
    });
}

/// Detect if a line is likely Arabic based on character width patterns
/// Arabic text tends to be more compact horizontally
pub fn detect_script_from_line(line_boxes: &[Rect], img_width: u32) -> Script {
    if line_boxes.is_empty() {
        return Script::Latin;
    }

    // Calculate average box width relative to image width
    let avg_width: f32 = line_boxes.iter().map(|b| b.w as f32).sum::<f32>() / line_boxes.len() as f32;
    let relative_width = avg_width / img_width as f32;

    // Arabic characters are typically narrower relative to line width
    // Also check aspect ratio - Arabic tends to have taller, narrower characters
    let avg_height: f32 = line_boxes.iter().map(|b| b.h as f32).sum::<f32>() / line_boxes.len() as f32;
    let aspect_ratio = avg_width / avg_height.max(1.0);

    // Heuristic: if aspect ratio is low (tall/narrow chars) or relative width is small
    if aspect_ratio < 0.8 || relative_width < 0.02 {
        Script::Arabic
    } else {
        Script::Latin
    }
}

/// Merge OCR lines into paragraphs with proper reading order
/// For Arabic: right-to-left within line, top-to-bottom for lines
/// For Latin: left-to-right within line, top-to-bottom for lines
pub fn merge_lines_to_text(
    lines: &[Vec<OcrLine>],
    script: Script,
) -> String {
    let mut result = String::new();

    for (line_idx, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }

        // Sort line boxes based on script
        let mut sorted_line = line.clone();
        if script == Script::Arabic {
            // Arabic: right-to-left
            sorted_line.sort_by_key(|l| -(l.rect.x + l.rect.w as i32));
        } else {
            // Latin: left-to-right
            sorted_line.sort_by_key(|l| l.rect.x);
        }

        let line_text: String = sorted_line.iter().map(|l| l.text.as_str()).collect();

        if !line_text.trim().is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&line_text);
        }
    }

    result
}

/// Auto-detect script per line and apply the best recognition
pub fn auto_detect_and_merge(
    latin_lines: &[Vec<OcrLine>],
    arabic_lines: &[Vec<OcrLine>],
    img_width: u32,
) -> (Vec<Vec<OcrLine>>, Script) {
    // For each line position, compare confidence between Latin and Arabic results
    let mut merged = Vec::new();
    let mut arabic_count = 0;
    let mut latin_count = 0;

    let max_lines = latin_lines.len().max(arabic_lines.len());

    for i in 0..max_lines {
        let latin_line = latin_lines.get(i).cloned().unwrap_or_default();
        let arabic_line = arabic_lines.get(i).cloned().unwrap_or_default();

        let latin_conf: f32 = if latin_line.is_empty() {
            0.0
        } else {
            latin_line.iter().map(|l| l.conf).sum::<f32>() / latin_line.len() as f32
        };

        let arabic_conf: f32 = if arabic_line.is_empty() {
            0.0
        } else {
            arabic_line.iter().map(|l| l.conf).sum::<f32>() / arabic_line.len() as f32
        };

        // If Arabic has significantly better confidence, use it
        // Also check visual characteristics
        let use_arabic = if arabic_conf > latin_conf + 0.1 {
            true
        } else if latin_conf > arabic_conf + 0.1 {
            false
        } else {
            // Similar confidence - check visual characteristics
            let boxes: Vec<Rect> = latin_line.iter().map(|l| l.rect).collect();
            detect_script_from_line(&boxes, img_width) == Script::Arabic
        };

        if use_arabic && !arabic_line.is_empty() {
            merged.push(arabic_line);
            arabic_count += 1;
        } else if !latin_line.is_empty() {
            merged.push(latin_line);
            latin_count += 1;
        } else if !arabic_line.is_empty() {
            merged.push(arabic_line);
            arabic_count += 1;
        }
    }

    let final_script = if arabic_count > latin_count {
        Script::Arabic
    } else {
        Script::Latin
    };

    (merged, final_script)
}

/// Convert Arabic text from visual order to logical order
/// This is a simplified version - in production use unicode-bidi
pub fn arabic_visual_to_logical(text: &str) -> String {
    // For now, just return as-is. The recognition model should output logical order.
    // If the model outputs visual order, we'd need proper bidi algorithm here.
    text.to_string()
}

/// Clean up spacing in recognized text
pub fn clean_text(text: &str, script: Script) -> String {
    let mut result = String::new();
    let mut prev_char = '\0';

    for ch in text.chars() {
        match script {
            Script::Arabic => {
                // Arabic: don't add spaces around Arabic characters
                // But keep spaces between words
                if ch.is_whitespace() {
                    if prev_char != ' ' && prev_char != '\0' {
                        result.push(' ');
                    }
                } else {
                    result.push(ch);
                }
            }
            _ => {
                // Latin: normalize whitespace
                if ch.is_whitespace() {
                    if prev_char != ' ' && prev_char != '\0' && !prev_char.is_ascii_punctuation() {
                        result.push(' ');
                    }
                } else {
                    result.push(ch);
                }
            }
        }
        prev_char = ch;
    }

    // Trim trailing spaces
    result.trim_end().to_string()
}

/// Calculate average confidence across all lines
pub fn avg_confidence(lines: &[Vec<OcrLine>]) -> f32 {
    let total: f32 = lines.iter().flatten().map(|l| l.conf).sum();
    let count = lines.iter().flatten().count();
    if count == 0 {
        0.0
    } else {
        total / count as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_boxes_into_lines() {
        let boxes = vec![
            Rect { x: 10, y: 10, w: 50, h: 20 },
            Rect { x: 70, y: 12, w: 50, h: 20 }, // Same line (y diff = 2)
            Rect { x: 10, y: 50, w: 50, h: 20 }, // Next line (y diff = 40)
        ];

        let lines = group_boxes_into_lines(&boxes, 0.5);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 2);
        assert_eq!(lines[1].len(), 1);
    }

    #[test]
    fn test_sort_lines_reading_order() {
        let mut lines = vec![
            vec![Rect { x: 10, y: 50, w: 50, h: 20 }],
            vec![Rect { x: 10, y: 10, w: 50, h: 20 }],
        ];

        sort_lines_reading_order(&mut lines);
        assert_eq!(lines[0][0].y, 10);
        assert_eq!(lines[1][0].y, 50);
    }

    #[test]
    fn test_merge_lines_to_text_latin() {
        let lines = vec![
            vec![
                OcrLine { text: "Hello".into(), conf: 0.9, rect: Rect { x: 10, y: 10, w: 50, h: 20 }, rtl: false },
                OcrLine { text: "World".into(), conf: 0.9, rect: Rect { x: 70, y: 10, w: 50, h: 20 }, rtl: false },
            ],
            vec![
                OcrLine { text: "Test".into(), conf: 0.9, rect: Rect { x: 10, y: 40, w: 40, h: 20 }, rtl: false },
            ],
        ];

        let text = merge_lines_to_text(&lines, Script::Latin);
        assert_eq!(text, "HelloWorld\nTest");
    }

    #[test]
    fn test_merge_lines_to_text_arabic() {
        let lines = vec![
            vec![
                OcrLine { text: "مرحبا".into(), conf: 0.9, rect: Rect { x: 70, y: 10, w: 50, h: 20 }, rtl: true },
                OcrLine { text: "العالم".into(), conf: 0.9, rect: Rect { x: 10, y: 10, w: 50, h: 20 }, rtl: true },
            ],
        ];

        let text = merge_lines_to_text(&lines, Script::Arabic);
        // Should be right-to-left: "العالم مرحبا"
        assert_eq!(text, "العالممرحبا");
    }

    #[test]
    fn test_clean_text_latin() {
        assert_eq!(clean_text("Hello  World", Script::Latin), "Hello World");
        assert_eq!(clean_text("  Hello World  ", Script::Latin), "Hello World");
    }

    #[test]
    fn test_clean_text_arabic() {
        assert_eq!(clean_text("مرحبا  العالم", Script::Arabic), "مرحبا العالم");
    }
}