//! Weighted merge in linear, white-balanced camera RGB of the reference, streamed frame by frame (bright → dark).
//!
//! Per frame: saturation plane from the raw mosaic (value / clip of its channel, 5×5 maximum), reference white balance on
//! the mosaic, demosaic (RCD / Markesteijn 3) on the mosaic **scaled by 1/max(WB)** so the demosaic never clips the
//! white-balanced values at 1.0 (cause of the green highlight cast of the first prototype), scaled back, Lanczos-3 warp
//! into the reference geometry (de-ringing: clamped to the 4 nearest samples; the reference itself is never resampled),
//! division by the exposure factor, accumulation with noise-optimal weights
//!
//! ```text
//! w_i = sat_i · k_i² / (a_i · k_i · X̂ + b_i)
//! ```
//!
//! (maximum likelihood for Poisson-Gaussian noise; Granados et al. 2010, Hasinoff et al. 2010) with the noise model
//! `var = a·v + b` of each frame and the pilot irradiance X̂ = value of the brightest unsaturated frame. One weight for
//! all channels (common saturation mask), saturated pixels get no weight (soft ramp 0.75 … 0.92 of clip). Pixels
//! without any weight keep the value of the darkest frame covering them.
//!
//! Memory: accumulators over the reference crop (32 B/px) plus one frame being processed.

use crate::exposure::proxy_map;
use crate::frames::Frame;
use crate::ghost;
use crate::img::{max_filter, Lanczos};
use crate::{Control, Error, Plan};
use raw::development::{demosaic_mosaic, Demosaic};
use raw::Sensor;
use rayon::prelude::*;

/// Saturation ramp: full weight below 0.75 of clip, none above 0.92.
#[inline]
pub fn ramp(s: f32) -> f32 {
    ((0.92 - s) / 0.17).clamp(0.0, 1.0)
}

/// Noise-optimal weight of a frame with factor `k`, noise model (`a`, `b`, white-balanced units) and pilot irradiance
/// `pilot` (reference units).
#[inline]
pub fn weight(sat: f32, k: f32, a: f32, b: f32, pilot: f32) -> f32 {
    let s = (k * pilot).max(0.0);
    sat * k * k / (a * s + b).max(1e-20)
}

/// Memory the merge may use (bytes); 0 = unlimited. The app sets it from the computer's memory; the merge then works
/// in horizontal bands (accumulators of a band at a time) when the whole would not fit. The result is the same.
static MEMORY_BUDGET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn set_memory_budget(bytes: u64) {
    MEMORY_BUDGET.store(bytes, std::sync::atomic::Ordering::Relaxed);
}

/// Estimated peak memory of a full merge in bands (bytes): frames and per-frame work plus the accumulators and the
/// output of one band. Measured (release, 4 threads): 24 MP × 5 frames with ghost handling 3.7 GB in one band, 3.15 GB in two, 2.8 GB in three.
pub fn estimate_memory(pixels: usize, frames: usize, crop_pixels: usize, ghost: bool, bands: usize) -> u64 {
    let base = (4 * frames + 42) as u64 * pixels as u64;
    let per_crop = if ghost { 64 } else { 32 } + 24;
    base + per_crop as u64 * (crop_pixels / bands.max(1)) as u64
}

/// Number of bands for the memory budget (`GRAINEO_HDR_BANDS` overrides, for tests and measurements).
pub fn bands_for(plan: &Plan) -> usize {
    if let Some(n) = std::env::var("GRAINEO_HDR_BANDS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.clamp(1, 64);
    }
    let budget = MEMORY_BUDGET.load(std::sync::atomic::Ordering::Relaxed);
    if budget == 0 {
        return 1;
    }
    let f = &plan.frames[plan.reference];
    let ghost = plan.settings.ghost != ghost::Level::Off || !plan.settings.paint.is_empty();
    // (10 % safety: the estimate stays a little under what is measured)
    (1..=16).find(|&b| estimate_memory(f.w * f.h, plan.frames.len(), plan.crop.w * plan.crop.h, ghost, b) / 10 * 11 <= budget).unwrap_or(16)
}

/// The merged radiance in the form the settings ask for.
fn deliver(settings: &crate::Settings, m: Merged, ctl: &Control) -> Result<Merged, Error> {
    match settings.output {
        crate::fusion::Output::Linear => Ok(m),
        crate::fusion::Output::Fusion => {
            let mut f = crate::fusion::fuse(&m, ctl)?;
            crate::fusion::blend(&m, &mut f, settings.fusion_strength);
            Ok(f)
        }
    }
}

/// Linear result of a merge.
pub struct Merged {
    /// white-balanced camera RGB of the reference, reference units (1.0 ≈ white of the reference), values > 1 allowed
    pub px: Vec<[f32; 3]>,
    pub w: usize,
    pub h: usize,
    /// noise model of the result per channel (`var = a·v + b`), analytic line fit (see [`crate::result_noise`])
    pub noise: raw::noise::NoiseModel,
}

impl Merged {
    /// Brightest value of the result (green) – the highlight headroom over the reference white.
    pub fn peak(&self) -> f32 {
        self.px.par_iter().map(|p| p[1]).reduce(|| 0.0, f32::max)
    }
}

/// Accumulator of one output pixel.
#[derive(Clone, Copy)]
struct AccPx {
    sum: [f32; 3],
    ws: f32,
    pilot: f32,
    last: [f32; 3],
}

impl AccPx {
    const EMPTY: AccPx = AccPx { sum: [0.0; 3], ws: 0.0, pilot: f32::NAN, last: [0.0; 3] };

    /// Add the value `x` (reference units) of a frame with factor `k`, noise model (`a`, `b`) and saturation ramp `hh`.
    /// Frames come bright → dark: the pilot is the green of the brightest frame that has weight here.
    #[inline]
    fn add(&mut self, x: [f32; 3], hh: f32, k: f32, a: f32, b: f32) {
        self.last = x;
        if self.pilot.is_nan() && hh * 255.0 >= 0.5 {
            self.pilot = x[1];
        }
        if hh > 0.0 {
            // at the very end of the saturation ramp (weight below half an 8-bit step) the frame is no pilot yet: its
            // own green stands in, so the weight has no jump there (a missing pilot counted as black)
            let pilot = if self.pilot.is_nan() { x[1] } else { self.pilot };
            let wt = weight(hh, k, a, b, pilot);
            for c in 0..3 {
                self.sum[c] += wt * x[c];
            }
            self.ws += wt;
        }
    }

    #[inline]
    fn value(&self) -> [f32; 3] {
        if self.ws > 0.0 {
            self.sum.map(|v| v / self.ws)
        } else {
            self.last
        }
    }
}

/// Accumulators over the output area.
struct Acc {
    px: Vec<AccPx>,
}

impl Acc {
    fn new(n: usize) -> Acc {
        Acc { px: vec![AccPx::EMPTY; n] }
    }

    fn finish(self) -> Vec<[f32; 3]> {
        self.px.par_iter().map(|p| p.value()).collect()
    }
}

/// Merges that run beside the plain one when ghost handling is active: the selective merge (reference, consistent
/// chain, painted sources) and its blend mask.
struct Select<'a> {
    maps: &'a ghost::Maps,
    acc: Acc,
    /// sensor pixels per cell of the grid in the coordinates of the merge
    cell: usize,
}

impl Select<'_> {
    /// Finish the selective merge: pixels in a hole (reference saturated, nothing consistent) keep the clipped
    /// reference, never a darker frame.
    fn finish(self, w: usize, x0: usize, y0: usize, fill: f32) -> Vec<[f32; 3]> {
        let (maps, cell) = (self.maps, self.cell);
        self.acc
            .px
            .par_iter()
            .enumerate()
            .map(|(p, a)| {
                let (x, y) = (x0 + p % w, y0 + p / w);
                if a.ws <= 0.0 && maps.is_hole(maps.cell_at(x, y, cell)) {
                    a.last.map(|v| v.min(fill))
                } else {
                    a.value()
                }
            })
            .collect()
    }
}

/// Add one frame's pixel to the plain accumulator and, where it may contribute there, to the selective one
/// (`sel` = accumulator and "this frame may contribute at this pixel").
#[inline]
fn put(all: Option<&mut AccPx>, sel: Option<(&mut AccPx, bool)>, x: [f32; 3], hh: f32, k: f32, a: f32, b: f32) {
    if let Some(all) = all {
        all.add(x, hh, k, a, b);
    }
    if let Some((s, include)) = sel {
        if include {
            s.add(x, hh, k, a, b);
        }
    }
}

/// Frame indices bright → dark.
fn bright_to_dark(ks: &[f64]) -> Vec<usize> {
    let mut o: Vec<usize> = (0..ks.len()).collect();
    o.sort_by(|&a, &b| ks[b].total_cmp(&ks[a]));
    o
}

/// Saturation plane of the raw mosaic: value / clip of its channel, maximum over 5×5.
fn saturation_plane(f: &Frame) -> Vec<f32> {
    let (w, h) = (f.w, f.h);
    let mut s = vec![0f32; w * h];
    if let Some(pl) = &f.planar {
        // Foveon: a pixel counts as saturated when any of its three layers is
        s.par_iter_mut().zip(pl.par_iter()).for_each(|(o, p)| *o = (p[0] / f.clip[0]).max(p[1] / f.clip[1]).max(p[2] / f.clip[2]));
        return max_filter(&s, w, h, 2);
    }
    s.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = f.mosaic[y * w + x] / f.clip[f.color_at(x, y)];
        }
    });
    max_filter(&s, w, h, 2)
}

/// Demosaic of a scaled mosaic (values ≤ 1) elsewhere, e.g. on the GPU: `(mosaic, w, h, pattern, sensor)` → RGB, or
/// `None` to compute on the CPU.
pub type DemosaicHook = dyn Fn(&[f32], usize, usize, [[u8; 6]; 6], Sensor) -> Option<Vec<[f32; 3]>> + Sync;

/// Ghost maps of the plan when they change anything (`None`: the plain merge is the result).
fn ghost_maps(plan: &Plan) -> Option<ghost::Maps> {
    if plan.settings.ghost == ghost::Level::Off && plan.settings.paint.is_empty() {
        return None;
    }
    let m = ghost::detect(plan);
    // painted strokes work in every level, including "off"
    let active = m.area > 0.0 || m.holes > 0.0 || m.paint.iter().any(|p| *p != ghost::NO_PAINT);
    active.then_some(m)
}

/// Combine the plain and the selective merge of the output area (`ox`, `oy`: its origin in the pixels the grid cells
/// of `cell` pixels are counted in).
#[allow(clippy::too_many_arguments)]
fn combine(all: Option<Acc>, sel: Option<Select>, w: usize, h: usize, ox: usize, oy: usize, fill: f32) -> Vec<[f32; 3]> {
    match (all, sel) {
        (Some(a), Some(s)) => {
            let mask = ghost::upsample(&s.maps.mask, s.maps.qw, s.maps.qh, s.cell, ox, oy, w, h);
            let a = a.finish();
            let b = s.finish(w, ox, oy, fill);
            ghost::blend(&a, &b, &mask, w, h)
        }
        (None, Some(s)) => s.finish(w, ox, oy, fill),
        (Some(a), None) => a.finish(),
        (None, None) => Vec::new(),
    }
}

/// Full-resolution merge (streams the frames; the mosaics of the plan are consumed).
pub fn full(plan: &mut Plan, ctl: &Control) -> Result<Merged, Error> {
    full_with(plan, ctl, None)
}

/// [`full`] with a demosaic hook (see [`DemosaicHook`]).
pub fn full_with(plan: &mut Plan, ctl: &Control, hook: Option<&DemosaicHook>) -> Result<Merged, Error> {
    full_banded(plan, ctl, hook, None)
}

/// [`full_with`] with a fixed number of bands (`None`: from the memory budget, see [`bands_for`]); the result does not
/// depend on it.
pub fn full_banded(plan: &mut Plan, ctl: &Control, hook: Option<&DemosaicHook>, bands: Option<usize>) -> Result<Merged, Error> {
    let ri = plan.reference;
    let (w, h) = (plan.frames[ri].w, plan.frames[ri].h);
    let r = plan.crop;
    if r.w == 0 || r.h == 0 {
        return Err(Error::Internal("crop outside the sensor".into()));
    }
    let wb = plan.wb;
    let smax = wb[0].max(wb[1]).max(wb[2]).max(1e-6);
    let ks: Vec<f64> = plan.k.clone();
    let order = bright_to_dark(&ks);
    let lz = Lanczos::new();
    let (cx, cy, cs) = (w as f64 / 2.0, h as f64 / 2.0, w.max(h) as f64 / 2.0);
    let maps = crate::profile("ghost-maps", || ghost_maps(plan));
    let fine = maps.as_ref().is_some_and(|m| m.level == ghost::Level::Fine);
    let fill = plan.frames[ri].clip[1] * wb[1];
    let n = order.len();
    // bands of output rows (see `set_memory_budget`): the accumulators of one band at a time; every band runs all the
    // frames again (demosaic included), so it costs time, never quality
    let bands = bands.unwrap_or_else(|| bands_for(plan)).clamp(1, r.h.max(1));
    let band_rows = r.h.div_ceil(bands);
    // rounding the rows up can cover the height with fewer bands: no band may start below the last row
    let bands = r.h.div_ceil(band_rows.max(1)).max(1);
    let mut px: Vec<[f32; 3]> = Vec::with_capacity(r.w * r.h);
    let mut px_sel: Vec<[f32; 3]> = Vec::new();
    for band in 0..bands {
    let y_off = band * band_rows;
    let rows_in_band = band_rows.min(r.h - y_off);
    let last_band = band + 1 == bands;
    let mut acc = (!fine).then(|| Acc::new(r.w * rows_in_band));
    let mut sel = maps.as_ref().map(|m| Select { maps: m, acc: Acc::new(r.w * rows_in_band), cell: m.qf });
    for (step, &i) in order.iter().enumerate() {
        ctl.check()?;
        let base = (band as f32 + step as f32 / n as f32) / bands as f32;
        ctl.report(base);
        let sat = crate::profile("saturation", || saturation_plane(&plan.frames[i]));
        let f = &mut plan.frames[i];
        let sensor = f.info.sensor;
        let pat = f.pattern;
        // (the mosaic stays for the next band)
        let rgb: Vec<[f32; 3]> = if let Some(pl) = f.planar.as_mut() {
            // Foveon: the three layers are the colour already; only the reference white balance is applied
            let planes = if last_band { std::mem::take(pl) } else { pl.clone() };
            f.mosaic = Vec::new();
            if planes.is_empty() {
                return Err(Error::Internal("planes already consumed".into()));
            }
            planes.into_par_iter().map(|p| [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]]).collect()
        } else {
            let mut m = if last_band { std::mem::take(&mut f.mosaic) } else { f.mosaic.clone() };
            if m.is_empty() {
                return Err(Error::Internal("mosaic already consumed".into()));
            }
            // reference white balance, scaled by 1/max(WB) so the demosaic input stays ≤ 1
            m.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    *v *= wb[pat[y % 6][x % 6] as usize] / smax;
                }
            });
            ctl.check()?;
            let dm = if sensor == Sensor::XTrans { Demosaic::Markesteijn3 } else { Demosaic::Rcd };
            let mut rgb = crate::profile("demosaic", || match hook.and_then(|hk| hk(&m, w, h, pat, sensor)) {
                Some(r) => r,
                None => demosaic_mosaic(&m, w, h, pat, dm, sensor),
            });
            drop(m);
            rgb.par_iter_mut().for_each(|p| *p = p.map(|v| v * smax));
            rgb
        };

        ctl.report(base + 0.6 / (n * bands) as f32);
        ctl.check()?;
        let k = ks[i] as f32;
        let (a, b) = (plan.noise[i].0, plan.noise[i].1);
        let warp = plan.warps[i];
        let ident = i == ri;
        let mut all_rows: Vec<Option<&mut [AccPx]>> = match &mut acc {
            Some(a) => a.px.chunks_mut(r.w).map(Some).collect(),
            None => (0..rows_in_band).map(|_| None).collect(),
        };
        let mut sel_rows: Vec<Option<&mut [AccPx]>> = match &mut sel {
            Some(s) => s.acc.px.chunks_mut(r.w).map(Some).collect(),
            None => (0..rows_in_band).map(|_| None).collect(),
        };
        let maps_ref = maps.as_ref();
        let t_warp = std::time::Instant::now();
        all_rows.par_iter_mut().zip(sel_rows.par_iter_mut()).enumerate().for_each(|(yo, (arow, srow))| {
            let y = r.y + y_off + yo;
            for xo in 0..r.w {
                let x = r.x + xo;
                let (v, s) = if ident {
                    let s = sat[y * w + x].max(sat[y * w + (x + 1).min(w - 1)]).max(sat[(y + 1).min(h - 1) * w + x]).max(sat[(y + 1).min(h - 1) * w + (x + 1).min(w - 1)]);
                    (rgb[y * w + x], s)
                } else {
                    let (xs, ys) = warp.apply(x as f64, y as f64, cx, cy, cs);
                    // (the last sensor column and row count as covered: a frame that lies exactly on the reference
                    // contributes there too – before, they came from the reference alone, a 1 px line where it clips)
                    if !(xs >= 0.0 && ys >= 0.0 && xs <= (w - 1) as f64 && ys <= (h - 1) as f64) {
                        continue;
                    }
                    let (x0, y0) = (xs as usize, ys as usize);
                    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
                    let (fx, fy) = (xs - x0 as f64, ys - y0 as f64);
                    let p00 = rgb[y0 * w + x0];
                    let p10 = rgb[y0 * w + x1];
                    let p01 = rgb[y1 * w + x0];
                    let p11 = rgb[y1 * w + x1];
                    let mut v = [0f32; 3];
                    if fx < 1e-6 && fy < 1e-6 {
                        v = p00;
                    } else if x0 >= 2 && y0 >= 2 && x0 + 3 < w && y0 + 3 < h {
                        let (wx, wy) = (lz.w(fx), lz.w(fy));
                        for (jy, wyv) in wy.iter().enumerate() {
                            let row = (y0 + jy - 2) * w;
                            let mut s = [0f32; 3];
                            for (jx, wxv) in wx.iter().enumerate() {
                                let p = rgb[row + x0 + jx - 2];
                                s[0] += wxv * p[0];
                                s[1] += wxv * p[1];
                                s[2] += wxv * p[2];
                            }
                            for c in 0..3 {
                                v[c] += wyv * s[c];
                            }
                        }
                        // de-ringing: clamp to the range of the 4 nearest samples
                        for c in 0..3 {
                            let lo = p00[c].min(p10[c]).min(p01[c]).min(p11[c]);
                            let hi = p00[c].max(p10[c]).max(p01[c]).max(p11[c]);
                            v[c] = v[c].clamp(lo, hi);
                        }
                    } else {
                        let (fx, fy) = (fx as f32, fy as f32);
                        for c in 0..3 {
                            v[c] = (p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy;
                        }
                    }
                    let s = sat[y0 * w + x0].max(sat[y0 * w + x1]).max(sat[y1 * w + x0]).max(sat[y1 * w + x1]);
                    (v, s)
                };
                let x_ref = v.map(|c| c / k);
                let hh = ramp(s);
                let all_px = arow.as_deref_mut().map(|r| &mut r[xo]);
                let sel_px = srow.as_deref_mut().zip(maps_ref).map(|(r, m)| {
                    let q = m.cell_at(x, y, m.qf);
                    (&mut r[xo], m.include(i, q) && (ident || !m.is_hole(q)))
                });
                put(all_px, sel_px, x_ref, hh, k, a, b);
            }
        });
        if std::env::var_os("GRAINEO_HDR_PROFILE").is_some() {
            eprintln!("hdr-profile warp-accumulate {:.3}", t_warp.elapsed().as_secs_f64());
        }
        drop(all_rows);
        drop(sel_rows);
        drop(rgb);
        drop(sat);
    }
    // (the pyramid blend of the plain and the selective merge works on the whole image: the bands deliver both)
    let (ox, oy) = (r.x, r.y + y_off);
    crate::profile("combine", || match (acc, sel) {
        (Some(a), Some(s)) => {
            px.extend(a.finish());
            px_sel.extend(s.finish(r.w, ox, oy, fill));
        }
        (None, Some(s)) => px.extend(s.finish(r.w, ox, oy, fill)),
        (Some(a), None) => px.extend(a.finish()),
        (None, None) => {}
    });
    }
    if !px_sel.is_empty() {
        let maps = maps.as_ref().ok_or_else(|| Error::Internal("ghost maps missing".into()))?;
        let mask = ghost::upsample(&maps.mask, maps.qw, maps.qh, maps.qf, r.x, r.y, r.w, r.h);
        px = crate::profile("blend", || ghost::blend(&px, &px_sel, &mask, r.w, r.h));
    }
    ctl.report(1.0);
    let merged = Merged { px, w: r.w, h: r.h, noise: plan.result_noise() };
    crate::profile("output", || deliver(&plan.settings, merged, ctl))
}

/// Quick merge at proxy resolution (per-colour binning of each mosaic instead of a demosaic, bilinear warp on the
/// proxy grid), downscaled to `max_edge`. Needs the mosaics (call before [`full`]).
pub fn preview(plan: &Plan, max_edge: usize, ctl: &Control) -> Result<Merged, Error> {
    let ri = plan.reference;
    let fr = &plan.frames[ri];
    let pf = fr.pf;
    let (pw, ph) = (fr.g.w, fr.g.h);
    let wb = plan.wb;
    let r = plan.crop;
    // crop in proxy coordinates
    let (qx, qy) = (r.x / pf, r.y / pf);
    if qx >= pw || qy >= ph {
        return Err(Error::Internal("crop outside the sensor".into()));
    }
    let (qw, qh) = ((r.w / pf).min(pw - qx), (r.h / pf).min(ph - qy));
    if qw == 0 || qh == 0 {
        return Err(Error::Internal("crop too small to merge".into()));
    }
    let ks = &plan.k;
    let order = bright_to_dark(ks);
    let maps = ghost_maps(plan);
    let fine = maps.as_ref().is_some_and(|m| m.level == ghost::Level::Fine);
    let mut acc = (!fine).then(|| Acc::new(qw * qh));
    let mut sel = maps.as_ref().map(|m| Select { maps: m, acc: Acc::new(qw * qh), cell: 2 });
    let fill = fr.clip[1] * wb[1];
    for &i in &order {
        ctl.check()?;
        let f = &plan.frames[i];
        if f.mosaic.is_empty() && f.planar.as_ref().is_none_or(|p| p.is_empty()) {
            return Err(Error::Internal("mosaic already consumed".into()));
        }
        // binned RGB per proxy block, white-balanced
        let mut rgb = vec![[0f32; 3]; pw * ph];
        rgb.par_chunks_mut(pw).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let mut s = [0f32; 3];
                let mut n = [0f32; 3];
                for dy in 0..pf {
                    for dx in 0..pf {
                        let (xx, yy) = (pf * x + dx, pf * y + dy);
                        if let Some(pl) = &f.planar {
                            let p = pl[yy * f.w + xx];
                            for c in 0..3 {
                                s[c] += p[c];
                                n[c] += 1.0;
                            }
                        } else {
                            let c = f.color_at(xx, yy);
                            s[c] += f.mosaic[yy * f.w + xx];
                            n[c] += 1.0;
                        }
                    }
                }
                *o = std::array::from_fn(|c| s[c] / n[c].max(1.0) * wb[c]);
            }
        });
        let k = ks[i] as f32;
        let (a, b) = plan.noise[i];
        let warp = plan.warps[i];
        let ident = i == ri;
        let mut all_rows: Vec<Option<&mut [AccPx]>> = match &mut acc {
            Some(a) => a.px.chunks_mut(qw).map(Some).collect(),
            None => (0..qh).map(|_| None).collect(),
        };
        let mut sel_rows: Vec<Option<&mut [AccPx]>> = match &mut sel {
            Some(s) => s.acc.px.chunks_mut(qw).map(Some).collect(),
            None => (0..qh).map(|_| None).collect(),
        };
        let maps_ref = maps.as_ref();
        all_rows.par_iter_mut().zip(sel_rows.par_iter_mut()).enumerate().for_each(|(yo, (arow, srow))| {
            let y = qy + yo;
            for xo in 0..qw {
                let x = qx + xo;
                let (v, s) = if ident {
                    (rgb[y * pw + x], f.mx.at(x, y))
                } else {
                    let (xs, ys) = proxy_map(&warp, x as f64, y as f64, pw, ph);
                    // (the last column and row included, as at full resolution)
                    let Some(s) = f.mx.sample_max_edge(xs, ys) else { continue };
                    let (x0, y0) = (xs as usize, ys as usize);
                    let (x1, y1) = ((x0 + 1).min(pw - 1), (y0 + 1).min(ph - 1));
                    let (fx, fy) = ((xs - x0 as f64) as f32, (ys - y0 as f64) as f32);
                    let p = |xx: usize, yy: usize| rgb[yy * pw + xx];
                    let (p00, p10, p01, p11) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
                    let v = std::array::from_fn(|c| (p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy);
                    (v, s)
                };
                let x_ref = v.map(|c| c / k);
                let hh = ramp(s);
                let all_px = arow.as_deref_mut().map(|r| &mut r[xo]);
                let sel_px = srow.as_deref_mut().zip(maps_ref).map(|(r, m)| {
                    let q = m.cell_at(x, y, 2);
                    (&mut r[xo], m.include(i, q) && (ident || !m.is_hole(q)))
                });
                put(all_px, sel_px, x_ref, hh, k, a, b);
            }
        });
    }
    let px = combine(acc, sel, qw, qh, qx, qy, fill);
    let f = (max_edge as f32 / qw.max(qh) as f32).min(1.0);
    let (nw, nh) = (((qw as f32 * f).round() as usize).max(1), ((qh as f32 * f).round() as usize).max(1));
    let px = raw::development::downscale(&px, qw, qh, nw, nh);
    deliver(&plan.settings, Merged { px, w: nw, h: nh, noise: plan.result_noise() }, ctl)
}

/// Every frame on its own at the size of [`preview`] (same proxy grid, warp and downscale): warped onto the reference
/// and divided by its exposure factor, so that a frame and the merge line up pixel for pixel. Where a frame does not
/// cover the reference the reference itself is used. For local areas ([`crate::regions`]) and for looking at a single
/// exposure.
pub fn preview_frames(plan: &Plan, max_edge: usize, ctl: &Control) -> Result<Vec<Vec<[f32; 3]>>, Error> {
    let ri = plan.reference;
    let fr = &plan.frames[ri];
    let pf = fr.pf;
    let (pw, ph) = (fr.g.w, fr.g.h);
    let wb = plan.wb;
    let r = plan.crop;
    let (qx, qy) = (r.x / pf, r.y / pf);
    let (qw, qh) = ((r.w / pf).min(pw - qx), (r.h / pf).min(ph - qy));
    let f = (max_edge as f32 / qw.max(qh) as f32).min(1.0);
    let (nw, nh) = (((qw as f32 * f).round() as usize).max(1), ((qh as f32 * f).round() as usize).max(1));
    let binned = |fm: &Frame| -> Vec<[f32; 3]> {
        let mut rgb = vec![[0f32; 3]; pw * ph];
        rgb.par_chunks_mut(pw).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let (mut s, mut n) = ([0f32; 3], [0f32; 3]);
                for dy in 0..pf {
                    for dx in 0..pf {
                        let (xx, yy) = (pf * x + dx, pf * y + dy);
                        if let Some(pl) = &fm.planar {
                            let p = pl[yy * fm.w + xx];
                            for c in 0..3 {
                                s[c] += p[c];
                                n[c] += 1.0;
                            }
                        } else {
                            let c = fm.color_at(xx, yy);
                            s[c] += fm.mosaic[yy * fm.w + xx];
                            n[c] += 1.0;
                        }
                    }
                }
                *o = std::array::from_fn(|c| s[c] / n[c].max(1.0) * wb[c]);
            }
        });
        rgb
    };
    let reference = binned(fr);
    let mut out = Vec::with_capacity(plan.frames.len());
    for (i, fm) in plan.frames.iter().enumerate() {
        ctl.check()?;
        if fm.mosaic.is_empty() && fm.planar.as_ref().is_none_or(|p| p.is_empty()) {
            return Err(Error::Internal("mosaic already consumed".into()));
        }
        let own;
        let rgb = if i == ri {
            &reference
        } else {
            own = binned(fm);
            &own
        };
        let k = plan.k[i] as f32;
        let warp = plan.warps[i];
        let mut px = vec![[0f32; 3]; qw * qh];
        px.par_chunks_mut(qw).enumerate().for_each(|(yo, row)| {
            let y = qy + yo;
            for (xo, o) in row.iter_mut().enumerate() {
                let x = qx + xo;
                if i == ri {
                    *o = rgb[y * pw + x];
                    continue;
                }
                let (xs, ys) = proxy_map(&warp, x as f64, y as f64, pw, ph);
                if !(xs >= 0.0 && ys >= 0.0 && xs <= (pw - 1) as f64 && ys <= (ph - 1) as f64) {
                    *o = reference[y * pw + x];
                    continue;
                }
                let (x0, y0) = (xs as usize, ys as usize);
                let (x1, y1) = ((x0 + 1).min(pw - 1), (y0 + 1).min(ph - 1));
                let (fx, fy) = ((xs - x0 as f64) as f32, (ys - y0 as f64) as f32);
                let p = |xx: usize, yy: usize| rgb[yy * pw + xx];
                let (p00, p10, p01, p11) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
                *o = std::array::from_fn(|c| ((p00[c] * (1.0 - fx) + p10[c] * fx) * (1.0 - fy) + (p01[c] * (1.0 - fx) + p11[c] * fx) * fy) / k);
            }
        });
        out.push(raw::development::downscale(&px, qw, qh, nw, nh));
    }
    Ok(out)
}

#[cfg(test)]
mod acc_tests {
    use super::*;

    /// The weight of a nearly saturated bright frame has no jump at the very end of the saturation ramp: just below
    /// the point where the frame becomes the pilot it weighs about as little as just above it (before, it counted as a
    /// black pixel there and took most of the weight).
    #[test]
    fn no_weight_jump_at_the_end_of_the_ramp() {
        let (a, b) = (1e-4f32, 1e-7f32);
        // bright frame (k = 4) nearly clipped at 0.2 reference units, dark frame (k = 1) at the same radiance
        let share = |hh: f32| {
            let mut p = AccPx::EMPTY;
            p.add([0.2; 3], hh, 4.0, a, b);
            let bright = p.ws;
            p.add([0.2; 3], 1.0, 1.0, a, b);
            bright / p.ws
        };
        let (below, above) = (share(0.0015), share(0.0025));
        assert!(below < 0.02 && above < 0.02, "{below} {above}");
        assert!((below / above - 0.6).abs() < 0.1, "proportional to the ramp: {below} {above}");
        // the pilot rule itself is unchanged: a frame below half an 8-bit step is no pilot for the next frame
        let mut p = AccPx::EMPTY;
        p.add([0.2; 3], 0.0015, 4.0, a, b);
        assert!(p.pilot.is_nan());
        p.add([0.3; 3], 1.0, 1.0, a, b);
        assert_eq!(p.pilot, 0.3);
    }
}
