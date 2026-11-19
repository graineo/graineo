//! `graineo-dev formats <folder> [--out DIR] [--export] [--import DIR]`: format check over a folder of sample files.
//!
//! For every file (recursively, hidden entries skipped) it runs the steps a photo goes through in the app and prints
//! one tab-separated line `FORMAT\t…` (header line `FORMAT\tfile\t…` first):
//!
//! - **grid**: format from the extension (`library::format_from_extension`, the list behind import, folder view and
//!   sync); `-` = the app does not show the file
//! - **thumb / large**: `library::preview::load` at 400 px and 2560 px (embedded preview or decode), time and size
//! - **info**: the metadata the info panel and the library get (`library::exif::read`: ImageIO on the Mac) and the
//!   platform-independent header reader (`read_portable`): camera, lens, capture time, exposure
//! - **develop**: `rawdev::RawDoc::open` + preview rendering (the edit view), full size, ΔE00 against the embedded
//!   preview of the file as a rough plausibility check of colors and white balance (not a quality measure)
//! - **export** (`--export`): full resolution JPEG and 16-bit TIFF via `rawdev::export`, times and sizes
//! - **import** (`--import DIR`): copies all files into `DIR/photos` with the import of the app (`card_import`,
//!   own library `DIR/library.sqlite`), reads the shooting data as the app does afterwards (`exif::backfill`), then
//!   checks the catalog entry and makes the small preview in `DIR/previews`
//!
//! Every step is guarded against panics. Run it only with `GRAINEO_DATA_DIR` set to a scratch folder and
//! `GRAINEO_NO_XMP=1` (nothing is written next to the files). `scripts/format-check.sh` wraps it and builds a table.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};

fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => Err(format!("PANIC {}", p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())),
    }
}

fn clean(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ").chars().take(160).collect()
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn summary(c: &library::exif::Capture) -> String {
    let mut v = Vec::new();
    v.push(c.camera.clone().unwrap_or_else(|| "?".into()));
    v.push(c.lens.clone().unwrap_or_else(|| "-".into()));
    v.push(c.capture.clone().unwrap_or_else(|| "-".into()));
    let mut shot = String::new();
    if let Some(a) = c.aperture {
        shot += &format!("f/{a:.1} ");
    }
    if let Some(t) = c.time {
        shot += &if t < 1.0 { format!("1/{:.0}s ", 1.0 / t) } else { format!("{t:.1}s ") };
    }
    if let Some(i) = c.iso {
        shot += &format!("ISO{i}");
    }
    v.push(if shot.is_empty() { "-".into() } else { shot.trim().to_string() });
    clean(&v.join(" | "))
}

fn preview_step(p: &Path, format: &str, max: u32) -> String {
    let t = Instant::now();
    match guarded(|| library::preview::load(p, format, max)) {
        Ok((img, m)) => format!("ok {}x{} {:.0}ms{}", img.width(), img.height(), t.elapsed().as_secs_f64() * 1000.0, if m.camera.is_none() && m.capture.is_none() { " nometa" } else { "" }),
        Err(e) => format!("ERR {:.0}ms {}", t.elapsed().as_secs_f64() * 1000.0, clean(&e)),
    }
}

struct Developed {
    text: String,
    export: String,
}

fn develop_step(p: &Path, out: Option<&Path>, export: bool) -> Developed {
    if !rawdev::is_editable(p) {
        return Developed { text: "-".into(), export: "-".into() };
    }
    let stem = p.file_name().map(|n| n.to_string_lossy().replace('.', "_")).unwrap_or_default();
    let t = Instant::now();
    let doc = match guarded(|| rawdev::RawDoc::open(p, &|| true).and_then(|d| d.ok_or_else(|| "cancelled".to_string()))) {
        Ok(d) => d,
        Err(e) => return Developed { text: format!("ERR {}", clean(&e)), export: "-".into() },
    };
    let t_open = t.elapsed();
    let s = develop::Settings::default();
    let t = Instant::now();
    let rendered = guarded(|| {
        let st = doc.preview_stage(&s);
        Ok(doc.render_stage(&st, &s))
    });
    let img = match rendered {
        Ok(i) => i,
        Err(e) => return Developed { text: format!("ERR render {}", clean(&e)), export: "-".into() },
    };
    let t_render = t.elapsed();
    let img = develop::geometry::apply(img, &s, doc.orientation);
    let own = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8());
    if let (Some(dir), Some(own)) = (out, &own) {
        let _ = own.save(dir.join(format!("{stem}-develop.jpg")));
    }
    // rough color check against the embedded camera preview (sensor orientation, turned like our output)
    let de = if doc.finished {
        String::new()
    } else {
        guarded(|| {
            let own = own.as_ref().ok_or("image")?;
            let r = raw::RawFile::open(p).map_err(|e| e.to_string())?.preview().ok_or("no embedded preview")?;
            let (r, _) = crate::raw_compare::reference_oriented(r, (own.width(), own.height()), doc.orientation, true);
            if let Some(dir) = out {
                let _ = r.save(dir.join(format!("{stem}-embedded.jpg")));
            }
            Ok(crate::raw_compare::compare_images(own, &r, false).de)
        })
        .map_or_else(|e| format!(" dE00 - ({})", clean(&e)), |d| format!(" dE00 {d:.1}"))
    };
    let mut text = format!(
        "ok {}x{} open {:.0}ms render {:.0}ms wb {:.0}K{}{}",
        doc.full_size[0],
        doc.full_size[1],
        t_open.as_secs_f64() * 1000.0,
        t_render.as_secs_f64() * 1000.0,
        doc.as_shot.0,
        de,
        doc.apple.as_ref().map(|_| " (system engine)").unwrap_or("")
    );
    if doc.finished {
        text += " (finished image)";
    }
    let export = if export {
        let dir = out.map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
        let mut parts = Vec::new();
        for (fmt, ext) in [(develop::export::Format::Jpeg { quality: 90 }, "jpg"), (develop::export::Format::Tiff16, "tif")] {
            let target = dir.join(format!("{stem}-export.{ext}"));
            let t = Instant::now();
            let r = guarded(|| rawdev::export::export(&doc, &s, fmt, &target));
            parts.push(match r {
                Ok(()) => {
                    let size = library::imageio::properties(&target).map(|e| format!("{}x{}", e.width.unwrap_or(0), e.height.unwrap_or(0))).unwrap_or_else(|| "?".into());
                    let bytes = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
                    // only the result line is kept; the exported files are large
                    if out.is_none() || ext == "tif" {
                        let _ = std::fs::remove_file(&target);
                    }
                    format!("{ext} ok {size} {:.1}s {:.1}MB", t.elapsed().as_secs_f64(), bytes as f64 / 1e6)
                }
                Err(e) => format!("{ext} ERR {}", clean(&e)),
            });
        }
        parts.join(", ")
    } else {
        "-".into()
    };
    doc.release_memory();
    Developed { text, export }
}

/// Copy every file into `dir/photos` with the app's import and check catalog entry and small preview per file name.
fn import_step(source: &Path, dir: &Path) -> Result<HashMap<String, String>> {
    use library::card_import::{self as ci, Mode, Options};
    let photos = dir.join("photos");
    std::fs::create_dir_all(&photos)?;
    let db_path = dir.join("library.sqlite");
    let db = library::db::Db::open(&db_path).context("library")?;
    library::reconcile::add_root(&db, &photos, &|_| {}).map_err(anyhow::Error::msg)?;
    let candidates = ci::scan(&db, source);
    let opt = Options { mode: Mode::CopyFiles, source: Some(source.to_path_buf()), target: Some(photos.clone()), by_date: false, ..Options::default() };
    let open = || library::db::Db::open(&db_path).map_err(|e| e.to_string());
    let never = std::sync::atomic::AtomicBool::new(false);
    let no_trash = |_: &[PathBuf]| -> Result<(), String> { Err("copy only".into()) };
    let o = ci::run_import(&open, &candidates, &opt, &no_trash, &never, &|_| {});
    eprintln!("import: {} copied, {} added, error {:?}, notes {:?}", o.copy_file.copied, o.added, o.catalog_error, o.notes);
    // the app reads the shooting data right after the import in the background (camera, capture time, lens …)
    let t = Instant::now();
    let read = library::exif::backfill(&db, usize::MAX, &never).map_err(anyhow::Error::msg)?;
    eprintln!("shooting data of {read} files read in {:.1} s", t.elapsed().as_secs_f64());
    let cache = library::preview::Cache::open(&dir.join("previews"));
    let mut out = HashMap::new();
    for (path, file) in &o.files {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let row: Option<(String, Option<String>, Option<String>, Option<i64>, Option<i64>)> = db
            .conn
            .query_row("SELECT format, camera, capture_time, width, height FROM file WHERE id=?1", [file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .ok();
        let Some((format, camera, capture, w, h)) = row else {
            out.insert(name, "ERR no catalog entry".into());
            continue;
        };
        let meta = std::fs::metadata(path).ok();
        let mtime = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
        let key = library::preview::cache_key(&path.to_string_lossy(), meta.map_or(0, |m| m.len() as i64), mtime);
        let job = library::preview::Job { file: *file, path: path.clone(), format: format.clone(), key, level: library::preview::small(), orientation: None };
        let t = Instant::now();
        let thumb = match guarded(|| library::preview::ensure(&cache, &job)) {
            Ok(_) if cache.has(key, job.level) => format!("preview {:.0}ms", t.elapsed().as_secs_f64() * 1000.0),
            Ok(_) => "preview missing".into(),
            Err(e) => format!("preview ERR {}", clean(&e)),
        };
        let size = match (w, h) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => "no size".into(),
        };
        out.insert(name, format!("{format} {} | {} | {size} | {thumb}", camera.as_deref().unwrap_or("no camera"), capture.as_deref().unwrap_or("no date")));
    }
    for c in &candidates {
        let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        out.entry(name).or_insert_with(|| "ERR not imported".into());
    }
    Ok(out)
}

pub fn run(args: &[String]) -> Result<()> {
    let Some(dir) = args.first().map(PathBuf::from) else {
        bail!("usage: graineo-dev formats <folder> [--out DIR] [--export] [--import DIR]");
    };
    if std::env::var_os("GRAINEO_DATA_DIR").is_none() {
        bail!("set GRAINEO_DATA_DIR to a scratch folder (and GRAINEO_NO_XMP=1)");
    }
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let export = args.iter().any(|a| a == "--export");
    let out = value("--out");
    if let Some(o) = &out {
        std::fs::create_dir_all(o)?;
    }
    let imported = match value("--import") {
        Some(d) => import_step(&dir, &d)?,
        None => HashMap::new(),
    };
    println!("FORMAT\tfile\text\tgrid\tthumb400\tlarge2560\tinfo\tinfo_portable\tdevelop\texport\timport");
    for p in files_in(&dir) {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
        let format = library::format_from_extension(&name);
        let Some(format) = format else {
            println!("FORMAT\t{name}\t{ext}\t-\t-\t-\t-\t-\t-\t-\t-");
            continue;
        };
        let thumb = preview_step(&p, format, 400);
        let large = preview_step(&p, format, 2560);
        let info = guarded(|| Ok(library::exif::read(&p, format))).ok().flatten().map_or("-".into(), |c| summary(&c));
        let portable = guarded(|| Ok(library::exif::read_portable(&p, format))).ok().flatten().map_or("-".into(), |c| summary(&c));
        let d = develop_step(&p, out.as_deref(), export);
        let imp = imported.get(&name).cloned().unwrap_or_else(|| "-".into());
        println!("FORMAT\t{name}\t{ext}\t{format}\t{thumb}\t{large}\t{info}\t{portable}\t{}\t{}\t{}", d.text, d.export, clean(&imp));
    }
    Ok(())
}
