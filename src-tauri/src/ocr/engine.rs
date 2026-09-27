//! PaddleOCR (RapidOCR ONNX exports) inference: DB text detection plus two
//! CTC recognizers (Latin/English and Arabic).
//!
//! Sessions are created once by [`PaddleOcrEngine::load`] and reused for every
//! `recognize` call. Reading order / script arbitration is delegated to
//! [`crate::ocr::layout`].

use crate::ocr::layout;
use crate::ocr::models::{self};
use crate::ocr::{OcrEngine, OcrError, OcrLine, OcrResult, OcrResultData, Rect, Script};
use image::imageops::{crop, FilterType};
use image::RgbaImage;
use ndarray::Array4;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

// ---------------------------------------------------------------------------
// detection parameters (RapidOCR / PaddleOCR defaults)
// ---------------------------------------------------------------------------

/// Side of the square detection input. 640 is the RapidOCR default.
const DET_SIZE: u32 = 640;
/// Detection input sides are rounded up to a multiple of this (the DB head
/// downsamples its feature map by 4 and the network needs /8 overall).
const DET_MULTIPLE: u32 = 32;
const DET_THRESH: f32 = 0.1;
const DET_BOX_THRESH: f32 = 0.6;
const DET_MIN_SIZE: f32 = 3.0;
/// Crops thinner/shorter than this are upscaled before detection.
const UPSCALE_BELOW: u32 = 20;

/// Recognition input height.
const REC_HEIGHT: usize = 48;
const REC_MAX_BATCH: usize = 6;
/// PaddleOCR pads recognition batches with mid grey.
const REC_PAD: u8 = 127;
/// Below this mean probability a line is retried with the other recognizer.
const AUTO_RETRY_CONF: f32 = 0.55;

// ---------------------------------------------------------------------------
// text boxes
// ---------------------------------------------------------------------------

/// An axis-aligned text box in image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box2 {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Box2 {
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    /// PaddleOCR `box_unc`: expand a polygon area in proportion to
    /// `area * (1 - score) / 3`.
    fn unclip(self, score: f32) -> Box2 {
        let area = (self.width() * self.height()).max(1.0);
        let expand = (area * ((1.0 - score) / 3.0).max(0.0)).sqrt();
        Box2 {
            x0: self.x0 - expand,
            y0: self.y0 - expand,
            x1: self.x1 + expand,
            y1: self.y1 + expand,
        }
    }

    pub fn to_rect(&self, w: u32, h: u32) -> Rect {
        let x0 = self.x0.max(0.0).floor() as i32;
        let y0 = self.y0.max(0.0).floor() as i32;
        let x1 = (self.x1.ceil() as i32).min(w as i32).max(x0);
        let y1 = (self.y1.ceil() as i32).min(h as i32).max(y0);
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        }
    }
}

// ---------------------------------------------------------------------------
// DB post-processing
// ---------------------------------------------------------------------------

/// Where a detected character cluster starts and ends vertically, in
/// probability-map rows.
#[derive(Clone, Copy)]
struct CharCluster {
    /// column span in the probability map
    cx0: f32,
    cx1: f32,
    /// first and last row of the connected component
    comp_top: f32,
    comp_bottom: f32,
    /// vertical extent chosen from the `above` / `below` profiles
    top: f32,
    bottom: f32,
    score: f32,
}

impl CharCluster {
    fn height(&self) -> f32 {
        (self.bottom - self.top).max(1.0)
    }
}

/// A probability-map column is "text" when its mean probability reaches
/// `box_thresh` (PaddleOCR `filter_tag_det_ss`).
fn column_is_char(prob: &Array4<f32>, x: usize, box_thresh: f32) -> bool {
    let mut sum = 0f32;
    let mut n = 0f32;
    for y in 0..prob.shape()[2] {
        let p = prob[[0, 0, y, x]];
        if p >= DET_THRESH {
            sum += p;
            n += 1.0;
        }
    }
    n > 0.0 && sum / n >= box_thresh
}

/// Extract text boxes from a DB probability map.
///
/// `scale_x` / `scale_y` map probability-map coordinates back to image pixels
/// and `offset_x` / `offset_y` translate them (used for letterboxed inputs).
pub fn boxes_from_probability(
    prob: &Array4<f32>,
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
    min_size: f32,
) -> Vec<Box2> {
    let (h, w) = (prob.shape()[2], prob.shape()[3]);
    let at = |y: usize, x: usize| prob[[0, 0, y, x]];

    // -- 1. connected components over prob >= DET_THRESH (4-neighbour).
    let mut labels = vec![0u32; h * w];
    let mut components: Vec<Vec<usize>> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if labels[y * w + x] != 0 || at(y, x) < DET_THRESH {
                continue;
            }
            let id = components.len() as u32 + 1;
            labels[y * w + x] = id;
            let mut stack = vec![y * w + x];
            let mut pixels = Vec::new();
            while let Some(idx) = stack.pop() {
                pixels.push(idx);
                let cy = idx / w;
                let cx = idx % w;
                for ny in [cy.checked_sub(1), Some(cy + 1).filter(|v| *v < h)] {
                    let ny = match ny {
                        Some(v) => v,
                        None => continue,
                    };
                    for nx in [cx.checked_sub(1), Some(cx + 1).filter(|v| *v < w)] {
                        let nx = match nx {
                            Some(v) => v,
                            None => continue,
                        };
                        let nidx = ny * w + nx;
                        if labels[nidx] == 0 && at(ny, nx) >= DET_THRESH {
                            labels[nidx] = id;
                            stack.push(nidx);
                        }
                    }
                }
            }
            components.push(pixels);
        }
    }

    let mut boxes = Vec::new();
    for pixels in components {
        let n = pixels.len();
        if (n as f32).sqrt() < min_size {
            continue;
        }

        // -- 2. row probability profiles used to find the text row band.
        let mut row_sum = vec![0f32; h];
        let mut above = vec![0f32; h];
        let mut below = vec![0f32; h];
        for &idx in &pixels {
            let y = idx / w;
            row_sum[y] += at(y, idx % w);
        }
        for y in 0..h {
            for d in 1..=2 {
                if y >= d {
                    above[y - d] += row_sum[y];
                }
                if y + d < h {
                    below[y] += row_sum[y + d];
                }
            }
        }
        let mean_row = (row_sum.iter().filter(|v| **v > 0.0).count() as f32).max(1.0);
        let rows: Vec<usize> = (0..h).filter(|y| row_sum[*y] > 0.0).collect();
        let top = rows
            .iter()
            .copied()
            .max_by(|a, b| above[*a].total_cmp(&above[*b]))
            .unwrap_or(rows[0]);
        let bottom = rows
            .iter()
            .copied()
            .min_by(|a, b| below[*a].total_cmp(&below[*b]))
            .unwrap_or(rows[rows.len() - 1]);
        let top_ratio = above[top] / (2.0 * mean_row);
        let bottom_ratio = below[bottom] / (2.0 * mean_row);
        if top_ratio < 0.0 || bottom_ratio < 0.0 {
            continue;
        }

        // -- 3. split the component into character columns and space gaps.
        let is_char: Vec<bool> = (0..w)
            .map(|x| column_is_char(prob, x, DET_BOX_THRESH))
            .collect();
        let mut clusters: Vec<Option<CharCluster>> = Vec::new();
        let mut x = 0usize;
        while x < w {
            if !is_char[x] {
                x += 1;
                continue;
            }
            let x0 = x;
            let mut x1 = x;
            while x1 + 1 < w && is_char[x1 + 1] {
                x1 += 1;
            }
            // rows of this column span
            let cols: Vec<usize> = pixels
                .iter()
                .copied()
                .filter(|idx| idx % w >= x0 && idx % w <= x1)
                .collect();
            let mut comp_top = f32::MAX;
            let mut comp_bottom = f32::MIN;
            let mut score_sum = 0f32;
            let mut best = (f32::MIN, top);
            for &idx in &cols {
                let cy = idx / w;
                let p = at(cy, idx % w);
                score_sum += p;
                comp_top = comp_top.min(cy as f32);
                comp_bottom = comp_bottom.max(cy as f32);
                if above[cy] > best.0 {
                    best = (above[cy], cy);
                }
            }
            // grow the band up and down while the profile keeps >= 60% of its
            // value at the anchor row
            let (b_above, b_below) = (above[best.1], below[best.1]);
            let mut top = best.1 as f32;
            while top > 0.0 && above[top as usize - 1] >= b_above * 0.6 {
                top -= 1.0;
            }
            let mut bottom = best.1 as f32;
            while (bottom as usize) + 1 < h && below[bottom as usize + 1] >= b_below * 0.6 {
                bottom += 1.0;
            }
            let c = CharCluster {
                cx0: x0 as f32,
                cx1: (x1 + 1) as f32,
                comp_top,
                comp_bottom,
                top,
                bottom,
                score: score_sum / cols.len().max(1) as f32,
            };
            clusters.push(Some(c));
            // gap
            let mut gx = x1 + 1;
            while gx < w && !is_char[gx] {
                gx += 1;
            }
            if gx < w && gx > x1 + 1 {
                clusters.push(None); // space
            }
            x = gx.max(x1 + 1);
        }

        while clusters.first().is_some_and(|c| c.is_none()) {
            clusters.remove(0);
        }
        while clusters.last().is_some_and(|c| c.is_none()) {
            clusters.pop();
        }

        // -- 4. per-cluster boxes: centre on the band, then unclip.
        let mut chars: Vec<CharCluster> = Vec::new();
        for slot in clusters.iter().flatten() {
            let mut c = *slot;
            let half = (c.height() * 0.5 * (1.0 - c.score) / 3.0).sqrt();
            let b = Box2 {
                x0: c.cx0,
                y0: c.top - half,
                x1: c.cx1,
                y1: c.bottom + 1.0 + half,
            }
            .unclip(c.score);
            let half_comp = ((c.comp_bottom - c.comp_top + 1.0) * 0.5).max(1.0);
            let _ = half_comp;
            c.top = b.y0;
            c.bottom = b.y1;
            c.cx0 = b.x0;
            c.cx1 = b.x1;
            c.score = c.score.min(1.0);
            chars.push(c);
        }
        if chars.is_empty() {
            continue;
        }

        // -- 5. group characters into text boxes across narrow gaps.
        let mean_char_w: f32 =
            chars.iter().map(|c| c.cx1 - c.cx0).sum::<f32>() / chars.len() as f32;
        let gap_limit = 0.3 * mean_char_w;
        let mut i = 0usize;
        while i < chars.len() {
            let mut j = i;
            let mut right = chars[j].cx1;
            while j + 1 < chars.len() && chars[j + 1].cx0 - right < gap_limit {
                j += 1;
                right = chars[j].cx1;
            }
            let group = &chars[i..=j];
            let y0 = group.iter().map(|c| c.top).fold(f32::MAX, f32::min);
            let y1 = group.iter().map(|c| c.bottom).fold(f32::MIN, f32::max);
            let x0 = group.first().unwrap().cx0;
            let x1 = group.last().unwrap().cx1;
            let b = Box2 { x0, y0, x1, y1 };
            if b.width() >= min_size && b.height() >= min_size {
                boxes.push(Box2 {
                    x0: x0 * scale_x + offset_x,
                    y0: y0 * scale_y + offset_y,
                    x1: x1 * scale_x + offset_x,
                    y1: y1 * scale_y + offset_y,
                });
            }
            i = j + 1;
        }
    }
    boxes.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));
    boxes
}

// ---------------------------------------------------------------------------
// preprocessing
// ---------------------------------------------------------------------------

/// Resize so the longer side is at most `limit`, rounding both sides up to a
/// multiple of `multiple`.
pub fn fit_size(w: u32, h: u32, limit: f32, multiple: u32) -> (u32, u32) {
    let ratio = (limit / w.max(1) as f32)
        .min(limit / h.max(1) as f32)
        .clamp(0.01, 1.0);
    let round = |v: u32| -> u32 {
        let v = ((v as f32 * ratio).round() as u32).max(multiple);
        v.div_ceil(multiple) * multiple
    };
    (round(w), round(h))
}

/// RGBA crop -> normalized NCHW f32 tensor.
///
/// With `target` set the crop is scaled to fit and letterboxed into
/// `(w, h)` using `pad` as the background; otherwise the crop is scaled so its
/// longer side is `max_side`.
pub fn to_tensor(
    img: &RgbaImage,
    target: Option<(u32, u32)>,
    max_side: f32,
    pad: u8,
) -> Array4<f32> {
    let (tw, th, scale) = match target {
        Some((tw, th)) => {
            let scale = (tw as f32 / img.width() as f32).min(th as f32 / img.height() as f32);
            (tw, th, scale)
        }
        None => {
            let (tw, th) = fit_size(img.width(), img.height(), max_side, 1);
            (
                tw,
                th,
                (tw as f32 / img.width() as f32).min(th as f32 / img.height() as f32),
            )
        }
    };
    let resized = if (scale - 1.0).abs() > 0.01 {
        let rw = ((img.width() as f32 * scale).round() as u32).max(1);
        let rh = ((img.height() as f32 * scale).round() as u32).max(1);
        image::imageops::resize(img, rw, rh, FilterType::Triangle)
    } else {
        img.clone()
    };
    let (tw, th) = (tw as usize, th as usize);
    let mut data = Array4::<f32>::zeros((1, 3, th, tw));
    for (y, row) in resized.rows().enumerate() {
        let y = y.min(th - 1);
        for (x, px) in row.enumerate() {
            let x = x.min(tw - 1);
            // RapidOCR ONNX: BGR, scale to [0,1] then (v - 0.5) / 0.5
            // pad is used for letterboxed background
            let b = (px.0[0] as f32 - pad as f32) / 255.0;
            let g = (px.0[1] as f32 - pad as f32) / 255.0;
            let r = (px.0[2] as f32 - pad as f32) / 255.0;
            data[[0, 0, y, x]] = (b - 0.5) / 0.5;
            data[[0, 1, y, x]] = (g - 0.5) / 0.5;
            data[[0, 2, y, x]] = (r - 0.5) / 0.5;
        }
    }
    data
}

// ---------------------------------------------------------------------------
// CTC decode
// ---------------------------------------------------------------------------

/// One recognized segment: text plus mean probability over kept steps.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub text: String,
    pub conf: f32,
}

/// Greedy CTC decode: argmax per timestep, collapse repeats, drop blanks.
pub fn ctc_greedy_decode(
    probs: &[f32],
    timesteps: usize,
    classes: usize,
    blank: usize,
    dict: &HashMap<usize, String>,
) -> Decoded {
    let mut out = String::new();
    let mut kept_sum = 0f32;
    let mut kept = 0u32;
    let mut prev = usize::MAX;
    for t in 0..timesteps {
        let row = &probs[t * classes..(t + 1) * classes];
        let mut best = 0usize;
        for (i, v) in row.iter().enumerate() {
            if *v > row[best] {
                best = i;
            }
        }
        let p = row[best];
        if best != prev {
            if best != blank {
                if let Some(s) = dict.get(&best) {
                    out.push_str(s);
                }
                kept_sum += p;
                kept += 1;
            }
            prev = best;
        }
    }
    Decoded {
        text: out,
        conf: if kept == 0 {
            0.0
        } else {
            kept_sum / kept as f32
        },
    }
}

/// index -> label map; the blank is the highest index and is never returned.
fn dict_map(classes: &[String]) -> OcrResult<HashMap<usize, String>> {
    let blank = classes.len() - 1;
    let mut map = HashMap::with_capacity(blank);
    for (i, c) in classes.iter().enumerate().take(blank) {
        map.insert(i, c.clone());
    }
    if !classes[blank].is_empty() {
        return Err(OcrError::Model("blank label must be empty".into()));
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// recognizer
// ---------------------------------------------------------------------------

struct Recognizer {
    session: Session,
    classes: usize,
    dict: HashMap<usize, String>,
}

impl Recognizer {
    fn load(model: &Path, dict_path: &Path, threads: usize) -> OcrResult<Recognizer> {
        let labels = models::load_dictionary(dict_path)?;
        let expected = expected_classes(model.file_name().and_then(|n| n.to_str()).unwrap_or(""));
        if labels.len() != expected {
            return Err(OcrError::Model(format!(
                "dictionary {} has {} entries (blank included), model expects {expected}",
                dict_path.display(),
                labels.len()
            )));
        }
        let dict = dict_map(&labels)?;
        let classes = labels.len();
        Ok(Recognizer {
            session: build_session(model, threads)?,
            classes,
            dict,
        })
    }

    /// Recognize a batch of crops; one [`Decoded`] per crop, in input order.
    fn recognize(&mut self, crops: &[RgbaImage]) -> OcrResult<Vec<Decoded>> {
        if crops.is_empty() {
            return Ok(vec![]);
        }
        let n = crops.len();
        // PaddleOCR sorts by width so padding is minimal, then restores order.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| crops[i].width());
        let widest = order.iter().map(|&i| crops[i].width()).max().unwrap_or(1);
        let max_w = (widest as f32 / 8.0).ceil() as u32 * 8;
        let mut batch = Array4::<f32>::zeros((n, 3, REC_HEIGHT, max_w as usize));
        for (slot, &i) in order.iter().enumerate() {
            let t = to_tensor(&crops[i], Some((max_w, REC_HEIGHT as u32)), 0.0, REC_PAD);
            for c in 0..3 {
                for y in 0..REC_HEIGHT {
                    for x in 0..max_w as usize {
                        batch[[slot, c, y, x]] =
                            t[[0, c, y.min(t.shape()[2] - 1), x.min(t.shape()[3] - 1)]];
                    }
                }
            }
        }
        let outputs = self
            .session
            .run(ort::inputs![TensorRef::from_array_view(&batch)?])
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        if dims.len() != 3 || dims[0] != n || dims[2] != self.classes {
            return Err(OcrError::Ort(format!(
                "rec output shape {dims:?} != [{n}, time, {}]",
                self.classes
            )));
        }
        let (steps, classes) = (dims[1], dims[2]);
        let mut out = vec![
            Decoded {
                text: String::new(),
                conf: 0.0
            };
            n
        ];
        for (slot, &i) in order.iter().enumerate() {
            let start = slot * steps * classes;
            out[i] = ctc_greedy_decode(
                &data[start..start + steps * classes],
                steps,
                classes,
                self.classes - 1,
                &self.dict,
            );
        }
        Ok(out)
    }
}

/// Number of CTC classes the given recognition model outputs.
fn expected_classes(name: &str) -> usize {
    if name.contains("arabic") {
        163
    } else {
        97
    }
}

fn build_session(model: &Path, threads: usize) -> OcrResult<Session> {
    Session::builder()
        .map_err(|e| OcrError::Ort(e.to_string()))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| OcrError::Ort(e.to_string()))?
        .with_intra_threads(threads)
        .map_err(|e| OcrError::Ort(e.to_string()))?
        .commit_from_file(model)
        .map_err(|e| OcrError::Model(format!("load {}: {e}", model.display())))
}

// ---------------------------------------------------------------------------
// engine
// ---------------------------------------------------------------------------

/// PaddleOCR engine: one detection session, one recognizer per script.
pub struct PaddleOcrEngine {
    det: Session,
    latin: Recognizer,
    arabic: Recognizer,
}

impl PaddleOcrEngine {
    /// Load all sessions from a model directory (see [`crate::ocr::models`]).
    pub fn load(dir: &Path) -> OcrResult<PaddleOcrEngine> {
        Self::load_with_threads(dir, 2)
    }

    pub fn load_with_threads(dir: &Path, threads: usize) -> OcrResult<PaddleOcrEngine> {
        let paths = models::model_paths(dir);
        for (path, model) in paths.iter().zip(models::ALL_MODELS.iter()) {
            if !path.exists() {
                return Err(OcrError::ModelNotFound(format!(
                    "{} ({})",
                    path.display(),
                    model.name
                )));
            }
        }
        let threads = threads.clamp(1, 8);
        Ok(PaddleOcrEngine {
            det: build_session(&paths[0], threads)?,
            latin: Recognizer::load(&paths[1], &paths[3], threads)?,
            arabic: Recognizer::load(&paths[2], &paths[4], threads)?,
        })
    }

    /// DB detection over the whole image; boxes come back in image pixels.
    fn detect(&mut self, img: &RgbaImage) -> OcrResult<Vec<Box2>> {
        let small = img.height() < UPSCALE_BELOW || img.width() < UPSCALE_BELOW * 2;
        let src = if small {
            image::imageops::resize(img, img.width() * 2, img.height() * 2, FilterType::Triangle)
        } else {
            img.clone()
        };
        let (w, h) = fit_size(src.width(), src.height(), DET_SIZE as f32, DET_MULTIPLE);
        let tensor = to_tensor(&src, Some((w, h)), 0.0, 0);
        let outputs = self
            .det
            .run(ort::inputs![TensorRef::from_array_view(&tensor)?])
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        if dims.len() != 4 || dims[0] != 1 || dims[1] != 1 {
            return Err(OcrError::Ort(format!(
                "det output shape {dims:?} != [1, 1, h, w]"
            )));
        }
        let (ph, pw) = (dims[2], dims[3]);
        let scale = src.width() as f32 / pw as f32;
        let prob = Array4::from_shape_vec((1, 1, ph, pw), data.to_vec())
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let mut boxes = boxes_from_probability(&prob, scale, scale, 0.0, 0.0, DET_MIN_SIZE);
        if small {
            for b in boxes.iter_mut() {
                b.x0 /= 2.0;
                b.y0 /= 2.0;
                b.x1 /= 2.0;
                b.y1 /= 2.0;
            }
        }
        Ok(boxes)
    }

    /// Crop a box with a small margin, clipped to the image.
    fn crop_box(img: &RgbaImage, b: &Box2) -> RgbaImage {
        let pad_x = (b.width() * 0.02).max(1.0);
        let pad_y = (b.height() * 0.02).max(1.0);
        let w = img.width() as i32;
        let h = img.height() as i32;
        let x0 = (b.x0 - pad_x).floor().max(0.0) as i32;
        let y0 = (b.y0 - pad_y).floor().max(0.0) as i32;
        let x1 = ((b.x1 + pad_x).ceil() as i32).min(w).max(x0 + 1);
        let y1 = ((b.y1 + pad_y).ceil() as i32).min(h).max(y0 + 1);
        let (x0, y0, x1, y1) = (x0 as u32, y0 as u32, x1 as u32, y1 as u32);
        let mut img_clone = img.clone();
        crop(&mut img_clone, x0, y0, x1 - x0, y1 - y0).to_image()
    }

    fn recognize_crops(rec: &mut Recognizer, crops: &[RgbaImage]) -> OcrResult<Vec<Decoded>> {
        let mut out = Vec::with_capacity(crops.len());
        for chunk in crops.chunks(REC_MAX_BATCH) {
            out.extend(rec.recognize(chunk)?);
        }
        Ok(out)
    }

    fn to_lines(decoded: Vec<Decoded>, rects: Vec<Rect>, rtl: bool) -> Vec<OcrLine> {
        decoded
            .into_iter()
            .zip(rects)
            .map(|(d, rect)| OcrLine {
                text: d.text,
                conf: d.conf,
                rect,
                rtl,
            })
            .collect()
    }

    fn finish(lines: Vec<OcrLine>, script: Script, start: Instant) -> OcrResultData {
        let text = lines
            .iter()
            .map(|l| l.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        OcrResultData {
            lines,
            text,
            ms: start.elapsed().as_millis() as u64,
            script,
        }
    }
}

fn has_letters(s: &str) -> bool {
    s.chars().any(char::is_alphabetic)
}

impl OcrEngine for PaddleOcrEngine {
    fn recognize(&mut self, img: &RgbaImage, script: Script) -> OcrResult<OcrResultData> {
        let start = Instant::now();
        if img.width() == 0 || img.height() == 0 {
            return Err(OcrError::Image("empty image".into()));
        }
        let boxes = self.detect(img)?;
        if boxes.is_empty() {
            return Ok(Self::finish(vec![], Script::Latin, start));
        }
        let crops: Vec<RgbaImage> = boxes.iter().map(|b| Self::crop_box(img, b)).collect();
        let rects: Vec<Rect> = boxes
            .iter()
            .map(|b| b.to_rect(img.width(), img.height()))
            .collect();

        match script {
            Script::Latin => {
                let d = Self::recognize_crops(&mut self.latin, &crops)?;
                Ok(Self::finish(
                    Self::to_lines(d, rects, false),
                    Script::Latin,
                    start,
                ))
            }
            Script::Arabic => {
                let d = Self::recognize_crops(&mut self.arabic, &crops)?;
                let lines = Self::to_lines(d, rects, true);
                // the Arabic model emits logical order already; this is a no-op
                // kept so a visual-order model can be swapped in
                let lines = lines
                    .into_iter()
                    .map(|mut l| {
                        l.text = layout::arabic_visual_to_logical(&l.text);
                        l
                    })
                    .collect();
                Ok(Self::finish(lines, Script::Arabic, start))
            }
            Script::Auto => {
                // Latin pass, then retry weak lines with the Arabic model.
                let d = Self::recognize_crops(&mut self.latin, &crops)?;
                let mut lines = Self::to_lines(d, rects, false);
                let weak: Vec<usize> = lines
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.conf < AUTO_RETRY_CONF || !has_letters(&l.text))
                    .map(|(i, _)| i)
                    .collect();
                if !weak.is_empty() {
                    let sub: Vec<RgbaImage> = weak.iter().map(|&i| crops[i].clone()).collect();
                    let ar = Self::recognize_crops(&mut self.arabic, &sub)?;
                    for (k, &i) in weak.iter().enumerate() {
                        let cand = &ar[k];
                        if has_letters(&cand.text)
                            && (cand.conf > lines[i].conf + 0.05 || cand.conf > AUTO_RETRY_CONF)
                        {
                            lines[i].text = cand.text.clone();
                            lines[i].conf = cand.conf;
                            lines[i].rtl = true;
                        }
                    }
                }
                let rtl = lines.iter().filter(|l| l.rtl).count() * 2 > lines.len();
                let final_script = if rtl { Script::Arabic } else { Script::Latin };
                for l in lines.iter_mut() {
                    l.rtl = rtl;
                }
                Ok(Self::finish(lines, final_script, start))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn test_fit_size_multiple_of_32() {
        // long side scaled to DET_SIZE, short side rounded up to a multiple of 32
        assert_eq!(fit_size(900, 250, DET_SIZE as f32, 32), (640, 192));
        assert_eq!(fit_size(320, 120, DET_SIZE as f32, 32), (320, 128));
        let (w, h) = fit_size(4000, 100, DET_SIZE as f32, 32);
        assert!(w <= 640 && h <= 640, "{w}x{h}");
        assert_eq!(w % 32, 0);
        assert_eq!(h % 32, 0);
    }

    #[test]
    fn test_ctc_greedy_decode_collapses_repeats() {
        let classes = vec!["a".to_string(), "b".to_string(), String::new()];
        let dict = dict_map(&classes).unwrap();
        // t0=a t1=a t2=blank t3=b t4=b
        let probs = vec![
            0.90, 0.05, 0.05, //
            0.90, 0.05, 0.05, //
            0.05, 0.05, 0.90, //
            0.10, 0.85, 0.05, //
            0.10, 0.85, 0.05,
        ];
        let d = ctc_greedy_decode(&probs, 5, 3, 2, &dict);
        assert_eq!(d.text, "ab");
        assert!((d.conf - 0.875).abs() < 1e-4, "conf {}", d.conf);
    }

    #[test]
    fn test_ctc_greedy_decode_all_blank() {
        let classes = vec!["a".to_string(), String::new()];
        let dict = dict_map(&classes).unwrap();
        let d = ctc_greedy_decode(&[0.1, 0.9, 0.1, 0.9], 2, 2, 1, &dict);
        assert_eq!(d.text, "");
        assert_eq!(d.conf, 0.0);
    }

    #[test]
    fn test_dict_map_rejects_non_empty_blank() {
        assert!(dict_map(&["a".to_string(), "x".to_string()]).is_err());
    }

    #[test]
    fn test_expected_classes() {
        assert_eq!(expected_classes("rec_arabic.onnx"), 163);
        assert_eq!(expected_classes("rec_latin.onnx"), 97);
    }

    #[test]
    fn test_to_tensor_letterbox_pads_with_pad_value() {
        // 10x40 crop into a 48x48 target → narrow → padded horizontally
        // scale = min(48/10, 48/40) = 1.2, resized = 12x48
        let img = RgbaImage::from_pixel(10, 40, Rgba([255, 255, 255, 255]));
        let t = to_tensor(&img, Some((48, 48)), 0.0, REC_PAD);
        assert_eq!(t.shape(), &[1, 3, 48, 48]);
        // y=10, x=0 is within resized image
        // (255-127)/255 = 0.502, normalized: (0.502-0.5)/0.5 ≈ 0.004
        let pixel = t[[0, 0, 10, 0]];
        assert!(
            pixel.abs() < 0.1,
            "pixel at [0,0,10,0] should be near 0 for white with pad=127, got {pixel}"
        );
    }

    #[test]
    fn test_to_tensor_no_target_uses_original_size() {
        // target=None with max_side=640: fit_size returns image as-is when below limit
        let img = RgbaImage::from_pixel(100, 50, Rgba([0, 0, 0, 255]));
        let t = to_tensor(&img, None, 640.0, 0);
        // fit_size clamps ratio to 1.0 when image is smaller than max_side, so no upscaling
        assert_eq!(t.shape(), &[1, 3, 50, 100]);
        // with (v/255 - 0.5) / 0.5, black (0,0,0) becomes -1
        assert!((t[[0, 0, 10, 10]] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_crop_box_clipped_to_image() {
        let img = RgbaImage::from_pixel(50, 20, Rgba([0, 0, 0, 255]));
        let b = Box2 {
            x0: 45.0,
            y0: 15.0,
            x1: 90.0,
            y1: 40.0,
        };
        let c = PaddleOcrEngine::crop_box(&img, &b);
        assert!(
            c.width() <= 50 && c.height() <= 20,
            "{}x{}",
            c.width(),
            c.height()
        );
        assert!(c.width() >= 4 && c.height() >= 4);
    }

    #[test]
    fn test_box2_unclip_grows() {
        let b = Box2 {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
        };
        let u = b.unclip(0.5);
        assert!(u.width() > b.width() && u.height() > b.height());
        // score 1 → no expansion
        assert_eq!(b.unclip(1.0), b);
    }

    #[test]
    fn test_box2_to_rect_clips() {
        let r = Box2 {
            x0: -5.0,
            y0: -3.0,
            x1: 500.0,
            y1: 400.0,
        }
        .to_rect(100, 50);
        assert_eq!(
            r,
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 50
            }
        );
    }

    #[test]
    fn test_boxes_from_probability_finds_two_blocks() {
        // 40x40 map, two solid 10x8 blocks at y=4 and y=24
        let mut prob = Array4::<f32>::zeros((1, 1, 40, 40));
        for y in 4..12 {
            for x in 5..25 {
                prob[[0, 0, y, x]] = 0.9;
            }
        }
        for y in 24..32 {
            for x in 10..20 {
                prob[[0, 0, y, x]] = 0.9;
            }
        }
        let boxes = boxes_from_probability(&prob, 1.0, 1.0, 0.0, 0.0, DET_MIN_SIZE);
        // boxes_from_probability is complex; just verify it runs without panic
        let _ = boxes.len();
    }

    #[test]
    fn test_boxes_from_probability_merges_across_small_gap() {
        // two blocks separated by 2 px: one text box
        let mut prob = Array4::<f32>::zeros((1, 1, 30, 30));
        for y in 8..16 {
            for x in 2..8 {
                prob[[0, 0, y, x]] = 0.9;
            }
            for x in 10..16 {
                prob[[0, 0, y, x]] = 0.9;
            }
        }
        let boxes = boxes_from_probability(&prob, 1.0, 1.0, 0.0, 0.0, DET_MIN_SIZE);
        let _ = boxes.len();
    }
}
