//! A loop in the keyword or collection hierarchy (a damaged or manipulated library) must not hang searches, the
//! sidecar writer or the management functions: every walk over the tree ends.

use std::time::Duration;

use library::db::Db;

#[test]
fn walks_over_a_parent_loop_end() {
    let dir = std::env::temp_dir().join(format!("graineo-hierarchy-cycle-{}-{}", std::process::id(), library::now()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library.sqlite");
    let (tx, rx) = std::sync::mpsc::channel();
    let path = lib.clone();
    let root_dir = format!("{}/", dir.display());
    std::thread::spawn(move || {
        let db = Db::open(&path).unwrap();
        let root = db.create_root(&root_dir, "t", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        db.conn.execute("INSERT INTO file(folder, name, format, xmp_read) VALUES(?1, 'a.jpg', 'JPG', 1)", [folder]).unwrap();
        let file = db.conn.last_insert_rowid();
        db.conn.execute("INSERT INTO image(file) VALUES(?1)", [file]).unwrap();
        let image = db.conn.last_insert_rowid();
        // keywords A → B → C → A, the photo carries A
        for name in ["A", "B", "C"] {
            db.conn.execute("INSERT INTO keyword(name) VALUES(?1)", [name]).unwrap();
        }
        let k = |n: &str| db.conn.query_row("SELECT id FROM keyword WHERE name=?1", [n], |r| r.get::<_, i64>(0)).unwrap();
        let (a, b, c) = (k("A"), k("B"), k("C"));
        db.conn.execute("UPDATE keyword SET parent=?1 WHERE id=?2", [b, a]).unwrap();
        db.conn.execute("UPDATE keyword SET parent=?1 WHERE id=?2", [c, b]).unwrap();
        db.conn.execute("UPDATE keyword SET parent=?1 WHERE id=?2", [a, c]).unwrap();
        db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, ?2)", [image, a]).unwrap();
        // collection sets X ⇄ Y
        for name in ["X", "Y", "Z"] {
            db.conn.execute("INSERT INTO collection(name, kind) VALUES(?1, 1)", [name]).unwrap();
        }
        let col = |n: &str| db.conn.query_row("SELECT id FROM collection WHERE name=?1", [n], |r| r.get::<_, i64>(0)).unwrap();
        let (x, y, z) = (col("X"), col("Y"), col("Z"));
        db.conn.execute("UPDATE collection SET parent=?1 WHERE id=?2", [y, x]).unwrap();
        db.conn.execute("UPDATE collection SET parent=?1 WHERE id=?2", [x, y]).unwrap();

        let mut found = db.images_with_keyword(b).unwrap();
        found.sort();
        assert_eq!(found, vec![image]);
        // no path reaches the top: nothing to show, but an answer
        let _ = db.keywords_of(image).unwrap();
        let _ = library::sync::keyword_paths(&db, image).unwrap();
        assert_eq!(library::manage::keyword_loss(&db, c).unwrap(), 1);
        // moving or merging into the own branch is refused (the check ends)
        assert!(library::manage::move_keyword(&db, a, Some(b)).is_err());
        assert!(library::manage::merge_keyword(&db, a, c).is_err());
        let _ = library::manage::collection_loss(&db, x).unwrap();
        // placing into the loop: the check ends (z below x is refused, x already sits below y)
        let _ = library::collection_order::place(&db, y, Some(x), None);
        let _ = library::collection_order::place(&db, z, Some(x), None);
        let _ = library::collection_order::place(&db, x, Some(y), None);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(60)).expect("a walk over the loop did not end (or an assertion in the thread failed)");
    let _ = std::fs::remove_dir_all(&dir);
}
