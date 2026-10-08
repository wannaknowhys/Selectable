// Minimal 2D geometry for DB post-processing: quads, min-area rect
// (convex hull + rotating calipers), and a centroid-expansion unclip.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone)]
pub struct Quad {
    pub points: [[f32; 2]; 4],
}

impl Quad {
    pub fn from_xyxy(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { points: [[x0, y0], [x1, y0], [x1, y1], [x0, y1]] }
    }

    pub fn scale(&mut self, sx: f32, sy: f32) {
        for p in &mut self.points {
            p[0] *= sx;
            p[1] *= sy;
        }
    }

    fn centroid(&self) -> [f32; 2] {
        let mut c = [0.0; 2];
        for p in &self.points {
            c[0] += p[0];
            c[1] += p[1];
        }
        [c[0] / 4.0, c[1] / 4.0]
    }

    /// Sort points clockwise around the centroid, starting at the topmost one.
    pub fn order_clockwise_in_place(&mut self) {
        let c = self.centroid();
        self.points.sort_by(|a, b| {
            (a[1] - c[1]).atan2(a[0] - c[0]).total_cmp(&(b[1] - c[1]).atan2(b[0] - c[0]))
        });
        // Rotate so the top-left-most point comes first.
        let start = self
            .points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a[1].total_cmp(&b[1]).then_with(|| a[0].total_cmp(&b[0]))
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.points.rotate_left(start);
    }

    /// Clockwise tl -> tr -> br -> bl ordering (for perspective warps).
    pub fn ordered(&self) -> Quad {
        let mut q = self.clone();
        q.order_clockwise_in_place();
        q
    }

    fn edge_len(&self, a: usize, b: usize) -> f32 {
        let dx = self.points[a][0] - self.points[b][0];
        let dy = self.points[a][1] - self.points[b][1];
        (dx * dx + dy * dy).sqrt()
    }

    pub fn width_f32(&self) -> f32 {
        self.edge_len(0, 1).max(self.edge_len(3, 2))
    }

    pub fn height_f32(&self) -> f32 {
        self.edge_len(0, 3).max(self.edge_len(1, 2))
    }

    pub fn short_side(&self) -> f32 {
        self.width_f32().min(self.height_f32())
    }

    /// (x0, y0, x1, y1) with floor/ceil so the box covers its pixels.
    pub fn axis_aligned_bounds(&self) -> (u32, u32, u32, u32) {
        let xs = self.points.map(|p| p[0]);
        let ys = self.points.map(|p| p[1]);
        let x0 = xs.iter().fold(f32::INFINITY, |a, b| a.min(*b)).floor().max(0.0) as u32;
        let y0 = ys.iter().fold(f32::INFINITY, |a, b| a.min(*b)).floor().max(0.0) as u32;
        let x1 = xs.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b)).ceil().max(0.0) as u32;
        let y1 = ys.iter().fold(f32::NEG_INFINITY, |a, b| a.max(*b)).ceil().max(0.0) as u32;
        (x0, y0, x1, y1)
    }

    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        // Ray cast to +x.
        let mut inside = false;
        for i in 0..4 {
            let a = self.points[i];
            let b = self.points[(i + 1) % 4];
            if (a[1] > y) != (b[1] > y) {
                let xin = a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
                if x < xin {
                    inside = !inside;
                }
            }
        }
        inside
    }

    pub fn crop_width(&self) -> u32 {
        self.width_f32().round().max(1.0) as u32
    }

    pub fn crop_height(&self) -> u32 {
        self.height_f32().round().max(1.0) as u32
    }

    /// Horizontal strip of an ordered (tl, tr, br, bl) quad between width
    /// fractions f0..f1. Used to map CTC char spans back onto the screen.
    /// For tall (vertical-text) quads the caller should fall back to the
    /// whole box; only near-horizontal strips are meaningful here.
    pub fn hstrip(&self, f0: f32, f1: f32) -> Quad {
        let top = |f: f32| {
            [
                self.points[0][0] + (self.points[1][0] - self.points[0][0]) * f,
                self.points[0][1] + (self.points[1][1] - self.points[0][1]) * f,
            ]
        };
        let bot = |f: f32| {
            [
                self.points[3][0] + (self.points[2][0] - self.points[3][0]) * f,
                self.points[3][1] + (self.points[2][1] - self.points[3][1]) * f,
            ]
        };
        Quad { points: [top(f0), top(f1), bot(f1), bot(f0)] }
    }

    pub fn is_horizontal(&self) -> bool {
        self.height_f32() <= self.width_f32() * 1.2 + 2.0
    }
}

fn cross(o: &Point, a: &Point, b: &Point) -> f32 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

fn convex_hull(mut pts: Vec<Point>) -> Vec<Point> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| a.x == b.x && a.y == b.y);
    if pts.len() <= 1 {
        return pts;
    }
    let mut lower: Vec<Point> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(&lower[lower.len() - 2], &lower[lower.len() - 1], p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Point> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(&upper[upper.len() - 2], &upper[upper.len() - 1], p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Minimum-area enclosing rectangle via rotating calipers over the convex hull.
pub fn min_area_rect(points: &[Point]) -> Option<Quad> {
    let hull = convex_hull(points.to_vec());
    if hull.len() < 3 {
        if hull.is_empty() {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for p in &hull {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        return Some(Quad::from_xyxy(x0, y0, (x1 + 1.0).max(x0 + 1.0), (y1 + 1.0).max(y0 + 1.0)));
    }
    let n = hull.len();
    let mut best_area = f32::INFINITY;
    let mut best: [[f32; 2]; 4] = [[0.0; 2]; 4];
    for i in 0..n {
        let p0 = hull[i];
        let p1 = hull[(i + 1) % n];
        let ex = p1.x - p0.x;
        let ey = p1.y - p0.y;
        let len = (ex * ex + ey * ey).sqrt();
        if len < 1e-9 {
            continue;
        }
        let (ux, uy) = (ex / len, ey / len);
        let (mut min_u, mut max_u, mut min_v, mut max_v) =
            (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
        for p in &hull {
            let u = p.x * ux + p.y * uy;
            let v = -p.x * uy + p.y * ux;
            min_u = min_u.min(u);
            max_u = max_u.max(u);
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }
        let area = (max_u - min_u) * (max_v - min_v);
        if area < best_area {
            best_area = area;
            let corners = [(min_u, min_v), (max_u, min_v), (max_u, max_v), (min_u, max_v)];
            for (k, (cu, cv)) in corners.iter().enumerate() {
                best[k] = [cu * ux - cv * uy, cu * uy + cv * ux];
            }
        }
    }
    if !best_area.is_finite() {
        return None;
    }
    Some(Quad { points: best })
}

/// Approximate pyclipper expansion: offset each vertex away from the centroid by
/// d = area * ratio / perimeter.
pub fn unclip_quad(quad: &Quad, ratio: f32) -> Option<Vec<Point>> {
    let pts = &quad.points;
    let mut area = 0.0f32;
    let mut perim = 0.0f32;
    for i in 0..4 {
        let a = pts[i];
        let b = pts[(i + 1) % 4];
        area += a[0] * b[1] - b[0] * a[1];
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        perim += (dx * dx + dy * dy).sqrt();
    }
    area = area.abs() * 0.5;
    if perim < 1e-6 {
        return None;
    }
    let d = area * ratio / perim;
    let c = quad.centroid();
    let out: Vec<Point> = pts
        .iter()
        .map(|p| {
            let dx = p[0] - c[0];
            let dy = p[1] - c[1];
            let l = (dx * dx + dy * dy).sqrt().max(1e-6);
            Point::new(p[0] + dx / l * d, p[1] + dy / l * d)
        })
        .collect();
    Some(out)
}
