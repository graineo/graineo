//! GPU against CPU (reference) at full resolution: `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test gpu -- --ignored --nocapture`
//! Measured: time of both paths, max/mean deviation of the linear stage, ΔE00 of the finished image.

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

#[test]
#[ignore]
fn gpu_vs_cpu() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let p = std::path::PathBuf::from(p);
    let Some(g) = rawdev::gpu::gpu() else {
        println!("no GPU");
        return;
    };
    println!("GPU: {}", g.name);
    let mut s = develop::Settings::default();
    // lens correction and noise reduction on
    s.raw.nr_luma = 30.0;
    let doc = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    let _ = doc.full_base().unwrap();
    // GPU (first run compiles the shaders)
    let t = Instant::now();
    let st_gpu = doc.full_stage(&s).unwrap();
    println!("GPU voll_stufe: {:.0?}", t.elapsed());
    let doc2 = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    let _ = doc2.full_base().unwrap();
    let t = Instant::now();
    let st_gpu2 = doc2.full_stage(&s).unwrap();
    println!("GPU voll_stufe (second run): {:.0?}", t.elapsed());
    drop(st_gpu2);
    // CPU
    pools::set_gpu_allowed(false); // `GRAINEO_GPU` is read only once per process
    let doc3 = rawdev::RawDoc::open(&p, &|| true).unwrap().unwrap();
    let _ = doc3.full_base().unwrap();
    let t = Instant::now();
    let st_cpu = doc3.full_stage(&s).unwrap();
    println!("CPU voll_stufe: {:.0?}", t.elapsed());
    pools::set_gpu_allowed(true);
    let (max, mean) = rawdev::gpu::compare(&st_gpu.px, &st_cpu.px);
    println!("linear stage: largest deviation {max:.2e}, mean {mean:.2e}");
    let a = doc.render_stage(&st_gpu, &s);
    let b = doc3.render_stage(&st_cpu, &s);
    let mut de_max = 0f32;
    let mut de_sum = 0f64;
    let mut above1 = 0usize;
    let n = a.width * a.height;
    for i in 0..n {
        let pa = [a.data[3 * i], a.data[3 * i + 1], a.data[3 * i + 2]];
        let pb = [b.data[3 * i], b.data[3 * i + 1], b.data[3 * i + 2]];
        let d = de2000(lab_from_srgb16(pa), lab_from_srgb16(pb));
        de_max = de_max.max(d);
        de_sum += d as f64;
        if d > 1.0 {
            above1 += 1;
        }
    }
    println!("finished image: ΔE00 mean {:.4}, largest {:.3}, pixels above 1: {} of {} ({:.4} %)", de_sum / n as f64, de_max, above1, n, above1 as f64 * 100.0 / n as f64);
    assert!(de_sum / (n as f64) < 0.05, "mean deviation too large");
}
