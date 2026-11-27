//! A photo with its own HDR gain map (HDR on automatically) must follow every tone slider: the developed SDR preview
//! and its HDR rendition (SDR × preview gain map, what an EDR display shows) change with exposure, contrast and
//! highlights. The test photo is made here: a small chart (`library/tests/heic/srgb.heic`) exported brightened as a
//! JPEG with gain map, then opened again like an iPhone photo with gain map.

use develop::Settings;
use std::path::PathBuf;

fn lin(v: u16) -> f64 {
    let c = v as f64 / 65535.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Mean linear value of the SDR preview and of its HDR rendition (None: no map for these settings).
fn means(doc: &rawdev::RawDoc, s: &Settings) -> (f64, Option<f64>) {
    let img = doc.render_preview(s);
    let (w, h) = (img.width, img.height);
    let rgb: Vec<u8> = img.data.iter().map(|v| (v >> 8) as u8).collect();
    let sdr = img.data.iter().map(|v| lin(*v)).sum::<f64>() / img.data.len() as f64;
    let hdr = rawdev::export::hdr_preview_map(doc, s, &rgb, w, h).map(|m| {
        let mut sum = 0.0;
        for y in 0..h {
            let ky = (y * m.h / h).min(m.h - 1);
            for x in 0..w {
                let g = (m.log2[ky * m.w + (x * m.w / w).min(m.w - 1)] as f64).exp2();
                let i = (y * w + x) * 3;
                sum += (0..3).map(|c| lin(img.data[i + c]) * g).sum::<f64>();
            }
        }
        sum / (w * h * 3) as f64
    });
    (sdr, hdr)
}

#[test]
fn sliders_change_photo_with_gain_map() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../library/tests/heic/srgb.heic");
    if !rawdev::RawDoc::open(&src, &|| true).is_ok_and(|d| d.is_some()) {
        eprintln!("HEIC not readable on this system, skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("graineo-hdr-sliders-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let jpg = dir.join("gm.jpg");
    // brightened export: the chart's light patches clip in SDR and go into the gain map
    let mut bright = Settings::default();
    bright.exposure = 2.0;
    let doc = rawdev::RawDoc::open_for_output(&src, &bright, &|| true).unwrap().unwrap();
    rawdev::export::export_color_opts(&doc, &bright, develop::export::Format::Jpeg { quality: 90 }, &jpg, graineo_color::Space::Srgb, &Default::default()).unwrap();
    let scene = rawdev::export::hdr_scene(&doc, &bright).unwrap();
    let written = rawdev::export::write_gain_map(&jpg, &scene, develop::HdrOutput::DEFAULT_HEADROOM).unwrap();
    assert!(written.is_some(), "no gain map written");

    let base = Settings::default();
    let doc = rawdev::RawDoc::open_for(&jpg, &base, &|| true).unwrap().unwrap();
    assert!(doc.hdr.is_some() && rawdev::export::hdr_auto(&doc), "own gain map not read: HDR not on automatically");
    let (sdr0, hdr0) = means(&doc, &base);
    assert!(hdr0.is_some(), "no HDR rendition of the gain map photo");
    let mut changes: Vec<(&str, Settings)> = Vec::new();
    for (name, f) in [("exposure -5", (|s: &mut Settings| s.exposure = -5.0) as fn(&mut Settings)), ("exposure -1", |s| s.exposure = -1.0), ("contrast +41", |s| s.contrast = 0.41), ("highlights -100", |s| s.highlight = -1.0)] {
        let mut s = base.clone();
        f(&mut s);
        changes.push((name, s));
    }
    for (name, s) in &changes {
        let (sdr, hdr) = means(&doc, s);
        assert!((sdr - sdr0).abs() > 0.005, "{name}: SDR preview unchanged ({sdr0:.4} → {sdr:.4})");
        let (h0, h) = (hdr0.unwrap(), hdr.unwrap_or(sdr));
        assert!((h - h0).abs() > 0.005, "{name}: HDR rendition unchanged ({h0:.4} → {h:.4})");
    }
    // with the basic settings switched off the same sliders do nothing (that is what the switch is for; the
    // section header then always shows it, see `lr::section_full`)
    for (name, s) in &mut changes {
        s.bypass.basic = true;
        let (sdr, _) = means(&doc, s);
        assert!((sdr - sdr0).abs() < 1e-4, "{name}: basic settings off but the preview changed");
    }
    std::fs::remove_dir_all(&dir).ok();
}
