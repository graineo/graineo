//! Test data and machine facts for the start page benchmarks (`tools/measure/startpage/`):
//!
//! - `graineo-dev bench-data library <data folder> --sources <folder> [--photos 65000] [--seed 1]`: a synthetic library
//!   in an own data folder (`library.sqlite` plus a filled preview store), shaped like a real one: day folders over
//!   fourteen years, ten cameras with their formats, file sizes and aspect ratios, landscape and portrait, ratings,
//!   a few virtual copies. The photo files themselves do not exist (only the day folders); every photo has its small
//!   preview (512 px, JPEG quality as the app writes it) under the key the app computes, so the grid never needs an
//!   original. The previews are crops of the embedded previews of the files in `--sources` (read only), each with its
//!   own crop, mirror and colour gain, so no two are alike.
//! - `graineo-dev bench-data import-source <folder> --sources <folder> [--jpegs 2000] [--raws 200] [--seed 1]`: a folder
//!   to import from: `jpeg/DCIM/100GRAIN/*.JPG` (4–12 MP, made from the same previews) and `raw/*` (copies of the
//!   source RAWs, each made unique with a few bytes at the end so the import does not take them for identical copies;
//!   on APFS the copies are clones and take almost no space).
//! - `graineo-dev bench-data system [--busy-secs 2] [--path <folder>]`: facts about the computer and its load as JSON
//!   (the same facts as the performance report: system, processor, memory, power, thermal state, load average, how busy
//!   the processor was during `busy-secs`, graphics adapters, drive of `path`).
//!
//! Never touches the real library: the data folder must be given and must not be the app's own.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use image::RgbImage;
use rayon::prelude::*;

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("library") => library(&args[1..]),
        Some("import-source") => import_source(&args[1..]),
        Some("system") => system(&args[1..]),
        _ => bail!("usage: graineo-dev bench-data library <data folder> --sources <folder> [--photos 65000] [--seed 1]\n       graineo-dev bench-data import-source <folder> --sources <folder> [--jpegs 2000] [--raws 200] [--seed 1]\n       graineo-dev bench-data system [--busy-secs 2] [--path <folder>]"),
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Small deterministic generator (xorshift64*).
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// 0 ≤ x < 1
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: u64, b: u64) -> u64 {
        a + self.next() % (b - a).max(1)
    }
}

/// Kind of source picture a camera's previews are cut from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    Merrill,
    Quattro,
    XTrans,
    Canon,
    Dng,
    Any,
}

struct Camera {
    name: &'static str,
    format: &'static str,
    ext: &'static str,
    prefix: &'static str,
    /// share of the shooting days
    weight: f64,
    years: (i32, i32),
    /// pixel size (landscape)
    size: (u32, u32),
    /// file size range in MB
    mb: (f64, f64),
    family: Family,
}

const CAMERAS: [Camera; 10] = [
    Camera { name: "SIGMA DP2 Merrill", format: "X3F", ext: "X3F", prefix: "DP2M", weight: 0.13, years: (2013, 2026), size: (4704, 3136), mb: (44.0, 56.0), family: Family::Merrill },
    Camera { name: "SIGMA DP3 Merrill", format: "X3F", ext: "X3F", prefix: "DP3M", weight: 0.08, years: (2013, 2026), size: (4704, 3136), mb: (44.0, 56.0), family: Family::Merrill },
    Camera { name: "SIGMA sd Quattro H", format: "X3F", ext: "X3F", prefix: "SDQH", weight: 0.07, years: (2017, 2026), size: (6192, 4128), mb: (50.0, 70.0), family: Family::Quattro },
    Camera { name: "FUJIFILM X-Pro2", format: "RAF", ext: "RAF", prefix: "DSCF", weight: 0.14, years: (2016, 2026), size: (6000, 4000), mb: (25.0, 52.0), family: Family::XTrans },
    Camera { name: "FUJIFILM X100F", format: "RAF", ext: "RAF", prefix: "DSCF", weight: 0.08, years: (2017, 2026), size: (6000, 4000), mb: (25.0, 52.0), family: Family::XTrans },
    Camera { name: "Canon EOS RP", format: "CR3", ext: "CR3", prefix: "IMG_", weight: 0.17, years: (2019, 2026), size: (6240, 4160), mb: (24.0, 34.0), family: Family::Canon },
    Camera { name: "RICOH GR III", format: "DNG", ext: "DNG", prefix: "R00", weight: 0.09, years: (2019, 2026), size: (6000, 4000), mb: (24.0, 32.0), family: Family::Dng },
    Camera { name: "LEICA Q (Typ 116)", format: "DNG", ext: "DNG", prefix: "L100", weight: 0.05, years: (2016, 2026), size: (6000, 4000), mb: (40.0, 46.0), family: Family::Dng },
    Camera { name: "Apple iPhone 13 Pro", format: "HEIC", ext: "HEIC", prefix: "IMG_", weight: 0.13, years: (2021, 2026), size: (4032, 3024), mb: (1.5, 4.5), family: Family::Any },
    Camera { name: "Canon EOS 30D", format: "JPG", ext: "JPG", prefix: "IMG_", weight: 0.06, years: (2013, 2017), size: (3504, 2336), mb: (2.5, 5.0), family: Family::Any },
];

fn family_of(path: &Path) -> Family {
    let name = path.file_name().map(|n| n.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
    match ext.as_str() {
        "X3F" if name.starts_with("SDQ") => Family::Quattro,
        "X3F" => Family::Merrill,
        "RAF" => Family::XTrans,
        "CR3" | "CR2" => Family::Canon,
        "DNG" => Family::Dng,
        _ => Family::Any,
    }
}

/// Source files (RAW/JPEG) in a folder, sorted.
fn source_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("{}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && library::format_from_extension(&p.to_string_lossy()).is_some())
        .collect();
    v.sort();
    if v.is_empty() {
        bail!("no photos in {}", dir.display());
    }
    Ok(v)
}

/// The embedded previews of the sources (upright, at most 2048 px), by family.
fn bases(dir: &Path) -> Result<Vec<(Family, Arc<RgbImage>)>> {
    let files = source_files(dir)?;
    let v: Vec<(Family, Arc<RgbImage>)> = files
        .par_iter()
        .filter_map(|p| {
            let f = library::format_from_extension(&p.to_string_lossy())?;
            match library::preview::load(p, f, 2048) {
                Ok((img, _)) => Some((family_of(p), Arc::new(img))),
                Err(e) => {
                    eprintln!("bench-data: {} skipped: {e}", p.display());
                    None
                }
            }
        })
        .collect();
    if v.is_empty() {
        bail!("no source preview could be read");
    }
    eprintln!("bench-data: {} source pictures ({})", v.len(), v.iter().map(|(f, i)| format!("{f:?} {}×{}", i.width(), i.height())).collect::<Vec<_>>().join(", "));
    Ok(v)
}

/// A picture of aspect `w:h` and long edge `edge`, cut from `base` (random crop of 55–100 % of the largest fitting
/// window, mirrored half of the time, own colour gain): no two are alike.
fn variant(base: &RgbImage, w: u32, h: u32, edge: u32, rng: &mut Rng) -> RgbImage {
    let (bw, bh) = (base.width() as f64, base.height() as f64);
    let aspect = w as f64 / h as f64;
    let (mut cw, mut ch) = if bw / bh > aspect { (bh * aspect, bh) } else { (bw, bw / aspect) };
    let s = 0.55 + rng.f() * 0.45;
    cw *= s;
    ch *= s;
    let x = (rng.f() * (bw - cw)).max(0.0) as u32;
    let y = (rng.f() * (bh - ch)).max(0.0) as u32;
    let crop = image::imageops::crop_imm(base, x, y, (cw as u32).max(1), (ch as u32).max(1)).to_image();
    let (ow, oh) = if w >= h { (edge, (edge as f64 / aspect).round() as u32) } else { ((edge as f64 * aspect).round() as u32, edge) };
    let mut img = image::imageops::resize(&crop, ow.max(1), oh.max(1), image::imageops::FilterType::Triangle);
    if rng.next() & 1 == 1 {
        image::imageops::flip_horizontal_in_place(&mut img);
    }
    let gain = [0.82 + rng.f() * 0.36, 0.82 + rng.f() * 0.36, 0.82 + rng.f() * 0.36];
    for p in img.pixels_mut() {
        for c in 0..3 {
            p.0[c] = (p.0[c] as f64 * gain[c]).min(255.0) as u8;
        }
    }
    img
}

/// One photo of the synthetic library.
struct Photo {
    folder: usize,
    name: String,
    camera: usize,
    size: i64,
    mtime: i64,
    capture: String,
    width: u32,
    height: u32,
    stars: i64,
    flag: i64,
    label: i64,
    copy: bool,
    seed: u64,
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Unix seconds of a local date and time (as UTC; only used as a file time).
fn unix(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    let mut days: i64 = 0;
    for yy in 1970..y {
        days += if (yy % 4 == 0 && yy % 100 != 0) || yy % 400 == 0 { 366 } else { 365 };
    }
    for mm in 1..mo {
        days += days_in_month(y, mm) as i64;
    }
    days += d as i64 - 1;
    days * 86400 + h as i64 * 3600 + mi as i64 * 60 + s as i64
}

/// The photos and day folders (`YYYY/YYYY-MM-DD/`) of a library of `n` photos.
fn plan(n: usize, seed: u64) -> (Vec<String>, Vec<Photo>) {
    let mut rng = Rng::new(seed);
    let mut folders = Vec::new();
    let mut photos = Vec::with_capacity(n);
    let mut counter = [0u32; CAMERAS.len()];
    let mut used_days = std::collections::HashSet::new();
    while photos.len() < n {
        // year: later years more photos
        let year = 2013 + ((rng.f().sqrt()) * 14.0) as i32;
        let year = year.min(2026);
        let month = rng.range(1, 13) as u32;
        let day = rng.range(1, days_in_month(year, month) as u64 + 1) as u32;
        if !used_days.insert((year, month, day)) {
            continue;
        }
        // cameras that existed that year, by weight
        let fits: Vec<usize> = (0..CAMERAS.len()).filter(|&c| CAMERAS[c].years.0 <= year && year <= CAMERAS[c].years.1).collect();
        let total: f64 = fits.iter().map(|&c| CAMERAS[c].weight).sum();
        let mut pick = rng.f() * total;
        let mut cam = fits[0];
        for &c in &fits {
            pick -= CAMERAS[c].weight;
            if pick <= 0.0 {
                cam = c;
                break;
            }
        }
        folders.push(format!("{year}/{year}-{month:02}-{day:02}/"));
        let folder = folders.len() - 1;
        // photos per shooting day: mostly a few dozen, sometimes hundreds
        let count = ((rng.f().powi(3) * 300.0) as usize + 5).min(n - photos.len());
        let cam_def = &CAMERAS[cam];
        let mut t = 8 * 3600 + rng.range(0, 6 * 3600) as u32;
        for _ in 0..count {
            counter[cam] = counter[cam] % 9999 + 1;
            let name = format!("{}{:04}.{}", cam_def.prefix, counter[cam], cam_def.ext);
            t += rng.range(2, 300) as u32;
            let (h, mi, s) = ((t / 3600).min(23), (t / 60) % 60, t % 60);
            let (mut w, mut hh) = cam_def.size;
            let r = rng.f();
            if r < 0.22 {
                std::mem::swap(&mut w, &mut hh);
            } else if r < 0.25 {
                w = hh;
            } else if r < 0.27 {
                hh = w * 9 / 16;
            }
            let stars = match rng.f() {
                x if x < 0.70 => 0,
                x if x < 0.80 => 1,
                x if x < 0.88 => 2,
                x if x < 0.95 => 3,
                x if x < 0.99 => 4,
                _ => 5,
            };
            let mb = cam_def.mb.0 + rng.f() * (cam_def.mb.1 - cam_def.mb.0);
            photos.push(Photo {
                folder,
                name,
                camera: cam,
                size: (mb * 1_000_000.0) as i64,
                mtime: unix(year, month, day, h, mi, s),
                capture: format!("{year}-{month:02}-{day:02}T{h:02}:{mi:02}:{s:02}"),
                width: w,
                height: hh,
                stars,
                flag: if rng.f() < 0.05 { 1 } else if rng.f() < 0.02 { -1 } else { 0 },
                label: if rng.f() < 0.04 { rng.range(1, 6) as i64 } else { 0 },
                copy: rng.f() < 0.01,
                seed: rng.next(),
            });
        }
    }
    (folders, photos)
}

fn library(args: &[String]) -> Result<()> {
    let data = PathBuf::from(args.first().context("data folder missing")?);
    let sources = PathBuf::from(opt(args, "--sources").context("--sources <folder> missing")?);
    let n: usize = opt(args, "--photos").map(str::parse).transpose()?.unwrap_or(65_000);
    let seed: u64 = opt(args, "--seed").map(str::parse).transpose()?.unwrap_or(1);
    // never the app's own data folder
    if let Some(own) = sysdirs::app_data_dir().map(|d| d.join(library::APP)) {
        if data.starts_with(&own) || std::env::current_dir().map(|c| c.join(&data)).is_ok_and(|d| d.starts_with(&own)) {
            bail!("refusing to write into the app's own data folder {}", own.display());
        }
    }
    let db_path = data.join("library.sqlite");
    if db_path.exists() {
        bail!("{} exists already (use a new, empty folder)", db_path.display());
    }
    std::fs::create_dir_all(&data)?;
    let data = data.canonicalize()?;
    std::env::set_var("GRAINEO_DATA_DIR", &data);
    let t0 = std::time::Instant::now();
    let bases = bases(&sources)?;
    let (folders, photos) = plan(n, seed);
    // the day folders exist (empty): the root and its folders are reachable like on a real drive
    let root = data.join("Photos");
    for f in &folders {
        std::fs::create_dir_all(root.join(f))?;
    }
    let db = library::db::Db::open(&db_path)?;
    let root_path = format!("{}/", root.to_string_lossy());
    let root_id = db.create_root(&root_path, "Photos", "own", None)?;
    let tx = db.conn.unchecked_transaction()?;
    let mut folder_ids = Vec::with_capacity(folders.len());
    for f in &folders {
        tx.execute("INSERT INTO folder(root, path) VALUES(?1, ?2)", rusqlite::params![root_id, f])?;
        folder_ids.push(tx.last_insert_rowid());
    }
    {
        let mut file = tx.prepare(
            "INSERT INTO file(folder, name, format, size, mtime, camera, capture_time, width, height, orientation, status, exif, partial_hash, text_read, xmp_read)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 0, 1, ?10, 1, 1)",
        )?;
        let mut image = tx.prepare("INSERT INTO image(file, copy, stars, flag, label, faces_scanned) VALUES(?1, ?2, ?3, ?4, ?5, 1)")?;
        for (i, p) in photos.iter().enumerate() {
            let c = &CAMERAS[p.camera];
            file.execute(rusqlite::params![folder_ids[p.folder], p.name, c.format, p.size, p.mtime, c.name, p.capture, p.width, p.height, format!("synthetic-{i}")])?;
            let id = tx.last_insert_rowid();
            image.execute(rusqlite::params![id, Option::<String>::None, p.stars, p.flag, p.label])?;
            if p.copy {
                image.execute(rusqlite::params![id, "Kopie 1", 0, 0, 0])?;
            }
        }
    }
    tx.commit()?;
    let folder_paths = db.folder_paths()?;
    let rows: Vec<(String, i64, i64)> = {
        let mut st = db.conn.prepare("SELECT folder, name, size, mtime FROM file ORDER BY id")?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?;
        r.map(|x| x.map(|(f, n, s, m)| (format!("{}{n}", folder_paths.get(&f).map(String::as_str).unwrap_or("")), s, m))).collect::<rusqlite::Result<_>>()?
    };
    drop(db);
    eprintln!("bench-data: {} photos in {} day folders written to the library in {:.1} s", photos.len(), folders.len(), t0.elapsed().as_secs_f64());
    let cache = library::preview::Cache::open(&data.join(library::PREVIEW_FOLDER));
    let level = library::preview::SMALL_DEFAULT;
    let done = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    photos.par_iter().zip(rows.par_iter()).for_each(|(p, (path, size, mtime))| {
        let key = library::preview::cache_key_with_orientation(library::preview::cache_key(path, *size, *mtime), Some(1));
        let mut rng = Rng::new(p.seed);
        let fam = CAMERAS[p.camera].family;
        let pool: Vec<&Arc<RgbImage>> = bases.iter().filter(|b| b.0 == fam || fam == Family::Any).map(|b| &b.1).collect();
        let pool = if pool.is_empty() { bases.iter().map(|b| &b.1).collect() } else { pool };
        let base = pool[(rng.next() % pool.len() as u64) as usize];
        let img = variant(base, p.width, p.height, level, &mut rng);
        if !cache.write(key, level, &img) {
            failed.fetch_add(1, Ordering::Relaxed);
        }
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        if d % 5000 == 0 {
            eprintln!("bench-data: {d} previews");
        }
    });
    let (bytes, files) = cache.occupied();
    eprintln!(
        "bench-data: library ready in {:.0} s: {} photos ({} virtual copies), {} previews ({:.0} MB, {} failed), level {level} px; data folder {}",
        t0.elapsed().as_secs_f64(),
        photos.len(),
        photos.iter().filter(|p| p.copy).count(),
        files,
        bytes as f64 / 1e6,
        failed.load(Ordering::Relaxed),
        data.display()
    );
    if failed.load(Ordering::Relaxed) > 0 {
        bail!("some previews could not be written (free space?)");
    }
    Ok(())
}

/// Append bytes to a copy so it is not identical to its source, keeping it readable: X3F keeps its directory pointer
/// as the last four bytes, CR3 (ISO media file) gets a `free` box, everything else (TIFF-like) a tail.
fn make_unique(path: &Path, n: u64) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new().read(true).append(true).open(path)?;
    let mut tail = vec![0u8; 16 + (n % 64) as usize];
    tail[..8].copy_from_slice(&n.to_le_bytes());
    match ext.as_str() {
        "X3F" => {
            let mut last = [0u8; 4];
            f.seek(SeekFrom::End(-4))?;
            f.read_exact(&mut last)?;
            f.write_all(&tail)?;
            f.write_all(&last)?;
        }
        "CR3" | "MP4" | "MOV" | "HEIC" => {
            let len = (tail.len() + 8) as u32;
            f.write_all(&len.to_be_bytes())?;
            f.write_all(b"free")?;
            f.write_all(&tail)?;
        }
        _ => f.write_all(&tail)?,
    }
    Ok(())
}

fn import_source(args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("folder missing")?);
    let sources = PathBuf::from(opt(args, "--sources").context("--sources <folder> missing")?);
    let jpegs: usize = opt(args, "--jpegs").map(str::parse).transpose()?.unwrap_or(2000);
    let raws: usize = opt(args, "--raws").map(str::parse).transpose()?.unwrap_or(200);
    let seed: u64 = opt(args, "--seed").map(str::parse).transpose()?.unwrap_or(1);
    let t0 = std::time::Instant::now();
    let jdir = out.join("jpeg/DCIM/100GRAIN");
    let rdir = out.join("raw");
    std::fs::create_dir_all(&jdir)?;
    std::fs::create_dir_all(&rdir)?;
    if jpegs > 0 {
        let bases = bases(&sources)?;
        // 4, 8 and 12 MP in 4:3 and 3:2, a fifth portrait; file times over three days
        let sizes = [(2448u32, 1632u32), (3264, 2448), (4000, 3000), (4240, 2832)];
        let start = unix(2026, 6, 12, 9, 0, 0);
        (0..jpegs).into_par_iter().try_for_each(|i| -> Result<()> {
            let mut rng = Rng::new(seed.wrapping_add(i as u64 * 7919));
            let (mut w, mut h) = sizes[(rng.next() % sizes.len() as u64) as usize];
            if rng.f() < 0.2 {
                std::mem::swap(&mut w, &mut h);
            }
            let base = &bases[(rng.next() % bases.len() as u64) as usize].1;
            let img = variant(base, w, h, w.max(h), &mut rng);
            let p = jdir.join(format!("IMG_{:04}.JPG", i + 1));
            let mut buf = Vec::new();
            img.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90))?;
            std::fs::write(&p, buf)?;
            let t = start + (i as i64 * 3 * 86400 / jpegs.max(1) as i64);
            let f = std::fs::File::options().write(true).open(&p)?;
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t as u64))?;
            Ok(())
        })?;
        eprintln!("bench-data: {jpegs} JPEGs in {}", jdir.display());
    }
    if raws > 0 {
        let files: Vec<PathBuf> = source_files(&sources)?.into_iter().filter(|p| library::format_from_extension(&p.to_string_lossy()).is_some_and(library::is_raw)).collect();
        if files.is_empty() {
            bail!("no RAW files in {}", sources.display());
        }
        for i in 0..raws {
            let src = &files[i % files.len()];
            let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = src.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let target = rdir.join(format!("{stem}_{:03}.{ext}", i + 1));
            // a clone on APFS: no extra space until the tail is written
            std::fs::copy(src, &target).with_context(|| format!("{}", target.display()))?;
            make_unique(&target, i as u64 + 1)?;
        }
        eprintln!("bench-data: {raws} RAW copies in {}", rdir.display());
    }
    eprintln!("bench-data: import source ready in {:.0} s", t0.elapsed().as_secs_f64());
    Ok(())
}

fn system(args: &[String]) -> Result<()> {
    use library::platform::info;
    let busy_secs: f64 = opt(args, "--busy-secs").map(str::parse).transpose()?.unwrap_or(2.0);
    let t = info::system_cpu_ticks();
    std::thread::sleep(std::time::Duration::from_secs_f64(busy_secs));
    let busy = match (t, info::system_cpu_ticks()) {
        (Some(a), Some(b)) => info::busy_share(a, b),
        _ => None,
    };
    let s = info::system_info();
    let drive = opt(args, "--path").map(|p| {
        let (k, internal) = info::drive(Path::new(p));
        format!("{}{}", k.name(), match internal {
            Some(true) => ", built in",
            Some(false) => ", external",
            None => "",
        })
    });
    let adapters: Vec<String> = rawdev::gpu::adapters().into_iter().map(|a| format!("{} ({}, {})", a.name, a.backend, a.device_type)).collect();
    let power = match s.power.source {
        info::PowerSource::Ac => "mains",
        info::PowerSource::Battery => "battery",
        info::PowerSource::Unknown => "unknown",
    };
    let j = serde_json::json!({
        "os": s.os,
        "model": s.model,
        "cpu": s.cpu,
        "physical_cores": s.physical_cores,
        "logical_cores": s.logical_cores,
        "core_kinds": s.core_kinds,
        "memory_total": s.memory_total,
        "memory_available": s.memory_available,
        "power": power,
        "battery_percent": s.power.battery_percent,
        "low_power_mode": s.low_power_mode,
        "thermal": s.thermal,
        "load_average": s.load_average,
        "busy_share": busy.map(|b| (b * 1000.0).round() / 1000.0),
        "busy_secs": busy_secs,
        "graphics": adapters,
        "drive": drive,
    });
    println!("{}", serde_json::to_string_pretty(&j)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_stable_and_realistic() {
        let (folders, photos) = plan(65_000, 1);
        assert_eq!(photos.len(), 65_000);
        let (f2, p2) = plan(65_000, 1);
        assert_eq!(folders, f2);
        assert_eq!(photos.iter().map(|p| &p.name).collect::<Vec<_>>(), p2.iter().map(|p| &p.name).collect::<Vec<_>>());
        // names unique per folder (UNIQUE(folder, name))
        let mut seen = std::collections::HashSet::new();
        assert!(photos.iter().all(|p| seen.insert((p.folder, p.name.clone()))));
        let portrait = photos.iter().filter(|p| p.height > p.width).count();
        assert!(portrait > 11_000 && portrait < 18_000, "{portrait}");
        assert!(photos.iter().all(|p| CAMERAS[p.camera].years.0 <= p.capture[..4].parse::<i32>().unwrap()));
        assert!(CAMERAS.iter().enumerate().all(|(c, _)| photos.iter().any(|p| p.camera == c)), "every camera present");
    }

    #[test]
    fn unix_time() {
        assert_eq!(unix(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(unix(2026, 11, 16, 12, 0, 0), 1_794_830_400);
    }

    #[test]
    fn variant_size_and_difference() {
        let base = RgbImage::from_fn(800, 600, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let a = variant(&base, 4704, 3136, 512, &mut Rng::new(1));
        let b = variant(&base, 4704, 3136, 512, &mut Rng::new(2));
        assert_eq!((a.width(), a.height()), (512, 341));
        assert_ne!(a.as_raw(), b.as_raw());
        let p = variant(&base, 3136, 4704, 512, &mut Rng::new(3));
        assert_eq!((p.width(), p.height()), (341, 512));
    }

    #[test]
    fn unique_copies_stay_readable() {
        let d = std::env::temp_dir().join(format!("bench-data-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let x = d.join("a.X3F");
        std::fs::write(&x, [1u8, 2, 3, 4, 5, 6, 0xAA, 0xBB, 0xCC, 0xDD]).unwrap();
        make_unique(&x, 3).unwrap();
        let b = std::fs::read(&x).unwrap();
        assert_eq!(&b[b.len() - 4..], &[0xAA, 0xBB, 0xCC, 0xDD], "directory pointer stays last");
        let c = d.join("a.CR3");
        std::fs::write(&c, [0u8; 8]).unwrap();
        make_unique(&c, 1).unwrap();
        let b = std::fs::read(&c).unwrap();
        assert_eq!(&b[12..16], b"free");
        assert_eq!(u32::from_be_bytes([b[8], b[9], b[10], b[11]]) as usize, b.len() - 8);
        let _ = std::fs::remove_dir_all(&d);
    }
}
