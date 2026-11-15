//! `graineo-dev gpu-compare <mode> <out_dir> FILES… [options]`: GPU path against the processor path (reference) with
//! the switch "Grafikkarte verwenden" (`pools::set_gpu_allowed`): time per variant (median of the runs) and deviation
//! ΔE00 of the finished 16-bit images (mean, 99.9 % quantile, maximum, pixels above 0.5). For a visual check a map of
//! the deviation is written (`<name>-<mode>-de.png`, white = ΔE00 ≥ 0.5); the edge ratio compares the mean deviation on
//! the edges of the 16×16 workgroups and of the 720-row strips with the rest (≈ 1 = no tile edges).
//!
//! Modes:
//!   voll      non-Sigma RAW, full resolution as exported (`RawDoc::render_full`)
//!   bereich   non-Sigma RAW, 100 % crop of 2400×1600 in the middle (`render_region`, as the zoomed view)
//!   fc        false color suppression alone (X-Trans) on the finished processor image
//!   skal      export scaling (`--edge 2048`): image crate (before) against the new processor and GPU path;
//!             X3F via the Sigma development (`Session::render_full`)
//!   film      non-Sigma RAW with film (`--film kodak_portra_400`), full resolution
//!
//! Options: `--runs N` (default 3), `--edge N`, `--film ID`, `--false-color X` (0 … 1), `--no-lens`, `--no-nr`,
//! `--no-grain`; `GRAINEO_GPU_COMPARE_PEAKS=1` lists the largest deviations with their position.
//!   demosaik  X-Trans demosaic alone (CPU Markesteijn against the GPU kernels) on the real mosaic

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use develop::{Rgb16, Settings};
use rayon::prelude::*;

pub fn run(args: &[String]) -> Result<()> {
    let (mode, out_dir) = match (args.first(), args.get(1)) {
        (Some(m), Some(o)) => (m.as_str(), Path::new(o)),
        _ => bail!("usage: graineo-dev gpu-compare full|region|fc|skal|film <out_dir> FILES… [--runs N] [--edge N] [--film ID]"),
    };
    std::fs::create_dir_all(out_dir)?;
    let mut files = Vec::new();
    let mut runs = 3usize;
    let mut edge = 2048usize;
    let mut film = "kodak_portra_400".to_string();
    let mut fc: Option<f32> = None;
    let (mut no_lens, mut no_nr, mut no_grain) = (false, false, false);
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--runs" => {
                runs = args.get(i + 1).context("--runs N")?.parse()?;
                i += 1;
            }
            "--edge" => {
                edge = args.get(i + 1).context("--edge N")?.parse()?;
                i += 1;
            }
            "--film" => {
                film = args.get(i + 1).context("--film ID")?.clone();
                i += 1;
            }
            "--false-color" => {
                fc = Some(args.get(i + 1).context("--false-color X")?.parse()?);
                i += 1;
            }
            "--no-lens" => no_lens = true,
            "--no-nr" => no_nr = true,
            "--no-grain" => no_grain = true,
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    println!("load {} · GPU {}", load(), rawdev::gpu::gpu().map(|g| g.name.clone()).unwrap_or_else(|| "none".into()));
    println!("| File | Mode | CPU ms | GPU ms | ΔE00 mean | ΔE00 99.9 % | ΔE00 max | px > 0.5 | edge ratio |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for f in &files {
        let name = Path::new(f).file_stem().unwrap_or_default().to_string_lossy().to_string();
        let mut s = Settings::default();
        if let Some(v) = fc {
            s.raw.false_color = v;
        }
        if no_lens {
            s.raw.lens_image = Some(false);
            s.raw.lens = false;
        }
        if no_nr {
            s.raw.nr_luma = 0.0;
            s.raw.nr_color = 0.0;
        }
        match mode {
            "full" | "region" | "film" => {
                if mode == "film" {
                    s.film.active = true;
                    s.film.stock = film.clone();
                    if no_grain {
                        s.film.grain = 0.0;
                    }
                }
                let region = mode == "region";
                let render = |gpu: bool| -> Result<(Rgb16, f64)> {
                    pools::set_gpu_allowed(gpu);
                    let mut best = Vec::new();
                    let mut img = None;
                    for _ in 0..runs {
                        // reopened each time: nothing cached from the previous variant
                        let doc = rawdev::RawDoc::open(Path::new(f), &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
                        let t = Instant::now();
                        let i = if region {
                            let [w, h] = doc.full_size;
                            let (rw, rh) = (2400.min(w), 1600.min(h));
                            // full size is oriented; the crop is in sensor coordinates, the middle is the same
                            doc.render_region(&s, [(w - rw) / 2, (h - rh) / 2, rw, rh]).map_err(|e| anyhow!(e))?
                        } else {
                            doc.render_full(&s).map_err(|e| anyhow!(e))?
                        };
                        best.push(t.elapsed().as_secs_f64() * 1000.0);
                        img = Some(i);
                    }
                    Ok((img.unwrap(), median(&mut best)))
                };
                let (cpu, tc) = render(false)?;
                let (gpu, tg) = render(true)?;
                report(&name, mode, tc, tg, &cpu, &gpu, out_dir)?;
            }
            "fc" => {
                pools::set_gpu_allowed(false);
                let doc = rawdev::RawDoc::open(Path::new(f), &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
                let mut s0 = s.clone();
                s0.raw.false_color = 0.0;
                let base = doc.render_full(&s0).map_err(|e| anyhow!(e))?;
                let strength = fc.unwrap_or(0.6);
                let time = |gpu: bool| -> (Rgb16, f64) {
                    pools::set_gpu_allowed(gpu);
                    let mut t = Vec::new();
                    let mut out = None;
                    for _ in 0..runs {
                        let mut img = Rgb16 { width: base.width, height: base.height, data: base.data.clone() };
                        let t0 = Instant::now();
                        if gpu {
                            rawdev::false_color::suppress(&mut img, strength);
                        } else {
                            rawdev::false_color::suppress_cpu(&mut img, strength);
                        }
                        t.push(t0.elapsed().as_secs_f64() * 1000.0);
                        out = Some(img);
                    }
                    (out.unwrap(), median(&mut t))
                };
                let (cpu, tc) = time(false);
                let (gpu, tg) = time(true);
                let changed = cpu.data.chunks(3).zip(base.data.chunks(3)).filter(|(a, b)| a != b).count();
                println!("  ({name}: {changed} pixels changed by the processor path, {} by the GPU path)", gpu.data.chunks(3).zip(base.data.chunks(3)).filter(|(a, b)| a != b).count());
                report(&name, mode, tc, tg, &cpu, &gpu, out_dir)?;
            }
            "skal" => {
                pools::set_gpu_allowed(true);
                let img = if f.to_ascii_lowercase().ends_with(".x3f") {
                    let mut session = develop::Session::open(f)?;
                    let img = session.render_full(&s);
                    develop::geometry::apply(img, &s, session.cal.orientation)
                } else {
                    let doc = rawdev::RawDoc::open(Path::new(f), &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
                    doc.render_full(&s).map_err(|e| anyhow!(e))?
                };
                let size = Some(develop::export::OutputSize::LongEdge(edge as u32));
                let time = |k: u8| -> (Rgb16, f64) {
                    let mut t = Vec::new();
                    let mut out = None;
                    for _ in 0..runs {
                        let i = Rgb16 { width: img.width, height: img.height, data: img.data.clone() };
                        let t0 = Instant::now();
                        let o = match k {
                            0 => develop::resize::lanczos3_image_crate(i, size),
                            1 => {
                                pools::set_gpu_allowed(false);
                                develop::export::apply_output_size(i, size)
                            }
                            _ => {
                                pools::set_gpu_allowed(true);
                                unsafe { std::env::set_var("GRAINEO_RESIZE_GPU", "1") };
                                let o = develop::export::apply_output_size(i, size);
                                unsafe { std::env::remove_var("GRAINEO_RESIZE_GPU") };
                                o
                            }
                        };
                        t.push(t0.elapsed().as_secs_f64() * 1000.0);
                        out = Some(o);
                    }
                    (out.unwrap(), median(&mut t))
                };
                let (old, t_old) = time(0);
                let (cpu, tc) = time(1);
                let (gpu, tg) = time(2);
                let same = old.data == cpu.data;
                let maxd = old.data.iter().zip(&cpu.data).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
                println!("  ({name} {}×{} → {}×{}: image crate {t_old:.0} ms; new processor path bit-identical to it: {same} (largest difference {maxd}))", img.width, img.height, cpu.width, cpu.height);
                report(&name, mode, tc, tg, &cpu, &gpu, out_dir)?;
            }
            "demosaic" => {
                // X-Trans demosaic alone on the real mosaic: CPU (Markesteijn 3 passes) against the GPU kernels, linear values
                // encoded with a plain sRGB curve (×4 so that the mid tones are compared, not only the shadows)
                let f = raw::RawFile::open(f).map_err(|e| anyhow!("{e}"))?;
                let b = f.base_mosaic(false).map_err(|e| anyhow!("{e}"))?;
                let m = b.mosaic.context("no mosaic")?;
                let t0 = Instant::now();
                let cpu = raw::development::demosaic_mosaic(&m.norm, m.w, m.h, m.pat, raw::development::Demosaic::Markesteijn3, m.sensor);
                let tc = t0.elapsed().as_secs_f64() * 1000.0;
                let t0 = Instant::now();
                let gpu = rawdev::gpu::xtrans::markesteijn(&m.norm, m.w, m.h, m.pat).context("GPU")?;
                let tg = t0.elapsed().as_secs_f64() * 1000.0;
                let enc = |px: &[[f32; 3]]| -> Rgb16 {
                    let data = px.par_iter().flat_map_iter(|p| p.map(|v| ((4.0 * v).clamp(0.0, 1.0).powf(1.0 / 2.2) * 65535.0 + 0.5) as u16)).collect();
                    Rgb16 { width: m.w, height: m.h, data }
                };
                report(&name, mode, tc, tg, &enc(&cpu), &enc(&gpu), out_dir)?;
            }
            m => bail!("unknown mode {m}"),
        }
    }
    pools::set_gpu_allowed(true);
    println!("load {}", load());
    Ok(())
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn load() -> String {
    let mut a = [0f64; 3];
    let n = unsafe { libc::getloadavg(a.as_mut_ptr(), 3) };
    if n >= 1 {
        format!("{:.1}/{:.1}/{:.1}", a[0], a[1], a[2])
    } else {
        "?".into()
    }
}

fn lab16(p: &[u16]) -> [f32; 3] {
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

/// ΔE00 statistics, edge ratio and deviation map of two equally sized images.
fn report(name: &str, mode: &str, tc: f64, tg: f64, cpu: &Rgb16, gpu: &Rgb16, out_dir: &Path) -> Result<()> {
    if (cpu.width, cpu.height) != (gpu.width, gpu.height) {
        bail!("{name}: sizes differ ({}×{} / {}×{})", cpu.width, cpu.height, gpu.width, gpu.height);
    }
    let (w, h) = (cpu.width, cpu.height);
    let de: Vec<f32> = cpu
        .data
        .par_chunks(3)
        .zip(gpu.data.par_chunks(3))
        .map(|(a, b)| if a == b { 0.0 } else { crate::raw_compare::de2000(lab16(a), lab16(b)) })
        .collect();
    let mean = de.iter().map(|v| *v as f64).sum::<f64>() / de.len().max(1) as f64;
    let max = de.iter().copied().fold(0f32, f32::max);
    let above = de.iter().filter(|v| **v > 0.5).count();
    let mut sorted = de.clone();
    sorted.par_sort_unstable_by(|a, b| a.total_cmp(b));
    let q999 = sorted[((sorted.len() - 1) as f64 * 0.999) as usize];
    // edge ratio: mean ΔE on the edges of the 16×16 workgroups and the 720-row strips against the rest
    let (mut se, mut ne, mut sr, mut nr) = (0f64, 0usize, 0f64, 0usize);
    for y in 0..h {
        for x in 0..w {
            let v = de[y * w + x] as f64;
            if matches!(x % 16, 0 | 15) || matches!(y % 16, 0 | 15) || matches!(y % 720, 0 | 719) {
                se += v;
                ne += 1;
            } else {
                sr += v;
                nr += 1;
            }
        }
    }
    let ratio = if sr > 0.0 { (se / ne.max(1) as f64) / (sr / nr.max(1) as f64) } else { 1.0 };
    let map: Vec<u8> = de.iter().map(|v| ((v / 0.5).min(1.0) * 255.0) as u8).collect();
    let img = image::GrayImage::from_raw(w as u32, h as u32, map).context("map")?;
    img.save(out_dir.join(format!("{name}-{mode}-de.png")))?;
    if std::env::var_os("GRAINEO_GPU_COMPARE_PEAKS").is_some() {
        // the largest deviations with position and values (finding the cause)
        let mut idx: Vec<usize> = (0..de.len()).filter(|&i| de[i] > 0.5).collect();
        idx.sort_by(|a, b| de[*b].total_cmp(&de[*a]));
        for &i in idx.iter().take(12) {
            println!("  ΔE {:.2} at ({}, {}): CPU {:?} GPU {:?}", de[i], i % w, i / w, &cpu.data[i * 3..i * 3 + 3], &gpu.data[i * 3..i * 3 + 3]);
        }
        let (mut bx, mut by) = (vec![0usize; 16], vec![0usize; 16]);
        for &i in &idx {
            bx[(i % w) * 16 / w] += 1;
            by[(i / w) * 16 / h] += 1;
        }
        println!("  pixels > 0.5 per 16th of the width {bx:?}, of the height {by:?}");
    }
    println!("| {name} | {mode} | {tc:.0} | {tg:.0} | {mean:.4} | {q999:.3} | {max:.3} | {above} | {ratio:.2} |");
    Ok(())
}
