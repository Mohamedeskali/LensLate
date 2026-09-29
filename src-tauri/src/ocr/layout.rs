use crate::ocr::{OcrLine, Rect, Script};

/// Group detection boxes into lines based on vertical overlap
pub fn group_boxes_into_lines(boxes: &[Rect], y_tolerance: f32) -> Vec<Vec<Rect>> {
    if boxes.is_empty() {
        return vec![];
    }

    // Sort boxes by vertical center
    let mut sorted: Vec<_> = boxes.to_vec();
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
    let avg_width: f32 =
        line_boxes.iter().map(|b| b.w as f32).sum::<f32>() / line_boxes.len() as f32;
    let relative_width = avg_width / img_width as f32;

    // Arabic characters are typically narrower relative to line width
    // Also check aspect ratio - Arabic tends to have taller, narrower characters
    let avg_height: f32 =
        line_boxes.iter().map(|b| b.h as f32).sum::<f32>() / line_boxes.len() as f32;
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
pub fn merge_lines_to_text(lines: &[Vec<OcrLine>], script: Script) -> String {
    let mut result = String::new();

    for line in lines.iter() {
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

/// Join detected boxes that sit on the same row into one line.
///
/// Two boxes share a row when their vertical extents overlap by at least half
/// of the shorter one. Within a row the text runs in reading order (left to
/// right, or right to left for RTL rows) separated by single spaces; rows are
/// returned top to bottom. The line confidence is the mean over characters.
pub fn join_rows(mut boxes: Vec<OcrLine>) -> Vec<OcrLine> {
    boxes.retain(|b| !b.text.trim().is_empty());
    boxes.sort_by(|a, b| a.rect.center_y().total_cmp(&b.rect.center_y()));
    let mut rows: Vec<Vec<OcrLine>> = Vec::new();
    for b in boxes {
        let top = b.rect.y;
        let bottom = b.rect.y + b.rect.h as i32;
        let row = rows.iter_mut().find(|row| {
            row.iter().any(|o| {
                let overlap = bottom.min(o.rect.y + o.rect.h as i32) - top.max(o.rect.y);
                overlap as f32 >= 0.5 * b.rect.h.min(o.rect.h) as f32
            })
        });
        match row {
            Some(row) => row.push(b),
            None => rows.push(vec![b]),
        }
    }
    let mut lines: Vec<OcrLine> = rows
        .into_iter()
        .map(|mut row| {
            let rtl = row.iter().filter(|b| b.rtl).count() * 2 > row.len();
            if rtl {
                row.sort_by_key(|b| -(b.rect.x + b.rect.w as i32));
            } else {
                row.sort_by_key(|b| b.rect.x);
            }
            let x0 = row.iter().map(|b| b.rect.x).min().unwrap_or(0);
            let y0 = row.iter().map(|b| b.rect.y).min().unwrap_or(0);
            let x1 = row
                .iter()
                .map(|b| b.rect.x + b.rect.w as i32)
                .max()
                .unwrap_or(0);
            let y1 = row
                .iter()
                .map(|b| b.rect.y + b.rect.h as i32)
                .max()
                .unwrap_or(0);
            let chars: usize = row.iter().map(|b| b.text.chars().count()).sum();
            let conf = row
                .iter()
                .map(|b| b.conf * b.text.chars().count() as f32)
                .sum::<f32>()
                / chars.max(1) as f32;
            let text = row
                .iter()
                .map(|b| b.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            OcrLine {
                text,
                conf,
                rect: Rect {
                    x: x0,
                    y: y0,
                    w: (x1 - x0) as u32,
                    h: (y1 - y0) as u32,
                },
                rtl,
            }
        })
        .collect();
    lines.sort_by_key(|l| l.rect.y);
    lines
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
            Rect {
                x: 10,
                y: 10,
                w: 50,
                h: 20,
            },
            Rect {
                x: 70,
                y: 12,
                w: 50,
                h: 20,
            }, // Same line (y diff = 2)
            Rect {
                x: 10,
                y: 50,
                w: 50,
                h: 20,
            }, // Next line (y diff = 40)
        ];

        let lines = group_boxes_into_lines(&boxes, 0.5);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 2);
        assert_eq!(lines[1].len(), 1);
    }

    #[test]
    fn test_sort_lines_reading_order() {
        let mut lines = vec![
            vec![Rect {
                x: 10,
                y: 50,
                w: 50,
                h: 20,
            }],
            vec![Rect {
                x: 10,
                y: 10,
                w: 50,
                h: 20,
            }],
        ];

        sort_lines_reading_order(&mut lines);
        assert_eq!(lines[0][0].y, 10);
        assert_eq!(lines[1][0].y, 50);
    }

    #[test]
    fn test_merge_lines_to_text_latin() {
        let lines = vec![
            vec![
                OcrLine {
                    text: "Hello".into(),
                    conf: 0.9,
                    rect: Rect {
                        x: 10,
                        y: 10,
                        w: 50,
                        h: 20,
                    },
                    rtl: false,
                },
                OcrLine {
                    text: "World".into(),
                    conf: 0.9,
                    rect: Rect {
                        x: 70,
                        y: 10,
                        w: 50,
                        h: 20,
                    },
                    rtl: false,
                },
            ],
            vec![OcrLine {
                text: "Test".into(),
                conf: 0.9,
                rect: Rect {
                    x: 10,
                    y: 40,
                    w: 40,
                    h: 20,
                },
                rtl: false,
            }],
        ];

        let text = merge_lines_to_text(&lines, Script::Latin);
        assert_eq!(text, "HelloWorld\nTest");
    }

    #[test]
    fn test_merge_lines_to_text_arabic() {
        let lines = vec![vec![
            OcrLine {
                text: "مرحبا".into(),
                conf: 0.9,
                rect: Rect {
                    x: 70,
                    y: 10,
                    w: 50,
                    h: 20,
                },
                rtl: true,
            },
            OcrLine {
                text: "العالم".into(),
                conf: 0.9,
                rect: Rect {
                    x: 10,
                    y: 10,
                    w: 50,
                    h: 20,
                },
                rtl: true,
            },
        ]];

        let text = merge_lines_to_text(&lines, Script::Arabic);
        // Should be right-to-left: "مرحبا" (x=70, rightmost) then "العالم" (x=10, leftmost)
        assert_eq!(text, "مرحباالعالم");
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

    fn line(text: &str, x: i32, y: i32, w: u32, h: u32, rtl: bool) -> OcrLine {
        OcrLine {
            text: text.into(),
            conf: 0.9,
            rect: Rect { x, y, w, h },
            rtl,
        }
    }

    #[test]
    fn test_join_rows_latin() {
        let lines = join_rows(vec![
            line("World", 80, 12, 60, 20, false),
            line("Next", 10, 40, 40, 20, false),
            line("Hello", 10, 10, 60, 20, false),
            line("  ", 10, 70, 40, 20, false),
        ]);
        let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, ["Hello World", "Next"]);
        assert_eq!(
            lines[0].rect,
            Rect {
                x: 10,
                y: 10,
                w: 130,
                h: 22
            }
        );
    }

    #[test]
    fn test_join_rows_rtl_runs_right_to_left() {
        let lines = join_rows(vec![
            line("العالم", 10, 10, 50, 20, true),
            line("مرحبا", 70, 10, 50, 20, true),
        ]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "مرحبا العالم");
        assert!(lines[0].rtl);
    }

    #[test]
    fn test_join_rows_confidence_is_per_char() {
        let mut a = line("aaa", 0, 0, 30, 10, false);
        a.conf = 1.0;
        let mut b = line("b", 40, 0, 10, 10, false);
        b.conf = 0.6;
        let lines = join_rows(vec![a, b]);
        assert!((lines[0].conf - 0.9).abs() < 1e-6);
    }
}
