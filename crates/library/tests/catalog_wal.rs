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
