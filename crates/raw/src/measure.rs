//! Lens measurement: estimate distortion, lateral chromatic aberration and vignetting from test shots and output them as a
//! profile ([`crate::lcp::Profile`]) that [`crate::lens`] applies as an own profile before all other
//! sources.
//!
//! Methods (all formulas from the literature, rewritten in Rust):
//! - **Distortion**: plumb-line method (Devernay & Faugeras 2001): edges are found with subpixel accuracy,
//!   linked into chains (gaps at crossings are bridged) and the radial model
//!   `x_q = x·(1 + k1 r² + k2 r⁴ + k3 r⁶)` (as in the Adobe camera model, `r` in units of the longer image edge)
//!   is determined so that all chains are straight after undistortion (Levenberg–Marquardt, robust against
//!   chains that are actually curved). Suitable are grids, tiles, facades – any subject with
//!   many long straight edges across the whole image.
//! - **Lateral CA**: position of the same edges in red and blue against green along the edge normal
//!   (parabola over the gradient profile), radial scaling `1 + a r² + b r⁴` by robust regression.
//! - **Vignetting**: uniform bright surface (linear raw data): radial profile, fit of
//!   `1 + a1 r² + a2 r⁴ + a3 r⁶` (source coordinates, corrected by dividing).

use crate::lcp::{Entry, Model, Profile, Vignette};

/// Linear RGB image (0…1 relative to the white point, any white balance).
pub struct MeasureImage<'a> {
    pub width: usize,
    pub height: usize,
    pub px: &'a [[f32; 3]],
}

// ---------------------------------------------------------------------------------------------
// Image processing

struct GrayImage {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

/// Green channel, area-preserving downscaled by an integer factor, with sRGB-like encoding
/// (edges equally visible in bright and dark tones). Returns: image, factor.
fn gray_downscaled(b: &MeasureImage, target: usize) -> (GrayImage, usize) {
    let long = b.width.max(b.height);
    let n = long.div_ceil(target.max(64)).max(1);
    let (w, h) = (b.width / n, b.height / n);
    let mut v = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for dy in 0..n {
                let row = (y * n + dy) * b.width + x * n;
                for dx in 0..n {
                    s += b.px[row + dx][1].clamp(0.0, 1.0);
                }
            }
            let m = s / (n * n) as f32;
            v[y * w + x] = m.sqrt();
        }
    }
    (GrayImage { w, h, v }, n)
}

fn gauss_kernel(sigma: f32) -> Vec<f32> {
    let r = (sigma * 3.0).ceil() as i32;
    let mut k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let s: f32 = k.iter().sum();
    k.iter_mut().for_each(|x| *x /= s);
    k
}

fn blur(g: &GrayImage, sigma: f32) -> GrayImage {
    let k = gauss_kernel(sigma);
    let r = (k.len() / 2) as i32;
    let (w, h) = (g.w, g.h);
    let mut t = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = (x as i32 + i as i32 - r).clamp(0, w as i32 - 1) as usize;
                s += kv * g.v[y * w + xx];
            }
            t[y * w + x] = s;
        }
    }
    let mut o = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let yy = (y as i32 + i as i32 - r).clamp(0, h as i32 - 1) as usize;
                s += kv * t[yy * w + x];
            }
            o[y * w + x] = s;
        }
    }
    GrayImage { w, h, v: o }
}

#[derive(Clone, Copy)]
struct Edge {
    pos: [f32; 2],
    /// direction of the edge (perpendicular to the gradient), 0…π
    angle: f32,
    step_no: f32,
}

/// Edge pixels (maximum of the gradient magnitude along the gradient) with subpixel position.
/// Returns: edges and pixel index (−1 = no edge).
fn find_edges(g: &GrayImage) -> (Vec<Edge>, Vec<i32>) {
    let (w, h) = (g.w, g.h);
    let b = blur(g, 1.2);
    let mut gx = vec![0f32; w * h];
    let mut gy = vec![0f32; w * h];
    let mut mag = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let dx = (b.v[i + 1] - b.v[i - 1]) * 0.5;
            let dy = (b.v[i + w] - b.v[i - w]) * 0.5;
            gx[i] = dx;
            gy[i] = dy;
            mag[i] = dx.hypot(dy);
        }
    }
    // threshold: fraction of the high percentile of the gradient magnitude
    let mut probe: Vec<f32> = mag.iter().step_by(7).copied().filter(|m| *m > 0.0).collect();
    probe.sort_by(f32::total_cmp);
    let p99 = probe.get(probe.len() * 99 / 100).copied().unwrap_or(0.0);
    let threshold = (0.18 * p99).max(0.01);
    let samp = |x: f32, y: f32| -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (xi, yi) = (x0 as usize, y0 as usize);
        let a = mag[yi * w + xi];
        let bb = mag[yi * w + xi + 1];
        let c = mag[(yi + 1) * w + xi];
        let d = mag[(yi + 1) * w + xi + 1];
        a * (1.0 - fx) * (1.0 - fy) + bb * fx * (1.0 - fy) + c * (1.0 - fx) * fy + d * fx * fy
    };
    let mut edges = Vec::new();
    let mut idx = vec![-1i32; w * h];
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let i = y * w + x;
            let m = mag[i];
            if m < threshold {
                continue;
            }
            let (nx, ny) = (gx[i] / m, gy[i] / m);
            let (xf, yf) = (x as f32, y as f32);
            let (before_s, after_s) = (samp(xf - nx, yf - ny), samp(xf + nx, yf + ny));
            if m < before_s || m <= after_s {
                continue;
            }
            let denom = before_s - 2.0 * m + after_s;
            let d = if denom.abs() > 1e-9 { (0.5 * (before_s - after_s) / denom).clamp(-0.5, 0.5) } else { 0.0 };
            let angle = (ny.atan2(nx) + std::f32::consts::FRAC_PI_2).rem_euclid(std::f32::consts::PI);
            idx[i] = edges.len() as i32;
            edges.push(Edge { pos: [xf + 0.5 + d * nx, yf + 0.5 + d * ny], angle, step_no: m });
        }
    }
    (edges, idx)
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(std::f32::consts::PI);
    d.min(std::f32::consts::PI - d)
}

/// Link edge pixels into chains: neighbors with similar direction, along the edge direction.
fn build_chains(edges: &[Edge], idx: &[i32], w: usize, h: usize) -> Vec<Vec<[f32; 2]>> {
    let mut visited_px = vec![false; edges.len()];
    let mut chains = Vec::new();
    let max_angle = 25f32.to_radians();
    // pixel coordinates of the edge pixel from the index
    let mut pix = vec![(0i32, 0i32); edges.len()];
    for y in 0..h {
        for x in 0..w {
            let i = idx[y * w + x];
            if i >= 0 {
                pix[i as usize] = (x as i32, y as i32);
            }
        }
    }
    let walk = |start: usize, forward_dir: bool, visited_px: &mut Vec<bool>| -> Vec<usize> {
        let mut path_px = Vec::new();
        let mut cur = start;
        let t0 = edges[start].angle;
        let mut heading = if forward_dir { [t0.cos(), t0.sin()] } else { [-t0.cos(), -t0.sin()] };
        loop {
            let (cx, cy) = pix[cur];
            let mut best: Option<(usize, f32, [f32; 2])> = None;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = idx[ny as usize * w + nx as usize];
                    if j < 0 || visited_px[j as usize] {
                        continue;
                    }
                    let j = j as usize;
                    let advance = [dx as f32, dy as f32];
                    let l = (advance[0] * advance[0] + advance[1] * advance[1]).sqrt();
                    let align = (advance[0] * heading[0] + advance[1] * heading[1]) / l;
                    if align < 0.55 {
                        continue;
                    }
                    let wd = angle_diff(edges[j].angle, edges[cur].angle);
                    if wd > max_angle {
                        continue;
                    }
                    let points = wd + 0.3 * (l - 1.0) + 0.3 * (1.0 - align);
                    if best.map_or(true, |b| points < b.1) {
                        best = Some((j, points, [advance[0] / l, advance[1] / l]));
                    }
                }
            }
            let Some((j, _, s)) = best else { break };
            visited_px[j] = true;
            path_px.push(j);
            heading = [0.6 * heading[0] + 0.4 * s[0], 0.6 * heading[1] + 0.4 * s[1]];
            let l = heading[0].hypot(heading[1]).max(1e-6);
            heading = [heading[0] / l, heading[1] / l];
            cur = j;
        }
        path_px
    };
    for s in 0..edges.len() {
        if visited_px[s] {
            continue;
        }
        visited_px[s] = true;
        let inverse = walk(s, false, &mut visited_px);
        let before_s = walk(s, true, &mut visited_px);
        let mut chain: Vec<[f32; 2]> = inverse.iter().rev().map(|&i| edges[i].pos).collect();
        chain.push(edges[s].pos);
        chain.extend(before_s.iter().map(|&i| edges[i].pos));
        if chain.len() >= 8 {
            chains.push(chain);
        }
    }
    chains
}

fn end_direction(k: &[[f32; 2]], at_end: bool) -> [f32; 2] {
    let n = k.len();
    let m = 10.min(n - 1);
    let (a, b) = if at_end { (k[n - 1 - m], k[n - 1]) } else { (k[m], k[0]) };
    let d = [b[0] - a[0], b[1] - a[1]];
    let l = d[0].hypot(d[1]).max(1e-6);
    [d[0] / l, d[1] / l]
}

/// Merge chains when their ends lie close together and point in the same direction
/// (the grid lines break apart at the crossings). Candidates via a grid, linking by
/// smallest gap; each end is linked at most once, loops are excluded.
fn join_chains(chains: Vec<Vec<[f32; 2]>>, gap: f32) -> Vec<Vec<[f32; 2]>> {
    let max_cos = 14f32.to_radians().cos();
    let n = chains.len();
    // end e: 2·i = start of chain i, 2·i + 1 = end
    let end_pos = |e: usize| -> [f32; 2] {
        let k = &chains[e / 2];
        if e % 2 == 1 { k[k.len() - 1] } else { k[0] }
    };
    let end_dir: Vec<[f32; 2]> = (0..2 * n).map(|e| end_direction(&chains[e / 2], e % 2 == 1)).collect();
    let cell = gap.max(1.0);
    let mut raster: std::collections::HashMap<(i32, i32), Vec<usize>> = std::collections::HashMap::new();
    for e in 0..2 * n {
        let p = end_pos(e);
        raster.entry(((p[0] / cell) as i32, (p[1] / cell) as i32)).or_default().push(e);
    }
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for e in 0..2 * n {
        let p = end_pos(e);
        let (cx, cy) = ((p[0] / cell) as i32, (p[1] / cell) as i32);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let Some(list) = raster.get(&(cx + dx, cy + dy)) else { continue };
                for &f in list {
                    if f <= e || f / 2 == e / 2 {
                        continue;
                    }
                    let q = end_pos(f);
                    let d = [q[0] - p[0], q[1] - p[1]];
                    let l = d[0].hypot(d[1]);
                    if l > gap {
                        continue;
                    }
                    let (te, tf) = (end_dir[e], end_dir[f]);
                    let dn = if l > 1e-6 { [d[0] / l, d[1] / l] } else { te };
                    if te[0] * dn[0] + te[1] * dn[1] < max_cos || -(te[0] * tf[0] + te[1] * tf[1]) < max_cos {
                        continue;
                    }
                    pairs.push((l, e, f));
                }
            }
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut joined: Vec<Option<usize>> = vec![None; 2 * n];
    let mut dsu: Vec<usize> = (0..n).collect();
    fn root_of(d: &mut Vec<usize>, mut x: usize) -> usize {
        while d[x] != x {
            d[x] = d[d[x]];
            x = d[x];
        }
        x
    }
    for (_, e, f) in pairs {
        if joined[e].is_some() || joined[f].is_some() {
            continue;
        }
        let (a, b) = (root_of(&mut dsu, e / 2), root_of(&mut dsu, f / 2));
        if a == b {
            continue;
        }
        dsu[a] = b;
        joined[e] = Some(f);
        joined[f] = Some(e);
    }
    // walk paths along the links: start at free ends
    let mut used = vec![false; n];
    let mut chains_out = Vec::new();
    for start in 0..n {
        if used[start] {
            continue;
        }
        // go to a free end of the path
        let mut c = start;
        let mut entry = 0usize; // start
        let mut counter = 0;
        // backwards to the free end: from the start of chain `c` via the link to the previous one
        while let Some(f) = joined[2 * c + entry] {
            // f is an end of another chain; we enter it there and leave it at the other end
            let (c2, e2) = (f / 2, f % 2);
            c = c2;
            entry = 1 - e2;
            counter += 1;
            if counter > n + 1 {
                break;
            }
        }
        // now enter c at the free end `entry` and collect forwards
        let mut path: Vec<[f32; 2]> = Vec::new();
        let mut cur = c;
        let mut entered = entry;
        loop {
            used[cur] = true;
            let k = &chains[cur];
            if entered == 0 {
                path.extend(k.iter().copied());
            } else {
                path.extend(k.iter().rev().copied());
            }
            let from_tail = 2 * cur + (1 - entered);
            match joined[from_tail] {
                Some(f) if !used[f / 2] => {
                    cur = f / 2;
                    entered = f % 2;
                }
                _ => break,
            }
        }
        chains_out.push(path);
    }
    chains_out
}

// ---------------------------------------------------------------------------------------------
// Distortion model (unit: longer image edge D = 1, image center at the origin)

#[inline]
fn forward_dir(k: &[f64; 3], r: f64) -> f64 {
    let r2 = r * r;
    r * (1.0 + r2 * (k[0] + r2 * (k[1] + r2 * k[2])))
}

/// Find r_u with vorwaerts(k, r_u) = r_q (Newton from r_q); false if the mapping is not invertible there.
#[inline]
fn backward(k: &[f64; 3], rq: f64) -> (f64, bool) {
    let mut r = rq;
    for _ in 0..8 {
        let r2 = r * r;
        let f = r * (1.0 + r2 * (k[0] + r2 * (k[1] + r2 * k[2]))) - rq;
        let d = 1.0 + r2 * (3.0 * k[0] + r2 * (5.0 * k[1] + 7.0 * r2 * k[2]));
        if d < 0.05 {
            return (r, false);
        }
        let n = r - f / d;
        if (n - r).abs() < 1e-12 {
            r = n;
            break;
        }
        r = n.max(0.0);
    }
    (r, true)
}

/// Result of the distortion measurement.
#[derive(Debug, Clone)]
pub struct Distortion {
    /// coefficients k1…k3 of the Adobe model for `fx = 1` (unit: longer image edge)
    pub k: [f64; 3],
    /// spread of the edge points around their line in pixels of the full image: without correction / with
    pub rms_before: f64,
    pub rms_after: f64,
    pub chains: usize,
    pub points: usize,
    /// largest radius (fraction of the half image diagonal) up to which edge points exist
    pub coverage: f64,
    /// number of fitted coefficients (1…3)
    pub order: usize,
    /// fraction of the 24 image regions (3 rings × 8 sectors) that contain line points (0…1)
    pub spread: f64,
}

struct ChainPoints {
    /// per chain: points in units of D (image center = origin)
    chains: Vec<Vec<[f64; 2]>>,
    weight: Vec<f64>,
}

/// Residual of the chains (perpendicular distance to the fitted line after undistortion, divided by the
/// mean scale of the undistortion) as a vector; additionally the RMS value per chain.
fn leftovers(kp: &ChainPoints, k: &[f64; 3], rmax: f64) -> (Vec<f64>, Vec<f64>) {
    // check invertibility up to rmax: otherwise high penalty
    let mut ok = true;
    let mut r = 0.0;
    while r <= rmax * 1.05 {
        let r2 = r * r;
        if 1.0 + r2 * (3.0 * k[0] + r2 * (5.0 * k[1] + 7.0 * r2 * k[2])) < 0.1 {
            ok = false;
            break;
        }
        r += rmax / 20.0;
    }
    let mut all = Vec::new();
    let mut rms = Vec::with_capacity(kp.chains.len());
    for (ci, chain) in kp.chains.iter().enumerate() {
        if !ok {
            all.extend(std::iter::repeat(1.0).take(chain.len()));
            rms.push(1.0);
            continue;
        }
        let mut u: Vec<[f64; 2]> = Vec::with_capacity(chain.len());
        let mut ms = 0.0;
        for p in chain {
            let rq = p[0].hypot(p[1]);
            if rq < 1e-9 {
                u.push(*p);
                ms += 1.0;
                continue;
            }
            let (ru, _) = backward(k, rq);
            let s = ru / rq;
            u.push([p[0] * s, p[1] * s]);
            ms += s;
        }
        let n = u.len() as f64;
        let m = (ms / n).max(1e-6);
        let (mut cx, mut cy) = (0.0, 0.0);
        for p in &u {
            cx += p[0];
            cy += p[1];
        }
        cx /= n;
        cy /= n;
        let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
        for p in &u {
            let (dx, dy) = (p[0] - cx, p[1] - cy);
            sxx += dx * dx;
            sxy += dx * dy;
            syy += dy * dy;
        }
        // principal axis of the covariance: angle; normal = direction of smallest spread
        let th = 0.5 * (2.0 * sxy).atan2(sxx - syy);
        let (nx, ny) = (-th.sin(), th.cos());
        let w = kp.weight[ci].sqrt();
        let mut q = 0.0;
        for p in &u {
            let e = ((p[0] - cx) * nx + (p[1] - cy) * ny) / m;
            all.push(e * w);
            q += e * e;
        }
        rms.push((q / n).sqrt());
    }
    (all, rms)
}

fn norm2(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum()
}

/// Levenberg–Marquardt over the first `n` coefficients.
fn adjust(kp: &ChainPoints, start: [f64; 3], n: usize, rmax: f64) -> [f64; 3] {
    let mut k = start;
    let (mut r, _) = leftovers(kp, &k, rmax);
    let mut lambda = 1e-3;
    for _ in 0..40 {
        let h = 1e-5;
        let mut jac: Vec<Vec<f64>> = Vec::new();
        for j in 0..n {
            let mut k2 = k;
            k2[j] += h;
            let (r2, _) = leftovers(kp, &k2, rmax);
            jac.push(r.iter().zip(&r2).map(|(a, b)| (b - a) / h).collect());
        }
        // normal equations
        let mut a = [[0f64; 3]; 3];
        let mut g = [0f64; 3];
        for i in 0..n {
            for j in 0..n {
                a[i][j] = jac[i].iter().zip(&jac[j]).map(|(x, y)| x * y).sum();
            }
            g[i] = jac[i].iter().zip(&r).map(|(x, y)| x * y).sum();
        }
        let f0 = norm2(&r);
        let mut improved = false;
        for _ in 0..8 {
            let mut m = a;
            for i in 0..n {
                m[i][i] *= 1.0 + lambda;
                m[i][i] += 1e-12;
            }
            let Some(d) = solve3(&m, &g, n) else {
                lambda *= 10.0;
                continue;
            };
            let mut k2 = k;
            for i in 0..n {
                k2[i] -= d[i];
            }
            let (r2, _) = leftovers(kp, &k2, rmax);
            if norm2(&r2) < f0 {
                let rel = (f0 - norm2(&r2)) / f0.max(1e-30);
                k = k2;
                r = r2;
                lambda = (lambda * 0.3).max(1e-9);
                improved = true;
                if rel < 1e-7 {
                    return k;
                }
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    k
}

/// Linear system of equations (n ≤ 3, Gaussian elimination with column pivoting).
fn solve3(a: &[[f64; 3]; 3], b: &[f64; 3], n: usize) -> Option<[f64; 3]> {
    let mut m = *a;
    let mut v = *b;
    for c in 0..n {
        let p = (c..n).max_by(|&i, &j| m[i][c].abs().total_cmp(&m[j][c].abs()))?;
        if m[p][c].abs() < 1e-30 {
            return None;
        }
        m.swap(c, p);
        v.swap(c, p);
        for r in c + 1..n {
            let f = m[r][c] / m[c][c];
            for cc in c..n {
                m[r][cc] -= f * m[c][cc];
            }
            v[r] -= f * v[c];
        }
    }
    let mut x = [0f64; 3];
    for c in (0..n).rev() {
        let mut s = v[c];
        for cc in c + 1..n {
            s -= m[c][cc] * x[cc];
        }
        x[c] = s / m[c][c];
    }
    Some(x)
}

/// Deviation of the chain from its chord, relative to its length (rough check whether it can be a line at all).
fn sag(k: &[[f32; 2]]) -> f32 {
    let (a, z) = (k[0], k[k.len() - 1]);
    let (dx, dy) = (z[0] - a[0], z[1] - a[1]);
    let l = dx.hypot(dy).max(1e-3);
    let (nx, ny) = (-dy / l, dx / l);
    k.iter().map(|p| ((p[0] - a[0]) * nx + (p[1] - a[1]) * ny).abs()).fold(0.0, f32::max) / l
}

/// Edges and chains of an image (downscaled to at most `target` pixels edge length).
/// Returns: chains in pixels of the **full** image (origin top left).
pub fn find_chains(b: &MeasureImage, target: usize) -> Vec<Vec<[f32; 2]>> {
    let (g, n) = gray_downscaled(b, target);
    if g.w < 32 || g.h < 32 {
        return Vec::new();
    }
    let (edges, idx) = find_edges(&g);
    let chains = build_chains(&edges, &idx, g.w, g.h);
    let long = g.w.max(g.h) as f32;
    let chains = join_chains(chains, 0.012 * long + 6.0);
    let min_length = 0.08 * long;
    let f = n as f32;
    chains
        .into_iter()
        .filter(|k| {
            let (a, z) = (k[0], k[k.len() - 1]);
            (a[0] - z[0]).hypot(a[1] - z[1]) >= min_length && sag(k) < 0.05
        })
        .map(|k| k.into_iter().map(|p| [p[0] * f, p[1] * f]).collect())
        .collect()
}

/// Mean residual of the lines: median of the chain RMS values in pixels (robust against spurious lines).
fn median_rms(rms: &[f64], d: f64) -> f64 {
    let mut v = rms.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0) * d
}

/// Weighted sum of squares of the residuals and weighted point count (for the F-test of the order).
fn sum_sq(kp: &ChainPoints, rms: &[f64]) -> (f64, f64) {
    let tau = 0.8 / (1.0 * 4000.0);
    let _ = tau;
    let (mut s, mut n) = (0.0, 0.0);
    for ((c, r), g) in kp.chains.iter().zip(rms).zip(&kp.weight) {
        let w = c.len() as f64 * g;
        s += r * r * w;
        n += w;
    }
    (s, n)
}

/// Mean residual of the points (pixels), outlier chains enter with a small weight.
fn rms_mean(kp: &ChainPoints, rms: &[f64], d: f64) -> f64 {
    let tau = 0.8 / d;
    let (mut s, mut n) = (0.0, 0.0);
    for (c, r) in kp.chains.iter().zip(rms) {
        let w = c.len() as f64 / (1.0 + (r / tau).powi(2));
        s += r * r * w;
        n += w;
    }
    (s / n.max(1e-12)).sqrt() * d
}

/// Estimate the distortion from the chains (pixels of the full image `width`×`height`).
pub fn estimate_distortion(chains: &[Vec<[f32; 2]>], width: usize, height: usize) -> Result<Distortion, String> {
    let d = width.max(height) as f64;
    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
    let rmax = (width as f64).hypot(height as f64) / 2.0 / d;
    // points in units of D; at most ~200 points per chain
    let chain_norm: Vec<Vec<[f64; 2]>> = chains
        .iter()
        .filter(|k| k.len() >= 12)
        .map(|k| {
            let advance = (k.len() / 200).max(1);
            k.iter().step_by(advance).map(|p| [(p[0] as f64 - cx) / d, (p[1] as f64 - cy) / d]).collect()
        })
        .collect();
    if chain_norm.len() < MIN_CHAINS {
        return Err(format!("zu wenige gerade Linien gefunden ({}, nötig sind mindestens {MIN_CHAINS})", chain_norm.len()));
    }
    let mut kp = ChainPoints { weight: vec![1.0; chain_norm.len()], chains: chain_norm };
    let (_, rms0) = leftovers(&kp, &[0.0; 3], rmax);
    let rms_before = median_rms(&rms0, d);
    let mut k = [0.0f64; 3];
    // The tolerance decreases from coarse to fine; spurious chains (curves in the subject) drop out. The order grows
    // as long as the fit gets clearly better.
    // The tolerance (Cauchy weight) decreases step by step from coarse to fine, so that strongly distorted lines at the edge
    // count at first; spurious chains (curves in the subject) are only rejected after the coarse fit.
    let tolerances = [10.0 / d, 5.0 / d, 2.5 / d, 1.4 / d, 0.8 / d];
    let mut outcomes: Vec<([f64; 3], f64, f64, f64)> = Vec::new();
    for step_no in 1..=3 {
        for (pass_no, tau) in tolerances.iter().enumerate() {
            let (_, rms) = leftovers(&kp, &k, rmax);
            for (g, r) in kp.weight.iter_mut().zip(&rms) {
                *g = 1.0 / (1.0 + (r / tau).powi(2));
            }
            if pass_no == 3 && step_no == 1 {
                let mut sorted = rms.clone();
                sorted.sort_by(f64::total_cmp);
                let med = sorted[sorted.len() / 2];
                let cutoff = (5.0 * med).max(3.0 * tau);
                let kept: Vec<usize> = (0..kp.chains.len()).filter(|&i| rms[i] <= cutoff).collect();
                if kept.len() >= 6 && kept.len() < kp.chains.len() {
                    kp.chains = kept.iter().map(|&i| kp.chains[i].clone()).collect();
                    kp.weight = kept.iter().map(|&i| kp.weight[i]).collect();
                }
            }
            k = adjust(&kp, k, step_no, rmax);
        }
        // after each step reject chains that fit clearly worse than the majority, and refit
        if step_no < 3 {
            let (_, rms) = leftovers(&kp, &k, rmax);
            let mut sorted = rms.clone();
            sorted.sort_by(f64::total_cmp);
            let med = sorted[sorted.len() / 2];
            let cutoff = (3.5 * med).max(1.6 / d);
            let kept: Vec<usize> = (0..kp.chains.len()).filter(|&i| rms[i] <= cutoff).collect();
            if kept.len() >= 8 && kept.len() < kp.chains.len() {
                kp.chains = kept.iter().map(|&i| kp.chains[i].clone()).collect();
                kp.weight = kept.iter().map(|&i| kp.weight[i]).collect();
                k = adjust(&kp, k, step_no, rmax);
            }
        }
        let (_, rms) = leftovers(&kp, &k, rmax);
        let g = rms_mean(&kp, &rms, d);
        let (ss, nn) = sum_sq(&kp, &rms);
        if std::env::var_os("GRAINEO_LENS_MEASURE_LOG").is_some() {
            eprintln!("step {step_no}: k = {k:?}, median {:.3} px, mean {g:.3} px, {} chains", median_rms(&rms, d), kp.chains.len());
        }
        outcomes.push((k, g, ss, nn));
    }
    let mut best_order = 1;
    for step_no in 2..=3 {
        // F-test: does the additional coefficient explain more than the noise? (one value per point, chains
        // are sufficiently independent because at most ~200 points per chain are used)
        let (ss_old, ss_new, n) = (outcomes[best_order - 1].2, outcomes[step_no - 1].2, outcomes[step_no - 1].3);
        let f = (ss_old - ss_new) * (n - step_no as f64).max(1.0) / ss_new.max(1e-30);
        if f > 60.0 && ss_new < ss_old * 0.985 {
            best_order = step_no;
        }
    }
    k = outcomes[best_order - 1].0;
    let (_, rms) = leftovers(&kp, &k, rmax);
    let (_, rms_v) = leftovers(&kp, &[0.0; 3], rmax);
    let mut max_dev = 0f64;
    let mut points = 0usize;
    let mut cells = [0usize; 24];
    for c in &kp.chains {
        points += c.len();
        for p in c {
            let r = p[0].hypot(p[1]);
            max_dev = max_dev.max(r);
            let ring = ((r / rmax) * 3.0).min(2.999) as usize;
            let sector = (((p[1].atan2(p[0]) + std::f64::consts::PI) / (2.0 * std::f64::consts::PI)) * 8.0).min(7.999) as usize;
            cells[ring * 8 + sector] += 1;
        }
    }
    let spread = cells.iter().filter(|&&n| n >= 30).count() as f64 / 24.0;
    let _ = rms_before;
    let outcome = Distortion {
        k,
        rms_before: median_rms(&rms_v, d),
        rms_after: median_rms(&rms, d),
        chains: kp.chains.len(),
        points,
        coverage: max_dev / rmax,
        order: best_order,
        spread,
    };
    // plausibility: with few lines or made-up values the fit is worthless
    let largest = (1..=20).map(|i| (ratio_val(&outcome.k, i as f64 / 20.0 * rmax) - 1.0).abs()).fold(0.0, f64::max);
    if outcome.points < MIN_POINTS {
        return Err(format!("zu wenig Linienpunkte ({}, nötig sind {MIN_POINTS})", outcome.points));
    }
    if outcome.spread < 0.4 {
        return Err(format!("die geraden Linien liegen nur in einem Teil des Bildes (Abdeckung {:.0} %) – ein Gitter oder Motiv mit Linien über das ganze Bild nötig", outcome.spread * 100.0));
    }
    if largest > 0.30 {
        return Err(format!("unplausible Verzeichnung ({:.0} %) – die Linien im Bild sind nicht gerade genug", largest * 100.0));
    }
    Ok(outcome)
}

const MIN_CHAINS: usize = 12;
const MIN_POINTS: usize = 2500;

/// Convert distortion `k` (unit D, fx = 1) into an Adobe model with normalization `fx`
/// (`k_j' = k_j · fx^(2j)`).
pub fn model_from_k(k: [f64; 3], fx: f64) -> Model {
    let f2 = fx * fx;
    Model { fx, fy: fx, cx: 0.5, cy: 0.5, k: [k[0] * f2, k[1] * f2 * f2, k[2] * f2 * f2 * f2], p: [0.0; 2], scale: 1.0 }
}

/// Undistort chains: points in pixels of the full image → undistorted points (for checking, report).
pub fn undistort(k: &[f64; 3], p: [f32; 2], width: usize, height: usize) -> [f32; 2] {
    let d = width.max(height) as f64;
    let (cx, cy) = (width as f64 / 2.0, height as f64 / 2.0);
    let (x, y) = ((p[0] as f64 - cx) / d, (p[1] as f64 - cy) / d);
    let rq = x.hypot(y);
    if rq < 1e-12 {
        return p;
    }
    let (ru, _) = backward(k, rq);
    let s = ru / rq;
    [(x * s * d + cx) as f32, (y * s * d + cy) as f32]
}

/// Resample an image radially: `source(r)` returns the source radius for a target radius (unit D, center = origin).
fn radial_convert(b: &MeasureImage, source: impl Fn(f64) -> f64 + Sync) -> Vec<[f32; 3]> {
    use rayon::prelude::*;
    let (w, h) = (b.width, b.height);
    let d = w.max(h) as f64;
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let mut chains_out = vec![[0f32; 3]; w * h];
    chains_out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (px, py) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
            let r = px.hypot(py) / d;
            let s = if r < 1e-12 { 1.0 } else { source(r) / r };
            let (sx, sy) = (cx + px * s - 0.5, cy + py * s - 0.5);
            if sx < 0.0 || sy < 0.0 || sx > w as f64 - 1.001 || sy > h as f64 - 1.001 {
                continue;
            }
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (fx, fy) = ((sx - x0 as f64) as f32, (sy - y0 as f64) as f32);
            let g = |xx: usize, yy: usize| b.px[yy * w + xx];
            let (a, bb, c, dd) = (g(x0, y0), g(x0 + 1, y0), g(x0, y0 + 1), g(x0 + 1, y0 + 1));
            for k in 0..3 {
                o[k] = a[k] * (1.0 - fx) * (1.0 - fy) + bb[k] * fx * (1.0 - fy) + c[k] * (1.0 - fx) * fy + dd[k] * fx * fy;
            }
        }
    });
    chains_out
}

/// Apply distortion `k` to an image (probe and self-test): an ideal point at radius r appears at
/// `forward_dir(k, r)`.
pub fn distort_img(b: &MeasureImage, k: &[f64; 3]) -> Vec<[f32; 3]> {
    let k = *k;
    radial_convert(b, move |r| backward(&k, r).0)
}

/// Undistort an image with the measured distortion ("after" preview; without crop/zoom).
pub fn undistort_img(b: &MeasureImage, k: &[f64; 3]) -> Vec<[f32; 3]> {
    let k = *k;
    radial_convert(b, move |r| forward_dir(&k, r))
}

/// Distortion of the distance: measured mapping r_ideal → r_captured / r_ideal at radius `r` (unit D).
pub fn ratio_val(k: &[f64; 3], r_ideal: f64) -> f64 {
    if r_ideal < 1e-12 {
        1.0
    } else {
        forward_dir(k, r_ideal) / r_ideal
    }
}

// ---------------------------------------------------------------------------------------------
// Lateral chromatic aberration

/// Result: radial scaling `s(r) = 1 + a r² + b r⁴ + c r⁶` of red or blue against green (r = radius of the
/// captured green position in units of D).
#[derive(Debug, Clone)]
pub struct LateralCa {
    pub red: [f64; 3],
    pub blue: [f64; 3],
    /// largest shift (pixels) at the image edge
    pub red_edge: f64,
    pub blue_edge: f64,
    pub points: usize,
}

fn bilinear(b: &MeasureImage, x: f32, y: f32, c: usize) -> f32 {
    let (w, h) = (b.width, b.height);
    let x = x.clamp(0.0, w as f32 - 1.001);
    let y = y.clamp(0.0, h as f32 - 1.001);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let g = |xx: usize, yy: usize| b.px[yy * w + xx][c].clamp(0.0, 1.0).sqrt();
    g(x0, y0) * (1.0 - fx) * (1.0 - fy) + g(x0 + 1, y0) * fx * (1.0 - fy) + g(x0, y0 + 1) * (1.0 - fx) * fy + g(x0 + 1, y0 + 1) * fx * fy
}

/// Profile of a channel along the normal `n` around `p`: `PROFILE_N` nodes at spacing `PROFILE_STEP`, brought to
/// mean 0 and spread 1. `None` if there is hardly any contrast.
const PROFILE_N: usize = 41;
const PROFILE_STEP: f32 = 0.2;

fn edge_profile(b: &MeasureImage, p: [f32; 2], n: [f32; 2], c: usize) -> Option<([f32; PROFILE_N], f32)> {
    let mut v = [0f32; PROFILE_N];
    for (i, w) in v.iter_mut().enumerate() {
        let t = (i as f32 - (PROFILE_N / 2) as f32) * PROFILE_STEP;
        *w = bilinear(b, p[0] + n[0] * t, p[1] + n[1] * t, c);
    }
    let mean = v.iter().sum::<f32>() / PROFILE_N as f32;
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / PROFILE_N as f32;
    let sd = var.sqrt();
    let span = v.iter().cloned().fold(f32::MIN, f32::max) - v.iter().cloned().fold(f32::MAX, f32::min);
    if span < 0.05 || sd < 1e-4 {
        return None;
    }
    for w in v.iter_mut() {
        *w = (*w - mean) / sd;
    }
    Some((v, span))
}

/// Shift (pixels, along the normal) of profile `c` against profile `g`: `c(t) = g(t − δ)`.
/// Least squares over the shift (grid 0.1 px, then parabola); `None` on poor agreement.
fn profile_shift(g: &[f32; PROFILE_N], c: &[f32; PROFILE_N]) -> Option<f32> {
    let probe = |d: f32| -> f32 {
        // g at t − δ by linear interpolation; skip the borders
        let mut s = 0.0;
        let mut n = 0.0;
        for (i, cv) in c.iter().enumerate() {
            let pos = i as f32 - d / PROFILE_STEP;
            if pos < 0.0 || pos > (PROFILE_N - 1) as f32 - 1e-3 {
                continue;
            }
            let (i0, f) = (pos as usize, pos - pos.floor());
            let gv = g[i0] * (1.0 - f) + g[i0 + 1] * f;
            s += (cv - gv) * (cv - gv);
            n += 1.0;
        }
        if n < 20.0 { f32::MAX } else { s / n }
    };
    let (mut best, mut bd) = (f32::MAX, 0.0f32);
    let mut d = -2.5f32;
    while d <= 2.5 {
        let e = probe(d);
        if e < best {
            best = e;
            bd = d;
        }
        d += 0.1;
    }
    if bd.abs() > 2.4 || best > 0.35 {
        return None;
    }
    let (e0, e1, e2) = (probe(bd - 0.1), best, probe(bd + 0.1));
    let denom = e0 - 2.0 * e1 + e2;
    let fine = if denom > 1e-9 { (0.5 * (e0 - e2) / denom).clamp(-0.5, 0.5) * 0.1 } else { 0.0 };
    Some(bd + fine)
}

/// Lateral CA from the edges (chains in pixels of the full image).
pub fn estimate_lateral_ca(b: &MeasureImage, chains: &[Vec<[f32; 2]>]) -> Option<LateralCa> {
    let d = b.width.max(b.height) as f64;
    let (cx, cy) = (b.width as f64 / 2.0, b.height as f64 / 2.0);
    // observations per channel (x1 = r²·rn, x2 = r⁴·rn, y = shift in pixels)
    let mut obs: [Vec<[f64; 4]>; 2] = [Vec::new(), Vec::new()];
    for chain in chains {
        let n = chain.len();
        if n < 20 {
            continue;
        }
        let advance = (n / 160).max(2);
        let mut i = 6;
        while i + 6 < n {
            let p = chain[i];
            let t = [chain[i + 6][0] - chain[i - 6][0], chain[i + 6][1] - chain[i - 6][1]];
            let l = t[0].hypot(t[1]);
            i += advance;
            if l < 1e-3 {
                continue;
            }
            let nrm = [-t[1] / l, t[0] / l];
            let rv = [p[0] as f64 - cx, p[1] as f64 - cy];
            let r = rv[0].hypot(rv[1]) / d;
            let rn = rv[0] * nrm[0] as f64 + rv[1] * nrm[1] as f64;
            // only points where the edge lies across the radius and that are far enough out
            if r < 0.15 || rn.abs() / (r * d) < 0.5 {
                continue;
            }
            let Some((pg, sg)) = edge_profile(b, p, nrm, 1) else { continue };
            if sg < 0.08 {
                continue;
            }
            for (ci, c) in [0usize, 2].into_iter().enumerate() {
                let Some((pc, sc)) = edge_profile(b, p, nrm, c) else { continue };
                if sc < 0.5 * sg {
                    continue;
                }
                if let Some(dc) = profile_shift(&pg, &pc) {
                    obs[ci].push([r * r * rn, r.powi(4) * rn, r.powi(6) * rn, dc as f64]);
                }
            }
        }
    }
    if std::env::var_os("GRAINEO_LENS_MEASURE_LOG").is_some() {
        // check: mean scaling (s − 1) per radius range from the raw measurements
        for (name, o) in ["rot", "blau"].iter().zip(&obs) {
            let mut row = format!("  Querfehler roh {name}:");
            for lo in 0..6 {
                let (a, bnd) = (0.1 + lo as f64 * 0.09, 0.19 + lo as f64 * 0.09);
                let (mut sxy, mut sxx, mut n) = (0.0, 0.0, 0);
                for x in o {
                    // x[0] = r²·rn → r from x[1]/x[0] = r²
                    let r2 = if x[0].abs() > 1e-12 { x[1] / x[0] } else { continue };
                    let r = r2.max(0.0).sqrt();
                    if r < a || r >= bnd {
                        continue;
                    }
                    let rn = x[0] / r2;
                    sxy += rn * x[3];
                    sxx += rn * rn;
                    n += 1;
                }
                if n > 10 {
                    row.push_str(&format!(" r{:.2}:{:+.5}({n})", (a + bnd) / 2.0, sxy / sxx));
                }
            }
            eprintln!("{row}");
        }
    }
    if obs[0].len() < 80 {
        return None;
    }
    // Robust regression (IRLS with Tukey weight) with 1…3 terms (a·r² + b·r⁴ + c·r⁶); more terms only with
    // clear gain (F-test), otherwise the curve oscillates for small lateral CA.
    let fit = |o: &[[f64; 4]]| -> [f64; 3] {
        let run_fit = |m: usize| -> ([f64; 3], f64, f64) {
            let mut w = vec![1.0; o.len()];
            let mut coef = [0.0f64; 3];
            for _ in 0..10 {
                let mut a = [[0f64; 3]; 3];
                let mut g = [0f64; 3];
                for (x, wi) in o.iter().zip(&w) {
                    for i in 0..m {
                        for j in 0..m {
                            a[i][j] += wi * x[i] * x[j];
                        }
                        g[i] += wi * x[i] * x[3];
                    }
                }
                let Some(sol) = solve3(&a, &g, m) else { break };
                coef = sol;
                let res: Vec<f64> = o.iter().map(|x| x[3] - coef[0] * x[0] - coef[1] * x[1] - coef[2] * x[2]).collect();
                let mut ab: Vec<f64> = res.iter().map(|r| r.abs()).collect();
                ab.sort_by(f64::total_cmp);
                let mad = ab[ab.len() / 2].max(1e-3);
                let c = 4.685 * 1.4826 * mad;
                for (wi, r) in w.iter_mut().zip(&res) {
                    let u = r / c;
                    *wi = if u.abs() < 1.0 { (1.0 - u * u).powi(2) } else { 0.0 };
                }
            }
            let (mut ss, mut nn) = (0.0, 0.0);
            for (x, wi) in o.iter().zip(&w) {
                let r = x[3] - coef[0] * x[0] - coef[1] * x[1] - coef[2] * x[2];
                ss += wi * r * r;
                nn += wi;
            }
            (coef, ss, nn)
        };
        let mut best = run_fit(1);
        for m in 2..=3 {
            let new_at = run_fit(m);
            let f = (best.1 - new_at.1) * (new_at.2 - m as f64).max(1.0) / new_at.1.max(1e-30);
            if f > 60.0 {
                best = new_at;
            }
        }
        best.0
    };
    let red = fit(&obs[0]);
    let blue = fit(&obs[1]);
    let rmax = (b.width as f64).hypot(b.height as f64) / 2.0 / d;
    let border = |k: [f64; 3]| (k[0] * rmax.powi(2) + k[1] * rmax.powi(4) + k[2] * rmax.powi(6)) * rmax * d;
    Some(LateralCa { red, blue, red_edge: border(red), blue_edge: border(blue), points: obs[0].len().min(obs[1].len()) })
}

// ---------------------------------------------------------------------------------------------
// Vignetting

#[derive(Debug, Clone)]
pub struct VignetteMeasurement {
    /// a1…a3 of the model `1 + a1 r² + a2 r⁴ + a3 r⁶` (r in units of D, fx = 1)
    pub a: [f64; 3],
    /// spread of the fit (relative), and spread over the angle within a ring (uniformity)
    pub rest: f64,
    pub angle_spread: f64,
    /// brightness at the edge relative to the center (corner)
    pub corner: f64,
}

/// Vignetting from a uniform bright surface (linear data).
pub fn estimate_vignette(b: &MeasureImage) -> Option<VignetteMeasurement> {
    let d = b.width.max(b.height) as f64;
    let (cx, cy) = (b.width as f64 / 2.0, b.height as f64 / 2.0);
    let rmax = (b.width as f64).hypot(b.height as f64) / 2.0 / d;
    const RINGS: usize = 48;
    const SECTORS: usize = 12;
    let mut sum_total = vec![[0f64; SECTORS]; RINGS];
    let mut number = vec![[0f64; SECTORS]; RINGS];
    let n = (b.width.max(b.height) / 600).max(1);
    let mut y = 0;
    while y + n <= b.height {
        let mut x = 0;
        while x + n <= b.width {
            let mut s = 0.0;
            for dy in 0..n {
                for dx in 0..n {
                    let p = b.px[(y + dy) * b.width + x + dx];
                    s += p[1] as f64;
                }
            }
            let v = s / (n * n) as f64;
            let (px, py) = (x as f64 + n as f64 / 2.0 - cx, y as f64 + n as f64 / 2.0 - cy);
            let r = px.hypot(py) / d;
            let ring = ((r / rmax) * RINGS as f64) as usize;
            let sector = (((py.atan2(px) + std::f64::consts::PI) / (2.0 * std::f64::consts::PI)) * SECTORS as f64) as usize % SECTORS;
            if ring < RINGS && v > 0.0 {
                sum_total[ring][sector] += v;
                number[ring][sector] += 1.0;
            }
            x += n;
        }
        y += n;
    }
    // per ring the mean of the sector means (sectors weighted equally: a gradient across the image cancels out)
    let mut ring_r = Vec::new();
    let mut ring_w = Vec::new();
    let mut ring_v = Vec::new();
    let mut spread: f64 = 0.0;
    let mut zs: f64 = 0.0;
    for i in 0..RINGS {
        let mut vs = Vec::new();
        for s in 0..SECTORS {
            if number[i][s] >= 3.0 {
                vs.push(sum_total[i][s] / number[i][s]);
            }
        }
        if vs.len() < SECTORS / 3 {
            continue;
        }
        let m = vs.iter().sum::<f64>() / vs.len() as f64;
        let sd = (vs.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / vs.len() as f64).sqrt();
        spread += sd / m.max(1e-9);
        zs += 1.0;
        ring_r.push((i as f64 + 0.5) / RINGS as f64 * rmax);
        ring_w.push(vs.len() as f64);
        ring_v.push(m);
    }
    if ring_r.len() < 12 {
        return None;
    }
    // fit I(r) = c0 + c1 r² + c2 r⁴ + c3 r⁶ (linear)
    let mut a = [[0f64; 4]; 4];
    let mut g = [0f64; 4];
    for ((r, w), v) in ring_r.iter().zip(&ring_w).zip(&ring_v) {
        let r2 = r * r;
        let basis = [1.0, r2, r2 * r2, r2 * r2 * r2];
        for i in 0..4 {
            for j in 0..4 {
                a[i][j] += w * basis[i] * basis[j];
            }
            g[i] += w * basis[i] * v;
        }
    }
    let c = solve4(a, g)?;
    if c[0] <= 0.0 {
        return None;
    }
    let ak = [c[1] / c[0], c[2] / c[0], c[3] / c[0]];
    let mut q = 0.0;
    for (r, v) in ring_r.iter().zip(&ring_v) {
        let r2 = r * r;
        let m = c[0] * (1.0 + r2 * (ak[0] + r2 * (ak[1] + r2 * ak[2])));
        q += ((v - m) / c[0]).powi(2);
    }
    let rest = (q / ring_r.len() as f64).sqrt();
    let r2 = rmax * rmax;
    Some(VignetteMeasurement { a: ak, rest, angle_spread: spread / zs.max(1.0), corner: 1.0 + r2 * (ak[0] + r2 * (ak[1] + r2 * ak[2])) })
}

fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> Option<[f64; 4]> {
    for c in 0..4 {
        let p = (c..4).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-30 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..4 {
            let f = a[r][c] / a[c][c];
            for cc in c..4 {
                a[r][cc] -= f * a[c][cc];
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = [0f64; 4];
    for c in (0..4).rev() {
        let mut s = b[c];
        for cc in c + 1..4 {
            s -= a[c][cc] * x[cc];
        }
        x[c] = s / a[c][c];
    }
    Some(x)
}

// ---------------------------------------------------------------------------------------------
// Assemble the profile

/// Pixels per focal length: distance for all entries (practically infinity).
const DISTANCE: f64 = 1000.0;

/// Measurement of the geometry at one focal length.
pub struct GeoMeasurement {
    pub focal_length: f64,
    pub aperture: f64,
    pub distortion: Distortion,
    pub lateral_ca: Option<LateralCa>,
}

/// Measurement of the vignetting at one focal length/aperture.
pub struct VigMeasurement {
    pub focal_length: f64,
    pub aperture: f64,
    pub vignette: VignetteMeasurement,
}

/// Profile from the measurements. `fx`: normalization (focal length in units of the longer image edge; 1 = without
/// knowledge of the sensor size), `crop` the sensor crop factor.
pub fn build_profile(name: &str, crop: f64, fx: impl Fn(f64) -> f64, geo: &[GeoMeasurement], vig: &[VigMeasurement]) -> Profile {
    let mut entries = Vec::new();
    for g in geo {
        let f = fx(g.focal_length);
        let m = model_from_k(g.distortion.k, f);
        let ca = g.lateral_ca.as_ref().map(|q| {
            let scale = |k: [f64; 3]| Model { fx: f, fy: f, cx: 0.5, cy: 0.5, k: [k[0] * f * f, k[1] * f.powi(4), k[2] * f.powi(6)], p: [0.0; 2], scale: 1.0 };
            (scale(q.red), scale(q.blue))
        });
        entries.push(Entry { focal_length: g.focal_length, distance: DISTANCE, av: 2.0 * g.aperture.max(0.5).log2(), geo: Some(m), ca, vig: None });
    }
    for v in vig {
        let f = fx(v.focal_length);
        let a = v.vignette.a;
        let vg = Vignette { fx: f, fy: f, cx: 0.5, cy: 0.5, a: [a[0] * f * f, a[1] * f.powi(4), a[2] * f.powi(6)] };
        entries.push(Entry { focal_length: v.focal_length, distance: DISTANCE, av: 2.0 * v.aperture.max(0.5).log2(), geo: None, ca: None, vig: Some(vg) });
    }
    entries.sort_by(|a, b| a.focal_length.total_cmp(&b.focal_length).then(a.av.total_cmp(&b.av)));
    Profile { name: name.to_string(), crop, entries }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grid image: dark lines (spacing `advance` in units of D) on a bright background, captured with
    /// distortion `k` (fx = 1) and optionally lateral CA (red/blue scaling `1 + a r²`).
    fn grid(w: usize, h: usize, k: [f64; 3], ca: Option<(f64, f64)>) -> Vec<[f32; 3]> {
        match ca {
            Some((a, b)) => grid_f(w, h, k, Some((&|r: f64| a * r * r, &|r: f64| b * r * r))),
            None => grid_f(w, h, k, None),
        }
    }

    /// Like `grid`, lateral CA as functions of the radius (scaling − 1 of red or blue).
    fn grid_f(w: usize, h: usize, k: [f64; 3], ca: Option<(&dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64)>) -> Vec<[f32; 3]> {
        let d = w.max(h) as f64;
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let advance = 0.075;
        let mut px = vec![[0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut rgb = [0f32; 3];
                for c in 0..3 {
                    let (mut ux, mut uy) = ((x as f64 + 0.5 - cx) / d, (y as f64 + 0.5 - cy) / d);
                    let rq = ux.hypot(uy);
                    if let Some((fr, fb)) = ca {
                        let s = match c {
                            0 => 1.0 + fr(rq),
                            2 => 1.0 + fb(rq),
                            _ => 1.0,
                        };
                        ux /= s;
                        uy /= s;
                    }
                    let rq = ux.hypot(uy);
                    let (ru, _) = backward(&k, rq);
                    let (ix, iy) = if rq > 1e-12 { (ux * ru / rq, uy * ru / rq) } else { (ux, uy) };
                    let m = if ru > 1e-9 { rq / ru } else { 1.0 };
                    let spacing = |v: f64| {
                        let f = (v / advance + 0.5).rem_euclid(1.0) - 0.5;
                        f.abs() * advance * d * m
                    };
                    let dist = spacing(ix).min(spacing(iy));
                    let dark = 1.0 - ((dist - 1.2) / 1.0).clamp(0.0, 1.0);
                    let v = 0.85 - 0.75 * dark;
                    rgb[c] = (v * v) as f32;
                }
                px[y * w + x] = rgb;
            }
        }
        px
    }

    fn measure_img(k: [f64; 3], w: usize, h: usize) -> Distortion {
        let px = grid(w, h, k, None);
        let b = MeasureImage { width: w, height: h, px: &px };
        let chains = find_chains(&b, 1500);
        assert!(chains.len() >= 8, "too few chains: {}", chains.len());
        let v = estimate_distortion(&chains, w, h).expect("result");
        eprintln!("true {k:?} measured {:?} chains {} rms {:.3} → {:.3} px, order {}", v.k, v.chains, v.rms_before, v.rms_after, v.order);
        v
    }

    fn deviation(a: &[f64; 3], b: &[f64; 3], w: usize, h: usize) -> f64 {
        let rmax = (w as f64).hypot(h as f64) / 2.0 / w.max(h) as f64;
        (1..=20).map(|i| i as f64 / 20.0 * rmax).map(|r| (ratio_val(a, r) - ratio_val(b, r)).abs()).fold(0.0, f64::max)
    }

    #[test]
    fn straight_lines_without_distortion() {
        let v = measure_img([0.0; 3], 1500, 1000);
        assert!(deviation(&v.k, &[0.0; 3], 1500, 1000) < 0.001, "{v:?}");
    }

    #[test]
    fn barrel_distortion() {
        let k = [-0.12, 0.03, 0.0];
        let v = measure_img(k, 1500, 1000);
        let a = deviation(&v.k, &k, 1500, 1000);
        assert!(a < 0.0015, "deviation {a}: {v:?}");
        assert!(v.rms_after < 0.3 * v.rms_before, "{v:?}");
    }

    #[test]
    fn pincushion_distortion() {
        let k = [0.08, -0.02, 0.0];
        let v = measure_img(k, 1500, 1000);
        let a = deviation(&v.k, &k, 1500, 1000);
        assert!(a < 0.0015, "deviation {a}: {v:?}");
    }

    #[test]
    fn wave_info_third_order() {
        // wave form (mustache): k1 < 0, k2 > 0 stronger
        let k = [-0.10, 0.12, -0.05];
        let v = measure_img(k, 1500, 1000);
        let a = deviation(&v.k, &k, 1500, 1000);
        assert!(a < 0.004, "deviation {a}: {v:?}");
    }

    #[test]
    fn lateral_ca_img() {
        let (w, h) = (1500, 1000);
        let (a, b) = (0.012, -0.008);
        let px = grid(w, h, [-0.05, 0.0, 0.0], Some((a, b)));
        let bi = MeasureImage { width: w, height: h, px: &px };
        let chains = find_chains(&bi, 1500);
        let q = estimate_lateral_ca(&bi, &chains).expect("result");
        // scaling at r = 0.5
        let s = |k: [f64; 3], r: f64| k[0] * r * r + k[1] * r.powi(4) + k[2] * r.powi(6);
        assert!((s(q.red, 0.5) - a * 0.25).abs() < 0.0006, "{q:?}");
        assert!((s(q.blue, 0.5) - b * 0.25).abs() < 0.0006, "{q:?}");
    }

    #[test]
    fn vignette_from_flat_field() {
        let (w, h) = (1200, 800);
        let d = w as f64;
        let a = [-0.9, 0.5, -0.2];
        let px: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64 + 0.5 - w as f64 / 2.0, (i / w) as f64 + 0.5 - h as f64 / 2.0);
                let r2 = (x * x + y * y) / (d * d);
                let v = 0.6 * (1.0 + r2 * (a[0] + r2 * (a[1] + r2 * a[2])));
                [v as f32; 3]
            })
            .collect();
        let m = estimate_vignette(&MeasureImage { width: w, height: h, px: &px }).expect("result");
        let rmax: f64 = (1200f64).hypot(800.0) / 2.0 / 1200.0;
        let f = |a: [f64; 3], r: f64| 1.0 + r * r * (a[0] + r * r * (a[1] + r * r * a[2]));
        for i in 1..=10 {
            let r = i as f64 / 10.0 * rmax;
            assert!((f(m.a, r) - f(a, r)).abs() < 0.01, "r={r}: {:?} vs. {a:?}", m.a);
        }
    }

    /// Blurry, noisy grid with slightly varying line width (closer to a real shot).
    fn grid_real(w: usize, h: usize, k: [f64; 3], seed: u32) -> Vec<[f32; 3]> {
        let raw_bytes = grid(w, h, k, None);
        let g = GrayImage { w, h, v: raw_bytes.iter().map(|p| p[1].sqrt()).collect() };
        let g = blur(&g, 1.3);
        let mut z = seed.wrapping_mul(2654435761).wrapping_add(12345);
        let mut rng = move || {
            z = z.wrapping_mul(1664525).wrapping_add(1013904223);
            ((z >> 8) as f32 / (1u32 << 24) as f32) - 0.5
        };
        g.v.iter().map(|v| {
            let n = (rng() + rng() + rng()) * 0.05 * std::env::var("GRAINEO_LENS_MEASURE_NOISE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
            let q = (v + n).clamp(0.0, 1.0);
            [q * q; 3]
        }).collect()
    }

    /// Only with locally installed Adobe profiles: the distortion of real lenses (from the profiles) is applied to a
    /// blurry, noisy grid, the measurement must recover it.
    #[test]
    fn against_adobe_profiles() {
        let idx = crate::lcp::index(None);
        if idx.is_empty() {
            return;
        }
        // choose profiles with clear distortion (corner ≥ 1.5 %), different lenses
        let mut chosen: Vec<(String, f64, [f64; 3])> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for header in idx.iter().enumerate().filter(|(i, k)| k.raw_bytes && i % 29 == 7).map(|(_, k)| k) {
            if chosen.len() >= 24 {
                break;
            }
            let Some(p) = crate::lcp::load(&header.path) else { continue };
            for e in &p.entries {
                let Some(g) = e.geo else { continue };
                if g.p[0].abs() > 1e-3 || g.p[1].abs() > 1e-3 {
                    continue;
                }
                let (w, h) = (3.0f64, 2.0f64);
                let rmax = (w * w + h * h).sqrt() / 2.0 / w;
                let corner = (g.k[0] * (rmax / g.fx).powi(2) + g.k[1] * (rmax / g.fx).powi(4) + g.k[2] * (rmax / g.fx).powi(6)).abs();
                if corner < 0.01 || corner > 0.3 || !seen.insert(header.lens.clone()) {
                    continue;
                }
                // coefficients in unit D (fx = 1)
                let f2 = g.fx * g.fx;
                chosen.push((format!("{} @ {:.1} mm", header.lens_pretty, e.focal_length), corner, [g.k[0] / f2, g.k[1] / f2.powi(2), g.k[2] / f2.powi(3)]));
                break;
            }
        }
        assert!(chosen.len() >= 4, "too few matching Adobe profiles");
        let (w, h) = (1800, 1200);
        let rmax = (w as f64).hypot(h as f64) / 2.0 / w as f64;
        let mut bad_count = 0.0f64;
        for (i, (name, corner, k)) in chosen.iter().enumerate() {
            let px = grid_real(w, h, *k, i as u32);
            let b = MeasureImage { width: w, height: h, px: &px };
            let chains = find_chains(&b, 1800);
            match estimate_distortion(&chains, w, h) {
                Ok(v) => {
                    let a = (1..=20).map(|j| j as f64 / 20.0 * rmax).map(|r| (ratio_val(&v.k, r) - ratio_val(k, r)).abs()).fold(0.0, f64::max);
                    eprintln!("{name}: corner Adobe {:.2} %, deviation {:.3} % = {:.2} px (order {}, {} chains)", corner * 100.0, a * 100.0, a * rmax * w as f64, v.order, v.chains);
                    bad_count = bad_count.max(a);
                }
                Err(e) => panic!("{name}: {e}"),
            }
        }
        assert!(bad_count < 0.004, "largest deviation {:.3} %", bad_count * 100.0);
    }

    /// The built profile computes the same as the measurement in the normalization of the Adobe model (fx ≠ 1).
    #[test]
    fn profile_computes_like_the_measurement() {
        let k = [-0.11, 0.05, -0.01];
        let a = [-0.8, 0.4, -0.1];
        let geo = GeoMeasurement {
            focal_length: 20.0,
            aperture: 8.0,
            distortion: Distortion { k, rms_before: 0.0, rms_after: 0.0, chains: 0, points: 0, coverage: 1.0, order: 3, spread: 1.0 },
            lateral_ca: Some(LateralCa { red: [0.004, -0.002, 0.0], blue: [-0.003, 0.001, 0.0], red_edge: 0.0, blue_edge: 0.0, points: 0 }),
        };
        let vig = VigMeasurement { focal_length: 20.0, aperture: 2.8, vignette: VignetteMeasurement { a, rest: 0.0, angle_spread: 0.0, corner: 0.0 } };
        let fx = 0.55;
        let p = build_profile("Test", 1.0, |_| fx, &[geo], &[vig]);
        let g = p.entries.iter().find_map(|e| e.geo).expect("geometry");
        let v = p.entries.iter().find_map(|e| e.vig).expect("vignetting");
        let (rg, _) = p.entries.iter().find_map(|e| e.ca).expect("lateral CA");
        for r in [0.1, 0.3, 0.5, 0.6] {
            let m = ratio_val(&k, r);
            let adobe = g.distort(r / fx, 0.0).0 * fx / r;
            assert!((m - adobe).abs() < 1e-12, "r={r}: {m} vs. {adobe}");
            let va = 1.0 + r * r * (a[0] + r * r * (a[1] + r * r * a[2]));
            assert!((va - v.falloff(r / fx, 0.0)).abs() < 1e-12);
            let s = 0.004 * r * r - 0.002 * r.powi(4);
            let ca = rg.scale * (rg.distort(r / fx, 0.0).0 - r / fx) * fx / r;
            assert!((s - ca).abs() < 1e-12, "r={r}: {s} vs. {ca}");
        }
    }

    /// Flat field with the vignetting from real Adobe profiles (falloff at source coordinates): the measurement finds it.
    #[test]
    fn vignette_against_adobe_profiles() {
        let idx = crate::lcp::index(None);
        if idx.is_empty() {
            return;
        }
        let (w, h) = (1500, 1000);
        let d = w as f64;
        let rmax = (w as f64).hypot(h as f64) / 2.0 / d;
        let mut checked = 0;
        for header in idx.iter().enumerate().filter(|(i, k)| k.raw_bytes && i % 41 == 3).map(|(_, k)| k) {
            if checked >= 12 {
                break;
            }
            let Some(p) = crate::lcp::load(&header.path) else { continue };
            let Some((e, v)) = p.entries.iter().filter_map(|e| e.vig.map(|v| (e, v))).find(|(_, v)| v.falloff(rmax / v.fx, 0.0) < 0.75 && v.falloff(rmax / v.fx, 0.0) > 0.2) else { continue };
            let px: Vec<[f32; 3]> = (0..w * h)
                .map(|i| {
                    let (x, y) = (((i % w) as f64 + 0.5 - w as f64 / 2.0) / d, ((i / w) as f64 + 0.5 - h as f64 / 2.0) / d);
                    let a = v.falloff(x / v.fx, y / v.fx).max(0.01);
                    [(0.7 * a) as f32; 3]
                })
                .collect();
            let m = estimate_vignette(&MeasureImage { width: w, height: h, px: &px }).expect("vignetting");
            let f = |a: [f64; 3], r: f64| 1.0 + r * r * (a[0] + r * r * (a[1] + r * r * a[2]));
            let mut maxd = 0.0f64;
            for j in 1..=20 {
                let r = j as f64 / 20.0 * rmax;
                maxd = maxd.max((f(m.a, r) - v.falloff(r / v.fx, 0.0)).abs());
            }
            eprintln!("{} @ {:.1} mm: corner {:.2}, largest deviation {:.4}", header.lens_pretty, e.focal_length, v.falloff(rmax / v.fx, 0.0), maxd);
            assert!(maxd < 0.01, "{}: {maxd}", header.lens_pretty);
            checked += 1;
        }
        assert!(checked >= 4);
    }

    /// Lateral CA of real Adobe profiles (red/blue against green) on the grid: the measurement finds the shifts.
    #[test]
    fn lateral_ca_against_adobe_profiles() {
        let idx = crate::lcp::index(None);
        if idx.is_empty() {
            return;
        }
        let (w, h) = (1800, 1200);
        let d = w as f64;
        let rmax = (w as f64).hypot(h as f64) / 2.0 / d;
        let mut checked = 0;
        for header in idx.iter().enumerate().filter(|(i, k)| k.raw_bytes && i % 5 == 2).map(|(_, k)| k) {
            if checked >= 8 {
                break;
            }
            let Some(p) = crate::lcp::load(&header.path) else { continue };
            let Some((e, (rg, bg))) = p.entries.iter().filter_map(|e| e.ca.map(|c| (e, c))).find(|(_, (r, b))| {
                let s = |m: &crate::lcp::Model| m.scale * (m.k[0] * (rmax / m.fx).powi(2) + m.k[1] * (rmax / m.fx).powi(4) + m.k[2] * (rmax / m.fx).powi(6));
                s(r).abs() * rmax * d > 0.8 && s(b).abs() * rmax * d > 0.8 && s(r).abs() * rmax * d < 12.0 && s(b).abs() * rmax * d < 12.0
            }) else { continue };
            let fkt = |m: crate::lcp::Model| move |r: f64| m.scale * (m.k[0] * (r / m.fx).powi(2) + m.k[1] * (r / m.fx).powi(4) + m.k[2] * (r / m.fx).powi(6));
            let (fr, fb) = (fkt(rg), fkt(bg));
            let px = grid_f(w, h, [-0.03, 0.0, 0.0], Some((&fr, &fb)));
            let b = MeasureImage { width: w, height: h, px: &px };
            let chains = find_chains(&b, 1800);
            let Some(q) = estimate_lateral_ca(&b, &chains) else { panic!("{}: no result", header.lens_pretty) };
            let sch = |k: [f64; 3], r: f64| k[0] * r * r + k[1] * r.powi(4) + k[2] * r.powi(6);
            let mut maxd = 0.0f64;
            for j in 1..=20 {
                let r = j as f64 / 20.0 * rmax;
                maxd = maxd.max(((sch(q.red, r) - fr(r)).abs()).max((sch(q.blue, r) - fb(r)).abs()) * r * d);
            }
            eprintln!("{} @ {:.1} mm: Adobe edge red {:+.2} px blue {:+.2} px, largest deviation {:.3} px", header.lens_pretty, e.focal_length, fr(rmax) * rmax * d, fb(rmax) * rmax * d, maxd);
            assert!(maxd < 0.5, "{}: {maxd} px", header.lens_pretty);
            checked += 1;
        }
        assert!(checked >= 3, "only {checked} profiles with lateral CA");
    }

    #[test]
    #[ignore]
    fn lateral_ca_sensitivity() {
        let (w, h) = (1800, 1200);
        for a in [0.002, 0.004, 0.008, 0.016] {
            let px = grid(w, h, [-0.03, 0.0, 0.0], Some((a, -a)));
            let b = MeasureImage { width: w, height: h, px: &px };
            let chains = find_chains(&b, 1800);
            let q = estimate_lateral_ca(&b, &chains).expect("result");
            eprintln!("a={a}: red {:?} blue {:?} points {}", q.red, q.blue, q.points);
        }
    }

    /// Built profile → JSON → selection as in the correction: geometry, lateral CA and vignetting are present.
    #[test]
    fn profile_json_and_selection() {
        let geo = GeoMeasurement {
            focal_length: 20.0,
            aperture: 8.0,
            distortion: Distortion { k: [-0.08, 0.02, 0.0], rms_before: 0.0, rms_after: 0.0, chains: 0, points: 0, coverage: 1.0, order: 2, spread: 1.0 },
            lateral_ca: Some(LateralCa { red: [0.002, 0.0, 0.0], blue: [-0.002, 0.0, 0.0], red_edge: 0.0, blue_edge: 0.0, points: 0 }),
        };
        let vig = [2.8, 5.6].map(|n| VigMeasurement { focal_length: 20.0, aperture: n, vignette: VignetteMeasurement { a: [-0.7, 0.3, 0.0], rest: 0.0, angle_spread: 0.0, corner: 0.0 } });
        let p = build_profile("T", 1.0, |f| f / 36.0, &[geo], &vig);
        let s = crate::lcp::Backup { format: 1, maker: "X".into(), model: "Y".into(), lens: "Z".into(), origin: "graineo-Messung".into(), profile: p };
        let json = serde_json::to_string(&s).unwrap();
        let s2: crate::lcp::Backup = serde_json::from_str(&json).unwrap();
        assert!(s2.profile.covers(Some(20.0)));
        let a = s2.profile.select(Some(20.0), Some(4.0), None);
        assert!(a.geo.is_some() && a.ca.is_some() && a.vig.is_some());
    }
}
