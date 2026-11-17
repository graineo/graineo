//! Preview cache of the library: thumbnails as JPEG in the package
//! `~/Library/Caches/graineo/graineo Previews.graineopreviews/` (macOS shows it as a single file;
//! elsewhere a normal folder `previews`). Two adjustable levels (settings `previewSmall`, `previewLarge`):
//! [`small`] (512 or 1024 px) for grid, filmstrip and search, [`large`] for single image and full screen.
//! Exactly these two levels exist; previews of other levels are tidied away in the background ([`Cache::tidy_levels`]).
//!
//! - Only for display, never for export. Can be regenerated at any time.
//! - Source: embedded preview (X3F via `x3f`, other RAWs via rawler), image files via
//!   macOS ImageIO ([`crate::imageio`]). The originals are only read. Developed previews
//!   ([`write_developed`]) take precedence: they show graineo's own development.
//! - Key: FNV-1a over path, size and modification time – if the file changes, a new
//!   thumbnail is created automatically.
//! - Offline copy (the large level) only for photos whose original is NOT on the startup volume (external drive,
//!   NAS – whether connected or not, [`Cache::set_protection`]): prepared in the background and never evicted, it
//!   is the only view once the drive is gone. Photos on this Mac get no large preview: the single image shows the
//!   small one as a placeholder and then loads the original. There is no limit in GB; below [`MIN_FREE`] of free
//!   space on the volume nothing more is written (and nothing is deleted for it).
//! - The offline copy shows the development, not the camera JPEG: for RAW and X3F the app installs a developer
//!   ([`set_developer`]); background jobs develop the photo in the chosen level (one at a time, low priority) and
//!   mark the JPEG as developed ([`Cache::is_developed`]). A camera JPEG stored meanwhile (single image before the
//!   development, "Move to …") is a stand-in that a background job replaces – first the new one, then the old one
//!   goes (atomic rename over it); it is never removed without a replacement. JPEG/HEIC originals: scaled from the
//!   original, never larger than it.
//! - [`Generator`]: background threads with three queues – visible tiles first, then jobs to do soon (after an import,
//!   stand-ins to develop), then (throttled, can be switched off) the rest of the library.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use image::RgbImage;

/// Choices of the small level (grid, filmstrip, search). Earlier choice 256 is read as 512.
pub const SMALL_CHOICES: [u32; 2] = [512, 1024];
/// Largest small level: everything above it is a large level (offline copy).
pub const SMALL_MAX: u32 = 1024;
/// "Full resolution" as a level (the embedded preview or the development unreduced; JPEG cannot be larger).
pub const FULL: u32 = 65535;
/// Choices of the large level, the offline copy for photos on other drives (setting `previewLarge`).
pub const LARGE_CHOICES: [u32; 3] = [2560, 4000, FULL];
pub const SMALL_DEFAULT: u32 = 512;
pub const LARGE_DEFAULT: u32 = 2560;
/// One GB (decimal, as the Finder counts).
pub const GB: u64 = 1_000_000_000;
/// Below this much free space nothing more is written, protected previews included; nothing is deleted for it
/// either (`GRAINEO_PREVIEW_MIN_FREE_GB`).
pub const MIN_FREE: u64 = 20 << 30;
/// Rough size of one large preview per level when there are none yet to measure (bytes; JPEG quality 82).
pub fn large_estimate(level: u32) -> u64 {
    match level {
        0..=2560 => 800_000,
        2561..=4000 => 2_500_000,
        _ => 10_000_000,
    }
}
const QUALITY: u8 = 82;

/// Develops the offline copy of a RAW or X3F (installed by the app, which knows the development): path, level →
/// image (upright, with crop), at most `level` px.
pub type Developer = dyn Fn(&Path, u32) -> Result<RgbImage, String> + Send + Sync;

static DEVELOPER: std::sync::OnceLock<Box<Developer>> = std::sync::OnceLock::new();

/// Install the developer for offline copies (once, at startup of the app).
pub fn set_developer(d: Box<Developer>) {
    let _ = DEVELOPER.set(d);
}

/// Is the offline copy of this format made by the development (RAW, X3F – with a developer installed)?
pub fn develops(format: &str) -> bool {
    DEVELOPER.get().is_some() && crate::is_raw(format)
}

/// Comment segment right after the start of a JPEG: this preview shows the development (not the camera JPEG).
const DEVELOPED_MARK: &[u8] = b"graineo:developed";
/// What [`has_mark`] checks: only the end of the comment. The part before it names the program and is not compared,
/// so previews in the cache stay valid when it changes (the cache is never rewritten; regenerating every developed
/// preview would cost hours of computing).
const DEVELOPED_SUFFIX: &[u8] = b":developed";

fn with_mark(buf: Vec<u8>) -> Vec<u8> {
    if buf.len() < 2 || buf[..2] != [0xFF, 0xD8] {
        return buf;
    }
    let len = (DEVELOPED_MARK.len() + 2) as u16;
    let mut out = Vec::with_capacity(buf.len() + DEVELOPED_MARK.len() + 4);
    out.extend_from_slice(&buf[..2]);
    out.extend_from_slice(&[0xFF, 0xFE]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(DEVELOPED_MARK);
    out.extend_from_slice(&buf[2..]);
    out
}

/// Does a preview file (its bytes) show the development? (see [`Cache::is_developed`])
pub fn is_developed_bytes(b: &[u8]) -> bool {
    b.len() >= 32 && has_mark(&b[..32])
}

fn has_mark(head: &[u8]) -> bool {
    if head.len() < 6 || head[..4] != [0xFF, 0xD8, 0xFF, 0xFE] {
        return false;
    }
    let n = (u16::from_be_bytes([head[4], head[5]]) as usize).saturating_sub(2);
    head.get(6..6 + n).is_some_and(|t| t.len() > DEVELOPED_SUFFIX.len() && t.ends_with(DEVELOPED_SUFFIX))
}

/// Stable key (FNV-1a 64) – independent of Rust version and database IDs.
pub fn cache_key(path: &str, size: i64, mtime: i64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes().chain(size.to_le_bytes()).chain(mtime.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn env_gb(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|s| s.trim().parse::<f64>().ok()).map(|g| (g * (1u64 << 30) as f64) as u64)
}

// ───────────────────────────────────────────── Levels

static SMALL_LEVEL: AtomicU32 = AtomicU32::new(0);
static LARGE_LEVEL: AtomicU32 = AtomicU32::new(0);
/// Levels changed since the library last looked ([`take_levels_changed`]).
static LEVELS_CHANGED: AtomicBool = AtomicBool::new(false);
/// "Regenerate previews" requested ([`take_regenerate`]).
static REGENERATE: AtomicBool = AtomicBool::new(false);

/// Small level from a setting value (unknown values and the earlier 256: default 512).
pub fn parse_small(v: &serde_json::Value) -> u32 {
    v.as_u64().map(|n| n as u32).filter(|n| SMALL_CHOICES.contains(n)).unwrap_or(SMALL_DEFAULT)
}

/// Large level from a setting value: 2560, 4000 or "full" (unknown values: default).
pub fn parse_large(v: &serde_json::Value) -> u32 {
    if v.as_str() == Some("full") {
        return FULL;
    }
    v.as_u64().map(|n| n as u32).filter(|n| LARGE_CHOICES.contains(n) && *n != FULL).unwrap_or(LARGE_DEFAULT)
}

/// Setting value of a large level.
pub fn large_value(level: u32) -> serde_json::Value {
    if level == FULL {
        serde_json::json!("full")
    } else {
        serde_json::json!(level)
    }
}

fn load_levels() {
    if SMALL_LEVEL.load(Ordering::Relaxed) == 0 {
        let s = crate::settings::read();
        let _ = SMALL_LEVEL.compare_exchange(0, parse_small(s.get("previewSmall").unwrap_or(&serde_json::Value::Null)), Ordering::Relaxed, Ordering::Relaxed);
        let _ = LARGE_LEVEL.compare_exchange(0, parse_large(s.get("previewLarge").unwrap_or(&serde_json::Value::Null)), Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Edge length of the small level (grid, filmstrip, search).
pub fn small() -> u32 {
    load_levels();
    SMALL_LEVEL.load(Ordering::Relaxed)
}

/// Edge length of the large level (single image, full screen); [`FULL`] = unreduced.
pub fn large() -> u32 {
    load_levels();
    LARGE_LEVEL.load(Ordering::Relaxed)
}

/// The two levels in use (small, large).
pub fn active_levels(small: u32, large: u32) -> [u32; 2] {
    [small, large]
}

/// Choose levels (settings `previewSmall`, `previewLarge`); the library then regenerates in the background.
pub fn set_levels(small_level: u32, large_level: u32) {
    let s = if SMALL_CHOICES.contains(&small_level) { small_level } else { SMALL_DEFAULT };
    let l = if LARGE_CHOICES.contains(&large_level) { large_level } else { LARGE_DEFAULT };
    if (s, l) == (small(), large()) {
        return;
    }
    SMALL_LEVEL.store(s, Ordering::Relaxed);
    LARGE_LEVEL.store(l, Ordering::Relaxed);
    crate::settings::set("previewSmall", serde_json::json!(s));
    crate::settings::set("previewLarge", large_value(l));
    LEVELS_CHANGED.store(true, Ordering::SeqCst);
}

/// Did the levels change? (Resets the flag.)
pub fn take_levels_changed() -> bool {
    LEVELS_CHANGED.swap(false, Ordering::SeqCst)
}

/// Ask the library to regenerate all previews (those of unreachable photos stay).
pub fn request_regenerate() {
    REGENERATE.store(true, Ordering::SeqCst);
}

pub fn take_regenerate() -> bool {
    REGENERATE.swap(false, Ordering::SeqCst)
}

/// Large level (offline copy): everything above the small levels.
pub fn is_heavy(level: u32) -> bool {
    level > SMALL_MAX
}

// ───────────────────────────────────────────── Package folder and moving in

/// Moves the previous cache folder `old` into `new` by renaming (never copying). If `new` does not exist
/// yet, each entry is renamed over; if it exists, only what is missing there is moved and the rest of the old folder
/// is removed, so nothing takes up space twice. The subfolder `developed` stays (it belongs to the data folder
/// in test runs). Returns: number of entries moved. Errors leave the remainder in place – the cache then simply
/// fills again.
pub fn move_into(old: &Path, new: &Path) -> Result<usize, String> {
    if !old.is_dir() || old == new {
        return Ok(0);
    }
    std::fs::create_dir_all(new).map_err(|e| format!("{}: {e}", new.display()))?;
    let mut moved = 0;
    let mut errors = Vec::new();
    for e in std::fs::read_dir(old).map_err(|e| format!("{}: {e}", old.display()))?.flatten() {
        let name = e.file_name();
        if name == "developed" {
            continue;
        }
        let target = new.join(&name);
        if !target.exists() {
            match std::fs::rename(e.path(), &target) {
                Ok(()) => moved += 1,
                Err(err) => errors.push(format!("{}: {err}", e.path().display())),
            }
            continue;
        }
        // present in both: merge one level down (files of the 2-hex folders, developed-grid)
        if e.path().is_dir() && target.is_dir() {
            if let Ok(rd) = std::fs::read_dir(e.path()) {
                for f in rd.flatten() {
                    let t = target.join(f.file_name());
                    if t.exists() {
                        let _ = std::fs::remove_file(f.path());
                    } else if std::fs::rename(f.path(), &t).is_ok() {
                        moved += 1;
                    }
                }
            }
            let _ = std::fs::remove_dir(e.path());
        } else if e.path().is_file() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = std::fs::remove_dir(old);
    if errors.is_empty() {
        Ok(moved)
    } else {
        Err(errors.join("; "))
    }
}

// ───────────────────────────────────────────── Cache

struct Entry {
    bytes: u64,
    /// last used (counter, larger = more recent)
    last: u64,
}

struct State {
    files: HashMap<(u64, u32), Entry>,
    sum_total: u64,
    clock: u64,
    /// keys of photos whose original is not on the startup volume or not reachable: never evicted
    protected: HashSet<u64>,
    /// of these: not reachable right now (also kept on "regenerate")
    unreachable: HashSet<u64>,
    /// counts the calls of [`Cache::set_protection`]
    protection_generation: u64,
    /// protection was set at least once: before that nothing is evicted (the app doesn't know yet which
    /// photos lie on other drives)
    protection_known: bool,
    /// bytes of the small levels (never evicted)
    small_bytes: u64,
    /// bytes of the large levels of photos on the startup volume (left over from earlier versions, tidied away)
    large_bytes: u64,
    /// bytes of the large levels of protected photos (other drives, unreachable; never evicted)
    large_protected_bytes: u64,
    /// number of large previews (all photos)
    large_files: usize,
}

impl State {
    fn add(&mut self, k: (u64, u32), bytes: u64, last: u64) {
        if let Some(old) = self.files.insert(k, Entry { bytes, last }) {
            self.account(k, old.bytes, false);
        }
        self.account(k, bytes, true);
    }

    fn take(&mut self, k: (u64, u32)) -> bool {
        match self.files.remove(&k) {
            Some(e) => {
                self.account(k, e.bytes, false);
                true
            }
            None => false,
        }
    }

    fn account(&mut self, k: (u64, u32), bytes: u64, add: bool) {
        if is_heavy(k.1) {
            self.large_files = if add { self.large_files + 1 } else { self.large_files.saturating_sub(1) };
        }
        let slot = if !is_heavy(k.1) {
            &mut self.small_bytes
        } else if self.protected.contains(&k.0) {
            &mut self.large_protected_bytes
        } else {
            &mut self.large_bytes
        };
        if add {
            *slot += bytes;
            self.sum_total += bytes;
        } else {
            *slot = slot.saturating_sub(bytes);
            self.sum_total = self.sum_total.saturating_sub(bytes);
        }
    }

    fn recount(&mut self) {
        let (mut sm, mut la, mut lp) = (0, 0, 0);
        for (k, e) in &self.files {
            if !is_heavy(k.1) {
                sm += e.bytes;
            } else if self.protected.contains(&k.0) {
                lp += e.bytes;
            } else {
                la += e.bytes;
            }
        }
        (self.small_bytes, self.large_bytes, self.large_protected_bytes) = (sm, la, lp);
    }
}

/// Usage of the preview store in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// small level (never evicted)
    pub small: u64,
    /// large level of photos whose original is on this Mac (left over from earlier versions, tidied away)
    pub large: u64,
    /// large level of photos whose original is on other drives or not reachable (never evicted)
    pub large_other: u64,
    /// number of large previews
    pub large_files: usize,
}

/// The cache on disk.
pub struct Cache {
    dir: PathBuf,
    pub min_free: u64,
    state: Mutex<State>,
    paused: AtomicBool,
}

/// File of a thumbnail in the cache folder `dir` (two hex digits of the key as subfolder).
pub fn file_in(dir: &Path, key: u64, level: u32) -> PathBuf {
    dir.join(format!("{:02x}", key >> 56)).join(format!("{key:016x}-{level}.jpg"))
}

/// Is this a folder of the cache (two hex digits, e.g. `3f`)?
fn is_shard(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Cache {
    /// Opens the cache and reads the inventory (file names + size, older ones evicted first).
    pub fn open(dir: &Path) -> Cache {
        let _ = std::fs::create_dir_all(dir);
        let mut list: Vec<((u64, u32), u64, std::time::SystemTime)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for sub in rd.flatten() {
                // only the key folders (not developed-grid etc.)
                if !sub.file_name().to_str().is_some_and(is_shard) {
                    continue;
                }
                let Ok(rd2) = std::fs::read_dir(sub.path()) else { continue };
                for f in rd2.flatten() {
                    let name = f.file_name();
                    let Some(n) = name.to_str() else { continue };
                    let Some((k, rest)) = n.split_once('-') else { continue };
                    let (Ok(k), Some(s)) = (u64::from_str_radix(k, 16), rest.strip_suffix(".jpg").and_then(|s| s.parse::<u32>().ok())) else { continue };
                    if let Ok(m) = f.metadata() {
                        list.push(((k, s), m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
                    }
                }
            }
        }
        list.sort_by_key(|e| e.2);
        let mut state = State { files: HashMap::with_capacity(list.len()), sum_total: 0, clock: 0, protected: HashSet::new(), unreachable: HashSet::new(), protection_known: false, protection_generation: 0, small_bytes: 0, large_bytes: 0, large_protected_bytes: 0, large_files: 0 };
        for (i, (k, b, _)) in list.into_iter().enumerate() {
            state.add(k, b, i as u64);
        }
        state.clock = state.files.len() as u64;
        Cache {
            dir: dir.to_path_buf(),
            min_free: env_gb("GRAINEO_PREVIEW_MIN_FREE_GB").unwrap_or(MIN_FREE),
            state: Mutex::new(state),
            paused: AtomicBool::new(false),
        }
    }

    /// The cache at its standard place; on first use the previous folder is moved into the package.
    pub fn standard() -> Cache {
        static MOVED: std::sync::Once = std::sync::Once::new();
        let dir = crate::cache_folder();
        MOVED.call_once(|| {
            if let Some(old) = crate::legacy_cache_folder() {
                if old.is_dir() {
                    match move_into(&old, &dir) {
                        Ok(n) => eprintln!("previews: {n} entries moved from {} to {}", old.display(), dir.display()),
                        Err(e) => eprintln!("previews: moving from {} incomplete ({e}); missing ones are regenerated", old.display()),
                    }
                }
            }
        });
        Self::open(&dir)
    }

    pub fn folder(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, key: u64, level: u32) -> PathBuf {
        file_in(&self.dir, key, level)
    }

    pub fn has(&self, key: u64, level: u32) -> bool {
        self.state.lock().unwrap().files.contains_key(&(key, level))
    }

    /// Usage (bytes, files).
    pub fn occupied(&self) -> (u64, usize) {
        let s = self.state.lock().unwrap();
        (s.sum_total, s.files.len())
    }

    /// Keys of photos whose original lies on another drive than the startup volume (`other_drives`, external or
    /// network, whether connected or not) or is not reachable right now (`unreachable`): their previews are never
    /// evicted – they are the only view once the drive is gone. Replaces the previous sets. Until the first call
    /// nothing is evicted at all.
    pub fn set_protection(&self, other_drives: HashSet<u64>, unreachable: HashSet<u64>) {
        let mut s = self.state.lock().unwrap();
        s.protected = other_drives;
        s.protected.extend(unreachable.iter().copied());
        s.unreachable = unreachable;
        s.protection_known = true;
        s.protection_generation += 1;
        s.recount();
    }

    /// Has the protection been set at least once (before that the app does not know which photos lie on other
    /// drives, and nothing may be trimmed)?
    pub fn protection_known(&self) -> bool {
        self.state.lock().unwrap().protection_known
    }

    /// Inventory for trimming ([`crate::cache_limit`]): (key, level, bytes, LRU counter; larger = more recent).
    pub fn inventory(&self) -> Vec<(u64, u32, u64, u64)> {
        self.state.lock().unwrap().files.iter().map(|(k, e)| (k.0, k.1, e.bytes, e.last)).collect()
    }

    /// Is this photo protected (original on another drive or not reachable)? Only these get their large preview
    /// prepared in the background; for photos on this Mac it is made when viewed (the original loads fast anyway).
    pub fn is_protected(&self, key: u64) -> bool {
        self.state.lock().unwrap().protected.contains(&key)
    }

    /// Changes with every [`Cache::set_protection`] (0 = not known yet).
    pub fn protection_generation(&self) -> u64 {
        self.state.lock().unwrap().protection_generation
    }

    /// Number of photos not reachable right now.
    pub fn unreachable_count(&self) -> usize {
        self.state.lock().unwrap().unreachable.len()
    }

    /// Usage split into small, large (original on this Mac) and large (original on other drives or not reachable).
    pub fn usage(&self) -> Usage {
        let s = self.state.lock().unwrap();
        Usage { small: s.small_bytes, large: s.large_bytes, large_other: s.large_protected_bytes, large_files: s.large_files }
    }

    /// Free space on the volume of the store (bytes).
    pub fn free_space(&self) -> Option<u64> {
        crate::free(&self.dir)
    }

    /// Writing paused (not enough space on the volume)?
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Read a thumbnail (decoded, RGB). Records the use for the LRU order.
    pub fn read(&self, key: u64, level: u32) -> Option<RgbImage> {
        {
            let mut s = self.state.lock().unwrap();
            s.clock += 1;
            let clock = s.clock;
            s.files.get_mut(&(key, level))?.last = clock;
        }
        let bytes = std::fs::read(self.path(key, level)).ok()?;
        match image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) {
            Ok(img) => Some(img.to_rgb8()),
            Err(_) => {
                self.remove(key, level);
                None
            }
        }
    }

    /// All levels present for `key` (ascending).
    pub fn levels_of(&self, key: u64) -> Vec<u32> {
        let s = self.state.lock().unwrap();
        let mut v: Vec<u32> = s.files.keys().filter(|k| k.0 == key).map(|k| k.1).collect();
        v.sort_unstable();
        v
    }

    /// The level present for `key` closest to `level` (preferably larger), e.g. after a change of the levels
    /// while the original is not reachable.
    pub fn nearest_level(&self, key: u64, level: u32) -> Option<u32> {
        let s = self.state.lock().unwrap();
        let levels: Vec<u32> = s.files.keys().filter(|k| k.0 == key).map(|k| k.1).collect();
        best_level(&levels, level)
    }

    /// Remove an entry (file and index), e.g. because a developed preview takes its place.
    pub fn remove(&self, key: u64, level: u32) {
        self.state.lock().unwrap().take((key, level));
        let _ = std::fs::remove_file(self.path(key, level));
    }

    /// Remove all levels of a key.
    pub fn remove_all(&self, key: u64) {
        let levels: Vec<u32> = {
            let mut s = self.state.lock().unwrap();
            let l: Vec<u32> = s.files.keys().filter(|k| k.0 == key).map(|k| k.1).collect();
            for &x in &l {
                s.take((key, x));
            }
            l
        };
        for l in levels {
            let _ = std::fs::remove_file(self.path(key, l));
        }
    }

    /// Write a thumbnail (JPEG). `false` if space on the volume is too low.
    pub fn write(&self, key: u64, level: u32, img: &RgbImage) -> bool {
        self.write_with(key, level, img, false)
    }

    /// Write a preview that shows the development (marked, see [`Cache::is_developed`]).
    pub fn write_marked(&self, key: u64, level: u32, img: &RgbImage) -> bool {
        self.write_with(key, level, img, true)
    }

    /// Does the stored preview show the development (not the camera JPEG)? Reads only the start of the file.
    pub fn is_developed(&self, key: u64, level: u32) -> bool {
        use std::io::Read;
        let mut head = [0u8; 32];
        std::fs::File::open(self.path(key, level)).and_then(|mut f| f.read_exact(&mut head)).is_ok() && has_mark(&head)
    }

    /// Free space below the minimum: pause writing (also before a development that would not be stored anyway).
    fn check_space(&self) -> bool {
        let ok = crate::free(&self.dir).unwrap_or(0) >= self.min_free;
        self.paused.store(!ok, Ordering::Relaxed);
        ok
    }

    fn write_with(&self, key: u64, level: u32, img: &RgbImage, developed: bool) -> bool {
        let free = crate::free(&self.dir).unwrap_or(0);
        if free < self.min_free {
            self.paused.store(true, Ordering::Relaxed);
            return false;
        }
        self.paused.store(false, Ordering::Relaxed);
        let mut buf = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, QUALITY);
        if img.write_with_encoder(enc).is_err() {
            return false;
        }
        let buf = if developed { with_mark(buf) } else { buf };
        let target = self.path(key, level);
        if let Some(d) = target.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = target.with_extension("tmp");
        if std::fs::write(&tmp, &buf).is_err() || std::fs::rename(&tmp, &target).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        let mut st = self.state.lock().unwrap();
        st.clock += 1;
        let clock = st.clock;
        st.add((key, level), buf.len() as u64, clock);
        true
    }

    /// The original moved (new path → new key): its previews move along under the new key (rename within the cache, nothing
    /// is computed again). An entry already present under the new key stays. Returns: number of entries moved.
    pub fn rekey(&self, old: u64, new: u64) -> usize {
        if old == new {
            return 0;
        }
        let entries: Vec<(u32, Entry)> = {
            let mut s = self.state.lock().unwrap();
            let levels: Vec<u32> = s.files.keys().filter(|k| k.0 == old).map(|k| k.1).collect();
            levels
                .into_iter()
                .filter_map(|l| {
                    let e = s.files.get(&(old, l)).map(|e| Entry { bytes: e.bytes, last: e.last })?;
                    s.take((old, l));
                    Some((l, e))
                })
                .collect()
        };
        let mut n = 0;
        for (l, e) in entries {
            let (a, b) = (self.path(old, l), self.path(new, l));
            if let Some(d) = b.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let mut s = self.state.lock().unwrap();
            if s.files.contains_key(&(new, l)) || b.exists() {
                drop(s);
                let _ = std::fs::remove_file(&a);
                continue;
            }
            if std::fs::rename(&a, &b).is_ok() {
                s.add((new, l), e.bytes, e.last);
                n += 1;
            }
        }
        n
    }

    /// Delete everything except the previews of unreachable photos (those of reachable photos on other drives are
    /// made again from the original). Returns: number removed.
    pub fn clear_unprotected(&self) -> usize {
        let gone: Vec<(u64, u32)> = {
            let mut s = self.state.lock().unwrap();
            let v: Vec<(u64, u32)> = s.files.keys().copied().filter(|k| !s.unreachable.contains(&k.0)).collect();
            for &k in &v {
                s.take(k);
            }
            v
        };
        for &(k, l) in &gone {
            let _ = std::fs::remove_file(self.path(k, l));
        }
        gone.len()
    }

    /// Tidy away previews of levels other than `small` and `large` (earlier levels, e.g. 256 or the former grid
    /// level 1024) and the large previews of photos on this Mac (they only get the small one). Missing previews of the chosen levels are first derived from a larger level present, without
    /// the original: the small one for every photo, the large one for protected photos. A superfluous preview of a
    /// protected photo (other drive, not reachable) only goes once the chosen level of its kind (small/large) exists,
    /// so such a photo never ends up without a view. `go_on` is asked between photos (stops early, e.g. when the
    /// levels changed again). Returns: (derived, removed).
    pub fn tidy_levels(&self, small: u32, large: u32, go_on: &dyn Fn() -> bool) -> (usize, usize) {
        let (per_key, protected) = {
            let s = self.state.lock().unwrap();
            let mut m: HashMap<u64, Vec<u32>> = HashMap::new();
            for &(k, l) in s.files.keys() {
                m.entry(k).or_default().push(l);
            }
            (m, s.protected.clone())
        };
        let (mut derived, mut removed) = (0, 0);
        for (key, levels) in per_key {
            if !go_on() {
                break;
            }
            let is_protected = protected.contains(&key);
            let mut have: Vec<u32> = levels.clone();
            for target in [small, large] {
                if have.contains(&target) || (target == large && !is_protected) {
                    continue;
                }
                let Some(src) = levels.iter().copied().filter(|&l| l > target).min() else { continue };
                let Some(b) = std::fs::read(self.path(key, src)).ok() else { continue };
                let marked = has_mark(&b);
                let Some(img) = jpeg_downscaled(&b, target) else { continue };
                if self.write_with(key, target, &downscale(img, target), marked) {
                    have.push(target);
                    derived += 1;
                }
            }
            for l in levels {
                // photos on this Mac keep only the small level (no offline copy)
                if l == small || (l == large && is_protected) {
                    continue;
                }
                let replacement = if l <= SMALL_MAX { small } else { large };
                if is_protected && !have.contains(&replacement) {
                    continue;
                }
                self.remove(key, l);
                removed += 1;
            }
        }
        (derived, removed)
    }

    /// Delete everything.
    pub fn clear(&self) {
        let mut s = self.state.lock().unwrap();
        for (k, st) in s.files.keys().copied().collect::<Vec<_>>() {
            let _ = std::fs::remove_file(self.path(k, st));
        }
        s.files.clear();
        s.sum_total = 0;
        s.large_files = 0;
        s.recount();
    }
}

/// Rough space for the offline copies of `photos` photos: the mean size of the offline copies present (once there
/// are at least 20 to go by), otherwise [`large_estimate`] of the level.
pub fn projected_offline(u: &Usage, photos: usize, level: u32) -> u64 {
    let per = if u.large_files >= 20 { (u.large + u.large_other) / u.large_files as u64 } else { large_estimate(level) };
    per * photos as u64
}

/// Do the offline copies still missing fit? `projected` (all), `present` (already there), `free` on the volume,
/// `min_free` must stay free. Returns the space they lack (`None`: they fit).
pub fn offline_shortfall(projected: u64, present: u64, free: u64, min_free: u64) -> Option<u64> {
    let missing = projected.saturating_sub(present);
    let usable = free.saturating_sub(min_free);
    (missing > usable).then(|| missing - usable)
}

/// Among the available levels, the one that serves `wanted` best: the smallest that is at least as large,
/// otherwise the largest.
pub fn best_level(available: &[u32], wanted: u32) -> Option<u32> {
    available.iter().copied().filter(|&l| l >= wanted).min().or_else(|| available.iter().copied().max())
}

/// Extra knowledge from generation (for the database; only empty fields are filled in).
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub capture: Option<String>,
    pub camera: Option<String>,
}

pub fn downscale(img: RgbImage, max: u32) -> RgbImage {
    if img.width().max(img.height()) <= max {
        return img;
    }
    let k = max as f32 / img.width().max(img.height()) as f32;
    let (w, h) = (((img.width() as f32 * k).round() as u32).max(1), ((img.height() as f32 * k).round() as u32).max(1));
    // intermediate step with a fast box average, then cleanly to the target
    let img = if img.width() > w * 3 { image::imageops::thumbnail(&img, w * 2, h * 2) } else { img };
    image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
}

fn rotate_quarter(img: RgbImage, q: u32) -> RgbImage {
    match q % 4 {
        1 => image::imageops::rotate90(&img),
        2 => image::imageops::rotate180(&img),
        3 => image::imageops::rotate270(&img),
        _ => img,
    }
}

fn orient(img: RgbImage, o: u16) -> RgbImage {
    use image::imageops::*;
    match o {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

/// Embedded preview of an X3F (largest), upright.
/// Decode a JPEG downscaled right away: the decoder computes only 1/2, 1/4 or 1/8 of the pixels
/// (the smallest level that still reaches `max`). For thumbnails from large embedded previews this is many
/// times faster than full decoding and downscaling. Result not yet scaled to `max`.
pub fn jpeg_downscaled(b: &[u8], max: u32) -> Option<RgbImage> {
    use jpeg_decoder::PixelFormat;
    let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(b));
    d.read_info().ok()?;
    let m = max.clamp(1, u16::MAX as u32) as u16;
    d.scale(m, m).ok()?;
    let px = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let rgb = match info.pixel_format {
        PixelFormat::RGB24 => px,
        PixelFormat::L8 => px.iter().flat_map(|&g| [g, g, g]).collect(),
        PixelFormat::L16 => px.chunks_exact(2).flat_map(|g| [g[0], g[0], g[0]]).collect(),
        PixelFormat::CMYK32 => px.chunks_exact(4).flat_map(|c| { let k = 255 - c[3] as u32; [c[0], c[1], c[2]].map(|v| ((255 - v as u32) * k / 255) as u8) }).collect(),
    };
    RgbImage::from_raw(w, h, rgb)
}

fn jpeg_or_full(b: &[u8], max: u32) -> Result<RgbImage, String> {
    match jpeg_downscaled(b, max) {
        Some(img) => Ok(img),
        None => Ok(image::load_from_memory_with_format(b, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?.to_rgb8()),
    }
}

fn x3f_preview(path: &Path, max: u32) -> Result<RgbImage, String> {
    let img = match x3f::read_embedded_jpeg(path, true) {
        Ok(b) => jpeg_or_full(&b, max)?,
        Err(_) => {
            let (w, h, rgb) = x3f::read_embedded_plain(path, true).ok_or("X3F ohne eingebettete Vorschau")?;
            RgbImage::from_raw(w, h, rgb).ok_or("X3F-Vorschau unlesbar")?
        }
    };
    let q = x3f::read_header_rotation(path).map_or(0, |deg| (deg / 90) % 4);
    Ok(rotate_quarter(img, q))
}

/// CR3: take the preview JPEG (PRVW, usually 1620 × 1080) directly from the start of the file, without
/// decoding the whole ISO BMFF structure (rawler decodes the large JPEG for this, ~10× slower).
/// Layout: box size (4 bytes), `PRVW`, 12-byte header (incl. width/height), JPEG length (4 bytes), JPEG.
fn cr3_header(path: &Path) -> Option<(Vec<u8>, std::ops::Range<usize>)> {
    use std::io::Read;
    // PRVW usually sits at ~90 KB: read 768 KB first, continue up to 4 MB only if needed
    // (on a memory card every megabyte read counts)
    let mut f = std::fs::File::open(path).ok()?;
    let mut header: Vec<u8> = Vec::new();
    for limit in [768usize << 10, 4 << 20] {
        let old = header.len();
        header.resize(limit, 0);
        let mut read_result = old;
        while read_result < header.len() {
            match f.read(&mut header[read_result..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => read_result += k,
            }
        }
        header.truncate(read_result);
        let found = (|| {
            let pos = header.windows(4).position(|w| w == b"PRVW")?;
            let soi = pos + header[pos..].windows(3).position(|w| w == [0xff, 0xd8, 0xff])?;
            let length = u32::from_be_bytes(header.get(soi - 4..soi)?.try_into().ok()?) as usize;
            Some((soi, length, soi.checked_add(length)?))
        })();
        if let Some((soi, length, end)) = found {
            if length > 1000 && end <= header.len() {
                return Some((header, soi..end));
            }
        }
        if read_result < limit {
            break;
        }
    }
    None
}

/// TIFF block (CMT1/CMT2 of a CR3): value of an entry in the first IFD – number (SHORT/LONG) or text.
fn tiff_value(t: &[u8], tag: u16) -> Option<Result<u32, String>> {
    let le = t.get(0..2)? == b"II";
    let u16_ = |o: usize| t.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_ = |o: usize| t.get(o..o + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let ifd = u32_(4)? as usize;
    for i in 0..u16_(ifd)? as usize {
        let e = ifd + 2 + i * 12;
        if u16_(e)? != tag {
            continue;
        }
        let (type_id, n) = (u16_(e + 2)?, u32_(e + 4)? as usize);
        return Some(match type_id {
            3 => Ok(u16_(e + 8)? as u32),
            4 => Ok(u32_(e + 8)?),
            2 => {
                let o = if n <= 4 { e + 8 } else { u32_(e + 8)? as usize };
                let b = t.get(o..o + n)?;
                Err(String::from_utf8_lossy(b).trim_end_matches('\0').trim().to_string())
            }
            _ => return None,
        });
    }
    None
}

/// Minimal TIFF reader for the directories at the start of the file (DNG): entries of an IFD.
struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(b: &'a [u8]) -> Option<Self> {
        match b.get(0..4)? {
            b"II*\0" => Some(Tiff { b, le: true }),
            b"MM\0*" => Some(Tiff { b, le: false }),
            _ => None,
        }
    }
    fn u16_(&self, o: usize) -> Option<u16> {
        let x = self.b.get(o..o + 2)?;
        Some(if self.le { u16::from_le_bytes([x[0], x[1]]) } else { u16::from_be_bytes([x[0], x[1]]) })
    }
    fn u32_(&self, o: usize) -> Option<u32> {
        let x = self.b.get(o..o + 4)?;
        Some(if self.le { u32::from_le_bytes([x[0], x[1], x[2], x[3]]) } else { u32::from_be_bytes([x[0], x[1], x[2], x[3]]) })
    }
    /// numbers of an entry (SHORT or LONG, also several)
    fn numbers(&self, ifd: usize, tag: u16) -> Option<Vec<u32>> {
        for i in 0..self.u16_(ifd)? as usize {
            let e = ifd + 2 + i * 12;
            if self.u16_(e)? != tag {
                continue;
            }
            let (type_id, n) = (self.u16_(e + 2)?, self.u32_(e + 4)? as usize);
            let size = match type_id {
                3 => 2,
                4 | 13 => 4,
                _ => return None,
            };
            let start = if n * size <= 4 { e + 8 } else { self.u32_(e + 8)? as usize };
            return (0..n.min(64)).map(|k| if size == 2 { self.u16_(start + k * 2).map(u32::from) } else { self.u32_(start + k * 4) }).collect();
        }
        None
    }
    fn number(&self, ifd: usize, tag: u16) -> Option<u32> {
        self.numbers(ifd, tag)?.first().copied()
    }
}

/// DNG: the preview JPEGs often sit behind the raw data at the end of the file. The directory at the
/// start gives their location; only the start and the block of the best-fitting preview
/// (the smallest that reaches `min`) are read instead of the whole file (~30 MB).
fn dng_fast(path: &Path, min: u32) -> Option<(RgbImage, Meta)> {
    use std::io::{Read, Seek, SeekFrom};
    let (header, _) = read_header(path, 1 << 20)?;
    let t = Tiff::new(&header)?;
    let ifd0 = t.u32_(4)? as usize;
    // candidates: IFD0 and its SubIFDs with a reduced image (NewSubFileType 1) as JPEG (Compression 6/7, 8 bit)
    let mut ifds = vec![ifd0];
    ifds.extend(t.numbers(ifd0, 330).unwrap_or_default().into_iter().map(|o| o as usize));
    let mut best: Option<(u32, u64, usize)> = None;
    for ifd in ifds {
        let kind = t.number(ifd, 254).unwrap_or(0);
        let compression = t.number(ifd, 259).unwrap_or(0);
        let bits = t.number(ifd, 258).unwrap_or(8);
        let (Some(off), Some(len)) = (t.numbers(ifd, 273), t.numbers(ifd, 279)) else { continue };
        if kind & 1 == 0 || !matches!(compression, 6 | 7) || bits != 8 || off.len() != 1 || len.len() != 1 {
            continue;
        }
        let w = t.number(ifd, 256).unwrap_or(0).max(t.number(ifd, 257).unwrap_or(0));
        let better = match best {
            None => true,
            Some((bw, _, _)) => (w >= min && (bw < min || w < bw)) || (bw < min && w > bw),
        };
        if better {
            best = Some((w, off[0] as u64, len[0] as usize));
        }
    }
    let (w, off, len) = best?;
    if w < min || len > 40 << 20 {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut jpeg = vec![0u8; len];
    f.read_exact(&mut jpeg).ok()?;
    let img = jpeg_downscaled(&jpeg, min)?;
    Some(tiff_finish(&t, &header, ifd0, img))
}

/// Orientation, camera and capture time from IFD0 of a TIFF-based RAW (start of the file in `header`), applied to
/// the preview `img`.
fn tiff_finish(t: &Tiff, header: &[u8], ifd0: usize, img: RgbImage) -> (RgbImage, Meta) {
    let o = t.number(ifd0, 0x0112).unwrap_or(1);
    let camera = tiff_value(header, 0x0110).and_then(|v| v.err()).filter(|s| !s.is_empty());
    let capture = t.number(ifd0, 0x8769).and_then(|exif| {
        // DateTimeOriginal in the EXIF IFD (ASCII, pointer to 20 bytes)
        for i in 0..t.u16_(exif as usize)? as usize {
            let e = exif as usize + 2 + i * 12;
            if t.u16_(e)? == 0x9003 {
                let o = t.u32_(e + 8)? as usize;
                let s = String::from_utf8_lossy(header.get(o..o + 19)?).to_string();
                return crate_exif_date(&s);
            }
        }
        None
    });
    (orient(img, o as u16), Meta { width: None, height: None, capture, camera })
}

/// NEF/NRW: the previews are JPEGs (JPEGInterchangeFormat 0x0201/0x0202) in IFD0 and its SubIFDs: a full-size one
/// and, on newer cameras, a medium one (about 1 MB). The RAW reader decodes the full-size JPEG (up to 45 MP) for every
/// thumbnail, which took seconds per photo. Here only the smallest JPEG that reaches `min` is read and decoded
/// downscaled. This also works for NEF compressions the RAW reader cannot decode (High Efficiency).
fn nef_fast(path: &Path, min: u32) -> Option<(RgbImage, Meta)> {
    use std::io::{Read, Seek, SeekFrom};
    let (header, _) = read_header(path, 1 << 20)?;
    let t = Tiff::new(&header)?;
    let ifd0 = t.u32_(4)? as usize;
    let mut ifds = vec![ifd0];
    ifds.extend(t.numbers(ifd0, 330).unwrap_or_default().into_iter().map(|o| o as usize));
    let mut jpegs: Vec<(u64, usize)> = ifds
        .into_iter()
        .filter(|&ifd| t.number(ifd, 259) == Some(6))
        .filter_map(|ifd| Some((t.number(ifd, 0x0201)? as u64, t.number(ifd, 0x0202)? as usize)))
        .filter(|&(_, len)| len > 1000 && len <= 40 << 20)
        .collect();
    jpegs.sort_by_key(|&(_, len)| len);
    let mut f = std::fs::File::open(path).ok()?;
    for (off, len) in jpegs {
        let mut jpeg = vec![0u8; len];
        if f.seek(SeekFrom::Start(off)).is_err() || f.read_exact(&mut jpeg).is_err() {
            continue;
        }
        let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(&jpeg));
        let Some(info) = d.read_info().ok().and_then(|_| d.info()) else { continue };
        if (info.width.max(info.height) as u32) < min {
            continue;
        }
        let img = jpeg_downscaled(&jpeg, min)?;
        return Some(tiff_finish(&t, &header, ifd0, img));
    }
    None
}

/// CR3 without the RAW reader: preview JPEG (PRVW), orientation, camera and capture time are all
/// in the first scant megabyte (CMT1 = IFD0, CMT2 = EXIF). The RAW reader, in contrast, maps the whole
/// file (~25 MB) into memory and lets the system read ahead – on a memory card
/// that cost many times more per thumbnail.
fn cr3_fast(path: &Path, min: u32) -> Option<(RgbImage, Meta)> {
    let (header, jpeg) = cr3_header(path)?;
    let img = jpeg_or_full(&header[jpeg], min).ok()?;
    if img.width().max(img.height()) < min {
        return None;
    }
    let block = |name: &[u8]| header.windows(4).position(|w| w == name).map(|p| &header[p + 4..]);
    let cmt1 = block(b"CMT1");
    let o = cmt1.and_then(|t| tiff_value(t, 0x0112)).and_then(|v| v.ok()).unwrap_or(1);
    let camera = cmt1.and_then(|t| tiff_value(t, 0x0110)).and_then(|v| v.err()).filter(|s| !s.is_empty());
    let capture = block(b"CMT2").and_then(|t| tiff_value(t, 0x9003)).and_then(|v| v.err()).and_then(|d| crate_exif_date(&d));
    Some((orient(img, o as u16), Meta { width: None, height: None, capture, camera }))
}

/// Embedded preview of a RAW via rawler (only the header and preview JPEG are read,
/// the sensor data is not decoded). `min`: desired minimum size.
fn raw_preview(path: &Path, min: u32) -> Result<(RgbImage, Meta), String> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cr3")) {
        if let Some(r) = cr3_fast(path, min) {
            return Ok(r);
        }
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")) {
        if let Some(r) = dng_fast(path, min) {
            return Ok(r);
        }
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("nef") || e.eq_ignore_ascii_case("nrw")) {
        if let Some(r) = nef_fast(path, min) {
            return Ok(r);
        }
    }
    // The embedded preview almost always lies in the first megabyte or the first few (CR2, ARW, NEF,
    // RAF, DNG …). Read only this start first; the whole file only if the preview is not completely
    // there. If a reference points behind the part read, the reader reports an error instead of partial data.
    for limit in [1usize << 20, 6 << 20] {
        match read_header(path, limit) {
            Some((header, whole)) => {
                let src = rawler::rawsource::RawSource::new_from_shared_vec(std::sync::Arc::new(header)).with_path(path);
                if let Ok((img, m)) = from_source(&src, min) {
                    if whole || img.width().max(img.height()) >= min {
                        if std::env::var_os("GRAINEO_PREVIEW_LOG").is_some() {
                            eprintln!("preview from the first {} MB: {}", limit >> 20, path.display());
                        }
                        return Ok((img, m));
                    }
                }
                if whole {
                    break;
                }
            }
            None => break,
        }
    }
    if std::env::var_os("GRAINEO_PREVIEW_LOG").is_some() {
        eprintln!("preview from the whole file: {}", path.display());
    }
    let src = rawler::rawsource::RawSource::new(path).map_err(|e| e.to_string())?;
    from_source(&src, min)
}

/// At most `limit` bytes from the start of the file; `true` if that is already the whole file.
fn read_header(path: &Path, limit: usize) -> Option<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut b = Vec::with_capacity(limit);
    std::fs::File::open(path).ok()?.take(limit as u64).read_to_end(&mut b).ok()?;
    let whole = b.len() < limit;
    Some((b, whole))
}

/// Camera model as stored in the EXIF data (TIFF-based RAWs: IFD0 at the start of the file) – the same
/// name the import records. Otherwise the RAW reader returns a cleaned-up name without the manufacturer.
fn exif_model(src: &rawler::rawsource::RawSource) -> Option<String> {
    let b = src.buf();
    if !(b.starts_with(b"II*\0") || b.starts_with(b"MM\0*")) {
        return None;
    }
    tiff_value(b, 0x0110)?.err().filter(|s| !s.is_empty())
}

fn from_source(src: &rawler::rawsource::RawSource, min: u32) -> Result<(RgbImage, Meta), String> {
    use rawler::decoders::RawDecodeParams;
    let dec = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rawler::get_decoder(src))).map_err(|_| "Leser abgestürzt".to_string())?.map_err(|e| e.to_string())?;
    let p = RawDecodeParams::default();
    let meta = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dec.raw_metadata(&src, &p))).ok().and_then(|m| m.ok());
    let mut best: Option<image::DynamicImage> = None;
    let attempts: [&dyn Fn() -> rawler::Result<Option<image::DynamicImage>>; 3] = [&|| dec.preview_image(&src, &p), &|| dec.full_image(&src, &p), &|| dec.thumbnail_image(&src, &p)];
    for v in attempts {
        if best.as_ref().is_some_and(|b| b.width().max(b.height()) >= min) {
            break;
        }
        if let Ok(Ok(Some(img))) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(v)) {
            let large_enough = img.width().max(img.height()) >= min;
            if best.as_ref().is_none_or(|b| img.width() > b.width()) {
                best = Some(img);
            }
            if large_enough {
                break;
            }
        }
    }
    let img = best.ok_or("keine eingebettete Vorschau")?.to_rgb8();
    let o = meta.as_ref().and_then(|m| m.exif.orientation).unwrap_or(1);
    let m = Meta {
        width: None,
        height: None,
        capture: meta.as_ref().and_then(|m| m.exif.date_time_original.clone().or(m.exif.create_date.clone())).and_then(|d| crate_exif_date(&d)),
        camera: exif_model(src).or_else(|| meta.as_ref().map(|m| if m.model.starts_with(&m.make) { m.model.clone() } else { format!("{} {}", m.make, m.model) })).filter(|s| !s.trim().is_empty()),
    };
    Ok((orient(img, o), m))
}

fn crate_exif_date(s: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        crate::imageio::exif_date(s)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = s;
        None
    }
}

/// Get the image from the file with at most `max` pixels edge length (upright, sRGB).
pub fn load(path: &Path, format: &str, max: u32) -> Result<(RgbImage, Meta), String> {
    // Apple Photos, photo cropped or rotated there: Apple's current version (what Photos shows; its face boxes refer to it)
    if format != "VIDEO" {
        if let Some(current) = crate::apple_photos::current_version(path) {
            let f = crate::format_from_extension(&current.to_string_lossy()).unwrap_or("JPG");
            return load(&current, f, max);
        }
    }
    // Apple Photos: original only in iCloud (or a video) → Apple's local preview file
    if crate::apple_photos::in_photos_library(path) && (format == "VIDEO" || !path.is_file()) {
        let m = path.ancestors().find(|a| a.extension().is_some_and(|e| e == "photoslibrary")).ok_or("Bibliothek nicht gefunden")?;
        let uuid = crate::apple_photos::uuid_from_path(path).ok_or("keine Kennung")?;
        let v = crate::apple_photos::preview_for(m, &uuid).ok_or("keine lokale Vorschau in Apple Fotos")?;
        let img = image::open(&v).map_err(|e| e.to_string())?.to_rgb8();
        return Ok((downscale(img, max), Meta::default()));
    }
    match format {
        "VIDEO" => Ok((downscale(crate::video::preview(path, max)?, max), Meta::default())),
        "X3F" => {
            // image size from the header of the largest embedded preview (camera output size, unrotated as with ImageIO)
            let size = x3f::read_image_size(path);
            Ok((downscale(x3f_preview(path, max)?, max), Meta { width: size.map(|s| s.0), height: size.map(|s| s.1), ..Meta::default() }))
        }
        f if crate::is_raw(f) => {
            let (img, m) = raw_preview(path, max)?;
            Ok((downscale(img, max), m))
        }
        _ => {
            #[cfg(target_os = "macos")]
            {
                let e = crate::imageio::properties(path);
                // the embedded thumbnail (HEIC: 320 px) suffices for the small level – otherwise decode downscaled
                let embedded = crate::imageio::thumb(path, max, false).filter(|(w, h, _)| (*w).max(*h) >= max * 9 / 10);
                let (w, h, rgb) = embedded.or_else(|| crate::imageio::thumb(path, max, true)).ok_or("ImageIO kann die Datei nicht lesen")?;
                let img = downscale(RgbImage::from_raw(w, h, rgb).ok_or("Bildgröße passt nicht")?, max);
                let m = e.map(|e| Meta { width: e.width, height: e.height, capture: e.capture, camera: e.camera }).unwrap_or_default();
                Ok((img, m))
            }
            #[cfg(not(target_os = "macos"))]
            {
                let img = image::open(path).map_err(|e| e.to_string())?.to_rgb8();
                Ok((downscale(img, max), Meta::default()))
            }
        }
    }
}

// ───────────────────────────────────────────── Developed previews (after editing, batch "Alle Vorschauen neu rechnen")

/// Folder of the developed previews (`<cache>/developed-grid/<file ID>-<level>.jpg`): what graineo's own development
/// of a photo shows (instead of the Lightroom export or camera JPEG). The generator takes them before decoding the file;
/// they are never evicted. Pure cache, can be regenerated by developing again.
pub fn developed_folder() -> PathBuf {
    crate::cache_folder().join("developed-grid")
}

fn developed_path(file: i64, level: u32) -> PathBuf {
    developed_folder().join(format!("{file}-{level}.jpg"))
}

/// Which levels exist per file (read once from the folder, then kept up to date).
fn developed_index() -> &'static Mutex<HashMap<i64, Vec<u32>>> {
    static INDEX: std::sync::OnceLock<Mutex<HashMap<i64, Vec<u32>>>> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut m: HashMap<i64, Vec<u32>> = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(developed_folder()) {
            for e in rd.flatten() {
                let name = e.file_name();
                let Some((f, l)) = name.to_str().and_then(|n| n.strip_suffix(".jpg")).and_then(|n| n.split_once('-')) else { continue };
                if let (Ok(f), Ok(l)) = (f.parse::<i64>(), l.parse::<u32>()) {
                    m.entry(f).or_default().push(l);
                }
            }
        }
        Mutex::new(m)
    })
}

/// Store a developed preview (JPEG, atomic). Returns: written.
pub fn write_developed(file: i64, level: u32, img: &RgbImage) -> bool {
    let target = developed_path(file, level);
    let _ = std::fs::create_dir_all(developed_folder());
    let part = target.with_extension("teil");
    let Ok(f) = std::fs::File::create(&part) else { return false };
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), 88);
    if enc.encode_image(img).is_ok() && std::fs::rename(&part, &target).is_ok() {
        let mut ix = developed_index().lock().unwrap();
        let v = ix.entry(file).or_default();
        if !v.contains(&level) {
            v.push(level);
        }
        true
    } else {
        let _ = std::fs::remove_file(&part);
        false
    }
}

/// Remove developed previews of these levels (e.g. the outdated large one after a new edit).
pub fn remove_developed(file: i64, levels: &[u32]) {
    for &l in levels {
        let _ = std::fs::remove_file(developed_path(file, l));
    }
    if let Some(v) = developed_index().lock().unwrap().get_mut(&file) {
        v.retain(|l| !levels.contains(l));
    }
}

/// Photo on this Mac: remove its developed large previews (no offline copy here). The small level `small` is
/// derived from them first if it is missing, so the edit stays visible in the grid. Returns: number removed.
pub fn remove_developed_large(file: i64, small: u32) -> usize {
    let levels = developed_index().lock().unwrap().get(&file).cloned().unwrap_or_default();
    let large: Vec<u32> = levels.iter().copied().filter(|&l| is_heavy(l)).collect();
    if large.is_empty() {
        return 0;
    }
    if !levels.contains(&small) {
        match read_developed_best(file, small) {
            Some((img, true)) if write_developed(file, small, &img) => {}
            _ => return 0,
        }
    }
    remove_developed(file, &large);
    large.len()
}

/// Is a developed preview of exactly this level present?
pub fn has_developed(file: i64, level: u32) -> bool {
    developed_index().lock().unwrap().get(&file).is_some_and(|v| v.contains(&level))
}

/// Read a developed preview of exactly this level, if present.
pub fn read_developed(file: i64, level: u32) -> Option<RgbImage> {
    image::open(developed_path(file, level)).ok().map(|i| i.to_rgb8())
}

/// Developed preview for `level`: exactly this one, otherwise the best other level (reduced if larger).
/// `bool`: exact or at least as large as requested (may go into the store under this level).
pub fn read_developed_best(file: i64, level: u32) -> Option<(RgbImage, bool)> {
    if let Some(img) = read_developed(file, level) {
        return Some((img, true));
    }
    let levels = developed_index().lock().unwrap().get(&file).cloned().unwrap_or_default();
    let other = best_level(&levels.into_iter().filter(|&l| l != level).collect::<Vec<_>>(), level)?;
    let img = read_developed(file, other)?;
    Some((downscale(img, level), other >= level))
}

/// A job for the generator.
#[derive(Clone, Debug)]
pub struct Job {
    pub file: i64,
    pub path: PathBuf,
    pub format: String,
    pub key: u64,
    pub level: u32,
    /// orientation according to the library (e.g. rotated in Lightroom); if it differs from the file, the preview is rotated afterwards
    pub orientation: Option<i64>,
}

/// Clockwise quarter turns of an EXIF orientation (pure rotations only, no mirroring).
pub fn quarter(o: i64) -> Option<u8> {
    match o {
        1 => Some(0),
        6 => Some(1),
        3 => Some(2),
        8 => Some(3),
        _ => None,
    }
}

/// EXIF orientation to quarter turns (0–3).
pub fn exif_orientation(quarter: u8) -> i64 {
    [1, 6, 3, 8][(quarter % 4) as usize]
}

/// By how many quarter turns the preview (already made upright according to the file) must be rotated so that
/// it matches the orientation in the library (e.g. a rotation from Lightroom). Reads only the
/// properties of the file, and only if the library knows a rotation at all.
pub fn extra_rotation(path: &Path, format: &str, library_orientation: Option<i64>) -> u8 {
    let Some(expected) = library_orientation.and_then(quarter) else { return 0 };
    if format == "VIDEO" || format == "X3F" {
        return 0;
    }
    let file = crate::imageio::properties(path).map_or(1, |e| e.orientation.map_or(1, i64::from));
    let Some(actual) = quarter(file) else { return 0 };
    (expected + 4 - actual) % 4
}

/// Rotate an image clockwise by quarter turns.
pub fn rotate(img: RgbImage, quarter: u8) -> RgbImage {
    match quarter % 4 {
        1 => image::imageops::rotate90(&img),
        2 => image::imageops::rotate180(&img),
        3 => image::imageops::rotate270(&img),
        _ => img,
    }
}

/// Cache key with the library's orientation: if it changes (rotated), a new thumbnail is created.
pub fn cache_key_with_orientation(key: u64, orientation: Option<i64>) -> u64 {
    match orientation {
        Some(o) if o != 1 => key ^ (o as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15),
        _ => key,
    }
}

/// Result: image in the requested level (only for visible jobs) and extra knowledge.
pub struct Outcome {
    pub file: i64,
    pub key: u64,
    pub level: u32,
    pub image: Option<RgbImage>,
    pub meta: Option<Meta>,
    pub error: Option<String>,
}

struct Inner {
    cache: Arc<Cache>,
    /// one job before all others (large preview of the single image); not replaced by [`Generator::visible`]
    urgent: Mutex<Option<Job>>,
    front: Mutex<VecDeque<Job>>,
    /// after the visible ones: large previews of the next images while paging (returned like visible ones)
    ahead: Mutex<VecDeque<Job>>,
    /// before the background, independent of its switch: previews right after an import (from the card), stand-ins
    /// of an offline copy to develop (seen in the single image)
    soon: Mutex<VecDeque<Job>>,
    back: Mutex<VecDeque<Job>>,
    /// keys whose development failed in this run (not tried again until the next start)
    failed: Mutex<HashSet<u64>>,
    running: Mutex<HashSet<(u64, u32)>>,
    signal: Condvar,
    wait_for: Mutex<()>,
    outcomes: Mutex<Vec<Outcome>>,
    wake: Box<dyn Fn(bool) + Send + Sync>,
    pub background: AtomicBool,
    generated: AtomicUsize,
    error: AtomicUsize,
    /// background jobs of a large level running right now (at most one, so visible tiles always find a free worker)
    heavy_running: AtomicUsize,
    /// how many background jobs of a large level may run at the same time (1; the app allows 2 while it is idle)
    heavy_limit: AtomicUsize,
    /// total generation compute time (µs) – for measurement
    time_us: AtomicU64,
}

/// Background threads for thumbnails (a clone is another handle to the same threads and queues).
#[derive(Clone)]
pub struct Generator {
    inner: Arc<Inner>,
}

impl Generator {
    /// `threads`: number of workers; `wake` is called after each result (e.g. redraw), with `true` for a visible
    /// tile (wanted at once) and `false` for background work (a redraw within a second is enough).
    pub fn new(cache: Arc<Cache>, threads: usize, wake: Box<dyn Fn(bool) + Send + Sync>) -> Generator {
        let inner = Arc::new(Inner {
            cache,
            urgent: Mutex::new(None),
            front: Mutex::new(VecDeque::new()),
            ahead: Mutex::new(VecDeque::new()),
            soon: Mutex::new(VecDeque::new()),
            back: Mutex::new(VecDeque::new()),
            failed: Mutex::new(HashSet::new()),
            running: Mutex::new(HashSet::new()),
            signal: Condvar::new(),
            wait_for: Mutex::new(()),
            outcomes: Mutex::new(Vec::new()),
            wake,
            background: AtomicBool::new(false),
            generated: AtomicUsize::new(0),
            error: AtomicUsize::new(0),
            heavy_running: AtomicUsize::new(0),
            heavy_limit: AtomicUsize::new(1),
            time_us: AtomicU64::new(0),
        });
        for i in 0..threads.max(1) {
            let inn = inner.clone();
            std::thread::Builder::new().name(format!("vorschau-{i}")).spawn(move || workers(inn, i)).ok();
        }
        Generator { inner }
    }

    pub fn cache(&self) -> &Arc<Cache> {
        &self.inner.cache
    }

    /// Visible jobs (replace the previous ones; order = priority).
    pub fn visible(&self, jobs: Vec<Job>) {
        *self.inner.front.lock().unwrap() = jobs.into();
        self.inner.signal.notify_all();
    }

    /// Jobs after the visible ones, returned like them (large previews of the next images while paging); replace
    /// the previous ones.
    pub fn ahead(&self, jobs: Vec<Job>) {
        *self.inner.ahead.lock().unwrap() = jobs.into();
        self.inner.signal.notify_all();
    }

    /// One job before all visible ones (e.g. the large preview of the photo in the single image view);
    /// a later call replaces it.
    pub fn urgent(&self, job: Job) {
        *self.inner.urgent.lock().unwrap() = Some(job);
        self.inner.signal.notify_all();
    }

    /// Jobs to do soon, before the background and even with it switched off (e.g. right after an import: from the
    /// file on the card, under the key of the copy). Added at the end; never replaced by [`Generator::background`].
    pub fn soon(&self, jobs: Vec<Job>) {
        self.inner.soon.lock().unwrap().extend(jobs);
        self.inner.signal.notify_all();
    }

    /// Jobs to do soon still open.
    pub fn open_soon(&self) -> usize {
        self.inner.soon.lock().unwrap().len()
    }

    /// Background jobs (the rest of the library, generation only without returning the image).
    pub fn background(&self, jobs: Vec<Job>) {
        *self.inner.back.lock().unwrap() = jobs.into();
        self.inner.signal.notify_all();
    }

    pub fn background_on(&self, old_name: bool) {
        self.inner.background.store(old_name, Ordering::Relaxed);
        self.inner.signal.notify_all();
    }

    pub fn background_running(&self) -> bool {
        self.inner.background.load(Ordering::Relaxed)
    }

    /// Background jobs of a large level (offline copies) allowed at the same time (at least 1). While the user works
    /// one is enough; when nothing happens, a second one uses the time the first spends reading and decoding.
    pub fn set_heavy_limit(&self, n: usize) {
        if self.inner.heavy_limit.swap(n.max(1), Ordering::SeqCst) < n {
            self.inner.signal.notify_all();
        }
    }

    /// Background jobs still open.
    pub fn open_set(&self) -> usize {
        self.inner.back.lock().unwrap().len()
    }

    /// (generated, errors, mean time per image in ms)
    pub fn counter(&self) -> (usize, usize, f64) {
        let n = self.inner.generated.load(Ordering::Relaxed);
        let us = self.inner.time_us.load(Ordering::Relaxed);
        (n, self.inner.error.load(Ordering::Relaxed), if n > 0 { us as f64 / n as f64 / 1000.0 } else { 0.0 })
    }

    /// Fetch finished results.
    pub fn fetch(&self) -> Vec<Outcome> {
        std::mem::take(&mut *self.inner.outcomes.lock().unwrap())
    }
}

/// Does a background job still have something to do: the preview is missing, or the offline copy of a RAW is only
/// the camera JPEG (stand-in) and its development has not failed in this run.
fn to_do(inn: &Inner, a: &Job) -> bool {
    if !inn.cache.has(a.key, a.level) {
        return true;
    }
    is_heavy(a.level) && develops(&a.format) && !inn.failed.lock().unwrap().contains(&a.key) && !inn.cache.is_developed(a.key, a.level)
}

fn nearest(inn: &Inner) -> Option<(Job, bool)> {
    let running = inn.running.lock().unwrap();
    if let Some(a) = inn.urgent.lock().unwrap().take() {
        if !running.contains(&(a.key, a.level)) {
            return Some((a, true));
        }
    }
    for queue in [&inn.front, &inn.ahead] {
        let mut v = queue.lock().unwrap();
        while let Some(a) = v.pop_front() {
            if !running.contains(&(a.key, a.level)) {
                return Some((a, true));
            }
        }
    }
    if inn.cache.paused() {
        return None;
    }
    // first the jobs to do soon, then (if switched on) the rest of the library; at most one large one at a time
    for (queue, on) in [(&inn.soon, true), (&inn.back, inn.background.load(Ordering::Relaxed))] {
        if !on {
            continue;
        }
        let mut h = queue.lock().unwrap();
        while let Some(a) = h.pop_front() {
            if !running.contains(&(a.key, a.level)) && to_do(inn, &a) {
                if is_heavy(a.level) && inn.heavy_running.load(Ordering::SeqCst) >= inn.heavy_limit.load(Ordering::SeqCst).max(1) {
                    // a large one is already being made: wait (the queue keeps its order)
                    h.push_front(a);
                    return None;
                }
                return Some((a, false));
            }
        }
    }
    None
}


/// Run the current thread at low priority (macOS: QoS utility, below the UI; only `BACKGROUND` would keep it on the efficiency cores), e.g. for previews made while
/// a move runs.
pub fn low_priority_thread() {
    low_priority();
}

/// Run the current thread at the lowest priority (macOS: QoS background – efficiency cores, throttled disk and network
/// access), for work nobody waits for (e.g. looking for folders not in the catalog).
pub fn background_priority_thread() {
    crate::platform::background_priority();
}

fn low_priority() {
    // QOS_CLASS_UTILITY: below the UI (visible tiles still come quickly; `BACKGROUND` would also throttle them)
    crate::platform::set_priority(crate::platform::Priority::Utility);
}

/// Store the preview of a job (with a large level also the small one if missing); `marked`: it shows the development.
fn store(cache: &Cache, a: &Job, img: &RgbImage, marked: bool) {
    cache.write_with(a.key, a.level, img, marked);
    let small_level = small();
    if a.level > small_level && !cache.has(a.key, small_level) {
        cache.write_with(a.key, small_level, &downscale(img.clone(), small_level), marked);
    }
}

/// Make the preview of a job and store it (a developed preview takes precedence over decoding the file; with a large
/// level the small one is stored along if missing). `develop` (background): the offline copy of a RAW/X3F is the
/// development in this level – if it fails, a camera JPEG present stays, otherwise one is stored as a stand-in.
/// Returns: the image and extra knowledge from decoding.
fn produce(cache: &Cache, a: &Job, develop: bool) -> Result<(RgbImage, Option<Meta>), String> {
    // a developed preview (after editing, batch) takes precedence over decoding the file
    let developed = read_developed_best(a.file, a.level);
    if develop && is_heavy(a.level) && develops(&a.format) {
        if let Some((img, true)) = developed {
            store(cache, a, &img, true);
            return Ok((img, None));
        }
        if !cache.check_space() {
            return Err("zu wenig freier Platz für Vorschauen".into());
        }
        let developer = DEVELOPER.get().ok_or("keine Entwicklung")?;
        match developer(&a.path, a.level) {
            Ok(img) => {
                let img = downscale(img, a.level);
                store(cache, a, &img, true);
                return Ok((img, None));
            }
            // the stand-in present stays (never removed without a replacement)
            Err(e) if cache.has(a.key, a.level) => return Err(e),
            Err(_) => {}
        }
    }
    // the file is made upright according to its own orientation; a rotation from the library is added on top
    // (developed previews already contain it)
    let loaded = || load(&a.path, &a.format, a.level).map(|(img, m)| (rotate(img, extra_rotation(&a.path, &a.format, a.orientation)), m, false));
    // a larger level already in the store serves as well and is much faster than decoding the original
    // (e.g. the small level after changing it, from the large one)
    let from_store = || {
        let l = [small(), large()].into_iter().filter(|&l| l > a.level && cache.has(a.key, l)).min()?;
        let b = std::fs::read(cache.path(a.key, l)).ok()?;
        let img = jpeg_downscaled(&b, a.level)?;
        Some((downscale(img, a.level), Meta::default(), has_mark(&b)))
    };
    let outcome = match developed {
        Some((img, full)) => Ok((img, None, full, true)),
        None => match from_store().map(Ok).unwrap_or_else(loaded) {
            Ok((img, meta, marked)) => Ok((img, Some(meta), true, marked)),
            // original not reachable: the nearest level still present (e.g. after changing the levels)
            // (stored if it was at least as large: then it serves the level fully)
            Err(e) => match cache.nearest_level(a.key, a.level).and_then(|l| cache.read(a.key, l).map(|img| (l, img))) {
                Some((l, img)) => Ok((downscale(img, a.level), None, l >= a.level, cache.is_developed(a.key, l))),
                None => Err(e),
            },
        },
    };
    let (img, meta, full, marked) = outcome?;
    // only store what serves the level (no smaller stand-in under the name of the large level)
    if full {
        store(cache, a, &img, marked);
    }
    Ok((img, meta))
}

/// Make sure the preview of this job is in the cache (e.g. before moving the original to a location that may be
/// unreachable later). Returns: `true` if it was made now, `false` if it was there already.
pub fn ensure(cache: &Cache, job: &Job) -> Result<bool, String> {
    if cache.has(job.key, job.level) {
        return Ok(false);
    }
    produce(cache, job, false)?;
    Ok(cache.has(job.key, job.level))
}

/// Make sure the offline copy (large level) of this job is in the cache, for a RAW/X3F as the development (a camera
/// JPEG present is replaced) – from the original at its current place, e.g. while "Move to …" still has it on this
/// Mac. If the development fails, a camera JPEG stands in. Returns: `true` if something was made now.
pub fn ensure_offline(cache: &Cache, job: &Job) -> Result<bool, String> {
    let developed_wanted = develops(&job.format);
    if cache.has(job.key, job.level) && (!developed_wanted || cache.is_developed(job.key, job.level)) {
        return Ok(false);
    }
    match produce(cache, job, true) {
        Ok(_) => Ok(cache.has(job.key, job.level)),
        // development failed, but a stand-in is there
        Err(_) if cache.has(job.key, job.level) => Ok(true),
        Err(e) => Err(e),
    }
}

fn workers(inn: Arc<Inner>, _nr: usize) {
    low_priority();
    loop {
        let Some((a, visible)) = nearest(&inn) else {
            let g = inn.wait_for.lock().unwrap();
            let _ = inn.signal.wait_timeout(g, std::time::Duration::from_millis(500)).unwrap();
            continue;
        };
        inn.running.lock().unwrap_or_else(|e| e.into_inner()).insert((a.key, a.level));
        // whatever happens below (also a panic in a reader): "running" and the heavy counter are reset
        let mut guard = RunningMark { inn: &inn, key: (a.key, a.level), heavy: false };
        // visible tiles soon (utility), the rest of the library at the lowest priority (efficiency cores); the gates
        // of the development serve them accordingly (soon / idle, never before the photo on screen)
        if visible {
            low_priority();
            pools::set_intent(pools::Intent::Soon);
        } else {
            background_priority_thread();
            pools::set_intent(pools::Intent::Idle);
        }
        let heavy = !visible && is_heavy(a.level);
        if heavy {
            inn.heavy_running.fetch_add(1, Ordering::SeqCst);
            guard.heavy = true;
        }
        let mut result = Outcome { file: a.file, key: a.key, level: a.level, image: None, meta: None, error: None };
        let existing = if visible { inn.cache.read(a.key, a.level) } else { None };
        if let Some(img) = existing {
            result.image = Some(img);
        } else {
            let t = std::time::Instant::now();
            // a damaged photo may make a reader (image crate, X3F, ImageIO) panic: that photo fails, the worker lives on
            let produced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| produce(&inn.cache, &a, !visible))).unwrap_or_else(|p| {
                let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                eprintln!("preview: reader crashed on {}: {why}", a.path.display());
                inn.failed.lock().unwrap_or_else(|e| e.into_inner()).insert(a.key);
                Err(format!("Leser abgestürzt: {why}"))
            });
            match produced {
                Ok((img, meta)) => {
                    result.meta = meta;
                    if visible {
                        result.image = Some(img);
                    }
                    inn.generated.fetch_add(1, Ordering::Relaxed);
                    inn.time_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                }
                Err(e) => {
                    inn.error.fetch_add(1, Ordering::Relaxed);
                    result.error = Some(e);
                }
            }
            if !visible {
                // throttled: leave the system some air
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
        if heavy {
            inn.heavy_running.fetch_sub(1, Ordering::SeqCst);
            guard.heavy = false;
        }
        if is_heavy(a.level) && develops(&a.format) && !inn.cache.is_developed(a.key, a.level) {
            if !visible {
                // development failed: the stand-in (if any) stays, not tried again in this run (unless space was short
                // or the file was not there, e.g. a card taken out after the import – the copy is tried later)
                if !inn.cache.paused() && a.path.is_file() {
                    inn.failed.lock().unwrap().insert(a.key);
                }
            } else if result.error.is_none() && !inn.failed.lock().unwrap().contains(&a.key) && a.path.is_file() {
                // the single image shows the camera JPEG for now: develop it soon (replaces it)
                let mut soon = inn.soon.lock().unwrap();
                if !soon.iter().any(|j| j.key == a.key && j.level == a.level) {
                    soon.push_back(a.clone());
                }
                drop(soon);
                inn.signal.notify_all();
            }
        }
        drop(guard);
        inn.outcomes.lock().unwrap_or_else(|e| e.into_inner()).push(result);
        (inn.wake)(visible);
    }
}

/// Marks a preview as being produced; on drop (also when unwinding) removes the mark and gives back the heavy slot.
struct RunningMark<'a> {
    inn: &'a Inner,
    key: (u64, u32),
    heavy: bool,
}

impl Drop for RunningMark<'_> {
    fn drop(&mut self) {
        if self.heavy {
            self.inn.heavy_running.fetch_sub(1, Ordering::SeqCst);
        }
        self.inn.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-preview-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn developed_mark_only_checks_the_end() {
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xDB, 0, 4, 0, 0];
        assert!(has_mark(&with_mark(jpeg.clone())));
        assert!(!has_mark(&jpeg));
        let comment = |text: &[u8]| {
            let mut v = vec![0xFF, 0xD8, 0xFF, 0xFE, 0, (text.len() + 2) as u8];
            v.extend_from_slice(text);
            v.extend_from_slice(&jpeg[2..]);
            v
        };
        assert!(has_mark(&comment(b"another-name:developed")), "the program name before the colon is not compared");
        assert!(!has_mark(&comment(b":developed")));
        assert!(!has_mark(&comment(b"developed by someone")));
        assert!(!has_mark(&comment(b"x:developed-not")));
    }

    #[test]
    fn levels_from_settings() {
        assert_eq!(parse_small(&json!(512)), 512);
        assert_eq!(parse_small(&json!(1024)), 1024);
        // the earlier choice 256 and unknown values become the default 512
        assert_eq!(parse_small(&json!(256)), 512);
        assert_eq!(parse_small(&json!(300)), SMALL_DEFAULT);
        assert_eq!(parse_small(&serde_json::Value::Null), 512);
        assert_eq!(parse_large(&json!(4000)), 4000);
        assert_eq!(parse_large(&json!("full")), FULL);
        assert_eq!(parse_large(&json!(65535)), LARGE_DEFAULT);
        assert_eq!(parse_large(&json!(1024)), LARGE_DEFAULT);
        assert_eq!(parse_large(&large_value(FULL)), FULL);
        assert_eq!(parse_large(&large_value(2560)), 2560);
    }

    #[test]
    fn exactly_two_levels() {
        assert_eq!(active_levels(512, 2560), [512, 2560]);
        assert_eq!(active_levels(1024, FULL), [1024, FULL]);
        // every small choice is a small level, every large choice a large one
        assert!(SMALL_CHOICES.iter().all(|&l| !is_heavy(l)));
        assert!(LARGE_CHOICES.iter().all(|&l| is_heavy(l)));
        // earlier levels (256, grid level 1024) count as small ones, not towards the limit
        assert!(!is_heavy(256) && !is_heavy(1024));
    }

    #[test]
    fn best_level_prefers_the_next_larger() {
        assert_eq!(best_level(&[256, 1024, 2560], 512), Some(1024));
        assert_eq!(best_level(&[256, 1024], 4000), Some(1024));
        assert_eq!(best_level(&[], 256), None);
    }

    #[test]
    fn protection_sets_and_regenerate() {
        let d = tmp("cache");
        let c = Cache::open(&d);
        let img = RgbImage::from_pixel(64, 48, image::Rgb([90, 120, 150]));
        // before the drives are known no offline copies are prepared in the background
        assert_eq!(c.protection_generation(), 0);
        // 7: on another drive (connected), 6: not reachable
        c.set_protection([7u64 << 56].into_iter().collect(), [6u64 << 56].into_iter().collect());
        assert_eq!(c.protection_generation(), 1);
        assert!(c.is_protected(7 << 56) && c.is_protected(6 << 56) && !c.is_protected(8 << 56));
        for (k, l) in [(7u64 << 56, 2560), (6 << 56, 2560), (8 << 56, 512), (8 << 56, 2560)] {
            assert!(c.write(k, l, &img));
        }
        // offline copies are never evicted: no limit
        assert!(c.has(7 << 56, 2560) && c.has(6 << 56, 2560));
        let u = c.usage();
        assert!(u.small > 0 && u.large > 0 && u.large_other > 0);
        assert_eq!(u.large_files, 3);
        assert_eq!(u.small + u.large + u.large_other, c.occupied().0);
        assert_eq!(c.unreachable_count(), 1);
        // "regenerate" keeps only the previews of unreachable photos (other drives are made again from the original)
        assert_eq!(c.clear_unprotected(), 3);
        assert!(c.has(6 << 56, 2560) && !c.has(7 << 56, 2560));
        assert_eq!(c.usage().large_files, 1);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn nothing_is_written_below_the_minimum_free_space() {
        let d = tmp("full");
        let c = Cache { min_free: u64::MAX, ..Cache::open(&d) };
        let img = RgbImage::from_pixel(64, 48, image::Rgb([90, 120, 150]));
        c.set_protection([1u64 << 56].into_iter().collect(), HashSet::new());
        // also offline copies (protected) are not written, and nothing is deleted for it
        assert!(!c.write(1 << 56, 2560, &img) && c.paused());
        assert_eq!(MIN_FREE, 20 << 30);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn tidying_keeps_the_chosen_levels_and_never_strands_a_protected_photo() {
        let d = tmp("tidy");
        let c = Cache::open(&d);
        let img = |w: u32| RgbImage::from_fn(w, w * 2 / 3, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, 90]));
        let (a, b, e, f, g) = (1u64 << 56, 2u64 << 56, 3u64 << 56, 4u64 << 56, 5u64 << 56);
        // a: on this Mac with the old levels 256 + 1024 + 4000; b: same, but on another drive;
        // e: unreachable, only 256 (nothing to derive 512 from); f: unreachable, 1024 + 4000;
        // g: on this Mac, 512 + 2560 (large previews of earlier versions)
        for (k, l) in [(a, 256), (a, 1024), (a, 4000), (b, 256), (b, 1024), (b, 4000), (e, 256), (f, 1024), (f, 4000), (g, 512), (g, 2560)] {
            assert!(c.write(k, l, &img(l.min(1200))));
        }
        c.set_protection([b].into_iter().collect(), [e, f].into_iter().collect());
        let (derived, removed) = c.tidy_levels(512, 2560, &|| true);
        // photos on this Mac: only the small level, the large ones go
        assert_eq!(c.levels_of(a), vec![512]);
        assert_eq!(c.levels_of(g), vec![512]);
        // b and f (protected): 512 from 1024 and the offline copy 2560 from 4000 derived, then the old levels go
        assert_eq!(c.levels_of(b), vec![512, 2560]);
        assert_eq!(c.levels_of(f), vec![512, 2560]);
        // e: no new small level could be made → the old one stays as the only view
        assert_eq!(c.levels_of(e), vec![256]);
        assert_eq!((derived, removed), (5, 9));
        // a derived preview has the size of its level
        let small = c.read(b, 512).unwrap();
        assert_eq!(small.width().max(small.height()), 512);
        // stopping early leaves the rest in place
        assert!(c.write(a, 1024, &img(1024)));
        assert_eq!(c.tidy_levels(512, 2560, &|| false), (0, 0));
        assert!(c.has(a, 1024));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn developed_large_of_local_photos_goes_after_the_small_one_exists() {
        // the developed folder belongs to the data folder: only in an own test folder
        let Some(dir) = std::env::var_os("GRAINEO_DATA_DIR") else { return };
        assert!(Path::new(&dir).starts_with("/private/tmp") || Path::new(&dir).starts_with(std::env::temp_dir()), "test only with an own data folder");
        let img = RgbImage::from_fn(2560, 1707, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, 90]));
        let file = 987_654_321;
        assert!(write_developed(file, 2560, &img));
        assert_eq!(remove_developed_large(file, 512), 1);
        assert!(has_developed(file, 512) && !has_developed(file, 2560));
        assert_eq!(read_developed(file, 512).unwrap().width(), 512);
        assert_eq!(remove_developed_large(file, 512), 0);
        remove_developed(file, &[512]);
    }

    #[test]
    fn offline_space_projection() {
        let few = Usage { small: 0, large: 0, large_other: 5_000_000, large_files: 5 };
        // too few offline copies to go by: estimate of the level
        assert_eq!(projected_offline(&few, 1000, 2560), 800_000_000);
        assert_eq!(projected_offline(&few, 1000, FULL), 10_000_000_000);
        let many = Usage { small: 0, large: 0, large_other: 40_000_000, large_files: 40 };
        assert_eq!(projected_offline(&many, 16_000, 4000), 16_000_000_000);
        // 16 GB projected, 1 GB there, 30 GB free − 20 GB minimum = 10 GB usable: 5 GB short
        assert_eq!(offline_shortfall(16 * GB, GB, 30 * GB, 20 * GB), Some(5 * GB));
        assert_eq!(offline_shortfall(16 * GB, GB, 40 * GB, 20 * GB), None);
        assert_eq!(offline_shortfall(GB, 0, 5 * GB, 20 * GB), Some(GB));
    }

    #[test]
    fn moving_in_renames_and_takes_no_space_twice() {
        let d = tmp("move");
        let (old, new) = (d.join("previews"), d.join(crate::PREVIEW_FOLDER));
        for (dir, name) in [("ab", "ab00000000000001-256.jpg"), ("ab", "ab00000000000002-256.jpg"), ("developed-grid", "5-256.jpg"), ("developed", "12.jpg")] {
            std::fs::create_dir_all(old.join(dir)).unwrap();
            std::fs::write(old.join(dir).join(name), b"x").unwrap();
        }
        // first time: everything is renamed into the package (except the data folder's "developed")
        assert_eq!(move_into(&old, &new).unwrap(), 2);
        assert!(new.join("ab/ab00000000000002-256.jpg").exists() && new.join("developed-grid/5-256.jpg").exists());
        assert!(!old.join("ab").exists() && old.join("developed/12.jpg").exists());
        // remains of an older program state: missing ones are moved, duplicates removed, the old folder empties
        std::fs::remove_dir_all(old.join("developed")).unwrap();
        std::fs::create_dir_all(old.join("ab")).unwrap();
        std::fs::write(old.join("ab/ab00000000000002-256.jpg"), b"alt").unwrap();
        std::fs::write(old.join("ab/ab00000000000003-256.jpg"), b"neu").unwrap();
        assert_eq!(move_into(&old, &new).unwrap(), 1);
        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("ab/ab00000000000002-256.jpg")).unwrap(), b"x");
        assert!(new.join("ab/ab00000000000003-256.jpg").exists());
        // nothing there: nothing to do
        assert_eq!(move_into(&old, &new).unwrap(), 0);
        // the moved cache is read completely
        assert_eq!(Cache::open(&new).occupied().1, 3);
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[cfg(test)]
mod nef_tests {
    use super::*;

    /// NEF/NRW sample files (`GRAINEO_TEST_NEF_DIR`, e.g. the CC0 files of raw.pixls.us; skipped without it): the
    /// fast way finds a preview for every file, also for High Efficiency NEFs, with the same aspect ratio and
    /// orientation as the RAW reader's full-size preview.
    #[test]
    fn nef_fast_matches_the_raw_reader() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_NEF_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_NEF_DIR not set, skipped");
            return;
        };
        let mut n = 0;
        for p in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())) {
            if !p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("nef") || e.eq_ignore_ascii_case("nrw")) {
                continue;
            }
            for min in [400, 1600] {
                let (fast, meta) = nef_fast(&p, min).unwrap_or_else(|| panic!("no fast preview: {}", p.display()));
                assert!(fast.width().max(fast.height()) >= min, "{} {min}: {}×{}", p.display(), fast.width(), fast.height());
                assert!(meta.camera.as_deref().is_some_and(|c| c.to_ascii_uppercase().contains("NIKON") || c.contains("COOLPIX")), "{}: {:?}", p.display(), meta.camera);
                let src = rawler::rawsource::RawSource::new(&p).unwrap();
                let (slow, _) = from_source(&src, min).unwrap();
                let ratio = |i: &RgbImage| i.width() as f32 / i.height() as f32;
                assert!((ratio(&fast) - ratio(&slow)).abs() < 0.02, "{}: aspect {} vs {}", p.display(), ratio(&fast), ratio(&slow));
            }
            n += 1;
        }
        assert!(n > 0, "no NEF/NRW in {}", dir.display());
    }
}
