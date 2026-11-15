//! Highlight reconstruction "inpaint opposed" on mosaic data (Bayer and X-Trans).
//!
//! Origin: Rust port of `_process_opposed()` and `_calc_refavg()` from **darktable**
//! `src/iop/hlreconstruct/opposed.c` and `segbased.c` (master, retrieved 2026-10-27),
//! license **GPL-3.0-or-later**, Copyright (C) 2022-2026 darktable developers
//! (method by garagecoder, Iain and Hanno Schwalm).
//!
//! Idea: for a blown channel, the mean of the two *other* channels in the
//! 3×3 neighborhood (averaged in cube-root space) is a good estimate. A global
//! chrominance correction from the unclipped photosites right next to blown
//! areas compensates color casts. Result: values above the white point (highlights get
//! detail/color instead of ending hard at 1).
//!
//! Changes: Rust/rayon instead of C/OpenMP, no hash/GUI cache, no "late correction"
//! (correction factor 1), mask in 3×3 superpixels as in the original.

use rayon::prelude::*;

/// darktable `highlights_clip_magics[OPPOSED]`
const CLIP_MAGIC: f32 = 0.987;

#[inline]
fn cube(x: f32) -> f32 {
    x * x * x
}

/// `cfa`: white-balanced, normalized mosaic values (white point per color = `wb[c]`).
/// Returns the number of blown photosites; with 0, `cfa` stays unchanged.
pub(crate) fn opposed(cfa: &mut [f32], w: usize, h: usize, wb: [f32; 3], color_at: &(impl Fn(usize, usize) -> usize + Sync)) -> usize {
    opposed_with(cfa, w, h, wb, color_at, true)
}

/// Like [`opposed`]; `local = true` (the default): *local*
/// chrominance correction instead of a global one – mean per 3×3 superpixel, gaps
/// filled from the surroundings with a push-pull pyramid (own addition, not from darktable).
pub(crate) fn opposed_with(cfa: &mut [f32], w: usize, h: usize, wb: [f32; 3], color_at: &(impl Fn(usize, usize) -> usize + Sync), local: bool) -> usize {
    let clips = [CLIP_MAGIC * wb[0], CLIP_MAGIC * wb[1], CLIP_MAGIC * wb[2]];
    let mean = w / 3;
    let mh = h / 3;
    if mean < 8 || mh < 8 {
        return 0;
    }
    let msize = mean * mh;
    // mask per color (3×3 superpixels)
    let mut mask = vec![0u8; 6 * msize];
    let n_clipped: usize = {
        let (m0, _) = mask.split_at_mut(3 * msize);
        let rows: Vec<(usize, [Vec<u8>; 3])> = (0..mh)
            .into_par_iter()
            .map(|mrow| {
                let mut r: [Vec<u8>; 3] = [vec![0; mean], vec![0; mean], vec![0; mean]];
                let mut n = 0usize;
                for mcol in 0..mean {
                    for y in 3 * mrow..3 * mrow + 3 {
                        for x in 3 * mcol..3 * mcol + 3 {
                            let c = color_at(x, y);
                            if cfa[y * w + x] >= clips[c] {
                                r[c][mcol] = 1;
                                n += 1;
                            }
                        }
                    }
                }
                (n, r)
            })
            .collect();
        let mut n = 0;
        for (mrow, (k, r)) in rows.into_iter().enumerate() {
            n += k;
            for c in 0..3 {
                m0[c * msize + mrow * mean..c * msize + (mrow + 1) * mean].copy_from_slice(&r[c]);
            }
        }
        n
    };
    if n_clipped == 0 {
        return 0;
    }
    // expand the mask slightly (circle with radius ~3 superpixels)
    {
        let (src, dst) = mask.split_at_mut(3 * msize);
        dst.par_chunks_mut(mean).enumerate().for_each(|(ri, out)| {
            let c = ri / mh;
            let row = ri % mh;
            let m = &src[c * msize..(c + 1) * msize];
            for col in 0..mean {
                let safe = col >= 3 && row >= 3 && col < mean - 3 && row < mh - 3;
                out[col] = if safe { dilated(m, row * mean + col, mean) } else { m[row * mean + col] };
            }
        });
    }
    // chrominance correction from photosites just below the clip next to blown areas
    let lo = [0.2 * clips[0], 0.2 * clips[1], 0.2 * clips[2]];
    let cfa_ro: &[f32] = cfa;
    // per superpixel row in parallel: sum and count per color and superpixel (global = sum of these)
    let row_acc: Vec<[(Vec<f32>, Vec<f32>); 3]> = (0..mh)
        .into_par_iter()
        .map(|mr| {
            let mut z: [(Vec<f32>, Vec<f32>); 3] = std::array::from_fn(|_| (vec![0f32; mean], vec![0f32; mean]));
            for row in 3 * mr..3 * mr + 3 {
                for col in 0..mean * 3 {
                    let c = color_at(col, row);
                    let v = cfa_ro[row * w + col];
                    let k = col / 3;
                    if v < clips[c] && v > lo[c] && mask[(c + 3) * msize + mr * mean + k] != 0 {
                        z[c].0[k] += v - refavg(cfa_ro, w, h, row, col, color_at);
                        z[c].1[k] += 1.0;
                    }
                }
            }
            z
        })
        .collect();
    let chroma: [f32; 3] = std::array::from_fn(|c| {
        let s: f64 = row_acc.iter().flat_map(|z| z[c].0.iter()).map(|&v| v as f64).sum();
        let n: f64 = row_acc.iter().flat_map(|z| z[c].1.iter()).map(|&v| v as f64).sum();
        if n > 100.0 { (s / n) as f32 } else { 0.0 }
    });
    let map: Option<[Vec<f32>; 3]> = local.then(|| {
        let mut v: Vec<Vec<f32>> = (0..3usize)
            .into_par_iter()
            .map(|c| {
                // only superpixels with data contribute; without any data the global value is enough
                if row_acc.iter().all(|z| z[c].1.iter().all(|&k| k == 0.0)) {
                    return vec![chroma[c]; msize];
                }
                let sum: Vec<f32> = row_acc.iter().flat_map(|z| z[c].0.iter().copied()).collect();
                let n: Vec<f32> = row_acc.iter().flat_map(|z| z[c].1.iter().copied()).collect();
                push_pull(&sum, &n, mean, mh, chroma[c])
            })
            .collect();
        let b = v.pop().unwrap_or_default();
        let g = v.pop().unwrap_or_default();
        let r = v.pop().unwrap_or_default();
        [r, g, b]
    });
    // replace (compute on a copy so that neighbors are read unchanged)
    let src = cfa.to_vec();
    cfa.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
        for (col, o) in out.iter_mut().enumerate() {
            let c = color_at(col, row);
            let v = src[row * w + col];
            if v >= clips[c] {
                let r = refavg(&src, w, h, row, col, color_at);
                let ch = match &map {
                    Some(k) => k[c][(row / 3).min(mh - 1) * mean + (col / 3).min(mean - 1)],
                    None => chroma[c],
                };
                *o = v.max(r + ch);
            }
        }
    });
    n_clipped
}

/// Fill gaps of a sparsely populated map (sum, count) from coarser levels (push-pull);
/// without any data: `fallback`.
fn push_pull(sum: &[f32], n: &[f32], w: usize, h: usize, fallback: f32) -> Vec<f32> {
    if w <= 1 || h <= 1 {
        let (s, k) = (sum.iter().sum::<f32>(), n.iter().sum::<f32>());
        return vec![if k > 0.0 { s / k } else { fallback }; w * h];
    }
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut s2 = vec![0f32; w2 * h2];
    let mut n2 = vec![0f32; w2 * h2];
    for y in 0..h {
        for x in 0..w {
            s2[(y / 2) * w2 + x / 2] += sum[y * w + x];
            n2[(y / 2) * w2 + x / 2] += n[y * w + x];
        }
    }
    let coarse = push_pull(&s2, &n2, w2, h2, fallback);
    // per cell: own mean, pulled towards the coarser value when there is little data
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let g = coarse[(y / 2) * w2 + x / 2];
            let k = n[i];
            if k <= 0.0 {
                g
            } else {
                let a = k / (k + 4.0);
                a * sum[i] / k + (1.0 - a) * g
            }
        })
        .collect()
}

fn dilated(m: &[u8], i: usize, w1: usize) -> u8 {
    let at = |d: isize| m[(i as isize + d) as usize];
    let w1 = w1 as isize;
    if at(0) != 0 {
        return 1;
    }
    for d in [-w1 - 1, -w1, -w1 + 1, -1, 1, w1 - 1, w1, w1 + 1] {
        if at(d) != 0 {
            return 1;
        }
    }
    let (w2, w3) = (2 * w1, 3 * w1);
    let ring = [
        -w3 - 2, -w3 - 1, -w3, -w3 + 1, -w3 + 2,
        -w2 - 3, -w2 - 2, -w2 - 1, -w2, -w2 + 1, -w2 + 2, -w2 + 3,
        -w1 - 3, -w1 - 2, -w1 + 2, -w1 + 3,
        -3, -2, 2, 3,
        w1 - 3, w1 - 2, w1 + 2, w1 + 3,
        w2 - 3, w2 - 2, w2 - 1, w2, w2 + 1, w2 + 2, w2 + 3,
        w3 - 2, w3 - 1, w3, w3 + 1, w3 + 2,
    ];
    ring.iter().any(|&d| at(d) != 0) as u8
}

/// Mean of the two opposite colors in the 3×3 neighborhood (cube-root space), back to linear.
fn refavg(cfa: &[f32], w: usize, h: usize, row: usize, col: usize, color_at: &impl Fn(usize, usize) -> usize) -> f32 {
    let color = color_at(col, row);
    let mut mean = [0f32; 3];
    let mut cnt = [0f32; 3];
    let (y0, x0) = (row.saturating_sub(1), col.saturating_sub(1));
    let (y1, x1) = ((row + 2).min(h - 1), (col + 2).min(w - 1));
    for y in y0..y1 {
        for x in x0..x1 {
            let c = color_at(x, y);
            mean[c] += cfa[y * w + x].max(0.0);
            cnt[c] += 1.0;
        }
    }
    for c in 0..3 {
        mean[c] = if cnt[c] > 0.0 { (mean[c] / cnt[c]).cbrt() } else { 0.0 };
    }
    let r = [0.5 * (mean[1] + mean[2]), 0.5 * (mean[0] + mean[2]), 0.5 * (mean[0] + mean[1])];
    cube(r[color])
}

/// Synthetic test: overexpose the image by `ev` (mosaic × 2^ev, clipped at white
/// per color), reconstruct and compare with the true values – only photosites that
/// are blown only because of the overexposure. Result: (count, mean |log2| error
/// per photosite, mean color error |log2(R/G)| + |log2(B/G)| per 3×3 superpixel) for
/// global (darktable) and local.
pub fn check_synthetic(rf: &crate::RawFile, ev: f32) -> Option<[(usize, f32, f32); 2]> {
    let (mut norm, pat) = rf.mosaic_normalized().ok()?;
    let (w, h) = (rf.info.width, rf.info.height);
    let wb = rf.info.wb.unwrap_or([1.0; 3]);
    let color_at = move |x: usize, y: usize| pat[y % 6][x % 6] as usize;
    let s = ev.exp2();
    let mut true_vals = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = color_at(x, y);
            let v = norm[y * w + x] * wb[c];
            // already blown in the original → do not evaluate (NaN)
            true_vals[y * w + x] = if v >= CLIP_MAGIC * wb[c] { f32::NAN } else { v * s };
            norm[y * w + x] = (v * s).min(wb[c]);
        }
    }
    let mut out = [(0usize, 0f32, 0f32); 2];
    for (k, local) in [false, true].into_iter().enumerate() {
        let mut m = norm.clone();
        opposed_with(&mut m, w, h, wb, &color_at, local);
        let (mut n, mut e) = (0usize, 0f64);
        for i in 0..w * h {
            let c = color_at(i % w, i / w);
            if true_vals[i].is_finite() && norm[i] >= CLIP_MAGIC * wb[c] && true_vals[i] > 0.0 {
                n += 1;
                e += ((m[i].max(1e-6) / true_vals[i]).log2()).abs() as f64;
            }
        }
        // color per superpixel (only fully evaluable ones with at least one blown photosite)
        let (mut nf, mut ef) = (0usize, 0f64);
        for my in 0..h / 3 {
            for mx in 0..w / 3 {
                let mut a = [[0f64; 3]; 2];
                let (mut ok, mut clip) = (true, false);
                for y in 3 * my..3 * my + 3 {
                    for x in 3 * mx..3 * mx + 3 {
                        let i = y * w + x;
                        let c = color_at(x, y);
                        if !true_vals[i].is_finite() {
                            ok = false;
                        }
                        clip |= norm[i] >= CLIP_MAGIC * wb[c];
                        a[0][c] += true_vals[i] as f64;
                        a[1][c] += m[i] as f64;
                    }
                }
                if ok && clip && a[0].iter().all(|v| *v > 0.0) && a[1].iter().all(|v| *v > 0.0) {
                    let f = |q: &[f64; 3]| ((q[0] / q[1]).log2(), (q[2] / q[1]).log2());
                    let (t, r) = (f(&a[0]), f(&a[1]));
                    ef += (t.0 - r.0).abs() + (t.1 - r.1).abs();
                    nf += 1;
                }
            }
        }
        out[k] = (n, (e / n.max(1) as f64) as f32, (ef / nf.max(1) as f64) as f32);
    }
    Some(out)
}
