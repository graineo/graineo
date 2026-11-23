//! Tournament "Übernehmen" on a real library of its own (temp folder, needs `GRAINEO_DATA_DIR`): the winner is
//! picked and becomes the cover, every other entrant is rejected, nobody gets stars – and one ⌘Z takes ratings and
//! cover back together.

use super::*;

/// A library with one series of three photos (empty files stand in for the RAWs); returns folder, database, images.
fn library_with_series(tag: &str) -> (PathBuf, Db, [i64; 3]) {
    let dir = std::env::temp_dir().join(format!("graineo-adopt-{tag}-{}-{}", std::process::id(), library::now()));
    std::fs::create_dir_all(dir.join("serie")).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let root = db.create_root(&format!("{}/", dir.display()), "test", "own", None).unwrap();
    let folder = db.create_folder(root, "serie/", None).unwrap();
    let run = db.new_run("sync", "").unwrap();
    let add = |name: &str, second: u32| -> i64 {
        std::fs::write(dir.join("serie").join(name), b"").unwrap();
        db.conn
            .execute(
                &format!(
                    "INSERT INTO file(folder, name, format, camera, capture_time, width, height, time, iso, aperture, size, mtime, exif, text_read, xmp_read)
                     VALUES({folder}, '{name}', 'RW2', 'Panasonic DMC-GX7', '2026-11-01T10:00:{second:02}', 4592, 3448, 0.01, 200, 8.0, 1, 1, 1, 1, 1)"
                ),
                [],
            )
            .unwrap();
        let file = db.conn.last_insert_rowid();
        db.conn.execute(&format!("INSERT INTO image(file, run) VALUES({file}, {run})"), []).unwrap();
        db.conn.last_insert_rowid()
    };
    let images = [add("P1000001.RW2", 10), add("P1000002.RW2", 11), add("P1000003.RW2", 12)];
    library::series::create(&db, &images, images[0]).unwrap();
    (dir, db, images)
}

/// Carry out the background writes of the view and read the rows again.
fn settle(b: &mut LibraryView, ctx: &egui::Context) -> Vec<Action> {
    let t = std::time::Instant::now();
    let mut actions = Vec::new();
    loop {
        let done = b.db_writer.fetch(ctx);
        let n = done.len();
        for finished in done {
            finished(b, &mut actions);
        }
        if n == 0 && !b.db_writer.busy() {
            break;
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "library writes not finished");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    b.reload_now();
    actions
}

#[test]
fn adopting_sets_flags_and_cover_and_one_undo_takes_all_back() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let (dir, db, [a, b_img, c]) = library_with_series("undo");
    // the second photo carries stars: they must stay
    db.conn.execute(&format!("UPDATE image SET stars = 3 WHERE id = {b_img}"), []).unwrap();
    let ctx = egui::Context::default();
    let mut b = LibraryView::new(&ctx);
    b.db = Some(Db::open(&db.path).unwrap());
    // the ratings are written by the view's writer: it gets this library (a new view writes to the app's own)
    b.writer = crate::rating::Writer::new(Some(db.path.clone()));
    b.reload_now();
    let stack: i64 = db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [a], |r| r.get(0)).unwrap();
    // the series straight from the database (the detection job is global and would disturb other tests)
    b.series.stacks = vec![library::series::get(&db.conn, stack).unwrap().unwrap()];
    let cover = |db: &Db| library::series::get(&db.conn, stack).unwrap().unwrap().top();
    let state = |db: &Db| -> Vec<(i64, i64)> { [a, b_img, c].iter().map(|i| db.conn.query_row("SELECT flag, stars FROM image WHERE id = ?1", [*i], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()).collect() };
    let wait_for = |want: &[(i64, i64)], what: &str| {
        let t = std::time::Instant::now();
        while state(&db) != want {
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "{what}: {:?}", state(&db));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    assert_eq!(cover(&db), a);
    let steps = b.manage.undo.len();
    let mut compare = Compare::new(Some(stack), None);
    compare.images = vec![a, b_img, c];
    b.compare = Some(compare);

    // the third photo wins, the first is second, the second third
    let mut actions = Vec::new();
    b.compare_adopt([Some(c), Some(a), Some(b_img)], &[a, b_img, c], &mut actions);
    actions.extend(settle(&mut b, &ctx));
    wait_for(&[(-1, 0), (-1, 3), (1, 0)], "winner picked, the others rejected, stars as they were");
    assert_eq!(cover(&db), c, "the winner is the cover");
    assert!(actions.iter().any(|x| matches!(x, Action::Status(s) if s.contains('2'))), "the message names the two rejected photos");
    assert_eq!(b.manage.undo.len(), steps + 1, "the cover is one library step");

    // one ⌘Z: ratings and cover
    assert!(b.undo_rating_change().is_some());
    settle(&mut b, &ctx);
    wait_for(&[(0, 0), (0, 3), (0, 0)], "one undo takes every flag back");
    assert_eq!(cover(&db), a, "and the cover");
    assert_eq!(b.manage.undo.len(), steps, "no second step left");
    assert_eq!(b.rating_stamp(), None);

    // without a winner nothing is set and nothing is claimed
    let mut actions = Vec::new();
    b.compare_adopt([None, None, None], &[a, b_img, c], &mut actions);
    settle(&mut b, &ctx);
    assert_eq!(state(&db), [(0, 0), (0, 3), (0, 0)]);
    assert!(actions.iter().all(|x| !matches!(x, Action::Status(s) if s.contains("abgelehnt"))));
    drop(b);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tournament of a free selection (no series compared as a whole) whose winner lies in a series with photos that did
/// not play: the winner still becomes the cover of its series, and ⌘Z brings the old cover back (QA 10, N-A5).
#[test]
fn the_winner_becomes_cover_of_its_series_also_when_not_all_of_it_played() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let (dir, db, [a, b_img, c]) = library_with_series("partial");
    let ctx = egui::Context::default();
    let mut b = LibraryView::new(&ctx);
    b.db = Some(Db::open(&db.path).unwrap());
    b.writer = crate::rating::Writer::new(Some(db.path.clone()));
    b.reload_now();
    let stack: i64 = db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [a], |r| r.get(0)).unwrap();
    b.series.stacks = vec![library::series::get(&db.conn, stack).unwrap().unwrap()];
    let cover = |db: &Db| library::series::get(&db.conn, stack).unwrap().unwrap().top();
    assert_eq!(cover(&db), a);
    // a free selection: only the second and third photo of the series play
    let mut compare = Compare::new(None, None);
    compare.images = vec![b_img, c];
    b.compare = Some(compare);
    let mut actions = Vec::new();
    b.compare_adopt([Some(c), Some(b_img), None], &[b_img, c], &mut actions);
    settle(&mut b, &ctx);
    assert_eq!(cover(&db), c, "the winner is the cover of its series");
    assert!(b.undo_rating_change().is_some());
    settle(&mut b, &ctx);
    assert_eq!(cover(&db), a, "one ⌘Z brings the old cover back");
    drop(b);
    let _ = std::fs::remove_dir_all(&dir);
}
