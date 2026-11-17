//! `graineo-dev series-measure <folder under /private/tmp> [photos]` and `graineo-dev series-stats <library.sqlite>`
//!
//! - **series-measure** builds an own synthetic library (default 65,000 photos, three cameras, gaps between shots
//!   drawn from a realistic distribution, image signatures that stay close within a scene and jump at a change of
//!   subject) and measures the grouping of `library::series`: first run over all days, a run without changes, a run
//!   after one new photo, loading the stacks for the grid. Function check only, nothing of the user's.
//! - **series-stats** groups a *copy* of a library (`VACUUM INTO` a temp file; the given file is opened read-only)
//!   and prints the size distribution of the series.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Result};
use library::db::Db;
use library::series;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn unit(&mut self) -> f64 {
        self.next() as f64 / (1u64 << 31) as f64
    }
}

/// Seconds to the previous photo, distributed like the measurement in the concept (share per class).
fn gap(r: &mut Rng) -> i64 {
    let u = r.unit();
    let (lo, hi) = match u {
        x if x < 0.10 => (0, 1),
        x if x < 0.15 => (1, 3),
        x if x < 0.29 => (3, 10),
        x if x < 0.47 => (10, 30),
        x if x < 0.66 => (30, 120),
        _ => (120, 3600),
    };
    lo + (r.unit() * (hi - lo) as f64) as i64
}

fn scene_signature(r: &mut Rng) -> Vec<u8> {
    let mut s = vec![0u8; series::SIGNATURE_LEN];
    for b in s.iter_mut() {
        *b = (r.next() & 0xff) as u8;
    }
    s
}

fn variant(r: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut s = base.to_vec();
    // a few hash bits flipped, the thumbnail a little noisy
    for _ in 0..(r.next() % 6) {
        let bit = r.next() % 64;
        s[(bit / 8) as usize] ^= 1 << (bit % 8);
    }
    for b in s[8..].iter_mut() {
        *b = (*b as i32 + (r.next() % 7) as i32 - 3).clamp(0, 255) as u8;
    }
    s
}

/// An own test folder below /private/tmp: no `.`/`..` components, not /private/tmp itself, and – when it exists –
/// no symbolic link and still below /private/tmp after resolving links.
fn own_tmp_folder(dir: &Path) -> Result<()> {
    let base = Path::new("/private/tmp");
    // (textual: `Path::components` drops inner "." silently)
    if !dir.is_absolute() || dir.to_string_lossy().split('/').any(|c| c == "." || c == "..") {
        bail!("only an absolute path without \".\" or \"..\": {}", dir.display());
    }
    if !dir.starts_with(base) || dir == base {
        bail!("only in an own folder under /private/tmp");
    }
    if let Ok(m) = std::fs::symlink_metadata(dir) {
        if m.file_type().is_symlink() {
            bail!("not a symbolic link: {}", dir.display());
        }
        let real = dir.canonicalize()?;
        if !real.starts_with(base) || real == base {
            bail!("resolves outside /private/tmp: {}", real.display());
        }
    }
    Ok(())
}

fn build(dir: &Path, n: usize) -> Result<Db> {
    own_tmp_folder(dir)?;
    if dir.exists() {
        // only an earlier measurement's folder (holds its library and nothing else of note) is replaced
        let real = dir.canonicalize()?;
        own_tmp_folder(&real)?;
        if !real.join("library.sqlite").exists() && std::fs::read_dir(&real)?.next().is_some() {
            bail!("{} exists and is not an earlier measurement: not removed", real.display());
        }
        std::fs::remove_dir_all(&real)?;
    }
    std::fs::create_dir_all(dir)?;
    let db = Db::open(&dir.join("library.sqlite"))?;
    let tx = db.conn.unchecked_transaction()?;
    tx.execute("INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/Users/Shared/graineo-series-test/', 'test', 'own')", [])?;
    tx.execute("INSERT INTO folder(id, root, path) VALUES(1, 1, '')", [])?;
    let mut r = Rng(20261116);
    let cameras = ["Camera A", "Camera B", "Camera C"];
    // start: 2015-01-01, about 3,000 shooting days
    let mut t: i64 = library::pairs::seconds("2015-01-01T09:00:00").unwrap();
    let mut base = scene_signature(&mut r);
    let mut cam = 0usize;
    for id in 1..=n as i64 {
        let g = gap(&mut r);
        // a new shooting day now and then (≈ 22 photos per day)
        if r.unit() < 1.0 / 22.0 {
            t += 86_400 + (r.unit() * 4.0 * 86_400.0) as i64;
            cam = (r.next() % 3) as usize;
        } else {
            t += g;
        }
        if g > 10 && r.unit() < 0.6 || g <= 10 && r.unit() < 0.15 {
            base = scene_signature(&mut r);
        }
        let capture = {
            let days = t.div_euclid(86_400);
            let s = t.rem_euclid(86_400);
            // civil date from days since 1970 (inverse of `pairs::seconds`)
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = yoe + era * 400 + (m <= 2) as i64;
            format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
        };
        tx.execute(
            "INSERT INTO file(id, folder, name, format, camera, capture_time, aperture, time, iso) VALUES(?1, 1, ?2, 'JPG', ?3, ?4, 4.0, 0.004, 200)",
            rusqlite::params![id, format!("IMG_{id:06}.JPG"), cameras[cam], capture],
        )?;
        tx.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id])?;
        let sig = variant(&mut r, &base);
        tx.execute("INSERT INTO image_quality(file, version, signature, computed) VALUES(?1, 1, ?2, 0)", rusqlite::params![id, sig])?;
    }
    tx.commit()?;
    Ok(db)
}

fn print_distribution(db: &Db, photos: usize) -> Result<()> {
    let (stacks, inside, b, largest) = series::distribution(&db.conn).map_err(anyhow::Error::msg)?;
    println!(
        "series: {stacks} with {inside} photos ({:.0} % of {photos}); 2–3: {}, 4–9: {}, 10–29: {}, ≥ 30: {}; largest {largest}",
        inside as f64 * 100.0 / photos.max(1) as f64,
        b[0],
        b[1],
        b[2],
        b[3]
    );
    Ok(())
}

pub fn measure(args: &[String]) -> Result<()> {
    let dir = PathBuf::from(args.first().map(String::as_str).unwrap_or("/private/tmp/graineo-series-measure"));
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(65_000);
    let t = Instant::now();
    let db = build(&dir, n)?;
    println!("built: {n} photos in {:.1} s ({})", t.elapsed().as_secs_f64(), dir.display());
    let no = || false;
    let s = series::regroup(&db, false, &no).map_err(anyhow::Error::msg)?;
    println!("first grouping: {} days, {} stacks in {} ms", s.days, s.stacks_created, s.ms);
    print_distribution(&db, n)?;
    let s = series::regroup(&db, false, &no).map_err(anyhow::Error::msg)?;
    println!("run without changes: {} days grouped again, {} ms", s.regrouped_days, s.ms);
    // one more photo on the last day
    let (last, cap): (i64, String) = db.conn.query_row("SELECT id, capture_time FROM file ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let id = last + 1;
    db.conn.execute("INSERT INTO file(id, folder, name, format, camera, capture_time) VALUES(?1, 1, 'IMG_NEW.JPG', 'JPG', 'Camera A', ?2)", rusqlite::params![id, cap])?;
    db.conn.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id])?;
    let s = series::regroup(&db, false, &no).map_err(anyhow::Error::msg)?;
    println!("run after one new photo: {} day(s) grouped again, {} ms", s.regrouped_days, s.ms);
    let t = Instant::now();
    let stacks = series::load(&db.conn).map_err(anyhow::Error::msg)?;
    let members: usize = stacks.iter().map(|s| s.images.len()).sum();
    println!("grid: {} stacks with {members} photos loaded in {:.1} ms", stacks.len(), t.elapsed().as_secs_f64() * 1000.0);
    let t = Instant::now();
    let open = series::open_count(&db.conn);
    println!("open series: {open} (counted in {:.1} ms)", t.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}

pub fn stats(args: &[String]) -> Result<()> {
    let Some(src) = args.first().map(PathBuf::from) else { bail!("usage: graineo-dev series-stats <library.sqlite>") };
    let copy = std::env::temp_dir().join(format!("graineo-series-stats-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&copy);
    {
        let ro = rusqlite::Connection::open_with_flags(&src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        ro.execute("VACUUM INTO ?1", [copy.to_string_lossy()])?;
    }
    let db = Db::open(&copy)?;
    let photos: usize = db.conn.query_row("SELECT count(*) FROM image WHERE copy IS NULL", [], |r| r.get::<_, i64>(0))? as usize;
    let s = series::regroup(&db, true, &|| false).map_err(anyhow::Error::msg)?;
    println!("copy {}: {} days, grouped in {} ms", copy.display(), s.days, s.ms);
    print_distribution(&db, photos)?;
    drop(db);
    let _ = std::fs::remove_file(&copy);
    Ok(())
}

/// `graineo-dev series-demo <photo folder> <data folder>`: an own test library (`<data folder>/library.sqlite`) with
/// the photos of the folder, for screenshots of the series views. Only for copies in an own test folder.
pub fn demo(args: &[String]) -> Result<()> {
    let (Some(photos), Some(data)) = (args.first().map(PathBuf::from), args.get(1).map(PathBuf::from)) else { bail!("usage: graineo-dev series-demo <photo folder> <data folder>") };
    own_tmp_folder(&photos)?;
    own_tmp_folder(&data)?;
    let db = Db::open(&data.join("library.sqlite"))?;
    let (root, report) = library::reconcile::add_root(&db, &photos, &|m| eprintln!("{m}")).map_err(anyhow::Error::msg)?;
    println!("root {root}: {report:?}");
    let mut st = db.conn.prepare("SELECT id, name, capture_time, camera FROM file ORDER BY name")?;
    for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?)))? {
        println!("{:?}", r?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_must_stay_below_private_tmp() {
        for bad in ["/private/tmp/../../Users/someone/Pictures", "/private/tmp/./x", "/private/tmp", "/private/tmp/", "/Users/x", "private/tmp/x", "/private/tmpx/y"] {
            assert!(own_tmp_folder(Path::new(bad)).is_err(), "{bad}");
        }
        assert!(own_tmp_folder(Path::new("/private/tmp/graineo-series-measure-test-never-created")).is_ok());
        // a link below /private/tmp pointing elsewhere is refused
        let link = Path::new("/private/tmp").join(format!("graineo-series-link-{}", std::process::id()));
        if Path::new("/private/tmp").is_dir() {
            let _ = std::fs::remove_file(&link);
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink("/Users", &link).unwrap();
                assert!(own_tmp_folder(&link).is_err());
                std::fs::remove_file(&link).unwrap();
            }
        }
    }
}
