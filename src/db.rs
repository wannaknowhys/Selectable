// DB post-processing adapted from RapidOCR's OpenCV-free Rust path:
// threshold mask -> 2x2 dilate -> 8-connected components -> boundary sampling ->
// min-area rect -> polygon score -> unclip -> min-area rect -> scale to image.
use std::collections::VecDeque;

use anyhow::Result;
use ndarray::{ArrayD, Ix4};

use crate::geometry::{min_area_rect, unclip_quad, Point, Quad};

const SCORE_EPS: f32 = 0.005;

#[derive(Debug, Clone)]
pub struct DbParams {
    pub thresh: f32,
    pub box_thresh: f32,
    pub max_candidates: usize,
    pub unclip_ratio: f32,
    pub min_size: u32,
}

pub struct DbCandidate {
    pub bbox: Quad,
    // Phase: overlay confidence display.
    #[allow(dead_code)]
    pub score: f32,
}

pub fn db_process(pred: ArrayD<f32>, dest_w: u32, dest_h: u32, cfg: &DbParams) -> Result<Vec<DbCandidate>> {
    let pred = pred.into_dimensionality::<Ix4>()?;
    let map_h = pred.shape()[2];
    let map_w = pred.shape()[3];
    let mask = dilate_2x2(&pred, cfg.thresh, map_w, map_h);
    let mut visited = vec![false; map_h * map_w];
    let mut boxes = Vec::new();

    for y in 0..map_h {
        for x in 0..map_w {
            let idx = y * map_w + x;
            if visited[idx] || !mask[idx] {
                continue;
            }
            let comp = collect_component(&pred, &mask, &mut visited, x, y, map_w, map_h);
            if comp.score() < cfg.box_thresh {
                continue;
            }
            if comp.width() < cfg.min_size || comp.height() < cfg.min_size {
                continue;
            }
            let Some(base_box) = comp.to_quad(&mask, map_w, map_h) else {
                continue;
            };
            if base_box.short_side() < cfg.min_size as f32 {
                continue;
            }
            let score = polygon_score(&pred, &base_box);
            if score + SCORE_EPS < cfg.box_thresh {
                continue;
            }
            let Some(expanded) = unclip_quad(&base_box, cfg.unclip_ratio) else {
                continue;
            };
            let Some(mut bbox) = min_area_rect(&expanded) else {
                continue;
            };
            if bbox.short_side() < (cfg.min_size + 2) as f32 {
                continue;
            }
            let sx = dest_w as f32 / map_w as f32;
            let sy = dest_h as f32 / map_h as f32;
            bbox.scale(sx, sy);
            clip_box(&mut bbox, dest_w, dest_h);
            bbox.order_clockwise_in_place();
            let (bw, bh) = (bbox.width_f32() as u32, bbox.height_f32() as u32);
            if bw <= 3 || bh <= 3 {
                continue;
            }
            if bw <= 4 && bh <= 4 {
                continue;
            }
            boxes.push(DbCandidate { bbox, score });
            if boxes.len() >= cfg.max_candidates {
                return Ok(sort_reading_order(boxes));
            }
        }
    }
    Ok(sort_reading_order(boxes))
}

fn clip_box(bbox: &mut Quad, w: u32, h: u32) {
    let max_x = w.saturating_sub(1) as f32;
    let max_y = h.saturating_sub(1) as f32;
    for p in &mut bbox.points {
        p[0] = p[0].round().clamp(0.0, max_x);
        p[1] = p[1].round().clamp(0.0, max_y);
    }
}

struct Component {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    sum: f32,
    count: usize,
    pixels: Vec<(usize, usize)>,
}

impl Component {
    fn score(&self) -> f32 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / self.count as f32
        }
    }
    fn width(&self) -> u32 {
        (self.max_x - self.min_x + 1) as u32
    }
    fn height(&self) -> u32 {
        (self.max_y - self.min_y + 1) as u32
    }
    fn to_quad(&self, mask: &[bool], w: usize, h: usize) -> Option<Quad> {
        let mut boundary = Vec::new();
        for &(x, y) in &self.pixels {
            if !is_boundary(mask, x, y, w, h) {
                continue;
            }
            boundary.push(Point::new(x as f32, y as f32));
            boundary.push(Point::new((x + 1) as f32, y as f32));
            boundary.push(Point::new((x + 1) as f32, (y + 1) as f32));
            boundary.push(Point::new(x as f32, (y + 1) as f32));
        }
        if boundary.len() < 3 {
            return Some(Quad::from_xyxy(
                self.min_x as f32,
                self.min_y as f32,
                (self.max_x + 1) as f32,
                (self.max_y + 1) as f32,
            ));
        }
        min_area_rect(&boundary)
    }
}

fn collect_component(
    pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>,
    mask: &[bool],
    visited: &mut [bool],
    sx: usize,
    sy: usize,
    w: usize,
    h: usize,
) -> Component {
    let mut queue = VecDeque::from([(sx, sy)]);
    let mut c = Component { min_x: sx, min_y: sy, max_x: sx, max_y: sy, sum: 0.0, count: 0, pixels: Vec::new() };
    while let Some((x, y)) = queue.pop_front() {
        let idx = y * w + x;
        if visited[idx] || !mask[idx] {
            continue;
        }
        visited[idx] = true;
        c.min_x = c.min_x.min(x);
        c.min_y = c.min_y.min(y);
        c.max_x = c.max_x.max(x);
        c.max_y = c.max_y.max(y);
        c.sum += pred[[0, 0, y, x]];
        c.count += 1;
        c.pixels.push((x, y));
        for dy in -1isize..=1 {
            for dx in -1isize..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let nx = x as isize + dx;
                let ny = y as isize + dy;
                if nx >= 0 && ny >= 0 && nx < w as isize && ny < h as isize {
                    let nidx = ny as usize * w + nx as usize;
                    if !visited[nidx] {
                        queue.push_back((nx as usize, ny as usize));
                    }
                }
            }
        }
    }
    c
}

fn is_boundary(mask: &[bool], x: usize, y: usize, w: usize, h: usize) -> bool {
    for dy in -1isize..=1 {
        for dx in -1isize..=1 {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                return true;
            }
            if !mask[ny as usize * w + nx as usize] {
                return true;
            }
        }
    }
    false
}

fn dilate_2x2(pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>, thresh: f32, w: usize, h: usize) -> Vec<bool> {
    let mut mask = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if pred[[0, 0, y, x]] <= thresh {
                continue;
            }
            for dy in 0..=1 {
                for dx in 0..=1 {
                    let nx = (x + dx).min(w - 1);
                    let ny = (y + dy).min(h - 1);
                    mask[ny * w + nx] = true;
                }
            }
        }
    }
    mask
}

fn polygon_score(pred: &ndarray::ArrayBase<ndarray::OwnedRepr<f32>, Ix4>, bbox: &Quad) -> f32 {
    let h = pred.shape()[2];
    let w = pred.shape()[3];
    let (x0, y0, x1, y1) = bbox.axis_aligned_bounds();
    let (x0, y0) = (x0.min(w.saturating_sub(1) as u32), y0.min(h.saturating_sub(1) as u32));
    let (x1, y1) = (x1.min(w as u32), y1.min(h as u32));
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for y in y0..=y1 {
        if y as usize >= h {
            continue;
        }
        for x in x0..=x1 {
            if x as usize >= w {
                continue;
            }
            if bbox.contains_point(x as f32 + 0.5, y as f32 + 0.5) {
                sum += pred[[0, 0, y as usize, x as usize]];
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}

/// Reading order: sort by Y, group lines within 10px, sort each line by X.
fn sort_reading_order(mut boxes: Vec<DbCandidate>) -> Vec<DbCandidate> {
    boxes.sort_by(|a, b| a.bbox.points[0][1].total_cmp(&b.bbox.points[0][1]));
    let mut start = 0;
    while start < boxes.len() {
        let mut end = start + 1;
        while end < boxes.len() && boxes[end].bbox.points[0][1] - boxes[end - 1].bbox.points[0][1] < 10.0 {
            end += 1;
        }
        boxes[start..end].sort_by(|a, b| a.bbox.points[0][0].total_cmp(&b.bbox.points[0][0]));
        start = end;
    }
    boxes
}
