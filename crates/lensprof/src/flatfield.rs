//! Flat-field measurement per channel (Bayer: R/G/B, Foveon: top/middle/bottom layer).
//!
//! Classic flat-field procedure (white frame through a diffuser in front of the lens). Input: camera channels in
//! sensor position, linear, black level removed. Steps:
//! 1. Block means on 96×64 blocks, trimmed per channel (top/bottom 10 % dropped: dust, hot pixels).
//! 2. Check of each shot: level of the middle channel, clipped pixels, edges inside the blocks.
//! 3. Pair A/B (B with the camera turned by 180°): the lens and sensor stay in place, a linear lighting
//!    gradient of the scene appears mirrored, so `F = √(A·B)` cancels it. Consistency of the pair:
//!    RMS of `log(A/B)` after removing a plane. Without a pair a common tilt of all channels counts as
//!    lighting and is removed (quality at most 70).
//! 4. Radial vignetting `V(r) = 1 + a1 r² + a2 r⁴ + a3 r⁶` from the middle channel (r in units of the longer
//!    edge, like `raw::measure::estimate_vignette`), robust (Tukey weights).
//! 5. Rest per channel: `log(F_c / V)` smoothed by Legendre polynomials up to total degree 4 plus a radial
//!    r⁶ term, IRLS with Tukey weight (c = 4.685 · 1.4826 · MAD), three passes; blocks with weight 0 count as
//!    dust. The colour-cast map is this rest relative to the image centre (centre = 1): white balance in the
//!    centre stays unchanged.
//! 6. Quality from fit residual, pair consistency and dust share.

use crate::cast::{rows_for, CastGrid, CastQuality, COLS};

/// One flat-field shot: `px` row by row, `width·height` pixels, `saturation` = clipping level in the same unit.
pub struct FlatShot<'a> {
    pub width: usize,
    pub height: usize,
    pub px: &'a [[f32; 3]],
    pub saturation: f32,
}

#[derive(Debug, Clone)]
pub struct FlatResult {
    /// radial vignetting a1, a2, a3 (r in units of the longer edge)
    pub vignette: [f64; 3],
    pub cast: CastGrid,
    pub quality: CastQuality,
}

/// Reduced shot: trimmed block means and the values of the check. Small (a few 10 kB); the full image is not
/// needed any more, so many shots can be collected before measuring.
#[derive(Debug, Clone)]
pub struct FlatBlocks {
    pub width: usize,
    pub height: usize,
    pub bx: usize,
    pub by: usize,
    /// trimmed mean per block and channel, row by row (`bx·by`)
    pub mean: Vec<[f32; 3]>,
    /// middle channel in the image centre relative to the saturation
    pub level: f32,
    /// share of pixels with a channel at ≥ 98 % of the saturation
    pub clipped: f32,
    /// 90th percentile over the blocks of the strongest second difference of tile means (relative): edges, texture
    pub edges: f32,
}

const MAX_CLIPPED: f32 = 0.001;
const MIN_LEVEL: f32 = 0.15;
const MAX_LEVEL: f32 = 0.85;
const MAX_EDGES: f32 = 0.10;

fn trimmed_mean(v: &mut [f32]) -> f32 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    let cut = n / 10;
    let (lo, hi) = (cut, n - cut);
    if cut > 0 {
        v.select_nth_unstable_by(lo, f32::total_cmp);
        v[lo..].select_nth_unstable_by(hi - lo - 1, f32::total_cmp);
    }
    let s: f64 = v[lo..hi].iter().map(|&x| x as f64).sum();
    (s / (hi - lo) as f64) as f32
}

fn percentile(mut v: Vec<f32>, q: f64) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let i = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(i, f32::total_cmp).1
}

/// Block statistics of a shot (step 1 and the values for [`check_blocks`]).
pub fn blocks(shot: &FlatShot) -> FlatBlocks {
    let (w, h) = (shot.width, shot.height);
    let (bx, by) = if w >= h { (96.min(w), 64.min(h)) } else { (64.min(w), 96.min(h)) };
    let sat = if shot.saturation > 0.0 { shot.saturation } else { 1.0 };
    let clip = 0.98 * sat;
    let mut mean = vec![[0f32; 3]; bx * by];
    let mut edges = Vec::with_capacity(bx * by);
    let mut clipped = 0usize;
    let mut buf: [Vec<f32>; 3] = Default::default();
    for j in 0..by {
        let (y0, y1) = (j * h / by, (j + 1) * h / by);
        for i in 0..bx {
            let (x0, x1) = (i * w / bx, (i + 1) * w / bx);
            buf.iter_mut().for_each(Vec::clear);
            // tile means of the middle channel (4×4 tiles) for the edge measure
            let mut tiles = [[0f64; 4]; 4];
            let mut tile_n = [[0u32; 4]; 4];
            for y in y0..y1 {
                let ty = ((y - y0) * 4 / (y1 - y0).max(1)).min(3);
                let row = &shot.px[y * w + x0..y * w + x1];
                for (k, p) in row.iter().enumerate() {
                    if p[0] >= clip || p[1] >= clip || p[2] >= clip {
                        clipped += 1;
                    }
                    for c in 0..3 {
                        buf[c].push(p[c]);
                    }
                    let tx = (k * 4 / (x1 - x0).max(1)).min(3);
                    tiles[ty][tx] += p[1] as f64;
                    tile_n[ty][tx] += 1;
                }
            }
            let m: [f32; 3] = std::array::from_fn(|c| trimmed_mean(&mut buf[c]));
            mean[j * bx + i] = m;
            let t: [[f64; 4]; 4] = std::array::from_fn(|a| std::array::from_fn(|b| tiles[a][b] / tile_n[a][b].max(1) as f64));
            let mut e = 0f64;
            for a in 0..4 {
                for b in 1..3 {
                    e = e.max((t[a][b - 1] - 2.0 * t[a][b] + t[a][b + 1]).abs());
                    e = e.max((t[b - 1][a] - 2.0 * t[b][a] + t[b + 1][a]).abs());
                }
            }
            edges.push((e / (m[1].abs() as f64).max(1e-9)) as f32);
        }
    }
    // level: median of the middle channel in the central blocks (|u|, |v| < 0.2)
    let mut centre = Vec::new();
    for j in 0..by {
        for i in 0..bx {
            let (u, v) = ((i as f32 + 0.5) / bx as f32 * 2.0 - 1.0, (j as f32 + 0.5) / by as f32 * 2.0 - 1.0);
            if u.abs() < 0.2 && v.abs() < 0.2 {
                centre.push(mean[j * bx + i][1]);
            }
        }
    }
    let level = percentile(centre, 0.5) / sat;
    FlatBlocks { width: w, height: h, bx, by, mean, level, clipped: clipped as f32 / (w * h).max(1) as f32, edges: percentile(edges, 0.9) }
}

/// Step 2: is the shot usable as a flat field? Error: reason for the user.
pub fn check_blocks(b: &FlatBlocks) -> Result<(), String> {
    if b.clipped > MAX_CLIPPED {
        return Err(format!("Fläche ausgebrannt ({:.1} % der Bildpunkte) – knapper belichten", b.clipped * 100.0));
    }
    if b.level < MIN_LEVEL {
        return Err(format!("Fläche zu dunkel ({:.0} % der Sättigung, nötig mindestens {:.0} %)", b.level * 100.0, MIN_LEVEL * 100.0));
    }
    if b.level > MAX_LEVEL {
        return Err(format!("Fläche zu hell ({:.0} % der Sättigung) – knapper belichten", b.level * 100.0));
    }
    if b.edges > MAX_EDGES {
        return Err("Kanten im Bild – Diffusor näher ans Objektiv, unscharf stellen".into());
    }
    Ok(())
}

pub fn check(shot: &FlatShot) -> Result<(), String> {
    check_blocks(&blocks(shot))
}

/// Measure one shot or a pair (`b` = camera turned by 180°).
pub fn measure(a: &FlatShot, b: Option<&FlatShot>) -> Result<FlatResult, String> {
    let ba = blocks(a);
    check_blocks(&ba)?;
    let bb = match b {
        Some(s) => {
            let x = blocks(s);
            check_blocks(&x).map_err(|e| format!("zweite Aufnahme: {e}"))?;
            Some(x)
        }
        None => None,
    };
    measure_blocks(&ba, bb.as_ref())
}

// ------------------------------------------------------------------------------------- fitting

fn legendre(n: usize, x: f64) -> f64 {
    match n {
        0 => 1.0,
        1 => x,
        2 => (3.0 * x * x - 1.0) / 2.0,
        3 => (5.0 * x * x * x - 3.0 * x) / 2.0,
        _ => (35.0 * x.powi(4) - 30.0 * x * x + 3.0) / 8.0,
    }
}

/// Exponents (i, j) of the Legendre terms up to total degree 4 (15 terms).
fn terms() -> Vec<(usize, usize)> {
    (0..=4).flat_map(|d| (0..=d).map(move |i| (i, d - i))).collect()
}

/// Solve the n×n system (Gauss with partial pivoting).
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for k in 0..n {
        let p = (k..n).max_by(|&i, &j| a[i][k].abs().total_cmp(&a[j][k].abs()))?;
        if a[p][k].abs() < 1e-14 {
            return None;
        }
        a.swap(k, p);
        b.swap(k, p);
        for i in k + 1..n {
            let f = a[i][k] / a[k][k];
            if f != 0.0 {
                for j in k..n {
                    a[i][j] -= f * a[k][j];
                }
                b[i] -= f * b[k];
            }
        }
    }
    let mut x = vec![0.0; n];
    for k in (0..n).rev() {
        let s: f64 = (k + 1..n).map(|j| a[k][j] * x[j]).sum();
        x[k] = (b[k] - s) / a[k][k];
    }
    Some(x)
}

fn weighted_lsq(rows: &[Vec<f64>], y: &[f64], w: &[f64]) -> Option<Vec<f64>> {
    let n = rows.first()?.len();
    let mut ata = vec![vec![0.0; n]; n];
    let mut aty = vec![0.0; n];
    for ((r, &yy), &ww) in rows.iter().zip(y).zip(w) {
        if ww <= 0.0 {
            continue;
        }
        for i in 0..n {
            aty[i] += ww * r[i] * yy;
            for j in i..n {
                ata[i][j] += ww * r[i] * r[j];
            }
        }
    }
    for i in 0..n {
        for j in 0..i {
            ata[i][j] = ata[j][i];
        }
    }
    solve(ata, aty)
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let m = v.len() / 2;
    *v.select_nth_unstable_by(m, f64::total_cmp).1
}

/// Robust fit: least squares, then three passes with Tukey weights. `scale(fit, i)` turns the residual into a
/// relative one (1 for log data). Returns coefficients and final weights.
fn robust_fit(rows: &[Vec<f64>], y: &[f64], valid: &[bool], scale: impl Fn(f64) -> f64) -> Option<(Vec<f64>, Vec<f64>)> {
    let mut w: Vec<f64> = valid.iter().map(|&v| if v { 1.0 } else { 0.0 }).collect();
    let mut beta = weighted_lsq(rows, y, &w)?;
    for _ in 0..3 {
        let res: Vec<f64> = rows
            .iter()
            .zip(y)
            .map(|(r, &yy)| {
                let f: f64 = r.iter().zip(&beta).map(|(a, b)| a * b).sum();
                (yy - f) / scale(f)
            })
            .collect();
        let live: Vec<f64> = res.iter().zip(valid).filter(|(_, &v)| v).map(|(e, _)| *e).collect();
        let med = median(live.clone());
        let mad = median(live.iter().map(|e| (e - med).abs()).collect());
        // floor: noise-free data would otherwise reject the tiniest model error
        let c = (4.685 * 1.4826 * mad).max(5e-4);
        w = res
            .iter()
            .zip(valid)
            .map(|(e, &v)| {
                if !v {
                    return 0.0;
                }
                let t = (e - med) / c;
                if t.abs() >= 1.0 {
                    0.0
                } else {
                    (1.0 - t * t).powi(2)
                }
            })
            .collect();
        beta = weighted_lsq(rows, y, &w)?;
    }
    Some((beta, w))
}

fn weighted_rms(rows: &[Vec<f64>], y: &[f64], beta: &[f64], w: &[f64]) -> f64 {
    let (mut s, mut n) = (0.0, 0.0);
    for ((r, &yy), &ww) in rows.iter().zip(y).zip(w) {
        if ww > 0.0 {
            let f: f64 = r.iter().zip(beta).map(|(a, b)| a * b).sum();
            s += ww * (yy - f).powi(2);
            n += ww;
        }
    }
    if n > 0.0 {
        (s / n).sqrt()
    } else {
        0.0
    }
}

/// Steps 3–6 on block statistics (see the module description).
pub fn measure_blocks(a: &FlatBlocks, b: Option<&FlatBlocks>) -> Result<FlatResult, String> {
    if let Some(b) = b {
        if (b.width, b.height, b.bx, b.by) != (a.width, a.height, a.bx, a.by) {
            return Err("die beiden Aufnahmen haben unterschiedliche Größe".into());
        }
    }
    let (w, h) = (a.width as f64, a.height as f64);
    let d = w.max(h);
    let rc = w.hypot(h) / 2.0 / d; // corner radius
    let (bx, by) = (a.bx, a.by);
    let n = bx * by;
    // block centres
    let pos: Vec<(f64, f64, f64)> = (0..n)
        .map(|k| {
            let (i, j) = (k % bx, k / bx);
            let xc = ((i * a.width / bx) + ((i + 1) * a.width / bx)) as f64 / 2.0;
            let yc = ((j * a.height / by) + ((j + 1) * a.height / by)) as f64 / 2.0;
            (xc / w * 2.0 - 1.0, yc / h * 2.0 - 1.0, (xc - w / 2.0).hypot(yc - h / 2.0) / d)
        })
        .collect();
    let f: Vec<[f64; 3]> = (0..n)
        .map(|k| {
            let pa = a.mean[k];
            match b {
                Some(b) => std::array::from_fn(|c| ((pa[c] as f64) * (b.mean[k][c] as f64)).max(0.0).sqrt()),
                None => std::array::from_fn(|c| pa[c] as f64),
            }
        })
        .collect();
    let valid: Vec<bool> = f.iter().map(|p| p.iter().all(|v| v.is_finite() && *v > 0.0)).collect();
    if valid.iter().filter(|v| **v).count() < n / 2 {
        return Err("zu wenige auswertbare Bereiche".into());
    }
    let mut zero = vec![false; n];
    // pair consistency
    let pair_rms = match b {
        Some(b) => {
            let rows: Vec<Vec<f64>> = pos.iter().map(|&(u, v, _)| vec![1.0, u, v]).collect();
            let ok: Vec<bool> = (0..n).map(|k| valid[k] && (0..3).all(|c| a.mean[k][c] > 0.0 && b.mean[k][c] > 0.0)).collect();
            let mut worst = 0f64;
            for c in 0..3 {
                let y: Vec<f64> = (0..n).map(|k| if ok[k] { (a.mean[k][c] as f64 / b.mean[k][c] as f64).ln() } else { 0.0 }).collect();
                let (beta, wt) = robust_fit(&rows, &y, &ok, |_| 1.0).ok_or("Paar nicht auswertbar")?;
                worst = worst.max(weighted_rms(&rows, &y, &beta, &wt));
            }
            Some(worst as f32)
        }
        None => None,
    };
    // radial vignetting of the middle channel (linear)
    let vrows: Vec<Vec<f64>> = pos.iter().map(|&(_, _, r)| vec![1.0, r * r, r.powi(4), r.powi(6)]).collect();
    let vy: Vec<f64> = f.iter().map(|p| p[1]).collect();
    let (vb, vw) = robust_fit(&vrows, &vy, &valid, |fit| fit.abs().max(1e-9)).ok_or("Vignettierung nicht ableitbar")?;
    if vb[0] <= 0.0 {
        return Err("Vignettierung nicht ableitbar".into());
    }
    let vignette = [vb[1] / vb[0], vb[2] / vb[0], vb[3] / vb[0]];
    let vfun = |r: f64| 1.0 + r * r * (vignette[0] + r * r * (vignette[1] + r * r * vignette[2]));
    // a real lens falls off monotonically and not below ~3 EV; anything else is no uniform surface
    let corner_v = vfun(rc);
    if !(0.1..=1.2).contains(&corner_v) || (1..=20).any(|k| vfun(rc * k as f64 / 20.0) > vfun(rc * (k - 1) as f64 / 20.0) + 0.02) {
        return Err(format!("Vignettierung unplausibel (Ecke {corner_v:.2}) – keine gleichmäßige Fläche"));
    }
    for k in 0..n {
        zero[k] |= valid[k] && vw[k] == 0.0;
    }
    // rest per channel in the logarithm
    let tl = terms();
    let crow = |u: f64, v: f64, r: f64| -> Vec<f64> {
        let mut z: Vec<f64> = tl.iter().map(|&(i, j)| legendre(i, u) * legendre(j, v)).collect();
        z.push((r / rc).powi(6));
        z
    };
    let rows: Vec<Vec<f64>> = pos.iter().map(|&(u, v, r)| crow(u, v, r)).collect();
    let mut betas: Vec<Vec<f64>> = Vec::new();
    let mut fit_rms = [0f32; 3];
    for c in 0..3 {
        let y: Vec<f64> = (0..n).map(|k| if valid[k] { (f[k][c] / vfun(pos[k].2).max(1e-6)).ln() } else { 0.0 }).collect();
        let (beta, wt) = robust_fit(&rows, &y, &valid, |_| 1.0).ok_or("Farbstich nicht ableitbar")?;
        fit_rms[c] = weighted_rms(&rows, &y, &beta, &wt) as f32;
        for k in 0..n {
            zero[k] |= valid[k] && wt[k] == 0.0;
        }
        betas.push(beta);
    }
    if b.is_none() {
        // single shot: a common tilt of all channels is lighting, not lens
        let iu = tl.iter().position(|&t| t == (1, 0)).unwrap();
        let iv = tl.iter().position(|&t| t == (0, 1)).unwrap();
        let (tu, tv) = (betas[1][iu], betas[1][iv]);
        for beta in betas.iter_mut() {
            beta[iu] -= tu;
            beta[iv] -= tv;
        }
    }
    let eval = |beta: &[f64], u: f64, v: f64, r: f64| -> f64 { crow(u, v, r).iter().zip(beta).map(|(a, b)| a * b).sum() };
    let centre: [f64; 3] = std::array::from_fn(|c| eval(&betas[c], 0.0, 0.0, 0.0));
    let (cols, rws) = (COLS, rows_for(a.width, a.height));
    let gain: Vec<[f32; 3]> = (0..rws as usize)
        .flat_map(|j| (0..cols as usize).map(move |i| (i, j)))
        .map(|(i, j)| {
            let (fu, fv) = ((i as f64 + 0.5) / cols as f64, (j as f64 + 0.5) / rws as f64);
            let (u, v) = (fu * 2.0 - 1.0, fv * 2.0 - 1.0);
            let r = ((fu - 0.5) * w).hypot((fv - 0.5) * h) / d;
            std::array::from_fn(|c| (eval(&betas[c], u, v, r) - centre[c]).exp() as f32)
        })
        .collect();
    if gain.iter().any(|g| g.iter().any(|v| !(0.5..=2.0).contains(v))) {
        return Err("Farbstich unplausibel (mehr als Faktor 2) – keine gleichmäßige Fläche".into());
    }
    let cast = CastGrid { cols, rows: rws, gain };
    let dust_cells = zero.iter().filter(|z| **z).count() as u32;
    let dust_pct = dust_cells as f64 / n as f64 * 100.0;
    let max_fit = fit_rms.iter().fold(0f32, |m, v| m.max(*v)) as f64;
    let raw = 100.0 - 1500.0 * max_fit - 1000.0 * pair_rms.unwrap_or(0.0) as f64 - 2.0 * dust_pct;
    let mut score = raw.clamp(0.0, 100.0).round() as u8;
    if pair_rms.is_none() {
        score = score.min(70);
    }
    let corner = cast.corner_extremes();
    Ok(FlatResult { vignette, cast, quality: CastQuality { fit_rms, pair_rms, dust_cells, corner, score } })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 1200;
    const H: usize = 800;

    /// Small deterministic random generator (xorshift) with Gaussian values (Box–Muller).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
        fn gauss(&mut self) -> f64 {
            let (a, b) = (self.next().max(1e-12), self.next());
            (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()
        }
    }

    fn rc() -> f64 {
        (W as f64).hypot(H as f64) / 2.0 / W as f64
    }

    /// Vignette with 0.55 in the corner.
    fn vig(r: f64) -> f64 {
        1.0 - 0.45 * (r / rc()).powi(2)
    }

    /// Colour cast: red +4 %, blue −5 % (with a slight tilt) towards the corner, middle channel neutral.
    fn cast_true(u: f64, v: f64, r: f64) -> [f64; 3] {
        let s = (r / rc()).powi(2);
        let _ = v;
        [1.0 + 0.04 * s, 1.0, 1.0 - 0.05 * s + 0.01 * u]
    }

    struct Spec {
        level: f64,
        tilt: f64,
        noise: f64,
        dust: usize,
        seed: u64,
    }

    fn shot(s: &Spec) -> Vec<[f32; 3]> {
        let mut rng = Rng(s.seed);
        let spots: Vec<(f64, f64)> = (0..s.dust).map(|_| (rng.next() * W as f64, rng.next() * H as f64)).collect();
        let mut px = vec![[0f32; 3]; W * H];
        for y in 0..H {
            for x in 0..W {
                let (xc, yc) = (x as f64 + 0.5, y as f64 + 0.5);
                let u = xc / W as f64 * 2.0 - 1.0;
                let v = yc / H as f64 * 2.0 - 1.0;
                let r = (xc - W as f64 / 2.0).hypot(yc - H as f64 / 2.0) / W as f64;
                let c = cast_true(u, v, r);
                let mut base = s.level * vig(r) * (1.0 + s.tilt * u);
                for &(sx, sy) in &spots {
                    let d2 = (xc - sx).powi(2) + (yc - sy).powi(2);
                    if d2 < 3600.0 {
                        base *= 1.0 - 0.15 * (-d2 / (2.0 * 144.0)).exp();
                    }
                }
                px[y * W + x] = std::array::from_fn(|k| (base * c[k] * (1.0 + s.noise * rng.gauss())) as f32);
            }
        }
        px
    }

    fn fs(px: &[[f32; 3]]) -> FlatShot<'_> {
        FlatShot { width: W, height: H, px, saturation: 1.0 }
    }

    /// Largest deviation of the measured map from the true one over the cells inside `[lo, hi]` (image fraction).
    fn cast_error(g: &CastGrid, lo: f64, hi: f64) -> f64 {
        let mut m = 0f64;
        for j in 0..g.rows as usize {
            for i in 0..g.cols as usize {
                let (fu, fv) = ((i as f64 + 0.5) / g.cols as f64, (j as f64 + 0.5) / g.rows as f64);
                if fu < lo || fu > hi || fv < lo || fv > hi {
                    continue;
                }
                let r = ((fu - 0.5) * W as f64).hypot((fv - 0.5) * H as f64) / W as f64;
                let t = cast_true(fu * 2.0 - 1.0, fv * 2.0 - 1.0, r);
                let got = g.gain[j * g.cols as usize + i];
                for k in 0..3 {
                    m = m.max((got[k] as f64 / t[k] - 1.0).abs());
                }
            }
        }
        m
    }

    fn corner_vignette(res: &FlatResult) -> f64 {
        let r = rc();
        1.0 + r * r * (res.vignette[0] + r * r * (res.vignette[1] + r * r * res.vignette[2]))
    }

    #[test]
    fn recovers_known_cast_within_0_3_percent() {
        let a = shot(&Spec { level: 0.5, tilt: 0.08, noise: 0.005, dust: 0, seed: 1 });
        let b = shot(&Spec { level: 0.5, tilt: -0.08, noise: 0.005, dust: 0, seed: 2 });
        let res = measure(&fs(&a), Some(&fs(&b))).unwrap();
        let e = cast_error(&res.cast, 0.0, 1.0);
        let vc = corner_vignette(&res);
        eprintln!("cast error {e:.5}, vignette corner {vc:.4}, quality {:?}", res.quality);
        assert!(e < 0.003, "{e}");
        assert!((vc - 0.55).abs() < 0.01, "{vc}");
        assert!(res.quality.score >= 80, "{:?}", res.quality);
        let c = res.quality.corner;
        assert!((c[0] - 1.04).abs() < 0.006 && c[1] > 0.99 && c[1] < 1.01 && c[2] < 0.96, "{c:?}");
    }

    #[test]
    fn dust_spots_are_ignored() {
        let clean = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.005, dust: 0, seed: 3 });
        let dusty = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.005, dust: 30, seed: 3 });
        let rc = measure(&fs(&clean), None).unwrap();
        let rd = measure(&fs(&dusty), None).unwrap();
        let m = rc.cast.gain.iter().zip(&rd.cast.gain).flat_map(|(a, b)| (0..3).map(move |k| (b[k] / a[k] - 1.0).abs())).fold(0f32, f32::max);
        eprintln!("dust: largest difference {m:.5}, dust cells {} (clean {})", rd.quality.dust_cells, rc.quality.dust_cells);
        assert!(m < 0.002, "{m}");
        assert!((corner_vignette(&rd) - corner_vignette(&rc)).abs() < 0.002);
        assert!(rd.quality.dust_cells > rc.quality.dust_cells, "{} vs {}", rd.quality.dust_cells, rc.quality.dust_cells);
    }

    #[test]
    fn single_shot_caps_score_at_70() {
        let a = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.005, dust: 0, seed: 4 });
        let res = measure(&fs(&a), None).unwrap();
        assert!(res.quality.pair_rms.is_none());
        assert_eq!(res.quality.score, 70, "{:?}", res.quality);
    }

    #[test]
    fn gradient_without_pair_biases_less_than_1_percent_in_center() {
        let a = shot(&Spec { level: 0.5, tilt: 0.08, noise: 0.0, dust: 0, seed: 5 });
        let res = measure(&fs(&a), None).unwrap();
        let e = cast_error(&res.cast, 0.25, 0.75);
        eprintln!("single shot with gradient: error in the centre half {e:.5}, whole image {:.5}", cast_error(&res.cast, 0.0, 1.0));
        assert!(e < 0.01, "{e}");
    }

    #[test]
    fn rejects_clipped_dark_and_edgy() {
        let mut clipped = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.005, dust: 0, seed: 6 });
        for p in clipped.iter_mut().step_by(100) {
            *p = [1.0; 3];
        }
        let e = check(&fs(&clipped)).unwrap_err();
        assert!(e.contains("ausgebrannt"), "{e}");
        let dark = shot(&Spec { level: 0.05, tilt: 0.0, noise: 0.005, dust: 0, seed: 7 });
        let e = check(&fs(&dark)).unwrap_err();
        assert!(e.contains("zu dunkel"), "{e}");
        let mut board = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.005, dust: 0, seed: 8 });
        for (k, p) in board.iter_mut().enumerate() {
            let (x, y) = (k % W, k / W);
            if ((x / 40) + (y / 40)) % 2 == 1 {
                p.iter_mut().for_each(|v| *v *= 0.4);
            }
        }
        let e = check(&fs(&board)).unwrap_err();
        assert!(e.contains("Kanten"), "{e}");
        let good = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.005, dust: 30, seed: 9 });
        assert_eq!(check(&fs(&good)), Ok(()));
    }

    #[test]
    fn center_is_one() {
        let a = shot(&Spec { level: 0.4, tilt: 0.05, noise: 0.005, dust: 5, seed: 10 });
        let b = shot(&Spec { level: 0.4, tilt: -0.05, noise: 0.005, dust: 5, seed: 11 });
        let res = measure(&fs(&a), Some(&fs(&b))).unwrap();
        let c = res.cast.at(0.5, 0.5);
        assert!(c.iter().all(|v| (v - 1.0).abs() < 1e-4), "{c:?}");
    }

    /// A normal photo forced through the measurement gives no map.
    #[test]
    fn scene_is_refused_even_when_forced() {
        let mut px = shot(&Spec { level: 0.4, tilt: 0.0, noise: 0.005, dust: 0, seed: 13 });
        for (k, p) in px.iter_mut().enumerate() {
            let (x, y) = (k % W, k / W);
            if x > 800 && y < 300 {
                p.iter_mut().for_each(|v| *v *= 0.05);
            }
            if x < 200 {
                p.iter_mut().for_each(|v| *v *= 1.9);
            }
        }
        let b = blocks(&fs(&px));
        assert!(measure_blocks(&b, None).is_err());
    }

    #[test]
    fn pair_of_different_size_is_refused() {
        let a = shot(&Spec { level: 0.5, tilt: 0.0, noise: 0.0, dust: 0, seed: 12 });
        let small: Vec<[f32; 3]> = a[..W * (H - 2)].to_vec();
        let b = FlatShot { width: W, height: H - 2, px: &small, saturation: 1.0 };
        assert!(measure(&fs(&a), Some(&b)).is_err());
    }
}
