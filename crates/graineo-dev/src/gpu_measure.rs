//! `graineo-dev gpu-measure <X3F> [--only cpu|gpu|both] [--runs n]`: Foveon development (working image) with the
//! CPU path and the GPU path: time (median), deviation, memory. One CSV line on stdout.
//!
//! Columns: datei;modell;iso;weg;lief_gpu;median_s;laeufe_s;max8;de_mean;de_p99;de_max;psnr_counts_db;peak_rss_mb;n_de_gt03;shift_r;shift_g;shift_b;kachel_spalte;kachel_zeile;de8_mean;de8_max;n8_gt03
//! (shift = mean difference GPU-CPU per channel in 8-bit steps; kachel = largest column/row mean of the deviation / median, tile edges)
//! `--diff <png>`: GPU-CPU difference image, amplified ×20, around mid grey (128)
//! With `--only cpu` or `--only gpu` the process measures only one path (then the peak memory is meaningful).
//! Default settings (sliders at the middle), display via `render_counts` (8-bit sRGB).
//! The CPU path turns the GPU off like the setting "Grafikkarte verwenden" (`pools::set_gpu_allowed`).

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::{Context, Result};
use develop::Settings;
use rayon::prelude::*;

fn peak_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    // macOS: Bytes, Linux: KiB
    #[cfg(target_os = "macos")]
    return u.ru_maxrss as f64 / 1048576.0;
    #[cfg(not(target_os = "macos"))]
    return u.ru_maxrss as f64 / 1024.0;
}

fn run_way(path: &Path, key: &develop::SppKey, gpu: bool, runs: usize, quattro: bool, warm: bool) -> Result<(Vec<f64>, bool, develop::Linear)> {
    let cancel = AtomicBool::new(false);
    let mut times = Vec::new();
    let mut used = false;
    let mut last = None;
    // same switch as the setting "Grafikkarte verwenden": off = the noise reduction of the
    // Quattro preparation also runs on the CPU (`GRAINEO_GPU=0` keeps both paths on the CPU)
    pools::set_gpu_allowed(gpu);
    let n = runs + usize::from(warm);
    for i in 0..n {
        if quattro {
            develop::quattro::forget(path);
        }
        let t = Instant::now();
        let c = develop::spp::working_key(path, key, gpu, &cancel).map_err(anyhow::Error::msg)?;
        let s = t.elapsed().as_secs_f64();
        used = develop::spp::last_used_gpu();
        if !(warm && i == 0) {
            times.push(s);
        }
        last = Some(c);
    }
    Ok((times, used, last.unwrap()))
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

pub fn run(args: &[String]) -> Result<()> {
    let file = args.first().context("file missing")?;
    let path = Path::new(file);
    let mut only = "both";
    let mut runs = 3usize;
    let mut diff_png: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--only" => { only = args.get(i + 1).map(String::as_str).unwrap_or("both"); i += 1; }
            "--diff" => { diff_png = args.get(i + 1).cloned(); i += 1; }
            "--runs" => { runs = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(3); i += 1; }
            _ => {}
        }
        i += 1;
    }
    let session = develop::Session::open(path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let settings = Settings::default();
    let key = settings.spp_key(path);
    let quattro = develop::quattro::is_prepared(path);
    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let model = session.cal.camera.replace(';', " ");
    let iso = session.cal.iso;
    let (mut cpu, mut gpu) = (None, None);
    if only != "gpu" {
        cpu = Some(run_way(path, &key, false, runs, quattro, false)?);
    }
    let rss_cpu = peak_rss_mb();
    if only != "cpu" {
        gpu = Some(run_way(path, &key, true, runs, quattro, true)?);
    }
    let rss = peak_rss_mb();
    let fmt = |v: &[f64]| v.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join("/");
    let (mut de8_mean, mut de8_max, mut n8_03) = (f64::NAN, f64::NAN, 0usize);
    let (mut n03, mut shift, mut tcol, mut trow) = (0usize, [f64::NAN; 3], f64::NAN, f64::NAN);
    let (mut max8, mut de_mean, mut de_p99, mut de_max, mut psnr) = (f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN);
    if let (Some((_, _, c)), Some((_, gused, g))) = (&cpu, &gpu) {
        let m = session.cal.mode(key.mode.as_deref()).cam_to_srgb;
        let rc = develop::render_counts(c, &session.cal, &settings, &m, None);
        let rg = develop::render_counts(g, &session.cal, &settings, &m, None);
        let (a8, b8) = (rc.to_rgb8(), rg.to_rgb8());
        max8 = a8.iter().zip(&b8).map(|(&x, &y)| (x as i32 - y as i32).abs()).max().unwrap_or(0) as f64;
        let de = delta_map(&rc.data, &rg.data);
        let n = de.len();
        let mut sorted = de.clone();
        sorted.par_sort_unstable_by(|x, y| x.total_cmp(y));
        de_mean = de.par_iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        de_p99 = sorted[(n as f64 * 0.99) as usize] as f64;
        de_max = sorted[n - 1] as f64;
        let peak = c.data.iter().cloned().fold(0f32, f32::max) as f64;
        let mse = c.data.par_iter().zip(g.data.par_iter()).map(|(&x, &y)| ((x - y) as f64).powi(2)).sum::<f64>() / c.data.len() as f64;
        psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (peak * peak / mse).log10() };
        // ΔE00 on the 8-bit images (what reaches the screen)
        let up = |v: &[u8]| v.iter().map(|&x| x as u16 * 257).collect::<Vec<u16>>();
        let de8 = delta_map(&up(&a8), &up(&b8));
        let n8 = de8.len();
        de8_mean = de8.par_iter().map(|&v| v as f64).sum::<f64>() / n8 as f64;
        de8_max = de8.par_iter().cloned().fold(|| 0f32, f32::max).reduce(|| 0f32, f32::max) as f64;
        n8_03 = de8.par_iter().filter(|&&v| v > 0.3).count();
        n03 = de.par_iter().filter(|&&v| v > 0.3).count();
        let npx = (a8.len() / 3) as f64;
        shift = std::array::from_fn(|ch| a8.chunks_exact(3).zip(b8.chunks_exact(3)).map(|(x, y)| y[ch] as f64 - x[ch] as f64).sum::<f64>() / npx);
        // Tile edges: mean deviation (counts) per column and per row of the sensor
        let (w, h) = (c.width, c.height);
        let mut col = vec![0f64; w];
        let mut row = vec![0f64; h];
        for (i, (x, y)) in c.data.iter().zip(g.data.iter()).enumerate() {
            let d = (*x as f64 - *y as f64).abs();
            col[(i / 3) % w] += d;
            row[i / 3 / w] += d;
        }
        let ratio = |v: &[f64]| {
            let mut s = v.to_vec();
            s.sort_by(|a, b| a.total_cmp(b));
            let med = s[s.len() / 2].max(1e-9);
            s[s.len() - 1] / med
        };
        tcol = ratio(&col);
        trow = ratio(&row);
        if let Some(png) = &diff_png {
            let (ow, oh) = (rc.width as u32, rc.height as u32);
            let mut img = image::RgbImage::new(ow, oh);
            for (i, p) in img.pixels_mut().enumerate() {
                for ch in 0..3 {
                    p[ch] = (128.0 + (b8[i * 3 + ch] as f64 - a8[i * 3 + ch] as f64) * 20.0).clamp(0.0, 255.0) as u8;
                }
            }
            img.save(png)?;
        }
        let _ = gused;
    }
    let (cm, cl) = cpu.as_ref().map_or((f64::NAN, String::new()), |(t, _, _)| (median(t.clone()), fmt(t)));
    let (gm, gl, gu) = gpu.as_ref().map_or((f64::NAN, String::new(), false), |(t, u, _)| (median(t.clone()), fmt(t), *u));
    println!("{name};{model};{iso};cpu;0;{cm:.2};{cl};;;;;;{rss_cpu:.0}");
    if only != "cpu" {
        let (g0, g1, g2, gb) = (shift[0], shift[1], shift[2], u8::from(gu));
        println!("{name};{model};{iso};gpu;{gb};{gm:.2};{gl};{max8};{de_mean:.4};{de_p99:.4};{de_max:.4};{psnr:.1};{rss:.0};{n03};{g0:.3};{g1:.3};{g2:.3};{tcol:.1};{trow:.1};{de8_mean:.4};{de8_max:.3};{n8_03}");
    }
    Ok(())
}

fn srgb_to_lab(v: [u16; 3]) -> [f64; 3] {
    let lin = |c: u16| {
        let c = c as f64 / 65535.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(v[0]), lin(v[1]), lin(v[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// ΔE00 per pixel (f32) between two 16-bit sRGB images.
fn delta_map(a: &[u16], b: &[u16]) -> Vec<f32> {
    a.par_chunks_exact(3)
        .zip(b.par_chunks_exact(3))
        .map(|(p, q)| if p == q { 0.0 } else { crate::color_chart::de00(srgb_to_lab([p[0], p[1], p[2]]), srgb_to_lab([q[0], q[1], q[2]])) as f32 })
        .collect()
}
