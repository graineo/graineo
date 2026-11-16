//! Lightroom's develop history of an image, read from a small catalog built here (never the user's catalog): names and
//! times in order, read-only, other images and snapshots left out.

use rusqlite::Connection;

#[test]
fn develop_history_of_an_image() {
    let dir = std::env::temp_dir().join(format!("graineo-lr-history-{}-{}", std::process::id(), library::now()));
    std::fs::create_dir_all(&dir).unwrap();
    let cat = dir.join("Mini.lrcat");
    {
        let c = Connection::open(&cat).unwrap();
        c.execute_batch(
            "CREATE TABLE Adobe_libraryImageDevelopHistoryStep (id_local INTEGER PRIMARY KEY, id_global TEXT, dateCreated REAL, digest TEXT, hasDevelopAdjustments INTEGER, image INTEGER, name TEXT, relValueString TEXT, text TEXT, valueString TEXT);
             CREATE TABLE Adobe_librarySnapshot (id_local INTEGER PRIMARY KEY, image INTEGER, name TEXT, snapshotID TEXT);
             INSERT INTO Adobe_libraryImageDevelopHistoryStep(id_local, dateCreated, image, name) VALUES
               (1, 600000000.0, 7, 'Importieren (12.01.20 10:00:00)'), (3, 600000100.5, 7, 'Belichtung'), (2, 600000050.0, 7, 'Freistellen - Rechteck'),
               (4, 600000000.0, 8, 'Andere');
             INSERT INTO Adobe_librarySnapshot(image, name) VALUES (7, 'Variante SW');",
        )
        .unwrap();
    }
    let before = std::fs::read(&cat).unwrap();
    let conn = library::lightroom::open_catalog(&cat).unwrap();
    let h = library::lightroom::develop_history(&conn, 7).unwrap();
    let names: Vec<&str> = h.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Importieren (12.01.20 10:00:00)", "Freistellen - Rechteck", "Belichtung"]);
    assert_eq!(h[0].1, 600_000_000 + 978_307_200, "Lightroom counts seconds from 2001");
    assert!(library::lightroom::develop_history(&conn, 99).unwrap().is_empty());
    drop(conn);
    assert_eq!(std::fs::read(&cat).unwrap(), before, "the catalog stays untouched");
    // a catalog without the table: nothing, no error
    let empty = dir.join("Leer.lrcat");
    Connection::open(&empty).unwrap().execute_batch("CREATE TABLE x (a);").unwrap();
    assert!(library::lightroom::develop_history(&library::lightroom::open_catalog(&empty).unwrap(), 7).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
