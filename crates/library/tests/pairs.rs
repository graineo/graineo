//! RAW+JPEG twice: detection, carrying data over and removing against a synthetic library (files only under temp).

use std::path::{Path, PathBuf};

use library::db::Db;
use library::pairs::{self, Disposal, Level, Options};
use rusqlite::params;

struct Fx {
    dir: PathBuf,
    db: Db,
    next: i64,
}

fn fx(name: &str) -> Fx {
    let dir = std::env::temp_dir().join(format!("graineo-pairs-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("photos")).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let root = format!("{}/", dir.join("photos").display());
    db.conn.execute("INSERT INTO root_folder(id, path, name, origin) VALUES(1, ?1, 'Fotos', 'own')", [&root]).unwrap();
    db.conn.execute("INSERT INTO folder(id, root, path) VALUES(1, 1, ''), (2, 1, 'jpg/')", []).unwrap();
    Fx { dir, db, next: 1 }
}

impl Fx {
    /// file + image; returns (file, image)
    fn photo(&mut self, folder: i64, name: &str, format: &str, cam: &str, time: &str, status: i64, write: bool) -> (i64, i64) {
        let id = self.next;
        self.next += 1;
        let sub = if folder == 2 { "jpg/" } else { "" };
        if write {
            let p = self.dir.join("photos").join(sub).join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"0123456789").unwrap();
        }
        self.db
            .conn
            .execute(
                "INSERT INTO file(id, folder, name, format, size, camera, capture_time, width, height, orientation, status) VALUES(?1, ?2, ?3, ?4, 10, ?5, ?6, 6000, 4000, 1, ?7)",
                params![id, folder, name, format, cam, time, status],
            )
            .unwrap();
        self.db.conn.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
        (id, id)
    }
    fn path(&self, folder: i64, name: &str) -> PathBuf {
        self.dir.join("photos").join(if folder == 2 { "jpg/" } else { "" }).join(name)
    }
}

fn fake_dispose(_: &Db, files: &[PathBuf]) -> Result<usize, String> {
    // stand-in for the Trash: moves into a folder of the test
    let mut n = 0;
    for f in files {
        let to = f.parent().unwrap().join("_weg").join(f.file_name().unwrap());
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::rename(f, to).map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}

fn run(fx: &Fx, pairs: &[pairs::Pair], transfer: bool) -> pairs::Outcome {
    pairs::remove(&fx.db, pairs, Options { disposal: Disposal::Trash, transfer }, &fake_dispose, &|_, _| {}, &|| false)
}

const T: &str = "2024-05-11T14:03:22";

#[test]
fn finds_levels_and_counts() {
    let mut f = fx("levels");
    // safe: same name, JPEG in another folder
    f.photo(1, "IMG_0001.CR3", "CR3", "Canon R", T, 0, true);
    f.photo(2, "IMG_0001.JPG", "JPG", "Canon R", T, 0, true);
    // RAW offline (drive missing)
    f.photo(1, "IMG_0002.CR3", "CR3", "Canon R", "2024-05-11T14:03:30", 2, false);
    f.photo(1, "IMG_0002.JPG", "JPG", "Canon R", "2024-05-11T14:03:30", 0, true);
    // same name, other camera: nothing
    f.photo(1, "IMG_0003.CR3", "CR3", "Canon R", "2024-05-11T14:04:00", 0, true);
    f.photo(1, "IMG_0003.JPG", "JPG", "Canon RP", "2024-05-11T14:04:00", 0, true);
    // burst: 2 RAWs + 2 JPEGs other names, same second
    for i in 0..2 {
        f.photo(1, &format!("S{i}.CR3"), "CR3", "Canon R", "2024-05-11T15:00:00", 0, true);
        f.photo(1, &format!("T{i}.JPG"), "JPG", "Canon R", "2024-05-11T15:00:00", 0, true);
    }
    // JPEG without RAW
    f.photo(1, "ALLEIN.JPG", "JPG", "Canon R", "2024-05-11T16:00:00", 0, true);
    let pairs = pairs::find(&f.db).unwrap();
    let s = pairs::summarize(&pairs);
    assert_eq!((s.safe, s.probable, s.unreachable, s.own_data), (1, 0, 1, 0), "{pairs:#?}");
    assert_eq!(s.safe_bytes, 10);
}

#[test]
fn offline_copy_entries_never_count() {
    // an offline copy is a preview-cache file (no row in `file`); a file row without an image (as the earlier
    // offloading left proxy JPEGs) never shows up as photo or pair
    let mut f = fx("offline");
    let (_raw, _) = f.photo(1, "IMG_0001.CR3", "CR3", "Canon R", T, 0, true);
    f.photo(1, "IMG_0001.JPG", "JPG", "Canon R", T, 0, true);
    f.db.conn
        .execute("INSERT INTO file(id, folder, name, format, size, camera, capture_time, status) VALUES(99, 1, 'IMG_0007.JPG', 'JPG', 10, 'Canon R', ?1, 0)", params!["2024-06-01T10:00:00"])
        .unwrap();
    f.photo(1, "IMG_0007.CR3", "CR3", "Canon R", "2024-06-01T10:00:00", 0, true);
    let pairs = pairs::find(&f.db).unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].jpeg.name, "IMG_0001.JPG");
}

#[test]
fn own_data_and_virtual_copies_keep_the_jpeg() {
    let mut f = fx("own");
    f.photo(1, "A.CR3", "CR3", "Canon R", T, 0, true);
    let (jf, _) = f.photo(1, "A.JPG", "JPG", "Canon R", T, 0, true);
    f.db.conn.execute("INSERT INTO develop_state(file, copy, settings) VALUES(?1, '', '{\"x\":1}')", [jf]).unwrap();
    f.photo(1, "B.CR3", "CR3", "Canon R", "2024-05-11T14:10:00", 0, true);
    let (bf, _) = f.photo(1, "B.JPG", "JPG", "Canon R", "2024-05-11T14:10:00", 0, true);
    f.db.conn.execute("INSERT INTO image(file, copy) VALUES(?1, 'Kopie 1')", [bf]).unwrap();
    let pairs = pairs::find(&f.db).unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(pairs.iter().all(|p| p.level == Level::OwnData));
    let out = run(&f, &pairs, true);
    assert_eq!(out.removed, 0);
    assert!(f.path(1, "A.JPG").exists() && f.path(1, "B.JPG").exists());
}

#[test]
fn transfer_adds_only_and_removes_the_jpeg() {
    let mut f = fx("transfer");
    let (_, rimg) = f.photo(1, "A.CR3", "CR3", "Canon R", T, 0, true);
    let (_, jimg) = f.photo(1, "A.JPG", "JPG", "Canon R", T, 0, true);
    f.db.conn.execute("UPDATE image SET stars = 4, flag = 1, label = 3, caption = 'Hallo' WHERE id = ?1", [jimg]).unwrap();
    f.db.conn.execute("UPDATE image SET stars = 2 WHERE id = ?1", [rimg]).unwrap();
    f.db.conn.execute("INSERT INTO keyword(id, name) VALUES(1, 'Anna'), (2, 'Rom')", []).unwrap();
    f.db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, 1), (?1, 2), (?2, 2)", params![jimg, rimg]).unwrap();
    f.db.conn.execute("INSERT INTO collection(id, name, kind) VALUES(1, 'Urlaub', 0)", []).unwrap();
    f.db.conn.execute("INSERT INTO collection_image(collection, image) VALUES(1, ?1)", [jimg]).unwrap();
    let pairs = pairs::find(&f.db).unwrap();
    assert_eq!(pairs[0].level, Level::Safe);
    let t = &pairs[0].transfer;
    assert_eq!((t.stars, t.flag, t.label), (None, Some(1), Some(3)), "RAW has stars already: not overwritten");
    assert_eq!(t.keywords, vec![1]);
    assert_eq!(t.collections.len(), 1);
    // without transfer: stays
    let out = run(&f, &pairs, false);
    assert_eq!((out.removed, out.skipped.len()), (0, 1));
    assert!(f.path(1, "A.JPG").exists());
    // with transfer
    let out = run(&f, &pairs, true);
    assert_eq!((out.removed, out.transferred, out.errors.len()), (1, 1, 0), "{out:?}");
    assert!(!f.path(1, "A.JPG").exists());
    assert!(f.path(1, "A.CR3").exists(), "the RAW is untouched");
    let (s, m, e, c): (i64, i64, i64, Option<String>) = f.db.conn.query_row("SELECT stars, flag, label, caption FROM image WHERE id = ?1", [rimg], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!((s, m, e, c.as_deref()), (2, 1, 3, Some("Hallo")));
    let kw: i64 = f.db.conn.query_row("SELECT count(*) FROM image_keyword WHERE image = ?1", [rimg], |r| r.get(0)).unwrap();
    assert_eq!(kw, 2);
    let col: i64 = f.db.conn.query_row("SELECT count(*) FROM collection_image WHERE image = ?1", [rimg], |r| r.get(0)).unwrap();
    assert_eq!(col, 1);
    let left: i64 = f.db.conn.query_row("SELECT count(*) FROM file WHERE name = 'A.JPG'", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
    assert_eq!(pairs::find(&f.db).unwrap().len(), 0);
}

#[test]
fn checks_run_again_before_removing() {
    let mut f = fx("verify");
    f.photo(1, "A.CR3", "CR3", "Canon R", T, 0, true);
    f.photo(1, "A.JPG", "JPG", "Canon R", T, 0, true);
    f.photo(1, "B.CR3", "CR3", "Canon R", "2024-05-11T14:10:00", 0, false); // RAW file gone from disk, library says present
    f.photo(1, "B.JPG", "JPG", "Canon R", "2024-05-11T14:10:00", 0, true);
    f.photo(1, "C.CR3", "CR3", "Canon R", "2024-05-11T14:20:00", 0, true);
    f.photo(1, "C.JPG", "JPG", "Canon R", "2024-05-11T14:20:00", 0, false); // JPEG file gone
    let pairs = pairs::find(&f.db).unwrap();
    assert_eq!(pairs.len(), 3);
    // sidecars: A.xmp belongs to the RAW and must stay, A.JPG.xmp goes with the JPEG
    std::fs::write(f.path(1, "A.xmp"), "x").unwrap();
    std::fs::write(f.path(1, "A.JPG.xmp"), "x").unwrap();
    let out = run(&f, &pairs, true);
    assert_eq!(out.removed, 1, "{out:?}");
    assert_eq!(out.skipped.len(), 2);
    assert!(f.path(1, "A.xmp").exists());
    assert!(!f.path(1, "A.JPG.xmp").exists());
    assert!(f.path(1, "B.JPG").exists(), "RAW not on disk: the JPEG stays");
    // symlink instead of file: refused
    f.photo(1, "D.CR3", "CR3", "Canon R", "2024-05-11T15:00:00", 0, true);
    f.photo(1, "D.JPG", "JPG", "Canon R", "2024-05-11T15:00:00", 0, false);
    let target = f.dir.join("outside.jpg");
    std::fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, f.path(1, "D.JPG")).unwrap();
    let d: Vec<_> = pairs::find(&f.db).unwrap().into_iter().filter(|p| p.jpeg.name == "D.JPG").collect();
    let out = run(&f, &d, true);
    assert_eq!(out.removed, 0);
    assert!(target.exists());
}

#[test]
fn cancelling_stops_between_pairs_and_permanent_deletes_regular_files_only() {
    let mut f = fx("cancel");
    for (i, n) in ["A", "B"].iter().enumerate() {
        let t = format!("2024-05-11T14:0{i}:00");
        f.photo(1, &format!("{n}.CR3"), "CR3", "Canon R", &t, 0, true);
        f.photo(1, &format!("{n}.JPG"), "JPG", "Canon R", &t, 0, true);
    }
    let pairs = pairs::find(&f.db).unwrap();
    let calls = std::cell::Cell::new(0);
    let out = pairs::remove(&f.db, &pairs, Options { disposal: Disposal::Permanent, transfer: true }, &|_, files| pairs::delete_permanently(files), &|_, _| {}, &|| {
        calls.set(calls.get() + 1);
        calls.get() > 1
    });
    assert!(out.cancelled);
    assert_eq!(out.removed, 1);
    assert!(f.path(1, "A.CR3").exists() && f.path(1, "B.CR3").exists());
    assert!(!f.path(1, "A.JPG").exists() && f.path(1, "B.JPG").exists());
    let dir: &Path = &f.dir;
    assert!(pairs::delete_permanently(&[dir.to_path_buf()]).is_err(), "a folder is never deleted");
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn removes_in_blocks_and_counts_partial_failures() {
    let mut f = fx("blocks");
    let n = pairs::BLOCK + 5;
    for i in 0..n {
        let t = format!("2024-05-{:02}T{:02}:{:02}:00", 1 + i / 1440, i / 60 % 24, i % 60);
        f.photo(1, &format!("P{i:04}.CR3"), "CR3", "Canon R", &t, 0, true);
        f.photo(1, &format!("P{i:04}.JPG"), "JPG", "Canon R", &t, 0, true);
    }
    let found = pairs::find(&f.db).unwrap();
    assert_eq!(found.len(), n);
    // one call per block
    let calls = std::cell::Cell::new(0);
    let out = pairs::remove(&f.db, &found[..pairs::BLOCK + 2], Options { disposal: Disposal::Trash, transfer: true }, &|db, files| {
        calls.set(calls.get() + 1);
        fake_dispose(db, files)
    }, &|_, _| {}, &|| false);
    assert_eq!((out.removed, calls.get(), out.errors.len()), (pairs::BLOCK + 2, 2, 0), "{out:?}");
    // the Trash fails after the first file: that one counts as removed, the others stay as errors
    let rest = pairs::find(&f.db).unwrap();
    assert_eq!(rest.len(), 3);
    let out = pairs::remove(&f.db, &rest, Options { disposal: Disposal::Trash, transfer: true }, &|db, files| {
        fake_dispose(db, &files[..1])?;
        Err("Papierkorb nicht erreichbar".into())
    }, &|_, _| {}, &|| false);
    assert_eq!((out.removed, out.errors.len()), (1, 2), "{out:?}");
    assert_eq!(pairs::find(&f.db).unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&f.dir);
}

/// The shared Trash building block: `pairs::dispose` hands all local files of a block to the system Trash in ONE
/// request (here a stand-in), and a failing file does not keep the others back.
#[test]
fn dispose_uses_one_trash_request_per_block() {
    let mut f = fx("gemeinsam");
    let n = 5;
    for i in 0..n {
        let t = format!("2024-06-01T10:00:{i:02}");
        f.photo(1, &format!("Q{i}.CR3"), "CR3", "Canon R", &t, 0, true);
        f.photo(1, &format!("Q{i}.JPG"), "JPG", "Canon R", &t, 0, true);
    }
    let found = pairs::find(&f.db).unwrap();
    let bin = f.dir.join("_papierkorb");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = std::cell::Cell::new(0);
    let stand_in = |files: &[PathBuf]| -> Result<(), String> {
        calls.set(calls.get() + 1);
        for x in files {
            std::fs::rename(x, bin.join(x.file_name().unwrap())).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let out = pairs::remove(&f.db, &found, Options { disposal: Disposal::Trash, transfer: true }, &|db, files| pairs::dispose(db, files, Disposal::Trash, &stand_in), &|_, _| {}, &|| false);
    assert_eq!((out.removed, out.errors.len(), calls.get()), (n, 0, 1), "{out:?}");
    assert!(bin.join("Q0.JPG").exists() && f.path(1, "Q0.CR3").exists());
    let _ = std::fs::remove_dir_all(&f.dir);
}
