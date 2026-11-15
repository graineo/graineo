//! Classic chain: the steps whose method is published in dcraw are rebuilt after that description.
//! Source: Dave Coffin, `dcraw.c` revision 1.478 (version 9.28, 2018), functions
//! `foveon_interpolate`, `foveon_avg`, `foveon_make_curve` and `foveon_apply_curve`; dcraw's Foveon
//! functions are distributed under the GNU GPL version 2 or later and are used here under GPL-3.0
//! (see `THIRD_PARTY.md`).
//!
//! Rebuilt after dcraw (method, filters, order and rounding as described there):
//! - sensor correction ([`drift`], [`sensor`]): dark level from the stored drift or the shielded
//!   strips, per-row black level from the side strips (left plus three times right, without the
//!   extremes), median of three, forward/backward exponential smoothing, flare as a share of the
//!   subsampled mean, column filter `(d + d²/2¹⁴) · f`, the post polynomial in 14-bit fixed point,
//!   the spatial gain interpolated across the line, limit 32000;
//! - defective pixels from the camera's list ([`bad_pixel`]);
//! - first-layer sharpening against a 1-4-6-4-1 blur ([`rot`]);
//! - the coring curve: `(cos x + 1)/2 · tanh(i · f / a) · a` ([`Curve`]);
//! - small and large chroma smoothing ([`small`], [`large`]), the colour step with coring of each
//!   layer and of its deviation from the 1:2:1 grey ([`color`]), the large recursive chroma
//!   smoothing at a quarter of the resolution ([`recursive`]).
//!
//! Inputs come from the camera data (drift, strips, filters, polynomial, gain tables, defect
//! list, saturation level, coring amplitudes) and from the calibration (layer gains, matrix,
//! amplitude scaling). Where dcraw has a fixed number that the
//! camera data also carries, the camera's value is used (flare factor, column filter, chroma sum
//! shift). Own forms and measurements against the reference exports: the highlight step
//! ([`highlights_gray`]: clipping level per layer from the image, fade into grey; dcraw's pull towards
//! the largest layer tinted nearly clipped skies), the column filter share of the second layer when
//! one filter serves all layers ([`COLUMN_G`]), the sum correction of the large chroma smoothing
//! ([`large`]) and the strength of the colour coring of cameras without per-layer amplitudes
//! (`kette`). The colour step uses a black level estimate from the image ([`super::color`];
//! without it dark images shift by up to ΔE00 2).

use rayon::prelude::*;

use super::color;
use super::calibration::{Data, Calibration};
use super::correction::{self, Drift, Grid, Line};
use super::meta::Meta;

#[inline]
fn px(img: &[i32], i: usize) -> i32 {
    img[i] as i16 as i32
}

// ---- sensor correction ----

/// Share of the column filter for the second layer when the camera lists one filter for all
/// layers (own measurement on the SD10 reference export, mean ΔE00: 1.0 → 0.45, 0.97 → 0.32,
/// 0.94 → 0.22, 0.9 → 0.35, 0.85 → 0.59).
pub const COLUMN_G: f64 = 0.94;

/// Dark level: the camera's stored drift when it carries one, else the mean of the two shielded
/// strips (no slope).
pub fn drift(img: &[i32], w: usize, h: usize, d: &Data) -> Drift {
    if d.values("DarkDrift").is_some_and(|p| p.len() >= 12) {
        return correction::drift_from_camera(d);
    }
    let keep = correction::rect(d, "KeepImageArea");
    let mean = |name: &str| -> [f64; 3] {
        let r = correction::rect(d, name);
        let (x0, x1) = ((r.links - keep.links).max(0) as usize, ((r.right - keep.links).max(0) as usize).min(w));
        let (y0, y1) = ((r.top - keep.top).max(0) as usize, ((r.bottom - keep.top).max(0) as usize).min(h));
        let mut s = [0.0f64; 3];
        let mut n = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                for c in 0..3 {
                    s[c] += f64::from(px(img, (y * w + x) * 3 + c));
                }
                n += 1.0;
            }
        }
        if n == 0.0 { [0.0; 3] } else { s.map(|v| v / n) }
    };
    Drift {
        top: Line { slope: [0.0; 3], mean: mean("DarkShieldTop") },
        bottom: Line { slope: [0.0; 3], mean: mean("DarkShieldBottom") },
    }
}

/// Drift line for image row `y`.
fn drift_at(dr: &Drift, y: usize, h: usize) -> Line {
    let t = if h > 1 { y as f64 / (h as f64 - 1.0) } else { 0.0 };
    let mut l = Line::default();
    for c in 0..3 {
        l.slope[c] = dr.top.slope[c] + t * (dr.bottom.slope[c] - dr.top.slope[c]);
        l.mean[c] = dr.top.mean[c] + t * (dr.bottom.mean[c] - dr.top.mean[c]);
    }
    l
}

/// Mean of a strip of one row with the column filter, without its smallest and largest value
/// (two values: their mean).
fn stripes_mean(img: &[i32], w: usize, y: usize, c: usize, from_idx: usize, until: usize, cf: f64) -> f64 {
    let p = |x: usize| f64::from(px(img, (y * w + x) * 3 + c));
    let (mut sum, mut min, mut max) = (0.0, f64::MAX, f64::MIN);
    for x in from_idx..=until {
        let v = p(x) + (p(x) - p(x.saturating_sub(1))) * cf;
        sum += v;
        min = min.min(v);
        max = max.max(v);
    }
    if until - from_idx == 1 {
        return sum / 2.0;
    }
    (sum - min - max) / (until - from_idx - 1) as f64
}

/// Per-row black level from the shielded side strips (cameras whose data list a row correction).
fn rows_black(img: &[i32], w: usize, h: usize, d: &Data, dr: &Drift, cf: [f64; 3]) -> Vec<[f64; 3]> {
    let mut black = vec![[0.0f64; 3]; h];
    let Some(sr) = d.values("DarkShieldColRange").filter(|v| v.len() >= 4) else { return black };
    if !d.present("RowCorrectionScale") || h < 5 {
        return black;
    }
    let [l0, l1, r0, r1] = [sr[0] as usize, sr[1] as usize, sr[2] as usize, sr[3] as usize];
    if l0 < 1 || l1 <= l0 || r1 <= r0 || r1 >= w {
        return black;
    }
    // time constant, gain of the deviation, share of the mean added back (as dcraw)
    let k = [24.0f64, 1.0, 0.5];
    for (y, z) in black.iter_mut().enumerate() {
        let l = drift_at(dr, y, h);
        for c in 0..3 {
            let li = stripes_mean(img, w, y, c, l0, l1, cf[c]);
            let re = stripes_mean(img, w, y, c, r0, r1, cf[c]);
            z[c] = (li + re * 3.0 - l.slope[c]) / 4.0 - l.mean[c];
        }
    }
    // rows outside the active area repeat the nearest active rows
    let keep = correction::rect(d, "KeepImageArea");
    let is_active = correction::rect(d, "ActiveImageArea");
    let (o, u) = (((is_active.top - keep.top).max(0) as usize).min(h), ((is_active.bottom - keep.top).max(0) as usize).min(h));
    if o < u {
        for y in 0..o {
            black[y] = black[(o + y).min(u - 1)];
        }
        for y in u..h {
            black[y] = black[y.saturating_sub(h - u).max(o)];
        }
    }
    // median of three along the rows, last row mean of the last two, first row mean of rows 1 and 3
    let orig = black.clone();
    for y in 1..h - 1 {
        for c in 0..3 {
            let (a, b, e) = (orig[y - 1][c], orig[y][c], orig[y + 1][c]);
            black[y][c] = a.max(b).min(a.min(b).max(e));
        }
    }
    for c in 0..3 {
        black[h - 1][c] = (orig[h - 2][c] + orig[h - 1][c]) / 2.0;
        black[0][c] = (black[1][c] + black[3][c]) / 2.0;
    }
    // forward smoothing, mean; backward smoothing of the deviation from the mean
    let a = 1.0 - (-1.0 / k[0]).exp();
    let mut sum = black[0];
    for y in 1..h {
        for c in 0..3 {
            black[y][c] = (black[y][c] - black[y - 1][c]) * a + black[y - 1][c];
            sum[c] += black[y][c];
        }
    }
    let mean = sum.map(|s| s / h as f64);
    let mut state = black[h - 1];
    for y in (0..h).rev() {
        for c in 0..3 {
            state[c] = (black[y][c] - mean[c] - state[c]) * a + state[c];
            black[y][c] = state[c] * k[1] + mean[c] * k[2];
        }
    }
    black
}

/// The sensor correction after the dark level (`dr`): row black level, flare, column filter, post
/// polynomial, spatial gain times layer gain, limit 32000.
pub fn sensor(img: &mut [i32], w: usize, h: usize, d: &Data, cal: &Calibration, dr: &Drift) -> Result<(), String> {
    let mut cf = if d.present("ColumnFilterRGB") { d.triple("ColumnFilterRGB", [0.0; 3]) } else { [f64::from(d.number("ColumnFilter")); 3] };
    // one column filter for all layers (SD9/SD10): the second layer takes a smaller share, own
    // measurement against the SD10 reference export ([`COLUMN_G`])
    if !d.present("ColumnFilterRGB") && cf[1] != 0.0 {
        cf[1] *= COLUMN_G;
    }
    let cq = if d.present("ColumnFilterSquareRGB") { d.triple("ColumnFilterSquareRGB", [0.0; 3]) } else { cf };
    let mut grid = Grid::load(d, &cal.grid).or_else(|| Grid::load(d, "SpatialGain"));
    if let (Some(g), Some(s)) = (grid.as_mut(), Grid::load(d, "Shading")) {
        if g.columns == s.columns && g.rows == s.rows {
            for (a, b) in g.data.iter_mut().zip(&s.data) {
                *a *= *b;
            }
        }
    }
    let mut pp = [0.0f64; 27];
    if let Some(m) = d.values("PostPolyMatrix").filter(|m| m.len() >= 27) {
        for i in 0..27 {
            pp[i] = f64::from(m[i] as f32);
        }
    }
    // flare: the camera's factor (dcraw: one hundredth) of the mean of every fourth pixel from (2, 2)
    let scatter_f = d.triple("FlareFactor", [0.01; 3]);
    let m = correction::subsampling(img, w, h, 4);
    let scatter: [f64; 3] = std::array::from_fn(|c| scatter_f[c] * m[c]);
    let black = rows_black(img, w, h, d, dr, cf);
    let gain = cal.amplification.permanent;
    let columns = grid.as_ref().map_or(2, |g| g.columns.max(2));
    let sgx = ((w + columns - 2) / (columns - 1)).max(1);
    img.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let l = drift_at(dr, y, h);
        let mut node = vec![1.0f32; columns * 3];
        if let Some(g) = &grid {
            correction::grid_row(g, y, h, &mut node);
        }
        let mut pre = [row[0] as i16 as i32, row[1] as i16 as i32, row[2] as i16 as i32];
        for x in 0..w {
            let mut v = [0i64; 3];
            for c in 0..3 {
                let p = row[x * 3 + c] as i16 as i32;
                let diff = p - pre[c];
                pre[c] = p;
                let square = f64::from((diff * diff) >> 14);
                let corr = f64::from(diff) * cf[c] + square * cq[c] - l.mean[c] - l.slope[c] * (x as f64 / w as f64 - 0.5) - black[y][c] - scatter[c];
                v[c] = i64::from(p) + corr.floor() as i64;
            }
            // post polynomial: squares, cross products (rg, rb, gb), cubes
            let q: [i64; 3] = std::array::from_fn(|c| (v[c] * v[c]) >> 14);
            let k3: [i64; 3] = std::array::from_fn(|c| (v[c] * q[c]) >> 14);
            let cross = [(v[0] * v[1]) >> 14, (v[2] * v[0]) >> 14, (v[1] * v[2]) >> 14];
            let i = x / sgx;
            let (i0, i1) = (i.min(columns - 1), (i + 1).min(columns - 1));
            let rest = (x % sgx) as f64;
            for c in 0..3 {
                let k = c * 9;
                let mut val = 0.0f64;
                for j in 0..3 {
                    val += pp[k + j] * q[j] as f64 + pp[k + 3 + j] * cross[j] as f64 + pp[k + 6 + j] * k3[j] as f64;
                }
                let g = (f64::from(node[i0 * 3 + c]) * (sgx as f64 - rest) + f64::from(node[i1 * 3 + c]) * rest) / sgx as f64;
                let mut o = ((v[c] as f64 + val.floor()) * g * gain[c]).floor() as i64;
                if o > 32000 {
                    o = 32000;
                }
                row[x * 3 + c] = o as i16 as i32;
            }
        }
    });
    Ok(())
}

// ---- defective pixels ----

/// Defective pixels: each listed pixel becomes the mean of the neighbours its entry names. When the
/// camera data carry a badness per entry and the threshold levels, only entries above the first
/// level are used.
pub fn bad_pixel(img: &mut [i32], w: usize, h: usize, m: &Meta) {
    const NEIGHBORS: [(i32, i32); 8] = [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)];
    let Some(list) = m.values("BadPixels") else { return };
    let bad = m.values("BPBadness").filter(|b| b.len() == list.len());
    let threshold = m.values("BadPixThreshLevels").and_then(|l| l.first().copied());
    let keep = m.fixed::<4>("KeepImageArea").unwrap_or([0.0; 4]);
    for (n, e) in list.iter().enumerate() {
        if let (Some(b), Some(s)) = (&bad, threshold) {
            if b[n] <= s {
                continue;
            }
        }
        let e = *e as u32;
        let x = ((e >> 8) & 0xfff) as i32 - keep[0] as i32;
        let y = (e >> 20) as i32 - keep[1] as i32;
        if (y - 1) as u32 > (h as u32).wrapping_sub(3) || (x - 1) as u32 > (w as u32).wrapping_sub(3) {
            continue;
        }
        let mut sum = [0.0f32; 3];
        let mut k = 0;
        for (bit, (dy, dx)) in NEIGHBORS.iter().enumerate() {
            if e & (1 << bit) != 0 {
                let i = (((y + dy) as usize) * w + (x + dx) as usize) * 3;
                for c in 0..3 {
                    sum[c] += px(img, i + c) as f32;
                }
                k += 1;
            }
        }
        if k > 0 {
            let i = (y as usize * w + x as usize) * 3;
            for c in 0..3 {
                img[i + c] = (sum[c] / k as f32) as i32 as i16 as i32;
            }
        }
    }
}

// ---- first-layer sharpening ----

/// The first layer is sharpened against its 5 × 5 binomial blur (1-4-6-4-1 along and across the
/// rows); the reference value is seven eighths of the pixel's blur plus one eighth of its left
/// neighbour's, the layer moves away from it by an eighth of the difference. Rows and columns two
/// from the border stay.
pub fn rot(img: &mut [i32], w: usize, h: usize) {
    if w < 5 || h < 5 {
        return;
    }
    let p = |img: &[i32], y: usize, x: usize| px(img, (y * w + x) * 3);
    let mut horiz = vec![0i32; w * h];
    for y in 0..h {
        for x in 2..w - 2 {
            horiz[y * w + x] = (p(img, y, x) * 6 + (p(img, y, x - 1) + p(img, y, x + 1)) * 4 + p(img, y, x - 2) + p(img, y, x + 2) + 8) >> 4;
        }
    }
    img.par_chunks_mut(w * 3).enumerate().filter(|(y, _)| *y >= 2 && *y < h - 2).for_each(|(y, row)| {
        let s = |r: usize, x: usize| horiz[r * w + x];
        let mut before = 0;
        for x in 2..w - 2 {
            let smooth = (6 * s(y, x) + 4 * (s(y - 1, x) + s(y + 1, x)) + s(y - 2, x) + s(y + 2, x) + 8) >> 4;
            if x == 2 {
                before = smooth;
            }
            let a = row[x * 3] as i16 as i32;
            let mut v = a + ((a - ((smooth * 7 + before) >> 3)) >> 3);
            if v > 32000 {
                v = 32000;
            }
            row[x * 3] = v as i16 as i32;
            before = smooth;
        }
    });
}

// ---- highlights ----

/// Parameters of the own highlight fade ([`highlights_gray`]).
#[derive(Clone, Copy, Debug)]
pub struct LightGray {
    /// start of the fade, as a fraction of the clipping level of the layer
    pub start: f64,
    /// target: mean of the three layers plus this share of (largest layer − mean)
    pub to_max: f64,
    /// the weight spreads to neighbours within this distance (pixels)
    pub radius: usize,
}

/// Own measurement against the reference exports of six files with nearly clipped skies and bright
/// coloured objects (mean ΔE00 in the bright parts, L* > 85: dcraw's form 1.24, start 0.95 → 0.62,
/// 0.85 → 0.58, 0.6 → 0.51, 0.4 → 0.55; the share towards the largest layer hardly matters;
/// spreading radius 0 → 0.47, 1 → 0.46, 2 → 0.66, 3 → 0.79 with the final clipping detection).
pub const LIGHT_GRAY: LightGray = LightGray { start: 0.6, to_max: 0.5, radius: 1 };

/// Clipping level of each layer, estimated from the image: a layer counts as clipped when more than
/// 0.2 % of the pixels lie within 5 % of its largest value and these are at least 15 % of the
/// pixels within 20 % of it (a pile-up at the top, not just a bright smooth area); its level is then
/// the median of the pixels within 10 % of that value. `None` for a layer without such a pile-up.
pub fn clip_limits(img: &[i32]) -> [Option<f64>; 3] {
    let n = img.len() / 3;
    std::array::from_fn(|c| {
        let max = img.iter().skip(c).step_by(3).map(|v| *v as i16 as i32).max().unwrap_or(0);
        if max <= 0 || n == 0 {
            return None;
        }
        let near = img.iter().skip(c).step_by(3).filter(|v| f64::from(**v as i16) > 0.95 * f64::from(max)).count();
        let band20 = img.iter().skip(c).step_by(3).filter(|v| f64::from(**v as i16) > 0.8 * f64::from(max)).count();
        if (near as f64) < 0.002 * n as f64 || (near as f64) < 0.15 * band20 as f64 {
            return None;
        }
        let mut band: Vec<i32> = img.iter().skip(c).step_by(3).map(|v| *v as i16 as i32).filter(|v| f64::from(*v) > 0.9 * f64::from(max)).collect();
        let center = band.len() / 2;
        let (_, m, _) = band.select_nth_unstable(center);
        Some(f64::from(*m))
    })
}

/// Own highlight step (replaces dcraw's pull towards the largest layer, which tints
/// nearly clipped skies): where a layer approaches its clipping level ([`clip_limits`]), the pixel
/// fades into a neutral grey (equal layers). Weight: smoothstep of the largest ratio
/// `layer / clipping level` of the clipped layers between `start` and 1; the grey is the mean of
/// the layers plus a share of the distance to the largest layer. The weight spreads to the
/// neighbours within `radius` (largest weight), so the edge pixels next to clipped areas do not keep
/// a coloured fringe. Layers that never clip (bright
/// coloured objects) leave the pixel alone. Parameters measured against the reference exports of
/// nearly clipped skies ([`LIGHT_GRAY`]).
pub fn highlights_gray(img: &mut [i32], w: usize, h: usize, p: LightGray) {
    let limits = clip_limits(img);
    if limits.iter().all(|g| g.is_none()) || p.start >= 1.0 || w == 0 {
        return;
    }
    // weight per pixel
    let mut t: Vec<f32> = img
        .par_chunks(3)
        .map(|q| {
            let r = (0..3).filter_map(|c| limits[c].map(|g| f64::from(q[c] as i16) / g)).fold(0.0f64, f64::max);
            if r <= p.start {
                return 0.0;
            }
            let x = ((r - p.start) / (1.0 - p.start)).min(1.0);
            (x * x * (3.0 - 2.0 * x)) as f32
        })
        .collect();
    // the weight spreads to the neighbours (largest weight within `radius`, rows then columns), so
    // that edge pixels next to clipped areas lose their colour error as well
    let r = p.radius;
    if r > 0 {
        let mut z = vec![0.0f32; t.len()];
        z.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for x in 0..w {
                let (x0, x1) = (x.saturating_sub(r), (x + r).min(w - 1));
                row[x] = t[y * w + x0..=y * w + x1].iter().copied().fold(0.0, f32::max);
            }
        });
        t.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let (y0, y1) = (y.saturating_sub(r), (y + r).min(h - 1));
            for x in 0..w {
                row[x] = (y0..=y1).map(|yy| z[yy * w + x]).fold(0.0, f32::max);
            }
        });
    }
    img.par_chunks_mut(3).zip(t.par_iter()).for_each(|(q, &t)| {
        if t <= 0.0 {
            return;
        }
        let t = f64::from(t);
        let v = [f64::from(q[0] as i16), f64::from(q[1] as i16), f64::from(q[2] as i16)];
        let max = v[0].max(v[1]).max(v[2]);
        let mean = (v[0] + v[1] + v[2]) / 3.0;
        let gray = mean + p.to_max * (max - mean);
        for c in 0..3 {
            q[c] = ((v[c] + t * (gray - v[c])).round() as i32).min(32000) as i16 as i32;
        }
    });
}

// ---- coring curve ----

/// Coring curve: `(cos(i · f / m / 4) + 1)/2 · tanh(i · f / a) · a`, rounded, for `|i|` below
/// `4π · m / f` (zero beyond); `a` is the amplitude, `m` the largest amplitude of a set of curves,
/// `f` = 0.8. Odd in `i`.
pub struct Curve {
    t: Vec<i32>,
}

impl Curve {
    pub fn new(max: f64, amp: f64) -> Curve {
        const F: f64 = 0.8;
        if !(max > 0.0 && amp > 0.0) {
            return Curve { t: Vec::new() };
        }
        let n = (4.0 * std::f64::consts::PI * max / F) as usize;
        let t = (0..n).map(|i| {
            let x = i as f64 * F / max / 4.0;
            (((x.cos() + 1.0) / 2.0 * (i as f64 * F / amp).tanh() * amp + 0.5) as i32) as i16 as i32
        });
        Curve { t: t.collect() }
    }
    #[inline]
    pub fn an(&self, i: i32) -> i32 {
        let a = i.unsigned_abs() as usize;
        if a >= self.t.len() {
            0
        } else if i < 0 {
            -self.t[a]
        } else {
            self.t[a]
        }
    }
}

/// Curves of three layers with a common fade-out (the largest amplitude).
pub fn curves(amp: [f64; 3]) -> [Curve; 3] {
    let max = amp.iter().copied().fold(0.0f64, f64::max);
    std::array::from_fn(|c| Curve::new(max, amp[c]))
}

// ---- chroma smoothing ----

fn as_i16(img: &[i32]) -> Vec<i32> {
    img.iter().map(|v| *v as i16 as i32).collect()
}

/// Small chroma smoothing: each layer is compared with its 1-2-1 blur (along, then across the rows);
/// the cored differences move the layers, minus their sum shifted right by `shift` (only the colour
/// part moves). Two rows and columns at the border stay.
pub fn small(img: &mut [i32], w: usize, h: usize, k: &Curve, shift: u32) {
    if w < 5 || h < 5 {
        return;
    }
    let orig = as_i16(img);
    let n = w * 3;
    let mut horiz = vec![0i32; w * h * 3];
    horiz.par_chunks_mut(n).enumerate().for_each(|(y, z)| {
        for x in 2..w - 2 {
            for c in 0..3 {
                let i = y * n + x * 3 + c;
                z[x * 3 + c] = (orig[i - 3] + 2 * orig[i] + orig[i + 3] + 2) >> 2;
            }
        }
    });
    img.par_chunks_mut(n).enumerate().filter(|(y, _)| *y >= 2 && *y < h - 2).for_each(|(y, row)| {
        for x in 2..w - 2 {
            let mut dev = [0i32; 3];
            for c in 0..3 {
                let i = x * 3 + c;
                let smooth = (horiz[(y - 1) * n + i] + 2 * horiz[y * n + i] + horiz[(y + 1) * n + i]) >> 2;
                dev[c] = -k.an(orig[y * n + i] - smooth);
            }
            let s = (dev[0] + dev[1] + dev[2]) >> shift;
            for c in 0..3 {
                row[x * 3 + c] = (orig[y * n + x * 3 + c] + dev[c] - s) as i16 as i32;
            }
        }
    });
}

/// Large chroma smoothing: the 5 × 5 sums of the layers, scaled to the brightness of the pixel
/// (`spatial` is the offset of that scaling, dcraw: 60), are the targets; the cored difference to
/// the target moves each layer. Unlike dcraw's version of this step, the summed correction shifted
/// right by `shift` is taken off again, as dcraw does in its small and recursive smoothing ("smooth
/// the hues without smoothing the total"); measured against the reference exports this keeps the
/// brightness of dark, noisy images (ΔE00 0.67 → 0.06 on an ISO 200 file). Two rows and columns at
/// the border stay.
pub fn large(img: &mut [i32], w: usize, h: usize, k: &Curve, spatial: i32, shift: u32) {
    if w < 5 || h < 5 {
        return;
    }
    let orig = as_i16(img);
    let n = w * 3;
    let off = i64::from((spatial * 25 / 4).max(1));
    let mut horiz = vec![0i32; w * h * 3];
    horiz.par_chunks_mut(n).enumerate().for_each(|(y, z)| {
        for x in 2..w - 2 {
            for c in 0..3 {
                let i = y * n + x * 3 + c;
                z[x * 3 + c] = (orig[i - 6] + orig[i - 3] + orig[i] + orig[i + 3] + orig[i + 6] + 2) >> 2;
            }
        }
    });
    img.par_chunks_mut(n).enumerate().filter(|(y, _)| *y >= 2 && *y < h - 2).for_each(|(y, row)| {
        for x in 2..w - 2 {
            let mut total = [0i64; 3];
            let mut all_items = off;
            let mut sum = i64::from(spatial);
            for c in 0..3 {
                let i = x * 3 + c;
                total[c] = (y - 2..=y + 2).map(|r| i64::from(horiz[r * n + i])).sum();
                all_items += total[c];
                sum += i64::from(orig[y * n + i]);
            }
            let sum = sum.max(0);
            let j = if all_items > off { (sum << 16) / all_items } else { sum * (65536 / off) };
            let mut corr = [0i32; 3];
            for c in 0..3 {
                let p = orig[y * n + x * 3 + c];
                corr[c] = k.an(((j * total[c] + 0x8000) >> 16) as i32 - p);
            }
            let s = (corr[0] + corr[1] + corr[2]) >> shift;
            for c in 0..3 {
                row[x * 3 + c] = (orig[y * n + x * 3 + c] + corr[c] - s) as i16 as i32;
            }
        }
    });
}

// ---- colour ----

/// Colour step: the black level offset estimated from the image is taken off first; each layer is
/// cored against zero, then against the 1:2:1 grey of the cored layers; the result goes through
/// the matrix (float, rounded) and is limited to `0 … 24000`. Returns the codes and the cored
/// layers.
pub fn color(img: &[i32], w: usize, h: usize, p: &color::Parameter) -> color::ColorResult {
    let k = curves(p.amp);
    let m = p.matrix;
    // black level offset estimated from the image
    let mut bo = color::black_level(img, w, h, p.amp);
    let gray = (bo[0] + 2 * bo[1] + bo[2]) >> 2;
    for v in &mut bo {
        *v -= gray;
    }
    let mut codes = vec![0i32; w * h * 3];
    let mut layers = vec![0i32; w * h * 3];
    codes.par_chunks_mut(3).zip(layers.par_chunks_mut(3)).zip(img.par_chunks(3)).for_each(|((code, layer), q)| {
        let v = [q[0] as i16 as i32 - bo[0], q[1] as i16 as i32 - bo[1], q[2] as i16 as i32 - bo[2]];
        let a: [i32; 3] = std::array::from_fn(|c| v[c] - k[c].an(v[c]));
        let gray = (a[0] + a[1] + a[1] + a[2]) >> 2;
        let b: [i32; 3] = std::array::from_fn(|c| a[c] - k[c].an(a[c] - gray));
        for c in 0..3 {
            let s = (m[c * 3] * f64::from(b[0]) + m[c * 3 + 1] * f64::from(b[1]) + m[c * 3 + 2] * f64::from(b[2])).clamp(0.0, 24000.0);
            code[c] = (s + 0.5).floor() as i32;
            layer[c] = b[c] as i16 as i32;
        }
    });
    color::ColorResult { codes, layers, green_sum: p.green_sum }
}

// ---- large recursive chroma smoothing ----

/// Large recursive chroma smoothing on the colour codes: 4 × 4 block means are smoothed from the
/// bottom up (weights 1840 : 141 of 4096 per block sum), every fourth row the block row is smoothed
/// right to left and left to right over the pixels (6707 : 1485 of 8192), between the rows the
/// smoothed line follows downwards with the same weights. The smoothed layers, scaled to the
/// brightness of the pixel (`reg` is the offset, dcraw: 30), are the targets of the coring per layer;
/// the summed correction shifted right by `shift` is taken off again. Codes stay ≥ 0.
pub fn recursive(img: &mut [i32], w: usize, h: usize, k: &[Curve; 3], reg: i32, shift: u32) {
    let (w4, h4) = (w / 4, h / 4);
    if w4 < 1 || h4 < 1 {
        return;
    }
    let mut small = vec![[0i32; 3]; w4 * h4];
    for r in (0..h4).rev() {
        for s in 0..w4 {
            let mut v = [0i32; 3];
            for i in 0..4 {
                for j in 0..4 {
                    for c in 0..3 {
                        v[c] += img[((r * 4 + i) * w + s * 4 + j) * 3 + c];
                    }
                }
            }
            for c in 0..3 {
                small[r * w4 + s][c] = if r + 2 > h4 { v[c] >> 4 } else { (small[(r + 1) * w4 + s][c] * 1840 + v[c] * 141 + 2048) >> 12 };
            }
        }
    }
    let b = w & !3;
    let mut z0 = vec![[0i32; 3]; w];
    let mut z1 = vec![[0i32; 3]; w];
    let mut z2 = vec![[0i32; 3]; w];
    for y in 0..(h & !3) {
        if y & 3 == 0 {
            let mut t = [0i32; 3];
            for x in (0..b).rev() {
                for c in 0..3 {
                    t[c] = (small[(y / 4) * w4 + x / 4][c] * 1485 + t[c] * 6707 + 4096) >> 13;
                    z0[x][c] = t[c];
                }
            }
        }
        let mut t = [0i32; 3];
        for x in 0..b {
            for c in 0..3 {
                t[c] = (z0[x][c] * 1485 + t[c] * 6707 + 4096) >> 13;
                z1[x][c] = t[c];
            }
        }
        if y == 0 {
            z2.copy_from_slice(&z1);
        } else {
            for x in 0..b {
                for c in 0..3 {
                    z2[x][c] = (z2[x][c] * 6707 + z1[x][c] * 1485 + 4096) >> 13;
                }
            }
        }
        for x in 0..b {
            let i = (y * w + x) * 3;
            let denominator = i64::from(reg) + z2[x].iter().map(|v| i64::from(*v)).sum::<i64>();
            let counter = i64::from(reg) + (0..3).map(|c| i64::from(img[i + c])).sum::<i64>();
            let j = if denominator != 0 { (counter << 16) / denominator } else { 0 };
            let mut corr = [0i32; 3];
            for c in 0..3 {
                corr[c] = k[c].an((((i64::from(z2[x][c]) * j + 0x8000) >> 16) as i32) - img[i + c]);
            }
            let s = (corr[0] + corr[1] + corr[2]) >> shift;
            for c in 0..3 {
                img[i + c] = (img[i + c] + corr[c] - s).max(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_is_odd_bounded_and_fades_out() {
        let k = Curve::new(40.0, 20.0);
        assert_eq!(k.an(0), 0);
        for i in 1..400 {
            assert_eq!(k.an(-i), -k.an(i));
            assert!(k.an(i) <= 20);
        }
        // small differences pass nearly unchanged, large ones are not touched
        assert_eq!(k.an(2), 2);
        assert_eq!(k.an(10_000), 0);
    }

    #[test]
    fn curve_matches_the_standard_table_inside_its_range() {
        // same amplitude and slope: the standard table and the curve agree below the fade-out end
        let t = super::super::tanh_table::build([30.0, 20.0, 40.0], [0.8; 3]);
        let k = curves([30.0, 20.0, 40.0]);
        for c in 0..3 {
            for i in -400..400 {
                assert!((t.get(c, i) - k[c].an(i)).abs() <= 1, "{c} {i}");
            }
        }
    }

    #[test]
    fn flat_images_stay_flat() {
        let (w, h) = (24, 20);
        let img: Vec<i32> = (0..w * h).flat_map(|_| [500, 700, 900]).collect();
        let k = Curve::new(30.0, 30.0);
        let mut a = img.clone();
        small(&mut a, w, h, &k, 3);
        assert_eq!(a, img);
        let mut r = img.clone();
        rot(&mut r, w, h);
        assert_eq!(r, img);
    }
}
