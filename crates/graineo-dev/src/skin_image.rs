//! `graineo-dev skin-image <in.png|jpg> <out-prefix> [--hue H] [--sat S] [--width W]`: runs one picture through the
//! look stage with the skin-tone sliders and writes `<prefix>-pixel.png` (per-pixel weight, no smoothing),
//! `<prefix>-smooth.png` (smoothed weight map as in the app) and the two weight maps as grey images. With `--width`
//! the picture is resized first (preview size or 24 MP export). Prints the times of the look stage.

use std::time::Instant;

use anyhow::{Context, Result};
use develop::look::Geo;
use develop::{Rgb16, Settings};

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let prefix = args.first().context("output prefix missing")?;
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f32>().ok());
    let (hue, sat) = (value("--hue").unwrap_or(0.0), value("--sat").unwrap_or(0.0));
    let mut im = image::open(file).with_context(|| format!("opening {file}"))?.to_rgb16();
    if let Some(w) = value("--width") {
        let h = (im.height() as f32 * w / im.width() as f32).round() as u32;
        im = image::imageops::resize(&im, w as u32, h, image::imageops::FilterType::Triangle);
    }
    let (w, h) = (im.width() as usize, im.height() as usize);
    let src = Rgb16 { width: w, height: h, data: im.into_raw() };
    let s = Settings { skin_hue: hue, skin_sat: sat, ..Default::default() };
    let geo = Geo::full(w, h);
    // per-pixel weight (the state before the smoothing): the look stage with default settings is only this stage
    let px = |c: [u16; 3]| c.map(|v| v as f32 / 65535.0);
    let t = Instant::now();
    let pixel: Vec<u16> = {
        use rayon::prelude::*;
        src.data
            .par_chunks(3)
            .flat_map_iter(|c| develop::skin::apply_pixel(px([c[0], c[1], c[2]]), hue, sat).map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16))
            .collect()
    };
    let t_pixel = t.elapsed();
    let mut smooth = Rgb16 { width: w, height: h, data: src.data.clone() };
    let t = Instant::now();
    develop::look::apply(&mut smooth, &s, geo);
    let t_smooth = t.elapsed();
    // only the weight maps (for the grey pictures)
    let col = |i: usize| px([src.data[i * 3], src.data[i * 3 + 1], src.data[i * 3 + 2]]);
    let t = Instant::now();
    let wmap = develop::skin::weight_map(w, h, develop::skin::smooth_radius(w, h, 1.0), col);
    let t_map = t.elapsed();
    let raw_w: Vec<f32> = (0..w * h).map(|i| develop::skin::weight_and_guide(col(i)).0).collect();
    println!("{w}×{h} r={}  per-pixel stage {t_pixel:.1?}  look stage with smoothing {t_smooth:.1?}  weight map alone {t_map:.1?}", develop::skin::smooth_radius(w, h, 1.0));
    // flicker: mean absolute difference of horizontal neighbours where any weight is present
    let flicker = |m: &[f32]| {
        let (mut sum, mut n) = (0.0f64, 0usize);
        for y in 0..h {
            for x in 0..w - 1 {
                let (a, b) = (m[y * w + x], m[y * w + x + 1]);
                if a > 0.0 || b > 0.0 {
                    sum += (a - b).abs() as f64;
                    n += 1;
                }
            }
        }
        sum / n.max(1) as f64
    };
    let zero_kept = raw_w.iter().zip(&wmap).filter(|(a, b)| **a == 0.0 && **b == 0.0).count();
    let zero_raw = raw_w.iter().filter(|a| **a == 0.0).count();
    println!(
        "neighbour flicker: per-pixel {:.4}  smoothed {:.4}; weight-0 pixels kept at 0: {zero_kept} of {zero_raw} ({:.1} %)",
        flicker(&raw_w),
        flicker(&wmap),
        100.0 * zero_kept as f64 / zero_raw.max(1) as f64
    );
    let save16 = |d: Vec<u16>, n: &str| -> Result<()> {
        let b: Vec<u8> = d.iter().map(|v| (v >> 8) as u8).collect();
        image::RgbImage::from_raw(w as u32, h as u32, b).context("size")?.save(format!("{prefix}-{n}.png"))?;
        Ok(())
    };
    save16(src.data.clone(), "orig")?;
    save16(pixel, "pixel")?;
    save16(smooth.data, "smooth")?;
    for (m, n) in [(raw_w, "weight-pixel"), (wmap, "weight-smooth")] {
        let b: Vec<u8> = m.iter().map(|v| (v * 255.0).round() as u8).collect();
        image::GrayImage::from_raw(w as u32, h as u32, b).context("size")?.save(format!("{prefix}-{n}.png"))?;
    }
    Ok(())
}
