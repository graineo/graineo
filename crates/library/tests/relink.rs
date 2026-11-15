//! "Locate missing folder": a root folder or folder renamed/moved outside the app is linked again (same IDs, previews
//! along, files untouched); partial matches, the wrong folder, a folder already in the library, a
//! folder that is not missing. Everything in own temporary folders; `GRAINEO_DATA_DIR` points there.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use library::db::Db;
use library::move_folder;
use library::preview::{self, Cache};
use library::reconcile;
use library::relink::{self, Found};

/// Environment variables are per process: the tests run one after another.
static LOCK: Mutex<()> = Mutex::new(());

struct Env {
    dir: PathBuf,
    /// root folder of the library: `<dir>/mac/Fotos`
    mac: PathBuf,
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
    let img = image::RgbImage::from_fn(320, 240, |x, y| image::Rgb([shade, (x % 256) as u8, (y % 256) as u8]));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    img.save_with_format(p, image::ImageFormat::Jpeg).unwrap();
}

fn env() -> Env {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("graineo-relink-{}-{n}", std::process::id()));
    let mac = dir.join("mac/Fotos");
    std::fs::create_dir_all(&mac).unwrap();
    std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
    std::env::set_var("GRAINEO_DATA_DIR", dir.join("data"));
    std::env::set_var("GRAINEO_LIBRARY_JOURNAL", dir.join("data/journal"));
    std::env::set_var("GRAINEO_LIBRARY_SIDECARS", "0");
    std::env::set_var("GRAINEO_PREVIEW_MIN_FREE_GB", "0");
    let vacation = mac.join("2024/Urlaub");
    for (i, n) in ["A.jpg", "B.jpg", "C.jpg"].iter().enumerate() {
        jpeg(&vacation.join(n), 40 * i as u8);
    }
    jpeg(&vacation.join("Tag 2/D.jpg"), 200);
    std::fs::write(vacation.join("notizen.txt"), b"Strand").unwrap();
    jpeg(&mac.join("2024/anderes.jpg"), 90);
    let db = Db::open(&dir.join("data/library.sqlite")).unwrap();
    reconcile::add_root(&db, &mac, &|_| {}).unwrap();
    let cache = Cache::open(&dir.join("data/previews"));
    Env { dir, mac, db, cache, _g: g }
}

fn root_s(p: &Path) -> String {
    format!("{}/", p.display())
}

/// Snapshot of a file tree: relative path → (bytes, modification time).
fn tree(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                let m = e.metadata().unwrap();
                out.push((p.strip_prefix(dir).unwrap().display().to_string(), m.len(), m.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

impl Env {
    fn folder(&self, root_path: &str, rel: &str) -> i64 {
        self.db.conn.query_row("SELECT o.id FROM folder o JOIN root_folder s ON s.id=o.root WHERE s.path=?1 AND o.path=?2", [root_path, rel], |r| r.get(0)).unwrap()
    }

    /// file id → path, rating, state
    fn files(&self) -> Vec<(i64, String, i64, i64)> {
        let mut st = self.db.conn.prepare("SELECT d.id, s.path||o.path||d.name, b.stars, d.status FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root JOIN image b ON b.file=d.id AND b.copy IS NULL ORDER BY d.id").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
    }

    /// Small previews of all photos of a folder (keys as the library view forms them); returns the photos.
    fn previews(&self, folder: i64) -> Vec<move_folder::Photo> {
        let plan = move_folder::plan(&self.db, folder, &self.dir.join("elsewhere")).unwrap();
        for p in &plan.photos {
            preview::ensure(&self.cache, &p.job(preview::small())).unwrap();
        }
        plan.photos
    }

    fn photos_of_root(&self, root_path: &str) -> Vec<move_folder::Photo> {
        move_folder::plan(&self.db, self.folder(root_path, ""), &self.dir.join("elsewhere")).unwrap().photos
    }
}

#[test]
fn renamed_root_folder_is_linked_again_with_previews() {
    let e = env();
    let root = root_s(&e.mac);
    let root_id: i64 = e.db.conn.query_row("SELECT id FROM root_folder WHERE path=?1", [&root], |r| r.get(0)).unwrap();
    let a: i64 = e.db.conn.query_row("SELECT id FROM file WHERE name='A.jpg'", [], |r| r.get(0)).unwrap();
    e.db.conn.execute("UPDATE image SET stars=4 WHERE file=?1", [a]).unwrap();
    e.db.conn.execute("INSERT INTO image(file, copy, stars) VALUES(?1, 'Kopie 1', 2)", [a]).unwrap();
    let photos = e.previews(e.folder(&root, ""));
    assert_eq!(photos.len(), 5);
    let before = e.files();

    // renamed in the Finder: the sync marks the root folder as not reachable (state 2)
    let renamed = e.dir.join("mac/Fotos Archiv");
    std::fs::rename(&e.mac, &renamed).unwrap();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert!(e.files().iter().all(|f| f.3 == 2));
    assert!(relink::locatable(&e.mac), "volume present, folder gone");
    let disk_before = tree(&renamed);

    let t = std::time::Instant::now();
    let plan = relink::plan(&e.db, e.folder(&root, ""), &renamed).unwrap();
    eprintln!("plan: {} photos in {:.3} s", plan.total(), t.elapsed().as_secs_f64());
    assert!(plan.is_root && !plan.lightroom);
    assert_eq!((plan.total(), plan.found(), plan.extra), (5, 5, 0));
    assert!(plan.complete());
    assert_eq!(plan.rename_root.as_deref(), Some("Fotos Archiv"));
    let out = relink::execute(&e.db, &plan, Some(&e.cache)).unwrap();
    eprintln!("execute: {} found, {} previews moved", out.found, out.previews_moved);
    assert_eq!((out.found, out.not_found, out.previews_moved, out.root), (5, 0, 5, root_id));

    // same IDs and ratings, new paths, present; root folder keeps its entry, gets path and name
    let after = e.files();
    let new_root = root_s(&renamed);
    for (b, a) in before.iter().zip(&after) {
        assert_eq!((b.0, b.2), (a.0, a.2));
        assert_eq!(a.1, b.1.replacen(&root, &new_root, 1));
        assert_eq!(a.3, 0);
    }
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM image WHERE file=?1", [a], |r| r.get::<_, i64>(0)).unwrap(), 2, "virtual copy stays");
    let (p, n): (String, String) = e.db.conn.query_row("SELECT path, name FROM root_folder WHERE id=?1", [root_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((p.as_str(), n.as_str()), (new_root.as_str(), "Fotos Archiv"));
    // previews under the new key, nothing has to be made again
    for ph in e.photos_of_root(&new_root) {
        assert!(e.cache.has(ph.key(), preview::small()), "{}", ph.path.display());
        assert!(!preview::ensure(&e.cache, &ph.job(preview::small())).unwrap());
    }
    // nothing on disk changed
    assert_eq!(tree(&renamed), disk_before);
    // the sync afterwards finds everything, nothing missing, nothing new
    let r = reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!((r.missing, r.newly_added), (0, 0), "{:?}", r.json());
    assert!(!relink::locatable(&renamed));
    assert!(relink::plan(&e.db, e.folder(&new_root, ""), &renamed).unwrap_err().contains("fehlt nicht"));
}

#[test]
fn subfolder_moved_out_becomes_own_root_partial_match_asks() {
    let e = env();
    let root = root_s(&e.mac);
    let folder = e.folder(&root, "2024/Urlaub/");
    let photos = e.previews(folder);
    assert_eq!(photos.len(), 4);
    // moved out of the root folder (other place on the disk), one photo deleted, one replaced by another file
    let ext = e.dir.join("extern/Urlaub 2024");
    std::fs::create_dir_all(ext.parent().unwrap()).unwrap();
    std::fs::rename(e.mac.join("2024/Urlaub"), &ext).unwrap();
    std::fs::remove_file(ext.join("C.jpg")).unwrap();
    jpeg(&ext.join("B.jpg"), 250); // other size (almost certainly)
    let b_size = std::fs::metadata(ext.join("B.jpg")).unwrap().len();
    jpeg(&ext.join("neu.jpg"), 10);
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert!(relink::locatable(&e.mac.join("2024/Urlaub")));

    let plan = relink::plan(&e.db, folder, &ext).unwrap();
    assert!(!plan.is_root);
    let b = plan.photos.iter().find(|p| p.rel == "B.jpg").unwrap();
    let other_size = Some(b_size as i64) != b.size;
    assert_eq!(plan.total(), 4);
    assert_eq!(plan.count(Found::Absent), 1, "C.jpg");
    assert_eq!(plan.found(), if other_size { 2 } else { 3 });
    assert!(!plan.complete());
    assert_eq!(plan.extra, 1, "neu.jpg");
    assert!(plan.photos.iter().any(|p| p.rel == "Tag 2/D.jpg" && p.found == Found::Exact));
    assert_eq!(plan.rename_root, None);
    let out = relink::execute(&e.db, &plan, Some(&e.cache)).unwrap();
    assert_eq!(out.found, plan.found());
    // new own root folder at the new place; the old root folder keeps the rest
    let new_root = root_s(&ext);
    assert_eq!(e.db.conn.query_row("SELECT origin FROM root_folder WHERE path=?1", [&new_root], |r| r.get::<_, String>(0)).unwrap(), "own");
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM folder WHERE root=?1", [out.root], |r| r.get::<_, i64>(0)).unwrap(), 2, "folder + subfolder");
    for ph in e.photos_of_root(&new_root).iter().filter(|p| p.path.ends_with("A.jpg") || p.path.ends_with("D.jpg")) {
        assert!(e.cache.has(ph.key(), preview::small()), "{}", ph.path.display());
    }
    let r = reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(r.missing, 1, "C.jpg stays missing: {:?}", r.json());
    assert_eq!(r.newly_added, 1, "neu.jpg");
    let c: i64 = e.db.conn.query_row("SELECT status FROM file WHERE name='C.jpg'", [], |r| r.get(0)).unwrap();
    assert_eq!(c, 1);
    assert!(e.mac.join("2024/anderes.jpg").is_file());
}

#[test]
fn wrong_or_known_folders_are_refused() {
    let e = env();
    let root = root_s(&e.mac);
    let folder = e.folder(&root, "2024/Urlaub/");
    let moved = e.dir.join("weg/Urlaub");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(e.mac.join("2024/Urlaub"), &moved).unwrap();

    // the wrong folder: none of the photos there
    let wrong = e.dir.join("falsch");
    jpeg(&wrong.join("X.jpg"), 1);
    let err = relink::plan(&e.db, folder, &wrong).unwrap_err();
    assert!(err.contains("keines der 4 Fotos"), "{err}");

    // a folder already known to the library (as a root folder of its own, or inside a known folder)
    reconcile::add_root(&e.db, &moved, &|_| {}).unwrap();
    let err = relink::plan(&e.db, folder, &moved).unwrap_err();
    assert!(err.contains("schon in der Bibliothek"), "{err}");
    let err = relink::plan(&e.db, folder, moved.parent().unwrap()).unwrap_err();
    assert!(err.contains("schon in der Bibliothek"), "{err}");

    // not missing: the root folder is there
    let err = relink::plan(&e.db, e.folder(&root, ""), &moved).unwrap_err();
    assert!(err.contains("fehlt nicht"), "{err}");
}

#[test]
fn root_moved_into_another_root_is_merged_when_unknown_there() {
    let e = env();
    let root = root_s(&e.mac);
    // second root folder, its subfolder "Alt" is not known yet (nothing synced there)
    let second = e.dir.join("mac/Bilder");
    jpeg(&second.join("Z.jpg"), 5);
    reconcile::add_root(&e.db, &second, &|_| {}).unwrap();
    let photos = e.previews(e.folder(&root, ""));
    let before = e.files();
    let target = second.join("Alt/Fotos");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::rename(&e.mac, &target).unwrap();

    let plan = relink::plan(&e.db, e.folder(&root, ""), &target).unwrap();
    assert_eq!(plan.found(), 5);
    let out = relink::execute(&e.db, &plan, Some(&e.cache)).unwrap();
    assert_eq!(out.previews_moved, photos.len());
    let second_s = root_s(&second);
    let second_id: i64 = e.db.conn.query_row("SELECT id FROM root_folder WHERE path=?1", [&second_s], |r| r.get(0)).unwrap();
    assert_eq!(out.root, second_id);
    assert_eq!(e.db.conn.query_row("SELECT count(*) FROM root_folder WHERE path=?1", [&root], |r| r.get::<_, i64>(0)).unwrap(), 0, "old root folder merged");
    let after = e.files();
    for b in &before {
        let a = after.iter().find(|a| a.0 == b.0).unwrap();
        assert_eq!(a.1, b.1.replacen(&root, &format!("{second_s}Alt/Fotos/"), 1));
    }
    let r = reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!((r.missing, r.newly_added), (0, 0), "{:?}", r.json());
}

#[test]
fn volume_detection() {
    assert_eq!(relink::volume_root(Path::new("/Volumes/nas/Photos/x")), Some(PathBuf::from("/Volumes/nas")));
    assert_eq!(relink::volume_root(Path::new("/Users/x/Bilder")), None);
    assert!(!relink::volume_present(Path::new("/Volumes/graineo-gibt-es-nicht-4711/Fotos")));
    assert!(!relink::locatable(Path::new("/Volumes/graineo-gibt-es-nicht-4711/Fotos")));
    assert!(relink::volume_present(&std::env::temp_dir()));
}
