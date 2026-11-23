//! Exposure-bracket merge of a Sigma SFD shot (sd Quattro H, `.X3I`: seven exposures on a tripod) in the way that
//! matches the camera maker's own output: the frames are laid over each other in file coordinates (no alignment, no
//! motion test) and averaged with weights from a fixed noise model and two logistic roll-offs.
//!
//! Domain: per frame the Quattro colour grid (bottom, middle, mean of the 2 × 2 top block; half resolution) and the top
//! layer at full resolution, in sensor counts above black (14-bit range, before white balance).
//!
//! Per frame (longest exposure first, factor `r = t_shortest / t_frame ≤ 1`):
//! - variance of a value `v ≥ 0`: `(a0 + a1·v + a2·v²)·r²`, model per grid channel and for the top layer;
//! - weight `w = 1 / Σ variance` (grid: sum over the three channels, one weight per grid pixel);
//! - highlight roll-off `1 / (exp((m − knee)·s_hi) + 1)` with `m` the largest value in the 3 × 3 neighbourhood (grid:
//!   largest channel; top layer: the 3 × 3 neighbours of the same 2 × 2 phase), own value on the border; not for the
//!   shortest frame;
//! - shadow roll-off `max(floor, 1 / (exp((c_lo − n)·s_lo) + 1))` with `n` the smallest channel; not for the longest
//!   frame;
//! - numerator `+= v·r·w`, denominator `+= w` (f32 accumulators).
//!
//! After the last frame: mean = numerator / denominator, the reciprocal denominators kept as noise maps, the largest grid
//! value inside the valid area scaled to `target` (9000 of 16383 counts); top-layer values above that maximum are clipped
//! to it; grid pixels at the maximum are marked saturated.
//!
//! Lookup tables over 65536 integer steps (index = truncated value, clamped to 65535), as in the reference.

use rayon::prelude::*;

pub type Px = [f32; 3];

/// Noise model `a0 + a1·v + a2·v²` (sensor counts above black, one frame at ISO 100).
#[derive(Clone, Copy, Debug)]
pub struct NoiseModel {
    pub a: [f64; 3],
}

/// Constants of the merge.
#[derive(Clone, Debug)]
pub struct Params {
    /// grid channels (bottom, middle, top mean)
    pub grid_model: [NoiseModel; 3],
    /// top layer at full resolution
    pub top_model: NoiseModel,
    /// knee of the highlight roll-off (counts) and its slope
    pub knee: f64,
    pub hi_slope: f64,
    /// centre of the shadow roll-off (counts), slope and smallest weight
    pub lo_centre: f64,
    pub lo_slope: f64,
    pub lo_floor: f64,
    /// brightest grid value after the merge
    pub target: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            grid_model: [
                NoiseModel { a: [302.25516, 0.25684, 7.6665e-6] },
                NoiseModel { a: [156.5858, 0.22657, 7.9716e-6] },
                NoiseModel { a: [24.5292, 0.13639, 1.5474e-5] },
            ],
            top_model: NoiseModel { a: [98.1168, 0.54556, 6.1896e-5] },
            knee: 9000.0,
            hi_slope: 0.005,
            lo_centre: 300.0,
            lo_slope: 0.015,
            lo_floor: 1e-10,
            target: 9000.0,
        }
    }
}

/// One exposure: grid `gw × gh`, top layer `2gw × 2gh` (row stride `tw`), exposure time.
pub struct Frame<'a> {
    pub grid: &'a [Px],
    pub top: &'a [f32],
    pub tw: usize,
    /// exposure time (any unit, same for all frames)
    pub time: f32,
}

/// Merge result.
pub struct Merged {
    pub gw: usize,
    pub gh: usize,
    /// grid (counts, brightest value at `target`)
    pub grid: Vec<Px>,
    /// top layer `2gw × 2gh`
    pub top: Vec<f32>,
    /// reciprocal weight sum per grid pixel and per top pixel, scaled like the values
    pub grid_noise: Vec<f32>,
    pub top_noise: Vec<f32>,
    /// sum of the roll-off factors per top pixel (effective number of exposures)
    pub top_frames: Vec<f32>,
    /// grid pixels at the maximum (saturated)
    pub saturated: Vec<bool>,
    /// largest grid value before scaling and the scale factor
    pub max: f32,
    pub scale: f32,
    /// exposure factor `r` of every frame (longest first), times the scale
    pub factors: Vec<f64>,
}

fn tables(p: &Params) -> (Vec<f64>, Vec<f64>) {
    let hi = (0..65536).map(|i| 1.0 / (((i as f64) - p.knee) * p.hi_slope).exp().add1()).collect();
    let lo = (0..65536).map(|i| (1.0 / ((p.lo_centre - i as f64) * p.lo_slope).exp().add1()).max(p.lo_floor)).collect();
    (hi, lo)
}

trait Add1 {
    fn add1(self) -> f64;
}
impl Add1 for f64 {
    fn add1(self) -> f64 {
        self + 1.0
    }
}

#[inline]
fn idx(v: f32) -> usize {
    // value clamped to the table (65535) and truncated, as the reference does
    let v = if v >= 65535.0 { 65535.0 } else { v };
    (v as i64 as u64 & 0xffff_ffff) as usize & 0xffff
}

#[inline]
fn var(m: &NoiseModel, v: f32) -> f64 {
    (v * v) as f64 * m.a[2] + m.a[1] * v as f64 + m.a[0]
}

/// Merges `frames` (longest exposure first). `valid`: grid area (left, top, right, bottom; exclusive bounds) in which
/// the maximum is taken.
pub fn merge(frames: &[Frame], gw: usize, gh: usize, valid: [usize; 4], p: &Params) -> Merged {
    let n = frames.len();
    assert!(n >= 1);
    let tw2 = 2 * gw;
    let (thi, tlo) = tables(p);
    let t_short = frames.iter().map(|f| f.time).fold(f32::MAX, f32::min);
    let mut num = vec![[0f32; 3]; gw * gh];
    let mut den = vec![0f32; gw * gh];
    let mut lnum = vec![0f32; tw2 * 2 * gh];
    let mut lden = vec![0f32; tw2 * 2 * gh];
    // sum of the roll-off factors per top pixel (number of contributing exposures)
    let mut lfac = vec![0f32; tw2 * 2 * gh];
    let mut factors = Vec::with_capacity(n);
    for (k, f) in frames.iter().enumerate() {
        let first = k == 0;
        let last = k + 1 == n;
        let r = t_short as f64 / f.time as f64;
        factors.push(r);
        let r2 = r * r;
        let grid = f.grid;
        let top = f.top;
        let tw = f.tw;
        // grid
        num.par_chunks_mut(gw).zip(den.par_chunks_mut(gw)).enumerate().for_each(|(y, (nrow, drow))| {
            for x in 0..gw {
                let px = grid[y * gw + x];
                let v = px.map(|c| if c <= 0.0 { 0.0 } else { c });
                let vs: f64 = (0..3).map(|c| var(&p.grid_model[c], v[c]) * r2).sum();
                let mut w = 1.0 / vs;
                if !last {
                    let border = y < 1 || x < 1 || y + 1 >= gh || x + 1 >= gw;
                    let m = if border {
                        v[0].max(v[1]).max(v[2])
                    } else {
                        let mut m = 0f32;
                        for yy in y - 1..=y + 1 {
                            for xx in x - 1..=x + 1 {
                                let q = grid[yy * gw + xx];
                                let c = q[0].max(q[1]).max(q[2]);
                                if c > m {
                                    m = c;
                                }
                            }
                        }
                        m
                    };
                    w *= thi[idx(m)];
                }
                if !first {
                    w *= tlo[idx(v[0].min(v[1]).min(v[2]))];
                }
                let rw = r * w;
                for c in 0..3 {
                    nrow[x][c] += (v[c] as f64 * rw) as f32;
                }
                drow[x] += w as f32;
            }
        });
        // top layer, 2 × 2 per grid pixel
        lnum.par_chunks_mut(2 * tw2).zip(lden.par_chunks_mut(2 * tw2)).zip(lfac.par_chunks_mut(2 * tw2)).enumerate().for_each(|(y, ((nrows, drows), frows))| {
            for x in 0..gw {
                let border = y < 1 || x < 1 || y + 1 >= gh || x + 1 >= gw;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (ty, tx) = (2 * y + dy, 2 * x + dx);
                        let raw = top[ty * tw + tx];
                        let v = if raw <= 0.0 { 0.0 } else { raw };
                        let w0 = 1.0 / (var(&p.top_model, v) * r2);
                        let mut w = w0;
                        if !last {
                            let m = if border {
                                v
                            } else {
                                let mut m = 0f32;
                                for yy in [ty - 2, ty, ty + 2] {
                                    for xx in [tx - 2, tx, tx + 2] {
                                        let q = top[yy * tw + xx];
                                        if q > m {
                                            m = q;
                                        }
                                    }
                                }
                                m
                            };
                            w *= thi[idx(m)];
                        }
                        if !first {
                            w *= tlo[idx(v)];
                        }
                        let o = dy * tw2 + tx;
                        nrows[o] += (w * r * v as f64) as f32;
                        drows[o] += w as f32;
                        frows[o] += (w / w0) as f32;
                    }
                }
            }
        });
    }
    // last frame: means, maximum, scale
    num.par_iter_mut().zip(den.par_iter_mut()).for_each(|(n, d)| {
        let i = 1.0f32 / *d;
        *d = i;
        for c in n.iter_mut() {
            *c *= i;
        }
    });
    lnum.par_iter_mut().zip(lden.par_iter_mut()).for_each(|(n, d)| {
        *d = 1.0f32 / *d;
        *n *= *d;
    });
    let [l, t, rr, b] = valid;
    let max = (0..gh)
        .into_par_iter()
        .filter(|&y| y > t && y < b)
        .map(|y| {
            let mut m = 0f32;
            for x in (l + 1)..rr.min(gw) {
                let q = num[y * gw + x];
                m = m.max(q[0]).max(q[1]).max(q[2]);
            }
            m
        })
        .reduce(|| 0f32, f32::max);
    let s = (p.target / max as f64) as f32;
    let saturated: Vec<bool> = num.par_iter().map(|q| q[0] >= max || q[1] >= max || q[2] >= max).collect();
    num.par_iter_mut().zip(den.par_iter_mut()).for_each(|(n, d)| {
        for c in n.iter_mut() {
            *c *= s;
        }
        *d *= s;
    });
    lnum.par_iter_mut().zip(lden.par_iter_mut()).for_each(|(n, d)| {
        if *n > max {
            *n = max;
        }
        *n *= s;
        *d *= s;
    });
    let factors = factors.into_iter().map(|f| if f < f32::MAX as f64 { f * (p.target / max as f64) } else { f }).collect();
    Merged { gw, gh, grid: num, top: lnum, grid_noise: den, top_noise: lden, top_frames: lfac, saturated, max, scale: s, factors }
}

/// Range scale and mix of [`smooth_top`] (mix as the middle step of the luminance slider; scale measured on SNR of
/// flat blocks against the reference, variance = weighted mean of the exposures' variances).
pub const LUMA_SCATTER: f32 = 1.0;
pub const LUMA_MIX: f32 = 0.5;

/// Brightness noise reduction of the merged top layer (`top`, `w × h`) with the noise of the merge: 5 × 5 range filter,
/// weight `exp(−Δ²/(scatter·σ²))` with σ² = `noise · scale` (the reciprocal weight sum of the merge, scaled like the
/// values, times the scale factor), result mixed with the share `mix` into the original.
pub fn smooth_top(top: &mut [f32], noise: &[f32], w: usize, h: usize, scale: f32, scatter: f32, mix: f32) {
    if mix <= 0.0 || w == 0 || h == 0 {
        return;
    }
    const R: isize = 2;
    let src = top.to_vec();
    top.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let c = src[i];
            let inv = 1.0 / (scatter * (noise[i] * scale).max(1e-12));
            let (mut s, mut n) = (0f32, 0f32);
            for dy in -R..=R {
                let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                for dx in -R..=R {
                    let v = src[yy * w + (x as isize + dx).clamp(0, w as isize - 1) as usize];
                    let wt = (-(v - c) * (v - c) * inv).exp();
                    s += wt * v;
                    n += wt;
                }
            }
            *o = mix * (s / n) + (1.0 - mix) * c;
        }
    });
}

/// Clipped highlights of one exposure: where a grid channel or a top pixel of the 2 × 2 block reaches `clip` (per
/// channel, units of the grid), all channels of the grid pixel and the four top pixels are set to grey at the level of
/// the brightest white-balanced channel (`gains`: white balance gains, neutral grey has values `L/gains`). The grid
/// pixels around a clipped one keep their top layer and take its grey in the lower layers (next to a saturated pixel
/// the lower layers read too high: a coloured ring around specular highlights). Returns the number of clipped grid
/// pixels.
pub fn neutralize_clipped(grid: &mut [Px], top: &mut [f32], gw: usize, tw: usize, clip: [f32; 3], gains: [f32; 3]) -> usize {
    if gw == 0 {
        return 0;
    }
    let gh = grid.len() / gw;
    let mut clipped = vec![false; grid.len()];
    let mut n = 0;
    for (i, p) in grid.iter_mut().enumerate() {
        let (x, y) = (i % gw, i / gw);
        let t = 2 * y * tw + 2 * x;
        let tops = [t, t + 1, t + tw, t + tw + 1];
        let top_clipped = tops.iter().any(|&j| top.get(j).is_some_and(|v| *v >= clip[2]));
        if p[0] >= clip[0] || p[1] >= clip[1] || p[2] >= clip[2] || top_clipped {
            let l = (0..3).map(|c| p[c] * gains[c]).fold(0f32, f32::max).max(tops.iter().filter_map(|&j| top.get(j)).fold(0f32, |a, v| a.max(*v * gains[2])));
            for c in 0..3 {
                p[c] = l / gains[c];
            }
            for &j in &tops {
                if let Some(v) = top.get_mut(j) {
                    *v = l / gains[2];
                }
            }
            clipped[i] = true;
            n += 1;
        }
    }
    if n > 0 {
        for y in 0..gh {
            for x in 0..gw {
                let i = y * gw + x;
                if clipped[i] {
                    continue;
                }
                let near = (y.saturating_sub(1)..=(y + 1).min(gh - 1)).any(|yy| (x.saturating_sub(1)..=(x + 1).min(gw - 1)).any(|xx| clipped[yy * gw + xx]));
                if near {
                    let p = &mut grid[i];
                    for c in 0..2 {
                        p[c] = p[2] * gains[2] / gains[c];
                    }
                }
            }
        }
    }
    n
}

/// Focus pixel lattice of the sd Quattro H in the cropped top layer: one column every 16, two rows every 32. Returns
/// the phase (column, row) whose pixels lie lowest against the mean of their left and right neighbours (median over
/// the image), and that median ratio.
pub fn focus_lattice(top: &[f32], tw: usize, th: usize) -> (usize, usize, f32) {
    let mut best = (0, 0, f32::MAX);
    for py in 0..32 {
        for px in 0..16 {
            let mut r: Vec<f32> = Vec::new();
            for y in (py..th).step_by(32).skip(1) {
                if y + 1 >= th {
                    break;
                }
                // column 0 has no left neighbour: start one period later (not one column later, which measured the
                // phase next to the asked one)
                for x in (px..tw - 1).step_by(16).filter(|x| *x >= 1) {
                    let i = y * tw + x;
                    let n = 0.5 * (top[i - 1] + top[i + 1]);
                    if n > 50.0 {
                        r.push(top[i] / n);
                    }
                }
            }
            if r.len() > 100 {
                r.sort_by(|a, b| a.total_cmp(b));
                let m = r[r.len() / 2];
                if m < best.2 {
                    best = (px, py, m);
                }
            }
        }
    }
    best
}

/// Replaces lattice pixels (columns `cx + k·cp`, rows `ry + k·rp` and the following `rows − 1`) by the mean of their
/// left and right neighbours.
pub fn repair_lattice(plane: &mut [f32], w: usize, h: usize, cx: usize, cp: usize, ry: usize, rp: usize, rows: usize) {
    for y0 in (ry..h).step_by(rp) {
        for y in y0..(y0 + rows).min(h) {
            for x in (cx..w - 1).step_by(cp).filter(|x| *x >= 1) {
                let i = y * w + x;
                plane[i] = 0.5 * (plane[i - 1] + plane[i + 1]);
            }
        }
    }
}

/// Tone of a merged bracket: a sigmoid over a gamma-2.4-encoded, levelled input (fixed shape: start −1.17, end 2.85,
/// steepness 1), the sliders acting on it: exposure in steps of 1.5 EV on the levelled input, contrast as the factor
/// `2^contrast` on the steepness, shadows/highlights moving start/end by `2^(∓v/8)` (each limited to ±2). The level
/// puts the upper brightness quantile of the encoded image (`top`) at half the curve range; the curve extends to the
/// image maximum. The curve value is read as sRGB-encoded and written with the pure power 1/2.4 (lifted black).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BracketTone {
    /// scene-linear units per count of the tone curve input
    pub scene_per_count: f64,
    /// scene-linear value treated as white of the encoding (values above are clipped)
    pub white: f64,
    /// upper quantile of the encoded channel mean (encoding relative to `white`)
    pub top: f64,
    /// encoded maximum / `top`
    pub pmax: f64,
}

impl BracketTone {
    /// Level of the image: from the scene-linear rendering `rgb` (interleaved) with `white` as encoding white.
    /// `rgb_scale`: scene-linear units per unit of `rgb`.
    pub fn analyse(rgb: &[f32], w: usize, h: usize, rgb_scale: f64, white: f64, scene_per_count: f64) -> Self {
        let enc = |v: f32| (v.max(0.0) as f64 * rgb_scale / white).min(1.0).powf(1.0 / 2.4);
        let mut v: Vec<f32> = Vec::with_capacity(w * h / 4);
        for y in (1..h.saturating_sub(1)).step_by(2) {
            for x in (1..w.saturating_sub(1)).step_by(2) {
                let Some(p) = rgb.get((y * w + x) * 3..(y * w + x) * 3 + 3) else { continue };
                v.push(((enc(p[0]) + enc(p[1]) + enc(p[2])) / 3.0) as f32);
            }
        }
        // an image too small to sample (or an `rgb` shorter than its size) gives a neutral level instead of no value
        if v.is_empty() {
            v.push(1.0);
        }
        v.sort_by(|a, b| a.total_cmp(b));
        let q = |k: usize| v[((v.len() as f64 * (k as f64 + 1.0) * 0.0125) as usize).min(v.len().saturating_sub(1))].max(1e-6) as f64;
        let top = 0.25 * q(79) + 0.75 * q(78);
        BracketTone { scene_per_count, white, top, pmax: q(79) / top }
    }

    /// Output (sRGB-encoded 0…1) for a scene-linear value `x`.
    pub fn eval(&self, x: f64, exposure: f64, contrast: f64, shadow: f64, highlight: f64) -> f64 {
        let k = contrast.clamp(-2.0, 2.0).exp2();
        let a = -1.17 * ((-shadow).clamp(-2.0, 2.0) / 8.0).exp2();
        let b = 2.85 * (highlight.clamp(-2.0, 2.0) / 8.0).exp2();
        let s = |t: f64| 1.0 / (1.0 + (-k * (a + t * (b - a))).exp());
        let g = (exposure / 1.5).exp2() * 0.5;
        let tmax = self.pmax * g;
        let e = (x.max(0.0) / self.white).min(1.0).powf(1.0 / 2.4);
        let t = (e / self.top * g).min(tmax);
        let v = ((s(t) - s(0.0)) / (s(tmax) - s(0.0))).clamp(0.0, 1.0);
        let lin = if v < 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        lin.powf(1.0 / 2.4)
    }

    /// 65536 LUT count → linear light 0…65535 (sRGB primaries), like the single-exposure tone curve.
    pub fn lut(&self, exposure: f64, contrast: f64, shadow: f64, highlight: f64) -> Vec<u16> {
        (0..65536)
            .into_par_iter()
            .map(|i| {
                let e = self.eval(i as f64 * self.scene_per_count, exposure, contrast, shadow, highlight);
                let lin = if e <= 0.04045 { e / 12.92 } else { ((e + 0.055) / 1.055).powf(2.4) };
                (lin * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16
            })
            .collect()
    }
}

/// Encoding white of the bracket tone in scene-linear units (middle grey 0.184); measured on three brackets.
pub const BRACKET_WHITE: f64 = 0.8;

/// Scene-linear units per unit of the preparation's linear RGB (`camera_to_rgb` of the normalised layers) of a merged
/// bracket (sd Quattro H, ISO 100; measured).
pub const SCENE_PER_PREPARED: f64 = 2.6;

/// [`BracketTone::lut`] with a small cache (sliders change often).
pub fn cached_lut(t: &BracketTone, exposure: f64, contrast: f64, shadow: f64, highlight: f64) -> std::sync::Arc<Vec<u16>> {
    type Entry = ((BracketTone, [u64; 4]), std::sync::Arc<Vec<u16>>);
    static CACHE: std::sync::Mutex<Vec<Entry>> = std::sync::Mutex::new(Vec::new());
    let key = (*t, [exposure.to_bits(), contrast.to_bits(), shadow.to_bits(), highlight.to_bits()]);
    if let Some((_, l)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key) {
        return l.clone();
    }
    let l = std::sync::Arc::new(t.lut(exposure, contrast, shadow, highlight));
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= 8 {
        c.remove(0);
    }
    c.push((key, l.clone()));
    l
}

static BRACKET_TONE: std::sync::Mutex<Option<BracketTone>> = std::sync::Mutex::new(None);

/// Quattro colour renderings use `t` instead of the single-exposure tone curve (`None`: off).
pub fn set_bracket_tone(t: Option<BracketTone>) {
    *BRACKET_TONE.lock().unwrap_or_else(|e| e.into_inner()) = t;
}

pub fn bracket_tone() -> Option<BracketTone> {
    *BRACKET_TONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Merged layers handed to the Quattro preparation in place of one frame's values (test bench): grid and full top
/// layer in the units of the first noise reduction stage (counts above black / (SensorISO/100)).
pub struct Replacement {
    pub grid: Vec<Px>,
    pub gw: usize,
    pub gh: usize,
    pub top: Vec<f32>,
    pub tw: usize,
    pub th: usize,
    /// reciprocal weight sums of the merge (grid), scaled like the values; `None` = noise as one frame
    pub grid_noise: Option<Vec<f32>>,
    /// factor from the merged values to the units of the base exposure (its exposure time over the shortest one,
    /// divided by the merge's scale); `None`: unknown (the bracket tone levels the image instead)
    pub base_gain: Option<f32>,
}

/// Factor from merged values to the units of the exposure `base_time`: the merge is in units of the shortest exposure
/// times `scale` ([`Merged::scale`]).
pub fn base_gain(base_time: f32, shortest_time: f32, scale: f32) -> Option<f32> {
    let g = base_time / shortest_time / scale;
    (g.is_finite() && g > 0.0).then_some(g)
}

/// Start tone of a merged bracket: `false` (default) develops it like its base exposure (same brightness and colour as
/// a single frame, the merge only lowers noise and keeps the highlights for the sliders); `GRAINEO_SFD_TONE=bracket`:
/// the bracket tone curve ([`BracketTone`], the dark and flat look of the camera maker's merged output).
pub fn bracket_tone_start() -> bool {
    std::env::var("GRAINEO_SFD_TONE").is_ok_and(|v| v == "bracket")
}

type Slot = std::sync::Mutex<Option<(std::path::PathBuf, std::sync::Arc<Replacement>)>>;

fn slot() -> &'static Slot {
    static S: Slot = std::sync::Mutex::new(None);
    &S
}

/// The preparation of `path` uses `r` instead of the file's own sensor values (one file at a time; test bench only).
pub fn set_replacement(path: &std::path::Path, r: Replacement) {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    *slot().lock().unwrap_or_else(|e| e.into_inner()) = Some((p, std::sync::Arc::new(r)));
}

/// Replacement registered for `path`.
pub fn replacement(path: &std::path::Path) -> Option<std::sync::Arc<Replacement>> {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    slot().lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(q, _)| *q == p).map(|(_, r)| r.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_repair_hits_the_asked_column() {
        // lattice at column phase 0 (period 4), row 1 (period 4): column 0 itself has no left neighbour, the others
        // of the phase are replaced; the column next to them stays
        let (w, h) = (12, 8);
        let mut p: Vec<f32> = (0..w * h).map(|i| if (i % w) % 4 == 0 { 0.0 } else { 10.0 }).collect();
        repair_lattice(&mut p, w, h, 0, 4, 1, 4, 1);
        assert_eq!(p[w + 4], 10.0);
        assert_eq!(p[w + 8], 10.0);
        assert_eq!(p[w + 5], 10.0);
        assert_eq!(p[w], 0.0);
        assert_eq!(p[4], 0.0, "other rows stay");
        // the detection names the same phase
        let (tw, th) = (320, 640);
        let top: Vec<f32> = (0..tw * th).map(|i| if (i % tw) % 16 == 0 && (i / tw) % 32 == 5 { 60.0 } else { 100.0 }).collect();
        let (px, py, ratio) = focus_lattice(&top, tw, th);
        assert_eq!((px, py), (0, 5));
        assert!((ratio - 0.6).abs() < 1e-3);
    }

    #[test]
    fn clipped_cells_and_their_ring_turn_grey() {
        let (gw, gh, tw) = (5, 5, 10);
        let gains = [1.0f32, 2.0, 4.0];
        // a coloured image; the centre cell has a clipped top pixel
        let mut grid = vec![[40.0f32, 10.0, 5.0]; gw * gh];
        let mut top = vec![5.0f32; tw * 2 * gh];
        top[2 * 2 * tw + 2 * 2] = 100.0;
        let n = neutralize_clipped(&mut grid, &mut top, gw, tw, [1000.0, 1000.0, 90.0], gains);
        assert_eq!(n, 1);
        // centre: grey at the brightest white-balanced value (top pixel 100 × 4)
        assert_eq!(grid[2 * gw + 2], [400.0, 200.0, 100.0]);
        assert_eq!(top[2 * 2 * tw + 2 * 2 + 1], 100.0);
        // ring: lower layers grey at the cell's own top value, top unchanged
        assert_eq!(grid[gw + 1], [20.0, 10.0, 5.0]);
        assert_eq!(top[2 * tw + 2], 5.0);
        // farther away: untouched
        assert_eq!(grid[0], [40.0, 10.0, 5.0]);
    }

    #[test]
    fn base_gain_lifts_the_merge_to_the_base_exposure() {
        // SDQH1644: base 1/500 s, shortest 1/4000 s, merge scale 0.2957 → 27.05 (+4.76 EV)
        let g = base_gain(2000.0, 250.0, 0.2957).unwrap();
        assert!((g - 27.054).abs() < 0.01, "{g}");
        assert!(base_gain(2000.0, 0.0, 0.3).is_none() && base_gain(2000.0, 250.0, 0.0).is_none());
    }

    #[test]
    fn two_frames_average_and_scale() {
        let (gw, gh) = (6, 5);
        let long: Vec<Px> = vec![[400.0, 300.0, 200.0]; gw * gh];
        let short: Vec<Px> = vec![[100.0, 75.0, 50.0]; gw * gh];
        let tl = vec![200.0f32; 4 * gw * gh];
        let ts = vec![50.0f32; 4 * gw * gh];
        let frames = [
            Frame { grid: &long, top: &tl, tw: 2 * gw, time: 4.0 },
            Frame { grid: &short, top: &ts, tw: 2 * gw, time: 1.0 },
        ];
        let m = merge(&frames, gw, gh, [0, 0, gw, gh], &Params::default());
        // both frames agree (100 counts of the shortest frame): the mean is that value, scaled to 9000
        assert!((m.max - 100.0).abs() < 0.05, "{}", m.max);
        assert!((m.grid[13][0] - 9000.0).abs() < 2.0);
        assert!((m.grid[13][2] - 4500.0).abs() < 2.0);
        assert!((m.top[30] - 4500.0).abs() < 2.0);
        assert!(m.saturated[13]);
    }
}

/// One exposure of a bracket, read for the merge.
pub struct FrameInput {
    pub grid: crate::quattro_grid::RawGrid,
    /// exposure time (µs)
    pub time: f32,
    /// clip level per grid channel and white balance gains in the units of the grid
    pub clip: [f32; 3],
    pub gains: [f32; 3],
    /// top of the stored range per grid channel (units of the grid)
    pub range: [f32; 3],
}

fn camf_scalar(camf: &x3f::Camf, name: &str) -> Option<f64> {
    camf.matrix(name).ok()?.as_f64().first().copied()
}

/// Reads one exposure: `camf` its camera data, `reader` its stored values.
pub fn read_frame(camf: &x3f::Camf, reader: x3f_core::Reader) -> Result<FrameInput, String> {
    let offset = camf_scalar(camf, "RawOffset").unwrap_or(2048.0) as f32;
    let sensor = camf_scalar(camf, "SensorISO").filter(|v| *v > 0.0).unwrap_or(100.0);
    let digital = camf.matrix("DigitalISOGain").ok().map(|m| m.as_f64()).filter(|v| v.len() == 3).unwrap_or(vec![1.0; 3]);
    let unit: [f32; 3] = std::array::from_fn(|c| (digital[c] / (sensor / 100.0)) as f32);
    let time = camf_scalar(camf, "CaptureShutter").ok_or("CaptureShutter missing")? as f32;
    let grid = crate::quattro_grid::read_reader(reader, offset, unit, true)?;
    let white = 16383.0 - offset;
    let clip: [f32; 3] = std::array::from_fn(|c| 0.97 * white * unit[c]);
    let g = camf.matrix("AutoWBGain").ok().map(|m| m.as_f64()).filter(|v| v.len() == 3).unwrap_or(vec![1.0; 3]);
    // the units of the grid include the digital gain of the layer: the white balance gains apply to them as they are
    // (divided by the digital gain once more, grey came out with the lower layers at twice the top layer's level)
    let gains: [f32; 3] = std::array::from_fn(|c| g[c] as f32);
    let range: [f32; 3] = std::array::from_fn(|c| white * unit[c]);
    Ok(FrameInput { grid, time, clip, gains, range })
}

/// Share of the measured saturation level from which a value counts as clipped.
const CLIP_SHARE: f32 = 0.97;

/// Clip level per grid channel from the exposures themselves. The top of the stored range is not where every sensor
/// saturates: the sd Quattro H (digital gain per layer) reaches only about half of it, at another level per layer.
/// A saturated layer shows as a pile-up at the top of its histogram ([`crate::quattro::layer_saturation`]); the lowest
/// level found in any exposure counts (the long exposures of a bracket always saturate somewhere). Channels without a
/// pile-up keep infinity (the level from the stored range applies).
pub fn measured_clip(frames: &[FrameInput]) -> [f32; 3] {
    let mut clip = [f32::INFINITY; 3];
    for f in frames {
        for c in 0..3 {
            let level = if c == 2 {
                crate::quattro::saturation_of_values(&f.grid.top, 1.0 / f.range[2])
            } else {
                let plane: Vec<f32> = f.grid.grid.par_iter().map(|p| p[c]).collect();
                crate::quattro::saturation_of_values(&plane, 1.0 / f.range[c])
            };
            if let Some(l) = level {
                clip[c] = clip[c].min(CLIP_SHARE * l * f.range[c]);
            }
        }
    }
    clip
}

/// Error text of a cancelled merge.
pub const CANCELLED: &str = "cancelled";

/// Merges the exposures (any order) with the focus-pixel repair, grey clipped highlights and the brightness noise
/// reduction of the top layer. Returns the replacement for the Quattro preparation (`tw × th` = size of the frames'
/// top layer) and the merge statistics.
pub fn merge_frames(mut frames: Vec<FrameInput>, cancel: &std::sync::atomic::AtomicBool, progress: &(dyn Fn(f32) + Sync)) -> Result<Replacement, String> {
    use std::sync::atomic::Ordering;
    if frames.is_empty() {
        return Err("no exposure".into());
    }
    frames.sort_by(|a, b| b.time.total_cmp(&a.time));
    // base exposure: the middle one (the preparation takes its calibration from it)
    let (base_time, shortest_time) = (frames[frames.len() / 2].time, frames[frames.len() - 1].time);
    let (gw, gh, tw, th) = (frames[0].grid.gw, frames[0].grid.gh, frames[0].grid.tw, frames[0].grid.th);
    if frames.iter().any(|f| (f.grid.gw, f.grid.gh, f.grid.tw, f.grid.th) != (gw, gh, tw, th)) || tw < 2 * gw || th < 2 * gh {
        return Err("exposures of different size".into());
    }
    // an exposure without pixels (or with fewer values than its size says) cannot be merged
    if gw == 0 || gh == 0 || frames.iter().any(|f| f.grid.grid.len() < gw * gh || f.grid.top.len() < tw * th) {
        return Err("empty exposure".into());
    }
    let (px, py, ratio) = focus_lattice(&frames[0].grid.top, tw, th);
    let n = frames.len();
    // the level at which each layer really saturates (see [`measured_clip`]), one for all exposures
    let measured = measured_clip(&frames);
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("merged bracket: measured clip levels {measured:?}, from the stored range {:?}, grey gains {:?}", frames[0].clip, frames[0].gains);
    }
    for f in frames.iter_mut() {
        for c in 0..3 {
            f.clip[c] = f.clip[c].min(measured[c]);
        }
    }
    frames.par_iter_mut().for_each(|f| {
        let g = &mut f.grid;
        if ratio < 0.97 {
            repair_lattice(&mut g.top, tw, th, px, 16, (py + 31) % 32, 32, 3);
            for c in 0..2 {
                let mut p: Vec<f32> = g.grid.iter().map(|q| q[c]).collect();
                repair_lattice(&mut p, gw, gh, px / 2, 8, py / 2, 16, 1);
                for (q, v) in g.grid.iter_mut().zip(p) {
                    q[c] = v;
                }
            }
            for y in 0..gh {
                for x in 0..gw {
                    let i = 2 * y * tw + 2 * x;
                    g.grid[y * gw + x][2] = 0.25 * (g.top[i] + g.top[i + 1] + g.top[i + tw] + g.top[i + tw + 1]);
                }
            }
        }
        neutralize_clipped(&mut g.grid, &mut g.top, gw, tw, f.clip, f.gains);
    });
    progress(0.1);
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
    }
    let views: Vec<Frame> = frames.iter().map(|f| Frame { grid: &f.grid.grid, top: &f.grid.top, tw, time: f.time }).collect();
    let mut m = merge(&views, gw, gh, [0, 0, gw.saturating_sub(1), gh.saturating_sub(1)], &Params::default());
    drop(views);
    drop(frames);
    progress(0.6);
    if cancel.load(Ordering::Relaxed) || n == 0 {
        return Err(CANCELLED.into());
    }
    let noise: Vec<f32> = m.top_noise.iter().zip(&m.top_frames).map(|(a, b)| a * b).collect();
    let scale = m.scale;
    smooth_top(&mut m.top, &noise, 2 * gw, 2 * gh, scale, LUMA_SCATTER, LUMA_MIX);
    progress(0.9);
    let mut top = vec![0f32; tw * th];
    top.par_chunks_mut(tw).enumerate().for_each(|(y, row)| {
        let sy = y.min(2 * gh - 1) * 2 * gw;
        for (x, o) in row.iter_mut().enumerate() {
            *o = m.top[sy + x.min(2 * gw - 1)];
        }
    });
    let base_gain = base_gain(base_time, shortest_time, m.scale);
    Ok(Replacement { grid: m.grid, gw, gh, top, tw, th, grid_noise: Some(m.grid_noise), base_gain })
}

/// Merge of all frames of an X3I file ([`merge_frames`]); `progress` 0…1 (reading the frames takes the first 70 %).
pub fn merge_x3i(path: &std::path::Path, cancel: &std::sync::atomic::AtomicBool, progress: &(dyn Fn(f32) + Sync)) -> Result<Replacement, String> {
    use std::sync::atomic::Ordering;
    let container = x3f_core::X3iContainer::open(path).map_err(|e| e.to_string())?;
    let ours = x3f::x3i::X3iFile::open(path).map_err(|e| e.to_string())?;
    let n = container.frames().len();
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let camf = ours.frame(i).and_then(|f| f.camf()).map_err(|e| e.to_string())?;
        let reader = container.open_frame(i).map_err(|e| e.to_string())?;
        frames.push(read_frame(&camf, reader)?);
        progress(0.7 * (i + 1) as f32 / n as f32);
    }
    merge_frames(frames, cancel, &|p| progress(0.7 + 0.3 * p))
}

/// A running merge (for the progress display and cancelling).
#[derive(Clone, Debug)]
pub struct Job {
    pub path: std::path::PathBuf,
    /// 0…1
    pub progress: f32,
    pub started: std::time::Instant,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Job {
    /// Estimated remaining seconds (after the first tenth).
    pub fn remaining_secs(&self) -> Option<f32> {
        let t = self.started.elapsed().as_secs_f32();
        (self.progress >= 0.1).then(|| t * (1.0 - self.progress) / self.progress)
    }
}

static JOBS: std::sync::Mutex<Vec<Job>> = std::sync::Mutex::new(Vec::new());
static GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The running merges.
pub fn jobs() -> Vec<Job> {
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Changes with every progress step (redraw).
pub fn generation() -> u32 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Cancels every running merge.
pub fn cancel_all() {
    for j in jobs() {
        j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

type Done = Result<std::sync::Arc<Replacement>, String>;
type DoneSlot = (std::path::PathBuf, std::sync::Arc<pools::Shared<Done>>);

fn done_cache() -> &'static std::sync::Mutex<Option<DoneSlot>> {
    static C: std::sync::Mutex<Option<DoneSlot>> = std::sync::Mutex::new(None);
    &C
}

/// The merge of the X3I `path` (the last result kept for the next preparation of the same file), registered as a job
/// while it runs. Uses all processor cores. Computed once per file and shared ([`pools::Shared`]): the quick and the
/// full preparation ask for it at the same time and get the same result (two merges side by side took twice the
/// memory). A failed or cancelled merge is not kept.
pub fn merged_x3i(path: &std::path::Path) -> Result<std::sync::Arc<Replacement>, String> {
    let slot = {
        let mut c = done_cache().lock().unwrap_or_else(|e| e.into_inner());
        match c.as_ref() {
            Some((p, s)) if p == path => s.clone(),
            _ => {
                let s = std::sync::Arc::new(pools::Shared::default());
                *c = Some((path.to_path_buf(), s.clone()));
                s
            }
        }
    };
    let r = slot.get_or_compute(|| merge_registered(path).map(std::sync::Arc::new));
    if r.is_err() {
        let mut c = done_cache().lock().unwrap_or_else(|e| e.into_inner());
        if c.as_ref().is_some_and(|(_, s)| std::sync::Arc::ptr_eq(s, &slot)) {
            *c = None;
        }
    }
    r
}

/// [`merge_x3i`] shown as a running job.
fn merge_registered(path: &std::path::Path) -> Result<Replacement, String> {
    use std::sync::atomic::Ordering;
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).push(Job { path: path.to_path_buf(), progress: 0.0, started: std::time::Instant::now(), cancel: cancel.clone() });
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let progress = |p: f32| {
        if let Some(j) = JOBS.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|j| j.path == path) {
            j.progress = p;
        }
        GENERATION.fetch_add(1, Ordering::Relaxed);
    };
    let r = merge_x3i(path, &cancel, &progress);
    JOBS.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| !std::sync::Arc::ptr_eq(&j.cancel, &cancel));
    GENERATION.fetch_add(1, Ordering::Relaxed);
    r
}

static FALLBACKS: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The merge of `path` failed and the preparation fell back to another merge: its result must not be stored as the
/// file's base (the disk cache key does not tell the two apart).
pub fn note_fallback(path: &std::path::Path) {
    let mut f = FALLBACKS.lock().unwrap_or_else(|e| e.into_inner());
    if !f.iter().any(|p| p == path) {
        if f.len() >= 64 {
            f.remove(0);
        }
        f.push(path.to_path_buf());
    }
}

/// Did the merge of `path` fall back ([`note_fallback`])?
pub fn fell_back(path: &std::path::Path) -> bool {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    FALLBACKS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| *p == canon || p == path)
}

/// X3I files merge on the sensor grid ([`merged_x3i`]); off: the scene-linear merge of x3fuse-core
/// (`GRAINEO_X3I_MERGE=x3fuse`, or the setting of the advanced options via [`set_grid_merge`]).
pub fn grid_merge() -> bool {
    GRID_MERGE.load(std::sync::atomic::Ordering::Relaxed) && std::env::var("GRAINEO_X3I_MERGE").map_or(true, |v| v != "x3fuse")
}

static GRID_MERGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_grid_merge(on: bool) {
    GRID_MERGE.store(on, std::sync::atomic::Ordering::Relaxed);
}
