//! Fused GPU path (detail + output stage) against the CPU (reference), full resolution:
//! `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test gpu_full -- --ignored --nocapture`
//! Measured: time per path (`render_full`), ΔE00 of the finished image against the CPU and against GPU detail + CPU output.
//! Optional `GRAINEO_BENCH_SETTINGS=<json>` (develop settings as in the sidecar).

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


fn compare(a: &develop::Rgb16, b: &develop::Rgb16, name: &str) -> (f64, f32, usize) {
    let n = a.width * a.height;
    let (mut sum, mut max, mut above1) = (0f64, 0f32, 0usize);
    for i in 0..n {
        let pa = [a.data[3 * i], a.data[3 * i + 1], a.data[3 * i + 2]];
        let pb = [b.data[3 * i], b.data[3 * i + 1], b.data[3 * i + 2]];
        let d = de2000(lab_from_srgb16(pa), lab_from_srgb16(pb));
        sum += d as f64;
        max = max.max(d);
        if d > 1.0 {
            above1 += 1;
        }
    }
    println!("ΔE00 {name}: mean {:.5}, largest {:.3}, above 1: {} of {} ({:.4} %)", sum / n as f64, max, above1, n, above1 as f64 * 100.0 / n as f64);
    (sum / n as f64, max, above1)
}

#[test]
#[ignore]
fn fusion_vs_cpu() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let p = std::path::PathBuf::from(p);
    let Some(g) = rawdev::gpu::gpu() else {
        println!("no GPU");
        return;
    };
    println!("GPU: {}", g.name);
    let mut s = develop::Settings::default();
    if let Ok(j) = std::env::var("GRAINEO_BENCH_SETTINGS") {
        s = serde_json::from_str(&std::fs::read_to_string(j).unwrap()).unwrap();
    }
    let n_runs: usize = std::env::var("GRAINEO_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    // per path: n runs (each with a freshly computed base, `release_memory` releases it); minimum and median
    let timed_render = |doc: &rawdev::RawDoc, name: &str| -> develop::Rgb16 {
        let mut z = Vec::new();
        let mut latest = None;
        for _ in 0..n_runs.max(2) {
            doc.release_memory();
            let t = Instant::now();
            let _ = doc.full_base().unwrap();
            let tb = t.elapsed().as_millis();
            let t = Instant::now();
            let img = doc.render_full(&s).unwrap();
            z.push((t.elapsed().as_millis(), tb));
            latest = Some(img);
        }
        let mut total: Vec<u128> = z.iter().skip(1).map(|x| x.0).collect();
        total.sort();
        let base_min = z.iter().skip(1).map(|x| x.1).min().unwrap();
        println!("{name}: render_voll (without the first run = shader compilation) min {} ms, median {} ms; of which CPU base (decoding/highlights/demosaic) min {} ms", total[0], total[total.len() / 2], base_min);
        latest.unwrap()
    };
    // 1. fused GPU path
    let doc = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    let a = timed_render(&doc, "fusioniert (GPU: Detail + Ausgabe)");
    // 2. GPU detail + CPU output (previous GPU path)
    unsafe { std::env::set_var("GRAINEO_GPU_FUSION", "0") };
    let doc2 = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    let b = timed_render(&doc2, "GPU-Detail + CPU-Ausgabe");
    unsafe { std::env::remove_var("GRAINEO_GPU_FUSION") };
    // 3. all CPU (reference)
    pools::set_gpu_allowed(false); // `GRAINEO_GPU` is read only once per process
    let doc3 = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    let c = timed_render(&doc3, "CPU (Referenz)");
    pools::set_gpu_allowed(true);
    let (m1, _, _) = compare(&a, &c, "fused vs. CPU");
    let (m2, _, _) = compare(&a, &b, "fused vs. GPU detail + CPU output (output stage only)");
    compare(&b, &c, "GPU detail + CPU output vs. CPU (detail stage only)");
    assert!(m1 < 0.05 && m2 < 0.05, "mean deviation too large");
}

/// Several slider settings: the fused GPU path against the CPU (ΔE00). Checks shadows/highlights/whites/blacks
/// (luminance map), contrast, presence, black & white, camera look, plus look stage/texture on the CPU afterwards.
#[test]
#[ignore]
fn fusion_slider_values() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let p = std::path::PathBuf::from(p);
    if rawdev::gpu::gpu().is_none() {
        return;
    }
    let mut states: Vec<(&str, develop::Settings)> = Vec::new();
    let d = develop::Settings::default;
    let mut s = d();
    s.exposure = 0.8;
    s.contrast = 0.4;
    states.push(("exposure +0.8, contrast 0.4", s));
    let mut s = d();
    s.shadow = 0.7;
    states.push(("shadows only +0.7", s));
    let mut s = d();
    s.highlight = -0.6;
    states.push(("highlights only −0.6", s));
    let mut s = d();
    s.raw.whites = 0.4;
    states.push(("whites only +0.4", s));
    let mut s = d();
    s.raw.blacks = -0.3;
    states.push(("blacks only −0.3", s));
    let mut s = d();
    s.saturation = 0.3;
    s.vibrance = 0.4;
    states.push(("saturation 0.3, vibrance 0.4", s));
    let mut s = d();
    s.monochrome = true;
    states.push(("black & white", s));
    let mut s = d();
    s.raw.nr_luma = 40.0;
    s.raw.nr_color = 50.0;
    s.raw.lens = true;
    states.push(("noise reduction 40/50, lens on", s));
    let mut s = d();
    s.profile_amount = 0.6;
    if let Some(l) = std::env::var("GRAINEO_BENCH_LOOK").ok() {
        s.raw.look = l;
    }
    states.push(("camera look (GRAINEO_BENCH_LOOK) with intensity 0.6", s));
    let mut s = d();
    s.raw.texture = 0.5;
    s.exposure = -0.4;
    states.push(("texture 0.5 (CPU after GPU), exposure −0.4", s));
    for (name, s) in &states {
        let doc = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
        let a = doc.render_full(s).unwrap();
        pools::set_gpu_allowed(false); // `GRAINEO_GPU` is read only once per process
        let doc3 = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
        let c = doc3.render_full(s).unwrap();
        pools::set_gpu_allowed(true);
        print!("{name}: ");
        compare(&a, &c, "GPU vs. CPU");
    }
}
