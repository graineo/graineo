//! Ghost handling for hand-held or moving scenes: which frame counts where.
//!
//! Method (literature up to 2009: Khan et al. 2006, Grosch 2006, Gallo et al. 2009; no tile merge with blend and
//! conflict maps, no motion weight inside the mixing formula):
//!
//! 1. **Consistency maps** on a coarse grid (cells of 2 × 2 proxy pixels, ≈ ¼ resolution): every frame, divided by its
//!    exposure factor and warped onto the reference, is compared with the reference in log2 where both are unsaturated
//!    and above the noise floor. The threshold of a frame is `4 · 1.4826 · MAD` of its own differences around their
//!    median (at least 0.05 EV): the image itself says how large the normal noise and misalignment are.
//! 2. **Reference chain with one-sided fallback:** where the reference is saturated, the next darker frame that is at
//!    least as bright as what saturates the reference is the anchor; further darker frames must agree with it. Where
//!    no consistent anchor exists the cell is a *hole*: the (clipped) reference is kept, never a darker frame.
//! 3. **Mask:** the cells with motion are opened (3 × 3), dilated, smoothed. Inside the mask only the reference (and
//!    the consistent chain where it is saturated) is merged; the two merges are blended with a Laplacian pyramid in
//!    log2 ([`blend`]), so the transition has no seam. Level *Fine* skips the mask: it excludes every frame that
//!    disagrees at a cell, with its own dilation, and does not blend.
//! 4. **Paint:** strokes of the user ([`Stroke`]) force one source frame at the painted cells.

use crate::exposure::proxy_map;
use crate::img::{max_filter, Img};
use crate::Plan;
use rayon::prelude::*;

/// Strength of the ghost handling (setting of the merge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// every frame everywhere (stationary scenes: no loss of noise gain)
    Off,
    /// reference + chain inside the (opened) motion mask, blended with a pyramid
    #[default]
    Standard,
    /// exclude per frame where it disagrees, no opening, no blend
    Fine,
}

/// One brush stroke of "Quelle malen": the cells under it take their pixels from one source frame (or, erasing, go
/// back to the automatic choice). Coordinates are normalised to the crop of the reference in sensor orientation
/// (0 … 1 across the crop width and height); `radius` is a fraction of the longer crop edge.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stroke {
    /// index of the source frame (position in the source list of the merge)
    pub frame: usize,
    #[serde(default)]
    pub erase: bool,
    pub radius: f32,
    pub points: Vec<[f32; 2]>,
}

/// Value of `paint` for "automatic".
pub const NO_PAINT: u8 = 255;

/// Result of the detection on the consistency grid.
pub struct Maps {
    pub level: Level,
    pub reference: usize,
    pub qw: usize,
    pub qh: usize,
    /// sensor pixels per cell
    pub qf: usize,
    /// cleaned motion (0 / 1) as shown to the user
    pub motion: Vec<f32>,
    /// soft blend mask 0 … 1 (Standard; includes painted cells)
    pub mask: Vec<f32>,
    /// per frame: consistent darker chain where the reference is saturated (1 = may be used)
    pub chain: Vec<Vec<u8>>,
    /// per frame: disagrees with the reference here, dilated (Fine)
    pub excl: Vec<Vec<u8>>,
    /// reference saturated and no consistent anchor
    pub hole: Vec<u8>,
    pub refsat: Vec<u8>,
    /// forced source frame per cell or [`NO_PAINT`]
    pub paint: Vec<u8>,
    /// threshold in EV per frame
    pub tau: Vec<f32>,
    /// fraction of cells with motion / holes
    pub area: f32,
    pub holes: f32,
}

impl Maps {
    /// Is there anything to do (otherwise the plain merge gives the same result)?
    pub fn active(&self) -> bool {
        self.level != Level::Off && (self.area > 0.0 || self.holes > 0.0 || self.paint.iter().any(|p| *p != NO_PAINT))
    }

    /// May frame `i` contribute at cell `q` in the selective merge?
    #[inline]
    pub fn include(&self, i: usize, q: usize) -> bool {
        let p = self.paint[q];
        if p != NO_PAINT {
            return i == p as usize;
        }
        match self.level {
            Level::Fine => i == self.reference || self.excl[i][q] == 0,
            _ => i == self.reference || (self.refsat[q] == 1 && self.chain[i][q] == 1),
        }
    }

    /// Cell of pixel (x, y) of an image in which a cell is `cell` pixels wide.
    #[inline]
    pub fn cell_at(&self, x: usize, y: usize, cell: usize) -> usize {
        (y / cell).min(self.qh - 1) * self.qw + (x / cell).min(self.qw - 1)
    }

    /// The reference is saturated at `q` and no consistent anchor exists.
    #[inline]
    pub fn is_hole(&self, q: usize) -> bool {
        self.hole[q] == 1 && self.paint[q] == NO_PAINT
    }
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let m = v.len() / 2;
    v.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
    v[m]
}

fn min_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let neg: Vec<f32> = src.iter().map(|v| -v).collect();
    max_filter(&neg, w, h, r).into_iter().map(|v| -v).collect()
}

/// 3 × 3 box mean.
fn box3(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (mut s, mut n) = (0.0, 0.0);
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    s += src[yy * w + xx];
                    n += 1.0;
                }
            }
            *o = s / n;
        }
    });
    out
}

/// Rasterise the strokes onto the cell grid (`NO_PAINT` where nothing is painted).
pub fn rasterize(strokes: &[Stroke], qw: usize, qh: usize, qf: usize, crop: raw::Region, n_frames: usize) -> Vec<u8> {
    let mut out = vec![NO_PAINT; qw * qh];
    let (cw, ch) = (crop.w.max(1) as f32, crop.h.max(1) as f32);
    let edge = cw.max(ch);
    for s in strokes {
        if s.frame >= n_frames || s.points.is_empty() {
            continue;
        }
        let value = if s.erase { NO_PAINT } else { s.frame as u8 };
        let rad = (s.radius.max(0.0) * edge).max(1.0);
        // sensor pixel coordinates of the points
        let pts: Vec<[f32; 2]> = s.points.iter().map(|p| [crop.x as f32 + p[0] * cw, crop.y as f32 + p[1] * ch]).collect();
        for k in 0..pts.len() {
            let (a, b) = (pts[k], pts[(k + 1).min(pts.len() - 1)]);
            let (x0, x1) = (a[0].min(b[0]) - rad, a[0].max(b[0]) + rad);
            let (y0, y1) = (a[1].min(b[1]) - rad, a[1].max(b[1]) + rad);
            let (qx0, qx1) = (((x0 / qf as f32).floor().max(0.0)) as usize, ((x1 / qf as f32).ceil().max(0.0) as usize).min(qw.saturating_sub(1)));
            let (qy0, qy1) = (((y0 / qf as f32).floor().max(0.0)) as usize, ((y1 / qf as f32).ceil().max(0.0) as usize).min(qh.saturating_sub(1)));
            for qy in qy0..=qy1 {
                for qx in qx0..=qx1 {
                    let c = [(qx as f32 + 0.5) * qf as f32, (qy as f32 + 0.5) * qf as f32];
                    // distance of the cell centre to the segment a–b
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let l2 = dx * dx + dy * dy;
                    let t = if l2 > 0.0 { (((c[0] - a[0]) * dx + (c[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
                    let (px, py) = (a[0] + t * dx - c[0], a[1] + t * dy - c[1]);
                    if px * px + py * py <= rad * rad {
                        out[qy * qw + qx] = value;
                    }
                }
            }
        }
    }
    out
}

/// Detect motion for a plan (frames with proxies, warps and exposure factors) with the settings of the plan.
pub fn detect(plan: &Plan) -> Maps {
    let level = plan.settings.ghost;
    let ri = plan.reference;
    let fr = &plan.frames[ri];
    let (pw, ph) = (fr.g.w, fr.g.h);
    let (qw, qh) = ((pw / 2).max(1), (ph / 2).max(1));
    let qf = 2 * fr.pf;
    let n = plan.frames.len();
    let ks = &plan.k;
    // below this raw level (of the frame itself) a cell is not tested and merged normally (measured on the bench:
    // 0.003 gives the lowest ghost residue; a higher floor misses moving objects in the shadows)
    let floor = 0.003f32;
    // value in reference units and maximum saturation ratio per cell and frame
    let cells: Vec<(Vec<f32>, Vec<f32>)> = (0..n)
        .map(|i| {
            let f = &plan.frames[i];
            let k = ks[i] as f32;
            let warp = plan.warps[i];
            let r: Vec<(f32, f32)> = (0..qw * qh)
                .into_par_iter()
                .map(|q| {
                    let (qx, qy) = (q % qw, q / qw);
                    let (mut s, mut m, mut cnt) = (0f32, 0f32, 0);
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let (x, y) = ((2 * qx + dx) as f64, (2 * qy + dy) as f64);
                            let (xs, ys) = if i == ri { (x, y) } else { proxy_map(&warp, x, y, pw, ph) };
                            if let (Some(gv), Some(mv)) = (f.g.sample(xs, ys), f.mx.sample_max(xs, ys)) {
                                s += gv;
                                m = m.max(mv);
                                cnt += 1;
                            }
                        }
                    }
                    if cnt == 0 {
                        (f32::NAN, 2.0)
                    } else {
                        (s / cnt as f32 / k, m)
                    }
                })
                .collect();
            (r.iter().map(|v| v.0).collect(), r.iter().map(|v| v.1).collect())
        })
        .collect();
    let (ys, ss): (Vec<&Vec<f32>>, Vec<&Vec<f32>>) = (cells.iter().map(|c| &c.0).collect(), cells.iter().map(|c| &c.1).collect());
    let valid = |i: usize, q: usize| ys[i][q].is_finite() && ss[i][q] < 0.9 && ys[i][q] * ks[i] as f32 >= floor;
    // threshold per frame from its own differences with the reference
    let mut tau = vec![1.0f32; n];
    let mut meds = vec![0f32; n];
    for i in 0..n {
        if i == ri {
            continue;
        }
        let d: Vec<f32> = (0..qw * qh).filter(|&q| valid(i, q) && valid(ri, q)).map(|q| (ys[i][q] / ys[ri][q]).log2()).collect();
        if d.len() < 100 {
            continue;
        }
        let mut dv = d.clone();
        let med = median(&mut dv);
        let mut ad: Vec<f32> = d.iter().map(|v| (v - med).abs()).collect();
        let mad = median(&mut ad);
        meds[i] = med;
        tau[i] = (4.0 * 1.4826 * mad).max(0.05);
    }
    let mut motion = vec![0f32; qw * qh];
    let mut cons = vec![vec![1u8; qw * qh]; n];
    let mut chain = vec![vec![0u8; qw * qh]; n];
    let mut hole = vec![0u8; qw * qh];
    let mut darker: Vec<usize> = (0..n).filter(|&i| ks[i] < ks[ri]).collect();
    darker.sort_by(|&a, &b| ks[b].total_cmp(&ks[a]));
    for q in 0..qw * qh {
        chain[ri][q] = 1;
        for &j in &darker {
            chain[j][q] = 1;
        }
        if valid(ri, q) {
            for i in 0..n {
                if i == ri || !valid(i, q) {
                    continue;
                }
                let dv = ((ys[i][q] / ys[ri][q]).log2() - meds[i]).abs();
                if dv > tau[i] {
                    cons[i][q] = 0;
                    chain[i][q] = 0;
                    motion[q] = 1.0;
                }
            }
        } else if ss[ri][q] >= 0.9 {
            // saturated reference: a darker frame must be at least as bright as what saturates the reference
            let mut anchor: Option<usize> = None;
            let mut any_unsat = false;
            for &j in &darker {
                if !ys[j][q].is_finite() || ss[j][q] >= 0.9 {
                    continue;
                }
                any_unsat = true;
                match anchor {
                    None => {
                        let lb = ss[j][q] / ks[j] as f32;
                        if valid(j, q) && lb >= 0.9 * 2f32.powf(-tau[j]) {
                            anchor = Some(j);
                        } else {
                            cons[j][q] = 0;
                            chain[j][q] = 0;
                            motion[q] = 1.0;
                        }
                    }
                    Some(a) => {
                        let dv = (ys[j][q] / ys[a][q]).log2().abs();
                        if !valid(j, q) || dv > tau[j] {
                            cons[j][q] = 0;
                            chain[j][q] = 0;
                            motion[q] = 1.0;
                        }
                    }
                }
            }
            for i in 0..n {
                if ks[i] > ks[ri] && valid(i, q) {
                    cons[i][q] = 0;
                    motion[q] = 1.0;
                }
            }
            if anchor.is_none() && any_unsat {
                hole[q] = 1;
                motion[q] = 1.0;
            }
        }
    }
    let refsat: Vec<u8> = ss[ri].iter().map(|v| (*v >= 0.9) as u8).collect();
    let area = motion.iter().filter(|v| **v > 0.5).count() as f32 / (qw * qh) as f32;
    let holes = hole.iter().filter(|v| **v == 1).count() as f32 / (qw * qh) as f32;
    // Standard: open 3 × 3 (isolated flags are noise or edges), keep holes, dilate 2 cells (8 px), smooth
    let opened = max_filter(&min_filter(&motion, qw, qh, 1), qw, qh, 1);
    let opened: Vec<f32> = opened.iter().zip(&hole).map(|(a, h)| if *h == 1 { 1.0 } else { *a }).collect();
    let dil = max_filter(&opened, qw, qh, 2);
    let paint = rasterize(&plan.settings.paint, qw, qh, qf, plan.crop, n);
    let mut with_paint = dil.clone();
    for (m, p) in with_paint.iter_mut().zip(&paint) {
        if *p != NO_PAINT {
            *m = 1.0;
        }
    }
    let mask: Vec<f32> = box3(&with_paint, qw, qh).iter().map(|v| (v * 1.5).min(1.0)).collect();
    let excl: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let inv: Vec<f32> = cons[i].iter().map(|v| if *v == 0 { 1.0 } else { 0.0 }).collect();
            max_filter(&inv, qw, qh, 2).iter().map(|v| (*v > 0.5) as u8).collect()
        })
        .collect();
    // what the user sees as "detected motion"
    let shown: Vec<f32> = match level {
        Level::Fine => (0..qw * qh).map(|q| if (0..n).any(|i| i != ri && excl[i][q] == 1) { 1.0 } else { 0.0 }).collect(),
        _ => dil,
    };
    Maps { level, reference: ri, qw, qh, qf, motion: shown, mask, chain, excl, hole, refsat, paint, tau, area, holes }
}

/// Detected motion at the size `w × h` of a preview of the plan (0 … 1 per pixel, sensor orientation): what the dialog
/// shows as "recognised movement". Nearest cell; the preview covers the crop of the reference.
pub fn overlay(plan: &Plan, maps: &Maps, w: usize, h: usize) -> Vec<f32> {
    let fr = &plan.frames[plan.reference];
    let pf = fr.pf;
    let (pw, ph) = (fr.g.w, fr.g.h);
    let r = plan.crop;
    let (qx, qy) = (r.x / pf, r.y / pf);
    let (cw, ch) = ((r.w / pf).min(pw - qx), (r.h / pf).min(ph - qy));
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let py = qy + (((y as f32 + 0.5) / h as f32) * ch as f32) as usize;
        for (x, o) in row.iter_mut().enumerate() {
            let px = qx + (((x as f32 + 0.5) / w as f32) * cw as f32) as usize;
            let q = maps.cell_at(px, py, 2);
            *o = if maps.paint[q] != NO_PAINT { 1.0 } else { maps.motion[q] };
        }
    });
    out
}

/// Mask on the grid → full output size by bilinear interpolation between the cell centres. `cell` = output pixels per
/// cell, `(ox, oy)` = position of the output's top-left pixel in grid pixels, output `w × h`.
pub fn upsample(mask: &[f32], qw: usize, qh: usize, cell: usize, ox: usize, oy: usize, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    let c = cell as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let fy = ((oy + y) as f32 + 0.5) / c - 0.5;
        let y0 = fy.floor().max(0.0) as usize;
        let (y0, y1) = (y0.min(qh - 1), (y0 + 1).min(qh - 1));
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        for (x, o) in row.iter_mut().enumerate() {
            let fx = ((ox + x) as f32 + 0.5) / c - 0.5;
            let x0 = fx.floor().max(0.0) as usize;
            let (x0, x1) = (x0.min(qw - 1), (x0 + 1).min(qw - 1));
            let tx = (fx - x0 as f32).clamp(0.0, 1.0);
            let a = mask[y0 * qw + x0] * (1.0 - tx) + mask[y0 * qw + x1] * tx;
            let b = mask[y1 * qw + x0] * (1.0 - tx) + mask[y1 * qw + x1] * tx;
            *o = a * (1.0 - ty) + b * ty;
        }
    });
    out
}

// ─────────────────────────────────────────────── Laplacian pyramid blend (Burt & Adelson 1983)

const K: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];

fn atc(img: &Img, x: isize, y: isize) -> f32 {
    img.p[(y.clamp(0, img.h as isize - 1) as usize) * img.w + x.clamp(0, img.w as isize - 1) as usize]
}

fn reduce(src: &Img) -> Img {
    let (w2, h2) = (src.w.div_ceil(2), src.h.div_ceil(2));
    let mut tmp = vec![0f32; w2 * src.h];
    tmp.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let xc = (2 * x) as isize;
            *o = K.iter().enumerate().map(|(j, kv)| kv * atc(src, xc + j as isize - 2, y as isize)).sum();
        }
    });
    let t = Img { w: w2, h: src.h, p: tmp };
    let mut out = vec![0f32; w2 * h2];
    out.par_chunks_mut(w2).enumerate().for_each(|(y2, row)| {
        let y = (2 * y2) as isize;
        for (x, o) in row.iter_mut().enumerate() {
            *o = K.iter().enumerate().map(|(j, kv)| kv * atc(&t, x as isize, y + j as isize - 2)).sum();
        }
    });
    Img { w: w2, h: h2, p: out }
}

fn expand(src: &Img, w: usize, h: usize) -> Img {
    let wts = |x: usize| -> [(isize, f32); 3] {
        let c = (x / 2) as isize;
        if x % 2 == 0 {
            [(c - 1, 0.125), (c, 0.75), (c + 1, 0.125)]
        } else {
            [(c, 0.5), (c + 1, 0.5), (c, 0.0)]
        }
    };
    let mut tmp = vec![0f32; w * src.h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = wts(x).iter().map(|(cx, wv)| wv * atc(src, *cx, y as isize)).sum();
        }
    });
    let t = Img { w, h: src.h, p: tmp };
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let wy = wts(y);
        for (x, o) in row.iter_mut().enumerate() {
            *o = wy.iter().map(|(cy, wv)| wv * atc(&t, x as isize, *cy)).sum();
        }
    });
    Img { w, h, p: out }
}

fn gauss_pyr(img: &Img, levels: usize) -> Vec<Img> {
    let mut v = vec![img.clone()];
    for _ in 1..levels {
        let n = reduce(v.last().expect("level"));
        v.push(n);
    }
    v
}

fn lap_pyr(img: &Img, levels: usize) -> Vec<Img> {
    let g = gauss_pyr(img, levels);
    let mut out = Vec::with_capacity(levels);
    for l in 0..levels - 1 {
        let e = expand(&g[l + 1], g[l].w, g[l].h);
        out.push(Img { w: g[l].w, h: g[l].h, p: g[l].p.iter().zip(&e.p).map(|(a, b)| a - b).collect() });
    }
    out.push(g[levels - 1].clone());
    out
}

fn collapse(pyr: &[Img]) -> Img {
    let mut img = pyr.last().expect("level").clone();
    for l in (0..pyr.len() - 1).rev() {
        let e = expand(&img, pyr[l].w, pyr[l].h);
        img = Img { w: pyr[l].w, h: pyr[l].h, p: e.p.iter().zip(&pyr[l].p).map(|(a, b)| a + b).collect() };
    }
    img
}

fn levels_for(w: usize, h: usize, min_side: usize) -> usize {
    let (mut n, mut a, mut b) = (1, w, h);
    while a.min(b) / 2 >= min_side {
        a = a.div_ceil(2);
        b = b.div_ceil(2);
        n += 1;
    }
    n
}

/// Blend `b` into `a` with the mask `m` (1 = take `b`) in log2 with Laplacian pyramids; where the mask is 0 or 1 the
/// original value stays exactly. Works on the bounding box of the mask only. `w × h` interleaved RGB.
pub fn blend(a: &[[f32; 3]], b: &[[f32; 3]], m: &[f32], w: usize, h: usize) -> Vec<[f32; 3]> {
    let mut out = a.to_vec();
    // bounding box of the soft transition and the masked area, with a margin of the pyramid support
    let lv_full = levels_for(w, h, 16).min(7);
    let margin = (1usize << lv_full).min(w.max(h));
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if m[y * w + x] > 0.0 {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 {
        return out;
    }
    let (x0, y0) = (x0.saturating_sub(margin), y0.saturating_sub(margin));
    let (x1, y1) = ((x1 + margin + 1).min(w), (y1 + margin + 1).min(h));
    let (bw, bh) = (x1 - x0, y1 - y0);
    let lv = levels_for(bw, bh, 16).min(7);
    let eps = 2f32.powi(-20);
    let mi = Img { w: bw, h: bh, p: (0..bw * bh).map(|k| m[(y0 + k / bw) * w + x0 + k % bw]).collect() };
    let gm = gauss_pyr(&mi, lv);
    for c in 0..3 {
        let get = |src: &[[f32; 3]]| Img { w: bw, h: bh, p: (0..bw * bh).map(|k| src[(y0 + k / bw) * w + x0 + k % bw][c].max(eps).log2()).collect() };
        let la = lap_pyr(&get(a), lv);
        let lb = lap_pyr(&get(b), lv);
        let mixed: Vec<Img> = (0..lv)
            .map(|l| Img { w: la[l].w, h: la[l].h, p: (0..la[l].p.len()).map(|k| la[l].p[k] + gm[l].p[k] * (lb[l].p[k] - la[l].p[k])).collect() })
            .collect();
        let r = collapse(&mixed);
        for k in 0..bw * bh {
            let p = (y0 + k / bw) * w + x0 + k % bw;
            out[p][c] = if m[p] <= 0.0 {
                a[p][c]
            } else if m[p] >= 1.0 {
                b[p][c]
            } else {
                r.p[k].exp2()
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_keeps_unmasked_and_fully_masked_pixels() {
        let (w, h) = (64, 48);
        let a = vec![[1.0f32, 1.0, 1.0]; w * h];
        let b = vec![[4.0f32, 4.0, 4.0]; w * h];
        let mut m = vec![0f32; w * h];
        for y in 10..30 {
            for x in 20..40 {
                m[y * w + x] = 1.0;
            }
        }
        let o = blend(&a, &b, &m, w, h);
        assert_eq!(o[0], a[0]);
        assert_eq!(o[20 * w + 30], b[0]);
        // a soft edge goes through intermediate values
        let mut m2 = m.clone();
        m2[20 * w + 19] = 0.5;
        let o2 = blend(&a, &b, &m2, w, h);
        assert!(o2[20 * w + 19][0] > 1.0 && o2[20 * w + 19][0] < 4.0);
    }

    #[test]
    fn strokes_paint_and_erase_cells() {
        let crop = raw::Region { x: 0, y: 0, w: 200, h: 100 };
        let s = |frame: usize, erase: bool| Stroke { frame, erase, radius: 0.1, points: vec![[0.5, 0.5], [0.6, 0.5]] };
        let p = rasterize(&[s(2, false)], 50, 25, 4, crop, 3);
        assert_eq!(p[12 * 50 + 25], 2);
        assert_eq!(p[0], NO_PAINT);
        let p = rasterize(&[s(2, false), s(0, true)], 50, 25, 4, crop, 3);
        assert_eq!(p[12 * 50 + 25], NO_PAINT);
        // a frame index outside the list is ignored
        let p = rasterize(&[s(7, false)], 50, 25, 4, crop, 3);
        assert!(p.iter().all(|v| *v == NO_PAINT));
    }

    #[test]
    fn upsample_is_smooth_and_constant_inside() {
        let m = vec![0.0, 1.0, 1.0, 1.0];
        let o = upsample(&m, 2, 2, 4, 0, 0, 8, 8);
        assert_eq!(o[7 * 8 + 7], 1.0);
        assert!(o[0] < 0.2);
    }
}
