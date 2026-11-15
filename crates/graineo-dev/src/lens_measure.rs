//! `graineo-dev lens <RAW …> [--png folder] [--area]`: check the lens measurement (`raw::measure`) on real
//! shots: estimate distortion, lateral color (or, with `--area`, vignetting) and compare them with the
//! locally installed LCP profile and lensfun for the same camera/lens combination.
//! Internal check only: the LCP data is not copied, only compared.

use std::path::Path;

use anyhow::{Context, Result};
use raw::development::{Demosaic, Size};
use raw::lcp;
use raw::measure::{self, MeasureImage};
use raw::RawFile;

pub fn run(args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) == Some("white-patch") {
        return flat_field(&args[1..]);
    }
    let area = args.iter().any(|a| a == "--area");
    let self_check = args.iter().any(|a| a == "--self");
    let row = args.iter().any(|a| a == "--row");
    let png = args.windows(2).find(|w| w[0] == "--png").map(|w| w[1].clone());
    // JPEG/PNG are handled without an EXIF reader: --exif="maker|model|lens|focal length|aperture"
    let exif = args.iter().find_map(|a| a.strip_prefix("--exif=")).map(|t| t.split('|').map(str::to_string).collect::<Vec<_>>());
    let mut out_list: Vec<String> = Vec::new();
    if let Some(l) = args.windows(2).find(|w| w[0] == "--list") {
        out_list = std::fs::read_to_string(&l[1])?.lines().filter(|z| !z.trim().is_empty()).map(str::to_string).collect();
    }
    let list_path = args.windows(2).find(|w| w[0] == "--list").map(|w| w[1].clone());
    let mut files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--") && Some(a.as_str()) != png.as_deref() && Some(a.as_str()) != list_path.as_deref()).collect();
    files.extend(out_list.iter());
    if args.iter().any(|a| a == "--correction") {
        return correction(&files);
    }
    if args.iter().any(|a| a == "--bau") {
        return build_profiles(args, exif.as_deref());
    }
    if args.iter().any(|a| a == "--folder") {
        return folder(&files, args.iter().any(|a| a == "--save"), args.iter().any(|a| a == "--check"));
    }
    if args.iter().any(|a| a == "--pool") {
        return pool(&files, exif.as_deref());
    }
    for d in files {
        if let Err(e) = measure_file(d, area, self_check, row, png.as_deref(), exif.as_deref()) {
            println!("{d}: error: {e:#}");
        }
    }
    Ok(())
}

struct Shot {
    maker: String,
    model: String,
    lens: Option<String>,
    focal_length: Option<f32>,
    aperture: Option<f32>,
    distance_m: Option<f32>,
    width: usize,
    height: usize,
    px: Vec<[f32; 3]>,
}

fn load(file: &str, exif: Option<&[String]>) -> Result<Shot> {
    let ext = Path::new(file).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
        let e = exif.context("JPEG/PNG: specify --exif=\"maker|model|lens|focal length|aperture\"")?;
        let img = image::open(file)?.to_rgb8();
        let lin = |v: u8| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let px = img.pixels().map(|p| [lin(p[0]), lin(p[1]), lin(p[2])]).collect();
        return Ok(Shot {
            maker: e.first().cloned().unwrap_or_default(),
            model: e.get(1).cloned().unwrap_or_default(),
            lens: e.get(2).cloned().filter(|s| !s.is_empty()),
            focal_length: e.get(3).and_then(|s| s.parse().ok()),
            aperture: e.get(4).and_then(|s| s.parse().ok()),
            distance_m: None,
            width: img.width() as usize,
            height: img.height() as usize,
            px,
        });
    }
    let rf = RawFile::open(file).with_context(|| format!("open: {file}"))?;
    let base = rf.basis(Demosaic::Auto, false, false, Size::Full).map_err(|e| anyhow::anyhow!("{e}"))?;
    let i = &rf.info;
    Ok(Shot {
        maker: i.maker.clone(),
        model: i.model.clone(),
        lens: i.exif.lens.clone(),
        focal_length: i.exif.focal_length,
        aperture: i.exif.aperture,
        distance_m: i.exif.distance,
        width: base.width,
        height: base.height,
        px: base.px,
    })
}

fn measure_file(file: &str, area: bool, self_check: bool, row: bool, png: Option<&str>, exif: Option<&[String]>) -> Result<()> {
    let t0 = std::time::Instant::now();
    let i = load(file, exif)?;
    let (w, h) = (i.width, i.height);
    let b = MeasureImage { width: w, height: h, px: &i.px };
    let f = i.focal_length.map(f64::from);
    let n = i.aperture.map(f64::from);
    println!(
        "\n{}  {} {} · {} · {} mm f/{} · {}×{}",
        Path::new(file).file_name().and_then(|x| x.to_str()).unwrap_or(file),
        i.maker,
        i.model,
        i.lens.as_deref().unwrap_or("?"),
        f.map_or("?".into(), |v| format!("{v:.1}")),
        n.map_or("?".into(), |v| format!("{v:.1}")),
        w,
        h
    );
    let header = lcp::search(&i.maker, &i.model, i.lens.as_deref(), None);
    let adobe = header.as_ref().and_then(|k| lcp::load(&k.path).map(|p| (k.path.file_name().unwrap().to_string_lossy().to_string(), p)));
    let selection = adobe.as_ref().map(|(_, p)| p.select(i.focal_length, i.aperture, i.distance_m));
    if let Some((name, _)) = &adobe {
        println!("  LCP profile: {name}");
    } else {
        println!("  no LCP profile found");
    }
    let rmax = (w as f64).hypot(h as f64) / 2.0 / w.max(h) as f64;
    let d = w.max(h) as f64;
    if area {
        let m = measure::estimate_vignette(&b).context("no vignetting derivable")?;
        println!("  vignette: a = [{:.4}, {:.4}, {:.4}] residual {:.4} angular spread {:.3} corner {:.3} ({:.2} EV)", m.a[0], m.a[1], m.a[2], m.rest, m.angle_spread, m.corner, m.corner.log2());
        if let Some(v) = selection.as_ref().and_then(|a| a.vig) {
            let adobe_ratio = |a: &[f64; 3], r: f64| 1.0 + r * r * (a[0] + r * r * (a[1] + r * r * a[2]));
            let mut maxd = 0f64;
            for k in 1..=20 {
                let r = k as f64 / 20.0 * rmax;
                let adobe_value = v.falloff(r / v.fx, 0.0);
                maxd = maxd.max((adobe_ratio(&m.a, r) - adobe_value).abs());
            }
            println!("  LCP: corner {:.3}; largest deviation measurement vs. LCP {:.3} ({:.2} EV)", v.falloff(rmax / v.fx, 0.0), maxd, maxd.max(1e-9));
        }
        return Ok(());
    }
    let chains = measure::find_chains(&b, 3200);
    println!("  {} chains ({:.1}s so far)", chains.len(), t0.elapsed().as_secs_f32());
    let v = measure::estimate_distortion(&chains, w, h).map_err(|e| anyhow::anyhow!(e))?;
    println!("  image coverage of the lines: {:.0} %", v.spread * 100.0);
    if row {
        let (mut full, mut eight) = (f64::NAN, f64::NAN);
        if let Some(g) = selection.as_ref().and_then(|a| a.geo) {
            let fa = |r: f64| g.distort(r / g.fx, 0.0).0 * g.fx / r;
            full = (1..=20).map(|k| k as f64 / 20.0 * rmax).map(|r| (measure::ratio_val(&v.k, r) - fa(r)).abs()).fold(0.0, f64::max) * 100.0;
            eight = (1..=16).map(|k| k as f64 / 20.0 * rmax).map(|r| (measure::ratio_val(&v.k, r) - fa(r)).abs()).fold(0.0, f64::max) * 100.0;
        }
        println!("ROW {} | k {:+.4} {:+.4} {:+.4} | chains {} points {} coverage {:.0}% | corner {:+.2}% | dev. LCP {:.2}% (80%: {:.2}%)", Path::new(file).file_name().unwrap().to_string_lossy(), v.k[0], v.k[1], v.k[2], v.chains, v.points, v.spread * 100.0, (measure::ratio_val(&v.k, rmax) - 1.0) * 100.0, full, eight);
        return Ok(());
    }
    println!(
        "  distortion: k = [{:+.5}, {:+.5}, {:+.5}] (order {}), {} chains/{} points, coverage {:.0} %, spread {:.2} → {:.2} px",
        v.k[0], v.k[1], v.k[2], v.order, v.chains, v.points, v.coverage * 100.0, v.rms_before, v.rms_after
    );
    // ratio captured/ideal at some radii (fractions of the half diagonal)
    let mut row = String::from("  r/rmax        ");
    for k in 1..=10 { row.push_str(&format!("{:>7.1}", k as f64 / 10.0)); }
    println!("{row}");
    let show = |name: &str, f: &dyn Fn(f64) -> f64| {
        let mut z = format!("  {name:<14}");
        for k in 1..=10 {
            let r = k as f64 / 10.0 * rmax;
            z.push_str(&format!("{:>+7.2}", (f(r) - 1.0) * 100.0));
        }
        println!("{z}   [% distance captured/ideal − 1]");
    };
    show("measured", &|r| measure::ratio_val(&v.k, r));
    if let Some(g) = selection.as_ref().and_then(|a| a.geo) {
        let f = |r: f64| g.distort(r / g.fx, 0.0).0 * g.fx / r;
        show("LCP", &f);
        let mut maxd = 0f64;
        for k in 1..=20 {
            let r = k as f64 / 20.0 * rmax;
            maxd = maxd.max((measure::ratio_val(&v.k, r) - f(r)).abs());
        }
        println!("  largest deviation measurement–LCP: {:.3} % = {:.2} px at the edge (at 100 % radius {:.2} px)", maxd * 100.0, maxd * rmax * d, ((measure::ratio_val(&v.k, rmax) - f(rmax)).abs()) * rmax * d);
        // also rate measurement vs. LCP without the measurement coverage: up to 80 % of the radius
        let mut m80 = 0f64;
        for k in 1..=16 {
            let r = k as f64 / 20.0 * rmax;
            m80 = m80.max((measure::ratio_val(&v.k, r) - f(r)).abs());
        }
        println!("  up to 80 % radius: {:.3} % = {:.2} px", m80 * 100.0, m80 * 0.8 * rmax * d);
    }
    if let Some((p, name)) = raw::lens::lensfun_profile(&i.maker, &i.model, i.lens.as_deref(), i.focal_length, i.aperture) {
        if p.distortion.is_some() {
            // lensfun: ratio source/target over the target radius in units of the half diagonal, mean slope ≠ 1:
            // convert to mean slope 1 (r' = r·g(0), g'(r') = g(r)/g(0))
            let g0 = p.geometry(1e-4)[1] as f64;
            let f = |r: f64| {
                // r in D; lensfun target radius r_l (half diagonal) with r' = r_l·g0·rmax
                let rl = r / rmax / g0;
                p.geometry(rl as f32)[1] as f64 / g0
            };
            show("lensfun", &f);
            let mut maxd = 0f64;
            for k in 1..=20 {
                let r = k as f64 / 20.0 * rmax;
                maxd = maxd.max((measure::ratio_val(&v.k, r) - f(r)).abs());
            }
            println!("  {name}: largest deviation measurement–lensfun {:.3} % = {:.2} px", maxd * 100.0, maxd * rmax * d);
        }
    }
    if self_check {
        self_test(&b, &v);
    }
    let q = measure::estimate_lateral_ca(&b, &chains);
    if let Some(q) = &q {
        println!("  lateral color: red {:+.5?} (edge {:+.2} px), blue {:+.5?} (edge {:+.2} px), {} points", q.red, q.red_edge, q.blue, q.blue_edge, q.points);
        if let Some((rg, bg)) = selection.as_ref().and_then(|a| a.ca) {
            let s = |m: &lcp::Model, r: f64| m.scale * (m.k[0] * (r / m.fx).powi(2) + m.k[1] * (r / m.fx).powi(4) + m.k[2] * (r / m.fx).powi(6));
            println!("  LCP lateral color at edge: red {:+.2} px, blue {:+.2} px", s(&rg, rmax) * rmax * d, s(&bg, rmax) * rmax * d);
        }
    } else {
        println!("  lateral color: not derivable (too few edges)");
    }
    if let Some(folder) = png {
        std::fs::create_dir_all(folder)?;
        let stem = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("image");
        check_image(&b, &chains, &v.k, &Path::new(folder).join(format!("{stem}-chains.png")))?;
    }
    Ok(())
}

/// Downscaled image with the chains (red: before, green: after undistortion, both drawn).
fn check_image(b: &MeasureImage, chains: &[Vec<[f32; 2]>], k: &[f64; 3], path: &Path) -> Result<()> {
    let n = b.width.max(b.height).div_ceil(1600).max(1);
    let (w, h) = (b.width / n, b.height / n);
    let mut img = image::RgbImage::new(w as u32, h as u32);
    for y in 0..h {
        for x in 0..w {
            let p = b.px[(y * n) * b.width + x * n];
            let g = (p[1].clamp(0.0, 1.0).sqrt() * 160.0) as u8;
            img.put_pixel(x as u32, y as u32, image::Rgb([g, g, g]));
        }
    }
    for chain in chains {
        for p in chain.iter().step_by(2) {
            let (x, y) = ((p[0] / n as f32) as i64, (p[1] / n as f32) as i64);
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                img.put_pixel(x as u32, y as u32, image::Rgb([255, 60, 60]));
            }
            let u = measure::undistort(k, *p, b.width, b.height);
            let (x, y) = ((u[0] / n as f32) as i64, (u[1] / n as f32) as i64);
            if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                img.put_pixel(x as u32, y as u32, image::Rgb([60, 255, 60]));
            }
        }
    }
    img.save(path)?;
    println!("  check image: {}", path.display());
    Ok(())
}

/// Self-test on real image content: the image is given a known distortion; the measurement must recover the
/// composition of the original (measured) and the applied distortion.
fn self_test(b: &MeasureImage, v0: &measure::Distortion) {
    let (w, h) = (b.width, b.height);
    let d = w.max(h) as f64;
    let rmax = (w as f64).hypot(h as f64) / 2.0 / d;
    for applied in [[-0.06, 0.0, 0.0], [-0.10, 0.04, 0.0], [0.05, 0.0, 0.0]] {
        let px = measure::distort_img(b, &applied);
        let b2 = MeasureImage { width: w, height: h, px: &px };
        let chains = measure::find_chains(&b2, 3200);
        match measure::estimate_distortion(&chains, w, h) {
            Ok(v1) => {
                // expected: ideal ρ → original r0 = F0(ρ) → distorted r1 = F_K(r0)
                let expected_ratio = |rho: f64| {
                    let r0 = rho * measure::ratio_val(&v0.k, rho);
                    r0 * measure::ratio_val(&applied, r0) / rho
                };
                let mut maxd = 0f64;
                for k in 1..=20 {
                    let rho = k as f64 / 20.0 * rmax;
                    maxd = maxd.max((measure::ratio_val(&v1.k, rho) - expected_ratio(rho)).abs());
                }
                println!(
                    "  self-test applied k={:?}: measured {:+.4?} ({} chains), deviation from expectation {:.3} % = {:.1} px at the edge",
                    applied, v1.k, v1.chains, maxd * 100.0, maxd * rmax * d
                );
            }
            Err(e) => println!("  self-test applied k={applied:?}: {e}"),
        }
    }
}

/// `--pool`: fit the chains of all shots (same camera/focal length/image size) together.
fn pool(files: &[&String], exif: Option<&[String]>) -> Result<()> {
    let mut per_image: Vec<(String, Vec<Vec<[f32; 2]>>)> = Vec::new();
    let mut first: Option<Shot> = None;
    let (mut w, mut h) = (0, 0);
    for d in files {
        let a = match load(d, exif) {
            Ok(a) => a,
            Err(e) => {
                println!("{d}: {e:#}");
                continue;
            }
        };
        if w != 0 && (a.width, a.height) != (w, h) {
            continue;
        }
        (w, h) = (a.width, a.height);
        let b = MeasureImage { width: w, height: h, px: &a.px };
        let k = measure::find_chains(&b, 3200);
        eprintln!("  {}: {} chains", Path::new(d.as_str()).file_name().unwrap().to_string_lossy(), k.len());
        per_image.push((d.to_string(), k));
        if first.is_none() {
            first = Some(Shot { px: Vec::new(), ..a });
        }
    }
    let i = first.context("no shot")?;
    let (v, used, rejected) = raw::measure_run::estimate_jointly(&per_image, w, h);
    let v = v.map_err(|e| anyhow::anyhow!(e))?;
    println!("{} of {} shots used, {} discarded", used.len(), per_image.len(), rejected.len());
    let rmax = (w as f64).hypot(h as f64) / 2.0 / w.max(h) as f64;
    let d = w.max(h) as f64;
    println!("pool: k = {:+.5?} (order {}), {} chains/{} points, coverage {:.0} %, spread {:.2} → {:.2} px", v.k, v.order, v.chains, v.points, v.spread * 100.0, v.rms_before, v.rms_after);
    let header = lcp::search(&i.maker, &i.model, i.lens.as_deref(), None);
    let adobe = header.as_ref().and_then(|k| lcp::load(&k.path));
    let selection = adobe.as_ref().map(|p| p.select(i.focal_length, i.aperture, i.distance_m));
    let mut z1 = String::from("  r/rmax   ");
    let mut z2 = String::from("  measured ");
    let mut z3 = String::from("  LCP      ");
    let mut maxd = 0f64;
    for k in 1..=10 {
        let r = k as f64 / 10.0 * rmax;
        z1.push_str(&format!("{:>7.1}", k as f64 / 10.0));
        z2.push_str(&format!("{:>+7.2}", (measure::ratio_val(&v.k, r) - 1.0) * 100.0));
        if let Some(g) = selection.as_ref().and_then(|a| a.geo) {
            let f = g.distort(r / g.fx, 0.0).0 * g.fx / r;
            z3.push_str(&format!("{:>+7.2}", (f - 1.0) * 100.0));
            maxd = maxd.max((measure::ratio_val(&v.k, r) - f).abs());
        }
    }
    println!("{z1}\n{z2}\n{z3}");
    if selection.is_some() {
        println!("  largest deviation measurement–LCP: {:.3} % = {:.1} px at the edge", maxd * 100.0, maxd * rmax * d);
    }
    Ok(())
}

/// `--folder <folder> [--save] [--check]`: the whole procedure as in the assistant (`raw::measure_run`);
/// `--save` creates the own profile (target folder via `GRAINEO_LENS_BACKUP`), `--check` develops a grid image
/// per focal length with this profile and measures the distortion of the result once more.
fn folder(inputs: &[&String], save: bool, check: bool) -> Result<()> {
    use raw::measure_run as run;
    let dir = inputs.first().context("specify a folder")?;
    let files = run::files_in_folder(Path::new(dir.as_str()));
    println!("{} RAW files", files.len());
    let abort = std::sync::atomic::AtomicBool::new(false);
    // X3F through the own Sigma chain, everything else through rawler
    let e = run::measure_with(&files, &rawdev::lens_shots::AnySource, &run::MeasureHints::default(), &|i, n, name| if !name.is_empty() { eprintln!("  [{}/{}] {}", i + 1, n, name) }, &abort).map_err(|e| anyhow::anyhow!(e))?;
    println!("{} {} · {} (crop factor {:.2}{})", e.maker, e.model, e.lens.as_deref().unwrap_or("?"), e.crop, if e.crop_known { "" } else { ", assumed" });
    for a in &e.shots {
        println!("  {:<18} {:>6} f/{:<4} {:?}", a.name, a.focal_length.map_or("?".into(), |f| format!("{f:.1}mm")), a.aperture.map_or("?".into(), |f| format!("{f:.1}")), a.role);
    }
    for g in &e.geo {
        match &g.distortion {
            Ok(v) => println!("  geometry {}: k = {:+.5?} order {}, {} chains/{} points, coverage {:.0}/{:.0} %, spread {:.2} → {:.2} px; lateral color {}", run::focal_length_text(g.focal_length), v.k, v.order, v.chains, v.points, v.coverage * 100.0, v.spread * 100.0, v.rms_before, v.rms_after, g.lateral_ca.as_ref().map_or("–".into(), |q| format!("red {:+.2} px, blue {:+.2} px at the edge", q.red_edge, q.blue_edge))),
            Err(m) => println!("  geometry {}: {m}", run::focal_length_text(g.focal_length)),
        }
    }
    for g in &e.vig {
        match &g.vignette {
            Ok(v) => println!("  vignette {} f/{}: corner {:.2} ({:.2} EV), residual {:.3}", run::focal_length_text(g.focal_length), g.aperture.map_or("?".into(), |b| format!("{b:.1}")), v.corner, v.corner.log2(), v.rest),
            Err(m) => println!("  vignette {}: {m}", run::focal_length_text(g.focal_length)),
        }
    }
    for g in &e.cast {
        match &g.cast {
            Ok(c) => println!("  colour cast {} f/{}: corner {:?}, quality {}", run::focal_length_text(g.focal_length), g.aperture.map_or("?".into(), |b| format!("{b:.1}")), c.quality.corner, c.quality.score),
            Err(m) => println!("  colour cast {}: {m}", run::focal_length_text(g.focal_length)),
        }
    }
    for h in &e.notes {
        println!("  note: {h}");
    }
    if save {
        let p = run::save(&e)?;
        println!("  profile saved: {}", p.display());
    }
    if check {
        for g in e.geo.iter().filter(|g| g.distortion.is_ok()) {
            let name = &g.images[0];
            let path = files.iter().find(|p| p.file_name().and_then(|n| n.to_str()) == Some(name.as_str())).unwrap();
            let rf = RawFile::open(path)?;
            let o = raw::development::Options { lens: Some(raw::lens::LensOptions { lensfun: false, adobe: false, ..Default::default() }), ..Default::default() };
            let r = rf.develop(&o).map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("  check {name}: correction: {:?} ({:?})", r.lens, r.lens_source);
            let img = r.img;
            let (w, h) = (img.width() as usize, img.height() as usize);
            let lin = |v: u8| { let c = v as f32 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
            let px: Vec<[f32; 3]> = img.pixels().map(|p| [lin(p[0]), lin(p[1]), lin(p[2])]).collect();
            let b = MeasureImage { width: w, height: h, px: &px };
            let chains = measure::find_chains(&b, 3200);
            let rmax = (w as f64).hypot(h as f64) / 2.0 / w.max(h) as f64;
            match measure::estimate_distortion(&chains, w, h) {
                Ok(v) => println!("  check {name}: after correction k = {:+.5?}, corner {:+.2} %, spread {:.2} px", v.k, (measure::ratio_val(&v.k, rmax) - 1.0) * 100.0, v.rms_after),
                Err(m) => println!("  check {name}: {m}"),
            }
        }
    }
    Ok(())
}

/// `--build_profiles <output_folder> --band <focal length>:<list> … --maker H --models "A;B" --lens O [--exif=…]`:
/// for each focal length band, evaluate the chains of many existing shots together (with an agreement check),
/// build a profile and store it as a separate profile (JSON) for each camera model. Distortion and lateral color only.
fn build_profiles(args: &[String], exif: Option<&[String]>) -> Result<()> {
    use raw::measure::GeoMeasurement;
    let value = |name: &str| args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone());
    let out = value("--bau").context("--bau <folder>")?;
    let maker = value("--maker").context("--maker")?;
    let lens = value("--lens").context("--lens")?;
    let models: Vec<String> = value("--models").context("--models")?.split(';').map(str::to_string).collect();
    let mut geo: Vec<GeoMeasurement> = Vec::new();
    for b in args.windows(2).filter(|w| w[0] == "--band") {
        let (f, list) = b[1].split_once(':').context("--band F:list")?;
        let f: f64 = f.parse()?;
        let files: Vec<String> = std::fs::read_to_string(list)?.lines().filter(|z| !z.trim().is_empty()).map(str::to_string).collect();
        let mut per_image: Vec<(String, Vec<Vec<[f32; 2]>>)> = Vec::new();
        let (mut w, mut h) = (0, 0);
        let mut ca: Vec<raw::measure::LateralCa> = Vec::new();
        for d in &files {
            let Ok(a) = load(d, exif) else { continue };
            if w != 0 && (a.width, a.height) != (w, h) {
                continue;
            }
            (w, h) = (a.width, a.height);
            let m = MeasureImage { width: w, height: h, px: &a.px };
            let k = measure::find_chains(&m, 3200);
            if k.len() >= 60 && ca.len() < 8 {
                if let Some(q) = measure::estimate_lateral_ca(&m, &k) {
                    ca.push(q);
                }
            }
            per_image.push((d.clone(), k));
        }
        let (v, used, rejected) = raw::measure_run::estimate_jointly(&per_image, w, h);
        match v {
            Ok(v) => {
                println!("band {f} mm: {} of {} shots, k = {:+.5?} (order {}), {} chains, coverage {:.0} %, spread {:.2} → {:.2} px, {} discarded", used.len(), per_image.len(), v.k, v.order, v.chains, v.spread * 100.0, v.rms_before, v.rms_after, rejected.len());
                let q = (!ca.is_empty()).then(|| {
                    let n = ca.len() as f64;
                    let mut q = raw::measure::LateralCa { red: [0.0; 3], blue: [0.0; 3], red_edge: 0.0, blue_edge: 0.0, points: 0 };
                    for c in &ca {
                        for j in 0..3 {
                            q.red[j] += c.red[j] / n;
                            q.blue[j] += c.blue[j] / n;
                        }
                        q.red_edge += c.red_edge / n;
                        q.blue_edge += c.blue_edge / n;
                    }
                    q
                });
                if let Some(q) = &q {
                    println!("    lateral color at the edge: red {:+.2} px, blue {:+.2} px (from {} shots)", q.red_edge, q.blue_edge, ca.len());
                }
                geo.push(GeoMeasurement { focal_length: f, aperture: 8.0, distortion: v, lateral_ca: q });
            }
            Err(e) => println!("band {f} mm: {e}"),
        }
    }
    anyhow::ensure!(!geo.is_empty(), "no band evaluated");
    std::fs::create_dir_all(&out)?;
    for m in &models {
        let profile = measure::build_profile(&format!("{maker} {m} {lens} (graineo-Messung aus vorhandenen Aufnahmen)"), 1.0, |f| f / 36.0, &geo, &[]);
        let s = raw::lcp::Backup { format: 1, maker: maker.clone(), model: m.clone(), lens: lens.clone(), origin: "graineo-Messung (vorhandene Aufnahmen)".into(), profile: profile };
        let name: String = format!("{maker} {m} {lens}").chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        let path = Path::new(&out).join(format!("{}.json", name.trim_matches('_')));
        std::fs::write(&path, serde_json::to_vec_pretty(&s)?)?;
        println!("written: {}", path.display());
    }
    Ok(())
}

/// `--correction <RAW …>`: for each shot, measure the distortion before and after the lens correction (with the
/// own profile just installed or given via `GRAINEO_LENS_BACKUP`).
fn correction(files: &[&String]) -> Result<()> {
    let rmax_of = |w: usize, h: usize| (w as f64).hypot(h as f64) / 2.0 / w.max(h) as f64;
    for d in files {
        let Ok(a) = load(d, None) else { continue };
        let (w, h) = (a.width, a.height);
        let before = {
            let m = MeasureImage { width: w, height: h, px: &a.px };
            let k = measure::find_chains(&m, 3200);
            measure::estimate_distortion(&k, w, h)
        };
        let rf = RawFile::open(d.as_str())?;
        let o = raw::development::Options { lens: Some(raw::lens::LensOptions { lensfun: false, adobe: false, ..Default::default() }), ..Default::default() };
        let r = rf.develop(&o).map_err(|e| anyhow::anyhow!("{e}"))?;
        let img = r.img;
        let (w2, h2) = (img.width() as usize, img.height() as usize);
        let lin = |v: u8| { let c = v as f32 / 255.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
        let px: Vec<[f32; 3]> = img.pixels().map(|p| [lin(p[0]), lin(p[1]), lin(p[2])]).collect();
        let m2 = MeasureImage { width: w2, height: h2, px: &px };
        let k2 = measure::find_chains(&m2, 3200);
        let after = measure::estimate_distortion(&k2, w2, h2);
        let corner = |v: &Result<measure::Distortion, String>, w, h| match v { Ok(v) => format!("{:+.2} % (spread {:.2} px)", (measure::ratio_val(&v.k, rmax_of(w, h)) - 1.0) * 100.0, v.rms_after), Err(e) => format!("– ({})", e.chars().take(40).collect::<String>()) };
        println!("{}  {:?}\n    before corner {}   after corner {}", Path::new(d.as_str()).file_name().unwrap().to_string_lossy(), r.lens_source, corner(&before, w, h), corner(&after, w2, h2));
    }
    Ok(())
}

/// `objektiv weissfeld <A> [<B>] [--png folder] [--force]`: flat-field measurement per channel/layer
/// (`lensprof::flatfield`), B = second shot with the camera turned by 180°. X3F through the own chain.
/// Prints the check of each shot (level, clipped, edges), vignetting and colour cast in the corners, quality;
/// `--png` writes the maps per channel (±5 % on grey: red = more, blue = less than in the centre).
/// `--force` measures also shots that fail the check (plausibility only: no real flat field).
fn flat_field(args: &[String]) -> Result<()> {
    use lensprof::flatfield;
    use raw::measure_run::ShotSource;
    let png = args.windows(2).find(|w| w[0] == "--png").map(|w| w[1].clone());
    if args.iter().any(|a| a == "--synthetic") {
        return flat_synthetic(png.as_deref());
    }
    let force = args.iter().any(|a| a == "--force");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--") && Some(a.as_str()) != png.as_deref()).collect();
    anyhow::ensure!(!files.is_empty() && files.len() <= 2, "one or two files (A, B rotated by 180°)");
    let mut blocks = Vec::new();
    let mut checks = Vec::new();
    let mut channels = ["R", "G", "B"];
    for f in &files {
        let path = Path::new(f.as_str());
        let x3f = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("x3f"));
        if x3f {
            channels = ["top", "middle", "bottom"];
            let props = rawdev::lens_shots::x3f_props(path).map_err(|e| anyhow::anyhow!(e))?;
            println!("{f}: PROP {}", props.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "));
        }
        let t = std::time::Instant::now();
        let s = rawdev::lens_shots::AnySource.open(path).map_err(|e| anyhow::anyhow!(e))?;
        println!("{f}: {} {} · {} · {} mm f/{} · {}×{} · {:?} · saturation {:.3} ({:.1}s)", s.maker, s.model, s.lens.as_deref().unwrap_or("no lens name"), s.focal_length.map_or("?".into(), |v| format!("{v}")), s.aperture.map_or("?".into(), |v| format!("{v}")), s.width, s.height, s.sensor, s.saturation, t.elapsed().as_secs_f32());
        let b = flatfield::blocks(&flatfield::FlatShot { width: s.width, height: s.height, px: &s.px, saturation: s.saturation });
        let c = flatfield::check_blocks(&b);
        println!("  check: level {:.1} % · clipped {:.3} % · edges {:.3} → {}", b.level * 100.0, b.clipped * 100.0, b.edges, match &c { Ok(()) => "usable".to_string(), Err(e) => format!("NOT usable: {e}") });
        checks.push(c);
        blocks.push(b);
    }
    if checks.iter().any(|c| c.is_err()) && !force {
        println!("not measured (--force measures anyway, plausibility only)");
        return Ok(());
    }
    if checks.iter().any(|c| c.is_err()) {
        println!("PLAUSIBILITY ONLY – not a real flat field");
    }
    let r = flatfield::measure_blocks(&blocks[0], blocks.get(1)).map_err(|e| anyhow::anyhow!(e))?;
    let (w, h) = (blocks[0].width as f64, blocks[0].height as f64);
    let rc = w.hypot(h) / 2.0 / w.max(h);
    let v = &r.vignette;
    let vc = 1.0 + rc * rc * (v[0] + rc * rc * (v[1] + rc * rc * v[2]));
    println!("  vignetting a = {:+.4?}, corner {:.3} ({:+.2} EV)", v, vc, vc.log2());
    let q = &r.quality;
    println!(
        "  colour cast corner: {} · fit rms {:?} · pair rms {} · dust blocks {} · quality {} ({})",
        (0..3).map(|k| format!("{} {:+.1} %", channels[k], (q.corner[k] - 1.0) * 100.0)).collect::<Vec<_>>().join(", "),
        q.fit_rms,
        q.pair_rms.map_or("– (single shot, quality ≤ 70)".into(), |p| format!("{p:.4}")),
        q.dust_cells,
        q.score,
        lensprof::quality::Level::of(q.score).text()
    );
    if let Some(dir) = png {
        let stem = Path::new(files[0].as_str()).file_stem().and_then(|s| s.to_str()).unwrap_or("flat").to_string();
        write_maps(&r.cast, &dir, &stem, channels)?;
    }
    Ok(())
}

/// Colour-cast maps per channel as PNG (±5 % on grey: red = more, blue = less than in the centre; one cell = 12 px).
fn write_maps(g: &lensprof::cast::CastGrid, dir: &str, stem: &str, channels: [&str; 3]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    const CELL: u32 = 12;
    for k in 0..3 {
        let mut img = image::RgbImage::new(g.cols as u32 * CELL, g.rows as u32 * CELL);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let gv = g.gain[(y / CELL) as usize * g.cols as usize + (x / CELL) as usize][k];
            let t = ((gv - 1.0) / 0.05).clamp(-1.0, 1.0);
            let end = if t >= 0.0 { [220.0, 40.0, 30.0] } else { [30.0, 70.0, 230.0] };
            *p = image::Rgb(std::array::from_fn(|c| (128.0 + (end[c] - 128.0) * t.abs()) as u8));
        }
        let out = Path::new(dir).join(format!("{stem}_cast_{}.png", channels[k]));
        img.save(&out)?;
        println!("  written: {}", out.display());
    }
    Ok(())
}

/// `objektiv weissfeld --synthetic [--png folder]`: pair at sd Quattro size (5424×3616) with known vignetting
/// (corner 0.60), colour cast (top layer +4 %, bottom −5 % in the corner, slightly off-centre), lighting gradient
/// ±8 % mirrored in the second shot, 1 % noise and 20 dust spots; prints measured against true values and the time.
fn flat_synthetic(png: Option<&str>) -> Result<()> {
    use lensprof::flatfield::{self, FlatShot};
    const W: usize = 5424;
    const H: usize = 3616;
    let rc = (W as f64).hypot(H as f64) / 2.0 / W as f64;
    let truth = |u: f64, v: f64, r: f64| -> [f64; 3] {
        let s = (r / rc).powi(2);
        [1.0 + 0.04 * s * (1.0 + 0.2 * u), 1.0 + 0.005 * v, 1.0 - 0.05 * s]
    };
    let make = |tilt: f64, seed0: u64| -> Vec<[f32; 3]> {
        let mut seed = seed0;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let spots: Vec<(f64, f64)> = (0..20).map(|_| (rnd() * W as f64, rnd() * H as f64)).collect();
        let mut px = vec![[0f32; 3]; W * H];
        for y in 0..H {
            for x in 0..W {
                let (xc, yc) = (x as f64 + 0.5, y as f64 + 0.5);
                let (u, v) = (xc / W as f64 * 2.0 - 1.0, yc / H as f64 * 2.0 - 1.0);
                let r = (xc - W as f64 / 2.0).hypot(yc - H as f64 / 2.0) / W as f64;
                let mut base = 0.4 * (1.0 - 0.4 * (r / rc).powi(2)) * (1.0 + tilt * u);
                for &(sx, sy) in &spots {
                    let d2 = (xc - sx).powi(2) + (yc - sy).powi(2);
                    if d2 < 40_000.0 {
                        base *= 1.0 - 0.15 * (-d2 / (2.0 * 2500.0)).exp();
                    }
                }
                let c = truth(u, v, r);
                px[y * W + x] = std::array::from_fn(|k| {
                    let (a, b) = (rnd().max(1e-12), rnd());
                    let n = (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos();
                    (base * c[k] * (1.0 + 0.01 * n)) as f32
                });
            }
        }
        px
    };
    let (a, b) = (make(0.08, 7), make(-0.08, 11));
    let t = std::time::Instant::now();
    let ba = flatfield::blocks(&FlatShot { width: W, height: H, px: &a, saturation: 1.0 });
    let bb = flatfield::blocks(&FlatShot { width: W, height: H, px: &b, saturation: 1.0 });
    let t_blocks = t.elapsed();
    println!("check A: {:?}, B: {:?}", flatfield::check_blocks(&ba), flatfield::check_blocks(&bb));
    let r = flatfield::measure_blocks(&ba, Some(&bb)).map_err(|e| anyhow::anyhow!(e))?;
    println!("time: blocks {:.2} s for two shots, fit {:.3} s", t_blocks.as_secs_f32(), (t.elapsed() - t_blocks).as_secs_f32());
    let g = &r.cast;
    let mut worst = [0f64; 3];
    for j in 0..g.rows as usize {
        for i in 0..g.cols as usize {
            let (fu, fv) = ((i as f64 + 0.5) / g.cols as f64, (j as f64 + 0.5) / g.rows as f64);
            let rr = ((fu - 0.5) * W as f64).hypot((fv - 0.5) * H as f64) / W as f64;
            let tv = truth(fu * 2.0 - 1.0, fv * 2.0 - 1.0, rr);
            let c0 = truth(0.0, 0.0, 0.0);
            for k in 0..3 {
                worst[k] = worst[k].max((g.gain[j * g.cols as usize + i][k] as f64 / (tv[k] / c0[k]) - 1.0).abs());
            }
        }
    }
    let v = &r.vignette;
    let vc = 1.0 + rc * rc * (v[0] + rc * rc * (v[1] + rc * rc * v[2]));
    println!("vignetting corner {vc:.4} (true 0.6000)");
    println!("largest error of the map: top {:.3} %, middle {:.3} %, bottom {:.3} %", worst[0] * 100.0, worst[1] * 100.0, worst[2] * 100.0);
    let q = &r.quality;
    println!("corner measured: top {:+.2} %, middle {:+.2} %, bottom {:+.2} % · pair rms {:?} · dust blocks {} · quality {} ({})", (q.corner[0] - 1.0) * 100.0, (q.corner[1] - 1.0) * 100.0, (q.corner[2] - 1.0) * 100.0, q.pair_rms, q.dust_cells, q.score, lensprof::quality::Level::of(q.score).text());
    if let Some(dir) = png {
        write_maps(g, dir, "synthetic_sdq", ["top", "middle", "bottom"])?;
    }
    Ok(())
}
