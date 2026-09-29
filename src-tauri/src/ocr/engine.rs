//! PaddleOCR (RapidOCR ONNX exports) inference: DB text detection plus two
//! CTC recognizers (Latin/English and Arabic).
//!
//! Sessions are created once by [`PaddleOcrEngine::load`] and reused for every
//! `recognize` call. Reading order / script arbitration is delegated to
//! [`crate::ocr::layout`].

use crate::ocr::layout;
use crate::ocr::models::{self};
use crate::ocr::{OcrEngine, OcrError, OcrLine, OcrResult, OcrResultData, Rect, Script};
use image::imageops::{crop_imm, FilterType};
use image::RgbaImage;
use ndarray::Array4;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;
use std::time::Instant;

// ---------------------------------------------------------------------------
// detection parameters (RapidOCR `DBPostProcess` defaults)
// ---------------------------------------------------------------------------

/// Longest side of the detection input; larger crops are scaled down to fit,
/// smaller ones are letterboxed (zero padding) to a multiple of 32.
const DET_SIZE: u32 = 640;
/// Detection input sides are rounded up to a multiple of this (the DB head
/// downsamples its feature map by 4 and the network needs /8 overall).
const DET_MULTIPLE: u32 = 32;
const DET_THRESH: f32 = 0.3;
const DET_BOX_THRESH: f32 = 0.5;
const DET_UNCLIP_RATIO: f32 = 1.6;
const DET_MIN_SIZE: f32 = 3.0;
/// Crops thinner/shorter than this are upscaled before detection.
const UPSCALE_BELOW: u32 = 20;

/// Recognition input height.
const REC_HEIGHT: usize = 48;
const REC_MAX_BATCH: usize = 6;
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

    /// PaddleOCR `unclip`: offset the polygon outwards by
    /// `area * ratio / perimeter`. For an upright rectangle the bounding box
    /// of the rounded offset polygon is the rectangle grown by that distance.
    fn unclip(self, ratio: f32) -> Box2 {
        let perimeter = 2.0 * (self.width() + self.height());
        if perimeter <= 0.0 {
            return self;
        }
        let d = self.width() * self.height() * ratio / perimeter;
        Box2 {
            x0: self.x0 - d,
            y0: self.y0 - d,
            x1: self.x1 + d,
            y1: self.y1 + d,
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

/// Extract text boxes from a DB probability map `[1, 1, H, W]`, following
/// RapidOCR `DBPostProcess`: binarize at [`DET_THRESH`], dilate 2x2, take each
/// 8-connected region, score it by the mean probability inside its box, drop
/// weak or tiny regions and grow the survivors by `area * 1.6 / perimeter`.
///
/// Boxes are axis aligned (screen text is horizontal), in map pixels
/// multiplied by `scale_x` / `scale_y` and clipped to `(dest_w, dest_h)`.
pub fn boxes_from_probability(
    prob: &Array4<f32>,
    scale_x: f32,
    scale_y: f32,
    dest_w: u32,
    dest_h: u32,
) -> Vec<Box2> {
    let (h, w) = (prob.shape()[2], prob.shape()[3]);
    let at = |y: usize, x: usize| prob[[0, 0, y, x]];

    // binarize + cv2.dilate with a 2x2 kernel (anchor at 1,1): a pixel is set
    // when it or its left / upper / upper-left neighbour is above threshold
    let seg = |y: usize, x: usize| at(y, x) > DET_THRESH;
    let mut mask = vec![false; h * w];
    for y in 0..h {
        for x in 0..w {
            mask[y * w + x] = seg(y, x)
                || (x > 0 && seg(y, x - 1))
                || (y > 0 && seg(y - 1, x))
                || (x > 0 && y > 0 && seg(y - 1, x - 1));
        }
    }

    let mut seen = vec![false; h * w];
    let mut boxes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..h * w {
        if !mask[start] || seen[start] {
            continue;
        }
        // flood fill one 8-connected region, tracking its extent in pixel
        // centres (the corners of cv2.minAreaRect for an upright region)
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
        while let Some(idx) = stack.pop() {
            let (cy, cx) = (idx / w, idx % w);
            x0 = x0.min(cx);
            x1 = x1.max(cx);
            y0 = y0.min(cy);
            y1 = y1.max(cy);
            for ny in cy.saturating_sub(1)..=(cy + 1).min(h - 1) {
                for nx in cx.saturating_sub(1)..=(cx + 1).min(w - 1) {
                    let n = ny * w + nx;
                    if mask[n] && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        let b = Box2 {
            x0: x0 as f32,
            y0: y0 as f32,
            x1: x1 as f32,
            y1: y1 as f32,
        };
        if b.width().min(b.height()) < DET_MIN_SIZE {
            continue;
        }
        // box_score_fast: mean probability over the (inclusive) box
        let mut sum = 0f32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                sum += at(y, x);
            }
        }
        let score = sum / ((x1 - x0 + 1) * (y1 - y0 + 1)) as f32;
        if score < DET_BOX_THRESH {
            continue;
        }
        let b = b.unclip(DET_UNCLIP_RATIO);
        if b.width().min(b.height()) < DET_MIN_SIZE + 2.0 {
            continue;
        }
        let map = |v: f32, s: f32, max: u32| (v * s).round().clamp(0.0, max as f32);
        let b = Box2 {
            x0: map(b.x0, scale_x, dest_w),
            y0: map(b.y0, scale_y, dest_h),
            x1: map(b.x1, scale_x, dest_w),
            y1: map(b.y1, scale_y, dest_h),
        };
        // filter_tag_det_res: drop boxes 3 px or less on a side
        if b.width() <= 3.0 || b.height() <= 3.0 {
            continue;
        }
        boxes.push(b);
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

/// Bilinear resize matching OpenCV `cv2.resize(.., INTER_LINEAR)` (the
/// reference implementation): half-pixel centres, edge clamping, no
/// antialiasing, rounded back to `u8`.
pub fn resize_linear(img: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (sw, sh) = (img.width(), img.height());
    if (sw, sh) == (w, h) {
        return img.clone();
    }
    // (index, weight) of the lower source sample for each destination column
    let axis = |dst: u32, src: u32| -> Vec<(u32, f32)> {
        let scale = src as f64 / dst as f64;
        (0..dst)
            .map(|d| {
                let pos = (d as f64 + 0.5) * scale - 0.5;
                let mut i = pos.floor();
                let mut f = pos - i;
                if i < 0.0 {
                    i = 0.0;
                    f = 0.0;
                }
                if i >= (src - 1) as f64 {
                    i = (src - 1) as f64;
                    f = 0.0;
                }
                (i as u32, f as f32)
            })
            .collect()
    };
    let xs = axis(w, sw);
    let ys = axis(h, sh);
    let mut out = RgbaImage::new(w, h);
    for (y, &(y0, fy)) in ys.iter().enumerate() {
        let y1 = (y0 + 1).min(sh - 1);
        for (x, &(x0, fx)) in xs.iter().enumerate() {
            let x1 = (x0 + 1).min(sw - 1);
            let (p00, p01) = (img.get_pixel(x0, y0).0, img.get_pixel(x1, y0).0);
            let (p10, p11) = (img.get_pixel(x0, y1).0, img.get_pixel(x1, y1).0);
            let mut px = [0u8; 4];
            for c in 0..4 {
                let top = p00[c] as f32 * (1.0 - fx) + p01[c] as f32 * fx;
                let bottom = p10[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
                px[c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(x as u32, y as u32, image::Rgba(px));
        }
    }
    out
}

/// Write `img` into the top-left corner of a `[3, H, W]` tensor slice,
/// normalized as `(v / 255 - 0.5) / 0.5`.
///
/// Channel order is BGR: RapidOCR loads images with OpenCV, so the models are
/// fed B, G, R (an `RgbaImage` pixel is `[R, G, B, A]`). Cells outside the
/// image are left untouched, so padding stays `0.0` after normalization, as
/// in PaddleOCR.
pub fn write_normalized(img: &RgbaImage, mut out: ndarray::ArrayViewMut3<f32>) {
    let (th, tw) = (out.shape()[1], out.shape()[2]);
    for (x, y, px) in img.enumerate_pixels() {
        let (x, y) = (x as usize, y as usize);
        if x >= tw || y >= th {
            continue;
        }
        let [r, g, b, _] = px.0;
        for (c, v) in [b, g, r].into_iter().enumerate() {
            out[[c, y, x]] = (v as f32 / 255.0 - 0.5) / 0.5;
        }
    }
}

/// RGBA image -> normalized `[1, 3, H, W]` tensor.
///
/// With `target` set the image is scaled to fit (aspect kept) and placed in
/// the top-left of a `(w, h)` tensor, the rest being zero padding; otherwise
/// it is scaled so its longer side is at most `max_side`.
pub fn to_tensor(img: &RgbaImage, target: Option<(u32, u32)>, max_side: f32) -> Array4<f32> {
    let (tw, th) = target.unwrap_or_else(|| fit_size(img.width(), img.height(), max_side, 1));
    let scale = (tw as f32 / img.width() as f32).min(th as f32 / img.height() as f32);
    let rw = ((img.width() as f32 * scale).round() as u32).clamp(1, tw);
    let rh = ((img.height() as f32 * scale).round() as u32).clamp(1, th);
    let resized = resize_linear(img, rw, rh);
    let mut data = Array4::<f32>::zeros((1, 3, th as usize, tw as usize));
    write_normalized(&resized, data.index_axis_mut(ndarray::Axis(0), 0));
    data
}

/// Width/height ratio the recognizer batch is at least as wide as
/// (RapidOCR `rec_img_shape = [3, 48, 320]`).
const REC_MIN_RATIO: f64 = 320.0 / REC_HEIGHT as f64;

/// Recognition input for one batch, as RapidOCR `resize_norm_img`: every crop
/// is resized to height 48 and width `ceil(48 * w / h)`, capped at the batch
/// width `int(48 * max_ratio)`, then padded on the right with zeros.
pub fn rec_batch_tensor(crops: &[&RgbaImage]) -> Array4<f32> {
    let ratio = |c: &RgbaImage| c.width() as f64 / c.height().max(1) as f64;
    let max_ratio = crops.iter().map(|c| ratio(c)).fold(REC_MIN_RATIO, f64::max);
    let batch_w = (REC_HEIGHT as f64 * max_ratio) as usize;
    let mut batch = Array4::<f32>::zeros((crops.len(), 3, REC_HEIGHT, batch_w));
    for (slot, crop) in crops.iter().enumerate() {
        let w = ((REC_HEIGHT as f64 * ratio(crop)).ceil() as usize).clamp(1, batch_w);
        let resized = resize_linear(crop, w as u32, REC_HEIGHT as u32);
        write_normalized(&resized, batch.index_axis_mut(ndarray::Axis(0), slot));
    }
    batch
}

// ---------------------------------------------------------------------------
// CTC decode
// ---------------------------------------------------------------------------

/// CTC blank class (PaddleOCR puts it first).
const CTC_BLANK: usize = 0;

/// One recognized segment: text plus mean probability over kept steps.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub text: String,
    pub conf: f32,
}

/// Greedy CTC decode: argmax per timestep, collapse repeats, drop blanks
/// (index 0). `labels` comes from [`models::ctc_labels`].
pub fn ctc_greedy_decode(probs: &[f32], timesteps: usize, labels: &[String]) -> Decoded {
    let classes = labels.len();
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
        if best != prev && best != CTC_BLANK {
            out.push_str(&labels[best]);
            kept_sum += row[best];
            kept += 1;
        }
        prev = best;
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

// ---------------------------------------------------------------------------
// recognizer
// ---------------------------------------------------------------------------

struct Recognizer {
    session: Session,
    labels: Vec<String>,
}

impl Recognizer {
    fn load(model: &Path, dict_path: &Path, threads: usize) -> OcrResult<Recognizer> {
        let labels = models::ctc_labels(models::load_dictionary(dict_path)?);
        let session = build_session(model, threads)?;
        let classes = session
            .outputs()
            .first()
            .and_then(|o| o.dtype().tensor_shape())
            .and_then(|s| s.last().copied())
            .unwrap_or(-1);
        if classes > 0 && classes as usize != labels.len() {
            return Err(OcrError::Model(format!(
                "dictionary {} gives {} classes (blank + {} + space), model {} outputs {classes}",
                dict_path.display(),
                labels.len(),
                labels.len() - 2,
                model.display()
            )));
        }
        Ok(Recognizer { session, labels })
    }

    /// Recognize crops; one [`Decoded`] per crop, in input order.
    ///
    /// As in RapidOCR, crops are sorted by aspect ratio and run in batches of
    /// [`REC_MAX_BATCH`] so each batch needs little padding.
    fn recognize(&mut self, crops: &[RgbaImage]) -> OcrResult<Vec<Decoded>> {
        let ratio = |c: &RgbaImage| c.width() as f64 / c.height().max(1) as f64;
        let mut order: Vec<usize> = (0..crops.len()).collect();
        order.sort_by(|&a, &b| ratio(&crops[a]).total_cmp(&ratio(&crops[b])));
        let mut out = vec![
            Decoded {
                text: String::new(),
                conf: 0.0
            };
            crops.len()
        ];
        for chunk in order.chunks(REC_MAX_BATCH) {
            let batch: Vec<&RgbaImage> = chunk.iter().map(|&i| &crops[i]).collect();
            let decoded = self.run_batch(&batch)?;
            for (&i, d) in chunk.iter().zip(decoded) {
                out[i] = d;
            }
        }
        Ok(out)
    }

    fn run_batch(&mut self, crops: &[&RgbaImage]) -> OcrResult<Vec<Decoded>> {
        let n = crops.len();
        let batch = rec_batch_tensor(crops);
        let outputs = self
            .session
            .run(ort::inputs![TensorRef::from_array_view(&batch)?])
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        if dims.len() != 3 || dims[0] != n || dims[2] != self.labels.len() {
            return Err(OcrError::Ort(format!(
                "rec output shape {dims:?} != [{n}, time, {}]",
                self.labels.len()
            )));
        }
        let steps = dims[1];
        let per = steps * dims[2];
        Ok((0..n)
            .map(|slot| ctc_greedy_decode(&data[slot * per..(slot + 1) * per], steps, &self.labels))
            .collect())
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
        let tensor = to_tensor(&src, Some((w, h)), 0.0);
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
        let prob = Array4::from_shape_vec((1, 1, ph, pw), data.to_vec())
            .map_err(|e| OcrError::Ort(e.to_string()))?;
        // the image fills the top-left of the letterboxed input at this scale
        let fit = (w as f32 / src.width() as f32).min(h as f32 / src.height() as f32);
        let up = if small { 2.0 } else { 1.0 };
        let scale = 1.0 / (fit * up);
        Ok(boxes_from_probability(
            &prob,
            scale * w as f32 / pw as f32,
            scale * h as f32 / ph as f32,
            img.width(),
            img.height(),
        ))
    }

    /// Crop a detected box. Boxes are upright with integer corners, so this is
    /// what RapidOCR's perspective crop produces for them.
    fn crop_box(img: &RgbaImage, b: &Box2) -> RgbaImage {
        let r = b.to_rect(img.width(), img.height());
        let x = (r.x as u32).min(img.width() - 1);
        let y = (r.y as u32).min(img.height() - 1);
        crop_imm(img, x, y, r.w.max(1), r.h.max(1)).to_image()
    }

    fn recognize_crops(rec: &mut Recognizer, crops: &[RgbaImage]) -> OcrResult<Vec<Decoded>> {
        rec.recognize(crops)
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
        let lines = layout::join_rows(lines);
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
            width: 0,
            height: 0,
            colors: Vec::new(),
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

    fn labels(dict: &[&str]) -> Vec<String> {
        models::ctc_labels(dict.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn test_ctc_greedy_decode_collapses_repeats() {
        // classes: 0 = blank, 1 = "a", 2 = "b", 3 = " "
        let labels = labels(&["a", "b"]);
        // t0=a t1=a t2=blank t3=b t4=b t5=space t6=a
        let probs = vec![
            0.05, 0.90, 0.05, 0.00, //
            0.05, 0.90, 0.05, 0.00, //
            0.90, 0.05, 0.05, 0.00, //
            0.05, 0.10, 0.85, 0.00, //
            0.05, 0.10, 0.85, 0.00, //
            0.10, 0.10, 0.00, 0.80, //
            0.10, 0.70, 0.10, 0.10,
        ];
        let d = ctc_greedy_decode(&probs, 7, &labels);
        assert_eq!(d.text, "ab a");
        assert!((d.conf - 0.8125).abs() < 1e-4, "conf {}", d.conf);
    }

    #[test]
    fn test_ctc_greedy_decode_blank_splits_repeats() {
        // "a" blank "a" is two letters; "a" "a" is one
        let labels = labels(&["a"]);
        let probs = vec![
            0.1, 0.9, 0.0, //
            0.9, 0.1, 0.0, //
            0.1, 0.9, 0.0,
        ];
        assert_eq!(ctc_greedy_decode(&probs, 3, &labels).text, "aa");
    }

    #[test]
    fn test_ctc_greedy_decode_all_blank() {
        let labels = labels(&["a"]);
        let d = ctc_greedy_decode(&[0.9, 0.1, 0.0, 0.9, 0.1, 0.0], 2, &labels);
        assert_eq!(d.text, "");
        assert_eq!(d.conf, 0.0);
    }

    #[test]
    fn test_ctc_decode_uses_dictionary_file_order() {
        // an unsorted dictionary must decode by file position, not sort order
        let labels = labels(&["z", "a", "m"]);
        let mut probs = vec![0.0; 3 * 5];
        for (t, class) in [1usize, 2, 3].into_iter().enumerate() {
            probs[t * 5 + class] = 1.0;
        }
        assert_eq!(ctc_greedy_decode(&probs, 3, &labels).text, "zam");
    }

    #[test]
    fn test_write_normalized_is_bgr_and_scaled() {
        let img = RgbaImage::from_pixel(2, 1, Rgba([255, 0, 51, 255]));
        let mut t = Array4::<f32>::zeros((1, 3, 1, 2));
        write_normalized(&img, t.index_axis_mut(ndarray::Axis(0), 0));
        // channel 0 = B, 1 = G, 2 = R; (v / 255 - 0.5) / 0.5
        assert!((t[[0, 0, 0, 0]] - (0.2 - 0.5) / 0.5).abs() < 1e-6);
        assert!((t[[0, 1, 0, 0]] + 1.0).abs() < 1e-6);
        assert!((t[[0, 2, 0, 0]] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_rec_batch_tensor_resize_and_zero_padding() {
        // 20x10 -> ratio 2 -> 96 px wide; batch width is at least 320
        let small = RgbaImage::from_pixel(20, 10, Rgba([255, 255, 255, 255]));
        let t = rec_batch_tensor(&[&small]);
        assert_eq!(t.shape(), &[1, 3, 48, 320]);
        assert!((t[[0, 0, 24, 95]] - 1.0).abs() < 1e-6, "white inside");
        assert_eq!(t[[0, 0, 24, 96]], 0.0, "padding stays 0");
        assert_eq!(t[[0, 2, 47, 319]], 0.0);

        // the widest crop sets the batch width: int(48 * 1000 / 30) = 1600
        let wide = RgbaImage::from_pixel(1000, 30, Rgba([0, 0, 0, 255]));
        let t = rec_batch_tensor(&[&small, &wide]);
        assert_eq!(t.shape(), &[2, 3, 48, 1600]);
        assert!((t[[1, 1, 0, 1599]] + 1.0).abs() < 1e-6, "black to the edge");
        assert_eq!(t[[0, 1, 0, 96]], 0.0);
        // width = ceil(48 * 7 / 5) = 68
        let odd = RgbaImage::from_pixel(7, 5, Rgba([255, 255, 255, 255]));
        let t = rec_batch_tensor(&[&odd]);
        assert!(t[[0, 0, 0, 67]] > 0.99);
        assert_eq!(t[[0, 0, 0, 68]], 0.0);
    }

    #[test]
    fn test_resize_linear_matches_opencv() {
        // cv2.resize(np.array([[0, 100, 200, 250]], np.uint8), (8, 1))
        //   -> [0, 25, 75, 125, 175, 213, 238, 250]
        let mut img = RgbaImage::new(4, 1);
        for (x, v) in [0u8, 100, 200, 250].into_iter().enumerate() {
            img.put_pixel(x as u32, 0, Rgba([v, v, v, 255]));
        }
        let out = resize_linear(&img, 8, 1);
        let got: Vec<u8> = out.pixels().map(|p| p.0[0]).collect();
        assert_eq!(got, [0, 25, 75, 125, 175, 213, 238, 250]);
    }

    #[test]
    fn test_to_tensor_letterbox_pads_with_zero() {
        // 10x40 into 48x48: scale 1.2 -> 12x48, the rest is padding
        let img = RgbaImage::from_pixel(10, 40, Rgba([255, 255, 255, 255]));
        let t = to_tensor(&img, Some((48, 48)), 0.0);
        assert_eq!(t.shape(), &[1, 3, 48, 48]);
        assert!((t[[0, 0, 10, 0]] - 1.0).abs() < 1e-6, "white is +1");
        assert_eq!(t[[0, 0, 10, 20]], 0.0, "padding stays 0");
    }

    #[test]
    fn test_to_tensor_no_target_uses_original_size() {
        let img = RgbaImage::from_pixel(100, 50, Rgba([0, 0, 0, 255]));
        let t = to_tensor(&img, None, 640.0);
        assert_eq!(t.shape(), &[1, 3, 50, 100]);
        // black becomes -1
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
    fn test_box2_unclip_grows_by_area_over_perimeter() {
        let b = Box2 {
            x0: 0.0,
            y0: 0.0,
            x1: 30.0,
            y1: 10.0,
        };
        // d = 30 * 10 * 1.6 / 80 = 6
        let u = b.unclip(1.6);
        assert_eq!(
            u,
            Box2 {
                x0: -6.0,
                y0: -6.0,
                x1: 36.0,
                y1: 16.0
            }
        );
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
    fn test_boxes_from_probability_finds_two_lines() {
        // 60x40 map, two solid lines at y=4..12 and y=24..32
        let mut prob = Array4::<f32>::zeros((1, 1, 40, 60));
        for y in 4..12 {
            for x in 5..45 {
                prob[[0, 0, y, x]] = 0.9;
            }
        }
        for y in 24..32 {
            for x in 10..30 {
                prob[[0, 0, y, x]] = 0.9;
            }
        }
        let boxes = boxes_from_probability(&prob, 1.0, 1.0, 60, 40);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        // first region after dilation: x 5..=45, y 4..=12 -> 40x8,
        // d = 40 * 8 * 1.6 / 96 = 5.33, rounded and clipped at 0
        assert_eq!(
            (boxes[0].x0, boxes[0].y0, boxes[0].x1, boxes[0].y1),
            (0.0, 0.0, 50.0, 17.0)
        );
        assert!(boxes[1].y0 > boxes[0].y1);
    }

    #[test]
    fn test_boxes_from_probability_drops_weak_and_tiny_regions() {
        let mut prob = Array4::<f32>::zeros((1, 1, 30, 30));
        // above the binarization threshold but below the box threshold
        for y in 2..10 {
            for x in 2..20 {
                prob[[0, 0, y, x]] = 0.4;
            }
        }
        // a 2x2 speck
        prob[[0, 0, 20, 20]] = 0.9;
        prob[[0, 0, 21, 21]] = 0.9;
        assert!(boxes_from_probability(&prob, 1.0, 1.0, 30, 30).is_empty());
    }

    #[test]
    fn test_boxes_from_probability_scales_and_clips() {
        let mut prob = Array4::<f32>::zeros((1, 1, 20, 40));
        for y in 5..15 {
            for x in 0..40 {
                prob[[0, 0, y, x]] = 0.95;
            }
        }
        let boxes = boxes_from_probability(&prob, 2.0, 2.0, 80, 40);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].x0, 0.0);
        assert_eq!(boxes[0].x1, 80.0);
    }
}
