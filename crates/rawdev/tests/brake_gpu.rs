//! Noise brake on the GPU against the CPU version (reference), at full resolution:
//! `GRAINEO_BENCH_RAW=<file> cargo test -p rawdev --release --test brake_gpu -- --ignored --nocapture`
//!
//! For each slider state: export on the GPU (fused path, fresh document) against the CPU output stage
//! (`GRAINEO_GPU_FUSION=0`, same detail stage). Prints CIEDE2000 (mean, 99.9 %, max), the largest 16-bit difference
//! and the times. The state without brake shows the known GPU/CPU distance of the output stage.

use std::time::Instant;

fn lab(p: &[u16]) -> [f64; 3] {
    let lin = |v: u16| {
        let v = v as f64 / 65535.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
    let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 (Sharma et al. 2005).
fn de00(a: [f64; 3], b: [f64; 3]) -> f64 {
    use std::f64::consts::PI;
    let (l1, a1, b1) = (a[0], a[1], a[2]);
    let (l2, a2, b2) = (b[0], b[1], b[2]);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cm = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hp = |b: f64, a: f64| if b == 0.0 && a == 0.0 { 0.0 } else { let h = b.atan2(a).to_degrees(); if h < 0.0 { h + 360.0 } else { h } };
    let (h1p, h2p) = (hp(b1, a1p), hp(b2, a2p));
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 { 0.0 } else if (h2p - h1p).abs() <= 180.0 { h2p - h1p } else if h2p - h1p > 180.0 { h2p - h1p - 360.0 } else { h2p - h1p + 360.0 };
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lm = (l1 + l2) / 2.0;
    let cmp = (c1p + c2p) / 2.0;
    let hm = if c1p * c2p == 0.0 { h1p + h2p } else if (h1p - h2p).abs() <= 180.0 { (h1p + h2p) / 2.0 } else if h1p + h2p < 360.0 { (h1p + h2p + 360.0) / 2.0 } else { (h1p + h2p - 360.0) / 2.0 };
    let t = 1.0 - 0.17 * ((hm - 30.0) * PI / 180.0).cos() + 0.24 * ((2.0 * hm) * PI / 180.0).cos() + 0.32 * ((3.0 * hm + 6.0) * PI / 180.0).cos() - 0.20 * ((4.0 * hm - 63.0) * PI / 180.0).cos();
    let dtheta = 30.0 * (-((hm - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cmp.powi(7) / (cmp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lm - 50.0).powi(2) / (20.0 + (lm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cmp;
    let sh = 1.0 + 0.015 * cmp * t;
    let rt = -(2.0 * dtheta * PI / 180.0).sin() * rc;
    ((dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhh / sh).powi(2) + rt * (dcp / sc) * (dhh / sh)).sqrt()
}

fn compare(a: &develop::Rgb16, b: &develop::Rgb16) -> (f64, f64, f64, u16) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut d: Vec<f64> = a.data.chunks_exact(3).zip(b.data.chunks_exact(3)).map(|(p, q)| if p == q { 0.0 } else { de00(lab(p), lab(q)) }).collect();
    let max16 = a.data.iter().zip(&b.data).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    d.sort_by(|x, y| x.total_cmp(y));
    (mean, d[(d.len() - 1) * 999 / 1000], d[d.len() - 1], max16)
}

/// Rounds per time measurement (best of), `ROUNDS`, default 3.
fn rounds() -> usize {
    std::env::var("ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(3)
}

#[test]
#[ignore]
fn noise_brake_gpu_matches_cpu() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let path = std::path::Path::new(&p);
    let mut cases: Vec<(&str, develop::Settings)> = Vec::new();
    let base = develop::Settings { exposure: 2.0, ..Default::default() };
    cases.push(("Belichtung +2, Bremse aus", develop::Settings { noise_brake: develop::NoiseBrake::Off, ..base.clone() }));
    cases.push(("Belichtung +2, Bremse Farbe", develop::Settings { noise_brake: develop::NoiseBrake::Color, ..base.clone() }));
    cases.push(("Belichtung +2, Bremse Farbe + Korn", develop::Settings { noise_brake: develop::NoiseBrake::ColorGrain, ..base.clone() }));
    cases.push((
        "Farbe + Korn, Dunst 0,5, Schatten 0,4",
        develop::Settings { noise_brake: develop::NoiseBrake::ColorGrain, exposure: 0.7, shadow: 0.4, dehaze: 0.5, ..Default::default() },
    ));
    for (name, s) in &cases {
        let mut best = f64::MAX;
        let mut gpu = None;
        for _ in 0..rounds() {
            let doc = rawdev::RawDoc::open(path, &|| true).unwrap().unwrap();
            let t = Instant::now();
            let img = doc.render_full(s).unwrap();
            best = best.min(t.elapsed().as_secs_f64());
            gpu = Some(img);
        }
        std::env::set_var("GRAINEO_GPU_FUSION", "0");
        let mut best_cpu = f64::MAX;
        let mut cpu = None;
        for _ in 0..rounds() {
            let doc = rawdev::RawDoc::open(path, &|| true).unwrap().unwrap();
            let t = Instant::now();
            let img = doc.render_full(s).unwrap();
            best_cpu = best_cpu.min(t.elapsed().as_secs_f64());
            cpu = Some(img);
        }
        std::env::remove_var("GRAINEO_GPU_FUSION");
        let (mean, p999, max, max16) = compare(gpu.as_ref().unwrap(), cpu.as_ref().unwrap());
        println!("{name}: GPU {best:.2} s · CPU-Ausgabe {best_cpu:.2} s · ΔE00 Mittel {mean:.5}, 99,9 % {p999:.4}, max {max:.3} · 16 bit max {max16}");
    }
}

/// Times of the zoomed view: crop first (as in the app), then the whole image from the cached stage; per stage
/// with `GRAINEO_GPU_STAGES=1`.
#[test]
#[ignore]
fn noise_brake_view_times() {
    let Ok(p) = std::env::var("GRAINEO_BENCH_RAW") else { return };
    let s = develop::Settings { exposure: 2.0, noise_brake: develop::NoiseBrake::ColorGrain, ..Default::default() };
    for round in 0..rounds() {
        let doc = rawdev::RawDoc::open(std::path::Path::new(&p), &|| true).unwrap().unwrap();
        let t = Instant::now();
        let _ = doc.render_region(&s, [2000, 1333, 2000, 1000]).unwrap();
        let t_crop = t.elapsed();
        rawdev::timings::collect(true);
        let t = Instant::now();
        let _ = doc.render_full(&s).unwrap();
        let t_view = t.elapsed();
        let laps = rawdev::timings::take_all();
        rawdev::timings::collect(false);
        println!("Round {round}: crop {t_crop:.0?}, whole image afterwards {t_view:.0?}");
        if round + 1 == rounds() {
            for (n, ms) in laps {
                println!("  {n}: {ms:.0} ms");
            }
        }
    }
}
