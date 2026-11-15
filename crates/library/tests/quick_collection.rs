//! Quick collection: B as in common photo apps, show, empty, save as collection; import from Lightroom.

use library::{reconcile, db::Db};
use rusqlite::params;

fn images(db: &Db) -> Vec<i64> {
    let mut st = db.conn.prepare("SELECT b.id FROM image b JOIN file d ON d.id=b.file ORDER BY d.name").unwrap();
    st.query_map([], |r| r.get(0)).unwrap().flatten().collect()
}

#[test]
fn toggle_clear_save_and_lightroom_import() {
    let d = std::env::temp_dir().join(format!("x3f-schnell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let f = d.join("Fotos");
    std::fs::create_dir_all(&f).unwrap();
    for n in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::write(f.join(n), b"\xFF\xD8x").unwrap();
    }
    let db = Db::open(&d.join("library.sqlite")).unwrap();
    reconcile::add_root(&db, &f, &|_| {}).unwrap();
    let b = images(&db);
    assert_eq!(b.len(), 3);
    assert_eq!(db.quick_id(), None, "read-only: none created yet");
    let sq = db.quick_collection().unwrap();
    assert_eq!(db.quick_collection().unwrap(), sq, "stable ID");
    assert_eq!(db.quick_id(), Some(sq));
    assert_eq!(db.quick_set(&[b[2]], true).unwrap(), 1);
    assert_eq!(db.quick_set(&[b[2]], true).unwrap(), 0, "repeatable");
    assert_eq!(db.quick_set(&[b[2]], false).unwrap(), 1);
    assert_eq!(db.quick_set(&[b[2]], false).unwrap(), 0, "repeatable");
    let n = |db: &Db| db.images_of_collection(sq).unwrap().len();

    assert_eq!(db.quick_toggle(&[b[0], b[1]]).unwrap(), (2, 0));
    assert_eq!(n(&db), 2);
    assert_eq!(db.quick_toggle(&[b[1]]).unwrap(), (0, 1), "an image that is in goes out");
    assert_eq!(db.quick_toggle(&[b[0], b[1]]).unwrap(), (1, 0), "if one is missing, all go in");
    assert_eq!(n(&db), 2);
    assert_eq!(db.quick_toggle(&[b[0], b[1]]).unwrap(), (0, 2), "if all are in, all go out");
    assert_eq!(n(&db), 0);

    db.quick_toggle(&[b[0], b[2]]).unwrap();
    let new = db.quick_as_collection("Auswahl").unwrap();
    assert_eq!(db.images_of_collection(new).unwrap().len(), 2);
    let second_one = db.quick_as_collection("Auswahl").unwrap();
    let name: String = db.conn.query_row("SELECT name FROM collection WHERE id=?1", [second_one], |r| r.get(0)).unwrap();
    assert_eq!(name, "Auswahl 2");
    assert_eq!(db.quick_clear().unwrap(), 2);
    assert_eq!(n(&db), 0);
    assert_eq!(db.images_of_collection(new).unwrap().len(), 2, "saved collection stays");

    // quick collection from Lightroom: taken over instead of creating a second one
    let d2 = d.join("zwei");
    std::fs::create_dir_all(&d2).unwrap();
    let db2 = Db::open(&d2.join("library.sqlite")).unwrap();
    db2.conn.execute("INSERT INTO collection(name, kind, lr_id) VALUES('Schnellsammlung', 0, 14)", []).unwrap();
    let lr = db2.conn.last_insert_rowid();
    assert_eq!(db2.quick_id(), Some(lr), "read-only: the Lightroom collection applies even before the takeover");
    assert_eq!(db2.quick_collection().unwrap(), lr);
    let kind: i64 = db2.conn.query_row("SELECT kind FROM collection WHERE id=?1", params![lr], |r| r.get(0)).unwrap();
    assert_eq!(kind, 3);
    let _ = std::fs::remove_dir_all(&d);
}
