//! Noise and dynamic range on the mosaic.
//!
//! - **Block statistics** (24×24 sensor pixels, a multiple of both pattern periods): per color
//!   mean and noise variance from second differences of same-color neighbors in a row (like
//!   `raw::noise::estimate`; gradients cancel out, texture does not).
//! - **Noise map**: signal-to-noise ratio (dB) per color in *flat* blocks (measured σ at most
//!   1.5× model σ), and the ratio measured/model as a map (texture, heat, banding > 1).
//! - **Dynamic range**: saturation against the noise floor – from the noise model (read noise
//!   √b) and, if present, from the optical black border (σ there, incl. banding).
//! - **SNR curve** per color over brightness (EV below saturation): model and measured points.

use rayon::prelude::*;
use serde_json::{json, Value};

use crate::canvas::{self, Canvas};
use crate::histogram::{chart_frame, Axes};
use crate::{region_json, round_to, Region, Layer, GridMap, Options, RawImage, Result, Role};

pub(crate) const BLOCK: usize = 24;

/// Statistics of a block per color: (mean, noise variance, count)
pub(crate) type Block = [(f32, f32, u32); 3];

/// Distances to the nearest same-color pixel left/right per pattern position.
fn neighbor_row(m: &[[u8; 6]; 6]) -> [[(usize, usize); 6]; 6] {
    let mut nb = [[(0usize, 0usize); 6]; 6];
    for (py, row) in nb.iter_mut().enumerate() {
        for (px, o) in row.iter_mut().enumerate() {
            let c = m[py][px];
            let l = (1..=6).find(|&d| m[py][(px + 12 - d) % 6] == c).unwrap_or(6);
            let r = (1..=6).find(|&d| m[py][(px + d) % 6] == c).unwrap_or(6);
            *o = (l, r);
        }
    }
    nb
}

/// Block statistics over the area `r` (sensor coordinates). Returns (blocks, width, height).
pub(crate) fn blocks(rb: &RawImage, r: Region) -> (Vec<Block>, usize, usize) {
    let nb = neighbor_row(&rb.pattern);
    let clip = rb.saturation_color().map(|v| 0.98 * v);
    let (bw, bh) = (r.w / BLOCK, r.h / BLOCK);
    let mut out = vec![[(0f32, 0f32, 0u32); 3]; bw * bh];
    out.par_chunks_mut(bw.max(1)).enumerate().for_each(|(by, row)| {
        let mut s = vec![[0f64; 3]; bw];
        let mut s2 = vec![[0f64; 3]; bw];
        let mut n = vec![[0u32; 3]; bw];
        for y in r.y + by * BLOCK..r.y + (by + 1) * BLOCK {
            let zr = &rb.norm[y * rb.width..(y + 1) * rb.width];
            let mr = &rb.pattern[y % 6];
            let nr = &nb[y % 6];
            for x in r.x..r.x + bw * BLOCK {
                let (l, rr) = nr[x % 6];
                if x < l || x + rr >= rb.width {
                    continue;
                }
                let c = mr[x % 6] as usize;
                let (a, v, b) = (zr[x - l], zr[x], zr[x + rr]);
                if v >= clip[c] || a >= clip[c] || b >= clip[c] {
                    continue;
                }
                let (fl, fr) = (rr as f32 / (l + rr) as f32, l as f32 / (l + rr) as f32);
                let d = v - fl * a - fr * b;
                let bx = (x - r.x) / BLOCK;
                s[bx][c] += v as f64;
                s2[bx][c] += (d * d / (1.0 + fl * fl + fr * fr)) as f64;
                n[bx][c] += 1;
            }
        }
        for (bx, o) in row.iter_mut().enumerate() {
            *o = std::array::from_fn(|c| if n[bx][c] >= 16 { ((s[bx][c] / n[bx][c] as f64) as f32, (s2[bx][c] / n[bx][c] as f64) as f32, n[bx][c]) } else { (0.0, -1.0, 0) });
        }
    });
    (out, bw, bh)
}

/// σ and mean per color in a sensor area (normalized), for black borders.
fn edge_stats(rb: &RawImage, r: Region) -> [(f64, f64, u64); 3] {
    let mut s = [0f64; 3];
    let mut s2 = [0f64; 3];
    let mut n = [0u64; 3];
    for y in r.y..(r.y + r.h).min(rb.height) {
        for x in r.x..(r.x + r.w).min(rb.width) {
            let c = rb.color(x, y);
            let v = rb.value(x, y) as f64;
            s[c] += v;
            s2[c] += v * v;
            n[c] += 1;
        }
    }
    std::array::from_fn(|c| {
        let m = s[c] / n[c].max(1) as f64;
        (m, (s2[c] / n[c].max(1) as f64 - m * m).max(0.0).sqrt(), n[c])
    })
}

/// Black border areas: according to rawler, otherwise strips outside the active area (if dark).
pub(crate) fn black_borders(rb: &RawImage) -> (Vec<Region>, &'static str) {
    if !rb.black_margins.is_empty() {
        return (rb.black_margins.clone(), "rawler (masked sensor areas)");
    }
    let a = rb.active;
    let mut v = Vec::new();
    if a.x >= 8 {
        v.push(Region { x: 0, y: a.y, w: a.x - 2, h: a.h });
    }
    if a.y >= 8 {
        v.push(Region { x: a.x, y: 0, w: a.w, h: a.y - 2 });
    }
    // only if really dark (otherwise these are not masked pixels)
    v.retain(|r| {
        let st = edge_stats(rb, *r);
        st.iter().all(|s| s.0.abs() < 0.01)
    });
    (v, "outside the active area")
}

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let z = rb.crop;
    let (bl, bw, bh) = blocks(rb, z);
    let rm = rb.noise_model();
    let sat = rb.saturation_color();
    let nb = bw * bh;

    // Maps at block resolution. The noise model comes from the *lowest tenth* of the
    // block variances (raw::rauschen::schaetzen), but the blocks here are an ordinary
    // cross-section – compared raw, almost everything would be "red". Therefore the ratio is
    // referenced to the 25 % quantile of the green ratios of all blocks (the same statistic as the
    // measured points of the SNR curve): quiet areas then lie at ≈ 1, texture/interference above.
    let raw_bytes = |b: &Block, c: usize| -> Option<f32> {
        let (m, var, n) = b[c];
        (n > 0 && var > 0.0 && m > 0.0).then(|| rm.map(|r| (var / (r.a[c] * m + r.b[c]).max(1e-12)).sqrt()).unwrap_or(1.0))
    };
    let mut green: Vec<f32> = bl.iter().filter_map(|b| raw_bytes(b, 1)).collect();
    let reference_value = if green.len() >= 16 { crate::percentile(&mut green, 0.25).clamp(0.5, 4.0) } else { 1.0 };
    let mut snr = [vec![f32::NAN; nb], vec![f32::NAN; nb], vec![f32::NAN; nb]];
    let mut ratios = vec![f32::NAN; nb];
    let mut flat = 0usize;
    for (i, b) in bl.iter().enumerate() {
        let mut r_g = f32::NAN;
        let mut is_flat = true;
        for c in 0..3 {
            let (m, var, _) = b[c];
            let Some(ratio) = raw_bytes(b, c).map(|r| r / reference_value) else {
                is_flat = false;
                continue;
            };
            if c == 1 {
                r_g = ratio;
            }
            if ratio <= 1.5 {
                snr[c][i] = 20.0 * (m / var.sqrt()).log10();
            } else {
                is_flat = false;
            }
        }
        ratios[i] = r_g;
        if is_flat {
            flat += 1;
        }
    }
    let k = GridMap::new_at(z, o.size);
    let reference = Region { x: 0, y: 0, w: bw * BLOCK, h: bh * BLOCK };
    let to_map = |values: &[f32], f: &dyn Fn(f32) -> Option<[u8; 4]>| -> Canvas {
        let mut b = Canvas::new_at(bw, bh, canvas::TRANSPARENT);
        for (i, &v) in values.iter().enumerate() {
            if v.is_finite() {
                if let Some(p) = f(v) {
                    b.set(i % bw, i / bw, p);
                }
            }
        }
        // blocks cover bw·24 × bh·24 → to the map scale of this area
        let (mean, mh) = ((reference.w * k.mean).div_ceil(z.w), (reference.h * k.mh).div_ceil(z.h));
        b.scaled(mean.max(1), mh.max(1))
    };
    let mut layers = Vec::new();
    for (c, name) in ["noise_r", "noise_g", "noise_b"].iter().enumerate() {
        let b = to_map(&snr[c], &|v| {
            let c = canvas::heat(v / 45.0);
            Some([c[0], c[1], c[2], 200])
        });
        layers.push(Layer { name: name.to_string(), role: Role::Overlay, reference: Some(reference), canvas: b });
    }
    let b = to_map(&ratios, &|v| {
        let t = 0.5 + v.max(1e-3).log2() / 2.0;
        let c = canvas::two_sided(t);
        Some([c[0], c[1], c[2], 190])
    });
    layers.push(Layer { name: "noise_ratio".into(), role: Role::Overlay, reference: Some(reference), canvas: b });

    // Dynamic range
    let dr_model = rm.map(|r| {
        let s0 = r.b[1].max(1e-12).sqrt();
        let threshold = |t: f32, c: usize| {
            let (a, b) = (r.a[c], r.b[c]);
            (t * t * a + (t.powi(4) * a * a + 4.0 * t * t * b).sqrt()) / 2.0
        };
        json!({
            "read_noise_normalised": round_to(s0 as f64, 7),
            "read_noise_dn": round_to((s0 * (rb.white_dn - rb.black_dn[1])) as f64, 2),
            "gain_a": r.a[1],
            "e_per_dn": round_to((1.0 / (r.a[1] * (rb.white_dn - rb.black_dn[1])).max(1e-12)) as f64, 3),
            "dr_ev_snr1": ([0, 1, 2].map(|c| round_to((sat[c] / threshold(1.0, c)).log2() as f64, 2))),
            "dr_ev_snr10": ([0, 1, 2].map(|c| round_to((sat[c] / threshold(10.0, c)).log2() as f64, 2))),
            "dr_ev_noise_floor": ([0, 1, 2].map(|c| round_to((sat[c] / s0).log2() as f64, 2))),
        })
    });
    let (borders, source) = black_borders(rb);
    let border = if borders.is_empty() {
        Value::Null
    } else {
        // the largest border counts (combining several would be distorted by offsets)
        let largest_entry = borders.iter().max_by_key(|r| r.w * r.h).copied().unwrap();
        let s = edge_stats(rb, largest_entry);
        json!({
            "source": source,
            "region": region_json(largest_entry),
            "sigma_normalised": s.map(|v| round_to(v.1, 7)),
            "sigma_dn": ([0, 1, 2].map(|c| round_to(s[c].1 * (rb.white_dn - rb.black_dn[c]) as f64, 2))),
            "dr_ev": ([0, 1, 2].map(|c| round_to((sat[c] as f64 / s[c].1.max(1e-9)).log2(), 2))),
        })
    };
    // Scene range (G): 99.9 % against 0.1 % (at least the noise floor)
    let h = rb.histo();
    // class 1 is G1 for Bayer, G for X-Trans
    let g = 1;
    let bottom = h.percentile(g, 0.001).max(rm.map(|r| r.b[1].sqrt()).unwrap_or(1e-4));
    let scene = (h.percentile(g, 0.999) / bottom.max(1e-6)).log2();
    let dr_mode = rb.info.as_ref().and_then(|i| i.fuji_dr).map(|d| json!({"fuji_dr": d, "underexposed_ev": round_to(raw::development::fuji_dr_ev(Some(d)) as f64, 2)}));

    // SNR curve: model per color and measured points (½ EV steps, 25 % quantile of the block variance)
    let evs: Vec<f32> = (0..=56).map(|i| -14.0 + i as f32 * 0.25).collect();
    let model_curve: Vec<Vec<f64>> = (0..3)
        .map(|c| {
            evs.iter()
                .map(|&e| {
                    let v = sat[c] * e.exp2();
                    rm.map(|r| round_to(20.0 * (v / (r.a[c] * v + r.b[c]).max(1e-20).sqrt()).log10() as f64, 2)).unwrap_or(0.0)
                })
                .collect()
        })
        .collect();
    let mut points: Vec<Vec<(f32, f32)>> = vec![Vec::new(); 3];
    for c in 0..3 {
        let mut push_stops: Vec<Vec<(f32, f32)>> = vec![Vec::new(); 30];
        for b in &bl {
            let (m, var, n) = b[c];
            if n == 0 || var <= 0.0 || m <= 0.0 {
                continue;
            }
            let e = (m / sat[c]).log2();
            let s = ((e + 15.0) * 2.0).floor();
            if (0.0..30.0).contains(&s) {
                push_stops[s as usize].push((m, var));
            }
        }
        for st in push_stops.iter_mut() {
            if st.len() < 12 {
                continue;
            }
            st.sort_by(|a, b| a.1.total_cmp(&b.1));
            let var = st[st.len() / 4].1 * 1.1;
            let m = st.iter().map(|p| p.0).sum::<f32>() / st.len() as f32;
            points[c].push(((m / sat[c]).log2(), 20.0 * (m / var.sqrt()).log10()));
        }
    }
    layers.push(Layer { name: "snr_curve".into(), role: Role::Chart, reference: None, canvas: chart_snr(&evs, &model_curve, &points) });

    // usable dynamic range (G): the black border measures the real noise floor; the model
    // extrapolates the intercept b and underestimates the read noise when deep, flat
    // shadows are missing (measured on GR III/X-Pro1: model 2–4 EV above the black border value)
    let usable = if let Some(v) = border["dr_ev"][1].as_f64() {
        json!({"ev": v, "source": "black border (saturation / σ of the masked pixels)"})
    } else if let Some(v) = dr_model.as_ref().and_then(|d| d["dr_ev_snr1"][1].as_f64()) {
        json!({"ev": v, "source": "noise model, SNR 1 (extrapolation, rather too high)"})
    } else {
        Value::Null
    };
    let kz = json!({
        "dynamic_range_usable": usable,
        "block": BLOCK,
        "blocks": {"width": bw, "height": bh, "flat_percent": round_to(flat as f64 / nb.max(1) as f64 * 100.0, 1)},
        "noise_model": rm.map(|r| json!({"a": r.a, "b": r.b, "formel": "variance = a·v + b (v normalised, without white balance)"})),
        "dynamic_range_model": dr_model,
        "dynamic_range_black_border": border,
        "scene_range_ev": round_to(scene as f64, 2),
        "ratio_reference": round_to(reference_value as f64, 3),
        "dr_mode": dr_mode,
        "saturation_rgb": sat.map(|v| round_to(v as f64, 5)),
        "snr": {
            "ev": evs.iter().map(|&e| round_to(e as f64, 2)).collect::<Vec<_>>(),
            "model_db": model_curve,
            "measure": points.iter().map(|p| p.iter().map(|&(e, d)| json!([round_to(e as f64, 2), round_to(d as f64, 2)])).collect::<Vec<_>>()).collect::<Vec<_>>(),
        },
        "maps": {
            "rauschen_r/g/b": "signal-to-noise ratio in dB (0…45 dB, heat scale) in flat blocks",
            "noise_ratio": "measured σ / model σ (green), relative to the 25 % quantile of all blocks; blue < 1 < red; > 1.5 = texture or disturbance",
        },
    });
    Ok((layers, kz))
}

fn chart_snr(evs: &[f32], model: &[Vec<f64>], points: &[Vec<(f32, f32)>]) -> Canvas {
    let (w, h) = (640usize, 360usize);
    let mut b = Canvas::new_at(w, h, canvas::BACKGROUND);
    let a = Axes { l: 46.0, r: w as f32 - 14.0, o: 26.0, u: h as f32 - 30.0, x0: -14.0, x1: 0.0 };
    let ymax = 50.0f32;
    let xt: Vec<(f32, String)> = (-14..=0).step_by(2).map(|e| (e as f32, format!("{e}"))).collect();
    let yt: Vec<(f32, String)> = (0..=5).map(|i| (i as f32 * 10.0 / ymax, format!("{}", i * 10))).collect();
    chart_frame(&mut b, &a, "SNR (dB) über EV unter Sättigung – Linie Modell, Punkte gemessen", &xt, &yt);
    let colors = [canvas::RED, canvas::GREEN, canvas::BLUE];
    for c in 0..3 {
        let pts: Vec<[f32; 2]> = evs.iter().zip(&model[c]).map(|(&e, &d)| [a.px(e), a.py(d as f32 / ymax)]).collect();
        b.polyline(&pts, colors[c]);
        for &(e, d) in &points[c] {
            if e >= a.x0 && e <= a.x1 {
                b.circle(a.px(e), a.py(d / ymax), 3.0, colors[c]);
            }
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze as analyse, Kind, Sensor};

    /// Simple random generator (xorshift) with Gaussian via Box-Muller.
    struct Z(u64);
    impl Z {
        fn uniform(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 11) as f64 / (1u64 << 53) as f64) as f32
        }
        fn gauss(&mut self) -> f32 {
            let (u, v) = (self.uniform().max(1e-9), self.uniform());
            (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
        }
    }

    #[test]
    fn noise_model_and_dynamic_range() {
        // Gradient 0.0005 … 0.6 with Poisson-Gaussian noise: a = 1e-4, b = 1e-6 (σ_read = 1e-3)
        let (w, h) = (480, 480);
        let mut z = Z(0x1234_5678);
        let (a, b) = (1e-4f32, 1e-6f32);
        let mut m = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let v = 0.0005 * (1200.0f32).powf(x as f32 / w as f32);
                m[y * w + x] = v + (a * v + b).sqrt() * z.gauss();
            }
        }
        let rggb = std::array::from_fn(|y| std::array::from_fn(|x| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        }));
        let rb = RawImage::from_mosaic(m, w, h, rggb, Sensor::Bayer, [1.0; 3]);
        let e = analyse(&rb, Kind::Noise, &Options { size: 240, ..Default::default() }).unwrap();
        let rm = &e.metrics["noise_model"];
        let ga = rm["a"][1].as_f64().unwrap();
        assert!((ga / 1e-4 - 1.0).abs() < 0.3, "a = {ga}");
        // Noise floor √b = 1e-3 at nominal saturation 1.0 → log2(1000) ≈ 9.97 EV
        let dr = e.metrics["dynamic_range_model"]["dr_ev_noise_floor"][1].as_f64().unwrap();
        assert!((dr - 9.97).abs() < 0.4, "DR = {dr}");
        assert!(e.layer("snr_curve").is_some());
    }
}
