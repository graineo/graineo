//! Demosaic comparison: the same detail area with RCD or Markesteijn 1-/3-pass and bilinear,
//! plus a false color/moiré warning map.
//!
//! Measure (own formula, idea similar to the false color masks in RawTherapee/darktable): after
//! demosaicing (white-balanced camera RGB) each channel is variance-stabilized with √; the
//! chroma components `√R − √G` and `√B − √G` are high-pass filtered (minus 9×9 mean), their energy
//! averaged over 5×5 and reduced by the noise contribution expected from the noise model. What remains
//! is high-frequency color that does not come from noise: moiré, false color on fine structures,
//! zipper fringes. Additionally: deviation between the methods (same domain) and, for
//! Bayer, the G1/G2 imbalance (cause of maze patterns).

use raw::development::Demosaic;
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::canvas::{self, Canvas};
use crate::{region_json, round_to, Region, Layer, GridMap, Options, RawImage, Result, Role, Sensor};

/// Margin around detail areas/strips (demosaic support)
const MARGIN: usize = 24;
/// Display scale of the warning map: chroma amplitude (√ units) that counts as "full"
const SCALE_FALSE: f32 = 0.035;
const SCALE_DEV: f32 = 0.05;

pub(crate) fn method(s: Sensor) -> Vec<(Demosaic, &'static str)> {
    if s == Sensor::Bayer {
        vec![(Demosaic::Rcd, "rcd"), (Demosaic::Bilinear, "bilinear")]
    } else {
        vec![(Demosaic::Markesteijn3, "markesteijn3"), (Demosaic::Markesteijn1, "markesteijn1"), (Demosaic::Bilinear, "bilinear")]
    }
}

/// Sub-mosaic (sensor area `r` plus margin, clipped at the sensor bounds), white-balanced,
/// with shifted pattern. Returns (values, width, height, pattern, offset of `r` within it).
pub(crate) fn sub_mosaic(rb: &RawImage, r: Region, border: usize) -> (Vec<f32>, usize, usize, [[u8; 6]; 6], (usize, usize)) {
    let x0 = r.x.saturating_sub(border);
    let y0 = r.y.saturating_sub(border);
    let x1 = (r.x + r.w + border).min(rb.width);
    let y1 = (r.y + r.h + border).min(rb.height);
    let (w, h) = (x1 - x0, y1 - y0);
    let mut m = vec![0f32; w * h];
    m.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
        let y = y0 + j;
        for (i, o) in row.iter_mut().enumerate() {
            let x = x0 + i;
            *o = rb.norm[y * rb.width + x] * rb.wb[rb.pattern[y % 6][x % 6] as usize];
        }
    });
    let pat = std::array::from_fn(|y| std::array::from_fn(|x| rb.pattern[(y0 + y) % 6][(x0 + x) % 6]));
    (m, w, h, pat, (r.x - x0, r.y - y0))
}

fn cut_out(px: &[[f32; 3]], w: usize, off: (usize, usize), aw: usize, ah: usize) -> Vec<[f32; 3]> {
    let mut o = Vec::with_capacity(aw * ah);
    for y in off.1..off.1 + ah {
        o.extend_from_slice(&px[y * w + off.0..y * w + off.0 + aw]);
    }
    o
}

/// Box mean (radius r) of a plane, borders mirrored and clipped.
fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        let mut acc = 0f32;
        let mut n = 0usize;
        for x in 0..=r.min(w - 1) {
            acc += s[x];
            n += 1;
        }
        for x in 0..w {
            row[x] = acc / n as f32;
            if x + r + 1 < w {
                acc += s[x + r + 1];
                n += 1;
            }
            if x >= r {
                acc -= s[x - r];
                n -= 1;
            }
        }
    });
    let mut out = vec![0f32; w * h];
    // vertical, column by column in row blocks (simple, fast enough)
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let a = y.saturating_sub(r);
        let b = (y + r + 1).min(h);
        for yy in a..b {
            let t = &tmp[yy * w..(yy + 1) * w];
            for (o, v) in row.iter_mut().zip(t) {
                *o += v;
            }
        }
        let n = (b - a) as f32;
        row.iter_mut().for_each(|v| *v /= n);
    });
    out
}

/// √ domain per channel.
fn root_of(px: &[[f32; 3]]) -> Vec<[f32; 3]> {
    px.par_iter().map(|p| [p[0].max(0.0).sqrt(), p[1].max(0.0).sqrt(), p[2].max(0.0).sqrt()]).collect()
}

/// Excess high-frequency chroma energy per pixel (√ units²).
pub(crate) fn chroma_hf(px: &[[f32; 3]], w: usize, h: usize, rb: &RawImage) -> Vec<f32> {
    let s = root_of(px);
    let cr: Vec<f32> = s.par_iter().map(|p| p[0] - p[1]).collect();
    let cb: Vec<f32> = s.par_iter().map(|p| p[2] - p[1]).collect();
    let lr = box_blur(&cr, w, h, 4);
    let lb = box_blur(&cb, w, h, 4);
    let e: Vec<f32> = (0..w * h).into_par_iter().map(|i| (cr[i] - lr[i]).powi(2) + (cb[i] - lb[i]).powi(2)).collect();
    let e = box_blur(&e, w, h, 2);
    // expected noise: Var(√x) ≈ Var(x)/(4x); WB factor k: Var(k·v) = k·a·(k v) + k²·b
    let rm = rb.noise_model();
    let wb = rb.wb;
    let sm = box_blur(&px.iter().map(|p| p[1]).collect::<Vec<_>>(), w, h, 2);
    // exclude the surroundings of clipped pixels: demosaicing inevitably produces color there
    let sat = rb.saturation_color();
    let cutoff: [f32; 3] = std::array::from_fn(|c| 0.95 * sat[c] * wb[c]);
    let clipped: Vec<f32> = px.par_iter().map(|p| if (0..3).any(|c| p[c] >= cutoff[c]) { 1.0 } else { 0.0 }).collect();
    let near = box_blur(&clipped, w, h, 4);
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let noise_level = match rm {
                Some(m) => {
                    let v = |c: usize, x: f32| -> f32 {
                        let x = x.max(1e-4);
                        (wb[c] * m.a[c] * x + wb[c] * wb[c] * m.b[c]) / (4.0 * x)
                    };
                    let g = sm[i].max(1e-4);
                    // R/B assumed to be exposed about like G; demosaicing smooths to about half
                    0.5 * (v(0, g) + v(2, g) + 2.0 * v(1, g))
                }
                None => 0.0,
            };
            if near[i] > 0.0 {
                return 0.0;
            }
            (e[i] - 3.0 * noise_level).max(0.0)
        })
        .collect()
}

/// Like the preview: camera → sRGB (camera clip at 1), exposure of the default development,
/// simple tone curve (`raw::tone_curve_srgb8`), then nearest neighbor to `zoom`.
fn render(px: &[[f32; 3]], w: usize, h: usize, rb: &RawImage, zoom: usize) -> Canvas {
    let lin = raw::Linear { width: w, height: h, px: px.to_vec() };
    let srgb = raw::camera_to_srgb(&lin, [1.0; 3], &rb.camera_srgb(), true);
    let img = raw::tone_curve_srgb8(&srgb, rb.standard_ev());
    Canvas::from_rgb(w, h, img.as_raw()).scaled(w * zoom, h * zoom)
}

fn heat_layer(v: &[f32], w: usize, h: usize, scale: f32, zoom: usize) -> Canvas {
    let mut b = Canvas::new_at(w * zoom, h * zoom, canvas::TRANSPARENT);
    for y in 0..h {
        for x in 0..w {
            let t = (v[y * w + x].sqrt() / scale).min(1.0);
            if t < 0.08 {
                continue;
            }
            let c = canvas::heat(t);
            let a = (90.0 + 165.0 * t) as u8;
            b.rect((x * zoom) as isize, (y * zoom) as isize, zoom, zoom, [c[0], c[1], c[2], a]);
        }
    }
    b
}

fn stats(v: &[f32]) -> Value {
    let mut amp: Vec<f32> = v.iter().map(|e| e.sqrt()).collect();
    let mean = amp.iter().sum::<f32>() / amp.len().max(1) as f32;
    let p99 = crate::percentile(&mut amp, 0.99);
    let warn = v.iter().filter(|e| e.sqrt() > 0.5 * SCALE_FALSE).count();
    json!({"mean": round_to(mean as f64, 5), "p99": round_to(p99 as f64, 5), "warning_fraction_percent": round_to(warn as f64 / v.len().max(1) as f64 * 100.0, 2)})
}

pub(crate) fn analyze(rb: &RawImage, o: &Options) -> Result<(Vec<Layer>, Value)> {
    let zoom = o.zoom.clamp(1, 16);
    let max_px = (2048 / zoom).max(16);
    let mut a = rb.detail(o, 128.min(max_px), 96.min(max_px));
    a.w = a.w.min(max_px);
    a.h = a.h.min(max_px);
    let z = rb.crop;
    let sr = Region { x: z.x + a.x, y: z.y + a.y, w: a.w, h: a.h };
    // Minimum size for RCD (64×32 plus margin)
    let border = MARGIN.max(64usize.saturating_sub(a.w.min(a.h)) / 2 + 8);
    let (m, mean, mh, pat, off) = sub_mosaic(rb, sr, border);
    let mut layers = Vec::new();
    let mut methods = serde_json::Map::new();
    let mut outcomes: Vec<(&str, Vec<[f32; 3]>)> = Vec::new();
    for (i, (d, name)) in method(rb.sensor).into_iter().enumerate() {
        let t = std::time::Instant::now();
        let px = raw::development::demosaic_mosaic(&m, mean, mh, pat, d, rb.sensor);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let px = cut_out(&px, mean, off, a.w, a.h);
        let hf = chroma_hf(&px, a.w, a.h, rb);
        let mut st = stats(&hf);
        st["duration_ms"] = json!(round_to(ms, 1));
        methods.insert(name.to_string(), st);
        layers.push(Layer { name: name.to_string(), role: Role::View, reference: Some(a), canvas: render(&px, a.w, a.h, rb, zoom) });
        if i == 0 {
            layers.push(Layer { name: "false_color".into(), role: Role::Overlay, reference: Some(a), canvas: heat_layer(&hf, a.w, a.h, SCALE_FALSE, zoom) });
        }
        outcomes.push((name, px));
    }
    // Deviation reference ↔ second method (√ domain, all channels)
    let (rn, rp) = (&outcomes[0].0, root_of(&outcomes[0].1));
    let (vn, vp) = (&outcomes[1].0, root_of(&outcomes[1].1));
    let sat = rb.saturation_color();
    let clipped = |p: &[f32; 3]| (0..3).any(|c| p[c] >= 0.9 * sat[c] * rb.wb[c]);
    let dev: Vec<f32> = rp
        .iter()
        .zip(&vp)
        .zip(&outcomes[0].1)
        .map(|((p, q), raw_bytes)| if clipped(raw_bytes) { 0.0 } else { (0..3).map(|c| (p[c] - q[c]).powi(2)).sum::<f32>() })
        .collect();
    let dev_s = box_blur(&dev, a.w, a.h, 1);
    layers.push(Layer { name: "deviation".into(), role: Role::Overlay, reference: Some(a), canvas: heat_layer(&dev_s, a.w, a.h, SCALE_DEV, zoom) });
    let dev_mean = dev.iter().map(|v| v.sqrt()).sum::<f32>() / dev.len().max(1) as f32;

    // Bayer: G1/G2 imbalance in the detail area (mosaic, relative to the green mean)
    let mut g12 = Value::Null;
    if rb.sensor == Sensor::Bayer {
        let (mut s1, mut s2, mut n1, mut n2) = (0f64, 0f64, 0u32, 0u32);
        for y in sr.y..sr.y + sr.h {
            for x in sr.x..sr.x + sr.w {
                match rb.class(x, y) {
                    1 => {
                        s1 += rb.value(x, y) as f64;
                        n1 += 1
                    }
                    2 => {
                        s2 += rb.value(x, y) as f64;
                        n2 += 1
                    }
                    _ => {}
                }
            }
        }
        let (g1, g2) = (s1 / n1.max(1) as f64, s2 / n2.max(1) as f64);
        g12 = json!(round_to((g1 - g2) / (0.5 * (g1 + g2)).max(1e-9) * 100.0, 3));
    }

    // whole image: warning map with the reference method
    let mut map_json = Value::Null;
    if o.full_image {
        let (canvas, kj) = full_map(rb, o)?;
        map_json = kj;
        layers.push(Layer { name: "false_color_map".into(), role: Role::Overlay, reference: Some(Region { x: 0, y: 0, w: z.w, h: z.h }), canvas });
    }

    let kz = json!({
        "crop": region_json(a),
        "zoom_percent": zoom * 100,
        "reference": rn,
        "method": Value::Object(methods),
        "deviation": {"between": [rn, vn], "mean": round_to(dev_mean as f64, 5)},
        "g1_g2_imbalance_percent": g12,
        "scale": {"false_color_full": SCALE_FALSE, "deviation_full": SCALE_DEV, "unit": "Amplitude in √(normiert·WB)"},
        "whole_image": map_json,
        "hint": "False colour = high-frequency chroma energy after subtracting the expected noise (√ domain). Warning share: pixels above half the scale.",
    });
    Ok((layers, kz))
}

/// Warning map over the whole image in strips (reference method), mean per map block.
fn full_map(rb: &RawImage, o: &Options) -> Result<(Canvas, Value)> {
    let z = rb.crop;
    let k = GridMap::new_at(z, o.size);
    let (d, name) = method(rb.sensor)[0];
    let mut sum_total = vec![0f64; k.mean * k.mh];
    let mut count = vec![0u32; k.mean * k.mh];
    let t = std::time::Instant::now();
    const STRIPE: usize = 384;
    let mut y = 0;
    while y < z.h {
        let hh = STRIPE.min(z.h - y);
        let sr = Region { x: z.x, y: z.y + y, w: z.w, h: hh };
        let (m, mean, mh, pat, off) = sub_mosaic(rb, sr, MARGIN);
        let px = raw::development::demosaic_mosaic(&m, mean, mh, pat, d, rb.sensor);
        drop(m);
        let px = cut_out(&px, mean, off, z.w, hh);
        let hf = chroma_hf(&px, z.w, hh, rb);
        for j in 0..hh {
            let yy = y + j;
            let my = (yy * k.mh / z.h).min(k.mh - 1);
            for x in 0..z.w {
                let i = my * k.mean + k.xmap[x] as usize;
                sum_total[i] += hf[j * z.w + x] as f64;
                count[i] += 1;
            }
        }
        y += hh;
    }
    let values: Vec<f32> = sum_total.iter().zip(&count).map(|(s, &n)| (s / n.max(1) as f64).sqrt() as f32).collect();
    // Display: block mean is smaller than peaks → half scale
    let mut b = Canvas::new_at(k.mean, k.mh, canvas::TRANSPARENT);
    for (i, &v) in values.iter().enumerate() {
        let t = (v / (0.5 * SCALE_FALSE)).min(1.0);
        if t < 0.1 {
            continue;
        }
        let c = canvas::heat(t);
        b.set(i % k.mean, i / k.mean, [c[0], c[1], c[2], (80.0 + 175.0 * t) as u8]);
    }
    // Hot spots: largest blocks with spacing (image coordinates of the block center)
    let mut idx: Vec<usize> = (0..values.len()).collect();
    idx.sort_by(|&a, &b| values[b].total_cmp(&values[a]));
    let mut points: Vec<(usize, usize, f32)> = Vec::new();
    let spacing = (k.mean.max(k.mh) / 12).max(4);
    for i in idx {
        if points.len() >= 8 || values[i] < 0.2 * SCALE_FALSE {
            break;
        }
        let (mx, my) = (i % k.mean, i / k.mean);
        if points.iter().all(|p| (p.0 as isize - mx as isize).abs().max((p.1 as isize - my as isize).abs()) as usize >= spacing) {
            points.push((mx, my, values[i]));
        }
    }
    let focal_points: Vec<Value> = points
        .iter()
        .map(|&(mx, my, v)| json!({"x": (mx * 2 + 1) * z.w / (2 * k.mean), "y": (my * 2 + 1) * z.h / (2 * k.mh), "amplitude": round_to(v as f64, 4)}))
        .collect();
    let mut amp = values.clone();
    let kj = json!({
        "method": name,
        "duration_ms": round_to(t.elapsed().as_secs_f64() * 1000.0, 0),
        "map": {"width": k.mean, "height": k.mh},
        "block_mean": round_to((values.iter().sum::<f32>() / values.len().max(1) as f32) as f64, 5),
        "block_p99": round_to(crate::percentile(&mut amp, 0.99) as f64, 5),
        "warning_blocks_percent": round_to(values.iter().filter(|&&v| v > 0.25 * SCALE_FALSE).count() as f64 / values.len().max(1) as f64 * 100.0, 2),
        "focus_points": focal_points,
    });
    Ok((b, kj))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{analyze as analyse, Kind};

    fn rggb() -> [[u8; 6]; 6] {
        std::array::from_fn(|y| std::array::from_fn(|x| match (y % 2, x % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        }))
    }

    #[test]
    fn xtrans_methods_are_distinct_in_the_same_neutral_output() {
        let pattern = b"GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG";
        let pat = std::array::from_fn(|y| std::array::from_fn(|x| match pattern[y*6+x] { b'R' => 0, b'G' => 1, _ => 2 }));
        let (w,h) = (192,192);
        let m: Vec<f32> = (0..w*h).map(|i| {
            let (x,y) = ((i%w) as f32-96.0,(i/w) as f32-96.0);
            0.45 + 0.35*(0.034*(x*x+y*y)).cos()
        }).collect();
        let rb = RawImage::from_mosaic(m,w,h,pat,Sensor::XTrans,[1.0;3]);
        let o = Options { detail: Some(Region { x: 32, y: 32, w: 128, h: 128 }), zoom: 1, full_image: false, ..Default::default() };
        let result = analyse(&rb, Kind::DemosaicComparison, &o).unwrap();
        let a = &result.layer("markesteijn3").unwrap().canvas.rgba;
        let deviation = |name: &str| -> f64 {
            let b = &result.layer(name).unwrap().canvas.rgba;
            a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(x,y)| (0..3).map(|c| (x[c] as f64-y[c] as f64).abs()).sum::<f64>()).sum::<f64>() / (a.len()/4*3) as f64
        };
        let bilinear = deviation("bilinear");
        let one = deviation("markesteijn1");
        eprintln!("X-Trans mittlere RGB-Abweichung in 8 Bit: bilinear={bilinear:.5}, Markesteijn 1={one:.5}");
        assert!(bilinear > 0.1, "bilinear: {bilinear}");
        assert!(one > 0.001, "1 gegen 3: {one}");
        let next = Options { detail: Some(Region { x: 40, y: 40, w: 96, h: 96 }), ..o.clone() };
        analyse(&rb, Kind::DemosaicComparison, &next).unwrap();
        assert!(rb.cache.lock().unwrap().is_empty(), "transient crops must not accumulate in the image cache");
    }

    #[test]
    fn zone_plate_produces_false_color_flat_area_does_not() {
        // left: flat gray; right: zone plate → moiré/false color
        // (with some noise, otherwise the noise model cannot be determined)
        let (w, h) = (256, 128);
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut m = vec![0.25f32; w * h];
        for y in 0..h {
            for x in 0..w {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let r = ((rng >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.004;
                // right: zone plate (rings beyond the R/B Nyquist limit) → moiré in all directions
                let (dx, dy) = (x as f32 - 192.0, y as f32 - 64.0);
                let zp = 0.25 + 0.2 * (0.025 * (dx * dx + dy * dy)).cos();
                m[y * w + x] = if x >= 128 { zp } else { 0.25 } + r;
            }
        }
        let rb = RawImage::from_mosaic(m, w, h, rggb(), Sensor::Bayer, [1.0; 3]);
        let o = Options { detail: Some(Region { x: 0, y: 16, w: 256, h: 96 }), zoom: 1, full_image: false, ..Default::default() };
        let e = analyse(&rb, Kind::DemosaicComparison, &o).unwrap();
        let f = &e.layer("false_color").unwrap().canvas;
        let alpha = |x0: usize, x1: usize| (0..96).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| f.get(x, y)[3] as u32).sum::<u32>() / (96 * (x1 - x0)) as u32;
        assert!(alpha(20, 100) < 10, "flat without warning");
        assert!(alpha(150, 250) > 40, "zone plate warned: {}", alpha(150, 250));
        assert!(e.layer("rcd").is_some() && e.layer("bilinear").is_some());
    }
}

#[cfg(test)]
mod check_file {
    use super::*;

    #[test]
    #[ignore]
    fn real_xtrans_method_difference() {
        let path = std::env::var("SENSOR_VIEW_FILE").expect("SENSOR_VIEW_FILE");
        let rb = RawImage::open(path).unwrap();
        assert_eq!(rb.sensor, Sensor::XTrans);
        rb.with_raw(|raw| {
            let data = raw.raw_data();
            for y in (rb.crop.y..rb.crop.y + rb.crop.h).step_by(997) {
                for x in (rb.crop.x..rb.crop.x + rb.crop.w).step_by(991) {
                    assert!((rb.value_dn(x,y)-data[y*rb.width+x]).abs() < 0.01, "raw DN mismatch at {x},{y}");
                }
            }
        }).unwrap();
        let z = rb.crop;
        let o = Options { detail: Some(Region { x: z.w / 2, y: z.h / 2, w: 256, h: 192 }), zoom: 1, full_image: false, ..Default::default() };
        let e = crate::analyze(&rb, crate::Kind::DemosaicComparison, &o).unwrap();
        let a = &e.layer("markesteijn3").unwrap().canvas.rgba;
        let deviation = |name: &str| -> f64 {
            let b = &e.layer(name).unwrap().canvas.rgba;
            a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(x,y)| (0..3).map(|c| (x[c] as f64-y[c] as f64).abs()).sum::<f64>()).sum::<f64>() / (a.len()/4*3) as f64
        };
        let b = deviation("bilinear");
        let m = deviation("markesteijn1");
        eprintln!("RAW: mittlere RGB-Abweichung in 8 Bit: bilinear={b:.5}, Markesteijn 1={m:.5}");
        assert!(b > 0.01 && m > 0.001);
    }

    /// Detail demosaic against the same area from a larger sub-image (pattern offset).
    /// `SENSOR_VIEW_FILE=<raw> cargo test --release -p sensor_view -- --ignored`
    #[test]
    #[ignore]
    fn detail_matches_full_image() {
        let Ok(p) = std::env::var("SENSOR_VIEW_FILE") else { return };
        let rb = RawImage::open(p).unwrap();
        let z = rb.crop;
        let sr = Region { x: z.x + 5350, y: z.y + 3737 - 3000, w: 128, h: 96 };
        for (d, name) in method(rb.sensor) {
            let (m, mean, mh, pat, off) = sub_mosaic(&rb, sr, MARGIN);
            let a = cut_out(&raw::development::demosaic_mosaic(&m, mean, mh, pat, d, rb.sensor), mean, off, sr.w, sr.h);
            let (m, mean, mh, pat, off) = sub_mosaic(&rb, sr, 301);
            let b = cut_out(&raw::development::demosaic_mosaic(&m, mean, mh, pat, d, rb.sensor), mean, off, sr.w, sr.h);
            let d: f32 = a.iter().zip(&b).map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).fold(0.0, f32::max);
            eprintln!("{name}: max Abweichung {d}");
        }
    }
}
