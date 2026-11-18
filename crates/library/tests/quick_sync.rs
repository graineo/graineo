//! Quick sync: unchanged folders are not checked file by file, but new, missing and renamed files are always found.

use library::db::Db;
use library::reconcile::{self, Depth, Options};

fn run(db: &Db, depth: Depth) -> reconcile::Report {
    let counted = std::cell::Cell::new((0usize, 0usize));
    let r = reconcile::run(db, None, &Options { depth, write_now: Some(false), progress: &|_| {}, count: &|d, t| counted.set((d, t)), pause: &|| {} }).unwrap();
    let (d, t) = counted.get();
    assert!(t > 0 && d == t, "progress ends at the total ({d}/{t})");
    r
}

/// Fixed folder time in the past (a time close to now is not trusted); `n` makes it differ from run to run.
fn age(dir: &std::path::Path, n: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + n);
    std::fs::File::open(dir).unwrap().set_modified(t).unwrap();
}

fn size_in_db(db: &Db, name: &str) -> i64 {
    db.conn.query_row("SELECT size FROM file WHERE name=?1", [name], |r| r.get(0)).unwrap()
}

fn status(db: &Db, name: &str) -> i64 {
    db.conn.query_row("SELECT status FROM file WHERE name=?1", [name], |r| r.get(0)).unwrap()
}

#[test]
fn quick_skips_unchanged_folders_but_finds_new_and_missing() {
    let d = std::env::temp_dir().join(format!("graineo-schnell-abgleich-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let f = d.join("Fotos");
    let sub = f.join("2024");
    std::fs::create_dir_all(&sub).unwrap();
    for n in ["a.jpg", "b.jpg"] {
        std::fs::write(sub.join(n), b"\xFF\xD8x").unwrap();
    }
    std::fs::write(f.join("c.jpg"), b"\xFF\xD8x").unwrap();
    let db = Db::open(&d.join("library.sqlite")).unwrap();
    reconcile::add_root(&db, &f, &|_| {}).unwrap();
    age(&sub, 0);
    age(&f, 0);
    // first run records the folder states
    let full = run(&db, Depth::Full);
    assert_eq!((full.existing, full.files_stat, full.folders_listed), (3, 3, 2));
    assert!(!reconcile::full_due(&db), "a full sync of all roots counts as the daily one");

    // nothing changed: no file is stat-ed
    let q = run(&db, Depth::Quick);
    assert_eq!((q.existing, q.files_stat, q.folders_unchanged, q.missing, q.newly_added), (3, 0, 2, 0, 0));

    // file overwritten in place (folder unchanged): the quick sync does not notice, the full one does
    std::fs::write(sub.join("a.jpg"), b"\xFF\xD8xxxxxx").unwrap();
    age(&sub, 0);
    run(&db, Depth::Quick);
    assert_eq!(size_in_db(&db, "a.jpg"), 3);
    run(&db, Depth::Full);
    assert_eq!(size_in_db(&db, "a.jpg"), 8);

    // new file: found, and only the changed folder is checked file by file
    std::fs::write(sub.join("n.jpg"), b"\xFF\xD8x").unwrap();
    age(&sub, 0);
    let q = run(&db, Depth::Quick);
    assert_eq!((q.newly_added, q.folders_unchanged, q.files_stat), (1, 1, 3));

    // removed file: missing, even if the folder time were the same (names are always compared)
    std::fs::rename(sub.join("b.jpg"), d.join("b.jpg")).unwrap();
    age(&sub, 0);
    let q = run(&db, Depth::Quick);
    assert_eq!(q.missing, 1);
    assert_eq!(status(&db, "b.jpg"), 1);
    // back again: present (missing files are always checked)
    std::fs::rename(d.join("b.jpg"), sub.join("b.jpg")).unwrap();
    age(&sub, 0);
    let q = run(&db, Depth::Quick);
    assert_eq!(q.missing, 0);
    assert_eq!(status(&db, "b.jpg"), 0);
    // replaced via a temporary file (same number of entries): the folder time changes, the file is checked
    std::fs::write(sub.join("tmp"), b"\xFF\xD8xxxxxxxxx").unwrap();
    std::fs::rename(sub.join("tmp"), sub.join("n.jpg")).unwrap();
    age(&sub, 1);
    run(&db, Depth::Quick);
    assert_eq!(size_in_db(&db, "n.jpg"), 11);

    // folder time not trustworthy (just changed): checked file by file
    std::fs::write(f.join("t.txt"), b"x").unwrap();
    let q = run(&db, Depth::Quick);
    assert_eq!(q.folders_unchanged, 1, "only 2024/ is skipped");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn unchanged_needs_same_time_and_count() {
    use reconcile::folder_unchanged as same;
    assert!(same(Some(&(Some(5), 3)), Some(5), 3));
    assert!(!same(Some(&(Some(5), 3)), Some(5), 4), "an entry more or less");
    assert!(!same(Some(&(Some(5), 3)), Some(6), 3), "time changed");
    assert!(!same(Some(&(None, 3)), Some(5), 3), "time was not trustworthy");
    assert!(!same(Some(&(Some(5), 3)), None, 3), "time is not trustworthy now");
    assert!(!same(None, Some(5), 3), "folder new");
}

/// A folder that cannot be listed this time (network hiccup, rights) does not make its photos "missing": what is
/// below it is unknown, not gone (before 0.108 every file in it was marked missing).
#[cfg(unix)]
#[test]
fn unreadable_folder_marks_nothing_missing() {
    use std::os::unix::fs::PermissionsExt;
    let d = std::env::temp_dir().join(format!("graineo-unlesbar-abgleich-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let f = d.join("Fotos");
    let sub = f.join("2024");
    std::fs::create_dir_all(sub.join("tief")).unwrap();
    std::fs::write(sub.join("a.jpg"), b"\xFF\xD8x").unwrap();
    std::fs::write(sub.join("tief/b.jpg"), b"\xFF\xD8x").unwrap();
    let db = Db::open(&d.join("library.sqlite")).unwrap();
    reconcile::add_root(&db, &f, &|_| {}).unwrap();
    run(&db, Depth::Full);
    assert_eq!((status(&db, "a.jpg"), status(&db, "b.jpg")), (0, 0));
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = run(&db, Depth::Full);
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r.missing, 0);
    assert_eq!((status(&db, "a.jpg"), status(&db, "b.jpg")), (0, 0));
    // really gone: missing
    std::fs::remove_file(sub.join("a.jpg")).unwrap();
    run(&db, Depth::Full);
    assert_eq!(status(&db, "a.jpg"), 1);
    drop(db);
    let _ = std::fs::remove_dir_all(&d);
}

/// A root folder that was not reachable (network drive not mounted) and is back: its files are present again, and the
/// report says that states changed – the app reloads its views only then (a sync that only turned "not reachable"
/// into "present" left the Develop module at "Original nicht erreichbar" until a restart).
#[test]
fn root_back_online_is_reported_as_change() {
    let d = std::env::temp_dir().join(format!("graineo-offline-back-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let f = d.join("NAS");
    std::fs::create_dir_all(&f).unwrap();
    for n in ["a.x3f", "b.jpg"] {
        std::fs::write(f.join(n), b"\xFF\xD8x").unwrap();
    }
    let db = Db::open(&d.join("library.sqlite")).unwrap();
    reconcile::add_root(&db, &f, &|_| {}).unwrap();
    age(&f, 0);
    let first = run(&db, Depth::Full);
    assert_eq!(first.status_changed, 0);
    // drive gone: all files "not reachable" (2), reported as a change
    std::fs::rename(&f, d.join("NAS-weg")).unwrap();
    let q = run(&db, Depth::Quick);
    assert_eq!(q.offline.len(), 1);
    assert_eq!((status(&db, "a.x3f"), status(&db, "b.jpg")), (2, 2));
    assert_eq!(q.status_changed, 2);
    // still gone: nothing changes
    assert_eq!(run(&db, Depth::Quick).status_changed, 0);
    // drive back (folder unchanged since the last listing): present again, reported as a change
    std::fs::rename(d.join("NAS-weg"), &f).unwrap();
    age(&f, 0);
    let q = run(&db, Depth::Quick);
    assert!(q.offline.is_empty());
    assert_eq!((status(&db, "a.x3f"), status(&db, "b.jpg")), (0, 0));
    assert_eq!((q.status_changed, q.missing, q.newly_added), (2, 0, 0));
    assert_eq!(q.json()["status_changed"], 2);
    // next quick run: nothing left to report
    assert_eq!(run(&db, Depth::Quick).status_changed, 0);
    let _ = std::fs::remove_dir_all(&d);
}

/// The system refuses access (macOS privacy: network volumes not allowed for the app; here: Unix permissions, same
/// `PermissionDenied`): never "missing", never "volume missing", reported as denied; a folder that cannot be listed
/// but whose files can be read brings "volume missing" files back.
#[cfg(unix)]
#[test]
fn permission_denied_keeps_state_and_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    let d = std::env::temp_dir().join(format!("graineo-denied-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let outer = d.join("share");
    let f = outer.join("NAS");
    std::fs::create_dir_all(&f).unwrap();
    std::fs::write(f.join("a.x3f"), b"\xFF\xD8x").unwrap();
    let db = Db::open(&d.join("library.sqlite")).unwrap();
    reconcile::add_root(&db, &f, &|_| {}).unwrap();
    run(&db, Depth::Full);
    assert_eq!(status(&db, "a.x3f"), 0);
    // root not even stat-able: denied, the state stays (present)
    mode(&outer, 0o000);
    let q = run(&db, Depth::Quick);
    mode(&outer, 0o755);
    assert!(q.offline.is_empty() && q.missing == 0, "{:?}", q.json());
    assert_eq!(q.denied.len(), 1);
    assert_eq!(status(&db, "a.x3f"), 0);
    // drive gone → 2
    std::fs::rename(&f, outer.join("weg")).unwrap();
    run(&db, Depth::Quick);
    assert_eq!(status(&db, "a.x3f"), 2);
    // back, but the root cannot be listed (files can be read): present again, folder reported as denied
    std::fs::rename(outer.join("weg"), &f).unwrap();
    mode(&f, 0o100);
    let q = run(&db, Depth::Quick);
    mode(&f, 0o755);
    assert_eq!((q.missing, q.status_changed, q.denied.len()), (0, 1, 1), "{:?}", q.json());
    assert_eq!(status(&db, "a.x3f"), 0);
    let _ = std::fs::remove_dir_all(&d);
}
