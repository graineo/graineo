//! "Learn this camera's look": collecting block pairs from the user's photos (read only, never writing to an original),
//! the learned looks in the app's data folder, and which of them is the standard of a camera and picture style.
//!
//! Storage (never in the library database): `<data folder>/looks/<camera>/<style>.x3look` (format `X3LOOK1`, 33³,
//! tetrahedral; metadata: camera, style, photos, fit, version) and `<data folder>/looks/learned.json` (switched on,
//! strength, revision, keys whose previews are due). A replaced or reset look is moved to
//! `looks/<camera>/replaced/`, never deleted. Bundled looks are never touched.
//!
//! A switched-on learned look is the standard development of its camera and style: for a neutral photo it replaces
//! the built-in hue table and the per-camera saturation (see `render::Params::new`), so the two never act together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use develop::Settings;
use image::RgbImage;
use raw::learn_look::{self, Alignment, Photo, ReferenceSource};
use raw::looks::Look;

use crate::doc::RawDoc;

/// Folder of the learned looks below the data folder.
pub const DIR: &str = library::backup::LOOKS_DIR;
const STATE: &str = "learned.json";
/// Folder (per camera) of replaced or reset looks.
const REPLACED: &str = "replaced";

/// `<data folder>/looks` (or the folder set for tests with [`set_dir`]).
pub fn dir() -> PathBuf {
    if let Some(d) = DIR_OVERRIDE.lock().unwrap().clone() {
        return d;
    }
    library::data_dir().join(DIR)
}

static DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Use another folder for the learned looks (tests, `graineo-dev`); reloads.
pub fn set_dir(d: Option<PathBuf>) {
    *DIR_OVERRIDE.lock().unwrap() = d;
    reload();
}

// ---------------------------------------------------------------------------------------------
// state

/// One learned look as the state file lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// `camera/style`
    pub key: String,
    pub enabled: bool,
    /// strength 0…1
    pub amount: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// raised with every change (switch, strength, new look, reset)
    pub revision: u64,
    pub looks: Vec<Entry>,
    /// keys whose stored previews still show an older state (renewed quietly, then removed)
    pub stale: Vec<String>,
}

fn state_path() -> PathBuf {
    dir().join(STATE)
}

/// The state of the learned looks. A missing file is the empty state. A file that cannot be read or parsed does not
/// count as "nothing switched on": the state is taken from the look files that are there (switched on, full
/// strength, as after installing); the broken file itself is kept (see [`write_state`]).
pub fn read_state() -> State {
    match std::fs::read_to_string(state_path()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(e) => {
            eprintln!("learned looks: {} unreadable ({e}), taken from the look files", state_path().display());
            recovered_state()
        }
        Ok(t) => parse_state(&t).unwrap_or_else(|| {
            eprintln!("learned looks: {} damaged, taken from the look files", state_path().display());
            recovered_state()
        }),
    }
}

fn parse_state(t: &str) -> Option<State> {
    let v = serde_json::from_str::<serde_json::Value>(t).ok()?;
    if !v.is_object() {
        return None;
    }
    let looks = v["looks"]
        .as_array()
        .map(|a| {
            a.iter()
                // a key that is no valid `camera/style` (e.g. "../x") never names a path
                .filter(|e| e["key"].as_str().is_some_and(valid_key))
                .filter_map(|e| Some(Entry { key: e["key"].as_str()?.to_string(), enabled: e["enabled"].as_bool().unwrap_or(true), amount: e["amount"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32 }))
                .collect()
        })
        .unwrap_or_default();
    let stale = v["stale"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()).unwrap_or_default();
    Some(State { revision: v["revision"].as_u64().unwrap_or(0), looks, stale })
}

/// The state rebuilt from the look files in the folder (`<camera>/<style>.x3look`, without `replaced/`): every look
/// switched on at full strength, its previews due.
fn recovered_state() -> State {
    let mut looks = Vec::new();
    for cam in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        let Some(camera) = cam.file_name().to_str().map(str::to_string) else { continue };
        if !cam.path().is_dir() {
            continue;
        }
        for f in std::fs::read_dir(cam.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("x3look") {
                continue;
            }
            let Some(style) = p.file_stem().and_then(|s| s.to_str()) else { continue };
            let key = format!("{camera}/{style}");
            if valid_key(&key) {
                looks.push(Entry { key, enabled: true, amount: 1.0 });
            }
        }
    }
    looks.sort_by(|a, b| a.key.cmp(&b.key));
    let stale = looks.iter().map(|e| e.key.clone()).collect();
    let revision = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    State { revision, looks, stale }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write the state. A state file that cannot be read or parsed is moved aside first (`learned-broken-<time>.json`,
/// kept), never written over.
fn write_state(s: &State) -> Result<(), String> {
    let path = state_path();
    let broken = match std::fs::read_to_string(&path) {
        Ok(t) => parse_state(&t).is_none(),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    };
    if broken {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let mut aside = dir().join(format!("learned-broken-{stamp}.json"));
        let mut nr = 1;
        while aside.exists() {
            nr += 1;
            aside = dir().join(format!("learned-broken-{stamp}-{nr}.json"));
        }
        std::fs::rename(&path, &aside).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("learned looks: damaged {} kept as {}", path.display(), aside.display());
    }
    let v = serde_json::json!({
        "version": 1,
        "revision": s.revision,
        "looks": s.looks.iter().map(|e| serde_json::json!({"key": e.key, "enabled": e.enabled, "amount": e.amount})).collect::<Vec<_>>(),
        "stale": s.stale,
    });
    write_atomic(&state_path(), serde_json::to_string_pretty(&v).unwrap_or_default().as_bytes())
}

/// Change the state (and mark `key` as stale for the previews), then reload the active looks.
fn change(key: &str, f: impl FnOnce(&mut State)) -> Result<(), String> {
    let _g = WRITE.lock().unwrap();
    let mut s = read_state();
    f(&mut s);
    s.revision += 1;
    if !s.stale.iter().any(|k| k == key) {
        s.stale.push(key.to_string());
    }
    write_state(&s)?;
    reload();
    Ok(())
}

static WRITE: Mutex<()> = Mutex::new(());

/// Path of the look file of `key` (`camera/style`).
pub fn look_path(key: &str) -> PathBuf {
    let (camera, style) = key.split_once('/').unwrap_or(("unknown", key));
    dir().join(safe(camera)).join(format!("{}.x3look", safe(style)))
}

/// File or folder name of one part of a key: no separators, never empty, never starting with a dot (no `.` or `..`
/// that would leave the looks folder, no hidden files).
fn safe(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || "+-._".contains(c) { c } else { '-' }).collect();
    let dots = out.len() - out.trim_start_matches('.').len();
    out.replace_range(..dots, &"-".repeat(dots));
    if out.is_empty() {
        out.push('-');
    }
    out
}

fn move_to_replaced(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Ok(());
    }
    let dir = path.parent().ok_or("no folder")?.join(REPLACED);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut target = dir.join(format!("{stem}-{stamp}.x3look"));
    let mut nr = 1;
    while target.exists() {
        nr += 1;
        target = dir.join(format!("{stem}-{stamp}-{nr}.x3look"));
    }
    std::fs::rename(path, &target).map_err(|e| format!("{}: {e}", target.display()))
}

/// Make `look` the learned look of `key`: written next to the others (an older one goes to `replaced/`), switched on
/// at full strength.
pub fn install(key: &str, look: &Look) -> Result<(), String> {
    if !valid_key(key) {
        return Err(format!("no camera and style for this look ({key})"));
    }
    // checked before anything changes: an invalid look never replaces the one in use or becomes the standard
    let bytes = look.to_bytes()?;
    let path = look_path(key);
    {
        let _g = WRITE.lock().unwrap();
        move_to_replaced(&path)?;
        write_atomic(&path, &bytes)?;
    }
    change(key, |s| {
        s.looks.retain(|e| e.key != key);
        s.looks.push(Entry { key: key.to_string(), enabled: true, amount: 1.0 });
    })
}

/// Switch the learned look of `key` on or off and set its strength (0…1).
pub fn set(key: &str, enabled: bool, amount: f32) -> Result<(), String> {
    change(key, |s| {
        if let Some(e) = s.looks.iter_mut().find(|e| e.key == key) {
            e.enabled = enabled;
            e.amount = amount.clamp(0.0, 1.0);
        }
    })
}

/// Reset: the learned look of `key` stops being the standard; its file goes to `replaced/` (kept, not deleted).
pub fn reset(key: &str) -> Result<(), String> {
    {
        let _g = WRITE.lock().unwrap();
        move_to_replaced(&look_path(key))?;
    }
    change(key, |s| s.looks.retain(|e| e.key != key))
}

/// Keys whose previews are due, and the state's revision.
pub fn stale() -> (Vec<String>, u64) {
    let s = read_state();
    (s.stale, s.revision)
}

/// The previews of `keys` were renewed for revision `revision`: remove them from the due list (only if nothing
/// changed in between).
pub fn mark_fresh(keys: &[String], revision: u64) {
    let _g = WRITE.lock().unwrap();
    let mut s = read_state();
    if s.revision != revision {
        return;
    }
    s.stale.retain(|k| !keys.contains(k));
    let _ = write_state(&s);
}

// ---------------------------------------------------------------------------------------------
// active looks

struct Active {
    key: String,
    look: Arc<Look>,
    amount: f32,
}

static ACTIVE: RwLock<Option<Vec<Active>>> = RwLock::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Read the state and the switched-on looks again.
pub fn reload() {
    let s = read_state();
    let list: Vec<Active> = s
        .looks
        .iter()
        .filter(|e| e.enabled)
        .filter_map(|e| Some(Active { key: e.key.clone(), look: Arc::new(Look::file(&look_path(&e.key))?), amount: e.amount }))
        .collect();
    *ACTIVE.write().unwrap() = Some(list);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Raised whenever the active learned looks change (part of the keys of cached renderings).
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The switched-on learned look of `key` and its strength.
pub fn active(key: &str) -> Option<(Arc<Look>, f32)> {
    if ACTIVE.read().unwrap().is_none() {
        reload();
    }
    let g = ACTIVE.read().unwrap();
    g.as_ref()?.iter().find(|a| a.key == key).map(|a| (a.look.clone(), a.amount))
}

type Entries = Vec<(Entry, Option<serde_json::Value>)>;
static ENTRIES: Mutex<Option<(u64, Arc<Entries>)>> = Mutex::new(None);

/// All learned looks (switched on or not) with their stored metadata; read again only after a change (cheap to call
/// every frame).
pub fn entries() -> Arc<Entries> {
    if ACTIVE.read().unwrap().is_none() {
        reload();
    }
    let g = generation();
    let mut c = ENTRIES.lock().unwrap();
    if let Some((gen, e)) = c.as_ref() {
        if *gen == g {
            return e.clone();
        }
    }
    let e: Arc<Entries> = Arc::new(
        read_state()
            .looks
            .into_iter()
            .map(|e| {
                let meta = Look::file(&look_path(&e.key)).and_then(|l| serde_json::from_str(&l.meta).ok());
                (e, meta)
            })
            .collect(),
    );
    *c = Some((g, e.clone()));
    e
}

// ---------------------------------------------------------------------------------------------
// photos

/// A RAW the learning may use.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    /// shooting day `YYYY-MM-DD`
    pub day: String,
    pub folder: String,
    /// camera as the library names it (for sampling before the makernotes are read; may be empty)
    pub camera: String,
    /// small preview of the photo in the app's cache, if there is one (statistics, filmstrip; never the original)
    pub thumb: Option<PathBuf>,
}

/// Photos whose makernote is read per camera at most (spread over the shooting days, see [`sample`]).
pub const SAMPLE_PER_CAMERA: usize = 400;

/// Spread the candidates of each camera (library name) over its shooting days, at most `per_camera` of them: reading
/// the picture style of every RAW on a network drive takes hours, a spread sample is enough to find the styles and
/// to learn (which uses at most [`learn_look::MAX_PHOTOS`] photos, at most 4 per day).
pub fn sample(cands: &[Candidate], per_camera: usize) -> Vec<Candidate> {
    let mut by_camera: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for (i, c) in cands.iter().enumerate() {
        by_camera.entry(c.camera.as_str()).or_default().push(i);
    }
    let mut out = Vec::new();
    for idx in by_camera.values() {
        let keys: Vec<(String, String)> = idx.iter().map(|&i| (cands[i].day.clone(), cands[i].path.display().to_string())).collect();
        out.extend(learn_look::select(&keys, per_camera).into_iter().map(|k| cands[idx[k]].clone()));
    }
    out
}

/// What the makernote says about a candidate (reads the start of the file only).
#[derive(Clone, Debug)]
pub struct Scanned {
    pub candidate: Candidate,
    /// short camera name (`x100v`)
    pub camera: String,
    /// camera as the file names it (maker and model)
    pub camera_name: String,
    /// style key (`provia_h+1_s+1`)
    pub style: String,
    /// adaptive camera setting (left out)
    pub adaptive: Option<&'static str>,
    pub has_jpeg: bool,
}

impl Scanned {
    pub fn key(&self) -> String {
        format!("{}/{}", self.camera, self.style)
    }
}

/// Could there be a camera JPEG next to the RAW? `false` only when a recent listing of the folder has no file of that
/// name with a JPEG ending (in any case): one listing per folder instead of four look-ups per file on a network drive.
/// `true` when unsure (the look-ups of [`camera_jpeg`] decide, so the result is the same as without the listing).
fn jpeg_possible(raw: &Path) -> bool {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};
    /// a listing counts this long (new photos copied into the folder meanwhile are seen after it)
    const FRESH: Duration = Duration::from_secs(60);
    type Listings = HashMap<PathBuf, (Instant, Option<Arc<HashSet<String>>>)>;
    static LISTINGS: Mutex<Option<Listings>> = Mutex::new(None);
    let (Some(dir), Some(stem)) = (raw.parent(), raw.file_stem()) else { return true };
    // names with other letters may be stored in another Unicode form than the path spells them: only the look-ups know
    if !stem.to_string_lossy().is_ascii() {
        return true;
    }
    let cached = {
        let g = LISTINGS.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().and_then(|m| m.get(dir)).filter(|(t, _)| t.elapsed() < FRESH).map(|(_, l)| l.clone())
    };
    let names = match cached {
        Some(l) => l,
        None => {
            raw::picture_style::simulate_io(0);
            let l = std::fs::read_dir(dir).ok().map(|rd| Arc::new(rd.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect::<HashSet<String>>()));
            let mut g = LISTINGS.lock().unwrap_or_else(|e| e.into_inner());
            let m = g.get_or_insert_with(HashMap::new);
            if m.len() > 4096 {
                m.retain(|_, (t, _)| t.elapsed() < FRESH);
            }
            m.insert(dir.to_path_buf(), (Instant::now(), l.clone()));
            l
        }
    };
    let Some(names) = names else { return true };
    let stem = stem.to_string_lossy().to_lowercase();
    ["jpg", "jpeg"].iter().any(|e| names.contains(&format!("{stem}.{e}")))
}

/// Camera JPEG next to the RAW (same name, `.JPG`/`.jpg`/`.jpeg`), if there is one.
pub fn camera_jpeg(raw: &Path) -> Option<PathBuf> {
    ["JPG", "jpg", "JPEG", "jpeg"].iter().map(|e| raw.with_extension(e)).find(|p| {
        raw::picture_style::simulate_io(0);
        p.is_file()
    })
}

/// Read picture style and camera of a candidate; `None` for Sigma (Foveon stays out), finished images and styles
/// that cannot be learned.
pub fn scan(c: &Candidate) -> Option<Scanned> {
    scan_checked(c).ok().flatten()
}

/// [`scan`]; `Err`: the file could not be read (not "no style", so not to be noted in the cache).
fn scan_checked(c: &Candidate) -> Result<Option<Scanned>, ()> {
    let Some(ps) = raw::picture_style::read_checked(&c.path)? else { return Ok(None) };
    if ps.maker.to_ascii_uppercase().contains("SIGMA") || !learn_look::learnable_style(&ps) {
        return Ok(None);
    }
    Ok(scan_from(c, &ps, true))
}

/// `listing`: ask the folder listing first whether a camera JPEG can be there ([`jpeg_possible`]).
fn scan_from(c: &Candidate, ps: &raw::picture_style::PictureStyle, listing: bool) -> Option<Scanned> {
    let model = ps.model.trim();
    let maker = ps.maker.split_whitespace().next().unwrap_or("").trim_end_matches(',');
    let camera_name = if model.to_ascii_uppercase().starts_with(&maker.to_ascii_uppercase()) { model.to_string() } else { format!("{maker} {model}") };
    Some(Scanned { candidate: c.clone(), camera: ps.camera_short(), camera_name, style: learn_look::style_key(ps), adaptive: learn_look::adaptive(ps), has_jpeg: (!listing || jpeg_possible(&c.path)) && camera_jpeg(&c.path).is_some() })
}

const ADAPTIVE: [&str; 6] = ["DR Auto", "Auto Lighting Optimizer", "DRO", "Highlight correction", "Shadow correction", "Auto"];

fn scan_cache_path() -> Option<PathBuf> {
    Some(sysdirs::app_cache_dir(library::APP)?.join("learn-look-scan.tsv"))
}

/// Picture styles read before, kept in the app's cache folder (`learn-look-scan.tsv`: path, size, modification time
/// → camera, name, style, adaptive setting, or `-`), so the files are read once and not on every opening.
#[derive(Default)]
pub struct ScanCache {
    map: HashMap<String, (u64, u64, String)>,
    /// entries read in this run and not saved yet
    fresh: Vec<String>,
}

impl ScanCache {
    fn parse(t: &str, map: &mut HashMap<String, (u64, u64, String)>) {
        for l in t.lines() {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            if let [path, size, mtime, rest] = f[..] {
                if let (Ok(a), Ok(b)) = (size.parse(), mtime.parse()) {
                    map.insert(path.to_string(), (a, b, rest.to_string()));
                }
            }
        }
    }

    /// The cache file (empty when there is none).
    pub fn load() -> Self {
        let mut c = ScanCache::default();
        if let Some(t) = scan_cache_path().and_then(|p| std::fs::read_to_string(p).ok()) {
            Self::parse(&t, &mut c.map);
        }
        c
    }

    /// Entries in the cache.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Is there a result for this file? (No file access: for the estimate of what is still to read.)
    pub fn knows(&self, path: &Path) -> bool {
        self.map.contains_key(&path.display().to_string())
    }

    fn get(&self, key: &str, size: u64, mtime: u64) -> Option<String> {
        self.map.get(key).filter(|(a, b, _)| *a == size && *b == mtime).map(|(_, _, r)| r.clone())
    }

    fn put(&mut self, key: String, size: u64, mtime: u64, rest: String) {
        if self.map.get(&key).is_none_or(|(a, b, r)| *a != size || *b != mtime || *r != rest) {
            self.fresh.push(key.clone());
            self.map.insert(key, (size, mtime, rest));
        }
    }

    /// Write the new entries: merged into the file as it is now (another reader may have saved meanwhile), atomically.
    pub fn save(&mut self) {
        if self.fresh.is_empty() {
            return;
        }
        let Some(p) = scan_cache_path() else { return };
        let mut all = HashMap::new();
        if let Ok(t) = std::fs::read_to_string(&p) {
            Self::parse(&t, &mut all);
        }
        for k in self.fresh.drain(..) {
            if let Some(v) = self.map.get(&k) {
                all.insert(k, v.clone());
            }
        }
        // merged entries of other readers count as known from now on
        for (k, v) in &all {
            self.map.entry(k.clone()).or_insert_with(|| v.clone());
        }
        let mut keys: Vec<&String> = all.keys().collect();
        keys.sort();
        let mut out = String::new();
        for k in keys {
            let (a, b, r) = &all[k];
            out.push_str(&format!("{k}\t{a}\t{b}\t{r}\n"));
        }
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = write_atomic(&p, out.as_bytes());
    }
}

/// Size and modification time (seconds) of a file.
fn stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    raw::picture_style::simulate_io(0);
    Some((m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())))
}

/// Result line of the cache for a read file.
fn cache_line(s: Option<&Scanned>) -> String {
    match s {
        Some(s) => format!("{}\t{}\t{}\t{}", s.camera, s.camera_name, s.style, s.adaptive.unwrap_or("")),
        None => "-".to_string(),
    }
}

/// What a candidate's picture style is: from the cache when size and modification time match, else read from the
/// file (only its head, see [`raw::picture_style::read`]) and noted in the cache. Outer `None`: the file is not there;
/// inner `None`: no learnable picture style.
pub fn scan_cached(c: &Candidate, cache: &Mutex<ScanCache>) -> Option<Option<Scanned>> {
    let key = c.path.display().to_string();
    let (size, mtime) = stamp(&c.path)?;
    let hit = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key, size, mtime);
    let rest = match hit {
        Some(r) => r,
        None => match scan_checked(c) {
            Ok(s) => {
                let r = cache_line(s.as_ref());
                cache.lock().unwrap_or_else(|e| e.into_inner()).put(key, size, mtime, r.clone());
                r
            }
            // a read error (network drive dropped out after the stamp): not counted now, read again next time
            Err(()) => return Some(None),
        },
    };
    let f: Vec<&str> = rest.split('\t').collect();
    Some(match f[..] {
        [camera, camera_name, style, adaptive] => Some(Scanned {
            candidate: c.clone(),
            camera: camera.into(),
            camera_name: camera_name.into(),
            style: style.into(),
            adaptive: ADAPTIVE.iter().find(|a| **a == adaptive).copied(),
            has_jpeg: jpeg_possible(&c.path) && camera_jpeg(&c.path).is_some(),
        }),
        _ => None,
    })
}

/// [`scan_cached`] for many candidates with `threads` readers at once (a network drive answers each file late; several
/// requests at a time overlap that wait), with the cache in the app's cache folder. Results in the order of `cands`,
/// the same as one reader would give. `None`: cancelled.
pub fn scan_all(cands: &[Candidate], threads: usize, keep_going: &(dyn Fn() -> bool + Sync), progress: &(dyn Fn(usize, usize) + Sync)) -> Option<Vec<Scanned>> {
    use std::sync::atomic::AtomicUsize;
    let cache = Mutex::new(ScanCache::load());
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<Option<Scanned>>>> = (0..cands.len()).map(|_| Mutex::new(None)).collect();
    progress(0, cands.len());
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, 16) {
            s.spawn(|| loop {
                if !keep_going() {
                    return;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(c) = cands.get(i) else { return };
                *results[i].lock().unwrap_or_else(|e| e.into_inner()) = scan_cached(c, &cache);
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                if d % 10 == 0 {
                    progress(d, cands.len());
                }
            });
        }
    });
    cache.into_inner().unwrap_or_else(|e| e.into_inner()).save();
    if !keep_going() {
        return None;
    }
    progress(cands.len(), cands.len());
    Some(results.into_iter().filter_map(|r| r.into_inner().unwrap_or_else(|e| e.into_inner()).flatten()).collect())
}

/// The old way of [`scan_all`] (one reader, the 4 MB head, no cache), for measurements only.
pub fn scan_all_one_by_one_without_cache(cands: &[Candidate]) -> Vec<Scanned> {
    let mut out = Vec::new();
    for c in cands {
        if stamp(&c.path).is_none() {
            continue;
        }
        let Some(ps) = raw::picture_style::read_large(&c.path) else { continue };
        if ps.maker.to_ascii_uppercase().contains("SIGMA") || !learn_look::learnable_style(&ps) {
            continue;
        }
        if let Some(s) = scan_from(c, &ps, false) {
            out.push(s);
        }
    }
    out
}

/// Develop a preview-size stage of `doc` to an 8-bit image in sensor orientation (GPU when available).
fn render(doc: &RawDoc, s: &Settings) -> RgbImage {
    let st = doc.preview_stage(s);
    let img = doc.render_stage_gpu(&st, s).unwrap_or_else(|| doc.render_stage(&st, s));
    RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).expect("image size")
}

fn rotate(img: &RgbImage, quarters: u8) -> RgbImage {
    use image::imageops::{rotate180, rotate270, rotate90};
    match quarters % 4 {
        1 => rotate90(img),
        2 => rotate180(img),
        3 => rotate270(img),
        _ => img.clone(),
    }
}

/// The camera's rendering of the shot: the camera JPEG next to it (wins when present) or the embedded preview.
pub fn reference(path: &Path) -> Result<(RgbImage, ReferenceSource), String> {
    if let Some(j) = camera_jpeg(path) {
        let img = image::open(&j).map_err(|e| format!("{}: {e}", j.display()))?.to_rgb8();
        return Ok((img, ReferenceSource::CameraJpeg));
    }
    let f = raw::RawFile::open(path).map_err(|e| e.to_string())?;
    let p = f.preview().ok_or("no embedded preview")?;
    Ok((p, ReferenceSource::Embedded))
}

/// Reference brought near the size of the development (block means do not need more) and placed on it: as stored,
/// otherwise turned by quarter turns (camera JPEGs and previews are normally stored unturned).
fn placed(base: &RgbImage, reference: RgbImage) -> (RgbImage, Alignment) {
    let long = base.width().max(base.height());
    let reference = if reference.width().max(reference.height()) > long * 3 / 2 {
        let f = long as f32 / reference.width().max(reference.height()) as f32;
        image::imageops::thumbnail(&reference, (reference.width() as f32 * f).round() as u32, (reference.height() as f32 * f).round() as u32)
    } else {
        reference
    };
    let mut best = (reference.clone(), learn_look::align(base, &reference));
    if best.1.correlation >= learn_look::MIN_CORRELATION {
        return best;
    }
    for q in 1..4u8 {
        let r = rotate(&reference, q);
        let a = learn_look::align(base, &r);
        if a.correlation > best.1.correlation {
            best = (r, a);
        }
    }
    best
}

/// Rendered images of one photo for learning: plain development (exposure matched to the reference), current
/// standard development (same exposure), reference placed on it.
pub struct Rendered {
    pub base: RgbImage,
    pub current: RgbImage,
    pub reference: RgbImage,
    pub alignment: Alignment,
    pub source: ReferenceSource,
    pub exposure_offset: f32,
    /// as-shot white balance in Kelvin
    pub kelvin: f32,
}

/// What learning is doing with a photo or the fit (live display of the "Kamera-Looks" window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// developing the RAW and reading the camera's rendering
    Reading,
    /// placing the camera's rendering on the development
    Aligning,
    /// taking block pairs
    Pairing,
    /// fitting a stage on the training photos
    Fitting(learn_look::Stage),
    /// measuring on the check photos
    Checking,
}

/// Live events of [`learn`].
#[derive(Clone, Copy, Debug)]
pub enum Event<'a> {
    Phase(Phase),
    /// a photo is being worked on (`done` of `total` finished); `preview` its development, small
    Photo { done: usize, total: usize, name: &'a str, preview: Option<&'a RgbImage> },
    /// a photo is finished: its mean ΔE00 today (standard development against the camera's rendering)
    PhotoDone { name: &'a str, delta_e_today: f32 },
    /// check result (median ΔE00) of a variant: `neutral`, `current`, `previous`, `matrix`, `curves`, `table`
    Measured(&'a str, f32),
}

/// Develop one candidate for learning (read only). `None`: cancelled. Errors: unreadable, no reference, alignment
/// below [`learn_look::MIN_CORRELATION`], too few midtones. `current` is the standard development without a learned
/// look (what the built-in development does today) unless `with_learned`.
pub fn render_photo(path: &Path, with_learned: bool, keep_going: &dyn Fn() -> bool) -> Result<Option<Rendered>, String> {
    render_photo_observed(path, with_learned, keep_going, &|_| {})
}

fn render_photo_observed(path: &Path, with_learned: bool, keep_going: &dyn Fn() -> bool, observe: &dyn Fn(Phase)) -> Result<Option<Rendered>, String> {
    observe(Phase::Reading);
    let Some(mut doc) = RawDoc::open(path, keep_going)? else { return Ok(None) };
    if doc.finished {
        return Err("not a RAW".into());
    }
    if !with_learned {
        doc.learn_key = None;
    }
    let (reference, source) = reference(path)?;
    doc.plain.store(true, Ordering::Relaxed);
    let mut s = Settings::default();
    let mut base = render(&doc, &s);
    observe(Phase::Aligning);
    let (reference, alignment) = placed(&base, reference);
    if alignment.correlation < learn_look::MIN_CORRELATION {
        return Err(format!("alignment {:.2}", alignment.correlation));
    }
    // exposure: two passes on the reference's midtones (the difference is not part of the look)
    observe(Phase::Pairing);
    let mut offset = 0.0;
    for _ in 0..2 {
        if !keep_going() {
            return Ok(None);
        }
        let pairs = learn_look::block_pairs(&base, None, &reference, &alignment);
        let Some(ev) = learn_look::midtone_ev(&pairs) else { return Err("too few midtones".into()) };
        if ev.abs() < 0.01 {
            break;
        }
        s.exposure = (s.exposure - ev as f64).clamp(-3.0, 3.0);
        offset += ev;
        base = render(&doc, &s);
    }
    doc.plain.store(false, Ordering::Relaxed);
    let current = render(&doc, &s);
    Ok(Some(Rendered { base, current, reference, alignment, source, exposure_offset: offset, kelvin: doc.as_shot.0 }))
}

/// Block pairs of one candidate (see [`render_photo`]): the photo, the as-shot white balance in Kelvin and a small
/// image of its development (for the live display).
pub fn extract(c: &Candidate, keep_going: &dyn Fn() -> bool) -> Result<Option<(Photo, f32)>, String> {
    Ok(extract_observed(c, keep_going, &|_| {})?.map(|(p, k, _)| (p, k)))
}

fn extract_observed(c: &Candidate, keep_going: &dyn Fn() -> bool, observe: &dyn Fn(Phase)) -> Result<Option<(Photo, f32, RgbImage)>, String> {
    let Some(r) = render_photo_observed(&c.path, false, keep_going, observe)? else { return Ok(None) };
    let pairs = learn_look::block_pairs(&r.base, Some(&r.current), &r.reference, &r.alignment);
    let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let small = small(&r.current, 320);
    Ok(Some((Photo { name, day: c.day.clone(), folder: c.folder.clone(), source: Some(r.source), exposure_offset: r.exposure_offset, correlation: r.alignment.correlation, pairs }, r.kelvin, small)))
}

// ---------------------------------------------------------------------------------------------
// tone curve matched per photo to the camera's rendering ("An Kamera-JPEG angleichen")

/// Look value of the tone curve matched per photo to the camera's own rendering of the shot (camera JPEG next to the
/// RAW, else the embedded preview). Chosen like a camera look; never a default.
pub const MATCHED_LOOK: &str = "match:camera";
/// Internal look value: a table without any change (the plain development, for computing the matched curve).
pub const IDENTITY_LOOK: &str = "match:identity";

/// A look that changes nothing (the development under it is the plain one, like under a camera look).
pub fn identity_look() -> Look {
    Look::from_table(IDENTITY_LOOK, "{}", 2, vec![[0.0; 3]; 8]).expect("table size")
}

/// The tone curve of this photo matched to the camera's rendering, as a look ([`MATCHED_LOOK`]): the plain development
/// at the photo's default settings (as-shot white balance, exposure 0) and the camera's rendering placed on it give a
/// curve on the luminance ([`learn_look::matched_curve`], [`learn_look::apply_matched_curve_luminance`]: hue and
/// saturation stay), baked into a 33³ table. It depends only on the photo, so it is computed once per opened photo
/// (see `RawDoc::look_for`) and holds for every zoom and crop. Rendered on the processor (it may be computed while
/// another rendering prepares its parameters). Idea: RawTherapee's "Auto-Matched Curve"; own implementation.
/// Errors: no camera rendering, alignment below [`learn_look::MIN_CORRELATION`], too few pixels.
pub fn matched_look(doc: &RawDoc) -> Result<Look, String> {
    let (base, reference, a) = matched_inputs(doc)?;
    let warped = learn_look::warp_reference(base.width(), base.height(), &reference, &a);
    let curve = learn_look::matched_curve(&base, &warped).ok_or("too few pixels")?;
    let n = learn_look::LUT_N;
    let d: Vec<[f32; 3]> = (0..n * n * n)
        .map(|k| {
            let x = [k / (n * n), (k / n) % n, k % n].map(|v| v as f32 / (n - 1) as f32);
            let y = learn_look::apply_matched_curve_luminance(&curve, x);
            [y[0] - x[0], y[1] - x[1], y[2] - x[2]]
        })
        .collect();
    let meta = serde_json::json!({"origin": "matched", "interpolation": "tetrahedral", "curve": curve.to_vec(), "correlation": a.correlation});
    Look::from_table(MATCHED_LOOK, &meta.to_string(), n, d).ok_or_else(|| "table size".to_string())
}

/// Plain development at default settings (processor), the camera's rendering placed on it and the alignment.
fn matched_inputs(doc: &RawDoc) -> Result<(RgbImage, RgbImage, Alignment), String> {
    if doc.finished {
        return Err("not a RAW".into());
    }
    let mut s = Settings::default();
    s.raw.look = IDENTITY_LOOK.into();
    let st = doc.preview_stage(&s);
    let img = doc.render_stage(&st, &s);
    let base = RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).ok_or("image size")?;
    let (reference, _) = reference(&doc.path)?;
    let (reference, a) = placed(&base, reference);
    if a.correlation < learn_look::MIN_CORRELATION {
        return Err(format!("alignment {:.2}", a.correlation));
    }
    Ok((base, reference, a))
}

/// For measuring [`MATCHED_LOOK`] as the app shows it (no exposure matching): the plain development at default
/// settings, the same with the matched look, the standard development (default settings) – all of the same size – the
/// camera's rendering placed on them and the alignment. `None`: cancelled.
pub fn matched_views(path: &Path, keep_going: &dyn Fn() -> bool) -> Result<Option<([RgbImage; 3], RgbImage, Alignment)>, String> {
    let Some(doc) = RawDoc::open(path, keep_going)? else { return Ok(None) };
    let (plain, reference, a) = matched_inputs(&doc)?;
    let to_rgb = |s: &Settings| {
        let st = doc.preview_stage(s);
        let img = doc.render_stage(&st, s);
        RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).expect("image size")
    };
    let mut s = Settings::default();
    s.raw.look = MATCHED_LOOK.into();
    let matched = to_rgb(&s);
    let current = to_rgb(&Settings::default());
    Ok(Some(([plain, matched, current], reference, a)))
}

/// `img` reduced to at most `long` pixels on the long edge.
pub fn small(img: &RgbImage, long: u32) -> RgbImage {
    let f = long as f32 / img.width().max(img.height()).max(1) as f32;
    if f >= 1.0 {
        return img.clone();
    }
    image::imageops::thumbnail(img, ((img.width() as f32 * f).round() as u32).max(1), ((img.height() as f32 * f).round() as u32).max(1))
}

/// Something the user should know about the photos of a camera and style.
#[derive(Clone, Debug, PartialEq)]
pub enum Warning {
    /// all photos come from one folder
    OneFolder,
    /// photos left out because of an adaptive camera setting (setting, count)
    Adaptive(&'static str, usize),
    /// photos with an adaptive camera setting used on request (setting, count)
    AdaptiveIncluded(&'static str, usize),
    /// both warm (below 4000 K) and daylight (above 5000 K) white balances in good numbers
    MixedWhiteBalance,
    /// few skin-like colours among the blocks
    ThinSkin,
    /// few strongly saturated colours among the blocks
    ThinSaturated,
}

/// Result of learning one camera and style.
pub struct Learned {
    pub key: String,
    pub outcome: learn_look::Outcome,
    pub photos: Vec<Photo>,
    /// the RAW of each photo (same order as `photos`)
    pub paths: Vec<PathBuf>,
    /// as-shot white balance in Kelvin of each photo (same order)
    pub kelvins: Vec<f32>,
    /// (file name, why it was not used)
    pub skipped: Vec<(String, String)>,
    pub warnings: Vec<Warning>,
    /// candidate whose check result is closest to the median (for the before/after view)
    pub example: Option<PathBuf>,
}

impl Learned {
    /// Is the photo `i` a check photo (of a check day, not used to fit)?
    pub fn is_check(&self, i: usize) -> bool {
        self.photos.get(i).is_some_and(|p| self.outcome.check_day_list.iter().any(|d| *d == p.day))
    }
}

/// Check days stored in a learned look (`check_days_list`), for a renewal on the same photos.
pub fn check_days_of(look: &Look) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&look.meta).ok()?;
    let d: Vec<String> = v["check_days_list"].as_array()?.iter().filter_map(|d| d.as_str().map(str::to_string)).collect();
    (!d.is_empty()).then_some(d)
}

/// Learn the look of the photos `scanned` (all of one camera and style). Photos with an adaptive camera setting are
/// left out (with a warning) unless `include_adaptive`. With `previous` (the learned look in use so far) the result
/// is measured against it on the same check days and only offered if clearly better. Quiet: one photo at a time;
/// `observe` gets the live events; `keep_going` false = cancel (returns `None`).
pub fn learn(scanned: &[Scanned], include_adaptive: bool, today: &str, previous: Option<&Look>, keep_going: &dyn Fn() -> bool, observe: &dyn Fn(Event)) -> Option<Learned> {
    let first = scanned.first()?;
    let (camera, style) = (first.camera.clone(), first.style.clone());
    let mut warnings = Vec::new();
    let mut adaptive: Vec<(&'static str, usize)> = Vec::new();
    for s in scanned {
        if let Some(a) = s.adaptive {
            match adaptive.iter_mut().find(|(n, _)| *n == a) {
                Some(e) => e.1 += 1,
                None => adaptive.push((a, 1)),
            }
        }
    }
    warnings.extend(adaptive.into_iter().map(|(a, n)| if include_adaptive { Warning::AdaptiveIncluded(a, n) } else { Warning::Adaptive(a, n) }));
    let usable: Vec<&Scanned> = scanned.iter().filter(|s| include_adaptive || s.adaptive.is_none()).collect();
    let keys: Vec<(String, String)> = usable.iter().map(|s| (s.candidate.day.clone(), s.candidate.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())).collect();
    let chosen = learn_look::select(&keys, learn_look::MAX_PHOTOS);
    let mut photos = Vec::new();
    let mut paths = Vec::new();
    let mut kelvins = Vec::new();
    let mut skipped = Vec::new();
    let mut last: Option<RgbImage> = None;
    for (n, &i) in chosen.iter().enumerate() {
        let c = &usable[i].candidate;
        let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        observe(Event::Photo { done: n, total: chosen.len(), name: &name, preview: last.as_ref() });
        if !keep_going() {
            return None;
        }
        match extract_observed(c, keep_going, &|ph| observe(Event::Phase(ph))) {
            Ok(Some((p, k, img))) => {
                let m = learn_look::measure(&[&p], &|q| q.current);
                observe(Event::PhotoDone { name: &name, delta_e_today: m.mean });
                observe(Event::Photo { done: n + 1, total: chosen.len(), name: &name, preview: Some(&img) });
                last = Some(img);
                photos.push(p);
                paths.push(c.path.clone());
                kelvins.push(k);
            }
            Ok(None) => return None,
            Err(e) => skipped.push((name, e)),
        }
    }
    let mut folders: Vec<&str> = photos.iter().map(|p| p.folder.as_str()).collect();
    folders.sort_unstable();
    folders.dedup();
    if folders.len() == 1 {
        warnings.push(Warning::OneFolder);
    }
    if mixed_white_balance(&kelvins) {
        warnings.push(Warning::MixedWhiteBalance);
    }
    let (skin, sat) = learn_look::coverage(&photos);
    if skin < learn_look::THIN_SKIN {
        warnings.push(Warning::ThinSkin);
    }
    if sat < learn_look::THIN_SATURATED {
        warnings.push(Warning::ThinSaturated);
    }
    if !keep_going() {
        return None;
    }
    let opts = learn_look::Options { previous: previous.cloned(), check_days: previous.and_then(check_days_of) };
    let outcome = learn_look::learn_with(&photos, &camera, &style, today, &opts, &|step| match step {
        learn_look::Step::Fitting(st) => observe(Event::Phase(Phase::Fitting(st))),
        learn_look::Step::Checking => observe(Event::Phase(Phase::Checking)),
        learn_look::Step::Measured(v, m) => observe(Event::Measured(v, m.median)),
    });
    // example: a check photo (other days than the training) with a typical result
    let example = outcome.variant("current").and_then(|(_, c)| {
        let usable: Vec<usize> = photos.iter().enumerate().filter(|(_, p)| p.pairs.len() >= 20).map(|(i, _)| i).collect();
        let check: Vec<usize> = usable.iter().copied().filter(|&i| outcome.check_day_list.iter().any(|d| *d == photos[i].day)).collect();
        let mut v: Vec<(f32, usize)> = check.iter().zip(&c.per_photo).map(|(&i, e)| (*e, i)).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v.get(v.len() / 2).map(|(_, i)| paths[*i].clone())
    });
    Some(Learned { key: format!("{camera}/{style}"), outcome, photos, paths, kelvins, skipped, warnings, example })
}

/// Both warm (below 4000 K) and daylight (above 5000 K) white balances, each in at least a fifth of the photos.
pub fn mixed_white_balance(kelvins: &[f32]) -> bool {
    let warm = kelvins.iter().filter(|k| **k > 0.0 && **k < 4000.0).count();
    let day = kelvins.iter().filter(|k| **k > 5000.0).count();
    !kelvins.is_empty() && warm * 5 >= kelvins.len() && day * 5 >= kelvins.len()
}

/// The four views of one photo for the comparison, at preview size and the same frame: plain development
/// (neutral), standard development as it is now (with a switched-on learned look), the camera's rendering placed on
/// it; the exposure is matched to the camera's rendering (the difference is not part of a look).
pub struct Views {
    pub neutral: RgbImage,
    pub current: RgbImage,
    pub camera: RgbImage,
    pub source: ReferenceSource,
    pub exposure_offset: f32,
}

/// [`Views`] of one RAW (read only). `None`: cancelled.
pub fn views(path: &Path, keep_going: &dyn Fn() -> bool) -> Result<Option<Views>, String> {
    let Some(r) = render_photo(path, true, keep_going)? else { return Ok(None) };
    let camera = learn_look::warp_reference(r.base.width(), r.base.height(), &r.reference, &r.alignment);
    Ok(Some(Views { neutral: r.base, current: r.current, camera, source: r.source, exposure_offset: r.exposure_offset }))
}

/// Before/after of one photo at preview size: (standard development today, plain development with `look` at full
/// strength, the camera's rendering), all in sensor orientation and the same crop as far as the alignment allows.
pub fn before_after(path: &Path, look: &Look, keep_going: &dyn Fn() -> bool) -> Result<Option<[RgbImage; 3]>, String> {
    let Some(r) = render_photo(path, false, keep_going)? else { return Ok(None) };
    let mut after = r.base.clone();
    look.apply(&mut after);
    let reference = learn_look::warp_reference(r.base.width(), r.base.height(), &r.reference, &r.alignment);
    Ok(Some([r.current, after, reference]))
}

/// Write the `.cube` of a learned look (33³, sRGB → sRGB, full strength).
pub fn export_cube(key: &str, target: &Path) -> Result<(), String> {
    export(&Look::file(&look_path(key)).ok_or("look not found")?, target)
}

// ---------------------------------------------------------------------------------------------
// versions and sharing

/// `look` with more metadata fields (replacing fields of the same name).
pub fn with_meta(look: &Look, extra: &[(&str, serde_json::Value)]) -> Look {
    let mut v: serde_json::Value = serde_json::from_str(&look.meta).unwrap_or_else(|_| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    for (k, x) in extra {
        v[*k] = x.clone();
    }
    let mut l = look.clone();
    l.meta = v.to_string();
    l
}

/// A stored version of a learned look: the one in use or one in `replaced/`.
#[derive(Clone, Debug)]
pub struct Version {
    pub path: PathBuf,
    /// the look file in use (not in `replaced/`)
    pub current: bool,
    pub meta: String,
    /// when it was replaced (Unix seconds), for the files in `replaced/`
    pub replaced: Option<u64>,
}

/// Versions of the learned look of `key`: the one in use first (if any), then the replaced ones, newest first.
pub fn versions(key: &str) -> Vec<Version> {
    let path = look_path(key);
    let mut out = Vec::new();
    if let Some(l) = Look::file(&path) {
        out.push(Version { path: path.clone(), current: true, meta: l.meta, replaced: None });
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = path.parent().map(|p| p.join(REPLACED));
    let mut old: Vec<Version> = dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().to_string();
            let rest = name.strip_prefix(&format!("{stem}-"))?;
            let mut parts = rest.split('-');
            let stamp: u64 = parts.next()?.parse().ok()?;
            if !parts.all(|x| x.parse::<u32>().is_ok()) || p.extension().is_none_or(|x| x != "x3look") {
                return None;
            }
            let l = Look::file(&p)?;
            Some(Version { path: p, current: false, meta: l.meta, replaced: Some(stamp) })
        })
        .collect();
    old.sort_by(|a, b| b.replaced.cmp(&a.replaced).then(b.path.cmp(&a.path)));
    out.extend(old);
    out
}

/// Go back to the replaced version `path` of `key`: the look in use goes to `replaced/` (kept), the chosen file
/// becomes the look in use, switched on at full strength. Nothing is deleted.
pub fn restore(key: &str, path: &Path) -> Result<(), String> {
    let target = look_path(key);
    let replaced = target.parent().ok_or("no folder")?.join(REPLACED);
    if path.parent() != Some(replaced.as_path()) || !path.is_file() {
        return Err("not a replaced version of this look".into());
    }
    Look::file(path).ok_or("not a look file")?;
    {
        let _g = WRITE.lock().unwrap();
        move_to_replaced(&target)?;
        std::fs::rename(path, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    change(key, |s| {
        s.looks.retain(|e| e.key != key);
        s.looks.push(Entry { key: key.to_string(), enabled: true, amount: 1.0 });
    })
}

/// Prefix of the `.cube` comment line that carries the metadata of a shared look.
const CUBE_META: &str = "graineo-look ";

/// Save a look to share: `.x3look` (the table as the app uses it) or `.cube` (33³, sRGB, also for other programs),
/// both with origin and quality in the metadata and nothing about the photos (see [`learn_look::shareable_meta`]).
pub fn export(look: &Look, target: &Path) -> Result<(), String> {
    let meta = learn_look::shareable_meta(&look.meta, &[]);
    let cube = target.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube"));
    let data = if cube {
        let p = learn_look::provenance(&meta);
        let quality = p.delta_e.map_or("no measurement".to_string(), |d| format!("dE00 {d:.2} against the camera JPEG on check photos"));
        let comments = [format!("Camera look {} ({})", look.name, p.origin.key()), quality, format!("{CUBE_META}{meta}")];
        let lut = look.cube_text(&format!("graineo look {}", look.name), 1.0)?;
        // the comments go after the creator line
        let (head, rest) = lut.split_once('\n').unwrap_or((&lut, ""));
        let mut t = format!("{head}\n");
        for c in &comments {
            t.push_str(&format!("# {c}\n"));
        }
        t.push_str(rest);
        t.into_bytes()
    } else {
        let (n, d) = look.table();
        Look::check_table(&look.name, &meta, n, d)?;
        Look::from_table(&look.name, &meta, n, d.to_vec()).ok_or("table")?.to_bytes()?
    };
    write_atomic(target, &data)
}

/// Is `key` a valid `camera/style` key?
pub fn valid_key(key: &str) -> bool {
    key.split_once('/').is_some_and(|(c, s)| !c.is_empty() && !s.is_empty() && c == safe(c) && s == safe(s) && !s.contains('/'))
}

/// Read a shared look file without installing it: the look (with the metadata it carries, marked as imported) and
/// its key. `.x3look` names its key itself; a `.cube` gets `key` (the camera and style it is loaded for). A file
/// without origin counts as a basis without measurement.
pub fn read_shared(path: &Path, key: Option<&str>) -> Result<(String, Look), String> {
    let cube = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube"));
    // a 65³ `.cube` has about 9 MB, an `.x3look` at most 1.7 MB: larger files are not read at all
    let limit: u64 = if cube { 32 << 20 } else { 4 << 20 };
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if size > limit {
        return Err(format!("{}: file too large for a look ({} MB)", path.display(), size >> 20));
    }
    let (name, meta, n, d) = if cube {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = text.lines().find_map(|l| l.trim_start_matches('#').trim_start().strip_prefix(CUBE_META).map(str::to_string)).unwrap_or_else(|| serde_json::json!({"origin": learn_look::ORIGIN_BASE}).to_string());
        if meta.len() > raw::looks::MAX_META {
            return Err(format!("look metadata too large ({} bytes)", meta.len()));
        }
        let (n, d) = raw::film_profile::cube_table(&text)?;
        (String::new(), meta, n, d)
    } else {
        let l = Look::file(path).ok_or("not a look file (X3LOOK1)")?;
        let (n, d) = l.table();
        (l.name.clone(), l.meta.clone(), n, d.to_vec())
    };
    let key = key.map(str::to_string).filter(|k| !k.is_empty()).unwrap_or(name);
    if !valid_key(&key) {
        return Err(format!("no camera and style for this look ({key})"));
    }
    let (camera, style) = key.split_once('/').unwrap_or_default();
    let had_origin = serde_json::from_str::<serde_json::Value>(&meta).ok().is_some_and(|v| v.get("origin").is_some() || v.get("fit").is_some() || v.get("de_look").is_some());
    let mut extra = vec![("imported", serde_json::json!(true)), ("camera", serde_json::json!(camera)), ("style", serde_json::json!(style))];
    if !had_origin {
        extra.push(("origin", serde_json::json!(learn_look::ORIGIN_BASE)));
    }
    if cube || !meta.contains("interpolation") {
        extra.push(("interpolation", serde_json::json!("tetrahedral")));
    }
    let meta = learn_look::shareable_meta(&meta, &extra);
    Look::check_table(&key, &meta, n, &d)?;
    let look = Look::from_table(&key, &meta, n, d).ok_or("table size")?;
    Ok((key, look))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_look_changes_nothing() {
        let l = identity_look();
        for c in [[0.0, 0.0, 0.0], [0.2, 0.5, 0.9], [1.0, 1.0, 1.0]] {
            let p = l.pixel(c);
            assert!((0..3).all(|k| (p[k] - c[k]).abs() < 1e-5), "{c:?} → {p:?}");
        }
        assert_ne!(MATCHED_LOOK, IDENTITY_LOOK);
    }

    /// A file that cannot be read after its size and time were taken (network drive dropped out) is not noted as
    /// "no picture style" (review 1.3.0, logic N3); a readable file without a style is.
    #[cfg(unix)]
    #[test]
    fn read_errors_are_not_cached_as_no_style() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("graineo-learn-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let cand = |p: &Path| Candidate { path: p.to_path_buf(), day: String::new(), folder: String::new(), camera: String::new(), thumb: None };
        let locked = d.join("IMG_0001.CR3");
        std::fs::write(&locked, b"not a raw file").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // root may read anyway: then there is nothing to test
        if std::fs::File::open(&locked).is_err() {
            let cache = Mutex::new(ScanCache::default());
            assert!(matches!(scan_cached(&cand(&locked), &cache), Some(None)));
            assert!(cache.lock().unwrap().map.is_empty(), "a read error was noted as no style");
        }
        let plain = d.join("IMG_0002.CR3");
        std::fs::write(&plain, b"not a raw file").unwrap();
        let cache = Mutex::new(ScanCache::default());
        assert!(matches!(scan_cached(&cand(&plain), &cache), Some(None)));
        assert_eq!(cache.lock().unwrap().map.len(), 1);
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The folder of the learned looks is global: the tests take turns.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn install_switch_reset_keep_files() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("graineo-learn-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        set_dir(Some(d.clone()));
        let n = 3;
        let look = Look::from_table("cam/std", r#"{"learned":true,"interpolation":"tetrahedral"}"#, n, vec![[0.01, 0.0, -0.01]; n * n * n]).unwrap();
        install("cam/std", &look).unwrap();
        let (l, a) = active("cam/std").expect("active after install");
        assert!(l.tetrahedral() && a == 1.0);
        let g = generation();
        set("cam/std", true, 0.4).unwrap();
        assert!(generation() > g);
        assert_eq!(active("cam/std").unwrap().1, 0.4);
        set("cam/std", false, 0.4).unwrap();
        assert!(active("cam/std").is_none());
        // a second install moves the first file aside, nothing is deleted
        install("cam/std", &look).unwrap();
        assert_eq!(std::fs::read_dir(d.join("cam").join(REPLACED)).unwrap().count(), 1);
        let (stale, rev) = stale();
        assert_eq!(stale, ["cam/std"]);
        mark_fresh(&stale, rev);
        assert!(super::stale().0.is_empty());
        reset("cam/std").unwrap();
        assert!(active("cam/std").is_none() && !look_path("cam/std").exists());
        assert_eq!(std::fs::read_dir(d.join("cam").join(REPLACED)).unwrap().count(), 2);
        let cube = d.join("x.cube");
        assert!(export_cube("cam/std", &cube).is_err());
        set_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A damaged `learned.json` neither switches every look off nor is written over: the looks on disk stay active,
    /// the damaged file is kept next to the new one. Keys like "../x" in it are ignored.
    #[test]
    fn damaged_state_file_is_kept_and_looks_stay_on() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("graineo-learn-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        set_dir(Some(d.clone()));
        let n = 3;
        let look = Look::from_table("cam/std", r#"{"learned":true}"#, n, vec![[0.01, 0.0, -0.01]; n * n * n]).unwrap();
        install("cam/std", &look).unwrap();
        install("cam/vivid", &look).unwrap();
        set("cam/vivid", true, 0.5).unwrap();
        std::fs::write(d.join(STATE), b"{\"looks\": [ {\"key\": \"cam/std\", \"ena").unwrap();
        reload();
        assert!(active("cam/std").is_some() && active("cam/vivid").is_some(), "still on");
        // the next change keeps the damaged file and writes a whole one
        set("cam/vivid", true, 0.3).unwrap();
        let kept: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("learned-broken-")).collect();
        assert_eq!(kept.len(), 1);
        assert!(std::fs::read_to_string(kept[0].path()).unwrap().contains("\"ena"));
        let s = read_state();
        assert_eq!(s.looks.iter().map(|e| (e.key.as_str(), e.amount)).collect::<Vec<_>>(), [("cam/std", 1.0), ("cam/vivid", 0.3)]);
        // keys that would leave the folder are never taken from the file
        std::fs::write(d.join(STATE), br#"{"revision": 3, "looks": [{"key": "../x"}, {"key": "cam/.."}, {"key": "cam/std"}]}"#).unwrap();
        assert_eq!(read_state().looks.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["cam/std"]);
        set_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn versions_restore_and_sharing_keep_every_file() {
        let _s = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("graineo-learn-versions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        set_dir(Some(d.clone()));
        let n = 3;
        let meta = |m: f32| format!(r#"{{"learned":true,"origin":"checked","photos":30,"check_days_list":["2026-01-02"],"interpolation":"tetrahedral","fit":{{"check":{{"current":[2,3,2,3],"learned":[1,2,{m},2]}}}}}}"#);
        let a = Look::from_table("cam/std", &meta(1.5), n, vec![[0.01, 0.0, 0.0]; n * n * n]).unwrap();
        let b = Look::from_table("cam/std", &meta(1.2), n, vec![[0.0, 0.02, 0.0]; n * n * n]).unwrap();
        install("cam/std", &a).unwrap();
        install("cam/std", &b).unwrap();
        let v = versions("cam/std");
        assert_eq!(v.len(), 2);
        assert!(v[0].current && !v[1].current);
        assert_eq!(learn_look::provenance(&v[1].meta).delta_e, Some(1.5));
        assert_eq!(check_days_of(&Look::file(&look_path("cam/std")).unwrap()), Some(vec!["2026-01-02".to_string()]));
        // back to the first version: the second goes to replaced/, nothing is lost
        restore("cam/std", &v[1].path).unwrap();
        let v2 = versions("cam/std");
        assert_eq!(v2.len(), 2);
        assert_eq!(learn_look::provenance(&v2[0].meta).delta_e, Some(1.5));
        assert!(active("cam/std").is_some());
        assert!(restore("cam/std", &look_path("cam/std")).is_err(), "only replaced versions");
        // a style whose name begins like another is not mixed up
        install("cam/std-x", &a).unwrap();
        install("cam/std-x", &a).unwrap();
        assert_eq!(versions("cam/std").len(), 2);
        // sharing: no shooting days in the file; origin and quality stay; a .cube carries them in a comment
        let look = Look::file(&look_path("cam/std")).unwrap();
        for ext in ["x3look", "cube"] {
            let f = d.join(format!("shared.{ext}"));
            export(&look, &f).unwrap();
            let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).to_string();
            assert!(!text.contains("2026-01-02"), "{ext}");
            let (key, back) = read_shared(&f, if ext == "cube" { Some("other/std") } else { None }).unwrap();
            assert_eq!(key, if ext == "cube" { "other/std" } else { "cam/std" });
            let p = learn_look::provenance(&back.meta);
            assert_eq!((p.origin, p.delta_e, p.imported), (learn_look::Origin::Checked, Some(1.5), true), "{ext}");
            assert!(back.tetrahedral());
            for c in [[0.2, 0.4, 0.6], [0.9, 0.1, 0.5]] {
                let (x, y) = (look.pixel(c), back.pixel(c));
                assert!((0..3).all(|k| (x[k] - y[k]).abs() < 1e-3), "{ext}: {x:?} {y:?}");
            }
        }
        // a foreign .cube without our metadata is a basis without measurement and needs a camera and style
        let plain = d.join("plain.cube");
        let identity = Look::from_table("x/y", "{}", 17, vec![[0.0; 3]; 17 * 17 * 17]).unwrap();
        std::fs::write(&plain, identity.cube_text("plain", 1.0).unwrap()).unwrap();
        assert!(read_shared(&plain, None).is_err());
        let (_, l) = read_shared(&plain, Some("cam/std")).unwrap();
        assert_eq!(learn_look::provenance(&l.meta).origin, learn_look::Origin::Base);
        assert!(!valid_key("../x/y") && !valid_key("cam") && valid_key("x100v/provia_h+1"));
        assert!(!valid_key("../x") && !valid_key("./x") && !valid_key("cam/..") && !valid_key("cam/.hidden") && !valid_key(".cam/std"));
        // a camera named ".." (EXIF model of a crafted RAW) never leads out of the looks folder
        assert!(!valid_key("../std") && !valid_key("./std") && !valid_key("cam/..") && !valid_key("cam/.hidden"));
        for key in ["../std", "./std", "cam/..", "../../x", ".../std"] {
            let p = look_path(key);
            assert!(p.starts_with(&d) && p.components().all(|c| !matches!(c, std::path::Component::ParentDir)), "{key}: {p:?}");
            assert_eq!(p.parent().and_then(|x| x.parent()), Some(d.as_path()), "{key}: {p:?}");
            assert!(install(key, &look).is_err(), "{key}");
        }
        // a shared .cube with oversized metadata is refused, and nothing is installed or switched on
        let before = std::fs::read(look_path("cam/std")).ok();
        let evil = d.join("evil.cube");
        let text = identity.cube_text("evil", 1.0).unwrap();
        let (head, rest) = text.split_once('\n').unwrap();
        let pad = "A".repeat(70_000);
        std::fs::write(&evil, format!("{head}\n# {CUBE_META}{{\"source\":{{\"name\":\"x\",\"pad\":\"{pad}\"}}}}\n{rest}")).unwrap();
        assert!(read_shared(&evil, Some("cam/std")).unwrap_err().contains("too large"));
        assert_eq!(std::fs::read(look_path("cam/std")).ok(), before);
        // files far too large for a look are not read at all
        let big = d.join("big.x3look");
        std::fs::write(&big, vec![0u8; 5 << 20]).unwrap();
        assert!(read_shared(&big, None).unwrap_err().contains("too large"));
        // a look that cannot be written correctly is never installed (the one in use stays)
        let mut bad = Look::from_table("cam/std", "{}", n, vec![[0.0; 3]; n * n * n]).unwrap();
        bad.meta = "x".repeat(70_000);
        assert!(install("cam/std", &bad).is_err());
        assert_eq!(std::fs::read(look_path("cam/std")).ok(), before);
        set_dir(None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
