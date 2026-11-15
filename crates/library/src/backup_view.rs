//! "Sicherung ansehen": a backup opened only for looking (Einstellungen → Sicherung). graineo starts once more with
//! `GRAINEO_BACKUP_VIEW=<backup>`; that process works on an unpacked copy (`<data>/backup-view/library.sqlite`) and
//! never opens the real library – except for [`take_over`], which writes the edit of one photo from the backup into the
//! real library in one short transaction (undo: [`undo_take_over`]).
//!
//! - The copy is a throwaway: what is changed in it is discarded when going back ([`cleanup`]); after a crash during the
//!   view the next normal start removes it. The real library is untouched in every case.
//! - In the view, everything that touches photos on the drive or the backups is refused ([`refuse`]): moving, renaming,
//!   deleting, importing, the trash, undo of moves, backups. Sync, faces and sidecars are off (the view process runs
//!   with `GRAINEO_LIBRARY_NO_SYNC` and `GRAINEO_FACE_OFF`, and [`crate::store::write_sidecars`] is off).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

/// Environment switch of the view process: path of the backup being viewed.
pub const ENV: &str = "GRAINEO_BACKUP_VIEW";
/// Environment switch of a restarted process: wait until this process (the one that restarted it) has ended.
pub const ENV_WAIT: &str = "GRAINEO_WAIT_PID";
/// Switches the view added to the environment (removed again when going back).
const ENV_ADDED: &str = "GRAINEO_VIEW_ADDED";

/// The backup being viewed in this process.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    /// the backup file (packed or not)
    pub backup: PathBuf,
    /// its date "YYYY-MM-DD"
    pub date: String,
    /// the unpacked copy this process works on
    pub copy: PathBuf,
}

/// Folder of the copy: `backup-view` in the data folder.
pub fn folder() -> PathBuf {
    crate::data_dir().join("backup-view")
}

fn copy_path() -> PathBuf {
    folder().join("library.sqlite")
}

/// The view of this process (`None`: normal library). Read once; only when the copy exists.
pub fn active() -> Option<&'static View> {
    static V: OnceLock<Option<View>> = OnceLock::new();
    V.get_or_init(|| {
        let backup = PathBuf::from(std::env::var_os(ENV)?);
        let copy = copy_path();
        copy.exists().then(|| View { date: date_of(&backup), backup, copy })
    })
    .as_ref()
}

/// Error for everything the view does not allow (moving, deleting, importing … ), `Ok` outside the view.
pub fn refuse() -> Result<(), String> {
    match active() {
        Some(v) => Err(format!("Nicht möglich, während du die Sicherung vom {} ansiehst – erst zurück zur Bibliothek", v.date)),
        None => Ok(()),
    }
}

fn date_of(backup: &Path) -> String {
    let n = backup.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    n.trim_start_matches("library-").trim_end_matches(".zst").trim_end_matches(".sqlite").to_string()
}

/// Prepare the view of `backup`: unpack (or copy) it into [`folder`] and check it. Returns where the copy lies; the
/// app then starts again with [`ENV`] set ([`restart_env`]).
pub fn prepare(backup: &Path) -> Result<PathBuf, String> {
    prepare_in(backup, &folder())
}

fn prepare_in(backup: &Path, dir: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let part = dir.join("library.sqlite.part");
    let r = (|| {
        crate::backup::materialize(backup, &part)?;
        crate::backup::check_backup_file(&part)?;
        let copy = dir.join("library.sqlite");
        std::fs::rename(&part, &copy).map_err(|e| e.to_string())?;
        crate::files::write_atomic(&dir.join("view.json"), json!({"backup": backup.to_string_lossy(), "date": date_of(backup)}).to_string().as_bytes()).map_err(|e| e.to_string())?;
        Ok(copy)
    })();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(dir);
    }
    r
}

/// Remove the copy (going back, or at a normal start after a crash during the view). Only the own folder.
pub fn cleanup() {
    if active().is_none() {
        cleanup_in(&folder());
    }
}

fn cleanup_in(dir: &Path) {
    if dir.file_name().is_some_and(|n| n == "backup-view") {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Environment for starting graineo again: `Some(backup)` into the view, `None` back to the library. The new process
/// waits until this one has ended ([`wait_for_parent`]).
pub fn restart_env(backup: Option<&Path>) -> Vec<(String, Option<String>)> {
    let mut v = vec![(ENV_WAIT.to_string(), Some(std::process::id().to_string()))];
    match backup {
        Some(b) => {
            v.push((ENV.into(), Some(b.to_string_lossy().into_owned())));
            // sync and faces off; remember which switches the view added (others were set by whoever started graineo)
            let added: Vec<&str> = ["GRAINEO_LIBRARY_NO_SYNC", "GRAINEO_FACE_OFF"].into_iter().filter(|k| std::env::var_os(k).is_none()).collect();
            for k in &added {
                v.push((k.to_string(), Some("1".into())));
            }
            v.push((ENV_ADDED.into(), Some(added.join(","))));
        }
        None => {
            v.push((ENV.into(), None));
            for k in std::env::var(ENV_ADDED).unwrap_or_default().split(',').filter(|k| !k.is_empty()) {
                v.push((k.to_string(), None));
            }
            v.push((ENV_ADDED.into(), None));
        }
    }
    v
}

/// At startup of a restarted process: wait (at most 15 s) until the previous process has ended, so both never work at
/// the same time.
pub fn wait_for_parent() {
    let Some(pid) = std::env::var("GRAINEO_WAIT_PID").ok().and_then(|p| p.parse::<i32>().ok()) else { return };
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < 15 {
        #[cfg(unix)]
        {
            // signal 0: only checks whether the process still exists
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            if t.elapsed().as_millis() > 1500 {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

// ─────────────────────────────────────────────── take over the edit of one photo

/// File id of `photo` in a library (by path), `None`: not there.
fn file_id(c: &Connection, photo: &Path) -> Option<i64> {
    let dir = crate::nfc(&format!("{}/", photo.parent()?.to_string_lossy().trim_end_matches('/')));
    let name = crate::nfc(&photo.file_name()?.to_string_lossy());
    c.query_row(
        "SELECT d.id FROM root_folder s
         JOIN folder o ON o.root = s.id AND o.path = substr(?1, length(s.path) + 1)
         JOIN file d ON d.folder = o.id AND d.name = ?2
         WHERE substr(?1, 1, length(s.path)) = s.path LIMIT 1",
        params![dir, name],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Is the photo (still) in the real library?
pub fn in_library(real: &Path, photo: &Path) -> bool {
    open_real(real).ok().and_then(|c| file_id(&c, photo)).is_some()
}

/// One row of `develop_state` (all columns but the keys), `None`: no row.
type DevelopRow = Option<(Option<String>, Option<i64>, Option<String>, Option<String>, i64, Option<String>)>;

fn develop_row(c: &Connection, file: i64, copy: &str) -> rusqlite::Result<DevelopRow> {
    c.query_row("SELECT settings, settings_time, lr_develop, lr_orientation, lr_edited, lr_file FROM develop_state WHERE file=?1 AND copy=?2", params![file, copy], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
    })
    .optional()
}

fn put_develop_row(c: &Connection, file: i64, copy: &str, row: &DevelopRow) -> rusqlite::Result<()> {
    match row {
        None => c.execute("DELETE FROM develop_state WHERE file=?1 AND copy=?2", params![file, copy]).map(|_| ()),
        Some((s, st, d, o, e, f)) => c
            .execute(
                "INSERT INTO develop_state(file, copy, settings, settings_time, lr_develop, lr_orientation, lr_edited, lr_file) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(file, copy) DO UPDATE SET settings=excluded.settings, settings_time=excluded.settings_time, lr_develop=excluded.lr_develop,
                    lr_orientation=excluded.lr_orientation, lr_edited=excluded.lr_edited, lr_file=excluded.lr_file",
                params![file, copy, s, st, d, o, e, f],
            )
            .map(|_| ()),
    }
}

/// Stars, flag, label and the keyword paths ("Eltern/Kind") of the image (`kopie IS copy`), without person keywords.
type Meta = Option<(i64, i64, i64, Vec<String>)>;

fn keyword_path(c: &Connection, mut id: i64) -> rusqlite::Result<Vec<String>> {
    let mut p = Vec::new();
    for _ in 0..64 {
        let (name, parent): (String, Option<i64>) = c.query_row("SELECT name, parent FROM keyword WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        p.push(name);
        match parent {
            Some(e) => id = e,
            None => break,
        }
    }
    p.reverse();
    Ok(p)
}

fn image_id(c: &Connection, file: i64, copy: Option<&str>) -> rusqlite::Result<Option<i64>> {
    c.query_row("SELECT id FROM image WHERE file=?1 AND copy IS ?2", params![file, copy], |r| r.get(0)).optional()
}

fn meta(c: &Connection, file: i64, copy: Option<&str>) -> rusqlite::Result<Meta> {
    let Some(image) = image_id(c, file, copy)? else { return Ok(None) };
    let (s, m, e): (i64, i64, i64) = c.query_row("SELECT stars, flag, label FROM image WHERE id=?1", [image], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let ids: Vec<i64> = c.prepare("SELECT k.id FROM image_keyword b JOIN keyword k ON k.id=b.keyword WHERE b.image=?1 AND k.person=0")?.query_map([image], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut paths = ids.into_iter().map(|i| keyword_path(c, i).map(|p| p.join("/"))).collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(Some((s, m, e, paths)))
}

/// Keyword "A/B/C" in the real library, created if missing.
fn ensure_keyword(c: &Connection, path: &str) -> rusqlite::Result<i64> {
    let mut parent: Option<i64> = None;
    for name in path.split('/') {
        let found: Option<i64> = c.query_row("SELECT id FROM keyword WHERE name=?1 AND parent IS ?2 AND person=0", params![name, parent], |r| r.get(0)).optional()?;
        parent = Some(match found {
            Some(i) => i,
            None => {
                c.execute("INSERT INTO keyword(name, parent) VALUES(?1, ?2)", params![name, parent])?;
                c.last_insert_rowid()
            }
        });
    }
    Ok(parent.unwrap_or(0))
}

fn put_meta(c: &Connection, image: i64, m: &(i64, i64, i64, Vec<String>)) -> rusqlite::Result<()> {
    c.execute("UPDATE image SET stars=?2, flag=?3, label=?4, changed=?5 WHERE id=?1", params![image, m.0, m.1, m.2, crate::now()])?;
    c.execute("DELETE FROM image_keyword WHERE image=?1 AND keyword IN (SELECT id FROM keyword WHERE person=0)", [image])?;
    for p in &m.3 {
        let k = ensure_keyword(c, p)?;
        c.execute("INSERT OR IGNORE INTO image_keyword(image, keyword) VALUES(?1, ?2)", params![image, k])?;
    }
    Ok(())
}

/// What the real library held before a take-over (to undo it).
#[derive(Clone, Debug)]
pub struct Undo {
    pub photo: PathBuf,
    pub copy: Option<String>,
    file: i64,
    develop: DevelopRow,
    meta: Meta,
}

/// What differs between the backup and the real library for one photo (for the question before taking it over).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Difference {
    /// the photo is in the real library
    pub in_library: bool,
    /// the image (original or this virtual copy) is there
    pub image_in_library: bool,
    pub develop: bool,
    pub rating: bool,
    pub keywords: bool,
}

fn open_real(real: &Path) -> Result<Connection, String> {
    if !real.exists() {
        return Err(format!("Bibliothek nicht gefunden: {}", real.display()));
    }
    let c = Connection::open(real).map_err(|e| e.to_string())?;
    c.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    Ok(c)
}

/// Compare one photo (original or virtual copy) between the backup (`view`) and the real library.
pub fn difference(view: &Path, real: &Path, photo: &Path, copy: Option<&str>) -> Result<Difference, String> {
    let v = Connection::open_with_flags(view, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let r = open_real(real)?;
    let vf = file_id(&v, photo).ok_or("Foto nicht in der Sicherung")?;
    let Some(rf) = file_id(&r, photo) else { return Ok(Difference::default()) };
    let e = |e: rusqlite::Error| e.to_string();
    let (vm, rm) = (meta(&v, vf, copy).map_err(e)?, meta(&r, rf, copy).map_err(e)?);
    Ok(Difference {
        in_library: true,
        image_in_library: rm.is_some(),
        develop: develop_row(&v, vf, copy.unwrap_or("")).map_err(e)? != develop_row(&r, rf, copy.unwrap_or("")).map_err(e)?,
        rating: vm.as_ref().map(|m| (m.0, m.1, m.2)) != rm.as_ref().map(|m| (m.0, m.1, m.2)),
        keywords: vm.as_ref().map(|m| &m.3) != rm.as_ref().map(|m| &m.3),
    })
}

/// Take the edit of one photo (original or virtual copy `copy`) from the backup (`view`, read only) into the real
/// library: develop settings and Lightroom data, with `with_meta` also stars, flag, label and keywords (person keywords
/// stay as they are, they belong to the faces). One short transaction; returns what was there before.
pub fn take_over(view: &Path, real: &Path, photo: &Path, copy: Option<&str>, with_meta: bool) -> Result<Undo, String> {
    let v = Connection::open_with_flags(view, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let mut r = open_real(real)?;
    let e = |e: rusqlite::Error| e.to_string();
    let vf = file_id(&v, photo).ok_or("Foto nicht in der Sicherung")?;
    let rf = file_id(&r, photo).ok_or("Dieses Foto gibt es in der Bibliothek nicht mehr")?;
    let key = copy.unwrap_or("");
    let new_develop = develop_row(&v, vf, key).map_err(e)?;
    let new_meta = if with_meta { meta(&v, vf, copy).map_err(e)? } else { None };
    let t = r.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let undo = Undo { photo: photo.to_path_buf(), copy: copy.map(str::to_string), file: rf, develop: develop_row(&t, rf, key).map_err(e)?, meta: if with_meta { meta(&t, rf, copy).map_err(e)? } else { None } };
    if copy.is_some() && image_id(&t, rf, copy).map_err(e)?.is_none() {
        return Err("Diese virtuelle Kopie gibt es in der Bibliothek nicht mehr".into());
    }
    put_develop_row(&t, rf, key, &new_develop).map_err(e)?;
    if let (Some(m), Some(image)) = (&new_meta, image_id(&t, rf, copy).map_err(e)?) {
        put_meta(&t, image, m).map_err(e)?;
    }
    queue_sidecar(&t, rf);
    t.commit().map_err(e)?;
    Ok(undo)
}

/// The sidecar next to the photo follows the change (only if writing sidecars is switched on; the normal app writes it).
fn queue_sidecar(c: &Connection, file: i64) {
    if crate::settings::value("writeSidecars").as_bool() == Some(true) {
        let _ = c.execute("INSERT OR IGNORE INTO pending_sidecar_write(file, since) VALUES(?1, ?2)", params![file, crate::now()]);
    }
}

/// Undo a [`take_over`]: put back what the real library held before.
pub fn undo_take_over(real: &Path, u: &Undo) -> Result<(), String> {
    let mut r = open_real(real)?;
    let e = |e: rusqlite::Error| e.to_string();
    let t = r.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    put_develop_row(&t, u.file, u.copy.as_deref().unwrap_or(""), &u.develop).map_err(e)?;
    if let (Some(m), Some(image)) = (&u.meta, image_id(&t, u.file, u.copy.as_deref()).map_err(e)?) {
        put_meta(&t, image, m).map_err(e)?;
    }
    queue_sidecar(&t, u.file);
    t.commit().map_err(e)
}

/// Photos of the backup copy that the real library no longer has (by path; for marking them in the view).
pub fn missing_in_library(view: &Path, real: &Path) -> Result<std::collections::HashSet<i64>, String> {
    let v = Connection::open_with_flags(view, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let r = Connection::open_with_flags(real, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let paths = |c: &Connection| -> rusqlite::Result<HashMap<String, i64>> {
        c.prepare("SELECT s.path || o.path || d.name, d.id FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    };
    let e = |e: rusqlite::Error| e.to_string();
    let real_paths = paths(&r).map_err(e)?;
    Ok(paths(&v).map_err(e)?.into_iter().filter(|(p, _)| !real_paths.contains_key(p)).map(|(_, id)| id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn tmp() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!("graineo-ansicht-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A library with two photos: a.jpg (4 stars, keyword "Reise/Rom", develop settings) and b.jpg.
    fn library(d: &Path) -> (PathBuf, i64, i64) {
        let lib = d.join("library.sqlite");
        let db = Db::open(&lib).unwrap();
        let root = db.create_root("/tmp/graineo-ansicht/", "test", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        let mut files = Vec::new();
        for n in ["a.jpg", "b.jpg"] {
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, ?2, 'JPG')", params![folder, n]).unwrap();
            let f = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file, stars) VALUES(?1, 4)", [f]).unwrap();
            files.push(f);
        }
        let image: i64 = db.conn.query_row("SELECT id FROM image WHERE file=?1", [files[0]], |r| r.get(0)).unwrap();
        let k = ensure_keyword(&db.conn, "Reise/Rom").unwrap();
        db.conn.execute("INSERT INTO image_keyword VALUES(?1, ?2)", [image, k]).unwrap();
        crate::store::put_settings(&db.conn, files[0], None, Some(r#"{"exposure":1.5}"#), 100).unwrap();
        drop(db);
        (lib, files[0], files[1])
    }

    fn settings(lib: &Path, file: i64) -> Option<String> {
        Connection::open(lib).unwrap().query_row("SELECT settings FROM develop_state WHERE file=?1 AND copy=''", [file], |r| r.get(0)).optional().unwrap().flatten()
    }

    /// Viewing writes only into the copy: the real library stays byte for byte as it was – also if the view process
    /// ends without cleaning up (crash); the next normal start removes the copy.
    #[test]
    fn view_never_touches_the_real_library() {
        let d = tmp();
        let (lib, a, _) = library(&d);
        let (backup, _) = crate::backup::create_backup(&Db::open(&lib).unwrap()).unwrap();
        let before = std::fs::read(&lib).unwrap();
        let dir = d.join("backup-view");
        let copy = prepare_in(&backup, &dir).unwrap();
        assert!(dir.join("view.json").exists());
        {
            // the view process: works on the copy, changes ratings and develop settings
            let db = Db::open(&copy).unwrap();
            db.conn.execute("UPDATE image SET stars=1", []).unwrap();
            crate::store::put_settings(&db.conn, a, None, Some(r#"{"exposure":-3}"#), 200).unwrap();
            // "crash": no cleanup
        }
        assert_eq!(std::fs::read(&lib).unwrap(), before, "real library untouched");
        assert_eq!(settings(&lib, a).as_deref(), Some(r#"{"exposure":1.5}"#));
        // the next normal start cleans up
        cleanup_in(&dir);
        assert!(!dir.exists());
        // only the own folder is ever removed
        let other = d.join("anderes");
        std::fs::create_dir_all(&other).unwrap();
        cleanup_in(&other);
        assert!(other.exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Take over the edit of one photo from the backup, then undo it; a photo the library no longer has is refused.
    #[test]
    fn take_over_one_edit_and_undo() {
        let d = tmp();
        let (lib, a, b) = library(&d);
        let (backup, _) = crate::backup::create_backup(&Db::open(&lib).unwrap()).unwrap();
        // after the backup: the edit of a.jpg changed, stars and keywords too; b.jpg removed from the library
        {
            let db = Db::open(&lib).unwrap();
            crate::store::put_settings(&db.conn, a, None, Some(r#"{"exposure":0.2}"#), 300).unwrap();
            db.conn.execute("UPDATE image SET stars=1 WHERE file=?1", [a]).unwrap();
            db.conn.execute("DELETE FROM image_keyword", []).unwrap();
            db.conn.execute("DELETE FROM file WHERE id=?1", [b]).unwrap();
        }
        let copy = prepare_in(&backup, &d.join("backup-view")).unwrap();
        let photo = Path::new("/tmp/graineo-ansicht/a.jpg");
        let diff = difference(&copy, &lib, photo, None).unwrap();
        assert!(diff.in_library && diff.develop && diff.rating && diff.keywords, "{diff:?}");
        // only the develop settings
        let u = take_over(&copy, &lib, photo, None, false).unwrap();
        assert_eq!(settings(&lib, a).as_deref(), Some(r#"{"exposure":1.5}"#));
        let c = Connection::open(&lib).unwrap();
        assert_eq!(c.query_row("SELECT stars FROM image WHERE file=?1", [a], |r| r.get::<_, i64>(0)).unwrap(), 1, "stars not taken over");
        undo_take_over(&lib, &u).unwrap();
        assert_eq!(settings(&lib, a).as_deref(), Some(r#"{"exposure":0.2}"#));
        // with stars, label and keywords
        let u = take_over(&copy, &lib, photo, None, true).unwrap();
        assert_eq!(c.query_row("SELECT stars FROM image WHERE file=?1", [a], |r| r.get::<_, i64>(0)).unwrap(), 4);
        assert_eq!(meta(&c, a, None).unwrap().unwrap().3, vec!["Reise/Rom".to_string()]);
        assert!(!difference(&copy, &lib, photo, None).unwrap().develop);
        undo_take_over(&lib, &u).unwrap();
        assert_eq!(c.query_row("SELECT stars FROM image WHERE file=?1", [a], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert!(meta(&c, a, None).unwrap().unwrap().3.is_empty());
        // b.jpg: no longer in the library
        let gone = Path::new("/tmp/graineo-ansicht/b.jpg");
        assert!(!difference(&copy, &lib, gone, None).unwrap().in_library);
        assert!(take_over(&copy, &lib, gone, None, true).unwrap_err().contains("nicht mehr"));
        assert!(!in_library(&lib, gone) && in_library(&lib, photo));
        assert_eq!(missing_in_library(&copy, &lib).unwrap().into_iter().collect::<Vec<_>>(), vec![b]);
        drop(c);
        std::fs::remove_dir_all(d).unwrap();
    }
}
