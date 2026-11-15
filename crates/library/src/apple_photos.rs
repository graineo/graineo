//! Read the Apple Photos library (read-only, like the Lightroom catalog): photos with original name,
//! capture time, dimensions, camera data, favorite, and the user's own albums.
//!
//! The database (`database/Photos.sqlite`) is undocumented and changes with macOS versions;
//! therefore only what can be found reliably via the table names is read (entity numbers from
//! `Z_PRIMARYKEY`), and nothing is ever written (`mode=ro`, `query_only`). If an original exists only in iCloud
//! ("Optimize Mac Storage"), the preview files under `resources/derivatives` exist locally.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// A photo or video of the library.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    /// identifier (`ZUUID`), also part of the PhotoKit identifier `UUID/L0/001`
    pub uuid: String,
    /// subfolder under `originals/` and file name of the original
    pub folder: String,
    pub file: String,
    /// name under which the photo was taken or imported (e.g. `IMG_1234.HEIC`)
    pub original_name: Option<String>,
    /// local capture time `YYYY-MM-DDThh:mm:ss`
    pub capture: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub size: Option<i64>,
    pub video: bool,
    /// duration of a video in seconds
    pub duration: Option<f64>,
    pub favorite: bool,
    pub hidden_flag: bool,
    pub camera: Option<String>,
    pub lens: Option<String>,
    /// last edited in Apple Photos (Unix seconds); changes the preview key so that the
    /// grid shows Apple's new version
    pub edited: Option<i64>,
}

impl Asset {
    /// Path of the original in the library (only present if it is not only in iCloud).
    pub fn original(&self, photos_library: &Path) -> PathBuf {
        photos_library.join("originals").join(&self.folder).join(&self.file)
    }

    /// Largest local preview file of Apple Photos (for photos whose original is only in iCloud).
    pub fn preview(&self, photos_library: &Path) -> Option<PathBuf> {
        preview_for(photos_library, &self.uuid)
    }
}

/// Local Apple Photos preview for an identifier: 1024 px (`_1_105_c`), else 360 px (`masters/…_4_5005_c`).
pub fn preview_for(photos_library: &Path, uuid: &str) -> Option<PathBuf> {
    let d = uuid.get(..1)?;
    let derivatives = photos_library.join("resources").join("derivatives");
    [derivatives.join(d).join(format!("{uuid}_1_105_c.jpeg")), derivatives.join("masters").join(d).join(format!("{uuid}_4_5005_c.jpeg"))].into_iter().find(|p| p.is_file())
}

// ───────────────────────────────────────────── Photos cropped or rotated in Apple Photos

/// Photos whose shape was changed in Apple Photos (cropped, straightened, rotated): the current version has another
/// upright size than the original. Apple's face boxes refer to the current version, and graineo shows it. `None` if
/// the library does not give the original sizes (older macOS).
fn reshaped_in(photos_library: &Path) -> Option<std::collections::HashSet<String>> {
    let c = open(photos_library).ok()?;
    let mut st = c
        .prepare(
            "SELECT a.ZUUID FROM ZASSET a JOIN ZADDITIONALASSETATTRIBUTES aa ON aa.ZASSET = a.Z_PK
             WHERE a.ZTRASHEDSTATE = 0 AND a.ZUUID IS NOT NULL AND a.ZADJUSTMENTTIMESTAMP IS NOT NULL AND coalesce(a.ZKIND, 0) = 0
               AND aa.ZORIGINALWIDTH > 0 AND aa.ZORIGINALHEIGHT > 0
               AND NOT ((coalesce(aa.ZORIGINALORIENTATION, 1) BETWEEN 5 AND 8 AND a.ZWIDTH = aa.ZORIGINALHEIGHT AND a.ZHEIGHT = aa.ZORIGINALWIDTH)
                     OR (coalesce(aa.ZORIGINALORIENTATION, 1) NOT BETWEEN 5 AND 8 AND a.ZWIDTH = aa.ZORIGINALWIDTH AND a.ZHEIGHT = aa.ZORIGINALHEIGHT))",
        )
        .ok()?;
    let r = st.query_map([], |r| r.get::<_, String>(0)).ok()?.flatten().collect();
    Some(r)
}

/// File with the identifiers of the reshaped photos (written at every sync).
pub fn reshaped_file() -> PathBuf {
    crate::data_dir().join("apple-photos").join("reshaped.json")
}

static RESHAPED: std::sync::RwLock<Option<std::sync::Arc<std::collections::HashSet<String>>>> = std::sync::RwLock::new(None);

fn reshaped_set() -> std::sync::Arc<std::collections::HashSet<String>> {
    if let Some(s) = RESHAPED.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return s.clone();
    }
    let s: std::collections::HashSet<String> = std::fs::read(reshaped_file()).ok().and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok()).unwrap_or_default().into_iter().collect();
    let s = std::sync::Arc::new(s);
    *RESHAPED.write().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
    s
}

/// Remember the reshaped photos (file and memory).
pub fn set_reshaped(set: std::collections::HashSet<String>) {
    let mut v: Vec<&String> = set.iter().collect();
    v.sort();
    let p = reshaped_file();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = crate::files::write_atomic(&p, serde_json::to_string(&v).unwrap_or_default().as_bytes());
    *RESHAPED.write().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(set));
}

/// Was this photo of the Photos library cropped or rotated in Apple Photos?
pub fn reshaped(path: &Path) -> bool {
    uuid_from_path(path).is_some_and(|u| reshaped_set().contains(&u))
}

/// Full size render of an edited photo (`resources/renders/<x>/<uuid>_1_201_a.*`), if it is on this Mac.
pub fn render_for(photos_library: &Path, uuid: &str) -> Option<PathBuf> {
    let d = photos_library.join("resources").join("renders").join(uuid.get(..1)?);
    ["heic", "jpeg", "jpg"].iter().map(|e| d.join(format!("{uuid}_1_201_a.{e}"))).find(|p| p.is_file())
}

/// For a photo cropped or rotated in Apple Photos: Apple's current version (full size render if on this Mac, else
/// Photos' preview). `None` for other photos (the original fits) or if no current version is on this Mac.
pub fn current_version(path: &Path) -> Option<PathBuf> {
    if !reshaped(path) {
        return None;
    }
    let library = path.ancestors().find(|a| a.extension().is_some_and(|e| e == "photoslibrary"))?;
    let uuid = uuid_from_path(path)?;
    render_for(library, &uuid).or_else(|| preview_for(library, &uuid))
}

/// Preview keys of the reshaped photos (their previews may still show the original): renewed once per revision
/// (info `apple_fassung`), afterwards a new edit in Photos changes the key anyway. Empty when done.
pub fn outdated_preview_keys(db: &crate::db::Db) -> Vec<u64> {
    const REVISION: &str = "1";
    if db.info("apple_revision").as_deref() == Some(REVISION) {
        return Vec::new();
    }
    let set = reshaped_set();
    if set.is_empty() && !reshaped_file().exists() {
        // not synced with this version yet
        return Vec::new();
    }
    let mut keys = Vec::new();
    if let Ok(mut st) = db.conn.prepare(
        "SELECT s.path || o.path || d.name, d.name, coalesce(d.size, 0), coalesce(d.mtime, 0), d.orientation FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE s.origin = ?1",
    ) {
        let rows = st.query_map([ORIGIN], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, Option<i64>>(4)?)));
        if let Ok(rows) = rows {
            for (path, name, size, mtime, orientation) in rows.flatten() {
                if set.contains(name.split('.').next().unwrap_or_default()) {
                    keys.push(crate::preview::cache_key_with_orientation(crate::preview::cache_key(&path, size, mtime), orientation));
                }
            }
        }
    }
    let _ = db.set_info("apple_revision", REVISION);
    keys
}

/// A user album with the identifiers of its photos.
#[derive(Clone, Debug, PartialEq)]
pub struct Album {
    pub title: String,
    pub uuids: Vec<String>,
}

/// The usual library (`~/Pictures/Photos Library.photoslibrary`), if it exists.
pub fn standard() -> Option<PathBuf> {
    let p = dirs_home()?.join("Pictures").join("Photos Library.photoslibrary");
    p.join("database").join("Photos.sqlite").is_file().then_some(p)
}

fn dirs_home() -> Option<PathBuf> {
    sysdirs::home()
}

/// Folder of the sidecars (XMP) for photos from Apple Photos, in graineo's data folder.
pub fn sidecar_folder() -> PathBuf {
    crate::data_dir().join("apple-photos").join("sidecars")
}

/// Is the path inside an Apple Photos library? graineo never writes there.
pub fn in_photos_library(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str().to_string_lossy().ends_with(".photoslibrary"))
}

/// Identifier of a photo from its path in the library (file name without extension).
pub fn uuid_from_path(p: &Path) -> Option<String> {
    in_photos_library(p).then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned())).flatten()
}

pub(crate) fn open(photos_library: &Path) -> Result<Connection, String> {
    let db = photos_library.join("database").join("Photos.sqlite");
    let uri = format!("{}?mode=ro", crate::lightroom::file_uri(&db));
    let c = Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| e.to_string())?;
    c.pragma_update(None, "query_only", true).map_err(|e| e.to_string())?;
    // Apple Photos is writing right now: wait briefly instead of failing
    c.pragma_update(None, "busy_timeout", 10_000).map_err(|e| e.to_string())?;
    Ok(c)
}

/// Seconds since 2001-01-01 (Core Data) plus time zone offset → local time `YYYY-MM-DDThh:mm:ss`.
fn local_time_of(secs_2001: f64, offset_val: i64) -> Option<String> {
    if !secs_2001.is_finite() {
        return None;
    }
    let unix = secs_2001 as i64 + 978_307_200 + offset_val;
    let days = unix.div_euclid(86_400);
    let rest = unix.rem_euclid(86_400);
    // days since 1970 → date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", rest / 3600, rest % 3600 / 60, rest % 60))
}

/// All photos and videos (without the trash) and the user's albums.
pub fn read(photos_library: &Path) -> Result<(Vec<Asset>, Vec<Album>), String> {
    let c = open(photos_library)?;
    let f = |e: rusqlite::Error| e.to_string();
    let mut st = c
        .prepare(
            "SELECT a.ZUUID, a.ZDIRECTORY, a.ZFILENAME, aa.ZORIGINALFILENAME, a.ZDATECREATED, coalesce(aa.ZTIMEZONEOFFSET, 0),
                    a.ZWIDTH, a.ZHEIGHT, aa.ZORIGINALFILESIZE, a.ZKIND, a.ZFAVORITE, a.ZHIDDEN,
                    ea.ZCAMERAMAKE, ea.ZCAMERAMODEL, ea.ZLENSMODEL, a.ZADJUSTMENTTIMESTAMP, a.ZDURATION
             FROM ZASSET a
             LEFT JOIN ZADDITIONALASSETATTRIBUTES aa ON aa.ZASSET = a.Z_PK
             LEFT JOIN ZEXTENDEDATTRIBUTES ea ON ea.ZASSET = a.Z_PK
             WHERE a.ZTRASHEDSTATE = 0 AND a.ZUUID IS NOT NULL AND a.ZFILENAME IS NOT NULL",
        )
        .map_err(f)?;
    let assets: Vec<Asset> = st
        .query_map([], |r| {
            let marker: Option<String> = r.get(12)?;
            let model: Option<String> = r.get(13)?;
            // as in the rest of the library: the model (for iPhones without "Apple"), else the manufacturer
            let clean = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            let camera = clean(model).or(clean(marker));
            Ok(Asset {
                uuid: r.get(0)?,
                folder: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                file: r.get(2)?,
                original_name: r.get(3)?,
                capture: r.get::<_, Option<f64>>(4)?.and_then(|s| local_time_of(s, r.get(5).unwrap_or(0))),
                width: r.get(6)?,
                height: r.get(7)?,
                size: r.get(8)?,
                video: r.get::<_, Option<i64>>(9)? == Some(1),
                duration: r.get::<_, Option<f64>>(16)?.filter(|d| *d > 0.0),
                favorite: r.get::<_, Option<i64>>(10)? == Some(1),
                hidden_flag: r.get::<_, Option<i64>>(11)? == Some(1),
                camera,
                lens: r.get::<_, Option<String>>(14)?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                edited: r.get::<_, Option<f64>>(15)?.filter(|s| s.is_finite()).map(|s| s as i64 + 978_307_200),
            })
        })
        .map_err(f)?
        .collect::<Result<_, _>>()
        .map_err(f)?;
    Ok((assets, albums(&c).unwrap_or_default()))
}

/// User albums (`ZGENERICALBUM.ZKIND = 2`). The join table is named after the entity numbers,
/// e.g. `Z_33ASSETS` with `Z_33ALBUMS`/`Z_3ASSETS`.
fn albums(c: &Connection) -> Result<Vec<Album>, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let entry = |name: &str| -> Result<i64, String> { c.query_row("SELECT Z_ENT FROM Z_PRIMARYKEY WHERE Z_NAME = ?1", [name], |r| r.get(0)).map_err(f) };
    let (asset, album) = (entry("Asset")?, entry("Album")?);
    let table = format!("Z_{album}ASSETS");
    let sql = format!(
        "SELECT g.Z_PK, g.ZTITLE, a.ZUUID FROM ZGENERICALBUM g
         JOIN {table} j ON j.Z_{album}ALBUMS = g.Z_PK
         JOIN ZASSET a ON a.Z_PK = j.Z_{asset}ASSETS
         WHERE g.ZKIND = 2 AND g.ZTRASHEDSTATE = 0 AND a.ZTRASHEDSTATE = 0 AND g.ZTITLE IS NOT NULL
         ORDER BY g.ZTITLE, j.Z_FOK_{asset}ASSETS"
    );
    let mut st = c.prepare(&sql).map_err(f)?;
    let mut albums: Vec<(i64, Album)> = Vec::new();
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).map_err(f)?;
    for row in rows {
        let (id, title, uuid) = row.map_err(f)?;
        match albums.iter_mut().find(|(i, _)| *i == id) {
            Some((_, a)) => a.uuids.push(uuid),
            None => albums.push((id, Album { title, uuids: vec![uuid] })),
        }
    }
    Ok(albums.into_iter().map(|(_, a)| a).collect())
}

/// Files of the photos in "Recently Deleted" (folder, file name): they are parked, not removed.
fn trashed_files(photos_library: &Path) -> Result<Vec<(String, String)>, String> {
    let c = open(photos_library)?;
    let f = |e: rusqlite::Error| e.to_string();
    let mut st = c.prepare("SELECT coalesce(ZDIRECTORY, ''), ZFILENAME FROM ZASSET WHERE ZTRASHEDSTATE <> 0 AND ZUUID IS NOT NULL AND ZFILENAME IS NOT NULL").map_err(f)?;
    let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(f)?.collect::<Result<_, _>>().map_err(f)?;
    Ok(v)
}

/// Would this many photos disappear for good at once, the read is taken as suspicious (Photos repairing or
/// rebuilding its library, iCloud resyncing) and the sync is held back until the user confirms: more than 200, or
/// more than 5 % of the known ones (up to 10 always pass, so a few deletions in a small library go through).
pub fn too_many_gone(gone: usize, known: usize) -> bool {
    gone > 200 || (gone > 10 && gone * 20 > known)
}

/// `file.status` of photos in "Recently Deleted" or hidden in Apple Photos: kept with everything (ratings,
/// keywords, edits), but not shown; back to 0 when restored or shown again in Apple Photos.
pub const PARKED: i64 = 4;

/// Origin of the Photos library root folder in the library.
pub const ORIGIN: &str = "apple-photos";
/// Marker (column `rules`) of collections that come from Apple Photos albums.
const COLLECTION_MARKER: &str = "apple-photos";

/// Result of a sync.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub total: usize,
    pub new: usize,
    /// gone from Apple Photos for good (also not in "Recently Deleted"): removed from the library
    pub removed: usize,
    /// in "Recently Deleted" or hidden: kept, not shown ([`PARKED`])
    pub parked: usize,
    /// entries parked or shown again in this run
    pub state_changed: usize,
    pub albums: usize,
    /// held back (nothing changed): (would be removed, known) – see [`too_many_gone`]; the user may confirm
    pub held_back: Option<(usize, usize)>,
}

/// File with the original names (file ID → name) so that grid and info show `IMG_1234.HEIC` instead of the identifier.
pub fn names_file() -> PathBuf {
    crate::data_dir().join("apple-photos").join("names.json")
}

/// Read original names (empty if there are none).
pub fn read_names() -> std::collections::HashMap<i64, String> {
    std::fs::read(names_file()).ok().and_then(|b| serde_json::from_slice::<std::collections::HashMap<String, String>>(&b).ok()).map(|m| m.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()).unwrap_or_default()
}

/// Import or sync the Photos library into the library: a root folder "Apple Fotos" with
/// the subfolders of `originals/`, one file per photo (name = file name in the library, its identifier).
/// Same state as Apple Photos: photos in "Recently Deleted" or hidden disappear from view but keep their row
/// (ratings, keywords, edits; [`PARKED`]) and come back with everything when restored; photos gone for good are
/// removed. An empty or unreadable Photos.sqlite is an error (nothing changed); a read that would remove too many
/// at once ([`too_many_gone`]) changes nothing and reports `held_back` – with `force` (the user confirmed) it is
/// carried out. User albums and favorites become collections in the set "Apple Fotos". Nothing is ever written into
/// the Photos library.
pub fn reconcile(db: &crate::db::Db, photos_library: &Path) -> Result<Report, String> {
    reconcile_with(db, photos_library, false)
}

/// [`reconcile`]; `force`: also when many photos would be removed at once (confirmed by the user).
pub fn reconcile_with(db: &crate::db::Db, photos_library: &Path, force: bool) -> Result<Report, String> {
    reconcile_inner(db, photos_library, force, &|| {})
}

/// Changes graineo itself makes to Apple Photos and the library rows of its photos (deleting via Apple Photos), per
/// library file: a counter per library, so that two libraries in one process (tests running in parallel) do not
/// discard each other's syncs.
static OWN_CHANGES: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, u64>>> = std::sync::Mutex::new(None);

fn own_changes(library: &Path) -> u64 {
    OWN_CHANGES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(library).copied()).unwrap_or(0)
}

/// graineo changes photos of Apple Photos (deleting): call before asking Apple Photos and again after the library
/// rows are gone. A sync that read Apple Photos before (or meanwhile) is discarded instead of putting the deleted
/// photos back as new ones; the next sync reads the new state. For the standard library ([`crate::db_path`]).
pub fn note_own_change() {
    note_own_change_in(&crate::db_path());
}

/// [`note_own_change`] for the library file `library` (the path the [`crate::db::Db`] was opened with).
pub fn note_own_change_in(library: &Path) {
    *OWN_CHANGES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).entry(library.to_path_buf()).or_insert(0) += 1;
}

/// Message of a sync discarded because graineo changed Apple Photos meanwhile (see [`note_own_change`]).
pub const OUTDATED: &str = "Apple Fotos wurde während des Abgleichs geändert – der nächste Abgleich liest neu";

/// [`reconcile_with`]; `after_read` runs between reading Apple Photos and writing the library (tests: a deletion
/// in between).
fn reconcile_inner(db: &crate::db::Db, photos_library: &Path, force: bool, after_read: &dyn Fn()) -> Result<Report, String> {
    use rusqlite::params;
    use std::collections::{HashMap, HashSet};
    let generation = own_changes(&db.path);
    let (assets, albums) = read(photos_library)?;
    let reshaped = reshaped_in(photos_library);
    // hidden in Apple Photos: not newly taken in, existing entries parked
    let (hidden, assets): (Vec<Asset>, Vec<Asset>) = assets.into_iter().partition(|a| a.hidden_flag);
    let trashed = trashed_files(photos_library)?;
    after_read();
    let f = |e: rusqlite::Error| e.to_string();
    let root_dir = format!("{}/originals/", photos_library.display());
    let tx = db.conn.unchecked_transaction().map_err(f)?;
    let root: i64 = match tx.query_row("SELECT id FROM root_folder WHERE origin = ?1 AND path = ?2", params![ORIGIN, root_dir], |r| r.get(0)) {
        Ok(id) => id,
        Err(_) => {
            tx.execute("INSERT INTO root_folder(path, name, origin) VALUES(?1, 'Apple Fotos', ?2)", params![root_dir, ORIGIN]).map_err(f)?;
            tx.last_insert_rowid()
        }
    };
    let mut folder: HashMap<String, i64> = tx
        .prepare("SELECT path, id FROM folder WHERE root = ?1")
        .map_err(f)?
        .query_map([root], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(f)?
        .collect::<Result<_, _>>()
        .map_err(f)?;
    // existing entries with their values: only what changed is written (Apple Photos changes its
    // database often; a sync without changes should not occupy the library)
    type Values = (Option<String>, Option<String>, Option<i64>, Option<i64>, Option<i64>, i64, i64);
    let mut existing: HashMap<(i64, String), (i64, i64, Values)> = tx
        .prepare("SELECT d.folder, d.name, d.id, (SELECT min(b.id) FROM image b WHERE b.file = d.id AND b.copy IS NULL), d.camera, d.capture_time, d.width, d.height, d.size, coalesce(d.mtime, 0), coalesce(d.status, 0) FROM file d JOIN folder o ON o.id = d.folder WHERE o.root = ?1")
        .map_err(f)?
        .query_map([root], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get::<_, Option<i64>>(3)?.unwrap_or(0), (r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?)))))
        .map_err(f)?
        .collect::<Result<_, _>>()
        .map_err(f)?;
    // an empty or broken read (Photos repairs its library, iCloud rebuilds, empty library at the same
    // path) must never touch what the library holds
    if assets.is_empty() && !existing.is_empty() {
        return Err(format!("Apple Fotos lieferte keine Fotos, die Bibliothek kennt {} – nichts geändert", existing.len()));
    }
    let known = existing.len();
    let mut report = Report { total: assets.len(), ..Report::default() };
    // in "Recently Deleted" or hidden: parked (row with everything kept, not shown)
    let mut parked: Vec<i64> = Vec::new();
    for (dir, file) in hidden.iter().map(|a| (&a.folder, &a.file)).chain(trashed.iter().map(|(d, n)| (d, n))) {
        if let Some((did, _, old)) = folder.get(&format!("{dir}/")).and_then(|&fid| existing.remove(&(fid, file.clone()))) {
            report.parked += 1;
            if old.6 != PARKED {
                parked.push(did);
            }
        }
    }
    // gone for good: known entries not among the photos read
    let present: HashSet<(String, String)> = assets.iter().map(|a| (format!("{}/", a.folder), a.file.clone())).collect();
    let by_id: HashMap<i64, &String> = folder.iter().map(|(p, id)| (*id, p)).collect();
    let gone = existing.keys().filter(|(fid, name)| !by_id.get(fid).is_some_and(|p| present.contains(&((*p).clone(), name.clone())))).count();
    if !force && too_many_gone(gone, known) {
        // nothing written: the transaction is rolled back
        report.held_back = Some((gone, known));
        return Ok(report);
    }
    for did in parked {
        tx.execute("UPDATE file SET status = ?2 WHERE id = ?1", rusqlite::params![did, PARKED]).map_err(f)?;
        report.state_changed += 1;
    }
    let mut to_uuid: HashMap<String, (i64, i64)> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    {
        let mut upd = tx.prepare("UPDATE file SET camera = ?2, capture_time = ?3, width = ?4, height = ?5, size = ?6, mtime = ?7, status = 0 WHERE id = ?1").map_err(f)?;
        let mut ins = tx.prepare("INSERT INTO file(folder, name, format, size, camera, capture_time, width, height, mtime, status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0)").map_err(f)?;
        let mut into_image = tx.prepare("INSERT INTO image(file) VALUES(?1)").map_err(f)?;
        for a in &assets {
            let folder_path = format!("{}/", a.folder);
            let folder_id = match folder.get(&folder_path) {
                Some(&id) => id,
                None => {
                    tx.execute("INSERT INTO folder(root, path) VALUES(?1, ?2)", params![root, folder_path]).map_err(f)?;
                    let id = tx.last_insert_rowid();
                    folder.insert(folder_path, id);
                    id
                }
            };
            let format = crate::format_from_extension(&a.file).unwrap_or("JPG");
            let ids = match existing.remove(&(folder_id, a.file.clone())) {
                Some((did, bid, old)) => {
                    // zustand 0: a photo marked missing that is back in Apple Photos is present again
                    let new: Values = (a.camera.clone(), a.capture.clone(), a.width, a.height, a.size, a.edited.unwrap_or(0), 0);
                    if old.6 != 0 {
                        report.state_changed += 1;
                    }
                    if old != new {
                        upd.execute(params![did, a.camera, a.capture, a.width, a.height, a.size, a.edited.unwrap_or(0)]).map_err(f)?;
                    }
                    (did, bid)
                }
                None => {
                    ins.execute(params![folder_id, a.file, format, a.size, a.camera, a.capture, a.width, a.height, a.edited.unwrap_or(0)]).map_err(f)?;
                    let did = tx.last_insert_rowid();
                    into_image.execute([did]).map_err(f)?;
                    report.new += 1;
                    (did, tx.last_insert_rowid())
                }
            };
            if let Some(n) = &a.original_name {
                names.insert(ids.0.to_string(), n.clone());
            }
            to_uuid.insert(a.uuid.clone(), ids);
        }
    }
    // durations of the videos (also those only in iCloud) for the badge in the grid
    crate::video::set_durations(assets.iter().filter_map(|a| Some((a.original(photos_library).to_string_lossy().into_owned(), a.duration?))));
    // gone from Apple Photos for good (also out of "Recently Deleted"): gone here too
    for (_, (did, _, _)) in existing {
        tx.execute("DELETE FROM file WHERE id = ?1", [did]).map_err(f)?;
        report.removed += 1;
    }
    // albums and favorites as collections in the set "Apple Fotos"
    let set_id: i64 = match tx.query_row("SELECT id FROM collection WHERE kind = 1 AND rules = ?1", [COLLECTION_MARKER], |r| r.get(0)) {
        Ok(id) => id,
        Err(_) => {
            tx.execute("INSERT INTO collection(name, kind, rules) VALUES('Apple Fotos', 1, ?1)", [COLLECTION_MARKER]).map_err(f)?;
            tx.last_insert_rowid()
        }
    };
    let mut targets: Vec<(String, Vec<String>)> = albums.into_iter().map(|a| (a.title, a.uuids)).collect();
    let favs: Vec<String> = assets.iter().filter(|a| a.favorite).map(|a| a.uuid.clone()).collect();
    if !favs.is_empty() {
        targets.insert(0, ("Favoriten".to_string(), favs));
    }
    let mut children: HashMap<String, i64> = tx
        .prepare("SELECT name, id FROM collection WHERE parent = ?1 AND rules = ?2")
        .map_err(f)?
        .query_map(params![set_id, COLLECTION_MARKER], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(f)?
        .collect::<Result<_, _>>()
        .map_err(f)?;
    let mut seen: HashSet<i64> = HashSet::new();
    for (title, uuids) in targets {
        let sid = match children.get(&title) {
            Some(&id) => id,
            None => {
                tx.execute("INSERT INTO collection(name, parent, kind, rules) VALUES(?1, ?2, 0, ?3)", params![title, set_id, COLLECTION_MARKER]).map_err(f)?;
                let id = tx.last_insert_rowid();
                children.insert(title.clone(), id);
                id
            }
        };
        if !seen.insert(sid) {
            continue; // two albums with the same name: the first one wins
        }
        report.albums += 1;
        // only rewrite if the content changed
        let expected: Vec<i64> = uuids.iter().filter_map(|u| to_uuid.get(u).map(|x| x.1)).collect();
        let actual: Vec<i64> = tx.prepare_cached("SELECT image FROM collection_image WHERE collection = ?1 ORDER BY position").map_err(f)?.query_map([sid], |r| r.get(0)).map_err(f)?.collect::<Result<_, _>>().map_err(f)?;
        if actual == expected {
            continue;
        }
        tx.execute("DELETE FROM collection_image WHERE collection = ?1", [sid]).map_err(f)?;
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO collection_image(collection, image, position) VALUES(?1, ?2, ?3)").map_err(f)?;
        for (pos, u) in uuids.iter().enumerate() {
            if let Some(&(_, bid)) = to_uuid.get(u) {
                ins.execute(params![sid, bid, pos as f64]).map_err(f)?;
            }
        }
    }
    for (_, id) in children.iter().filter(|(_, id)| !seen.contains(id)) {
        tx.execute("DELETE FROM collection WHERE id = ?1", [id]).map_err(f)?;
    }
    // graineo deleted photos of Apple Photos while this sync was reading: what it read is outdated (the deleted photos
    // would come back as new ones) – nothing is written (the transaction is rolled back), the next sync reads afresh
    if own_changes(&db.path) != generation {
        return Err(OUTDATED.to_string());
    }
    tx.commit().map_err(f)?;
    let p = names_file();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = crate::files::write_atomic(&p, serde_json::to_string(&names).unwrap_or_default().as_bytes());
    if let Some(r) = reshaped {
        set_reshaped(r);
    }
    Ok(report)
}

/// A named face from Apple Photos; box normalized to the upright image, origin top left.
#[derive(Clone, Debug)]
pub struct Face {
    /// identifier of the face in Apple Photos
    pub id: String,
    /// identifier of the photo
    pub photo: String,
    pub rect_list: (f32, f32, f32, f32),
    pub name: String,
}

/// Box from Apple's data: center normalized with origin **bottom** left, size as a fraction of the longer image side
/// (square in pixels). Verified on own photos, including rotated ones (dimensions are already upright).
pub(crate) fn rect_list(cx: f64, cy: f64, size: f64, width: f64, height: f64) -> Option<(f32, f32, f32, f32)> {
    if !(width > 0.0 && height > 0.0 && size > 0.0) {
        return None;
    }
    let px = size * width.max(height);
    let (b, h) = ((px / width).min(1.0), (px / height).min(1.0));
    let x = (cx - b / 2.0).clamp(0.0, 1.0 - b);
    let y = ((1.0 - cy) - h / 2.0).clamp(0.0, 1.0 - h);
    Some((x as f32, y as f32, b as f32, h as f32))
}

/// Read the library's named faces (read-only). Apple's unnamed groups are left out.
pub fn read_faces(photos_library: &Path) -> Result<Vec<Face>, String> {
    let c = open(photos_library)?;
    let f = |e: rusqlite::Error| e.to_string();
    let mut st = c
        .prepare(
            "SELECT f.ZUUID, a.ZUUID, f.ZCENTERX, f.ZCENTERY, f.ZSIZE, a.ZWIDTH, a.ZHEIGHT, coalesce(nullif(trim(p.ZFULLNAME), ''), trim(p.ZDISPLAYNAME))
             FROM ZDETECTEDFACE f JOIN ZASSET a ON a.Z_PK = f.ZASSETFORFACE JOIN ZPERSON p ON p.Z_PK = f.ZPERSONFORFACE
             WHERE a.ZTRASHEDSTATE = 0 AND coalesce(f.ZHIDDEN, 0) = 0 AND f.ZUUID IS NOT NULL AND a.ZUUID IS NOT NULL
               AND coalesce(nullif(trim(p.ZFULLNAME), ''), trim(p.ZDISPLAYNAME), '') <> ''",
        )
        .map_err(f)?;
    let r = st
        .query_map([], |r| {
            let n = |i: usize| -> rusqlite::Result<f64> { Ok(r.get::<_, Option<f64>>(i)?.unwrap_or(0.0)) };
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, n(2)?, n(3)?, n(4)?, n(5)?, n(6)?, r.get::<_, String>(7)?))
        })
        .map_err(f)?
        // a row that cannot be read is an error of the whole read, not a face removed in Apple Photos
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(f)?
        .into_iter()
        .filter_map(|(id, photo, cx, cy, g, b, h, name)| Some(Face { id, photo, rect_list: rect_list(cx, cy, g, b, h)?, name }))
        .collect();
    Ok(r)
}

/// Result of [`match_faces`].
#[derive(Debug, Default)]
pub struct FacesReport {
    pub total: usize,
    pub new: usize,
    /// joined with an already existing box (own detection)
    pub connected: usize,
    pub removed: usize,
    pub people: usize,
    /// faces that would have been removed but were kept: the read looked incomplete ([`too_many_faces_gone`])
    pub held_back: usize,
}

/// A read of Apple's faces that would remove more than half of the faces taken from Apple Photos at once (more than
/// 10) is taken as incomplete (Photos repairing or rebuilding its database): nothing is removed.
pub fn too_many_faces_gone(gone: usize, known: usize) -> bool {
    gone > 10 && gone * 2 > known
}

/// Import or update named faces from Apple Photos into the library. The names become people
/// (existing people with the same name are reused), the photos get the person keyword. What was
/// decided in graineo (`local = 1`) stays; an existing box at the same position is joined instead of duplicated.
pub fn match_faces(db: &crate::db::Db, photos_library: &Path) -> Result<FacesReport, String> {
    import_faces(db, &read_faces(photos_library)?)
}

pub fn import_faces(db: &crate::db::Db, list: &[Face]) -> Result<FacesReport, String> {
    use rusqlite::params;
    use std::collections::HashMap;
    let f = |e: rusqlite::Error| e.to_string();
    let mut report = FacesReport { total: list.len(), ..Default::default() };
    // photo identifier → image
    let images: HashMap<String, i64> = {
        let mut st = db
            .conn
            .prepare("SELECT d.name, b.id FROM image b JOIN file d ON d.id = b.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE s.origin = ?1 AND b.copy IS NULL")
            .map_err(f)?;
        let r = st.query_map([ORIGIN], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(f)?.filter_map(|z| z.ok()).map(|(n, b)| (n.split('.').next().unwrap_or_default().to_string(), b)).collect();
        r
    };
    // people (before the transaction: creates keywords)
    let mut people: HashMap<String, i64> = HashMap::new();
    for g in list.iter().filter(|g| images.contains_key(&g.photo)) {
        if !people.contains_key(&g.name) {
            people.insert(g.name.clone(), crate::people::find_or_create_person(db, &g.name)?);
        }
    }
    report.people = people.len();
    // existing Apple faces: identifier → (id, image, person, local, source); `local` as stored: 1 = decided by the
    // user (stays), 2 = assigned from an unnamed Apple group (a name given in Apple Photos replaces it)
    let existing: HashMap<String, (i64, i64, Option<i64>, i64, String)> = {
        let mut st = db.conn.prepare("SELECT apple_id, id, image, person, local, source FROM face WHERE apple_id IS NOT NULL").map_err(f)?;
        let r = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)?, r.get(5)?)))).map_err(f)?.filter_map(|z| z.ok()).collect();
        r
    };
    let group = crate::apple_groups::LOCAL_APPLE_GROUP;
    // (image, old person, new person) for the keywords
    let mut switch: Vec<(i64, Option<i64>, Option<i64>)> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let now = crate::now();
    // write in blocks: the library is never occupied for long (the UI and other tasks write too)
    let mut tx = db.conn.unchecked_transaction().map_err(f)?;
    let mut im_block = 0usize;
    for g in list {
        if im_block >= 300 {
            tx.commit().map_err(f)?;
            tx = db.conn.unchecked_transaction().map_err(f)?;
            im_block = 0;
        }
        im_block += 1;
        let (Some(&image), Some(&person)) = (images.get(&g.photo), people.get(&g.name)) else { continue };
        seen.insert(&g.id);
        let (x, y, b, h) = g.rect_list;
        match existing.get(&g.id) {
            Some((id, _, old, local, source)) => {
                if source == "apple" {
                    tx.execute("UPDATE face SET image=?2, x=?3, y=?4, w=?5, h=?6 WHERE id=?1", params![id, image, x, y, b, h]).map_err(f)?;
                }
                if *local != 1 && *old != Some(person) {
                    tx.execute("UPDATE face SET person=?2, status='confirmed', local=0, suspect=0, guessed=NULL WHERE id=?1", params![id, person]).map_err(f)?;
                    switch.push((image, *old, Some(person)));
                } else if *local == group {
                    // named in Apple Photos now: an ordinary Apple face
                    tx.execute("UPDATE face SET local=0 WHERE id=?1", [id]).map_err(f)?;
                }
            }
            None => {
                // join a box at the same position (own detection, Lightroom) instead of duplicating it
                let nearest: Option<(i64, Option<i64>, i64)> = {
                    let mut st = tx.prepare_cached("SELECT id, x, y, w, h, person, local FROM face INDEXED BY face_image WHERE image=?1 AND apple_id IS NULL").map_err(f)?;
                    let candidates: Vec<(f32, i64, Option<i64>, i64)> = st
                        .query_map([image], |r| Ok((crate::people::overlap((x, y, b, h), (r.get::<_, f64>(1)? as f32, r.get::<_, f64>(2)? as f32, r.get::<_, f64>(3)? as f32, r.get::<_, f64>(4)? as f32)), r.get(0)?, r.get(5)?, r.get::<_, i64>(6)?)))
                        .map_err(f)?
                        .filter_map(|z| z.ok())
                        .collect();
                    candidates.into_iter().filter(|k| k.0 > 0.45).max_by(|a, b| a.0.total_cmp(&b.0)).map(|k| (k.1, k.2, k.3))
                };
                match nearest {
                    Some((id, old, local)) => {
                        report.connected += 1;
                        tx.execute("UPDATE face SET apple_id=?2 WHERE id=?1", params![id, g.id]).map_err(f)?;
                        // a name already assigned or decided in graineo stays; one assigned from an Apple group gives
                        // way to the name now given in Apple Photos
                        if (old.is_none() && local == 0) || local == group {
                            tx.execute("UPDATE face SET person=?2, status='confirmed', local=0, suspect=0, guessed=NULL WHERE id=?1", params![id, person]).map_err(f)?;
                            if old != Some(person) {
                                switch.push((image, old, Some(person)));
                            }
                        }
                    }
                    None => {
                        report.new += 1;
                        tx.execute(
                            "INSERT INTO face(image, x, y, w, h, source, apple_id, person, status, detected) VALUES(?1,?2,?3,?4,?5,'apple',?6,?7,'confirmed',?8)",
                            params![image, x, y, b, h, g.id, person, now],
                        )
                        .map_err(f)?;
                        switch.push((image, None, Some(person)));
                    }
                }
            }
        }
    }
    // faces removed in Apple Photos or renamed to nameless (without an own decision)
    let removable = existing.iter().filter(|(aid, (_, _, _, local, _))| !seen.contains(aid.as_str()) && *local == 0).count();
    let hold = removable > 0 && (list.is_empty() || too_many_faces_gone(removable, existing.len()));
    if hold {
        report.held_back = removable;
    }
    for (aid, (id, image, old, local, source)) in &existing {
        if hold || seen.contains(aid.as_str()) || *local != 0 {
            continue;
        }
        report.removed += 1;
        if source == "apple" {
            tx.execute("DELETE FROM face WHERE id=?1", [id]).map_err(f)?;
        } else {
            tx.execute("UPDATE face SET apple_id=NULL WHERE id=?1", [id]).map_err(f)?;
            continue;
        }
        if old.is_some() {
            switch.push((*image, *old, None));
        }
    }
    tx.commit().map_err(f)?;
    // person keywords in blocks of one transaction each (individually every change would be a separate write)
    for block in switch.chunks(500) {
        let tx = db.conn.unchecked_transaction().map_err(f)?;
        for (image, old, new) in block {
            crate::people::change_image_person_without_file(db, *image, *old, *new)?;
        }
        tx.commit().map_err(f)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_from_core_data() {
        // 2001-01-01T00:00:00 UTC
        assert_eq!(local_time_of(0.0, 0).as_deref(), Some("2001-01-01T00:00:00"));
        // 2013-04-21 … with +2 h
        assert_eq!(local_time_of(388_256_391.0, 7200).as_deref(), Some("2013-04-21T18:59:51"));
        assert_eq!(local_time_of(f64::NAN, 0), None);
    }

    #[test]
    fn paths() {
        let m = Path::new("/Users/x/Pictures/Photos Library.photoslibrary");
        assert!(in_photos_library(&m.join("originals/1/ABC.heic")));
        assert!(!in_photos_library(Path::new("/Users/x/Pictures/2019/IMG_1.HEIC")));
        assert_eq!(uuid_from_path(&m.join("originals/1/ABC-1.heic")).as_deref(), Some("ABC-1"));
    }

    /// Small library with the layout of macOS 26 (entity numbers as found).
    #[test]
    fn read_with_albums() {
        let dir = std::env::temp_dir().join(format!("graineo-apple-{}.photoslibrary", std::process::id()));
        std::fs::create_dir_all(dir.join("database")).unwrap();
        let c = Connection::open(dir.join("database").join("Photos.sqlite")).unwrap();
        c.execute_batch(
            "CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
             INSERT INTO Z_PRIMARYKEY VALUES (3,'Asset'),(32,'GenericAlbum'),(33,'Album');
             CREATE TABLE ZASSET (Z_PK INTEGER PRIMARY KEY, ZUUID TEXT, ZDIRECTORY TEXT, ZFILENAME TEXT, ZDATECREATED REAL, ZWIDTH INTEGER, ZHEIGHT INTEGER, ZKIND INTEGER, ZFAVORITE INTEGER, ZHIDDEN INTEGER, ZTRASHEDSTATE INTEGER, ZADJUSTMENTTIMESTAMP REAL, ZDURATION REAL);
             CREATE TABLE ZADDITIONALASSETATTRIBUTES (ZASSET INTEGER, ZORIGINALFILENAME TEXT, ZTIMEZONEOFFSET INTEGER, ZORIGINALFILESIZE INTEGER);
             CREATE TABLE ZEXTENDEDATTRIBUTES (ZASSET INTEGER, ZCAMERAMAKE TEXT, ZCAMERAMODEL TEXT, ZLENSMODEL TEXT);
             CREATE TABLE ZGENERICALBUM (Z_PK INTEGER PRIMARY KEY, ZKIND INTEGER, ZTITLE TEXT, ZTRASHEDSTATE INTEGER);
             CREATE TABLE Z_33ASSETS (Z_33ALBUMS INTEGER, Z_3ASSETS INTEGER, Z_FOK_3ASSETS INTEGER);
             INSERT INTO ZASSET VALUES (1,'AAA','A','AAA.heic',0,4032,3024,0,1,0,0,NULL,NULL),(2,'BBB','B','BBB.mov',0,1920,1080,1,0,0,0,NULL,12.5),(3,'CCC','C','CCC.heic',0,1,1,0,0,0,1,NULL,NULL);
             INSERT INTO ZADDITIONALASSETATTRIBUTES VALUES (1,'IMG_0001.HEIC',3600,123);
             INSERT INTO ZEXTENDEDATTRIBUTES VALUES (1,'Apple','iPhone 13 Pro','iPhone 13 Pro back camera');
             INSERT INTO ZGENERICALBUM VALUES (10,2,'Urlaub',0),(11,2,'Weg',1);
             INSERT INTO Z_33ASSETS VALUES (10,1,1),(10,2,2),(10,3,3),(11,1,1);",
        )
        .unwrap();
        drop(c);
        let (a, albums) = read(&dir).unwrap();
        assert_eq!(a.len(), 2, "not the trash");
        let x = &a[0];
        assert_eq!((x.original_name.as_deref(), x.camera.as_deref(), x.favorite, x.video), (Some("IMG_0001.HEIC"), Some("iPhone 13 Pro"), true, false));
        assert_eq!(x.capture.as_deref(), Some("2001-01-01T01:00:00"));
        assert_eq!(x.original(&dir), dir.join("originals/A/AAA.heic"));
        assert!(a[1].video);
        assert_eq!(a[1].duration, Some(12.5));
        assert_eq!(albums, vec![Album { title: "Urlaub".into(), uuids: vec!["AAA".into(), "BBB".into()] }]);

        // into the library: root folder, files, collections; a second run changes nothing, deletion takes effect
        let db = crate::db::Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.total, b.new, b.removed, b.albums), (2, 2, 0, 2), "Urlaub + favorites");
        let n: i64 = db.conn.query_row("SELECT count(*) FROM image", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        let on_vacation: i64 = db.conn.query_row("SELECT count(*) FROM collection_image sb JOIN collection s ON s.id = sb.collection WHERE s.name = 'Urlaub'", [], |r| r.get(0)).unwrap();
        assert_eq!(on_vacation, 2);
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.new, b.removed), (0, 0));
        let photos = Connection::open(dir.join("database").join("Photos.sqlite")).unwrap();
        db.conn.execute("UPDATE image SET stars = 4 WHERE file = (SELECT id FROM file WHERE name = 'BBB.mov')", []).unwrap();
        let state = || -> Option<(i64, i64)> { db.conn.query_row("SELECT d.status, b.stars FROM file d JOIN image b ON b.file = d.id WHERE d.name = 'BBB.mov'", [], |r| Ok((r.get(0)?, r.get(1)?))).ok() };
        let shown = || db.rows().unwrap().iter().filter(|r| r.name == "BBB.mov").count();
        // (a) in "Recently Deleted": parked – not shown, rating kept
        photos.execute("UPDATE ZASSET SET ZTRASHEDSTATE = 1 WHERE ZUUID = 'BBB'", []).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.new, b.removed, b.parked, b.state_changed), (0, 0, 1, 1));
        assert_eq!(state(), Some((PARKED, 4)));
        assert_eq!(shown(), 0, "not in the grid");
        // (a) restored in Apple Photos: back with everything
        photos.execute("UPDATE ZASSET SET ZTRASHEDSTATE = 0 WHERE ZUUID = 'BBB'", []).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.new, b.removed, b.parked, b.state_changed), (0, 0, 0, 1));
        assert_eq!(state(), Some((0, 4)));
        assert_eq!(shown(), 1);
        // (c) hidden in Apple Photos: kept, not shown (as before: hidden photos do not appear), back when shown again
        photos.execute("UPDATE ZASSET SET ZHIDDEN = 1 WHERE ZUUID = 'BBB'", []).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.total, b.removed, b.parked), (1, 0, 1));
        assert_eq!(state(), Some((PARKED, 4)));
        photos.execute("UPDATE ZASSET SET ZHIDDEN = 0", []).unwrap();
        reconcile(&db, &dir).unwrap();
        assert_eq!(state(), Some((0, 4)));
        // (b) gone for good (also out of "Recently Deleted"): removed without asking
        photos.execute("DELETE FROM ZASSET WHERE ZUUID = 'BBB'", []).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.new, b.removed, b.held_back), (0, 1, None));
        assert_eq!(state(), None);
        // (d) empty read (library being rebuilt): error, nothing changed
        let before: i64 = db.conn.query_row("SELECT count(*) FROM file", [], |r| r.get(0)).unwrap();
        photos.execute("UPDATE ZASSET SET ZUUID = NULL", []).unwrap();
        assert!(reconcile(&db, &dir).is_err());
        let after: i64 = db.conn.query_row("SELECT count(*) FROM file", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after);
        photos.execute("UPDATE ZASSET SET ZUUID = 'AAA' WHERE Z_PK = 1", []).unwrap();
        drop(photos);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (d) A read that would remove too many photos at once is held back (nothing changed) until confirmed.
    #[test]
    fn incomplete_read_is_held_back() {
        assert!(!too_many_gone(10, 12));
        assert!(too_many_gone(11, 100));
        assert!(!too_many_gone(11, 1000));
        assert!(too_many_gone(201, 1_000_000));
        let dir = std::env::temp_dir().join(format!("graineo-apple-viele-{}.photoslibrary", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("database")).unwrap();
        let photos = Connection::open(dir.join("database").join("Photos.sqlite")).unwrap();
        photos
            .execute_batch(
                "CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
                 INSERT INTO Z_PRIMARYKEY VALUES (3,'Asset'),(32,'GenericAlbum'),(33,'Album');
                 CREATE TABLE ZASSET (Z_PK INTEGER PRIMARY KEY, ZUUID TEXT, ZDIRECTORY TEXT, ZFILENAME TEXT, ZDATECREATED REAL, ZWIDTH INTEGER, ZHEIGHT INTEGER, ZKIND INTEGER, ZFAVORITE INTEGER, ZHIDDEN INTEGER, ZTRASHEDSTATE INTEGER, ZADJUSTMENTTIMESTAMP REAL, ZDURATION REAL);
                 CREATE TABLE ZADDITIONALASSETATTRIBUTES (ZASSET INTEGER, ZORIGINALFILENAME TEXT, ZTIMEZONEOFFSET INTEGER, ZORIGINALFILESIZE INTEGER);
                 CREATE TABLE ZEXTENDEDATTRIBUTES (ZASSET INTEGER, ZCAMERAMAKE TEXT, ZCAMERAMODEL TEXT, ZLENSMODEL TEXT);
                 CREATE TABLE ZGENERICALBUM (Z_PK INTEGER PRIMARY KEY, ZKIND INTEGER, ZTITLE TEXT, ZTRASHEDSTATE INTEGER);
                 CREATE TABLE Z_33ASSETS (Z_33ALBUMS INTEGER, Z_3ASSETS INTEGER, Z_FOK_3ASSETS INTEGER);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100)
                 INSERT INTO ZASSET SELECT i, 'U' || i, 'A', 'U' || i || '.heic', 0, 10, 10, 0, 0, 0, 0, NULL, NULL FROM n;",
            )
            .unwrap();
        let db = crate::db::Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        assert_eq!(reconcile(&db, &dir).unwrap().new, 100);
        db.conn.execute("UPDATE image SET stars = 3", []).unwrap();
        let rated = || -> i64 { db.conn.query_row("SELECT count(*) FROM image WHERE stars = 3", [], |r| r.get(0)).unwrap() };
        // 20 of 100 gone for good at once: held back, nothing removed
        photos.execute("DELETE FROM ZASSET WHERE Z_PK <= 20", []).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.held_back, b.removed), (Some((20, 100)), 0));
        assert_eq!(rated(), 100);
        // the user confirms: carried out
        let b = reconcile_with(&db, &dir, true).unwrap();
        assert_eq!((b.held_back, b.removed), (None, 20));
        assert_eq!(rated(), 80);
        // 30 of 80 in "Recently Deleted" at once: only parked, nothing removed – no confirmation needed
        photos.execute("UPDATE ZASSET SET ZTRASHEDSTATE = 1 WHERE Z_PK <= 50", []).unwrap();
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.held_back, b.removed, b.parked), (None, 0, 30));
        assert_eq!(rated(), 80);
        // unreadable Photos.sqlite: error, nothing changed
        drop(photos);
        std::fs::write(dir.join("database").join("Photos.sqlite"), b"kaputt").unwrap();
        assert!(reconcile(&db, &dir).is_err());
        assert_eq!(rated(), 80);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn faces_from_apple() {
        let dir = std::env::temp_dir().join(format!("graineo-apple-faces-{}.photoslibrary", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("database")).unwrap();
        let c = Connection::open(dir.join("database").join("Photos.sqlite")).unwrap();
        c.execute_batch(
            "CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
             INSERT INTO Z_PRIMARYKEY VALUES (3,'Asset'),(32,'GenericAlbum'),(33,'Album');
             CREATE TABLE ZASSET (Z_PK INTEGER PRIMARY KEY, ZUUID TEXT, ZDIRECTORY TEXT, ZFILENAME TEXT, ZDATECREATED REAL, ZWIDTH INTEGER, ZHEIGHT INTEGER, ZKIND INTEGER, ZFAVORITE INTEGER, ZHIDDEN INTEGER, ZTRASHEDSTATE INTEGER, ZADJUSTMENTTIMESTAMP REAL, ZDURATION REAL);
             CREATE TABLE ZADDITIONALASSETATTRIBUTES (ZASSET INTEGER, ZORIGINALFILENAME TEXT, ZTIMEZONEOFFSET INTEGER, ZORIGINALFILESIZE INTEGER);
             CREATE TABLE ZEXTENDEDATTRIBUTES (ZASSET INTEGER, ZCAMERAMAKE TEXT, ZCAMERAMODEL TEXT, ZLENSMODEL TEXT);
             CREATE TABLE ZGENERICALBUM (Z_PK INTEGER PRIMARY KEY, ZKIND INTEGER, ZTITLE TEXT, ZTRASHEDSTATE INTEGER);
             CREATE TABLE Z_33ASSETS (Z_33ALBUMS INTEGER, Z_3ASSETS INTEGER, Z_FOK_3ASSETS INTEGER);
             INSERT INTO ZASSET VALUES (1,'AAA','A','AAA.heic',0,4032,3024,0,1,0,0,NULL,NULL);",
        )
        .unwrap();
        drop(c);
        let db = crate::db::Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        reconcile(&db, &dir).unwrap();

        // faces: two named ones on AAA, one of them overlaps a box of the own detection
        let image: i64 = db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id = b.file WHERE d.name = 'AAA.heic'", [], |r| r.get(0)).unwrap();
        db.conn.execute("INSERT INTO face(image, x, y, w, h, source, status) VALUES(?1, 0.60, 0.20, 0.10, 0.133, 'graineo', 'unknown')", [image]).unwrap();
        let mk = |id: &str, r: (f32, f32, f32, f32), name: &str| Face { id: id.into(), photo: "AAA".into(), rect_list: r, name: name.into() };
        let list = vec![mk("F1", (0.1, 0.1, 0.1, 0.133), "Lotte"), mk("F2", (0.6, 0.2, 0.1, 0.133), "Hugo"), Face { photo: "FEHLT".into(), ..mk("F3", (0.1, 0.1, 0.1, 0.1), "X") }];
        let g = import_faces(&db, &list).unwrap();
        assert_eq!((g.new, g.connected, g.people), (1, 1, 2));
        let number = |sql: &str| -> i64 { db.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(number("SELECT count(*) FROM face"), 2, "not duplicated");
        assert_eq!(number("SELECT count(*) FROM face WHERE status='confirmed' AND person IS NOT NULL"), 2);
        let paths = crate::sync::keyword_paths(&db, image).unwrap();
        assert!(paths.iter().any(|p| p.ends_with("Lotte")) && paths.iter().any(|p| p.ends_with("Hugo")), "{paths:?}");
        // corrected in graineo: stays; removed in Apple: goes (only the own Apple face)
        db.conn.execute("UPDATE face SET local=1 WHERE apple_id='F2'", []).unwrap();
        let g = import_faces(&db, &[mk("F2", (0.6, 0.2, 0.1, 0.133), "Jemand")]).unwrap();
        assert_eq!((g.new, g.removed), (0, 1));
        assert_eq!(number("SELECT count(*) FROM face WHERE apple_id='F1'"), 0);
        assert_eq!(db.conn.query_row("SELECT s.name FROM face g JOIN keyword s ON s.id=g.person WHERE g.apple_id='F2'", [], |r| r.get::<_, String>(0)).unwrap(), "Hugo");
        // an empty read (Photos rebuilding its database) removes nothing
        db.conn.execute("UPDATE face SET local=0 WHERE apple_id='F2'", []).unwrap();
        let g = import_faces(&db, &[]).unwrap();
        assert_eq!((g.removed, g.held_back), (0, 1));
        assert_eq!(number("SELECT count(*) FROM face WHERE apple_id='F2'"), 1);
        assert!(too_many_faces_gone(60, 100) && !too_many_faces_gone(40, 100) && !too_many_faces_gone(5, 6));
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn face_rect_from_apple_data() {
        // portrait 768 × 1024: center measured from the bottom, size relative to the longer side
        let (x, y, b, h) = rect_list(0.31, 0.669, 0.296, 768.0, 1024.0).unwrap();
        assert!((b - 0.3947).abs() < 1e-3 && (h - 0.296).abs() < 1e-3);
        assert!((x + b / 2.0 - 0.31).abs() < 1e-3 && (y + h / 2.0 - 0.331).abs() < 1e-3);
        assert!(rect_list(0.5, 0.5, 0.1, 0.0, 100.0).is_none());
    }

    /// Deleting via Apple Photos while a sync is reading (13.11.2026, 0.99.0): the sync had read Apple Photos before
    /// the deletion and wrote after it – the deleted photos came back into the library as new ones (and were only
    /// parked at the next start). Now the outdated sync writes nothing.
    #[test]
    fn deletion_during_sync_is_not_undone() {
        let dir = std::env::temp_dir().join(format!("graineo-apple-race-{}.photoslibrary", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("database")).unwrap();
        let photos = Connection::open(dir.join("database").join("Photos.sqlite")).unwrap();
        photos
            .execute_batch(
                "CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER, Z_NAME TEXT);
                 INSERT INTO Z_PRIMARYKEY VALUES (3,'Asset'),(32,'GenericAlbum'),(33,'Album');
                 CREATE TABLE ZASSET (Z_PK INTEGER PRIMARY KEY, ZUUID TEXT, ZDIRECTORY TEXT, ZFILENAME TEXT, ZDATECREATED REAL, ZWIDTH INTEGER, ZHEIGHT INTEGER, ZKIND INTEGER, ZFAVORITE INTEGER, ZHIDDEN INTEGER, ZTRASHEDSTATE INTEGER, ZADJUSTMENTTIMESTAMP REAL, ZDURATION REAL);
                 CREATE TABLE ZADDITIONALASSETATTRIBUTES (ZASSET INTEGER, ZORIGINALFILENAME TEXT, ZTIMEZONEOFFSET INTEGER, ZORIGINALFILESIZE INTEGER);
                 CREATE TABLE ZEXTENDEDATTRIBUTES (ZASSET INTEGER, ZCAMERAMAKE TEXT, ZCAMERAMODEL TEXT, ZLENSMODEL TEXT);
                 CREATE TABLE ZGENERICALBUM (Z_PK INTEGER PRIMARY KEY, ZKIND INTEGER, ZTITLE TEXT, ZTRASHEDSTATE INTEGER);
                 CREATE TABLE Z_33ASSETS (Z_33ALBUMS INTEGER, Z_3ASSETS INTEGER, Z_FOK_3ASSETS INTEGER);
                 INSERT INTO ZASSET VALUES (1,'AAA','A','AAA.heic',0,10,10,0,0,0,0,NULL,NULL),(2,'BBB','B','BBB.heic',0,10,10,0,0,0,0,NULL,NULL);",
            )
            .unwrap();
        let db = crate::db::Db::open(&dir.join("bibliothek-test.sqlite")).unwrap();
        reconcile(&db, &dir).unwrap();
        let rows = || -> i64 { db.conn.query_row("SELECT count(*) FROM image b JOIN file d ON d.id = b.file WHERE d.name = 'BBB.heic'", [], |r| r.get(0)).unwrap() };
        assert_eq!(rows(), 1);
        let image: i64 = db.conn.query_row("SELECT b.id FROM image b JOIN file d ON d.id = b.file WHERE d.name = 'BBB.heic'", [], |r| r.get(0)).unwrap();
        // the sync has read Apple Photos (BBB still there); meanwhile graineo deletes BBB there and in the library
        let deleting = || {
            note_own_change_in(&db.path);
            photos.execute("UPDATE ZASSET SET ZTRASHEDSTATE = 1 WHERE ZUUID = 'BBB'", []).unwrap();
            note_own_change_in(&db.path);
            db.remove_images(&[image], &[]).unwrap();
            note_own_change_in(&db.path);
        };
        let r = reconcile_inner(&db, &dir, false, &deleting);
        assert_eq!(r, Err(OUTDATED.to_string()), "an outdated sync writes nothing");
        assert_eq!(rows(), 0, "the deleted photo does not come back");
        // the next sync reads afresh: BBB is in "Recently Deleted" and stays out
        let b = reconcile(&db, &dir).unwrap();
        assert_eq!((b.new, b.parked), (0, 0));
        assert_eq!(rows(), 0);
        drop(db);
        drop(photos);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
