//! `graineo-dev learn-look --list CANDIDATES.tsv [--key camera/style] [--select-only] [--include-adaptive] [--out DIR]
//! [--cube DIR]`:
//! learn camera looks from RAWs and the camera's own rendering (camera JPEG next to the RAW, otherwise the embedded
//! preview) with the same code as the app's "Learn this camera's look" (`rawdev::learn`, `raw::learn_look`).
//!
//! `CANDIDATES.tsv`: one RAW per line, `RAW<TAB>DAY[<TAB>FOLDER]` (day `YYYY-MM-DD`; folder defaults to the RAW's
//! folder). Lines starting with `#` are skipped. The files are only read.
//!
//! Output (stdout, tab-separated):
//! - `GROUP key photos adaptive with_jpeg`: per camera and style after reading the makernotes
//! - `SELECT key day path`: the photos learning would use (with `--select-only` nothing else happens)
//! - `PHOTO key name day source pairs correlation exposure_offset`, `SKIP key name reason`
//! - `METRIC key variant part photos blocks median p90 p95 mean`: `variant` neutral (plain development), current
//!   (standard development with the built-in hue table and saturation), matrix, curves, table, and bundled (the
//!   bundled camera look of the base style, if there is one); `part` train/check
//! - `DECISION key text`, `RESULT key stage|none reason`, `WARNING key warning`
//!
//! `--include-adaptive` also uses photos with adaptive camera settings (DR Auto, shadow correction …), which the app
//! leaves out unless asked. `--out DIR` writes each kept look as `DIR/<camera>/<style>.x3look`, `--cube DIR` as `.cube`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rawdev::learn::{self, Candidate, Scanned};

pub fn run(args: &[String]) -> Result<()> {
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let only = value("--key");
    let select_only = args.iter().any(|a| a == "--select-only");
    let include_adaptive = args.iter().any(|a| a == "--include-adaptive");
    let out = value("--out").map(PathBuf::from);
    let cube = value("--cube").map(PathBuf::from);
    let list = value("--list").context("--list CANDIDATES.tsv missing")?;
    let mut cands = Vec::new();
    for l in std::fs::read_to_string(&list)?.lines() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let t: Vec<&str> = l.split('\t').collect();
        let path = PathBuf::from(t[0]);
        let day = t.get(1).map(|s| s.to_string()).unwrap_or_default();
        let folder = t.get(2).map(|s| s.to_string()).unwrap_or_else(|| path.parent().map(|p| p.display().to_string()).unwrap_or_default());
        cands.push(Candidate { path, day, folder, camera: String::new(), thumb: None });
    }
    // picture styles through the app's cache (in GRAINEO_DATA_DIR/cache for test runs)
    let mut groups: BTreeMap<String, Vec<Scanned>> = BTreeMap::new();
    for s in learn::scan_all(&cands, 4, &|| true, &|d, n| eprint!("\rstyles {d}/{n}   ")).unwrap_or_default() {
        groups.entry(s.key()).or_default().push(s);
    }
    eprintln!();
    for (key, v) in &groups {
        println!("GROUP\t{key}\t{}\t{}\t{}", v.len(), v.iter().filter(|s| s.adaptive.is_some()).count(), v.iter().filter(|s| s.has_jpeg).count());
    }
    let today = {
        let t = library::platform::local_time(library::now());
        format!("{:04}-{:02}-{:02}", t.year, t.month, t.tag)
    };
    for (key, v) in &groups {
        if only.as_ref().is_some_and(|k| k != key) {
            continue;
        }
        if select_only {
            let usable: Vec<&Scanned> = v.iter().filter(|s| include_adaptive || s.adaptive.is_none()).collect();
            let keys: Vec<(String, String)> = usable.iter().map(|s| (s.candidate.day.clone(), name(&s.candidate.path))).collect();
            for i in raw::learn_look::select(&keys, raw::learn_look::MAX_PHOTOS) {
                println!("SELECT\t{key}\t{}\t{}", usable[i].candidate.day, usable[i].candidate.path.display());
            }
            continue;
        }
        let t0 = std::time::Instant::now();
        let Some(r) = learn::learn(v, include_adaptive, &today, None, &|| true, &|e| {
            if let learn::Event::Photo { done, total, .. } = e {
                eprint!("\r{key}: {done}/{total}   ")
            }
        }) else {
            continue;
        };
        eprintln!();
        for p in &r.photos {
            println!("PHOTO\t{key}\t{}\t{}\t{}\t{}\t{:.3}\t{:+.2}", p.name, p.day, p.source.map_or("-", |s| s.key()), p.pairs.len(), p.correlation, p.exposure_offset);
        }
        for (n, e) in &r.skipped {
            println!("SKIP\t{key}\t{n}\t{e}");
        }
        let o = &r.outcome;
        for (variant, t, c) in &o.variants {
            for (part, m) in [("train", t), ("check", c)] {
                println!("METRIC\t{key}\t{variant}\t{part}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}", m.photos, m.blocks, m.median, m.p90, m.p95, m.mean);
            }
        }
        // for comparison only: the bundled look of the base style (fitted earlier, possibly on some of the same photos)
        let base_style = key.split_once('/').map_or("", |(_, st)| st).split('_').next().unwrap_or("");
        let camera = key.split_once('/').map_or("", |(c, _)| c);
        if let Some(bundled) = raw::looks::Look::built_in(&format!("{camera}/{base_style}")).filter(|_| !r.photos.is_empty()) {
            let usable: Vec<raw::learn_look::Photo> = r.photos.iter().filter(|p| p.pairs.len() >= 20).cloned().collect();
            let (ti, ci) = raw::learn_look::split_by_day(&usable, key);
            for (part, idx) in [("train", &ti), ("check", &ci)] {
                let set: Vec<&raw::learn_look::Photo> = idx.iter().map(|&i| &usable[i]).collect();
                let m = raw::learn_look::measure(&set, &|q| bundled.pixel(q.base));
                println!("METRIC\t{key}\tbundled\t{part}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}", m.photos, m.blocks, m.median, m.p90, m.p95, m.mean);
            }
        }
        for d in &o.decisions {
            println!("DECISION\t{key}\t{d}");
        }
        for w in &r.warnings {
            println!("WARNING\t{key}\t{w:?}");
        }
        println!(
            "RESULT\t{key}\t{}\t{}\ttrain {} photos/{} days, check {} photos/{} days, {:.0} s",
            o.stage.map_or("none", |s| s.key()),
            o.refused.clone().unwrap_or_default(),
            o.train_photos,
            o.train_days,
            o.check_photos,
            o.check_days,
            t0.elapsed().as_secs_f32()
        );
        if let Some(look) = &o.look {
            let (camera, style) = key.split_once('/').unwrap_or((key, ""));
            if let Some(d) = &out {
                let p = d.join(camera).join(format!("{style}.x3look"));
                std::fs::create_dir_all(p.parent().unwrap())?;
                std::fs::write(&p, look.to_bytes().map_err(anyhow::Error::msg)?)?;
            }
            if let Some(d) = &cube {
                std::fs::create_dir_all(d)?;
                let text = look.cube_text(&format!("graineo learned look {key}"), 1.0).map_err(anyhow::Error::msg)?;
                std::fs::write(d.join(format!("{camera}-{style}.cube")), text)?;
            }
        }
    }
    Ok(())
}

fn name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}
