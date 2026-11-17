//! `graineo-dev apple-raw FILE [out.png] [--steps neutral|standard] [--version V] [--max PX] [--runs N]
//! [--exposure EV] [--white K,TINT] [--region x,y,w,h]`: the system RAW engine of macOS (`raw::apple`, the code the app's
//! comparison view uses) – probe, render, timings.
//!
//! Prints one `APPLEINFO` line (decoder versions, engine default, native size, baseline exposure) and per run one
//! `APPLERUN` line (run, decoder used, width, height, milliseconds). The first run of a fresh program build includes the
//! engine's shader compilation. With `out.png` the last run is written as 8-bit sRGB (display conversion of the app).

use anyhow::{anyhow, bail, Result};
use raw::apple::{self, Params, Steps};

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let out = args.first().filter(|a| !a.starts_with("--")).cloned();
    let steps = match value("--steps").as_deref() {
        None | Some("neutral") => Steps::Neutral,
        Some("standard") => Steps::Standard,
        Some(o) => bail!("--steps neutral|standard, not {o}"),
    };
    let nums = |s: String| -> Result<Vec<f32>> { s.split(',').map(|x| x.trim().parse::<f32>().map_err(|e| anyhow!("{x}: {e}"))).collect() };
    let white = match value("--white") {
        Some(w) => {
            let v = nums(w)?;
            Some((v[0], *v.get(1).unwrap_or(&0.0)))
        }
        None => None,
    };
    let region = match value("--region") {
        Some(r) => {
            let v = nums(r)?;
            if v.len() != 4 {
                bail!("--region x,y,w,h");
            }
            Some([v[0] as usize, v[1] as usize, v[2] as usize, v[3] as usize])
        }
        None => None,
    };
    let params = Params {
        decoder: value("--version"),
        steps,
        exposure_ev: value("--exposure").map_or(Ok(0.0), |v| v.parse::<f32>())?,
        white,
        max_edge: value("--max").map(|v| v.parse::<usize>()).transpose()?,
        region,
    };
    let runs: usize = value("--runs").map_or(Ok(1), |v| v.parse())?;
    let path = std::path::Path::new(file);
    let mut engine = apple::Engine::new().map_err(|e| anyhow!("{e}"))?;
    let p = engine.probe(path).map_err(|e| anyhow!("{e}"))?;
    println!("APPLEINFO\t{}\tversions={}\tdefault={}\tnative={}x{}\tbaselineExposure={:+.2}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), p.versions.join(","), p.engine_default, p.native[0], p.native[1], p.baseline_exposure);
    let mut last = None;
    for i in 0..runs.max(1) {
        // a fresh engine per run except the first: what the app does when it opens another photo
        if i > 0 {
            engine.forget();
        }
        let r = engine.render(path, &params).map_err(|e| anyhow!("{e}"))?;
        println!("APPLERUN\t{i}\t{}\t{}\t{}\t{:.0}", r.decoder, r.width, r.height, r.millis);
        last = Some(r);
    }
    if let (Some(out), Some(r)) = (out, last) {
        let rgba = apple::to_srgb8(&r.rgba);
        let img = image::RgbaImage::from_raw(r.width as u32, r.height as u32, rgba).ok_or_else(|| anyhow!("buffer size"))?;
        image::DynamicImage::ImageRgba8(img).to_rgb8().save(&out)?;
        println!("written {out}");
    }
    Ok(())
}

/// `graineo-dev apple-stage FILE [--version V] [--reference GRAINEO_FILE]`: the Apple path of a development (rawdev::apple):
/// open (probe + first preview), preview stage, full resolution; luminance of the linear preview against graineo's own
/// linear preview of the same file (or of `--reference`, another file of the same camera and scene) – the median ratio
/// in EV is the exposure offset of the engine's linear output (`rawdev::apple::LINEAR_OFFSET_EV`).
/// Lines: `APPLESTAGE open|preview|full ms`, `APPLEEV median_ev p25 p75 n`.
pub fn stage(file: &str, args: &[String]) -> Result<()> {
    use std::time::Instant;
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let engine = value("--version").map(|v| rawdev::apple::apple_value(&v));
    let s = develop::Settings { raw: develop::settings::RawAdjust { engine, ..Default::default() }, ..Default::default() };
    let t = Instant::now();
    let doc = rawdev::RawDoc::open_for(std::path::Path::new(file), &s, &|| true).map_err(|e| anyhow!(e))?.ok_or_else(|| anyhow!("cancelled"))?;
    let open = t.elapsed().as_secs_f64() * 1000.0;
    let a = doc.apple.clone().ok_or_else(|| anyhow!("not developed by the Apple engine"))?;
    println!("APPLEINFO\t{}\t{:?}\tversions {:?}\tnative {:?}\tas shot {:?}", a.used, a.reason, a.probe.versions, a.probe.native, a.probe.as_shot);
    println!("APPLESTAGE\topen\t{open:.0}");
    let t = Instant::now();
    let st = doc.preview_stage(&s);
    println!("APPLESTAGE\tpreview-detail\t{:.0}", t.elapsed().as_secs_f64() * 1000.0);
    let t = Instant::now();
    let _img = doc.render_preview(&s);
    println!("APPLESTAGE\tpreview-render\t{:.0}", t.elapsed().as_secs_f64() * 1000.0);
    if args.iter().any(|a| a == "--full") {
        let t = Instant::now();
        let full = doc.render_full(&s).map_err(|e| anyhow!(e))?;
        println!("APPLESTAGE\tfull\t{:.0}\t{}x{}", t.elapsed().as_secs_f64() * 1000.0, full.width, full.height);
    }
    let reference = value("--reference").unwrap_or_else(|| file.to_string());
    if let Ok(Some(g)) = rawdev::RawDoc::open_for(std::path::Path::new(&reference), &develop::Settings { raw: develop::settings::RawAdjust { engine: Some("graineo".into()), ..Default::default() }, ..Default::default() }, &|| true) {
        if g.apple.is_none() {
            let lum = |d: &rawdev::RawDoc| -> Vec<f32> {
                let w = d.lum();
                let gain = d.base_ev.exp2();
                let mut v: Vec<f32> = d.preview.px.iter().map(|p| (w[0] * p[0] + w[1] * p[1] + w[2] * p[2]) * gain).filter(|y| *y > 1e-4).collect();
                v.sort_by(|a, b| a.total_cmp(b));
                v
            };
            let (ya, yg) = (lum(&doc), lum(&g));
            let q = |v: &[f32], f: f32| v[((v.len() - 1) as f32 * f) as usize];
            let ev = |f: f32| (q(&yg, f) / q(&ya, f)).log2();
            println!("APPLEEV\t{:+.3}\t{:+.3}\t{:+.3}\t{}", ev(0.5), ev(0.25), ev(0.75), ya.len().min(yg.len()));
        }
    }
    let _ = st;
    Ok(())
}
