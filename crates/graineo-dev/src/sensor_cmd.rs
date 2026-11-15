//! `graineo-dev sensor <file> [--kind a,b,…|alle] [--crop x,y,w,h] [--zoom N] [--size PX]
//! [--kein-jpeg] [--ohne-karte] [--blatt] --ausgabe <folder>`
//!
//! Analyses from `crates/sensor_view` on a Bayer/X-Trans RAW: writes one PNG per layer
//! (`<kind>_<layer>.png`), the preview (`preview.png`), all metrics as `metrics.json` and,
//! with `--sheet`, a demo sheet (`sheet.png`). Times: decoding, preparation, per analysis
//! (first call and – for the fast ones – a second call).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use sensor_view::{analyze, Kind, Region, Options, RawImage};
use serde_json::json;

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let mut kinds: Vec<Kind> = Kind::ALL.to_vec();
    let mut o = Options::default();
    let mut output: Option<PathBuf> = None;
    let mut sheet = false;
    let mut i = 0;
    let value = |i: &mut usize| -> Result<String> {
        *i += 1;
        args.get(*i).cloned().context("value missing")
    };
    while i < args.len() {
        match args[i].as_str() {
            "--kind" => {
                let v = value(&mut i)?;
                if v != "all" {
                    kinds = v.split(',').map(|s| Kind::from_name(s.trim()).with_context(|| format!("unknown kind {s}"))).collect::<Result<_>>()?;
                }
            }
            "--crop" => {
                let v: Vec<usize> = value(&mut i)?.split(',').map(|s| s.trim().parse()).collect::<std::result::Result<_, _>>()?;
                anyhow::ensure!(v.len() == 4, "--crop x,y,w,h");
                o.detail = Some(Region { x: v[0], y: v[1], w: v[2], h: v[3] });
            }
            "--zoom" => o.zoom = value(&mut i)?.parse()?,
            "--size" => o.size = value(&mut i)?.parse()?,
            "--no-jpeg" => o.render_comparison = false,
            "--no-map" => o.full_image = false,
            "--no-grid" => o.grid = false,
            "--sheet" => sheet = true,
            "--output" => output = Some(PathBuf::from(value(&mut i)?)),
            a => bail!("unknown option {a}"),
        }
        i += 1;
    }
    let out = output.context("--output <folder> missing")?;
    std::fs::create_dir_all(&out)?;

    let t = Instant::now();
    let rb = RawImage::open(file).map_err(|e| anyhow::anyhow!("{e}"))?;
    let t_decode = t.elapsed();
    let t = Instant::now();
    rb.prepare();
    let t_pre = t.elapsed();
    eprintln!(
        "{}: {}×{} {:?}, crop {:?} – decoded {:.0} ms, prepared (histogram, saturation, noise model) {:.0} ms",
        file,
        rb.width,
        rb.height,
        rb.sensor,
        rb.crop,
        t_decode.as_secs_f64() * 1000.0,
        t_pre.as_secs_f64() * 1000.0
    );
    // without --ausschnitt: detail area around the sharpest point (more meaningful than the image center)
    if o.detail.is_none() {
        if let Ok(e) = analyze(&rb, Kind::Sharpness, &o) {
            let (x, y) = (e.metrics["sharpest_point"]["x"].as_u64().unwrap_or(0) as usize, e.metrics["sharpest_point"]["y"].as_u64().unwrap_or(0) as usize);
            o.detail = Some(Region { x: x.saturating_sub(64), y: y.saturating_sub(48), w: 128, h: 96 });
            eprintln!("  detail area around the sharpest point: {:?}", o.detail.unwrap());
        }
    }
    let mut all = serde_json::Map::new();
    let mut outcomes = Vec::new();
    let mut times = serde_json::Map::new();
    for kind in kinds {
        let e = match analyze(&rb, kind, &o) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("  {:<11} error: {err}", kind.name());
                all.insert(kind.name().into(), json!({"error": err.to_string()}));
                continue;
            }
        };
        let first_one = e.duration.as_secs_f64() * 1000.0;
        // second call: expensive kinds come from the cache, fast ones are recomputed
        let t2 = Instant::now();
        let _ = analyze(&rb, kind, &o);
        let second_one = t2.elapsed().as_secs_f64() * 1000.0;
        eprintln!("  {:<11} {:>7.1} ms (second call {:>6.1} ms{})  {} layers", kind.name(), first_one, second_one, if kind.expensive() { ", cache" } else { "" }, e.layers.len());
        times.insert(kind.name().into(), json!({"first_ms": first_one, "second_ms": second_one}));
        for eb in &e.layers {
            eb.canvas.write_png(out.join(format!("{}_{}.png", kind.name(), eb.name)))?;
        }
        all.insert(kind.name().into(), e.metrics.clone());
        outcomes.push(e);
    }
    // without JPEG comparison (pure histogram time)
    if o.render_comparison && outcomes.iter().any(|e| e.kind == Kind::Histogram) {
        let o2 = Options { render_comparison: false, ..o.clone() };
        let t = Instant::now();
        let _ = analyze(&rb, Kind::Histogram, &o2);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("  histogram without JPEG comparison {ms:.1} ms");
        times.insert("histogram_no_jpeg".into(), json!({"ms": ms}));
    }
    all.insert(
        "times".into(),
        json!({"decode_ms": t_decode.as_secs_f64() * 1000.0, "prepare_ms": t_pre.as_secs_f64() * 1000.0, "analyses": times}),
    );
    rb.preview(o.size).write_png(out.join("preview.png"))?;
    std::fs::write(out.join("metrics.json"), serde_json::to_string_pretty(&serde_json::Value::Object(all))?)?;
    if sheet {
        let b = sensor_view::sheet::demo_sheet(&rb, &outcomes, &o);
        b.write_png(out.join("sheet.png"))?;
        eprintln!("  sheet: {}", out.join("sheet.png").display());
    }
    Ok(())
}
