//! "Kamera-Looks": one window for the cameras of the library and their looks (learned, bundled, built-in hue table).
//!
//! Layout (fixed places, the window can be resized and remembers its size): on the left the cameras that have RAW
//! photos in the library, most photos first, each with its picture styles and their status (bundled look, built-in hue
//! table, learned look with date and photos, none), search and filters; in the middle the comparison of one photo
//! (neutral, current, new, camera JPEG; wipe or side by side, coupled zoom and pan) with a filmstrip of the training
//! and check photos; on the right statistics and an honest estimate ("lernbar: ja / eingeschränkt / nein"), the
//! learning job with a live display, the versions of a learned look and sharing (export/import).
//!
//! "Lernen" (the one main button; "Neu lernen" when a learned look exists) runs quietly in the background (two threads,
//! background priority, cancel at any time; it keeps running when the window is closed). A renewal is measured on the
//! same check days as the look in use and only offered if it is clearly better; otherwise "Der bisherige Look bleibt".
//! Replaced looks go to `looks/<camera>/replaced/` and can be restored. Photos are only read; only colour values of
//! the look are stored and shared.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use raw::learn_look::{self as ll, Coverage, Origin};
use rawdev::learn::{self, Candidate, Event, Learned, Phase, Scanned, Warning};

use crate::learn_look_ui::{learned_label, style_label};
use crate::{dialog, lr};

const DEFAULT_SIZE: [f32; 2] = [1280.0, 820.0];
const MIN_SIZE: [f32; 2] = [900.0, 560.0];
const LIST_W: f32 = 290.0;
const SIDE_W: f32 = 340.0;
const STRIP_H: f32 = 84.0;
/// settings key of the remembered window size
const SIZE_KEY: &str = "cameraLooksWindow";
const EXPORT: &str = "camera-looks-export";
const IMPORT: &str = "camera-looks-import";
const GREEN: Color32 = Color32::from_rgb(0x4c, 0xc2, 0x6a);
const YELLOW: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x40);
const RED: Color32 = Color32::from_rgb(0xe0, 0x5c, 0x50);

// ---------------------------------------------------------------------------------------------
// background work

/// A background job of the window (quiet: background priority, few threads).
struct Job<T> {
    cancel: Arc<AtomicBool>,
    out: Arc<Mutex<Option<Option<T>>>>,
}

impl<T: Send + 'static> Job<T> {
    fn start(name: &str, threads: usize, work: impl FnOnce(&dyn Fn() -> bool, &dyn Fn(usize, usize)) -> Option<T> + Send + 'static) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new((0, 0)));
        let out = Arc::new(Mutex::new(None));
        let (c, p, o) = (cancel.clone(), progress.clone(), out.clone());
        let what = name.to_string();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
            library::platform::background_priority();
            pools::set_intent(pools::Intent::Idle);
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build();
            let keep_going = || !c.load(Ordering::Relaxed);
            let report = |d: usize, n: usize| *p.lock().unwrap_or_else(|e| e.into_inner()) = (d, n);
            // a panic ends the job without a result (else the window stayed "busy" and blocked learning until restart)
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match pool {
                Ok(pool) => pool.install(|| work(&keep_going, &report)),
                Err(_) => work(&keep_going, &report),
            }))
            .unwrap_or_else(|_| {
                crate::log(&format!("{what}: panic"));
                None
            });
            *o.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        });
        if spawned.is_err() {
            *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(None);
        }
        Job { cancel, out }
    }

    fn take(&self) -> Option<Option<T>> {
        self.out.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// What the learning job is doing right now (written by the job, read by the window).
#[derive(Default)]
struct Live {
    phase: Option<Phase>,
    done: usize,
    total: usize,
    name: String,
    preview: Option<image::RgbImage>,
    preview_new: bool,
    /// mean ΔE00 per finished photo, standard development today
    today: Vec<f32>,
    /// check medians of the variants so far
    measured: Vec<(String, f32)>,
}

struct Learning {
    key: String,
    label: String,
    renewing: bool,
    job: Job<Learned>,
    live: Arc<Mutex<Live>>,
    started: Instant,
    preview: Option<egui::TextureHandle>,
}

/// Result of learning, kept until the next learning of any camera.
struct Outcome {
    key: String,
    learned: Learned,
    renewing: bool,
    adopted: bool,
}

// ---------------------------------------------------------------------------------------------
// rows

/// A camera and picture style of the library.
#[derive(Clone)]
struct StyleRow {
    key: String,
    style: String,
    camera_name: String,
    scanned: Vec<Scanned>,
    /// suitable photos (no adaptive setting, unless included)
    usable: usize,
    adaptive: usize,
    days: usize,
    folders: usize,
    with_jpeg: usize,
    /// indices into `scanned` of the photos learning would use (at most 4 per day)
    chosen: Vec<usize>,
}

/// A camera with RAW photos in the library.
struct CameraRow {
    photos: usize,
    /// camera as the library names it
    library: String,
    name: String,
    styles: Vec<StyleRow>,
}

fn style_row(key: String, v: Vec<Scanned>, include_adaptive: bool) -> StyleRow {
    let usable: Vec<usize> = (0..v.len()).filter(|&i| include_adaptive || v[i].adaptive.is_none()).collect();
    let keys: Vec<(String, String)> = usable.iter().map(|&i| (v[i].candidate.day.clone(), v[i].candidate.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())).collect();
    let chosen: Vec<usize> = ll::select(&keys, ll::MAX_PHOTOS).into_iter().map(|k| usable[k]).collect();
    let count = |f: &dyn Fn(&Scanned) -> &str| {
        let mut d: Vec<&str> = usable.iter().map(|&i| f(&v[i])).collect();
        d.sort_unstable();
        d.dedup();
        d.len()
    };
    StyleRow {
        style: v[0].style.clone(),
        camera_name: v[0].camera_name.clone(),
        usable: usable.len(),
        adaptive: v.iter().filter(|s| s.adaptive.is_some()).count(),
        days: count(&|s| s.candidate.day.as_str()),
        folders: count(&|s| s.candidate.folder.as_str()),
        with_jpeg: usable.iter().filter(|&&i| v[i].has_jpeg).count(),
        chosen,
        key,
        scanned: v,
    }
}

fn camera_rows(cands: &[Candidate], scanned: Option<&[Scanned]>, include_adaptive: bool) -> Vec<CameraRow> {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for c in cands {
        *count.entry(c.camera.clone()).or_default() += 1;
    }
    let mut by_camera: BTreeMap<String, BTreeMap<String, Vec<Scanned>>> = BTreeMap::new();
    for s in scanned.unwrap_or(&[]) {
        by_camera.entry(s.candidate.camera.clone()).or_default().entry(s.key()).or_default().push(s.clone());
    }
    let mut rows: Vec<CameraRow> = count
        .into_iter()
        .map(|(library, photos)| {
            let mut styles: Vec<StyleRow> = by_camera.remove(&library).unwrap_or_default().into_iter().map(|(k, v)| style_row(k, v, include_adaptive)).collect();
            styles.sort_by(|a, b| b.scanned.len().cmp(&a.scanned.len()).then(a.key.cmp(&b.key)));
            let name = styles.first().map(|s| s.camera_name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| library.clone());
            CameraRow { photos, name, styles, library }
        })
        .collect();
    rows.sort_by(|a, b| b.photos.cmp(&a.photos).then(a.name.cmp(&b.name)));
    rows
}

// ---------------------------------------------------------------------------------------------
// status, estimate

/// Looks a camera and style has.
struct Status {
    /// learned look: switched on, metadata
    learned: Option<(bool, Option<serde_json::Value>)>,
    /// look of the base style (`camera/style`): bundled, or the user's own look in the data folder (wins over a bundled
    /// one of the same name, `rawdev::looks`)
    bundled: Option<String>,
    /// [`Self::bundled`] is the user's own look, not a bundled one
    own: bool,
    /// built-in hue table of the standard development
    hue_table: bool,
}

fn status(row: &StyleRow) -> Status {
    let learned = learn::entries().iter().find(|(e, _)| e.key == row.key).map(|(e, m)| (e.enabled, m.clone()));
    let (camera, style) = row.key.split_once('/').unwrap_or((&row.key, ""));
    let base = style.split('_').next().unwrap_or(style);
    let name = format!("{camera}/{base}");
    let source = rawdev::looks::source(&name);
    let own = source == Some(rawdev::looks::Source::Own);
    let bundled = source.map(|_| name);
    let hue_table = raw::development::camera_look(&row.camera_name, &row.camera_name).is_some();
    Status { learned, bundled, own, hue_table }
}

/// Can a look be learned from these photos?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Learnable {
    Yes,
    Limited,
    No,
}

/// Honest estimate in one sentence: from the number of usable photos, shooting days, folders and (if known) the
/// colour coverage. Promises nothing.
fn assess(row: &StyleRow, include_adaptive: bool, cov: Option<&Coverage>) -> (Learnable, String) {
    let n = row.chosen.len();
    let left_out = if !include_adaptive && row.adaptive > 0 { crate::trf!("; {a} Fotos mit adaptiver Kamera-Einstellung ausgelassen", a = row.adaptive) } else { String::new() };
    if n < ll::MIN_PHOTOS {
        return (Learnable::No, crate::trf!("Nur {n} nutzbare Fotos, mindestens {m} sind nötig{rest}.", n = n, m = ll::MIN_PHOTOS, rest = left_out));
    }
    if row.days < 2 {
        return (Learnable::No, crate::tr("Alle Fotos stammen von einem Aufnahmetag: eine Kontrolle an anderen Tagen ist nicht möglich.").to_string());
    }
    let mut limits: Vec<String> = Vec::new();
    // at most 4 photos per day: below 8 days the check rests on very few days
    if row.days < 8 {
        limits.push(crate::trf!("nur {d} Aufnahmetage", d = row.days));
    }
    if row.folders == 1 {
        limits.push(crate::tr("alle aus einem Ordner").into());
    }
    if n < 30 {
        limits.push(crate::tr("knapp genug Fotos").into());
    }
    if cov.is_some_and(|c| c.photos > 0 && c.saturated < 0.01) {
        limits.push(crate::tr("kaum kräftige Farben").into());
    }
    let head = crate::trf!("{n} Fotos an {d} Tagen in {f} Ordnern", n = n, d = row.days, f = row.folders);
    if limits.is_empty() {
        let skin = if cov.is_some_and(|c| c.photos > 0 && c.skin < 0.01) { crate::tr(", kaum Hauttöne").to_string() } else { String::new() };
        (Learnable::Yes, format!("{head}{skin}{left_out}."))
    } else {
        (Learnable::Limited, format!("{head}, {}{left_out}.", limits.join(", ")))
    }
}

fn learnable_text(l: Learnable) -> &'static str {
    match l {
        Learnable::Yes => crate::tr("lernbar: ja"),
        Learnable::Limited => crate::tr("lernbar: eingeschränkt"),
        Learnable::No => crate::tr("lernbar: nein"),
    }
}

fn learnable_colour(l: Learnable) -> Color32 {
    match l {
        Learnable::Yes => GREEN,
        Learnable::Limited => YELLOW,
        Learnable::No => RED,
    }
}

fn quality_text(delta_e: f32) -> String {
    let class = match ll::quality_class(delta_e) {
        0 => crate::tr("sehr nah"),
        1 => crate::tr("nah"),
        2 => crate::tr("mäßig"),
        _ => crate::tr("grob"),
    };
    crate::trf!("ΔE00 {d} ({c})", d = fmt2(delta_e), c = class)
}

/// One line: where a look comes from and how good it measured.
fn provenance_text(meta: &str) -> String {
    let p = ll::provenance(meta);
    let origin = match p.origin {
        Origin::Checked => crate::tr("an Kontrollfotos geprüft"),
        Origin::Makernote => crate::tr("aus der Kamera-Makernote, ohne Messung"),
        Origin::Samples => crate::tr("Basis aus Fremdbeispielen, nicht geprüft"),
        Origin::Base => crate::tr("Basis ohne Messung"),
    };
    let mut parts = vec![origin.to_string()];
    if let Some(s) = &p.source {
        parts.push(crate::trf!("Quelle: {a} Dateien", a = s));
    }
    if let Some(d) = p.delta_e {
        parts.push(quality_text(d));
    }
    if let Some(n) = p.photos {
        parts.push(crate::trf!("{n} Fotos", n = n));
    }
    if let Some(from) = &p.derived_from {
        parts.push(crate::trf!("Startwert von {a}", a = from));
    }
    if p.imported {
        parts.push(crate::tr("geladen").into());
    }
    parts.join(" · ")
}

fn fmt2(v: f32) -> String {
    if v.is_finite() {
        crate::menu::i18n::format::decimal(crate::menu::i18n::language(), v as f64, 2)
    } else {
        "–".into()
    }
}

fn date_text(iso: &str) -> String {
    let p: Vec<&str> = iso.split('-').collect();
    match p[..] {
        [y, m, d] => match (y.parse(), m.parse(), d.parse()) {
            (Ok(y), Ok(m), Ok(d)) => crate::menu::i18n::format::date(crate::menu::i18n::language(), y, m, d).unwrap_or_else(|| iso.to_string()),
            _ => iso.to_string(),
        },
        _ => iso.to_string(),
    }
}

fn today() -> String {
    let t = library::platform::local_time(library::now());
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.tag)
}

/// New photos of the camera since the look was learned (library photos then and now), if clearly more.
fn new_since(meta: Option<&serde_json::Value>, photos_now: usize) -> Option<usize> {
    let then = meta?.get("library_photos")?.as_u64()? as usize;
    let more = photos_now.saturating_sub(then);
    (more >= 20 && more * 4 >= then).then_some(more)
}

/// Looks of related cameras with the same picture style as a starting point (learned ones first, then bundled ones),
/// best match first: (key, already learned).
fn related(key: &str) -> Vec<(String, bool)> {
    let (camera, style) = key.split_once('/').unwrap_or((key, ""));
    let base_style = style.split('_').next().unwrap_or(style);
    let common = |o: &str| camera.chars().zip(o.chars()).take_while(|(a, b)| a == b).count();
    let mut v: Vec<(String, bool, usize)> = Vec::new();
    for (e, _) in learn::entries().iter() {
        if let Some((c, s)) = e.key.split_once('/') {
            if c != camera && s == style && common(c) >= 2 {
                v.push((e.key.clone(), true, common(c)));
            }
        }
    }
    for n in rawdev::looks::names() {
        if let Some((c, s)) = n.split_once('/') {
            if c != camera && s == base_style && common(c) >= 2 && !v.iter().any(|x| x.0 == n) {
                v.push((n.clone(), false, common(c)));
            }
        }
    }
    v.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
    v.into_iter().map(|(k, l, _)| (k, l)).collect()
}

/// Take over the look of a related camera as the starting point of `key` (clearly marked: basis without measurement).
fn adopt(key: &str, from: &str, learned: bool) -> Result<(), String> {
    let look = if learned { raw::looks::Look::file(&learn::look_path(from)) } else { rawdev::looks::find(from).map(|l| (*l).clone()) }.ok_or("look not found")?;
    let (camera, style) = key.split_once('/').unwrap_or((key, ""));
    let meta = serde_json::json!({"learned": false, "origin": ll::ORIGIN_BASE, "derived_from": from, "camera": camera, "style": style, "version": ll::FORMAT_VERSION, "created": today(), "interpolation": if look.tetrahedral() { "tetrahedral" } else { "trilinear" }});
    let (n, d) = look.table();
    let copy = raw::looks::Look::from_table(key, &meta.to_string(), n, d.to_vec()).ok_or("table")?;
    learn::install(key, &copy)
}

// ---------------------------------------------------------------------------------------------
// comparison

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Neutral,
    Current,
    New,
    Camera,
}

/// Mark of a photo in the filmstrip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Training,
    Check,
    Unknown,
}

struct Shown {
    path: PathBuf,
    size: Vec2,
    neutral_img: image::RgbImage,
    neutral: egui::TextureHandle,
    current: egui::TextureHandle,
    camera: egui::TextureHandle,
    /// texture of the new look and what it was made with (key, look file or result)
    new: Option<(String, egui::TextureHandle)>,
    exposure: f32,
    source: ll::ReferenceSource,
}

struct Compare {
    photo: Option<PathBuf>,
    job: Option<(PathBuf, Job<learn::Views>)>,
    shown: Option<Shown>,
    left: Mode,
    right: Mode,
    side_by_side: bool,
    wipe: f32,
    dragging_divider: bool,
    zoom: f32,
    center: Vec2,
    trackpad: bool,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

impl Default for Compare {
    fn default() -> Self {
        Compare {
            photo: None,
            job: None,
            shown: None,
            left: Mode::Current,
            right: Mode::Camera,
            side_by_side: false,
            wipe: 0.5,
            dragging_divider: false,
            zoom: 1.0,
            center: Vec2::splat(0.5),
            trackpad: true,
            thumbs: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// window state

struct Window {
    open: bool,
    cands: Vec<Candidate>,
    /// picture styles read so far (`style_scan`: ahead in idle time, in parallel, camera by camera)
    snap: crate::style_scan::Snapshot,
    /// when the list was last built from the readings (built again at most every few frames while reading)
    rows_at: Instant,
    cameras: Vec<CameraRow>,
    selected: Option<String>,
    filter: String,
    only_learnable: bool,
    only_without: bool,
    include_adaptive: bool,
    stats: HashMap<String, Coverage>,
    stats_job: Option<(String, Job<Coverage>)>,
    learning: Option<Learning>,
    outcome: Option<Outcome>,
    message: Option<(bool, String)>,
    compare: Compare,
    /// version file to restore (asked once more) and when
    restore_armed: Option<(PathBuf, f64)>,
    saved_size: Option<Vec2>,
    /// window size in the last frame, and whether the user is resizing it
    inner: Option<Vec2>,
    resized: bool,
    /// pending action from the panels (done after drawing)
    action: Option<Action>,
}

enum Action {
    Learn,
    Adopt,
    Restore(PathBuf),
    StartFrom(String, bool),
    Export,
    Import,
}

static WINDOW: Mutex<Option<Window>> = Mutex::new(None);

/// The look files of one key as last read: its versions, the look in use and that look's check days. Read again only
/// when the learned looks change ([`learn::generation`]), not in every frame (each version is a whole table file, and
/// `replaced/` keeps growing).
struct LookFiles {
    key: String,
    generation: u64,
    versions: Vec<learn::Version>,
    current: Option<Arc<raw::looks::Look>>,
    days: Option<Vec<String>>,
}

static LOOK_FILES: Mutex<Option<Arc<LookFiles>>> = Mutex::new(None);

fn look_files(key: &str) -> Arc<LookFiles> {
    let generation = learn::generation();
    let mut g = LOOK_FILES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = g.as_ref().filter(|f| f.key == key && f.generation == generation) {
        return f.clone();
    }
    let current = raw::looks::Look::file(&learn::look_path(key)).map(Arc::new);
    let days = current.as_deref().and_then(learn::check_days_of);
    let f = Arc::new(LookFiles { key: key.to_string(), generation, versions: learn::versions(key), current, days });
    *g = Some(f.clone());
    f
}

/// Is the window on screen? (Its keys then belong to it.)
pub fn is_open() -> bool {
    WINDOW.lock().unwrap().as_ref().is_some_and(|w| w.open)
}

/// Open the window with the library's RAW photos; `key` preselects a camera and style (`camera/style`).
pub fn open(cands: Vec<Candidate>, key: Option<String>) {
    let mut g = WINDOW.lock().unwrap();
    // read what is missing (most of it is usually known from idle time)
    crate::style_scan::open(&cands);
    let snap = crate::style_scan::snapshot();
    if let Some(w) = g.as_mut() {
        // a learning job kept the window's state: show it again
        w.open = true;
        w.cands = cands;
        w.snap = snap;
        w.rows_at = Instant::now();
        w.cameras = camera_rows(&w.cands, Some(&w.snap.scanned), w.include_adaptive);
        if key.is_some() {
            w.selected = key;
        }
        return;
    }
    let mut w = Window {
        open: true,
        cands,
        snap,
        rows_at: Instant::now(),
        cameras: Vec::new(),
        selected: key,
        filter: String::new(),
        only_learnable: false,
        only_without: false,
        include_adaptive: false,
        stats: HashMap::new(),
        stats_job: None,
        learning: None,
        outcome: None,
        message: None,
        compare: Compare::default(),
        restore_armed: None,
        saved_size: library::settings::value(SIZE_KEY).as_array().and_then(|a| Some(Vec2::new(a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32))),
        action: None,
        inner: None,
        resized: false,
    };
    w.cameras = camera_rows(&w.cands, Some(&w.snap.scanned), false);
    *g = Some(w);
}

impl Window {
    fn row(&self, key: &str) -> Option<(&CameraRow, &StyleRow)> {
        self.cameras.iter().find_map(|c| c.styles.iter().find(|s| s.key == key).map(|s| (c, s)))
    }

    fn select(&mut self, key: String) {
        if self.selected.as_deref() != Some(key.as_str()) {
            // the photos of this camera are read first if some are still missing
            if let Some(cam) = self.library_camera(&key) {
                if !self.snap.camera_done(&cam) {
                    crate::style_scan::prefer(&cam);
                }
            }
            self.selected = Some(key);
            self.compare.photo = None;
            self.restore_armed = None;
        }
    }

    /// Camera of a style key as the library names it (the reading progress is kept per library camera).
    fn library_camera(&self, key: &str) -> Option<String> {
        self.row(key).and_then(|(_, r)| r.scanned.first()).map(|s| s.candidate.camera.clone())
    }

    /// Photos of the camera's sample read and in total while some are still missing.
    fn reading(&self, key: &str) -> Option<(usize, usize)> {
        let cam = self.library_camera(key)?;
        self.snap.cameras.get(&cam).copied().filter(|(d, n)| d < n)
    }

    /// Photos of the filmstrip for `key`: the photos of the last learning (marked by its check days), else the photos
    /// learning would use (marked by the check days of the learned look in use, if any).
    fn strip(&self, key: &str) -> Vec<(PathBuf, Option<PathBuf>, Mark)> {
        let Some((_, row)) = self.row(key) else { return Vec::new() };
        let thumb = |p: &PathBuf| row.scanned.iter().find(|s| &s.candidate.path == p).and_then(|s| s.candidate.thumb.clone());
        if let Some(o) = self.outcome.as_ref().filter(|o| o.key == key) {
            return o.learned.paths.iter().enumerate().map(|(i, p)| (p.clone(), thumb(p), if o.learned.is_check(i) { Mark::Check } else { Mark::Training })).collect();
        }
        let files = look_files(key);
        let days = files.days.as_ref();
        // nothing to learn from (e.g. all photos adaptive): still show the photos, to compare them
        let all: Vec<usize> = (0..row.scanned.len().min(ll::MAX_PHOTOS)).collect();
        let shown = if row.chosen.is_empty() { &all } else { &row.chosen };
        shown
            .iter()
            .map(|&i| {
                let c = &row.scanned[i].candidate;
                let mark = match &days {
                    Some(d) if d.contains(&c.day) => Mark::Check,
                    Some(_) => Mark::Training,
                    None => Mark::Unknown,
                };
                (c.path.clone(), c.thumb.clone(), mark)
            })
            .collect()
    }

    /// Id of the look "Neu" shows for `key` (changes when that look changes); see [`Self::new_look`].
    fn new_look_id(&self, key: &str) -> Option<String> {
        if let Some(o) = self.outcome.as_ref().filter(|o| o.key == key && o.learned.outcome.look.is_some()) {
            return Some(format!("result:{}", o.learned.outcome.check_photos));
        }
        let f = look_files(key);
        f.current.as_ref().map(|_| format!("file:{}:{}", learn::look_path(key).display(), f.generation))
    }

    /// The look "Neu" shows for `key`: the result of the last learning, else the learned look file.
    fn new_look(&self, key: &str) -> Option<Arc<raw::looks::Look>> {
        if let Some(o) = self.outcome.as_ref().filter(|o| o.key == key) {
            if let Some(l) = &o.learned.outcome.look {
                return Some(Arc::new(l.clone()));
            }
        }
        look_files(key).current.clone()
    }
}

// ---------------------------------------------------------------------------------------------
// frame

/// Draw the window and collect finished jobs (every frame).
pub fn show(ctx: &egui::Context) {
    let mut guard = WINDOW.lock().unwrap();
    let Some(w) = guard.as_mut() else { return };
    collect(ctx, w);
    let busy = !w.snap.complete() || w.learning.is_some() || w.stats_job.is_some() || w.compare.job.is_some();
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
    if !w.open {
        // closed: only a running learning job keeps the state (its result is shown on the next opening)
        if w.learning.is_none() {
            *guard = None;
        }
        return;
    }
    // test aid: the comparison mode for screenshots
    test_view(w);
    let content = ctx.content_rect();
    let max = (content.size() - Vec2::splat(24.0)).max(Vec2::new(MIN_SIZE[0], MIN_SIZE[1]));
    let size = w.saved_size.unwrap_or(Vec2::from(DEFAULT_SIZE)).min(max).max(Vec2::from(MIN_SIZE));
    // the rest of the app lies dimmed below and takes no clicks while the window is open
    egui::Area::new(egui::Id::unique("camera-looks-dim")).order(egui::Order::Middle).fixed_pos(content.min).show(ctx, |ui| {
        ui.painter().rect_filled(content, 0.0, Color32::from_black_alpha(150));
        let _ = ui.allocate_rect(content, egui::Sense::click_and_drag());
    });
    let mut open = true;
    let r = egui::Window::new(egui::RichText::new(crate::tr("Kamera-Looks")).font(lr::semibold(15.0)))
        .id(egui::Id::unique("camera-looks"))
        .order(egui::Order::Foreground)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(size)
        .min_size(Vec2::from(MIN_SIZE))
        .default_pos(content.center() - size / 2.0)
        .constrain_to(content)
        .frame(egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(12, 10)).corner_radius(0.0))
        .show(ctx, |ui| body(ui, w));
    // remember the size once the user let go of the corner (only a resize by the user is written)
    if let Some(r) = &r {
        let s = r.response.rect.size().round();
        let pressed = ctx.input(|i| i.pointer.any_down());
        if pressed && w.inner.is_some_and(|o| (o - s).length() > 0.5) {
            w.resized = true;
        }
        if !pressed && w.resized {
            w.resized = false;
            w.saved_size = Some(s);
            library::settings::set(SIZE_KEY, serde_json::json!([s.x, s.y]));
        }
        w.inner = Some(s);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !ctx.egui_wants_keyboard_input() {
        open = false;
    }
    let action = w.action.take();
    if let Some(a) = action {
        run_action(ctx, w, a);
    }
    file_results(w);
    if !open {
        w.open = false;
        w.compare.thumbs.clear();
        w.compare.shown = None;
        if let Some((_, j)) = &w.stats_job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        if let Some((_, j)) = &w.compare.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        // what is left is read in idle time
        crate::style_scan::close();
        if w.learning.is_none() {
            *guard = None;
        }
    }
}

fn collect(ctx: &egui::Context, w: &mut Window) {
    // new readings: the list again (while reading at most about three times a second; the rows are rebuilt whole)
    if crate::style_scan::generation() != w.snap.generation && (w.rows_at.elapsed().as_millis() >= 300 || w.snap.done + 1 >= w.snap.total) {
        w.snap = crate::style_scan::snapshot();
        w.rows_at = Instant::now();
        w.cameras = camera_rows(&w.cands, Some(&w.snap.scanned), w.include_adaptive);
        // the statistics of a style whose photos grew are measured again
        let growing: Vec<String> = w.stats.keys().filter(|k| w.reading(k).is_some()).cloned().collect();
        for k in growing {
            w.stats.remove(&k);
        }
    }
    // nothing chosen (or an unknown style): the first style of the camera with the most photos
    if w.selected.as_ref().is_none_or(|k| w.row(k).is_none()) {
        if let Some(k) = w.cameras.iter().flat_map(|c| c.styles.iter()).next().map(|s| s.key.clone()) {
            w.select(k);
        }
    }
    if let Some((key, j)) = &w.stats_job {
        if let Some(r) = j.take() {
            if let Some(c) = r {
                w.stats.insert(key.clone(), c);
            }
            w.stats_job = None;
        }
    }
    if let Some(l) = &mut w.learning {
        {
            let mut live = l.live.lock().unwrap();
            if live.preview_new {
                live.preview_new = false;
                if let Some(img) = &live.preview {
                    l.preview = Some(texture(ctx, "camera-looks-live", img));
                }
            }
        }
        if let Some(r) = l.job.take() {
            let (key, renewing) = (l.key.clone(), l.renewing);
            w.learning = None;
            match r {
                Some(learned) => {
                    w.outcome = Some(Outcome { key: key.clone(), learned, renewing, adopted: false });
                    if w.selected.as_deref() == Some(key.as_str()) {
                        // the filmstrip now shows training and check photos
                        w.compare.photo = None;
                    }
                }
                None => w.message = Some((false, crate::tr("Lernen abgebrochen").into())),
            }
        }
    }
    if let Some((path, j)) = &w.compare.job {
        if let Some(r) = j.take() {
            let path = path.clone();
            w.compare.job = None;
            match r {
                Some(v) => {
                    let size = Vec2::new(v.neutral.width() as f32, v.neutral.height() as f32);
                    w.compare.shown = Some(Shown {
                        path,
                        size,
                        neutral: texture(ctx, "camera-looks-neutral", &v.neutral),
                        current: texture(ctx, "camera-looks-current", &v.current),
                        camera: texture(ctx, "camera-looks-camera", &v.camera),
                        neutral_img: v.neutral,
                        new: None,
                        exposure: v.exposure_offset,
                        source: v.source,
                    });
                }
                None => {}
            }
        }
    }
}

fn texture(ctx: &egui::Context, name: &str, img: &image::RgbImage) -> egui::TextureHandle {
    let c = egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], img.as_raw());
    ctx.load_texture(name, c, egui::TextureOptions::LINEAR)
}

fn body(ui: &mut egui::Ui, w: &mut Window) {
    let full = ui.available_rect_before_wrap();
    let footer_h = 18.0;
    let area = Rect::from_min_max(full.min, Pos2::new(full.max.x, full.max.y - footer_h - 4.0));
    let gap = 10.0;
    let list = Rect::from_min_size(area.min, Vec2::new(LIST_W, area.height()));
    let side = Rect::from_min_max(Pos2::new(area.max.x - SIDE_W, area.min.y), area.max);
    let middle = Rect::from_min_max(Pos2::new(list.max.x + gap, area.min.y), Pos2::new(side.min.x - gap, area.max.y));
    let p = ui.painter();
    for x in [list.max.x + gap / 2.0, side.min.x - gap / 2.0] {
        p.line_segment([Pos2::new(x, area.min.y), Pos2::new(x, area.max.y)], egui::Stroke::new(1.0, crate::lr::gray(0x3a)));
    }
    let mut child = |r: Rect, salt: &str| ui.new_child(egui::UiBuilder::new().id_salt(salt).max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
    list_ui(&mut child(list, "list"), w);
    compare_ui(&mut child(middle, "compare"), w);
    side_ui(&mut child(side, "side"), w);
    // one fixed line for the result of the last action
    let footer = Rect::from_min_max(Pos2::new(full.min.x, full.max.y - footer_h), full.max);
    let (ok, text) = w.message.clone().unwrap_or((true, String::new()));
    let mut f = child(footer, "footer");
    f.add(egui::Label::new(egui::RichText::new(text).small().color(if ok { lr::TEXT_DIM() } else { RED })).truncate());
    ui.allocate_rect(full, egui::Sense::hover());
}

// ---------------------------------------------------------------------------------------------
// list

fn list_ui(ui: &mut egui::Ui, w: &mut Window) {
    ui.spacing_mut().item_spacing.y = 4.0;
    lr::edit_field(ui, egui::TextEdit::singleline(&mut w.filter).hint_text(crate::tr("Kamera oder Bildstil suchen …")).desired_width(f32::INFINITY));
    ui.horizontal(|ui| {
        let a = ui.checkbox(&mut w.only_learnable, crate::tr("nur lernbare"));
        lr::hover_tip(ui, &a, crate::tr("Nur Bildstile zeigen, aus deren Fotos sich ein Look lernen lässt"));
        let b = ui.checkbox(&mut w.only_without, crate::tr("nur ohne Look"));
        lr::hover_tip(ui, &b, crate::tr("Nur Bildstile ohne mitgelieferten und ohne gelernten Look"));
    });
    if !w.snap.complete() {
        let (d, n) = (w.snap.done, w.snap.total);
        let mut text = crate::trf!("Bildstile lesen: {d} von {n} Fotos", d = dialog::number(d), n = dialog::number(n));
        if let Some(e) = w.snap.eta {
            text = format!("{text} · {}", crate::style_scan::eta_text(e));
        }
        let r = ui.scope(|ui| dialog::progress(ui, Some(d as f32 / n.max(1) as f32), &text)).response;
        lr::hover_tip(
            ui,
            &r,
            &crate::trf!(
                "Gelesen wird nur der Dateikopf, mehrere Fotos gleichzeitig, je Kamera höchstens {n} über die Aufnahmetage verteilt; die Liste füllt sich Kamera für Kamera. Einmal gelesen, bleibt das Ergebnis gespeichert (neue Fotos werden im Leerlauf vorab gelesen).",
                n = learn::SAMPLE_PER_CAMERA
            ),
        );
    }
    ui.add_space(2.0);
    if w.cameras.is_empty() {
        dialog::hint(ui, crate::tr("Keine RAW-Fotos in der Bibliothek (Sigma/Foveon bleibt außen vor)."));
        return;
    }
    let filter = w.filter.to_lowercase();
    let scanning = !w.snap.complete();
    let mut pick: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("camera-looks-list").auto_shrink([false; 2]).show(ui, |ui| {
        for cam in &w.cameras {
            let styles: Vec<&StyleRow> = cam
                .styles
                .iter()
                .filter(|s| filter.is_empty() || cam.name.to_lowercase().contains(&filter) || style_label(&s.style).to_lowercase().contains(&filter))
                .filter(|s| !w.only_learnable || assess(s, w.include_adaptive, None).0 != Learnable::No)
                .filter(|s| {
                    if !w.only_without {
                        return true;
                    }
                    let st = status(s);
                    st.learned.is_none() && st.bundled.is_none()
                })
                .collect();
            let camera_matches = filter.is_empty() || cam.name.to_lowercase().contains(&filter);
            if styles.is_empty() && (!camera_matches || w.only_learnable || w.only_without || !cam.styles.is_empty()) {
                continue;
            }
            // camera head: name, photos in the library, new photos since learning
            let newest = cam.styles.iter().filter_map(|s| status(s).learned.and_then(|(_, m)| new_since(m.as_ref(), cam.photos))).max();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(cam.name.to_uppercase()).font(lr::semibold(11.5)).color(lr::CATEGORY_LABEL()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let n = ui.label(egui::RichText::new(dialog::number(cam.photos)).small().color(lr::TEXT_DIM()));
                    lr::hover_tip(ui, &n, crate::tr("RAW-Fotos dieser Kamera in der Bibliothek"));
                });
            });
            // this camera's photos are still being read: the numbers below grow
            let reading = w.snap.cameras.get(&cam.library).copied().filter(|(d, n)| d < n);
            if let Some((d, n)) = reading {
                let r = ui.label(egui::RichText::new(crate::trf!("Bildstile: {d} von {n} gelesen", d = d, n = n)).small().color(lr::TEXT_DIM()));
                lr::hover_tip(ui, &r, crate::tr("Die Zahlen dieser Kamera sind vorläufig, bis ihre Auswahl ganz gelesen ist; Lernen geht danach"));
            }
            if let Some(more) = newest {
                let r = ui.label(egui::RichText::new(crate::trf!("{n} neue Fotos seit dem Lernen", n = more)).small().color(lr::BLUE_TEXT()));
                lr::hover_tip(ui, &r, crate::tr("Mit den neuen Fotos lässt sich der Look neu lernen; er wird nur ersetzt, wenn er an denselben Kontrollfotos besser ist"));
            }
            if cam.styles.is_empty() {
                // while this camera is read the line above says so
                if reading.is_none() {
                    ui.label(egui::RichText::new(if scanning { crate::tr("Bildstile werden gelesen …") } else { crate::tr("kein lesbarer Bildstil") }).small().color(lr::TEXT_DIM()));
                }
                continue;
            }
            for s in styles {
                let selected = w.selected.as_deref() == Some(s.key.as_str());
                if style_line(ui, s, selected, w.include_adaptive) {
                    pick = Some(s.key.clone());
                }
            }
        }
    });
    if let Some(k) = pick {
        w.select(k);
    }
}

/// One picture style in the list: name, learnable dot, status tags. Returns clicked.
fn style_line(ui: &mut egui::Ui, s: &StyleRow, selected: bool, include_adaptive: bool) -> bool {
    let st = status(s);
    let (learnable, why) = assess(s, include_adaptive, None);
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 38.0), egui::Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 0.0, lr::ACTIVE_FILL());
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, crate::lr::gray(0x26));
    }
    p.circle_filled(Pos2::new(rect.min.x + 8.0, rect.min.y + 11.0), 3.5, learnable_colour(learnable));
    let name = style_label(&s.style);
    let font = egui::FontId::proportional(lr::UI_SIZE);
    // one line only: a long name is cut with "…" so it never runs into the tags below (full name in the tooltip)
    let mut job = egui::text::LayoutJob::simple_singleline(name.clone(), font, lr::TEXT());
    job.wrap = egui::text::TextWrapping::truncate_at_width(width - 70.0);
    let g = p.layout_job(job);
    p.galley(Pos2::new(rect.min.x + 18.0, rect.min.y + 3.0), g, lr::TEXT());
    p.text(Pos2::new(rect.max.x - 4.0, rect.min.y + 4.0), egui::Align2::RIGHT_TOP, dialog::number(s.scanned.len()), egui::FontId::proportional(10.5), lr::TEXT_DIM());
    // status tags
    let mut tags: Vec<(String, Color32)> = Vec::new();
    if let Some((on, meta)) = &st.learned {
        let date = meta.as_ref().and_then(|m| m["created"].as_str()).map(date_text).unwrap_or_default();
        let photos = meta.as_ref().and_then(|m| m["photos"].as_u64());
        let mut t = crate::tr("GELERNT").to_string();
        if !date.is_empty() {
            t.push_str(&format!(" {date}"));
        }
        if let Some(n) = photos {
            t.push_str(&format!(" · {n}"));
        }
        if !on {
            t.push_str(&format!(" · {}", crate::tr("AUS")));
        }
        tags.push((t, GREEN));
    }
    if st.bundled.is_some() {
        tags.push((if st.own { crate::tr("EIGENER LOOK") } else { crate::tr("MITGELIEFERT") }.into(), lr::BLUE_TEXT()));
    }
    if st.hue_table {
        tags.push((crate::tr("FARBTONTABELLE").into(), lr::TEXT_DIM()));
    }
    if tags.is_empty() {
        tags.push((crate::tr("KEINER").into(), crate::lr::gray(0x80)));
    }
    let mut x = rect.min.x + 18.0;
    for (t, c) in tags {
        let g = p.layout_no_wrap(t, egui::FontId::monospace(9.5), c);
        let r = Rect::from_min_size(Pos2::new(x, rect.min.y + 21.0), g.size() + Vec2::new(8.0, 3.0));
        if r.max.x > rect.max.x {
            break;
        }
        p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, c.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        p.galley(r.min + Vec2::new(4.0, 1.5), g, c);
        x = r.max.x + 4.0;
    }
    // tooltip: estimate and where the looks come from
    let mut tip = vec![name.to_string(), format!("{} – {why}", learnable_text(learnable))];
    if let Some((_, Some(m))) = &st.learned {
        tip.push(crate::trf!("Gelernt: {a}", a = provenance_text(&m.to_string())));
    }
    if let Some(b) = st.bundled.as_ref().and_then(|b| rawdev::looks::find(b)) {
        tip.push(if st.own { crate::trf!("Eigener Look aus dem Datenordner: {a}", a = provenance_text(&b.meta)) } else { crate::trf!("Mitgeliefert: {a}", a = provenance_text(&b.meta)) });
    }
    if st.hue_table {
        tip.push(crate::tr("Eingebaute Farbtontabelle der Standardentwicklung (an Kontrollfotos gemessen)").into());
    }
    lr::hover_tip(ui, &resp, &tip.join("\n"));
    resp.clicked()
}

// ---------------------------------------------------------------------------------------------
// side panel: estimate, statistics, learning, versions, sharing

fn side_ui(ui: &mut egui::Ui, w: &mut Window) {
    ui.spacing_mut().item_spacing.y = 5.0;
    let Some(key) = w.selected.clone().filter(|k| w.row(k).is_some()) else {
        dialog::hint(ui, crate::tr("Links eine Kamera und einen Bildstil wählen."));
        main_button_row(ui, w, None, None);
        return;
    };
    // statistics of the colours: from the learning blocks if there are, else from the small previews (background)
    let from_blocks = w.outcome.as_ref().filter(|o| o.key == key).map(|o| Coverage::of_photos(&o.learned.photos));
    if from_blocks.is_none() && !w.stats.contains_key(&key) && w.stats_job.as_ref().is_none_or(|(k, _)| *k != key) {
        if let Some((_, j)) = &w.stats_job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        let thumbs: Vec<PathBuf> = w
            .row(&key)
            .map(|(_, r)| {
                // the photos learning would use; with none, all photos of the style (statistics stay informative)
                let all: Vec<usize> = (0..r.scanned.len().min(ll::MAX_PHOTOS)).collect();
                let idx = if r.chosen.is_empty() { &all } else { &r.chosen };
                idx.iter().filter_map(|&i| r.scanned[i].candidate.thumb.clone()).collect()
            })
            .unwrap_or_default();
        w.stats_job = Some((
            key.clone(),
            Job::start("camera-looks-stats", 1, move |keep_going, progress| {
                let mut c = Coverage::default();
                for (n, t) in thumbs.iter().enumerate() {
                    if !keep_going() {
                        return None;
                    }
                    progress(n, thumbs.len());
                    if let Ok(img) = image::open(t) {
                        c.add_image(&img.to_rgb8());
                    }
                }
                Some(c)
            }),
        ));
    }
    let cov = from_blocks.clone().or_else(|| w.stats.get(&key).cloned());
    let Some((cam, row)) = w.row(&key) else { return };
    let (cam_name, cam_photos) = (cam.name.clone(), cam.photos);
    let row = row.clone();
    let st = status(&row);
    let (learnable, why) = assess(&row, w.include_adaptive, cov.as_ref());
    // head
    ui.label(egui::RichText::new(format!("{} · {}", cam_name, style_label(&row.style))).font(lr::semibold(13.0)).color(crate::lr::gray(0xee)));
    ui.horizontal_wrapped(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 14.0), egui::Sense::hover());
        ui.painter().circle_filled(r.center(), 4.0, learnable_colour(learnable));
        ui.label(egui::RichText::new(learnable_text(learnable)).strong().color(lr::TEXT()));
    });
    ui.add(egui::Label::new(egui::RichText::new(&why).color(lr::TEXT())).wrap());
    let expect = match learnable {
        Learnable::Yes => crate::tr("Erfahrungswert: an Kontrollfotos meist nah am Kamera-JPEG (ΔE00 etwa 0,7–1,7). Zugesagt ist nichts: angeboten wird nur, was messbar besser ist."),
        Learnable::Limited => crate::tr("Ergebnis unsicher: angeboten wird nur, was an Kontrollfotos messbar besser ist."),
        Learnable::No => "",
    };
    if !expect.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(expect).small().color(lr::TEXT_DIM())).wrap());
    }
    let reading = w.reading(&key);
    main_button_row(ui, w, Some((&key, learnable, st.learned.is_some())), reading);
    ui.add_space(2.0);
    let small_height = ui.available_height() < 420.0;
    egui::ScrollArea::vertical().id_salt("camera-looks-side").auto_shrink([false; 2]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 4.0);
        // the job or its result
        if w.learning.is_some() {
            live_ui(ui, w);
        } else if w.outcome.as_ref().is_some_and(|o| o.key == key) {
            result_ui(ui, w);
        }
        // looks of this camera and style
        lr::category(ui, crate::tr("Looks"));
        looks_ui(ui, w, &key, &row, &st, cam_photos);
        // statistics (folded up when the window is low)
        egui::CollapsingHeader::new(egui::RichText::new(crate::tr("STATISTIK")).font(lr::semibold(11.0)).color(lr::CATEGORY_LABEL()))
            .id_salt(("camera-looks-stats", small_height))
            .default_open(!small_height)
            .show(ui, |ui| stats_ui(ui, w, &row, cam_photos, cov.as_ref(), from_blocks.is_some()));
        // versions and sharing
        versions_ui(ui, w, &key);
        lr::category(ui, crate::tr("Teilen"));
        ui.horizontal(|ui| {
            let has = look_files(&key).versions.first().is_some_and(|v| v.current);
            let e = ui.add_enabled(has, egui::Button::new(crate::tr("Exportieren …")));
            lr::hover_tip(ui, &e, crate::tr("Gelernten Look als .x3look (für graineo) oder .cube (auch für andere Programme) speichern: nur Farbwerte mit Herkunft und Güte, keine Bilder, keine Aufnahmedaten"));
            if e.clicked() {
                w.action = Some(Action::Export);
            }
            let i = ui.button(crate::tr("Importieren …"));
            lr::hover_tip(ui, &i, crate::tr("Geteilten Look (.x3look oder .cube) für diese Kamera und diesen Bildstil laden; ein vorhandener gelernter Look wird aufbewahrt"));
            if i.clicked() {
                w.action = Some(Action::Import);
            }
        });
    });
}

/// The one main button: "Lernen" / "Neu lernen", or "Übernehmen" while a better result waits.
fn main_button_row(ui: &mut egui::Ui, w: &mut Window, sel: Option<(&str, Learnable, bool)>, reading: Option<(usize, usize)>) {
    let pending = w.outcome.as_ref().is_some_and(|o| sel.is_some_and(|(k, ..)| k == o.key) && o.learned.outcome.look.is_some() && !o.adopted);
    let running = w.learning.is_some();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if pending {
            let b = lr::main_button_min(ui, "Übernehmen", true, 150.0);
            lr::hover_tip(ui, &b, crate::tr("Den neuen Look als Standard dieser Kamera und dieses Bildstils verwenden; ein bisheriger gelernter Look wird aufbewahrt"));
            if b.clicked() {
                w.action = Some(Action::Adopt);
            }
            let again = ui.add_enabled(!running, egui::Button::new(crate::tr("Erneut lernen")));
            lr::hover_tip(ui, &again, crate::tr("Noch einmal lernen (verwirft das Ergebnis, nicht den Look in Gebrauch)"));
            if again.clicked() {
                w.action = Some(Action::Learn);
            }
        } else {
            let reading_tip = reading.map(|(d, n)| crate::trf!("Die Bildstile dieser Kamera werden noch gelesen ({d} von {n}); gelernt wird mit der ganzen Auswahl", d = d, n = n)).unwrap_or_default();
            let (active, renew, tip) = match sel {
                None => (false, false, crate::tr("Erst eine Kamera und einen Bildstil wählen")),
                Some(_) if running => (false, false, crate::tr("Gerade läuft ein Lernvorgang")),
                Some((_, _, has)) if reading.is_some() => (false, has, reading_tip.as_str()),
                Some((_, Learnable::No, has)) => (false, has, crate::tr("Zu wenige geeignete Fotos (siehe Einschätzung)")),
                Some((_, _, true)) => (true, true, crate::tr("Mit aktuellen Fotos neu lernen: das Ergebnis wird an denselben Kontrollfotos gegen den bisherigen Look und gegen heute gemessen und nur angeboten, wenn es besser ist")),
                Some(_) => (true, false, crate::tr("Look aus RAW und Kamera-JPEG dieser Fotos lernen (leise im Hintergrund, jederzeit abbrechbar)")),
            };
            let b = lr::main_button_min(ui, if renew { "Neu lernen" } else { "Lernen" }, active, 150.0);
            lr::hover_tip(ui, &b, tip);
            if b.clicked() {
                w.action = Some(Action::Learn);
            }
        }
        let mut inc = w.include_adaptive;
        let r = ui.add_enabled(!running, egui::Checkbox::new(&mut inc, crate::tr("adaptive einbeziehen")));
        lr::hover_tip(ui, &r, crate::tr("Auch Fotos mit adaptiven Kamera-Einstellungen nutzen (DR Auto, Schattenkorrektur, Auto-Optimierung …); sie passen das Kamera-JPEG an die Szene an, daher sonst ausgelassen"));
        if inc != w.include_adaptive {
            w.include_adaptive = inc;
            w.cameras = camera_rows(&w.cands, Some(&w.snap.scanned), inc);
            w.stats.clear();
        }
    });
}

/// Names of the learning phases in order (see [`phase_index`]).
fn phase_names() -> [&'static str; 7] {
    [crate::tr("Prüfen"), crate::tr("Ausrichtung"), crate::tr("Paare"), crate::tr("Matrix"), crate::tr("Kurven"), crate::tr("Tabelle"), crate::tr("Kontrolle")]
}

fn phase_index(p: Phase) -> u8 {
    match p {
        Phase::Reading => 0,
        Phase::Aligning => 1,
        Phase::Pairing => 2,
        Phase::Fitting(ll::Stage::Matrix) => 3,
        Phase::Fitting(ll::Stage::Curves) => 4,
        Phase::Fitting(ll::Stage::Table) => 5,
        Phase::Checking => 6,
    }
}

fn variant_label(v: &str) -> &'static str {
    match v {
        "neutral" => crate::tr("Neutral"),
        "current" => crate::tr("Heute"),
        "previous" => crate::tr("Bisher gelernt"),
        "matrix" => crate::tr("Matrix"),
        "curves" => crate::tr("Kurven"),
        "table" => crate::tr("Tabelle"),
        _ => "",
    }
}

fn live_ui(ui: &mut egui::Ui, w: &mut Window) {
    let Some(l) = &w.learning else { return };
    lr::category(ui, &crate::trf!("Lernen · {a}", a = l.label));
    let live = l.live.lock().unwrap();
    let now = live.phase.map(phase_index);
    // phases: done ones dim, the current one bright
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        let names = phase_names();
        for (i, name) in names.iter().enumerate() {
            let idx = i as u8;
            let c = match now {
                Some(n) if n == idx => crate::lr::TEXT_STRONG(),
                Some(n) if n > idx && (n >= 3 || idx >= 3) => GREEN.gamma_multiply(0.8),
                _ => crate::lr::gray(0x70),
            };
            ui.label(egui::RichText::new(*name).small().color(c));
            if i + 1 < names.len() {
                ui.label(egui::RichText::new("›").small().color(crate::lr::gray(0x60)));
            }
        }
    });
    let frac = (live.total > 0).then(|| live.done as f32 / live.total as f32);
    let what = if now.is_some_and(|n| n >= 3) { crate::tr("Anpassen und kontrollieren").to_string() } else { crate::trf!("Foto {d} von {n} · {a}", d = (live.done + 1).min(live.total.max(1)), n = live.total, a = live.name) };
    dialog::progress(ui, if now.is_some_and(|n| n >= 3) { None } else { frac }, &what);
    ui.horizontal_top(|ui| {
        if let Some(t) = &l.preview {
            let s = t.size_vec2();
            let k = 120.0 / s.x.max(s.y);
            let r = ui.add(egui::Image::new((t.id(), s * k)));
            lr::hover_tip(ui, &r, crate::tr("Zuletzt entwickeltes Foto"));
        }
        ui.vertical(|ui| {
            if !live.today.is_empty() {
                let mut v = live.today.clone();
                v.sort_by(|a, b| a.total_cmp(b));
                let r = ui.label(egui::RichText::new(crate::trf!("Heute bisher: ΔE00 {a}", a = fmt2(v[v.len() / 2]))).small().color(lr::TEXT()));
                lr::hover_tip(ui, &r, crate::tr("Median der Fotos so weit: Abstand der heutigen Entwicklung zum Kamera-JPEG"));
            }
            for (v, m) in &live.measured {
                ui.label(egui::RichText::new(format!("{} {}", variant_label(v), fmt2(*m))).small().color(lr::TEXT_DIM()));
            }
            ui.label(egui::RichText::new(dialog::duration(l.started.elapsed())).small().color(lr::TEXT_DIM()));
        });
    });
    drop(live);
    let b = ui.button(crate::tr("Abbrechen"));
    lr::hover_tip(ui, &b, crate::tr("Lernen beenden; es wird nichts geändert"));
    if b.clicked() {
        l.job.cancel.store(true, Ordering::Relaxed);
    }
    ui.label(egui::RichText::new(crate::tr("Läuft leise weiter, auch wenn das Fenster geschlossen wird.")).small().color(lr::TEXT_DIM()));
    ui.add_space(4.0);
}

fn result_ui(ui: &mut egui::Ui, w: &mut Window) {
    let Some(r) = &w.outcome else { return };
    let o = &r.learned.outcome;
    lr::category(ui, crate::tr("Ergebnis"));
    let details: Vec<String> = o.decisions.iter().cloned().chain(r.learned.skipped.iter().map(|(n, e)| format!("{n}: {e}"))).collect();
    if o.refused.is_some() {
        let usable = r.learned.photos.iter().filter(|p| p.pairs.len() >= 20).count();
        let why = if usable < ll::MIN_PHOTOS {
            crate::trf!("Nicht gelernt: nur {n} brauchbare Fotos, mindestens {m} sind nötig.", n = usable, m = ll::MIN_PHOTOS)
        } else {
            crate::tr("Nicht gelernt: zu wenige Aufnahmetage für eine getrennte Kontrolle.").to_string()
        };
        dialog::outcome(ui, dialog::Verdict::Yellow, &why, &details);
    } else if o.previous_kept {
        dialog::outcome(ui, dialog::Verdict::Yellow, crate::tr("Der bisherige Look bleibt: an denselben Kontrollfotos war der neue nicht deutlich besser."), &details);
    } else if o.stage.is_none() {
        dialog::outcome(ui, dialog::Verdict::Yellow, crate::tr("Kein besserer Look gefunden: an den Kontrollfotos war keine Stufe deutlich besser als die heutige Entwicklung."), &details);
    } else {
        let stage = match o.stage {
            Some(ll::Stage::Matrix) => crate::tr("Farbmatrix"),
            Some(ll::Stage::Curves) => crate::tr("Farbmatrix und Kurven"),
            _ => crate::tr("Farbmatrix, Kurven und 3D-Tabelle"),
        };
        let summary = if r.adopted { crate::trf!("Übernommen ({a})", a = stage) } else { crate::trf!("Besserer Look gefunden ({a})", a = stage) };
        dialog::outcome(ui, dialog::Verdict::Green, &summary, &details);
    }
    if o.refused.is_none() {
        let after = o.stage.map(|s| s.key());
        egui::Grid::new("camera-looks-metrics").num_columns(3).spacing([14.0, 2.0]).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::tr("ΔE00 Median / p90")).small().color(lr::TEXT_DIM()));
            let t = ui.label(egui::RichText::new(crate::trf!("Kontrolle ({a})", a = o.check_photos)).small().color(lr::TEXT_DIM()));
            lr::hover_tip(ui, &t, crate::tr("Herausgehaltene Fotos anderer Aufnahmetage: der ehrliche Kontrollwert"));
            let c = ui.label(egui::RichText::new(crate::trf!("Training ({a})", a = o.train_photos)).small().color(lr::TEXT_DIM()));
            lr::hover_tip(ui, &c, crate::tr("Fotos, aus denen gelernt wurde"));
            ui.end_row();
            let mut rows: Vec<(&str, &str)> = vec![("neutral", crate::tr("Neutral")), ("current", crate::tr("Heute"))];
            if o.variant("previous").is_some() {
                rows.push(("previous", crate::tr("Bisher gelernt")));
            }
            let best = after.or_else(|| o.variants.last().map(|v| v.0.as_str()).filter(|v| ["matrix", "curves", "table"].contains(v)));
            if let Some(b) = best {
                rows.push((b, crate::tr("Neu")));
            }
            for (v, label) in rows {
                ui.label(label);
                if let Some((t, c)) = o.variant(v) {
                    ui.label(format!("{} / {}", fmt2(c.median), fmt2(c.p90)));
                    ui.label(format!("{} / {}", fmt2(t.median), fmt2(t.p90)));
                }
                ui.end_row();
            }
        });
    }
    if r.renewing && o.refused.is_none() {
        ui.label(egui::RichText::new(crate::tr("Gemessen an denselben Kontrollfotos wie der bisherige Look.")).small().color(lr::TEXT_DIM()));
    }
    for wn in &r.learned.warnings {
        dialog::warning(ui, &warning_text(wn));
    }
    ui.add_space(4.0);
}

fn warning_text(w: &Warning) -> String {
    match w {
        Warning::OneFolder => crate::tr("Alle Fotos aus einem Ordner").into(),
        Warning::Adaptive(a, n) => crate::trf!("{n} Fotos mit „{a}“ ausgelassen", n = n, a = a),
        Warning::AdaptiveIncluded(a, n) => crate::trf!("{n} Fotos mit „{a}“ einbezogen (adaptive Kamera-Einstellung)", n = n, a = a),
        Warning::MixedWhiteBalance => crate::tr("Gemischte Weißabgleiche (Kunstlicht und Tageslicht)").into(),
        Warning::ThinSkin => crate::tr("Kaum Hauttöne unter den Fotos").into(),
        Warning::ThinSaturated => crate::tr("Kaum kräftige Farben unter den Fotos").into(),
    }
}

/// The looks of the selected camera and style: learned (switch in the look panel), bundled, hue table; with origin
/// and quality; a related camera's look as a starting point when there are too few photos.
fn looks_ui(ui: &mut egui::Ui, w: &mut Window, key: &str, row: &StyleRow, st: &Status, cam_photos: usize) {
    let line = |ui: &mut egui::Ui, title: &str, text: &str| {
        ui.horizontal_top(|ui| {
            ui.label(egui::RichText::new(title).small().strong().color(lr::TEXT()));
            ui.add(egui::Label::new(egui::RichText::new(text).small().color(lr::TEXT_DIM())).wrap());
        });
    };
    if let Some((on, meta)) = &st.learned {
        let label = learned_label(key, &row.camera_name, meta.as_ref());
        let mut t = provenance_text(&meta.as_ref().map(|m| m.to_string()).unwrap_or_default());
        if !on {
            t.push_str(&format!(" · {}", crate::tr("ausgeschaltet (Look-Fach)")));
        }
        line(ui, crate::tr("Gelernt"), &format!("{label}: {t}"));
        if let Some(more) = new_since(meta.as_ref(), cam_photos) {
            ui.label(egui::RichText::new(crate::trf!("{n} neue Fotos seit dem Lernen", n = more)).small().color(lr::BLUE_TEXT()));
        }
        if let Some(wb) = meta.as_ref().and_then(|m| m["wb_spread"].as_array().cloned()) {
            if let [a, b] = &wb[..] {
                line(ui, crate::tr("Weißabgleich"), &format!("{} – {} K", a.as_f64().unwrap_or(0.0).round(), b.as_f64().unwrap_or(0.0).round()));
            }
        }
    }
    if let Some(b) = st.bundled.as_ref().and_then(|b| rawdev::looks::find(b)) {
        line(ui, if st.own { crate::tr("Eigener Look") } else { crate::tr("Mitgeliefert") }, &format!("{}: {}", style_label(b.name.split_once('/').map_or("", |x| x.1)), provenance_text(&b.meta)));
    }
    if st.hue_table {
        line(ui, crate::tr("Eingebaut"), crate::tr("Farbtontabelle der Standardentwicklung, an Kontrollfotos gemessen"));
    }
    if st.learned.is_none() && st.bundled.is_none() && !st.hue_table {
        line(ui, crate::tr("Keiner"), crate::tr("neutrale Entwicklung mit der Farbmatrix der Kamera"));
    }
    // too few photos: a related camera's look as a clearly marked starting point
    if st.learned.is_none() && st.bundled.is_none() && assess(row, w.include_adaptive, None).0 == Learnable::No {
        if let Some((from, learned)) = related(key).into_iter().next() {
            let b = ui.add_enabled(w.learning.is_none(), egui::Button::new(crate::trf!("Look von {a} als Startwert …", a = from.split_once('/').map_or(from.clone(), |(c, st)| format!("{} {}", c.to_uppercase(), style_label(st))))));
            lr::hover_tip(ui, &b, crate::tr("Zu wenige Fotos: den Look einer verwandten Kamera übernehmen, gekennzeichnet als Basis ohne Messung"));
            if b.clicked() {
                w.action = Some(Action::StartFrom(from, learned));
            }
        }
    }
}

fn stats_ui(ui: &mut egui::Ui, w: &Window, row: &StyleRow, cam_photos: usize, cov: Option<&Coverage>, from_blocks: bool) {
    egui::Grid::new("camera-looks-stats-grid").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
        let mut kv = |k: &str, v: String, tip: &str| {
            let r = ui.label(egui::RichText::new(k).small().color(lr::TEXT_DIM()));
            lr::hover_tip(ui, &r, tip);
            ui.label(egui::RichText::new(v).small().color(lr::TEXT()));
            ui.end_row();
        };
        kv(crate::tr("Fotos der Kamera"), dialog::number(cam_photos), crate::tr("RAW-Fotos dieser Kamera in der Bibliothek"));
        kv(crate::tr("in diesem Bildstil"), dialog::number(row.scanned.len()), crate::tr("Bei mehr als 400 Fotos einer Kamera wird eine über die Aufnahmetage verteilte Auswahl gelesen"));
        kv(crate::tr("nutzbar"), dialog::number(row.usable), crate::tr("Ohne Fotos mit adaptiven Kamera-Einstellungen (außer sie sind einbezogen)"));
        kv(crate::tr("davon zum Lernen"), dialog::number(row.chosen.len()), crate::tr("Höchstens 4 je Aufnahmetag und 80 insgesamt, über den Tag verteilt"));
        kv(crate::tr("adaptiv"), dialog::number(row.adaptive), crate::tr("DR Auto, Schattenkorrektur, Auto-Optimierung und Ähnliches"));
        kv(crate::tr("Aufnahmetage"), dialog::number(row.days), crate::tr("Kontrollfotos stammen von anderen Tagen als die Trainingsfotos"));
        kv(crate::tr("Ordner"), dialog::number(row.folders), "");
        kv(crate::tr("mit Kamera-JPEG"), dialog::number(row.with_jpeg), crate::tr("RAW+JPEG; sonst dient die im RAW eingebettete Vorschau als Kamera-Bild"));
    });
    ui.add_space(4.0);
    match cov.filter(|c| c.photos > 0) {
        None => {
            let t = if w.stats_job.is_some() { crate::tr("Farbabdeckung wird aus den Vorschauen ermittelt …") } else { crate::tr("Keine Vorschauen für die Farbabdeckung") };
            ui.label(egui::RichText::new(t).small().color(lr::TEXT_DIM()));
        }
        Some(c) => {
            let src = if from_blocks { crate::tr("aus den Lernblöcken (Kamera-JPEG)") } else { crate::tr("aus den Vorschauen der Bibliothek") };
            ui.label(egui::RichText::new(crate::trf!("Abdeckung, {a}", a = src)).small().color(lr::TEXT_DIM()));
            bars(ui, crate::tr("Helligkeit"), &c.brightness, &|_| crate::lr::gray(0xb0), crate::tr("Anteil dunkler bis heller Bildteile (fünf Stufen)"));
            bars(ui, crate::tr("Farben"), &c.hues, &|i| hue_colour(i), crate::tr("Anteil farbiger Bildteile je Farbton (Rot, Gelb, Grün, Cyan, Blau, Magenta)"));
            ui.horizontal(|ui| {
                let s = ui.label(egui::RichText::new(crate::trf!("Hauttöne {a} %", a = (c.skin * 100.0).round())).small().color(if c.skin < 0.01 { YELLOW } else { lr::TEXT() }));
                lr::hover_tip(ui, &s, crate::tr("Anteil hauttonähnlicher Farben; unter 1 % lernt der Look Hauttöne kaum"));
                let k = ui.label(egui::RichText::new(crate::trf!("kräftige Farben {a} %", a = (c.saturated * 100.0).round())).small().color(if c.saturated < 0.01 { YELLOW } else { lr::TEXT() }));
                lr::hover_tip(ui, &k, crate::tr("Anteil stark gesättigter Farben; unter 1 % bleiben kräftige Farben unsicher"));
            });
        }
    }
    // white balance: measured from the RAWs while learning
    let wb = w.outcome.as_ref().filter(|o| o.key == row.key).map(|o| o.learned.kelvins.clone());
    let text = match wb.as_deref() {
        Some(k) if !k.is_empty() => {
            let (lo, hi) = spread(k);
            let mixed = if learn::mixed_white_balance(k) { crate::tr(" · gemischt (Kunstlicht und Tageslicht)").to_string() } else { String::new() };
            crate::trf!("Weißabgleich {a}–{b} K{c}", a = lo.round(), b = hi.round(), c = mixed)
        }
        _ => crate::tr("Weißabgleich-Streuung: wird beim Lernen gemessen").to_string(),
    };
    let r = ui.label(egui::RichText::new(text).small().color(lr::TEXT_DIM()));
    lr::hover_tip(ui, &r, crate::tr("10. bis 90. Perzentil des Weißabgleichs der Aufnahmen"));
}

fn spread(k: &[f32]) -> (f32, f32) {
    let mut v: Vec<f32> = k.iter().copied().filter(|x| *x > 0.0).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() {
        return (0.0, 0.0);
    }
    (v[v.len() / 10], v[(v.len() * 9 / 10).min(v.len() - 1)])
}

fn hue_colour(i: usize) -> Color32 {
    // Oklab hue sectors of 30°, from about red (0–30°) onwards
    let h = (i as f32 * 30.0 + 15.0).to_radians();
    let lab = [0.72, 0.12 * h.cos(), 0.12 * h.sin()];
    let s = raw::development::oklab_to_srgb_lin(lab).map(|v| graineo_color::srgb_encode(v.clamp(0.0, 1.0)));
    Color32::from_rgb((s[0] * 255.0) as u8, (s[1] * 255.0) as u8, (s[2] * 255.0) as u8)
}

fn bars(ui: &mut egui::Ui, label: &str, v: &[f32], colour: &dyn Fn(usize) -> Color32, tip: &str) {
    ui.horizontal(|ui| {
        let l = ui.add_sized([72.0, 16.0], egui::Label::new(egui::RichText::new(label).small().color(lr::TEXT_DIM())));
        lr::hover_tip(ui, &l, tip);
        let width = (ui.available_width() - 4.0).min(220.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 24.0), egui::Sense::hover());
        lr::hover_tip(ui, &resp, tip);
        let p = ui.painter();
        p.rect_filled(rect, 0.0, crate::lr::gray(0x16));
        let max = v.iter().copied().fold(0.0f32, f32::max).max(1e-6);
        let bw = rect.width() / v.len() as f32;
        for (i, x) in v.iter().enumerate() {
            let h = (rect.height() - 2.0) * (x / max).sqrt();
            let r = Rect::from_min_max(Pos2::new(rect.min.x + i as f32 * bw + 1.0, rect.max.y - 1.0 - h), Pos2::new(rect.min.x + (i + 1) as f32 * bw - 1.0, rect.max.y - 1.0));
            p.rect_filled(r, 0.0, colour(i));
        }
    });
}

fn versions_ui(ui: &mut egui::Ui, w: &mut Window, key: &str) {
    let files = look_files(key);
    let versions = &files.versions;
    if versions.is_empty() {
        return;
    }
    lr::category(ui, crate::tr("Fassungen"));
    let now = ui.input(|i| i.time);
    if w.restore_armed.as_ref().is_some_and(|(_, t)| now - t > 4.0) {
        w.restore_armed = None;
    }
    for v in versions {
        let p = ll::provenance(&v.meta);
        let date = p.created.as_deref().map(date_text).unwrap_or_default();
        let quality = p.delta_e.map(quality_text).unwrap_or_else(|| crate::tr("ohne Messung").into());
        let photos = p.photos.map(|n| crate::trf!("{n} Fotos", n = n)).unwrap_or_default();
        ui.horizontal(|ui| {
            let state = if v.current { crate::tr("in Gebrauch") } else { crate::tr("ersetzt") };
            ui.label(egui::RichText::new(format!("{date} · {photos} · {quality}")).small().color(lr::TEXT()));
            ui.label(egui::RichText::new(state).small().color(if v.current { GREEN } else { lr::TEXT_DIM() }));
            if !v.current {
                let armed = w.restore_armed.as_ref().is_some_and(|(p, _)| *p == v.path);
                let b = ui.add_enabled(w.learning.is_none(), egui::Button::new(if armed { crate::tr("Wirklich?") } else { crate::tr("Zurück zu dieser") }).small());
                lr::hover_tip(ui, &b, crate::tr("Diese frühere Fassung wieder verwenden; die jetzige wird aufbewahrt"));
                if b.clicked() {
                    if armed {
                        w.action = Some(Action::Restore(v.path.clone()));
                        w.restore_armed = None;
                    } else {
                        w.restore_armed = Some((v.path.clone(), now));
                    }
                }
            }
        });
    }
    if w.restore_armed.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------------------------------------
// actions

fn run_action(ctx: &egui::Context, w: &mut Window, a: Action) {
    let Some(key) = w.selected.clone() else { return };
    match a {
        Action::Learn => start_learning(w, &key),
        Action::Adopt => {
            let Some(o) = w.outcome.as_mut().filter(|o| o.key == key) else { return };
            let Some(look) = &o.learned.outcome.look else { return };
            let (lo, hi) = spread(&o.learned.kelvins);
            let (cam_photos, camera_name) = w.cameras.iter().find_map(|c| c.styles.iter().find(|s| s.key == key).map(|s| (c.photos, s.camera_name.clone()))).unwrap_or_default();
            let look = learn::with_meta(look, &[("library_photos", serde_json::json!(cam_photos)), ("camera_name", serde_json::json!(camera_name)), ("wb_spread", serde_json::json!([lo.round(), hi.round()]))]);
            match learn::install(&key, &look) {
                Ok(()) => {
                    o.adopted = true;
                    w.message = Some((true, crate::trf!("Übernommen: {a} ist jetzt der Standard dieser Kamera. Vorschauen werden im Leerlauf erneuert.", a = key)));
                    if let Some(s) = &mut w.compare.shown {
                        s.new = None;
                    }
                }
                Err(e) => w.message = Some((false, crate::trf!("Nicht übernommen: {a}", a = e))),
            }
        }
        Action::Restore(path) => {
            w.message = Some(match learn::restore(&key, &path) {
                Ok(()) => (true, crate::tr("Frühere Fassung wieder in Gebrauch; die bisherige ist aufbewahrt.").into()),
                Err(e) => (false, crate::trf!("Nicht zurückgesetzt: {a}", a = e)),
            });
        }
        Action::StartFrom(from, learned) => {
            w.message = Some(match adopt(&key, &from, learned) {
                Ok(()) => (true, crate::trf!("Startwert übernommen: Look von {a} für {b}", a = from, b = key)),
                Err(e) => (false, crate::trf!("Startwert nicht übernommen: {a}", a = e)),
            });
        }
        Action::Export => {
            let name = format!("{}.x3look", key.replace('/', "-"));
            crate::file_dialog::save(ctx, EXPORT, move |d| d.set_title(crate::tr("Look exportieren")).set_file_name(name).add_filter(crate::tr("graineo-Look"), &["x3look"]).add_filter(crate::tr("Farb-LUT"), &["cube"]));
        }
        Action::Import => {
            crate::file_dialog::file(ctx, IMPORT, |d| d.set_title(crate::tr("Look importieren")).add_filter(crate::tr("Look"), &["x3look", "cube"]));
        }
    }
}

fn file_results(w: &mut Window) {
    if let Some(target) = crate::file_dialog::fetch_one(EXPORT) {
        let Some(key) = w.selected.clone() else { return };
        let res = raw::looks::Look::file(&learn::look_path(&key)).ok_or_else(|| "look not found".to_string()).and_then(|l| learn::export(&l, &target));
        w.message = Some(match res {
            Ok(()) => (true, crate::trf!("Gespeichert: {a}", a = target.display())),
            Err(e) => (false, crate::trf!("Nicht gespeichert: {a}", a = e)),
        });
    }
    if let Some(source) = crate::file_dialog::fetch_one(IMPORT) {
        let res = learn::read_shared(&source, w.selected.as_deref()).and_then(|(key, look)| learn::install(&key, &look).map(|_| key));
        w.message = Some(match res {
            Ok(key) => {
                w.select(key.clone());
                (true, crate::trf!("Geladen als Look von {a}", a = key))
            }
            Err(e) => (false, crate::trf!("Nicht geladen: {a}", a = e)),
        });
    }
}

/// Start learning `key` in the background (the previous result goes).
fn start_learning(w: &mut Window, key: &str) {
    if w.learning.is_some() || w.reading(key).is_some() {
        return;
    }
    let Some((_, row)) = w.row(key) else { return };
    let row = row.clone();
    let label = format!("{} {}", row.camera_name, style_label(&row.style));
    let previous = raw::looks::Look::file(&learn::look_path(key));
    let renewing = previous.is_some();
    let live = Arc::new(Mutex::new(Live::default()));
    let l2 = live.clone();
    let include_adaptive = w.include_adaptive;
    let scanned = row.scanned.clone();
    let job = Job::start("camera-looks-learn", 2, move |keep_going, _progress| {
        learn::learn(&scanned, include_adaptive, &today(), previous.as_ref(), keep_going, &|e| {
            let mut l = l2.lock().unwrap();
            match e {
                Event::Phase(p) => l.phase = Some(p),
                Event::Photo { done, total, name, preview } => {
                    l.done = done;
                    l.total = total;
                    l.name = name.to_string();
                    if let Some(p) = preview {
                        l.preview = Some(learn::small(p, 240));
                        l.preview_new = true;
                    }
                }
                Event::PhotoDone { delta_e_today, .. } => l.today.push(delta_e_today),
                Event::Measured(v, m) => l.measured.push((v.to_string(), m)),
            }
        })
    });
    w.outcome = None;
    w.message = None;
    w.learning = Some(Learning { key: key.to_string(), label, renewing, job, live, started: Instant::now(), preview: None });
}

// ---------------------------------------------------------------------------------------------
// comparison

fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::Neutral => crate::tr("Neutral"),
        Mode::Current => crate::tr("Aktuell"),
        Mode::New => crate::tr("Neu"),
        Mode::Camera => crate::tr("Kamera-JPEG"),
    }
}

fn compare_ui(ui: &mut egui::Ui, w: &mut Window) {
    // nothing of the comparison (toolbar, image) ever reaches into the side panels
    ui.set_clip_rect(ui.max_rect());
    let Some(key) = w.selected.clone().filter(|k| w.row(k).is_some()) else {
        dialog::hint(ui, crate::tr("Hier erscheint der Vergleich eines Fotos: neutral, aktuell, neu gelernt und Kamera-JPEG."));
        return;
    };
    let strip = w.strip(&key);
    // a photo to show: the first check photo, else the first one
    if w.compare.photo.as_ref().is_none_or(|p| !strip.iter().any(|s| &s.0 == p)) {
        w.compare.photo = strip.iter().find(|s| s.2 == Mark::Check).or(strip.first()).map(|s| s.0.clone());
    }
    let want = w.compare.photo.clone();
    if let Some(p) = &want {
        let have = w.compare.shown.as_ref().is_some_and(|s| &s.path == p);
        let pending = w.compare.job.as_ref().is_some_and(|(q, _)| q == p);
        if !have && !pending {
            if let Some((_, j)) = &w.compare.job {
                j.cancel.store(true, Ordering::Relaxed);
            }
            let path = p.clone();
            w.compare.job = Some((p.clone(), Job::start("camera-looks-view", 2, move |keep_going, _| match learn::views(&path, keep_going) {
                Ok(v) => v,
                Err(e) => {
                    crate::log(&format!("camera looks view {}: {e}", path.display()));
                    None
                }
            })));
        }
    }
    // toolbar
    let modes = [(Mode::Neutral, mode_label(Mode::Neutral)), (Mode::Current, mode_label(Mode::Current)), (Mode::New, mode_label(Mode::New)), (Mode::Camera, mode_label(Mode::Camera))];
    let tips = [
        crate::tr("Entwicklung ohne kameraeigene Farbkorrektur: die Grundlage, auf der gelernt wird"),
        crate::tr("Standardentwicklung, wie sie jetzt ist (mit eingeschaltetem gelernten Look)"),
        crate::tr("Grundlage mit dem neu gelernten Look (oder dem gelernten Look in Gebrauch)"),
        crate::tr("Das Bild der Kamera: JPEG neben dem RAW oder die eingebettete Vorschau"),
    ];
    // two fixed rows: what is compared (left | right), then how
    ui.horizontal(|ui| {
        lr::segmented_tips(ui, &mut w.compare.left, &modes, &tips, 6.0);
        ui.label(egui::RichText::new("|").color(crate::lr::gray(0x60)));
        lr::segmented_tips(ui, &mut w.compare.right, &modes, &tips, 6.0);
    });
    ui.horizontal(|ui| {
        let mut side = w.compare.side_by_side;
        let r = lr::segmented_tips(ui, &mut side, &[(false, crate::tr("Wischen")), (true, crate::tr("Nebeneinander"))], &[crate::tr("Ein Bild, links und rechts der Trennlinie verschieden (Linie ziehen)"), crate::tr("Beide Bilder nebeneinander, Zoomen und Verschieben gekoppelt")], 6.0);
        if r {
            w.compare.side_by_side = side;
        }
        let zoomed = w.compare.zoom > 1.01;
        let b = ui.add(egui::Button::new(if zoomed { crate::tr("Einpassen") } else { crate::tr("100 %") }).selected(zoomed));
        lr::hover_tip(ui, &b, crate::tr("Zwischen eingepasst und 100 % der Vorschau wechseln (auch Doppelklick ins Bild); Scrollrad oder Zwei-Finger-Geste zoomt, Ziehen verschiebt"));
        if b.clicked() {
            w.compare.zoom = if zoomed { 1.0 } else { -1.0 };
            w.compare.center = Vec2::splat(0.5);
        }
    });
    // the photo's line: name, mark, exposure note
    let info = match (&w.compare.shown, &want) {
        (Some(s), Some(p)) if &s.path == p => {
            let src = match s.source {
                ll::ReferenceSource::CameraJpeg => crate::tr("Kamera-JPEG neben dem RAW"),
                ll::ReferenceSource::Embedded => crate::tr("eingebettete Vorschau"),
            };
            crate::trf!("{a} · Belichtung an das Kamera-Bild angeglichen ({b} EV) · {c}", a = file_name(p), b = format!("{:+.2}", -s.exposure), c = src)
        }
        (_, Some(p)) => crate::trf!("{a} · wird entwickelt …", a = file_name(p)),
        _ => String::new(),
    };
    ui.add(egui::Label::new(egui::RichText::new(info).small().color(lr::TEXT_DIM())).truncate());
    let frame = ui.max_rect();
    let avail = Rect::from_min_max(Pos2::new(frame.min.x, ui.cursor().min.y), frame.max);
    let image_area = Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - STRIP_H - 6.0));
    let strip_area = Rect::from_min_max(Pos2::new(avail.min.x, image_area.max.y + 6.0), avail.max);
    images_ui(ui, w, &key, image_area);
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("camera-looks-strip").max_rect(strip_area).layout(egui::Layout::left_to_right(egui::Align::Min)));
    strip_ui(&mut child, w, &strip);
    ui.allocate_rect(avail, egui::Sense::hover());
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn images_ui(ui: &mut egui::Ui, w: &mut Window, key: &str, area: Rect) {
    let resp = ui.interact(area, egui::Id::unique("camera-looks-image"), egui::Sense::click_and_drag());
    ui.painter().rect_filled(area, 0.0, lr::IMAGE_BG());
    // the texture of the new look (made on demand from the neutral image)
    // (the look itself only when its texture has to be made, not in every frame)
    let new_id = w.new_look_id(key);
    let stale = matches!((&new_id, w.compare.shown.as_ref()), (Some(id), Some(s)) if s.new.as_ref().is_none_or(|(i, _)| i != id));
    let new_look = if stale { w.new_look(key) } else { None };
    if let Some(s) = &mut w.compare.shown {
        match (&new_id, &new_look) {
            (Some(id), Some(look)) => {
                let mut img = s.neutral_img.clone();
                look.apply(&mut img);
                s.new = Some((id.clone(), texture(ui.ctx(), "camera-looks-new", &img)));
            }
            (None, _) => s.new = None,
            _ => {}
        }
    }
    let c = &mut w.compare;
    let Some(s) = c.shown.as_ref() else {
        let t = if c.job.is_some() { crate::tr("Foto wird entwickelt …") } else { crate::tr("Kein Foto zum Vergleichen") };
        ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, t, egui::FontId::proportional(13.0), lr::TEXT_DIM());
        return;
    };
    let gap = 8.0;
    let panes: Vec<Rect> = if c.side_by_side { crate::compare_view::pane_rects(2, area, 2, 1, gap) } else { vec![area] };
    let pane = panes[0];
    let fit = (pane.width() / s.size.x).min(pane.height() / s.size.y);
    let ppp = ui.ctx().pixels_per_point();
    let one = 1.0 / (fit * ppp);
    let max_zoom = (one * 4.0).max(2.0);
    if c.zoom < 0.0 {
        c.zoom = one.max(1.0);
    }
    // gestures: pinch and mouse wheel zoom around the pointer, a trackpad swipe pans when zoomed in, dragging pans
    let pointer = resp.hover_pos();
    let pane_at = |p: Pos2| panes.iter().copied().find(|r| r.contains(p)).unwrap_or(pane);
    let mut size = s.size * fit * c.zoom;
    if resp.hovered() {
        if let Some(t) = crate::zoom::scroll_source(ui.ctx()) {
            c.trackpad = t;
        }
        let (pinch, swipe, alt) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
        let (f, pan) = crate::zoom::develop_gesture(pinch, swipe, c.trackpad, alt, c.zoom > 1.01);
        if f != 1.0 {
            if let Some(p) = pointer {
                let pr = pane_at(p);
                let uv = c.center + (p - pr.center()) / size;
                c.zoom = (c.zoom * f).clamp(1.0, max_zoom);
                size = s.size * fit * c.zoom;
                c.center = uv - (p - pr.center()) / size;
            }
        }
        if pan != Vec2::ZERO {
            c.center -= pan / size;
        }
    }
    let split_x = pane.min.x + c.wipe * pane.width();
    if resp.drag_started() {
        c.dragging_divider = !c.side_by_side && resp.interact_pointer_pos().is_some_and(|p| (p.x - split_x).abs() < 10.0);
    }
    if resp.dragged() {
        if c.dragging_divider {
            if let Some(p) = resp.interact_pointer_pos() {
                c.wipe = ((p.x - pane.min.x) / pane.width()).clamp(0.0, 1.0);
            }
        } else {
            c.center -= resp.drag_delta() / size;
        }
    }
    if resp.drag_stopped() {
        c.dragging_divider = false;
    }
    if resp.double_clicked() {
        if c.zoom > 1.01 {
            c.zoom = 1.0;
            c.center = Vec2::splat(0.5);
        } else if let Some(p) = pointer {
            let pr = pane_at(p);
            let uv = c.center + (p - pr.center()) / size;
            c.zoom = one.max(1.0).min(max_zoom);
            size = s.size * fit * c.zoom;
            c.center = uv - (p - pr.center()) / size;
        }
    }
    // no empty border next to the image
    for k in 0..2 {
        let (sz, vis) = (size[k], pane.size()[k]);
        c.center[k] = if sz <= vis + 1.0 { 0.5 } else { c.center[k].clamp(0.5 * vis / sz, 1.0 - 0.5 * vis / sz) };
    }
    let near_divider = !c.side_by_side && pointer.is_some_and(|p| (p.x - split_x).abs() < 10.0);
    if c.dragging_divider || near_divider {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    } else if c.zoom > 1.01 && resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if c.zoom > 1.01 && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let tex = |m: Mode| -> Option<&egui::TextureHandle> {
        match m {
            Mode::Neutral => Some(&s.neutral),
            Mode::Current => Some(&s.current),
            Mode::New => s.new.as_ref().map(|(_, t)| t),
            Mode::Camera => Some(&s.camera),
        }
    };
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let paint = |painter: &egui::Painter, pr: Rect, m: Mode| {
        let r = Rect::from_center_size(pr.center() + (Vec2::splat(0.5) - c.center) * size, size);
        match tex(m) {
            Some(t) => {
                painter.image(t.id(), r, uv, Color32::WHITE);
            }
            None => {
                painter.text(pr.center(), egui::Align2::CENTER_CENTER, crate::tr("Noch kein neuer Look: erst lernen"), egui::FontId::proportional(12.0), lr::TEXT_DIM());
            }
        }
    };
    let painter = ui.painter().with_clip_rect(area);
    let painter = &painter;
    let label_w = |r: Rect| r.width() * 0.45;
    if c.side_by_side {
        for (k, m) in [(0, c.left), (1, c.right)] {
            let pr = panes[k];
            let clip = painter.with_clip_rect(pr);
            clip.rect_filled(pr, 0.0, lr::IMAGE_BG());
            paint(&clip, pr, m);
            crate::compare_view::tag(&clip, pr.min + Vec2::new(8.0, 8.0), mode_label(m), false, false, label_w(pr));
        }
    } else {
        let left = Rect::from_min_max(pane.min, Pos2::new(split_x, pane.max.y));
        let right = Rect::from_min_max(Pos2::new(split_x, pane.min.y), pane.max);
        paint(&painter.with_clip_rect(left), pane, c.left);
        paint(&painter.with_clip_rect(right), pane, c.right);
        painter.line_segment([Pos2::new(split_x, pane.min.y), Pos2::new(split_x, pane.max.y)], egui::Stroke::new(1.0, crate::lr::overlay(200)));
        let handle = Rect::from_center_size(Pos2::new(split_x, pane.center().y), Vec2::new(8.0, 36.0));
        painter.rect_filled(handle, 2.0, crate::lr::overlay(if c.dragging_divider || near_divider { 230 } else { 160 }));
        crate::compare_view::tag(&painter.with_clip_rect(left), pane.min + Vec2::new(8.0, 8.0), mode_label(c.left), false, false, label_w(left).max(60.0));
        let rt = mode_label(c.right).to_uppercase();
        let tw = painter.layout_no_wrap(rt, egui::FontId::monospace(11.0), crate::lr::TEXT_STRONG()).size().x + 14.0;
        crate::compare_view::tag(&painter.with_clip_rect(right), Pos2::new(pane.max.x - 8.0 - tw, pane.min.y + 8.0), mode_label(c.right), false, false, label_w(right).max(60.0));
    }
    if c.zoom > 1.01 {
        let z = format!("{:.0} %", c.zoom / one * 100.0);
        painter.text(area.right_bottom() - Vec2::new(8.0, 8.0), egui::Align2::RIGHT_BOTTOM, z, egui::FontId::monospace(10.5), crate::lr::overlay(200));
    }
}

fn strip_ui(ui: &mut egui::Ui, w: &mut Window, strip: &[(PathBuf, Option<PathBuf>, Mark)]) {
    // load a few thumbnails per frame (small JPEGs of the app's cache, never the originals)
    let mut loaded = 0;
    for (p, t, _) in strip {
        if loaded >= 6 {
            ui.ctx().request_repaint();
            break;
        }
        if !w.compare.thumbs.contains_key(p) {
            let tex = t.as_ref().and_then(|t| image::open(t).ok()).map(|i| texture(ui.ctx(), "camera-looks-thumb", &learn::small(&i.to_rgb8(), 160)));
            w.compare.thumbs.insert(p.clone(), tex);
            loaded += 1;
        }
    }
    let h = ui.available_height() - 14.0;
    let mut pick = None;
    egui::ScrollArea::horizontal().id_salt("camera-looks-strip-scroll").auto_shrink([false; 2]).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (p, _, mark) in strip {
                let tex = w.compare.thumbs.get(p).and_then(|t| t.as_ref());
                let wdt = tex.map_or(h * 1.5, |t| h * t.size_vec2().x / t.size_vec2().y.max(1.0)).clamp(h * 0.6, h * 1.6);
                let (r, resp) = ui.allocate_exact_size(Vec2::new(wdt, h), egui::Sense::click());
                let painter = ui.painter();
                painter.rect_filled(r, 0.0, crate::lr::gray(0x22));
                if let Some(t) = tex {
                    painter.image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
                let selected = w.compare.photo.as_ref() == Some(p);
                if selected {
                    painter.rect_stroke(r, 0.0, egui::Stroke::new(2.0, crate::lr::TEXT_STRONG()), egui::StrokeKind::Inside);
                } else if resp.hovered() {
                    painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0x90)), egui::StrokeKind::Inside);
                }
                let (letter, colour, tip) = match mark {
                    Mark::Check => (crate::tr("K"), GREEN, crate::tr("Kontrollfoto: nicht zum Lernen benutzt, daran wird gemessen")),
                    Mark::Training => (crate::tr("T"), lr::BLUE_TEXT(), crate::tr("Trainingsfoto: daraus wird gelernt")),
                    Mark::Unknown => ("", lr::TEXT_DIM(), crate::tr("Foto, das zum Lernen dienen würde (Training oder Kontrolle entscheidet sich beim Lernen)")),
                };
                if !letter.is_empty() {
                    let b = Rect::from_min_size(r.min + Vec2::new(3.0, 3.0), Vec2::new(14.0, 13.0));
                    painter.rect_filled(b, 0.0, Color32::from_black_alpha(190));
                    painter.text(b.center(), egui::Align2::CENTER_CENTER, letter, egui::FontId::monospace(9.5), colour);
                }
                lr::hover_tip(ui, &resp, &format!("{} – {tip}", file_name(p)));
                if resp.clicked() {
                    pick = Some(p.clone());
                }
            }
        });
    });
    if let Some(p) = pick {
        w.compare.photo = Some(p);
    }
}

// ---------------------------------------------------------------------------------------------
// test aids

/// Test hook (with `GRAINEO_DATA_DIR` only): `GRAINEO_LEARN_LOOK=open` opens the window at start, `open:<camera/style>`
/// with that style selected, `learn:<camera/style>` also starts learning it once the styles are read.
/// `GRAINEO_LEARN_LOOK_VIEW=side|wipe[,zoom][,adaptive][,left=<mode>][,right=<mode>]` sets the comparison for
/// screenshots (modes neutral, current, new, camera; `adaptive` ticks "adaptive einbeziehen").
pub fn test_hook(cands: impl FnOnce() -> Vec<Candidate>) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let Ok(v) = crate::env::var("GRAINEO_LEARN_LOOK") else {
        DONE.store(true, Ordering::Relaxed);
        return;
    };
    let c = cands();
    if c.is_empty() {
        return;
    }
    DONE.store(true, Ordering::Relaxed);
    let (learn_key, open_key) = match v.split_once(':') {
        Some(("learn", k)) => (Some(k.to_string()), Some(k.to_string())),
        Some(("open", k)) => (None, Some(k.to_string())),
        _ => (None, None),
    };
    open(c, open_key);
    if let Some(k) = learn_key {
        *AUTO_LEARN.lock().unwrap() = Some(k);
    }
}

static AUTO_LEARN: Mutex<Option<String>> = Mutex::new(None);

/// Part of the test hook: start learning the preselected style once the styles are read.
pub fn auto_learn() {
    let mut a = AUTO_LEARN.lock().unwrap();
    let Some(key) = a.clone() else { return };
    let mut g = WINDOW.lock().unwrap();
    let Some(w) = g.as_mut() else { return };
    if !w.snap.complete() || w.learning.is_some() {
        return;
    }
    *a = None;
    if w.row(&key).is_some() {
        w.select(key.clone());
        start_learning(w, &key);
    }
}

fn test_view(w: &mut Window) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Relaxed) || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    DONE.store(true, Ordering::Relaxed);
    let Ok(v) = crate::env::var("GRAINEO_LEARN_LOOK_VIEW") else { return };
    let mode = |s: &str| match s {
        "neutral" => Some(Mode::Neutral),
        "current" => Some(Mode::Current),
        "new" => Some(Mode::New),
        "camera" => Some(Mode::Camera),
        _ => None,
    };
    for part in v.split(',') {
        match part {
            "side" => w.compare.side_by_side = true,
            "wipe" => w.compare.side_by_side = false,
            "zoom" => w.compare.zoom = -1.0,
            "adaptive" => {
                w.include_adaptive = true;
                w.cameras = camera_rows(&w.cands, Some(&w.snap.scanned), true);
            }
            p => {
                if let Some(m) = p.strip_prefix("left=").and_then(mode) {
                    w.compare.left = m;
                }
                if let Some(m) = p.strip_prefix("right=").and_then(mode) {
                    w.compare.right = m;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(n: usize, days: usize, folders: usize, adaptive: usize) -> Vec<Scanned> {
        (0..n)
            .map(|i| Scanned {
                candidate: Candidate { path: PathBuf::from(format!("/p/{i}.RAF")), day: format!("2026-01-{:02}", i % days + 1), folder: format!("f{}", i % folders), camera: "X100V".into(), thumb: None },
                camera: "x100v".into(),
                camera_name: "FUJIFILM X100V".into(),
                style: "provia".into(),
                adaptive: (i < adaptive).then_some("DR Auto"),
                has_jpeg: i % 2 == 0,
            })
            .collect()
    }

    #[test]
    fn estimate_is_honest() {
        let row = style_row("x100v/provia".into(), scanned(10, 5, 3, 0), false);
        assert_eq!(assess(&row, false, None).0, Learnable::No);
        let row = style_row("x100v/provia".into(), scanned(60, 1, 3, 0), false);
        assert_eq!(assess(&row, false, None).0, Learnable::No, "one day: no check possible");
        let row = style_row("x100v/provia".into(), scanned(60, 12, 3, 0), false);
        assert_eq!(row.chosen.len(), 48);
        assert_eq!(assess(&row, false, None).0, Learnable::Yes);
        let thin = Coverage { photos: 10, saturated: 0.001, ..Default::default() };
        assert_eq!(assess(&row, false, Some(&thin)).0, Learnable::Limited);
        let row = style_row("x100v/provia".into(), scanned(60, 6, 3, 0), false);
        assert_eq!(assess(&row, false, None).0, Learnable::Limited, "six days");
        // adaptive photos are left out unless included
        let row = style_row("x100v/provia".into(), scanned(60, 12, 3, 50), false);
        assert_eq!((row.usable, row.adaptive), (10, 50));
        assert_eq!(assess(&row, false, None).0, Learnable::No);
        let row = style_row("x100v/provia".into(), scanned(60, 12, 3, 50), true);
        assert_eq!(assess(&row, true, None).0, Learnable::Yes);
    }

    #[test]
    fn cameras_of_the_library_sorted_by_photos() {
        let mut cands: Vec<Candidate> = scanned(5, 2, 1, 0).into_iter().map(|s| s.candidate).collect();
        cands.extend((0..9).map(|i| Candidate { path: PathBuf::from(format!("/q/{i}.CR3")), day: "2026-02-01".into(), folder: "g".into(), camera: "Canon EOS RP".into(), thumb: None }));
        let rows = camera_rows(&cands, Some(&scanned(5, 2, 1, 0)), false);
        assert_eq!(rows.iter().map(|r| (r.name.as_str(), r.photos, r.styles.len())).collect::<Vec<_>>(), [("Canon EOS RP", 9, 0), ("FUJIFILM X100V", 5, 1)]);
    }

    #[test]
    fn new_photos_hint_only_when_clearly_more() {
        let m = serde_json::json!({"library_photos": 100});
        assert_eq!(new_since(Some(&m), 110), None);
        assert_eq!(new_since(Some(&m), 130), Some(30));
        assert_eq!(new_since(None, 500), None);
    }
}

impl crate::App {
    /// Learned-look key (`camera/style`) of the photo on screen, if it is a RAW with a learnable picture style.
    pub(crate) fn current_learn_key(&self) -> Option<String> {
        self.current.as_ref()?.loaded.previews.foreign.as_ref()?.learn_key.clone()
    }
}
