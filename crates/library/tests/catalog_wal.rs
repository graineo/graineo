//! Lightroom catalog with an uncheckpointed WAL: the import must see the changes from the WAL
//! (with `immutable=1` they were missing).

use library::lightroom::open_catalog;
use rusqlite::Connection;

#[test]
fn wal_content_is_read() {
    let dir = std::env::temp_dir().join(format!("x3f-katalog-wal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cat = dir.join("Katalog mit Leerzeichen.lrcat");

    // "Lightroom": its own connection stays open, row 2 exists only in the WAL
    let writer = Connection::open(&cat).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    writer.execute("INSERT INTO t VALUES(2)", []).unwrap();
    let mut wal = cat.as_os_str().to_owned();
    wal.push("-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0, "WAL not empty");

    let c = open_catalog(&cat).unwrap();
    let n: i64 = c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2, "change from the WAL missing");
    // read-only
    assert!(c.execute("INSERT INTO t VALUES(3)", []).is_err());
    drop(c);

    // empty WAL (Lightroom closed): the immutable path reads the main file
    writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    let c = open_catalog(&cat).unwrap();
    let n: i64 = c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `-wal` that holds only its 32-byte header (Lightroom closed after a checkpoint) counts as empty: the catalog is
/// opened `immutable`, so SQLite creates no `-shm` and neither the catalog nor its folder changes.
#[test]
fn wal_header_only_is_treated_as_empty() {
    let dir = std::env::temp_dir().join(format!("graineo-catalog-walheader-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cat = dir.join("Katalog.lrcat");
    {
        let c = Connection::open(&cat).unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);").unwrap();
    }
    let mut wal = cat.as_os_str().to_owned();
    wal.push("-wal");
    // a header without frames (content does not matter, SQLite never reads it with `immutable`)
    std::fs::write(&wal, [0x37u8, 0x7f, 0x06, 0x82, 0, 0x2d, 0xe2, 0x18, 0, 0, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    assert!(library::lightroom::wal_without_changes(&cat));
    assert!(!library::lightroom::catalog_in_use(&cat));
    let listing = |d: &std::path::Path| {
        let mut v: Vec<(String, u64, std::time::SystemTime)> = std::fs::read_dir(d).unwrap().flatten().map(|e| {
            let m = e.metadata().unwrap();
            (e.file_name().to_string_lossy().into_owned(), m.len(), m.modified().unwrap())
        }).collect();
        v.sort();
        v
    };
    let before = listing(&dir);
    let cat_bytes = std::fs::read(&cat).unwrap();
    let c = open_catalog(&cat).unwrap();
    let n: i64 = c.query_row("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    drop(c);
    assert_eq!(listing(&dir), before, "folder touched");
    assert_eq!(std::fs::read(&cat).unwrap(), cat_bytes);
    // one byte more: a frame could follow – Lightroom may be writing
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes.push(0);
    std::fs::write(&wal, bytes).unwrap();
    assert!(!library::lightroom::wal_without_changes(&cat));
    assert!(library::lightroom::catalog_in_use(&cat));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Catalog search for the Lightroom dialog: `*.lrcat` in the folder and its direct subfolders, newest first; backups
/// and Lightroom's packages are skipped; nothing is opened.
#[test]
fn find_catalogs_lists_newest_first() {
    let dir = std::env::temp_dir().join(format!("graineo-catalog-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["Alt", "Backups/2026-11-01", "Lightroom Catalog Previews.lrdata"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join("Alt/Alt.lrcat"), b"x").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(dir.join("Lightroom Catalog.lrcat"), b"x").unwrap();
    std::fs::write(dir.join("Backups/2026-11-01/Lightroom Catalog.lrcat"), b"x").unwrap();
    std::fs::write(dir.join("Lightroom Catalog Previews.lrdata/x.lrcat"), b"x").unwrap();
    std::fs::write(dir.join("Notiz.txt"), b"x").unwrap();
    std::fs::write(dir.join(".versteckt.lrcat"), b"x").unwrap();
    let found = library::lightroom::find_catalogs(&dir);
    assert_eq!(found, vec![dir.join("Lightroom Catalog.lrcat"), dir.join("Alt/Alt.lrcat")]);
    assert!(library::lightroom::find_catalogs(&dir.join("gibt-es-nicht")).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
