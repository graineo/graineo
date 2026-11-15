//! Auto upright: find straight edges in the preview and
//! estimate angle and perspective from them.
//!
//! Edge detection modelled on the LSD method (Line Segment Detector, von Gioi et al.),
//! heavily simplified and written from scratch: logarithmic brightness (contrast independent of
//! exposure), gradients over 2 × 2 pixels, then regions of equal edge direction are grown from the
//! strongest pixels (tolerance 22.5°). A region counts as a line if it is
//! long, narrow, dense and straight; foliage, clouds and waves drop out this way.
//!
//! Estimation: for angle and perspective the end points of the lines are mapped through the
//! rectification and the deviation from vertical or horizontal is measured. The search runs
//! coarsely on a grid, then finely; costs are capped (robust) so that individual slanted lines
//! (roofs, branches) do not pull. Without enough lines nothing is changed.

use crate::geometry::Frame;
use crate::Linear;

/// Long edge of the analysis image (pixels).
const MAX_EDGE: usize = 1024;
/// Minimum gradient (natural logarithm of brightness per pixel).
const GRAD_MIN: f32 = 0.08;
/// Angle tolerance when growing a region (degrees).
const TOLERANCE: f32 = 22.5;

/// Straight edge in the upright image (full resolution, before perspective).
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    pub len: f32,
}

/// What "upright" should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Only the angle (horizon/horizontals)
    Level,
    /// Angle and converging verticals (vertical perspective)
    Vertical,
    /// Angle, vertical and moderately horizontal
    Auto,
}

/// Result: new values for angle and perspective.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Upright {
    pub angle: f32,
    pub pv: f32,
    pub ph: f32,
    /// Number of lines supporting the result
    pub lines: usize,
}

/// Finds straight edges in the preview `lin` (sensor orientation). `base` describes the whole image at
/// full resolution (sensor size and 90° rotation); the segments lie in the upright image.
pub fn detect(lin: &Linear, base: &Frame) -> Vec<Segment> {
    let k = lin.width.max(lin.height).div_ceil(MAX_EDGE).max(1);
    let (w, h) = (lin.width / k, lin.height / k);
    if w < 16 || h < 16 {
        return Vec::new();
    }
    // Brightness (block mean), then logarithmic
    let mut g = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for yy in y * k..y * k + k {
                let row = &lin.data[(yy * lin.width + x * k) * 3..(yy * lin.width + x * k + k) * 3];
                for p in row.chunks_exact(3) {
                    acc += p[0] + p[1] + p[2];
                }
            }
            g[y * w + x] = acc.max(0.0);
        }
    }
    let mean = (g.iter().map(|&v| v as f64).sum::<f64>() / g.len() as f64) as f32;
    let eps = (mean * 0.02).max(1e-6);
    for v in &mut g {
        *v = (*v + eps).ln();
    }
    // Gradient between each 2 × 2 pixels (centre at x + 1, y + 1 in pixel edge coordinates)
    let (gw, gh) = (w - 1, h - 1);
    let mut mag = vec![0.0f32; gw * gh];
    let mut ang = vec![0.0f32; gw * gh];
    let mut gxy = vec![[0.0f32; 2]; gw * gh];
    for y in 0..gh {
        for x in 0..gw {
            let (a, b, c, d) = (g[y * w + x], g[y * w + x + 1], g[(y + 1) * w + x], g[(y + 1) * w + x + 1]);
            let gx = (b + d - a - c) * 0.5;
            let gy = (c + d - a - b) * 0.5;
            let m = (gx * gx + gy * gy).sqrt();
            let i = y * gw + x;
            mag[i] = m;
            gxy[i] = [gx, gy];
            // Direction of the edge (perpendicular to the gradient), modulo 180°
            ang[i] = gx.atan2(-gy).rem_euclid(std::f32::consts::PI);
        }
    }
    // Thin edges (only maxima across the edge, as in Canny); otherwise closely
    // adjacent edges (cornices, stone joints) merge into wide blobs
    let mut thin = vec![0.0f32; gw * gh];
    let mut cand: Vec<u32> = Vec::new();
    for y in 1..gh.saturating_sub(1) {
        for x in 1..gw - 1 {
            let i = y * gw + x;
            let m = mag[i];
            if m <= GRAD_MIN {
                continue;
            }
            let [gx, gy] = gxy[i];
            let (sx, sy) = ((gx / m).round() as isize, (gy / m).round() as isize);
            let at = |dx: isize, dy: isize| mag[(y as isize + dy) as usize * gw + (x as isize + dx) as usize];
            if m >= at(sx, sy) && m > at(-sx, -sy) {
                thin[i] = m;
                cand.push(i as u32);
            }
        }
    }
    let mag = thin;
    cand.sort_unstable_by(|&a, &b| mag[b as usize].total_cmp(&mag[a as usize]));
    let tol = TOLERANCE.to_radians();
    let ang_diff = |a: f32, b: f32| {
        let d = (a - b).rem_euclid(std::f32::consts::PI);
        d.min(std::f32::consts::PI - d)
    };
    let mut used = vec![false; gw * gh];
    let min_len = 0.035 * w.max(h) as f32;
    let scale = [k as f32 * base.sw / lin.width as f32, k as f32 * base.sh / lin.height as f32];
    let mut out = Vec::new();
    let mut region: Vec<u32> = Vec::new();
    for &seed in &cand {
        if used[seed as usize] {
            continue;
        }
        region.clear();
        region.push(seed);
        used[seed as usize] = true;
        let a0 = ang[seed as usize] * 2.0;
        let (mut sc, mut ss) = (a0.cos(), a0.sin());
        let mut phi = ang[seed as usize];
        let mut i = 0;
        while i < region.len() {
            let p = region[i] as usize;
            let (px, py) = ((p % gw) as isize, (p / gw) as isize);
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let (nx, ny) = (px + dx, py + dy);
                    if nx < 0 || ny < 0 || nx >= gw as isize || ny >= gh as isize {
                        continue;
                    }
                    let n = ny as usize * gw + nx as usize;
                    if used[n] || mag[n] <= GRAD_MIN || ang_diff(ang[n], phi) > tol {
                        continue;
                    }
                    used[n] = true;
                    region.push(n as u32);
                    let a2 = ang[n] * 2.0;
                    sc += a2.cos();
                    ss += a2.sin();
                    phi = (ss.atan2(sc) * 0.5).rem_euclid(std::f32::consts::PI);
                }
            }
            i += 1;
        }
        if region.len() < 10 || (region.len() as f32) < min_len {
            continue;
        }
        // Centroid and principal axis (weighted by the gradient)
        let (mut sw, mut mx, mut my) = (0.0f64, 0.0f64, 0.0f64);
        for &p in &region {
            let p = p as usize;
            let m = mag[p] as f64;
            sw += m;
            mx += m * (p % gw) as f64;
            my += m * (p / gw) as f64;
        }
        let (mx, my) = (mx / sw, my / sw);
        let (mut cxx, mut cyy, mut cxy) = (0.0f64, 0.0f64, 0.0f64);
        for &p in &region {
            let p = p as usize;
            let m = mag[p] as f64;
            let (dx, dy) = ((p % gw) as f64 - mx, (p / gw) as f64 - my);
            cxx += m * dx * dx;
            cyy += m * dy * dy;
            cxy += m * dx * dy;
        }
        let theta = 0.5 * (2.0 * cxy).atan2(cxx - cyy);
        let (dx, dy) = (theta.cos(), theta.sin());
        let (mut t0, mut t1, mut s0, mut s1, mut rms) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN, 0.0f64);
        for &p in &region {
            let p = p as usize;
            let (rx, ry) = ((p % gw) as f64 - mx, (p / gw) as f64 - my);
            let t = rx * dx + ry * dy;
            let s = -rx * dy + ry * dx;
            t0 = t0.min(t);
            t1 = t1.max(t);
            s0 = s0.min(s);
            s1 = s1.max(s);
            rms += s * s;
        }
        let len = (t1 - t0 + 1.0) as f32;
        let width = (s1 - s0 + 1.0) as f32;
        let rms = (rms / region.len() as f64).sqrt() as f32;
        let density = region.len() as f32 / (len * width);
        if len < min_len || len < 8.0 * width || density < 0.3 || rms > 1.0 {
            continue;
        }
        // Aligned with the region angle? (otherwise it is more likely a texture of short strokes)
        if ang_diff(theta.rem_euclid(std::f64::consts::PI) as f32, phi) > tol {
            continue;
        }
        let to_full = |t: f64| {
            let (x, y) = (mx + dx * t + 1.0, my + dy * t + 1.0);
            base.src_to_ori([x as f32 * scale[0], y as f32 * scale[1]])
        };
        let (p0, p1) = (to_full(t0), to_full(t1));
        let l = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt();
        out.push(Segment { p0, p1, len: l });
    }
    out
}

/// Deviation of the line from vertical (`vertical`) or horizontal in degrees, after
/// it has been mapped with the perspective and angle of `f`.
fn deviation(f: &Frame, s: &Segment, vertical: bool) -> f32 {
    let (a, b) = (f.ori_to_persp(s.p0), f.ori_to_persp(s.p1));
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let r = f.angle.to_radians();
    let (sn, cs) = (r.sin(), r.cos());
    let (lx, ly) = (cs * dx + sn * dy, -sn * dx + cs * dy);
    let d = if vertical { lx.atan2(ly) } else { ly.atan2(lx) };
    // modulo 180°, to −90 … 90
    let d = d.to_degrees();
    (d + 90.0).rem_euclid(180.0) - 90.0
}

/// Direction in the upright image (degrees, 0 = horizontal, −90 … 90).
fn raw_angle(s: &Segment) -> f32 {
    let d = (s.p1[1] - s.p0[1]).atan2(s.p1[0] - s.p0[0]).to_degrees();
    (d + 90.0).rem_euclid(180.0) - 90.0
}

/// Capped quadratic cost (degrees): outliers count at most `cap²`.
fn rho(e: f32, cap: f32) -> f32 {
    (e * e).min(cap * cap)
}

struct Lines<'a> {
    vert: Vec<&'a Segment>,
    horiz: Vec<&'a Segment>,
}

fn classify(segs: &[Segment]) -> Lines<'_> {
    let mut l = Lines { vert: Vec::new(), horiz: Vec::new() };
    for s in segs {
        let a = raw_angle(s).abs();
        if a >= 60.0 {
            l.vert.push(s);
        } else if a <= 20.0 {
            l.horiz.push(s);
        }
    }
    l
}

/// Sum of the lengths of the lines that deviate by at most `tol` degrees at `f`, and their count.
fn support(f: &Frame, segs: &[&Segment], vertical: bool, tol: f32) -> (f32, usize) {
    segs.iter().filter(|s| deviation(f, s, vertical).abs() <= tol).fold((0.0, 0), |(l, n), s| (l + s.len, n + 1))
}

/// Angle only: horizontals in the whole image, verticals near the image centre (those stay
/// vertical with converging lines). Perspective stays as it is.
fn level(lines: &Lines, base: &Frame) -> Option<(f32, usize)> {
    let (ow, oh) = base.oriented();
    let center_w = |s: &Segment| {
        let p = base.ori_to_persp([(s.p0[0] + s.p1[0]) / 2.0, (s.p0[1] + s.p1[1]) / 2.0]);
        let dx = (p[0] - ow / 2.0) / (0.2 * ow);
        (-dx * dx).exp()
    };
    let vw: Vec<f32> = lines.vert.iter().map(|s| s.len * if base.has_perspective() { 1.0 } else { center_w(s) }).collect();
    let cost = |angle: f32, cap: f32| {
        let f = Frame { angle, ..*base };
        let h: f32 = lines.horiz.iter().map(|s| s.len * rho(deviation(&f, s, false), cap)).sum();
        let v: f32 = lines.vert.iter().zip(&vw).map(|(s, w)| w * rho(deviation(&f, s, true), cap)).sum();
        h + v
    };
    let best = search1(-15.0, 15.0, 0.25, |a| cost(a, 2.0));
    let best = search1(best - 0.3, best + 0.3, 0.02, |a| cost(a, 0.7));
    let f = Frame { angle: best, ..*base };
    let (lh, nh) = support(&f, &lines.horiz, false, 0.6);
    let (lv, nv) = lines.vert.iter().zip(&vw).filter(|(s, _)| deviation(&f, s, true).abs() <= 0.6).fold((0.0, 0), |(l, n), (s, w)| (l + s.len * w, n + 1));
    let total: f32 = lines.horiz.iter().map(|s| s.len).sum::<f32>() + vw.iter().sum::<f32>();
    let sup = lh + lv;
    // viable: enough length and a clear share of all candidates
    (sup >= 0.3 * ow.max(oh) && sup >= 0.25 * total && nh + nv >= 1).then_some((best, nh + nv))
}

fn search1(lo: f32, hi: f32, step: f32, cost: impl Fn(f32) -> f32) -> f32 {
    let n = ((hi - lo) / step).round() as i32;
    (0..=n).map(|i| lo + i as f32 * step).map(|x| (x, cost(x))).min_by(|a, b| a.1.total_cmp(&b.1)).map_or(0.0, |r| r.0)
}

/// Angle and vertical perspective (converging verticals); horizontal is kept at `ph`.
fn vertical(lines: &Lines, base: &Frame, ph: f32) -> Option<(f32, f32, usize)> {
    if lines.vert.len() < 2 {
        return None;
    }
    let total_v: f32 = lines.vert.iter().map(|s| s.len).sum();
    let cost = |angle: f32, pv: f32, cap: f32| {
        let f = Frame { angle, pv, ph, ..*base };
        let v: f32 = lines.vert.iter().map(|s| s.len * rho(deviation(&f, s, true), cap)).sum();
        let h: f32 = lines.horiz.iter().map(|s| 0.5 * s.len * rho(deviation(&f, s, false), cap)).sum();
        // slight preference for little correction (for equally good solutions)
        v + h + 0.05 * pv * pv * total_v * cap * cap
    };
    let mut best = (0.0f32, 0.0f32, f32::MAX);
    for i in 0..=40 {
        let pv = -1.0 + i as f32 * 0.05;
        for j in 0..=48 {
            let a = -12.0 + j as f32 * 0.5;
            let c = cost(a, pv, 2.5);
            if c < best.2 {
                best = (a, pv, c);
            }
        }
    }
    let (a0, p0) = (best.0, best.1);
    let mut best = (a0, p0, f32::MAX);
    for i in -10..=10 {
        let pv = (p0 + i as f32 * 0.005).clamp(-1.0, 1.0);
        for j in -12..=12 {
            let a = a0 + j as f32 * 0.04;
            let c = cost(a, pv, 0.8);
            if c < best.2 {
                best = (a, pv, c);
            }
        }
    }
    let f = Frame { angle: best.0, pv: best.1, ph, ..*base };
    let (_, oh) = base.oriented();
    let (lv, nv) = support(&f, &lines.vert, true, 0.6);
    // at least three verticals that together make up a good half of the image height
    (nv >= 3 && lv >= 0.5 * oh && lv >= 0.2 * total_v).then_some((best.0, best.1, nv))
}

/// Horizontal perspective from the horizontals (with fixed angle and `pv`).
fn horizontal(lines: &Lines, base: &Frame) -> Option<(f32, usize)> {
    let horiz: Vec<&Segment> = lines.horiz.clone();
    if horiz.len() < 3 {
        return None;
    }
    let cost = |ph: f32, cap: f32| {
        let f = Frame { ph, ..*base };
        horiz.iter().map(|s| s.len * rho(deviation(&f, s, false), cap)).sum::<f32>()
    };
    let p = search1(-1.0, 1.0, 0.04, |p| cost(p, 2.5));
    let p = search1(p - 0.04, p + 0.04, 0.004, |p| cost(p, 0.8));
    let f = Frame { ph: p, ..*base };
    let (ow, _) = base.oriented();
    let (lh, nh) = support(&f, &horiz, false, 0.6);
    (nh >= 3 && lh >= 0.6 * ow).then_some((p, nh))
}

/// Estimates angle and perspective. `base`: whole image with the current values (the user's angle and
/// perspective). `Err` with a short message if the lines are not sufficient.
pub fn solve(segs: &[Segment], base: &Frame, mode: Mode) -> Result<Upright, &'static str> {
    let lines = classify(segs);
    match mode {
        Mode::Level => {
            let (angle, n) = level(&lines, base).ok_or("Keine deutliche Waagerechte gefunden – nichts geändert")?;
            Ok(Upright { angle, pv: base.pv, ph: base.ph, lines: n })
        }
        Mode::Vertical => {
            let (angle, pv, n) = vertical(&lines, &Frame { ph: 0.0, ..*base }, 0.0).ok_or("Keine deutlichen Senkrechten gefunden – nichts geändert")?;
            Ok(Upright { angle, pv, ph: 0.0, lines: n })
        }
        Mode::Auto => {
            let flat = Frame { pv: 0.0, ph: 0.0, ..*base };
            let (angle, pv, mut n) = match vertical(&lines, &flat, 0.0) {
                Some(v) => v,
                None => {
                    let (a, n) = level(&lines, &flat).ok_or("Keine deutlichen Linien gefunden – nichts geändert")?;
                    (a, 0.0, n)
                }
            };
            // Horizontal only moderately (half correction), as with "Auto" in common photo apps: a facade
            // seen at an angle should not look flat from the front
            let ph = match horizontal(&lines, &Frame { angle, pv, ..flat }) {
                Some((p, nh)) => {
                    n += nh;
                    p * 0.5
                }
                None => 0.0,
            };
            Ok(Upright { angle, pv, ph, lines: n })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic image: bright area with dark stripes whose edges converge
    /// towards the top (building seen from below).
    fn keystone_image(w: usize, h: usize, pv: f32) -> (Linear, Frame) {
        let truth = Frame { pv, ..Frame::full(w, h, 0) };
        let mut data = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                // Point in the upright image → rectified: straight vertical stripes there
                // (4 × 4 supersampled so the edges are not jagged)
                let mut v = 0.0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let p = truth.ori_to_persp([x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0]);
                        let stripe = ((p[0] / (w as f32 / 9.0)).floor() as i32).rem_euclid(2) == 0;
                        let band = ((p[1] / (h as f32 / 5.0)).floor() as i32).rem_euclid(2) == 0;
                        v += if stripe { 0.8 } else { 0.1 } * if band { 1.0 } else { 0.7 } / 16.0;
                    }
                }
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v, v, v]);
            }
        }
        (Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None }, truth)
    }

    #[test]
    fn vertical_finds_converging_lines() {
        let (lin, _) = keystone_image(900, 600, -0.4);
        let base = Frame::full(900, 600, 0);
        let segs = detect(&lin, &base);
        assert!(segs.len() >= 6, "{} Linien", segs.len());
        let r = solve(&segs, &base, Mode::Vertical).expect("solution");
        assert!((r.pv + 0.4).abs() < 0.05, "{r:?}");
        assert!(r.angle.abs() < 0.3, "{r:?}");
    }

    #[test]
    fn noise_yields_nothing() {
        let (w, h) = (600, 400);
        let mut seed = 1u32;
        let data = (0..w * h * 3)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                0.2 + (seed >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        let lin = Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
        let base = Frame::full(w, h, 0);
        let segs = detect(&lin, &base);
        assert!(solve(&segs, &base, Mode::Auto).is_err(), "{} Linien", segs.len());
    }
}
