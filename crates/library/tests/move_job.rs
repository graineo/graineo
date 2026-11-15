//! "Move to …" as a job (folder by folder, resumable) and merging into an existing folder: merge with duplicates
//! (same / other content), cancel + resume, crash in the middle (copy and switch), minimum free
//! space, stop, same volume, no duplicates after the sync. Everything in own temporary folders; `GRAINEO_DATA_DIR`
//! points there. Throughput on an own disk image: `GRAINEO_TEST_MOVE_TARGET=/Volumes/<image> cargo test --release -p
//! library --test move_job throughput -- --nocapture --ignored`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use library::db::Db;
use library::move_job::{self, Job, Pause, Phase, Stop};
use library::preview::{self, Cache};
use library::reconcile;

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

fn jpeg(p: &Path, w: u32, shade: u8) {
    let img = image::RgbImage::from_fn(w, w * 3 / 4, |x, y| image::Rgb([shade, (x % 256) as u8, (y % 256) as u8]));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    img.save_with_format(p, image::ImageFormat::Jpeg).unwrap();
}

/// Mac: root `Pictures` with 2019 (A, B, C, notiz.txt, Tag/D) and 2020/E. NAS: root `Fotos` with 2019
/// (B identical, C other content, notiz.txt same size other content, X only there).
fn env(copy: bool) -> Env {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("graineo-job-{}-{n}", std::process::id()));
    let (mac, nas, trash) = (dir.join("mac/Pictures"), dir.join("nas/Fotos"), dir.join("trash"));
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
    let y = mac.join("2019");
    jpeg(&y.join("A.jpg"), 640, 10);
    jpeg(&y.join("B.jpg"), 640, 60);
    jpeg(&y.join("C.jpg"), 640, 110);
    std::fs::write(y.join("A.jpg.xmp"), b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>").unwrap();
    std::fs::write(y.join("notiz.txt"), b"AAAA").unwrap();
    jpeg(&y.join("Tag/D.jpg"), 640, 200);
    jpeg(&mac.join("2020/E.jpg"), 640, 230);
    let ny = nas.join("2019");
    std::fs::create_dir_all(&ny).unwrap();
    std::fs::copy(y.join("B.jpg"), ny.join("B.jpg")).unwrap();
    jpeg(&ny.join("C.jpg"), 320, 30);
    std::fs::write(ny.join("notiz.txt"), b"BBBB").unwrap();
    jpeg(&ny.join("X.jpg"), 320, 90);
    let db = Db::open(&dir.join("data/library.sqlite")).unwrap();
    reconcile::add_root(&db, &mac, &|_| {}).unwrap();
    reconcile::add_root(&db, &nas, &|_| {}).unwrap();
    let cache = Cache::open(&dir.join("data/previews"));
    Env { dir, mac, nas, trash, db, cache, _g: g }
}

impl Env {
    fn folder(&self, root: &Path, rel: &str) -> i64 {
        self.db.conn.query_row("SELECT o.id FROM folder o JOIN root_folder s ON s.id=o.root WHERE s.path=?1 AND o.path=?2", [format!("{}/", root.display()), rel.to_string()], |r| r.get(0)).unwrap()
    }
    /// (name, path) of all rows
    fn rows(&self) -> Vec<(String, String)> {
        let mut st = self.db.conn.prepare("SELECT d.name, s.path||o.path||d.name FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root ORDER BY 2").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
    }
    fn count(&self, name: &str) -> i64 {
        self.db.conn.query_row("SELECT count(*) FROM file WHERE name=?1", [name], |r| r.get(0)).unwrap()
    }
    fn dispose(&self) -> impl Fn(&[PathBuf], &Path) -> Result<(), String> + '_ {
        move |list: &[PathBuf], _root: &Path| {
            for p in list {
                let n = std::fs::read_dir(&self.trash).map(|r| r.count()).unwrap_or(0);
                std::fs::rename(p, self.trash.join(format!("{n}-{}", p.file_name().unwrap().to_string_lossy()))).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
    fn job(&self, offline: bool) -> Job {
        let mut plan = move_job::plan(&self.db, self.folder(&self.mac, ""), &self.nas, true, Some(&self.cache)).unwrap();
        plan.offline = offline; // the test target lies on the startup volume; like a NAS here
        let j = Job::new(&plan);
        j.save().unwrap();
        j
    }
    fn run(&self, job: &mut Job, cancel: &AtomicBool, progress: &(dyn Fn(&move_job::Progress) + Sync)) -> Stop {
        let dispose = self.dispose();
        let space = move_job::space_of(&self.cache);
        move_job::run(&self.db, job, &self.cache, cancel, progress, &dispose, &space).unwrap()
    }
}

fn none(_: &move_job::Progress) {}

#[test]
fn merge_into_existing_folders_with_duplicates() {
    let e = env(true);
    // rating and keyword on the Mac copy of B (more recent than the NAS row)
    let b_mac: i64 = e.db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id=b.file JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE d.name='B.jpg' AND s.name='Pictures'", [], |r| r.get(0)).unwrap();
    e.db.conn.execute("UPDATE image SET stars=4, rating_time=?2 WHERE id=?1", [b_mac, library::now()]).unwrap();
    e.db.conn.execute("INSERT INTO keyword(id, name) VALUES(77, 'Strand')", []).unwrap();
    e.db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, 77)", [b_mac]).unwrap();
    let a_id: i64 = e.db.conn.query_row("SELECT id FROM file WHERE name='A.jpg'", [], |r| r.get(0)).unwrap();

    // a Lightroom catalog folder and an Apple Photos library inside the folder never move
    std::fs::create_dir_all(e.mac.join("Lightroom/Katalog Previews.lrdata/0")).unwrap();
    std::fs::write(e.mac.join("Lightroom/Katalog.lrcat"), b"SQLite").unwrap();
    std::fs::write(e.mac.join("Lightroom/Katalog Previews.lrdata/0/x.lrprev"), b"p").unwrap();
    std::fs::create_dir_all(e.mac.join("Fotos-Mediathek.photoslibrary/originals")).unwrap();
    std::fs::write(e.mac.join("Fotos-Mediathek.photoslibrary/originals/IMG_1.HEIC"), b"h").unwrap();
    std::fs::write(e.mac.join("Alt.lrcat-wal"), b"w").unwrap();
    let plan = move_job::plan(&e.db, e.folder(&e.mac, ""), &e.nas, true, Some(&e.cache)).unwrap();
    assert!(plan.merge && plan.contents && !plan.same_volume);
    assert_eq!(plan.figures.kept_folders, vec!["Fotos-Mediathek.photoslibrary".to_string(), "Lightroom".to_string()]);
    assert_eq!(plan.figures.kept_files, 1);
    let fig = &plan.figures;
    eprintln!("{fig:?}");
    assert_eq!(fig.copy_files, 4, "A, A.xmp, D, E");
    assert_eq!(fig.same_files, 2, "B and notiz.txt (same size; its other content shows only in the checksum while running)");
    assert_eq!(fig.conflict_files, 1, "C (other size)");
    assert_eq!(fig.merge_rows, 1, "B has a row on the NAS");
    // the same-size text file is only known as a conflict after the checksum: in the plan it counts as same
    let fig_same_txt = fig.conflicts.iter().any(|c| c.contains("notiz"));
    assert!(!fig_same_txt);
    let mut job = Job::new(&{
        let mut p = plan.clone();
        p.offline = true;
        p
    });
    job.save().unwrap();
    assert!(move_job::touches(&format!("{}/", e.nas.display())) && move_job::active());
    let stop = e.run(&mut job, &AtomicBool::new(false), &none);
    assert_eq!(stop, Stop::Finished, "{}", job.message);
    let r = &job.report;
    eprintln!("{r:?}");
    assert!(!move_job::active());

    let n2019 = e.nas.join("2019");
    for f in ["A.jpg", "A.jpg.xmp", "B.jpg", "Tag/D.jpg"] {
        assert!(n2019.join(f).is_file(), "{f}");
    }
    assert!(e.nas.join("2020/E.jpg").is_file());
    // conflicts stay, untouched on both sides
    assert!(e.mac.join("2019/C.jpg").is_file() && e.mac.join("2019/notiz.txt").is_file());
    assert_eq!(std::fs::read(n2019.join("notiz.txt")).unwrap(), b"BBBB");
    assert_eq!(r.conflict_files, 2, "{:?}", r.conflicts);
    // sources of moved and present files in the trash: A, A.xmp, B, D, E
    assert_eq!(std::fs::read_dir(&e.trash).unwrap().count(), 5);
    assert!(!e.mac.join("2020").exists(), "empty folder removed");
    assert!(e.mac.join("Lightroom/Katalog.lrcat").is_file() && e.mac.join("Lightroom/Katalog Previews.lrdata/0/x.lrprev").is_file());
    assert!(e.mac.join("Fotos-Mediathek.photoslibrary/originals/IMG_1.HEIC").is_file() && e.mac.join("Alt.lrcat-wal").is_file());
    assert!(!e.nas.join("Lightroom").exists() && !e.nas.join("Fotos-Mediathek.photoslibrary").exists());
    assert_eq!(r.files_present, 1);
    assert_eq!(r.files_copied, 4);
    assert_eq!(r.rows_merged, 1);
    assert_eq!(r.photos_moved, 4, "A, B (merged), D, E");

    // library: A keeps its ID at the new place; B only once (NAS row, with the Mac's rating and keyword)
    let a_path: String = e.db.conn.query_row("SELECT s.path||o.path||d.name FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE d.id=?1", [a_id], |r| r.get(0)).unwrap();
    assert_eq!(a_path, format!("{}/2019/A.jpg", e.nas.display()));
    assert_eq!(e.count("B.jpg"), 1);
    let (stars, kw): (i64, i64) = e.db.conn.query_row("SELECT b.stars, (SELECT count(*) FROM image_keyword k WHERE k.image=b.id) FROM image b JOIN file d ON d.id=b.file WHERE d.name='B.jpg'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((stars, kw), (4, 1));
    assert_eq!(e.count("C.jpg"), 2, "both C stay");
    // previews under the new keys (small + offline copy)
    let (small, large) = (preview::small(), preview::large());
    let mut st = e.db.conn.prepare("SELECT s.path||o.path||d.name, d.size, d.mtime, d.orientation FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE s.name='Fotos' AND d.name IN ('A.jpg','B.jpg','D.jpg','E.jpg')").unwrap();
    let keys: Vec<u64> = st.query_map([], |r| Ok(preview::cache_key_with_orientation(preview::cache_key(&r.get::<_, String>(0)?, r.get(1)?, r.get(2)?), r.get(3)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(keys.len(), 4);
    for k in keys {
        assert!(e.cache.has(k, small) && e.cache.has(k, large));
    }
    // sync of both roots: no duplicates, nothing new
    let before = e.rows();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(e.rows(), before);
    assert_eq!(e.count("A.jpg"), 1);
    assert_eq!(move_job::jobs()[0].status, "finished");
}

#[test]
fn cancel_and_resume_copies_nothing_twice() {
    let e = env(true);
    let mut job = e.job(true);
    let cancel = AtomicBool::new(false);
    let progress = |p: &move_job::Progress| {
        if p.phase == Phase::Copy && p.files >= 2 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    assert_eq!(e.run(&mut job, &cancel, &progress), Stop::Paused(Pause::User));
    assert_eq!(move_job::jobs()[0].status, "paused");
    // paused: the sync leaves both roots alone (the copies at the target are not new photos)
    let before = e.rows();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(e.rows(), before, "nothing synced while paused");
    let copied: Vec<(PathBuf, u64)> = walk(&e.nas).into_iter().filter(|p| !["B.jpg", "C.jpg", "X.jpg", "notiz.txt"].contains(&p.file_name().unwrap().to_str().unwrap())).map(|p| (p.clone(), ino(&p))).collect();
    assert!(!copied.is_empty());
    // resume (e.g. after a restart: from the journal)
    let mut job = move_job::load_open().unwrap();
    assert_eq!(e.run(&mut job, &AtomicBool::new(false), &none), Stop::Finished, "{}", job.message);
    for (p, i) in copied {
        assert_eq!(ino(&p), i, "not copied again: {}", p.display());
    }
    assert_eq!(job.report.files_copied + job.report.files_present, 5);
    assert_eq!(e.count("B.jpg"), 1);
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(e.count("A.jpg") + e.count("D.jpg") + e.count("E.jpg"), 3);
}

#[test]
fn crash_during_copy_and_during_switch_is_resumed() {
    let e = env(true);
    let mut job = e.job(true);
    let crash = |p: &move_job::Progress| {
        if p.phase == Phase::Copy && p.files >= 2 {
            panic!("Stromausfall");
        }
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e.run(&mut job, &AtomicBool::new(false), &crash)));
    assert!(r.is_err());
    assert!(!move_job::running());
    assert_eq!(move_job::jobs()[0].status, "running", "the journal says running");
    let mut job = move_job::load_open().unwrap();
    assert_eq!(job.status, "paused");
    // second crash: right after the library switch of a directory (before trash)
    let n = AtomicUsize::new(0);
    let crash2 = |p: &move_job::Progress| {
        if p.phase == Phase::Trash && n.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("Stromausfall 2");
        }
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| e.run(&mut job, &AtomicBool::new(false), &crash2)));
    assert!(r.is_err());
    let mut job = move_job::load_open().unwrap();
    assert_eq!(e.run(&mut job, &AtomicBool::new(false), &none), Stop::Finished, "{}", job.message);
    for f in ["2019/A.jpg", "2019/A.jpg.xmp", "2019/B.jpg", "2019/Tag/D.jpg", "2020/E.jpg"] {
        assert!(e.nas.join(f).is_file(), "{f}");
    }
    assert_eq!(std::fs::read_dir(&e.trash).unwrap().count(), 5, "every source once into the trash");
    assert_eq!(e.count("B.jpg"), 1);
    let before = e.rows();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(e.rows(), before, "no duplicates after the sync");
    for (n, p) in &before {
        if ["A.jpg", "D.jpg", "E.jpg"].contains(&n.as_str()) {
            assert!(p.starts_with(&e.nas.display().to_string()), "{p}");
        }
    }
}

#[test]
fn pauses_at_the_minimum_free_space() {
    let e = env(true);
    let mut job = e.job(true);
    let low = AtomicBool::new(false);
    let space = || if low.load(Ordering::SeqCst) { (5 << 30, 20 << 30) } else { (100 << 30, 20 << 30) };
    let dispose = e.dispose();
    let progress = |p: &move_job::Progress| {
        // after the first directory the Mac runs full
        if p.dir >= 1 {
            low.store(true, Ordering::SeqCst);
        }
    };
    let stop = move_job::run(&e.db, &mut job, &e.cache, &AtomicBool::new(false), &progress, &dispose, &space).unwrap();
    assert!(matches!(stop, Stop::Paused(Pause::Space(_))), "{stop:?}");
    assert!(job.message.contains("Platz"), "{}", job.message);
    assert!(job.dirs_done() >= 1 && job.dirs_done() < job.dirs.len());
    // the trash emptied: continue
    low.store(false, Ordering::SeqCst);
    let stop = move_job::run(&e.db, &mut job, &e.cache, &AtomicBool::new(false), &none, &dispose, &|| (100 << 30, 20 << 30)).unwrap();
    assert_eq!(stop, Stop::Finished, "{}", job.message);
    assert!(e.nas.join("2020/E.jpg").is_file());
}

#[test]
fn stop_removes_own_copies_of_the_folder_in_progress() {
    let e = env(true);
    let mut job = e.job(false);
    let cancel = AtomicBool::new(false);
    // stop in the second directory ("2019/"), after its first copy
    let progress = |p: &move_job::Progress| {
        if p.phase == Phase::Copy && p.rel == "2019/" && p.files >= 1 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    assert_eq!(e.run(&mut job, &cancel, &progress), Stop::Paused(Pause::User));
    let dispose = e.dispose();
    move_job::stop(&e.db, &mut job, &e.cache, &dispose).unwrap();
    assert_eq!(job.status, "stopped");
    assert!(!move_job::active());
    // in 2019 only what was there before
    let mut names: Vec<String> = std::fs::read_dir(e.nas.join("2019")).unwrap().map(|x| x.unwrap().file_name().to_string_lossy().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["B.jpg", "C.jpg", "X.jpg", "notiz.txt"]);
    assert!(e.mac.join("2019/A.jpg").is_file());
    let before = e.rows();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(e.rows(), before, "no duplicates after stop and sync");
}

#[test]
fn same_volume_merge_renames() {
    let e = env(false);
    let mut job = e.job(false);
    assert!(job.same_volume);
    assert_eq!(e.run(&mut job, &AtomicBool::new(false), &none), Stop::Finished, "{}", job.message);
    assert!(e.nas.join("2019/A.jpg").is_file() && e.nas.join("2020/E.jpg").is_file());
    // only the duplicate B goes to the trash (the others were renamed)
    assert_eq!(std::fs::read_dir(&e.trash).unwrap().count(), 1);
    assert_eq!(e.count("B.jpg"), 1);
    let before = e.rows();
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    assert_eq!(e.rows(), before);
}

#[test]
fn into_new_subfolder_and_refusals() {
    let e = env(true);
    // "2020" into an empty place: a new folder there, not a merge
    let other = e.dir.join("extern");
    std::fs::create_dir_all(&other).unwrap();
    let plan = move_job::plan(&e.db, e.folder(&e.mac, "2020/"), &other, false, Some(&e.cache)).unwrap();
    assert!(!plan.merge);
    assert_eq!(plan.target, other.join("2020"));
    let mut job = Job::new(&plan);
    job.save().unwrap();
    assert_eq!(e.run(&mut job, &AtomicBool::new(false), &none), Stop::Finished, "{}", job.message);
    assert!(other.join("2020/E.jpg").is_file());
    let n: i64 = e.db.conn.query_row("SELECT count(*) FROM root_folder WHERE path=?1", [format!("{}/2020/", other.display())], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "new root folder at the target");
    // into itself
    assert!(move_job::plan(&e.db, e.folder(&e.mac, "2019/"), &e.mac.join("2019/Tag"), false, Some(&e.cache)).is_err());
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(d).unwrap().flatten() {
        if e.file_type().unwrap().is_dir() {
            v.extend(walk(&e.path()));
        } else {
            v.push(e.path());
        }
    }
    v
}

fn ino(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).unwrap().ino()
}

/// Throughput on another drive (an own disk image): many small folders with JPEG files.
#[test]
#[ignore]
fn throughput() {
    let Some(target) = std::env::var_os("GRAINEO_TEST_MOVE_TARGET").map(PathBuf::from) else { return };
    let e = env(false);
    // real JPEGs with noise (≈ 2 MB each, like camera JPEGs), so that the previews have work too
    let (dirs, per) = (20usize, 30usize);
    let src = e.mac.join("Massen");
    let mut seed: u32 = 12345;
    for d in 0..dirs {
        for i in 0..per {
            let p = src.join(format!("{d:03}/IMG_{i:04}.jpg"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let img = image::RgbImage::from_fn(2000, 1500, |x, y| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                image::Rgb([((x + d as u32) % 256) as u8, ((y + i as u32) % 256) as u8, (seed >> 24) as u8])
            });
            img.save_with_format(&p, image::ImageFormat::Jpeg).unwrap();
        }
    }
    jpeg(&src.join("titel.jpg"), 640, 5); // a folder of the library needs a photo of its own
    reconcile::reconcile(&e.db, None, &|_| {}).unwrap();
    let dest = target.join(format!("graineo-durchsatz-{}", std::process::id()));
    std::fs::create_dir_all(&dest).unwrap();
    let mut plan = move_job::plan(&e.db, e.folder(&e.mac, "Massen/"), &dest, false, Some(&e.cache)).unwrap();
    if std::env::var_os("GRAINEO_TEST_NO_OFFLINE").is_some() {
        plan.offline = false;
    }
    eprintln!("same volume: {}, offline: {}, files {}, {:.1} MB", plan.same_volume, plan.offline, plan.figures.files, plan.figures.bytes as f64 / 1e6);
    let mut job = Job::new(&plan);
    job.save().unwrap();
    let t = std::time::Instant::now();
    assert_eq!(e.run(&mut job, &AtomicBool::new(false), &none), Stop::Finished, "{}", job.message);
    let s = t.elapsed().as_secs_f64();
    eprintln!("{} files, {:.1} MB in {s:.1} s: {:.0} files/s, {:.0} MB/s, previews made {}", job.report.files_copied, job.report.bytes_copied as f64 / 1e6, job.report.files_copied as f64 / s, job.report.bytes_copied as f64 / 1e6 / s, job.report.previews_made);
    let _ = std::fs::remove_dir_all(&dest);
}
