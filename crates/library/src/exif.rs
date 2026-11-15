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
}

pub fn read(path: &Path, format: &str) -> Option<Capture> {
    if format == "X3F" {
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
        });
    }
    #[cfg(target_os = "macos")]
    {
        let e = crate::imageio::properties(path)?;
        Some(Capture { camera: e.camera, lens: e.lens, aperture: e.aperture, focal: e.focal, iso: e.iso, time: e.time, capture: e.capture, width: None, height: None })
    }
    #[cfg(not(target_os = "macos"))]
    None
}

fn date(s: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::imageio::exif_date(s)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = s;
        None
    }
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
                .map(|part| sc.spawn(move || part.iter().map(|(id, p, fm)| (*id, fm == "X3F", read(Path::new(p), fm), std::fs::File::open(p).is_ok())).collect::<Vec<_>>()))
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
                 time=coalesce(time,?6), camera=coalesce(camera,?7), capture_time=coalesce(capture_time,?8), width=coalesce(width,?10), height=coalesce(height,?11) WHERE id=?1",
                params![id, a.lens, a.aperture, a.focal, a.iso, a.time, a.camera, a.capture, if x3f { 3 } else { 1 }, a.width, a.height],
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
}
