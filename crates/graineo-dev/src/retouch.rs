//! Small, file-safe demo; JPEG/PNG are interpreted as sRGB.
use anyhow::{bail, Context, Result};
use retouch::{Image, Mask, Method, Options, Search, Source};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let mut circle = None;
    let mut points = None;
    let mut radius = 20.0;
    let mut hardness = 0.7;
    let mut source = None;
    let mut output = None;
    let mut options = Options::default();
    let mut search = Search::default();
    let mut i = 0;
    while i < args.len() {
        let value = args
            .get(i + 1)
            .context("option needs a value")?;
        match args[i].as_str() {
            "--mask" => {
                let v = numbers(value, 3)?;
                circle = Some(([v[0], v[1]], v[2]));
            }
            "--brush" => {
                points = Some(
                    value
                        .split(';')
                        .map(|s| {
                            let v = numbers(s, 2)?;
                            Ok([v[0], v[1]])
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
            }
            "--radius" => radius = value.parse()?,
            "--hardness" => hardness = value.parse()?,
            "--source" => {
                let v = value
                    .split(',')
                    .map(str::parse::<i32>)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                anyhow::ensure!(v.len() == 2, "source: dx,dy");
                source = Some(Source { dx: v[0], dy: v[1] });
            }
            "--method" => {
                options.method = match value.as_str() {
                    "clone" => Method::Clone,
                    "heal" => Method::Heal,
                    _ => bail!("method: klonen or heilen"),
                }
            }
            "--search" => search.radius = value.parse()?,
            "--step" => search.step = value.parse()?,
            "--output" => output = Some(value),
            _ => bail!("unknown option {}", args[i]),
        }
        i += 2;
    }
    anyhow::ensure!(
        circle.is_some() != points.is_some(),
        "specify exactly one mask: --mask x,y,r or --brush 'x,y;x,y' --radius r"
    );
    let mask = if let Some((center, r)) = circle {
        Mask::circle(center, r, hardness)?
    } else {
        Mask::brush(&points.unwrap(), radius, hardness)?
    };
    let output = Path::new(output.context("--output missing")?);
    let format = match output.extension().and_then(|s| s.to_str()) {
        Some("jpg" | "jpeg") => image::ImageFormat::Jpeg,
        Some("png") => image::ImageFormat::Png,
        _ => bail!("output must be .jpg, .jpeg or .png"),
    };
    anyhow::ensure!(
        !output.exists(),
        "output file already exists (never overwritten)"
    );
    let rgb = image::open(file)?.to_rgb32f();
    let linear: Vec<f32> = rgb
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
    let start = Instant::now();
    let img = Image::new(rgb.width() as usize, rgb.height() as usize, &linear)?;
    let source = match source {
        Some(s) => s,
        None => {
            let found = retouch::find_source(img, &mask, search)?;
            eprintln!(
                "source search: {:?}, border MSE {:.8}, {} candidates",
                found.source, found.mean_squared_error, found.candidates
            );
            found.source
        }
    };
    let patch = retouch::compute(img, &mask, source, options)?;
    let result = patch.apply(img)?;
    eprintln!(
        "{}×{}, {:?}, {:.2?} incl. check/search/image copy, solver: {:?}",
        rgb.width(),
        rgb.height(),
        options.method,
        start.elapsed(),
        patch.report
    );
    anyhow::ensure!(
        patch.report.converged,
        "solver did not converge; no file written"
    );
    let encoded: Vec<u8> = result
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
    let out = image::RgbImage::from_raw(rgb.width(), rgb.height(), encoded)
        .context("invalid output buffer")?;
    let writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let mut writer = BufWriter::new(writer);
    image::DynamicImage::ImageRgb8(out).write_to(&mut writer, format)?;
    writer.flush()?;
    Ok(())
}
fn numbers(s: &str, n: usize) -> Result<Vec<f32>> {
    let v = s
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<Vec<f32>, _>>()?;
    anyhow::ensure!(v.len() == n, "expected: {n} numbers, separated by commas");
    Ok(v)
}
