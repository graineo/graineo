//! Markesteijn (X-Trans) on the GPU against the CPU (reference). There is no X-Trans file available for
//! testing, so an X-Trans mosaic is built from a real, demosaiced image (color filter pattern
//! of a Fuji camera, 6×6, applied to the linear camera RGB).
//! `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test xtrans -- --ignored --nocapture`

use std::time::Instant;

fn lab_from_srgb16(p: [u16; 3]) -> [f32; 3] {
    let lin = |v: u16| {
        let c = v as f32 / 65535.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    let w = [0.95047, 1.0, 1.08883];
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / w[0]), f(y / w[1]), f(z / w[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

pub fn de2000(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (l1, a1, b1) = (a[0] as f64, a[1] as f64, a[2] as f64);
    let (l2, a2, b2) = (b[0] as f64, b[1] as f64, b[2] as f64);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cm = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + 25f64.powi(7))).sqrt());
    let a1p = (1.0 + g) * a1;
    let a2p = (1.0 + g) * a2;
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let hp = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            let h = b.atan2(a).to_degrees();
            if h < 0.0 {
                h + 360.0
            } else {
                h
            }
        }
    };
    let h1p = hp(b1, a1p);
    let h2p = hp(b2, a2p);
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p - h1p > 180.0 {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let dhp_big = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lpm = (l1 + l2) / 2.0;
    let cpm = (c1p + c2p) / 2.0;
    let hpm = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hpm - 30.0).to_radians().cos() + 0.24 * (2.0 * hpm).to_radians().cos() + 0.32 * (3.0 * hpm + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hpm - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hpm - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cpm.powi(7) / (cpm.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lpm - 50.0).powi(2) / (20.0 + (lpm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cpm;
    let sh = 1.0 + 0.015 * cpm * t;
    let rt = -(2.0 * dtheta.to_radians()).sin() * rc;
    let v = (dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhp_big / sh).powi(2) + rt * (dcp / sc) * (dhp_big / sh);
    v.max(0.0).sqrt() as f32
}


const XT: [[u8; 6]; 6] = [[1, 2, 1, 1, 0, 1], [0, 1, 0, 2, 1, 2], [1, 2, 1, 1, 0, 1], [1, 0, 1, 1, 2, 1], [2, 1, 2, 0, 1, 0], [1, 0, 1, 1, 2, 1]];

fn encode_srgb(v: f32) -> u16 {
    // simple display: linear → sRGB (encoding), with exposure factor 3 (midtones ≈ 0.5)
    (raw::srgb_oetf((v * 3.0).clamp(0.0, 1.0)) * 65535.0 + 0.5) as u16
}

#[test]
#[ignore]
fn markesteijn_gpu_vs_cpu() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let doc = rawdev::RawDoc::open(std::path::Path::new(&p), &|| true).unwrap().unwrap();
    let st = doc.full_base().unwrap();
    let (w, h) = (st.w, st.h);
    println!("source: {w}×{h} ({:.1} MP)", (w * h) as f64 / 1e6);
    // X-Trans mosaic from the RGB image
    let mut m = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            m[y * w + x] = st.px[y * w + x][XT[y % 6][x % 6] as usize];
        }
    }
    let mut zc = Vec::new();
    let mut cpu = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        cpu = raw::development::demosaic_mosaic(&m, w, h, XT, raw::development::Demosaic::Markesteijn3, raw::Sensor::XTrans);
        zc.push(t.elapsed().as_millis());
    }
    println!("CPU Markesteijn 3-Pass: {zc:?} ms");
    let Some(g) = rawdev::gpu::gpu() else {
        println!("no GPU");
        return;
    };
    println!("GPU: {} (largest buffer {} MB)", g.name, rawdev::gpu::max_buffer().unwrap_or(0) / 1_000_000);
    let mut zg = Vec::new();
    let mut gp = None;
    for _ in 0..5 {
        let t = Instant::now();
        gp = rawdev::gpu::xtrans::markesteijn(&m, w, h, XT);
        zg.push(t.elapsed().as_millis());
    }
    println!("GPU Markesteijn 3-pass (whole path incl. upload/readback): {zg:?} ms");
    if std::env::var("GRAINEO_GPU_PROFILE").is_ok() {
        rawdev::timings::collect(true);
        let _ = rawdev::gpu::xtrans::markesteijn(&m, w, h, XT);
        let mut per: std::collections::BTreeMap<String, f64> = Default::default();
        for (n, ms) in rawdev::timings::take_all() {
            *per.entry(n).or_default() += ms;
        }
        let mut v: Vec<_> = per.into_iter().collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for (n, ms) in v {
            println!("  {n}: {ms:.1} ms");
        }
    }
    let gp = gp.expect("GPU result");
    // linear deviation
    let (mut max, mut sum, mut n) = (0f32, 0f64, 0usize);
    for (a, b) in cpu.iter().zip(&gp) {
        for c in 0..3 {
            let d = (a[c] - b[c]).abs();
            max = max.max(d);
            sum += d as f64;
            n += 1;
        }
    }
    println!("linear: largest deviation {max:.3e}, mean {:.3e}", sum / n as f64);
    // ΔE00 of the displayed images
    let (mut de_sum, mut de_max, mut above1, mut above01) = (0f64, 0f32, 0usize, 0usize);
    for (a, b) in cpu.iter().zip(&gp) {
        let pa = [encode_srgb(a[0]), encode_srgb(a[1]), encode_srgb(a[2])];
        let pb = [encode_srgb(b[0]), encode_srgb(b[1]), encode_srgb(b[2])];
        let d = de2000(lab_from_srgb16(pa), lab_from_srgb16(pb));
        de_sum += d as f64;
        de_max = de_max.max(d);
        if d > 1.0 {
            above1 += 1;
        }
        if d > 0.1 {
            above01 += 1;
        }
    }
    let nn = cpu.len();
    println!("ΔE00 GPU vs. CPU: mean {:.5}, largest {:.3}, above 0.1: {} ({:.4} %), above 1: {} ({:.4} %)", de_sum / nn as f64, de_max, above01, above01 as f64 * 100.0 / nn as f64, above1, above1 as f64 * 100.0 / nn as f64);
}
