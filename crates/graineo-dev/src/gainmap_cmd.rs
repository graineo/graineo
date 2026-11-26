//! `graineo-dev gainmap SRC OUTDIR [--headroom EV] [--settings JSON] [--engine spp]`: export SRC (RAW, merged photo,
//! JPEG/HEIC with gain map, X3F) as the app does – a normal JPEG and the same JPEG with HDR gain map – then check that
//! the SDR pixels are identical, read the map back and print times and sizes.
//! `graineo-dev gainmap-read FILE`: metadata and map statistics of a gain map JPEG (own reader).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn decode(p: &Path) -> Result<Vec<u8>> {
    let b = std::fs::read(p)?;
    Ok(jpeg_decoder::Decoder::new(&b[..]).decode()?)
}

pub fn read(args: &[String]) -> Result<()> {
    let p = args.first().context("FILE missing")?;
    let b = std::fs::read(p)?;
    let Some(m) = gainmap::read(&b) else { bail!("no gain map found") };
    let (mut lo, mut hi, mut sum) = (f32::MAX, f32::MIN, 0f64);
    for v in &m.log2 {
        lo = lo.min(*v);
        hi = hi.max(*v);
        sum += *v as f64;
    }
    println!("map {}×{} channels {} metadata from {}", m.w, m.h, m.channels, if m.iso { "ISO 21496-1" } else { "XMP" });
    println!("{:?}", m.meta);
    println!("log2 gain: min {lo:.3} max {hi:.3} mean {:.3}", sum / m.log2.len().max(1) as f64);
    Ok(())
}

pub fn run(args: &[String]) -> Result<()> {
    let src = PathBuf::from(args.first().context("SRC missing")?);
    let out = PathBuf::from(args.get(1).context("OUTDIR missing")?);
    std::fs::create_dir_all(&out)?;
    let mut s = develop::Settings::default();
    let mut headroom = develop::HdrOutput::DEFAULT_HEADROOM;
    let mut spp = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--headroom" => {
                headroom = args.get(i + 1).context("value")?.parse()?;
                i += 1;
            }
            "--settings" => {
                s = serde_json::from_str(&std::fs::read_to_string(args.get(i + 1).context("value")?)?)?;
                i += 1;
            }
            "--engine" => {
                spp = args.get(i + 1).is_some_and(|v| v == "spp");
                i += 1;
            }
            a => bail!("unknown argument {a}"),
        }
        i += 1;
    }
    s.hdr.on = Some(true);
    s.hdr.headroom = headroom;
    let stem = src.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "x".into());
    let sdr = out.join(format!("{stem}.jpg"));
    let hdr = out.join(format!("{stem}-hdr.jpg"));
    let format = develop::export::Format::Jpeg { quality: 90 };
    let t = Instant::now();
    let scene;
    let (t_sdr, t_scene);
    if src.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f")) {
        let mut sess = develop::session::Session::open_for(&src, &s)?;
        let engine = if spp { develop::export::Engine::Spp } else { develop::export::Engine::Own };
        develop::export::color::export(&mut sess, &s, format, engine, &sdr, graineo_color::Space::Srgb, &Default::default())?;
        t_sdr = t.elapsed();
        let t1 = Instant::now();
        let sc = sess.render_full_scene(&s, spp).map_err(|e| anyhow::anyhow!(e))?;
        let sc = develop::geometry::apply_f32(sc, &s, sess.cal.orientation);
        scene = (rawdev::export::luminance(&sc.data), sc.width, sc.height);
        t_scene = t1.elapsed();
    } else {
        let mut doc = rawdev::RawDoc::open_for_output(&src, &s, &|| true).map_err(|e| anyhow::anyhow!(e))?.context("cancelled")?;
        rawdev::lightroom::apply_orientation(&mut doc);
        println!("source: merged {} · own gain map {}", doc.merged.is_some(), doc.hdr.is_some());
        rawdev::export::export_color_opts(&doc, &s, format, &sdr, graineo_color::Space::Srgb, &Default::default()).map_err(|e| anyhow::anyhow!(e))?;
        t_sdr = t.elapsed();
        let t1 = Instant::now();
        scene = rawdev::export::hdr_scene(&doc, &s).map_err(|e| anyhow::anyhow!(e))?;
        t_scene = t1.elapsed();
    }
    std::fs::copy(&sdr, &hdr)?;
    let t2 = Instant::now();
    let w = rawdev::export::write_gain_map(&hdr, &scene, headroom).map_err(|e| anyhow::anyhow!(e))?;
    let t_map = t2.elapsed();
    let (a, b) = (decode(&sdr)?, decode(&hdr)?);
    println!("SDR pixels identical: {}", a == b);
    let (ls, lh) = (std::fs::metadata(&sdr)?.len(), std::fs::metadata(&hdr)?.len());
    println!("times: SDR export {t_sdr:.2?}, scene {t_scene:.2?}, map+write {t_map:.2?} (+{:.0} %)", 100.0 * (t_scene + t_map).as_secs_f64() / t_sdr.as_secs_f64());
    println!("sizes: SDR {ls} bytes, with map {lh} bytes (+{:.1} %)", 100.0 * (lh - ls.min(lh)) as f64 / ls as f64);
    match w {
        Some(w) => println!("map {}×{}, max {:.2} EV, {} bytes → {}", w.w, w.h, w.max, w.bytes, hdr.display()),
        None => println!("nothing above SDR white: no map"),
    }
    read(&[hdr.to_string_lossy().to_string()]).ok();
    Ok(())
}
