//! The develop history goes through the store's writer: queued steps reach the library in the same transaction as the
//! settings, reads see them before that, and a queue without a settings change is written on its own.

use library::develop_history::{self as h, Op, Step};
use serde_json::json;

fn step(seq: i64, kind: &str, patch: serde_json::Value) -> Step {
    Step { seq, time: 1_000 + seq, kind: kind.into(), label: None, patch, count: 1, command: None }
}

#[test]
fn history_is_written_with_the_settings() {
    let dir = std::env::temp_dir().join(format!("graineo-history-store-{}-{}", std::process::id(), library::now()));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library.sqlite");
    std::env::set_var("GRAINEO_LIBRARY_DB", &lib);
    let db = library::db::Db::open(&lib).unwrap();
    let root = db.create_root(&format!("{}/", dir.display()), "t", "own", None).unwrap();
    let folder = db.create_folder(root, "", None).unwrap();
    db.conn.execute("INSERT INTO file(folder, name, format, xmp_read) VALUES(?1, 'a.jpg', 'JPG', 1)", [folder]).unwrap();
    let file = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO image(file) VALUES(?1)", [file]).unwrap();

    let a = json!({ "exposure": 0.0 });
    let b = json!({ "exposure": 0.5 });
    library::store::queue_history(file, None, vec![Op::Put(step(0, h::kind::START, a.clone())), Op::Put(step(1, h::kind::SLIDER, h::diff(&a, &b)))]);
    // reads see the queued steps before they are written
    let (steps, cursor) = library::store::load_history(file, None).unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(cursor, None);
    let n: i64 = db.conn.query_row("SELECT count(*) FROM develop_step", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "not written before the settings are saved");

    // the settings save takes the steps along
    library::store::set_develop(file, None, Some(b.to_string()));
    assert!(library::store::flush(std::time::Duration::from_secs(20)));
    let (stored, _) = h::load(&db.conn, file, None).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(h::state_at(&stored, 1), Some(b.clone()));
    let settings: String = db.conn.query_row("SELECT settings FROM develop_state WHERE file = ?1", [file], |r| r.get(0)).unwrap();
    assert_eq!(settings, b.to_string());

    // a cursor move without a settings change of its own: sent alone
    library::store::queue_history(file, None, vec![Op::Cursor(Some(0))]);
    library::store::send_history();
    assert!(library::store::flush(std::time::Duration::from_secs(20)));
    assert_eq!(h::load(&db.conn, file, None).unwrap().1, Some(0));
    assert_eq!(library::store::load_history(file, None).unwrap().1, Some(0));

    // deleting every history (the user's command)
    assert_eq!(library::store::forget_all_histories(&db).unwrap(), 2);
    assert!(library::store::load_history(file, None).unwrap().0.is_empty());

    // review 1.2.0, N3: an error in the history never holds back the settings (the history's operations are dropped)
    db.conn.execute_batch("CREATE TRIGGER fail_steps BEFORE INSERT ON develop_step BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
    let c = json!({ "exposure": 1.0 });
    library::store::queue_history(file, None, vec![Op::Put(step(0, h::kind::START, c.clone()))]);
    library::store::set_develop(file, None, Some(c.to_string()));
    assert!(library::store::flush(std::time::Duration::from_secs(20)), "the settings are saved");
    let settings: String = db.conn.query_row("SELECT settings FROM develop_state WHERE file = ?1", [file], |r| r.get(0)).unwrap();
    assert_eq!(settings, c.to_string());
    assert!(h::load(&db.conn, file, None).unwrap().0.is_empty(), "the failed history was not written");
    assert!(library::store::load_history(file, None).unwrap().0.is_empty(), "and is not retried forever");
    db.conn.execute_batch("DROP TRIGGER fail_steps").unwrap();

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
