//! Guided perspective: up to four lines drawn by the user (each meant to be vertical or horizontal) or one
//! quadrilateral placed on a rectangle in the scene determine a general homography (8 degrees of freedom).
//!
//! Textbook rectification from vanishing points (Hartley/Zisserman, "Multiple View Geometry", ch. 2.7):
//! the vertical lines meet in the vertical vanishing point V, the horizontal ones in H. The projective part sends
//! the vanishing line V × H to infinity, the affine part turns V's direction to the y axis and H's to the x axis.
//! The image centre stays fixed with scale 1 (area) there, so crop, masks and retouching stay in place when only
//! the lines change slightly. With one line of a kind its direction stands in for the vanishing point (rotation
//! only); with three or more the vanishing point is the least-squares intersection.
//!
//! All points are stored in the upright image (after 90° steps, before perspective), normalised 0 … 1 per axis.

use serde::{Deserialize, Serialize};

/// Maximum number of guide lines.
pub const MAX_LINES: usize = 4;

/// What a guide line should become.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[serde(rename = "v")]
    Vertical,
    #[serde(rename = "h")]
    Horizontal,
}

impl Role {
    /// Role by angle: steeper than 45° → vertical (in image pixels, `aspect` = width / height of the upright image).
    pub fn by_angle(p0: [f32; 2], p1: [f32; 2], aspect: f32) -> Self {
        let (dx, dy) = ((p1[0] - p0[0]) * aspect, p1[1] - p0[1]);
        if dy.abs() > dx.abs() { Role::Vertical } else { Role::Horizontal }
    }

    pub fn toggled(self) -> Self {
        match self {
            Role::Vertical => Role::Horizontal,
            Role::Horizontal => Role::Vertical,
        }
    }
}

/// One guide line, end points normalised 0 … 1 in the upright image.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    pub role: Role,
}

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Guided perspective of a photo (part of the settings). Empty = off: the sliders of the old keystone model apply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Guides {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
    /// Quadrilateral on a rectangle in the scene: top left, top right, bottom right, bottom left (wins over `lines`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quad: Option<[[f32; 2]; 4]>,
    /// Aspect ratio correction −1 … +1 (0 = area-neutral at the centre; +1 = 1.5 × wider)
    #[serde(skip_serializing_if = "is_zero")]
    pub aspect: f32,
    /// Strength 0 … 1 of the projective part (1 = lines exactly vertical/horizontal)
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub amount: f32,
}

impl Default for Guides {
    fn default() -> Self {
        Self { lines: Vec::new(), quad: None, aspect: 0.0, amount: 1.0 }
    }
}

impl Guides {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Nothing set: the old perspective sliders apply.
    pub fn is_off(&self) -> bool {
        self.lines.is_empty() && self.quad.is_none()
    }

    /// The lines that are solved for (a quadrilateral as two vertical and two horizontal edges).
    pub fn effective_lines(&self) -> Vec<Line> {
        match self.quad {
            Some([a, b, c, d]) => vec![
                Line { p0: a, p1: d, role: Role::Vertical },
                Line { p0: b, p1: c, role: Role::Vertical },
                Line { p0: a, p1: b, role: Role::Horizontal },
                Line { p0: d, p1: c, role: Role::Horizontal },
            ],
            None => self.lines.iter().take(MAX_LINES).copied().collect(),
        }
    }
}

/// A homography in centred coordinates: x_n = (x − w/2) / r with r = half the long side of the upright image.
/// `fwd` maps the rectified image to the upright image, `inv` the reverse (row-major 3 × 3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hom {
    pub fwd: [f32; 9],
    pub inv: [f32; 9],
}

/// Applies a row-major 3 × 3 matrix to (x, y, 1); the denominator is kept away from 0.
pub fn apply(m: &[f32; 9], p: [f32; 2]) -> [f32; 2] {
    let w = (m[6] * p[0] + m[7] * p[1] + m[8]).max(1e-4);
    [(m[0] * p[0] + m[1] * p[1] + m[2]) / w, (m[3] * p[0] + m[4] * p[1] + m[5]) / w]
}

/// Result of [`solve`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solution {
    pub hom: Hom,
    /// The correction was too strong and has been reduced
    pub limited: bool,
}

/// Smallest ratio of the denominators over the image corners (keeps the vanishing line well outside the image) and the
/// largest allowed ratio between them (one image edge at most this many times as large as the other).
const MIN_W: f64 = 0.3;
const MAX_RATIO: f64 = 3.5;

/// Guide end points farther outside the image than this (normalised) are ignored (manipulated or broken values).
const MAX_COORD: f32 = 10.0;

type V3 = [f64; 3];

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// Eigenvector of the smallest eigenvalue of a symmetric 3 × 3 matrix (Jacobi rotations).
fn smallest_eigenvector(mut a: [[f64; 3]; 3]) -> V3 {
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..50 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off < 1e-15 {
            break;
        }
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-18 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let t = if theta == 0.0 { 1.0 } else { t };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for k in 0..3 {
                let (akp, akq) = (a[k][p], a[k][q]);
                a[k][p] = c * akp - s * akq;
                a[k][q] = s * akp + c * akq;
            }
            for k in 0..3 {
                let (apk, aqk) = (a[p][k], a[q][k]);
                a[p][k] = c * apk - s * aqk;
                a[q][k] = s * apk + c * aqk;
            }
            for row in v.iter_mut() {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    let i = (0..3).min_by(|&i, &j| a[i][i].total_cmp(&a[j][j])).unwrap_or(0);
    [v[0][i], v[1][i], v[2][i]]
}

/// Vanishing point of a group of lines (homogeneous): one line → its direction at infinity; two → their
/// intersection; more → least squares.
fn vanishing(lines: &[(V3, V3)]) -> Option<V3> {
    match lines.len() {
        0 => None,
        1 => {
            let (a, b) = lines[0];
            Some([b[0] - a[0], b[1] - a[1], 0.0])
        }
        _ => {
            let mut m = [[0.0f64; 3]; 3];
            for (a, b) in lines {
                let l = cross(*a, *b);
                let n = (l[0] * l[0] + l[1] * l[1]).sqrt();
                if n < 1e-12 {
                    continue;
                }
                let l = [l[0] / n, l[1] / n, l[2] / n];
                for i in 0..3 {
                    for j in 0..3 {
                        m[i][j] += l[i] * l[j];
                    }
                }
            }
            let v = smallest_eigenvector(m);
            (norm(v) > 1e-12).then_some(v)
        }
    }
}

/// Unit direction from the image centre (origin) towards the vanishing point.
fn direction(p: V3) -> [f64; 2] {
    let n = norm(p);
    let (x, y) = if p[2].abs() < 1e-9 * n { (p[0], p[1]) } else { (p[0] / p[2], p[1] / p[2]) };
    let l = (x * x + y * y).sqrt().max(1e-300);
    [x / l, y / l]
}

fn invert(m: &[f64; 9]) -> Option<[f64; 9]> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        (m[4] * m[8] - m[5] * m[7]) * d,
        (m[2] * m[7] - m[1] * m[8]) * d,
        (m[1] * m[5] - m[2] * m[4]) * d,
        (m[5] * m[6] - m[3] * m[8]) * d,
        (m[0] * m[8] - m[2] * m[6]) * d,
        (m[2] * m[3] - m[0] * m[5]) * d,
        (m[3] * m[7] - m[4] * m[6]) * d,
        (m[1] * m[6] - m[0] * m[7]) * d,
        (m[0] * m[4] - m[1] * m[3]) * d,
    ])
}

/// Solves the homography for an upright image of `ow` × `oh` pixels. `None` if nothing is set or the lines contradict
/// each other (e.g. a vertical and a horizontal guide in the same direction).
pub fn solve(g: &Guides, ow: f32, oh: f32) -> Option<Solution> {
    let lines = g.effective_lines();
    // values from a file can be anything: no image size, NaN or infinite numbers give no correction at all
    if lines.is_empty() || !(ow.is_finite() && oh.is_finite() && ow >= 1.0 && oh >= 1.0) {
        return None;
    }
    let (ow, oh) = (ow as f64, oh as f64);
    let r = 0.5 * ow.max(oh);
    let to_n = |p: [f32; 2]| -> V3 { [(p[0] as f64 - 0.5) * ow / r, (p[1] as f64 - 0.5) * oh / r, 1.0] };
    let group = |role: Role| -> Vec<(V3, V3)> {
        lines
            .iter()
            .filter(|l| l.role == role && l.p0.iter().chain(&l.p1).all(|v| v.is_finite() && v.abs() <= MAX_COORD))
            .map(|l| (to_n(l.p0), to_n(l.p1)))
            .filter(|(a, b)| (a[0] - b[0]).hypot(a[1] - b[1]) > 1e-4)
            .collect()
    };
    let (vl, hl) = (group(Role::Vertical), group(Role::Horizontal));
    let v = vanishing(&vl);
    let h = vanishing(&hl);
    let (v, h) = match (v, h) {
        (None, None) => return None,
        (Some(v), None) => {
            let d = direction(v);
            (v, [-d[1], d[0], 0.0])
        }
        (None, Some(h)) => {
            let d = direction(h);
            ([-d[1], d[0], 0.0], h)
        }
        (Some(v), Some(h)) => (v, h),
    };
    // projective part: the vanishing line through V and H goes to infinity
    let l = cross(v, h);
    let (mut a, mut b) = if l[2].abs() > 1e-9 * norm(l) { (l[0] / l[2], l[1] / l[2]) } else if norm(l) < 1e-12 { (0.0, 0.0) } else { return None };
    let amount = if g.amount.is_finite() { g.amount.clamp(0.0, 1.0) as f64 } else { 1.0 };
    a *= amount;
    b *= amount;
    // limit extreme distortion: shrink the projective part until the image corners stay well in front of the camera
    let corners = [[-ow / 2.0 / r, -oh / 2.0 / r], [ow / 2.0 / r, -oh / 2.0 / r], [ow / 2.0 / r, oh / 2.0 / r], [-ow / 2.0 / r, oh / 2.0 / r]];
    let ok = |k: f64| {
        let w: Vec<f64> = corners.iter().map(|c| 1.0 + k * (a * c[0] + b * c[1])).collect();
        let lo = w.iter().cloned().fold(f64::MAX, f64::min);
        let hi = w.iter().cloned().fold(f64::MIN, f64::max);
        lo >= MIN_W && hi / lo <= MAX_RATIO
    };
    let mut limited = false;
    if !ok(1.0) {
        limited = true;
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..40 {
            let k = (lo + hi) / 2.0;
            if ok(k) {
                lo = k;
            } else {
                hi = k;
            }
        }
        a *= lo;
        b *= lo;
    }
    // affine part: V's direction becomes the y axis, H's the x axis; centre fixed
    let dv = direction(v);
    let dh = direction(h);
    let mut n1 = [dv[1], -dv[0]];
    if n1[0] < 0.0 {
        n1 = [-n1[0], -n1[1]];
    }
    let mut n2 = [-dh[1], dh[0]];
    if n2[1] < 0.0 {
        n2 = [-n2[0], -n2[1]];
    }
    let det = n1[0] * n2[1] - n1[1] * n2[0];
    if det < 0.05 {
        return None;
    }
    let k = 1.0 / det.sqrt();
    let aspect = if g.aspect.is_finite() { g.aspect.clamp(-1.0, 1.0) as f64 } else { 0.0 };
    let f = 1.5f64.powf(aspect).sqrt();
    let (sx, sy) = (k * f, k / f);
    let rect = [n1[0] * sx, n1[1] * sx, 0.0, n2[0] * sy, n2[1] * sy, 0.0, a, b, 1.0];
    let fwd = invert(&rect)?;
    let to32 = |m: [f64; 9]| m.map(|v| v as f32);
    if !rect.iter().chain(&fwd).all(|v| v.is_finite() && v.abs() < 1e6) {
        return None;
    }
    Some(Solution { hom: Hom { fwd: to32(fwd), inv: to32(rect) }, limited })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f32 = 6000.0;
    const H: f32 = 4000.0;

    /// Pixel point in the upright image → rectified image through the solution.
    fn rect(s: &Solution, p: [f32; 2]) -> [f32; 2] {
        let r = 0.5 * W.max(H);
        let q = apply(&s.hom.inv, [(p[0] - W / 2.0) / r, (p[1] - H / 2.0) / r]);
        [q[0] * r + W / 2.0, q[1] * r + H / 2.0]
    }

    fn line(p0: [f32; 2], p1: [f32; 2], role: Role) -> Line {
        Line { p0: [p0[0] / W, p0[1] / H], p1: [p1[0] / W, p1[1] / H], role }
    }

    fn slope_deg(a: [f32; 2], b: [f32; 2], vertical: bool) -> f32 {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        if vertical { dx.atan2(dy).to_degrees() } else { dy.atan2(dx).to_degrees() }
    }

    fn check(g: &Guides) -> Solution {
        let s = solve(g, W, H).expect("solution");
        for l in g.effective_lines() {
            let (a, b) = (rect(&s, [l.p0[0] * W, l.p0[1] * H]), rect(&s, [l.p1[0] * W, l.p1[1] * H]));
            let d = slope_deg(a, b, l.role == Role::Vertical);
            let d = (d + 90.0).rem_euclid(180.0) - 90.0;
            assert!(d.abs() < 0.05, "{l:?} still {d}°");
        }
        // forward and inverse agree, centre fixed
        let c = rect(&s, [W / 2.0, H / 2.0]);
        assert!((c[0] - W / 2.0).abs() < 0.01 && (c[1] - H / 2.0).abs() < 0.01, "{c:?}");
        let p = apply(&s.hom.fwd, apply(&s.hom.inv, [0.3, -0.2]));
        assert!((p[0] - 0.3).abs() < 1e-4 && (p[1] + 0.2).abs() < 1e-4);
        s
    }

    #[test]
    fn one_line_is_rotation() {
        let g = Guides { lines: vec![line([1000.0, 3000.0], [5000.0, 2800.0], Role::Horizontal)], ..Default::default() };
        let s = check(&g);
        assert_eq!(s.hom.inv[6], 0.0);
        assert_eq!(s.hom.inv[7], 0.0);
        assert!(!s.limited);
    }

    #[test]
    fn two_verticals_converging() {
        // building seen from below: verticals lean inwards at the top
        let g = Guides { lines: vec![line([1500.0, 3800.0], [1800.0, 300.0], Role::Vertical), line([4600.0, 3800.0], [4250.0, 250.0], Role::Vertical)], ..Default::default() };
        let s = check(&g);
        assert!(!s.limited);
    }

    #[test]
    fn two_plus_two_full() {
        let g = Guides {
            lines: vec![
                line([1500.0, 3800.0], [1800.0, 300.0], Role::Vertical),
                line([4600.0, 3700.0], [4250.0, 250.0], Role::Vertical),
                line([1000.0, 900.0], [5000.0, 600.0], Role::Horizontal),
                line([900.0, 3300.0], [5100.0, 3500.0], Role::Horizontal),
            ],
            ..Default::default()
        };
        check(&g);
        // aspect changes the ratio, nothing else: lines still straight
        check(&Guides { aspect: 0.6, ..g.clone() });
    }

    #[test]
    fn more_lines_least_squares() {
        let g = Guides {
            lines: vec![
                line([1500.0, 3800.0], [1800.0, 300.0], Role::Vertical),
                line([4600.0, 3800.0], [4250.0, 250.0], Role::Vertical),
                line([3000.0, 3800.0], [3015.0, 300.0], Role::Vertical),
            ],
            ..Default::default()
        };
        let s = solve(&g, W, H).unwrap();
        for l in g.effective_lines() {
            let (a, b) = (rect(&s, [l.p0[0] * W, l.p0[1] * H]), rect(&s, [l.p1[0] * W, l.p1[1] * H]));
            let d = (slope_deg(a, b, true) + 90.0).rem_euclid(180.0) - 90.0;
            assert!(d.abs() < 0.5, "{d}");
        }
    }

    #[test]
    fn quad_becomes_rectangle() {
        let q = [[1200.0, 800.0], [4700.0, 1100.0], [4500.0, 3300.0], [1400.0, 3600.0]].map(|p: [f32; 2]| [p[0] / W, p[1] / H]);
        let g = Guides { quad: Some(q), ..Default::default() };
        let s = check(&g);
        let c = q.map(|p| rect(&s, [p[0] * W, p[1] * H]));
        assert!((c[0][1] - c[1][1]).abs() < 0.5 && (c[3][1] - c[2][1]).abs() < 0.5);
        assert!((c[0][0] - c[3][0]).abs() < 0.5 && (c[1][0] - c[2][0]).abs() < 0.5);
    }

    #[test]
    fn extreme_is_limited() {
        // nearly meeting verticals: the vanishing point lies just above the image
        let g = Guides { lines: vec![line([500.0, 3900.0], [2900.0, 100.0], Role::Vertical), line([5500.0, 3900.0], [3100.0, 100.0], Role::Vertical)], ..Default::default() };
        let s = solve(&g, W, H).unwrap();
        assert!(s.limited);
        let r = 0.5 * W.max(H);
        for c in [[-W / 2.0, -H / 2.0], [W / 2.0, -H / 2.0], [W / 2.0, H / 2.0], [-W / 2.0, H / 2.0]] {
            let w = s.hom.inv[6] * c[0] / r + s.hom.inv[7] * c[1] / r + 1.0;
            assert!(w >= 0.29, "{w}");
        }
    }

    #[test]
    fn contradiction_is_none() {
        let l = line([1000.0, 1000.0], [5000.0, 1000.0], Role::Horizontal);
        let g = Guides { lines: vec![l, Line { role: Role::Vertical, ..l }], ..Default::default() };
        assert!(solve(&g, W, H).is_none());
    }

    #[test]
    fn serde_round_trip() {
        let g = Guides { lines: vec![line([1.0, 2.0], [3.0, 4000.0], Role::Vertical)], aspect: 0.25, amount: 0.8, quad: None };
        let j = serde_json::to_string(&g).unwrap();
        assert!(j.contains("\"role\":\"v\""), "{j}");
        let back: Guides = serde_json::from_str(&j).unwrap();
        assert_eq!(back, g);
        assert_eq!(serde_json::to_string(&Guides::default()).unwrap(), "{}");
    }
}
