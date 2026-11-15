//! Cleaning up duplicates against a synthetic library (files only under /private/tmp): suggestion, full content
//! check, carrying data over, collected Trash, ⌘Z of the entries.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use library::db::Db;
use library::duplicate_cleanup::{self as dc, Options, Removal};
use library::duplicates::{self, Kind};
use library::trash_batch::Targets;
use rusqlite::params;

struct Fx {
    dir: PathBuf,
    db: Db,
    next: i64,
}

fn fx(name: &str) -> Fx {
    let dir = PathBuf::from(format!("/private/tmp/graineo-dupes-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("photos/a")).unwrap();
    std::fs::create_dir_all(dir.join("photos/Backup")).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let root = format!("{}/", dir.join("photos").display());
    db.conn.execute("INSERT INTO root_folder(id, path, name, origin) VALUES(1, ?1, 'Fotos', 'own')", [&root]).unwrap();
    db.conn.execute("INSERT INTO folder(id, root, path) VALUES(1, 1, 'a/'), (2, 1, 'Backup/')", []).unwrap();
    Fx { dir, db, next: 1 }
}

/// 200 KB: start and end fixed, the middle byte `mid` (partial checksum does not see it)
fn content(mid: u8) -> Vec<u8> {
    let mut v: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    v[100_000] = mid;
    v
}

impl Fx {
    fn photo(&mut self, folder: i64, name: &str, data: &[u8]) -> i64 {
        let id = self.next;
        self.next += 1;
        let sub = if folder == 2 { "Backup" } else { "a" };
        std::fs::write(self.dir.join("photos").join(sub).join(name), data).unwrap();
        self.db
            .conn
            .execute(
                "INSERT INTO file(id, folder, name, format, size, camera, capture_time, width, height, status) VALUES(?1, ?2, ?3, 'JPG', ?4, 'Cam', '2024-05-11T14:03:22', 6000, 4000, 0)",
                params![id, folder, name, data.len() as i64],
            )
            .unwrap();
        self.db.conn.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
        id
    }
    fn path(&self, folder: i64, name: &str) -> PathBuf {
        self.dir.join("photos").join(if folder == 2 { "Backup" } else { "a" }).join(name)
    }
}

fn fake_trash(calls: &Cell<usize>) -> impl Fn(&[PathBuf]) -> Result<(), String> + '_ {
    move |files: &[PathBuf]| {
        calls.set(calls.get() + 1);
        for f in files {
            let to = f.parent().unwrap().parent().unwrap().parent().unwrap().join("_papierkorb").join(f.file_name().unwrap());
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::rename(f, to).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn removals(p: &dc::Plan) -> Vec<Removal> {
    p.members.iter().zip(&p.suggested).filter(|(_, s)| **s).map(|(m, _)| Removal { remove: m.clone(), keep: p.members[0].clone(), identical: p.kind == Kind::Identical }).collect()
}

#[test]
fn identical_backup_copy_goes_data_moves_undo_restores() {
    let mut f = fx("ganz");
    let keep = f.photo(1, "IMG_1.JPG", &content(1));
    let copy = f.photo(2, "IMG_1.JPG", &content(1));
    std::fs::write(f.path(2, "IMG_1.JPG.xmp"), b"<x/>").unwrap();
    // the copy in the backup folder has the rating and a keyword
    f.db.conn.execute("UPDATE image SET stars = 4 WHERE id = ?1", [copy]).unwrap();
    f.db.conn.execute("INSERT INTO keyword(id, name) VALUES(7, 'Urlaub')", []).unwrap();
    f.db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, 7)", [copy]).unwrap();

    let groups = duplicates::find(&f.db, None).unwrap();
    assert_eq!(groups.len(), 1);
    assert!(groups[0].verified);
    let (plans, left_out) = dc::plans(&f.db, &groups).unwrap();
    assert_eq!(left_out, 0);
    let p = &plans[0];
    // the rated copy wins (data before main folder)
    assert_eq!(p.members[0].image, copy);
    assert_eq!(dc::decisive(&p.members[0], &p.members[1]), Some(dc::Criterion::Data));
    assert_eq!(p.suggested, vec![false, true]);

    // without data on the copy the main folder wins
    f.db.conn.execute("UPDATE image SET stars = 0 WHERE id = ?1", [copy]).unwrap();
    f.db.conn.execute("DELETE FROM image_keyword", []).unwrap();
    let (plans, _) = dc::plans(&f.db, &groups).unwrap();
    assert_eq!(plans[0].members[0].image, keep);
    assert_eq!(dc::decisive(&plans[0].members[0], &plans[0].members[1]), Some(dc::Criterion::MainFolder));
    // the data comes back on the copy: it is carried over to the staying photo before the copy goes
    f.db.conn.execute("UPDATE image SET stars = 4 WHERE id = ?1", [copy]).unwrap();
    f.db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, 7)", [copy]).unwrap();

    let calls = Cell::new(0);
    let local = fake_trash(&calls);
    let t = Targets { local: &local, is_network: &|_| false, root_of: &|_: &Path| None, day: "2026-11-11".into() };
    let out = dc::remove(&f.db, &removals(&plans[0]), Options { transfer: true }, &t, &|_, _, _| {}, &|| false);
    assert_eq!(out.removed, 1, "{:?} {:?}", out.skipped, out.errors);
    assert_eq!(out.transferred, 1);
    assert_eq!(calls.get(), 1);
    assert!(!f.path(2, "IMG_1.JPG").exists() && !f.path(2, "IMG_1.JPG.xmp").exists());
    assert!(f.path(1, "IMG_1.JPG").exists());
    let stars: i64 = f.db.conn.query_row("SELECT stars FROM image WHERE id = ?1", [keep], |r| r.get(0)).unwrap();
    assert_eq!(stars, 4);
    let kw: i64 = f.db.conn.query_row("SELECT count(*) FROM image_keyword WHERE image = ?1", [keep], |r| r.get(0)).unwrap();
    assert_eq!(kw, 1);
    let gone: i64 = f.db.conn.query_row("SELECT count(*) FROM file WHERE id = ?1", [copy], |r| r.get(0)).unwrap();
    assert_eq!(gone, 0);
    // not excluded: put back from the Trash, the sync takes it in again
    let excluded: i64 = f.db.conn.query_row("SELECT count(*) FROM excluded_path", [], |r| r.get(0)).unwrap();
    assert_eq!(excluded, 0);
    // ⌘Z brings the entry back with its rating
    library::manage::undo(&f.db, &out.snapshot).unwrap();
    let back: i64 = f.db.conn.query_row("SELECT stars FROM image WHERE id = ?1", [copy], |r| r.get(0)).unwrap();
    assert_eq!(back, 4);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn full_checksum_stops_same_looking_files() {
    let mut f = fx("pruefsumme");
    let a = f.photo(1, "IMG_2.JPG", &content(1));
    let b = f.photo(2, "IMG_2.JPG", &content(2));
    let groups = duplicates::find(&f.db, None).unwrap();
    assert_eq!(groups.len(), 1, "partial checksum cannot tell them apart");
    let (plans, _) = dc::plans(&f.db, &groups).unwrap();
    let calls = Cell::new(0);
    let local = fake_trash(&calls);
    let t = Targets { local: &local, is_network: &|_| false, root_of: &|_: &Path| None, day: "2026-11-11".into() };
    let out = dc::remove(&f.db, &removals(&plans[0]), Options { transfer: true }, &t, &|_, _, _| {}, &|| false);
    assert_eq!(out.removed, 0);
    assert_eq!(out.skipped.len(), 1);
    assert!(out.skipped[0].1.contains("Prüfsumme"));
    assert_eq!(calls.get(), 0);
    assert!(f.path(1, "IMG_2.JPG").exists() && f.path(2, "IMG_2.JPG").exists());
    let _ = (a, b);
    let _ = std::fs::remove_dir_all(&f.dir);
}

#[test]
fn developed_and_missing_photos_stay_and_transfer_can_be_off() {
    let mut f = fx("bleiben");
    let a = f.photo(1, "IMG_3.JPG", &content(3));
    let b = f.photo(2, "IMG_3.JPG", &content(3));
    let c = f.photo(2, "IMG_3 kopie.JPG", &content(3));
    // b: developed → keeps (criterion 2); c: rated, transfer off → stays
    f.db.conn.execute("INSERT INTO develop_state(file, copy, settings) VALUES(?1, '', '{}')", [b]).unwrap();
    f.db.conn.execute("UPDATE image SET stars = 2 WHERE id = ?1", [c]).unwrap();
    let groups = duplicates::find(&f.db, None).unwrap();
    let (plans, _) = dc::plans(&f.db, &groups).unwrap();
    let p = &plans[0];
    assert_eq!(p.members[0].image, b);
    let calls = Cell::new(0);
    let local = fake_trash(&calls);
    let t = Targets { local: &local, is_network: &|_| false, root_of: &|_: &Path| None, day: "2026-11-11".into() };
    let all: Vec<Removal> = p.members[1..].iter().map(|m| Removal { remove: m.clone(), keep: p.members[0].clone(), identical: true }).collect();
    let out = dc::remove(&f.db, &all, Options { transfer: false }, &t, &|_, _, _| {}, &|| false);
    assert_eq!(out.removed, 1, "only the plain one: {:?}", out.skipped);
    assert!(!f.path(1, "IMG_3.JPG").exists());
    assert!(f.path(2, "IMG_3 kopie.JPG").exists());
    // the developed one as the one to remove: refused
    let r = Removal { remove: p.members[0].clone(), keep: p.members.iter().find(|m| m.image == c).unwrap().clone(), identical: true };
    let out = dc::remove(&f.db, &[r], Options { transfer: true }, &t, &|_, _, _| {}, &|| false);
    assert_eq!(out.removed, 0);
    assert!(out.skipped[0].1.contains("Entwicklung"));
    let _ = a;
    let _ = std::fs::remove_dir_all(&f.dir);
}
