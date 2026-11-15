//! Focus/sharpness map from the green pixels – before demosaicing and sharpening.
//!
//! Green is averaged per block (Bayer 2×2 = both greens, X-Trans 3×3 = 5 greens, aligned to the
//! 3-fold division of the pattern), log-transformed (ln(G + c), c ≈ 3 × read noise: contrast
//! instead of brightness – bright, blurry highlights no longer count more than a sharp dark edge)
//! and scored with **Tenengrad** (Sobel magnitude², Krotkov 1987). Clipped surroundings do not count. The noise contribution expected from the noise model is subtracted from the result
//! (for white noise σ², E[Tenengrad] = 24 σ²). In addition, the Laplacian variance as the classic
//! overall figure.
//! - Map: mean of the excess per map block, as amplitude (square root), heat scale.
//! - Focus peaking: green pixels with Tenengrad above `peaking_threshold` × 99.9 % value (and clearly
//!   above the noise), condensed onto the map as "any pixel in the block".

use rayon::prelude::*;
use serde_json::{json, Value};

use crate::canvas::{self, Canvas};
use crate::{round_to, Region, Layer, GridMap, Options, RawImage, Result, Role, Sensor};

/// Green plane (block means) over the crop: (values, width, height, block size, greens per block, offset).
pub(crate) fn green_plane(rb: &RawImage) -> (Vec<f32>, usize, usize, usize, usize, (usize, usize)) {
    let z = rb.crop;
    let k = if rb.sensor == Sensor::Bayer { 2 } else { 3 };
    // align to the k-division of the sensor
    let (x0, y0) = (z.x.div_ceil(k) * k, z.y.div_ceil(k) * k);
    let gw = (z.x + z.w - x0) / k;
    let gh = (z.y + z.h - y0) / k;
    // Green positions within a block per block position (the pattern repeats after 6/k blocks)
    let rep = 6 / k;
    let mut layers: Vec<Vec<(usize, usize)>> = Vec::with_capacity(rep * rep);
    for by in 0..rep {
        for bx in 0..rep {
            let mut v = Vec::new();
            for dy in 0..k {
                for dx in 0..k {
                    if rb.pattern[(y0 + by * k + dy) % 6][(x0 + bx * k + dx) % 6] == 1 {
                        v.push((dx, dy));
                    }
                }
            }
            layers.push(v);
        }
    }
    let n_green = layers.iter().map(|v| v.len()).min().unwrap_or(1).max(1);
    let mut g = vec![0f32; gw * gh];
    g.par_chunks_mut(gw.max(1)).enumerate().for_each(|(by, row)| {
        let y = y0 + by * k;
        for (bx, o) in row.iter_mut().enumerate() {
            let x = x0 + bx * k;
            let l = &layers[(by % rep) * rep + bx % rep];
            let mut s = 0f32;
            for &(dx, dy) in l {
                s += rb.norm[(y + dy) * rb.width + x + dx];
            }
            *o = s / l.len().max(1) as f32;
        }
    });
    (g, gw, gh, k, n_green, (x0 - z.x, y0 - z.y))
}

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let z = rb.crop;
    let (g, gw, gh, kb, ng, off) = green_plane(rb);
    if gw < 8 || gh < 8 {
        return Err(crate::Error("Bild zu klein für die Schärfekarte".into()));
    }
    let rm = rb.noise_model();
    let c0 = rm.map(|r| 3.0 * r.b[1].max(0.0).sqrt()).unwrap_or(0.003).max(0.002);
    let s: Vec<f32> = g.par_iter().map(|&v| (v.max(0.0) + c0).ln()).collect();
    // Noise variance in the ln domain at value v (mean of ng green pixels): Var(v)/(v + c)²
    let var_s = |v: f32| -> f32 {
        match rm {
            Some(r) => {
                let v = v.max(0.0);
                (r.a[1] * v + r.b[1]) / (ng as f32 * (v + c0) * (v + c0))
            }
            None => 0.0,
        }
    };
    let clip_s = (0.9 * rb.saturation_color()[1] + c0).ln();
    // Tenengrad excess and Laplacian per green pixel
    let mut ten = vec![0f32; gw * gh];
    let lap_sums: Vec<(f64, f64, u64)> = ten
        .par_chunks_mut(gw)
        .enumerate()
        .map(|(y, row)| {
            let mut ls = (0f64, 0f64, 0u64);
            if y == 0 || y + 1 >= gh {
                return ls;
            }
            let (a, b, c) = (&s[(y - 1) * gw..y * gw], &s[y * gw..(y + 1) * gw], &s[(y + 1) * gw..(y + 2) * gw]);
            for x in 1..gw - 1 {
                let gx = (a[x + 1] + 2.0 * b[x + 1] + c[x + 1]) - (a[x - 1] + 2.0 * b[x - 1] + c[x - 1]);
                let gy = (c[x - 1] + 2.0 * c[x] + c[x + 1]) - (a[x - 1] + 2.0 * a[x] + a[x + 1]);
                // Edges at clipped areas are not sharpness (highlights, windows)
                let light_weight = a[x - 1].max(a[x]).max(a[x + 1]).max(b[x - 1]).max(b[x + 1]).max(c[x - 1]).max(c[x]).max(c[x + 1]);
                if light_weight >= clip_s {
                    continue;
                }
                let t = gx * gx + gy * gy;
                let noise_level = 24.0 * var_s(g[y * gw + x]);
                row[x] = (t - 3.0 * noise_level).max(0.0);
                let l = (a[x] + c[x] + b[x - 1] + b[x + 1] - 4.0 * b[x]) as f64;
                ls.0 += l;
                ls.1 += l * l;
                ls.2 += 1;
            }
            ls
        })
        .collect();
    let (l1, l2, ln) = lap_sums.iter().fold((0f64, 0f64, 0u64), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
    let lap_var = l2 / ln.max(1) as f64 - (l1 / ln.max(1) as f64).powi(2);

    // Map: mean of the excess per block (green plane → map scale)
    let k = GridMap::new_at(z, o.size);
    let mut sorted: Vec<f32> = ten.iter().copied().step_by(7).collect();
    let p999 = crate::percentile(&mut sorted, 0.999).max(1e-12);
    let p99 = crate::percentile(&mut sorted, 0.99);
    let threshold = o.peaking_threshold.clamp(0.01, 1.0) * p999;
    let kx: Vec<usize> = (0..gw).map(|x| ((off.0 + x * kb + kb / 2).min(z.w - 1) * k.mean / z.w).min(k.mean - 1)).collect();
    // Green rows per map row (monotonic) → map rows in parallel
    let ky: Vec<usize> = (0..gh).map(|y| ((off.1 + y * kb + kb / 2).min(z.h - 1) * k.mh / z.h).min(k.mh - 1)).collect();
    let mut class_at = vec![gh; k.mh + 1];
    for (y, &my) in ky.iter().enumerate().rev() {
        class_at[my] = y;
    }
    for my in (0..k.mh).rev() {
        class_at[my] = class_at[my].min(class_at[my + 1]);
    }
    let mut rows: Vec<(Vec<f64>, Vec<u32>, Vec<bool>, usize)> = (0..k.mh)
        .into_par_iter()
        .map(|my| {
            let mut su = vec![0f64; k.mean];
            let mut ans1 = vec![0u32; k.mean];
            let mut pk = vec![false; k.mean];
            let mut np = 0usize;
            for y in class_at[my]..class_at[my + 1] {
                let tr = &ten[y * gw..(y + 1) * gw];
                for x in 0..gw {
                    let t = tr[x];
                    su[kx[x]] += t as f64;
                    ans1[kx[x]] += 1;
                    if t > threshold {
                        pk[kx[x]] = true;
                        np += 1;
                    }
                }
            }
            (su, ans1, pk, np)
        })
        .collect();
    let n_peak: usize = rows.iter().map(|z| z.3).sum();
    let sum_total: Vec<f64> = rows.iter_mut().flat_map(|z| std::mem::take(&mut z.0)).collect();
    let count: Vec<u32> = rows.iter_mut().flat_map(|z| std::mem::take(&mut z.1)).collect();
    let peak: Vec<bool> = rows.iter_mut().flat_map(|z| std::mem::take(&mut z.2)).collect();
    let values: Vec<f32> = sum_total.iter().zip(&count).map(|(s, &n)| (s / n.max(1) as f64).sqrt() as f32).collect();
    let mut tmp = values.clone();
    let scale = crate::percentile(&mut tmp, 0.995).max(1e-6);
    let mut map = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
    let mut mask = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
    for (i, &v) in values.iter().enumerate() {
        let t = (v / scale).min(1.0);
        let c = canvas::heat(t);
        map.set(i % k.mean, i / k.mean, [c[0], c[1], c[2], (60.0 + 170.0 * t) as u8]);
        if peak[i] {
            mask.set(i % k.mean, i / k.mean, [60, 255, 90, 255]);
        }
    }
    // Zones 3×3 (relative to the best) and sharpest block
    let mut zones = [[0f64; 3]; 3];
    let mut zn = [[0u32; 3]; 3];
    for (i, &v) in values.iter().enumerate() {
        let (zx, zy) = ((i % k.mean) * 3 / k.mean, (i / k.mean) * 3 / k.mh);
        zones[zy][zx] += v as f64;
        zn[zy][zx] += 1;
    }
    let zones: Vec<Vec<f64>> = (0..3).map(|y| (0..3).map(|x| zones[y][x] / zn[y][x].max(1) as f64).collect()).collect();
    let zmax = zones.iter().flatten().cloned().fold(0.0, f64::max).max(1e-12);
    let zones_rel: Vec<Vec<f64>> = zones.iter().map(|r| r.iter().map(|v| round_to(v / zmax, 2)).collect()).collect();
    let (imax, vmax) = values.iter().enumerate().fold((0, 0f32), |a, (i, &v)| if v > a.1 { (i, v) } else { a });
    let top1: f64 = {
        let v: Vec<f32> = sorted.iter().copied().filter(|&t| t >= p99).collect();
        (v.iter().map(|&t| t as f64).sum::<f64>() / v.len().max(1) as f64).sqrt()
    };
    let reference = Region { x: 0, y: 0, w: z.w, h: z.h };
    let kz = json!({
        "green_plane": {"width": gw, "height": gh, "block": kb, "green_per_block": ng},
        "sharpness_index": round_to(top1, 5),
        "laplace_variance": round_to(lap_var, 8),
        "peaking_threshold": round_to(threshold as f64, 8),
        "peaking_fraction_percent": round_to(n_peak as f64 / (gw * gh) as f64 * 100.0, 2),
        "sharpest_point": {"x": ((imax % k.mean) * 2 + 1) * z.w / (2 * k.mean), "y": ((imax / k.mean) * 2 + 1) * z.h / (2 * k.mh), "amplitude": round_to(vmax as f64, 5)},
        "zones_relative": zones_rel,
        "map": {"width": k.mean, "height": k.mh, "scale_p995": round_to(scale as f64, 5)},
        "noise_subtraction": rm.is_some(),
        "domain": format!("ln(G + {c0:.4})"),
    });
    let layers = vec![
        Layer { name: "sharpness".into(), role: Role::Overlay, reference: Some(reference), canvas: map },
        Layer { name: "peaking".into(), role: Role::Overlay, reference: Some(reference), canvas: mask },
    ];
    Ok((layers, kz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze as analyse, Kind};

    #[test]
    fn edges_are_sharp_flat_areas_not() {
        // left sharp edges (checkerboard 16 px), right the same content heavily blurred
        let (w, h) = (512, 256);
        let field = |x: usize, y: usize| if ((x / 16) + (y / 16)) % 2 == 0 { 0.5f32 } else { 0.05 };
        let mut m = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                m[y * w + x] = if x < 256 {
                    field(x, y)
                } else {
                    // box mean 15×15
                    let mut s = 0.0;
                    for dy in 0..15 {
                        for dx in 0..15 {
                            s += field((x + dx).saturating_sub(7), (y + dy).saturating_sub(7));
                        }
                    }
                    s / 225.0
                };
            }
        }
        let rggb = std::array::from_fn(|y| std::array::from_fn(|x| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        }));
        // some noise: otherwise the exact pile-up at 0.5 would count as saturation (clipping)
        let mut z = 0x2545_f491_4f6c_dd1du64;
        for v in m.iter_mut() {
            z ^= z << 13;
            z ^= z >> 7;
            z ^= z << 17;
            *v += ((z >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.004;
        }
        let rb = RawImage::from_mosaic(m, w, h, rggb, Sensor::Bayer, [1.0; 3]);
        let e = analyse(&rb, Kind::Sharpness, &Options { size: 512, ..Default::default() }).unwrap();
        let p = &e.layer("peaking").unwrap().canvas;
        let count_in = |x0: usize, x1: usize| (0..h).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| p.get(x, y)[3] > 0).count();
        let (left, right) = (count_in(8, 248), count_in(264, 504));
        assert!(left > 500, "edges left: {left}");
        assert!(right * 20 < left, "right {right} vs. left {left}");
        let zones = &e.metrics["zones_relative"];
        assert!(zones[1][0].as_f64().unwrap() > 2.0 * zones[1][2].as_f64().unwrap());
    }
}
