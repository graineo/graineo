//! "Move to …" for folders: between two places (copy path forced with `GRAINEO_MOVE_COPY`, as on two volumes),
//! cancel in the middle, conflict at the target, previews after the move, target unreachable afterwards, back again,
//! same volume (rename). Everything in own temporary folders; `GRAINEO_DATA_DIR` points there.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use library::db::Db;
use library::move_folder::{self, Developed};
use library::preview::{self, Cache};
use library::reconcile;

/// Environment variables are per process: the tests run one after another.
static LOCK: Mutex<()> = Mutex::new(());

struct Env {
    dir: PathBuf,
    mac: PathBuf,
    nas: PathBuf,
    trash: PathBuf,
    db: Db,
    cache: Cache,
    _g: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn jpeg(p: &Path, shade: u8) {
    let img = image::RgbImage::from_fn(640, 480, |x, y| image::Rgb([shade, (x % 256) as u8, (y % 256) as u8]));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    img.save_with_format(p, image::ImageFormat::Jpeg).unwrap();
}

fn env(copy: bool) -> Env {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("graineo-move-{}-{n}", std::process::id()));
    let (mac, nas, trash) = (dir.join("mac/Fotos"), dir.join("nas"), dir.join("trash"));
    for d in [&mac, &nas, &trash] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::env::set_var("GRAINEO_DATA_DIR", dir.join("data"));
    std::env::set_var("GRAINEO_LIBRARY_JOURNAL", dir.join("data/journal"));
    std::env::set_var("GRAINEO_LIBRARY_SIDECARS", "0");
    std::env::set_var("GRAINEO_PREVIEW_MIN_FREE_GB", "0");
    if copy {
        std::env::set_var("GRAINEO_MOVE_COPY", "1");
    } else {
        std::env::remove_var("GRAINEO_MOVE_COPY");
    }
    let vacation = mac.join("2024/Urlaub");
    for (i, n) in ["A.jpg", "B.jpg", "C.jpg"].iter().enumerate() {
        jpeg(&vacation.join(n), 40 * i as u8);
    }
    jpeg(&vacation.join("Tag 2/D.jpg"), 200);
    std::fs::write(vacation.join("A.jpg.xmp"), b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>").unwrap();
    std::fs::create_dir_all(vacation.join(".graineo")).unwrap();
    std::fs::write(vacation.join(".graineo/collections.json"), b"{}").unwrap();
    std::fs::write(vacation.join("notizen.txt"), b"Strand").unwrap();
    std::fs::write(vacation.join(".DS_Store"), b"x").unwrap();
    jpeg(&mac.join("2024/anderes.jpg"), 90);
    let db = Db::open(&dir.join("data/library.sqlite")).unwrap();
    reconcile::add_root(&db, &mac, &|_| {}).unwrap();
    let cache = Cache::open(&dir.join("data/previews"));
    Env { dir, mac, nas, trash, db, cache, _g: g }
}

impl Env {
    fn folder(&self, root_path: &str, rel: &str) -> i64 {
        self.db.conn.query_row("SELECT o.id FROM folder o JOIN root_folder s ON s.id=o.root WHERE s.path=?1 AND o.path=?2", [root_path, rel], |r| r.get(0)).unwrap()
    }

    fn root_s(p: &Path) -> String {
        format!("{}/", p.display())
    }

    /// file id → path, rating
    fn files(&self) -> Vec<(i64, String, i64)> {
        let mut st = self.db.conn.prepare("SELECT d.id, s.path||o.path||d.name, b.stars FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root JOIN image b ON b.file=d.id AND b.copy IS NULL ORDER BY d.id").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
    }

    /// trash that moves into an own folder (like the macOS Trash: nothing is deleted)
    fn dispose(&self) -> impl Fn(&[PathBuf], &Path) -> Result<(), String> + '_ {
        move |list: &[PathBuf], _root: &Path| {
            for p in list {
                let n = std::fs::read_dir(&self.trash).map(|r| r.count()).unwrap_or(0);
                let t = self.trash.join(format!("{n}-{}", p.file_name().unwrap().to_string_lossy()));
                std::fs::rename(p, t).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
}

fn no_develop(_: &move_folder::Photo, _: bool) -> Developed {
    Developed::Nothing
}

#[test]
fn move_between_locations_keeps_everything_and_previews() {
    let e = env(true);
    let root = Env::root_s(&e.mac);
    let folder = e.folder(&root, "2024/Urlaub/");
    // rating, keyword, collection, virtual copy on photo A
    let a: i64 = e.db.conn.query_row("SELECT d.id FROM file d WHERE d.name='A.jpg'", [], |r| r.get(0)).unwrap();
    let image_a: i64 = e.db.conn.query_row("SELECT id FROM image WHERE file=?1", [a], |r| r.get(0)).unwrap();
    e.db.conn.execute("UPDATE image SET stars=4 WHERE id=?1", [image_a]).unwrap();
    e.db.conn.execute("INSERT INTO image(file, copy, stars) VALUES(?1, 'Kopie 1', 2)", [a]).unwrap();
    e.db.conn.execute("INSERT INTO keyword(id, name) VALUES(77, 'Strand')", []).unwrap();
    e.db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, 77)", [image_a]).unwrap();
    e.db.conn.execute("INSERT INTO collection(id, name, kind) VALUES(5, 'Urlaub', 0)", []).unwrap();
    e.db.conn.execute("INSERT INTO collection_image(collection, image) VALUES(5, ?1)", [image_a]).unwrap();
    let before = e.files();

    let plan = move_folder::plan(&e.db, folder, &e.nas).unwrap();
    assert!(!plan.same_volume);
    assert_eq!(plan.photos.len(), 4);
    assert_eq!(plan.files.len(), 7, "{:?}", plan.files); // 4 JPEG, XMP, .graineo/…, notizen.txt (no .DS_Store)
    assert_eq!(plan.target, e.nas.join("Urlaub"));

    let cancel = AtomicBool::new(false);
    let t = std::time::Instant::now();
    let p = move_folder::prepare(&plan, &e.cache, &cancel, &|_| {}, &no_develop).unwrap();
    eprintln!("prepare: {} made, {} present in {:.2} s", p.made, p.present, t.elapsed().as_secs_f64());
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let (small, large) = (preview::small(), preview::large());
    for ph in &plan.photos {
        assert!(e.cache.has(ph.key(), small) && e.cache.has(ph.key(), large));
    }
    let src_mtime = std::fs::metadata(e.mac.join("2024/Urlaub/B.jpg")).unwrap().modified().unwrap();
    let dispose = e.dispose();
    let out = move_folder::execute(&e.db, &plan, Some(&e.cache), &cancel, &|_| {}, &dispose).unwrap();
    eprintln!("copy: {} files, {} bytes in {:.3} s, previews moved: {}", out.files, out.bytes, out.copy_secs, out.previews_moved);
    assert_eq!(out.previews_moved, 8, "small + large of 4 photos");
    assert!(out.notes.is_empty(), "{:?}", out.notes);

    // files at the target, same content, same modification time; source in the trash
    let t = e.nas.join("Urlaub");
    for r in ["A.jpg", "B.jpg", "C.jpg", "Tag 2/D.jpg", "A.jpg.xmp", ".graineo/collections.json", "notizen.txt"] {
        assert!(t.join(r).is_file(), "{r}");
    }
    assert_eq!(std::fs::metadata(t.join("B.jpg")).unwrap().modified().unwrap(), src_mtime);
    assert!(!e.mac.join("2024/Urlaub").exists());
    assert_eq!(std::fs::read_dir(&e.trash).unwrap().count(), 1, "whole folder into the trash");
    assert!(e.mac.join("2024/anderes.jpg").exists());

    // library: same IDs, new paths, ratings/keyword/collection/copy kept; new root folder
    let after = e.files();
    assert_eq!(before.len(), after.len());
    for (b, a) in before.iter().zip(&after) {
        assert_eq!(b.0, a.0);
        assert_eq!(b.2, a.2);
        if b.1.contains("/Urlaub/") {
            assert!(a.1.starts_with(&format!("{}/Urlaub/", e.nas.display())), "{}", a.1);
        } else {
            assert_eq!(b.1, a.1);
        }
    }
    let n: i64 = e.db.conn.query_row("SELECT count(*) FROM image WHERE file=?1", [a], |r| r.get(0)).unwrap();
    assert_eq!(n, 2, "virtual copy stays");
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM image_keyword WHERE image=?1", [image_a], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM collection_image WHERE image=?1", [image_a], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let new_root = format!("{}/Urlaub/", e.nas.display());
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM root_folder WHERE path=?1", [&new_root], |r| r.get::<_, i64>(0)).unwrap(), 1);

    // previews under the new key, nothing has to be made again
    let moved = move_folder::plan(&e.db, e.folder(&new_root, ""), &e.mac).unwrap();
    for ph in &moved.photos {
        assert!(e.cache.has(ph.key(), small) && e.cache.has(ph.key(), large), "{}", ph.path.display());
        assert!(!preview::ensure(&e.cache, &ph.job(large)).unwrap(), "not made again");
    }
    assert_eq!(library::relocate::journals()[0].status, "done");
    // ⌘Z does not apply to a move between locations (the source is in the trash)
    let mut j = library::relocate::journals().remove(0);
    assert!(library::relocate::undo(&e.db, &mut j).is_err());
    assert!(t.join("A.jpg").is_file());

    // target not reachable afterwards (volume gone): state 2, previews still there
    let gone = e.dir.join("nas-weg");
    std::fs::rename(&e.nas, &gone).unwrap();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    let states: Vec<i64> = {
        let mut st = e.db.conn.prepare("SELECT d.status FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE s.path=?1").unwrap();
        st.query_map([&new_root], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(states, vec![2, 2, 2, 2]);
    for ph in &moved.photos {
        assert!(e.cache.read(ph.key(), large).is_some(), "large preview visible while unreachable");
    }
    std::fs::rename(&gone, &e.nas).unwrap();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();

    // back to this Mac, into the existing root folder: the separate root folder disappears again, IDs stay
    let back = move_folder::plan(&e.db, e.folder(&new_root, ""), &e.mac.join("2024")).unwrap();
    move_folder::execute(&e.db, &back, Some(&e.cache), &cancel, &|_| {}, &dispose).unwrap();
    assert_eq!(e.files(), before);
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM root_folder WHERE path=?1", [&new_root], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert!(e.mac.join("2024/Urlaub/Tag 2/D.jpg").is_file());
    let home = move_folder::plan(&e.db, folder, &e.nas).unwrap();
    for ph in &home.photos {
        assert!(e.cache.has(ph.key(), large), "previews back under the old key");
    }
}

#[test]
fn cancel_in_the_middle_rolls_everything_back() {
    let e = env(true);
    let root = Env::root_s(&e.mac);
    let folder = e.folder(&root, "2024/Urlaub/");
    let before = e.files();
    let plan = move_folder::plan(&e.db, folder, &e.nas).unwrap();
    let cancel = AtomicBool::new(false);
    let calls = AtomicUsize::new(0);
    let progress = |p: move_folder::Progress| {
        if p.phase == move_folder::Phase::Copy && p.done >= 3 && calls.fetch_add(1, Ordering::SeqCst) == 0 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let dispose = e.dispose();
    let err = move_folder::execute(&e.db, &plan, Some(&e.cache), &cancel, &progress, &dispose).unwrap_err();
    assert_eq!(err, library::relocate::CANCELLED);
    assert!(!e.nas.join("Urlaub").exists(), "copies and folders removed");
    assert_eq!(std::fs::read_dir(&e.nas).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&e.trash).unwrap().count(), 0, "nothing into the trash");
    for r in ["A.jpg", "B.jpg", "C.jpg", "Tag 2/D.jpg", "A.jpg.xmp", "notizen.txt"] {
        assert!(e.mac.join("2024/Urlaub").join(r).is_file(), "{r}");
    }
    assert_eq!(e.files(), before);
    assert_eq!(library::relocate::journals()[0].status, "failed");
}

#[test]
fn interrupted_copy_is_rolled_back_at_start() {
    let e = env(true);
    let root = Env::root_s(&e.mac);
    let plan = move_folder::plan(&e.db, e.folder(&root, "2024/Urlaub/"), &e.nas).unwrap();
    // an error in the middle of the copy: a foreign file appears at the target after planning
    std::fs::create_dir_all(e.nas.join("Urlaub/Tag 2")).unwrap();
    std::fs::write(e.nas.join("Urlaub/Tag 2/D.jpg"), b"fremd").unwrap();
    let dispose = e.dispose();
    let err = move_folder::execute(&e.db, &plan, Some(&e.cache), &AtomicBool::new(false), &|_| {}, &dispose).unwrap_err();
    assert!(err.contains("schon"), "{err}");
    assert_eq!(std::fs::read(e.nas.join("Urlaub/Tag 2/D.jpg")).unwrap(), b"fremd", "foreign file untouched");
    assert!(!e.nas.join("Urlaub/A.jpg").exists());
    assert!(e.mac.join("2024/Urlaub/A.jpg").exists());
    // power failure in the middle of the copy (here: a panic after the third file): the journal says "running";
    // at the next start everything copied is removed again
    std::fs::remove_dir_all(e.nas.join("Urlaub")).unwrap();
    let plan = move_folder::plan(&e.db, e.folder(&root, "2024/Urlaub/"), &e.nas).unwrap();
    let crash = |p: move_folder::Progress| {
        if p.phase == move_folder::Phase::Copy && p.done == 3 {
            panic!("Stromausfall");
        }
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| move_folder::execute(&e.db, &plan, Some(&e.cache), &AtomicBool::new(false), &crash, &dispose)));
    assert!(r.is_err());
    let j = library::relocate::journals().remove(0);
    assert_eq!(j.status, "running");
    // the third copy is installed, but not yet noted in the journal
    assert_eq!(j.steps.iter().filter(|s| s.finished).count(), 2);
    assert!(e.nas.join("Urlaub").exists());
    let m = move_folder::resume(&e.db, &dispose);
    assert_eq!(m.len(), 1, "{m:?}");
    assert!(!e.nas.join("Urlaub").exists(), "{m:?}");
    assert!(e.mac.join("2024/Urlaub/A.jpg").exists());
    assert_eq!(library::relocate::journals()[0].status, "failed");
}

#[test]
fn conflict_at_the_target_stops_before_anything_happens() {
    let e = env(true);
    let root = Env::root_s(&e.mac);
    let folder = e.folder(&root, "2024/Urlaub/");
    std::fs::create_dir_all(e.nas.join("Urlaub")).unwrap();
    let err = move_folder::plan(&e.db, folder, &e.nas).unwrap_err();
    assert!(err.contains("schon „Urlaub“"), "{err}");
    // into itself
    assert!(move_folder::plan(&e.db, folder, &e.mac.join("2024/Urlaub/Tag 2")).is_err());
    // the target appears between plan and start
    std::fs::remove_dir(e.nas.join("Urlaub")).unwrap();
    let plan = move_folder::plan(&e.db, folder, &e.nas).unwrap();
    std::fs::create_dir_all(e.nas.join("Urlaub")).unwrap();
    let dispose = e.dispose();
    assert!(move_folder::execute(&e.db, &plan, Some(&e.cache), &AtomicBool::new(false), &|_| {}, &dispose).is_err());
    assert!(e.mac.join("2024/Urlaub/A.jpg").exists());
}


#[test]
fn same_volume_is_a_rename_and_root_keeps_its_id() {
    let e = env(false);
    let root = Env::root_s(&e.mac);
    let before = e.files();
    let plan = move_folder::plan(&e.db, e.folder(&root, "2024/Urlaub/"), &e.nas).unwrap();
    assert!(plan.same_volume);
    let dispose = |_: &[PathBuf], _: &Path| -> Result<(), String> { panic!("no trash on the same volume") };
    let out = move_folder::execute(&e.db, &plan, Some(&e.cache), &AtomicBool::new(false), &|_| {}, &dispose).unwrap();
    assert!(out.same_volume);
    assert!(e.nas.join("Urlaub/Tag 2/D.jpg").is_file() && !e.mac.join("2024/Urlaub").exists());
    assert_eq!(e.files().len(), before.len());
    // whole root folder outside of all root folders: the root keeps its ID and gets the new path
    let root_id: i64 = e.db.conn.query_row("SELECT id FROM root_folder WHERE path=?1", [&root], |r| r.get(0)).unwrap();
    let other = e.dir.join("extern");
    std::fs::create_dir_all(&other).unwrap();
    let plan = move_folder::plan(&e.db, e.folder(&root, ""), &other).unwrap();
    move_folder::execute(&e.db, &plan, Some(&e.cache), &AtomicBool::new(false), &|_| {}, &dispose).unwrap();
    let p: String = e.db.conn.query_row("SELECT path FROM root_folder WHERE id=?1", [root_id], |r| r.get(0)).unwrap();
    assert_eq!(p, format!("{}/Fotos/", other.display()));
    assert!(other.join("Fotos/2024/anderes.jpg").is_file());
}

#[test]
fn sidecar_written_during_the_copy_arrives_at_the_target() {
    let e = env(true);
    let root = Env::root_s(&e.mac);
    let plan = move_folder::plan(&e.db, e.folder(&root, "2024/Urlaub/"), &e.nas).unwrap();
    let xmp = e.mac.join("2024/Urlaub/A.jpg.xmp");
    let written = AtomicBool::new(false);
    let progress = |p: move_folder::Progress| {
        // the XMP is copied early (sorted); shortly before the end a rating lands in the source
        if p.phase == move_folder::Phase::Copy && p.done + 1 == p.total && !written.swap(true, Ordering::SeqCst) {
            std::fs::write(&xmp, b"<x:xmpmeta xmlns:x='adobe:ns:meta/'><!-- 5 Sterne --></x:xmpmeta>").unwrap();
        }
    };
    let dispose = e.dispose();
    move_folder::execute(&e.db, &plan, Some(&e.cache), &AtomicBool::new(false), &progress, &dispose).unwrap();
    assert!(written.load(Ordering::SeqCst));
    assert!(std::fs::read_to_string(e.nas.join("Urlaub/A.jpg.xmp")).unwrap().contains("5 Sterne"));
}
