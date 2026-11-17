//! `graineo-dev raw <FILE> [out.jpg] [--info] [--max PX] [--rawler] [--phase1] [--compare REF.jpg]`
//! for non-Sigma RAWs.
//!
//! Opens the file via `crates/raw` (rawler), prints the key data and develops with the
//! default development (`RawFile::entwickeln`: RCD/Markesteijn, highlights, DNG color,
//! sigmoid, capture sharpening); `--phase1` uses the simple test development instead.
//! Options of the default development: `--demosaic auto|bilinear|rcd|markesteijn1|markesteijn3`, `--no-highlights`,
//! `--sharpness S` (0 = off), `--ev X`, `--contrast K`, `--skew S`, `--saturation S`,
//! `--hue F`, `--space rec2020|srgb`, `--no-camera-ev`.
//! `--compare REF.jpg [--ref-adobergb] [--compare-image B.jpg]`: comparison with a reference
//! (e.g. a reference preview, Adobe RGB) – ΔE2000 by zones (shadows/midtones/highlights by L* of the
//! reference), exposure (EV, midtones), PSNR; line `COMPARE\t…`.
//! The result is also
//! roughly compared against the embedded camera preview (ΔE76, ΔL, Δa/Δb, PSNR, exposure in EV).
//! `--max PX` downscales the output (long edge) and places the camera preview at the same size as
//! `<out>-kamera.jpg` next to it; `--rawler` additionally computes the complete rawler path
//! (`<out>-rawler.jpg`) to attribute errors to rawler or to our layer.
//! Last line `RESULT\t…` (tab-separated) for batch evaluations.

use std::time::Instant;

use anyhow::{Context, Result};
use image::RgbImage;
use raw::{Linear, RawFile};

pub fn run(file: &str, rest: &[String]) -> Result<()> {
    let info_only = rest.iter().any(|a| a == "--info");
    let with_rawler = rest.iter().any(|a| a == "--rawler");
    let max: Option<u32> = rest.iter().position(|a| a == "--max").and_then(|i| rest.get(i + 1)).and_then(|v| v.parse().ok());
    const WITH_VALUE: [&str; 26] = [
        "--max", "--black", "--compare", "--compare-image", "--demosaic", "--sharpness", "--ev", "--contrast", "--skew", "--saturation", "--hue",
        "--space", "--sigma", "--iter", "--look-file", "--matrix-file", "--nr-luminance", "--nr-detail", "--nr-color", "--nr-color-detail",
        "--distortion", "--vignette", "--crop", "--look", "--geo-compare", "--highlights-test",
    ];
    let out = rest.iter().enumerate().find(|(i, a)| !a.starts_with("--") && (*i == 0 || !WITH_VALUE.contains(&rest[i - 1].as_str()))).map(|(_, a)| a.clone());
    let value = |k: &str| rest.iter().position(|a| a == k).and_then(|i| rest.get(i + 1)).cloned();
    let number = |k: &str| value(k).and_then(|v| v.parse::<f32>().ok());
    let phase1 = rest.iter().any(|a| a == "--phase1");
    let name = std::path::Path::new(file).file_name().and_then(|s| s.to_str()).unwrap_or(file).to_string();

    let t0 = Instant::now();
    let rf = match RawFile::open(file) {
        Ok(r) => r,
        Err(e) => {
            println!("RESULT\t{name}\tERROR\t{e}");
            return Err(anyhow::anyhow!("{e}"));
        }
    };
    let t_open = t0.elapsed();
    // `--highlights-test EV`: synthetic overexposure, highlight reconstruction global/local against the ground truth
    if let Some(ev) = number("--highlights-test") {
        if let Some(r) = raw::highlights::check_synthetic(&rf, ev) {
            println!("HIGHLIGHTS\t{name}\t{}\t{:.4}\t{:.4}\t{:.4}\t{:.4}", r[0].0, r[0].1, r[0].2, r[1].1, r[1].2);
        }
        return Ok(());
    }
    let mut rf = rf;
    // `--black N`: debugging – override the black point (all channels)
    if let Some(b) = rest.iter().position(|a| a == "--black").and_then(|i| rest.get(i + 1)).and_then(|v| v.parse::<f32>().ok()) {
        rf.info.black.iter_mut().for_each(|v| *v = b);
    }
    // `--matrix-file F`: replace the ForwardMatrix per illuminant (lines "Kelvin 9×CM 9×FM", e.g.
    // fitted from spectral sensitivities); the ColorMatrix from rawler stays for the color temperature.
    if let Some(f) = rest.iter().position(|a| a == "--matrix-file").and_then(|i| rest.get(i + 1)) {
        let txt = std::fs::read_to_string(f).with_context(|| format!("matrix file {f}"))?;
        let rows: Vec<Vec<f32>> = txt.lines().map(|l| l.split_whitespace().filter_map(|x| x.parse().ok()).collect::<Vec<f32>>()).filter(|v| v.len() >= 19).collect();
        for k in rf.info.calibration.iter_mut() {
            if let Some(z) = rows.iter().min_by(|a, b| (a[0] - k.kelvin).abs().total_cmp(&(b[0] - k.kelvin).abs())) {
                let m = &z[10..19];
                k.forward = Some([[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]]);
            }
        }
    }
    print_info(&rf);
    eprintln!("decoded in {t_open:.2?}");
    if info_only && out.is_none() {
        return Ok(());
    }
    let out = out.context("output file missing")?;

    let ev = raw::BASE_EV + rf.info.baseline_exposure;
    let (ours, t_lin, t_col) = if phase1 {
        // own layer, simple test development
        let t1 = Instant::now();
        let lin = rf.linear().map_err(|e| anyhow::anyhow!("{e}"))?;
        let t_lin = t1.elapsed();
        let t2 = Instant::now();
        // `--wb-neutral`: debugging (e.g. already balanced data such as Canon sRAW)
        let wb = if rest.iter().any(|a| a == "--wb-neutral") { [1.0; 3] } else { rf.info.wb.unwrap_or([1.0; 3]) };
        let m = rf.camera_to_srgb().map_err(|e| anyhow::anyhow!("{e}"))?;
        let srgb = raw::camera_to_srgb(&lin, wb, &m, true);
        let ours = raw::tone_curve_srgb8(&srgb, ev);
        let t_col = t2.elapsed();
        eprintln!(
            "test development (phase 1): linear+demosaic {t_lin:.2?} · color+tone {t_col:.2?} · total from open {:.2?} ({}×{})",
            t0.elapsed(),
            ours.width(),
            ours.height()
        );
        (ours, t_lin, t_col)
    } else {
        use raw::development::{WorkingSpace, Demosaic, Options};
        let mut o = Options::default();
        if let Some(d) = value("--demosaic") {
            o.demosaic = match d.as_str() {
                "bilinear" => Demosaic::Bilinear,
                "rcd" => Demosaic::Rcd,
                "markesteijn1" => Demosaic::Markesteijn1,
                "markesteijn3" => Demosaic::Markesteijn3,
                _ => Demosaic::Auto,
            };
        }
        o.highlights = !rest.iter().any(|a| a == "--no-highlights");
        o.camera_ev = !rest.iter().any(|a| a == "--no-camera-ev");
        if let Some(s) = number("--sharpness") {
            if s <= 0.0 {
                o.sharpness = None;
            } else if let Some(sc) = o.sharpness.as_mut() {
                sc.strength = s;
            }
        }
        if let (Some(sg), Some(sc)) = (number("--sigma"), o.sharpness.as_mut()) {
            sc.sigma = sg;
        }
        if let (Some(it), Some(sc)) = (number("--iter"), o.sharpness.as_mut()) {
            sc.iterations = it as usize;
        }
        if let Some(v) = number("--ev") {
            o.ev = v;
        }
        if let Some(v) = number("--contrast") {
            o.tone.contrast = v;
        }
        if let Some(v) = number("--skew") {
            o.tone.skew = v;
        }
        if let Some(v) = number("--saturation") {
            o.tone.saturation = v;
        }
        if let Some(v) = number("--hue") {
            o.tone.hue = v;
        }
        if rest.iter().any(|a| a == "--no-look") {
            o.look = None;
            o.camera_look = false;
        }
        if let Some(f) = value("--look-file") {
            // 18 lines "Δh chroma-factor" (experiments/cross-validation)
            let txt = std::fs::read_to_string(&f).with_context(|| format!("look file {f}"))?;
            let mut l = raw::development::STANDARD_LOOK;
            for (k, line) in txt.lines().filter(|l| !l.trim().is_empty()).take(18).enumerate() {
                let v: Vec<f32> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if v.len() >= 2 {
                    l[k] = [v[0], v[1]];
                }
            }
            o.look = Some(l);
        }
        if value("--space").as_deref() == Some("srgb") {
            o.working_space = WorkingSpace::Srgb;
        }
        // camera look: `--look camera/style` (own look in the data folder, else bundled), `auto` (picture style of the
        // shot from the makernote) or a `.x3look` file; replaces the Lightroom look.
        let style_look = match value("--look") {
            Some(n) => {
                let l = if n == "auto" {
                    let b = raw::picture_style::read(std::path::Path::new(file));
                    eprintln!("picture style: {}", b.as_ref().map(|b| format!("{}/{}", b.camera_short(), b.style)).unwrap_or("-".into()));
                    b.and_then(|b| rawdev::looks::for_picture_style(&b)).map(|l| (*l).clone())
                } else if n.ends_with(".x3look") {
                    raw::looks::Look::file(std::path::Path::new(&n))
                } else {
                    rawdev::looks::find(&n).map(|l| (*l).clone())
                };
                if l.is_none() && n != "auto" {
                    anyhow::bail!("look {n} not found (own and bundled: {})", rawdev::looks::names().join(", "));
                }
                o.look = None;
                o.camera_look = false;
                l
            }
            None => None,
        };
        // noise reduction (Lightroom-style sliders 0…100) and lens correction
        if rest.iter().any(|a| a == "--no-noise") {
            o.noise = raw::noise::NoiseReduction::OFF;
        }
        for (k, patch) in [("--nr-luminance", 0), ("--nr-detail", 1), ("--nr-color", 2), ("--nr-color-detail", 3)] {
            if let Some(v) = number(k) {
                let r = &mut o.noise;
                *[&mut r.luminance, &mut r.luminance_detail, &mut r.color, &mut r.color_detail][patch] = v;
            }
        }
        if rest.iter().any(|a| a == "--no-lens") {
            o.lens = None;
        }
        if let Some(ob) = o.lens.as_mut() {
            ob.lensfun = !rest.iter().any(|a| a == "--no-lensfun");
            ob.lateral_ca = !rest.iter().any(|a| a == "--no-lateral-error");
            ob.adobe = !rest.iter().any(|a| a == "--no-adobe");
            ob.backup_enabled = !rest.iter().any(|a| a == "--no-backup");
            if let Some(v) = number("--distortion") {
                ob.distortion = v;
            }
            if let Some(v) = number("--vignette") {
                ob.vignette = v;
            }
        }
        let t1 = Instant::now();
        let e = rf.develop(&o).map_err(|e| anyhow::anyhow!("{e}"))?;
        let t_all = t1.elapsed();
        let z: Vec<String> = e.timings.iter().map(|(n, d)| format!("{n} {d:.0?}")).collect();
        let evs: Vec<String> = e.ev_parts.iter().map(|(n, v)| format!("{n} {v:+.2}")).collect();
        eprintln!(
            "development ({:?}{}): {} · total {t_all:.2?} (from open {:.2?}, {}×{})",
            e.demosaic,
            if e.clipped_frac > 0 { format!(", {} photosites blown out", e.clipped_frac) } else { String::new() },
            z.join(" · "),
            t0.elapsed(),
            e.img.width(),
            e.img.height()
        );
        if let Some(m) = &e.noise_model {
            eprintln!("noise model (variance = a·value + b, R/G/B): a {:.2e}/{:.2e}/{:.2e} · b {:.2e}/{:.2e}/{:.2e} · ISO {}", m.a[0], m.a[1], m.a[2], m.b[0], m.b[1], m.b[2], rf.info.exif.iso.map(|v| v.to_string()).unwrap_or("?".into()));
        }
        if let Some(ob) = &e.lens {
            eprintln!("lens: {ob} · profile: {}", e.lens_source.unwrap_or("–"));
        }
        if let Some(f) = &e.color {
            eprintln!("color: {:.0} K (weight illuminant 1: {:.2}){} · exposure {:+.2} EV ({})", f.kelvin, f.weight, if f.forward { " · ForwardMatrix" } else { "" }, e.ev, evs.join(", "));
        }
        let demo = e.timings.iter().filter(|(n, _)| *n != "Farbe+Ton").map(|(_, d)| *d).sum();
        let col = e.timings.iter().filter(|(n, _)| *n == "Farbe+Ton").map(|(_, d)| *d).sum();
        let mut photo_path = e.img;
        if let Some(l) = &style_look {
            let t = Instant::now();
            l.apply(&mut photo_path);
            eprintln!("camera look {} ({:?})", l.name, t.elapsed());
        }
        (photo_path, demo, col)
    };
    // comparison with a reference (e.g. a reference preview)
    if let Some(r) = value("--compare") {
        let adobe = rest.iter().any(|a| a == "--ref-adobergb");
        let oriented = raw::orient(ours.clone(), rf.info.orientation);
        let refimg = image::open(&r).with_context(|| format!("reference {r}"))?.to_rgb8();
        let v = crate::raw_compare::compare_images(&oriented, &refimg, adobe);
        eprintln!(
            "comparison {}: ΔE2000 {:.2} (shadows {:.2} · midtones {:.2} · highlights {:.2}; shares {:.0}/{:.0}/{:.0} %) · EV midtones {:+.2} · EV total {:+.2} · ΔL {:+.1} · Δa {:+.1} · Δb {:+.1} · ΔC {:+.1} · PSNR {:.1} dB",
            std::path::Path::new(&r).file_name().and_then(|s| s.to_str()).unwrap_or(""),
            v.de, v.de_zone[0], v.de_zone[1], v.de_zone[2], v.fraction[0] * 100.0, v.fraction[1] * 100.0, v.fraction[2] * 100.0, v.ev_mid, v.ev, v.dl, v.da, v.db, v.dc, v.psnr
        );
        println!(
            "COMPARE\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.0}",
            v.de, v.de_zone[0], v.de_zone[1], v.de_zone[2], v.ev_mid, v.ev, v.dl, v.dc, v.psnr, v.da, v.db, (t_lin + t_col).as_secs_f64() * 1000.0
        );
        if let Some(b) = value("--compare-image") {
            crate::raw_compare::side_by_side(&oriented, &refimg, adobe, &b)?;
        }
    }
    // geometry/vignette against a reference with lens correction
    if let Some(r) = value("--geo-compare") {
        let oriented = raw::orient(ours.clone(), rf.info.orientation);
        let refimg = image::open(&r).with_context(|| format!("reference {r}"))?.to_rgb8();
        let g = crate::geo_compare::compare_images(&oriented, &refimg);
        eprintln!(
            "geometry: {} fields · residual RMS {:.2} px · max {:.2} px · radial outside−inside {:+.2} px · scale {:.4} · vignette corners ours {:+.2} EV / reference {:+.2} EV",
            g.patches, g.rms, g.max, g.radial, g.scale, g.vig.0, g.vig.1
        );
        println!("GEO\t{name}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.4}\t{:.3}\t{:.3}", g.patches, g.rms, g.max, g.radial, g.scale, g.vig.0, g.vig.1);
    }
    let t3 = Instant::now();
    let preview = if rest.iter().any(|a| a == "--no-preview") { None } else { rf.preview() };
    let t_prev = t3.elapsed();
    let cmp = preview.as_ref().map(|p| compare(&ours, p));
    match (&preview, &cmp) {
        (Some(p), Some(c)) => eprintln!(
            "preview {}×{} ({t_prev:.2?}): ΔE76 {:.1} · ΔL {:+.1} · Δa {:+.1} · Δb {:+.1} · PSNR {:.1} dB · exposure {:+.2} EV",
            p.width(),
            p.height(),
            c.de,
            c.dl,
            c.da,
            c.db,
            c.psnr,
            c.ev
        ),
        _ => eprintln!("no embedded preview"),
    }
    let mut rawler_cmp = None;
    if with_rawler {
        let t4 = Instant::now();
        match rf.rawler_linear_srgb() {
            Ok(l) => {
                let img = raw::tone_curve_srgb8(&Linear { px: l.px, ..l }, ev);
                let t = t4.elapsed();
                if let Some(p) = &preview {
                    let c = compare(&img, p);
                    eprintln!(
                        "rawler path ({t:.2?}, {}×{}): ΔE76 {:.1} · ΔL {:+.1} · Δa {:+.1} · Δb {:+.1} · exposure {:+.2} EV",
                        img.width(),
                        img.height(),
                        c.de,
                        c.dl,
                        c.da,
                        c.db,
                        c.ev
                    );
                    rawler_cmp = Some(c);
                }
                save(&img, rf.info.orientation, max, &suffix(&out, "-rawler"))?;
            }
            Err(e) => eprintln!("rawler path: {e}"),
        }
    }
    save(&ours, rf.info.orientation, max, &out)?;
    // `--crop X,Y,W,H`: 100 % crop (coordinates in the rotated image) as PNG next to it
    if let Some(a) = value("--crop") {
        let v: Vec<u32> = a.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 4 {
            let img = raw::orient(ours.clone(), rf.info.orientation);
            let (x, y) = (v[0].min(img.width().saturating_sub(1)), v[1].min(img.height().saturating_sub(1)));
            let (w, h) = (v[2].min(img.width() - x), v[3].min(img.height() - y));
            let crop = image::imageops::crop_imm(&img, x, y, w, h).to_image();
            crop.save(suffix(&out, "-ausschnitt").replace(".jpg", ".png"))?;
        }
    }
    if let (Some(p), Some(_)) = (&preview, max) {
        save(p, rf.info.orientation, max, &suffix(&out, "-kamera"))?;
    }
    let i = &rf.info;
    let f = |c: &Option<RoughComparison>, g: fn(&RoughComparison) -> f32| c.as_ref().map(|c| format!("{:.2}", g(c))).unwrap_or("-".into());
    println!(
        "RESULT\t{name}\tok\t{}\t{}\t{}\t{:?}\t{}x{}\t{:.0}\t{:.0}\t{}\t{}\t{}\t{}\t{}\t{}",
        raw::camera_display(&i.maker, &i.model),
        i.format,
        i.mode,
        i.sensor,
        ours.width(),
        ours.height(),
        t_open.as_secs_f64() * 1000.0,
        (t_lin + t_col).as_secs_f64() * 1000.0,
        f(&cmp, |c| c.de),
        f(&cmp, |c| c.dl),
        f(&cmp, |c| c.da),
        f(&cmp, |c| c.db),
        f(&cmp, |c| c.ev),
        f(&rawler_cmp, |c| c.de),
    );
    Ok(())
}

fn suffix(out: &str, s: &str) -> String {
    match out.rfind('.') {
        Some(p) => format!("{}{s}{}", &out[..p], &out[p..]),
        None => format!("{out}{s}"),
    }
}

fn save(img: &RgbImage, orient: u16, max: Option<u32>, path: &str) -> Result<()> {
    let img = match max {
        Some(m) if img.width().max(img.height()) > m => {
            let s = m as f32 / img.width().max(img.height()) as f32;
            image::imageops::thumbnail(img, (img.width() as f32 * s).round() as u32, (img.height() as f32 * s).round() as u32)
        }
        _ => img.clone(),
    };
    let img = raw::orient(img, orient);
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 85).encode_image(&img)?;
    Ok(())
}

fn print_info(rf: &RawFile) {
    let i = &rf.info;
    println!("camera:        {}  (format {}, mode \"{}\")", raw::camera_display(&i.maker, &i.model), i.format, i.mode);
    println!("sensor:        {}×{} · {} channel(s) · {} bit · {:?}", i.width, i.height, i.channels, i.bits, i.sensor);
    if !i.cfa.is_empty() {
        println!("CFA:           {} ({}×{})", i.cfa, i.cfa_size.0, i.cfa_size.1);
    }
    let bl: Vec<String> = i.black.iter().take(8).map(|v| format!("{v:.0}")).collect();
    println!(
        "black/white:   [{}]{} (pattern {}×{}×{}) / {:?}",
        bl.join(", "),
        if i.black.len() > 8 { " …" } else { "" },
        i.black_pattern.0,
        i.black_pattern.1,
        i.black_pattern.2,
        i.white
    );
    match i.wb {
        Some(w) => println!("white balance: R {:.3} · G 1 · B {:.3} (as shot)", w[0], w[2]),
        None => println!("white balance: – (not in the file)"),
    }
    match &i.matrix {
        Some((l, m)) => println!(
            "camera matrix: {l} [{:.4} {:.4} {:.4} | {:.4} {:.4} {:.4} | {:.4} {:.4} {:.4}]  (available: {})",
            m[0][0],
            m[0][1],
            m[0][2],
            m[1][0],
            m[1][1],
            m[1][2],
            m[2][0],
            m[2][1],
            m[2][2],
            i.matrix_illuminants.join(", ")
        ),
        None => println!("camera matrix: – missing"),
    }
    // raw value distribution (every 7th value) – shows whether the black/white points fit the data
    let d = rf.raw_data();
    let mut v: Vec<f32> = d.iter().step_by(7).copied().collect();
    v.sort_unstable_by(f32::total_cmp);
    if !v.is_empty() {
        let q = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
        println!("raw values:    min {:.0} · p1 {:.0} · p50 {:.0} · p99 {:.0} · p99.9 {:.0} · max {:.0}", v[0], q(0.01), q(0.5), q(0.99), q(0.999), v[v.len() - 1]);
    }
    println!("orientation:   {}", i.orientation);
    let r = |b: &Option<raw::Region>| b.map(|b| format!("{}+{} {}×{}", b.x, b.y, b.w, b.h)).unwrap_or("–".into());
    println!("active/crop:   {} / {}", r(&i.active), r(&i.crop));
    if i.format == "DNG" {
        println!("DNG:           BaselineExposure {:+.2} EV · profile tags: {}", i.baseline_exposure, if i.dng_profile.is_empty() { "–".into() } else { i.dng_profile.join(", ") });
    }
    if let Some(dr) = i.fuji_dr {
        println!("Fuji DR:       {dr} % (makernote)");
    }
    for k in &i.corrections {
        println!("correction:    {k}");
    }
    if i.fuji_rotated {
        println!("note:          Fuji SuperCCD (rotated)");
    }
    let e = &i.exif;
    println!(
        "shot:          ISO {} · {} s · f/{} · {} mm · {}",
        e.iso.map(|v| v.to_string()).unwrap_or("?".into()),
        e.exposure_time.map(|t| if t < 1.0 { format!("1/{:.0}", 1.0 / t) } else { format!("{t:.1}") }).unwrap_or("?".into()),
        e.aperture.map(|v| format!("{v:.1}")).unwrap_or("?".into()),
        e.focal_length.map(|v| format!("{v:.1}")).unwrap_or("?".into()),
        e.capture_date.clone().unwrap_or_default()
    );
    println!(
        "lens:          {} {}{}",
        e.lens_maker.clone().unwrap_or_default(),
        e.lens.clone().unwrap_or("–".into()),
        e.lens_db.as_ref().map(|d| format!("  (database: {d})")).unwrap_or_default()
    );
}

pub struct RoughComparison {
    pub de: f32,
    pub dl: f32,
    pub da: f32,
    pub db: f32,
    pub psnr: f32,
    pub ev: f32,
}

/// Rough comparison: center-crop both images to a common aspect ratio,
/// downscale to a 512 px long edge, Lab means and PSNR.
fn compare(a: &RgbImage, b: &RgbImage) -> RoughComparison {
    let ra = a.width() as f32 / a.height() as f32;
    let rb = b.width() as f32 / b.height() as f32;
    let crop = |img: &RgbImage, r: f32| -> RgbImage {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let (cw, ch) = if w / h > r { (h * r, h) } else { (w, w / r) };
        image::imageops::crop_imm(img, ((w - cw) / 2.0) as u32, ((h - ch) / 2.0) as u32, cw as u32, ch as u32).to_image()
    };
    let r = ra.min(rb); // common aspect ratio: the narrower one
    let (a, b) = (crop(a, r), crop(b, r));
    let tw = 512u32;
    let th = (tw as f32 / r).round() as u32;
    let a = image::imageops::thumbnail(&a, tw, th);
    let b = image::imageops::thumbnail(&b, tw, th);
    let n = (tw * th) as f64;
    let (mut de, mut dl, mut da, mut db, mut se, mut ya, mut yb) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        let la = lab(pa.0);
        let lb = lab(pb.0);
        let d = [la[0] - lb[0], la[1] - lb[1], la[2] - lb[2]];
        de += ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64).sqrt();
        dl += d[0] as f64;
        da += d[1] as f64;
        db += d[2] as f64;
        for c in 0..3 {
            let e = pa.0[c] as f64 - pb.0[c] as f64;
            se += e * e;
        }
        ya += lum(pa.0) as f64;
        yb += lum(pb.0) as f64;
    }
    let mse = se / (n * 3.0);
    RoughComparison {
        de: (de / n) as f32,
        dl: (dl / n) as f32,
        da: (da / n) as f32,
        db: (db / n) as f32,
        psnr: (10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()) as f32,
        ev: ((ya / yb.max(1e-9)).log2()) as f32,
    }
}

fn lin(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn lum(p: [u8; 3]) -> f32 {
    0.2126 * lin(p[0]) + 0.7152 * lin(p[1]) + 0.0722 * lin(p[2])
}

fn lab(p: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (lin(p[0]), lin(p[1]), lin(p[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}
