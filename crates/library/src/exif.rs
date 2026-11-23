//! Shooting data (lens, aperture, focal length, ISO, time) for files that do not come from the
//! Lightroom catalog: only the file header is read (ImageIO or the embedded JPEG
//! of an X3F). Runs in the background; each file is tried once (`file.exif = 1`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::params;

use crate::db::Db;

#[derive(Debug, Default, Clone)]
pub struct Capture {
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub aperture: Option<f64>,
    pub focal: Option<f64>,
    pub iso: Option<i64>,
    pub time: Option<f64>,
    pub capture: Option<String>,
    /// image size (unrotated); only read for X3F here, ImageIO formats get it with the preview
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// EXIF orientation 1–8 of the file
    pub orientation: Option<u32>,
}

pub fn read(path: &Path, format: &str) -> Option<Capture> {
    if format == "X3F" || format == "X3I" {
        // the small preview carries no EXIF data, the large one does: the large one first (saves one access on the NAS)
        let size = x3f::read_image_size(path);
        let e = [true, false].into_iter().find_map(|large| x3f::exif::parse_jpeg_exif(&x3f::read_embedded_jpeg_up_to(path, large, 128 * 1024).ok()?));
        let Some(e) = e else {
            return size.map(|(w, h)| Capture { width: Some(w), height: Some(h), ..Capture::default() });
        };
        return Some(Capture {
            width: size.map(|s| s.0),
            height: size.map(|s| s.1),
            camera: e.model,
            lens: None,
            aperture: e.f_number,
            focal: e.focal_length,
            iso: e.iso.map(|v| v as i64),
            time: e.exposure_time,
            capture: e.date_time.as_deref().and_then(date),
            orientation: e.orientation.map(u32::from).filter(|o| (1..=8).contains(o)),
        });
    }
    // macOS: ImageIO; elsewhere the own header reader (same fields, see `exif_header`)
    #[cfg(target_os = "macos")]
    let e = complete_from_header(crate::imageio::properties(path), path, format)?;
    #[cfg(not(target_os = "macos"))]
    let e = crate::exif_header::properties(path).or_else(|| crate::is_raw(format).then(|| rawler_properties(path)).flatten())?;
    Some(capture_of(e, path, format))
}

/// ImageIO leaves fields empty for some RAWs (CR3: no ISO; Kodak KDC of the P880: nothing at all). Only then the
/// own header reader (and for RAWs the decoder's metadata) fills the gaps; what ImageIO found stays.
#[cfg(target_os = "macos")]
fn complete_from_header(e: Option<crate::imageio::Properties>, path: &Path, format: &str) -> Option<crate::imageio::Properties> {
    if e.as_ref().is_some_and(|e| e.camera.is_some() && e.capture.is_some() && e.iso.is_some()) {
        return e;
    }
    let Some(h) = crate::exif_header::properties(path).or_else(|| crate::is_raw(format).then(|| rawler_properties(path)).flatten()) else { return e };
    let Some(mut e) = e else { return Some(h) };
    macro_rules! fill {
        ($($f:ident),*) => { $( if e.$f.is_none() { e.$f = h.$f; } )* };
    }
    fill!(width, height, orientation, capture, camera, maker, lens, aperture, focal, iso, time);
    Some(e)
}

fn capture_of(e: crate::imageio::Properties, path: &Path, format: &str) -> Capture {
    let lens = e.lens.or_else(|| if format == "NEF" { nikon_lens(path) } else { None });
    Capture { camera: e.camera, lens, aperture: e.aperture, focal: e.focal, iso: e.iso, time: e.time, capture: e.capture, width: None, height: None, orientation: e.orientation.filter(|o| (1..=8).contains(o)) }
}

/// [`read`] with the platform-independent header reader also on the Mac (comparison with ImageIO, tests).
pub fn read_portable(path: &Path, format: &str) -> Option<Capture> {
    if format == "X3F" || format == "X3I" {
        return read(path, format);
    }
    let e = crate::exif_header::properties(path).or_else(|| crate::is_raw(format).then(|| rawler_properties(path)).flatten())?;
    Some(capture_of(e, path, format))
}

/// [`read_portable`] with the image size from the header (pixel dimensions as stored, unrotated), for photos that
/// are not in the library (a viewed folder). Reads only the file header (64–192 KiB for most formats).
pub fn read_header(path: &Path, format: &str) -> Option<Capture> {
    if format == "X3F" || format == "X3I" {
        return read(path, format);
    }
    let e = crate::exif_header::properties(path).or_else(|| crate::is_raw(format).then(|| rawler_properties(path)).flatten())?;
    let (width, height) = (e.width, e.height);
    Some(Capture { width, height, ..capture_of(e, path, format) })
}

/// RAW decoder on the start of the file (formats the header reader does not know, e.g. CRW): metadata only, no
/// sensor data, at most the first 6 MB (CRW: the whole file, its directory is at the end; a truncated CRW makes the
/// decoder panic).
fn rawler_metadata(path: &Path) -> Option<rawler::decoders::RawMetadata> {
    use std::io::Read;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let crw = path.extension().and_then(|e| e.to_str()).and_then(crate::extension_format) == Some("CRW");
    let limits: &[u64] = if crw { &[u64::MAX] } else { &[1 << 20, 6 << 20] };
    for &limit in limits {
        let mut b = Vec::new();
        std::fs::File::open(path).ok()?.take(limit).read_to_end(&mut b).ok()?;
        let whole = (b.len() as u64) < limit;
        let src = rawler::rawsource::RawSource::new_from_shared_vec(std::sync::Arc::new(b)).with_path(path);
        let meta = catch_unwind(AssertUnwindSafe(|| {
            let dec = rawler::get_decoder(&src).ok()?;
            dec.raw_metadata(&src, &rawler::decoders::RawDecodeParams::default()).ok()
        }))
        .ok()
        .flatten();
        if meta.is_some() || whole {
            return meta;
        }
    }
    None
}

fn ratio(r: rawler::formats::tiff::Rational) -> f64 {
    if r.d == 0 { 0.0 } else { r.n as f64 / r.d as f64 }
}

fn rawler_properties(path: &Path) -> Option<crate::imageio::Properties> {
    let m = rawler_metadata(path)?;
    let x = &m.exif;
    let pos = |v: Option<f64>| v.filter(|v| v.is_finite() && *v > 0.0);
    let text = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    Some(crate::imageio::Properties {
        width: None,
        height: None,
        orientation: x.orientation.map(u32::from).filter(|o| (1..=8).contains(o)),
        capture: x.date_time_original.as_deref().or(x.create_date.as_deref()).and_then(crate::imageio_common::exif_date),
        // cleaned-up names of the decoder: the model without the maker, as ImageIO's TIFF Model mostly has it
        camera: text(&m.model),
        maker: text(&m.make),
        lens: x.lens_model.as_deref().and_then(text),
        aperture: pos(x.fnumber.map(ratio)),
        focal: pos(x.focal_length.map(ratio)),
        iso: x.iso_speed_ratings.map(i64::from).or(x.iso_speed.map(i64::from)).filter(|v| *v > 0),
        time: pos(x.exposure_time.map(ratio)),
    })
}

/// Nikon DSLRs (F mount) and some Coolpix write no EXIF lens name; the lens is only coded in the makernote
/// (lens data with the lens ID). The RAW reader resolves it with its lens database, e.g. "Nikon AF-S Nikkor 50mm
/// f/1.4G". Only the metadata is read, not the sensor data.
fn nikon_lens(path: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    let meta = {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let src = rawler::rawsource::RawSource::new(path).ok()?;
        let dec = catch_unwind(AssertUnwindSafe(|| rawler::get_decoder(&src))).ok()?.ok()?;
        catch_unwind(AssertUnwindSafe(|| dec.raw_metadata(&src, &rawler::decoders::RawDecodeParams::default()))).ok()?.ok()?
    };
    // elsewhere only the start of the file (network drives); the makernote lies in the first megabyte
    #[cfg(not(target_os = "macos"))]
    let meta = rawler_metadata(path)?;
    let l = meta.lens?;
    let name = if l.lens_model.to_ascii_lowercase().starts_with(&l.lens_make.to_ascii_lowercase()) { l.lens_model } else { format!("{} {}", l.lens_make, l.lens_model) };
    Some(name.trim().to_string()).filter(|s| !s.is_empty())
}

fn date(s: &str) -> Option<String> {
    crate::imageio_common::exif_date(s)
}

/// Cameras with a fixed lens: camera name (case-insensitive) → lens name. Added only where the file
/// itself carries no lens name.
const FIXED_LENSES: &[(&str, &str)] = &[
    ("FinePix X100", "FUJINON 23mm f/2 (fest)"),
    ("X100S", "FUJINON 23mm f/2 (fest)"),
    ("X100T", "FUJINON 23mm f/2 (fest)"),
    ("X100F", "FUJINON 23mm f/2 (fest)"),
    ("X100V", "FUJINON 23mm f/2 (fest)"),
    ("X100VI", "FUJINON 23mm f/2 (fest)"),
    ("SIGMA DP1 Merrill", "SIGMA 19mm f/2.8 (fest)"),
    ("SIGMA DP2 Merrill", "SIGMA 30mm f/2.8 (fest)"),
    ("SIGMA DP3 Merrill", "SIGMA 50mm f/2.8 (fest)"),
    ("SIGMA dp0 Quattro", "SIGMA 14mm f/4 (fest)"),
    ("SIGMA dp1 Quattro", "SIGMA 19mm f/2.8 (fest)"),
    ("SIGMA dp2 Quattro", "SIGMA 30mm f/2.8 (fest)"),
    ("SIGMA dp3 Quattro", "SIGMA 50mm f/2.8 (fest)"),
    ("GR", "GR 18.3mm f/2.8 (fest)"),
    ("GR II", "GR 18.3mm f/2.8 (fest)"),
    ("RICOH GR III", "GR 18.3mm f/2.8 (fest)"),
    ("RICOH GR IV", "GR 18.3mm f/2.8 (fest)"),
    ("RICOH GR IIIx", "GR 26.1mm f/2.8 (fest)"),
];

/// Lens name the library gives a camera with a fixed lens (case-insensitive camera name), e.g. for the
/// lens measurement of X3F files, which carry no lens name.
pub fn fixed_lens(camera: &str) -> Option<&'static str> {
    FIXED_LENSES.iter().find(|(c, _)| c.eq_ignore_ascii_case(camera.trim())).map(|(_, l)| *l)
}

/// Add the lens for cameras with a fixed lens. Returns: changed files.
pub fn add_fixed_lens(db: &Db) -> Result<usize, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut n = 0;
    for (camera, lens) in FIXED_LENSES {
        n += db.conn.execute("UPDATE file SET lens=?2 WHERE lens IS NULL AND lower(camera)=lower(?1)", params![camera, lens]).map_err(f)?;
    }
    Ok(n)
}

/// `file.exif` of a file that could not be opened: read again at the next program start.
const UNREADABLE: i64 = -1;

/// [`read`] with the pixel size from the image header where the shooting data have none (CR2 and other RAWs), as
/// "Metadaten neu einlesen" fills it ([`crate::metadata_reread::read_file`]). Before, a CR2 imported stayed without
/// width and height until it was read again by hand (QA 11, N-6).
fn read_with_size(path: &Path, format: &str) -> Option<Capture> {
    let mut a = read(path, format);
    if format != "X3F" && format != "X3I" && a.as_ref().is_none_or(|a| a.width.is_none()) {
        if let Some(p) = crate::imageio::properties(path) {
            let c = a.get_or_insert_with(Capture::default);
            c.width = p.width;
            c.height = p.height;
            c.orientation = c.orientation.or(p.orientation.filter(|o| (1..=8).contains(o)));
        }
    }
    a
}

/// Fill in missing shooting data (at most `max` files). Returns: processed.
pub fn backfill(db: &Db, max: usize, cancel: &AtomicBool) -> Result<usize, String> {
    backfill_with(db, max, cancel, &|_| {})
}

/// Like [`backfill`]; `progress` is called after each committed batch with the count so far
/// (the UI can then already show what has been read).
pub fn backfill_with(db: &Db, max: usize, cancel: &AtomicBool, progress: &dyn Fn(usize)) -> Result<usize, String> {
    let f = |e: rusqlite::Error| e.to_string();
    // files that could not be opened at an earlier run (network drive asleep) are tried again once per program start
    static RETRY: std::sync::Once = std::sync::Once::new();
    RETRY.call_once(|| {
        let _ = db.conn.execute("UPDATE file SET exif = 0 WHERE exif = ?1", [UNREADABLE]);
    });
    let open_set: Vec<(i64, String, String)> = {
        let mut st = db
            .conn
            .prepare(
                "SELECT d.id, s.path || o.path || d.name, d.format FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root
                 WHERE (d.exif = 0 OR (d.exif IN (1, 2) AND d.format = 'X3F' AND (d.camera IS NULL OR d.width IS NULL))) AND d.status = 0 AND d.format <> 'VIDEO' LIMIT ?1",
            )
            .map_err(f)?;
        let r = st.query_map([max as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?.collect::<rusqlite::Result<_>>().map_err(f)?;
        r
    };
    let mut n = 0;
    for chunk in open_set.chunks(64) {
        // On network drives the wait per file is the limit, not the CPU: read several files at once
        let read_result: Vec<(i64, bool, Option<Capture>, bool)> = std::thread::scope(|sc| {
            let parts: Vec<_> = chunk
                .chunks(chunk.len().div_ceil(8).max(1))
                .map(|part| sc.spawn(move || part.iter().map(|(id, p, fm)| (*id, (fm == "X3F" || fm == "X3I"), read_with_size(Path::new(p), fm), std::fs::File::open(p).is_ok())).collect::<Vec<_>>()))
                .collect();
            parts.into_iter().flat_map(|t| t.join().unwrap_or_default()).collect()
        });
        let tx = db.conn.unchecked_transaction().map_err(f)?;
        for (id, x3f, a, readable) in read_result {
            if a.is_none() && !readable {
                // not readable right now (network drive, file being copied): not "done with empty data" – before 0.108
                // capture date, camera and lens stayed empty for good
                tx.execute("UPDATE file SET exif = ?2 WHERE id = ?1", params![id, UNREADABLE]).map_err(f)?;
                n += 1;
                continue;
            }
            let a = a.unwrap_or_default();
            // X3F: 2 = second attempt made (up to 0.39 only the small preview without EXIF data was read),
            // 3 = image size read as well (up to 0.82 X3F had no width/height)
            tx.execute(
                "UPDATE file SET exif=?9, lens=coalesce(lens,?2), aperture=coalesce(aperture,?3), focal_length=coalesce(focal_length,?4), iso=coalesce(iso,?5),
                 time=coalesce(time,?6), camera=coalesce(camera,?7), capture_time=coalesce(capture_time,?8), width=coalesce(width,?10), height=coalesce(height,?11),
                 orientation=coalesce(orientation,?12) WHERE id=?1",
                params![id, a.lens, a.aperture, a.focal, a.iso, a.time, a.camera, a.capture, if x3f { 3 } else { 1 }, a.width, a.height, a.orientation],
            )
            .map_err(f)?;
            n += 1;
        }
        tx.commit().map_err(f)?;
        progress(n);
        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }
    add_fixed_lens(db)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file that cannot be opened (network drive asleep) is not taken as "read, no data": it stays open for the
    /// next start (before 0.108 capture date, camera and lens stayed empty for good). Same for the text fields.
    #[test]
    fn unreadable_file_is_not_done() {
        let d = std::env::temp_dir().join(format!("graineo-exif-unlesbar-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        let root = db.create_root(&format!("{}/", d.display()), "t", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'weg.jpg', 'JPG')", [folder]).unwrap();
        std::fs::write(d.join("da.jpg"), b"\xFF\xD8 kein echtes JPEG").unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'da.jpg', 'JPG')", [folder]).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(backfill(&db, 100, &cancel).unwrap(), 2);
        let state = |name: &str, col: &str| db.conn.query_row(&format!("SELECT {col} FROM file WHERE name=?1"), [name], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(state("weg.jpg", "exif"), UNREADABLE);
        assert_eq!(state("da.jpg", "exif"), 1, "readable without data: done");
        crate::textmeta::backfill(&db, 100, &cancel).unwrap();
        assert_eq!(state("weg.jpg", "text_read"), -1);
        assert_eq!(state("da.jpg", "text_read"), 1);
        drop(db);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The RAW decoder on the first megabytes (path outside macOS) finds the same Nikon lens as on the whole file
    /// (`GRAINEO_TEST_EXIF_DIR`, all NEF there; skipped without it).
    #[test]
    fn nikon_lens_from_the_file_start() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_EXIF_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_EXIF_DIR not set, skipped");
            return;
        };
        for p in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("nef"))) {
            let whole = {
                let src = rawler::rawsource::RawSource::new(&p).unwrap();
                rawler::get_decoder(&src).unwrap().raw_metadata(&src, &rawler::decoders::RawDecodeParams::default()).unwrap().lens.map(|l| l.lens_model)
            };
            let start = rawler_metadata(&p).and_then(|m| m.lens).map(|l| l.lens_model);
            assert_eq!(start, whole, "{}", p.display());
        }
    }

    /// NEF samples (`GRAINEO_TEST_NEF_DIR`, skipped without it): F-mount lenses of Nikon DSLRs have no EXIF lens
    /// name; the library gets them from the makernote. Checked on the CC0 sample of a D810 (AF-S 50mm f/1.4G).
    #[cfg(target_os = "macos")]
    #[test]
    fn nikon_dslr_lens_from_the_makernote() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_NEF_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_NEF_DIR not set, skipped");
            return;
        };
        let Some(p) = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).find(|p| p.to_string_lossy().contains("D810")) else {
            eprintln!("no D810 sample, skipped");
            return;
        };
        let c = read(&p, "NEF").expect("readable");
        assert_eq!(c.lens.as_deref(), Some("Nikon AF-S Nikkor 50mm f/1.4G"), "{}", p.display());
    }

    /// The header reader for viewed folders: the shooting data and the pixel size from the file header.
    #[test]
    fn read_header_with_size() {
        let dir = std::env::temp_dir().join(format!("graineo-read-header-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.jpg");
        std::fs::write(&p, crate::exif_header::tests::jpeg_with(Some(&crate::exif_header::tests::sample(true)))).unwrap();
        let c = read_header(&p, "JPG").expect("readable");
        assert_eq!(c.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(c.capture.as_deref(), Some("2024-05-11T14:03:22"));
        assert_eq!((c.aperture, c.focal, c.iso), (Some(5.6), Some(35.0), Some(400)));
        assert_eq!(c.lens.as_deref(), Some("RF24-105mm F4 L IS USM"));
        assert_eq!((c.width, c.height), (Some(8), Some(8)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Measurement (`GRAINEO_HEADER_DIR=<folder>`, ignored by default): time of `read_header` per file.
    #[test]
    #[ignore]
    fn measure_read_header() {
        let Ok(dir) = std::env::var("GRAINEO_HEADER_DIR") else { return };
        let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        let t0 = std::time::Instant::now();
        let mut n = 0;
        for p in &entries {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let Some(f) = crate::format_from_extension(&name) else { continue };
            let t = std::time::Instant::now();
            let c = read_header(p, f);
            n += 1;
            if n <= 12 {
                eprintln!("{name}: {:.2} ms, camera {:?}, {:?}x{:?}", t.elapsed().as_secs_f64() * 1000.0, c.as_ref().and_then(|c| c.camera.clone()), c.as_ref().and_then(|c| c.width), c.as_ref().and_then(|c| c.height));
            }
        }
        eprintln!("{n} files in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    }
}
