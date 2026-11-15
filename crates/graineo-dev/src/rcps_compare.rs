//! `rcps-vergleich <X3F> <folder> <name> <luma …>`: finished image from the noise reduction on the
//! CPU against the GPU. Test bench only:
//! writes image pairs and one JSON line per slider position to `<folder>/results.jsonl`.
use anyhow::{Context, Result};
use develop::{session::Preview, Session, Settings};
use rayon::prelude::*;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const GRID: usize = 8;
const FLIP_MAX_DELTA: f32 = 20.0;

fn lab(p: &[u8]) -> [f32; 3] {
    let l = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (l(p[0]), l(p[1]), l(p[2]));
    let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    [116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z))]
}

fn delta_e(a: &[u8], b: &[u8]) -> Vec<f32> {
    a.par_chunks(3).zip(b.par_chunks(3)).map(|(p, q)| crate::raw_compare::de2000(lab(p), lab(q))).collect()
}

fn stats(a: &[u8], b: &[u8], de: &[f32]) -> serde_json::Value {
    let n = de.len() as f64;
    let share = |t: f32| 100.0 * de.par_iter().filter(|d| **d > t).count() as f64 / n;
    let se: f64 = a.par_iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    serde_json::json!({
        "de_mean": de.par_iter().map(|d| *d as f64).sum::<f64>() / n,
        "de_max": de.par_iter().cloned().reduce(|| 0.0, f32::max),
        "above1": share(1.0), "above2": share(2.0), "above5": share(5.0),
        "psnr": if se > 0.0 { 10.0 * (255.0f64 * 255.0 / se).log10() } else { 99.0 },
    })
}

fn luma(rgb: &[u8]) -> Vec<f32> {
    rgb.par_chunks(3).map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).collect()
}

/// Mean jump across the tile borders against the same measure 8 pixels away.
fn seam(v: &[f32], w: usize, h: usize) -> f64 {
    let (mut on, mut off, mut non, mut noff) = (0f64, 0f64, 0f64, 0f64);
    for i in 1..GRID {
        let x = w * i / GRID;
        for y in 0..h {
            on += (v[y * w + x] - v[y * w + x - 1]).abs() as f64;
            non += 1.0;
            for c in [x - 8, x + 8] {
                off += (v[y * w + c] - v[y * w + c - 1]).abs() as f64;
                noff += 1.0;
            }
        }
        let y = h * i / GRID;
        for x in 0..w {
            on += (v[y * w + x] - v[(y - 1) * w + x]).abs() as f64;
            non += 1.0;
            for c in [y - 8, y + 8] {
                off += (v[c * w + x] - v[(c - 1) * w + x]).abs() as f64;
                noff += 1.0;
            }
        }
    }
    (on / non) / (off / noff).max(1e-9)
}

fn block_std(v: &[f32], w: usize, x: usize, y: usize, size: usize) -> (f64, f64) {
    let (mut s, mut q) = (0f64, 0f64);
    for r in y..y + size {
        for c in x..x + size {
            let a = v[r * w + c] as f64;
            s += a;
            q += a * a;
        }
    }
    let n = (size * size) as f64;
    (s / n, (q / n - (s / n).powi(2)).max(0.0).sqrt())
}

fn crop(rgb: &[u8], w: usize, h: usize, c: (usize, usize), size: (usize, usize)) -> image::RgbImage {
    let cw = size.0.min(w);
    let ch = size.1.min(h);
    let x0 = c.0.saturating_sub(cw / 2).min(w - cw);
    let y0 = c.1.saturating_sub(ch / 2).min(h - ch);
    let mut out = image::RgbImage::new(cw as u32, ch as u32);
    for y in 0..ch {
        for x in 0..cw {
            let i = ((y0 + y) * w + x0 + x) * 3;
            out.put_pixel(x as u32, y as u32, image::Rgb([rgb[i], rgb[i + 1], rgb[i + 2]]));
        }
    }
    out
}

fn turn(img: image::RgbImage, quarters: u8) -> image::RgbImage {
    match quarters % 4 {
        1 => image::imageops::rotate90(&img),
        2 => image::imageops::rotate180(&img),
        3 => image::imageops::rotate270(&img),
        _ => img,
    }
}

fn save(img: &image::RgbImage, folder: &Path, name: &str, zoom: u32) -> Result<()> {
    let path = folder.join(name);
    if zoom > 1 {
        image::imageops::resize(img, img.width() * zoom, img.height() * zoom, image::imageops::FilterType::Nearest).save(&path)?;
    } else {
        img.save(&path)?;
    }
    Ok(())
}

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let folder = Path::new(args.first().context("folder missing")?);
    let name = args.get(1).context("name missing")?;
    std::fs::create_dir_all(folder)?;
    let path = Path::new(file);
    let cancel = AtomicBool::new(false);
    let _ = develop::spp::gpu_adapter();
    let mut session = Session::open(file)?;
    let cal = session.cal.clone();
    let quarters = cal.orientation;
    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(folder.join("results.jsonl"))?;
    for arg in &args[2..] {
        let value: f64 = arg.parse()?;
        let mut s = Settings::default();
        s.nr_luma = value;
        let key = s.spp_key(path);
        let t = Instant::now();
        let cpu = develop::spp::working_key(path, &key, false, &cancel).map_err(anyhow::Error::msg)?;
        let cpu_ms = t.elapsed().as_secs_f64() * 1000.0;
        let mut gpu_times = Vec::new();
        let mut gpu = None;
        for _ in 0..2 {
            let t = Instant::now();
            gpu = Some(develop::spp::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?);
            gpu_times.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let used = develop::spp::last_used_gpu();
        let gpu = gpu.unwrap();
        let (lw, lh) = (cpu.width, cpu.height);
        // Tiles whose scrambling flipped: a large deviation, unlike rounding alone.
        let mut flipped = Vec::new();
        let mut tile_means = Vec::new();
        for ty in 0..GRID {
            for tx in 0..GRID {
                let (mut max, mut sum, mut n) = (0f32, 0f64, 0f64);
                for y in lh * ty / GRID..lh * (ty + 1) / GRID {
                    for x in lw * tx / GRID..lw * (tx + 1) / GRID {
                        for c in 0..3 {
                            let d = (cpu.data[(y * lw + x) * 3 + c] - gpu.data[(y * lw + x) * 3 + c]).abs();
                            max = max.max(d);
                            sum += d as f64;
                            n += 1.0;
                        }
                    }
                }
                tile_means.push(sum / n);
                if max > FLIP_MAX_DELTA {
                    flipped.push((tx, ty));
                }
            }
        }
        let render = |session: &mut Session, counts: develop::Linear| -> Result<(Vec<u8>, usize, usize, Vec<u8>, usize, usize)> {
            session.install_spp(key.clone(), counts);
            anyhow::ensure!(session.has_spp_for(&s.effective()), "key of the effective settings differs");
            let full = session.render_full(&s);
            let base = session.spp_base().context("base")?;
            let matrix = session.spp_matrix().context("matrix")?;
            let prev = Preview::from_spp(&base, matrix, 2400).render(&cal, &s);
            Ok((full.to_rgb8(), full.width, full.height, prev.to_rgb8(), prev.width, prev.height))
        };
        let (fc, w, h, pc, pw, ph) = render(&mut session, cpu)?;
        let (fg, _, _, pg, _, _) = render(&mut session, gpu)?;
        let de_full = delta_e(&fc, &fg);
        let de_prev = delta_e(&pc, &pg);
        let (yc, yg) = (luma(&fc), luma(&fg));
        let ydiff: Vec<f32> = yc.iter().zip(&yg).map(|(a, b)| b - a).collect();
        // Worst 64-pixel block
        let size = 64;
        let blocks: Vec<(usize, usize)> = (0..h / size).flat_map(|y| (0..w / size).map(move |x| (x * size, y * size))).collect();
        let worst = blocks.par_iter().map(|&(x, y)| {
            let mut sum = 0f64;
            for r in y..y + size { for c in x..x + size { sum += de_full[r * w + c] as f64; } }
            (sum, x, y)
        }).max_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
        // Smooth area: in a flipped tile if there is one, otherwise anywhere
        let in_flip = |x: usize, y: usize| flipped.iter().any(|&(tx, ty)| x >= w * tx / GRID && x + size <= w * (tx + 1) / GRID && y >= h * ty / GRID && y + size <= h * (ty + 1) / GRID);
        let smooth_of = |flip_only: bool| blocks.par_iter().filter(|&&(x, y)| !flip_only || in_flip(x, y)).filter_map(|&(x, y)| {
            let (m, sd) = block_std(&yc, w, x, y, size);
            (m > 50.0 && m < 210.0).then_some((sd, x, y, m))
        }).min_by(|a, b| a.0.total_cmp(&b.0));
        let smooth = smooth_of(!flipped.is_empty()).or_else(|| smooth_of(false));
        let noise = smooth.map(|(sc, x, y, m)| (sc, block_std(&yg, w, x, y, size).1, x, y, m));
        // Border of the flipped tile with the largest deviation against an unflipped neighbour
        let edge = flipped.iter().filter_map(|&(tx, ty)| {
            let m = tile_means[ty * GRID + tx];
            let free = |ax: isize, ay: isize| ax >= 0 && ay >= 0 && ax < GRID as isize && ay < GRID as isize && !flipped.contains(&(ax as usize, ay as usize));
            if free(tx as isize + 1, ty as isize) { Some((m, w * (tx + 1) / GRID, h * ty / GRID + h / GRID / 2)) }
            else if free(tx as isize - 1, ty as isize) { Some((m, w * tx / GRID, h * ty / GRID + h / GRID / 2)) }
            else if free(tx as isize, ty as isize + 1) { Some((m, w * tx / GRID + w / GRID / 2, h * (ty + 1) / GRID)) }
            else if free(tx as isize, ty as isize - 1) { Some((m, w * tx / GRID + w / GRID / 2, h * ty / GRID)) }
            else { None }
        }).max_by(|a, b| a.0.total_cmp(&b.0));

        let stem = format!("{name}_L{}", (value * 100.0).round() as i32);
        let diff_full: Vec<u8> = fc.iter().zip(&fg).map(|(a, b)| ((*a as i32 - *b as i32).unsigned_abs() * 8).min(255) as u8).collect();
        let mut places = vec![("worst", (worst.1 + size / 2, worst.2 + size / 2))];
        if let Some((_, x, y)) = edge { places.push(("tile_edge", (x, y))); }
        if let Some((_, _, x, y, _)) = noise { places.push(("smooth", (x + size / 2, y + size / 2))); }
        for (label, c) in &places {
            for (side, img) in [("cpu", &fc), ("gpu", &fg), ("diff", &diff_full)] {
                save(&turn(crop(img, w, h, *c, (420, 300)), quarters), folder, &format!("{stem}_{label}_{side}_100.png"), 1)?;
                save(&turn(crop(img, w, h, *c, (130, 94)), quarters), folder, &format!("{stem}_{label}_{side}_300.png"), 3)?;
            }
        }
        let whole = |rgb: &[u8]| image::RgbImage::from_raw(pw as u32, ph as u32, rgb.to_vec()).unwrap();
        turn(whole(&pc), quarters).save(folder.join(format!("{stem}_full_cpu.jpg")))?;
        turn(whole(&pg), quarters).save(folder.join(format!("{stem}_full_gpu.jpg")))?;
        // Whole-image difference: mean |Δ| per block, amplified, tile pattern visible
        let f = w.max(h).div_ceil(1200).max(1);
        let (bw, bh) = (w / f, h / f);
        let mut map = vec![0u8; bw * bh * 3];
        for by in 0..bh {
            for bx in 0..bw {
                for c in 0..3 {
                    let mut sum = 0u32;
                    for y in 0..f { for x in 0..f {
                        let i = ((by * f + y) * w + bx * f + x) * 3 + c;
                        sum += (fc[i] as i32 - fg[i] as i32).unsigned_abs();
                    } }
                    map[(by * bw + bx) * 3 + c] = ((sum as f64 / (f * f) as f64) * 40.0).min(255.0) as u8;
                }
            }
        }
        turn(image::RgbImage::from_raw(bw as u32, bh as u32, map).unwrap(), quarters).save(folder.join(format!("{stem}_full_diff.png")))?;
        let line = serde_json::json!({
            "file": name, "luma": value, "nr": key.nr, "gpu_used": used,
            "cpu_ms": cpu_ms, "gpu_ms": gpu_times, "tilted": flipped.len(), "tiles": GRID * GRID,
            "preview": stats(&pc, &pg, &de_prev), "full": stats(&fc, &fg, &de_full),
            "seam_cpu": seam(&yc, w, h), "seam_gpu": seam(&yg, w, h), "seam_difference": seam(&ydiff, w, h),
            "noise_cpu": noise.map(|n| n.0), "noise_gpu": noise.map(|n| n.1), "noise_brightness": noise.map(|n| n.4),
            "noise_in_tilted_tile": !flipped.is_empty(),
            "places": places.iter().map(|p| p.0).collect::<Vec<_>>(), "stem": stem,
        });
        writeln!(out, "{line}")?;
        println!("{line}");
    }
    Ok(())
}
