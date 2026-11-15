//! Import sources without a folder: a camera connected by USB that does not appear as a drive (PTP). Its photos get a
//! pseudopath `camera://<device>/<path on the camera>`, so the rest of the import (grid, RAW+JPEG pairs, journal,
//! report) works as for a card.
//!
//! - [`Source`] is what the import needs from such a source: list the files, read a range (partial checksum for
//!   "already in the library"), download one file, a thumbnail. The camera itself is in the app (ImageCaptureCore);
//!   [`FolderSource`] serves a folder in the same way and stands in for a camera in the tests (it can be "pulled").
//! - Copying ([`fetch_verified`]): the camera writes the file into a hidden folder of the target; it is then read back
//!   past the system's file cache (SHA-256), its first and last 64 KB are compared with the camera's, and only then does
//!   it appear under its name (hard link, never overwriting) – the same rules as for a card. Nothing is ever deleted
//!   on the camera.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::card_import::{self as ci, Candidate};
use crate::db::Db;

/// Start of a camera pseudopath.
pub const SCHEME: &str = "camera://";

/// Is this the pseudopath of a file on a camera?
pub fn is_camera(p: &Path) -> bool {
    p.as_os_str().as_encoded_bytes().starts_with(SCHEME.as_bytes())
}

/// Pseudopath of the file `rel` (path on the camera, `/` separated) of the device `device` (`""`: the camera itself).
pub fn camera_path(device: &str, rel: &str) -> PathBuf {
    PathBuf::from(format!("{SCHEME}{}/{rel}", device_key(device)))
}

/// A device identifier usable in a pseudopath (no `/`).
pub fn device_key(id: &str) -> String {
    id.chars().map(|c| if c == '/' || c == '\\' { '_' } else { c }).collect()
}

/// (device, path on the camera) of a pseudopath.
pub fn parts(p: &Path) -> Option<(String, String)> {
    let s = p.to_string_lossy();
    let rest = s.strip_prefix(SCHEME)?;
    let (d, r) = rest.split_once('/').unwrap_or((rest, ""));
    Some((d.to_string(), r.to_string()))
}

// ───────────────────────────────────────────── camera state (window and diagnosis)

/// State of a camera as the source column shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum CamState {
    /// connected, not opened yet
    Found,
    /// session is being opened
    Opening,
    /// the camera lists its files (percent)
    Catalog(u32),
    /// ready, with this many files
    Ready(usize),
    /// phone or camera locked, or access not confirmed on the device
    Restricted,
    /// pulled, switched off or asleep
    Gone,
    /// connected, but the camera only offers storages without photos (e.g. an info storage with licence texts
    /// instead of the memory card); the descriptions of its storages
    InfoOnly(String),
    Failed(String),
}

/// One storage of a camera as it reports it (PTP `GetStorageInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct Storage {
    pub id: u32,
    /// 1 fixed ROM, 2 removable ROM, 3 fixed RAM (internal memory), 4 removable RAM (memory card)
    pub kind: u16,
    pub description: String,
    pub label: String,
    pub capacity: u64,
    pub free: u64,
}

impl Storage {
    /// Can hold photos (memory card or internal memory, not a read-only info storage).
    pub fn holds_photos(&self) -> bool {
        matches!(self.kind, 3 | 4)
    }
}

/// Parse PTP data: `GetStorageIDs` (array of u32) – a data container header in front is skipped.
pub fn ptp_u32_array(data: &[u8]) -> Vec<u32> {
    let d = ptp_payload(data);
    let n = d.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    // at most as many as the data holds (a damaged count would run billions of times)
    (0..n.min(d.len() / 4)).filter_map(|i| d.get(4 + 4 * i..8 + 4 * i).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))).collect()
}

/// Parse PTP data of `GetStorageInfo`.
pub fn ptp_storage_info(id: u32, data: &[u8]) -> Option<Storage> {
    let d = ptp_payload(data);
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u64_at = |o: usize| d.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])));
    let kind = u16_at(0)?;
    let capacity = u64_at(6).unwrap_or(0);
    let free = u64_at(14).unwrap_or(0);
    // free objects u32 at 22, then two PTP strings
    let (description, next) = ptp_string(d, 26);
    let (label, _) = ptp_string(d, next);
    Some(Storage { id, kind, description, label, capacity, free })
}

/// A PTP string at `o`: count of UTF-16 units (with the closing zero) as u8, then the units. Returns it and the offset
/// after it.
fn ptp_string(d: &[u8], o: usize) -> (String, usize) {
    let n = d.get(o).copied().unwrap_or(0) as usize;
    let units: Vec<u16> = (0..n).filter_map(|i| d.get(o + 1 + 2 * i..o + 3 + 2 * i).map(|b| u16::from_le_bytes([b[0], b[1]]))).take_while(|&u| u != 0).collect();
    (String::from_utf16_lossy(&units), o + 1 + 2 * n)
}

/// The payload of PTP data: without a data container header (length, type 2) if there is one.
fn ptp_payload(data: &[u8]) -> &[u8] {
    if data.len() >= 12 && u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize == data.len() && u16::from_le_bytes([data[4], data[5]]) == 2 {
        &data[12..]
    } else {
        data
    }
}

#[derive(Clone)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub state: CamState,
}

/// One file of a source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceFile {
    /// path on the source, `/` separated (e.g. `DCIM/100CANON/IMG_0001.CR3`)
    pub rel: String,
    pub size: u64,
    /// capture or creation time, local, `YYYY-MM-DDTHH:MM:SS`
    pub created: Option<String>,
}

/// What the import needs from a source that is not a folder on a drive.
pub trait Source: Send + Sync {
    /// identifier of the device (part of the pseudopaths, stable across reconnecting)
    fn id(&self) -> String;
    /// name shown in the window
    fn name(&self) -> String;
    /// connected and ready (catalog read); while not, an import waits
    fn connected(&self) -> bool;
    /// all photo and video files
    fn list(&self) -> Result<Vec<SourceFile>, String>;
    /// `len` bytes from `offset` of a file
    fn read_range(&self, rel: &str, offset: u64, len: u64) -> Result<Vec<u8>, String>;
    /// write the file as `dir/name` (never overwriting); `bytes` reports progress
    fn download(&self, rel: &str, dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String>;
    /// a thumbnail with about `edge` pixels on the long side, upright
    fn thumbnail(&self, _rel: &str, _edge: u32) -> Option<image::RgbImage> {
        None
    }
}

// ───────────────────────────────────────────── active sources (for thumbnails of the window)

static ACTIVE: Mutex<Vec<Arc<dyn Source>>> = Mutex::new(Vec::new());

/// Make a source known (replaces one with the same identifier).
pub fn register(s: Arc<dyn Source>) {
    let mut g = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    let key = device_key(&s.id());
    g.retain(|x| device_key(&x.id()) != key);
    g.push(s);
}

/// Forget a source.
pub fn unregister(id: &str) {
    let key = device_key(id);
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| device_key(&x.id()) != key);
}

/// The known source with this identifier.
pub fn active(id: &str) -> Option<Arc<dyn Source>> {
    let key = device_key(id);
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|x| device_key(&x.id()) == key).cloned()
}

/// Source and path on it for a pseudopath.
pub fn of_path(p: &Path) -> Option<(Arc<dyn Source>, String)> {
    let (d, rel) = parts(p)?;
    Some((active(&d)?, rel))
}

// ───────────────────────────────────────────── reading the source

/// The photos of a source as import candidates (sorted by path; day by capture/creation time).
pub fn list(src: &dyn Source) -> Result<Vec<Candidate>, String> {
    let id = src.id();
    let mut out: Vec<Candidate> = src
        .list()?
        .into_iter()
        .filter_map(|f| {
            let name = f.rel.rsplit('/').next()?;
            if name.starts_with('.') {
                return None;
            }
            let format = crate::format_from_extension(name)?;
            // a day folder name only from a well-formed date (it becomes a folder: no '/', '..' or other characters)
            let tag = f.created.as_deref().and_then(|c| c.get(..10)).filter(|t| ci::is_day(t)).map(str::to_string).unwrap_or_else(ci::today);
            Some(Candidate { path: camera_path(&id, &f.rel), size: f.size, format: format.to_string(), tag, capture: f.created, ..Default::default() })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Like [`crate::card_import::scan_with`] for a source: `upfront` gets all photos right after listing ("already there"
/// provisionally by name and size), then partial checksums are read where a file of the same size exists and the
/// complete result is returned.
pub fn scan(db: &Db, src: &dyn Source, upfront: &dyn Fn(Vec<Candidate>), progress: &dyn Fn(usize, usize)) -> Result<Vec<Candidate>, String> {
    let mut out = list(src)?;
    for k in &mut out {
        k.duplicate = ci::known_by_name(db, k);
    }
    upfront(out.clone());
    complete(db, src, &mut out, progress);
    Ok(out)
}

/// Second step of [`scan`]: partial checksums (one file after the other: a camera answers one request at a time),
/// RAW+JPEG days, "already there" and "identical" by the same rules as for a card.
pub fn complete(db: &Db, src: &dyn Source, out: &mut [Candidate], progress: &dyn Fn(usize, usize)) {
    let mut per_size: HashMap<u64, usize> = HashMap::new();
    for k in out.iter() {
        *per_size.entry(k.size).or_default() += 1;
    }
    let need: Vec<usize> = (0..out.len()).filter(|&i| per_size[&out[i].size] > 1 || ci::library_has_size(db, out[i].size)).collect();
    let total = need.len();
    for (n, &i) in need.iter().enumerate() {
        if let Some((_, rel)) = parts(&out[i].path) {
            out[i].partial = partial_of(src, &rel, out[i].size).ok();
        }
        progress(n + 1, total);
    }
    ci::raw_days(out);
    ci::classify(db, out, None);
}

/// Partial checksum of a file on the source (first and last 64 KB, see [`crate::reconcile::partial_sum`]).
pub fn partial_of(src: &dyn Source, rel: &str, size: u64) -> Result<String, String> {
    const EDGE: u64 = 65536;
    let first = src.read_range(rel, 0, size.min(EDGE))?;
    let last = if size > 2 * EDGE { Some(src.read_range(rel, size - EDGE, EDGE)?) } else { None };
    Ok(crate::reconcile::partial_sum_of(&first, last.as_deref(), size))
}

// ───────────────────────────────────────────── copying

static TEMP_N: AtomicUsize = AtomicUsize::new(0);

/// Hidden folder of the app for one download in `dir` (own name: `.graineo-camera-<process>-<n>`).
fn temp_folder(dir: &Path) -> Result<PathBuf, String> {
    for _ in 0..8 {
        let p = dir.join(format!(".graineo-camera-{}-{}", std::process::id(), TEMP_N.fetch_add(1, Ordering::Relaxed)));
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    Err(format!("{}: kein Platz für eine Teildatei", dir.display()))
}

/// Remove an own download folder (only the files the app put there, then the folder).
fn remove_temp(p: &Path) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = std::fs::remove_dir(p);
}

/// Download folders of an earlier, interrupted run in `dir` (other process, older than an hour): removed once per
/// folder and run of the app.
fn tidy(dir: &Path) {
    static DONE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    {
        let mut g = DONE.lock().unwrap_or_else(|e| e.into_inner());
        if g.iter().any(|d| d == dir) {
            return;
        }
        g.push(dir.to_path_buf());
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let own = format!(".graineo-camera-{}-", std::process::id());
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = n.strip_prefix(".graineo-camera-") else { continue };
        let ours = rest.split('-').count() == 2 && rest.split('-').all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit()));
        if !ours || n.starts_with(&own) {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() > 3600);
        if old && e.path().is_dir() {
            remove_temp(&e.path());
        }
    }
}

/// Verified copy of a camera file into `target_dir` as `name` (if taken by other content: `NAME-1.EXT` …). Returns
/// the file and its SHA-256 (read back past the cache). See the module documentation.
pub fn fetch_verified(src: &dyn Source, k: &Candidate, target_dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<(PathBuf, String), String> {
    let (_, rel) = parts(&k.path).ok_or_else(|| format!("{}: kein Kamerapfad", k.path.display()))?;
    crate::write_target::check(target_dir)?;
    std::fs::create_dir_all(target_dir).map_err(|e| format!("{}: {e}", target_dir.display()))?;
    tidy(target_dir);
    let temp = temp_folder(target_dir)?;
    let r = (|| {
        let got = src.download(&rel, &temp, name, bytes).map_err(|e| format!("{}: {e}", k.path.display()))?;
        let (sum, first, last, len) = ci::sha256_uncached_edges(&got)?;
        if len != k.size {
            return Err(format!("{}: unvollständig übertragen ({len} von {} Bytes)", k.path.display(), k.size));
        }
        let on_disk = crate::reconcile::partial_sum_of(&first, last.as_deref(), len);
        let on_camera = match &k.partial {
            Some(p) => p.clone(),
            None => partial_of(src, &rel, k.size).map_err(|e| format!("{}: {e}", k.path.display()))?,
        };
        if on_disk != on_camera {
            return Err(format!("Prüfsumme stimmt nicht: {}", k.path.display()));
        }
        ci::place(&got, target_dir, name, &sum).map(|p| (p, sum))
    })();
    remove_temp(&temp);
    r
}

// ───────────────────────────────────────────── a folder as source (and stand-in camera for tests)

/// A folder served like a camera: the tests use it as a stand-in ("pulled" with [`FolderSource::plug`], a transfer
/// interrupted with [`FolderSource::pull_during_next_download`]), the app shows it with `GRAINEO_FAKE_CAMERA`.
pub struct FolderSource {
    root: PathBuf,
    id: String,
    name: String,
    plugged: AtomicBool,
    pull_next: AtomicBool,
    /// downloads so far (tests)
    pub downloads: AtomicUsize,
}

impl FolderSource {
    pub fn new(root: &Path, id: &str, name: &str) -> FolderSource {
        FolderSource { root: root.to_path_buf(), id: id.to_string(), name: name.to_string(), plugged: AtomicBool::new(true), pull_next: AtomicBool::new(false), downloads: AtomicUsize::new(0) }
    }

    /// Connect or "pull" the stand-in camera.
    pub fn plug(&self, on: bool) {
        self.plugged.store(on, Ordering::SeqCst);
    }

    /// The next download fails as if the cable had been pulled during the transfer (and the camera is gone).
    pub fn pull_during_next_download(&self) {
        self.pull_next.store(true, Ordering::SeqCst);
    }

    fn file(&self, rel: &str) -> Result<PathBuf, String> {
        if !self.connected() {
            return Err("Kamera nicht verbunden".into());
        }
        if rel.split('/').any(|c| c == ".." || c.is_empty()) {
            return Err(format!("{rel}: ungültiger Pfad"));
        }
        Ok(self.root.join(rel))
    }
}

/// Local time of a file time as `YYYY-MM-DDTHH:MM:SS`.
fn local_stamp(t: std::time::SystemTime) -> String {
    let s = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let z = crate::platform::local_time(s);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", z.year, z.month, z.tag, z.hour, z.minute, z.second)
}

impl Source for FolderSource {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn connected(&self) -> bool {
        self.plugged.load(Ordering::SeqCst) && self.root.is_dir()
    }
    fn list(&self) -> Result<Vec<SourceFile>, String> {
        if !self.connected() {
            return Err("Kamera nicht verbunden".into());
        }
        let mut out = Vec::new();
        let mut todo = vec![self.root.clone()];
        while let Some(d) = todo.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with('.') {
                    continue;
                }
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    todo.push(e.path());
                } else if let (Ok(m), Ok(rel)) = (e.metadata(), e.path().strip_prefix(&self.root)) {
                    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
                    out.push(SourceFile { rel, size: m.len(), created: m.modified().ok().map(local_stamp) });
                }
            }
        }
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(out)
    }
    fn read_range(&self, rel: &str, offset: u64, len: u64) -> Result<Vec<u8>, String> {
        use std::io::{Read, Seek, SeekFrom};
        let p = self.file(rel)?;
        let mut f = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut v = Vec::with_capacity(len as usize);
        f.take(len).read_to_end(&mut v).map_err(|e| e.to_string())?;
        Ok(v)
    }
    fn download(&self, rel: &str, dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String> {
        use std::io::{Read, Write};
        let p = self.file(rel)?;
        if self.pull_next.swap(false, Ordering::SeqCst) {
            self.plug(false);
            return Err("Verbindung zur Kamera unterbrochen".into());
        }
        self.downloads.fetch_add(1, Ordering::SeqCst);
        let target = dir.join(name);
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        let mut f = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        let mut n = 0u64;
        loop {
            let k = f.read(&mut buf).map_err(|e| e.to_string())?;
            if k == 0 {
                break;
            }
            out.write_all(&buf[..k]).map_err(|e| e.to_string())?;
            n += k as u64;
            bytes(n);
        }
        out.sync_all().map_err(|e| e.to_string())?;
        Ok(target)
    }
    fn thumbnail(&self, rel: &str, edge: u32) -> Option<image::RgbImage> {
        let p = self.file(rel).ok()?;
        let format = crate::format_from_extension(rel)?;
        crate::preview::load(&p, format, edge).ok().map(|(img, _)| img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card_import::{CollectionTarget, Mode, Options};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-import-source-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn content(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    /// A stand-in camera with RAW+JPEG and a lone JPEG under `DCIM/100TEST`.
    fn camera(dir: &Path) -> Arc<FolderSource> {
        let c = dir.join("Kamera/DCIM/100TEST");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("IMG_0001.CR3"), content(1, 300_000)).unwrap();
        std::fs::write(c.join("IMG_0001.JPG"), content(2, 150_000)).unwrap();
        std::fs::write(c.join("IMG_0002.JPG"), content(3, 90_000)).unwrap();
        std::fs::write(c.join("NOTIZ.TXT"), b"keine Fotodatei").unwrap();
        Arc::new(FolderSource::new(&dir.join("Kamera"), "ersatz/1", "Ersatz-Kamera"))
    }

    #[test]
    fn pseudopaths() {
        let p = camera_path("Canon/1", "DCIM/100CANON/IMG_1.CR3");
        assert_eq!(p.to_string_lossy(), "camera://Canon_1/DCIM/100CANON/IMG_1.CR3");
        assert!(is_camera(&p) && !is_camera(Path::new("/Volumes/camera://x")));
        assert_eq!(parts(&p), Some(("Canon_1".into(), "DCIM/100CANON/IMG_1.CR3".into())));
        // RAW+JPEG pairs by the same rule as on a card
        assert_eq!(ci::pair_key(&p), ci::pair_key(&camera_path("Canon/1", "DCIM/100CANON/img_1.jpg")));
        assert_eq!(parts(&camera_path("x", "")), Some(("x".into(), "".into())));
    }

    #[test]
    fn partial_sum_from_the_camera_matches_the_file() {
        let dir = tmp("teilsumme");
        let src = camera(&dir);
        for (rel, size) in [("DCIM/100TEST/IMG_0001.CR3", 300_000u64), ("DCIM/100TEST/IMG_0002.JPG", 90_000)] {
            let file = dir.join("Kamera").join(rel);
            assert_eq!(partial_of(&*src, rel, size).unwrap(), crate::reconcile::partial_sum(&file).unwrap());
            let (sum, first, last, len) = ci::sha256_uncached_edges(&file).unwrap();
            assert_eq!(sum, crate::relocate::sha256(&file).unwrap());
            assert_eq!(crate::reconcile::partial_sum_of(&first, last.as_deref(), len), crate::reconcile::partial_sum(&file).unwrap());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Copy from the stand-in camera into date folders, register, collection; the second time everything is "already
    /// there" by content (partial checksum read from the camera), even under another name.
    #[test]
    fn copy_from_camera_and_recognise_afterwards() {
        let dir = tmp("kopieren");
        let photos = dir.join("Fotos");
        std::fs::create_dir_all(&photos).unwrap();
        let db_path = dir.join("library.sqlite");
        let db = Db::open(&db_path).unwrap();
        crate::reconcile::add_root(&db, &photos, &|_| {}).unwrap();
        let src = camera(&dir);
        let k = scan(&db, &*src, &|_| {}, &|_, _| {}).unwrap();
        assert_eq!(k.len(), 3, "the text file is no photo: {k:?}");
        assert!(k.iter().all(|c| is_camera(&c.path) && !c.duplicate));
        // RAW+JPEG = one photo
        assert_eq!(ci::photos(&k).len(), 2);
        let open = || Db::open(&db_path).map_err(|e| e.to_string());
        let opt = Options { mode: Mode::MoveFiles, source: Some(camera_path(&src.id(), "")), target: Some(photos.clone()), by_date: true, collection: CollectionTarget::New("Kamera".into()), stars: 0, journal: true, ..Options::default() };
        let journal = dir.join("journal");
        let e = ci::run_import_from(&open, &*src, &k, &opt, &AtomicBool::new(false), &|_| {}, Some(&journal));
        assert_eq!((e.copy_file.copied, e.copy_file.moved, e.added), (3, 0, 3), "{e:?}");
        assert!(e.copy_file.error.is_empty() && e.catalog_error.is_none(), "{e:?}");
        assert_eq!(e.collection, Some(("Kamera".to_string(), 3)));
        // the camera keeps everything ("move" copies)
        assert_eq!(std::fs::read_dir(dir.join("Kamera/DCIM/100TEST")).unwrap().count(), 4);
        // no download folders are left in the target
        for f in &e.copy_file.folder {
            assert!(std::fs::read_dir(f).unwrap().flatten().all(|x| !x.file_name().to_string_lossy().starts_with('.')), "{f:?}");
        }
        // the copies are byte for byte the camera's files, the SHA-256 is the one of the copy
        for (t, s) in e.copy_file.target_files.iter().zip(&e.copy_file.sums) {
            let name = t.file_name().unwrap();
            assert_eq!(std::fs::read(t).unwrap(), std::fs::read(dir.join("Kamera/DCIM/100TEST").join(name)).unwrap());
            assert_eq!(&crate::relocate::sha256(t).unwrap(), s);
        }
        // the journal is finished
        assert!(crate::import_journal::unfinished_in(&journal).is_empty());
        // second look: all already there; a renamed copy on the camera too
        std::fs::write(dir.join("Kamera/DCIM/100TEST/KOPIE.JPG"), content(3, 90_000)).unwrap();
        let k = scan(&db, &*src, &|_| {}, &|_, _| {}).unwrap();
        assert!(k.iter().all(|c| c.duplicate), "{k:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The camera is pulled during a transfer: the run waits (no error per further file) and continues once it is back.
    #[test]
    fn pulled_camera_makes_the_run_wait() {
        let dir = tmp("abgezogen");
        let target = dir.join("Ziel");
        std::fs::create_dir_all(&target).unwrap();
        let src = camera(&dir);
        let k = list(&*src).unwrap();
        let waited = AtomicBool::new(false);
        let back = Mutex::new(None);
        let pulled = AtomicBool::new(false);
        let s2 = src.clone();
        let opt = Options { mode: Mode::CopyFiles, target: Some(target.clone()), by_date: false, journal: false, ..Options::default() };
        let db_path = dir.join("library.sqlite");
        let open = || Db::open(&db_path).map_err(|e| e.to_string());
        let e = ci::run_import_from(
            &open,
            &*src,
            &k,
            &opt,
            &AtomicBool::new(false),
            &|s| {
                if s.waiting {
                    waited.store(true, Ordering::Relaxed);
                }
                // second photo: the cable is pulled during its transfer and plugged in again a second later
                if s.file == 1 && !pulled.swap(true, Ordering::Relaxed) {
                    s2.pull_during_next_download();
                    let s3 = s2.clone();
                    *back.lock().unwrap() = Some(std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(1200));
                        s3.plug(true);
                    }));
                }
            },
            None,
        );
        back.lock().unwrap().take().unwrap().join().unwrap();
        assert!(waited.load(Ordering::Relaxed));
        assert_eq!((e.copy_file.copied, e.copy_file.error.len()), (3, 0), "{e:?}");
        assert!(e.copy_file.waited_secs >= 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A transfer that delivers other bytes than the camera holds is not taken (nothing appears under the name).
    #[test]
    fn wrong_bytes_are_refused() {
        struct Liar(FolderSource);
        impl Source for Liar {
            fn id(&self) -> String {
                self.0.id()
            }
            fn name(&self) -> String {
                self.0.name()
            }
            fn connected(&self) -> bool {
                true
            }
            fn list(&self) -> Result<Vec<SourceFile>, String> {
                self.0.list()
            }
            fn read_range(&self, rel: &str, offset: u64, len: u64) -> Result<Vec<u8>, String> {
                self.0.read_range(rel, offset, len)
            }
            fn download(&self, rel: &str, dir: &Path, name: &str, bytes: &mut dyn FnMut(u64)) -> Result<PathBuf, String> {
                let p = self.0.download(rel, dir, name, bytes)?;
                let mut v = std::fs::read(&p).unwrap();
                v[10] ^= 0xff;
                std::fs::write(&p, v).unwrap();
                Ok(p)
            }
        }
        let dir = tmp("falsch");
        let target = dir.join("Ziel");
        let src = camera(&dir);
        let liar = Liar(FolderSource::new(&dir.join("Kamera"), "ersatz/1", "Ersatz-Kamera"));
        let k = list(&*src).unwrap();
        let one = k.iter().find(|c| c.format == "JPG").unwrap();
        let r = fetch_verified(&liar, one, &target, "IMG_0001.JPG", &mut |_| {});
        assert!(r.unwrap_err().contains("Prüfsumme"));
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0, "neither the file nor the download folder stays");
        // the honest camera: the same file arrives
        let (p, _) = fetch_verified(&*src, one, &target, "IMG_0001.JPG", &mut |_| {}).unwrap();
        assert_eq!(p, target.join("IMG_0001.JPG"));
        // again: the same content is already there under the name → nothing new
        let (q, _) = fetch_verified(&*src, one, &target, "IMG_0001.JPG", &mut |_| {}).unwrap();
        assert_eq!(q, p);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ptp_storages() {
        // GetStorageIDs with a data container header: two storages
        let mut d = vec![24, 0, 0, 0, 2, 0, 0x04, 0x10, 1, 0, 0, 0];
        d.extend([2, 0, 0, 0, 1, 0, 1, 0, 1, 0, 8, 0]);
        assert_eq!(ptp_u32_array(&d), vec![0x0001_0001, 0x0008_0001]);
        // GetStorageInfo without header: read-only info storage "INFO", no label
        let mut s = Vec::new();
        s.extend(1u16.to_le_bytes());
        s.extend(3u16.to_le_bytes());
        s.extend(1u16.to_le_bytes());
        s.extend(1000u64.to_le_bytes());
        s.extend(10u64.to_le_bytes());
        s.extend(0u32.to_le_bytes());
        s.push(5);
        for u in "INFO\0".encode_utf16() {
            s.extend(u.to_le_bytes());
        }
        s.push(0);
        let st = ptp_storage_info(0x000F_0001, &s).unwrap();
        assert_eq!((st.kind, st.description.as_str(), st.capacity, st.free, st.holds_photos()), (1, "INFO", 1000, 10, false));
    }

    #[test]
    fn registry() {
        let dir = tmp("register");
        let src = camera(&dir);
        register(src.clone());
        let (s, rel) = of_path(&camera_path("ersatz/1", "DCIM/100TEST/IMG_0002.JPG")).unwrap();
        assert_eq!((s.name(), rel.as_str()), ("Ersatz-Kamera".to_string(), "DCIM/100TEST/IMG_0002.JPG"));
        unregister("ersatz/1");
        assert!(active("ersatz/1").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
