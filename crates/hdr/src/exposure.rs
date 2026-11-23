//! Exposure factors: for neighbouring frames (sorted by exposure) the median of the ratios of well-exposed proxy
//! pixels after alignment, chained to the reference. EXIF serves only as a plausibility check: a measurement more than
//! 0.3 EV away from EXIF (or too little overlap) falls back to EXIF with a warning. With neither a measurement nor
//! EXIF the step is unknown: the frames count as equally exposed (factor 1) and the plan says so.

use crate::align::Warp;
use crate::frames::Frame;
use rayon::prelude::*;

/// Deviation from EXIF (EV) above which the measured factor is not trusted.
pub const EXIF_TOLERANCE_EV: f64 = 0.3;

/// Proxy coordinates of the reference → proxy coordinates of the frame.
#[inline]
pub fn proxy_map(warp: &Warp, x: f64, y: f64, w: usize, h: usize) -> (f64, f64) {
    warp.apply(x, y, w as f64 / 2.0, h as f64 / 2.0, w.max(h) as f64 / 2.0)
}

/// Median of `dark / bright` over proxy pixels where the brighter frame is unsaturated (< 0.85 of clip) and above 2 %
/// of white, and the darker frame above its noise floor. `None` if fewer than 0.5 % of the samples qualify.
pub fn pair_ratio(b: &Frame, wb_: &Warp, d: &Frame, wd: &Warp) -> Option<f64> {
    let (w, h) = (b.g.w, b.g.h);
    if w < 40 || h < 40 {
        return None;
    }
    let mut q: Vec<f64> = (16..h - 16)
        .into_par_iter()
        .step_by(2)
        .flat_map_iter(|y| {
            let mut v = Vec::new();
            for x in (16..w - 16).step_by(2) {
                let (xb, yb) = proxy_map(wb_, x as f64, y as f64, w, h);
                let (xd, yd) = proxy_map(wd, x as f64, y as f64, w, h);
                let (Some(bv), Some(bm), Some(dv)) = (b.g.sample(xb, yb), b.mx.sample_max(xb, yb), d.g.sample(xd, yd)) else { continue };
                if bm >= 0.85 || bv < 0.02 || dv < 0.004 {
                    continue;
                }
                v.push(dv as f64 / bv as f64);
            }
            v
        })
        .collect();
    let total = ((h - 32) / 2) * ((w - 32) / 2);
    if q.len() * 200 < total {
        return None;
    }
    let m = q.len() / 2;
    q.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
    Some(q[m])
}

/// One step of the chain (from the brighter to the darker neighbour).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Step {
    /// measured ratio dark/bright (`None`: too little overlap)
    pub measured: Option<f64>,
    /// EXIF ratio (`None`: EXIF incomplete)
    pub exif: Option<f64>,
    /// used ratio (1.0 when neither a measurement nor EXIF exists)
    pub used: f64,
    /// EXIF was used although a measurement existed (deviation > [`EXIF_TOLERANCE_EV`]) or none was possible.
    /// `false` when there was no EXIF to fall back to (`measured` and `exif` both `None`).
    pub fell_back: bool,
}

/// Exposure factor per frame relative to the reference (`k[ref] = 1`, brighter frames > 1). `order`: frame indices
/// dark → bright; `warps[i]`: warp of frame i onto the reference.
pub fn factors(frames: &[Frame], warps: &[Warp], order: &[usize], reference: usize) -> (Vec<f64>, Vec<Step>) {
    let n = frames.len();
    let mut steps = Vec::with_capacity(n.saturating_sub(1));
    // step[j] = exposure(order[j-1]) / exposure(order[j])  (dark / bright)
    let mut step = vec![1f64; n];
    for j in 1..n {
        let (bi, di) = (order[j], order[j - 1]);
        let measured = pair_ratio(&frames[bi], &warps[bi], &frames[di], &warps[di]).filter(|r| r.is_finite() && *r > 0.0);
        let exif = match (frames[bi].exif, frames[di].exif) {
            (Some(x), Some(y)) if x > 0.0 => Some(y / x),
            _ => None,
        };
        let (used, fell_back) = match (measured, exif) {
            (Some(r), Some(q)) if (r / q).log2().abs() < EXIF_TOLERANCE_EV => (r, false),
            (Some(_), Some(q)) => (q, true),
            (Some(r), None) => (r, false),
            (None, Some(q)) => (q, true),
            // nothing known: equal exposure is assumed, and no EXIF fallback took place
            (None, None) => (1.0, false),
        };
        step[j] = used;
        steps.push(Step { measured, exif, used, fell_back });
    }
    let pos_ref = order.iter().position(|&i| i == reference).unwrap_or(0);
    let mut ks = vec![1f64; n];
    for j in 0..n {
        let mut k = 1.0;
        if j < pos_ref {
            for s in &step[j + 1..=pos_ref] {
                k *= s;
            }
        } else {
            for s in &step[pos_ref + 1..=j] {
                k /= s;
            }
        }
        ks[order[j]] = k;
    }
    (ks, steps)
}
