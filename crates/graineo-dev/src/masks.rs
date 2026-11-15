//! graineo mask test command: sRGB demo, no metadata import.
use anyhow::{Context, Result};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};
pub fn run(file: &str, args: &[String]) -> Result<()> {
    anyhow::ensure!(
        (args.len() == 4 || (args.len() == 6 && args[4] == "--backend"))
            && args[0] == "--settings"
            && args[2] == "--output",
        "usage: graineo-dev masks image.jpg --settings masks.json --output after.png"
    );
    let settings = std::fs::read_to_string(&args[1])?;
    let document = masks::Document::from_json(&settings).map_err(anyhow::Error::msg)?;
    let output = Path::new(&args[3]);
    anyhow::ensure!(
        output.extension().and_then(|s| s.to_str()) == Some("png"),
        "output must be PNG"
    );
    anyhow::ensure!(!output.exists(), "output file already exists");
    let photo = image::open(file)?.to_rgb32f();
    let linear: Vec<f32> = photo
        .as_raw()
        .iter()
        .map(|&v| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
        .collect();
    let t = Instant::now();
    let image = masks::Image::new(photo.width() as usize, photo.height() as usize, &linear)
        .map_err(anyhow::Error::msg)?;
    let corrected = match args.get(5).map(String::as_str).unwrap_or("auto") {
        "cpu" => masks::apply(image, &document).map_err(anyhow::Error::msg)?,
        "gpu" => masks::gpu::apply(image, &document)
            .map_err(anyhow::Error::msg)?
            .context("GPU or format version 2 missing")?,
        "auto" => masks::gpu::apply_auto(image, &document).map_err(anyhow::Error::msg)?,
        _ => anyhow::bail!("backend: cpu, gpu or auto"),
    };
    eprintln!(
        "{}×{}, {} layers, {:.2?} incl. image check and output buffer",
        photo.width(),
        photo.height(),
        document.layers.len(),
        t.elapsed()
    );
    let encoded = corrected
        .iter()
        .map(|&v| {
            let v = v.max(0.0);
            let s = if v <= 0.0031308 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (s.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();
    let rgb = image::RgbImage::from_raw(photo.width(), photo.height(), encoded)
        .context("invalid RGB")?;
    let mut writer = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?,
    );
    image::DynamicImage::ImageRgb8(rgb).write_to(&mut writer, image::ImageFormat::Png)?;
    writer.flush()?;
    Ok(())
}
