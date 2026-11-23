//! Global alignment of one frame onto the reference: coarse integer search on a ≈ 160 px pyramid level, then
//! coarse-to-fine Gauss-Newton on log-luma proxies (Lucas-Kanade family, Baker & Matthews 2004) for a similarity
//! model (translation, scale, rotation). Both proxies are clamped to the common valid range, so frames of different
//! exposure compare directly and clipped or noisy areas are flat and do not contribute.

use crate::img::Img;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Projective warp in normalised coordinates u = (x − cx)/s, s = max(w, h)/2, so the same parameters hold on every
/// pyramid level and at full resolution. Maps reference coordinates to frame coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Warp {
    pub p: [f64; 8],
}

impl Warp {
    #[inline]
    pub fn apply(&self, x: f64, y: f64, cx: f64, cy: f64, s: f64) -> (f64, f64) {
        let p = &self.p;
        let (u, v) = ((x - cx) / s, (y - cy) / s);
        let xx = (1.0 + p[0]) * u + p[1] * v + p[2];
        let yy = p[3] * u + (1.0 + p[4]) * v + p[5];
        let z = p[6] * u + p[7] * v + 1.0;
        (cx + s * xx / z, cy + s * yy / z)
    }

    /// d(x', y')/dp
    fn jac(&self, x: f64, y: f64, cx: f64, cy: f64, s: f64) -> ([f64; 8], [f64; 8]) {
        let p = &self.p;
        let (u, v) = ((x - cx) / s, (y - cy) / s);
        let xx = (1.0 + p[0]) * u + p[1] * v + p[2];
        let yy = p[3] * u + (1.0 + p[4]) * v + p[5];
        let z = p[6] * u + p[7] * v + 1.0;
        let iz = s / z;
        let jx = [u * iz, v * iz, iz, 0.0, 0.0, 0.0, -xx * u * iz / z, -xx * v * iz / z];
        let jy = [0.0, 0.0, 0.0, u * iz, v * iz, iz, -yy * u * iz / z, -yy * v * iz / z];
        (jx, jy)
    }

    pub fn is_identity(&self) -> bool {
        self.p.iter().all(|v| *v == 0.0)
    }

    /// Translation (pixels), rotation (degrees) and scale of a similarity warp for an image of size `w × h`.
    pub fn describe(&self, w: usize, h: usize) -> (f64, f64, f64, f64) {
        let s = w.max(h) as f64 / 2.0;
        let a = 1.0 + self.p[0];
        let b = self.p[3];
        (self.p[2] * s, self.p[5] * s, b.atan2(a).to_degrees(), (a * a + b * b).sqrt())
    }

    /// Largest displacement of the four image corners (pixels) – the size of the move this warp corrects.
    pub fn corner_shift(&self, w: usize, h: usize) -> f64 {
        let (cx, cy, s) = (w as f64 / 2.0, h as f64 / 2.0, w.max(h) as f64 / 2.0);
        [(0.0, 0.0), (w as f64, 0.0), (0.0, h as f64), (w as f64, h as f64)]
            .iter()
            .map(|&(x, y)| {
                let (a, b) = self.apply(x, y, cx, cy, s);
                ((a - x).powi(2) + (b - y).powi(2)).sqrt()
            })
            .fold(0.0, f64::max)
    }
}

/// Largest scale change, rotation (degrees) and corner shift (fraction of the long edge) a measured alignment may
/// have: frames of one bracket, from a tripod or hand-held. Beyond that the measurement went astray.
pub const MAX_SCALE: f64 = 1.25;
pub const MAX_ROTATION: f64 = 15.0;
pub const MAX_SHIFT: f64 = 0.35;

impl Warp {
    /// Can this be the alignment of two frames of one bracket (finite, within [`MAX_SCALE`], [`MAX_ROTATION`] and
    /// [`MAX_SHIFT`])?
    pub fn plausible(&self, w: usize, h: usize) -> bool {
        if !self.p.iter().all(|v| v.is_finite()) {
            return false;
        }
        let (_, _, rotation, scale) = self.describe(w, h);
        scale <= MAX_SCALE && scale >= 1.0 / MAX_SCALE && rotation.abs() <= MAX_ROTATION && self.corner_shift(w, h) <= MAX_SHIFT * w.max(h) as f64
    }
}

/// How well the frame proxy `m` under `warp` agrees with the reference proxy `r` (both log-clamped with the bounds
/// `lo`/`hi`, as for [`align`]): the correlation of the two on a coarse level (≈ 160 px), over the places both show
/// inside the common valid range. 1 = the same picture, about 0 = unrelated pictures. `None` when too little of
/// the two overlaps to tell (less than 5 % of the level) or one of them is flat there.
pub fn agreement(r: &Img, m: &Img, warp: &Warp, lo: f32, hi: f32) -> Option<f64> {
    let (llo, lhi) = (lo.log2() + 0.05, hi.log2() - 0.05);
    let (mut r, mut m) = (r.clone(), m.clone());
    while r.w.min(r.h) > 160 {
        r = r.half();
        m = m.half();
    }
    if r.w < 3 || r.h < 3 || (m.w, m.h) != (r.w, r.h) || !warp.p.iter().all(|v| v.is_finite()) {
        return None;
    }
    let (cx, cy, s) = (r.w as f64 / 2.0, r.h as f64 / 2.0, r.w.max(r.h) as f64 / 2.0);
    let (mut n, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for y in 0..r.h {
        for x in 0..r.w {
            let a = r.at(x, y);
            if a <= llo || a >= lhi {
                continue;
            }
            let (xw, yw) = warp.apply(x as f64, y as f64, cx, cy, s);
            let Some(b) = m.sample(xw, yw) else { continue };
            if b <= llo || b >= lhi {
                continue;
            }
            let (a, b) = (a as f64, b as f64);
            n += 1.0;
            sa += a;
            sb += b;
            saa += a * a;
            sbb += b * b;
            sab += a * b;
        }
    }
    if n < 64.0 || n < 0.05 * (r.w * r.h) as f64 {
        return None;
    }
    let (va, vb) = (saa / n - (sa / n).powi(2), sbb / n - (sb / n).powi(2));
    if va <= 1e-9 || vb <= 1e-9 {
        return None;
    }
    Some(((sab / n - sa / n * sb / n) / (va * vb).sqrt()).clamp(-1.0, 1.0))
}

/// Motion model of the alignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Translation,
    Similarity,
}

impl Model {
    fn n(self) -> usize {
        match self {
            Model::Translation => 2,
            Model::Similarity => 4,
        }
    }

    /// dp/dq as (p index, q index, factor)
    fn map(self) -> &'static [(usize, usize, f64)] {
        match self {
            Model::Translation => &[(2, 0, 1.0), (5, 1, 1.0)],
            Model::Similarity => &[(2, 0, 1.0), (5, 1, 1.0), (0, 2, 1.0), (4, 2, 1.0), (1, 3, -1.0), (3, 3, 1.0)],
        }
    }
}

/// Gauss-Jordan solve of the augmented system `a` (n × (n+1)).
fn solve(mut a: Vec<Vec<f64>>) -> Option<Vec<f64>> {
    let n = a.len();
    for c in 0..n {
        let piv = (c..n).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[piv][c].abs() < 1e-14 {
            return None;
        }
        a.swap(c, piv);
        for r in 0..n {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in c..=n {
                    a[r][k] -= f * a[c][k];
                }
            }
        }
    }
    Some((0..n).map(|i| a[i][n] / a[i][i]).collect())
}

/// One pyramid level of Gauss-Newton with Huber weights; returns the RMS residual (log2 units).
#[allow(clippy::too_many_arguments)]
fn gn_level(ri: &Img, mi: &Img, wp: &mut Warp, model: Model, llo: f32, lhi: f32, step: usize, iters: usize) -> f64 {
    let (cx, cy, s) = (ri.w as f64 / 2.0, ri.h as f64 / 2.0, ri.w.max(ri.h) as f64 / 2.0);
    let n = model.n();
    let map = model.map();
    let mut rms = 0.0;
    if ri.w < 3 || ri.h < 3 {
        return rms;
    }
    for _ in 0..iters {
        let w0 = *wp;
        let rows: Vec<(Vec<f64>, f64, f64)> = (1..ri.h - 1)
            .into_par_iter()
            .step_by(step)
            .map(|y| {
                let mut a = vec![0f64; n * (n + 1)];
                let (mut e2, mut cnt) = (0.0, 0.0);
                let mut jq = vec![0f64; n];
                for x in (1..ri.w - 1).step_by(step) {
                    let rv = ri.at(x, y);
                    if rv <= llo || rv >= lhi {
                        continue;
                    }
                    let (xw, yw) = w0.apply(x as f64, y as f64, cx, cy, s);
                    let (Some(mv), Some(l), Some(rr), Some(u), Some(d)) =
                        (mi.sample(xw, yw), mi.sample(xw - 1.0, yw), mi.sample(xw + 1.0, yw), mi.sample(xw, yw - 1.0), mi.sample(xw, yw + 1.0))
                    else {
                        continue;
                    };
                    if mv <= llo || mv >= lhi {
                        continue;
                    }
                    let gx = 0.5 * (rr - l) as f64;
                    let gy = 0.5 * (d - u) as f64;
                    let res = (mv - rv) as f64;
                    let wt = if res.abs() < 0.15 { 1.0 } else { 0.15 / res.abs() };
                    let (jx, jy) = w0.jac(x as f64, y as f64, cx, cy, s);
                    jq.iter_mut().for_each(|v| *v = 0.0);
                    for &(pi, qi, f) in map {
                        jq[qi] += f * (gx * jx[pi] + gy * jy[pi]);
                    }
                    for p in 0..n {
                        for q in 0..n {
                            a[p * (n + 1) + q] += wt * jq[p] * jq[q];
                        }
                        a[p * (n + 1) + n] -= wt * jq[p] * res;
                    }
                    e2 += res * res;
                    cnt += 1.0;
                }
                (a, e2, cnt)
            })
            .collect();
        let mut a = vec![vec![0f64; n + 1]; n];
        let (mut e2, mut cnt) = (0.0, 0.0);
        for (ra, re, rn) in rows {
            for (p, row) in a.iter_mut().enumerate() {
                for (q, v) in row.iter_mut().enumerate() {
                    *v += ra[p * (n + 1) + q];
                }
            }
            e2 += re;
            cnt += rn;
        }
        rms = (e2 / cnt.max(1.0)).sqrt();
        let Some(d) = solve(a) else { break };
        for &(pi, qi, f) in map {
            wp.p[pi] += f * d[qi];
        }
        let mag: f64 = d.iter().map(|v| v.abs()).sum::<f64>() * s;
        if mag < 1e-3 {
            break;
        }
    }
    rms
}

/// Result of [`align`]: the warp and the RMS residual of the finest level (log2 units).
pub struct Aligned {
    pub warp: Warp,
    pub rms: f64,
}

/// Coarse-to-fine alignment of `m` onto `r` (both log-clamped proxies with the bounds `lo`/`hi` in linear units).
pub fn align(r: &Img, m: &Img, lo: f32, hi: f32, model: Model) -> Aligned {
    let (llo, lhi) = (lo.log2() + 0.05, hi.log2() - 0.05);
    let mut pr = vec![r.clone()];
    let mut pm = vec![m.clone()];
    while pr.last().map_or(0, |l| l.w.min(l.h)) > 160 {
        let a = pr.last().unwrap().half();
        let b = pm.last().unwrap().half();
        pr.push(a);
        pm.push(b);
    }
    let top = pr.len() - 1;
    let (rt, mt) = (&pr[top], &pm[top]);
    // integer search ±10 px on the top level (≈ ±640 px at full resolution of a 24 MP frame)
    let rad = 10i64.min(rt.w.min(rt.h) as i64 / 4);
    let best = (-rad..=rad)
        .into_par_iter()
        .flat_map_iter(|dy| (-rad..=rad).map(move |dx| (dx, dy)))
        .map(|(dx, dy)| {
            let (mut s, mut n) = (0f64, 0f64);
            for y in (rad as usize)..rt.h - rad as usize {
                for x in (rad as usize)..rt.w - rad as usize {
                    let a = rt.at(x, y);
                    let b = mt.at((x as i64 + dx) as usize, (y as i64 + dy) as usize);
                    s += (a - b).abs() as f64;
                    n += 1.0;
                }
            }
            (s / f64::max(n, 1.0), dx, dy)
        })
        .reduce(|| (f64::MAX, 0, 0), |a, b| if b.0 < a.0 || (b.0 == a.0 && (b.1.abs() + b.2.abs()) < (a.1.abs() + a.2.abs())) { b } else { a });
    let st = rt.w.max(rt.h) as f64 / 2.0;
    let mut warp = Warp::default();
    warp.p[2] = best.1 as f64 / st;
    warp.p[5] = best.2 as f64 / st;
    let mut rms = 0.0;
    for lv in (0..=top).rev() {
        let step = if lv == 0 { 2 } else { 1 };
        rms = gn_level(&pr[lv], &pm[lv], &mut warp, model, llo, lhi, step, 12);
    }
    Aligned { warp, rms }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smooth test scene with structure at several scales.
    fn scene(x: f64, y: f64) -> f32 {
        let v: f64 = 0.3 + 0.12 * (x * 0.031).sin() * (y * 0.023).cos() + 0.08 * ((x + 2.0 * y) * 0.071).sin() + 0.05 * ((x * 0.13).cos() * (y * 0.11).sin());
        v.max(0.01) as f32
    }

    fn render(w: usize, h: usize, warp: &Warp, gain: f32) -> Img {
        let (cx, cy, s) = (w as f64 / 2.0, h as f64 / 2.0, w.max(h) as f64 / 2.0);
        // `warp` maps reference → frame, so the frame shows the scene at the inverse position: render the frame as
        // scene(warp⁻¹(x)); for small similarity warps the inverse is found by a fixed-point iteration
        let p = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                let (mut u, mut v) = (x, y);
                for _ in 0..20 {
                    let (a, b) = warp.apply(u, v, cx, cy, s);
                    u += x - a;
                    v += y - b;
                }
                scene(u, v) * gain
            })
            .collect();
        Img { w, h, p }
    }

    #[test]
    fn recovers_known_similarity() {
        let (w, h) = (640, 420);
        let s = w.max(h) as f64 / 2.0;
        let a = 0.3f64.to_radians();
        let sc = 1.0005;
        let truth = Warp { p: [sc * a.cos() - 1.0, -sc * a.sin(), 7.3 / s, sc * a.sin(), sc * a.cos() - 1.0, -4.6 / s, 0.0, 0.0] };
        let r = render(w, h, &Warp::default(), 1.0);
        let m = render(w, h, &truth, 0.25);
        let (lo, hi) = (0.004, 0.9);
        let rr = r.log_clamped(1.0, lo, hi);
        let mm = m.log_clamped(0.25, lo, hi);
        let got = align(&rr, &mm, lo, hi, Model::Similarity);
        // compare the mapping of the corners
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let mut err: f64 = 0.0;
        for (x, y) in [(0.0, 0.0), (w as f64, 0.0), (0.0, h as f64), (w as f64, h as f64)] {
            let (a, b) = got.warp.apply(x, y, cx, cy, s);
            let (c, d) = truth.apply(x, y, cx, cy, s);
            err = err.max(((a - c).powi(2) + (b - d).powi(2)).sqrt());
        }
        assert!(err < 0.1, "corner error {err:.3} px");
    }
}
