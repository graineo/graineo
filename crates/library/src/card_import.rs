//! Import from a memory card or folder into date folders `YYYY/YYYY-MM-DD`.
//!
//! - The source is only read. Each copy is created as a hidden partial file in the target folder, flushed to disk,
//!   read back past the system's file cache and compared with the source via SHA-256; only then does it appear under
//!   its name (hard link, never overwriting; for the same name with different content: `NAME-1.EXT`).
//! - Already in the library = same length and same partial checksum (first and last 64 KB) as a file of the library,
//!   whatever its name. Identical copies within the source are recognised the same way (the first by path stays).
//!   Only when the library file cannot be read (drive not connected) the old rule applies: same size and same name or
//!   capture time – shown as "probably already there", never skipped silently.
//! - RAW+JPEG stay together (same name without extension → same date folder, date of the RAW); moving disposes of the
//!   originals of a photo only after all its copies (both halves, second copy, XMP) are verified.
//! - Optionally a second, equally verified copy into a backup folder (same structure).
//! - A drive or card that goes away during the run makes it wait instead of counting every further file as an error.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::params;

use crate::db::{Db, Root};
use crate::import_journal::{Journal, Planned};

#[derive(Clone, Debug, Default)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub format: String,
    /// 'YYYY-MM-DD' (capture, else modification date)
    pub tag: String,
    pub capture: Option<String>,
    /// already in the library (same length and partial checksum as a library file; the name does not matter)
    pub duplicate: bool,
    /// probably in the library: a library file of the same size and name or capture time whose content could not be
    /// compared (its drive is not connected). Shown, not checked, never skipped silently.
    pub probable: bool,
    /// identical copy of another photo of this source (same length and partial checksum); the first by path is not marked
    pub identical: bool,
    /// partial checksum (only computed where a file of the same size exists in the library or the source)
    pub partial: Option<String>,
}

/// Mounted memory cards (drives with `DCIM`); while the import window is open [`crate::card_watch`] keeps them current.
pub fn cards() -> Vec<PathBuf> {
    crate::card_watch::cards()
}

/// Today's date (local time) as `YYYY-MM-DD`.
pub fn today() -> String {
    local_date(std::time::SystemTime::now())
}

fn local_date(t: std::time::SystemTime) -> String {
    let s = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (j, m, t) = crate::platform::local_date(s);
    format!("{j:04}-{m:02}-{t:02}")
}

/// Scan the source (recursively, hidden folders excluded) and compare with the library.
pub fn scan(db: &Db, source: &Path) -> Vec<Candidate> {
    scan_with(db, source, &|_| {}, &|_, _| {})
}

/// List the photos of a folder (recursively, hidden folders excluded), sorted by path; day by modification time.
pub fn list(source: &Path) -> Vec<Candidate> {
    let mut files = Vec::new();
    let mut batch = vec![source.to_path_buf()];
    while let Some(d) = batch.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                batch.push(e.path());
            } else if let Some(fm) = crate::format_from_extension(&n) {
                if let Ok(m) = e.metadata() {
                    files.push((e.path(), fm.to_string(), m));
                }
            }
        }
    }
    let mut out: Vec<Candidate> = files
        .into_iter()
        .map(|(p, fm, m)| {
            let tag = local_date(m.modified().unwrap_or(std::time::UNIX_EPOCH));
            Candidate { path: p, size: m.len(), format: fm, tag, ..Default::default() }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Like [`scan`], in two steps so that the dialog shows something right away: `upfront` gets all photos right after
/// listing (day still by modification time, "already there" provisionally by name and size); then capture dates and
/// partial checksums are read in parallel (`progress`: read, total) and the complete result is returned.
pub fn scan_with(db: &Db, source: &Path, upfront: &dyn Fn(Vec<Candidate>), progress: &(dyn Fn(usize, usize) + Sync)) -> Vec<Candidate> {
    let mut out = list(source);
    for k in &mut out {
        k.duplicate = known_by_name(db, k);
    }
    upfront(out.clone());
    complete(db, &mut out, progress);
    out
}

/// Second step of [`scan_with`]: capture dates, RAW+JPEG days, partial checksums, "already there" and "identical".
pub fn complete(db: &Db, out: &mut [Candidate], progress: &(dyn Fn(usize, usize) + Sync)) {
    // a partial checksum is only needed where a file of the same size exists (library or source): almost never for new photos
    let mut per_size: HashMap<u64, usize> = HashMap::new();
    for k in out.iter() {
        *per_size.entry(k.size).or_default() += 1;
    }
    let need: Vec<bool> = out.iter().map(|k| per_size[&k.size] > 1 || library_has_size(db, k.size)).collect();
    // capture dates: only the file header; several at once (card and disk deliver faster that way), with low priority
    let total = out.len();
    let read_result = std::sync::atomic::AtomicUsize::new(0);
    let read = |(k, need): (&Candidate, &bool)| {
        let a = crate::exif::read(&k.path, &k.format).and_then(|a| a.capture);
        let s = if *need { crate::reconcile::partial_sum(&k.path) } else { None };
        progress(read_result.fetch_add(1, Ordering::Relaxed) + 1, total);
        (a, s)
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).start_handler(|_| crate::preview::low_priority_thread()).build().ok();
    let read_all: Vec<(Option<String>, Option<String>)> = match &pool {
        Some(pool) => pool.install(|| {
            use rayon::prelude::*;
            out.par_iter().zip(need.par_iter()).map(read).collect()
        }),
        None => out.iter().zip(need.iter()).map(read).collect(),
    };
    for (k, (a, s)) in out.iter_mut().zip(read_all) {
        if let Some(t) = a.as_deref().and_then(|a| a.get(..10)) {
            k.tag = t.to_string();
        }
        k.capture = a;
        k.partial = s;
    }
    raw_days(out);
    classify(db, out, pool.as_ref());
}

/// RAW+JPEG: the date of the RAW for the whole group.
pub(crate) fn raw_days(out: &mut [Candidate]) {
    let mut day_per: HashMap<(PathBuf, String), String> = HashMap::new();
    for k in out.iter().filter(|k| crate::is_raw(&k.format)) {
        day_per.insert(pair_key(&k.path), k.tag.clone());
    }
    for k in out.iter_mut() {
        if let Some(t) = day_per.get(&pair_key(&k.path)) {
            k.tag = t.clone();
        }
    }
}

/// RAW+JPEG belong together: same folder and same name without extension (case ignored).
pub fn pair_key(p: &Path) -> (PathBuf, String) {
    (p.parent().unwrap_or(Path::new("")).to_path_buf(), p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default())
}

pub(crate) fn library_has_size(db: &Db, size: u64) -> bool {
    db.conn.query_row("SELECT EXISTS (SELECT 1 FROM file WHERE size = ?1)", [size as i64], |r| r.get::<_, bool>(0)).unwrap_or(false)
}

/// Provisional "already there" while the source is still being read: same size and same name.
pub(crate) fn known_by_name(db: &Db, k: &Candidate) -> bool {
    let name = crate::nfc(&k.path.file_name().unwrap_or_default().to_string_lossy());
    db.conn.query_row("SELECT EXISTS (SELECT 1 FROM file WHERE size = ?2 AND name = ?1 COLLATE NOCASE)", params![name, k.size as i64], |r| r.get::<_, bool>(0)).unwrap_or(false)
}

/// A file of the library with the same size as a candidate.
struct LibFile {
    id: i64,
    sum: Option<String>,
    name: String,
    capture: Option<String>,
}

/// Decide "already in the library", "probably" and "identical" (needs [`Candidate::partial`] where a file of the same
/// size exists). Library files without a stored partial checksum are read once (if reachable) and their checksum is
/// stored, so the next card does not read them again.
pub(crate) fn classify(db: &Db, out: &mut [Candidate], pool: Option<&rayon::ThreadPool>) {
    let mut same_size: HashMap<u64, Vec<LibFile>> = HashMap::new();
    if let Ok(mut st) = db.conn.prepare_cached("SELECT id, partial_hash, name, capture_time FROM file WHERE size = ?1") {
        for k in out.iter().filter(|k| k.partial.is_some()) {
            if same_size.contains_key(&k.size) {
                continue;
            }
            let rows: Vec<LibFile> = st
                .query_map([k.size as i64], |r| Ok(LibFile { id: r.get(0)?, sum: r.get(1)?, name: r.get(2)?, capture: r.get(3)? }))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default();
            same_size.insert(k.size, rows);
        }
    }
    // library files without a stored sum: read them now (in parallel, low priority) – skipped when not reachable
    let missing: Vec<(i64, PathBuf)> = same_size.values().flatten().filter(|f| f.sum.is_none()).filter_map(|f| db.file_path(f.id).map(|p| (f.id, p))).collect();
    let compute = |(id, p): &(i64, PathBuf)| -> Option<(i64, String)> { p.is_file().then(|| crate::reconcile::partial_sum(p)).flatten().map(|s| (*id, s)) };
    let computed: Vec<(i64, String)> = match pool {
        Some(pool) => pool.install(|| {
            use rayon::prelude::*;
            missing.par_iter().filter_map(compute).collect()
        }),
        None => missing.iter().filter_map(compute).collect(),
    };
    if !computed.is_empty() {
        // remember them (a short transaction; the import works without it, so an error only costs time next time)
        let store = || -> rusqlite::Result<()> {
            let tx = db.conn.unchecked_transaction()?;
            for (id, s) in &computed {
                tx.execute("UPDATE file SET partial_hash = ?2 WHERE id = ?1 AND partial_hash IS NULL", params![id, s])?;
            }
            tx.commit()
        };
        if let Err(e) = store() {
            eprintln!("import: partial checksums not stored: {e}");
        }
        let by_id: HashMap<i64, &String> = computed.iter().map(|(i, s)| (*i, s)).collect();
        for f in same_size.values_mut().flatten() {
            if f.sum.is_none() {
                f.sum = by_id.get(&f.id).map(|s| (*s).clone());
            }
        }
    }
    for k in out.iter_mut() {
        k.duplicate = false;
        k.probable = false;
        k.identical = false;
        let Some(sum) = &k.partial else { continue };
        let Some(files) = same_size.get(&k.size) else { continue };
        if files.iter().any(|f| f.sum.as_deref() == Some(sum.as_str())) {
            k.duplicate = true;
            continue;
        }
        // content not comparable (drive not connected): the old rule, but only as "probably"
        let name = crate::nfc(&k.path.file_name().unwrap_or_default().to_string_lossy());
        k.probable = files.iter().any(|f| f.sum.is_none() && (f.name.eq_ignore_ascii_case(&name) || (f.capture.is_some() && f.capture == k.capture)));
    }
    // identical copies within the source: the first by path stays, the others are marked
    let mut first: HashMap<(u64, &str), usize> = HashMap::new();
    let mut identical = Vec::new();
    for (i, k) in out.iter().enumerate() {
        if let Some(s) = k.partial.as_deref() {
            match first.get(&(k.size, s)) {
                Some(_) => identical.push(i),
                None => {
                    first.insert((k.size, s), i);
                }
            }
        }
    }
    for i in identical {
        out[i].identical = true;
    }
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub copied: usize,
    pub duplicate: usize,
    pub backed_up: usize,
    pub error: Vec<String>,
    pub bytes: u64,
    pub duration_ms: u128,
    pub folder: Vec<PathBuf>,
    /// target files of the copied or moved photos (for collection and keywords)
    pub target_files: Vec<PathBuf>,
    /// their sources (same order as `target_files`): previews right after the import come from the file on the card
    pub source_files: Vec<PathBuf>,
    /// SHA-256 of each copied file (same order as `target_files`)
    pub sums: Vec<String>,
    /// originals moved to the Trash after the verified copy ("move"), counted per photo (RAW+JPEG = one)
    pub moved: usize,
    /// the originals went to a trash on the card or a drive whose space is only freed when that trash is emptied
    pub trash_on_volume: bool,
    /// cancelled by the user
    pub cancelled: bool,
    /// how long the run waited for a drive or card that was gone (seconds)
    pub waited_secs: u64,
}

/// Verified copy: partial file → compare SHA-256 → rename; free name, never overwrite.
pub fn copy_verified(source: &Path, target_dir: &Path) -> Result<PathBuf, String> {
    copy_verified_with(source, target_dir, &mut |_| {})
}

/// Like [`copy_verified`]; the source is read only **once** (the checksum is computed while copying),
/// verification is against the written copy. `bytes` continuously reports how much has been copied.
pub fn copy_verified_with(source: &Path, target_dir: &Path, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String> {
    let name = source.file_name().ok_or("kein Name")?.to_string_lossy().to_string();
    copy_verified_as(source, target_dir, &name, bytes)
}

/// Like [`copy_verified_with`], under the name `name` in `target_dir` (if taken: `name-1.ext` …).
pub fn copy_verified_as(source: &Path, target_dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String> {
    copy_verified_sum(source, target_dir, name, bytes).map(|(p, _)| p)
}

/// Like [`copy_verified_as`]; also returns the SHA-256 of the file. The copy is flushed to disk and read back past the
/// system's file cache, so the comparison proves what is on the target (NAS, external drive), not what is in memory.
pub fn copy_verified_sum(source: &Path, target_dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<(PathBuf, String), String> {
    crate::write_target::check(target_dir)?;
    std::fs::create_dir_all(target_dir).map_err(|e| format!("{}: {e}", target_dir.display()))?;
    tidy_side_files(target_dir);
    let name = name.to_string();
    // side file with an unpredictable name, created exclusively (follows no prepared link)
    let (part, a) = crate::files::side_copy_with_sum(source, &target_dir.join(&name), bytes).map_err(|e| format!("{}: {e}", source.display()))?;
    let b = match sha256_uncached(&part) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    if a != b {
        let _ = std::fs::remove_file(&part);
        return Err(format!("Prüfsumme stimmt nicht: {}", source.display()));
    }
    place(&part, target_dir, &name, &a).map(|p| (p, a))
}

/// Give a verified partial file `part` (in `target_dir`) its name: hard link to `name` or, if taken by other content,
/// `NAME-1.EXT` …; never overwrites. The same content already under the name: the partial file goes, that one stays.
pub(crate) fn place(part: &Path, target_dir: &Path, name: &str, a: &str) -> Result<PathBuf, String> {
    let name = name.to_string();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    for i in 0.. {
        let target = if i == 0 { target_dir.join(&name) } else { target_dir.join(format!("{stem}-{i}{ext}")) };
        if target.exists() {
            // same file already there → do nothing
            if crate::relocate::sha256(&target).ok().as_deref() == Some(a) {
                let _ = std::fs::remove_file(part);
                return Ok(target);
            }
            continue;
        }
        // never overwrite: rename that fails if the target exists by now (with fallbacks for file systems without
        // exclusive rename or hard links, e.g. exFAT cards/disks and some SMB shares – plain hard_link failed there for
        // every photo and left a full partial copy behind each time)
        match crate::files::rename_no_replace(part, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                let _ = std::fs::remove_file(part);
                return Err(format!("{}: {e}", target.display()));
            }
        }
    }
    let _ = std::fs::remove_file(part);
    Err(format!("{}: kein freier Name", target_dir.display()))
}

/// Is `t` a day `YYYY-MM-DD` (digits and dashes only, so safe as a folder name and to slice)?
pub(crate) fn is_day(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// SHA-256 of a file, read past the file cache of the system (macOS `F_NOCACHE`, Windows `FILE_FLAG_NO_BUFFERING`):
/// the bytes come from the drive, not from the memory that just wrote them.
pub fn sha256_uncached(p: &Path) -> Result<String, String> {
    read_uncached(p, &mut |_| {})
}

/// Like [`sha256_uncached`]; also returns the first 64 KB and (for files over 128 KB) the last 64 KB as read from the
/// drive, for the partial checksum ([`crate::reconcile::partial_sum_of`]).
pub fn sha256_uncached_edges(p: &Path) -> Result<(String, Vec<u8>, Option<Vec<u8>>, u64), String> {
    const EDGE: usize = 65536;
    let mut first: Vec<u8> = Vec::with_capacity(EDGE);
    // ring of the last bytes read (only the last 64 KB are kept)
    let mut tail: std::collections::VecDeque<u8> = std::collections::VecDeque::with_capacity(2 * EDGE);
    let mut len = 0u64;
    let sum = read_uncached(p, &mut |b| {
        len += b.len() as u64;
        if first.len() < EDGE {
            let n = (EDGE - first.len()).min(b.len());
            first.extend_from_slice(&b[..n]);
        }
        let keep = &b[b.len().saturating_sub(EDGE)..];
        tail.extend(keep.iter().copied());
        while tail.len() > EDGE {
            tail.pop_front();
        }
    })?;
    let last = (len > 2 * EDGE as u64).then(|| tail.into_iter().collect());
    Ok((sum, first, last, len))
}

/// Read a file past the system's file cache; every chunk goes to `chunk`; returns the SHA-256.
fn read_uncached(p: &Path, chunk: &mut dyn FnMut(&[u8])) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let err = |e: std::io::Error| format!("{}: {e}", p.display());
    #[cfg(windows)]
    let mut f = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().read(true).custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING).open(p).map_err(err)?
    };
    #[cfg(not(windows))]
    let mut f = std::fs::File::open(p).map_err(err)?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: plain fcntl on an open descriptor
        unsafe { libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1) };
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: advisory call on an open descriptor
        unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    }
    // unbuffered reads need a buffer aligned to the sector size (Windows); 4 KB alignment suits every drive
    const CHUNK: usize = 1 << 20;
    let mut raw = vec![0u8; CHUNK + 4096];
    let off = raw.as_ptr().align_offset(4096);
    let buf = &mut raw[off..off + CHUNK];
    let mut h = Sha256::new();
    loop {
        let n = f.read(buf).map_err(err)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        chunk(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Remove partial files of an earlier, interrupted copy in `dir` (own names only: `.NAME.<process>-<n>-<time>.copying`, see [`crate::files::partial_copy_stem`],
/// from another process and older than an hour). Once per folder and run of the app.
fn tidy_side_files(dir: &Path) {
    static DONE: std::sync::Mutex<Option<HashSet<PathBuf>>> = std::sync::Mutex::new(None);
    if !DONE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(dir.to_path_buf()) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let own = std::process::id().to_string();
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = crate::files::partial_copy_stem(&n) else { continue };
        let Some((_, marker)) = rest.rsplit_once('.') else { continue };
        let parts: Vec<&str> = marker.split('-').collect();
        let ours = parts.len() == 3 && parts[0].chars().all(|c| c.is_ascii_digit()) && parts[1].chars().all(|c| c.is_ascii_digit()) && parts[2].chars().all(|c| c.is_ascii_hexdigit());
        if !ours || parts[0] == own {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 3600);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Copy progress: photos and bytes (for bar, speed and remaining time), counters for the window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    /// photo being copied (RAW+JPEG = one)
    pub file: usize,
    pub files: usize,
    pub bytes: u64,
    pub bytes_total: u64,
    /// files copied and verified
    pub verified: usize,
    /// files of the second copy
    pub second: usize,
    /// photos whose originals went to the Trash
    pub trashed: usize,
    pub errors: usize,
    /// a drive or the card is gone: the run waits until it is back
    pub waiting: bool,
}

pub fn import(candidates: &[Candidate], target: &Path, backup: Option<&Path>, cancel: &AtomicBool, progress: &dyn Fn(State)) -> Report {
    import_with(candidates, target, true, backup, None, cancel, progress)
}

/// Sidecars of a photo next to it (XMP): `NAME.xmp` and `NAME.EXT.xmp`.
fn sidecar_xmp(p: &Path) -> Vec<PathBuf> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    [p.with_extension("xmp"), p.with_file_name(format!("{name}.xmp"))].into_iter().filter(|x| x != p && x.is_file()).collect()
}

/// Group the candidates into photos (RAW+JPEG = one), in the order of the list.
pub fn photos(candidates: &[Candidate]) -> Vec<Vec<&Candidate>> {
    let mut index: HashMap<(PathBuf, String), usize> = HashMap::new();
    let mut out: Vec<Vec<&Candidate>> = Vec::new();
    for k in candidates {
        match index.get(&pair_key(&k.path)) {
            Some(&i) => out[i].push(k),
            None => {
                index.insert(pair_key(&k.path), out.len());
                out.push(vec![k]);
            }
        }
    }
    out
}

/// Copy (and with `dispose` move): each photo with its XMP sidecars verified into the target folder
/// (`by_date`: subfolders `YYYY/YYYY-MM-DD`), optionally also into the backup folder. With `dispose`
/// (move) the originals of a photo go to the Trash only after all its copies are verified – never before
/// and never on an error. Otherwise the source is only read.
pub fn import_with(
    candidates: &[Candidate],
    target: &Path,
    by_date: bool,
    backup: Option<&Path>,
    dispose: Option<&dyn Fn(&[PathBuf]) -> Result<(), String>>,
    cancel: &AtomicBool,
    progress: &dyn Fn(State),
) -> Report {
    copy_photos(candidates, target, by_date, backup, dispose, None, None, cancel, progress)
}

/// What one attempt at a photo produced (merged into the report only once it is final).
#[derive(Default)]
struct Attempt {
    copies: Vec<(PathBuf, PathBuf, String, u64)>,
    backed_up: usize,
    folder: Vec<PathBuf>,
    error: Vec<(PathBuf, String)>,
}

/// Is every place this photo needs reachable (source folder, target, backup)?
fn reachable(roots: &[&Path]) -> bool {
    roots.iter().all(|p| p.is_dir())
}

/// The base folders to watch for a photo: target, backup (as found at the start) and the folders of its source files
/// (a card that is pulled).
fn watched<'a>(group: &'a [&'a Candidate], bases: &'a [PathBuf]) -> Vec<&'a Path> {
    let mut v: Vec<&Path> = bases.iter().map(PathBuf::as_path).collect();
    // a camera is no folder: whether it is there, the source itself says (see `copy_photos`)
    for k in group.iter().filter(|k| !crate::import_source::is_camera(&k.path)) {
        if let Some(p) = k.path.parent() {
            if !v.contains(&p) {
                v.push(p);
            }
        }
    }
    v
}

/// The folder that tells whether a target's drive is there, as found at the start of the run: the target itself if it
/// exists, otherwise its nearest existing ancestor – but never `/Volumes` itself (a drive not connected yet).
fn base_of(p: &Path) -> PathBuf {
    let mut a = p;
    loop {
        if a.is_dir() {
            return a.to_path_buf();
        }
        match a.parent() {
            Some(parent) if parent == Path::new("/Volumes") => return a.to_path_buf(),
            Some(parent) => a = parent,
            None => return p.to_path_buf(),
        }
    }
}

/// Wait until `there` says everything is reachable (polled twice a second). Returns the seconds waited, `None` if
/// cancelled meanwhile.
fn wait_for(there: &dyn Fn() -> bool, cancel: &AtomicBool, waiting: &mut dyn FnMut(bool)) -> Option<u64> {
    if there() {
        return Some(0);
    }
    let t = std::time::Instant::now();
    waiting(true);
    while !there() {
        if cancel.load(Ordering::Relaxed) {
            waiting(false);
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    waiting(false);
    Some(t.elapsed().as_secs())
}

#[allow(clippy::too_many_arguments)]
fn copy_photos(
    candidates: &[Candidate],
    target: &Path,
    by_date: bool,
    backup: Option<&Path>,
    dispose: Option<&dyn Fn(&[PathBuf]) -> Result<(), String>>,
    journal: Option<&Journal>,
    src: Option<&dyn crate::import_source::Source>,
    cancel: &AtomicBool,
    progress: &dyn Fn(State),
) -> Report {
    use crate::import_source::is_camera;
    let t = std::time::Instant::now();
    let mut b = Report::default();
    if let Err(e) = crate::backup_view::refuse() {
        b.error.push(e);
        return b;
    }
    // never into a package of another program (Apple Photos library …): nothing is copied, nothing moved
    if let Some(e) = std::iter::once(target).chain(backup).find_map(|p| crate::write_target::check(p).err()) {
        b.error.push(e);
        return b;
    }
    // writable? a clear message before the first file instead of one error per file
    if !candidates.is_empty() {
        if let Some(e) = std::iter::once(target).chain(backup).find_map(|p| crate::write_target::probe(p).err()) {
            b.error.push(e);
            return b;
        }
    }
    let list: Vec<Candidate> = candidates.iter().filter(|k| !k.duplicate).cloned().collect();
    b.duplicate = candidates.len() - list.len();
    let groups = photos(&list);
    let bases: Vec<PathBuf> = std::iter::once(target).chain(backup).map(base_of).collect();
    let once: u64 = list.iter().map(|k| k.size).sum();
    let state = std::cell::Cell::new(State { file: 0, files: groups.len(), bytes: 0, bytes_total: once * if backup.is_some() { 2 } else { 1 }, ..Default::default() });
    // report at most about 20 times per second
    let reported = std::cell::Cell::new(std::time::Instant::now());
    let notify = |always: bool| {
        if always || reported.get().elapsed().as_millis() >= 50 {
            reported.set(std::time::Instant::now());
            progress(state.get());
        }
    };
    let set = |f: &dyn Fn(&mut State)| {
        let mut s = state.get();
        f(&mut s);
        state.set(s);
    };
    'photos: for (i, group) in groups.iter().enumerate() {
        set(&|s| s.file = i);
        notify(true);
        let rel = |k: &Candidate| if by_date && is_day(&k.tag) { format!("{}/{}", &k.tag[..4], k.tag) } else { String::new() };
        let roots = watched(group, &bases);
        // a camera (no drive): it says itself whether it is connected and ready
        let camera = group.iter().any(|k| is_camera(&k.path));
        let there = || reachable(&roots) && (!camera || src.is_some_and(|s| s.connected()));
        let start_bytes = state.get().bytes;
        let attempt = loop {
            if cancel.load(Ordering::Relaxed) {
                b.cancelled = true;
                b.error.push("abgebrochen".into());
                break 'photos;
            }
            match wait_for(&there, cancel, &mut |w| {
                set(&|s| s.waiting = w);
                notify(true);
            }) {
                Some(secs) => b.waited_secs += secs,
                None => continue,
            }
            set(&|s| s.bytes = start_bytes);
            let mut a = Attempt::default();
            for k in group {
                let r = rel(k);
                let dir = if r.is_empty() { target.to_path_buf() } else { target.join(&r) };
                let before = state.get().bytes;
                let name = k.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let mut bytes = |n: u64| {
                    set(&|s| s.bytes = before + n);
                    notify(false);
                };
                let copy = if is_camera(&k.path) {
                    match src {
                        Some(s) => crate::import_source::fetch_verified(s, k, &dir, &name, &mut bytes),
                        None => Err(format!("{}: Kamera nicht verbunden", k.path.display())),
                    }
                } else {
                    copy_verified_sum(&k.path, &dir, &name, &mut bytes)
                };
                set(&|s| s.bytes = before + k.size);
                let first = match copy {
                    Ok((z, sum)) => {
                        a.copies.push((k.path.clone(), z.clone(), sum, k.size));
                        if !a.folder.contains(&dir) {
                            a.folder.push(dir.clone());
                        }
                        Some(z)
                    }
                    Err(e) => {
                        a.error.push((k.path.clone(), e));
                        None
                    }
                };
                let xmps = sidecar_xmp(&k.path);
                for x in &xmps {
                    if let Err(e) = copy_verified(x, &dir) {
                        a.error.push((k.path.clone(), e));
                    }
                }
                if let Some(s) = backup {
                    let backup_dir = if r.is_empty() { s.to_path_buf() } else { s.join(&r) };
                    // second copy from the first one just verified (faster than reading the card a second time)
                    let from = first.clone().unwrap_or_else(|| k.path.clone());
                    let before = state.get().bytes;
                    let copy = copy_verified_sum(&from, &backup_dir, &k.path.file_name().unwrap_or_default().to_string_lossy(), &mut |n| {
                        set(&|s| s.bytes = before + n);
                        notify(false);
                    });
                    set(&|s| s.bytes = before + k.size);
                    match copy {
                        Ok(_) => a.backed_up += 1,
                        Err(e) => a.error.push((k.path.clone(), format!("Sicherung: {e}"))),
                    }
                    for x in &xmps {
                        if let Err(e) = copy_verified(x, &backup_dir) {
                            a.error.push((k.path.clone(), format!("Sicherung: {e}")));
                        }
                    }
                }
            }
            // a camera that was pulled during the transfer may say so only a moment later
            if !a.error.is_empty() && camera {
                for _ in 0..4 {
                    if !there() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
            // an error because a drive went away: wait for it and try this photo again (finished copies are found again)
            if !a.error.is_empty() && !there() {
                continue;
            }
            break a;
        };
        let all_ok = attempt.error.is_empty();
        for (src, dst, sum, size) in attempt.copies {
            if let Some(j) = journal {
                j.done(&src, &dst);
            }
            b.copied += 1;
            b.bytes += size;
            b.target_files.push(dst);
            b.source_files.push(src);
            b.sums.push(sum);
        }
        for d in attempt.folder {
            if !b.folder.contains(&d) {
                b.folder.push(d);
            }
        }
        b.backed_up += attempt.backed_up;
        for (src, e) in attempt.error {
            if let Some(j) = journal {
                j.failed(&src, &e);
            }
            b.error.push(e);
        }
        set(&|s| {
            s.verified = b.copied;
            s.second = b.backed_up;
            s.errors = b.error.len();
        });
        // move: only when every file of the photo (both halves, XMP, second copy) is verified
        if let (Some(remove_fn), true) = (dispose, all_ok) {
            let mut files: Vec<PathBuf> = Vec::new();
            for k in group {
                files.push(k.path.clone());
                files.extend(sidecar_xmp(&k.path));
            }
            match remove_fn(&files) {
                Ok(()) => {
                    b.moved += 1;
                    set(&|s| s.trashed = b.moved);
                }
                Err(e) => b.error.push(format!("Original nicht in den Papierkorb gelegt: {e}")),
            }
        }
    }
    set(&|s| s.file = groups.len().saturating_sub(1));
    notify(true);
    b.duration_ms = t.elapsed().as_millis();
    b
}

/// Dispose of the originals of a moved photo: on a network drive into its own trash folder (`network_trash`, the
/// system Trash does not exist there), otherwise via `system` (the Trash of the Mac; for a card or external drive
/// that is the volume's own trash, its space is only freed when the Trash is emptied).
pub fn dispose_originals(files: &[PathBuf], source_root: Option<&Path>, is_network: &dyn Fn(&Path) -> bool, system: &dyn Fn(&[PathBuf]) -> Result<(), String>) -> Result<(), String> {
    let Some(first) = files.first() else { return Ok(()) };
    let folder = first.parent().unwrap_or(Path::new("/"));
    if is_network(folder) {
        let root = source_root.filter(|r| first.starts_with(r)).ok_or_else(|| format!("{}: kein Quellordner für den Papierkorb des Netzlaufwerks", first.display()))?;
        crate::network_trash::discard(root, files, &today()).map(|_| ())
    } else {
        system(files)
    }
}

/// Register new date folders in the library and sync – afterwards the images are under "Zuletzt importiert".
///
/// - Target inside a root folder (on this Mac, an external drive or the NAS): the new folders belong to it.
/// - Target outside all root folders: it becomes a root folder itself. If it already contains root folders (chosen a
///   level too high), the topmost folder of each import folder that contains none becomes the root folder instead
///   (import folders inside one of those root folders belong to it).
///
/// If a background run is currently locking the database, it retries for up to about a minute. If it still fails,
/// the photos are complete in the target and appear under "Nicht im Katalog", where they can be added.
pub fn register(db: &Db, target: &Path, b: &Report) -> Result<crate::reconcile::Report, String> {
    use crate::not_in_catalog::with_retry;
    let slash = |p: &Path| format!("{}/", crate::nfc(p.to_string_lossy().trim_end_matches('/')));
    let contains_root = |p: &Path| -> Result<bool, String> {
        let s = slash(p);
        Ok(db.roots().map_err(|e| e.to_string())?.iter().any(|r| r.path.starts_with(&s)))
    };
    let total = if db.root_to_path(target).is_none() && !contains_root(target)? {
        with_retry(|| crate::reconcile::add_root(db, target, &|_| {})).map(|(_, a)| a)?
    } else {
        let mut roots: Vec<i64> = db.root_to_path(target).map(|s| s.id).into_iter().collect();
        let mut total = crate::reconcile::Report::default();
        for d in &b.folder {
            if let Some(s) = db.root_to_path(d) {
                let rel = slash(d).strip_prefix(&s.path).unwrap_or("").to_string();
                with_retry(|| db.create_folder(s.id, &rel, None).map(|_| ()).map_err(|e| e.to_string()))?;
                if !roots.contains(&s.id) {
                    roots.push(s.id);
                }
                continue;
            }
            // outside all root folders, the target contains some: the topmost folder below the target without one
            let below: Vec<&Path> = d.ancestors().take_while(|a| a.starts_with(target) && *a != target).collect();
            let Some(top) = below.into_iter().rev().find(|a| !contains_root(a).unwrap_or(true)) else {
                return Err(format!("{} enthält schon Stammordner – bitte einen Ordner darin als Ziel wählen", target.display()));
            };
            let (id, a) = with_retry(|| crate::reconcile::add_root(db, top, &|_| {}))?;
            total.newly_added += a.newly_added;
            roots.retain(|&r| r != id);
        }
        for root in roots {
            let a = with_retry(|| crate::reconcile::reconcile(db, Some(root), &|_| {}))?;
            db.mark_run(a.run, "import").map_err(|e| e.to_string())?;
            total.newly_added += a.newly_added;
        }
        total
    };
    store_sums(db, b);
    Ok(total)
}

/// Store the checksums of the copies just made (partial checksum for recognising them next time, SHA-256 from the
/// verified copy). Failure only costs reading them again later.
fn store_sums(db: &Db, b: &Report) {
    let rows: Vec<(i64, Option<String>, &String)> = b
        .target_files
        .iter()
        .zip(&b.sums)
        .filter_map(|(p, sum)| {
            let file = db.image_to_path(p).and_then(|i| db.file_of(i))?;
            Some((file, crate::reconcile::partial_sum(p), sum))
        })
        .collect();
    let r = crate::not_in_catalog::with_retry(|| {
        let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        for (file, part, sum) in &rows {
            tx.execute("UPDATE file SET partial_hash = coalesce(?2, partial_hash), sha256 = ?3 WHERE id = ?1", params![file, part, sum]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    });
    if let Err(e) = r {
        eprintln!("import: checksums not stored: {e}");
    }
}

// ───────────────────────────────────────────── One import for everything

/// What happens to the photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// copy nothing: the folders with the photos are added to the catalog ("Hinzufügen")
    InPlace,
    /// copy verified into the target folder; the source stays unchanged
    #[default]
    CopyFiles,
    /// copy verified, then move the originals to the Trash
    MoveFiles,
}

impl Mode {
    pub fn key(self) -> &'static str {
        match self {
            Mode::InPlace => "inplace",
            Mode::CopyFiles => "copy",
            Mode::MoveFiles => "move",
        }
    }
    pub fn from_key(k: &str) -> Mode {
        match k {
            "inplace" => Mode::InPlace,
            "move" => Mode::MoveFiles,
            _ => Mode::CopyFiles,
        }
    }
}

/// Collection for the imported photos.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum CollectionTarget {
    #[default]
    Nothing,
    Existing(i64),
    New(String),
}

/// Settings of an import.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub mode: Mode,
    /// the chosen source folder (needed for "Hinzufügen" outside all root folders and the network trash when moving)
    pub source: Option<PathBuf>,
    pub target: Option<PathBuf>,
    pub by_date: bool,
    pub backup: Option<PathBuf>,
    pub collection: CollectionTarget,
    /// name of a collection set (e.g. "Importe") into which a *new* collection is placed; created if needed
    pub parent_name: Option<String>,
    pub keywords: Vec<String>,
    /// Only the photos of these folders (`None`: every folder with a candidate), in every mode; each folder counts on its
    /// own (a chosen subfolder of a folder left out is taken, the folder's own photos not). "Hinzufügen": the folders
    /// left out of an own root folder are remembered as excluded, so the sync does not take them in
    pub folders: Option<Vec<PathBuf>>,
    /// rating 0–5, color label (0 = none) and copyright for every imported photo (0/empty: leave as it is)
    pub stars: u8,
    pub label: u8,
    pub copyright: String,
    /// keep a journal (crash → "import the rest")
    pub journal: bool,
}

impl Options {
    /// The settings worth remembering in the journal (to import the rest after a crash).
    pub fn to_json(&self) -> serde_json::Value {
        let collection = match &self.collection {
            CollectionTarget::Nothing => serde_json::Value::Null,
            CollectionTarget::Existing(id) => serde_json::json!({"id": id}),
            CollectionTarget::New(n) => serde_json::json!({"new": n}),
        };
        serde_json::json!({
            "mode": self.mode.key(), "source": self.source, "target": self.target, "by_date": self.by_date, "backup": self.backup,
            "collection": collection, "parent": self.parent_name, "keywords": self.keywords, "stars": self.stars, "label": self.label,
            "copyright": self.copyright,
        })
    }

    pub fn from_json(v: &serde_json::Value) -> Options {
        let path = |k: &str| v[k].as_str().map(PathBuf::from);
        let collection = if let Some(id) = v["collection"]["id"].as_i64() {
            CollectionTarget::Existing(id)
        } else if let Some(n) = v["collection"]["new"].as_str() {
            CollectionTarget::New(n.to_string())
        } else {
            CollectionTarget::Nothing
        };
        Options {
            mode: Mode::from_key(v["mode"].as_str().unwrap_or("copy")),
            source: path("source"),
            target: path("target"),
            by_date: v["by_date"].as_bool().unwrap_or(true),
            backup: path("backup"),
            collection,
            parent_name: v["parent"].as_str().map(str::to_string),
            keywords: v["keywords"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            folders: None,
            stars: v["stars"].as_u64().unwrap_or(0).min(5) as u8,
            label: v["label"].as_u64().unwrap_or(0).min(9) as u8,
            copyright: v["copyright"].as_str().unwrap_or("").to_string(),
            journal: true,
        }
    }
}

/// Result of an import.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub copy_file: Report,
    /// photos newly added to the catalog
    pub added: usize,
    /// copying worked, the catalog entry did not (the photos are in the target)
    pub catalog_error: Option<String>,
    pub collection: Option<(String, usize)>,
    pub keyword_images: usize,
    pub notes: Vec<String>,
    /// the imported photos in the catalog: (path, file id) – for a develop preset applied afterwards
    pub files: Vec<(PathBuf, i64)>,
    /// image ids of `files` (same order)
    pub images: Vec<i64>,
}

/// The folders "Hinzufügen" would add: every folder with a candidate, with the number of photos and whether all of them
/// are already in the library (then it starts unchecked).
pub fn folders_of(candidates: &[Candidate]) -> Vec<(PathBuf, usize, bool)> {
    let mut m: std::collections::BTreeMap<PathBuf, (usize, bool)> = std::collections::BTreeMap::new();
    for k in candidates {
        if let Some(p) = k.path.parent() {
            let e = m.entry(p.to_path_buf()).or_insert((0, true));
            e.0 += 1;
            e.1 &= k.duplicate || k.identical;
        }
    }
    m.into_iter().map(|(p, (n, all))| (p, n, all)).collect()
}

fn add_in_place(db: &Db, candidates: &[Candidate], source: Option<&Path>, chosen: Option<&[PathBuf]>) -> Result<usize, String> {
    use crate::not_in_catalog::with_retry;
    let all: BTreeSet<PathBuf> = candidates.iter().filter_map(|k| k.path.parent().map(Path::to_path_buf)).collect();
    let folder: BTreeSet<PathBuf> = match chosen {
        Some(c) => c.iter().filter(|p| all.contains(*p)).cloned().collect(),
        None => all.clone(),
    };
    let left_out: Vec<&PathBuf> = all.iter().filter(|p| !folder.contains(*p)).collect();
    let mut known: Vec<(Root, String)> = Vec::new();
    let mut outside = false;
    for d in &folder {
        match db.root_to_path(d) {
            Some(s) => {
                let abs = format!("{}/", crate::nfc(d.to_string_lossy().trim_end_matches('/')));
                let rel = abs.strip_prefix(&s.path).unwrap_or("").to_string();
                known.push((s, rel));
            }
            None => outside = true,
        }
    }
    let mut new = 0;
    if !known.is_empty() {
        // first the chosen folders (creating one lifts exclusions around it), then the exclusions of the folders left
        // out, then one sync per root folder: an own root folder takes in every folder on disk that is not excluded
        let mut roots: Vec<Root> = Vec::new();
        for (s, rel) in &known {
            with_retry(|| db.create_folder(s.id, rel, None).map(|_| ()).map_err(|e| e.to_string()))?;
            if !roots.iter().any(|r| r.id == s.id) {
                roots.push(s.clone());
            }
        }
        for s in &roots {
            if s.origin == "own" {
                exclude_left_out(db, s.id, &s.path, &left_out, &folder, candidates)?;
            }
            let b = with_retry(|| crate::reconcile::reconcile(db, Some(s.id), &|_| {}))?;
            new += b.newly_added;
        }
    }
    if outside {
        let q = source.ok_or("kein Quellordner gewählt")?;
        if db.root_to_path(q).is_some() || left_out.is_empty() {
            let (_, a) = with_retry(|| crate::reconcile::add_root(db, q, &|_| {}))?;
            new += a.newly_added;
        } else {
            // new root folder with folders left out: remember those as excluded before the first sync reads them
            let p = format!("{}/", crate::nfc(q.to_string_lossy().trim_end_matches('/')));
            if let Some(s) = db.roots().map_err(|e| e.to_string())?.into_iter().find(|s| s.path.starts_with(&p)) {
                return Err(format!("{} enthält schon den Stammordner {} – bitte einen Ordner darin wählen", p, s.path));
            }
            let name = q.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
            let id = with_retry(|| db.create_root(&p, &name, "own", None).map_err(|e| e.to_string()))?;
            with_retry(|| db.create_folder(id, "", None).map(|_| ()).map_err(|e| e.to_string()))?;
            exclude_left_out(db, id, &p, &left_out, &folder, candidates)?;
            let a = with_retry(|| crate::reconcile::reconcile(db, Some(id), &|_| {}))?;
            db.mark_run(a.run, "folder").map_err(|e| e.to_string())?;
            new += a.newly_added;
        }
    }
    Ok(new)
}

/// "Hinzufügen" with folders left out, in an own root folder (whose sync takes in every folder it finds): each folder
/// left out is remembered as excluded, so neither this sync nor a later one takes its photos in. Each folder counts on
/// its own: a folder left out with a chosen folder below it is excluded only with its own photos (excluding the folder
/// would hide the chosen one too); without one, as a whole. A folder with photos in the library already stays
/// as it is (excluding it would remove them from the library).
fn exclude_left_out(db: &Db, root: i64, root_path: &str, left_out: &[&PathBuf], chosen: &BTreeSet<PathBuf>, candidates: &[Candidate]) -> Result<(), String> {
    use crate::not_in_catalog::with_retry;
    for d in left_out {
        let abs = format!("{}/", crate::nfc(d.to_string_lossy().trim_end_matches('/')));
        let Some(rel) = abs.strip_prefix(root_path) else { continue };
        let holds_photos = db.conn.query_row("SELECT 1 FROM file d JOIN folder o ON o.id = d.folder WHERE o.root = ?1 AND o.path = ?2 LIMIT 1", params![root, rel], |_| Ok(())).is_ok();
        if holds_photos {
            continue;
        }
        // photos of the library in a folder below: excluding the folder as a whole would hide them (marked missing)
        let photos_below = db
            .conn
            .query_row("SELECT 1 FROM file d JOIN folder o ON o.id = d.folder WHERE o.root = ?1 AND substr(o.path, 1, length(?2)) = ?2 LIMIT 1", params![root, rel], |_| Ok(()))
            .is_ok();
        let chosen_below = photos_below || chosen.iter().any(|c| c != *d && c.starts_with(d));
        let rels: Vec<String> = if chosen_below || rel.is_empty() {
            candidates
                .iter()
                .filter(|k| k.path.parent() == Some(d.as_path()))
                .filter_map(|k| k.path.file_name().map(|n| format!("{rel}{}", crate::nfc(&n.to_string_lossy()))))
                .collect()
        } else {
            vec![rel.to_string()]
        };
        for r in rels {
            with_retry(|| db.conn.execute("INSERT OR IGNORE INTO excluded_path(root, rel, time) VALUES(?1, ?2, ?3)", params![root, r, crate::now()]).map(|_| ()).map_err(|e| e.to_string()))?;
        }
    }
    Ok(())
}

/// Take over photos: depending on the mode add in place, copy or move, then register them in the
/// catalog and optionally assign a collection, keywords, rating, label and copyright. `db` opens the library only
/// after copying (no holding on to the database during long runs). `dispose` moves files to the
/// Trash (only for "move").
pub fn run_import(
    db: &dyn Fn() -> Result<Db, String>,
    candidates: &[Candidate],
    opt: &Options,
    dispose: &dyn Fn(&[PathBuf]) -> Result<(), String>,
    cancel: &AtomicBool,
    progress: &dyn Fn(State),
) -> Outcome {
    run_import_in(db, candidates, opt, dispose, cancel, progress, None)
}

/// Like [`run_import`] from a source that is not a folder (a camera, [`crate::import_source`]): only copying; "move"
/// copies as well (nothing is ever deleted on a camera), "add in place" is not possible.
#[allow(clippy::too_many_arguments)]
pub fn run_import_from(
    db: &dyn Fn() -> Result<Db, String>,
    src: &dyn crate::import_source::Source,
    candidates: &[Candidate],
    opt: &Options,
    cancel: &AtomicBool,
    progress: &dyn Fn(State),
    journal_dir: Option<&Path>,
) -> Outcome {
    if opt.mode == Mode::InPlace {
        return Outcome { catalog_error: Some("Von einer Kamera lässt sich nur kopieren".into()), ..Default::default() };
    }
    let opt = Options { mode: Mode::CopyFiles, ..opt.clone() };
    let never = |_: &[PathBuf]| -> Result<(), String> { Err("nichts wird auf der Kamera gelöscht".into()) };
    import_core(db, candidates, &opt, &never, cancel, progress, journal_dir, Some(src))
}

/// Like [`run_import`]; `journal_dir`: where the journal goes (default: [`crate::import_journal::folder`]).
pub fn run_import_in(
    db: &dyn Fn() -> Result<Db, String>,
    candidates: &[Candidate],
    opt: &Options,
    dispose: &dyn Fn(&[PathBuf]) -> Result<(), String>,
    cancel: &AtomicBool,
    progress: &dyn Fn(State),
    journal_dir: Option<&Path>,
) -> Outcome {
    import_core(db, candidates, opt, dispose, cancel, progress, journal_dir, None)
}

#[allow(clippy::too_many_arguments)]
fn import_core(
    db: &dyn Fn() -> Result<Db, String>,
    candidates: &[Candidate],
    opt: &Options,
    dispose: &dyn Fn(&[PathBuf]) -> Result<(), String>,
    cancel: &AtomicBool,
    progress: &dyn Fn(State),
    journal_dir: Option<&Path>,
    src: Option<&dyn crate::import_source::Source>,
) -> Outcome {
    use crate::not_in_catalog::with_retry;
    let mut e = Outcome::default();
    // only the chosen folders, in every mode (moving never touches a photo of a folder left out)
    let all_candidates = candidates;
    let only_chosen: Vec<Candidate>;
    let candidates: &[Candidate] = match &opt.folders {
        Some(f) => {
            only_chosen = candidates.iter().filter(|k| k.path.parent().is_some_and(|p| f.iter().any(|x| x == p))).cloned().collect();
            &only_chosen
        }
        None => candidates,
    };
    let paths: Vec<PathBuf> = match opt.mode {
        Mode::InPlace => candidates.iter().map(|k| k.path.clone()).collect(),
        Mode::CopyFiles | Mode::MoveFiles => {
            let Some(target) = &opt.target else {
                e.catalog_error = Some("kein Zielordner gewählt".into());
                return e;
            };
            let remove_fn: Option<&dyn Fn(&[PathBuf]) -> Result<(), String>> = (opt.mode == Mode::MoveFiles).then_some(dispose);
            let journal = opt.journal.then(|| {
                let planned: Vec<Planned> = candidates.iter().filter(|k| !k.duplicate).map(|k| Planned { path: k.path.clone(), size: k.size, format: k.format.clone(), tag: k.tag.clone() }).collect();
                match journal_dir {
                    Some(d) => Journal::start_in(d, opt.to_json(), &planned),
                    None => Journal::start(opt.to_json(), &planned),
                }
            });
            e.copy_file = copy_photos(candidates, target, opt.by_date, opt.backup.as_deref(), remove_fn, journal.as_ref(), src, cancel, progress);
            if let Some(j) = &journal {
                j.finish(e.copy_file.cancelled);
            }
            e.copy_file.target_files.clone()
        }
    };
    let db = match db() {
        Ok(d) => d,
        Err(x) => {
            e.catalog_error = Some(x);
            return e;
        }
    };
    let entry = match opt.mode {
        Mode::InPlace => add_in_place(&db, all_candidates, opt.source.as_deref(), opt.folders.as_deref()),
        _ => register(&db, opt.target.as_deref().unwrap_or(Path::new("")), &e.copy_file).map(|a| a.newly_added),
    };
    match entry {
        Ok(n) => e.added = n,
        Err(x) => {
            e.catalog_error = Some(x);
            return e;
        }
    }
    let found: Vec<(PathBuf, i64)> = paths.iter().filter_map(|p| db.image_to_path(p).map(|i| (p.clone(), i))).collect();
    let ids: Vec<i64> = found.iter().map(|(_, i)| *i).collect();
    for (p, i) in &found {
        if let Some(f) = db.file_of(*i) {
            e.files.push((p.clone(), f));
            e.images.push(*i);
        }
    }
    if ids.is_empty() {
        return e;
    }
    if !opt.keywords.is_empty() {
        match with_retry(|| crate::sync::change_keywords(&db, &ids, &opt.keywords, &[]).map_err(|x| x.to_string())) {
            Ok(_) => e.keyword_images = ids.len(),
            Err(x) => e.notes.push(format!("Stichwörter nicht gesetzt: {x}")),
        }
    }
    // rating and label: through the store (the library is the main store; sidecars follow if they are switched on)
    if opt.stars > 0 || opt.label > 0 {
        for (_, f) in &e.files {
            let (s, flag, l) = crate::store::meta(*f, None).unwrap_or((0, 0, 0));
            let new = (if opt.stars > 0 { opt.stars } else { s }, flag, if opt.label > 0 { opt.label } else { l });
            if new != (s, flag, l) {
                crate::store::set_meta(*f, None, new);
            }
        }
    }
    let copyright = opt.copyright.trim();
    if !copyright.is_empty() {
        let r = with_retry(|| {
            let tx = db.conn.unchecked_transaction().map_err(|x| x.to_string())?;
            for i in &ids {
                tx.execute("UPDATE image SET copyright = ?2 WHERE id = ?1", params![i, copyright]).map_err(|x| x.to_string())?;
            }
            // sidecars next to the photos carry it too (only when writing them is switched on)
            for (_, f) in &e.files {
                crate::store::queue_sidecar(&tx, *f)?;
            }
            tx.commit().map_err(|x| x.to_string())
        });
        if let Err(x) = r {
            e.notes.push(format!("Copyright nicht gesetzt: {x}"));
        }
    }
    let collection = match &opt.collection {
        CollectionTarget::Nothing => None,
        CollectionTarget::Existing(id) => db.collections().ok().and_then(|s| s.into_iter().find(|x| x.id == *id)).map(|s| (s.id, s.name)),
        CollectionTarget::New(name) => match with_retry(|| {
            let parent = match opt.parent_name.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
                Some(u) => Some(db.find_or_create_collection_set(u).map_err(|x| x.to_string())?),
                None => None,
            };
            db.create_collection_in(name, parent).map_err(|x| x.to_string())
        }) {
            Ok(id) => Some((id, name.trim().to_string())),
            Err(x) => {
                e.notes.push(format!("Sammlung nicht angelegt: {x}"));
                None
            }
        },
    };
    if let Some((id, name)) = collection {
        match with_retry(|| db.add_to_collection(id, &ids).map_err(|x| x.to_string())) {
            Ok(n) => e.collection = Some((name, n)),
            Err(x) => e.notes.push(format!("Sammlung: {x}")),
        }
    }
    e
}

#[cfg(test)]
mod tests {

    /// Day folders only from well-formed dates; damaged camera data never becomes a path or panics when sliced.
    #[test]
    fn day_tags_are_checked() {
        assert!(is_day("2026-11-14"));
        for no in ["", "2026", "2026-11-1", "ÄÄ26-11-14", "../11-1414", "2026/11/14", "2026-11-14T"] {
            assert!(!is_day(no), "{no}");
        }
        assert_eq!(crate::import_source::ptp_u32_array(&[0xff, 0xff, 0xff, 0xff, 1, 0, 0, 0]), vec![1]);
    }

    /// Placing a partial copy: never over an existing file (next free name), the partial file never stays behind.
    #[test]
    fn place_never_overwrites_and_cleans_up() {
        let d = std::env::temp_dir().join(format!("graineo-place-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("A.JPG"), b"other").unwrap();
        let part = d.join(".A.JPG.copying");
        std::fs::write(&part, b"new").unwrap();
        let sum = crate::relocate::sha256(&part).unwrap();
        let t = place(&part, &d, "A.JPG", &sum).unwrap();
        assert_eq!(t, d.join("A-1.JPG"));
        assert_eq!(std::fs::read(d.join("A.JPG")).unwrap(), b"other");
        assert_eq!(std::fs::read(&t).unwrap(), b"new");
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(&d);
    }
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-card-import-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Content of a fake camera file: `len` bytes, different from `seed` to `seed` at start and end.
    fn content(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    fn by_name<'a>(k: &'a [Candidate], n: &str) -> &'a Candidate {
        k.iter().find(|c| c.path.file_name().unwrap() == n).unwrap()
    }

    /// The two errors of the old rule: a renamed copy of a photo in the library counted as new (until a background run
    /// had filled in the capture time), and a series with fixed file size and the same capture second counted as
    /// "already there" and was skipped silently.
    #[test]
    fn renamed_copy_is_known_and_series_of_same_size_is_new() {
        let dir = tmp("erkennen");
        let photos = dir.join("Fotos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DP2M5414.JPG"), content(1, 300_000)).unwrap();
        std::fs::write(photos.join("IMG_0001.RAF"), content(2, 200_000)).unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        crate::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
        // the library knows the capture time of the RAF (as after the background run)
        db.conn.execute("UPDATE file SET capture_time = '2026-05-02T14:21:08' WHERE name = 'IMG_0001.RAF'", []).unwrap();
        let card = dir.join("Karte/DCIM/100");
        std::fs::create_dir_all(&card).unwrap();
        // renamed copy: same bytes, other name
        std::fs::write(card.join("KOPIE_5414.JPG"), content(1, 300_000)).unwrap();
        // next photo of the series: same size, (same capture second, see below), other content
        std::fs::write(card.join("IMG_0002.RAF"), content(3, 200_000)).unwrap();
        // the same photo under the old name (would be found by the old rule too)
        std::fs::write(card.join("IMG_0001.RAF"), content(2, 200_000)).unwrap();
        let mut k = list(&dir.join("Karte"));
        // capture times come from the header; the fake files have none: set them like a camera series would
        for c in &mut k {
            c.capture = Some("2026-05-02T14:21:08".into());
        }
        complete(&db, &mut k, &|_, _| {});
        let renamed = by_name(&k, "KOPIE_5414.JPG");
        assert!(renamed.duplicate, "renamed copy is recognised by its content: {renamed:?}");
        let series = by_name(&k, "IMG_0002.RAF");
        assert!(!series.duplicate && !series.probable, "same size, same second, other content → new: {series:?}");
        assert!(by_name(&k, "IMG_0001.RAF").duplicate);
        // the sums read from the library are remembered
        let stored: i64 = db.conn.query_row("SELECT count(*) FROM file WHERE partial_hash IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Library file not reachable (drive not connected): same size and name only make it "probably there" (shown,
    /// unchecked), never a silent skip; the same size alone does nothing.
    #[test]
    fn unreachable_library_file_gives_probable() {
        let dir = tmp("vermutlich");
        let photos = dir.join("Fotos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("A.JPG"), content(1, 100_000)).unwrap();
        std::fs::write(photos.join("B.JPG"), content(2, 120_000)).unwrap();
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        crate::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
        std::fs::remove_dir_all(&photos).unwrap();
        let card = dir.join("Karte");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::write(card.join("A.JPG"), content(1, 100_000)).unwrap();
        std::fs::write(card.join("C.JPG"), content(5, 120_000)).unwrap();
        let k = scan(&db, &card);
        let a = by_name(&k, "A.JPG");
        assert!(!a.duplicate && a.probable, "{a:?}");
        let c = by_name(&k, "C.JPG");
        assert!(!c.duplicate && !c.probable, "{c:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Identical copies on the card: the first by path stays, the others are marked; same size alone is not identical.
    #[test]
    fn identical_copies_within_the_source() {
        let dir = tmp("identisch");
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        let card = dir.join("Karte");
        std::fs::create_dir_all(card.join("100")).unwrap();
        std::fs::create_dir_all(card.join("101")).unwrap();
        std::fs::write(card.join("100/IMG_1.JPG"), content(7, 150_000)).unwrap();
        std::fs::write(card.join("101/IMG_1 Kopie.JPG"), content(7, 150_000)).unwrap();
        std::fs::write(card.join("100/IMG_2.JPG"), content(8, 150_000)).unwrap();
        let k = scan(&db, &card);
        assert!(!by_name(&k, "IMG_1.JPG").identical);
        assert!(by_name(&k, "IMG_1 Kopie.JPG").identical);
        assert!(!by_name(&k, "IMG_2.JPG").identical);
        assert!(k.iter().all(|c| !c.duplicate));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Moving RAW+JPEG: if one half cannot be copied, neither original is disposed of (ersatz trash, never the real one).
    #[test]
    fn move_disposes_only_when_every_copy_of_the_photo_is_verified() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp("verschieben");
        let card = dir.join("Karte");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::write(card.join("A.CR3"), b"raw-a").unwrap();
        std::fs::write(card.join("A.JPG"), b"jpg-a").unwrap();
        std::fs::write(card.join("B.CR3"), b"raw-bb").unwrap();
        std::fs::write(card.join("B.JPG"), b"jpg-bb").unwrap();
        std::fs::set_permissions(card.join("B.JPG"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let k = list(&card);
        let trash = dir.join("Ersatz-Papierkorb");
        std::fs::create_dir_all(&trash).unwrap();
        let disposed = std::sync::Mutex::new(Vec::<PathBuf>::new());
        let dispose = |files: &[PathBuf]| -> Result<(), String> {
            for f in files {
                std::fs::rename(f, trash.join(f.file_name().unwrap())).map_err(|e| e.to_string())?;
                disposed.lock().unwrap().push(f.clone());
            }
            Ok(())
        };
        let target = dir.join("Ziel");
        let b = import_with(&k, &target, false, None, Some(&dispose), &AtomicBool::new(false), &|_| {});
        std::fs::set_permissions(card.join("B.JPG"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(b.moved, 1, "{b:?}");
        assert_eq!(disposed.lock().unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), vec!["A.CR3", "A.JPG"]);
        assert!(card.join("B.CR3").exists() && card.join("B.JPG").exists(), "B stays complete on the card");
        assert_eq!(b.error.len(), 1, "{:?}", b.error);
        assert_eq!(b.sums.len(), b.target_files.len());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// From a network drive the originals go into the drive's own trash folder, not to the system Trash.
    #[test]
    fn network_source_uses_its_own_trash_folder() {
        let dir = tmp("netz");
        let root = dir.join("NAS/Fotos");
        std::fs::create_dir_all(root.join("2026")).unwrap();
        let f = root.join("2026/A.JPG");
        std::fs::write(&f, b"x").unwrap();
        let no_system = |_: &[PathBuf]| -> Result<(), String> { panic!("system Trash must not be used for a network drive") };
        dispose_originals(&[f.clone()], Some(&root), &|_| true, &no_system).unwrap();
        assert!(!f.exists());
        assert!(crate::network_trash::base(&root).join(today()).join("2026/A.JPG").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A target that goes away makes the run wait, not fail; it continues when the drive is back.
    #[test]
    fn waits_for_a_target_that_comes_back() {
        let dir = tmp("warten");
        let card = dir.join("Karte");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::write(card.join("A.JPG"), b"a").unwrap();
        std::fs::write(card.join("B.JPG"), b"bb").unwrap();
        let volume = dir.join("Laufwerk");
        let target = volume.join("Fotos");
        std::fs::create_dir_all(&target).unwrap();
        let k = list(&card);
        let waited = std::sync::atomic::AtomicBool::new(false);
        let pulled = std::sync::atomic::AtomicBool::new(false);
        let back = std::sync::Mutex::new(None);
        // after the first photo the drive goes away (folder gone) and comes back a second later
        let b = import_with(&k, &target, false, None, None, &AtomicBool::new(false), &|s| {
            if s.waiting {
                waited.store(true, Ordering::Relaxed);
            }
            if s.file == 1 && !pulled.swap(true, Ordering::Relaxed) {
                std::fs::remove_dir_all(&volume).unwrap();
                let v = volume.clone();
                *back.lock().unwrap() = Some(std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    std::fs::create_dir_all(v.join("Fotos")).unwrap();
                }));
            }
        });
        back.lock().unwrap().take().unwrap().join().unwrap();
        assert!(waited.load(Ordering::Relaxed));
        assert_eq!((b.copied, b.error.len()), (2, 0), "{b:?}");
        assert!(target.join("B.JPG").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uncached_read_back_matches() {
        let dir = tmp("nocache");
        let f = dir.join("a.bin");
        std::fs::write(&f, content(9, 3_000_000)).unwrap();
        assert_eq!(sha256_uncached(&f).unwrap(), crate::relocate::sha256(&f).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
