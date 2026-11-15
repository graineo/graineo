//! Classic chain, range-based reconstruction (high ISO): the filters on the reduced
//! image (range filter, edge maps, dark neutralisation, edge-guided local mean) and on the full
//! image (diffusion of the brightness signal, removal of single bright peaks).
//!
//! Several of these filters work in six bands of rows; the bands only decide which rows a band
//! writes, so they are processed one after the other in a fixed order. Values that are not written
//! stay zero (fresh buffers) or keep the previous content of the buffer, as noted at each function.
//! The arithmetic is `f32` or `f64` as noted, and sums are formed in a fixed order (bit-exact
//! results).

use rayon::prelude::*;

use super::region_scaling::Rgb;

/// Number of row bands.
const BANDS: u32 = 6;

/// Row bands `(first, last)` of `n` rows.
pub fn bands(n: usize) -> Vec<(usize, usize)> {
    let n = n as u32;
    let q = n / BANDS;
    (0..BANDS)
        .map(|k| {
            let s = k * q;
            let e = if k < BANDS - 1 { s + q } else { n };
            (s as usize, e.wrapping_sub(1) as usize)
        })
        .collect()
}

/// Table `exp(-i · scale / 100)` for the range filter (1600 entries).
pub fn exp_table(noise_scale: f32) -> Vec<f32> {
    (0..1600u32).map(|i| ((i as f32 * noise_scale) / -100.0f32).exp()).collect()
}

/// Range filter on the reduced image. `size` is the window size (odd), `subsample` uses only
/// every other window position (alternating rows offset by one). `model` is the colour noise model
/// (three offsets, three slopes, three curvatures; only offsets and slopes are used). Columns closer
/// than `size/2 - 1` to the left/right edge are not written (stay zero).
pub fn region_filter(inp: &Rgb, size: u32, model: &[f64; 9], subsample: bool, tab: &[f32]) -> Rgb {
    let (w, h) = (inp.w as u32, inp.h as u32);
    let mut outp = Rgb::new(inp.w, inp.h);
    let half = size >> 1;
    let step_switch = if subsample { half * 2 - 1 } else { size & !1 };
    let border = half.wrapping_sub(1);
    if !(border < w.wrapping_sub(border)) {
        return outp;
    }
    let step = if subsample { 2 } else { 1 };
    // rows are independent (each pixel reads only the input): in parallel
    outp.d.par_chunks_mut(w as usize * 3).enumerate().for_each(|(y, row)| {
        let y = y as u32;
        let mut weights: Vec<f32> = Vec::with_capacity((size * size) as usize);
        let mut neighbors: Vec<[f32; 3]> = Vec::with_capacity((size * size) as usize);
        for x in border..w - border {
            let c = inp.px(x as usize, y as usize);
            let k1 = 1.0f32 / ((f64::from(c[1].abs()) * model[4] + model[1]) as f32);
            let k0 = 1.0f32 / ((model[0] + model[3] * f64::from(c[0].abs())) as f32);
            let k2 = 1.0f32 / ((model[2] + model[5] * f64::from(c[2].abs())) as f32);
            weights.clear();
            neighbors.clear();
            let mut sum_w = 0.0f32;
            let mut reach = half;
            for j in 0..size {
                let zy = (y as i32) - (half as i32) + j as i32;
                let zy = if zy < 0 { 0 } else if (zy as u32) < h { zy as u32 } else { h - 1 };
                let mut zx = x as i32 - reach as i32;
                let end = x as i32 + reach as i32;
                while zx <= end {
                    let sx = if zx < 0 { 0 } else if (zx as u32) < w { zx as u32 } else { w - 1 };
                    let n = inp.px(sx as usize, zy as usize);
                    let d1 = n[1] - c[1];
                    let d0 = n[0] - c[0];
                    let d2 = n[2] - c[2];
                    let dist = ((d1 * d1) * k1 + (d0 * d0) * k0) + (d2 * d2) * k2;
                    if dist < 16.0 {
                        let g = tab[(dist * 100.0) as i32 as usize];
                        weights.push(g);
                        sum_w += g;
                        neighbors.push(n);
                    }
                    zx += step;
                }
                reach = step_switch.wrapping_sub(reach);
            }
            let (mut s0, mut s1, mut s2) = (0.0f32, 0.0f32, 0.0f32);
            for (g, n) in weights.iter().zip(&neighbors).rev() {
                s0 += n[0] * g;
                s1 += n[1] * g;
                s2 += n[2] * g;
            }
            let i = (x * 3) as usize;
            row[i] = s0 / sum_w;
            row[i + 1] = s1 / sum_w;
            row[i + 2] = s2 / sum_w;
        }
    });
    outp
}

/// Edge map from the gradients of the range-filtered image (each layer difference divided by the
/// noise level `a + b·value` of the centre). Value `1 - tanh(scale · sqrt(sum / 3))`; the outermost
/// rows and columns are not written (stay zero).
pub fn edge_map(inp: &Rgb, scale: f32, model: &[f64; 9]) -> Vec<f32> {
    let (w, h) = (inp.w, inp.h);
    let mut outp = vec![0.0f32; w * h];
    if h < 3 || w < 3 {
        return outp;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let p = inp.px(x, y);
            let k: [f32; 3] = std::array::from_fn(|c| 1.0f32 / ((f64::from(p[c]) * model[3 + c] + model[c]) as f32));
            let l = inp.px(x - 1, y);
            let r = inp.px(x + 1, y);
            let o = inp.px(x, y - 1);
            let u = inp.px(x, y + 1);
            let hq: [f32; 3] = std::array::from_fn(|c| {
                let v = (l[c] - r[c]) * k[c];
                v * v
            });
            let vq: [f32; 3] = std::array::from_fn(|c| {
                let v = (o[c] - u[c]) * k[c];
                v * v
            });
            let sum = (vq[2] + (vq[1] + vq[0])) + (hq[2] + (hq[1] + hq[0]));
            let t = (-((sum / 3.0f32).sqrt() * scale)).tanh();
            outp[y * w + x] = t + 1.0;
        }
    }
    outp
}

/// Second edge map from a weighted brightness of the range-filtered image: the larger of the
/// squared vertical and horizontal differences to the neighbours gives `min(1, 2·(1 − 1/(1+e)))`
/// with `e = exp(−difference)`. Two row buffers are used, so the edge
/// columns of a row read stale values of an earlier row; the outermost rows and columns are not
/// written (stay zero).
pub fn edge_map2(inp: &Rgb, g: [f32; 3]) -> Vec<f32> {
    let (w, h) = (inp.w, inp.h);
    let mut outp = vec![0.0f32; w * h];
    let mono = |p: [f32; 3]| (p[0] * g[0] + p[1] * g[1]) + p[2] * g[2];
    let mut a: Vec<f32> = (0..w).map(|x| mono(inp.px(x, 1.min(h.saturating_sub(1))))).collect();
    let mut b: Vec<f32> = (0..w).map(|x| mono(inp.px(x, 0))).collect();
    if h < 3 || w < 3 {
        return outp;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let m = mono(inp.px(x, y + 1));
            let c = a[x];
            let q = |d: f32| d * d;
            let v = q(m - c) + q(b[x] - c);
            let hz = q(a[x - 1] - c) + q(a[x + 1] - c);
            b[x] = m;
            let gr = if v > hz { v } else { hz };
            let e = (-gr).exp();
            let d = 1.0 - 1.0 / (f64::from(e) + 1.0);
            let value = (d + d) as f32;
            outp[y * w + x] = if value > 1.0 { 1.0 } else { value };
        }
        std::mem::swap(&mut a, &mut b);
    }
    outp
}

/// Neutralises dark pixels of the range-filtered image: below `low` the pixel becomes grey (its
/// largest layer), above `high` it stays, in between it is blended linearly.
pub fn dark_neutral(image: &mut Rgb, low: u32, high: u32) {
    let denominator = (high.wrapping_sub(low)) as i32 as f32;
    let low = low as f32;
    for p in image.d.chunks_exact_mut(3) {
        let mut m = p[0];
        if m <= p[1] {
            m = p[1];
        }
        if m <= p[2] {
            m = p[2];
        }
        let t0 = (m - low) / denominator;
        let t1 = if t0 <= 1.0 { t0 } else { 1.0 };
        let t = if 0.0 <= t1 { t1 } else { 0.0 };
        let v = ((1.0 - f64::from(t)) * f64::from(m)) as f32;
        p[0] = v + t * p[0];
        p[1] = v + t * p[1];
        p[2] = t * p[2] + v;
    }
}

/// One pass of the edge-guided recursive mean along the rows; the result is written transposed.
/// `edge` is the edge weight of the source (same layout); with `edge_from` the transposed edge
/// weights are handed out for the second pass.
fn recursive_rows(inp: &Rgb, edge: &[f32], falloff: f32, edge_from: Option<&mut Vec<f32>>) -> Rgb {
    let (w, h) = (inp.w, inp.h);
    let mut outp = Rgb::new(h, w);
    let mut ka = edge_from;
    if let Some(k) = ka.as_deref_mut() {
        k.clear();
        k.resize(w * h, 0.0);
    }
    let mut f = vec![[0.0f32; 3]; w];
    let mut b = vec![[0.0f32; 3]; w];
    let mut nf = vec![0.0f32; w];
    let mut nb = vec![0.0f32; w];
    for r in 0..h {
        let s = |x: usize| inp.px(x, r);
        let e = |x: usize| edge[r * w + x];
        f[0] = s(0);
        b[w - 1] = s(w - 1);
        nf[0] = 1.0;
        nb[w - 1] = 1.0;
        if let Some(k) = ka.as_deref_mut() {
            k[r] = e(0);
        }
        if w > 1 {
            for i in 0..w - 1 {
                if let Some(k) = ka.as_deref_mut() {
                    k[(i + 1) * h + r] = e(i + 1);
                }
                let a = e(i + 1) * falloff;
                let si = s(i + 1);
                for c in 0..3 {
                    f[i + 1][c] = f[i][c] * a + si[c];
                }
                nf[i + 1] = a * nf[i] + 1.0;
                let j = w - 2 - i;
                let bb = e(j) * falloff;
                let sj = s(j);
                for c in 0..3 {
                    b[j][c] = b[j + 1][c] * bb + sj[c];
                }
                nb[j] = bb * nb[j + 1] + 1.0;
            }
        }
        let mut set = |x: usize, v: [f32; 3]| {
            let i = (x * h + r) * 3;
            outp.d[i..i + 3].copy_from_slice(&v);
        };
        set(0, std::array::from_fn(|c| b[0][c] / nb[0]));
        if w >= 3 {
            for x in 1..w - 1 {
                let sx = s(x);
                let n = (nf[x] + nb[x]) + -1.0f32;
                set(x, std::array::from_fn(|c| ((f[x][c] + b[x][c]) - sx[c]) / n));
            }
        }
        set(w - 1, std::array::from_fn(|c| f[w - 1][c] / nf[w - 1]));
    }
    outp
}

/// Edge-guided local mean: recursive means forwards and backwards along the rows, then along the
/// columns, `iter` times; `edge` weights the propagation (0 stops it), `falloff` is the decay.
pub fn local_mean(image: &mut Rgb, edge: &[f32], falloff: f32, iter: u32) {
    let mut kt = Vec::new();
    for _ in 0..iter {
        let t = recursive_rows(image, edge, falloff, Some(&mut kt));
        *image = recursive_rows(&t, &kt, falloff, None);
    }
}

/// Diffusion of a brightness image (one band of rows). Conduction
/// `1/(d²·k² + 1)` with `k²` from `k2(centre)`; the new value is the centre plus `lambda` times the
/// conducted differences to the four neighbours. Each row is written one row late from a
/// row buffer, and the first column of the next input row goes into the output's first column.
fn diffusion_band(outp: &mut [f32], inp: &[f32], w: usize, h: usize, s: usize, e: usize, lambda: f32, k2: &dyn Fn(f32) -> f32) {
    let start = if s == 0 { 0 } else { s - 2 };
    let end = (if h - 1 <= e { e } else { e + 2 }) - 1;
    let mut buffer = vec![0.0f32; w];
    let mut first = true;
    for y in start + 1..=end {
        buffer[0] = inp[y * w];
        outp[(y - 1) * w] = inp[y * w];
        if w - 1 > 1 {
            for x in 1..w - 1 {
                let c = inp[y * w + x];
                let kk = k2(c);
                let g = |d: f32| 1.0f32 / (d * d * kk + 1.0);
                let du = inp[(y - 1) * w + x] - c;
                let dd = inp[(y + 1) * w + x] - c;
                let dr = inp[y * w + x + 1] - c;
                let dl = inp[y * w + x - 1] - c;
                let new = (((g(dl) * dl + g(dr) * dr) + g(dd) * dd) + g(du) * du) * lambda + c;
                if !first {
                    outp[(y - 1) * w + x] = buffer[x];
                }
                buffer[x] = new;
            }
        }
        first = false;
    }
    outp[end * w..end * w + w].copy_from_slice(&buffer);
}

/// Diffusion of the brightness image with the intensity noise model (`k = 1/(a + b·value)`).
pub fn diffusion_noise_model(outp: &mut [f32], inp: &[f32], w: usize, h: usize, iter: u32, model: [f32; 2], lambda: f32) {
    let k2 = |c: f32| {
        let k = 1.0f32 / (model[1] * c + model[0]);
        k * k
    };
    diffusion(outp, inp, w, h, iter, lambda, &k2);
}

/// Diffusion of the brightness image with a fixed conduction coefficient `kk` (`k² = 1/kk²`).
pub fn diffusion_fixed(outp: &mut [f32], inp: &[f32], w: usize, h: usize, iter: u32, kk: i32, lambda: f32) {
    let k = 1.0f32 / (kk.wrapping_mul(kk) as f32);
    diffusion(outp, inp, w, h, iter, lambda, &move |_| k);
}

fn diffusion(outp: &mut [f32], inp: &[f32], w: usize, h: usize, iter: u32, lambda: f32, k2: &dyn Fn(f32) -> f32) {
    if h < 3 || w < 2 {
        return;
    }
    for i in 0..iter {
        // from the second pass on the step reads its own output
        let source: Vec<f32> = if i == 0 { inp.to_vec() } else { outp.to_vec() };
        for (s, e) in bands(h) {
            diffusion_band(outp, &source, w, h, s, e, lambda, k2);
        }
    }
}

/// Removes single bright peaks: a pixel that is the largest of its column of three and strictly
/// larger than the columns left and right takes the largest of its eight neighbours. Written one
/// row late: the output's first row gets the unfiltered first input row, the
/// last two rows and columns keep the previous content of `outp`, and the first column of each row
/// gets the first value of the next input row.
pub fn peaks(outp: &mut [f32], inp: &[f32], w: usize, h: usize) {
    if h < 3 || w < 4 {
        return;
    }
    let mut buffer: Vec<f32> = inp[..w].to_vec();
    let mx = |a: f32, b: f32| if a > b { a } else { b };
    for y in 1..h - 1 {
        let o = &inp[(y - 1) * w..y * w];
        let z = &inp[y * w..(y + 1) * w];
        let u = &inp[(y + 1) * w..(y + 2) * w];
        buffer[0] = z[0];
        outp[(y - 1) * w] = z[0];
        // column maxima of the three rows (left and centre column)
        let mut links = mx(u[0], mx(z[0], o[0]));
        let mut center = mx(u[1], mx(z[1], o[1]));
        for x in 1..w - 2 {
            let c = z[x];
            let right = mx(u[x + 1], mx(z[x + 1], o[x + 1]));
            let mut new = c;
            if c == center && c > links && c > right {
                let t = mx(right, links);
                let t = mx(o[x], t);
                new = mx(u[x], t);
            }
            outp[(y - 1) * w + x] = buffer[x];
            buffer[x] = new;
            links = center;
            center = right;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_like_reference() {
        assert_eq!(bands(448), vec![(0, 73), (74, 147), (148, 221), (222, 295), (296, 369), (370, 447)]);
    }

    #[test]
    fn peak_is_removed() {
        let (w, h) = (6, 5);
        let mut inp = vec![10.0f32; w * h];
        inp[2 * w + 2] = 50.0;
        let mut outp = vec![0.0f32; w * h];
        peaks(&mut outp, &inp, w, h);
        assert_eq!(outp[2 * w + 2], 10.0);
        assert_eq!(outp[w + 1], 10.0);
    }
}
