//! Geometry and vignette comparison with a reference (e.g. a reference preview with active
//! lens correction).
//!
//! Geometry: both images scaled to the reference size, brightness; on a 9×6 grid, per field (96 px),
//! the shift with the highest normalized cross-correlation (±20 px, subpixel via parabola) –
//! insensitive to tonal differences. Then a similarity (scale + offset) is
//! factored out; what remains is the residual distortion (RMS, max., radial component outside − inside).
//! Vignette: brightness (linear) of the four corners (8 % of the edges each) relative to the image center in EV,
//! ours minus reference.

use image::RgbImage;

fn lum(img: &RgbImage, w: u32, h: u32) -> Vec<f32> {
    let r = image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle);
    r.pixels().map(|p| 0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2])).collect()
}

fn lin(v: u8) -> f32 {
    let x = v as f32 / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

pub struct Geo {
    pub patches: usize,
    pub rms: f32,
    pub max: f32,
    /// mean radial residual shift (ours − reference, px at reference size) outside minus inside
    pub radial: f32,
    pub scale: f32,
    /// vignette corners − center in EV: ours, reference
    pub vig: (f32, f32),
}

fn ncc(a: &[f32], b: &[f32], w: usize, ax: usize, ay: usize, bx: isize, by: isize, n: usize) -> f32 {
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for y in 0..n {
        for x in 0..n {
            let va = a[(ay + y) * w + ax + x] as f64;
            let vb = b[(by as usize + y) * w + bx as usize + x] as f64;
            sa += va;
            sb += vb;
            saa += va * va;
            sbb += vb * vb;
            sab += va * vb;
        }
    }
    let m = (n * n) as f64;
    let cov = sab - sa * sb / m;
    let va = saa - sa * sa / m;
    let vb = sbb - sb * sb / m;
    if va <= 1e-12 || vb <= 1e-12 {
        return -1.0;
    }
    (cov / (va * vb).sqrt()) as f32
}

pub fn compare_images(own: &RgbImage, reference: &RgbImage) -> Geo {
    let (w, h) = (reference.width(), reference.height());
    let a = lum(own, w, h);
    let b = lum(reference, w, h);
    let (wu, hu) = (w as usize, h as usize);
    // cube root against tonal differences (perceptually closer, lower contrast in highlights)
    let a: Vec<f32> = a.iter().map(|v| v.max(0.0).cbrt()).collect();
    let b: Vec<f32> = b.iter().map(|v| v.max(0.0).cbrt()).collect();
    const N: usize = 96;
    const S: isize = 20;
    let mut points: Vec<([f32; 2], [f32; 2])> = Vec::new();
    for gy in 0..6 {
        for gx in 0..9 {
            let cx = (wu as f32 * (gx as f32 + 0.5) / 9.0) as usize;
            let cy = (hu as f32 * (gy as f32 + 0.5) / 6.0) as usize;
            let (ax, ay) = (cx.saturating_sub(N / 2), cy.saturating_sub(N / 2));
            if ax < S as usize || ay < S as usize || ax + N + S as usize >= wu || ay + N + S as usize >= hu {
                continue;
            }
            // texture required
            let m: f32 = (0..N * N).map(|i| a[(ay + i / N) * wu + ax + i % N]).sum::<f32>() / (N * N) as f32;
            let sd = ((0..N * N).map(|i| (a[(ay + i / N) * wu + ax + i % N] - m).powi(2)).sum::<f32>() / (N * N) as f32).sqrt();
            if sd < 0.01 {
                continue;
            }
            let mut best = (-2f32, 0isize, 0isize);
            let mut patch = vec![-2f32; ((2 * S + 1) * (2 * S + 1)) as usize];
            for dy in -S..=S {
                for dx in -S..=S {
                    let v = ncc(&a, &b, wu, ax, ay, ax as isize + dx, ay as isize + dy, N);
                    patch[((dy + S) * (2 * S + 1) + dx + S) as usize] = v;
                    if v > best.0 {
                        best = (v, dx, dy);
                    }
                }
            }
            if best.0 < 0.6 || best.1.abs() == S || best.2.abs() == S {
                continue;
            }
            let at = |dx: isize, dy: isize| patch[((dy + S) * (2 * S + 1) + dx + S) as usize];
            let parab = |l: f32, c: f32, r: f32| {
                let d = l - 2.0 * c + r;
                if d.abs() < 1e-9 { 0.0 } else { (0.5 * (l - r) / d).clamp(-0.5, 0.5) }
            };
            let sx = best.1 as f32 + parab(at(best.1 - 1, best.2), best.0, at(best.1 + 1, best.2));
            let sy = best.2 as f32 + parab(at(best.1, best.2 - 1), best.0, at(best.1, best.2 + 1));
            points.push(([cx as f32, cy as f32], [sx, sy]));
        }
    }
    // factor out the similarity: d = (s − 1)·(p − c) + t  (3 parameters, least squares)
    let (mx, my) = (wu as f32 / 2.0, hu as f32 / 2.0);
    let (mut s11, mut s1x, mut s1y, mut sn, mut sdx, mut sdy, mut srd) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for (p, d) in &points {
        let (u, v) = ((p[0] - mx) as f64, (p[1] - my) as f64);
        s11 += u * u + v * v;
        s1x += u;
        s1y += v;
        sn += 1.0;
        sdx += d[0] as f64;
        sdy += d[1] as f64;
        srd += u * d[0] as f64 + v * d[1] as f64;
    }
    // normal equations [s11 s1x s1y; s1x n 0; s1y 0 n]·[k tx ty] = [srd sdx sdy]
    let det = s11 * sn * sn - s1x * s1x * sn - s1y * s1y * sn;
    let (k, tx, ty) = if points.len() >= 4 && det.abs() > 1e-9 {
        let k = (srd * sn * sn - sdx * s1x * sn - sdy * s1y * sn) / det;
        ((k), (sdx - k * s1x) / sn, (sdy - k * s1y) / sn)
    } else {
        (0.0, 0.0, 0.0)
    };
    let mut res = Vec::new();
    let rmax = mx.hypot(my);
    let (mut outer, mut na, mut inner, mut ni) = (0f32, 0f32, 0f32, 0f32);
    for (p, d) in &points {
        let (u, v) = (p[0] - mx, p[1] - my);
        let rx = d[0] - (k as f32 * u + tx as f32);
        let ry = d[1] - (k as f32 * v + ty as f32);
        res.push(rx.hypot(ry));
        let r = u.hypot(v);
        let rad = if r > 1.0 { (rx * u + ry * v) / r } else { 0.0 };
        if r > 0.6 * rmax {
            outer += rad;
            na += 1.0;
        } else if r < 0.35 * rmax {
            inner += rad;
            ni += 1.0;
        }
    }
    let rms = (res.iter().map(|v| v * v).sum::<f32>() / res.len().max(1) as f32).sqrt();
    let max = res.iter().cloned().fold(0f32, f32::max);
    // Vignette
    let lin_a = lum(own, w, h);
    let lin_b = lum(reference, w, h);
    let bx = (wu as f32 * 0.08) as usize;
    let by = (hu as f32 * 0.08) as usize;
    let mean = |img: &[f32], x0: usize, y0: usize| -> f32 {
        let mut s = 0f32;
        for y in y0..y0 + by {
            for x in x0..x0 + bx {
                s += img[y * wu + x];
            }
        }
        s / (bx * by) as f32
    };
    let corners = |img: &[f32]| -> f32 {
        let e = [mean(img, 0, 0), mean(img, wu - bx, 0), mean(img, 0, hu - by), mean(img, wu - bx, hu - by)];
        let m = mean(img, (wu - bx) / 2, (hu - by) / 2);
        e.iter().map(|v| (v.max(1e-6) / m.max(1e-6)).log2()).sum::<f32>() / 4.0
    };
    Geo {
        patches: points.len(),
        rms,
        max,
        radial: if na > 0.0 && ni > 0.0 { outer / na - inner / ni } else { 0.0 },
        scale: 1.0 + k as f32,
        vig: (corners(&lin_a), corners(&lin_b)),
    }
}
