//! `graineo-dev perf <levels|basis|masks> <target> <files …>`: performance measurements.
//! Measurement only, changes nothing in the app. Result per command as `<target>/<command>.json`.
//!
//! * `levels`: cost and image deviation of smaller computation stages (candidate) against today's
//!   computation at 2400 px (reference), per case and target size.
//! * `basis`: base time per noise level (GPU), disk cache hits, `from_spp`, cache sizes.
//!   `--full` as the first file argument computes all 25 level pairs instead of the diagonal plus the neighbours of the middle.
//! * `baseline-compare <reference> <output> <files …>`: computes the set from `perf-reference` (version v0.86.2)
//!   with the current version and reports bit-identical yes/no, otherwise ΔE00.
//! * `fractions`: time shares of the first computation (Merrill): file, preparation, noise reduction, rest; GPU versus
//!   CPU, per noise level.
//! * `grain <target> <files …>`: image pairs for visual inspection (grain): per photo, film and target size "today"
//!   (computed large, downscaled) and "new" (computed small) as PNG, plus a 200 % crop.
//! * `masks`: cost of a mask in the crop against the same crop without a mask.

use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use develop::film::{self, FilmSettings};
use develop::session::Preview;
use develop::{Rgb16, Session, Settings};
use serde_json::{json, Value};

const PREVIEW_EDGE: usize = 2400;
const RUNS: usize = 3;
const TARGETS: [usize; 4] = [220, 380, 600, 1200];

pub fn run(args: &[String]) -> Result<()> {
    let (sub, rest) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => bail!("usage: graineo-dev perf <levels|basis|masks> <output_folder> <files …>"),
    };
    if sub == "baseline-compare" {
        return basis_compare(rest);
    }
    anyhow::ensure!(rest.len() >= 2, "usage: graineo-dev perf {sub} <output_folder> <files …>");
    let out = Path::new(&rest[0]);
    std::fs::create_dir_all(out)?;
    let files: Vec<&str> = rest[1..].iter().map(String::as_str).collect();
    match sub {
        "levels" => levels(out, &files),
        "basis" => basis(out, &files),
        "masks" => masks(out, &files),
        "fractions" => fractions(out, &files),
        "grain" => grain(out, &files),
        "film-time" => film_duration(out, &files),
        _ => bail!("unknown subcommand {sub}"),
    }
}

/// `film-time <target> <X3F …>`: duration of a film computation (Kodak Tri-X 400) at full resolution via `render_full`
/// (with and without film, difference = film), plus film alone at 2400 px and 380 px on a synthetic scene.
/// Three runs each, median. This is the longest time a running background film holds up what is visible
/// (film gate).
fn film_duration(out: &Path, files: &[&str]) -> Result<()> {
    let trix = cases(true).into_iter().find(|c| c.0 == "film").map(|c| c.1).unwrap_or_default();
    let mut entries = Vec::new();
    for file in files {
        let mut session = Session::open(file).with_context(|| file.to_string())?;
        let [w, h] = session.size();
        let plain = Settings::default();
        // warm up (base, disk cache)
        let _ = session.render_full(&plain);
        let mut t_plain = Vec::new();
        let mut t_film = Vec::new();
        for _ in 0..RUNS {
            let t = Instant::now();
            let _ = session.render_full(&plain);
            t_plain.push(ms(t));
            let t = Instant::now();
            let _ = session.render_full(&trix);
            t_film.push(ms(t));
        }
        let (p, f) = (median(t_plain.clone()), median(t_film.clone()));
        entries.push(json!({"file": name_of(file), "width": w, "height": h, "full_no_film_ms": num(p), "full_with_film_ms": num(f), "film_fraction_ms": num(f - p), "runs_with_film_ms": t_film}));
        eprintln!("{}: {}×{} full {:.0} ms, with film {:.0} ms (film ≈ {:.0} ms)", name_of(file), w, h, p, f, f - p);
    }
    let mut synthetic = Vec::new();
    for (w, h) in [(2400usize, 1600usize), (380, 253)] {
        let scene = film::light_points(w, h);
        let geo = develop::look::Geo::full(w, h);
        let _ = film::develop(&scene, &trix.film, geo);
        let mut v = Vec::new();
        for _ in 0..RUNS {
            let t = Instant::now();
            let _ = film::develop(&scene, &trix.film, geo);
            v.push(ms(t));
        }
        eprintln!("film alone {w}×{h}: {:.0} ms", median(v.clone()));
        synthetic.push(json!({"width": w, "height": h, "film_ms": num(median(v.clone())), "runs_ms": v}));
    }
    let result = json!({"command": "perf film-time", "last": num(load()), "film": trix.film.stock, "files": entries, "synthetic": synthetic});
    std::fs::write(out.join("film-time.json"), serde_json::to_string_pretty(&result)?)?;
    Ok(())
}

// ---------------------------------------------------------------- helpers

/// 1-minute load of the machine (like `uptime`).
pub fn load() -> f64 {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg fills at most 3 values into the array
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    if n >= 1 { l[0] } else { f64::NAN }
}

pub fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

pub fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Numbers without NaN/infinity into the JSON (null instead of NaN).
fn num(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
}

fn name_of(file: &str) -> String {
    Path::new(file).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// Mean over blocks like `shrink_rgb` in the app (`jobs.rs`).
fn shrink_rgb(rgb: &[u8], w: usize, h: usize, edge: usize) -> (usize, usize, Vec<u8>) {
    let k = w.max(h).div_ceil(edge.max(1)).max(1);
    let (tw, th) = ((w / k).max(1), (h / k).max(1));
    let mut out = vec![0u8; tw * th * 3];
    for y in 0..th {
        for x in 0..tw {
            let mut acc = [0u32; 3];
            for dy in 0..k {
                for dx in 0..k {
                    let i = ((y * k + dy) * w + x * k + dx) * 3;
                    for c in 0..3 {
                        acc[c] += rgb[i + c] as u32;
                    }
                }
            }
            for c in 0..3 {
                out[(y * tw + x) * 3 + c] = (acc[c] / (k * k) as u32) as u8;
            }
        }
    }
    (tw, th, out)
}

/// Divisor of the smaller computation stage: the integer divisor that brings the stage closest to an edge of `want_edge`
/// (2 × target), but never below `min_edge` (the target itself, with crop target / share).
/// Previously it was rounded up twice (divisor of the stage, then of the downscale) and the result was up to 8 % below the target.
pub fn stage_divisor(long_edge: usize, want_edge: usize, min_edge: usize) -> usize {
    let nearest = (long_edge as f64 / want_edge.max(1) as f64).round() as usize;
    nearest.clamp(1, (long_edge / min_edge.max(1)).max(1))
}

/// Value for `Preview::scaled(max_edge)` at which the divisor becomes exactly `stage_divisor` (stage >= `min_edge`).
pub fn stage_edge(long_edge: usize, want_edge: usize, min_edge: usize) -> usize {
    long_edge.div_ceil(stage_divisor(long_edge, want_edge, min_edge))
}

fn to_image(img: &Rgb16) -> image::RgbImage {
    image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).expect("RGB buffer")
}

/// Area mean to exactly `w × h` (fractions of source pixels weighted), 8 bit like `shrink_rgb`.
pub fn area_to(src: &image::RgbImage, w: usize, h: usize) -> image::RgbImage {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (sx, sy) = (sw as f64 / w as f64, sh as f64 / h as f64);
    let mut out = image::RgbImage::new(w as u32, h as u32);
    for y in 0..h {
        let (y0, y1) = (y as f64 * sy, (y + 1) as f64 * sy);
        for x in 0..w {
            let (x0, x1) = (x as f64 * sx, (x + 1) as f64 * sx);
            let mut acc = [0f64; 3];
            let mut wsum = 0f64;
            for yy in (y0.floor() as usize)..(y1.ceil() as usize).min(sh) {
                let wy = (y1.min((yy + 1) as f64) - y0.max(yy as f64)).max(0.0);
                for xx in (x0.floor() as usize)..(x1.ceil() as usize).min(sw) {
                    let wx = (x1.min((xx + 1) as f64) - x0.max(xx as f64)).max(0.0);
                    let p = src.get_pixel(xx as u32, yy as u32).0;
                    for c in 0..3 {
                        acc[c] += p[c] as f64 * wx * wy;
                    }
                    wsum += wx * wy;
                }
            }
            let v = acc.map(|a| (a / wsum.max(1e-9)).round().clamp(0.0, 255.0) as u8);
            out.put_pixel(x as u32, y as u32, image::Rgb(v));
        }
    }
    out
}

fn shrunk(img: &Rgb16, edge: usize) -> image::RgbImage {
    let (w, h, rgb) = shrink_rgb(&img.to_rgb8(), img.width, img.height, edge);
    image::RgbImage::from_raw(w as u32, h as u32, rgb).expect("RGB buffer")
}

fn film_on(stock: &str) -> FilmSettings {
    // PERF_NO_GRAIN=1: diagnostic, grain off (shows how much of the deviation comes from grain)
    let grain = if std::env::var_os("PERF_NO_GRAIN").is_some() { 0.0 } else { 1.0 };
    FilmSettings { active: true, stock: stock.to_string(), grain, ..FilmSettings::default() }
}

/// The cases: (name, settings, adapt computation size after crop?).
fn cases(sigma: bool) -> Vec<(&'static str, Settings, bool)> {
    let trix = if film::stock("kodak_trix_400").is_some() {
        "kodak_trix_400".to_string()
    } else {
        film::films().iter().find(|f| f.category == film::Category::BlackWhite).map(|f| f.id.clone()).unwrap_or_default()
    };
    let color = film::films().iter().find(|f| f.category == film::Category::ColorNegative).map(|f| f.id.clone()).unwrap_or_default();
    let base = Settings::default();
    let mut v = vec![("standard", base.clone(), false)];
    v.push(("film", Settings { film: film_on(&trix), ..base.clone() }, false));
    v.push(("film_color", Settings { film: film_on(&color), ..base.clone() }, false));
    let crop = Settings { crop: [0.5, 0.5, 0.4, 0.4], ..base.clone() };
    v.push(("crop", crop.clone(), false));
    // additionally: computation size adapted after crop (the way the planned rework is meant to do it)
    v.push(("crop_scaled", crop, true));
    let sharp = if sigma { Settings { sigma_sharpness: 1.0, ..base.clone() } } else { Settings { sharpen: 1.0, ..base } };
    v.push(("sharpness", sharp, false));
    v
}

// ---------------------------------------------------------------- stufen

/// A source from which reference and candidate are computed.
enum Source {
    Sigma { cal: std::sync::Arc<develop::Calibration>, prev: Preview },
    Raw(Box<rawdev::RawDoc>),
}

impl Source {
    /// Today's computation: compute preview size, apply geometry.
    fn reference(&self, s: &Settings) -> Rgb16 {
        match self {
            Source::Sigma { cal, prev } => develop::geometry::apply(prev.render(cal, s), s, cal.orientation),
            Source::Raw(doc) => develop::geometry::apply(doc.render_preview(s), s, doc.orientation),
        }
    }

    /// Candidate: compute a stage with a longest edge of about `edge`. Returns: image and time for downscaling the base.
    /// `old`: old rounding (divisor rounded up) for comparison.
    fn candidate(&self, s: &Settings, edge: usize, min_edge: usize, old: bool) -> (Rgb16, f64) {
        match self {
            Source::Sigma { cal, prev } => {
                let t = Instant::now();
                let long = prev.lin.width.max(prev.lin.height);
                let scaled = prev.scaled(cal, if old { edge } else { stage_edge(long, edge, min_edge) });
                let t_scaled = ms(t);
                let p = scaled.as_ref().unwrap_or(prev);
                (develop::geometry::apply(p.render(cal, s), s, cal.orientation), t_scaled)
            }
            Source::Raw(doc) => {
                let t = Instant::now();
                let st = doc.small(s, edge);
                let t_scaled = ms(t);
                (develop::geometry::apply(doc.render_stage(&st, s), s, doc.orientation), t_scaled)
            }
        }
    }
}

fn open_source(file: &str) -> Result<(Source, &'static str)> {
    if file.to_ascii_lowercase().ends_with(".x3f") {
        let mut session = Session::open(file).map_err(|e| anyhow::anyhow!("{e}"))?;
        let s0 = Settings::default();
        session.ensure_spp(&s0).map_err(anyhow::Error::msg)?;
        let (base, matrix) = (session.spp_base().context("SPP base")?, session.spp_matrix().context("matrix")?);
        let prev = Preview::from_spp(&base, matrix, PREVIEW_EDGE);
        Ok((Source::Sigma { cal: session.cal.clone(), prev }, "sigma"))
    } else {
        let doc = rawdev::RawDoc::open(Path::new(file), &|| true).map_err(anyhow::Error::msg)?.context("cancelled")?;
        Ok((Source::Raw(Box::new(doc)), "raw"))
    }
}

fn levels(out: &Path, files: &[&str]) -> Result<()> {
    // `--small`: only the film cases (film; non-Sigma also without film) and targets 220/380/600.
    let small = files.first() == Some(&"--small");
    let files = if small { &files[1..] } else { files };
    let targets: Vec<usize> = if small { vec![220, 380, 600] } else { TARGETS.to_vec() };
    let jpg_dir = out.join("levels");
    std::fs::create_dir_all(&jpg_dir)?;
    let load_start = load();
    let t_all = Instant::now();
    let mut rows: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    for file in files {
        let name = name_of(file);
        let (src, file_kind) = match open_source(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{name}: {e:#}");
                errors.push(json!({"file": name, "error": format!("{e:#}")}));
                continue;
            }
        };
        let is_sigma = matches!(src, Source::Sigma { .. });
        for (case, s, adapt) in cases(is_sigma) {
            if small {
                let keep = if is_sigma { matches!(case, "film" | "film_color") } else { matches!(case, "standard" | "film" | "film_color" | "crop_scaled") };
                if !keep {
                    continue;
                }
            }
            // Reference: render three times, keep the image from the last round
            let mut ref_render = Vec::new();
            let mut ref_img = None;
            for _ in 0..RUNS {
                let t = Instant::now();
                let img = src.reference(&s);
                ref_render.push(ms(t));
                ref_img = Some(img);
            }
            let ref_img = ref_img.unwrap();
            let ref_render_ms = median(ref_render);
            for &target in &targets {
                // downscale the reference to the target size (measured per target size)
                let mut shr = Vec::new();
                let mut ref_small = None;
                for _ in 0..RUNS {
                    let t = Instant::now();
                    ref_small = Some(shrunk(&ref_img, target));
                    shr.push(ms(t));
                }
                let ref_small = ref_small.unwrap();
                let ms_reference = ref_render_ms + median(shr.clone());
                let share = if adapt { s.crop[2].min(s.crop[3]).max(0.01) } else { 1.0 };
                let edge = (((2 * target) as f32 / share).ceil() as usize).min(PREVIEW_EDGE);
                let min_edge = ((target as f32 / share).ceil() as usize).min(PREVIEW_EDGE);
                let (mut tot, mut scl) = (Vec::new(), Vec::new());
                let mut cand_small = None;
                for _ in 0..RUNS {
                    let t = Instant::now();
                    let (img, t_scaled) = src.candidate(&s, edge, min_edge, false);
                    // time with the integer downscale as in the app; matching the size of the
                    // reference (area mean) is only for the comparison and is outside the timing.
                    let _app_small = shrunk(&img, target);
                    tot.push(ms(t));
                    scl.push(t_scaled);
                    cand_small = Some(area_to(&to_image(&img), ref_small.width() as usize, ref_small.height() as usize));
                }
                let cand_small = cand_small.unwrap();
                let c = crate::raw_compare::compare_images(&cand_small, &ref_small, false);
                // for comparison the old rounding (Sigma only, where the error was); own size, comparison scaled
                let de_old = if is_sigma {
                    let (img, _) = src.candidate(&s, edge, min_edge, true);
                    let old_small = shrunk(&img, target);
                    Some((crate::raw_compare::compare_images(&old_small, &ref_small, false).de as f64, [old_small.width(), old_small.height()]))
                } else {
                    None
                };
                if target == 600 {
                    let p = jpg_dir.join(format!("{}_{case}.jpg", Path::new(file).file_stem().unwrap().to_string_lossy()));
                    crate::raw_compare::side_by_side(&cand_small, &ref_small, false, &p.to_string_lossy())?;
                }
                let row = json!({
                    "file": name, "kind": file_kind, "case": case, "target": target, "compute_edge": edge,
                    "ms_reference": num(ms_reference), "ms_candidate": num(median(tot)), "ms_candidate_scale": num(median(scl)),
                    "de_old_rounding": de_old.map(|a| num(a.0)), "image_old_rounding": de_old.map(|a| a.1),
                    "de": num(c.de as f64), "de_zone": c.de_zone.map(|v| num(v as f64)),
                    "image_reference": [ref_small.width(), ref_small.height()], "image_candidate": [cand_small.width(), cand_small.height()],
                    "last_1min": num(load()),
                });
                eprintln!(
                    "{name:<26} {case:<19} {target:>5}  ref {:>7.1} ms  cand {:>7.1} ms  dE {:.3}",
                    ms_reference, row["ms_candidate"].as_f64().unwrap_or(f64::NAN), c.de
                );
                rows.push(row);
            }
        }
    }
    let nan = rows.iter().filter(|r| r["de"].is_null() || r["ms_candidate"].is_null()).count();
    let doc = json!({
        "command": "perf levels", "runs_per_measure": RUNS, "median": true,
        "last_start": num(load_start), "last_end": num(load()), "duration_s": t_all.elapsed().as_secs_f64(),
        "entries": rows.len(), "entries_with_null": nan, "error": errors, "rows": rows,
    });
    std::fs::write(out.join("levels.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!("{} entries, {} with null → {}", rows.len(), nan, out.join("levels.json").display());
    Ok(())
}

// ---------------------------------------------------------------- basis

/// Level pairs (luma, chroma): all 25 or the diagonal plus the neighbours of the middle.
fn nr_pairs(all: bool) -> Vec<(i8, i8)> {
    if all {
        return (0..5).flat_map(|a| (0..5).map(move |b| (a, b))).collect();
    }
    let mut v: Vec<(i8, i8)> = (0..5).map(|i| (i, i)).collect();
    v.extend([(1, 2), (3, 2), (2, 1), (2, 3)]);
    v
}

fn key_with(nr: (i8, i8)) -> develop::SppKey {
    let mut k = Settings::default().spp_key_class(false);
    k.nr = nr;
    k
}

fn basis(out: &Path, files: &[&str]) -> Result<()> {
    use std::sync::atomic::AtomicBool;
    let all = files.first() == Some(&"--full");
    let files = if all { &files[1..] } else { files };
    // own disk cache, never the real one
    let cache = out.join("basis-cache");
    std::fs::create_dir_all(&cache)?;
    std::env::set_var("GRAINEO_CACHE_DIR", &cache);
    let cancel = AtomicBool::new(false);
    let load_start = load();
    let mut per_file = Vec::new();
    for file in files {
        let name = name_of(file);
        if !file.to_ascii_lowercase().ends_with(".x3f") {
            eprintln!("{name}: not an X3F, skipped");
            continue;
        }
        let path = Path::new(file);
        let one = (|| -> Result<Value> {
            let t = Instant::now();
            let session = Session::open(path).map_err(|e| anyhow::anyhow!("{e}"))?;
            let open_ms = ms(t);
            let mut combos = Vec::new();
            // warm-up (GPU start), not counted
            let warm = develop::spp::working_key(path, &key_with((2, 2)), true, &cancel).map_err(anyhow::Error::msg)?;
            drop(warm);
            for nr in nr_pairs(all) {
                let key = key_with(nr);
                let mut times = Vec::new();
                for _ in 0..RUNS {
                    let t = Instant::now();
                    let lin = develop::spp::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?;
                    times.push(ms(t));
                    drop(lin);
                }
                eprintln!("{name:<26} nr {nr:?}  Basis GPU {:.0} ms", median(times.clone()));
                combos.push(json!({"nr": [nr.0, nr.1], "ms": times.iter().map(|v| num(*v)).collect::<Vec<_>>(), "median_ms": num(median(times))}));
            }
            // disk cache: fill once (miss), then measure hits
            let key = key_with((2, 2));
            let (bytes0, _) = develop::diskcache::stats();
            let t = Instant::now();
            let lin = develop::diskcache::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?;
            let miss_ms = ms(t);
            // The storage thread works in the background: wait until the size stops changing
            let mut last = 0;
            let mut still = 0;
            for _ in 0..600 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let (b, _) = develop::diskcache::stats();
                if b > bytes0 && b == last {
                    still += 1;
                    if still >= 3 {
                        break;
                    }
                } else {
                    still = 0;
                }
                last = b;
            }
            let (bytes1, _) = develop::diskcache::stats();
            let entry_bytes = bytes1.saturating_sub(bytes0);
            let mut hits = Vec::new();
            for _ in 0..RUNS {
                let t = Instant::now();
                let l = develop::diskcache::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?;
                hits.push(ms(t));
                drop(l);
            }
            // preview from the base at 2400 and the disk sizes of the stages
            let matrix = session.cal.mode(key.mode.as_deref()).cam_to_srgb;
            let base = develop::session::SppBase::new(key.clone(), lin);
            let mut from = Vec::new();
            for _ in 0..RUNS {
                let t = Instant::now();
                let p = Preview::from_spp(&base, matrix, PREVIEW_EDGE);
                from.push(ms(t));
                drop(p);
            }
            let (w, h) = (base.counts.width, base.counts.height);
            let mem = |l: &develop::Linear| (l.data.len() * 4 + l.detail.as_ref().map_or(0, |d| d.l.len() * 2)) as u64;
            let p2400 = Preview::from_spp(&base, matrix, PREVIEW_EDGE);
            let p2 = Preview::from_spp(&base, matrix, w.max(h).div_ceil(2));
            let p4 = Preview::from_spp(&base, matrix, w.max(h).div_ceil(4));
            let sizes = json!({
                "full": {"width": w, "height": h, "memory": mem(&base.counts), "encoded": develop::diskcache::encoded_size(&base.counts), "entry_on_disk": entry_bytes},
                "preview_2400": {"factor": p2400.factor, "width": p2400.lin.width, "height": p2400.lin.height, "memory": mem(&p2400.lin), "encoded": develop::diskcache::encoded_size(&p2400.lin)},
                "factor_2": {"factor": p2.factor, "memory": mem(&p2.lin), "encoded": develop::diskcache::encoded_size(&p2.lin)},
                "factor_4": {"factor": p4.factor, "memory": mem(&p4.lin), "encoded": develop::diskcache::encoded_size(&p4.lin)},
            });
            eprintln!(
                "{name:<26} open {open_ms:.0} ms · cache miss {miss_ms:.0} ms · hit {:.0} ms · from_spp {:.0} ms · entry {:.0} MB",
                median(hits.clone()), median(from.clone()), entry_bytes as f64 / 1e6
            );
            Ok(json!({
                "file": name, "open_ms": num(open_ms), "basis_per_level": combos,
                "cache_miss_ms": num(miss_ms), "cache_hit_ms": hits.iter().map(|v| num(*v)).collect::<Vec<_>>(),
                "cache_hit_median_ms": num(median(hits)), "from_spp_2400_ms": from.iter().map(|v| num(*v)).collect::<Vec<_>>(),
                "from_spp_2400_median_ms": num(median(from)), "sizes": sizes,
            }))
        })();
        match one {
            Ok(v) => per_file.push(v),
            Err(e) => {
                eprintln!("{name}: {e:#}");
                per_file.push(json!({"file": name, "error": format!("{e:#}")}));
            }
        }
    }
    let doc = json!({
        "command": "perf basis", "levels": if all { "alle 25" } else { "Diagonale plus Nachbarn der Mitte" },
        "runs_per_measure": RUNS, "median": true, "last_start": num(load_start), "last_end": num(load()),
        "disk_cache": cache.to_string_lossy(), "files": per_file,
    });
    std::fs::write(out.join("basis.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!("→ {}", out.join("basis.json").display());
    Ok(())
}

// ---------------------------------------------------------------- masken

/// Radial mask, exposure +0.5.
fn radial_mask() -> Result<masks::Document> {
    masks::Document::from_json(
        r#"{"version":2,"layers":[{"mask":{"type":"radial","center":[0.5,0.5],"radii":[0.45,0.45],"angle":0.0,"feather":0.5},"adjustments":{"exposure":0.5}}]}"#,
    )
    .map_err(anyhow::Error::msg)
}

fn masks(out: &Path, files: &[&str]) -> Result<()> {
    let doc_mask = radial_mask()?;
    let load_start = load();
    let mut per_file = Vec::new();
    for file in files {
        let name = name_of(file);
        let plain = Settings::default();
        let masked = Settings { local: Some(doc_mask.clone()), ..Settings::default() };
        let one = (|| -> Result<Value> {
            let sigma = file.to_ascii_lowercase().ends_with(".x3f");
            let mut rows = Vec::new();
            // render(s, region) -> (time ms, width, height)
            let mut sess;
            let mut rawdoc;
            let (full_w, full_h);
            if sigma {
                sess = Some(Session::open(file).map_err(|e| anyhow::anyhow!("{e}"))?);
                rawdoc = None;
                let sess = sess.as_mut().unwrap();
                sess.ensure_spp(&plain).map_err(anyhow::Error::msg)?;
                [full_w, full_h] = sess.size();
            } else {
                sess = None;
                let d = rawdev::RawDoc::open(Path::new(file), &|| true).map_err(anyhow::Error::msg)?.context("cancelled")?;
                [full_w, full_h] = d.full_size;
                rawdoc = Some(d);
            }
            let mut run_region = |s: &Settings, r: [usize; 4]| -> Result<(f64, usize, usize)> {
                let t = Instant::now();
                let img = match (&mut sess, &mut rawdoc) {
                    (Some(se), _) => se.render_region(s, r),
                    (_, Some(d)) => d.render_region(s, r).map_err(anyhow::Error::msg)?,
                    _ => unreachable!(),
                };
                Ok((ms(t), img.width, img.height))
            };
            for side in [800usize, 1600] {
                let (w, h) = (side.min(full_w), side.min(full_h));
                let region = [(full_w - w) / 2, (full_h - h) / 2, w, h];
                // first run (cold: stages are computed), then counted three times
                let cold_plain = run_region(&plain, region)?.0;
                let cold_mask = run_region(&masked, region)?.0;
                let mut a = Vec::new();
                let mut b = Vec::new();
                for _ in 0..RUNS {
                    a.push(run_region(&plain, region)?.0);
                    b.push(run_region(&masked, region)?.0);
                }
                let (ma, mb) = (median(a), median(b));
                eprintln!("{name:<26} {side}²  without mask {ma:>7.1} ms  with mask {mb:>8.1} ms  ({:.1}x)  cold {cold_plain:.0}/{cold_mask:.0}", mb / ma);
                rows.push(json!({
                    "crop": [region[0], region[1], w, h], "no_mask_median_ms": num(ma), "with_mask_median_ms": num(mb),
                    "factor": num(mb / ma), "cold_no_mask_ms": num(cold_plain), "cold_with_mask_ms": num(cold_mask),
                }));
            }
            Ok(json!({"file": name, "kind": if sigma { "sigma" } else { "raw" }, "image": [full_w, full_h], "crops": rows}))
        })();
        match one {
            Ok(v) => per_file.push(v),
            Err(e) => {
                eprintln!("{name}: {e:#}");
                per_file.push(json!({"file": name, "error": format!("{e:#}")}));
            }
        }
    }
    let doc = json!({
        "command": "perf masks", "mask": "Radial, Belichtung +0,5", "runs_per_measure": RUNS, "median": true,
        "last_start": num(load_start), "last_end": num(load()), "files": per_file,
        "note_call_in_app": "The app sends Job::RenderFull (whole image) when the visible part with margin covers at least 60 % of the image or the image is computed completely once per settings state in the background (only up to about 51 MP, not with the grain loupe or gain-map HDR, main.rs at needs_detail: nearly_all/full_free); otherwise Job::RenderRegion with the visible part plus a 25 % margin (main.rs, branch stale), also for hovered presets, the left side of the darkroom comparison and comparison fields (compare_view.rs). With an active mask, Session::render_region and RawDoc::render_region currently compute the whole image and crop afterwards (local::active), no matter how small the crop is.",
    });
    std::fs::write(out.join("masks.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!("→ {}", out.join("masks.json").display());
    Ok(())
}

// ---------------------------------------------------------------- basis-vergleich

/// Computes the reference set with the current version and compares with the checksums and images from the
/// reference folder (created with `graineo-dev perf-reference` on `v0.86.2`).
fn basis_compare(args: &[String]) -> Result<()> {
    anyhow::ensure!(args.len() >= 3, "usage: graineo-dev perf baseline-compare <reference_folder> <output_folder> <files …>");
    let (refdir, out) = (Path::new(&args[0]), Path::new(&args[1]));
    std::fs::create_dir_all(out)?;
    let files: Vec<&str> = args[2..].iter().map(String::as_str).collect();
    let reference: Value = serde_json::from_str(&std::fs::read_to_string(refdir.join("reference.json")).context("reference.json")?)?;
    let ref_rows = reference["rows"].as_array().context("rows")?;
    let mut rows: Vec<Value> = Vec::new();
    let (mut equal_level, mut equal_full, mut missing) = (0usize, 0usize, 0usize);
    crate::baseline_compare::render_set(&files, |e| {
        let r = ref_rows.iter().find(|z| z["file"] == e.file.as_str() && z["case"] == e.case);
        let Some(r) = r else {
            missing += 1;
            rows.push(json!({"file": e.file, "case": e.case, "error": "no reference entry"}));
            return Ok(());
        };
        let level_equal = r["level_sha256"] == e.level_sha.as_str();
        let full_equal = r["full_sha256"] == e.full_sha.as_str();
        equal_level += level_equal as usize;
        equal_full += full_equal as usize;
        let mut row = json!({
            "file": e.file, "case": e.case, "level_bit_identical": level_equal, "full_bit_identical": full_equal,
            "full_size_equal": r["full_width"] == e.full_width && r["full_height"] == e.full_height,
        });
        if !level_equal {
            let old = crate::baseline_compare::read_level(&crate::baseline_compare::level_file(refdir, &e.file, e.case))?;
            if (old.width, old.height) == (e.level.width, e.level.height) {
                let c = crate::raw_compare::compare_images(&shrunk(&e.level, 400), &shrunk(&old, 400), false);
                row["level_de"] = num(c.de as f64);
                row["level_de_zone"] = json!(c.de_zone.iter().map(|v| num(*v as f64)).collect::<Vec<_>>());
            } else {
                row["level_size_new"] = json!([e.level.width, e.level.height]);
                row["level_size_ref"] = json!([old.width, old.height]);
            }
        }
        eprintln!("{:<28} {:<11} stage {}  full {}", e.file, e.case, if level_equal { "bit-identical" } else { "DEVIATION" }, if full_equal { "bit-identical" } else { "DEVIATION" });
        rows.push(row);
        Ok(())
    })?;
    let doc = json!({
        "command": "perf baseline-compare", "last_1min": num(load()), "entries": rows.len(),
        "level_bit_identical": equal_level, "full_bit_identical": equal_full, "no_reference": missing,
        "all_bit_identical": equal_level == rows.len() && equal_full == rows.len(), "rows": rows,
    });
    std::fs::write(out.join("baseline-compare.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!(
        "{} entries: output stage bit-identical {equal_level}, full resolution bit-identical {equal_full}, without reference {missing} → {}",
        rows.len(), out.join("baseline-compare.json").display()
    );
    Ok(())
}

// ---------------------------------------------------------------- anteile

/// Duration from the output of `{:.0?}` (e.g. `123ms`, `2s`, `45µs`) in ms.
fn parse_duration_ms(t: &str) -> Option<f64> {
    let t = t.trim();
    if let Some(v) = t.strip_suffix("ms") {
        return v.trim().parse().ok();
    }
    if let Some(v) = t.strip_suffix("µs") {
        return v.trim().parse::<f64>().ok().map(|x| x / 1e3);
    }
    if let Some(v) = t.strip_suffix("ns") {
        return v.trim().parse::<f64>().ok().map(|x| x / 1e6);
    }
    t.strip_suffix('s')?.trim().parse::<f64>().ok().map(|x| x * 1e3)
}

/// Runs `f` and additionally returns everything `f` wrote to stderr (the process stream is briefly
/// redirected to a file for this; measurement runs only).
fn with_stderr<T>(tmp: &Path, f: impl FnOnce() -> T) -> (T, String) {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::create(tmp).expect("tmp");
    // SAFETY: dup/dup2 on the standard error output, restored at the end
    let saved = unsafe { libc::dup(2) };
    unsafe { libc::dup2(file.as_raw_fd(), 2) };
    let r = f();
    unsafe {
        libc::dup2(saved, 2);
        libc::close(saved);
    }
    (r, std::fs::read_to_string(tmp).unwrap_or_default())
}

/// Splits the `X3F_SPP_TIMING` lines into individual times (ms).
fn parse_timing(text: &str) -> Value {
    let mut stages = Vec::new();
    let (mut setup, mut pre, mut nr) = (None, None, None);
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("SPP ") else { continue };
        if let Some(r) = rest.strip_prefix("setup (noise parameters, matrices): ") {
            setup = parse_duration_ms(r);
        } else if let Some(r) = rest.strip_prefix("pre-stages total ") {
            if let Some((a, b)) = r.split_once(" · noise reduction ") {
                pre = parse_duration_ms(a);
                nr = parse_duration_ms(b);
            }
        } else if let Some((name, t)) = rest.rsplit_once(": ") {
            if let Some(v) = parse_duration_ms(t) {
                stages.push(json!({"level": name, "ms": v}));
            }
        }
    }
    json!({"setup_ms": setup, "preparation_total_ms": pre, "noise_reduction_ms": nr, "pre_stages": stages})
}

fn fractions(out: &Path, files: &[&str]) -> Result<()> {
    use std::sync::atomic::AtomicBool;
    std::env::set_var("GRAINEO_NO_DISK_CACHE", "1");
    let cancel = AtomicBool::new(false);
    let load_start = load();
    let tmp = out.join("anteile-stderr.tmp");
    let keys = [(2i8, 2i8), (1, 2), (3, 2), (2, 1), (2, 3)];
    let mut per_file = Vec::new();
    for file in files {
        let name = name_of(file);
        if !file.to_ascii_lowercase().ends_with(".x3f") {
            continue;
        }
        let path = Path::new(file);
        let one = (|| -> Result<Value> {
            // pure file read time (fetch bytes, page cache warm)
            let _ = std::fs::read(path)?;
            let t = Instant::now();
            let bytes = std::fs::read(path)?;
            let read_ms = ms(t);
            drop(bytes);
            let t = Instant::now();
            let session = Session::open(path).map_err(|e| anyhow::anyhow!("{e}"))?;
            let open_ms = ms(t);
            drop(session);
            // reading the ISO from the noise parameters of the computation is awkward: from the file name prefix `isoNNN_` or `info`
            let mut rows = Vec::new();
            for vname in ["graineo"] {
                for nr in keys {
                    let key = key_with(nr);
                    // warm-up, not counted
                    drop(develop::spp::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?);
                    let (mut gpu_t, mut gpu_used) = (Vec::new(), true);
                    for _ in 0..RUNS {
                        let t = Instant::now();
                        drop(develop::spp::working_key(path, &key, true, &cancel).map_err(anyhow::Error::msg)?);
                        gpu_t.push(ms(t));
                        gpu_used &= develop::spp::last_used_gpu();
                    }
                    // time shares: one run with X3F_SPP_TIMING
                    std::env::set_var("X3F_SPP_TIMING", "1");
                    let t = Instant::now();
                    let (r, text) = with_stderr(&tmp, || develop::spp::working_key(path, &key, true, &cancel).map(drop));
                    let total_timed = ms(t);
                    std::env::remove_var("X3F_SPP_TIMING");
                    r.map_err(anyhow::Error::msg)?;
                    let gpu_timed = develop::spp::last_used_gpu();
                    let timing = parse_timing(&text);
                    // CPU path for comparison (one run only, bit-exact path)
                    let t = Instant::now();
                    drop(develop::spp::working_key(path, &key, false, &cancel).map_err(anyhow::Error::msg)?);
                    let cpu_ms = ms(t);
                    let gpu_med = median(gpu_t.clone());
                    eprintln!(
                        "{name:<28} {vname:<11} nr {nr:?}  total {gpu_med:>7.0} ms  ({}),  CPU path {cpu_ms:>7.0} ms  setup {:?} preparation {:?} noise reduction {:?}",
                        if gpu_used { "GPU" } else { "CPU" }, timing["setup_ms"], timing["preparation_total_ms"], timing["noise_reduction_ms"]
                    );
                    rows.push(json!({
                        "method": vname, "nr": [nr.0, nr.1], "total_gpu_path_ms": gpu_t.iter().map(|v| num(*v)).collect::<Vec<_>>(),
                        "total_median_ms": num(gpu_med), "ran_on_gpu": gpu_used, "run_with_timing_ms": num(total_timed),
                        "ran_on_gpu_timed_run": gpu_timed, "times": timing, "cpu_path_ms": num(cpu_ms),
                    }));
                }
            }
            Ok(json!({"file": name, "file_read_ms": num(read_ms), "session_open_ms": num(open_ms), "cases": rows}))
        })();
        match one {
            Ok(v) => per_file.push(v),
            Err(e) => {
                eprintln!("{name}: {e:#}");
                per_file.push(json!({"file": name, "error": format!("{e:#}")}));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    let doc = json!({
        "command": "perf fractions", "runs_per_measure": RUNS, "median": true, "last_start": num(load_start), "last_end": num(load()),
        "hint": "Total time = develop::spp::working_key (without disk cache). Time shares from X3F_SPP_TIMING of an additional run; rest = total minus setup minus preparation minus noise reduction (open/read file, copy into the result, detail image).",
        "files": per_file,
    });
    std::fs::write(out.join("fractions.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!("→ {}", out.join("fractions.json").display());
    Ok(())
}

// ---------------------------------------------------------------- korn

/// Crop (half edge length) with a lot of fine structure in the midtones, chosen from the "today" image.
fn grain_tile(img: &image::RgbImage) -> (u32, u32, u32, u32) {
    let (w, h) = (img.width(), img.height());
    let (tw, th) = ((w / 2).max(1), (h / 2).max(1));
    let luma = |p: [u8; 3]| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
    let (mut best, mut best_xy) = (-1f32, (0, 0));
    let step = (tw.min(th) / 6).max(1);
    let mut y0 = 0;
    while y0 + th <= h {
        let mut x0 = 0;
        while x0 + tw <= w {
            let (mut mean, mut grad, mut n) = (0f32, 0f32, 0f32);
            for y in y0..y0 + th - 1 {
                for x in x0..x0 + tw - 1 {
                    let a = luma(img.get_pixel(x, y).0);
                    mean += a;
                    grad += (a - luma(img.get_pixel(x + 1, y).0)).abs() + (a - luma(img.get_pixel(x, y + 1).0)).abs();
                    n += 1.0;
                }
            }
            mean /= n;
            let score = if (60.0..190.0).contains(&mean) { grad / n } else { 0.0 };
            if score > best {
                best = score;
                best_xy = (x0, y0);
            }
            x0 += step;
        }
        y0 += step;
    }
    (best_xy.0, best_xy.1, tw, th)
}

/// (mean luma difference of neighbouring pixels = fine-structure/grain measure, mean luma, luma spread).
fn fine_stats(img: &image::RgbImage) -> (f64, f64, f64) {
    let luma = |p: [u8; 3]| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
    let (w, h) = (img.width(), img.height());
    let (mut g, mut m, mut m2, mut n) = (0f64, 0f64, 0f64, 0f64);
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let a = luma(img.get_pixel(x, y).0);
            g += (a - luma(img.get_pixel(x + 1, y).0)).abs() + (a - luma(img.get_pixel(x, y + 1).0)).abs();
            m += a;
            m2 += a * a;
            n += 1.0;
        }
    }
    (g / (2.0 * n), m / n, (m2 / n - (m / n).powi(2)).max(0.0).sqrt())
}

fn zoom200(img: &image::RgbImage, t: (u32, u32, u32, u32)) -> image::RgbImage {
    let crop = image::imageops::crop_imm(img, t.0, t.1, t.2, t.3).to_image();
    image::imageops::resize(&crop, t.2 * 2, t.3 * 2, image::imageops::FilterType::Nearest)
}

fn grain(out: &Path, files: &[&str]) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let color = film::films().iter().find(|f| f.category == film::Category::ColorNegative).map(|f| f.id.clone()).unwrap_or_default();
    let films = [("kodak_trix_400".to_string(), "Kodak Tri-X 400"), (color.clone(), "Farbnegativfilm (erster der Liste)")];
    let mut entries: Vec<Value> = Vec::new();
    for file in files {
        let name = name_of(file);
        let stem = Path::new(file).file_stem().unwrap().to_string_lossy().to_string();
        let (src, _) = open_source(file)?;
        for (stock, label) in &films {
            let s = Settings { film: film_on(stock), ..Settings::default() };
            let ref_img = src.reference(&s);
            for target in [220usize, 380, 600] {
                let today = shrunk(&ref_img, target);
                let (img, _) = src.candidate(&s, 2 * target, target, false);
                let new = area_to(&to_image(&img), today.width() as usize, today.height() as usize);
                let t = grain_tile(&today);
                let base = format!("{stem}_{stock}_{target}");
                today.save(out.join(format!("{base}_today.png")))?;
                new.save(out.join(format!("{base}_new.png")))?;
                zoom200(&today, t).save(out.join(format!("{base}_today_200.png")))?;
                zoom200(&new, t).save(out.join(format!("{base}_new_200.png")))?;
                let c = crate::raw_compare::compare_images(&new, &today, false);
                let (fh, fn_) = (fine_stats(&today), fine_stats(&new));
                eprintln!("{name:<26} {stock:<20} {target:>4}  dE {:.3}  fine structure new/today {:.2}", c.de, fn_.0 / fh.0.max(1e-9));
                entries.push(json!({"file": name, "stem": stem, "film": stock, "film_name": label, "target": target,
                    "basis": base, "image": [today.width(), today.height()], "crop": [t.0, t.1, t.2, t.3], "de": num(c.de as f64),
                    "fine_today": num(fh.0), "fine_new": num(fn_.0), "luma_today": num(fh.1), "luma_new": num(fn_.1), "spread_today": num(fh.2), "spread_new": num(fn_.2)}));
            }
        }
    }
    std::fs::write(out.join("grain.json"), serde_json::to_string_pretty(&json!({"entries": entries}))?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_is_never_smaller_than_target() {
        for long in [1176usize, 1200, 1799, 2352, 2400, 2401, 3000] {
            for target in [100usize, 220, 380, 600, 1200, 2000, 5000] {
                let max_edge = stage_edge(long, 2 * target, target);
                let k_used = long.div_ceil(max_edge);
                assert!(k_used <= stage_divisor(long, 2 * target, target).max(1), "divisor {k_used} too large (long {long}, target {target})");
                if target <= long {
                    assert!(long / k_used >= target, "long {long} target {target}: stage {} < target", long / k_used);
                }
            }
        }
        // 2352 px preview: target 220 -> divisor 5 (470 px, close to 2 x target), target 600 -> 2 (1176 px)
        assert_eq!(stage_divisor(2352, 440, 220), 5);
        assert_eq!(stage_divisor(2352, 1200, 600), 2);
    }

    #[test]
    fn area_to_keeps_size_and_mean() {
        let mut img = image::RgbImage::new(10, 7);
        for (i, p) in img.pixels_mut().enumerate() {
            *p = image::Rgb([(i % 256) as u8, 100, 200]);
        }
        let o = area_to(&img, 4, 3);
        assert_eq!((o.width(), o.height()), (4, 3));
        assert_eq!(o.get_pixel(0, 0).0[1], 100);
    }
}
