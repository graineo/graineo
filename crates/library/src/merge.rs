//! Merged photos (HDR from an exposure bracket).
//!
//! The result is a `file` row in the folder of the reference photo **without a file on disk** (status
//! [`STATUS_VIRTUAL`]); its pixels live in the app cache and are recomputed from the sources when the cache is gone.
//! The recipe (sources, settings, algorithm version) is in the table `merge`. The sources stay ordinary photos and are
//! never changed. No sidecar is ever written for a virtual file.
//!
//! Older graineo versions do not know status 3: their sync marks the row missing (status 1/2) – nothing is deleted.
//! [`repair`] (run at the start of every sync) sets it back.

use crate::db::Db;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

type R<T> = Result<T, String>;

fn e(x: rusqlite::Error) -> String {
    x.to_string()
}

/// `file.status` of a merged photo without a file on disk.
pub const STATUS_VIRTUAL: i64 = 3;

/// One source photo of a merge, as it was when the merge was created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub file: i64,
    /// absolute path at creation time (the file id is authoritative; the path is the fallback when it is gone)
    pub path: String,
    pub size: Option<i64>,
    pub mtime: Option<i64>,
}

/// Recipe of a merged photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub file: i64,
    pub kind: String,
    pub sources: Vec<Source>,
    /// JSON of the merge settings (`hdr::Settings`)
    pub settings: String,
    pub algo: u32,
    pub state: String,
    pub created: i64,
}

impl Recipe {
    /// Current paths of the sources (where the library has them now), in source order. A source whose library row is
    /// gone or holds another photo by now ([`Source::row`]) keeps the path it had.
    pub fn source_paths(&self, c: &Connection) -> Vec<PathBuf> {
        self.sources.iter().map(|s| s.row(c).map_or_else(|| PathBuf::from(&s.path), |r| r.0)).collect()
    }

    /// Key of the computed result: changes when a source file, the settings or the algorithm change. It starts with
    /// [`key_prefix`] of the merged file, so every merged photo has cache files of its own: two merges of the same
    /// frames never share (and never remove) each other's result, and the files of one photo can be listed.
    pub fn cache_key(&self, c: &Connection) -> String {
        self.cache_key_for(c, &self.settings)
    }

    /// [`Self::cache_key`] for other settings of the same sources (the merge without its areas, an area hidden for a
    /// comparison).
    pub fn cache_key_for(&self, c: &Connection, settings: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for x in b {
                h ^= *x as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        eat(self.kind.as_bytes());
        eat(pixel_settings(settings).as_bytes());
        eat(&self.algo.to_le_bytes());
        for s in &self.sources {
            // size and modification time as stored now (a changed source changes the key)
            let (size, mtime) = s.row(c).map_or((s.size, s.mtime), |r| (r.1, r.2));
            eat(&s.file.to_le_bytes());
            eat(&size.unwrap_or(-1).to_le_bytes());
            eat(&mtime.unwrap_or(-1).to_le_bytes());
        }
        format!("{}{h:016x}", key_prefix(self.file))
    }
}

/// Start of every cache key of the merged file `file` (see [`Recipe::cache_key`]).
pub fn key_prefix(file: i64) -> String {
    format!("f{file}-")
}

impl Source {
    /// The library row of this source as it is now: path, size, modification time. `None` when the row is gone or
    /// belongs to another photo by now (file ids are given again after the row with the highest id was deleted): the
    /// row counts only while its name is the stored one, or size and modification time both are (a renamed or moved
    /// source keeps those, a source changed in place keeps its name).
    pub fn row(&self, c: &Connection) -> Option<(PathBuf, Option<i64>, Option<i64>)> {
        let (path, name, size, mtime): (String, String, Option<i64>, Option<i64>) = c
            .query_row(
                "SELECT s.path || o.path || d.name, d.name, d.size, d.mtime FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1",
                [self.file],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten()?;
        let stored = Path::new(&self.path).file_name().map(|n| crate::nfc(&n.to_string_lossy()));
        let same_name = stored.is_some_and(|n| n.to_lowercase() == crate::nfc(&name).to_lowercase());
        let same_data = size.is_some() && mtime.is_some() && size == self.size && mtime == self.mtime;
        (same_name || same_data).then(|| (PathBuf::from(path), size, mtime))
    }
}

/// Keys of the merge settings that do not change the pixels (the automatic tone of a linear merge only sets develop
/// sliders): left out of the cache key, so changing them computes nothing again.
const NOT_IN_KEY: &[&str] = &["auto_tone"];

/// The settings JSON as far as the pixels depend on it, always in the same normal form (keys sorted, no white space):
/// the same pixel settings give the same key whether or not a key that changes no pixels stands beside them, and
/// whatever the order they were written in. A text that is no JSON object is taken as it is.
fn pixel_settings(settings: &str) -> std::borrow::Cow<'_, str> {
    match serde_json::from_str::<serde_json::Value>(settings) {
        Ok(serde_json::Value::Object(mut m)) => {
            for k in NOT_IN_KEY {
                m.remove(*k);
            }
            std::borrow::Cow::Owned(serde_json::Value::Object(m).to_string())
        }
        _ => std::borrow::Cow::Borrowed(settings),
    }
}

/// Absolute path of a library file.
pub fn file_path(c: &Connection, file: i64) -> Option<PathBuf> {
    c.query_row(
        "SELECT s.path || o.path || d.name FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE d.id = ?1",
        [file],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .map(PathBuf::from)
}

/// Recipe of a merged file.
pub fn recipe(c: &Connection, file: i64) -> Option<Recipe> {
    c.query_row("SELECT file, kind, sources, settings, algo, state, created FROM merge WHERE file = ?1", [file], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, String>(5)?, r.get::<_, i64>(6)?))
    })
    .optional()
    .ok()
    .flatten()
    .and_then(|(file, kind, sources, settings, algo, state, created)| {
        Some(Recipe { file, kind, sources: serde_json::from_str(&sources).ok()?, settings, algo: algo as u32, state, created })
    })
}

/// Recipe of the merged photo at `path` (thread's read connection); `None` for every ordinary photo.
pub fn recipe_for_path(path: &Path) -> Option<(Recipe, Vec<PathBuf>, String)> {
    crate::store::with_conn(|c| {
        let (file, _) = crate::store::locate_in(c, path)?;
        let r = recipe(c, file)?;
        let paths = r.source_paths(c);
        let key = r.cache_key(c);
        Some((r, paths, key))
    })
}

/// Recipe of a merged file with the current source paths (thread's read connection).
pub fn recipe_of_file(file: i64) -> Option<(Recipe, Vec<PathBuf>)> {
    crate::store::with_conn(|c| {
        let r = recipe(c, file)?;
        let paths = r.source_paths(c);
        Some((r, paths))
    })
}

/// Cache keys of a merged file for the given settings JSONs (thread's read connection), e.g. the settings shown and
/// the merge without its areas.
pub fn keys_for(file: i64, settings: &[&str]) -> Option<Vec<String>> {
    crate::store::with_conn(|c| {
        let r = recipe(c, file)?;
        Some(settings.iter().map(|s| r.cache_key_for(c, s)).collect())
    })
}

/// All merged files of the library with their source paths as they are now (thread's read connection; `None`: no
/// library to read).
pub fn all_virtual() -> Option<Vec<(i64, Vec<PathBuf>)>> {
    crate::store::with_conn(|c| {
        let mut st = c.prepare("SELECT file FROM merge WHERE state = 'virtual' ORDER BY file").ok()?;
        let files: Vec<i64> = st.query_map([], |r| r.get(0)).ok()?.collect::<rusqlite::Result<_>>().ok()?;
        Some(files.into_iter().map(|f| (f, recipe(c, f).map(|r| r.source_paths(c)).unwrap_or_default())).collect())
    })
}

/// Can `path` be the path of a merged photo by its name (`<stem>-HDR.dng`, `<stem>-HDR-<n>.dng`, as [`create`] names
/// them)? Asked before the library is, so opening an ordinary photo never looks into it.
pub fn has_merge_name(path: &Path) -> bool {
    let dng = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng"));
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let numbered = stem.rsplit_once("-HDR-").is_some_and(|(head, n)| !head.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    dng && (stem.strip_suffix("-HDR").is_some_and(|head| !head.is_empty()) || numbered)
}

/// Is `path` the path a merged photo without a file is addressed by (thread's read connection)? Nothing may be
/// written there: a real file at that path would be taken for the merged photo.
pub fn is_virtual_path(path: &Path) -> bool {
    crate::store::with_conn(|c| {
        let (file, _) = crate::store::locate_in(c, path)?;
        is_virtual(c, file).then_some(())
    })
    .is_some()
}

/// Is this file a merged photo without a file on disk?
pub fn is_virtual(c: &Connection, file: i64) -> bool {
    c.query_row("SELECT 1 FROM merge WHERE file = ?1 AND state = 'virtual'", [file], |_| Ok(())).optional().ok().flatten().is_some()
}

/// Name a merged photo is shown with: `<stem>-HDR` without the extension. The merged photo is not a file, but its stored
/// name keeps the `.dng` of the path it is addressed by (type, preview and sidecar lookups go through the path); the
/// extension only returns on export. Names of other photos stay as they are.
pub fn shown_name(name: &str, status: i64) -> String {
    if status == STATUS_VIRTUAL {
        if let Some(stem) = name.strip_suffix(".dng").or_else(|| name.strip_suffix(".DNG")) {
            return stem.to_string();
        }
    }
    name.to_string()
}

/// Number of exposures a merged photo is made of (`None` for every other photo).
pub fn source_count(file: i64) -> Option<usize> {
    crate::store::with_conn(|c| recipe(c, file).map(|r| r.sources.len()))
}

/// Created rows of a new merge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Created {
    pub file: i64,
    pub image: i64,
}

/// Free file name `<stem>-HDR.dng` (`-HDR-2.dng` … when taken) in a folder.
fn free_name(c: &Connection, folder: i64, stem: &str) -> R<String> {
    for n in 1..1000 {
        let name = if n == 1 { format!("{stem}-HDR.dng") } else { format!("{stem}-HDR-{n}.dng") };
        let taken: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM file WHERE folder = ?1 AND name = ?2)", params![folder, crate::nfc(&name)], |r| r.get(0)).map_err(e)?;
        if !taken {
            return Ok(crate::nfc(&name));
        }
    }
    Err("kein freier Name".into())
}

/// `info` key: the highest file id a merged photo ever got.
const LAST_FILE_ID: &str = "merge_last_file_id";
/// `info` key: the highest image id a merged photo ever got.
const LAST_IMAGE_ID: &str = "merge_last_image_id";

/// SQL expression for the id of a new `file` row of an ordinary photo: above every id in use **and** above every id
/// a merged photo ever had. The database alone would give the id of a removed row again (highest id + 1); a merged
/// photo that was removed can come back with ⌘Z, and its cache files and previews hang on its id – so no other
/// photo ever takes it.
pub const NEW_FILE_ID: &str = "(SELECT max(coalesce((SELECT max(id) FROM file), 0), coalesce((SELECT CAST(value AS INTEGER) FROM info WHERE name = 'merge_last_file_id'), 0)) + 1)";
/// [`NEW_FILE_ID`] for a new `image` row.
pub const NEW_IMAGE_ID: &str = "(SELECT max(coalesce((SELECT max(id) FROM image), 0), coalesce((SELECT CAST(value AS INTEGER) FROM info WHERE name = 'merge_last_image_id'), 0)) + 1)";

/// `info` key: merged photos whose first computation has not ended (JSON list of [`Unfinished`]).
const UNFINISHED: &str = "merge_unfinished";

/// A merged photo created in the library whose first result is not computed yet. Left behind when the app ends
/// while it computes (a batch creates all its photos before it computes the first): found at the next start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unfinished {
    pub file: i64,
    /// what creating it changed in the stacks (to put them back when the photo is removed)
    #[serde(default)]
    pub change: Option<crate::series::Change>,
}

/// The merged photos noted as unfinished.
pub fn unfinished(c: &Connection) -> Vec<Unfinished> {
    c.query_row("SELECT value FROM info WHERE name = ?1", [UNFINISHED], |r| r.get::<_, String>(0)).optional().ok().flatten().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()
}

fn set_unfinished(c: &Connection, list: &[Unfinished]) -> R<()> {
    if list.is_empty() {
        c.execute("DELETE FROM info WHERE name = ?1", [UNFINISHED]).map_err(e)?;
    } else {
        c.execute("INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value", params![UNFINISHED, serde_json::to_string(list).map_err(|x| x.to_string())?]).map_err(e)?;
    }
    Ok(())
}

/// Note the merged photo `file` as unfinished (inside the transaction that creates it).
pub fn note_unfinished(tx: &Connection, file: i64, change: Option<&crate::series::Change>) -> R<()> {
    let mut list = unfinished(tx);
    list.retain(|u| u.file != file);
    list.push(Unfinished { file, change: change.cloned() });
    set_unfinished(tx, &list)
}

/// These merged photos are not unfinished any more (their first result is there, or they are gone).
pub fn note_finished(c: &Connection, files: &[i64]) -> R<()> {
    let list = unfinished(c);
    let rest: Vec<Unfinished> = list.iter().filter(|u| !files.contains(&u.file)).cloned().collect();
    if rest.len() != list.len() {
        set_unfinished(c, &rest)?;
    }
    Ok(())
}

/// A file id for a new merged photo: above every id in use and above every id a merged photo ever had (remembered in
/// `info`, also after that photo was removed). Inside the caller's transaction.
fn fresh_file_id(tx: &Connection) -> R<i64> {
    let used: i64 = tx.query_row("SELECT coalesce(max(id), 0) FROM file", [], |r| r.get(0)).map_err(e)?;
    let last: i64 = tx.query_row("SELECT value FROM info WHERE name = ?1", [LAST_FILE_ID], |r| r.get::<_, String>(0)).optional().map_err(e)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let id = used.max(last) + 1;
    tx.execute("INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value", params![LAST_FILE_ID, id.to_string()]).map_err(e)?;
    Ok(id)
}

/// Remember `file` (and `image`) as ids a merged photo had: the highest ones ever are kept in `info`.
fn note_merged_ids(tx: &Connection, file: i64, image: Option<i64>) -> R<()> {
    let sql = "INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value = max(CAST(value AS INTEGER), CAST(excluded.value AS INTEGER))";
    tx.execute(sql, params![LAST_FILE_ID, file.to_string()]).map_err(e)?;
    if let Some(image) = image {
        tx.execute(sql, params![LAST_IMAGE_ID, image.to_string()]).map_err(e)?;
    }
    Ok(())
}

/// Create the virtual result of a merge: a `file` row next to the reference (shooting data of the reference), its
/// original `image` and the recipe. `sources`: library files in source order; `reference`: index into `sources`.
pub fn create(db: &Db, kind: &str, sources: &[i64], reference: usize, settings: &str, algo: u32) -> R<Created> {
    let tx = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let c = create_in(&tx, kind, sources, reference, settings, algo)?;
    tx.commit().map_err(e)?;
    Ok(c)
}

/// [`create`] inside a transaction the caller holds (and commits): the caller puts the photo into its stack and
/// collection and gives it its keywords in the same transaction, so a merged photo is either there with all of it or
/// not at all.
pub fn create_in(tx: &Connection, kind: &str, sources: &[i64], reference: usize, settings: &str, algo: u32) -> R<Created> {
    if sources.len() < 2 || reference >= sources.len() {
        return Err("mindestens zwei Fotos".into());
    }
    let mut src = Vec::with_capacity(sources.len());
    for &f in sources {
        let path = file_path(tx, f).ok_or_else(|| format!("Datei {f} fehlt in der Bibliothek"))?;
        let (size, mtime): (Option<i64>, Option<i64>) = tx.query_row("SELECT size, mtime FROM file WHERE id = ?1", [f], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
        src.push(Source { file: f, path: path.to_string_lossy().to_string(), size, mtime });
    }
    let rf = sources[reference];
    let (folder, name): (i64, String) = tx.query_row("SELECT folder, name FROM file WHERE id = ?1", [rf], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s).to_string();
    let new_name = free_name(tx, folder, &stem)?;
    // shooting data of the reference; exposure time/ISO left empty (a merge has no single exposure, and the series
    // detection must not take the result for one more bracket frame)
    // a file id no merged photo ever had: the database gives the id of a removed row again (highest id + 1), and
    // the cache files, grid previews and what a session remembers of a merged photo hang on that id – a new merged
    // photo must never inherit them from one that was removed
    let file = fresh_file_id(tx)?;
    tx.execute(
        "INSERT INTO file(id, folder, name, format, camera, capture_time, width, height, orientation, status, lens, aperture, focal_length, exif, text_read, xmp_read)
         SELECT ?4, folder, ?2, 'DNG', camera, capture_time, width, height, orientation, ?3, lens, aperture, focal_length, 1, 1, 1 FROM file WHERE id = ?1",
        params![rf, new_name, STATUS_VIRTUAL, file],
    )
    .map_err(e)?;
    // the import run of the reference: the merged photo belongs to the same import ("Zuletzt importiert")
    tx.execute(
        &format!("INSERT INTO image(id, file, changed, run) VALUES({NEW_IMAGE_ID}, ?1, ?2, (SELECT run FROM image WHERE file = ?3 ORDER BY id LIMIT 1))"),
        params![file, crate::now(), rf],
    )
    .map_err(e)?;
    let image = tx.last_insert_rowid();
    // (its image id is never given to another photo either, see [`NEW_IMAGE_ID`])
    tx.execute("INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value = max(CAST(value AS INTEGER), CAST(excluded.value AS INTEGER))", params![LAST_IMAGE_ID, image.to_string()]).map_err(e)?;
    tx.execute(
        "INSERT INTO merge(file, kind, sources, settings, algo, state, created) VALUES(?1, ?2, ?3, ?4, ?5, 'virtual', ?6)",
        params![file, kind, serde_json::to_string(&src).map_err(|x| x.to_string())?, settings, algo as i64, crate::now()],
    )
    .map_err(e)?;
    Ok(Created { file, image })
}

/// Change the settings of a merged photo (ghost level, brush strokes) and return the new cache key: the result is
/// computed again from the sources. Only virtual files.
pub fn set_settings(db: &Db, file: i64, settings: &str) -> R<String> {
    if !is_virtual(&db.conn, file) {
        return Err("kein zusammengefügtes Foto".into());
    }
    db.conn.execute("UPDATE merge SET settings = ?2 WHERE file = ?1", params![file, settings]).map_err(e)?;
    recipe(&db.conn, file).map(|r| r.cache_key(&db.conn)).ok_or_else(|| "Rezept fehlt".to_string())
}

/// Change the sources and settings of a merged photo together (a frame taken in or left out; the indices in the
/// settings belong to the new list). Returns the new cache key. Only virtual files.
pub fn set_sources(db: &Db, file: i64, sources: &[i64], settings: &str) -> R<String> {
    if !is_virtual(&db.conn, file) {
        return Err("kein zusammengefügtes Foto".into());
    }
    if sources.len() < 2 {
        return Err("mindestens zwei Fotos".into());
    }
    let old = recipe(&db.conn, file).ok_or_else(|| "Rezept fehlt".to_string())?;
    let mut src = Vec::with_capacity(sources.len());
    for &f in sources {
        // a source kept keeps its record – while its row still is that photo ([`Source::row`]: a file id may belong
        // to another photo by now); a new one is taken as it is now
        if let Some(s) = old.sources.iter().find(|s| s.file == f && s.row(&db.conn).is_some()) {
            src.push(s.clone());
            continue;
        }
        let path = file_path(&db.conn, f).ok_or_else(|| format!("Datei {f} fehlt in der Bibliothek"))?;
        let (size, mtime): (Option<i64>, Option<i64>) = db.conn.query_row("SELECT size, mtime FROM file WHERE id = ?1", [f], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
        src.push(Source { file: f, path: path.to_string_lossy().to_string(), size, mtime });
    }
    db.conn
        .execute("UPDATE merge SET sources = ?2, settings = ?3 WHERE file = ?1", params![file, serde_json::to_string(&src).map_err(|x| x.to_string())?, settings])
        .map_err(e)?;
    recipe(&db.conn, file).map(|r| r.cache_key(&db.conn)).ok_or_else(|| "Rezept fehlt".to_string())
}

/// Remove a merged photo again (undo of [`create`]): its file row with image, edits and recipe. Only virtual files.
pub fn remove(db: &Db, file: i64) -> R<()> {
    if !is_virtual(&db.conn, file) {
        return Err("kein zusammengefügtes Foto".into());
    }
    let tx = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    // rows that refer to the image without a cascade
    tx.execute("DELETE FROM develop_state WHERE file = ?1", [file]).map_err(e)?;
    tx.execute("DELETE FROM merge WHERE file = ?1", [file]).map_err(e)?;
    tx.execute("DELETE FROM image WHERE file = ?1", [file]).map_err(e)?;
    tx.execute("DELETE FROM file WHERE id = ?1", [file]).map_err(e)?;
    tx.commit().map_err(e)
}

/// Undo of creating merged photos, all or nothing in one transaction: for every item (in the given order) its stack
/// goes back as it was, then the photo goes. The photos are taken into the snapshot, so [`bring_back`] can return a
/// photo that was worked on after it was made.
pub fn undo_create(db: &Db, items: &[(i64, Option<crate::series::Change>)]) -> R<crate::manage::Snapshot> {
    let tx = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let mut snap = crate::manage::Snapshot::default();
    for (file, change) in items {
        if let Some(c) = change {
            crate::series::restore_in(&tx, c)?;
        }
        snap.merge(take_out_in(&tx, &[*file])?);
    }
    tx.commit().map_err(e)?;
    Ok(snap)
}

/// Take merged photos out of the library so that they can be put back ([`bring_back`], ⌘Z): the file rows with
/// everything that hangs on them (image, recipe, develop settings and history, ratings, keywords, collections, stack
/// membership) go into the snapshot. One transaction; only virtual files. The cached results are not touched.
pub fn take_out(db: &Db, files: &[i64]) -> R<crate::manage::Snapshot> {
    let tx = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
    let snap = take_out_in(&tx, files)?;
    tx.commit().map_err(e)?;
    Ok(snap)
}

/// [`take_out`] inside a transaction the caller holds (and commits).
pub fn take_out_in(tx: &Connection, files: &[i64]) -> R<crate::manage::Snapshot> {
    if let Some(f) = files.iter().find(|f| !is_virtual(tx, **f)) {
        return Err(format!("kein zusammengefügtes Foto ({f})"));
    }
    let ids: Vec<rusqlite::types::Value> = files.iter().map(|f| rusqlite::types::Value::Integer(*f)).collect();
    crate::manage::remove_rows_in(tx, "file", "id", &ids, crate::manage::Snapshot::default())
}

/// Put back what [`take_out`] took (rows that exist again stay as they are). Refused when the place of a photo is
/// taken by now – its file id belongs to another photo (ids of removed rows are given again), or its name exists in
/// its folder: putting the recipe back would turn that other photo into the merged one.
pub fn bring_back(db: &Db, snap: &crate::manage::Snapshot) -> R<()> {
    if let Some(t) = snap.tables.iter().find(|t| t.table == "file") {
        let col = |name: &str| t.columns.iter().position(|c| c == name);
        if let (Some(id), Some(folder), Some(name)) = (col("id"), col("folder"), col("name")) {
            for r in &t.rows {
                let taken: bool = db
                    .conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM file d WHERE (d.id = ?1 OR (d.folder = ?2 AND d.name = ?3)) AND NOT EXISTS(SELECT 1 FROM merge m WHERE m.file = d.id AND d.id = ?1))",
                        params![r[id], r[folder], r[name]],
                        |x| x.get(0),
                    )
                    .map_err(e)?;
                if taken {
                    return Err(PLACE_TAKEN.into());
                }
            }
        }
    }
    crate::manage::undo(db, snap)
}

/// Error of [`bring_back`]: another photo has the place of the removed merged photo by now.
pub const PLACE_TAKEN: &str = "Der Platz des HDR ist inzwischen von einem anderen Foto belegt";

/// What a merged photo carries beyond the merge itself (asked before it is removed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Work {
    /// areas taken from single frames (painted by the user)
    pub areas: usize,
    /// own develop settings of the photo and its virtual copies (JSON), newest first
    pub develop: Vec<String>,
    /// virtual copies
    pub copies: usize,
}

/// The work stored for the merged file `file`.
pub fn work(c: &Connection, file: i64) -> Work {
    let count = |v: &serde_json::Value, k: &str| v.get(k).and_then(|a| a.as_array()).map_or(0, |a| a.len());
    let areas = recipe(c, file).and_then(|r| serde_json::from_str::<serde_json::Value>(&r.settings).ok()).map_or(0, |v| count(&v, "regions") + count(&v, "paint"));
    let develop = c
        .prepare("SELECT settings FROM develop_state WHERE file = ?1 AND settings IS NOT NULL ORDER BY coalesce(settings_time, 0) DESC")
        .and_then(|mut st| st.query_map([file], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_default();
    let copies = c.query_row("SELECT count(*) FROM image WHERE file = ?1 AND copy IS NOT NULL", [file], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize;
    Work { areas, develop, copies }
}

/// Merged files that use the library file `file` as a source: its row has to be that source still ([`Source::row`]) –
/// a file id given again to another photo is not the source of an older recipe.
pub fn using_source(c: &Connection, file: i64) -> Vec<i64> {
    let Ok(mut st) = c.prepare_cached("SELECT file, sources FROM merge ORDER BY file") else { return Vec::new() };
    let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) else { return Vec::new() };
    let rows: Vec<(i64, String)> = rows.flatten().collect();
    rows.into_iter().filter(|(_, json)| serde_json::from_str::<Vec<Source>>(json).is_ok_and(|v| v.iter().any(|s| s.file == file && s.row(c).is_some()))).map(|(f, _)| f).collect()
}

/// Number of merged photos that have at least one of `files` as a source.
pub fn count_using(c: &Connection, files: &[i64]) -> usize {
    let want: std::collections::HashSet<i64> = files.iter().copied().collect();
    let Ok(mut st) = c.prepare_cached("SELECT file, sources FROM merge ORDER BY file") else { return 0 };
    let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) else { return 0 };
    let rows: Vec<(i64, String)> = rows.flatten().collect();
    rows.into_iter()
        .filter(|(f, json)| !want.contains(f) && serde_json::from_str::<Vec<Source>>(json).is_ok_and(|v| v.iter().any(|s| want.contains(&s.file) && s.row(c).is_some())))
        .count()
}

/// The library row `old` of a source was merged into the row `new` (the same photo, known twice): every recipe that
/// names `old` names `new` from now on, with its path, size and modification time as stored now. Returns the number
/// of changed recipes. Inside the caller's transaction, before `old` is deleted.
pub fn replace_source(tx: &Connection, old: i64, new: i64) -> R<usize> {
    let mut changed = 0;
    for file in using_source(tx, old) {
        let json: String = tx.query_row("SELECT sources FROM merge WHERE file = ?1", [file], |r| r.get(0)).map_err(e)?;
        let mut src: Vec<Source> = serde_json::from_str(&json).map_err(|x| x.to_string())?;
        let now = file_path(tx, new).map(|p| p.to_string_lossy().to_string());
        let (size, mtime): (Option<i64>, Option<i64>) = tx.query_row("SELECT size, mtime FROM file WHERE id = ?1", [new], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
        for s in src.iter_mut().filter(|s| s.file == old) {
            s.file = new;
            if let Some(p) = &now {
                s.path = p.clone();
            }
            s.size = size;
            s.mtime = mtime;
        }
        tx.execute("UPDATE merge SET sources = ?2 WHERE file = ?1", params![file, serde_json::to_string(&src).map_err(|x| x.to_string())?]).map_err(e)?;
        changed += 1;
    }
    Ok(changed)
}

/// Sources of recipes were moved and keep their rows: the stored record of every source in `files` follows (path,
/// size, modification time as the library has them now), so that the row still counts as that source
/// ([`Source::row`]) when the file got another name at its new place.
pub fn refresh_sources(tx: &Connection, files: &[i64]) -> R<usize> {
    let want: std::collections::HashSet<i64> = files.iter().copied().collect();
    let rows: Vec<(i64, String)> = {
        let mut st = tx.prepare_cached("SELECT file, sources FROM merge ORDER BY file").map_err(e)?;
        let v = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
        v
    };
    let mut changed = 0;
    for (file, json) in rows {
        let Ok(mut src) = serde_json::from_str::<Vec<Source>>(&json) else { continue };
        if !src.iter().any(|s| want.contains(&s.file)) {
            continue;
        }
        for s in src.iter_mut().filter(|s| want.contains(&s.file)) {
            let Some(p) = file_path(tx, s.file) else { continue };
            let (size, mtime): (Option<i64>, Option<i64>) = tx.query_row("SELECT size, mtime FROM file WHERE id = ?1", [s.file], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
            *s = Source { file: s.file, path: p.to_string_lossy().to_string(), size, mtime };
        }
        tx.execute("UPDATE merge SET sources = ?2 WHERE file = ?1", params![file, serde_json::to_string(&src).map_err(|x| x.to_string())?]).map_err(e)?;
        changed += 1;
    }
    Ok(changed)
}

/// Merged photos of the folder row `folder` follow their reference (the middle source when the settings name none)
/// when it lies in another folder now: after the photos of a folder were moved, the merged photo stays next to them
/// instead of in a folder that is gone. Its name is kept, or a free one taken when the name exists there. Returns
/// the number of moved rows. Inside the caller's transaction.
pub fn follow_sources(tx: &Connection, folder: i64) -> R<usize> {
    let files: Vec<(i64, String)> = {
        let mut st = tx.prepare_cached("SELECT d.id, d.name FROM file d JOIN merge m ON m.file = d.id WHERE d.folder = ?1 AND m.state = 'virtual'").map_err(e)?;
        let v = st.query_map([folder], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
        v
    };
    let mut moved = 0;
    for (file, name) in files {
        let Some(r) = recipe(tx, file) else { continue };
        let reference = serde_json::from_str::<serde_json::Value>(&r.settings).ok().and_then(|v| v.get("reference").and_then(|x| x.as_u64())).map_or(r.sources.len() / 2, |i| i as usize);
        let Some(src) = r.sources.get(reference).or_else(|| r.sources.first()) else { continue };
        let Some(target): Option<i64> = tx.query_row("SELECT folder FROM file WHERE id = ?1", [src.file], |r| r.get(0)).optional().map_err(e)? else { continue };
        if target == folder {
            continue;
        }
        let taken: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM file WHERE folder = ?1 AND name = ?2)", params![target, name], |r| r.get(0)).map_err(e)?;
        let new_name = if taken {
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
            free_name(tx, target, stem.strip_suffix("-HDR").unwrap_or(stem))?
        } else {
            name.clone()
        };
        tx.execute("UPDATE file SET folder = ?2, name = ?3 WHERE id = ?1", params![file, target, new_name]).map_err(e)?;
        moved += 1;
    }
    Ok(moved)
}

/// Outcome of [`copy_from`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Copied {
    /// merged photos taken over
    pub copied: usize,
    /// merged photos left behind because a source is not in the new library
    pub lost: usize,
}

fn table_columns(c: &Connection, table: &str) -> R<Vec<String>> {
    let mut st = c.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid").map_err(e)?;
    let v = st.query_map([table], |r| r.get::<_, String>(0)).map_err(e)?.collect::<rusqlite::Result<Vec<_>>>().map_err(e)?;
    Ok(v)
}

/// Copy the rows of `table` in `src` matching `where_sql` (one parameter) into `dst`, with the columns both know
/// except `skip`, and the columns of `set` given these values. Returns the new row ids, in source order.
fn copy_rows(src: &Connection, dst: &Connection, table: &str, where_sql: &str, key: i64, skip: &[&str], set: &[(&str, rusqlite::types::Value)]) -> R<Vec<(i64, i64)>> {
    use rusqlite::types::Value;
    let there = table_columns(dst, table)?;
    let cols: Vec<String> = table_columns(src, table)?.into_iter().filter(|c| there.contains(c) && !skip.contains(&c.as_str()) && !set.iter().any(|(k, _)| k == c)).collect();
    let quoted = |v: &[String]| v.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(",");
    let mut st = src.prepare(&format!("SELECT rowid, {} FROM \"{table}\" WHERE {where_sql} ORDER BY rowid", quoted(&cols))).map_err(e)?;
    let rows: Vec<Vec<Value>> = st.query_map([key], |r| (0..=cols.len()).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>()).map_err(e)?.collect::<rusqlite::Result<_>>().map_err(e)?;
    let mut all = cols.clone();
    all.extend(set.iter().map(|(k, _)| k.to_string()));
    let marks = vec!["?"; all.len()].join(",");
    let mut ins = dst.prepare(&format!("INSERT INTO \"{table}\"({}) VALUES({marks})", quoted(&all))).map_err(e)?;
    let mut out = Vec::new();
    for row in rows {
        let old = match row[0] {
            Value::Integer(i) => i,
            _ => 0,
        };
        let values: Vec<Value> = row.into_iter().skip(1).chain(set.iter().map(|(_, v)| v.clone())).collect();
        ins.execute(rusqlite::params_from_iter(values.iter())).map_err(e)?;
        out.push((old, dst.last_insert_rowid()));
    }
    Ok(out)
}

fn locate(c: &Connection, path: &str) -> Option<i64> {
    c.query_row(
        "SELECT d.id FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE s.path || o.path || d.name = ?1",
        [crate::nfc(path)],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Take the merged photos of a previous library over into a rebuilt one (`db`): a merged photo has no file and no
/// sidecar, so the rebuild cannot find it anywhere else. Per photo its file row, its images (original and virtual
/// copies with rating and caption), its develop settings and the recipe, with the sources named by their rows in the
/// new library (found by path). A photo of which a source is not in the new library is left behind and counted.
/// Not taken over: keywords, collections, stacks and the develop history of the merged photo.
pub fn copy_from(src: &Connection, db: &Db) -> R<Copied> {
    use rusqlite::types::Value;
    let rows: Vec<(i64, String)> = match src.prepare("SELECT file, sources FROM merge WHERE state = 'virtual' ORDER BY file") {
        Ok(mut st) => st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.collect::<rusqlite::Result<_>>().map_err(e)?,
        Err(_) => return Ok(Copied::default()), // older library without the table
    };
    let mut out = Copied::default();
    for (old_file, json) in rows {
        let tx = rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate).map_err(e)?;
        let Ok(sources) = serde_json::from_str::<Vec<Source>>(&json) else {
            out.lost += 1;
            continue;
        };
        // every source by the path the previous library has for it now
        let mut new_sources = Vec::with_capacity(sources.len());
        for s in &sources {
            let path = s.row(src).map_or_else(|| s.path.clone(), |r| r.0.to_string_lossy().to_string());
            let Some(id) = locate(&tx, &path) else { break };
            let (size, mtime): (Option<i64>, Option<i64>) = tx.query_row("SELECT size, mtime FROM file WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
            new_sources.push(Source { file: id, path: crate::nfc(&path), size, mtime });
        }
        // its folder: where it was, otherwise next to its first source
        let own: Option<(String, String)> = file_path(src, old_file).and_then(|p| Some((p.parent()?.to_string_lossy().to_string() + "/", p.file_name()?.to_string_lossy().to_string())));
        let folder: Option<i64> = own
            .as_ref()
            .and_then(|(dir, _)| tx.query_row("SELECT o.id FROM folder o JOIN root_folder s ON s.id = o.root WHERE s.path || o.path = ?1", [crate::nfc(dir)], |r| r.get(0)).optional().ok().flatten())
            .or_else(|| new_sources.first().and_then(|s| tx.query_row("SELECT folder FROM file WHERE id = ?1", [s.file], |r| r.get(0)).optional().ok().flatten()));
        let (Some(folder), Some((_, name)), true) = (folder, own, new_sources.len() == sources.len() && sources.len() >= 2) else {
            out.lost += 1;
            continue;
        };
        let taken: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM file WHERE folder = ?1 AND name = ?2)", params![folder, crate::nfc(&name)], |r| r.get(0)).map_err(e)?;
        if taken {
            // already there (the rebuild ran twice into the same library)
            continue;
        }
        let files = copy_rows(src, &tx, "file", "id = ?1", old_file, &["id", "lr_id"], &[("folder", Value::Integer(folder)), ("status", Value::Integer(STATUS_VIRTUAL))])?;
        let Some(&(_, file)) = files.first() else {
            out.lost += 1;
            continue;
        };
        let images = copy_rows(src, &tx, "image", "file = ?1", old_file, &["id", "lr_id", "lr_state", "run"], &[("file", Value::Integer(file))])?;
        // (these ids belong to a merged photo from now on: never given to another photo, see [`NEW_FILE_ID`])
        note_merged_ids(&tx, file, images.iter().map(|i| i.1).max())?;
        copy_rows(src, &tx, "develop_state", "file = ?1", old_file, &["id"], &[("file", Value::Integer(file))])?;
        copy_rows(src, &tx, "merge", "file = ?1", old_file, &[], &[("file", Value::Integer(file)), ("sources", Value::Text(serde_json::to_string(&new_sources).map_err(|x| x.to_string())?))])?;
        tx.commit().map_err(e)?;
        out.copied += 1;
    }
    Ok(out)
}

/// Merged files made of exactly these source files (in any order), oldest first: asked before a second merge of the
/// same frames. A recipe counts only while the rows of all its sources still are those photos ([`Source::row`]): after
/// a source was deleted its file id may belong to another photo, which is not a frame of that merge.
pub fn made_from(c: &Connection, files: &[i64]) -> Vec<i64> {
    let mut want: Vec<i64> = files.to_vec();
    want.sort_unstable();
    want.dedup();
    let Ok(mut st) = c.prepare_cached("SELECT file, sources FROM merge ORDER BY created, file") else { return Vec::new() };
    let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) else { return Vec::new() };
    let rows: Vec<(i64, String)> = rows.flatten().collect();
    rows.into_iter()
        .filter(|(_, json)| {
            let sources = serde_json::from_str::<Vec<Source>>(json).unwrap_or_default();
            if sources.iter().any(|s| s.row(c).is_none()) {
                return false;
            }
            let mut got: Vec<i64> = sources.into_iter().map(|s| s.file).collect();
            got.sort_unstable();
            got.dedup();
            got == want
        })
        .map(|(file, _)| file)
        .collect()
}

/// Is there a merged photo an older version marked missing ([`repair`] has something to do)?
pub fn needs_repair(c: &Connection) -> bool {
    c.query_row("SELECT EXISTS(SELECT 1 FROM merge m JOIN file d ON d.id = m.file WHERE m.state = 'virtual' AND d.status <> ?1)", [STATUS_VIRTUAL], |r| r.get(0)).unwrap_or(false)
}

/// Set merged photos back to [`STATUS_VIRTUAL`] where an older graineo version marked them missing (status 1/2).
/// Returns the number of repaired rows.
pub fn repair(db: &Db) -> R<usize> {
    let has: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM merge)", [], |r| r.get(0)).unwrap_or(false);
    if !has {
        return Ok(0);
    }
    db.conn
        .execute("UPDATE file SET status = ?1 WHERE status <> ?1 AND id IN (SELECT file FROM merge WHERE state = 'virtual')", [STATUS_VIRTUAL])
        .map_err(e)
}

/// Sources of a recipe that are not reachable now (missing file, drive not connected).
pub fn missing_sources(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| !p.is_file()).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> (Db, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("merge-test-{}-{}", std::process::id(), crate::now() ^ rand_seed()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own');
                 INSERT INTO folder(id, root, path) VALUES(1, 1, 'reihe/');
                 INSERT INTO file(id, folder, name, format, size, mtime, camera, capture_time, time, iso) VALUES
                   (1, 1, 'IMG_1.CR2', 'CR2', 100, 10, 'Canon', '2026-01-01T10:00:00', 0.01, 100),
                   (2, 1, 'IMG_2.CR2', 'CR2', 101, 11, 'Canon', '2026-01-01T10:00:01', 0.04, 100),
                   (3, 1, 'IMG_3.CR2', 'CR2', 102, 12, 'Canon', '2026-01-01T10:00:02', 0.16, 100);
                 INSERT INTO image(id, file) VALUES(11, 1), (12, 2), (13, 3);",
            )
            .unwrap();
        (db, dir)
    }

    fn rand_seed() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as i64
    }

    /// B6 of the third audit: an ordinary photo that comes into the library after a merged photo was removed never
    /// takes its file or image id (the database alone gives the highest id again) – so ⌘Z can bring the merged photo
    /// back, and the new photo does not meet its previews and cache files.
    #[test]
    fn ordinary_photos_never_take_the_ids_of_a_removed_merge() {
        let (db, dir) = library();
        let a = create(&db, "hdr", &[1, 2, 3], 1, "{}", 1).unwrap();
        let snap = take_out(&db, &[a.file]).unwrap();
        // as the sync inserts a new photo
        db.conn.execute(&format!("INSERT INTO file(id, folder, name, format) VALUES({NEW_FILE_ID}, 1, 'IMG_9.CR2', 'CR2')"), []).unwrap();
        let file = db.conn.last_insert_rowid();
        db.conn.execute(&format!("INSERT INTO image(id, file) VALUES({NEW_IMAGE_ID}, ?1)"), [file]).unwrap();
        let image = db.conn.last_insert_rowid();
        assert!(file > a.file && image > a.image, "file {file} (merge {}), image {image} (merge {})", a.file, a.image);
        bring_back(&db, &snap).unwrap();
        assert!(is_virtual(&db.conn, a.file));
        let back: i64 = db.conn.query_row("SELECT id FROM image WHERE file = ?1", [a.file], |r| r.get(0)).unwrap();
        assert_eq!(back, a.image);
        // a library that never had a merged photo: ids as the database gives them
        let (db2, dir2) = library();
        db2.conn.execute(&format!("INSERT INTO file(id, folder, name, format) VALUES({NEW_FILE_ID}, 1, 'IMG_9.CR2', 'CR2')"), []).unwrap();
        assert_eq!(db2.conn.last_insert_rowid(), 4);
        drop((db, db2));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    /// N2/B4: a merged photo is noted as unfinished when it is created and until its first result is there; the note
    /// carries what is needed to put its stack back.
    #[test]
    fn unfinished_merges_are_noted() {
        let (db, dir) = library();
        let tx = db.begin_write().unwrap();
        let a = create_in(&tx, "hdr", &[1, 2], 0, "{}", 2).unwrap();
        let (change, _) = crate::series::create_in(&tx, &[a.image, 11, 12], a.image).unwrap();
        note_unfinished(&tx, a.file, Some(&change)).unwrap();
        let b = create_in(&tx, "hdr", &[2, 3], 0, "{}", 2).unwrap();
        note_unfinished(&tx, b.file, None).unwrap();
        tx.commit().unwrap();
        let list = unfinished(&db.conn);
        assert_eq!(list.iter().map(|u| u.file).collect::<Vec<_>>(), vec![a.file, b.file]);
        assert_eq!(list[0].change.as_ref(), Some(&change), "the change survives being stored");
        note_finished(&db.conn, &[b.file]).unwrap();
        assert_eq!(unfinished(&db.conn).len(), 1);
        // removed as when it is cancelled: the stack goes back
        undo_create(&db, &[(a.file, list[0].change.clone())]).unwrap();
        note_finished(&db.conn, &[a.file]).unwrap();
        assert!(unfinished(&db.conn).is_empty());
        let stacks: i64 = db.conn.query_row("SELECT count(*) FROM stack", [], |r| r.get(0)).unwrap();
        assert_eq!(stacks, 0);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A new merged photo never gets the file id of one that was removed (the database would give the highest id
    /// again): cache files, grid previews and session state hang on the id.
    #[test]
    fn file_ids_of_merged_photos_are_never_given_again() {
        let (db, dir) = library();
        let a = create(&db, "hdr", &[1, 2, 3], 1, "{}", 1).unwrap();
        remove(&db, a.file).unwrap();
        let b = create(&db, "hdr", &[1, 2], 1, "{}", 1).unwrap();
        assert!(b.file > a.file, "{} after {}", b.file, a.file);
        // taken out for ⌘Z and a third one made meanwhile
        let snap = take_out(&db, &[b.file]).unwrap();
        let c = create(&db, "hdr", &[1, 2], 1, "{}", 1).unwrap();
        assert!(c.file > b.file);
        // the name of the one taken out is taken by the new one: it cannot come back over it
        assert_eq!(bring_back(&db, &snap).unwrap_err(), PLACE_TAKEN);
        assert_eq!(recipe(&db.conn, c.file).unwrap().sources.len(), 2, "the new photo keeps its recipe");
        assert!(recipe(&db.conn, b.file).is_none());
        remove(&db, c.file).unwrap();
        bring_back(&db, &snap).unwrap();
        assert!(is_virtual(&db.conn, b.file));
        // an ordinary photo that got the id of a merged photo taken out: the recipe is not put on it
        let snap = take_out(&db, &[b.file]).unwrap();
        db.conn.execute("INSERT INTO file(id, folder, name, format) VALUES(?1, 1, 'IMG_9.CR2', 'CR2')", [b.file]).unwrap();
        assert_eq!(bring_back(&db, &snap).unwrap_err(), PLACE_TAKEN);
        assert!(recipe(&db.conn, b.file).is_none(), "the other photo did not become a merged one");
        let status: i64 = db.conn.query_row("SELECT status FROM file WHERE id = ?1", [b.file], |r| r.get(0)).unwrap();
        assert_ne!(status, STATUS_VIRTUAL);
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn create_recipe_and_remove() {
        let (db, dir) = library();
        let c = create(&db, "hdr", &[1, 2, 3], 1, r#"{"reference":1,"align":true}"#, 1).unwrap();
        let (name, status, folder, time, xmp): (String, i64, i64, Option<f64>, Option<i64>) =
            db.conn.query_row("SELECT name, status, folder, time, xmp_read FROM file WHERE id = ?1", [c.file], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((name.as_str(), status, folder, time, xmp), ("IMG_2-HDR.dng", STATUS_VIRTUAL, 1, None, Some(1)));
        assert!(has_merge_name(Path::new("/fotos/reihe/IMG_2-HDR.dng")) && has_merge_name(Path::new("/x/IMG_2-HDR-12.DNG")));
        for other in ["/x/IMG_2.dng", "/x/IMG_2-HDR.jpg", "/x/-HDR.dng", "/x/IMG_2-HDR-b.dng", "/x/IMG_2-HDRI.dng"] {
            assert!(!has_merge_name(Path::new(other)), "{other}");
        }
        // shown without the extension: a merged photo is no file
        assert_eq!(shown_name(&name, status), "IMG_2-HDR");
        assert_eq!(shown_name("IMG_2.dng", 0), "IMG_2.dng");
        let r = recipe(&db.conn, c.file).unwrap();
        assert_eq!(r.sources.iter().map(|s| s.file).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(r.source_paths(&db.conn)[0], PathBuf::from("/fotos/reihe/IMG_1.CR2"));
        // the key changes with a source and with the settings
        let k = r.cache_key(&db.conn);
        db.conn.execute("UPDATE file SET mtime = 99 WHERE id = 2", []).unwrap();
        assert_ne!(k, r.cache_key(&db.conn));
        // a second merge of the same reference gets its own name
        let c2 = create(&db, "hdr", &[1, 2], 1, "{}", 1).unwrap();
        let n2: String = db.conn.query_row("SELECT name FROM file WHERE id = ?1", [c2.file], |r| r.get(0)).unwrap();
        assert_eq!(n2, "IMG_2-HDR-2.dng");
        // the merges of exactly these frames, in any order (QA 1.5.0-test.5: a second HDR of the same frames unasked)
        assert_eq!(made_from(&db.conn, &[3, 1, 2]), vec![c.file]);
        assert_eq!(made_from(&db.conn, &[2, 1]), vec![c2.file]);
        assert!(made_from(&db.conn, &[1, 3]).is_empty());
        // remove leaves the sources alone
        remove(&db, c.file).unwrap();
        assert!(recipe(&db.conn, c.file).is_none());
        let n: i64 = db.conn.query_row("SELECT count(*) FROM file WHERE id IN (1, 2, 3)", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        assert!(remove(&db, 1).is_err(), "an ordinary photo is never removed this way");
        std::fs::remove_dir_all(dir).ok();
    }

    /// The merged photo belongs to the import of its reference: "Zuletzt importiert" shows it next to its sources.
    #[test]
    fn merged_photo_joins_the_import_of_its_reference() {
        let (db, dir) = library();
        let run = db.new_run("sync", "").unwrap();
        db.mark_run(run, "import").unwrap();
        db.conn.execute("UPDATE image SET run = ?1", [run]).unwrap();
        let c = create(&db, "hdr", &[1, 2, 3], 1, "{}", 1).unwrap();
        let got: Option<i64> = db.conn.query_row("SELECT run FROM image WHERE id = ?1", [c.image], |r| r.get(0)).unwrap();
        assert_eq!(got, Some(run));
        assert_eq!(db.last_import(), Some(run));
        // a reference without a run (Lightroom takeover, viewed folder made library photo): none either
        db.conn.execute("UPDATE image SET run = NULL WHERE file = 2", []).unwrap();
        let c2 = create(&db, "hdr", &[1, 2], 1, "{}", 1).unwrap();
        let got: Option<i64> = db.conn.query_row("SELECT run FROM image WHERE id = ?1", [c2.image], |r| r.get(0)).unwrap();
        assert_eq!(got, None);
        std::fs::remove_dir_all(dir).ok();
    }

    /// What an older graineo version does to the row (its sync marks it missing; its grid shows it) is undone by
    /// [`repair`]; the recipe and the edits survive.
    #[test]
    fn new_settings_give_a_new_key() {
        let (db, dir) = library();
        let c = create(&db, "hdr", &[1, 2, 3], 1, r#"{"reference":1}"#, 2).unwrap();
        let k1 = recipe(&db.conn, c.file).unwrap().cache_key(&db.conn);
        let k2 = set_settings(&db, c.file, r#"{"reference":1,"paint":[{"frame":0,"radius":0.03,"points":[[0.5,0.5]]}]}"#).unwrap();
        assert_ne!(k1, k2);
        assert_eq!(recipe(&db.conn, c.file).unwrap().settings.contains("paint"), true);
        // the same settings again: the same key (cached result stays valid)
        assert_eq!(k2, set_settings(&db, c.file, &recipe(&db.conn, c.file).unwrap().settings).unwrap());
        assert!(set_settings(&db, 1, "{}").is_err(), "an ordinary photo has no recipe");
        std::fs::remove_dir_all(dir).ok();
    }

    /// The automatic tone of a linear merge only sets develop sliders: changing it keeps the cache key (nothing is
    /// computed again); a recipe without it keeps the key it always had; the fusion strength is part of the key.
    #[test]
    fn automatic_tone_is_not_part_of_the_key() {
        let (db, dir) = library();
        let old = r#"{"reference":1,"align":true,"ghost":"standard"}"#;
        let c = create(&db, "hdr", &[1, 2, 3], 1, old, 2).unwrap();
        let r = recipe(&db.conn, c.file).unwrap();
        let k_old = r.cache_key(&db.conn);
        assert_eq!(k_old, r.cache_key_for(&db.conn, old));
        let a = r#"{"reference":1,"align":true,"ghost":"standard","auto_tone":{"strength":0.5}}"#;
        let b = r#"{"reference":1,"align":true,"ghost":"standard","auto_tone":{"strength":0.8,"applied":[0.1,-0.3,0.2,0.0,0.0]}}"#;
        assert_eq!(r.cache_key_for(&db.conn, a), r.cache_key_for(&db.conn, b));
        // the automatic tone coming into a recipe for the first time keeps the key (the result stays in the cache)
        assert_eq!(k_old, r.cache_key_for(&db.conn, a), "without and with the automatic tone");
        // the order of the keys and white space do not matter
        assert_eq!(k_old, r.cache_key_for(&db.conn, r#"{ "ghost": "standard", "align": true, "reference": 1 }"#));
        assert_ne!(k_old, r.cache_key_for(&db.conn, r#"{"reference":1,"align":true,"ghost":"fine"}"#));
        let k_new = set_settings(&db, c.file, a).unwrap();
        assert_eq!(k_new, set_settings(&db, c.file, b).unwrap(), "a new strength computes nothing again");
        let f1 = r#"{"reference":1,"output":"fusion"}"#;
        let f2 = r#"{"reference":1,"output":"fusion","fusion_strength":0.7}"#;
        assert_ne!(r.cache_key_for(&db.conn, f1), r.cache_key_for(&db.conn, f2), "the fusion strength changes the pixels");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sources_change_together_with_the_settings() {
        let (db, dir) = library();
        let c = create(&db, "hdr", &[1, 2], 1, r#"{"reference":1}"#, 2).unwrap();
        let k1 = recipe(&db.conn, c.file).unwrap().cache_key(&db.conn);
        let k2 = set_sources(&db, c.file, &[1, 2, 3], r#"{"reference":1}"#).unwrap();
        assert_ne!(k1, k2, "another frame gives another result");
        let r = recipe(&db.conn, c.file).unwrap();
        assert_eq!(r.sources.iter().map(|s| s.file).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(r.sources[2].path, "/fotos/reihe/IMG_3.CR2");
        // the key of other settings of the same sources (merge without areas) differs from the shown one
        assert_ne!(r.cache_key_for(&db.conn, "{}"), r.cache_key(&db.conn));
        assert!(set_sources(&db, c.file, &[1], "{}").is_err(), "at least two");
        assert!(set_sources(&db, 1, &[1, 2], "{}").is_err(), "an ordinary photo has no recipe");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn older_version_marks_missing_and_repair_restores() {
        let (db, dir) = library();
        let c = create(&db, "hdr", &[1, 2, 3], 1, "{}", 1).unwrap();
        db.conn.execute("INSERT INTO develop_state(file, copy, settings) VALUES(?1, '', '{\"exposure\":1}')", [c.file]).ok();
        // older sync: file not on disk → missing; drive offline → status 2
        assert!(!needs_repair(&db.conn));
        db.conn.execute("UPDATE file SET status = 1 WHERE id = ?1", [c.file]).unwrap();
        assert!(needs_repair(&db.conn));
        assert_eq!(repair(&db).unwrap(), 1);
        assert!(!needs_repair(&db.conn));
        let st: i64 = db.conn.query_row("SELECT status FROM file WHERE id = ?1", [c.file], |r| r.get(0)).unwrap();
        assert_eq!(st, STATUS_VIRTUAL);
        db.conn.execute("UPDATE file SET status = 2 WHERE folder = 1", []).unwrap();
        assert_eq!(repair(&db).unwrap(), 1);
        assert_eq!(repair(&db).unwrap(), 0);
        assert!(recipe(&db.conn, c.file).is_some());
        // an older version's "remove missing photos" deletes the row: the recipe goes with it (cascade), sources stay
        db.conn.execute("DELETE FROM image WHERE file = ?1", [c.file]).unwrap();
        db.conn.execute("DELETE FROM develop_state WHERE file = ?1", [c.file]).ok();
        db.conn.execute("DELETE FROM file WHERE id = ?1", [c.file]).unwrap();
        assert!(recipe(&db.conn, c.file).is_none());
        let n: i64 = db.conn.query_row("SELECT count(*) FROM file", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        std::fs::remove_dir_all(dir).ok();
    }

    /// Two merged photos of the same frames with the same settings have keys (cache files) of their own; the key names
    /// its photo.
    #[test]
    fn two_merges_of_the_same_frames_do_not_share_a_key() {
        let (db, dir) = library();
        let a = create(&db, "hdr", &[1, 2, 3], 1, r#"{"reference":1}"#, 2).unwrap();
        let b = create(&db, "hdr", &[1, 2, 3], 1, r#"{"reference":1}"#, 2).unwrap();
        let (ka, kb) = (recipe(&db.conn, a.file).unwrap().cache_key(&db.conn), recipe(&db.conn, b.file).unwrap().cache_key(&db.conn));
        assert_ne!(ka, kb);
        assert!(ka.starts_with(&key_prefix(a.file)) && kb.starts_with(&key_prefix(b.file)), "{ka} {kb}");
        assert_eq!(ka.len(), key_prefix(a.file).len() + 16);
        assert!(!key_prefix(1).starts_with(&key_prefix(12)) && !key_prefix(12).starts_with(&key_prefix(1)));
        std::fs::remove_dir_all(dir).ok();
    }

    /// A file id given again to another photo is not taken for the source: the recipe keeps the stored path (the
    /// merge then reports the source missing instead of merging a wrong photo). A renamed or changed source stays.
    #[test]
    fn a_reused_file_id_is_not_the_source() {
        let (db, dir) = library();
        let c = create(&db, "hdr", &[1, 2, 3], 1, "{}", 2).unwrap();
        let paths = |db: &Db| recipe(&db.conn, c.file).unwrap().source_paths(&db.conn);
        let key = |db: &Db| recipe(&db.conn, c.file).unwrap().cache_key(&db.conn);
        let k0 = key(&db);
        // renamed in the library: same data, other name – still the source, at its new path
        db.conn.execute("UPDATE file SET name = 'Abend_3.CR2' WHERE id = 3", []).unwrap();
        assert_eq!(paths(&db)[2], PathBuf::from("/fotos/reihe/Abend_3.CR2"));
        assert_eq!(key(&db), k0);
        db.conn.execute("UPDATE file SET name = 'IMG_3.CR2' WHERE id = 3", []).unwrap();
        // changed in place: same name, other data – still the source, another key
        db.conn.execute("UPDATE file SET size = 555, mtime = 99 WHERE id = 3", []).unwrap();
        assert_eq!(paths(&db)[2], PathBuf::from("/fotos/reihe/IMG_3.CR2"));
        assert_ne!(key(&db), k0);
        // the source deleted, its id given to another photo
        db.conn.execute("DELETE FROM file WHERE id = 3", []).unwrap();
        db.conn.execute("INSERT INTO file(id, folder, name, format, size, mtime) VALUES(3, 1, 'Katze.JPG', 'JPG', 7, 8)", []).unwrap();
        assert_eq!(paths(&db)[2], PathBuf::from("/fotos/reihe/IMG_3.CR2"), "the stored path, not the other photo");
        assert_eq!(recipe(&db.conn, c.file).unwrap().sources[2].row(&db.conn), None);
        // … and the other photo is no frame of that merge: not "made from these", not "used by a merge", and a change
        // of the frames that names the id takes the photo as it is now, not the record of the deleted one
        assert!(made_from(&db.conn, &[1, 2, 3]).is_empty(), "the photo with the reused id is not a frame of the merge");
        assert!(using_source(&db.conn, 3).is_empty());
        assert_eq!(using_source(&db.conn, 2), vec![c.file]);
        assert_eq!(count_using(&db.conn, &[3]), 0);
        assert_eq!(count_using(&db.conn, &[2, 3]), 1);
        set_sources(&db, c.file, &[1, 2, 3], "{}").unwrap();
        let r = recipe(&db.conn, c.file).unwrap();
        assert_eq!((r.sources[2].path.as_str(), r.sources[2].size, r.sources[2].mtime), ("/fotos/reihe/Katze.JPG", Some(7), Some(8)));
        assert_eq!(r.sources[1].path, "/fotos/reihe/IMG_2.CR2", "a source that is still its photo keeps its record");
        assert_eq!(made_from(&db.conn, &[1, 2, 3]), vec![c.file], "now it is made of these three");
        std::fs::remove_dir_all(dir).ok();
    }

    /// "HDR entfernen" can be taken back: row, image, recipe, develop settings, rating, stack membership and cover come
    /// back as they were. Ordinary photos are never taken out this way.
    #[test]
    fn taken_out_and_brought_back() {
        let (db, dir) = library();
        let c = create(&db, "hdr", &[1, 2, 3], 1, r#"{"reference":1,"regions":[{"frame":0,"strokes":[]}]}"#, 2).unwrap();
        let (_, stack) = crate::series::create(&db, &[c.image, 11, 12, 13], c.image).unwrap();
        db.conn.execute("INSERT INTO develop_state(file, copy, settings, settings_time) VALUES(?1, '', '{\"exposure\":1}', 5)", [c.file]).unwrap();
        db.conn.execute("UPDATE image SET stars = 4 WHERE id = ?1", [c.image]).unwrap();
        db.conn.execute("INSERT INTO image(file, copy) VALUES(?1, 'Kopie 1')", [c.file]).unwrap();
        let w = work(&db.conn, c.file);
        assert_eq!((w.areas, w.develop.len(), w.copies), (1, 1, 1));
        let before = recipe(&db.conn, c.file).unwrap();
        assert!(take_out(&db, &[1]).is_err(), "an ordinary photo is never taken out");
        assert!(take_out(&db, &[c.file, 1]).is_err());
        assert!(recipe(&db.conn, c.file).is_some(), "nothing happened");
        let snap = take_out(&db, &[c.file]).unwrap();
        let n = |sql: &str| db.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n(&format!("SELECT count(*) FROM file WHERE id = {}", c.file)), 0);
        assert_eq!(n(&format!("SELECT count(*) FROM develop_state WHERE file = {}", c.file)), 0);
        assert_eq!(n("SELECT count(*) FROM merge"), 0);
        assert_eq!(n("SELECT count(*) FROM file"), 3, "the sources stay");
        assert_eq!(n(&format!("SELECT count(*) FROM stack_image WHERE stack = {stack}")), 3);
        bring_back(&db, &snap).unwrap();
        assert_eq!(recipe(&db.conn, c.file).unwrap(), before);
        assert_eq!(n(&format!("SELECT stars FROM image WHERE id = {}", c.image)), 4);
        assert_eq!(n(&format!("SELECT count(*) FROM image WHERE file = {}", c.file)), 2);
        assert_eq!(n(&format!("SELECT count(*) FROM develop_state WHERE file = {} AND settings = '{{\"exposure\":1}}'", c.file)), 1);
        assert_eq!(n(&format!("SELECT stack FROM stack_image WHERE image = {}", c.image)), stack);
        assert_eq!(n(&format!("SELECT cover FROM stack WHERE id = {stack}")), c.image);
        assert_eq!(n(&format!("SELECT status FROM file WHERE id = {}", c.file)), STATUS_VIRTUAL);
        std::fs::remove_dir_all(dir).ok();
    }

    /// ⌘Z of creating merges: the stacks go back and the photos go in one transaction – all or nothing – and the
    /// photos can be brought back (a merge worked on since is not lost).
    #[test]
    fn undo_of_creating_is_one_transaction_and_can_be_taken_back() {
        let (db, dir) = library();
        let a = create(&db, "hdr", &[1, 2], 0, "{}", 2).unwrap();
        let (change, stack) = crate::series::create(&db, &[a.image, 11, 12], a.image).unwrap();
        let b = create(&db, "hdr", &[2, 3], 0, "{}", 2).unwrap();
        db.conn.execute("INSERT INTO develop_state(file, copy, settings) VALUES(?1, '', '{\"shadow\":20}')", [a.file]).unwrap();
        let n = |sql: &str| db.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        // the second item is no merged photo: nothing of the first one is done either
        assert!(undo_create(&db, &[(a.file, Some(change.clone())), (1, None)]).is_err());
        assert_eq!(n("SELECT count(*) FROM merge"), 2);
        assert_eq!(n(&format!("SELECT count(*) FROM stack WHERE id = {stack}")), 1, "the stack is still there");
        let snap = undo_create(&db, &[(b.file, None), (a.file, Some(change))]).unwrap();
        assert_eq!(n("SELECT count(*) FROM merge"), 0);
        assert_eq!(n("SELECT count(*) FROM file"), 3);
        assert_eq!(n(&format!("SELECT count(*) FROM stack WHERE id = {stack}")), 0, "the stack made for the merge is gone");
        bring_back(&db, &snap).unwrap();
        assert_eq!(n("SELECT count(*) FROM merge"), 2);
        assert_eq!(n(&format!("SELECT count(*) FROM develop_state WHERE file = {} AND settings = '{{\"shadow\":20}}'", a.file)), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A source row merged into another row of the same photo (folder moved onto a folder the library knows): the
    /// recipe names the kept row; a merged photo follows its reference into the new folder; which merged photos use a
    /// file as a source.
    #[test]
    fn recipes_follow_merged_and_moved_sources() {
        let (db, dir) = library();
        db.conn
            .execute_batch(
                "INSERT INTO folder(id, root, path) VALUES(2, 1, 'ziel/');
                 INSERT INTO file(id, folder, name, format, size, mtime) VALUES(21, 2, 'IMG_1.CR2', 'CR2', 100, 10), (22, 2, 'IMG_2.CR2', 'CR2', 101, 11), (23, 2, 'IMG_3.CR2', 'CR2', 102, 12);",
            )
            .unwrap();
        let c = create(&db, "hdr", &[1, 2, 3], 1, r#"{"reference":1}"#, 2).unwrap();
        assert_eq!(using_source(&db.conn, 2), vec![c.file]);
        assert_eq!(count_using(&db.conn, &[3, 99]), 1);
        assert_eq!(count_using(&db.conn, &[99]), 0);
        assert_eq!(count_using(&db.conn, &[3, c.file]), 0, "a merged photo that goes as well is not counted");
        let tx = db.conn.unchecked_transaction().unwrap();
        for (s, t) in [(1, 21), (2, 22), (3, 23)] {
            assert_eq!(replace_source(&tx, s, t).unwrap(), 1);
            tx.execute("DELETE FROM file WHERE id = ?1", [s]).unwrap();
        }
        assert_eq!(follow_sources(&tx, 1).unwrap(), 1);
        tx.commit().unwrap();
        let r = recipe(&db.conn, c.file).unwrap();
        assert_eq!(r.sources.iter().map(|s| s.file).collect::<Vec<_>>(), vec![21, 22, 23]);
        assert_eq!(r.source_paths(&db.conn), vec![PathBuf::from("/fotos/ziel/IMG_1.CR2"), PathBuf::from("/fotos/ziel/IMG_2.CR2"), PathBuf::from("/fotos/ziel/IMG_3.CR2")]);
        assert_eq!(r.sources[0].path, "/fotos/ziel/IMG_1.CR2");
        assert_eq!(file_path(&db.conn, c.file), Some(PathBuf::from("/fotos/ziel/IMG_2-HDR.dng")));
        // moved rows that keep their id and get another name: the record follows, the row still counts
        db.conn.execute("UPDATE file SET name = 'IMG_2 (2).CR2', size = 500, mtime = 600 WHERE id = 22", []).unwrap();
        assert_eq!(recipe(&db.conn, c.file).unwrap().sources[1].row(&db.conn), None, "other name and other data: not taken for the source");
        assert_eq!(refresh_sources(&db.conn, &[22]).unwrap(), 1);
        assert_eq!(recipe(&db.conn, c.file).unwrap().source_paths(&db.conn)[1], PathBuf::from("/fotos/ziel/IMG_2 (2).CR2"));
        std::fs::remove_dir_all(dir).ok();
    }

    /// A rebuilt library takes the merged photos of the previous one: sources by path (new ids), rating, develop
    /// settings and recipe; a merge whose source the new library does not have is counted as lost.
    #[test]
    fn rebuilt_library_takes_the_merged_photos_over() {
        let (old, dir) = library();
        let c = create(&old, "hdr", &[1, 2, 3], 1, r#"{"reference":1,"ghost":"standard"}"#, 2).unwrap();
        old.conn.execute("INSERT INTO develop_state(file, copy, settings, settings_time) VALUES(?1, '', '{\"exposure\":1}', 5)", [c.file]).unwrap();
        old.conn.execute("UPDATE image SET stars = 5, flag = 1 WHERE id = ?1", [c.image]).unwrap();
        old.conn.execute("INSERT INTO file(id, folder, name, format) VALUES(9, 1, 'weg.CR2', 'CR2')", []).unwrap();
        let lost = create(&old, "hdr", &[1, 9], 0, "{}", 2).unwrap();
        let _ = lost;
        // the new library: the same photos under other ids, without 'weg.CR2'
        let new = Db::open(&dir.join("rebuild.sqlite")).unwrap();
        new.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(5, '/fotos/', 'fotos', 'own');
                 INSERT INTO folder(id, root, path) VALUES(7, 5, 'reihe/');
                 INSERT INTO file(id, folder, name, format, size, mtime) VALUES(41, 7, 'IMG_3.CR2', 'CR2', 102, 12), (42, 7, 'IMG_2.CR2', 'CR2', 101, 11), (43, 7, 'IMG_1.CR2', 'CR2', 100, 10);
                 INSERT INTO image(file) VALUES(41), (42), (43);",
            )
            .unwrap();
        assert_eq!(copy_from(&old.conn, &new).unwrap(), Copied { copied: 1, lost: 1 });
        let file: i64 = new.conn.query_row("SELECT file FROM merge", [], |r| r.get(0)).unwrap();
        let r = recipe(&new.conn, file).unwrap();
        assert_eq!(r.sources.iter().map(|s| s.file).collect::<Vec<_>>(), vec![43, 42, 41]);
        assert_eq!(r.settings, r#"{"reference":1,"ghost":"standard"}"#);
        assert!(is_virtual(&new.conn, file));
        assert_eq!(file_path(&new.conn, file), Some(PathBuf::from("/fotos/reihe/IMG_2-HDR.dng")));
        let (status, folder): (i64, i64) = new.conn.query_row("SELECT status, folder FROM file WHERE id = ?1", [file], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((status, folder), (STATUS_VIRTUAL, 7));
        let (stars, flag): (i64, i64) = new.conn.query_row("SELECT stars, flag FROM image WHERE file = ?1", [file], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((stars, flag), (5, 1));
        let s: String = new.conn.query_row("SELECT settings FROM develop_state WHERE file = ?1", [file], |r| r.get(0)).unwrap();
        assert_eq!(s, "{\"exposure\":1}");
        // a second run takes nothing twice
        assert_eq!(copy_from(&old.conn, &new).unwrap().copied, 0);
        let n: i64 = new.conn.query_row("SELECT count(*) FROM merge", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A library with merges opens in a version that does not know the table: the schema version is unchanged, so an
    /// older graineo (schema 20) opens it normally and ignores the table.
    #[test]
    fn schema_version_unchanged() {
        let (db, dir) = library();
        create(&db, "hdr", &[1, 2], 0, "{}", 1).unwrap();
        let v: i32 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, crate::db::VERSION);
        assert_eq!(crate::db::VERSION, 20);
        drop(db);
        // reopening keeps everything
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        let n: i64 = db.conn.query_row("SELECT count(*) FROM merge", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        std::fs::remove_dir_all(dir).ok();
    }
}

/// Tests of the fourth audit that show open findings (ignored).
#[cfg(test)]
#[path = "merge_audit4_tests.rs"]
mod audit4;
