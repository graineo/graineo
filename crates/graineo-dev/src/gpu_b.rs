//! `graineo-dev gpub …`: measurements for moving Sigma stages to the GPU (GPU against the processor path).
//!
//! * `gpub spp <X3F> [--nr=l,c] [--ca=profile[:amount]|manual:r,b] [--runs=n] [--diff=png]`: Sigma base
//!   (`working_key`) on the processor and on the GPU, both rendered with default settings to 16-bit sRGB;
//!   times, whether the GPU ran, ΔE00 statistics and the tile-edge check. Slider values 0…1, −0.25 = "Off".
//! * `gpub tif <A.tif> <B.tif> [--diff=png]`: ΔE00 statistics of two 16-bit sRGB TIFFs.
//! * `gpub pixhash <TIFF …>`: hash of the decoded pixel data only (ignores tags such as the software
//!   version), one line per file.
//! * `gpub region <file> [--mask] [--runs=n]`: crop with/without a radial mask (time) and ΔE00 of the crop
//!   against the same part of the whole image.
//!
//! ΔE00 statistics: mean, p99, maximum, pixels above 0.3, largest 8-bit step, and the ratio of the largest
//! column/row mean of ΔE00 to the median of all column/row means (tile borders show up as single
//! columns/rows far above the median).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

fn lin(v: u16) -> f64 {
    let v = f64::from(v) / 65535.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn lab(p: &[u16]) -> [f32; 3] {
    let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    [(116.0 * fy - 16.0) as f32, (500.0 * (fx - fy)) as f32, (200.0 * (fy - fz)) as f32]
}

/// ΔE00 statistics of two images (interleaved RGB, 16 bit, sRGB).
pub struct Stats {
    pub mean: f64,
    pub p99: f64,
    pub max: f64,
    pub over03: usize,
    pub step8: u32,
    pub col_ratio: f64,
    pub row_ratio: f64,
    pub shift: [f64; 3],
}

fn ratio(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let med = s[s.len() / 2];
    let max = *s.last().unwrap();
    if med > 0.0 {
        max / med
    } else if max > 0.0 {
        f64::INFINITY
    } else {
        0.0
    }
}

pub fn stats(a: &[u16], b: &[u16], w: usize, h: usize, diff: Option<&str>) -> Result<Stats> {
    anyhow::ensure!(a.len() == b.len() && a.len() == w * h * 3, "sizes differ");
    let de: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (p, q) = (&a[i * 3..i * 3 + 3], &b[i * 3..i * 3 + 3]);
            if p == q {
                0.0
            } else {
                crate::raw_compare::de2000(lab(p), lab(q))
            }
        })
        .collect();
    let n = de.len() as f64;
    let mean = de.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    let mut sorted = de.clone();
    sorted.par_sort_by(f32::total_cmp);
    let p99 = f64::from(sorted[((n * 0.99) as usize).min(sorted.len() - 1)]);
    let max = f64::from(*sorted.last().unwrap());
    let over03 = de.iter().filter(|&&v| v > 0.3).count();
    let step8 = a.iter().zip(b).map(|(&x, &y)| ((i32::from(x) - i32::from(y)).unsigned_abs() + 128) / 257).max().unwrap_or(0);
    let mut cols = vec![0f64; w];
    let mut rows = vec![0f64; h];
    for (i, &v) in de.iter().enumerate() {
        cols[i % w] += f64::from(v);
        rows[i / w] += f64::from(v);
    }
    let mut shift = [0f64; 3];
    for (i, (&x, &y)) in a.iter().zip(b).enumerate() {
        shift[i % 3] += (f64::from(y) - f64::from(x)) / 257.0;
    }
    let shift = shift.map(|s| s / (w * h) as f64);
    if let Some(path) = diff {
        // difference B − A per channel, ×20, around grey 128
        let mut img = image::RgbImage::new(w as u32, h as u32);
        for (i, px) in img.pixels_mut().enumerate() {
            for c in 0..3 {
                let d = (f64::from(b[i * 3 + c]) - f64::from(a[i * 3 + c])) / 257.0 * 20.0;
                px.0[c] = (128.0 + d).round().clamp(0.0, 255.0) as u8;
            }
        }
        img.save(path)?;
    }
    Ok(Stats { mean, p99, max, over03, step8, col_ratio: ratio(&cols), row_ratio: ratio(&rows), shift })
}

pub fn print_stats(label: &str, s: &Stats) {
    println!(
        "{label}: ΔE00 mean {:.4} · p99 {:.3} · max {:.3} · >0.3: {} px · max step 8 bit {} · column/row max÷median {:.1}/{:.1} · shift 8 bit {:+.3}/{:+.3}/{:+.3}",
        s.mean, s.p99, s.max, s.over03, s.step8, s.col_ratio, s.row_ratio, s.shift[0], s.shift[1], s.shift[2]
    );
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().find_map(|a| a.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
}

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("spp") => spp(args.get(1).context("X3F missing")?, &args[2..]),
        Some("tif") => {
            let (wa, ha, a) = crate::read_tiff16(args.get(1).context("A missing")?)?;
            let (wb, hb, b) = crate::read_tiff16(args.get(2).context("B missing")?)?;
            anyhow::ensure!((wa, ha) == (wb, hb), "sizes differ: {wa}×{ha} vs {wb}×{hb}");
            let s = stats(&a, &b, wa as usize, ha as usize, flag(&args[3..], "--diff"))?;
            print_stats("TIFF", &s);
            Ok(())
        }
        Some("pixhash") => {
            for f in &args[1..] {
                let (w, h, d) = crate::read_tiff16(f)?;
                let mut x: u64 = 0xcbf2_9ce4_8422_2325;
                for v in &d {
                    for byte in v.to_le_bytes() {
                        x = (x ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
                    }
                }
                println!("{} {w}x{h} {x:016x}", Path::new(f).file_stem().unwrap_or_default().to_string_lossy());
            }
            Ok(())
        }
        Some("region") => region(args.get(1).context("file missing")?, &args[2..]),
        _ => bail!("usage: graineo-dev gpub spp|tif|pixhash|region …"),
    }
}

fn spp(file: &str, args: &[String]) -> Result<()> {
    let path = Path::new(file);
    let session = develop::Session::open(file).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut s = develop::Settings::default();
    if let Some(v) = flag(args, "--nr") {
        let (l, c) = v.split_once(',').context("--nr=l,c")?;
        (s.nr_luma, s.nr_chroma) = (l.parse()?, c.parse()?);
    }
    if let Some(v) = flag(args, "--ca") {
        match v.split_once(':') {
            None if v == "profile" => s.ca_mode = develop::lens::CaMode::Profile,
            Some(("profile", a)) => (s.ca_mode, s.ca_profile) = (develop::lens::CaMode::Profile, a.parse()?),
            Some(("manual", rb)) => {
                let (r, b) = rb.split_once(',').context("--ca=manual:r,b")?;
                (s.ca_mode, s.ca_red, s.ca_blue) = (develop::lens::CaMode::Manual, r.parse()?, b.parse()?);
            }
            _ => bail!("--ca=profile[:amount]|manual:r,b"),
        }
    }
    let runs: usize = flag(args, "--runs").map_or(Ok(3), str::parse)?;
    let key = s.spp_key(path);
    eprintln!("key: nr {:?} ca {:?}", key.nr, key.ca);
    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let cpu = develop::spp::working_key(path, &key, false, &cancel).map_err(anyhow::Error::msg)?;
    let t_cpu = t.elapsed().as_secs_f64();
    let mut times = Vec::new();
    let mut gpu = None;
    // one warm-up run (device, shaders) that is not counted
    for i in 0..=runs {
        let t = Instant::now();
        let g = develop::spp::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?;
        if i > 0 {
            times.push(t.elapsed().as_secs_f64());
        }
        gpu = Some(g);
    }
    let used = develop::spp::last_used_gpu();
    let gpu = gpu.unwrap();
    times.sort_by(f64::total_cmp);
    println!(
        "{}: CPU {t_cpu:.2} s · GPU min {:.2} s median {:.2} s (runs {times:.2?}) · GPU used: {used}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        times[0],
        times[times.len() / 2]
    );
    let m = session.cal.mode(key.mode.as_deref()).cam_to_srgb;
    let a = develop::render_counts(&cpu, &session.cal, &s, &m, None);
    let b = develop::render_counts(&gpu, &session.cal, &s, &m, None);
    let st = stats(&a.data, &b.data, a.width, a.height, flag(args, "--diff"))?;
    print_stats("GPU against CPU", &st);
    Ok(())
}

fn radial_mask() -> Result<masks::Document> {
    masks::Document::from_json(
        r#"{"version":2,"layers":[{"mask":{"type":"radial","center":[0.5,0.5],"radii":[0.3,0.25],"angle":0.4,"feather":0.5},"adjustments":{"exposure":0.5,"clarity":0.4,"saturation":0.2}},{"mask":{"type":"brush","points":[[0.3,0.3],[0.5,0.45],[0.7,0.5]],"radius":0.05,"hardness":0.5,"flow":0.6},"adjustments":{"exposure":-0.3,"texture":0.5,"noise_reduction":0.3}}]}"#,
    )
    .map_err(anyhow::Error::msg)
}

fn region(file: &str, args: &[String]) -> Result<()> {
    let runs: usize = flag(args, "--runs").map_or(Ok(5), str::parse)?;
    let mut s = develop::Settings::default();
    if args.iter().any(|a| a == "--mask") {
        s.local = Some(radial_mask()?);
    }
    let sigma = file.to_ascii_lowercase().ends_with(".x3f");
    let mut sess = None;
    let mut doc = None;
    let [fw, fh] = if sigma {
        let se = develop::Session::open(file).map_err(|e| anyhow::anyhow!("{e}"))?;
        se.ensure_spp(&s).map_err(anyhow::Error::msg)?;
        let size = se.size();
        sess = Some(se);
        size
    } else {
        let d = rawdev::RawDoc::open(Path::new(file), &|| true).map_err(anyhow::Error::msg)?.context("cancelled")?;
        let size = d.full_size;
        doc = Some(d);
        size
    };
    let mut render = |s: &develop::Settings, r: Option<[usize; 4]>| -> Result<develop::Rgb16> {
        Ok(match (&mut sess, &mut doc, r) {
            (Some(se), _, Some(r)) => se.render_region(s, r),
            (Some(se), _, None) => se.render_full(s),
            (_, Some(d), Some(r)) => d.render_region(s, r).map_err(anyhow::Error::msg)?,
            (_, Some(d), None) => d.render_full(s).map_err(anyhow::Error::msg)?,
            _ => unreachable!(),
        })
    };
    let full = render(&s, None)?;
    for (side, cx, cy) in [(800usize, 0.5, 0.5), (1600, 0.5, 0.5), (600, 0.02, 0.98)] {
        let (w, h) = (side.min(fw), side.min(fh));
        let x = (((fw as f64) * cx) as usize).saturating_sub(w / 2).min(fw - w);
        let y = (((fh as f64) * cy) as usize).saturating_sub(h / 2).min(fh - h);
        let r = [x, y, w, h];
        let _ = render(&s, Some(r))?;
        let mut times = Vec::new();
        let mut last = None;
        for _ in 0..runs {
            let t = Instant::now();
            last = Some(render(&s, Some(r))?);
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        let crop = develop::local::crop(&full, r);
        let img = last.unwrap();
        let st = stats(&crop.data, &img.data, img.width, img.height, None)?;
        println!("crop {r:?}: median {:.1} ms (min {:.1}) · against the whole image:", times[times.len() / 2], times[0]);
        print_stats("  crop", &st);
    }
    Ok(())
}
