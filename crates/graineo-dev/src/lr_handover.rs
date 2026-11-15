//! Handover of the Lightroom develop data into the sidecars (`library::lr_handover`):
//!
//! - `graineo-dev lr-handover-dry-run --catalog K [--library DB] [--log FILE] [--write]`
//!   (also `lr-uebergabe-pruefen`): check run on a library opened **read-only** – only counts (written /
//!   already there / photo not found / target unreachable …), nothing is written. `--write` really
//!   writes the sidecars; only allowed with `GRAINEO_DATA_DIR` (test folders).
//! - `graineo-dev lr-handover-check --catalog K [--library DB] [--render]`:
//!   for every photo of the library that exists on disk, compares the starting values as the app
//!   computed them from the catalog until now with those it computes from the sidecar now (orientation,
//!   settings; with `--render` also the developed preview, bit for bit). Virtual copies: their own
//!   Lightroom settings from the catalog against those in the sidecar.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Result};

fn value(args: &[String], n: &str) -> Option<String> {
    args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned()
}

fn catalog_arg(args: &[String]) -> PathBuf {
    value(args, "--catalog").map(PathBuf::from).unwrap_or_else(crate::dev_catalog)
}

fn library_arg(args: &[String]) -> PathBuf {
    value(args, "--library").map(PathBuf::from).unwrap_or_else(library::db_path)
}

pub fn dry_run(args: &[String]) -> Result<()> {
    let write = args.iter().any(|a| a == "--write");
    if write && std::env::var_os("GRAINEO_DATA_DIR").is_none() {
        bail!("--write only with GRAINEO_DATA_DIR (test folder)");
    }
    let catalog = catalog_arg(args);
    let db_path = library_arg(args);
    let db = if write { library::db::Db::open(&db_path)? } else { library::db::Db::read_only(&db_path)? };
    let r = library::lr_handover::run(&db, &catalog, write, &|d, t| eprint!("\r{d} / {t}"), &AtomicBool::new(false)).map_err(anyhow::Error::msg)?;
    eprintln!();
    println!("{}", serde_json::to_string_pretty(&r.json())?);
    if let Some(p) = value(args, "--log") {
        let mut t = String::new();
        for l in &r.log {
            t.push_str(l);
            t.push('\n');
        }
        std::fs::write(&p, t)?;
        eprintln!("Protokoll: {p}");
    }
    Ok(())
}

fn rgb_hash(img: &develop::Rgb16) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (img.width, img.height).hash(&mut h);
    img.to_rgb8().hash(&mut h);
    h.finish()
}

fn render(doc: &rawdev::RawDoc, s: &develop::Settings) -> u64 {
    rgb_hash(&develop::geometry::apply(doc.render_preview(s), s, doc.orientation))
}

pub fn check(args: &[String]) -> Result<()> {
    let catalog = catalog_arg(args);
    let db = library::db::Db::read_only(&library_arg(args))?;
    let do_render = args.iter().any(|a| a == "--render");
    let files: Vec<(String, String)> = {
        let mut st = db.conn.prepare(
            "SELECT s.path || o.path || d.name, d.format FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root
             WHERE d.format NOT IN ('X3F', 'VIDEO') ORDER BY 1",
        )?;
        let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<Vec<(String, String)>, _>>()?;
        r
    };
    let (mut same, mut differ, mut skipped, mut copies_same, mut copies_differ) = (0, 0, 0, 0, 0);
    for (p, _format) in &files {
        let path = Path::new(p);
        if !path.is_file() {
            skipped += 1;
            continue;
        }
        let Some(mut doc) = rawdev::RawDoc::open(path, &|| true).map_err(anyhow::Error::msg)? else { continue };
        // without the catalog (archived): only what the app computes now, from the sidecar
        if !catalog.exists() {
            rawdev::lightroom::apply_orientation(&mut doc);
            let s = rawdev::lightroom::initial_settings(&doc);
            let h = do_render.then(|| render(&doc, &s.clone().unwrap_or_default()));
            println!("NACHHER\t{}\torientation {}\t{}", path.display(), doc.orientation, h.map(|h| format!("bild {h:016x}")).unwrap_or_default());
            if let library::xmp::ReadResult::Ok(w) = library::xmp::read(&library::xmp::path_to(path)) {
                for k in library::sidecar::read_copies(w.copies_json.as_deref()).into_iter().filter(|k| k.lightroom.is_some()) {
                    let s = rawdev::lightroom::copy_settings(&doc, &k.name);
                    let h = do_render.then(|| render(&doc, &s.clone().unwrap_or_default()));
                    println!("NACHHER\t{} [Kopie „{}“]\t{}", path.display(), k.name, h.map(|h| format!("bild {h:016x}")).unwrap_or_default());
                }
            }
            same += 1;
            continue;
        }
        let o0 = doc.orientation;
        rawdev::lightroom::apply_orientation_from_catalog(&mut doc, &catalog);
        let (oa, sa) = (doc.orientation, rawdev::lightroom::initial_settings_from_catalog(&doc, &catalog));
        let ra = do_render.then(|| render(&doc, &sa.clone().unwrap_or_default()));
        doc.orientation = o0;
        rawdev::lightroom::apply_orientation(&mut doc);
        let (ob, sb) = (doc.orientation, rawdev::lightroom::initial_settings(&doc));
        let rb = do_render.then(|| render(&doc, &sb.clone().unwrap_or_default()));
        let ja = serde_json::to_string(&sa)?;
        let jb = serde_json::to_string(&sb)?;
        let ok = oa == ob && ja == jb && ra == rb;
        let what = |s: &Option<develop::Settings>| match s {
            None => "ohne".to_string(),
            Some(s) => format!("crop {:?} angle {:.2} local {} spots {}", s.crop, s.crop_angle, s.local.is_some(), s.spots.len()),
        };
        println!(
            "{}\t{}\torientation {o0}→{oa}/{ob}\t{}{}",
            if ok { "EQUAL" } else { "DIFFERENT" },
            path.display(),
            what(&sb),
            ra.map(|a| format!("\tbild {a:016x}/{:016x}", rb.unwrap_or(0))).unwrap_or_default()
        );
        if ok {
            same += 1;
        } else {
            differ += 1;
            if args.iter().any(|a| a == "--show") {
                println!("  before: {ja}\n  after: {jb}");
            }
        }
        // virtual copies: own Lightroom settings from the catalog ↔ from the sidecar
        doc.orientation = ob;
        for e in library::lr_develop::read(&catalog, path).map_err(anyhow::Error::msg)?.into_iter().filter(|e| e.copy.is_some()) {
            let name = e.copy.clone().unwrap_or_default();
            let sa = rawdev::lightroom::settings_from_text(&doc, &e.text);
            let sb = rawdev::lightroom::copy_settings(&doc, &name);
            let (ja, jb) = (serde_json::to_string(&sa)?, serde_json::to_string(&sb)?);
            let (ra, rb) = if do_render { (Some(render(&doc, &sa.clone().unwrap_or_default())), Some(render(&doc, &sb.clone().unwrap_or_default()))) } else { (None, None) };
            let ok = ja == jb && ra == rb;
            println!("{}\t{} [Kopie „{name}“]\t{}{}", if ok { "EQUAL" } else { "DIFFERENT" }, path.display(), what(&sb), ra.map(|a| format!("\tbild {a:016x}/{:016x}", rb.unwrap_or(0))).unwrap_or_default());
            if ok {
                copies_same += 1;
            } else {
                copies_differ += 1;
            }
        }
    }
    println!("{}", serde_json::json!({"equal": same, "different": differ, "not_on_disk": skipped, "copies_equal": copies_same, "copies_different": copies_differ}));
    if differ + copies_differ > 0 {
        bail!("{} Unterschiede", differ + copies_differ);
    }
    Ok(())
}
