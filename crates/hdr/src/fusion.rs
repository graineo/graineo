//! Exposure fusion of the merged radiance image (Mertens, Kautz & Van Reeth 2007; Laplacian pyramid after Burt &
//! Adelson 1983), computed on the CPU.
//!
//! The ghost-free linear merge is turned into a series of synthetic exposures (the radiance scaled by 2^e and clipped),
//! every exposure gets a per-pixel quality (contrast × saturation × well-exposedness) and the exposures are blended
//! in the Laplacian pyramid, so that no seams or halos appear where the weights change. The result is a natural-looking
//! image with a compressed range (1.0 = white): written back as linear values, it goes through the normal development
//! with a neutral curve. The pyramid blend is a mixing over frequency bands, not a per-pixel shader mix, and it runs
//! on the CPU.
//!
//! Memory: the accumulated pyramid (≈ 1.33 images) plus the pyramids of the exposure being processed.

use crate::merge::Merged;
use crate::{Control, Error};
use rayon::prelude::*;

/// How the merged radiance is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// linear radiance, values above 1 keep the highlight range (the default)
    #[default]
    Linear,
    /// exposure fusion: natural look, range compressed to 0 … 1
    Fusion,
}

impl Output {
    pub fn is_default(&self) -> bool {
        *self == Output::Linear
    }
}

/// Gamma of the synthetic exposures (the quality measures act on display-referred values).
const GAMMA: f32 = 2.2;
/// Width of the well-exposedness curve.
const SIGMA: f32 = 0.2;
/// Brightest synthetic exposure relative to the reference white (EV).
const MAX_LIFT: f32 = 2.0;
/// Most synthetic exposures.
const MAX_EXPOSURES: usize = 7;

/// One plane of a pyramid.
#[derive(Clone)]
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn new(w: usize, h: usize) -> Plane {
        Plane { w, h, v: vec![0.0; w * h] }
    }
}

const K5: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * n - 2 - i;
    }
    i.clamp(0, n - 1) as usize
}

/// Blur with the 5-tap kernel and keep every second pixel.
fn reduce(p: &Plane) -> Plane {
    let (w2, h2) = (p.w.div_ceil(2), p.h.div_ceil(2));
    // horizontal pass on all rows, subsampled in x
    let mut tmp = vec![0f32; w2 * p.h];
    tmp.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        let src = &p.v[y * p.w..(y + 1) * p.w];
        for (x2, o) in row.iter_mut().enumerate() {
            let x = (x2 * 2) as isize;
            let mut s = 0.0;
            for (k, wk) in K5.iter().enumerate() {
                s += wk * src[mirror(x + k as isize - 2, p.w)];
            }
            *o = s;
        }
    });
    let mut out = Plane::new(w2, h2);
    out.v.par_chunks_mut(w2).enumerate().for_each(|(y2, row)| {
        let y = (y2 * 2) as isize;
        for (x, o) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for (k, wk) in K5.iter().enumerate() {
                s += wk * tmp[mirror(y + k as isize - 2, p.h) * w2 + x];
            }
            *o = s;
        }
    });
    out
}

/// Interpolating upsample to `tw × th` (linear on the doubled grid).
fn expand(p: &Plane, tw: usize, th: usize) -> Plane {
    let mut out = Plane::new(tw, th);
    out.v.par_chunks_mut(tw).enumerate().for_each(|(y, row)| {
        let (y0, fy) = (y / 2, if y % 2 == 1 { 0.5 } else { 0.0 });
        let (ya, yb) = (y0.min(p.h - 1), (y0 + 1).min(p.h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let (x0, fx) = (x / 2, if x % 2 == 1 { 0.5 } else { 0.0 });
            let (xa, xb) = (x0.min(p.w - 1), (x0 + 1).min(p.w - 1));
            let top = p.v[ya * p.w + xa] * (1.0 - fx) + p.v[ya * p.w + xb] * fx;
            let bot = p.v[yb * p.w + xa] * (1.0 - fx) + p.v[yb * p.w + xb] * fx;
            *o = top * (1.0 - fy) + bot * fy;
        }
    });
    out
}

fn gaussian_pyramid(base: Plane, levels: usize) -> Vec<Plane> {
    let mut v = vec![base];
    for _ in 1..levels {
        let next = reduce(v.last().unwrap());
        v.push(next);
    }
    v
}

/// Laplacian pyramid from a Gaussian one (the top level stays the Gaussian level).
fn laplacian_pyramid(g: Vec<Plane>) -> Vec<Plane> {
    let n = g.len();
    let mut out = Vec::with_capacity(n);
    for l in 0..n - 1 {
        let up = expand(&g[l + 1], g[l].w, g[l].h);
        let mut d = g[l].clone();
        d.v.par_iter_mut().zip(up.v.par_iter()).for_each(|(a, b)| *a -= b);
        out.push(d);
    }
    out.push(g[n - 1].clone());
    out
}

/// Synthetic exposure `e` (display-referred 0 … 1, planar RGB).
fn exposure_image(px: &[[f32; 3]], w: usize, h: usize, e: f32) -> [Plane; 3] {
    let s = e.exp2();
    let mut planes = [Plane::new(w, h), Plane::new(w, h), Plane::new(w, h)];
    let inv = 1.0 / GAMMA;
    let [r, g, b] = &mut planes;
    r.v.par_iter_mut().zip(g.v.par_iter_mut()).zip(b.v.par_iter_mut()).zip(px.par_iter()).for_each(|(((r, g), b), p)| {
        *r = (p[0] * s).clamp(0.0, 1.0).powf(inv);
        *g = (p[1] * s).clamp(0.0, 1.0).powf(inv);
        *b = (p[2] * s).clamp(0.0, 1.0).powf(inv);
    });
    planes
}

/// Quality of every pixel of one exposure: contrast (|Laplacian| of the grey image) × saturation (spread of the three
/// channels) × well-exposedness (closeness of every channel to 0.5).
fn quality(img: &[Plane; 3]) -> Plane {
    let (w, h) = (img[0].w, img[0].h);
    let grey: Vec<f32> = (0..w * h).into_par_iter().map(|i| 0.2126 * img[0].v[i] + 0.7152 * img[1].v[i] + 0.0722 * img[2].v[i]).collect();
    let mut q = Plane::new(w, h);
    q.v.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let c = (grey[ya * w + x] + grey[yb * w + x] + grey[y * w + xa] + grey[y * w + xb] - 4.0 * grey[y * w + x]).abs();
            let i = y * w + x;
            let (r, g, b) = (img[0].v[i], img[1].v[i], img[2].v[i]);
            let m = (r + g + b) / 3.0;
            let s = (((r - m).powi(2) + (g - m).powi(2) + (b - m).powi(2)) / 3.0).sqrt();
            let well = |v: f32| (-(v - 0.5).powi(2) / (2.0 * SIGMA * SIGMA)).exp();
            *o = (c + 1e-3) * (s + 1e-3) * well(r) * well(g) * well(b) + 1e-9;
        }
    });
    q
}

/// The exposure shifts (EV) of the synthetic series: from the one that brings the brightest highlights into range to
/// the one that lifts the shadows to a usable level.
fn exposures(px: &[[f32; 3]]) -> Vec<f32> {
    let mut lum: Vec<f32> = px.iter().step_by(13).map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).filter(|v| *v > 0.0).collect();
    if lum.len() < 16 {
        return vec![0.0];
    }
    lum.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f32| lum[((lum.len() - 1) as f32 * q) as usize];
    let (lo, hi) = (at(0.02).max(1e-5), at(0.999).max(1e-3));
    // darkest synthetic exposure: the 99.9 % highlight at 1.0; brightest: the 2 % shadow at about 0.25
    let e_min = -hi.log2();
    // (at most 2 EV above the reference white: lifting further makes the whole image bright and flat)
    let e_max = (0.25 / lo).log2().min(MAX_LIFT).max(e_min);
    let n = ((e_max - e_min).ceil() as usize + 1).clamp(1, MAX_EXPOSURES);
    if n == 1 {
        return vec![(e_min + e_max) / 2.0];
    }
    (0..n).map(|j| e_min + (e_max - e_min) * j as f32 / (n - 1) as f32).collect()
}

/// Exposure fusion of a merged radiance image.
pub fn fuse(m: &Merged, ctl: &Control) -> Result<Merged, Error> {
    let (w, h) = (m.w, m.h);
    if w < 8 || h < 8 {
        return Ok(Merged { px: m.px.clone(), w, h, noise: m.noise });
    }
    let es = exposures(&m.px);
    let levels = ((w.min(h) as f32).log2().floor() as usize).saturating_sub(2).clamp(1, 9);
    // pass 1: sum of the qualities
    let mut sum = Plane::new(w, h);
    for (j, &e) in es.iter().enumerate() {
        ctl.check()?;
        ctl.report(0.3 * j as f32 / es.len() as f32);
        let q = quality(&exposure_image(&m.px, w, h, e));
        sum.v.par_iter_mut().zip(q.v.par_iter()).for_each(|(a, b)| *a += b);
    }
    // pass 2: blend the Laplacian pyramids with the Gaussian pyramids of the normalised weights
    let mut acc: Option<[Vec<Plane>; 3]> = None;
    for (j, &e) in es.iter().enumerate() {
        ctl.check()?;
        ctl.report(0.3 + 0.7 * j as f32 / es.len() as f32);
        let img = exposure_image(&m.px, w, h, e);
        let mut q = quality(&img);
        q.v.par_iter_mut().zip(sum.v.par_iter()).for_each(|(a, s)| *a /= s);
        let gw = gaussian_pyramid(q, levels);
        let [r, g, b] = img;
        let lp = [laplacian_pyramid(gaussian_pyramid(r, levels)), laplacian_pyramid(gaussian_pyramid(g, levels)), laplacian_pyramid(gaussian_pyramid(b, levels))];
        match &mut acc {
            None => {
                let mut a = lp;
                for ch in a.iter_mut() {
                    for (pl, wl) in ch.iter_mut().zip(&gw) {
                        pl.v.par_iter_mut().zip(wl.v.par_iter()).for_each(|(x, wv)| *x *= wv);
                    }
                }
                acc = Some(a);
            }
            Some(a) => {
                for (ch, lch) in a.iter_mut().zip(&lp) {
                    for ((pl, lv), wl) in ch.iter_mut().zip(lch).zip(&gw) {
                        pl.v.par_iter_mut().zip(lv.v.par_iter()).zip(wl.v.par_iter()).for_each(|((x, l), wv)| *x += l * wv);
                    }
                }
            }
        }
    }
    let acc = acc.ok_or_else(|| Error::Internal("no exposures".into()))?;
    // collapse
    let mut planes: Vec<Plane> = Vec::with_capacity(3);
    for ch in acc {
        let mut cur = ch[ch.len() - 1].clone();
        for l in (0..ch.len() - 1).rev() {
            let mut up = expand(&cur, ch[l].w, ch[l].h);
            up.v.par_iter_mut().zip(ch[l].v.par_iter()).for_each(|(a, b)| *a += b);
            cur = up;
        }
        planes.push(cur);
    }
    let mut px = vec![[0f32; 3]; w * h];
    px.par_iter_mut().enumerate().for_each(|(i, p)| {
        for c in 0..3 {
            p[c] = planes[c].v[i].clamp(0.0, 1.0).powf(GAMMA);
        }
    });
    ctl.report(1.0);
    Ok(Merged { px, w, h, noise: m.noise })
}

/// Strength of the fusion: blends the fused image `fused` with the reference exposure of the merge `m` (its radiance
/// clipped at the reference white), `strength` 0 = the reference exposure, 1 = the full fusion. The blend is one global
/// weight for the whole image, in display-referred values (gamma 2.2, where the fusion itself judges the exposures):
/// mixing linear values would let the bright reference dominate the midtones, so half strength would look nearly like
/// the reference. Per pixel and without any spatial step, it cannot add halos that neither image has.
pub fn blend(m: &Merged, fused: &mut Merged, strength: f32) {
    let s = strength.clamp(0.0, 1.0);
    if s >= 1.0 {
        return;
    }
    let inv = 1.0 / GAMMA;
    fused.px.par_iter_mut().zip(m.px.par_iter()).for_each(|(f, r)| {
        for c in 0..3 {
            let a = r[c].clamp(0.0, 1.0).powf(inv);
            let b = f[c].clamp(0.0, 1.0).powf(inv);
            f[c] = (a + (b - a) * s).powf(GAMMA);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn ctl_run<T>(f: impl FnOnce(&Control) -> T) -> T {
        let cancel = AtomicBool::new(false);
        let progress = |_: f32| {};
        f(&Control { cancel: &cancel, progress: &progress })
    }

    /// A scene with a bright sky (up to 16) and a dark ground (0.01): the fusion brings both into 0 … 1 without
    /// clipping the sky or losing the ground, and keeps the order of brightness.
    #[test]
    fn compresses_a_wide_scene_into_range() {
        let (w, h) = (96, 64);
        let mut px = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let v = if y < h / 2 { 2.0 + 14.0 * t } else { 0.01 + 0.04 * t };
                px[y * w + x] = [v, v * 0.9, v * 0.8];
            }
        }
        let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
        let r = ctl_run(|c| fuse(&m, c)).unwrap();
        let peak = r.px.iter().map(|p| p[1]).fold(0f32, f32::max);
        assert!(peak <= 1.0 + 1e-4, "range {peak}");
        let (sky, ground) = (r.px[8 * w + 48][1], r.px[(h - 8) * w + 48][1]);
        assert!(sky > ground, "{sky} {ground}");
        assert!(ground > 0.01, "shadows are lifted: {ground}");
        assert!(sky < 0.99, "highlights keep some room: {sky}");
    }

    /// Halo check: at a hard edge between a bright and a dark area the result must not overshoot the levels of the two
    /// sides (a halo is the overshoot next to the edge); measured in a 6-pixel band on both sides.
    #[test]
    fn no_halo_at_a_hard_edge() {
        let (w, h) = (128, 96);
        let mut px = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 8.0 } else { 0.02 };
                px[y * w + x] = [v, v, v];
            }
        }
        let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
        let r = ctl_run(|c| fuse(&m, c)).unwrap();
        let row = |x: usize| r.px[(h / 2) * w + x][1];
        let (far_bright, far_dark) = (row(8), row(w - 9));
        let (lo, hi) = (far_bright.min(far_dark), far_bright.max(far_dark));
        let mut worst = 0f32;
        for x in (w / 2 - 6)..(w / 2 + 6) {
            worst = worst.max(lo - row(x)).max(row(x) - hi);
        }
        // overshoot relative to the step height
        assert!(worst / (hi - lo).max(1e-6) < 0.05, "halo {worst} for a step of {}", hi - lo);
    }

    /// Halo measure over hard edges of 2 … 14 EV (bright side 1 … 16, dark side down to 1/1000): excursion beyond the
    /// two plateau levels in a 14-pixel band on both sides, in units of the step in the output (target ≤ 0.05, the
    /// acceptance limit for fusion halos). Also a thin dark bar in a bright window (the
    /// classic halo scene): the bar's neighbours may not get brighter than the window far away.
    #[test]
    fn halo_over_edge_contrasts_and_a_window_bar() {
        let (w, h) = (192, 96);
        let mut worst = 0f32;
        for ev in [2.0f32, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0] {
            let bright = 4.0f32;
            let dark = bright / ev.exp2();
            let mut px = vec![[0f32; 3]; w * h];
            for y in 0..h {
                for x in 0..w {
                    let v = if x < w / 2 { bright } else { dark };
                    px[y * w + x] = [v, v, v];
                }
            }
            let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
            let r = ctl_run(|c| fuse(&m, c)).unwrap();
            let row = |x: usize| r.px[(h / 2) * w + x][1];
            let (a, b) = (row(w / 2 - 20), row(w / 2 + 20));
            let (lo, hi) = (a.min(b), a.max(b));
            let mut over = 0f32;
            for x in (w / 2 - 14)..(w / 2 + 14) {
                over = over.max(lo - row(x)).max(row(x) - hi);
            }
            let rel = over / (hi - lo).max(1e-6);
            eprintln!("halo edge {ev:>4} EV: step {:.3}, overshoot {:.4} of the step", hi - lo, rel);
            worst = worst.max(rel);
        }
        // window with a 3-pixel dark bar: the glow beside the bar (window level far away = the plateau)
        let mut px = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let v = if (w / 2 - 1..w / 2 + 2).contains(&x) { 0.02 } else if y < h / 2 { 8.0 } else { 0.05 };
                px[y * w + x] = [v, v, v];
            }
        }
        let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
        let r = ctl_run(|c| fuse(&m, c)).unwrap();
        let y = h / 4;
        let (far, bar) = (r.px[y * w + 10][1], r.px[y * w + w / 2][1]);
        let glow = (3..9).map(|d| r.px[y * w + w / 2 - 1 - d][1] - far).fold(f32::MIN, f32::max);
        eprintln!("halo window bar: window {far:.3}, bar {bar:.3}, brightest neighbour above the window {glow:+.4}");
        assert!(worst < 0.05, "edge overshoot {worst}");
        assert!(glow / (far - bar).max(1e-6) < 0.05, "glow beside the bar {glow}");
    }

    /// Strength 0 gives the reference exposure (clipped at white), 1 the fusion unchanged, half lies in between in
    /// display values; the order of brightness stays.
    #[test]
    fn strength_blends_reference_and_fusion() {
        let (w, h) = (96, 64);
        let mut px = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let v = if y < h / 2 { 2.0 + 14.0 * t } else { 0.01 + 0.04 * t };
                px[y * w + x] = [v, v * 0.9, v * 0.8];
            }
        }
        let m = Merged { px, w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
        let full = ctl_run(|c| fuse(&m, c)).unwrap();
        let at = |s: f32| {
            let mut f = Merged { px: full.px.clone(), w, h, noise: m.noise };
            blend(&m, &mut f, s);
            f
        };
        let (zero, half, one) = (at(0.0), at(0.5), at(1.0));
        assert_eq!(one.px, full.px, "100 % is the fusion");
        for (z, r) in zero.px.iter().zip(&m.px) {
            for c in 0..3 {
                assert!((z[c] - r[c].min(1.0)).abs() < 1e-5, "0 % is the reference exposure");
            }
        }
        let i = (h - 8) * w + 48;
        let g = |p: &[f32; 3]| p[1].powf(1.0 / GAMMA);
        assert!((g(&half.px[i]) - (g(&zero.px[i]) + g(&full.px[i])) / 2.0).abs() < 1e-4, "half way in display values");
        assert!(half.px[8 * w + 48][1] > half.px[i][1], "sky stays brighter than the ground");
    }

    /// A flat grey image stays flat (no pyramid artefacts at the borders).
    #[test]
    fn flat_image_stays_flat() {
        let (w, h) = (40, 33);
        let m = Merged { px: vec![[0.18; 3]; w * h], w, h, noise: raw::noise::NoiseModel { a: [0.0; 3], b: [0.0; 3] } };
        let r = ctl_run(|c| fuse(&m, c)).unwrap();
        let (lo, hi) = r.px.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        assert!(hi - lo < 1e-3, "{lo} {hi}");
    }
}
