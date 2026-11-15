//! `graineo-dev grain FILM [options]`: measure the graininess of the film simulation (RMS granularity as in the
//! data sheet: σ of density × 1000, circular aperture 48 µm). A uniform patch goes through
//! the simulation, the film is scanned directly (without paper); from σ of the linear scan follows
//! σ_D = σ_T / (T · ln 10).
//!
//! ```text
//! graineo-dev grain FILM [--ev <EV>] [--grain <x>] [--format <mm>] [--edge <px>] [--aperture <µm>] [--dev <min>]
//!                        [--korn even|film]
//! ```
//! Output (one line): `KORN film ev grain pixel_um sigma_d_x1000`

use anyhow::{Context, Result};
use develop::film::{self, FilmSettings};
use develop::look::Geo;
use develop::Linear;

pub fn run(args: &[String]) -> Result<()> {
    let stock = args.first().context("usage: graineo-dev grain FILM [--ev EV] …")?.clone();
    let value = |n: &str, std: f32| -> f32 {
        args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(std)
    };
    let ev = value("--ev", 0.0);
    let grain = value("--grain", 1.0);
    let format = value("--format", 35.0);
    let edge = value("--edge", 2400.0) as usize;
    let aperture = value("--aperture", 48.0);
    let dev = value("--dev", 0.0);
    let text = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let rgb: [f32; 3] = text("--rgb").map(|t| { let v: Vec<f32> = t.split(',').filter_map(|x| x.parse().ok()).collect(); [v[0], v[1], v[2]] }).unwrap_or([0.184; 3]);
    let is_print = args.iter().any(|a| a == "--print");
    let fs = FilmSettings { active: true, stock: stock.clone(), negative: !is_print, taking_filter: text("--filter").unwrap_or_default(), taking_filter_amount: value("--strength", 1.0), exposure: ev, grain, format_mm: format, film_dev: dev, grain_model: text("--grain").unwrap_or_default(), ..FilmSettings::default() };
    // test image instead of a patch: `--image in.png --off out.png` develops an sRGB image (e.g. ColorChecker)
    if let Some(photo_path) = text("--image") {
        let img = image::open(&photo_path)?.to_rgb8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let dec = |v: u8| { let c = v as f32 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
        let data: Vec<f32> = img.pixels().flat_map(|p| p.0.map(dec)).collect();
        let scene = Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
        let out = film::develop(&scene, &fs, Geo::full(w, h)).map_err(anyhow::Error::msg)?;
        let rgb8 = out.to_rgb8();
        let target = text("--out").context("--out missing")?;
        image::RgbImage::from_raw(w as u32, h as u32, rgb8).context("image size")?.save(target)?;
        return Ok(());
    }
    let scene = Linear { mono: false, haze: None, toneq: None, width: edge, height: edge, data: (0..edge * edge).flat_map(|_| rgb).collect(), detail: None };
    let out = film::develop(&scene, &fs, Geo::full(edge, edge)).map_err(anyhow::Error::msg)?;
    let pix_um = format * 1000.0 / edge as f32;
    // green channel, sRGB-encoded → linear
    let lin: Vec<f64> = out
        .data
        .chunks_exact(3)
        .map(|p| {
            let v = p[1] as f64 / 65535.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        })
        .collect();
    // circular aperture (diameter `aperture` µm) as the mean over the patch, center of the image
    let r = aperture / pix_um / 2.0;
    let ri = r.ceil() as i32;
    let mut kernel = Vec::new();
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            let distance = ((dx * dx + dy * dy) as f32).sqrt();
            if distance <= r {
                kernel.push((dx, dy));
            }
        }
    }
    let margin = ri as usize + 2;
    let (mut s, mut s2, mut n) = (0.0f64, 0.0f64, 0.0f64);
    let step = (r.max(1.0)) as usize;
    let mut y = margin;
    while y + margin < edge {
        let mut x = margin;
        while x + margin < edge {
            let m: f64 = kernel.iter().map(|&(dx, dy)| lin[(y as i32 + dy) as usize * edge + (x as i32 + dx) as usize]).sum::<f64>() / kernel.len() as f64;
            s += m;
            s2 += m * m;
            n += 1.0;
            x += step;
        }
        y += step;
    }
    let mean = s / n;
    let sigma = (s2 / n - mean * mean).max(0.0).sqrt();
    let sigma_d = sigma / (mean * std::f64::consts::LN_10);
    println!("GRAIN\t{stock}\t{ev}\t{grain}\t{pix_um:.2}\t{:.2}\t{mean:.5}", sigma_d * 1000.0);
    Ok(())
}
