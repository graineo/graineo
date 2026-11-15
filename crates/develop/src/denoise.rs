//! Noise reduction modelled on SPP's results.
//!
//! Two layers:
//! - **Base image (colour):** bilateral filter without spatial weight using the camera's
//!   signal-dependent noise model, followed by an edge-guided
//!   recursive averaging. This smooths colour noise over large areas.
//! - **Luminance detail:** quotient of the (gently diffused) luminance of the
//!   original and the luminance of the base image. It is multiplied back in,
//!   slightly amplified. This preserves the Foveon sharpness.
//!
//! Simplifications: no tiling, no resampling at high
//! ISO, no banding correction, no pattern scrambling,
//! no ColorDQ.

use rayon::prelude::*;

use crate::calib::Calibration;
use crate::Linear;

/// Sliders as in SPP: 0 … 1 in quarter steps, 0.5 = neutral table row.
#[derive(Clone, Copy, Debug)]
pub struct DenoiseSliders {
    pub luma: f64,
    pub chroma: f64,
}

impl DenoiseSliders {
    pub fn new(luma: f64, chroma: f64) -> Self {
        Self { luma, chroma }
    }
}

impl Default for DenoiseSliders {
    fn default() -> Self {
        Self { luma: 0.5, chroma: 0.5 }
    }
}

#[derive(Debug)]
struct Params {
    noise_scaling: f32,
    alpha: f32,
    locality: f32,
    edge_strength: f32,
    decay: f32,
    residual_mixing: f32,
    residual_color_mixing: f32,
    residual_strength: f32,
    aniso_scale: f32,
}

fn params(cal: &Calibration, s: DenoiseSliders) -> Params {
    let iso = cal.iso.max(1.0);
    // Interpolate ISONoiseSettings linearly in ISO (column 0 = ISO).
    let t = &cal.iso_noise_settings;
    let row = |i: usize, j: usize| t[i * 13 + j];
    let n = t.len() / 13;
    let col = |j: usize, default: f64| -> f64 {
        if n == 0 {
            return default;
        }
        let iso_c = iso.clamp(row(0, 0), row(n - 1, 0));
        let k = (0..n).find(|&k| row(k, 0) >= iso_c).unwrap_or(n - 1);
        let i = k.saturating_sub(1);
        if i + 1 >= n || row(i + 1, 0) == row(i, 0) {
            return row(i, j);
        }
        let w = (iso_c - row(i, 0)) / (row(i + 1, 0) - row(i, 0));
        (1.0 - w) * row(i, j) + w * row(i + 1, j)
    };
    let il = (4.0 * s.luma.clamp(0.0, 1.0)) as usize;
    let ic = (4.0 * s.chroma.clamp(0.0, 1.0)) as usize;
    let st = &cal.denoise_slider_steps;
    let step = |i: usize, j: usize| if st.len() == 65 { st[i * 13 + j] } else { 1.0 };
    let all_ones_11 = (0..5).all(|i| step(i, 11) == 1.0);
    let strength_factor = if all_ones_11 { [1.3333, 1.1667, 1.0, 0.8333, 0.6667][il] } else { step(il, 11) };

    let tt = (iso.ln() - 100f64.ln()) / (6400f64.ln() - 100f64.ln());
    let lo = (0.80 - 0.15 * tt, 1.0, 0.80 - 0.10 * tt, 0.95 + 0.03 * tt);
    let hi = (1.0, 0.2 - 0.1 * tt, 0.70 + 0.25 * tt, 0.95);
    let f = [0.0, 0.25, 0.5, 0.75, 1.0][ic];
    let alpha = match ic {
        0 => lo.0,
        1 => (lo.0 + hi.0) / 2.0,
        _ => 1.0,
    };
    Params {
        noise_scaling: col(4, 0.005) as f32,
        alpha: alpha as f32,
        locality: (lo.1 + f * (hi.1 - lo.1)) as f32,
        edge_strength: (lo.2 + f * (hi.2 - lo.2)).clamp(0.0, 0.99) as f32,
        decay: (lo.3 + f * (hi.3 - lo.3)).clamp(0.0, 1.0) as f32,
        residual_mixing: (col(9, 0.8) * step(il, 9)).clamp(0.0, 1.0) as f32,
        residual_color_mixing: (col(10, 0.0) * step(ic, 10)).clamp(0.0, 1.0) as f32,
        residual_strength: (col(11, 2.0) * strength_factor).max(0.0) as f32,
        aniso_scale: step(il, 0) as f32,
    }
}

/// Signal-dependent noise variance per channel: σ² = A + B·|v| + C·v².
#[derive(Clone, Copy)]
struct NoiseModel([[f32; 3]; 3]);

impl NoiseModel {
    #[inline]
    fn var(&self, c: usize, v: f32) -> f32 {
        let m = &self.0;
        (m[0][c] + m[1][c] * v.abs() + m[2][c] * v * v).max(1e-3)
    }
}

/// Radius of the bilateral window per 32×32 block: dark areas are relatively noisier
/// and get large windows.
fn radius_map(t: &[f32], w: usize, h: usize, iso: f64, scale: usize) -> (Vec<u8>, usize) {
    let b_size = (32 / scale).max(4);
    #[allow(non_snake_case)]
    let B = b_size;
    let (bw, bh) = (w.div_ceil(B), h.div_ceil(B));
    let means: Vec<f32> = (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % bw, i / bw);
            let (mut s, mut n) = (0f32, 0usize);
            for y in by * B..((by + 1) * B).min(h) {
                for x in bx * B..((bx + 1) * B).min(w) {
                    let p = &t[(y * w + x) * 3..];
                    s += p[0].min(p[1]).min(p[2]);
                    n += 1;
                }
            }
            s / n.max(1) as f32
        })
        .collect();
    let table: [u8; 5] = if iso <= 400.0 { [9, 5, 4, 3, 2] } else { [9, 7, 6, 5, 3] };
    let radii = (0..bw * bh)
        .map(|i| {
            let (bx, by) = (i % bw, i / bw);
            let mut m = f32::MAX;
            for yy in by.saturating_sub(1)..=(by + 1).min(bh - 1) {
                for xx in bx.saturating_sub(1)..=(bx + 1).min(bw - 1) {
                    m = m.min(means[yy * bw + xx]);
                }
            }
            let r = match m {
                m if m < 150.0 => table[0],
                m if m < 300.0 => table[1],
                m if m < 500.0 => table[2],
                m if m < 700.0 => table[3],
                _ => table[4],
            };
            (r as usize).div_ceil(scale).max(1) as u8
        })
        .collect();
    (radii, bw)
}

/// Bilateral filter without spatial weight; distance = noise-normalised channel differences.
/// `scale` = downscale factor of the image; blocks and radii are converted accordingly.
fn bilateral(t: &[f32], w: usize, h: usize, nm: NoiseModel, p: &Params, iso: f64, scale: usize) -> Vec<f32> {
    let (radii, bw) = radius_map(t, w, h, iso, scale);
    let ns = p.noise_scaling;
    let mut out = vec![0f32; t.len()];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, line)| {
        for x in 0..w {
            let block = (32 / scale).max(4);
            let r = radii[(y / block) * bw + x / block] as usize;
            let c = &t[(y * w + x) * 3..(y * w + x) * 3 + 3];
            let inv = [ns / nm.var(0, c[0]), ns / nm.var(1, c[1]), ns / nm.var(2, c[2])];
            let sub = r > 3;
            let (mut acc, mut ws) = ([0f32; 3], 0f32);
            for yy in y.saturating_sub(r)..=(y + r).min(h - 1) {
                let x_start = x.saturating_sub(r);
                let mut xx = if sub && (x_start + yy) % 2 != (x + y) % 2 { x_start + 1 } else { x_start };
                let x_end = (x + r).min(w - 1);
                while xx <= x_end {
                    let q = &t[(yy * w + xx) * 3..(yy * w + xx) * 3 + 3];
                    let d0 = q[0] - c[0];
                    let d1 = q[1] - c[1];
                    let d2 = q[2] - c[2];
                    let arg = d0 * d0 * inv[0] + d1 * d1 * inv[1] + d2 * d2 * inv[2];
                    if arg < 8.0 {
                        let wt = (-arg).exp();
                        ws += wt;
                        acc[0] += wt * q[0];
                        acc[1] += wt * q[1];
                        acc[2] += wt * q[2];
                    }
                    xx += if sub { 2 } else { 1 };
                }
            }
            let o = &mut line[x * 3..x * 3 + 3];
            for k in 0..3 {
                let v = if ws > 0.0 { acc[k] / ws } else { c[k] };
                o[k] = p.alpha * v + (1.0 - p.alpha) * c[k];
            }
        }
    });
    out
}

fn median5(a: [f32; 5]) -> f32 {
    let mut a = a;
    a.sort_unstable_by(|x, y| x.total_cmp(y));
    a[2]
}

/// Edge strength per pixel → recursion factor `a` of the recursive averaging.
fn edge_factors(t: &[f32], w: usize, h: usize, nm: NoiseModel, p: &Params, lin_shift: f32, scale: usize) -> Vec<f32> {
    let get = |x: usize, y: usize, c: usize| t[(y * w + x) * 3 + c] + lin_shift;
    // Cross median per channel
    let mut med = vec![0f32; t.len()];
    med.par_chunks_mut(w * 3).enumerate().for_each(|(y, line)| {
        for x in 0..w {
            for c in 0..3 {
                line[x * 3 + c] = if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                    get(x, y, c)
                } else {
                    median5([get(x, y, c), get(x - 1, y, c), get(x + 1, y, c), get(x, y - 1, c), get(x, y + 1, c)])
                };
            }
        }
    });
    let m = |x: usize, y: usize, c: usize| med[(y * w + x) * 3 + c];
    let mut e = vec![0f32; w * h];
    e.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        if y == 0 || y == h - 1 {
            return;
        }
        for x in 1..w - 1 {
            let (mut gh, mut gv) = (0f32, 0f32);
            for c in 0..3 {
                let s = nm.0[0][c] + nm.0[1][c] * m(x, y, c);
                let h2 = ((m(x - 1, y, c) - m(x + 1, y, c)) / s).powi(2);
                let v2 = ((m(x, y - 1, c) - m(x, y + 1, c)) / s).powi(2);
                gh = gh.max(h2);
                gv = gv.max(v2);
            }
            line[x] = ((gh + gv) / 2.0).sqrt();
        }
    });
    let (s, loc, decay) = (p.edge_strength, p.locality, p.decay);
    let mut a = vec![0f32; w * h];
    a.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        for x in 0..w {
            let ee = if x == 0 || y == 0 || x == w - 1 || y == h - 1 {
                e[y * w + x]
            } else {
                0.2 * (e[y * w + x] + e[y * w + x - 1] + e[y * w + x + 1] + e[(y - 1) * w + x] + e[(y + 1) * w + x])
            };
            // A preview pixel spans `scale` image pixels → raise the recursion factor to that power.
            line[x] = (decay * ((1.0 - (loc * ee).tanh() - s) / (1.0 - s)).max(0.0)).powi(scale as i32);
        }
    });
    a
}

/// Edge-guided, bidirectional recursive averaging along the rows (one channel triple).
fn local_mean_rows(img: &mut [f32], a: &[f32], w: usize) {
    img.par_chunks_mut(w * 3).zip(a.par_chunks(w)).for_each(|(line, a)| {
        let mut f = vec![0f32; w * 3];
        let mut nf = vec![0f32; w];
        let (mut pf, mut pn) = ([0f32; 3], 0f32);
        for x in 0..w {
            nf[x] = 1.0 + a[x] * pn;
            pn = nf[x];
            for c in 0..3 {
                f[x * 3 + c] = line[x * 3 + c] + a[x] * pf[c];
                pf[c] = f[x * 3 + c];
            }
        }
        let (mut pb, mut pnb) = ([0f32; 3], 0f32);
        for x in (0..w).rev() {
            let nb = 1.0 + a[x] * pnb;
            pnb = nb;
            let denom = nf[x] + nb - 1.0;
            for c in 0..3 {
                let i = line[x * 3 + c];
                let b = i + a[x] * pb[c];
                pb[c] = b;
                line[x * 3 + c] = (f[x * 3 + c] + b - i) / denom;
            }
        }
    });
}

fn transpose3(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut dst = vec![0f32; src.len()];
    dst.par_chunks_mut(h * 3).enumerate().for_each(|(x, col)| {
        for y in 0..h {
            col[y * 3..y * 3 + 3].copy_from_slice(&src[(y * w + x) * 3..(y * w + x) * 3 + 3]);
        }
    });
    dst
}

fn transpose1(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut dst = vec![0f32; src.len()];
    dst.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for y in 0..h {
            col[y] = src[y * w + x];
        }
    });
    dst
}

/// One explicit Perona-Malik step (4-neighbourhood, λ = 0.25) with conductivity `g`.
fn diffuse(l: &[f32], w: usize, h: usize, g: impl Fn(f32, f32) -> f32 + Sync) -> Vec<f32> {
    let mut out = l.to_vec();
    out.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        if y == 0 || y == h - 1 {
            return;
        }
        for x in 1..w - 1 {
            let u = l[y * w + x];
            let mut s = 0f32;
            for n in [l[y * w + x - 1], l[y * w + x + 1], l[(y - 1) * w + x], l[(y + 1) * w + x]] {
                let d = n - u;
                s += g(d, u) * d;
            }
            line[x] = u + 0.25 * s;
        }
    });
    out
}

/// Detail amplification, symmetric in the logarithm (x = 1 means no detail): `x^s`, base and detail
/// separated in the log domain (Durand & Dorsey 2002).
#[inline]
fn f_s(x: f32, s: f32) -> f32 {
    x.max(1e-6).powf(s)
}

pub fn denoise(lin: &mut Linear, cal: &Calibration, sliders: DenoiseSliders) {
    denoise_scaled(lin, cal, sliders, 1);
}

/// Like [`denoise`] for an image downscaled by `factor` (preview): averaging over
/// factor² pixels lowers the noise variance accordingly; the noise model is adapted.
pub fn denoise_scaled(lin: &mut Linear, cal: &Calibration, sliders: DenoiseSliders, factor: usize) {
    let Some(mut cnm) = cal.noise_model else { return };
    let var_scale = 1.0 / (factor * factor).max(1) as f64;
    for row in &mut cnm {
        for v in row.iter_mut() {
            *v *= var_scale;
        }
    }
    let p = params(cal, sliders);
    let (w, h) = (lin.width, lin.height);
    // Computed in sensor DN with WB gains · count_scale (DP 2, SD1 1/0.55), as in SPP.
    let g: [f32; 3] = cal.wb_gain.map(|v| (cal.count_scale * v) as f32);
    let sat = cal.saturation as f32;
    let nm = NoiseModel(std::array::from_fn(|k| {
        std::array::from_fn(|c| match k {
            0 => cnm[0][c] as f32 * g[c] * g[c],
            1 => cnm[1][c] as f32 * g[c],
            _ => cnm[2][c] as f32,
        })
    }));
    let t: Vec<f32> = lin.data.par_chunks(3).flat_map_iter(|q| [q[0] * sat * g[0], q[1] * sat * g[1], q[2] * sat * g[2]]).collect();

    // Base image: bilateral + recursive averaging (horizontal, then vertical via transposition).
    let mut a_img = bilateral(&t, w, h, nm, &p, cal.iso, factor.max(1));
    let a = edge_factors(&t, w, h, nm, &p, cal.edge_lin_shift as f32, factor.max(1));
    local_mean_rows(&mut a_img, &a, w);
    let mut a_t = transpose3(&a_img, w, h);
    let a_tr = transpose1(&a, w, h);
    local_mean_rows(&mut a_t, &a_tr, h);
    let base = transpose3(&a_t, h, w);
    drop((a_img, a_t, a_tr, a));

    // Luminance detail as a quotient.
    let wl: [f32; 3] = g.map(|gc| 1.0 / (3.0 * gc));
    let luma = |px: &[f32]| wl[0] * px[0] + wl[1] * px[1] + wl[2] * px[2];
    let l: Vec<f32> = t.par_chunks(3).map(luma).collect();
    let inm: [f32; 3] = std::array::from_fn(|k| (0..3).map(|c| cnm[k][c] as f32).sum::<f32>() / 9.0 * p.aniso_scale);
    let l_ad = diffuse(&l, w, h, |d, u| {
        let s = inm[0] + inm[1] * u + inm[2] * u * u;
        1.0 / (1.0 + d * d / (s * s).max(1e-6))
    });
    let l_ad = diffuse(&l_ad, w, h, |d, _| 1.0 / (1.0 + d * d / 4.0));

    let eps = cal.residual_epsilon as f32;
    let (mix, cmix, strength) = (p.residual_mixing, p.residual_color_mixing, p.residual_strength);
    const CHUNK: usize = 1024;
    lin.data.par_chunks_mut(3 * CHUNK).enumerate().for_each(|(ci, chunk)| {
        let mut rc = [0f32; 3 * CHUNK];
        for (k, _) in chunk.chunks_exact(3).enumerate() {
            let i = ci * CHUNK + k;
            let b = &base[i * 3..i * 3 + 3];
            let tp = &t[i * 3..i * 3 + 3];
            let lb = (wl[0] * b[0] + wl[1] * b[1] + wl[2] * b[2]).max(0.0) + eps;
            let r = (l_ad[i] + eps) / lb;
            let r_raw = ((wl[0] * tp[0]).max(0.0) + (wl[1] * tp[1]).max(0.0) + (wl[2] * tp[2]).max(0.0) + eps) / lb;
            let m = mix * r + (1.0 - mix) * r_raw;
            for c in 0..3 {
                let x = if cmix > 0.0 {
                    let rcc = (3.0 * (wl[c] * tp[c]).max(0.0) + eps) / (3.0 * wl[c] * b[c] + eps);
                    cmix * rcc + (1.0 - cmix) * m
                } else {
                    m
                };
                rc[3 * k + c] = f_s(x, strength);
            }
        }
        for (k, out) in chunk.chunks_exact_mut(3).enumerate() {
            let i = ci * CHUNK + k;
            let b = &base[i * 3..i * 3 + 3];
            for c in 0..3 {
                out[c] = b[c].max(0.0) * rc[3 * k + c] / (sat * g[c]);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Detail scaling is `x^s` (1 stays 1, symmetric in the logarithm).
    #[test]
    fn detail_scaling() {
        for s in [0.0f32, 0.5, 1.5, 2.0] {
            assert!((f_s(1.0, s) - 1.0).abs() < 1e-6);
            for x in [0.2f32, 0.5, 0.9, 1.1, 2.0, 5.0] {
                assert!((f_s(x, s) * f_s(1.0 / x, s) - 1.0).abs() < 1e-4, "x={x} s={s}");
                assert!((f_s(x, s) - x.powf(s)).abs() < 1e-5);
            }
        }
    }
}
