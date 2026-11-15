//! `graineo-dev film`: film simulation (spektrafilm) on the command line.
//!
//! ```text
//! graineo-dev film --list
//! graineo-dev film IMAGE.X3F out.jpg|tif --film <id> [--paper <id>] [--negative] [--ev <EV>]
//!        [--print-ev <EV>] [--warmth <−1…1>] [--tint <−1…1>] [--grain <0…2>] [--grain-size <x>]
//!        [--halation <x>] [--halation-radius <x>] [--couplers <x>] [--film-gamma <x>]
//!        [--paper-gamma <x>] [--format <mm>] [--sigma-ev <EV>] [--mode <color mode>] [--amount <0…1>] [--mono]
//!        [--nr off] [--cpu] [--region x,y,w,h] [--preview <edge>] [--bench] [--sheet <out.jpg>]
//!        [--filter <id>] [--schichten an|aus] [--korn-ausgleich an|aus] [--wb <preset>]
//! ```
//! `--bench` measures the preview (2400 px as in the app), a detail crop (1200×900), the full export
//! (GPU f32 and CPU) and the image grid of all films (256 px). `--sheet` writes the image grid.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use develop::film::{self, FilmSettings};
use develop::session::Preview;
use develop::{Session, Settings};

pub fn run(args: &[String]) -> Result<()> {
    if args.first().is_some_and(|a| a == "--list") {
        list();
        return Ok(());
    }
    let (file, out) = match (args.first(), args.get(1)) {
        (Some(f), Some(o)) if !o.starts_with("--") => (f.as_str(), Some(o.as_str())),
        (Some(f), _) => (f.as_str(), None),
        _ => bail!("usage: graineo-dev film IMAGE.X3F out.jpg|tif --film <id> … or graineo-dev film --list"),
    };
    let rest = &args[if out.is_some() { 2 } else { 1 }..];
    let mut s = Settings::default();
    s.film = FilmSettings { active: true, ..FilmSettings::default() };
    let (mut region, mut preview, mut bench, mut sheet) = (None::<[usize; 4]>, None::<usize>, false, None::<String>);
    let mut compare = false;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (a, None),
        };
        let flag = matches!(key, "--negative" | "--mono" | "--cpu" | "--bench" | "--off" | "--compare");
        let val = if flag {
            String::new()
        } else if let Some(v) = inline {
            v
        } else {
            i += 1;
            rest.get(i).cloned().with_context(|| format!("value missing for {key}"))?
        };
        let num = || -> Result<f32> { val.parse().with_context(|| format!("number expected: {key} {val}")) };
        let f = &mut s.film;
        match key {
            "--film" => f.stock = val.clone(),
            "--paper" => f.paper = Some(val.clone()),
            "--negative" => f.negative = true,
            "--off" => f.active = false,
            "--ev" => f.exposure = num()?,
            "--print-ev" => f.print_exposure = num()?,
            "--warmth" => f.print_warmth = num()?,
            "--tint" => f.print_tint = num()?,
            "--grain" => f.grain = num()?,
            "--grain-size" => f.grain_size = num()?,
            "--grain-model" => f.grain_model = val.clone(),
            "--halation" => f.halation = num()?,
            "--halation-radius" => f.halation_radius = num()?,
            "--couplers" => f.couplers = num()?,
            "--film-gamma" => f.film_gamma = num()?,
            "--paper-gamma" => f.paper_gamma = num()?,
            "--filter" => f.taking_filter = val.clone(),
            "--push" => f.push = num()?,
            "--paper-tone" => f.paper_tone = num()?,
            "--paper-matte" => f.paper_matte = num()?,
            "--format" => f.format_mm = num()?,
            // Sigma-Analog (Merrill, B&W film): exposure from the layers and grain balance, an|aus
            "--layers" => f.layer_exposure = val == "an" || val == "on" || val == "1",
            "--grain-compensation" => f.grain_balance = val == "an" || val == "on" || val == "1",
            "--wb" => s.white_balance = Some(val.clone()),
            "--sigma-ev" => s.exposure = num()? as f64,
            "--mode" => s.color_mode = Some(val.clone()),
            "--amount" => s.profile_amount = num()?,
            "--mono" => s.monochrome = true,
            "--nr" => s.denoise = !(val == "off" || val == "0"),
            "--cpu" => film::set_backend(film::Backend::Cpu),
            "--region" => {
                let v: Vec<usize> = val.split(',').map(str::parse).collect::<Result<_, _>>()?;
                anyhow::ensure!(v.len() == 4, "--region x,y,w,h");
                region = Some([v[0], v[1], v[2], v[3]]);
            }
            "--preview" => preview = Some(num()? as usize),
            "--bench" => bench = true,
            "--compare" => compare = true,
            "--sheet" => sheet = Some(val.clone()),
            _ => bail!("unknown option {a}"),
        }
        i += 1;
    }
    if s.film.active && film::stock(&s.film.stock).is_none() {
        bail!("unknown film: {} (graineo-dev film --list)", s.film.stock);
    }
    let t = Instant::now();
    let mut session = Session::open(file)?;
    if Session::uses_spp(&s) {
        session.ensure_spp(&s).map_err(anyhow::Error::msg)?;
    }
    eprintln!("{} opened, base ready in {:.2?} · film computation: {}{}", session.cal.camera, t.elapsed(), film::backend_name(), if film::F64 { " (f64)" } else { " (f32)" });
    if bench {
        return run_bench(&mut session, &s);
    }
    if compare {
        return run_compare(&mut session, &s);
    }
    if let Some(path) = sheet {
        return write_sheet(&session, &s, &path);
    }
    let out = out.context("output file missing")?;
    let t = Instant::now();
    let img = if let Some(r) = region {
        session.render_region(&s, r)
    } else if let Some(edge) = preview {
        preview_of(&session, &s, edge).render(&session.cal, &s)
    } else {
        develop::geometry::apply(session.render_full(&s), &s, session.cal.orientation)
    };
    eprintln!("developed {}×{} in {:.2?}", img.width, img.height, t.elapsed());
    write(&img, out)
}

fn write(img: &develop::Rgb16, out: &str) -> Result<()> {
    let path = std::path::Path::new(out);
    if out.ends_with(".tif") || out.ends_with(".tiff") {
        develop::export::write_tiff16(img, path)?;
    } else {
        develop::export::write_jpeg(img, path, 92)?;
    }
    Ok(())
}

fn list() {
    let Some(dir) = film::data_dir() else {
        println!("film data not found");
        return;
    };
    println!("data: {}", dir.display());
    println!("\nfilms:");
    for f in film::films() {
        let fs = FilmSettings { active: true, stock: f.id.clone(), ..FilmSettings::default() };
        let g = film::gray_response(&fs).map(|g| format!("{:.3} {:.3} {:.3}", g[0], g[1], g[2])).unwrap_or_else(|e| e);
        println!("  {:<28} {:<36} {:<12} paper: {:<34} mid gray: {g}", f.id, f.name, f.category.label(), f.default_paper.as_deref().unwrap_or("– (scanned directly)"));
    }
    println!("\npapers:");
    for p in film::papers() {
        println!("  {:<28} {:<36} {}", p.id, p.name, p.category.label());
    }
}

/// Preview as in the app (SPP base downscaled or own pipeline).
fn preview_of(session: &Session, s: &Settings, edge: usize) -> Preview {
    match (session.spp_base(), session.spp_matrix()) {
        (Some(b), Some(m)) if Session::uses_spp(s) => Preview::from_spp(&b, m, edge),
        _ => {
            let (small, f) = session.preview_linear(edge);
            Preview::developed(small, f, &session.cal, s)
        }
    }
}

fn time<T>(label: &str, n: usize, mut f: impl FnMut() -> T) -> T {
    let mut out = None;
    let mut times = Vec::new();
    for _ in 0..n {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let rest = if times.len() > 1 { format!(", then {:.0} ms", times[1..].iter().sum::<f64>() / (times.len() - 1) as f64) } else { String::new() };
    eprintln!("{label}: first run {:.0} ms{rest}", times[0]);
    out.unwrap()
}

fn run_bench(session: &mut Session, s: &Settings) -> Result<()> {
    let [w, h] = session.size();
    let p = preview_of(session, s, 2400);
    let cal = session.cal.clone();
    let mut off = s.clone();
    off.film.active = false;
    let img = time("preview without film (Sigma tone curve)", 3, || p.render(&cal, &off));
    eprintln!("  preview size {}×{}", img.width, img.height);
    time(&format!("preview with film ({})", film::backend_name()), 4, || p.render(&cal, s));
    let mut k = 0;
    time("preview, warmth changed (new calibration)", 3, || {
        k += 1;
        let mut t = s.clone();
        t.film.print_warmth = 0.1 * k as f32;
        p.render(&cal, &t)
    });
    time("preview, film exposure changed (calibration + neutral balance)", 3, || {
        k += 1;
        let mut t = s.clone();
        t.film.exposure = 0.1 * k as f32;
        p.render(&cal, &t)
    });
    let r = [w / 2 - 600, h / 2 - 450, 1200, 900];
    time("detail 1200×900 without film", 3, || session.render_region(&off, r));
    time(&format!("detail 1200×900 with film ({})", film::backend_name()), 3, || session.render_region(s, r));
    time(&format!("export {w}×{h} with film ({})", film::backend_name()), 2, || session.render_full(s));
    let (scene, geo) = p.film_scene(&cal, s, 256);
    eprintln!("  image grid: {} films, {}×{}", film::films().len(), scene.width, scene.height);
    time("image grid of all films", 2, || film::thumbnails(&scene, &s.film, geo, None));
    film::set_backend(film::Backend::Cpu);
    time("preview with film (CPU)", 2, || p.render(&cal, s));
    time("detail 1200×900 with film (CPU)", 2, || session.render_region(s, r));
    time(&format!("export {w}×{h} with film (CPU, {})", if film::F64 { "f64" } else { "f32" }), 1, || session.render_full(s));
    time("image grid of all films (CPU)", 1, || film::thumbnails(&scene, &s.film, geo, None));
    Ok(())
}

/// Image grid of all films (6 columns, order as in `--list`) as JPEG.
fn write_sheet(session: &Session, s: &Settings, path: &str) -> Result<()> {
    let p = preview_of(session, s, 2400);
    let (scene, geo) = p.film_scene(&session.cal, s, 256);
    let t = Instant::now();
    let thumbs = film::thumbnails(&scene, &s.film, geo, None);
    eprintln!("{} films in {:.2?}", thumbs.len(), t.elapsed());
    let (tw, th) = (scene.width, scene.height);
    let cols = 6;
    let rows = thumbs.len().div_ceil(cols);
    let (gw, gh) = (cols * (tw + 4), rows * (th + 4));
    let mut data = vec![0u16; gw * gh * 3];
    for (i, t) in thumbs.iter().enumerate() {
        let (cx, cy) = ((i % cols) * (tw + 4) + 2, (i / cols) * (th + 4) + 2);
        match &t.image {
            Ok(img) => {
                for y in 0..th {
                    let src = &img.data[y * tw * 3..(y + 1) * tw * 3];
                    data[((cy + y) * gw + cx) * 3..((cy + y) * gw + cx + tw) * 3].copy_from_slice(src);
                }
            }
            Err(e) => eprintln!("{}: {e}", t.stock.id),
        }
        eprintln!("{:2}: {} ({})", i, t.stock.name, t.stock.category.label());
    }
    write(&develop::Rgb16 { width: gw, height: gh, data }, path)
}

/// Preview (2400 px, as in the app) against the downscaled full export, and detail crop
/// against the same area of the export: mean deviation per channel (8-bit steps).
fn run_compare(session: &mut Session, s: &Settings) -> Result<()> {
    let p = preview_of(session, s, 2400);
    let cal = session.cal.clone();
    let prev = p.render(&cal, s);
    let full = session.render_full(s);
    let f = full.width / prev.width;
    let stat = |a: &develop::Rgb16, b: &dyn Fn(usize, usize, usize) -> f64| {
        let (mut sum, mut abs) = ([0f64; 3], [0f64; 3]);
        for y in 0..a.height {
            for x in 0..a.width {
                for c in 0..3 {
                    let d = (a.data[(y * a.width + x) * 3 + c] as f64 - b(x, y, c)) / 257.0;
                    sum[c] += d;
                    abs[c] += d.abs();
                }
            }
        }
        let n = (a.width * a.height) as f64;
        (sum.map(|v| v / n), abs.map(|v| v / n))
    };
    let (mean, mad) = stat(&prev, &|x, y, c| {
        let mut acc = 0f64;
        for yy in y * f..(y + 1) * f {
            for xx in x * f..(x + 1) * f {
                acc += full.data[(yy.min(full.height - 1) * full.width + xx.min(full.width - 1)) * 3 + c] as f64;
            }
        }
        acc / (f * f) as f64
    });
    eprintln!("preview {}×{} vs. export/{f}: mean deviation {:+.2?}, mean magnitude {:.2?} (8-bit steps)", prev.width, prev.height, mean, mad);
    let [w, h] = session.size();
    let r = [w / 2 - 600, h / 2 - 450, 1200, 900];
    let det = session.render_region(s, r);
    let (mean, mad) = stat(&det, &|x, y, c| full.data[((r[1] + y) * full.width + r[0] + x) * 3 + c] as f64);
    eprintln!("detail 1200×900 vs. export: mean deviation {:+.3?}, mean magnitude {:.3?}", mean, mad);
    film::set_backend(film::Backend::Cpu);
    let det_cpu = session.render_region(s, r);
    film::set_backend(film::Backend::Auto);
    let (mean, mad) = stat(&det_cpu, &|x, y, c| det.data[(y * det.width + x) * 3 + c] as f64);
    eprintln!("detail CPU vs. {}: mean deviation {:+.3?}, mean magnitude {:.3?}", film::backend_name(), mean, mad);
    Ok(())
}
