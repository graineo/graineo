//! `graineo-dev pairs-measure <folder under /private/tmp> [pairs] [single|blocks] [filler]`
//!
//! Measures removing the JPEGs of RAW+JPEG pairs (the same code as the app: `library::pairs::remove`
//! and `library::pairs::dispose` with the shared block building block `library::trash_batch`). The system Trash is
//! replaced by a stand-in that moves the files into `<folder>/_papierkorb` (the user's Trash is never touched); the
//! number of Trash requests is printed – each real request costs about 0.25–1 s. Builds an own test library with `pairs` pairs of small test
//! files (`graineo-testpaar-NNNNN.CR2/.JPG`) plus `filler` further photos (only in the database, for a realistic
//! size). "einzeln" removes one pair per call, like before the change (one Trash request and one transaction per
//! JPEG); "bloecke" (default) as the app does now. Only ever on own test files.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Result};
use library::db::Db;
use library::pairs::{self, Disposal, Level, Options, Outcome};

fn build(dir: &Path, n: usize, filler: usize) -> Result<Db> {
    if !dir.starts_with("/private/tmp/") {
        bail!("only in an own folder under /private/tmp");
    }
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    let photos = dir.join("photos");
    std::fs::create_dir_all(&photos)?;
    let db = Db::open(&dir.join("library.sqlite"))?;
    let tx = db.conn.unchecked_transaction()?;
    tx.execute("INSERT INTO root_folder(path, name, origin) VALUES(?1, 'fotos', 'own')", [format!("{}/", photos.display())])?;
    let root = tx.last_insert_rowid();
    tx.execute("INSERT INTO folder(root, path) VALUES(?1, '')", [root])?;
    let folder = tx.last_insert_rowid();
    tx.execute("INSERT INTO folder(root, path) VALUES(?1, 'filler/')", [root])?;
    let filler_folder = tx.last_insert_rowid();
    {
        let mut file = tx.prepare("INSERT INTO file(folder, name, format, size, camera, capture_time, width, height, status) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 6000, 4000, ?7)")?;
        let mut image = tx.prepare("INSERT INTO image(file) VALUES(?1)")?;
        for i in 0..n {
            let capture = format!("2020-01-{:02}T{:02}:{:02}:{:02}", 1 + i / 86_400, i / 3600 % 24, i / 60 % 60, i % 60);
            for (ext, size) in [("CR2", 64usize), ("JPG", 2048)] {
                let name = format!("graineo-testpaar-{i:05}.{ext}");
                std::fs::write(photos.join(&name), vec![0u8; size])?;
                file.execute((folder, &name, ext, size as i64, "Testkamera", &capture, 0))?;
                image.execute([tx.last_insert_rowid()])?;
            }
        }
        for i in 0..filler {
            file.execute((filler_folder, format!("filler-{i:07}.JPG"), "JPG", 1_000_000i64, "Andere", "2019-05-05T10:00:00", 1))?;
            image.execute([tx.last_insert_rowid()])?;
        }
    }
    tx.commit()?;
    Ok(db)
}

pub fn run(args: &[String]) -> Result<()> {
    let Some(dir) = args.first().map(PathBuf::from) else { bail!("usage: pairs-measure <folder under /private/tmp> [pairs] [single|blocks] [filler]") };
    let n: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(1000);
    let single = args.get(2).is_some_and(|m| m == "single");
    let filler: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(200_000);
    let t = Instant::now();
    let db = build(&dir, n, filler)?;
    println!("library: {n} pairs + {filler} photos in {:.1} s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let found = pairs::find(&db).map_err(anyhow::Error::msg)?;
    println!("find: {} pairs in {:.2} s", found.len(), t.elapsed().as_secs_f64());
    if found.len() != n || found.iter().any(|p| p.level != Level::Safe) {
        bail!("unexpected pairs");
    }
    // stand-in for the system Trash: one rename per file into a folder of the test
    let bin = dir.join("_papierkorb");
    std::fs::create_dir_all(&bin)?;
    let stand_in = |files: &[PathBuf]| -> Result<(), String> {
        for f in files {
            std::fs::rename(f, bin.join(f.file_name().unwrap_or_default())).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let disposing = std::cell::Cell::new(0.0f64);
    let calls = std::cell::Cell::new(0usize);
    let dispose = |db: &Db, files: &[PathBuf]| -> Result<usize, String> {
        let t = Instant::now();
        let r = pairs::dispose(db, files, Disposal::Trash, &stand_in);
        disposing.set(disposing.get() + t.elapsed().as_secs_f64());
        calls.set(calls.get() + 1);
        r
    };
    let opt = Options { disposal: Disposal::Trash, transfer: true };
    let t = Instant::now();
    let outcome = if single {
        let mut all = Outcome::default();
        for p in found.chunks(1) {
            let o = pairs::remove(&db, p, opt, &dispose, &|_, _| {}, &|| false);
            all.removed += o.removed;
            all.skipped.extend(o.skipped);
            all.errors.extend(o.errors);
        }
        all
    } else {
        pairs::remove(&db, &found, opt, &dispose, &|_, _| {}, &|| false)
    };
    let total = t.elapsed().as_secs_f64();
    println!(
        "remove ({}): {} JPEGs in {total:.1} s ({:.1} ms per JPEG); Trash {:.1} s in {} requests, checks and database {:.1} s; skipped {}, errors {}",
        if single { "single" } else { "blocks" },
        outcome.removed,
        total * 1000.0 / n as f64,
        disposing.get(),
        calls.get(),
        total - disposing.get(),
        outcome.skipped.len(),
        outcome.errors.len()
    );
    for e in outcome.errors.iter().take(3) {
        println!("  error: {e}");
    }
    let photos = dir.join("photos");
    let left_jpeg = (0..n).filter(|i| photos.join(format!("graineo-testpaar-{i:05}.JPG")).exists()).count();
    let left_raw = (0..n).filter(|i| photos.join(format!("graineo-testpaar-{i:05}.CR2")).exists()).count();
    let rows: i64 = db.conn.query_row("SELECT count(*) FROM file d JOIN folder o ON o.id = d.folder WHERE d.format = 'JPG' AND o.path = ''", [], |r| r.get(0))?;
    println!("check: JPEG files left {left_jpeg}, RAW files left {left_raw} of {n}, JPEG rows left {rows}");
    Ok(())
}
