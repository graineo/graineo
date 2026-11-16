//! The measurement behind Help → Create Performance Report …: chooses one photo per camera family from what the user
//! picked and times every stage of opening and developing it with the app's own code paths (not with `graineo-dev`),
//! three runs each (first run = cold, second = warm, median over three), on the GPU and on the processor where both
//! exist. Meanwhile a sampler records the memory and threads of the app and how busy the whole processor was.
//!
//! Nothing here changes how the app computes: the stages are the functions the app calls (`Session::open_for`,
//! `Session::preview_linear` + `Preview::developed`, `develop::spp::working_key` in the base pool behind the base gate,
//! `Preview::from_spp`, `session::full_from_base`, the export path; `RawDoc::open`, `preview_stage`, `render_full`,
//! the raw export). Only the GPU switch is turned off for the processor runs and restored afterwards, and the app's
//! own background work is held back (as while the user works) so it does not disturb the times.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use develop::session::Preview;
use develop::{Session, Settings};

/// Runs per stage (first = cold, second = warm, median over all).
pub const RUNS: usize = 3;
/// At most this many photos per report.
pub const MAX_PHOTOS: usize = 8;
/// At most this many files may be picked one by one.
pub const MAX_PICKED: usize = 10;
/// Files looked at when a folder is chosen (a folder with the whole archive must not take minutes).
const MAX_SCANNED: usize = 20_000;
/// Folder depth searched below the chosen folder.
const MAX_DEPTH: usize = 3;

/// Camera family of a photo: one photo per family is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    Merrill,
    Quattro,
    Classic,
    XTrans,
    Bayer,
    Finished,
}

impl Family {
    /// English name for the report.
    pub fn name(self) -> &'static str {
        match self {
            Family::Merrill => "Foveon Merrill",
            Family::Quattro => "Foveon Quattro",
            Family::Classic => "Foveon Classic",
            Family::XTrans => "Fujifilm X-Trans",
            Family::Bayer => "Bayer RAW",
            Family::Finished => "JPEG/HEIC/TIFF",
        }
    }

    /// Name in the dialog (translated there).
    pub fn label(self) -> &'static str {
        match self {
            Family::Merrill => "Merrill",
            Family::Quattro => "Quattro",
            Family::Classic => "Foveon Classic",
            Family::XTrans => "X-Trans",
            Family::Bayer => "Bayer-RAW",
            Family::Finished => "JPEG/HEIC",
        }
    }

    pub fn is_sigma(self) -> bool {
        matches!(self, Family::Merrill | Family::Quattro | Family::Classic)
    }

    /// Family of a file from its ending (X3F: from its header, which reads a few bytes). `None`: not a photo.
    pub fn of(path: &Path) -> Option<Family> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        Some(match ext.as_str() {
            "x3f" => {
                if develop::quattro::is_quattro(path) {
                    Family::Quattro
                } else if develop::quattro::is_classic(path) {
                    Family::Classic
                } else {
                    Family::Merrill
                }
            }
            "raf" => Family::XTrans,
            "cr2" | "cr3" | "crw" | "arw" | "srf" | "sr2" | "nef" | "nrw" | "dng" | "orf" | "rw2" | "pef" | "srw" | "3fr" | "iiq" | "erf" | "mos" | "gpr" | "rwl" | "raw" => Family::Bayer,
            "jpg" | "jpeg" | "heic" | "heif" | "tif" | "tiff" | "png" => Family::Finished,
            _ => return None,
        })
    }

    /// Family from the ending only (no file access; X3F counts as a Sigma file of unknown generation).
    fn quick(path: &Path) -> Option<Option<Family>> {
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        if ext == "x3f" {
            return Some(None);
        }
        Family::of(path).map(Some)
    }
}

/// The photos chosen for a report.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub photos: Vec<(Family, PathBuf)>,
    /// photo files found (before choosing one per family)
    pub found: usize,
    /// picked files beyond [`MAX_PICKED`], left out
    pub skipped: usize,
}

/// One photo per family from `paths` (in name order: the first of each family), Sigma first, at most
/// [`MAX_PHOTOS`]. X3F headers are read only until all three Sigma generations are found.
pub fn choose(mut paths: Vec<PathBuf>, cancel: &AtomicBool) -> Selection {
    paths.sort();
    let mut by: std::collections::BTreeMap<Family, PathBuf> = Default::default();
    let mut found = 0;
    for p in paths {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let fam = match Family::quick(&p) {
            None => continue,
            Some(Some(f)) => f,
            Some(None) => {
                found += 1;
                if by.contains_key(&Family::Merrill) && by.contains_key(&Family::Quattro) && by.contains_key(&Family::Classic) {
                    continue;
                }
                match Family::of(&p) {
                    Some(f) => f,
                    None => continue,
                }
            }
        };
        if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f")) {
            found += 1;
        }
        by.entry(fam).or_insert(p);
    }
    let photos: Vec<(Family, PathBuf)> = by.into_iter().take(MAX_PHOTOS).collect();
    Selection { photos, found, skipped: 0 }
}

/// Picked files: at most [`MAX_PICKED`] are looked at.
pub fn choose_files(mut files: Vec<PathBuf>, cancel: &AtomicBool) -> Selection {
    let skipped = files.len().saturating_sub(MAX_PICKED);
    files.truncate(MAX_PICKED);
    Selection { skipped, ..choose(files, cancel) }
}

/// Photo files in `dir` and up to [`MAX_DEPTH`] folders below (hidden entries and packages such as the Photos
/// library skipped), at most [`MAX_SCANNED`] entries.
pub fn scan_folder(dir: &Path, cancel: &AtomicBool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > MAX_SCANNED || cancel.load(Ordering::Relaxed) {
                return out;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                let package = p.extension().is_some();
                if depth < MAX_DEPTH && !package {
                    stack.push((p, depth + 1));
                }
            } else if Family::quick(&p).is_some() {
                out.push(p);
            }
        }
    }
    out
}

// ───────────────────────────────────────────── results

/// One stage of one photo: its time in every run.
#[derive(Clone, Debug, Default)]
pub struct Stage {
    /// stable identifier (JSON)
    pub id: String,
    /// English description
    pub label: String,
    /// which path ran: "disk", "CPU", "GPU", "GPU→CPU" (asked for the GPU, computed on the processor) …
    pub path: String,
    pub ms: Vec<f64>,
    /// processor time of the app divided by the wall time (≈ cores busy on average) per run
    pub cores: Vec<f64>,
}

/// One photo.
#[derive(Clone, Debug, Default)]
pub struct Photo {
    pub index: usize,
    pub family: String,
    /// camera model and container format ("SIGMA DP2 Merrill", "X3F") – never the file name
    pub camera: String,
    pub format: String,
    pub size: [usize; 2],
    pub iso: Option<u32>,
    pub bytes: u64,
    pub drive: String,
    pub stages: Vec<Stage>,
    /// finer stages reported by the development code (rawdev timings), median per name in ms
    pub details: Vec<(String, Vec<f64>)>,
    pub notes: Vec<String>,
    pub error: Option<String>,
    pub peak_memory: Option<u64>,
    pub max_threads: Option<usize>,
    /// share of the whole processor busy while this photo was measured
    pub system_busy: Option<f64>,
    pub seconds: f64,
}

impl Photo {
    fn stage(&mut self, id: &str, label: &str) -> &mut Stage {
        if let Some(i) = self.stages.iter().position(|s| s.id == id) {
            return &mut self.stages[i];
        }
        self.stages.push(Stage { id: id.into(), label: label.into(), ..Default::default() });
        self.stages.last_mut().unwrap()
    }
}

/// Display of the window that started the report (from egui).
#[derive(Clone, Debug, Default)]
pub struct Display {
    pub scale: f32,
    /// monitor size in points, when known
    pub monitor: Option<[f32; 2]>,
    pub window: [f32; 2],
}

/// Graphics as wgpu sees it.
#[derive(Clone, Debug, Default)]
pub struct Graphics {
    pub adapters: Vec<rawdev::gpu::AdapterSummary>,
    /// adapter of the development of other formats (None: none usable or GPU off)
    pub raw_in_use: Option<String>,
    /// adapter of the Foveon noise reduction, or why there is none ("not available (…)")
    pub foveon: String,
    pub setting_on: bool,
    /// a GPU device was lost or the GPU is switched off by the environment
    pub available: bool,
}

/// Everything the report shows.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub version: String,
    pub target: String,
    pub created: String,
    pub file_stamp: String,
    pub system_before: library::platform::info::SystemInfo,
    pub system_after: library::platform::info::SystemInfo,
    pub busy_before: Option<f64>,
    pub display: Display,
    pub graphics: Graphics,
    pub threads: Vec<(String, usize)>,
    pub switches: Vec<String>,
    pub caches_drive: String,
    pub photos: Vec<Photo>,
    pub notes: Vec<String>,
    pub timing_log: Vec<String>,
    pub timing_log_on: bool,
    pub seconds: f64,
    pub cancelled: bool,
}

/// What the dialog shows while measuring.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub photo: usize,
    pub photos: usize,
    pub family: String,
    pub stage: String,
    pub run: usize,
    /// 0–1
    pub fraction: f32,
}

// ───────────────────────────────────────────── measuring

enum Stop {
    Cancelled,
    Failed(String),
}

impl From<String> for Stop {
    fn from(e: String) -> Self {
        Stop::Failed(e)
    }
}

/// Switches the GPU off while alive and restores the previous state (also on panic). If the setting "Use graphics
/// card" was changed meanwhile, the new setting wins (the app must compute as the preferences show).
struct GpuOff(bool, bool);

impl GpuOff {
    fn new() -> GpuOff {
        let before = pools::gpu_allowed();
        pools::set_gpu_allowed(false);
        GpuOff(before, crate::prefs::gpu_setting())
    }
}

impl Drop for GpuOff {
    fn drop(&mut self) {
        let setting = crate::prefs::gpu_setting();
        pools::set_gpu_allowed(if setting != self.1 { setting } else { self.0 });
    }
}

/// Memory and threads of the app, sampled every 100 ms while a photo is measured.
struct Sampler {
    stop: Arc<AtomicBool>,
    peak: Arc<Mutex<(u64, usize)>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    fn start() -> Sampler {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(Mutex::new((0u64, 0usize)));
        let (s, p) = (stop.clone(), peak.clone());
        let handle = std::thread::Builder::new()
            .name("perf-sampler".into())
            .spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    let m = library::platform::info::process_memory().unwrap_or(0);
                    let t = library::platform::info::process_threads().unwrap_or(0);
                    {
                        let mut g = p.lock().unwrap_or_else(|e| e.into_inner());
                        g.0 = g.0.max(m);
                        g.1 = g.1.max(t);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok();
        Sampler { stop, peak, handle }
    }

    fn finish(mut self) -> (u64, usize) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        *self.peak.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Sampler {
    /// Also after a panic in the measurement (caught by the caller): the sampling thread ends instead of running until
    /// the app quits.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Stage times of `rawdev` are collected while alive; on drop (end, cancel or panic) collecting stops and the
/// temporary export folder goes.
struct Collecting(PathBuf);

impl Collecting {
    fn start(tmp: PathBuf) -> Collecting {
        let _ = std::fs::create_dir_all(&tmp);
        rawdev::timings::collect(true);
        Collecting(tmp)
    }
}

impl Drop for Collecting {
    fn drop(&mut self) {
        rawdev::timings::collect(false);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Ctx<'a> {
    progress: &'a Mutex<Progress>,
    cancel: &'a AtomicBool,
    /// stages done / total, for the progress bar
    done: usize,
    total: usize,
    run: usize,
    tmp: PathBuf,
}

impl Ctx<'_> {
    /// Time one stage of `photo` and record it under `id`. Laps of the development code during the stage are kept
    /// as details.
    fn time<T>(&mut self, photo: &mut Photo, id: &str, label: &str, path: &str, f: impl FnOnce() -> T) -> Result<T, Stop> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Stop::Cancelled);
        }
        {
            let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            p.stage = id.to_string();
            p.run = self.run + 1;
            p.fraction = (self.done as f32 / self.total.max(1) as f32).min(1.0);
        }
        // the app's own background work waits meanwhile, as while the user works
        crate::jobs::note_input();
        let _ = rawdev::timings::take_all();
        let cpu0 = library::platform::cpu_seconds();
        let t = Instant::now();
        let out = f();
        let wall = t.elapsed().as_secs_f64();
        let cpu1 = library::platform::cpu_seconds();
        let laps = rawdev::timings::take_all();
        self.done += 1;
        let st = photo.stage(id, label);
        st.ms.push(wall * 1000.0);
        if let (Some(a), Some(b)) = (cpu0, cpu1) {
            if wall > 0.0 {
                st.cores.push((b - a) / wall);
            }
        }
        if st.path.is_empty() {
            st.path = path.to_string();
        }
        for (name, ms) in laps {
            let key = format!("{id}: {}", lap_name(&name));
            match photo.details.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => v.push(ms),
                None => photo.details.push((key, vec![ms])),
            }
        }
        Ok(out)
    }
}

/// English names for the stage names the development code reports (internal identifiers).
pub fn lap_name(n: &str) -> String {
    const NAMES: &[(&str, &str)] = &[
        ("normieren+WB", "normalise and white balance"),
        ("Rauschmodell", "noise model"),
        ("Lichter", "highlights"),
        ("Vorschau", "binned preview"),
        ("Demosaik", "demosaic"),
        ("Zuschnitt", "crop"),
        ("Rauschen", "noise reduction"),
        ("Rauschminderung (Vorschau)", "noise reduction (preview)"),
        ("Rauschminderung", "noise reduction"),
        ("Schärfe", "sharpening"),
        ("Objektiv (Vorschau)", "lens correction (preview)"),
        ("Objektiv", "lens correction"),
        ("Farbe+Ton", "colour and tone"),
        ("Helligkeitskarte", "brightness map"),
        ("Ausgabe (Ton/Farbe)", "output (tone/colour)"),
        ("Ausgabe (Vorschau)", "output (preview)"),
        ("Hochladen (einmal je Stufe)", "upload (once per stage)"),
        ("Hochladen", "upload"),
        ("Zurücklesen", "read back"),
        ("Rechnen (Detail + Ausgabe, ein Auftrag)", "compute (detail + output, one job)"),
        ("Rechnen (Detail, ein Auftrag)", "compute (detail, one job)"),
        ("Ausgabe: Parameter und Tabellen (CPU)", "output: parameters and tables (CPU)"),
        ("Ausgabe: GPU gesamt", "output: GPU total"),
        ("Ausgabe: Nachstufe (CPU)", "output: post stage (CPU)"),
        ("Look-Stufe (CPU)", "look stage (CPU)"),
        ("Hochladen (Falschfarben)", "upload (false colour)"),
        ("Falschfarben gesamt", "false colour total"),
        ("Falschfarben", "false colour"),
    ];
    let (prefix, rest) = match n.split_once(' ') {
        Some((p @ ("CPU" | "GPU"), r)) => (Some(p), r),
        _ => (None, n),
    };
    let rest = NAMES.iter().find(|(de, _)| *de == rest).map_or(rest.to_string(), |(_, en)| en.to_string());
    match prefix {
        Some(p) => format!("{p} {rest}"),
        None => rest,
    }
}

/// Measure the chosen photos and build the report (background thread; `cancel` stops after the running stage).
pub fn run(sel: &Selection, display: Display, progress: &Mutex<Progress>, cancel: &AtomicBool) -> Report {
    library::platform::set_priority(library::platform::Priority::UserInitiated);
    let t_all = Instant::now();
    let mut r = Report { version: library::update::current_version(), target: target(), display, ..Default::default() };
    {
        let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
        *p = Progress { photos: sel.photos.len(), stage: "System".into(), ..Default::default() };
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let lt = library::platform::local_time(now);
    r.created = format!("{:04}-{:02}-{:02} {:02}:{:02} (UTC{}{:02}:{:02})", lt.year, lt.month, lt.tag, lt.hour, lt.minute, if lt.offset_val < 0 { '-' } else { '+' }, lt.offset_val.abs() / 3600, lt.offset_val.abs() % 3600 / 60);
    r.file_stamp = format!("{:04}-{:02}-{:02}-{:02}{:02}{:02}", lt.year, lt.month, lt.tag, lt.hour, lt.minute, lt.second);
    // how busy the computer is before we start (one second)
    let t0 = library::platform::info::system_cpu_ticks();
    std::thread::sleep(Duration::from_secs(1));
    r.busy_before = t0.zip(library::platform::info::system_cpu_ticks()).and_then(|(a, b)| library::platform::info::busy_share(a, b));
    r.system_before = library::platform::info::system_info();
    r.graphics = graphics();
    r.threads = vec![
        ("work in front (per logical core)".into(), library::platform::foreground_threads()),
        ("Foveon base".into(), (library::platform::foreground_threads() / 2).max(2)),
        ("background".into(), library::platform::background_threads()),
        ("global compute pool".into(), rayon::current_num_threads()),
    ];
    r.switches = switches();
    r.caches_drive = sysdirs::app_cache_dir(library::APP).filter(|d| d.exists()).or_else(|| Some(library::data_dir())).map(|d| drive_text(&d)).unwrap_or_default();
    let tmp = library::data_dir().join("perf-reports").join("tmp");
    let collecting = Collecting::start(tmp.clone());
    let gpu_usable = r.graphics.setting_on && r.graphics.available;
    if r.busy_before.is_some_and(|b| b > 0.5) {
        r.notes.push(format!("The computer was busy before the measurement ({:.0} % of the processor): the times are upper estimates.", r.busy_before.unwrap_or(0.0) * 100.0));
    }
    if !r.graphics.setting_on {
        r.notes.push("The setting \"Use graphics card\" is off: only the processor path was measured.".into());
    } else if !r.graphics.available {
        r.notes.push("No usable GPU (switched off by the environment or a lost device): only the processor path was measured.".into());
    }
    if r.graphics.raw_in_use.is_none() && gpu_usable {
        r.notes.push("Other formats: no GPU adapter usable for the development (software renderer or shaders failed) – their GPU stages ran on the processor.".into());
    }
    let per_photo: Vec<usize> = sel.photos.iter().map(|(f, _)| stages_per_run(*f, gpu_usable) * RUNS).collect();
    let mut ctx = Ctx { progress, cancel, done: 0, total: per_photo.iter().sum(), run: 0, tmp };
    for (i, (fam, path)) in sel.photos.iter().enumerate() {
        {
            let mut p = progress.lock().unwrap_or_else(|e| e.into_inner());
            p.photo = i + 1;
            p.family = fam.label().to_string();
        }
        let mut photo = Photo { index: i + 1, family: fam.name().into(), ..Default::default() };
        photo.bytes = std::fs::metadata(path).map_or(0, |m| m.len());
        photo.drive = drive_text(path);
        let sampler = Sampler::start();
        let ticks0 = library::platform::info::system_cpu_ticks();
        let t = Instant::now();
        let done_before = ctx.done;
        let result = if fam.is_sigma() { measure_sigma(&mut ctx, &mut photo, *fam, path, gpu_usable) } else { measure_other(&mut ctx, &mut photo, path, gpu_usable) };
        photo.seconds = t.elapsed().as_secs_f64();
        photo.system_busy = ticks0.zip(library::platform::info::system_cpu_ticks()).and_then(|(a, b)| library::platform::info::busy_share(a, b));
        let (mem, threads) = sampler.finish();
        photo.peak_memory = (mem > 0).then_some(mem);
        photo.max_threads = (threads > 0).then_some(threads);
        // a failed photo: its remaining stages count as done for the bar
        ctx.done = done_before + per_photo[i];
        match result {
            Ok(()) => {}
            Err(Stop::Cancelled) => {
                r.cancelled = true;
                photo.notes.push("Cancelled.".into());
                r.photos.push(photo);
                break;
            }
            Err(Stop::Failed(e)) => photo.error = Some(e),
        }
        notes_for(&mut photo);
        r.photos.push(photo);
    }
    drop(collecting);
    r.system_after = library::platform::info::system_info();
    r.timing_log_on = crate::timing_log::enabled();
    r.timing_log = crate::timing_log::folder().map(|d| crate::timing_log::last_lines(&d, 100)).unwrap_or_default();
    r.seconds = t_all.elapsed().as_secs_f64();
    r
}

/// Timed stages per run (for the progress bar).
fn stages_per_run(f: Family, gpu: bool) -> usize {
    match f {
        Family::Merrill => 13 + gpu as usize,
        Family::Quattro | Family::Classic => 9 + gpu as usize,
        _ => 6 + gpu as usize,
    }
}

/// "SSD, internal" / "network drive" for the drive of `p` (never its name).
fn drive_text(p: &Path) -> String {
    let (kind, internal) = library::platform::info::drive(p);
    match internal {
        Some(true) => format!("{}, internal", kind.name()),
        Some(false) if kind != library::platform::info::DriveKind::Network => format!("{}, external", kind.name()),
        _ => kind.name().to_string(),
    }
}

fn target() -> String {
    format!("{} {}{}", std::env::consts::OS, std::env::consts::ARCH, if cfg!(debug_assertions) { ", debug build" } else { "" })
}

/// `GRAINEO_*` switches of this run: names, values only when they are short words or numbers (paths never).
pub fn switches() -> Vec<String> {
    let mut v: Vec<String> = std::env::vars_os()
        .filter_map(|(k, val)| {
            let k = k.to_str()?.to_string();
            if !k.starts_with("GRAINEO_") {
                return None;
            }
            Some(switch_text(&k, &val.to_string_lossy()))
        })
        .collect();
    v.sort();
    v
}

/// One switch for the report: `NAME=value` for short plain values, otherwise `NAME (set)`.
pub fn switch_text(name: &str, value: &str) -> String {
    // only numbers and switch words: any other short value may be a name (`GRAINEO_IMAGE=IMG_1234.CR3`, a search)
    let v = value.trim();
    let plain = v.len() <= 16 && (v.parse::<f64>().is_ok() || ["true", "false", "on", "off", "yes", "no"].contains(&v.to_ascii_lowercase().as_str()));
    if plain {
        format!("{name}={value}")
    } else {
        format!("{name} (set)")
    }
}

fn graphics() -> Graphics {
    let setting_on = crate::prefs::gpu_setting();
    // off by the setting, by `GRAINEO_GPU=0` or after a lost device
    let available = pools::gpu_allowed();
    // the devices open only when the GPU is allowed (both are opened by the app anyway when it develops)
    let raw_in_use = if available { rawdev::gpu::gpu().map(|g| g.name.clone()) } else { rawdev::gpu::adapter_in_use() };
    let foveon = match if available { develop::spp::gpu_adapter() } else { Err("GPU switched off".into()) } {
        Ok(name) => name,
        Err(e) => format!("not available ({e})"),
    };
    Graphics { adapters: rawdev::gpu::adapters(), raw_in_use, foveon, setting_on, available }
}

/// Notes a reader should not miss.
fn notes_for(p: &mut Photo) {
    for s in &p.stages {
        if s.path.contains("GPU→CPU") {
            p.notes.push(format!("{}: asked for the GPU, computed on the processor.", s.label));
        }
    }
    let get = |id: &str| p.stages.iter().find(|s| s.id == id).map(|s| crate::perf_text::stats(&s.ms).median);
    if let (Some(g), Some(c)) = (get("base_gpu"), get("base_cpu")) {
        if g > c * 0.9 && c > 50.0 {
            p.notes.push(format!("Exact base: the GPU ({g:.0} ms) is not faster than the processor ({c:.0} ms) on this computer."));
        }
    }
    if let (Some(g), Some(c)) = (get("full_gpu"), get("full_cpu")) {
        if g > c * 0.9 && c > 50.0 {
            p.notes.push(format!("Full resolution: the GPU ({g:.0} ms) is not faster than the processor ({c:.0} ms) on this computer."));
        }
    }
    let cores: Vec<f64> = p.stages.iter().filter(|s| s.id.starts_with("base") || s.id.starts_with("full")).flat_map(|s| s.cores.iter().copied()).collect();
    if let Some(m) = (!cores.is_empty()).then(|| crate::perf_text::stats(&cores).median) {
        let n = library::platform::foreground_threads() as f64;
        if m < n * 0.35 {
            p.notes.push(format!("The heavy stages kept only {m:.1} of {n:.0} logical cores busy on average (GPU-, memory- or serially bound)."));
        }
    }
}

/// Sigma: the app's opening (with the finer steps of the Merrill path measured separately), the first picture, the
/// exact base on the GPU and the processor, the fitted view from it, full resolution and the JPEG export.
fn measure_sigma(ctx: &mut Ctx, photo: &mut Photo, fam: Family, path: &Path, gpu: bool) -> Result<(), Stop> {
    let s = Settings::default();
    let key = s.spp_key(path);
    let prepared = fam != Family::Merrill;
    for run in 0..RUNS {
        ctx.run = run;
        ctx.time(photo, "read", "read file", "disk", || std::fs::read(path).map(|v| v.len())).and_then(|r| r.map_err(|e| Stop::Failed(e.to_string())))?;
        if fam == Family::Merrill {
            // the steps of `Session::open` one by one (same functions, for the breakdown)
            let file = ctx.time(photo, "x3f_parse", "open: parse X3F container", "CPU", || x3f::X3fFile::open(path)).and_then(|r| r.map_err(|e| Stop::Failed(e.to_string())))?;
            let (mut raw, _) = ctx.time(photo, "decode", "open: decode raw data", "CPU", || file.decode_raw_rescue()).and_then(|r| r.map_err(|e| Stop::Failed(e.to_string())))?;
            let cal = ctx.time(photo, "calibration", "open: black level and calibration", "CPU", || develop::Calibration::read(&file, &raw)).and_then(|r| r.map_err(|e| Stop::Failed(e.to_string())))?;
            ctx.time(photo, "defects", "open: defect pixels", "CPU", || develop::defects::replace_bad_pixels_raw(&mut raw, &cal))?;
        }
        if prepared {
            develop::quattro::forget(path);
        }
        let label = match fam {
            Family::Quattro => "open (app path, incl. Quattro preparation prepared_quick)",
            Family::Classic => "open (app path, incl. Classic preparation)",
            _ => "open (app path)",
        };
        let mut session = ctx.time(photo, "open", label, "CPU", || Session::open_for(path, &s)).and_then(|r| r.map_err(|e| Stop::Failed(e.to_string())))?;
        if run == 0 {
            photo.size = session.size();
            photo.camera = session.props.get("CAMMODEL").unwrap_or("").trim().to_string();
            photo.format = "X3F".into();
            photo.iso = session.props.get("ISO").and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v.round() as u32);
        }
        let (small, factor) = ctx.time(photo, "first_linear", "first picture: linearise to 2400 px", "CPU", || session.preview_linear(crate::jobs::PREVIEW_EDGE))?;
        let developed = ctx.time(photo, "first_develop", "first picture: own pipeline (noise reduction)", "CPU", || Preview::developed(small, factor, &session.cal, &s))?;
        let cal = session.cal.clone();
        ctx.time(photo, "first_render", "first picture: output", "CPU", || drop(developed.render(&cal, &s)))?;
        drop(developed);
        let cancel = ctx.cancel;
        let failed = |e: String| if cancel.load(Ordering::Relaxed) { Stop::Cancelled } else { Stop::Failed(e) };
        if gpu {
            let lin = ctx.time(photo, "base_gpu", "exact base, GPU", "GPU", || {
                let _gate = pools::gate::SPP_BASE.lock();
                crate::jobs::spp_pool().install(|| develop::spp::working_key(path, &key, true, cancel))
            })?;
            // only the time counts: one base at a time in memory, the processor base below is kept
            drop(lin.map_err(failed)?);
            if fam == Family::Merrill {
                let used = develop::spp::last_used_gpu();
                photo.stage("base_gpu", "").path = if used { "GPU".into() } else { "GPU→CPU".into() };
            }
        }
        let lin = {
            let _off = GpuOff::new();
            if prepared {
                // the processor run starts from the same quick preparation as after opening, computed on the processor
                develop::quattro::forget(path);
                let _ = develop::quattro::prepared_quick(path);
            }
            let lin = ctx.time(photo, "base_cpu", "exact base, processor", "CPU", || {
                let _gate = pools::gate::SPP_BASE.lock();
                crate::jobs::spp_pool().install(|| develop::spp::working_key(path, &key, false, cancel))
            })?;
            lin.map_err(failed)?
        };
        let base = Arc::new(develop::session::SppBase::new(key.clone(), lin));
        let matrix = base.matrix(&cal);
        ctx.time(photo, "fitted", "fitted view from the exact base (2400 px + output)", "CPU", || drop(Preview::from_spp(&base, matrix, crate::jobs::PREVIEW_EDGE).render(&cal, &s)))?;
        ctx.time(photo, "full", "full resolution (100 %) from the exact base", "CPU", || drop(develop::session::full_from_base(&cal, &base, &s)))?;
        session.install_spp_base(base);
        let out = ctx.tmp.join(format!("measure-{}.jpg", photo.index));
        let r = ctx.time(photo, "export_jpeg", "export JPEG (quality 90, sRGB)", "CPU", || {
            develop::export::color::export(&mut session, &s, develop::export::Format::Jpeg { quality: 90 }, develop::export::Engine::Spp, &out, graineo_color::Space::Srgb, &develop::export::Options::default())
        })?;
        let _ = std::fs::remove_file(&out);
        r.map_err(|e| Stop::Failed(e.to_string()))?;
        if prepared {
            develop::quattro::forget(path);
        }
    }
    Ok(())
}

/// Other formats: opening with the binned preview, the first picture, full resolution on the GPU and the processor,
/// the JPEG export.
fn measure_other(ctx: &mut Ctx, photo: &mut Photo, path: &Path, gpu: bool) -> Result<(), Stop> {
    let s = Settings::default();
    for run in 0..RUNS {
        ctx.run = run;
        ctx.time(photo, "read", "read file", "disk", || std::fs::read(path).map(|v| v.len())).and_then(|r| r.map_err(|e| Stop::Failed(e.to_string())))?;
        let cancel = ctx.cancel;
        let keep_going = || !cancel.load(Ordering::Relaxed);
        let doc = ctx.time(photo, "open", "open: decode, binned preview (2048 px)", "CPU", || rawdev::RawDoc::open(path, &keep_going))?;
        let doc = match doc {
            Ok(Some(d)) => d,
            Ok(None) => return Err(Stop::Cancelled),
            Err(e) => return Err(Stop::Failed(e)),
        };
        if run == 0 {
            photo.size = doc.full_size;
            photo.camera = format!("{} {}", doc.info.maker, doc.info.model).trim().to_string();
            photo.format = crate::timing_log::raw_family(&doc);
            photo.iso = doc.info.exif.iso;
        }
        ctx.time(photo, "first_detail", "first picture: detail stage (noise, sharpening, lens)", "CPU", || drop(doc.preview_stage(&s)))?;
        // finished images (JPEG, HEIC …) compute their output on the processor
        let gpu_out = gpu && !doc.finished && rawdev::gpu::gpu().is_some();
        ctx.time(photo, "first_render", "first picture: output", if gpu_out { "GPU" } else { "CPU" }, || drop(doc.render_preview(&s)))?;
        if gpu {
            let r = ctx.time(photo, "full_gpu", "full resolution incl. demosaic, GPU", "GPU", || doc.render_full(&s))?;
            r.map_err(Stop::Failed)?;
            // did a GPU stage run at all?
            let used = photo.details.iter().any(|(k, _)| k.starts_with("full_gpu: GPU") || k.starts_with("full_gpu: output: GPU"));
            if !used {
                photo.stage("full_gpu", "").path = "GPU→CPU".into();
            }
            doc.release_memory();
        }
        {
            let _off = GpuOff::new();
            let r = ctx.time(photo, "full_cpu", "full resolution incl. demosaic, processor", "CPU", || doc.render_full(&s))?;
            r.map_err(Stop::Failed)?;
        }
        doc.release_memory();
        let out = ctx.tmp.join(format!("measure-{}.jpg", photo.index));
        let r = ctx.time(photo, "export_jpeg", "export JPEG (quality 90, sRGB)", if gpu_out { "GPU" } else { "CPU" }, || {
            rawdev::export::export_color_opts(&doc, &s, develop::export::Format::Jpeg { quality: 90 }, &out, graineo_color::Space::Srgb, &develop::export::Options::default())
        })?;
        let _ = std::fs::remove_file(&out);
        r.map_err(Stop::Failed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside the measurement (caught by the caller) ends the sampler and the stage collector and removes the
    /// temporary folder.
    #[test]
    fn panic_ends_sampler_and_collector() {
        let tmp = std::env::temp_dir().join(format!("graineo-perf-panic-{}", std::process::id()));
        let stop = std::sync::Mutex::new(None);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _collecting = Collecting::start(tmp.clone());
            let sampler = Sampler::start();
            *stop.lock().unwrap() = Some(sampler.stop.clone());
            assert!(rawdev::timings::active() && tmp.is_dir());
            panic!("decoder failed");
        }));
        assert!(r.is_err());
        assert!(stop.lock().unwrap().as_ref().unwrap().load(Ordering::Relaxed), "sampler told to stop");
        assert!(!rawdev::timings::active(), "collector off");
        assert!(!tmp.exists(), "temporary folder removed");
    }

    #[test]
    fn one_photo_per_family_sigma_first() {
        let dir = std::env::temp_dir().join(format!("graineo-perf-choose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        // not real photos: only endings count (X3F needs a header, so none here)
        for n in ["b.CR3", "a.cr2", "c.RAF", "d.jpg", "e.heic", "f.txt", "sub/g.NEF", "sub/deeper/h.DNG", ".hidden.cr2"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let never = AtomicBool::new(false);
        let files = scan_folder(&dir, &never);
        assert_eq!(files.len(), 7, "{files:?}");
        let sel = choose(files, &never);
        let fams: Vec<Family> = sel.photos.iter().map(|(f, _)| *f).collect();
        assert_eq!(fams, vec![Family::XTrans, Family::Bayer, Family::Finished]);
        // the first in name order
        assert!(sel.photos[1].1.ends_with("a.cr2"), "{:?}", sel.photos);
        assert_eq!(sel.found, 7);
        // picking files one by one: at most ten are looked at
        let many: Vec<PathBuf> = (0..14).map(|i| dir.join(format!("p{i:02}.jpg"))).collect();
        let sel = choose_files(many, &never);
        assert_eq!((sel.skipped, sel.found, sel.photos.len()), (4, 10, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_choice_stops() {
        let cancel = AtomicBool::new(true);
        let sel = choose(vec![PathBuf::from("/x/a.cr2")], &cancel);
        assert!(sel.photos.is_empty());
    }

    #[test]
    fn switches_never_show_paths() {
        assert_eq!(switch_text("GRAINEO_DATA_DIR", "/Users/x/graineo-test"), "GRAINEO_DATA_DIR (set)");
        assert_eq!(switch_text("GRAINEO_DATA_DIR", r"C:\Users\x\test"), "GRAINEO_DATA_DIR (set)");
        assert_eq!(switch_text("GRAINEO_NO_PHOTOKIT", "1"), "GRAINEO_NO_PHOTOKIT=1");
        assert_eq!(switch_text("GRAINEO_LIBRARY_TEST", "search=Urlaub mit Anna"), "GRAINEO_LIBRARY_TEST (set)");
        assert_eq!(switch_text("GRAINEO_LIBRARY_TEST", "search=Anna"), "GRAINEO_LIBRARY_TEST (set)");
        assert_eq!(switch_text("GRAINEO_IMAGE", "IMG_1234.CR3"), "GRAINEO_IMAGE (set)");
        assert_eq!(switch_text("GRAINEO_QUIT", "2.5"), "GRAINEO_QUIT=2.5");
        assert_eq!(switch_text("GRAINEO_GPU", "off"), "GRAINEO_GPU=off");
    }

    #[test]
    fn stage_names_in_english() {
        assert_eq!(lap_name("CPU Demosaik"), "CPU demosaic");
        assert_eq!(lap_name("GPU Hochladen"), "GPU upload");
        assert_eq!(lap_name("Ausgabe: GPU gesamt"), "output: GPU total");
        assert_eq!(lap_name("kern foo"), "kern foo");
    }

    /// Cancelling between two stages: the next stage does not start and nothing is recorded for it.
    #[test]
    fn cancel_stops_before_the_next_stage() {
        let progress = Mutex::new(Progress::default());
        let cancel = AtomicBool::new(false);
        let mut ctx = Ctx { progress: &progress, cancel: &cancel, done: 0, total: 2, run: 0, tmp: std::env::temp_dir() };
        let mut photo = Photo::default();
        assert_eq!(ctx.time(&mut photo, "a", "stage a", "CPU", || 7).ok(), Some(7));
        cancel.store(true, Ordering::Relaxed);
        let mut ran = false;
        assert!(matches!(ctx.time(&mut photo, "b", "stage b", "CPU", || ran = true), Err(Stop::Cancelled)));
        assert!(!ran);
        assert_eq!(photo.stages.len(), 1);
        assert_eq!(photo.stages[0].ms.len(), 1);
        assert_eq!(progress.lock().unwrap().stage, "a");
    }
}
