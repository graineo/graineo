//! Editable preview on the GPU against the CPU output stage (reference), per slider:
//! `GRAINEO_BENCH_RAWS=<file>[:<file>…] cargo test -p rawdev --release --test preview_gpu -- --ignored --nocapture`
//!
//! For each file and slider: several slider steps (as when dragging) on the cached preview stage, once with the CPU output
//! stage (`render_stage`, the previous preview path) and once with `render_preview` (GPU, CPU only as fallback). Prints
//! the median time per step for both and CIEDE2000 (mean, 99.9 %, max) of the GPU image against the CPU image. Also
//! checks that slider moves outside noise reduction/lens never recompute the detail stage.

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
    let (l1, a1, b1) = (a[0], a[1], a[2]);
    let (l2, a2, b2) = (b[0], b[1], b[2]);
    let cm = ((a1 * a1 + b1 * b1).sqrt() + (a2 * a2 + b2 * b2).sqrt()) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hp = |b: f64, a: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let (h1p, h2p) = (hp(b1, a1p), hp(b2, a2p));
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p > h1p {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let dh = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
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
    let t = 1.0 - 0.17 * (hpm - 30.0).to_radians().cos() + 0.24 * (2.0 * hpm).to_radians().cos() + 0.32 * (3.0 * hpm + 6.0).to_radians().cos() - 0.20 * (4.0 * hpm - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hpm - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cpm.powi(7) / (cpm.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lpm - 50.0).powi(2) / (20.0 + (lpm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cpm;
    let sh = 1.0 + 0.015 * cpm * t;
    let rt = -(2.0 * dtheta.to_radians()).sin() * rc;
    let (dl, dc) = ((l2 - l1) / sl, (c2p - c1p) / sc);
    let dh = dh / sh;
    (dl * dl + dc * dc + dh * dh + rt * dc * dh).max(0.0).sqrt()
}

/// ΔE00 mean, 99.9 % quantile, max; largest 16-bit difference.
fn compare(a: &develop::Rgb16, b: &develop::Rgb16) -> (f64, f64, f64, u16) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut d: Vec<f64> = a.data.chunks_exact(3).zip(b.data.chunks_exact(3)).map(|(p, q)| de00(lab(p), lab(q))).collect();
    let max16 = a.data.iter().zip(&b.data).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    d.sort_by(|x, y| x.total_cmp(y));
    (mean, d[(d.len() - 1) * 999 / 1000], d[d.len() - 1], max16)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

type Step = fn(&mut develop::Settings, f64);

#[test]
#[ignore]
fn preview_gpu_matches_cpu_and_is_fast() {
    let Ok(files) = std::env::var("GRAINEO_BENCH_RAWS") else { return };
    let gpu = rawdev::gpu::gpu();
    println!("GPU: {}", gpu.map_or("none".to_string(), |g| g.name.clone()));
    let steps: usize = std::env::var("GRAINEO_BENCH_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(7);
    let sliders: Vec<(&str, Step)> = vec![
        ("Belichtung", |s, k| s.exposure = 0.3 + 0.05 * k),
        ("Kontrast", |s, k| s.contrast = 0.1 + 0.04 * k),
        ("Lichter/Tiefen", |s, k| {
            s.highlight = -0.3 - 0.03 * k;
            s.shadow = 0.3 + 0.03 * k;
        }),
        ("Weißabgleich", |s, k| {
            s.raw.wb_mode = "custom".into();
            s.raw.temperature = 4800.0 + 150.0 * k as f32;
            s.raw.tint = 5.0;
        }),
        ("Dunst", |s, k| s.dehaze = 0.2 + 0.05 * k as f32),
        ("Belichtung + Rauschbremse", |s, k| {
            s.noise_brake = develop::NoiseBrake::ColorGrain;
            s.exposure = 1.0 + 0.05 * k;
        }),
        ("Dynamik + Sättigung", |s, k| {
            s.vibrance = 0.1 + 0.03 * k as f32;
            s.saturation = 0.1;
        }),
    ];
    for f in files.split(':').filter(|f| !f.is_empty()) {
        let doc = rawdev::RawDoc::open(std::path::Path::new(f), &|| true).unwrap().unwrap();
        let base = develop::Settings::default();
        let t = Instant::now();
        let st = doc.preview_stage(&base);
        let _ = st.mask(doc.lum());
        println!("\n{} {} ({}): preview {}×{} (full {}×{}), detail level {:.0?}", doc.info.maker, doc.info.model, f.rsplit('/').next().unwrap_or(f), st.w, st.h, doc.full_size[0], doc.full_size[1], t.elapsed());
        // warm up (shader pipelines, tables, dehaze analysis)
        let _ = doc.render_preview(&base);
        for (name, step) in &sliders {
            let (mut tc, mut tg) = (Vec::new(), Vec::new());
            let mut worst = (0f64, 0f64, 0f64, 0u16);
            let mut gpu_used = true;
            let mut first = base.clone();
            step(&mut first, 0.0);
            let _ = doc.render_preview(&first);
            let _ = doc.render_stage(&st, &first);
            for k in 0..steps {
                let mut s = base.clone();
                step(&mut s, k as f64 + 1.0);
                let t = Instant::now();
                let c = doc.render_stage(&st, &s);
                tc.push(t.elapsed().as_secs_f64() * 1000.0);
                rawdev::timings::collect(k == steps - 1 && std::env::var_os("GRAINEO_BENCH_DETAIL").is_some());
                let t = Instant::now();
                let g = doc.render_preview(&s);
                tg.push(t.elapsed().as_secs_f64() * 1000.0);
                for (n, ms) in rawdev::timings::take_all() {
                    println!("      {n}: {ms:.1} ms");
                }
                rawdev::timings::collect(false);
                gpu_used &= doc.render_stage_gpu(&st, &s).is_some();
                assert!(doc.preview_ready(&s), "{name}: detail stage was recomputed");
                let r = compare(&g, &c);
                if r.0 > worst.0 {
                    worst.0 = r.0;
                }
                worst.1 = worst.1.max(r.1);
                worst.2 = worst.2.max(r.2);
                worst.3 = worst.3.max(r.3);
            }
            println!(
                "  {name:<28} CPU {:>6.1} ms · preview {:>6.1} ms ({}) · ΔE00 mean {:.5}, 99.9 % {:.4}, max {:.3} · max 16 bit {}",
                median(tc),
                median(tg),
                if gpu_used { "GPU" } else { "CPU" },
                worst.0,
                worst.1,
                worst.2,
                worst.3
            );
            if gpu_used {
                assert!(worst.0 < 0.05, "{name}: GPU preview differs from CPU (mean ΔE00 {})", worst.0);
            }
        }
        // a short look at another noise reduction (hovering a preset) keeps the stage the sliders work on
        let mut nr = base.clone();
        nr.raw.nr_luma = (base.raw.nr_luma + 20.0).min(100.0);
        let t = Instant::now();
        let _ = doc.render_preview(&nr);
        let t_nr = t.elapsed();
        let t = Instant::now();
        let _ = doc.render_preview(&base);
        println!("  different noise reduction (new detail level) {t_nr:.0?}, back {:.0?}", t.elapsed());
        assert!(doc.preview_ready(&base) && doc.preview_ready(&nr), "detail stage of the slider state was dropped");
        // CPU fallback: `GRAINEO_GPU_FUSION=0` renders the preview on the CPU, bit for bit as `render_stage`
        unsafe { std::env::set_var("GRAINEO_GPU_FUSION", "0") };
        assert!(doc.render_stage_gpu(&st, &base).is_none());
        assert_eq!(doc.render_preview(&base).data, doc.render_stage(&st, &base).data);
        unsafe { std::env::remove_var("GRAINEO_GPU_FUSION") };
    }
}
