// PP-OCRv6 det -> rec pipeline over ONNX Runtime (ort 2.0.0-rc).
// Model triple per tier: det.onnx + det.yml (thresholds) + rec.onnx + rec.yml
// (char dict). Dict/params are parsed with a tolerant line scanner because the
// dict contains YAML-hostile scalars (`@`, `%`, `:` ...).
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use image::{Rgb, RgbImage};
use imageproc::geometric_transformations::{warp_into, Interpolation, Projection};
use ndarray::{s, Array3, Array4, Ix3};
use ort::{
    session::{builder::GraphOptimizationLevel, Session},
    value::TensorRef,
};

use crate::{
    db::{db_process, DbParams},
    geometry::Quad,
};

pub struct OcrLine {
    pub quad: Quad,
    pub text: String,
    pub score: f32,
    /// Per-char hits from CTC alignment; f0/f1 are fractions of crop width.
    pub chars: Vec<CharHit>,
}

#[derive(Debug, Clone)]
pub struct CharHit {
    pub text: String,
    pub f0: f32,
    pub f1: f32,
    pub score: f32,
}

#[derive(Debug, Default)]
pub struct OcrTimings {
    pub det_pre_ms: f64,
    pub det_inf_ms: f64,
    pub det_post_ms: f64,
    pub rec_pre_ms: f64,
    pub rec_inf_ms: f64,
    pub rec_dec_ms: f64,
}

pub struct OcrEngine {
    tier: String,
    det: Session,
    rec: Session,
    dict: Vec<String>,
    det_params: DbParams,
    rec_h: usize,
    rec_base_w: usize,
    batch_size: usize,
}

fn build_session(path: &Path, dynamic: bool, use_dml: bool) -> Result<Session> {
    let mk = |dml: bool| -> Result<Session> {
        let mut b = Session::builder()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
            .with_memory_pattern(!dynamic && !dml)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if dml {
            b = b
                .with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        } else {
            b = b
                .with_execution_providers([ort::ep::CPU::default().build()])
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        b.commit_from_file(path)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .with_context(|| format!("failed to load {}", path.display()))
    };
    if use_dml {
        match mk(true) {
            Ok(s) => Ok(s),
            Err(e) => {
                eprintln!("DirectML session failed ({e:#}), falling back to CPU");
                mk(false)
            }
        }
    } else {
        mk(false)
    }
}

/// tolerance: find `key:` line, parse the scalar after the colon.
fn yml_scalar(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(key) && t[key.len()..].trim_start().starts_with(':') {
            let v = t[key.len() + 1..].trim();
            if !v.is_empty() && !v.starts_with('#') {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn yml_f32(text: &str, key: &str, default: f32) -> f32 {
    yml_scalar(text, key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn yml_usize(text: &str, key: &str, default: usize) -> usize {
    yml_scalar(text, key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Read the dash-list following a `key:` line (used for image_shape).
fn yml_list_after(text: &str, key: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with(key) && t[key.len()..].trim_start().starts_with(':') {
            let mut out = Vec::new();
            for l in lines.iter().skip(i + 1).take(n) {
                let lt = l.trim();
                if let Some(v) = lt.strip_prefix("- ") {
                    out.push(v.trim().to_string());
                } else {
                    break;
                }
            }
            return out;
        }
    }
    Vec::new()
}

/// character_dict entries: strip `- ` prefix and one layer of single quotes.
fn parse_dict(text: &str) -> Result<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| {
            let t = l.trim();
            t.starts_with("character_dict") && t.contains(':')
        })
        .context("character_dict not found in rec.yml")?;
    let mut chars = vec!["blank".to_string()];
    for l in lines.iter().skip(start + 1) {
        // NOTE: do NOT trim() the line: the U+3000 ideographic-space entry is
        // "  - \u{3000}", and trim() eats it, silently truncating the dict.
        let t = l.trim_start_matches(' ');
        let Some(rest) = t.strip_prefix('-') else { break };
        // Exactly one ASCII space separates the dash from the scalar.
        let v = rest.strip_prefix(' ').unwrap_or(rest);
        let v = v.strip_suffix('\'').unwrap_or(v);
        let v = v.strip_prefix('\'').unwrap_or(v);
        // YAML escape for backslash entry appears as `\\`
        let v = if v == "\\\\" { "\\".to_string() } else { v.to_string() };
        chars.push(v);
    }
    if chars.len() <= 1 {
        bail!("empty character_dict in rec.yml");
    }
    chars.push(" ".to_string());
    Ok(chars)
}

fn triple_complete(dir: &Path) -> bool {
    ["det.onnx", "det.yml", "rec.onnx", "rec.yml"]
        .iter()
        .all(|f| dir.join(f).is_file())
}

impl OcrEngine {
    pub fn load_cascade(
        models_root: &Path,
        order: &[String],
        explicit: Option<&str>,
        batch_size: usize,
        use_dml: bool,
    ) -> Result<(Self, String)> {
        if let Some(want) = explicit {
            let dir = models_root.join(want);
            if !triple_complete(&dir) {
                bail!(
                    "ocr.model = \"{want}\" but triple incomplete in {} (need det.onnx/det.yml/rec.onnx/rec.yml); run tools/fetch-models.mjs",
                    dir.display()
                );
            }
            return Ok((Self::load_tier(&dir, want, batch_size, use_dml)?, want.to_string()));
        }
        for tier in order {
            let dir = models_root.join(tier);
            if triple_complete(&dir) {
                return Ok((Self::load_tier(&dir, tier, batch_size, use_dml)?, tier.clone()));
            }
        }
        bail!(
            "no complete model triple under {} (tried {:?}); run: node tools/fetch-models.mjs --tier small",
            models_root.display(),
            order
        );
    }

    fn load_tier(dir: &Path, tier: &str, batch_size: usize, use_dml: bool) -> Result<Self> {
        let det_yml = std::fs::read_to_string(dir.join("det.yml"))?;
        let rec_yml = std::fs::read_to_string(dir.join("rec.yml"))?;
        let det_params = DbParams {
            thresh: yml_f32(&det_yml, "thresh", 0.2),
            box_thresh: yml_f32(&det_yml, "box_thresh", 0.45),
            max_candidates: yml_usize(&det_yml, "max_candidates", 3000),
            unclip_ratio: yml_f32(&det_yml, "unclip_ratio", 1.4),
            min_size: 3,
        };
        let shape = yml_list_after(&rec_yml, "image_shape", 3);
        let rec_h = shape.first().and_then(|_| shape.get(1)).and_then(|v| v.parse().ok()).unwrap_or(48);
        let rec_base_w = shape.get(2).and_then(|v| v.parse().ok()).unwrap_or(320);
        let dict = parse_dict(&rec_yml)?;
        let det = build_session(&dir.join("det.onnx"), true, use_dml)?;
        let rec = build_session(&dir.join("rec.onnx"), false, use_dml)?;
        Ok(Self {
            tier: tier.to_string(),
            det,
            rec,
            dict,
            det_params,
            rec_h,
            rec_base_w,
            batch_size,
        })
    }

    pub fn tier(&self) -> &str {
        &self.tier
    }

    pub fn run(&mut self, img: &RgbImage) -> Result<(Vec<OcrLine>, OcrTimings)> {
        use std::time::Instant;
        let mut t = OcrTimings::default();

        let start = Instant::now();
        let input = det_preprocess(img);
        t.det_pre_ms = start.elapsed().as_secs_f64() * 1000.0;

        let start = Instant::now();
        let pred = {
            let outputs = self
                .det
                .run(ort::inputs![TensorRef::from_array_view(&input).map_err(|e| anyhow::anyhow!(e.to_string()))?])
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            outputs[0]
                .try_extract_array::<f32>()
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .to_owned()
        };
        t.det_inf_ms = start.elapsed().as_secs_f64() * 1000.0;
        if std::env::var("SELECTABLE_DEBUG").is_ok() {
            let n = pred.len() as f32;
            let (mut mn, mut mx, mut sum) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f32);
            for &v in pred.iter() {
                mn = mn.min(v);
                mx = mx.max(v);
                sum += v;
            }
            eprintln!("[debug] det out shape {:?} min={mn:.3} max={mx:.3} mean={:.4}", pred.shape(), sum / n);
            // Dump det input (approx denormalized) and probability heatmap.
            let (ih, iw) = (input.shape()[2], input.shape()[3]);
            let mut din = RgbImage::new(iw as u32, ih as u32);
            const STD: [f32; 3] = [0.229, 0.224, 0.225];
            const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
            for (x, y, p) in din.enumerate_pixels_mut() {
                let mut px = [0u8; 3];
                for c in 0..3 {
                    px[c] = ((input[[0, c, y as usize, x as usize]] * STD[c] + MEAN[c]) * 255.0).clamp(0.0, 255.0) as u8;
                }
                *p = Rgb(px);
            }
            let _ = din.save("temp/det_in.png");
            let (mh, mw) = (pred.shape()[2], pred.shape()[3]);
            let mut heat = image::GrayImage::new(mw as u32, mh as u32);
            for (x, y, p) in heat.enumerate_pixels_mut() {
                p.0 = [(pred[[0, 0, y as usize, x as usize]].clamp(0.0, 1.0) * 255.0) as u8];
            }
            let _ = heat.save("temp/det_heat.png");
        }

        let start = Instant::now();
        let boxes = db_process(pred, img.width(), img.height(), &self.det_params)?;
        t.det_post_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Perspective-crop each box for the recognizer.
        let mut crops = Vec::with_capacity(boxes.len());
        for b in &boxes {
            crops.push(perspective_crop(img, &b.bbox)?);
        }

        // Aspect-sort + batch like RapidOCR so wide crops don't blow up padding.
        let mut order: Vec<usize> = (0..crops.len()).collect();
        order.sort_by(|&a, &b| {
            aspect(&crops[a]).total_cmp(&aspect(&crops[b]))
        });
        let mut texts: Vec<Option<(String, f32, Vec<CharHit>)>> = (0..crops.len()).map(|_| None).collect();
        for chunk in order.chunks(self.batch_size) {
            let max_ratio = chunk
                .iter()
                .map(|&i| aspect(&crops[i]))
                .fold(self.rec_base_w as f32 / self.rec_h as f32, f32::max);
            let batch_w = (self.rec_h as f32 * max_ratio) as usize;

            let start = Instant::now();
            let mut batch = Array4::<f32>::zeros((chunk.len(), 3, self.rec_h, batch_w));
            for (bi, &ci) in chunk.iter().enumerate() {
                let norm = rec_preprocess(&crops[ci], self.rec_h, batch_w);
                batch.slice_mut(s![bi, .., .., ..]).assign(&norm);
            }
            t.rec_pre_ms += start.elapsed().as_secs_f64() * 1000.0;

            let start = Instant::now();
            let pred = {
                let outputs = self
                    .rec
                    .run(ort::inputs![TensorRef::from_array_view(&batch).map_err(|e| anyhow::anyhow!(e.to_string()))?])
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                let arr = outputs[0]
                    .try_extract_array::<f32>()
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
                    .to_owned();
                arr.into_dimensionality::<Ix3>()
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
            };
            t.rec_inf_ms += start.elapsed().as_secs_f64() * 1000.0;

            let start = Instant::now();
            for (bi, &ci) in chunk.iter().enumerate() {
                if std::env::var("SELECTABLE_DEBUG").is_ok() && bi == 0 {
                    eprintln!("[debug] rec out {:?}, dict len {}", pred.shape(), self.dict.len());
                }
                texts[ci] = Some(self.decode(pred.slice(s![bi, .., ..])));
            }
            t.rec_dec_ms += start.elapsed().as_secs_f64() * 1000.0;
        }

        let lines = boxes
            .into_iter()
            .zip(texts.into_iter())
            .map(|(b, r)| {
                let (text, score, chars) = r.unwrap_or_default();
                OcrLine { quad: b.bbox, text, score, chars }
            })
            .collect();
        Ok((lines, t))
    }

    /// Greedy CTC decode that also records each emitted char's timestep span,
    /// so callers can map chars back to horizontal fractions of the crop
    /// (the same alignment Paddle's single-char coordinates use).
    fn decode(&self, logits: ndarray::ArrayView2<'_, f32>) -> (String, f32, Vec<CharHit>) {
        let t_total = logits.nrows().max(1) as f32;
        let mut text = String::new();
        let mut chars: Vec<CharHit> = Vec::new();
        let mut last = usize::MAX;
        // (char index into `chars`, accumulated prob, count)
        let mut open: Option<(usize, f32, usize, usize)> = None; // (pos, sum, n, t0)
        let flush = |chars: &mut Vec<CharHit>, o: Option<(usize, f32, usize, usize)>, t1: usize| {
            if let Some((pos, sum, n, _t0)) = o {
                chars[pos].f1 = t1 as f32 / t_total;
                chars[pos].score = sum / n as f32;
            }
        };
        for (t, step) in logits.outer_iter().enumerate() {
            let (idx, prob) = step
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(i, p)| (i, *p))
                .unwrap_or((0, 0.0));
            if idx == 0 || idx == last {
                if idx == last {
                    if let Some(o) = open.as_mut() {
                        o.1 += prob;
                        o.2 += 1;
                    }
                }
                last = idx;
                continue;
            }
            flush(&mut chars, open.take(), t);
            if let Some(ch) = self.dict.get(idx) {
                text.push_str(ch);
                chars.push(CharHit { text: ch.clone(), f0: t as f32 / t_total, f1: (t + 1) as f32 / t_total, score: prob });
                open = Some((chars.len() - 1, prob, 1, t));
            }
            last = idx;
        }
        flush(&mut chars, open.take(), logits.nrows());
        let score = if chars.is_empty() {
            0.0
        } else {
            chars.iter().map(|c| c.score).sum::<f32>() / chars.len() as f32
        };
        (text, score, chars)
    }
}

fn aspect(img: &RgbImage) -> f32 {
    img.width() as f32 / img.height().max(1) as f32
}

// M1: 1600px keeps 1080p screenshots near native scale and keeps 4K text
// legible (~6px+). Release build + tiling later; see DESIGN.
const DET_LIMIT: u32 = 1600;

fn round32(v: u32) -> u32 {
    ((v + 16) / 32 * 32).max(32)
}

fn det_preprocess(img: &RgbImage) -> Array4<f32> {
    let (w, h) = (img.width(), img.height());
    let ratio = if w.max(h) > DET_LIMIT { DET_LIMIT as f32 / w.max(h) as f32 } else { 1.0 };
    let (rw, rh) = (round32((w as f32 * ratio) as u32), round32((h as f32 * ratio) as u32));
    let resized = image::imageops::resize(img, rw, rh, image::imageops::FilterType::Triangle);
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let mut a = Array4::<f32>::zeros((1, 3, rh as usize, rw as usize));
    for (x, y, p) in resized.enumerate_pixels() {
        for c in 0..3 {
            a[[0, c, y as usize, x as usize]] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    a
}

fn rec_preprocess(img: &RgbImage, h: usize, out_w: usize) -> Array3<f32> {
    let ratio = img.width() as f32 / img.height().max(1) as f32;
    let rw = ((h as f32 * ratio).ceil() as usize).min(out_w).max(1);
    let resized = image::imageops::resize(img, rw as u32, h as u32, image::imageops::FilterType::Triangle);
    let mut out = Array3::<f32>::zeros((3, h, out_w));
    for (x, y, p) in resized.enumerate_pixels() {
        // BGR order, normalized to [-1, 1].
        for c in 0..3 {
            out[[c, y as usize, x as usize]] = p[2 - c] as f32 / 255.0 / 0.5 - 1.0;
        }
    }
    out
}

fn perspective_crop(img: &RgbImage, quad: &Quad) -> Result<RgbImage> {
    let q = quad.ordered();
    let (cw, ch) = (q.crop_width(), q.crop_height());
    if cw == 0 || ch == 0 {
        bail!("degenerate crop");
    }
    let from = [
        (q.points[0][0], q.points[0][1]),
        (q.points[1][0], q.points[1][1]),
        (q.points[2][0], q.points[2][1]),
        (q.points[3][0], q.points[3][1]),
    ];
    let to = [(0.0, 0.0), (cw as f32, 0.0), (cw as f32, ch as f32), (0.0, ch as f32)];
    let Some(proj) = Projection::from_control_points(from, to) else {
        let (x0, y0, x1, y1) = q.axis_aligned_bounds();
        let (iw, ih) = img.dimensions();
        let (x1, y1) = (x1.min(iw), y1.min(ih));
        if x1 <= x0 || y1 <= y0 {
            bail!("degenerate crop");
        }
        return Ok(image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image());
    };
    let mut out = RgbImage::new(cw, ch);
    warp_into(img, &proj, Interpolation::Bicubic, Rgb([0, 0, 0]), &mut out);
    if out.height() as f32 / out.width().max(1) as f32 >= 1.5 {
        Ok(image::imageops::rotate270(&out))
    } else {
        Ok(out)
    }
}

pub fn models_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let next_to_exe = dir.join("models");
            if next_to_exe.is_dir() {
                return next_to_exe;
            }
        }
    }
    PathBuf::from("models")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

    fn rasterize(font: &FontRef, text: &str, px: f32) -> RgbImage {
        let scale = PxScale::from(px);
        let scaled = font.as_scaled(scale);
        let w = (scaled.h_advance(font.glyph_id('M')) * text.chars().count() as f32) as u32 + 20;
        let h = (px * 1.6) as u32 + 10;
        let mut img = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
        let mut x = 10.0;
        let y = 10.0;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            let g = id.with_scale_and_position(px, ab_glyph::point(x, y));
            if let Some(out) = scaled.outline_glyph(g) {
                out.draw(|ox, oy, c| {
                    if c > 0.5 {
                        let (ix, iy) = (out.px_bounds().min.x as u32 + ox, out.px_bounds().min.y as u32 + oy);
                        if ix < w && iy < h {
                            img.put_pixel(ix, iy, Rgb([0, 0, 0]));
                        }
                    }
                });
                x += scaled.h_advance(id) + 4.0;
            }
        }
        img
    }

    fn stack(imgs: Vec<RgbImage>) -> RgbImage {
        let w = imgs.iter().map(|i| i.width()).max().unwrap();
        let h: u32 = imgs.iter().map(|i| i.height()).sum();
        let mut out = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
        let mut y = 0;
        for im in imgs {
            for (x, yy, p) in im.enumerate_pixels() {
                out.put_pixel(x, y + yy, *p);
            }
            y += im.height();
        }
        out
    }

    #[test]
    fn synth_big_text_is_detected() {
        let yahei = std::fs::read("C:/Windows/Fonts/msyh.ttc").expect("msyh.ttc missing");
        let font = FontRef::try_from_slice_and_index(&yahei, 0).expect("font parse");
        let img = stack(vec![
            rasterize(&font, "Hello World 123", 64.0),
            rasterize(&font, "截屏选词测试", 64.0),
            rasterize(&font, "Selectable OCR 2026", 64.0),
        ]);
        img.save("temp/test_text.png").unwrap();

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
        let (mut engine, tier) =
            OcrEngine::load_cascade(&root, &["small".to_string()], None, 6, false)
                .expect("load small");
        assert_eq!(tier, "small");
        let (lines, t) = engine.run(&img).expect("ocr run");
        eprintln!("timings: {t:?}");
        for l in &lines {
            eprintln!("[{:.2}] {}", l.score, l.text);
        }
        assert!(lines.len() >= 2, "expected >=2 lines, got {}", lines.len());
        let all: String = lines.iter().map(|l| l.text.clone()).collect();
        assert!(all.len() > 4, "recognized text too short: {all:?}");
        // The middle line is Chinese: with a truncated dict it decodes empty.
        assert!(!lines[1].text.is_empty() && lines[1].score > 0.5, "chinese line lost: {all:?}");
    }

    #[test]
    fn rec_dict_covers_all_model_classes() {
        // Regression: a trim() on the U+3000 dict entry once truncated the dict
        // to 1750/18710 classes, silently dropping spaces, CJK and symbols.
        let yml = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/small/rec.yml"),
        )
        .expect("run tools/fetch-models.mjs --tier small first");
        let dict = parse_dict(&yml).expect("parse dict");
        assert!(dict.len() >= 18710, "dict truncated: {}", dict.len());
    }
}
