//! Correct the capture time: shift several photos by the same offset,
//! e.g. because the camera clock was wrong, or restore the original time.
//!
//! - The originals stay untouched. The corrected time is stored in the sidecar (`exif:DateTimeOriginal`,
//!   which Lightroom reads too) and in the library; both remember the camera's original time
//!   (`graineo:OriginalDateTime` or `file.capture_time_original`) so that it can be restored.
//! - Photos from Apple Photos stay unchanged (their time comes from the iPhone and is authoritative there).
//! - Sidecars only for existing files; missing ones: only the library (the sync
//!   writes them later, as with ratings).

use rusqlite::{params, OptionalExtension};

use crate::db::Db;

/// Result of a correction.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub changed: usize,
    pub sidecars: usize,
    /// without capture time (nothing to shift)
    pub without_time: usize,
    /// from Apple Photos (stay unchanged)
    pub apple: usize,
    pub error: Vec<String>,
}

/// Days since 1970-01-01 for a date (proleptic Gregorian calendar).
fn days(j: i64, m: i64, t: i64) -> i64 {
    let j = if m <= 2 { j - 1 } else { j };
    let era = j.div_euclid(400);
    let yoe = j - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + t - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn date_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let t = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, t)
}

/// 'YYYY-MM-DDThh:mm:ss' → seconds since 1970 (without time zone, like EXIF).
pub fn seconds(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |a: usize, e: usize| t.get(a..e)?.parse::<i64>().ok();
    let (j, m, d, h, mi, s) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days(j, m, d) * 86_400 + h * 3600 + mi * 60 + s)
}

/// Seconds since 1970 → 'YYYY-MM-DDThh:mm:ss'.
pub fn text(secs: i64) -> String {
    let (j, m, d) = date_from_days(secs.div_euclid(86_400));
    let r = secs.rem_euclid(86_400);
    format!("{j:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", r / 3600, r % 3600 / 60, r % 60)
}

/// Shift the time by `seconds`.
pub fn moved(t: &str, secs: i64) -> Option<String> {
    seconds(t).map(|s| text(s + secs))
}

/// Shift files by `secs` seconds. `sidecar`: write sidecars (else only the library).
pub fn relocate(db: &Db, files: &[i64], secs: i64, sidecar: bool) -> Result<Report, String> {
    change(db, files, sidecar, |capture, _| moved(capture, secs))
}

/// Restore the original capture time.
pub fn restore(db: &Db, files: &[i64], sidecar: bool) -> Result<Report, String> {
    change(db, files, sidecar, |_, original| original.map(str::to_string))
}

fn change(db: &Db, files: &[i64], sidecar: bool, new: impl Fn(&str, Option<&str>) -> Option<String>) -> Result<Report, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut b = Report::default();
    let mut to_write = Vec::new();
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    {
        let mut read = tx
            .prepare(
                "SELECT d.capture_time, d.capture_time_original, d.status, s.origin FROM file d JOIN folder o ON o.id = d.folder
                 JOIN root_folder s ON s.id = o.root WHERE d.id = ?1",
            )
            .map_err(f)?;
        let mut set = tx.prepare("UPDATE file SET capture_time = ?2, capture_time_original = ?3 WHERE id = ?1").map_err(f)?;
        for &d in files {
            let Some((capture, original, status, origin)) =
                read.query_row([d], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))).optional().map_err(f)?
            else {
                continue;
            };
            if origin == crate::apple_photos::ORIGIN {
                b.apple += 1;
                continue;
            }
            let Some(capture) = capture else {
                b.without_time += 1;
                continue;
            };
            let Some(target) = new(&capture, original.as_deref()) else { continue };
            if target == capture {
                continue;
            }
            // the original time stays the camera's; if the correction lands there again, it is cancelled
            let original_time = original.unwrap_or_else(|| capture.clone());
            let original_time = (original_time != target).then_some(original_time);
            // unreachable location: the sidecar write waits in the queue (`crate::location`)
            if status == 2 {
                crate::location::note_before_change(db, &[d]);
            }
            set.execute(params![d, target, original_time]).map_err(f)?;
            b.changed += 1;
            if status == 0 || status == 2 {
                to_write.push(d);
            }
        }
    }
    tx.commit().map_err(f)?;
    if sidecar {
        for d in to_write {
            match crate::sync::write_sidecar(db, d) {
                Ok(()) => b.sidecars += 1,
                Err(e) => b.error.push(e),
            }
        }
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_writes_library_and_xmp_and_can_be_undone() {
        let dir = std::env::temp_dir().join(format!("graineo-zeit-{}-{}", std::process::id(), crate::now()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG_1.JPG"), b"kein echtes Bild").unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(&format!(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '{}/', 'test', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, '');
                 INSERT INTO file(id, folder, name, format, capture_time, status) VALUES(1, 1, 'IMG_1.JPG', 'JPG', '2024-05-01T10:00:00', 0);
                 INSERT INTO image(id, file) VALUES(1, 1);",
                dir.display()
            ))
            .unwrap();
        let time = |db: &Db| db.conn.query_row("SELECT capture_time, capture_time_original FROM file WHERE id=1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).unwrap();
        // camera was 1 h 30 min slow: set back
        let b = relocate(&db, &[1], -5400, true).unwrap();
        assert_eq!((b.changed, b.sidecars), (1, 1));
        // the sidecar follows from the library (background writer, here directly)
        crate::store::write_sidecar_from_library(&db, 1, true).unwrap();
        assert_eq!(time(&db), ("2024-05-01T08:30:00".into(), Some("2024-05-01T10:00:00".into())));
        let xmp = std::fs::read_to_string(dir.join("IMG_1.xmp")).unwrap();
        assert!(xmp.contains("exif:DateTimeOriginal=\"2024-05-01T08:30:00\"") && xmp.contains("graineo:OriginalDateTime=\"2024-05-01T10:00:00\""), "{xmp}");
        // shift once more: the original time stays the camera's
        relocate(&db, &[1], 60, true).unwrap();
        crate::store::write_sidecar_from_library(&db, 1, true).unwrap();
        assert_eq!(time(&db), ("2024-05-01T08:31:00".into(), Some("2024-05-01T10:00:00".into())));
        // the sidecar is the source of truth: an empty library takes the correction from the XMP
        db.conn.execute("UPDATE file SET capture_time='2024-05-01T10:00:00', capture_time_original=NULL WHERE id=1", []).unwrap();
        let p = dir.join("IMG_1.JPG");
        crate::sync::reconcile_file(&db, 1, &p, "JPG", false, true).unwrap();
        assert_eq!(time(&db), ("2024-05-01T08:31:00".into(), Some("2024-05-01T10:00:00".into())));
        // restore: camera time, fields removed from library and XMP
        let b = restore(&db, &[1], true).unwrap();
        assert_eq!(b.changed, 1);
        crate::store::write_sidecar_from_library(&db, 1, true).unwrap();
        assert_eq!(time(&db), ("2024-05-01T10:00:00".into(), None));
        let xmp = std::fs::read_to_string(dir.join("IMG_1.xmp")).unwrap();
        assert!(!xmp.contains("DateTimeOriginal"), "{xmp}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn computes_across_day_month_year_and_leap_year() {
        assert_eq!(moved("2026-11-02T23:30:00", 3600).as_deref(), Some("2026-11-03T00:30:00"));
        assert_eq!(moved("2026-12-31T23:59:59", 1).as_deref(), Some("2027-01-01T00:00:00"));
        assert_eq!(moved("2024-02-28T12:00:00", 86_400).as_deref(), Some("2024-02-29T12:00:00"));
        assert_eq!(moved("2023-03-01T00:00:00", -86_400).as_deref(), Some("2023-02-28T00:00:00"));
        assert_eq!(moved("1999-01-01 10:00:00", -36_000).as_deref(), Some("1999-01-01T00:00:00"));
        assert_eq!(seconds("1970-01-01T00:00:00"), Some(0));
        assert_eq!(seconds("Unsinn"), None);
        let a = seconds("2026-07-14T08:15:00").unwrap();
        let b = seconds("2025-07-14T09:16:30").unwrap();
        assert_eq!(text(b - a + a), "2025-07-14T09:16:30");
    }
}
