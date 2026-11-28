//! Exports written into a folder of the library join the library, stacked with their original.
//!
//! The export job calls [`take_in`] right after a file is finished: the file is entered into the library at once
//! (no race with the sync, which then finds it known) and put into the stack of its original – the original stays on
//! top, a new stack starts collapsed. A virtual copy's export is stacked with the original of the copy. An export
//! outside all root folders (or into an excluded folder) is left alone.
//!
//! The link is noted in `export_link` first; when entering fails (library locked), the next sync stacks the file
//! ([`settle`]). Stacks the user changed are never broken up: an export that already sits in a stack stays there.

use crate::db::Db;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

type R<T> = Result<T, String>;

fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Links older than this are dropped by [`settle`] (the file never came into the library).
const KEEP_S: i64 = 7 * 24 * 3600;

/// The original image of `image` (itself, or the original of a virtual copy).
fn original_of(conn: &Connection, image: i64) -> R<Option<i64>> {
    conn.query_row("SELECT o.id FROM image b JOIN image o ON o.file = b.file AND o.copy IS NULL WHERE b.id = ?1", [image], |r| r.get(0)).optional().map_err(f)
}

/// Innermost root folder holding `dir` (not Apple Photos): (root id, folder path relative to it, ending in `/` or empty).
fn place_of(db: &Db, dir: &Path) -> Option<(i64, String)> {
    let p = format!("{}/", crate::nfc(dir.to_string_lossy().trim_end_matches('/')));
    db.roots()
        .ok()?
        .into_iter()
        .filter(|r| r.origin != crate::apple_photos::ORIGIN)
        .filter_map(|r| p.strip_prefix(r.path.as_str()).map(|rel| (r.id, rel.to_string())))
        .min_by_key(|(_, rel)| rel.len())
}

/// The export `exported` of photo `image` was written: enter it into the library and stack it with its original, when
/// it lies in a folder of the library. Returns the image of the export in the library (`None`: outside the library or
/// not a photo format). Never touches the file itself.
pub fn take_in(db: &Db, exported: &Path, image: i64) -> R<Option<i64>> {
    let path = crate::nfc(&exported.to_string_lossy());
    let (Some(dir), Some(name)) = (exported.parent(), exported.file_name().map(|n| crate::nfc(&n.to_string_lossy()))) else { return Ok(None) };
    let Some(format) = crate::format_from_extension(&name) else { return Ok(None) };
    let Some((root, rel)) = place_of(db, dir) else { return Ok(None) };
    if crate::manage::is_excluded(&crate::manage::exclusions(&db.conn, root), &format!("{rel}{name}")) || (!rel.is_empty() && crate::manage::is_excluded(&crate::manage::exclusions(&db.conn, root), &rel)) {
        return Ok(None);
    }
    if original_of(&db.conn, image)?.is_none() {
        return Ok(None);
    }
    db.conn.execute("INSERT OR REPLACE INTO export_link(path, image, created) VALUES(?1, ?2, ?3)", params![path, image, crate::now()]).map_err(f)?;
    let folder = db.create_folder(root, &rel, None).map_err(f)?;
    let meta = std::fs::metadata(exported).map_err(|e| e.to_string())?;
    let size = meta.len() as i64;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    let tx = db.begin_write()?;
    let known: Option<i64> = tx.query_row("SELECT id FROM file WHERE folder = ?1 AND name = ?2", params![folder, name], |r| r.get(0)).optional().map_err(f)?;
    let file = match known {
        Some(id) => id,
        None => {
            tx.execute(
                &format!("INSERT INTO file(id, folder, name, format, size, mtime, status) VALUES({}, ?1, ?2, ?3, ?4, ?5, 0)", crate::merge::NEW_FILE_ID),
                params![folder, name, format, size, mtime],
            )
            .map_err(f)?;
            let id = tx.last_insert_rowid();
            crate::preview::remove_all_developed(id);
            // only a file this export created is marked: a known file stays what it was
            mark_exported(&tx, id, image)?;
            id
        }
    };
    let exp: i64 = match tx.query_row("SELECT id FROM image WHERE file = ?1 AND copy IS NULL", [file], |r| r.get(0)).optional().map_err(f)? {
        Some(i) => i,
        None => {
            tx.execute(&format!("INSERT INTO image(id, file) VALUES({}, ?1)", crate::merge::NEW_IMAGE_ID), [file]).map_err(f)?;
            tx.last_insert_rowid()
        }
    };
    stack_in(&tx, &path, image, exp)?;
    tx.commit().map_err(f)?;
    Ok(Some(exp))
}

/// May the export of photo `image` replace the existing file `target` ("Overwrite")? Only a file graineo itself
/// wrote as an export of the same original (marked in `exported_file`). Every other file inside a folder of the
/// library is an original (import, sync, Lightroom, camera JPEG of a RAW+JPEG pair, a file the sync has not seen yet)
/// and is never replaced, whatever stack it is in: the export gets a free name instead. A target outside every
/// folder of the library (an export folder chosen by the user): yes. Library not available: no.
pub fn may_replace(db: Option<&Db>, target: &Path, image: i64) -> bool {
    let Some(db) = db else { return false };
    let Some(dir) = target.parent() else { return false };
    if place_of(db, dir).is_none() {
        return true;
    }
    let Some(t) = db.image_to_path(target) else { return false };
    let marked: Option<i64> = db
        .conn
        .query_row("SELECT e.image FROM exported_file e JOIN image b ON b.file = e.file WHERE b.id = ?1", [t], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    let Some(marked) = marked else { return false };
    match (original_of(&db.conn, marked), original_of(&db.conn, image)) {
        (Ok(Some(a)), Ok(Some(b))) => a == b && a != t,
        _ => false,
    }
}

/// Note `file` as written by graineo as an export of `image` (inside a transaction).
fn mark_exported(tx: &Connection, file: i64, image: i64) -> R<()> {
    tx.execute("INSERT OR IGNORE INTO exported_file(file, image, created) VALUES(?1, ?2, ?3)", params![file, image, crate::now()]).map_err(f)?;
    Ok(())
}

/// Put export `exp` (of photo `image`) into the stack of the original and drop its link (inside a transaction).
fn stack_in(tx: &Connection, path: &str, image: i64, exp: i64) -> R<()> {
    tx.execute("DELETE FROM export_link WHERE path = ?1", [path]).map_err(f)?;
    let Some(orig) = original_of(tx, image)? else { return Ok(()) };
    // already in a stack (the user put it somewhere, or an earlier run did): left as it is
    if orig == exp || crate::series::stack_of(tx, exp)?.is_some() {
        return Ok(());
    }
    match crate::series::stack_of(tx, orig)? {
        // the stack of the original: the export goes in last, the cover stays; the stack becomes the user's (the
        // background grouping leaves it alone)
        Some(stack) => {
            tx.execute("UPDATE stack SET origin = 'user' WHERE id = ?1 AND origin IN ('auto', 'lightroom')", [stack]).map_err(f)?;
            tx.execute(
                "INSERT INTO stack_image(image, stack, position, pinned) VALUES(?1, ?2, (SELECT coalesce(max(position), 0) + 1 FROM stack_image WHERE stack = ?2), 1)",
                params![exp, stack],
            )
            .map_err(f)?;
        }
        // a new stack (collapsed), the original on top
        None => {
            crate::series::create_in(tx, &[orig, exp], orig)?;
        }
    }
    Ok(())
}

/// After a sync: exports the sync entered (their link was noted, entering them right away failed) join the stack of
/// their original. Links of files that never came into the library are dropped after a week.
pub fn settle(db: &Db) -> R<usize> {
    let rows: Vec<(String, i64, i64)> = {
        let mut st = db.conn.prepare("SELECT path, image, created FROM export_link").map_err(f)?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(f)?.collect::<Result<_, _>>().map_err(f)?;
        v
    };
    if rows.is_empty() {
        return Ok(0);
    }
    let mut n = 0;
    let tx = db.begin_write()?;
    for (path, image, created) in rows {
        match db.image_to_path(Path::new(&path)) {
            Some(exp) => {
                // entered by the sync after the export noted it: the file is the export
                let file: Option<i64> = tx.query_row("SELECT file FROM image WHERE id = ?1", [exp], |r| r.get(0)).optional().map_err(f)?;
                if let Some(file) = file {
                    mark_exported(&tx, file, image)?;
                }
                stack_in(&tx, &path, image, exp)?;
                n += 1;
            }
            None if crate::now() - created > KEEP_S => {
                tx.execute("DELETE FROM export_link WHERE path = ?1", [&path]).map_err(f)?;
            }
            None => {}
        }
    }
    tx.commit().map_err(f)?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lib {
        dir: std::path::PathBuf,
        db: Db,
        photos: std::path::PathBuf,
        orig: i64,
    }

    fn lib(name: &str) -> Lib {
        let dir = std::env::temp_dir().join(format!("graineo-export-link-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let photos = dir.join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("A.JPG"), b"original").unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        crate::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
        let orig = db.image_to_path(&photos.join("A.JPG")).unwrap();
        Lib { dir, db, photos, orig }
    }

    fn count(db: &Db) -> i64 {
        db.conn.query_row("SELECT count(*) FROM image", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn export_into_library_is_stacked_once() {
        let l = lib("in");
        // beside the original and into another folder of the library
        for (i, out) in [l.photos.join("A-edit.jpg"), l.photos.join("export/A.jpg")].iter().enumerate() {
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            std::fs::write(out, b"export").unwrap();
            let exp = take_in(&l.db, out, l.orig).unwrap().unwrap();
            let stack = crate::series::stack_of(&l.db.conn, l.orig).unwrap().unwrap();
            let s = crate::series::get(&l.db.conn, stack).unwrap().unwrap();
            assert_eq!(s.images.len(), i + 2, "the export joins the existing stack");
            assert_eq!(s.top(), l.orig, "the original stays on top");
            assert!(s.collapsed);
            assert_eq!(crate::series::stack_of(&l.db.conn, exp).unwrap(), Some(stack));
        }
        // the sync finds them known: no second entry
        let before = count(&l.db);
        crate::reconcile::reconcile(&l.db, None, &|_| {}).unwrap();
        settle(&l.db).unwrap();
        assert_eq!(count(&l.db), before);
        let links: i64 = l.db.conn.query_row("SELECT count(*) FROM export_link", [], |r| r.get(0)).unwrap();
        assert_eq!(links, 0);
        let _ = std::fs::remove_dir_all(&l.dir);
    }

    #[test]
    fn virtual_copy_stacks_with_its_original_and_sync_settles() {
        let l = lib("copy");
        let copy = crate::sync::create_copy(&l.db, l.orig, None).unwrap();
        let out = l.photos.join("A-copy.jpg");
        std::fs::write(&out, b"export").unwrap();
        // entering failed (only the link is there): the sync enters the file and stacks it (`settle` at its end)
        l.db.conn.execute("INSERT INTO export_link(path, image, created) VALUES(?1, ?2, ?3)", params![crate::nfc(&out.to_string_lossy()), copy, crate::now()]).unwrap();
        crate::reconcile::reconcile(&l.db, None, &|_| {}).unwrap();
        assert_eq!(settle(&l.db).unwrap(), 0, "nothing left for later");
        let exp = l.db.image_to_path(&out).unwrap();
        let stack = crate::series::stack_of(&l.db.conn, l.orig).unwrap().unwrap();
        assert_eq!(crate::series::stack_of(&l.db.conn, exp).unwrap(), Some(stack));
        assert_eq!(crate::series::get(&l.db.conn, stack).unwrap().unwrap().top(), l.orig);
        let _ = std::fs::remove_dir_all(&l.dir);
    }

    #[test]
    fn export_outside_library_does_nothing() {
        let l = lib("out");
        let other = l.dir.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        let out = other.join("A.jpg");
        std::fs::write(&out, b"export").unwrap();
        let before = count(&l.db);
        assert_eq!(take_in(&l.db, &out, l.orig).unwrap(), None);
        assert_eq!(count(&l.db), before);
        assert_eq!(crate::series::stack_of(&l.db.conn, l.orig).unwrap(), None);
        let links: i64 = l.db.conn.query_row("SELECT count(*) FROM export_link", [], |r| r.get(0)).unwrap();
        assert_eq!(links, 0);
        let _ = std::fs::remove_dir_all(&l.dir);
    }

    /// "Overwrite" next to the original: only a file graineo wrote as an export of the same photo is replaced; never
    /// the original, the camera JPEG of a RAW+JPEG pair in the same stack, another photo, a file the sync has not seen
    /// yet, or the export of another photo.
    #[test]
    fn overwrite_never_replaces_an_original() {
        let l = lib("replace");
        // RAW + camera JPEG of the same name, stacked together (as a pair or by the user)
        std::fs::write(l.photos.join("A.CR2"), b"raw").unwrap();
        std::fs::write(l.photos.join("B.JPG"), b"other original").unwrap();
        crate::reconcile::reconcile(&l.db, None, &|_| {}).unwrap();
        let raw = l.db.image_to_path(&l.photos.join("A.CR2")).unwrap();
        let b = l.db.image_to_path(&l.photos.join("B.JPG")).unwrap();
        crate::series::create(&l.db, &[raw, l.orig], raw).unwrap();
        // exporting the RAW as "A.JPG" next to it: the camera JPEG is in its stack, still never replaced
        assert!(!may_replace(Some(&l.db), &l.photos.join("A.JPG"), raw), "camera JPEG in the same stack");
        assert!(!may_replace(Some(&l.db), &l.photos.join("A.CR2"), raw), "its own original");
        assert!(!may_replace(Some(&l.db), &l.photos.join("B.JPG"), raw), "another original");
        assert!(!may_replace(Some(&l.db), &l.photos.join("new.jpg"), raw), "in a library folder, not seen by the sync yet");
        // an export graineo wrote: replaceable by the next export of the same photo (also of its virtual copy)
        let out = l.photos.join("A-export.jpg");
        std::fs::write(&out, b"export").unwrap();
        take_in(&l.db, &out, raw).unwrap();
        assert!(may_replace(Some(&l.db), &out, raw), "earlier export of the same photo");
        let copy = crate::sync::create_copy(&l.db, raw, None).unwrap();
        assert!(may_replace(Some(&l.db), &out, copy), "export of a virtual copy of the same photo");
        assert!(!may_replace(Some(&l.db), &out, b), "the export of another photo");
        // a file entered by the sync after the export noted it counts as export too
        let late = l.photos.join("A-late.jpg");
        std::fs::write(&late, b"export").unwrap();
        l.db.conn.execute("INSERT INTO export_link(path, image, created) VALUES(?1, ?2, ?3)", params![crate::nfc(&late.to_string_lossy()), raw, crate::now()]).unwrap();
        crate::reconcile::reconcile(&l.db, None, &|_| {}).unwrap();
        assert!(may_replace(Some(&l.db), &late, raw));
        // outside every library folder: the user's export folder, replacing is allowed; no library: never
        let other = l.dir.join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        assert!(may_replace(Some(&l.db), &other.join("A.jpg"), raw));
        assert!(!may_replace(None, &other.join("A.jpg"), raw));
        let _ = std::fs::remove_dir_all(&l.dir);
    }

    #[test]
    fn user_placed_export_stays_where_it_is() {
        let l = lib("user");
        let out = l.photos.join("B.jpg");
        std::fs::write(&out, b"export").unwrap();
        std::fs::write(l.photos.join("C.JPG"), b"other").unwrap();
        crate::reconcile::reconcile(&l.db, None, &|_| {}).unwrap();
        let b = l.db.image_to_path(&out).unwrap();
        let c = l.db.image_to_path(&l.photos.join("C.JPG")).unwrap();
        let (_, mine) = crate::series::create(&l.db, &[c, b], c).unwrap();
        take_in(&l.db, &out, l.orig).unwrap();
        assert_eq!(crate::series::stack_of(&l.db.conn, b).unwrap(), Some(mine));
        assert_eq!(crate::series::stack_of(&l.db.conn, l.orig).unwrap(), None);
        let _ = std::fs::remove_dir_all(&l.dir);
    }
}
