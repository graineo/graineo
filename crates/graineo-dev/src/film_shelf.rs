//! `graineo-dev film-shelf IMAGE [--mode crop|thumb] [--edge N] [--passes N] [--films N]`: time per film of the small
//! film previews (darkroom film shelf, "Film & Look" grid) with the app's pipeline. `FILM_SHELF_NO_GRAIN=1`: tiles
//! without grain (as the app computes them since 1.0.2); `FILM_SHELF_ALL=1`: one line per film.
//!
//! - `crop`: a 128 × 80 px crop at full resolution per film (the shelf until 1.0.2): Sigma from the exact base
//!   (`develop::session::region_from_base`), other formats from the full-resolution stage (`RawDoc::render_region`).
//! - `thumb`: the whole photo, downscaled to `--edge` px long side (default 192; the app uses 240) once from the 2400 px preview, then the
//!   film per tile (`develop::analog::thumbnails` / `develop::film::thumbnails`).
//!
//! Every film is timed on its own, in shelf order. Run each mode in a fresh process: the calibrated pipelines and the
//! neutral balances are cached per process, so the first pass shows the cold costs and `--passes 2` the warm ones.

use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use develop::film;
use develop::session::Preview;
use develop::{Session, Settings};

fn set_film(s: &mut Settings, id: &str) {
    s.film.active = true;
    s.film.stock = id.to_string();
    s.film.paper = None;
    s.film.film_dev = 0.0;
    s.film.paper_grade = -1.0;
    s.film.push = 0.0;
    if std::env::var_os("FILM_SHELF_NO_GRAIN").is_some() {
        s.film.grain = 0.0;
    }
}

pub fn run(args: &[String]) -> Result<()> {
    let file = args.first().context("usage: graineo-dev film-shelf IMAGE [--mode crop|thumb] [--edge N] [--passes N] [--films N]")?;
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let mode = value("--mode").unwrap_or_else(|| "thumb".into());
    let edge: usize = value("--edge").map(|v| v.parse()).transpose()?.unwrap_or(192);
    let passes: usize = value("--passes").map(|v| v.parse()).transpose()?.unwrap_or(1);
    let limit: usize = value("--films").map(|v| v.parse()).transpose()?.unwrap_or(usize::MAX);
    let ids: Vec<String> = film::films().iter().take(limit).map(|f| f.id.clone()).collect();
    let path = std::path::Path::new(file);
    let sigma = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f"));
    let t = Instant::now();
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); passes];
    let (w_out, h_out);
    if sigma {
        let session = Session::open(file)?;
        let base_settings = Settings::default();
        let base = session.ensure_spp(&base_settings).map_err(|e| anyhow!(e))?;
        let cal = session.cal.clone();
        let [fw, fh] = session.size();
        eprintln!("{} opened, base ready in {:.2?} ({fw}×{fh})", cal.camera, t.elapsed());
        match mode.as_str() {
            "crop" => {
                let region = [fw / 2 - 64, fh / 2 - 40, 128, 80];
                (w_out, h_out) = (128, 80);
                for pass in times.iter_mut() {
                    for id in &ids {
                        let mut s = base_settings.clone();
                        set_film(&mut s, id);
                        let t = Instant::now();
                        let img = develop::session::region_from_base(&cal, &base, &s, region).context("no crop from the base")?;
                        std::hint::black_box(img);
                        pass.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            "thumb" => {
                let m = session.spp_matrix().context("matrix")?;
                let t = Instant::now();
                let preview = Preview::from_spp(&base, m, 2400);
                eprintln!("preview 2400 px (exists in the app): {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
                let mut s0 = base_settings.clone();
                set_film(&mut s0, &ids[0]);
                let t = Instant::now();
                let (scene, _) = preview.film_scene(&cal, &s0, edge);
                eprintln!("small whole image {}×{} from the preview: {:.1} ms", scene.width, scene.height, t.elapsed().as_secs_f64() * 1000.0);
                (w_out, h_out) = (scene.width, scene.height);
                for pass in times.iter_mut() {
                    for id in &ids {
                        let mut s = base_settings.clone();
                        set_film(&mut s, id);
                        let t = Instant::now();
                        let r = develop::analog::thumbnails(&cal, &s, &[id.as_str()], |s| preview.film_scene(&cal, s, edge));
                        std::hint::black_box(r);
                        pass.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            _ => bail!("--mode crop|thumb"),
        }
    } else {
        let doc = rawdev::RawDoc::open(path, &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
        let base_settings = Settings::default();
        let [fw, fh] = doc.full_size;
        eprintln!("{} {} opened in {:.2?} ({fw}×{fh})", doc.info.maker, doc.info.model, t.elapsed());
        match mode.as_str() {
            "crop" => {
                let t = Instant::now();
                doc.full_stage(&base_settings).map_err(|e| anyhow!(e))?;
                eprintln!("full-resolution stage (the shelf waits for it): {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
                let region = [fw / 2 - 64, fh / 2 - 40, 128, 80];
                (w_out, h_out) = (128, 80);
                for pass in times.iter_mut() {
                    for id in &ids {
                        let mut s = base_settings.clone();
                        set_film(&mut s, id);
                        let t = Instant::now();
                        let img = doc.render_region(&s, region).map_err(|e| anyhow!(e))?;
                        std::hint::black_box(img);
                        pass.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            "thumb" => {
                let t = Instant::now();
                let st = doc.small(&base_settings, edge);
                let (scene, geo) = doc.film_scene(&st, &base_settings);
                eprintln!("small whole image {}×{} from the preview stage: {:.1} ms", scene.width, scene.height, t.elapsed().as_secs_f64() * 1000.0);
                (w_out, h_out) = (scene.width, scene.height);
                for pass in times.iter_mut() {
                    for id in &ids {
                        let mut s = base_settings.clone();
                        set_film(&mut s, id);
                        let t = Instant::now();
                        let r = film::thumbnails(&scene, &s.film, geo, Some(&[id.as_str()]));
                        std::hint::black_box(r);
                        pass.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            _ => bail!("--mode crop|thumb"),
        }
    }
    for (p, pass) in times.iter().enumerate() {
        let mut sorted = pass.clone();
        sorted.sort_by(f64::total_cmp);
        let sum: f64 = pass.iter().sum();
        let med = sorted[sorted.len() / 2];
        println!(
            "mode={mode} pass={} films={} size={w_out}×{h_out} total={sum:.0} ms median={med:.1} ms max={:.0} ms first={:.0} ms",
            p + 1,
            pass.len(),
            sorted.last().copied().unwrap_or(0.0),
            pass.first().copied().unwrap_or(0.0)
        );
        if std::env::var_os("FILM_SHELF_ALL").is_some() {
            for (id, t) in ids.iter().zip(pass) {
                println!("  {id:<30} {t:7.1} ms");
            }
        }
    }
    Ok(())
}
