//! Backup and rebuild of the library. Since 08.11.2026 the library is the main store (develop settings, ratings,
//! keywords, Lightroom data – sidecars next to the photos only if switched on): the daily backup protects it, and
//! optionally a second copy goes to another target (e.g. the NAS, [`second_target`]). A rebuild takes what the
//! folders and existing sidecars hold; without sidecars the edits are only in the library and its backups.
//!
//! - Backup: one snapshot per day via `VACUUM INTO` (consistent even while in use), without the regenerable search
//!   index, checked (`quick_check`), then packed with zstd (`library-YYYY-MM-DD.sqlite.zst`, since 0.107.0) and the
//!   packed file decoded once more and compared (SHA-256) before it counts. Target freely selectable (e.g. NAS).
//!   Older unpacked backups (`library-YYYY-MM-DD.sqlite`) stay readable and are only removed by the normal rule.
//! - Kept ([`prune`]): the [`KEEP`] newest backups plus exactly one monthly backup ([`monthly_backup`]); a backup
//!   holding at least twice as many photos as the newest is never removed.
//! - The root folder list additionally as a readable file `roots.json` in Application Support (for
//!   rebuilding without a database).
//! - Rebuild into a new file: root folders from `roots.json`, Lightroom catalog (read-only),
//!   sync (sidecars), shooting data, search index.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::db::Db;

/// Daily backups kept (the newest ones).
pub const KEEP: usize = 3;
/// Monthly backups kept in addition (see [`monthly_backup`]).
pub const MONTHLY: usize = 1;
/// zstd level of the packed backups. Measured (one core, 14.11.2026): text and numbers of the library pack to about a
/// sixth, the float vectors (faces, search) hardly at all; level 9 is 2–15 % smaller than level 3 at ~1 s per 80 MB,
/// higher levels gain under 1 % and take three to eight times longer.
const LEVEL: i32 = 9;
/// File ending of packed backups.
const PACKED: &str = ".sqlite.zst";
/// zstd skippable frame (ignored by every zstd decoder) at the start of a packed backup: a small JSON with the photo
/// count and schema, so cleaning up and the list need not unpack 800 MB to know what a backup holds.
const HEADER_MAGIC: u32 = 0x184D_2A50;

/// Default target: `backups/` next to the database.
pub fn default_target(db: &Db) -> PathBuf {
    db.path.parent().unwrap_or(Path::new(".")).join("backups")
}

/// Second target for the daily backup (e.g. the NAS; `info.backup_dir2`): the library holds all edits since 08.11.2026,
/// a copy elsewhere protects them. `None`: not chosen.
pub fn second_target(db: &Db) -> Option<PathBuf> {
    db.info("backup_dir2").filter(|d| !d.trim().is_empty()).map(PathBuf::from)
}

/// Copy a backup into the second target (if chosen and reachable) and clean up there by the same rule ([`prune`]).
fn copy_to_second(db: &Db, backup: &Path) -> Result<(), String> {
    let Some(dir) = second_target(db) else { return Ok(()) };
    if !dir.is_dir() || dir == backup.parent().unwrap_or(Path::new("")) {
        return Ok(());
    }
    let target = dir.join(backup.file_name().ok_or("kein Dateiname")?);
    let tmp = dir.join(format!(".library-sicherung-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(backup, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    drop_superseded(&target);
    prune(&dir);
    Ok(())
}

pub fn target(db: &Db) -> PathBuf {
    db.info("backup_dir").map(PathBuf::from).unwrap_or_else(|| default_target(db))
}

/// Local date "YYYY-MM-DD".
pub fn today() -> String {
    let s = crate::now();
    let (j, m, t) = crate::platform::local_date(s);
    format!("{j:04}-{m:02}-{t:02}")
}

/// Date ("YYYY-MM-DD") of an automatic daily backup: `library-YYYY-MM-DD.sqlite.zst` (packed) or
/// `library-YYYY-MM-DD.sqlite` (unpacked, before 0.107.0). Only those are counted and removed when cleaning up; other
/// files in the folder (e.g. a manually created `library-prior-0.36.0-….sqlite`) stay untouched and do not displace
/// a daily backup.
fn backup_date(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("library-")?;
    let rest = rest.strip_suffix(PACKED).or_else(|| rest.strip_suffix(".sqlite"))?;
    let b = rest.as_bytes();
    (b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })).then_some(rest)
}

fn is_daily_backup(name: &str) -> bool {
    backup_date(name).is_some()
}

fn date_of(p: &Path) -> String {
    p.file_name().and_then(|n| backup_date(&n.to_string_lossy()).map(str::to_string)).unwrap_or_default()
}

/// Is this backup packed (zstd)?
pub fn is_packed(p: &Path) -> bool {
    p.to_string_lossy().ends_with(PACKED)
}

/// Existing daily backups, packed and unpacked (newest first; of the same day the packed one first).
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| is_daily_backup(&n.to_string_lossy()))).collect())
        .unwrap_or_default();
    // "library-D.sqlite.zst" sorts after "library-D.sqlite": reversed, the packed one of a day comes first
    v.sort();
    v.reverse();
    v
}

/// Already backed up today (packed or, from before the update, unpacked)?
pub fn due(db: &Db) -> bool {
    if crate::backup_view::active().is_some() {
        return false;
    }
    let (dir, d) = (target(db), today());
    !dir.join(format!("library-{d}{PACKED}")).exists() && !dir.join(format!("library-{d}.sqlite")).exists()
}

/// The JSON at the start of a packed backup (photos, schema, size unpacked), `None` if missing or unreadable.
pub fn packed_header(p: &Path) -> Option<serde_json::Value> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut head = [0u8; 8];
    f.read_exact(&mut head).ok()?;
    if u32::from_le_bytes(head[..4].try_into().ok()?) != HEADER_MAGIC {
        return None;
    }
    let n = u32::from_le_bytes(head[4..].try_into().ok()?) as usize;
    if n > 65536 {
        return None;
    }
    let mut b = vec![0u8; n];
    f.read_exact(&mut b).ok()?;
    serde_json::from_slice(&b).ok()
}

/// Number of images in a backup file (read only), `None` if it cannot be read. Packed: from its header.
fn image_count(p: &Path) -> Option<i64> {
    if is_packed(p) {
        return packed_header(p)?["images"].as_i64();
    }
    let c = rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    count_images(&c).ok()
}

/// Number of images in a library file of any schema (backups made before schema 20 keep their old table names).
fn count_images(c: &rusqlite::Connection) -> rusqlite::Result<i64> {
    let schema: i32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    c.query_row(legacy_names::sql::image_count_sql(schema), [], |r| r.get(0))
}

/// Reader that hashes (SHA-256) and counts what passes through.
struct Hashing<R> {
    inner: R,
    sum: sha2::Sha256,
    len: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let n = self.inner.read(buf)?;
        self.sum.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }
}

impl<R> Hashing<R> {
    fn new(inner: R) -> Self {
        use sha2::Digest;
        Hashing { inner, sum: sha2::Sha256::new(), len: 0 }
    }
    fn finish(self) -> (Vec<u8>, u64) {
        use sha2::Digest;
        (self.sum.finalize().to_vec(), self.len)
    }
}

/// Writer that only hashes (for checking a packed file without writing it out).
struct HashSink(sha2::Sha256, u64);

impl Write for HashSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        self.0.update(buf);
        self.1 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Pack the SQLite file `raw` into `out` (header frame + one zstd frame with checksum) and check it: the written
/// file is decoded again and must give the same bytes (SHA-256, length). Single-threaded.
fn pack(raw: &Path, out: &Path, header: &serde_json::Value) -> Result<(), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let len = std::fs::metadata(raw).map_err(|e| err(raw, e))?.len();
    let (sum, n) = {
        let f = std::fs::File::create(out).map_err(|e| err(out, e))?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        let h = header.to_string();
        w.write_all(&HEADER_MAGIC.to_le_bytes()).and_then(|_| w.write_all(&(h.len() as u32).to_le_bytes())).and_then(|_| w.write_all(h.as_bytes())).map_err(|e| err(out, e))?;
        let mut enc = zstd::stream::Encoder::new(w, LEVEL).map_err(|e| err(out, e))?;
        enc.include_checksum(true).map_err(|e| err(out, e))?;
        enc.set_pledged_src_size(Some(len)).map_err(|e| err(out, e))?;
        let mut src = Hashing::new(std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(raw).map_err(|e| err(raw, e))?));
        std::io::copy(&mut src, &mut enc).map_err(|e| err(out, e))?;
        let w = enc.finish().map_err(|e| err(out, e))?;
        let f = w.into_inner().map_err(|e| err(out, e.into_error()))?;
        f.sync_all().map_err(|e| err(out, e))?;
        src.finish()
    };
    // check: decode what landed on the drive
    use sha2::Digest;
    let mut sink = HashSink(sha2::Sha256::new(), 0);
    let f = std::fs::File::open(out).map_err(|e| err(out, e))?;
    zstd::stream::copy_decode(std::io::BufReader::with_capacity(1 << 20, f), &mut sink).map_err(|e| format!("Sicherung nach dem Packen unlesbar ({}): {e}", out.display()))?;
    if sink.1 != n || sink.0.finalize().to_vec() != sum {
        return Err(format!("Sicherung nach dem Packen verändert ({})", out.display()));
    }
    Ok(())
}

/// Unpack a packed backup into `out` (overwritten).
pub fn unpack(p: &Path, out: &Path) -> Result<(), String> {
    let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let o = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut w = std::io::BufWriter::with_capacity(1 << 20, o);
    let r = zstd::stream::copy_decode(std::io::BufReader::with_capacity(1 << 20, f), &mut w).and_then(|_| w.flush());
    if let Err(e) = r {
        drop(w);
        let _ = std::fs::remove_file(out);
        return Err(format!("Sicherung nicht entpackbar ({}): {e}", p.display()));
    }
    Ok(())
}

/// Put the backup `p` as a plain SQLite file at `out`: unpack a packed one, copy an unpacked one.
pub(crate) fn materialize(p: &Path, out: &Path) -> Result<(), String> {
    if is_packed(p) {
        unpack(p, out)
    } else {
        std::fs::copy(p, out).map(|_| ()).map_err(|e| format!("{}: {e}", out.display()))
    }
}

/// The monthly backup kept besides the [`KEEP`] newest: of the backups older than those, the oldest one of the newest
/// calendar month among them. In practice the first backup of the current month; until it has left the daily backups
/// (the first days of a month), the first backup of the previous month stays. So it is between about 3 and 33 days old,
/// and there is always exactly one (once a backup older than the daily ones exists).
fn monthly_backup(all: &[PathBuf]) -> Option<&PathBuf> {
    let month = all.iter().skip(KEEP).map(|p| date_of(p)).filter(|d| d.len() == 10).map(|d| d[..7].to_string()).max()?;
    // newest first: the last one of that month is its oldest
    all.iter().skip(KEEP).filter(|p| date_of(p).starts_with(&month)).last()
}

/// Remove daily backups beyond the [`KEEP`] newest and the monthly one ([`monthly_backup`]) in `dir` – but never one
/// that holds clearly more photos than the newest (at least twice as many): a library that was emptied or lost most of
/// its rows (deleted file → new empty library, a mistaken mass removal) would otherwise push the last good backups out
/// within [`KEEP`] days.
fn prune(dir: &Path) {
    let all = list(dir);
    let newest = all.first().and_then(|p| image_count(p)).unwrap_or(0);
    let monthly = monthly_backup(&all).cloned();
    for old in all.into_iter().skip(KEEP) {
        if monthly.as_ref() == Some(&old) {
            continue;
        }
        if let Some(n) = image_count(&old).filter(|&n| n > 0 && n >= newest.saturating_mul(2)) {
            eprintln!("backup kept (holds {n} photos, the newest {newest}): {}", old.display());
            continue;
        }
        let _ = std::fs::remove_file(old);
    }
}

/// A packed backup replaces the unpacked one of the same day (written before the update, or by an earlier
/// "Jetzt sichern" of the same day) – as a new backup of the day always replaced the earlier one.
fn drop_superseded(packed: &Path) {
    let name = packed.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if let Some(stem) = name.strip_suffix(".zst").filter(|_| is_packed(packed)) {
        let _ = std::fs::remove_file(packed.with_file_name(stem));
    }
}

/// Write a snapshot, pack it, clean up (see [`prune`]). Returns: file, size (packed). The snapshot is checked
/// (`quick_check`) and the packed file decoded and compared before it replaces today's backup or displaces older
/// ones. Runs quietly: the calling thread goes to the background (efficiency cores, throttled disk), one core.
pub fn create_backup(db: &Db) -> Result<(PathBuf, u64), String> {
    // viewing a backup: its copy is never backed up (and the real backups are not cleaned up from there)
    crate::backup_view::refuse()?;
    crate::platform::background_priority();
    let dir = target(db);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let target = dir.join(format!("library-{}{PACKED}", today()));
    write_packed(db, &dir, &target)?;
    drop_superseded(&target);
    prune(&dir);
    let n = std::fs::metadata(&target).map_or(0, |m| m.len());
    if let Err(e) = copy_to_second(db, &target) {
        eprintln!("second backup target: {e}");
    }
    // the learned camera looks live next to the library, not in it: copied along (small, only new or changed files)
    let looks = db.path.parent().unwrap_or(Path::new(".")).join(LOOKS_DIR);
    if let Err(e) = mirror(&looks, &dir.join(LOOKS_DIR)) {
        eprintln!("backup of learned looks: {e}");
    }
    if let Some(second) = second_target(db) {
        if let Err(e) = mirror(&looks, &second.join(LOOKS_DIR)) {
            eprintln!("second backup target, learned looks: {e}");
        }
    }
    Ok((target, n))
}

/// Folder of the learned camera looks next to the library (see `rawdev::learn`), copied with the daily backup.
pub const LOOKS_DIR: &str = "looks";

/// Copy the files of `from` (recursively) to `to` where missing or different in size or newer; never deletes in `to`
/// (a reset look stays in the backup). Missing `from`: nothing to do.
pub fn mirror(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Ok(());
    }
    let rd = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') || name.to_string_lossy().contains(".tmp") {
            continue;
        }
        let target = to.join(&name);
        if p.is_dir() {
            mirror(&p, &target)?;
            continue;
        }
        let (a, b) = (std::fs::metadata(&p).ok(), std::fs::metadata(&target).ok());
        let same = match (&a, &b) {
            (Some(a), Some(b)) => a.len() == b.len() && a.modified().ok() <= b.modified().ok(),
            _ => false,
        };
        if !same {
            std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
            let tmp = to.join(format!(".{}.tmp", name.to_string_lossy()));
            std::fs::copy(&p, &tmp).and_then(|_| std::fs::rename(&tmp, &target)).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
}

/// Folder (below the backup target) of the backups made before large actions.
pub const ACTION_DIR: &str = "before-action";
/// Backups before an action of one kind older than this are removed (the newest of each kind always stays).
const ACTION_KEEP_SECS: u64 = 7 * 86_400;

/// Backup before an action that changes many photos at once (merging, deleting keywords, applying suggestions):
/// its own file `before-action/library-before-<kind>-YYYY-MM-DD-HHMMSS.sqlite.zst` below the backup target. It never
/// replaces the daily backup (a second action of the day would otherwise overwrite the state before the first) and
/// is never counted or removed with the daily ones. Backups of the same kind older than 7 days are removed, the
/// newest of each kind always stays. Checked and packed like the daily backup.
pub fn create_action_backup(db: &Db, kind: &str) -> Result<(PathBuf, u64), String> {
    crate::backup_view::refuse()?;
    crate::platform::background_priority();
    let kind: String = kind.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let dir = target(db).join(ACTION_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let t = crate::platform::local_time(crate::now());
    let stamp = format!("{:04}-{:02}-{:02}-{:02}{:02}{:02}", t.year, t.month, t.tag, t.hour, t.minute, t.second);
    // never over an existing file (two actions within one second)
    let mut target = dir.join(format!("library-before-{kind}-{stamp}{PACKED}"));
    let mut nr = 1;
    while target.exists() {
        nr += 1;
        target = dir.join(format!("library-before-{kind}-{stamp}-{nr}{PACKED}"));
    }
    write_packed(db, &dir, &target)?;
    prune_actions(&dir, &kind, &target);
    let n = std::fs::metadata(&target).map_or(0, |m| m.len());
    Ok((target, n))
}

/// Remove backups before actions of `kind` older than [`ACTION_KEEP_SECS`]; `newest` (just written) always stays.
fn prune_actions(dir: &Path, kind: &str, newest: &Path) {
    let prefix = format!("library-before-{kind}-");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for p in rd.flatten().map(|e| e.path()) {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // only this kind: "library-before-merge-2026-…" (a kind continuing with a letter is another kind)
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        if !rest.starts_with(|c: char| c.is_ascii_digit()) || !name.ends_with(PACKED) || p == newest {
            continue;
        }
        let old = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|a| a.as_secs() > ACTION_KEEP_SECS);
        if old {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Write a checked, packed snapshot of the library to `target` (in `dir`): via a temporary file, renamed at the end.
fn write_packed(db: &Db, dir: &Path, target: &Path) -> Result<(), String> {
    // the snapshot next to the library (local), only the packed file goes to the target (possibly the NAS)
    // own names per run: the daily backup and "Jetzt sichern" may meet
    static NR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!("{}-{}", std::process::id(), NR.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let raw = db.path.with_file_name(format!(".library-sicherung-{id}.sqlite.tmp"));
    let tmp = dir.join(format!(".library-sicherung-{id}.tmp"));
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(&tmp);
    let r = (|| -> Result<(), String> {
        db.conn.execute("VACUUM INTO ?1", [raw.to_string_lossy()]).map_err(|e| e.to_string())?;
        let (images, schema) = {
            // the search index can be regenerated – do not back it up (saves ~140 MB)
            let c = rusqlite::Connection::open(&raw).map_err(|e| e.to_string())?;
            c.execute_batch("DROP TABLE IF EXISTS search_vocab; DROP TABLE IF EXISTS search_word; DROP TABLE IF EXISTS search_trigram; DELETE FROM info WHERE name='search_index'; VACUUM;").map_err(|e| e.to_string())?;
            let ok: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            if ok != "ok" {
                return Err(format!("Sicherung fehlerhaft ({ok}) – ältere Sicherungen bleiben"));
            }
            let images: i64 = c.query_row("SELECT count(*) FROM image", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            let schema: i32 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            (images, schema)
        };
        let size = std::fs::metadata(&raw).map_or(0, |m| m.len());
        pack(&raw, &tmp, &json!({"app": crate::APP, "images": images, "schema": schema, "size": size}))?;
        std::fs::rename(&tmp, target).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&raw);
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// What a backup holds (checked before restoring it).
#[derive(Clone, Debug, PartialEq)]
pub struct BackupInfo {
    pub images: i64,
    pub schema: i32,
}

/// Check a backup before restoring: readable, intact (`quick_check`), a graineo library this version can open
/// (schema not newer, or newer but marked compatible). Read only; a packed backup is unpacked into a temporary file
/// for the check (removed afterwards).
pub fn check_backup(p: &Path) -> Result<BackupInfo, String> {
    if !is_packed(p) {
        return check_backup_file(p);
    }
    static NR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nr = NR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("graineo-sicherung-pruefen-{}-{nr}.sqlite", std::process::id()));
    let r = unpack(p, &tmp).and_then(|_| check_backup_file(&tmp));
    let _ = std::fs::remove_file(&tmp);
    r
}

/// [`check_backup`] of a plain SQLite file.
pub(crate) fn check_backup_file(p: &Path) -> Result<BackupInfo, String> {
    let c = rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| format!("{}: {e}", p.display()))?;
    let ok: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(|e| format!("{}: {e}", p.display()))?;
    if ok != "ok" {
        return Err(format!("Sicherung beschädigt ({ok})"));
    }
    let schema: i32 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if schema < 1 {
        return Err("keine Bibliothek von graineo".into());
    }
    if schema > crate::db::VERSION {
        let compatible = legacy_names::sql::compatible_marker(&c, schema);
        if compatible.is_none_or(|k| k > crate::db::VERSION) {
            return Err(format!("Sicherung stammt aus einer neueren Version (Schema {schema})"));
        }
    }
    let images = count_images(&c).map_err(|e| format!("keine Bibliothek von graineo: {e}"))?;
    Ok(BackupInfo { images, schema })
}

/// Marker next to the library: restore this backup at the next start (before anything opens the library).
fn restore_marker(db_path: &Path) -> PathBuf {
    db_path.with_file_name("restore-pending.json")
}

/// Restore `backup` at the next start of the app (the running app has the library open in many places; swapping the
/// file under them would lose their writes). Checks the backup first. Nothing is removed: the current library is kept
/// as `library-before-restore-<time>.sqlite` next to it.
pub fn schedule_restore(db_path: &Path, backup: &Path) -> Result<BackupInfo, String> {
    let info = check_backup(backup)?;
    let v = json!({"backup": backup.to_string_lossy(), "images": info.images});
    crate::files::write_atomic(&restore_marker(db_path), v.to_string().as_bytes()).map_err(|e| e.to_string())?;
    Ok(info)
}

/// Backup waiting to be restored at the next start, if any.
pub fn pending_restore(db_path: &Path) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(restore_marker(db_path)).ok()?).ok()?;
    v["backup"].as_str().map(PathBuf::from)
}

/// Do not restore after all (removes the marker).
pub fn cancel_restore(db_path: &Path) {
    let _ = std::fs::remove_file(restore_marker(db_path));
}

/// A restore carried out at startup.
#[derive(Clone, Debug)]
pub struct Restored {
    pub backup: PathBuf,
    pub images: i64,
    /// the library before the restore (kept), `None` if there was none
    pub kept: Option<PathBuf>,
}

/// Result of a restore at startup (for the notice in the library).
static RESTORED: std::sync::Mutex<Option<Result<Restored, String>>> = std::sync::Mutex::new(None);

/// Result of the restore carried out at this start (once).
pub fn take_restore_message() -> Option<Result<Restored, String>> {
    RESTORED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// At startup, before anything opens the library: carry out a restore requested with [`schedule_restore`]. The
/// current library (with `-wal`/`-shm`) is renamed to `library-before-restore-<time>.sqlite`, the backup is unpacked
/// (or, unpacked from before 0.107.0, copied) in its place (via a side file, checked, then renamed). On an error the current library stays (or is put back) and the marker is
/// removed so the app does not try again at every start. Returns the message (also kept for
/// [`take_restore_message`]); `None`: nothing to restore.
pub fn apply_pending_restore(db_path: &Path) -> Option<Result<Restored, String>> {
    let marker = restore_marker(db_path);
    if !marker.exists() {
        return None;
    }
    let backup = pending_restore(db_path);
    let _ = std::fs::remove_file(&marker);
    let r = (|| -> Result<Restored, String> {
        let backup = backup.ok_or("Wiederherstellung: Angabe der Sicherung unlesbar")?;
        let name = db_path.file_name().ok_or("Bibliothek ohne Dateiname")?.to_string_lossy().into_owned();
        let part = db_path.with_file_name(format!("{name}.wiederherstellen"));
        let _ = std::fs::remove_file(&part);
        // unpack (or copy) first, then check exactly the file that takes the library's place
        materialize(&backup, &part)?;
        let info = match check_backup_file(&part) {
            Ok(i) => i,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        let kept = db_path.with_file_name(format!("library-before-restore-{}.sqlite", crate::now()));
        let with = |p: &Path, ext: &str| PathBuf::from(format!("{}{ext}", p.display()));
        let mut moved: Vec<&str> = Vec::new();
        for ext in ["", "-wal", "-shm"] {
            let a = with(db_path, ext);
            if a.exists() {
                if let Err(e) = std::fs::rename(&a, with(&kept, ext)) {
                    // put back what was already moved: the library stays as it was
                    for x in &moved {
                        let _ = std::fs::rename(with(&kept, x), with(db_path, x));
                    }
                    let _ = std::fs::remove_file(&part);
                    return Err(format!("{}: {e}", a.display()));
                }
                moved.push(ext);
            }
        }
        if let Err(e) = std::fs::rename(&part, db_path) {
            for x in &moved {
                let _ = std::fs::rename(with(&kept, x), with(db_path, x));
            }
            let _ = std::fs::remove_file(&part);
            return Err(format!("{}: {e}", db_path.display()));
        }
        Ok(Restored { backup, images: info.images, kept: (!moved.is_empty()).then_some(kept) })
    })();
    eprintln!("restore: {r:?}");
    *RESTORED.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
    Some(r)
}

/// `roots.json` next to the database.
pub fn roots_file(db_path: &Path) -> PathBuf {
    db_path.parent().unwrap_or(Path::new(".")).join("roots.json")
}

/// Root folder list as a readable file (for rebuilding without a database).
pub fn write_roots(db: &Db) -> Result<(), String> {
    let s = db.roots().map_err(|e| e.to_string())?;
    let v = json!({
        "app": crate::APP,
        "version": 1,
        "lightroomCatalog": db.info("lightroom_catalog"),
        "roots": s.iter().map(|r| json!({"path": r.path, "name": r.name, "origin": match r.origin.as_str() { "own" => "own", crate::apple_photos::ORIGIN => "apple-photos", _ => "lightroom" }})).collect::<Vec<_>>(),
    });
    let p = roots_file(&db.path);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    crate::files::write_atomic(&p, serde_json::to_string_pretty(&v).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())
}

/// Build a new library from the sources. Returns: report (JSON).
/// `roots_path`: `roots.json` of the previous library; `default_catalog`: Lightroom catalog, if none is listed there.
///
/// Nothing is removed before it is certain that the description is usable: the rebuild is created in
/// a side file and replaces `target` only at the end. An existing database is only replaced if it is itself
/// an earlier rebuild (name contains "rebuild"); the running library stays untouched.
pub fn rebuild(target: &Path, roots_path: &Path, default_catalog: Option<PathBuf>, progress: &dyn Fn(&str)) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(roots_path).map_err(|e| format!("Stammordner-Beschreibung nicht lesbar ({}): {e}", roots_path.display()))?;
    let w: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Stammordner-Beschreibung ungültig: {e}"))?;
    if !w["roots"].is_array() {
        return Err("Stammordner-Beschreibung enthält keine Liste der Stammordner".into());
    }
    let name = target.file_name().ok_or("Ziel ohne Dateiname")?.to_string_lossy().into_owned();
    let replaceable = name.contains("rebuild");
    if std::fs::symlink_metadata(target).is_ok() && !replaceable {
        return Err(format!("Ziel {} existiert bereits und wird nicht ersetzt", target.display()));
    }
    let part = target.with_file_name(format!("{name}.teil"));
    let prune = |p: &Path| {
        for extension_of in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(PathBuf::from(format!("{}{extension_of}", p.display())));
        }
    };
    prune(&part);
    let report = {
        let db = Db::open(&part).map_err(|e| e.to_string())?;
        let r = build(&db, &w, default_catalog, progress).and_then(|mut report| {
            // the rasters of bitmap masks and removed objects exist only in the library: take them from the previous
            // one (next to `roots.json`), the sidecars only name them
            let previous = roots_path.with_file_name("library.sqlite");
            if previous.exists() && previous != target && previous != part {
                progress("Masken-Raster übernehmen …");
                // a damaged previous library (the reason for many rebuilds) does not stop the rebuild
                report["rasters"] = match copy_rasters(&db, &previous) {
                    Ok(n) => json!(n),
                    Err(e) => json!({ "error": e }),
                };
            }
            Ok(report)
        });
        drop(db);
        r
    };
    let report = match report {
        Ok(b) => b,
        Err(e) => {
            prune(&part);
            return Err(e);
        }
    };
    if replaceable {
        prune(target);
    }
    std::fs::rename(&part, target).map_err(|e| e.to_string())?;
    Ok(report)
}

/// Copy every raster of the library at `previous` (opened read only) into the rebuilt one: also those of photos
/// outside the library, whose sidecars name them. Returns: copied.
fn copy_rasters(db: &Db, previous: &Path) -> Result<usize, String> {
    let src = rusqlite::Connection::open_with_flags(previous, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("{}: {e}", previous.display()))?;
    let ids: std::collections::HashSet<String> = match src.prepare("SELECT id FROM mask_raster") {
        Ok(mut st) => st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?.collect::<Result<_, _>>().map_err(|e| e.to_string())?,
        Err(_) => return Ok(0), // older library without the table
    };
    crate::mask_raster::copy_from(&src, &db.conn, &ids)
}

fn build(db: &Db, w: &serde_json::Value, default_catalog: Option<PathBuf>, progress: &dyn Fn(&str)) -> Result<serde_json::Value, String> {
    let mut report = json!({});
    let catalog = w["lightroomCatalog"].as_str().map(PathBuf::from).or(default_catalog);
    if let Some(catalog) = catalog.filter(|k| k.exists()) {
        progress("Lightroom-Katalog lesen …");
        let b = crate::lightroom::import(db, &catalog, progress)?;
        report["import"] = b.json();
    }
    for r in w["roots"].as_array().into_iter().flatten() {
        if r["origin"] == "own" {
            if let Some(p) = r["path"].as_str() {
                let p = p.trim_end_matches('/');
                let name = Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let id = db.create_root(&format!("{p}/"), &name, "own", None).map_err(|e| e.to_string())?;
                db.create_folder(id, "", None).map_err(|e| e.to_string())?;
            }
        }
    }
    progress("Abgleich mit den Begleitdateien …");
    let a = crate::reconcile::reconcile(db, None, progress)?;
    report["sync"] = a.json();
    // every existing sidecar once (also those of the photos from the catalog): the edits come back from them
    progress("Begleitdateien lesen …");
    let mut read = crate::sidecar_import::Report::default();
    loop {
        let r = crate::sidecar_import::import_unread(db, 2000, &|| {})?;
        if r.files == 0 {
            break;
        }
        read.files += r.files;
        read.sidecars += r.sidecars;
        read.taken += r.taken;
        read.develop += r.develop;
        read.lightroom += r.lightroom;
    }
    report["sidecars"] = read.json();
    progress("Aufnahmedaten …");
    let _ = crate::exif::backfill(db, 1_000_000, &std::sync::atomic::AtomicBool::new(false));
    progress("Texte in den Dateien …");
    let _ = crate::textmeta::backfill(db, 1_000_000, &std::sync::atomic::AtomicBool::new(false));
    progress("Ortsnamen …");
    let _ = crate::places::backfill(db, 1_000_000, &std::sync::atomic::AtomicBool::new(false), &|_| {});
    progress("Suchindex …");
    let (n, ms) = crate::search::build_index(db)?;
    report["index"] = json!({"images": n, "ms": ms});
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-sich-{}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn mirror_copies_new_and_changed_files_and_never_deletes() {
        let d = tmp();
        let (from, to) = (d.join("looks"), d.join("backup").join("looks"));
        std::fs::create_dir_all(from.join("gr3")).unwrap();
        std::fs::write(from.join("learned.json"), b"{}").unwrap();
        std::fs::write(from.join("gr3").join("standard.x3look"), b"one").unwrap();
        mirror(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("gr3").join("standard.x3look")).unwrap(), b"one");
        std::fs::write(from.join("gr3").join("standard.x3look"), b"second").unwrap();
        std::fs::write(to.join("old.x3look"), b"kept").unwrap();
        mirror(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("gr3").join("standard.x3look")).unwrap(), b"second");
        assert!(to.join("old.x3look").exists());
        assert!(mirror(&d.join("missing"), &to).is_ok());
    }

    /// Rebuilding takes every raster of the previous library (sidecars only name them); a previous library without
    /// the table copies nothing.
    #[test]
    fn rebuild_takes_the_rasters_of_the_previous_library() {
        let d = tmp();
        let previous = d.join("library.sqlite");
        let id = {
            let old = Db::open(&previous).unwrap();
            crate::mask_raster::put_rgb_in(&old.conn, 2, 1, &[1, 2, 3, 4, 5, 6], &json!({"file": "x.jpg", "keep": true})).unwrap()
        };
        let new = Db::open(&d.join("library-rebuild.sqlite.teil")).unwrap();
        assert_eq!(copy_rasters(&new, &previous).unwrap(), 1);
        assert_eq!(crate::mask_raster::get_rgb_in(&new.conn, &id), Some((2, 1, vec![1, 2, 3, 4, 5, 6])));
        let bare = d.join("bare.sqlite");
        rusqlite::Connection::open(&bare).unwrap().execute_batch("CREATE TABLE t(x)").unwrap();
        assert_eq!(copy_rasters(&new, &bare).unwrap(), 0);
        drop(new);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn unusable_description_changes_nothing() {
        let d = tmp();
        let target = d.join("library-rebuild.sqlite");
        std::fs::write(&target, b"vorhandener Neuaufbau").unwrap();
        // roots.json missing
        assert!(rebuild(&target, &d.join("roots.json"), None, &|_| {}).is_err());
        // roots.json without root folder list
        std::fs::write(d.join("roots.json"), b"{}").unwrap();
        assert!(rebuild(&target, &d.join("roots.json"), None, &|_| {}).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"vorhandener Neuaufbau");
        assert!(!d.join("library-rebuild.sqlite.teil").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn running_library_is_never_replaced() {
        let d = tmp();
        let live = d.join("library.sqlite");
        std::fs::write(&live, b"echte Bibliothek").unwrap();
        std::fs::write(d.join("roots.json"), br#"{"roots": []}"#).unwrap();
        let e = rebuild(&live, &d.join("roots.json"), None, &|_| {}).unwrap_err();
        assert!(e.contains("existiert bereits"), "{e}");
        assert_eq!(std::fs::read(&live).unwrap(), b"echte Bibliothek");
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Backup and restore end to end (test library): back up, change and remove photos, restore at the "next start",
    /// everything is back and the changed library is kept next to it.
    #[test]
    fn backup_and_restore_end_to_end() {
        let d = tmp();
        let lib = d.join("library.sqlite");
        let image = {
            let db = Db::open(&lib).unwrap();
            let root = db.create_root("/tmp/graineo-restore-test/", "test", "own", None).unwrap();
            let folder = db.create_folder(root, "", None).unwrap();
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'Ä bild.jpg', 'JPG')", [folder]).unwrap();
            let file = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file, stars) VALUES(?1, 4)", [file]).unwrap();
            let image = db.conn.last_insert_rowid();
            let (p, n) = create_backup(&db).unwrap();
            assert!(p.starts_with(d.join("backups")) && n > 0);
            assert_eq!(check_backup(&p).unwrap().images, 1);
            // after the backup: rating changed, photo removed
            db.set_rating(&[image], Some(1), None, None).unwrap();
            db.remove_images(&[image], &[]).unwrap();
            assert_eq!(db.image_count(), 0);
            let info = schedule_restore(&lib, &p).unwrap();
            assert_eq!(info.images, 1);
            assert_eq!(pending_restore(&lib).as_deref(), Some(p.as_path()));
            image
        };
        // next start
        let done = apply_pending_restore(&lib).unwrap().unwrap();
        assert_eq!(done.images, 1);
        assert!(done.kept.as_ref().is_some_and(|k| k.exists()));
        assert!(apply_pending_restore(&lib).is_none(), "only once");
        let db = Db::open(&lib).unwrap();
        assert_eq!(db.conn.query_row("SELECT stars FROM image WHERE id=?1", [image], |r| r.get::<_, i64>(0)).unwrap(), 4);
        assert_eq!(db.file_path(db.file_of(image).unwrap()).unwrap(), PathBuf::from("/tmp/graineo-restore-test/Ä bild.jpg"));
        // the search index (not in the backup) is there again as empty tables
        assert!(db.conn.query_row("SELECT count(*) FROM search_word", [], |r| r.get::<_, i64>(0)).is_ok());
        let kept: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("library-before-restore-")).collect();
        assert!(!kept.is_empty(), "the library before the restore is kept");
        drop(db);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Regression (review 1.2.0, M2): the backup before a large action has its own name and never replaces the daily
    /// backup of the day; two actions give two backups; older ones of the same kind go after 7 days, the newest stays.
    #[test]
    fn action_backup_never_replaces_the_daily_one() {
        let d = tmp();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        db.conn.execute_batch("INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own'); INSERT INTO folder(id, root, path) VALUES(1, 1, ''); INSERT INTO file(id, folder, name, format) VALUES(10, 1, 'a.jpg', 'JPG'); INSERT INTO image(id, file) VALUES(20, 10);").unwrap();
        let (daily, _) = create_backup(&db).unwrap();
        let before = std::fs::read(&daily).unwrap();
        db.conn.execute("INSERT INTO file(id, folder, name, format) VALUES(11, 1, 'b.jpg', 'JPG')", []).unwrap();
        db.conn.execute("INSERT INTO image(id, file) VALUES(21, 11)", []).unwrap();
        let (a, _) = create_action_backup(&db, "merge").unwrap();
        let (b, _) = create_action_backup(&db, "merge").unwrap();
        assert_ne!(a, b, "one backup per action");
        assert!(a.starts_with(d.join("backups").join(ACTION_DIR)), "{}", a.display());
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("library-before-merge-"));
        assert_eq!(std::fs::read(&daily).unwrap(), before, "the daily backup stays as it was");
        assert_eq!(list(&d.join("backups")), vec![daily.clone()], "not counted as a daily backup");
        assert_eq!(check_backup(&a).unwrap().images, 2);
        // older than 7 days: removed with the next backup of the same kind, another kind stays
        let old = |p: &Path| std::fs::File::options().write(true).open(p).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400)).unwrap();
        old(&a);
        let (other, _) = create_action_backup(&db, "delete-keywords").unwrap();
        old(&other);
        let (c, _) = create_action_backup(&db, "merge").unwrap();
        assert!(!a.exists() && b.exists() && c.exists() && other.exists());
        // only the backups older than 7 days go
        old(&c);
        old(&b);
        let (e, _) = create_action_backup(&db, "merge").unwrap();
        assert!(e.exists() && !b.exists() && !c.exists());
        drop(db);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// A damaged or foreign backup is refused; the library stays untouched.
    #[test]
    fn restore_refuses_bad_backups() {
        let d = tmp();
        let lib = d.join("library.sqlite");
        drop(Db::open(&lib).unwrap());
        let junk = d.join("library-2026-01-01.sqlite");
        std::fs::write(&junk, b"kein SQLite").unwrap();
        assert!(schedule_restore(&lib, &junk).is_err());
        let other = d.join("other.sqlite");
        rusqlite::Connection::open(&other).unwrap().execute_batch("CREATE TABLE x(a)").unwrap();
        assert!(check_backup(&other).is_err(), "no graineo library");
        let newer = d.join("newer.sqlite");
        {
            let c = rusqlite::Connection::open(&newer).unwrap();
            c.execute_batch(&format!("CREATE TABLE image(id); CREATE TABLE info(name TEXT PRIMARY KEY, value TEXT); INSERT INTO info VALUES('schema_compatible', '{v}'); PRAGMA user_version={v};", v = crate::db::VERSION + 1)).unwrap();
        }
        assert!(check_backup(&newer).unwrap_err().contains("neueren Version"));
        // a marker pointing at a vanished backup: error, library untouched, marker gone
        std::fs::write(restore_marker(&lib), br#"{"backup": "/nonexistent/x.sqlite"}"#).unwrap();
        let before = std::fs::read(&lib).unwrap();
        assert!(apply_pending_restore(&lib).unwrap().is_err());
        assert_eq!(std::fs::read(&lib).unwrap(), before);
        assert!(!restore_marker(&lib).exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Rotation never pushes out a backup with far more photos than the newest (emptied or lost library).
    #[test]
    fn rotation_keeps_backups_of_a_fuller_library() {
        let d = tmp();
        let mk = |name: &str, images: usize| {
            let c = rusqlite::Connection::open(d.join(name)).unwrap();
            c.execute_batch(&format!("CREATE TABLE image(id INTEGER PRIMARY KEY); PRAGMA user_version={};", crate::db::VERSION)).unwrap();
            for _ in 0..images {
                c.execute("INSERT INTO image DEFAULT VALUES", []).unwrap();
            }
        };
        mk("library-2026-10-01.sqlite", 100);
        mk("library-2026-10-02.sqlite", 100);
        mk("library-2026-10-03.sqlite", 0);
        mk("library-2026-10-04.sqlite", 0);
        mk("library-2026-10-05.sqlite", 0);
        prune(&d);
        let l = list(&d);
        assert_eq!(l.len(), 5, "{l:?}");
        // normal growth: old ones go
        mk("library-2026-10-06.sqlite", 100);
        mk("library-2026-10-07.sqlite", 100);
        mk("library-2026-10-08.sqlite", 101);
        prune(&d);
        // the three newest and the monthly one (the first of October)
        let l = list(&d);
        assert_eq!(l.len(), KEEP + MONTHLY, "{l:?}");
        assert!(l[KEEP].ends_with("library-2026-10-01.sqlite"));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Keeping rule with made-up dates: one backup per day (with gaps) from late September to mid-November, cleaned up
    /// after each one like the app does. Always the three newest plus exactly one monthly backup.
    #[test]
    fn rotation_keeps_three_days_and_one_month() {
        let d = tmp();
        let mut days: Vec<String> = Vec::new();
        for (m, last) in [(9, 30), (10, 31), (11, 12)] {
            for t in 1..=last {
                // September only from the 25th; a gap (app not opened) on 31.10. and 5.–6.11.
                if (m == 9 && t < 25) || (m == 10 && t == 31) || (m == 11 && (t == 5 || t == 6)) {
                    continue;
                }
                days.push(format!("2026-{m:02}-{t:02}"));
            }
        }
        let names = |d: &Path| list(d).iter().map(|p| date_of(p)).collect::<Vec<_>>();
        for (i, day) in days.iter().enumerate() {
            // until 15.10. unpacked (as before the update), from then on packed
            let ext = if day.as_str() < "2026-10-15" { ".sqlite" } else { PACKED };
            std::fs::write(d.join(format!("library-{day}{ext}")), b"x").unwrap();
            prune(&d);
            let have = names(&d);
            assert!(have.len() <= KEEP + MONTHLY, "{day}: {have:?}");
            assert_eq!(have[..KEEP.min(i + 1)], days[i + 1 - KEEP.min(i + 1)..=i].iter().rev().cloned().collect::<Vec<_>>()[..], "{day}");
            let expected_monthly = match day.as_str() {
                "2026-09-25" | "2026-09-26" | "2026-09-27" => None,
                "2026-11-01" | "2026-11-02" | "2026-11-03" => Some("2026-10-01"),
                x if x < "2026-10-04" => Some("2026-09-25"),
                x if x < "2026-11-01" => Some("2026-10-01"),
                _ => Some("2026-11-01"),
            };
            assert_eq!(have.get(KEEP).map(String::as_str), expected_monthly, "{day}: {have:?}");
        }
        // what is left: 12., 11., 10.11. and the monthly backup 1.11. (packed)
        assert_eq!(names(&d), ["2026-11-12", "2026-11-11", "2026-11-10", "2026-11-01"]);
        assert!(list(&d).iter().all(|p| is_packed(p)));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Packing: the backup is a zstd file that unpacks to an intact library (quick_check) with the same content; the
    /// header names the photo count; a damaged packed file is refused (unpacking and checking).
    #[test]
    fn packed_backup_unpacks_to_an_intact_library() {
        let d = tmp();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        let root = db.create_root("/tmp/graineo-pack-test/", "test", "own", None).unwrap();
        let folder = db.create_folder(root, "", None).unwrap();
        for i in 0..500 {
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, ?2, 'JPG')", rusqlite::params![folder, format!("bild-{i:04}.jpg")]).unwrap();
            let file = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file, stars) VALUES(?1, ?2)", rusqlite::params![file, i % 6]).unwrap();
        }
        let (p, n) = create_backup(&db).unwrap();
        assert!(is_packed(&p) && p.file_name().unwrap().to_string_lossy() == format!("library-{}.sqlite.zst", today()));
        assert_eq!(std::fs::read(&p).unwrap().len() as u64, n);
        let h = packed_header(&p).unwrap();
        assert_eq!(h["images"], 500);
        assert_eq!(h["schema"], crate::db::VERSION);
        assert!(n < h["size"].as_u64().unwrap() / 2, "packed {n}, unpacked {}", h["size"]);
        assert_eq!(image_count(&p), Some(500));
        let out = d.join("unpacked.sqlite");
        unpack(&p, &out).unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().len(), h["size"].as_u64().unwrap());
        {
            let c = rusqlite::Connection::open(&out).unwrap();
            assert_eq!(c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
            assert_eq!(c.query_row("SELECT sum(stars) FROM image", [], |r| r.get::<_, i64>(0)).unwrap(), (0..500).map(|i| i % 6).sum::<i64>());
        }
        assert_eq!(check_backup(&p).unwrap(), BackupInfo { images: 500, schema: crate::db::VERSION });
        // nothing left over: no temporary files in the target or next to the library
        let stray: Vec<_> = std::fs::read_dir(&d).unwrap().chain(std::fs::read_dir(d.join("backups")).unwrap()).flatten().filter(|e| e.file_name().to_string_lossy().contains(".tmp")).collect();
        assert!(stray.is_empty(), "{stray:?}");
        // damaged in the middle: refused
        let mut b = std::fs::read(&p).unwrap();
        let m = b.len() / 2;
        b[m] ^= 0x5a;
        let bad = d.join("library-2026-01-01.sqlite.zst");
        std::fs::write(&bad, &b).unwrap();
        assert!(check_backup(&bad).is_err());
        assert!(schedule_restore(&d.join("library.sqlite"), &bad).is_err());
        drop(db);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Restoring an old unpacked backup (from before 0.107.0) still works; the next backup of the same day replaces it
    /// packed.
    #[test]
    fn restore_from_old_unpacked_backup() {
        let d = tmp();
        let lib = d.join("library.sqlite");
        let old = d.join("backups").join(format!("library-{}.sqlite", today()));
        {
            let db = Db::open(&lib).unwrap();
            let root = db.create_root("/tmp/graineo-old-test/", "test", "own", None).unwrap();
            let folder = db.create_folder(root, "", None).unwrap();
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'a.jpg', 'JPG')", [folder]).unwrap();
            let file = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file, stars) VALUES(?1, 5)", [file]).unwrap();
            std::fs::create_dir_all(d.join("backups")).unwrap();
            db.conn.execute("VACUUM INTO ?1", [old.to_string_lossy()]).unwrap();
            assert!(!due(&db), "the unpacked backup of today counts");
            assert_eq!(list(&d.join("backups")), vec![old.clone()]);
            db.conn.execute("DELETE FROM image", []).unwrap();
            schedule_restore(&lib, &old).unwrap();
        }
        let done = apply_pending_restore(&lib).unwrap().unwrap();
        assert_eq!(done.images, 1);
        let db = Db::open(&lib).unwrap();
        assert_eq!(db.image_count(), 1);
        assert!(!d.join("library.sqlite.wiederherstellen").exists());
        // "Jetzt sichern" on the same day: the packed backup replaces the unpacked one of the day
        let (p, _) = create_backup(&db).unwrap();
        assert_eq!(list(&d.join("backups")), vec![p]);
        drop(db);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Second target (e.g. the NAS): gets the packed backup, is cleaned up by the same rule, its packed and old
    /// unpacked backups can be restored.
    #[test]
    fn second_target_gets_packed_backups() {
        let d = tmp();
        let lib = d.join("library.sqlite");
        let nas = d.join("nas");
        std::fs::create_dir_all(&nas).unwrap();
        // old backups on the NAS: three unpacked from earlier days (one of them a real library), one foreign file
        for day in ["2026-09-01", "2026-09-02"] {
            std::fs::write(nas.join(format!("library-{day}.sqlite")), b"x").unwrap();
        }
        std::fs::write(nas.join("notizen.txt"), b"bleibt").unwrap();
        let p = {
            let db = Db::open(&lib).unwrap();
            let root = db.create_root("/tmp/graineo-nas-test/", "test", "own", None).unwrap();
            let folder = db.create_folder(root, "", None).unwrap();
            db.conn.execute("INSERT INTO file(folder, name, format) VALUES(?1, 'n.jpg', 'JPG')", [folder]).unwrap();
            let file = db.conn.last_insert_rowid();
            db.conn.execute("INSERT INTO image(file, stars) VALUES(?1, 3)", [file]).unwrap();
            db.conn.execute("VACUUM INTO ?1", [nas.join("library-2026-09-03.sqlite").to_string_lossy()]).unwrap();
            db.set_info("backup_dir2", &nas.to_string_lossy()).unwrap();
            let (p, _) = create_backup(&db).unwrap();
            p
        };
        let copy = nas.join(p.file_name().unwrap());
        assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&p).unwrap());
        // 4 daily backups on the NAS: the packed one of today, 03., 02. and the monthly one (01.)
        let l = list(&nas);
        assert_eq!(l.len(), KEEP + MONTHLY, "{l:?}");
        assert!(nas.join("notizen.txt").exists());
        assert_eq!(check_backup(&copy).unwrap().images, 1);
        assert_eq!(check_backup(&nas.join("library-2026-09-03.sqlite")).unwrap().images, 1);
        // restore from the NAS copy
        schedule_restore(&lib, &copy).unwrap();
        assert_eq!(apply_pending_restore(&lib).unwrap().unwrap().images, 1);
        assert_eq!(Db::open(&lib).unwrap().image_count(), 1);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn prune_only_touches_daily_backups() {
        assert!(is_daily_backup("library-2026-10-30.sqlite"));
        assert!(is_daily_backup("library-2026-10-30.sqlite.zst"));
        for no in ["library-vor-0.36.0-20261030-114030.sqlite", "library-vor-0.9.1.sqlite", "library-rebuild.sqlite", "library-2026-10-30.sqlite-wal", "library-2026-1-30.sqlite", "x-2026-10-30.sqlite", "library-2026-10-30.zst", "library-2026-10-30.sqlite.zst.tmp"] {
            assert!(!is_daily_backup(no), "{no}");
        }
        let d = std::env::temp_dir().join(format!("graineo-sicherung-rotation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for n in ["library-vor-0.9.1.sqlite", "library-vor-0.36.0-20261030-114030.sqlite", "library-2026-10-01.sqlite", "library-2026-10-02.sqlite"] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        let l = list(&d);
        assert_eq!(l.len(), 2, "{l:?}");
        assert!(l[0].ends_with("library-2026-10-02.sqlite"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
