//! `graineo-dev rawapp FILE out.jpg [options]`: develop a non-Sigma RAW exactly as in the app
//! (crates/rawdev): preview with timing, optionally full resolution, starting values from Lightroom
//! and comparison with a reference export (ΔE00).
//!
//! Options:
//!   --lr                 starting values from the Lightroom catalog (read only)
//!   --katalog PATH       a different catalog
//!   --lr-pfad PATH       location of the file in the Lightroom catalog (e.g. for an archived RAW that lives at the target)
//!   --xmp FILE           starting values from the crs: fields of an XMP file
//!   --vergleich REF|eingebettet   compare with a reference image or the embedded preview; a reference in sensor
//!                         orientation is turned like the output (automatic if the format differs, `--ref-sensor` always)
//!   --voll               full resolution instead of preview
//!   --ref-adobergb      the reference is Adobe RGB (otherwise sRGB); result ΔE00
//!   --bild PATH          side-by-side image (ours | reference)
//!   --einstellungen FILE  settings as JSON (as in the sidecar)
//!   --ohne-lr-masken    comparison run without imported local corrections
//!   --einstellungen-ausgabe FILE  save the imported starting values as JSON
//!   --zeige              print the Lightroom values read and the settings
//!   --anzeige            output of the app's display path (extended sRGB, clipped for the file) instead of the export path
//!   --ohne-look          without camera look (as "Neutral")
//!   --engine VALUE       engine of the raw stage: graineo, apple:8, apple:9 … (default: automatic)
//!   --kamera-look        with the camera look of the shot's picture style (as offered in the app; line `LOOK\tname`)

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use develop::Settings;

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let out = args.first().context("output file missing")?;
    let has_flag = |n: &str| args.iter().any(|a| a == n);
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let t = Instant::now();
    // `--engine graineo|apple:8|…`: engine of the raw stage (document field `RawAdjust::engine`); default automatic
    let engine_settings = Settings { raw: develop::settings::RawAdjust { engine: value("--engine"), ..Default::default() }, ..Settings::default() };
    let mut doc = rawdev::RawDoc::open_for(Path::new(file), &engine_settings, &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
    match &doc.apple {
        Some(a) => println!("ENGINE\tapple\t{}\t{:?}\t{}", a.used, a.reason, a.graineo_blocked.as_deref().unwrap_or("-")),
        None => println!("ENGINE\tgraineo"),
    }
    if let Some(p) = value("--lr-path") {
        doc.logical_path = std::path::PathBuf::from(p);
    }
    if has_flag("--lr") && std::env::var_os("GRAINEO_LR_ORIENTATION_OFF").is_none() {
        let catalog = value("--catalog").map(std::path::PathBuf::from).unwrap_or_else(crate::dev_catalog);
        rawdev::lightroom::apply_orientation_from_catalog(&mut doc, &catalog);
    }
    let t_open = t.elapsed();
    let mut s = engine_settings.clone();
    if let Some(j) = value("--settings") {
        s = serde_json::from_str(&std::fs::read_to_string(j)?)?;
    }
    let photo_path = rawdev::lightroom::ImageContext { camera_short: &raw::picture_style::camera_short(&doc.info.maker, &doc.info.model), looks: &doc.looks, orientation: doc.orientation, finished: doc.finished, size: doc.full_size };
    let mut lr_text = None;
    if has_flag("--lr") {
        let catalog = value("--catalog").map(std::path::PathBuf::from).unwrap_or_else(crate::dev_catalog);
        let lr_path = value("--lr-path").unwrap_or_else(|| file.to_string());
        let e = library::lr_develop::read(&catalog, Path::new(&lr_path)).map_err(|e| anyhow!(e))?;
        let e = e.into_iter().find(|e| e.copy.is_none()).context("not in the catalog or without settings")?;
        let w = rawdev::lightroom::from_catalog(&e.text).context("settings not readable")?;
        s = rawdev::lightroom::to_settings(&w, &photo_path, &s);
        lr_text = Some(e.text);
    }
    if let Some(x) = value("--xmp") {
        let w = rawdev::lightroom::from_xmp(&std::fs::read_to_string(x)?);
        s = rawdev::lightroom::to_settings(&w, &photo_path, &s);
    }
    if has_flag("--no-look") {
        s.raw.look = "neutral".into();
    }
    if has_flag("--camera-look") {
        // the look of the shot's picture style, as offered in the app (none if the camera has no bundled look for it)
        s.raw.look = doc.auto_look.clone().unwrap_or_else(|| "neutral".into());
        println!("LOOK\t{}", s.raw.look);
    }
    if has_flag("--show") {
        if let Some(t) = &lr_text {
            println!("{t}");
        }
        println!("{}", serde_json::to_string_pretty(&s)?);
    }
    if has_flag("--no-lr-masks") && s.raw.lightroom_masks.is_some() {
        s.local = None;
        s.raw.lightroom_masks = None;
    }
    if let Some(path) = value("--settings-output") {
        std::fs::write(path, serde_json::to_vec_pretty(&s)?)?;
    }
    if let Some(r) = &s.raw.lightroom_masks {
        println!("LRMASKS\t{}", serde_json::to_string(r)?);
    }
    rawdev::film_profiles::check(&s).map_err(|e| anyhow!(e))?;
    // preview: first the detail stage (noise/lens), then measure slider runs
    let t = Instant::now();
    let st = doc.preview_stage(&s);
    let t_stage = t.elapsed();
    let _ = st.mask(doc.lum());
    let mut times = Vec::new();
    let mut img = None;
    for k in 0..5 {
        let mut s2 = s.clone();
        s2.exposure += 0.01 * k as f64; // as when dragging: different values each time
        let t = Instant::now();
        let i = doc.render_stage(&st, &s2);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        if k == 0 {
            img = Some(i);
        }
    }
    times.sort_by(|a, b| a.total_cmp(b));
    let mut img = img.unwrap();
    if has_flag("--display") {
        // the app's display path (extended sRGB, rawdev::color::render_preview_encoded), clipped to 0 … 1 for the file
        if let Some(e) = rawdev::color::render_preview_encoded(&doc, &s).map_err(|e| anyhow!(e))? {
            let data = e.rgb.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect();
            img = develop::Rgb16 { width: e.width, height: e.height, data };
        }
    }
    let mut t_full = None;
    if has_flag("--full") {
        let t = Instant::now();
        img = doc.render_full(&s).map_err(|e| anyhow!(e))?;
        t_full = Some(t.elapsed());
    }
    let (qa, wa, ha) = (doc.orientation, img.width, img.height);
    let img = develop::geometry::apply(img, &s, doc.orientation);
    develop::export::write_jpeg(&img, Path::new(out), 92)?;
    println!(
        "{} · {}×{} (full {}×{}, orientation {qa}) · open {:.0?} · noise/lens preview {:.0?} · sliders {:.0}–{:.0} ms{}",
        raw::camera_display(&doc.info.maker, &doc.info.model),
        wa,
        ha,
        doc.full_size[0],
        doc.full_size[1],
        t_open,
        t_stage,
        times[0],
        times[times.len() / 2],
        t_full.map(|t| format!(" · full resolution {t:.1?}")).unwrap_or_default()
    );
    if let Some(r) = value("--compare") {
        let own = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).context("image")?;
        // reference: `eingebettet` = embedded camera preview of the file (sensor orientation); otherwise an image file
        let embedded = r == "embedded";
        let reference = if embedded {
            raw::RawFile::open(file).map_err(|e| anyhow!("{e}"))?.preview().context("no embedded preview")?
        } else {
            image::open(&r)?.to_rgb8()
        };
        // camera previews are not rotated by the camera: turn like our output (`--ref-sensor` also for 180°)
        let (reference, turned) = crate::raw_compare::reference_oriented(reference, (own.width(), own.height()), doc.orientation, embedded || has_flag("--ref-sensor"));
        if turned {
            println!("reference turned by {}° (orientation {})", u32::from(doc.orientation) * 90, doc.orientation);
        }
        let e = crate::raw_compare::compare_images(&own, &reference, has_flag("--ref-adobergb"));
        println!(
            "LRCOMPARE\t{}\t{}\t{:.2}\t{:.2}/{:.2}/{:.2}\t{:+.2}\t{:+.1}\t{:+.1}\t{:.1}",
            Path::new(file).file_name().unwrap_or_default().to_string_lossy(),
            raw::camera_display(&doc.info.maker, &doc.info.model),
            e.de,
            e.de_zone[0],
            e.de_zone[1],
            e.de_zone[2],
            e.ev_mid,
            e.dl,
            e.dc,
            e.psnr
        );
        if let Some(b) = value("--image") {
            crate::raw_compare::side_by_side(&own, &reference, has_flag("--ref-adobergb"), &b)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rotated BF sample (internal, skipped without it): the embedded preview is in sensor orientation, our
    /// output is turned (orientation 6/8/3); the reference has to be turned too, otherwise the comparison is useless.
    #[test]
    fn embedded_preview_is_turned_like_output() {
        let base = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testbilder/sigma-neue-kameras/BF"));
        for (name, quarters) in [("BF_00661.DNG", 1u8), ("BF_00986.DNG", 3u8)] {
            let p = base.join(name);
            if !p.exists() {
                continue;
            }
            let f = raw::RawFile::open(&p).unwrap();
            assert_eq!(match f.info.orientation { 6 => 1, 3 => 2, 8 => 3, _ => 0 }, quarters, "{name}");
            let prev = f.preview().expect("preview");
            assert!(prev.width() > prev.height(), "preview is in sensor orientation (landscape)");
            // our output: sensor format with the orientation applied
            let own = (f.info.height as u32, f.info.width as u32);
            let (r, turned) = crate::raw_compare::reference_oriented(prev, own, quarters, true);
            assert!(turned);
            assert!(r.height() > r.width(), "turned preview is portrait like the output");
        }
    }
}
