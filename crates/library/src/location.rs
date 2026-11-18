//! Locations: the volume or share a root folder lives on ("Macintosh HD", the NAS, an external drive).
//! A location is reachable or not; the database already knows "volume missing" (`datei.zustand = 2`).
//!
//! Since 08.11.2026 the library is the main store ([`crate::store`]): ratings, keywords, copies, develop settings and
//! corrected capture times change in the library, whatever the location. If writing sidecars next to the photos is
//! on, `pending_sidecar_write` lists the files whose sidecar is to be written from the library; the background writer
//! takes the reachable ones and keeps the others until their location is back. The library wins – a sidecar changed
//! by another program meanwhile is only taken over through "Begleitdateien einlesen …" ([`crate::sidecar_import::run`]).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::OptionalExtension;

use crate::db::Db;



/// Root folder (id, path with '/' at the end) of a file.
pub fn root_of_file(db: &Db, file: i64) -> Option<(i64, String)> {
    db.conn
        .query_row("SELECT s.id, s.path FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1", [file], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()
        .flatten()
}

/// Is the location of this root folder reachable (the folder exists, and on `/Volumes` the drive is really mounted –
/// not an empty mount point left over on the system disk)?
pub fn root_reachable(path: &str) -> bool {
    root_access(path) == Reach::Yes
}

/// Access to a root folder as the sync sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// the folder is there (and its drive is mounted)
    Yes,
    /// the drive is not connected (or the folder is gone): its files are "volume missing" (2)
    Offline,
    /// the system refuses access (macOS privacy: "Files and Folders" / "Network Volumes" not allowed for graineo).
    /// Nothing is known about the files: their state stays as it is, never "missing" or "volume missing"
    Denied,
}

/// [`Reach`] of a root folder.
pub fn root_access(path: &str) -> Reach {
    let p = Path::new(path);
    reach(std::fs::metadata(p).map(|m| m.is_dir()).map_err(|e| e.kind()), || crate::relink::volume_present(p))
}

/// [`Reach`] from the answer of `stat` on the root folder (is a folder / error kind); `volume`: is its drive mounted.
pub fn reach(stat: Result<bool, std::io::ErrorKind>, volume: impl FnOnce() -> bool) -> Reach {
    match stat {
        Ok(true) if volume() => Reach::Yes,
        Err(std::io::ErrorKind::PermissionDenied) => Reach::Denied,
        _ => Reach::Offline,
    }
}

/// Is the location of this file reachable? Unknown files count as reachable (nothing to queue).
pub fn file_reachable(db: &Db, file: i64) -> bool {
    root_of_file(db, file).is_none_or(|(_, p)| root_reachable(&p))
}

/// Is a sidecar write of this file waiting?
pub fn is_pending(db: &Db, file: i64) -> bool {
    db.conn.query_row("SELECT 1 FROM pending_sidecar_write WHERE file = ?1", [file], |_| Ok(())).optional().ok().flatten().is_some()
}

/// Number of files whose sidecar write is waiting for their location.
pub fn pending_count(conn: &rusqlite::Connection) -> usize {
    conn.query_row("SELECT count(*) FROM pending_sidecar_write", [], |r| r.get::<_, i64>(0)).map(|n| n as usize).unwrap_or(0)
}

/// Waiting sidecar writes per root folder (root id → files).
pub fn pending_per_root(conn: &rusqlite::Connection) -> HashMap<i64, usize> {
    let Ok(mut st) = conn.prepare("SELECT o.root, count(*) FROM pending_sidecar_write p JOIN file d ON d.id = p.file JOIN folder o ON o.id = d.folder GROUP BY o.root") else { return HashMap::new() };
    st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize))).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Kept for the callers that change values of files on unreachable locations: the library holds the change, the
/// sidecar follows through the queue ([`crate::sync::write_sidecar`]).
pub fn note_before_change(_db: &Db, _files: &[i64]) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_is_not_offline() {
        use std::io::ErrorKind;
        assert_eq!(reach(Ok(true), || true), Reach::Yes);
        // empty mount point left on the system disk / drive not connected
        assert_eq!(reach(Ok(true), || false), Reach::Offline);
        assert_eq!(reach(Err(ErrorKind::NotFound), || true), Reach::Offline);
        assert_eq!(reach(Ok(false), || true), Reach::Offline);
        // macOS privacy refuses access (network volumes not allowed for the app): not "not reachable"
        assert_eq!(reach(Err(ErrorKind::PermissionDenied), || true), Reach::Denied);
    }

    use crate::xmp::{self, ReadResult, Values};
    use rusqlite::params;

    struct Setup {
        dir: std::path::PathBuf,
        root: std::path::PathBuf,
        db: Db,
        image: i64,
        file: i64,
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Library with one root folder ("location") and one JPEG.
    fn setup(name: &str) -> Setup {
        let dir = std::env::temp_dir().join(format!("graineo-location-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let root = dir.join("Volume/Fotos");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.jpg"), b"\xFF\xD8camera").unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        crate::reconcile::add_root(&db, &root, &|_| {}).unwrap();
        let (image, file): (i64, i64) = db.conn.query_row("SELECT b.id, d.id FROM image b JOIN file d ON d.id = b.file", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        Setup { dir, root, db, image, file }
    }

    /// "Unplug" the location: rename the root's parent folder away (and back).
    fn unplug(s: &Setup) {
        std::fs::rename(s.dir.join("Volume"), s.dir.join("Volume-away")).unwrap();
    }
    fn plug(s: &Setup) {
        std::fs::rename(s.dir.join("Volume-away"), s.dir.join("Volume")).unwrap();
    }
    fn xmp_of(s: &Setup) -> Values {
        match xmp::read(&s.root.join("a.xmp")) {
            ReadResult::Ok(w) => w,
            other => panic!("XMP not readable: {other:?}"),
        }
    }
    fn rate(s: &Setup, stars: u8) {
        s.db.set_rating(&[s.image], Some(stars), None, None).unwrap();
        s.db.conn.execute("UPDATE image SET rating_time=?2 WHERE id=?1", params![s.image, crate::now()]).unwrap();
        // as `sync::write_sidecar` with writing sidecars on
        crate::store::enqueue(&s.db.conn, s.file).unwrap();
    }

    /// A change while the location is away stays in the library and in the queue; once it is back, the writer writes it.
    #[test]
    fn change_while_unreachable_waits_and_is_written_on_reconnect() {
        let s = setup("queue");
        s.db.conn.execute("UPDATE file SET xmp_read = 1", []).unwrap();
        unplug(&s);
        rate(&s, 4);
        let r = crate::store::write_queued(&s.db, 50, true, &|| {}).unwrap();
        assert_eq!(r.written, 0);
        assert_eq!(pending_count(&s.db.conn), 1, "waits for the location");
        plug(&s);
        let r = crate::store::write_queued(&s.db, 50, true, &|| {}).unwrap();
        assert_eq!(r.written, 1);
        assert_eq!(pending_count(&s.db.conn), 0);
        assert_eq!(xmp_of(&s).stars, 4);
    }

    /// Entries waiting for a folder that is gone (root reachable) do not block the queue: the writer continues after
    /// them (before 0.108 fifty of them at the front stopped every further sidecar).
    #[test]
    fn waiting_entries_do_not_block_the_queue() {
        let s = setup("starve");
        let gone = s.db.create_folder(crate::location::root_of_file(&s.db, s.file).unwrap().0, "weg/", None).unwrap();
        for i in 0..60 {
            s.db.conn.execute("INSERT INTO file(folder, name, format, xmp_read) VALUES(?1, ?2, 'JPG', 1)", params![gone, format!("x{i}.jpg")]).unwrap();
            let d = s.db.conn.last_insert_rowid();
            s.db.conn.execute("INSERT INTO image(file) VALUES(?1)", [d]).unwrap();
            s.db.conn.execute("INSERT INTO pending_sidecar_write(file, since) VALUES(?1, 1)", [d]).unwrap();
        }
        s.db.conn.execute("UPDATE file SET xmp_read = 1", []).unwrap();
        rate(&s, 3);
        let mut after = None;
        let mut rounds = 0;
        loop {
            let r = crate::store::write_queued_after(&s.db, 50, true, &|| {}, after).unwrap();
            if r.last.is_none() {
                break;
            }
            after = r.last;
            rounds += 1;
            assert!(rounds < 10);
        }
        assert_eq!(xmp_of(&s).stars, 3, "the photo behind the waiting ones got its sidecar");
        assert_eq!(pending_count(&s.db.conn), 60, "the waiting ones stay queued");
    }

    /// The library wins: a sidecar changed elsewhere is overwritten in our fields (foreign fields stay).
    #[test]
    fn library_wins_over_a_sidecar_changed_elsewhere() {
        let s = setup("wins");
        s.db.conn.execute("UPDATE file SET xmp_read = 1", []).unwrap();
        let foreign = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" xmp:Rating=\"1\" crs:Exposure2012=\"+0.50\"/></rdf:RDF></x:xmpmeta>";
        std::fs::write(s.root.join("a.xmp"), foreign).unwrap();
        rate(&s, 5);
        crate::store::write_queued(&s.db, 50, true, &|| {}).unwrap();
        assert_eq!(xmp_of(&s).stars, 5);
        assert!(std::fs::read_to_string(s.root.join("a.xmp")).unwrap().contains("crs:Exposure2012=\"+0.50\""), "foreign fields stay");
        let stars: i64 = s.db.conn.query_row("SELECT stars FROM image WHERE id=?1", [s.image], |r| r.get(0)).unwrap();
        assert_eq!(stars, 5);
    }
}
