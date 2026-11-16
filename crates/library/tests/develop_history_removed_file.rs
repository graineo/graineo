//! Queued history changes of a photo that left the library (removed, or merged into another one) must not keep the
//! store's writer busy: they are dropped instead of failing on the foreign key again and again.

use library::develop_history::{self as h, Op, Step};
use serde_json::json;

fn step(seq: i64, kind: &str, patch: serde_json::Value) -> Step {
    Step { seq, time: 1_000 + seq, kind: kind.into(), label: None, patch, count: 1, command: None }
}

#[test]
fn history_of_a_removed_file_is_dropped() {
    let dir = std::env::temp_dir().join(format!("graineo-history-removed-{}-{}", std::process::id(), library::now()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library.sqlite");
    std::env::set_var("GRAINEO_LIBRARY_DB", &lib);
    let db = library::db::Db::open(&lib).unwrap();
    let root = db.create_root(&format!("{}/", dir.display()), "t", "own", None).unwrap();
    let folder = db.create_folder(root, "", None).unwrap();
    let mut files = Vec::new();
    for name in ["a.jpg", "b.jpg"] {
        db.conn.execute("INSERT INTO file(folder, name, format, xmp_read) VALUES(?1, ?2, 'JPG', 1)", rusqlite::params![folder, name]).unwrap();
        let file = db.conn.last_insert_rowid();
        db.conn.execute("INSERT INTO image(file) VALUES(?1)", [file]).unwrap();
        files.push(file);
    }
    let (gone, kept) = (files[0], files[1]);

    let a = json!({ "exposure": 0.0 });
    let b = json!({ "exposure": 0.5 });
    for &file in &files {
        library::store::queue_history(file, None, vec![Op::Put(step(0, h::kind::START, a.clone())), Op::Put(step(1, h::kind::SLIDER, h::diff(&a, &b)))]);
    }
    // the photo is removed before its history reached the library
    db.conn.execute("DELETE FROM file WHERE id = ?1", [gone]).unwrap();

    let t = std::time::Instant::now();
    library::store::send_history();
    assert!(library::store::flush(std::time::Duration::from_secs(30)), "writer still busy or changes left: {:?}", library::store::last_error());
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "took {:?}", t.elapsed());
    assert_eq!(library::store::unsaved(), 0);
    // nothing is left queued for the removed photo, the other one is written
    assert!(library::store::load_history(gone, None).unwrap().0.is_empty());
    assert_eq!(h::load(&db.conn, kept, None).unwrap().0.len(), 2);
    let n: i64 = db.conn.query_row("SELECT count(*) FROM develop_step WHERE file = ?1", [gone], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);

    // later saves are not held up by it
    library::store::set_develop(kept, None, Some(b.to_string()));
    assert!(library::store::flush(std::time::Duration::from_secs(10)));

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
