//! "HDR zusammenfügen …" without a dialog: the selection is checked ([`refusal`]) and merged at once with the standard
//! settings; the app switches to Develop with the tool "Zusammenfügen" open, shows the reference with the progress and
//! puts the merge in its place as soon as it is there (`hdr_live.rs`). Everything else (reference, frames, motion,
//! output, areas) is changed afterwards in the tool (`hdr_paint.rs`); where the result goes and its keyword are
//! settings of the library (Settings → Library).
//!
//! Background jobs ([`rawdev::merged::run_job_with`]) hand their preview and full resolution to the app as events;
//! progress, time still to go and cancelling sit in the title bar ([`activities`], [`cancel_all`]) and in the tool.

use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::dialog;

/// One photo offered for merging (from the library).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceRow {
    pub file: i64,
    pub image: i64,
    pub path: PathBuf,
    pub name: String,
    pub format: String,
    pub camera: Option<String>,
    pub time: Option<f64>,
    pub iso: Option<i64>,
    pub aperture: Option<f64>,
    pub focal: Option<f64>,
    /// capture time in seconds (for finding brackets)
    pub capture: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

impl SourceRow {
    /// Exposure value from EXIF (log2 of `t·ISO/N²`), `None` without exposure time.
    pub(crate) fn exif_ev(&self) -> Option<f64> {
        let t = self.time.filter(|t| *t > 0.0)?;
        let iso = self.iso.filter(|v| *v > 0).unwrap_or(100) as f64;
        let n = self.aperture.filter(|a| *a > 0.0).unwrap_or(1.0);
        Some((t * iso / 100.0 / (n * n)).log2())
    }
}

/// What the user asked for with "Zusammenfügen".
#[derive(Clone, Debug)]
pub struct Request {
    pub rows: Vec<SourceRow>,
    pub settings: hdr::Settings,
    /// the result goes on top of the stack of its reference (otherwise: a photo of its own beside the sources)
    pub stack: bool,
    /// keyword of the result (empty: none)
    pub keyword: String,
}

/// Setting keys of the library (Settings → Library).
pub const KEY_STACK: &str = "hdrInStack";
pub const KEY_KEYWORD: &str = "hdrKeyword";
/// Keyword of a merged photo unless the user changed it.
pub const DEFAULT_KEYWORD: &str = "HDR";

/// Most photos a merge takes.
pub const MAX: usize = hdr::MAX_FRAMES;

/// Smallest spread of the exposures (EXIF) that is worth a merge.
pub const MIN_SPREAD_EV: f64 = 0.5;

/// Put the result on top of the stack of its sources (Settings → Library)?
pub fn stack_setting() -> bool {
    library::settings::value(KEY_STACK).as_bool().unwrap_or(true)
}

/// Keyword of the result (Settings → Library; empty: none).
pub fn keyword_setting() -> String {
    library::settings::value(KEY_KEYWORD).as_str().map(str::to_string).unwrap_or_else(|| DEFAULT_KEYWORD.to_string())
}

/// Why these photos cannot be merged (`None`: they can): number, kind, same camera and size, reachable, enough
/// difference in exposure.
pub fn refusal(rows: &[SourceRow]) -> Option<String> {
    // an SFD shot (X3I) holds its own bracket: merged when it is developed, never with other photos
    if let Some(r) = rows.iter().find(|r| r.format == "X3I") {
        return Some(crate::trf!("{a} ist eine SFD-Aufnahme: ihre Belichtungen fügt graineo beim Entwickeln selbst zusammen. Das Foto einzeln in Entwickeln öffnen.", a = r.name));
    }
    if rows.len() < hdr::MIN_FRAMES {
        return Some(crate::tr("Mindestens zwei Fotos einer Belichtungsreihe wählen.").to_string());
    }
    if rows.len() > MAX {
        return Some(crate::trf!("Höchstens {n} Fotos.", n = MAX));
    }
    // Foveon single frames (X3F) merge among themselves, never with mosaic RAWs (another sensor, other colour)
    let foveon = rows.iter().filter(|r| r.format == "X3F").count();
    if foveon > 0 && foveon < rows.len() {
        let r = rows.iter().find(|r| r.format == "X3F").unwrap_or(&rows[0]);
        return Some(crate::trf!("{a} ist ein Foveon-Foto (X3F): es lässt sich nur mit anderen X3F-Fotos derselben Kamera zusammenfügen.", a = r.name));
    }
    if let Some(r) = rows.iter().find(|r| !library::is_raw(&r.format)) {
        return Some(crate::trf!("{a}: nur RAW-Fotos (Bayer, X-Trans oder Foveon).", a = r.name));
    }
    let distinct = |v: Vec<String>| {
        let mut out: Vec<String> = Vec::new();
        for s in v {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        out
    };
    // only what is known is compared: a camera or size not read yet (metadata of a freshly added photo come in the
    // background) is no proof of a different camera – the merge itself checks the sensor data again
    let cameras = distinct(rows.iter().filter_map(|r| r.camera.clone()).collect());
    if cameras.len() > 1 {
        return Some(crate::trf!("Die Fotos stammen von verschiedenen Kameras: {a}.", a = cameras.join(", ")));
    }
    let sizes = distinct(rows.iter().filter_map(|r| Some(format!("{}×{}", r.width.filter(|w| *w > 0)?, r.height.filter(|h| *h > 0)?))).collect());
    if sizes.len() > 1 {
        return Some(crate::trf!("Die Fotos haben verschiedene Größen: {a}.", a = sizes.join(", ")));
    }
    let evs: Vec<f64> = rows.iter().filter_map(|r| r.exif_ev()).collect();
    if evs.len() == rows.len() {
        let spread = evs.iter().copied().fold(f64::MIN, f64::max) - evs.iter().copied().fold(f64::MAX, f64::min);
        if spread < MIN_SPREAD_EV {
            return Some(crate::tr("Die Fotos unterscheiden sich kaum in der Belichtung (weniger als 0,5 EV).").to_string());
        }
    }
    if let Some(r) = rows.iter().find(|r| !r.path.is_file()) {
        return Some(crate::trf!("{a} ist nicht erreichbar.", a = r.name));
    }
    None
}

/// The merge of these photos with the standard settings (no questions beforehand: everything can be changed in the
/// tool "Zusammenfügen" afterwards), or why it is not possible. Sorted dark → bright; the middle exposure is the
/// reference.
pub fn request(mut rows: Vec<SourceRow>) -> Result<Request, String> {
    rows.sort_by(|a, b| a.exif_ev().unwrap_or(0.0).total_cmp(&b.exif_ev().unwrap_or(0.0)).then(a.name.cmp(&b.name)));
    if let Some(why) = refusal(&rows) {
        return Err(why);
    }
    Ok(Request { rows, settings: hdr::Settings::new_merge(), stack: stack_setting(), keyword: keyword_setting() })
}

/// Threads of the merge: all cores in the mode "Genau" (a merge is work the user asked for and ends by itself; the
/// threads sleep afterwards), four in "Sparsam"; at utility priority either way, so the UI keeps its responsiveness.
pub(crate) fn merge_threads(saving: bool, cores: usize) -> usize {
    if saving {
        4.min(cores)
    } else {
        cores.clamp(4, 16)
    }
}

/// Memory the merge may use: more than half of the computer's memory in "Genau", about a third in "Sparsam"; a bigger
/// merge then works in bands (same result, a little slower).
pub(crate) fn merge_memory_budget(saving: bool, ram: u64) -> u64 {
    (ram as f64 * if saving { 0.35 } else { 0.55 }) as u64
}

/// Pool of the merge computations for the operating mode as it is now: asked at every use, so a change of the mode
/// counts from the next merge on (one pool per mode, built when first needed; idle threads sleep). The memory budget
/// follows the mode as well.
pub fn pool() -> &'static rayon::ThreadPool {
    static POOLS: [std::sync::OnceLock<rayon::ThreadPool>; 2] = [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
    let saving = crate::perf_mode::tier_key() == "saving";
    hdr::merge::set_memory_budget(merge_memory_budget(saving, pools::physical_memory().unwrap_or(16 << 30)));
    POOLS[saving as usize].get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(merge_threads(saving, cores))
            .thread_name(move |i| format!("hdr-{}{i}", if saving { "s" } else { "" }))
            .start_handler(|_| library::platform::set_priority(library::platform::Priority::Utility))
            .build()
            .expect("hdr pool")
    })
}

/// Text of a warning of the analysis.
pub fn warning_text(w: &hdr::Warning, names: &[String]) -> String {
    let name = |i: &usize| names.get(*i).cloned().unwrap_or_default();
    match w {
        hdr::Warning::ExifDeviation { dark, bright, ev } => crate::trf!("{a} → {b}: Helligkeit weicht {e} EV von den Aufnahmedaten ab – Licht verändert?", a = name(dark), b = name(bright), e = format!("{:+.1}", ev)),
        hdr::Warning::NotMeasured { dark, bright } => crate::trf!("{a} → {b}: zu wenig Überlappung, Belichtung aus den Aufnahmedaten", a = name(dark), b = name(bright)),
        hdr::Warning::ExposureUnknown { dark, bright } => crate::trf!("{a} → {b}: Belichtung unbekannt (keine Messung, keine Aufnahmedaten) – als gleich hell angenommen", a = name(dark), b = name(bright)),
        hdr::Warning::LargeShift { frame, px } => crate::trf!("{a} ist um {p} px verschoben – aus der Hand?", a = name(frame), p = format!("{:.0}", px)),
        hdr::Warning::AlignmentRejected { frame } => crate::trf!("{a}: Ausrichtung nicht plausibel – wie aufgenommen verwendet", a = name(frame)),
        hdr::Warning::ApertureDiffers => crate::tr("Verschiedene Blenden: Schärfentiefe und Vignette unterscheiden sich").to_string(),
        hdr::Warning::LargeGap { dark, bright, ev } => crate::trf!("{a} → {b}: großer Abstand ({e} EV)", a = name(dark), b = name(bright), e = format!("{:.1}", ev)),
    }
}

/// Signed EV label ("−2", "0", "+1,3").
pub fn ev_label(ev: f64) -> String {
    let r = (ev * 10.0).round() / 10.0;
    if r.abs() < 0.05 {
        return "0".into();
    }
    let places = if r.fract().abs() < 1e-6 { 0 } else { 1 };
    let v = crate::menu::i18n::format::decimal(crate::menu::i18n::language(), r.abs(), places);
    format!("{}{v}", if r < 0.0 { "−" } else { "+" })
}

// ─────────────────────────────────────────────── background job and its events

/// Something the app has to take over from a merge job (on the UI thread).
pub enum Event {
    /// developed preview of a merged photo for the grid
    /// (`key`: the result it shows)
    Thumbnail { file: i64, key: String, img: image::RgbImage },
    /// finished (text for the title bar); `created`: a new merge (not a change of one)
    Done { text: String, file: i64, created: bool },
    /// new pixels of a merged photo (result `key`): the preview first, then the full resolution; the open photo takes
    /// them in place (`hdr_live.rs`)
    Pixels { file: i64, key: String, preview: Option<Arc<hdr::Merged>>, full: Option<Arc<hdr::Merged>>, meta: rawdev::merged::Meta },
    /// a merged photo was created in the library just now (nothing kept by its path or file belongs to it)
    New { file: i64 },
    /// failed or cancelled; `removed`: the merged photo was taken out of the library again; `ticket`: the number of
    /// the change this job computed (a newer change of the photo is not touched by this end)
    Failed { text: String, removed: bool, file: i64, ticket: u64 },
    /// the automatic tone of a merged photo nobody looks at was set in the library (`hdr_auto::tune_unseen`): the
    /// five sliders it set
    Tuned { file: i64, tone: [f32; 5] },
    /// a batch ("Alle Reihen zusammenfügen") ended – however: `removed`: merged photos were taken out of the library
    /// again; `cancelled`: it was stopped
    BatchEnded { removed: bool, cancelled: bool },
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// Events of the merge jobs since the last call.
pub fn take_events() -> Vec<Event> {
    std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()))
}

pub(crate) fn push(e: Event) {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push(e);
}

/// Lines for the title bar: "HDR IMG_2-HDR 45 % · noch etwa 9 s".
pub fn activities() -> Vec<String> {
    rawdev::merged::jobs()
        .iter()
        .map(|j| {
            let p = (j.overall() * 100.0).floor() as u32;
            if let Some((k, n)) = crate::hdr_batch::position(j.file) {
                return crate::trf!("HDR Reihe {k} von {n} · {a} {p} %", k = k, n = n, a = j.name, p = p);
            }
            match j.remaining_secs() {
                Some(r) => crate::trf!("HDR {a} {p} % · noch etwa {t}", a = j.name, p = p, t = dialog::duration(std::time::Duration::from_secs_f32(r.max(1.0)))),
                None => crate::trf!("HDR {a} {p} %", a = j.name, p = p),
            }
        })
        .collect()
}

/// A merge batch moved on (redraw the title bar).
pub(crate) fn mark_changed() {
    rawdev::merged::touch();
}

/// Cancel every merge: the batch, the running job, the jobs waiting for their turn (they leave at once) and the
/// merges computed for an output.
pub fn cancel_all() {
    crate::hdr_batch::cancel();
    let files: Vec<i64> = FLAGS.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|f| f.0).collect();
    for f in files.into_iter().chain(rawdev::merged::jobs().iter().map(|j| j.file)) {
        cancel(f);
    }
    for f in OUTPUTS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        f.1.store(true, Ordering::SeqCst);
    }
    turn::wake();
}

/// The one entrance for merges: one computation at a time, in the order they asked (a batch asks again for each of
/// its brackets, so a change the user makes meanwhile comes before the next bracket). A job told to stop while it
/// waits leaves at once. Two merges never share the pool (a worker waiting for its own parallel work would pick up
/// the other merge, which then waits for a cache lock its own thread holds – both hang) nor the memory budget.
pub(crate) mod turn {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

    struct State {
        running: bool,
        next: u64,
        /// (order of arrival, file)
        waiting: Vec<(u64, i64)>,
    }

    static STATE: Mutex<State> = Mutex::new(State { running: false, next: 0, waiting: Vec::new() });
    static MOVED: Condvar = Condvar::new();

    /// The turn of one merge; the next one goes on when it is dropped (also by a panic).
    pub(crate) struct Turn(());

    impl Drop for Turn {
        fn drop(&mut self) {
            STATE.lock().unwrap_or_else(|e| e.into_inner()).running = false;
            MOVED.notify_all();
        }
    }

    /// Wait for the turn of a merge of `file`. `None`: `cancel` was set while waiting (the job is out of the queue).
    pub(crate) fn take(file: i64, cancel: &AtomicBool) -> Option<Turn> {
        // (waiting jobs count as busy where cache files are removed)
        let _busy = rawdev::merged::waiting(file);
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let seq = g.next;
        g.next += 1;
        g.waiting.push((seq, file));
        loop {
            if cancel.load(Ordering::SeqCst) {
                g.waiting.retain(|w| w.0 != seq);
                drop(g);
                MOVED.notify_all();
                return None;
            }
            if !g.running && g.waiting.iter().map(|w| w.0).min() == Some(seq) {
                g.waiting.retain(|w| w.0 != seq);
                g.running = true;
                return Some(Turn(()));
            }
            g = MOVED.wait_timeout(g, std::time::Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Does a job of `file` wait for its turn (another merge is computed first)?
    pub(crate) fn waiting(file: i64) -> bool {
        STATE.lock().unwrap_or_else(|e| e.into_inner()).waiting.iter().any(|w| w.1 == file)
    }

    /// Jobs waiting for their turn.
    #[cfg(test)]
    pub(crate) fn queue() -> Vec<i64> {
        let g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let mut w = g.waiting.clone();
        w.sort_unstable();
        w.into_iter().map(|w| w.1).collect()
    }

    /// Waiting jobs look at their cancel flags now.
    pub(crate) fn wake() {
        MOVED.notify_all();
    }
}

/// Does a job of the merged photo `file` wait behind another merge (shown as "wartet …")?
pub(crate) fn waits(file: i64) -> bool {
    job_alive(file) && turn::waiting(file)
}

/// Cancel flags of the merges computed for an output now (export, print, full resolution on zooming in): (file, flag).
static OUTPUTS: Mutex<Vec<(i64, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

/// The app's entrance for merges that an output needs ([`rawdev::merged::compute_output`]): they take their turn
/// like every job, run in the merge pool with its memory budget, show in the title bar and can be cancelled there.
pub(crate) fn install() {
    rawdev::merged::set_output_runner(|src, asked| {
        // "Abbrechen" holds: what the app needs by itself (the sharp picture of the photo shown, previews) does not
        // merge a result the user stopped or that failed – only an output the user asks for does, or "Neu berechnen"
        if !asked && stopped(src.file, &src.key) {
            return Err(crate::tr(STOPPED).to_string());
        }
        let flag = Arc::new(AtomicBool::new(false));
        OUTPUTS.lock().unwrap_or_else(|e| e.into_inner()).push((src.file, flag.clone()));
        struct Listed(Arc<AtomicBool>);
        impl Drop for Listed {
            fn drop(&mut self) {
                OUTPUTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| !Arc::ptr_eq(&f.1, &self.0));
            }
        }
        let _listed = Listed(flag.clone());
        let name = library::db::Db::standard()
            .ok()
            .and_then(|db| library::merge::file_path(&db.conn, src.file))
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_default();
        let Some(turn) = turn::take(src.file, &flag) else { return Err(hdr::Error::Cancelled.to_string()) };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pool().install(|| rawdev::merged::run_job_flag(src, &name, flag.clone(), &mut |_, _| {}, &mut |_, _| {}))));
        drop(turn);
        let r = r.unwrap_or_else(|_| Err(crate::tr(INTERNAL).to_string()));
        if r.is_ok() {
            // the result is there now: it does not count as cancelled or failed any more (the tool says "Aktuell")
            FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| !(f.0 == src.file && f.1 == src.key));
        }
        r
    });
}

/// Error of a full resolution that is not merged by itself because the user cancelled its computation (or it failed).
const STOPPED: &str = "HDR: die volle Auflösung wurde nicht berechnet (abgebrochen) – „Neu berechnen“ im Werkzeug „Zusammenfügen“";

/// Text of a merge that ended by an internal error (a panic in the computation).
const INTERNAL: &str = "Interner Fehler beim Zusammenfügen";

/// Has a merge job changed since the last call (redraw the title bar)?
pub fn changed() -> bool {
    static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let g = rawdev::merged::generation();
    SEEN.swap(g, Ordering::Relaxed) != g
}

/// The result each merged photo should show now, with the number of the change that asked for it (the newest change
/// wins; older jobs are cancelled and their pixels ignored).
static WANTED: Mutex<Vec<(u64, rawdev::merged::MergedSource)>> = Mutex::new(Vec::new());

/// Numbers of the changes, in the order the user made them (taken on the UI thread, see [`ticket`]).
static TICKET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The cancel flags of the jobs that were started and have not ended: (file, number of its change, flag). A job is
/// listed before its thread starts, so "cancel" reaches it at any time.
static FLAGS: Mutex<Vec<(i64, u64, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

/// Results that were not computed: (file, key, reason; `None`: the user cancelled). Such a result is not started
/// again by itself (the tool says why and offers "Neu berechnen"); a new change or request of the user clears it.
static FAILED: Mutex<Vec<(i64, String, Option<String>)>> = Mutex::new(Vec::new());

/// The number of a change the user makes now. Changes are stored and looked up in the background and may arrive in
/// another order: the one with the highest number counts.
pub(crate) fn ticket() -> u64 {
    TICKET.fetch_add(1, Ordering::SeqCst)
}

pub(crate) fn wanted(file: i64) -> Option<rawdev::merged::MergedSource> {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|s| s.1.file == file).map(|s| s.1.clone())
}

fn wanted_ticket(file: i64) -> Option<u64> {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|s| s.1.file == file).map(|s| s.0)
}

/// This change takes over its photo: it is the result wanted, older jobs of the photo stop. `None` when a newer change
/// took over already (this one arrived late and is dropped).
fn take_over(src: &rawdev::merged::MergedSource, ticket: u64) -> Option<Arc<AtomicBool>> {
    let mut w = WANTED.lock().unwrap_or_else(|e| e.into_inner());
    if w.iter().any(|s| s.1.file == src.file && s.0 > ticket) {
        return None;
    }
    w.retain(|s| s.1.file != src.file);
    w.push((ticket, src.clone()));
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| f.0 != src.file);
    SOURCE_GONE.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| f.0 != src.file);
    let flag = Arc::new(AtomicBool::new(false));
    let mut f = FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    for old in f.iter().filter(|o| o.0 == src.file) {
        old.2.store(true, Ordering::SeqCst);
    }
    f.push((src.file, ticket, flag.clone()));
    CANCELLING.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| *c != src.file);
    crate::hdr_paint::change_arrived();
    Some(flag)
}

/// Nothing is wanted for this photo any more (its job failed).
pub(crate) fn unwant(file: i64) {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.1.file != file);
}

/// The job of the change `ticket` ended without a result: its result is not wanted any more – a newer change of the
/// photo (another ticket) stays wanted.
pub(crate) fn unwant_change(file: i64, ticket: u64) {
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| !(s.1.file == file && s.0 == ticket));
}

/// Cancel every job of the merged photo `file` – also one whose thread has not started computing yet.
pub(crate) fn cancel(file: i64) {
    CANCELLING.lock().unwrap_or_else(|e| e.into_inner()).push(file);
    for f in FLAGS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|f| f.0 == file) {
        f.2.store(true, Ordering::SeqCst);
    }
    rawdev::merged::cancel(file);
    turn::wake();
}

/// Photos whose jobs were cancelled and are still winding down (their thread notices the flag at its next step).
static CANCELLING: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// Was the merge of `file` cancelled and is its job still ending? The panel says so at once instead of the progress
/// of a job that only goes on until it notices (QA 10, N-A8). Forgotten once no job of the photo is alive.
pub(crate) fn cancelling(file: i64) -> bool {
    let alive = job_alive(file) || rawdev::merged::jobs().iter().any(|j| j.file == file);
    let mut c = CANCELLING.lock().unwrap_or_else(|e| e.into_inner());
    if !alive {
        c.retain(|f| *f != file);
        return false;
    }
    c.contains(&file)
}

/// Jobs (by the number of their change) that must leave their photo in the library whatever happens to them.
static KEEP: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Cancel every job of `file` – and none of them takes the photo out of the library (the caller removes it itself, as
/// a step ⌘Z can take back: a creating job that ended after that ⌘Z took the photo out for good).
pub(crate) fn cancel_and_keep(file: i64) {
    let tickets: Vec<u64> = FLAGS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|f| f.0 == file).map(|f| f.1).collect();
    KEEP.lock().unwrap_or_else(|e| e.into_inner()).extend(tickets);
    cancel(file);
    unwant(file);
}

/// Is a job of this photo started and not ended (computing, or about to)?
pub(crate) fn job_alive(file: i64) -> bool {
    FLAGS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|f| f.0 == file)
}

/// Why the result `key` of `file` could not be computed (`None`: it was not tried, or a new request cleared it).
pub(crate) fn failed_reason(file: i64, key: &str) -> Option<String> {
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|f| f.0 == file && f.1 == key).and_then(|f| f.2.clone())
}

/// Was the result `key` of `file` cancelled by the user or did it fail (it is not started again by itself)?
pub(crate) fn stopped(file: i64, key: &str) -> bool {
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|f| f.0 == file && f.1 == key)
}

/// Results that were not computed because a source file was not there: (file, key). Tried again by itself once the
/// sources are back ([`sources_back`]).
static SOURCE_GONE: Mutex<Vec<(i64, String)>> = Mutex::new(Vec::new());

/// The sources of the merged photo `file` are all there (again): results that failed for a missing source do not
/// count as failed any more – the photo is computed again by itself. `true` when there was such a result.
pub(crate) fn sources_back(file: i64) -> bool {
    let keys: Vec<String> = {
        let mut g = SOURCE_GONE.lock().unwrap_or_else(|e| e.into_inner());
        let keys = g.iter().filter(|f| f.0 == file).map(|f| f.1.clone()).collect();
        g.retain(|f| f.0 != file);
        keys
    };
    if keys.is_empty() {
        return false;
    }
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| !(f.0 == file && keys.contains(&f.1)));
    true
}

/// Everything this session remembers by the file of a merged photo is forgotten (the photo is new, or gone): what is
/// wanted, what failed, the automatic tone, the tool's recipe.
pub(crate) fn forget_session(file: i64) {
    unwant(file);
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| f.0 != file);
    SOURCE_GONE.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| f.0 != file);
    crate::hdr_auto::forget(file);
}

/// The merged photo `file` is gone for good: its cached results, its grid previews and what this session remembers
/// of it go (nothing of it may meet another photo that gets its file id later).
pub(crate) fn forget_file(file: i64) {
    // (looking at a backup: the photo is gone from the copy only – cache and previews belong to the real library)
    if rawdev::merged::other_library() {
        forget_session(file);
        return;
    }
    rawdev::merged::forget_file(file);
    library::preview::remove_all_developed(file);
    forget_session(file);
}

/// Merged photos whose first computation (the job that created them) is running.
static CREATING: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// Is the job that created the merged photo `file` still running? Cancelling it takes the photo out of the library
/// again (unless it shows a picture already or carries work); cancelling a later change does not.
pub(crate) fn creating(file: i64) -> bool {
    CREATING.lock().unwrap_or_else(|e| e.into_inner()).contains(&file)
}

/// What a merge job that ended without a result does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterFailure {
    /// a newer change of the same photo took over (it cancelled this job): nothing to say, nothing to remove
    Superseded,
    /// a change failed or was cancelled: said, the photo stays
    Report,
    /// the photo could not be made (failed, or cancelled by the user): out of the library again
    Discard,
}

/// `update`: the job computed a change of an existing photo; `superseded`: the photo wants the result of another
/// change by now – also for the job that created the photo: a setting changed while a new merge was still computing
/// must not remove the photo. `keep`: the photo shows a picture already or carries work of the user (a development,
/// areas): it is never taken out by a job, whatever happened to the computation.
pub(crate) fn after_failure(update: bool, superseded: bool, keep: bool) -> AfterFailure {
    match (superseded, update || keep) {
        (true, _) => AfterFailure::Superseded,
        (false, true) => AfterFailure::Report,
        (false, false) => AfterFailure::Discard,
    }
}

/// How a merge job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobEnd {
    Computed,
    /// a newer change computes the photo (it stays in the library)
    Superseded,
    Failed,
}

/// Compute a merged photo just created in the library (background thread). On cancel or failure the photo is taken
/// out of the library again (`undo` restores the stacks as they were) – unless it shows a picture or carries work.
pub fn run(src: rawdev::merged::MergedSource, name: String, undo: Option<library::series::Change>) {
    let ticket = ticket();
    let Some(flag) = take_over(&src, ticket) else { return };
    spawn(move || {
        job_as(&src, &name, undo.as_ref(), false, ticket, flag);
    });
}

/// Compute a merged photo for changed settings (frames, reference, motion, output, areas): a job of the same photo
/// still running is cancelled first; on failure or cancel the photo stays as it was.
pub fn run_update(src: rawdev::merged::MergedSource, name: String) {
    run_update_as(src, name, ticket());
}

/// [`run_update`] for the change with the number `ticket` ([`ticket`], taken when the user made it): dropped when a
/// newer change of the photo took over meanwhile.
pub fn run_update_as(src: rawdev::merged::MergedSource, name: String, ticket: u64) {
    let Some(flag) = take_over(&src, ticket) else { return };
    spawn(move || {
        job_as(&src, &name, None, true, ticket, flag);
    });
}

/// Take a merged photo that could not be computed (or was cancelled) out of the library again; `true` when it is gone.
pub(crate) fn discard(src: &rawdev::merged::MergedSource, undo: Option<&library::series::Change>) -> bool {
    let removed = library::db::Db::standard()
        .map_err(|e| e.to_string())
        .and_then(|db| {
            // the stack back and the photo out in one transaction (nothing of its own to keep: it never showed)
            library::merge::undo_create(&db, &[(src.file, undo.cloned())]).map(|_| ())
        })
        .is_ok();
    if removed {
        forget_file(src.file);
    } else {
        rawdev::merged::forget_file(src.file);
    }
    removed
}

/// Does the merged photo carry work of the user (areas, a development by hand, virtual copies)? Asked by a job that
/// ended without a result (background thread, a connection of its own).
pub(crate) fn carries_work(file: i64) -> bool {
    library::db::Db::standard().is_ok_and(|db| crate::library_ui::library_hdr::has_own_work(&db, file))
}

/// Text of the event for a failed or cancelled merge.
fn failure_text(e: &str) -> String {
    if e == hdr::Error::Cancelled.to_string() {
        crate::tr("HDR abgebrochen").to_string()
    } else {
        crate::trf!("HDR nicht möglich: {e}", e = reason_text(e))
    }
}

/// The reason a merge failed in the language of the interface: the texts of the merge itself (`hdr::Error`, English
/// developer texts) by their form, everything else through the dictionary of messages.
pub(crate) fn reason_text(e: &str) -> String {
    let count = |tail: &str| e.strip_suffix(tail).and_then(|h| h.split_once(" frames: ")).and_then(|(n, _)| n.parse::<usize>().ok());
    if let Some(name) = e.strip_suffix(": other camera or sensor size") {
        return crate::trf!("{a}: andere Kamera oder Sensorgröße", a = name);
    }
    if let Some(name) = e.strip_suffix(": other orientation (portrait and landscape mixed)") {
        return crate::trf!("{a}: andere Ausrichtung – Hoch- und Querformat lassen sich nicht zusammenfügen", a = name);
    }
    if let Some(n) = count(" needed").filter(|_| e.contains(" frames: at least ")) {
        return crate::trf!("{n} Aufnahmen: mindestens {m} nötig", n = n, m = hdr::MIN_FRAMES);
    }
    if let Some(n) = count(" supported").filter(|_| e.contains(" frames: at most ")) {
        return crate::trf!("{n} Aufnahmen: höchstens {m} möglich", n = n, m = hdr::MAX_FRAMES);
    }
    crate::menu::i18n::tr_msg(e).to_string()
}

/// The merge job itself (blocking; the caller provides the thread): a job of a batch, which nobody asked for by a
/// change (nothing is "wanted" for it).
pub(crate) fn job(src: &rawdev::merged::MergedSource, name: &str, undo: Option<&library::series::Change>, update: bool) -> JobEnd {
    let ticket = ticket();
    let flag = Arc::new(AtomicBool::new(false));
    FLAGS.lock().unwrap_or_else(|e| e.into_inner()).push((src.file, ticket, flag.clone()));
    CANCELLING.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| *c != src.file);
    UNSEEN.lock().unwrap_or_else(|e| e.into_inner()).push(ticket);
    let end = job_as(src, name, undo, update, ticket, flag);
    UNSEEN.lock().unwrap_or_else(|e| e.into_inner()).retain(|t| *t != ticket);
    if end == JobEnd::Computed {
        // nobody looks at this photo: its automatic tone is set here (when it is opened, by the tool), then its tile
        // is rendered – tile and export show what Develop will show
        let tuned = crate::hdr_auto::tune_unseen(src.file);
        let settings = tuned.unwrap_or_else(|| develop_settings(src.file));
        if let (Some(meta), Some(pv)) = (rawdev::merged::read_meta(&src.key), rawdev::merged::read_preview(&src.key)) {
            if let Ok(img) = pool().install(|| rawdev::merged::render_preview_with(&src.reference(), &pv, &meta, &settings)) {
                if let Some(rgb) = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()) {
                    push(Event::Thumbnail { file: src.file, key: src.key.clone(), img: rgb });
                }
            }
        }
    }
    end
}

/// Jobs (by the number of their change) of photos nobody looks at (a batch): their tile is rendered when the job is
/// done and the automatic tone is set, not from the first preview.
static UNSEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// The develop settings the grid preview of a merged photo is rendered with: the photo's own (the standard
/// development while it has none).
fn develop_settings(file: i64) -> crate::Settings {
    library::db::Db::standard()
        .ok()
        .and_then(|db| library::merge::file_path(&db.conn, file))
        .map(|p| crate::raw_app::load_settings(&p, None).0)
        .unwrap_or_default()
}

fn job_as(src: &rawdev::merged::MergedSource, name: &str, undo: Option<&library::series::Change>, update: bool, ticket: u64, flag: Arc<AtomicBool>) -> JobEnd {
    let t = std::time::Instant::now();
    let file = src.file;
    if !update {
        CREATING.lock().unwrap_or_else(|e| e.into_inner()).push(file);
    }
    // Whatever happens to this thread (a panic in the code below as well): the photo does not count as "being
    // created" and the job not as alive for ever – both would lock the tool until the app is started again.
    struct Alive {
        file: i64,
        ticket: u64,
        update: bool,
    }
    impl Drop for Alive {
        fn drop(&mut self) {
            if !self.update {
                CREATING.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| *f != self.file);
            }
            FLAGS.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| !(f.0 == self.file && f.1 == self.ticket));
        }
    }
    let alive = Alive { file, ticket, update };
    let reference = src.reference();
    let key = src.key.clone();
    #[cfg(test)]
    let session = test_session();
    #[cfg(test)]
    let gate_id = gate::enter(file, &key, update);
    #[cfg(test)]
    gate::at(gate_id, gate::Point::Start);
    // one merge at a time, in the order asked for ([`turn`]); a job told to stop while it waits ends here
    let r = match turn::take(file, &flag) {
        None => Err(hdr::Error::Cancelled.to_string()),
        Some(turn) => {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool().install(|| {
                    rawdev::merged::run_job_flag(
                        src,
                        name,
                        flag.clone(),
                        &mut |pv, meta| {
                            #[cfg(test)]
                            gate::at(gate_id, gate::Point::Preview);
                            let pv = Arc::new(hdr::Merged { px: pv.px.clone(), w: pv.w, h: pv.h, noise: pv.noise });
                            push(Event::Pixels { file, key: key.clone(), preview: Some(pv.clone()), full: None, meta: meta.clone() });
                            // the grid shows the preview right away (rendered beside the job, it goes on meanwhile) –
                            // not for a photo of a batch: its tile follows with the automatic tone ([`job`])
                            if UNSEEN.lock().unwrap_or_else(|e| e.into_inner()).contains(&ticket) {
                                return;
                            }
                            let (meta, reference, key) = (meta.clone(), reference.clone(), key.clone());
                            std::thread::spawn(move || {
                                let settings = develop_settings(file);
                                if let Ok(img) = pool().install(|| rawdev::merged::render_preview_with(&reference, &pv, &meta, &settings)) {
                                    if let Some(rgb) = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()) {
                                        push(Event::Thumbnail { file, key, img: rgb });
                                    }
                                }
                            });
                        },
                        &mut |full, meta| {
                            #[cfg(test)]
                            gate::at(gate_id, gate::Point::Full);
                            let full = Arc::new(hdr::Merged { px: full.px.clone(), w: full.w, h: full.h, noise: full.noise });
                            push(Event::Pixels { file, key: key.clone(), preview: None, full: Some(full), meta: meta.clone() });
                        },
                    )
                })
            }));
            drop(turn);
            // a panic in the computation is a failed merge, not a job that runs for ever
            r.unwrap_or_else(|p| {
                let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                crate::log(&format!("hdr: panic in the merge of {name}: {why}"));
                Err(crate::tr(INTERNAL).to_string())
            })
        }
    };
    #[cfg(test)]
    gate::at(gate_id, gate::Point::End);
    // Test: the session this job belongs to has ended (the app was "quit" while it ran): as for a process that is
    // gone, nothing of its end reaches the library or the next session.
    #[cfg(test)]
    if session != test_session() {
        std::mem::forget(alive);
        gate::leave(gate_id, JobEnd::Failed);
        return JobEnd::Failed;
    }
    // decided before the photo stops counting as "being created" (⌘Z looks at both)
    let kept = {
        let mut k = KEEP.lock().unwrap_or_else(|e| e.into_inner());
        let n = k.len();
        k.retain(|t| *t != ticket);
        k.len() != n
    };
    let superseded = kept || wanted_ticket(file).is_some_and(|w| w != ticket);
    let end = match r {
        Ok(_) => {
            // the first result of a photo is there: it is not "unfinished" any more (see `library_hdr::settle_unfinished`)
            crate::library_ui::library_hdr::finished(file);
            if !update {
                // (no longer "being created" before the app hears that it is done: ⌘Z then takes the ordinary way)
                CREATING.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| *f != file);
                push(Event::Done { text: crate::trf!("HDR fertig · {s} s", s = t.elapsed().as_secs_f32().round().max(1.0) as u32), file, created: !crate::hdr_batch::pending(file) });
            }
            JobEnd::Computed
        }
        Err(e) => {
            let cancelled = e == hdr::Error::Cancelled.to_string();
            // a new photo that shows its picture already (the preview is there, only the full resolution was stopped)
            // or carries work stays: "cancel" stops a computation, it never takes work away
            let keep = !update && !superseded && ((cancelled && rawdev::merged::preview_cached(&src.key)) || carries_work(file));
            match after_failure(update, superseded, keep) {
                // a newer change took over: nothing to say, the photo stays
                AfterFailure::Superseded => JobEnd::Superseded,
                AfterFailure::Report => {
                    // (no longer "being created" before the app hears of it: a ⌘Z waiting for this job goes on)
                    if !update {
                        CREATING.lock().unwrap_or_else(|e| e.into_inner()).retain(|f| *f != file);
                    }
                    FAILED.lock().unwrap_or_else(|e| e.into_inner()).push((file, src.key.clone(), (!cancelled).then(|| reason_text(&e))));
                    if e.starts_with(rawdev::merged::MISSING) {
                        SOURCE_GONE.lock().unwrap_or_else(|e| e.into_inner()).push((file, src.key.clone()));
                    }
                    push(Event::Failed { text: failure_text(&e), removed: false, file, ticket });
                    JobEnd::Failed
                }
                AfterFailure::Discard => {
                    let removed = discard(src, undo);
                    push(Event::Failed { text: failure_text(&e), removed, file, ticket });
                    JobEnd::Failed
                }
            }
        }
    };
    drop(alive);
    #[cfg(test)]
    gate::leave(gate_id, end);
    end
}

/// Test: the number of the session (counted up by [`test_new_session`]); jobs of an earlier session end without a
/// trace, as if their process had gone.
#[cfg(test)]
static SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
pub(crate) fn test_session() -> u64 {
    SESSION.load(Ordering::SeqCst)
}

/// Test: the app is "quit" while jobs run – they are told to stop and belong to no session any more.
#[cfg(test)]
pub(crate) fn test_end_session() {
    SESSION.fetch_add(1, Ordering::SeqCst);
    crate::hdr_batch::cancel();
    for f in FLAGS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        f.2.store(true, Ordering::SeqCst);
    }
    for j in rawdev::merged::jobs() {
        j.cancel.store(true, Ordering::SeqCst);
    }
    for f in OUTPUTS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        f.1.store(true, Ordering::SeqCst);
    }
    turn::wake();
}

/// Test: a new session – nothing wanted, nothing being created, no events, no question (no job may be alive).
#[cfg(test)]
pub(crate) fn test_new_session() {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    *LAST_ASKED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    WANTED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    FLAGS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    KEEP.lock().unwrap_or_else(|e| e.into_inner()).clear();
    SESSION.fetch_add(1, Ordering::SeqCst);
    FAILED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    SOURCE_GONE.lock().unwrap_or_else(|e| e.into_inner()).clear();
    CREATING.lock().unwrap_or_else(|e| e.into_inner()).clear();
    *ASKED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    OPEN_TOOL.store(false, Ordering::Relaxed);
    CHECKED.store(false, Ordering::Relaxed);
    OUTPUTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Test: close the open question without an answer.
#[cfg(test)]
pub(crate) fn test_new_question() {
    *QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Test: the question open now (its debug text).
#[cfg(test)]
pub(crate) fn test_question() -> Option<String> {
    QUESTION.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|q| format!("{q:?}").chars().take(60).collect())
}

/// A helper thread of the merge tool (a job, the lookup before it, a document being built).
pub(crate) fn spawn(f: impl FnOnce() + Send + 'static) {
    #[cfg(test)]
    let f = gate::counted(f);
    std::thread::spawn(f);
}

/// Time of the tool's short-lived memories (debounce of the menu command, the recipe read twice a second, "a change
/// went out just now"). The real time; a test may put its own thread on a clock it moves itself.
pub(crate) mod clock {
    use std::time::{Duration, Instant};

    #[cfg(test)]
    thread_local! {
        static VIRTUAL: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    }

    pub(crate) fn now() -> Instant {
        #[cfg(test)]
        if let Some(t) = VIRTUAL.with(|v| v.get()) {
            return t;
        }
        Instant::now()
    }

    /// Time since `t` on this clock.
    pub(crate) fn since(t: Instant) -> Duration {
        now().saturating_duration_since(t)
    }

    /// Test: this thread's clock stands still from now on and moves only with [`advance`] (`false`: real time again).
    #[cfg(test)]
    pub(crate) fn set_virtual(on: bool) {
        VIRTUAL.with(|v| v.set(on.then(Instant::now)));
    }

    #[cfg(test)]
    pub(crate) fn advance(by: Duration) {
        VIRTUAL.with(|v| v.set(v.get().map(|t| t + by)));
    }
}

/// Test hook: merge jobs stop at fixed points until the test lets them go on, so that a test decides what happens
/// while a job is at which point (`hdr_stress_test.rs`). Off unless a test switches it on.
#[cfg(test)]
pub(crate) mod gate {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};

    /// Where a job waits.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(crate) enum Point {
        /// counted as being created and wanted, not registered as running yet
        Start,
        /// the preview is computed, not handed to the app yet
        Preview,
        /// the full resolution is computed, neither handed over nor stored yet
        Full,
        /// the computation ended (no longer listed as running), its outcome is not decided yet
        End,
    }

    /// A job between [`enter`] and [`leave`].
    #[derive(Debug, Clone, PartialEq)]
    pub(crate) struct Job {
        pub id: u64,
        pub file: i64,
        pub key: String,
        pub update: bool,
        /// the point it waits at (`None`: it runs)
        pub at: Option<Point>,
        /// it may pass every point from now on
        free: bool,
        /// points it may pass
        permits: usize,
    }

    #[derive(Default)]
    struct State {
        on: bool,
        next: u64,
        jobs: Vec<Job>,
        /// (id, file, key, how it ended) of the jobs that ended, oldest first
        ended: Vec<(u64, i64, String, super::JobEnd)>,
        started: usize,
        /// the next job that reaches this point panics there (once)
        panic_at: Option<Point>,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);
    static MOVED: Condvar = Condvar::new();
    /// Helper threads of the tool running now ([`super::spawn`]).
    static THREADS: AtomicUsize = AtomicUsize::new(0);

    fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        f(g.get_or_insert_with(State::default))
    }

    /// Jobs stop at the points from now on (`false`: every job runs through, waiting ones go on).
    pub(crate) fn set(on: bool) {
        with(|s| {
            s.on = on;
            if !on {
                for j in &mut s.jobs {
                    j.free = true;
                }
            }
        });
        MOVED.notify_all();
    }

    /// Forget everything (a new test sequence; no job may be alive).
    pub(crate) fn reset() {
        with(|s| *s = State::default());
        MOVED.notify_all();
    }

    pub(super) fn enter(file: i64, key: &str, update: bool) -> u64 {
        with(|s| {
            s.next += 1;
            s.started += 1;
            let id = s.next;
            let free = !s.on;
            s.jobs.push(Job { id, file, key: key.to_string(), update, at: None, free, permits: 0 });
            id
        })
    }

    /// The next job that reaches `point` panics there (a bug in the computation; once).
    pub(crate) fn panic_at(point: Point) {
        with(|s| s.panic_at = Some(point));
    }

    pub(super) fn at(id: u64, point: Point) {
        if with(|s| s.panic_at == Some(point) && s.panic_at.take().is_some()) {
            panic!("test: panic in a merge at {point:?}");
        }
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let s = g.get_or_insert_with(State::default);
            let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) else { return };
            if j.free {
                j.at = None;
                return;
            }
            if j.permits > 0 {
                j.permits -= 1;
                j.at = None;
                return;
            }
            j.at = Some(point);
            g = MOVED.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub(super) fn leave(id: u64, end: super::JobEnd) {
        with(|s| {
            if let Some(i) = s.jobs.iter().position(|j| j.id == id) {
                let j = s.jobs.remove(i);
                s.ended.push((j.id, j.file, j.key, end));
            }
        });
    }

    /// The jobs alive, oldest first.
    pub(crate) fn jobs() -> Vec<Job> {
        with(|s| s.jobs.clone())
    }

    /// Jobs started since the last [`reset`].
    pub(crate) fn started() -> usize {
        with(|s| s.started)
    }

    /// The jobs that ended since the last call.
    pub(crate) fn take_ended() -> Vec<(u64, i64, String, super::JobEnd)> {
        with(|s| std::mem::take(&mut s.ended))
    }

    /// Let job `id` pass the point it waits at (it stops at the next one).
    pub(crate) fn step(id: u64) {
        with(|s| {
            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                j.permits += 1;
                j.at = None;
            }
        });
        MOVED.notify_all();
    }

    /// Let job `id` run to its end.
    pub(crate) fn finish(id: u64) {
        with(|s| {
            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                j.free = true;
                j.at = None;
            }
        });
        MOVED.notify_all();
    }

    pub(super) fn counted<F: FnOnce() + Send + 'static>(f: F) -> impl FnOnce() + Send + 'static {
        THREADS.fetch_add(1, Ordering::SeqCst);
        move || {
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    THREADS.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _done = Done;
            f()
        }
    }

    /// Helper threads that neither wait at a point nor ended: they are still doing something.
    pub(crate) fn busy() -> usize {
        let waiting = with(|s| s.jobs.iter().filter(|j| j.at.is_some()).count());
        THREADS.load(Ordering::SeqCst).saturating_sub(waiting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, t: f64) -> SourceRow {
        SourceRow {
            file: 1,
            image: 1,
            path: PathBuf::from(format!("/nicht/da/{name}")),
            name: name.into(),
            format: "CR2".into(),
            camera: Some("Canon".into()),
            time: Some(t),
            iso: Some(100),
            aperture: Some(8.0),
            focal: None,
            capture: None,
            width: Some(100),
            height: Some(80),
        }
    }

    /// A setting changed while a new merge was still computing cancels the creating job – that job must not take the
    /// photo out of the library (the change computes it); only a real failure or cancel of the creating job does, and
    /// never when the photo shows a picture already or carries work.
    #[test]
    fn a_superseded_creating_job_keeps_the_photo() {
        use AfterFailure::*;
        assert_eq!(after_failure(false, true, false), Superseded);
        assert_eq!(after_failure(false, false, false), Discard);
        assert_eq!(after_failure(false, false, true), Report, "a photo with a picture or work is never taken out by its job");
        assert_eq!(after_failure(true, true, false), Superseded);
        assert_eq!(after_failure(true, false, false), Report);
    }

    /// The same through the job: a creating job that cannot compute (no sources) and was overtaken by a change leaves
    /// the photo alone and ends as superseded.
    #[test]
    fn job_overtaken_by_a_change_does_not_discard() {
        let file = 4_711_001;
        let first = rawdev::merged::MergedSource::new(file, Vec::new(), hdr::Settings::default(), "f4711001-0000000000000001".into(), "f4711001-00000000000000b1".into());
        let newer = rawdev::merged::MergedSource::new(file, Vec::new(), hdr::Settings::default(), "f4711001-0000000000000002".into(), "f4711001-00000000000000b1".into());
        let (t1, t2) = (ticket(), ticket());
        let f1 = take_over(&first, t1).unwrap();
        let _f2 = take_over(&newer, t2).unwrap();
        assert!(f1.load(Ordering::SeqCst), "the older job is told to stop before it started");
        assert!(!creating(file));
        assert_eq!(job_as(&first, "x", None, false, t1, f1), JobEnd::Superseded);
        assert!(!creating(file));
        assert!(!take_events().iter().any(|e| matches!(e, Event::Failed { file: f, .. } if *f == file)), "nothing is reported for it");
        unwant(file);
        FLAGS.lock().unwrap().retain(|f| f.0 != file);
    }

    /// Two changes stored in the background may arrive in the other order: the one the user made last counts, the late
    /// older one is dropped (it neither becomes the result wanted nor stops the newer job). The same result asked for
    /// again by a newer change is a change of its own (the older job of that key counts as overtaken).
    #[test]
    fn the_newest_change_wins_whatever_arrives_last() {
        let file = 4_711_002;
        let src = |n: u8| rawdev::merged::MergedSource::new(file, Vec::new(), hdr::Settings::default(), format!("f4711002-000000000000000{n}"), "f4711002-00000000000000b1".into());
        let (older, newer) = (ticket(), ticket());
        let flag = take_over(&src(2), newer).expect("the newer change takes over");
        assert!(take_over(&src(1), older).is_none(), "the older change arrives late: dropped");
        assert_eq!(wanted(file).map(|w| w.key), Some(src(2).key));
        assert!(!flag.load(Ordering::SeqCst), "the newer job goes on");
        // A → B → A: the first job of A was overtaken, although the key wanted is its own again
        let again = ticket();
        let _ = take_over(&src(2), again).unwrap();
        assert!(flag.load(Ordering::SeqCst));
        assert_eq!(wanted_ticket(file), Some(again));
        assert!(job_alive(file));
        cancel(file);
        unwant(file);
        FLAGS.lock().unwrap().retain(|f| f.0 != file);
        assert!(!job_alive(file));
    }

    /// B10: the end of a job without a result takes back what that job was wanted for – not the wish of a newer
    /// change of the same photo that started before the app heard of the end.
    #[test]
    fn a_failed_job_does_not_unwant_a_newer_change() {
        let file = 4_711_004;
        let src = |n: u8| rawdev::merged::MergedSource::new(file, Vec::new(), hdr::Settings::default(), format!("f4711004-000000000000000{n}"), "f4711004-00000000000000b1".into());
        let (a, b) = (ticket(), ticket());
        let _ = take_over(&src(1), a).unwrap();
        // A failed; before its event is handled, change B takes over
        let _ = take_over(&src(2), b).unwrap();
        unwant_change(file, a);
        assert_eq!(wanted(file).map(|w| w.key), Some(src(2).key), "the newer change stays wanted");
        unwant_change(file, b);
        assert!(wanted(file).is_none());
        FLAGS.lock().unwrap().retain(|f| f.0 != file);
    }

    /// A result that could not be computed is remembered with its reason until the user asks again.
    #[test]
    fn a_failed_result_is_remembered_until_asked_again() {
        let file = 4_711_003;
        let src = rawdev::merged::MergedSource::new(file, Vec::new(), hdr::Settings::default(), "f4711003-0000000000000001".into(), "f4711003-00000000000000b1".into());
        // a change (the photo exists): fails for want of sources
        let t = ticket();
        let f = take_over(&src, t).unwrap();
        assert_eq!(job_as(&src, "x", None, true, t, f), JobEnd::Failed);
        assert!(failed_reason(file, &src.key).is_some());
        assert!(failed_reason(file, "f4711003-0000000000000009").is_none());
        let _ = take_events();
        let _ = take_over(&src, ticket());
        assert!(failed_reason(file, &src.key).is_none(), "asked again: tried again");
        forget_session(file);
        FLAGS.lock().unwrap().retain(|f| f.0 != file);
    }

    /// Threads and memory follow the operating mode asked for at each merge (they were fixed by the first merge).
    #[test]
    fn threads_and_memory_follow_the_mode() {
        assert_eq!(merge_threads(true, 10), 4);
        assert_eq!(merge_threads(true, 2), 2);
        assert_eq!(merge_threads(false, 10), 10);
        assert_eq!(merge_threads(false, 32), 16);
        assert!(merge_memory_budget(true, 16 << 30) < merge_memory_budget(false, 16 << 30));
    }

    /// The merge's own (English) error texts are shown in the language of the interface.
    #[test]
    fn merge_errors_are_shown_in_the_interface_language() {
        let mismatch = hdr::Error::Mismatch { path: PathBuf::from("/x/IMG_3.CR2") }.to_string();
        assert_eq!(reason_text(&mismatch), "IMG_3.CR2: andere Kamera oder Sensorgröße");
        assert_eq!(reason_text(&hdr::Error::Turned { path: PathBuf::from("/x/IMG_4.CR2") }.to_string()), "IMG_4.CR2: andere Ausrichtung – Hoch- und Querformat lassen sich nicht zusammenfügen");
        assert_eq!(reason_text(&hdr::Error::TooFewFrames(1).to_string()), "1 Aufnahmen: mindestens 2 nötig");
        assert_eq!(reason_text(&hdr::Error::TooManyFrames(12).to_string()), format!("12 Aufnahmen: höchstens {} möglich", hdr::MAX_FRAMES));
        // German messages of the layers below stay (and go through the dictionary in other languages)
        assert_eq!(reason_text("Quelle fehlt: A.CR2, B.CR2"), "Quelle fehlt: A.CR2, B.CR2");
        assert!(failure_text(&mismatch).ends_with("andere Kamera oder Sensorgröße"));
    }

    /// The plausibility check before any computation: number, kind, camera and size, exposure spread, reachable.
    #[test]
    fn refuses_with_reason() {
        assert!(refusal(&[row("a", 0.01)]).is_some());
        let mut b = row("b", 0.04);
        b.camera = Some("Nikon".into());
        assert!(refusal(&[row("a", 0.01), b]).unwrap().contains("Kameras"));
        // a camera not read yet is no other camera
        let mut u = row("u", 0.02);
        u.camera = None;
        assert!(!refusal(&[row("a", 0.01), u]).unwrap().contains("Kameras"));
        let mut j = row("c.jpg", 0.04);
        j.format = "JPG".into();
        assert!(refusal(&[row("a", 0.01), j]).is_some());
        let mut s = row("s", 0.04);
        s.width = Some(50);
        assert!(refusal(&[row("a", 0.01), s]).unwrap().contains("Größen"));
        // hardly any difference in exposure (1/100 and 1/80 s ≈ 0.3 EV)
        assert!(refusal(&[row("a", 0.01), row("b", 0.0125)]).unwrap().contains("0,5 EV"));
        // two exposures of a series one EV apart pass up to the reachability (the test files do not exist)
        assert!(refusal(&[row("a", 0.01), row("b", 0.02)]).unwrap().contains("nicht erreichbar"));
        // without exposure data the spread is not checked
        let (mut x, mut y) = (row("x", 0.01), row("y", 0.01));
        x.time = None;
        y.time = None;
        assert!(refusal(&[x, y]).unwrap().contains("nicht erreichbar"));
        let ten: Vec<SourceRow> = (0..10).map(|i| row(&format!("{i}"), 0.01 * (i + 1) as f64)).collect();
        assert!(refusal(&ten).is_some());
    }

    /// The request sorts dark → bright and takes the standard settings (middle exposure as reference, aligned,
    /// motion standard, linear).
    #[test]
    fn request_sorts_and_takes_the_defaults() {
        let dir = std::env::temp_dir().join(format!("hdr-request-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk = |n: &str, t: f64| {
            let mut r = row(n, t);
            r.path = dir.join(n);
            std::fs::write(&r.path, b"x").unwrap();
            r
        };
        let req = request(vec![mk("b.CR2", 0.04), mk("a.CR2", 0.01), mk("c.CR2", 0.16)]).unwrap();
        assert_eq!(req.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["a.CR2", "b.CR2", "c.CR2"]);
        assert_eq!(req.settings, hdr::Settings::new_merge());
        assert!(req.settings.align && req.settings.ghost == hdr::ghost::Level::Standard && req.settings.output == hdr::fusion::Output::Linear);
        assert!(request(vec![mk("a.CR2", 0.01)]).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    /// The questions before a merge: an open dialog keeps asking, "Abbrechen" drops the merge, the main button goes on
    /// with the next check (or opens the existing HDR), "Trotzdem neues HDR" goes on after the check of existing ones;
    /// a refusal only closes.
    #[test]
    fn question_answers() {
        use dialog::Answer::*;
        let req = || Request { rows: vec![row("a", 0.01), row("b", 0.04)], settings: hdr::Settings::default(), stack: true, keyword: String::new() };
        let doubt = Question::Doubt { req: req(), why: crate::library_ui::Doubt::Gap(600) };
        assert!(decide(&doubt, Open, false).is_none(), "still asking");
        assert!(matches!(decide(&doubt, Cancel, false), Some(Next::Nothing)), "cancelled: nothing is created");
        assert!(matches!(decide(&doubt, Primary, false), Some(Next::Continue { from: Check::Start, ref req, .. }) if req.rows.len() == 2));
        let exists = Question::Exists { req: req(), outside: Some("X.CR2".into()), image: 7, name: "a-HDR.dng".into(), part: false };
        assert!(matches!(decide(&exists, Primary, false), Some(Next::Open(7))), "the existing HDR opens");
        assert!(matches!(decide(&exists, Open, true), Some(Next::Continue { from: Check::Selection, outside: Some(_), .. })), "a new one: the next checks still ask");
        assert!(matches!(decide(&exists, Cancel, false), Some(Next::Nothing)));
        // the HDR in the selection holds these frames among others: a new one still asks about an HDR of exactly them
        let part = Question::Exists { req: req(), outside: None, image: 7, name: "a-HDR.dng".into(), part: true };
        assert!(matches!(decide(&part, Primary, false), Some(Next::Open(7))));
        assert!(matches!(decide(&part, Open, true), Some(Next::Continue { from: Check::Exists, .. })));
        // an HDR left out of the selection: "Neues HDR" goes on with every check, "Abbrechen" creates nothing
        let left = Question::LeftOut { req: req(), outside: None, left: "a-HDR.dng".into() };
        assert!(decide(&left, Open, false).is_none());
        assert!(matches!(decide(&left, Primary, false), Some(Next::Continue { from: Check::Exists, .. })));
        assert!(matches!(decide(&left, Cancel, false), Some(Next::Nothing)));
        let sel = Question::Selection { req: req(), outside: "X.CR2".into() };
        assert!(matches!(decide(&sel, Primary, false), Some(Next::Continue { from: Check::Doubt, outside: None, .. })));
        assert!(matches!(decide(&Question::Refused("x".into()), Cancel, false), Some(Next::Nothing)));
        assert!(Check::Exists < Check::Selection && Check::Selection < Check::Doubt && Check::Doubt < Check::Start);
        assert_eq!(crate::library_ui::Doubt::Gap(600).text(), "Zwischen zwei Aufnahmen liegen 10 Minuten.");
    }

    /// The reason of a refusal names what is wrong (QA 1.5.0-test.5: "HDR nicht möglich" without a visible reason).
    #[test]
    fn refusal_names_the_reason() {
        let mut x3i = row("SDQH1646.X3I", 0.01);
        x3i.format = "X3I".into();
        assert!(refusal(&[x3i, row("b", 0.04)]).unwrap().contains("SFD"));
        let mut x3f = row("SDQH0001.X3F", 0.01);
        x3f.format = "X3F".into();
        // with a mosaic RAW: refused; a bracket of X3F frames alone merges (Foveon single frames, 1.5.1)
        assert!(refusal(&[x3f.clone(), row("b", 0.04)]).unwrap().contains("Foveon"));
        let mut x3f2 = row("SDQH0002.X3F", 0.04);
        x3f2.format = "X3F".into();
        let (a, b) = (std::env::temp_dir().join("graineo-refusal-a.X3F"), std::env::temp_dir().join("graineo-refusal-b.X3F"));
        std::fs::write(&a, b"FOVb").unwrap();
        std::fs::write(&b, b"FOVb").unwrap();
        x3f.path = a.clone();
        x3f2.path = b.clone();
        assert_eq!(refusal(&[x3f, x3f2]), None);
        let _ = (std::fs::remove_file(a), std::fs::remove_file(b));
        let mut nikon = row("b", 0.04);
        nikon.camera = Some("Nikon D7200".into());
        assert_eq!(refusal(&[row("a", 0.01), nikon]).unwrap(), "Die Fotos stammen von verschiedenen Kameras: Canon, Nikon D7200.");
    }

    #[test]
    fn ev_labels() {
        assert_eq!(ev_label(0.0), "0");
        assert_eq!(ev_label(2.0), "+2");
        assert_eq!(ev_label(-1.96), "−2");
        assert_eq!(ev_label(-1.3).replace(',', "."), "−1.3");
    }
}

// ─────────────────────────────────────────────── the app's side

/// Once per start: the checks of the merged photos (unfinished ones of the last session, grid previews gone).
static CHECKED: AtomicBool = AtomicBool::new(false);

/// The tool "Zusammenfügen" should open with the next merged photo shown (set when a merge was started).
static OPEN_TOOL: AtomicBool = AtomicBool::new(false);

pub(crate) fn open_tool_next() {
    OPEN_TOOL.store(true, Ordering::Relaxed);
}

/// Photos the library's context menu asked to merge (taken by the app in the next frame).
static ASKED: Mutex<Option<Vec<SourceRow>>> = Mutex::new(None);

/// The checks before a merge, in this order; each one may ask, the answer goes on with the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Check {
    /// an HDR of exactly these frames exists already
    Exists,
    /// the selection is merged although the photo at hand is not part of it
    Selection,
    /// the photos do not look like one exposure bracket
    Doubt,
    /// nothing left to ask: merge
    Start,
}

/// A question shown before a merge (or why there is none) – a dialog until it is answered.
#[derive(Clone, Debug)]
pub(crate) enum Question {
    /// an HDR of exactly these frames exists (its image and name); `outside`: see [`Question::Selection`]; `part`: the
    /// HDR is one of the selection and these frames are (some of) its sources, not exactly them
    Exists { req: Request, outside: Option<String>, image: i64, name: String, part: bool },
    /// the selection also holds merged photos (`left`, named), which are never sources: merge the others anew?
    LeftOut { req: Request, outside: Option<String>, left: String },
    /// the selection is merged, the photo at hand (named) is not part of it
    Selection { req: Request, outside: String },
    /// the photos do not look like one exposure bracket
    Doubt { req: Request, why: crate::library_ui::Doubt },
    /// the merge is not possible: why
    Refused(String),
}

/// What an answer asks the app to do.
#[derive(Debug)]
pub(crate) enum Next {
    /// nothing (cancelled, or a refusal read)
    Nothing,
    /// open this merged photo in the tool "Zusammenfügen"
    Open(i64),
    /// go on with the checks from this one
    Continue { req: Request, outside: Option<String>, from: Check },
}

/// The open question (one at a time).
static QUESTION: Mutex<Option<Question>> = Mutex::new(None);

pub(crate) fn ask_question(q: Question) {
    *QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = Some(q);
}

/// What the answer to a question means; `None` while it is still open. `extra`: the button "Trotzdem neues HDR".
pub(crate) fn decide(q: &Question, answer: dialog::Answer, extra: bool) -> Option<Next> {
    use dialog::Answer::*;
    if let Question::Exists { req, outside, part, .. } = q {
        if extra {
            // a part of the HDR's frames: an HDR of exactly these may still exist (asked about next)
            let from = if *part { Check::Exists } else { Check::Selection };
            return Some(Next::Continue { req: req.clone(), outside: outside.clone(), from });
        }
    }
    match answer {
        Open => None,
        Cancel => Some(Next::Nothing),
        Primary => Some(match q {
            Question::Exists { image, .. } => Next::Open(*image),
            Question::LeftOut { req, outside, .. } => Next::Continue { req: req.clone(), outside: outside.clone(), from: Check::Exists },
            Question::Selection { req, .. } => Next::Continue { req: req.clone(), outside: None, from: Check::Doubt },
            Question::Doubt { req, .. } => Next::Continue { req: req.clone(), outside: None, from: Check::Start },
            Question::Refused(_) => Next::Nothing,
        }),
    }
}

/// The photos of a request as one line ("A.CR2, B.CR2, C.CR2").
fn names(req: &Request) -> String {
    req.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// Show the question; returns the answer and whether "Trotzdem neues HDR" was clicked.
fn show_question(ctx: &egui::Context, q: &Question) -> (dialog::Answer, bool) {
    let title = crate::tr("HDR zusammenfügen");
    match q {
        Question::Exists { req, name, part, .. } => dialog::show_extra(
            ctx,
            if *part { "hdr-exists-part" } else { "hdr-exists" },
            title,
            dialog::WIDTH,
            |ui| {
                if *part {
                    dialog::text(ui, &crate::trf!("Die gewählten Aufnahmen gehören schon zum HDR {a} – ein HDR wird nie selbst zur Quelle.", a = name));
                } else {
                    dialog::text(ui, &crate::trf!("Aus diesen Aufnahmen gibt es schon ein HDR: {a}.", a = name));
                }
                dialog::hint(ui, &names(req));
            },
            dialog::Buttons::confirm(crate::tr("Öffnen"), true),
            Some(crate::tr("Trotzdem neues HDR")),
        ),
        Question::Selection { req, outside } => {
            let a = dialog::show(
                ctx,
                "hdr-selection",
                title,
                dialog::WIDTH,
                |ui| {
                    dialog::text(ui, &crate::trf!("„{a}“ gehört nicht zur Auswahl. Zusammengefügt werden die {n} ausgewählten Fotos:", a = outside, n = req.rows.len()));
                    dialog::hint(ui, &names(req));
                },
                dialog::Buttons::confirm(crate::tr("Zusammenfügen"), true),
            );
            (a, false)
        }
        Question::LeftOut { req, left, .. } => {
            let a = dialog::show(
                ctx,
                "hdr-left-out",
                title,
                dialog::WIDTH,
                |ui| {
                    dialog::text(ui, &crate::trf!("Schon zusammengefügt, wird nicht verwendet: {a}. Aus diesen {n} Aufnahmen ein neues HDR zusammenfügen?", a = left, n = req.rows.len()));
                    dialog::hint(ui, &names(req));
                },
                dialog::Buttons::confirm(crate::tr("Neues HDR"), true),
            );
            (a, false)
        }
        Question::Doubt { req, why } => {
            let a = dialog::show(
                ctx,
                "hdr-doubt",
                title,
                dialog::WIDTH,
                |ui| {
                    dialog::text(ui, crate::tr("Diese Fotos sehen nicht nach einer Belichtungsreihe aus. Trotzdem zusammenfügen?"));
                    dialog::hint(ui, &why.text());
                    dialog::hint(ui, &names(req));
                },
                dialog::Buttons::confirm(crate::tr("Zusammenfügen"), true),
            );
            (a, false)
        }
        Question::Refused(why) => {
            let a = dialog::show(ctx, "hdr-refused", crate::tr("HDR nicht möglich"), dialog::WIDTH, |ui| dialog::text(ui, why), dialog::Buttons::close());
            (a, false)
        }
    }
}

static LAST_ASKED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// The menu item and the key shortcut both reach "HDR zusammenfügen …" for one key press: once per 0.8 s at most.
fn debounced() -> bool {
    let mut l = LAST_ASKED.lock().unwrap_or_else(|e| e.into_inner());
    if l.is_some_and(|t| clock::since(t) < std::time::Duration::from_millis(800)) {
        return false;
    }
    *l = Some(clock::now());
    true
}

/// "HDR zusammenfügen …" from a place without the app at hand (context menu of the grid, test hook).
pub(crate) fn ask(rows: Vec<SourceRow>) {
    *ASKED.lock().unwrap_or_else(|e| e.into_inner()) = Some(rows);
}

impl crate::App {
    /// "HDR zusammenfügen …" (menu bar, ⇧⌘H): the selection when it holds more than one photo, otherwise the photo at
    /// hand (the shown one in the single image) with its bracket; a merged photo opens in the tool "Zusammenfügen".
    /// Merges with the standard settings; asks first when an HDR of these frames exists, when the selection does not
    /// hold the photo at hand, or when the photos do not look like one bracket. A refusal says why in a dialog.
    pub(crate) fn open_hdr(&mut self) {
        let image = if self.in_grid() { None } else { self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image) };
        match self.lib.hdr_pick(image) {
            crate::library_ui::library_hdr::Pick::Open(image) => {
                if debounced() {
                    self.open_merge(image);
                }
            }
            crate::library_ui::library_hdr::Pick::Develop(image) => {
                let mut actions = Vec::new();
                self.lib.show_merge(image, &mut actions);
                self.library_actions(actions);
                self.status = crate::tr("SFD-Aufnahme: graineo fügt ihre Belichtungen beim Entwickeln zusammen").to_string();
            }
            crate::library_ui::library_hdr::Pick::Merge(rows) => self.start_hdr_with(rows, None),
            crate::library_ui::library_hdr::Pick::Selection(rows, outside) => self.start_hdr_with(rows, Some(outside)),
            crate::library_ui::library_hdr::Pick::LeavesOut { rows, left, own, outside } => {
                if debounced() {
                    self.hdr_leaves_out(rows, left, own, outside);
                }
            }
        }
    }

    /// The selection holds merged photos besides ordinary ones (see `Pick::LeavesOut`): never silently left out. A
    /// merged photo whose frames these are is offered to open (like an HDR of exactly these frames); otherwise the
    /// question names the photos left out before merging the others.
    pub(crate) fn hdr_leaves_out(&mut self, rows: Vec<SourceRow>, left: Vec<(i64, String)>, own: Option<i64>, outside: Option<String>) {
        let left_names = left.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ");
        let own = own.and_then(|o| left.iter().find(|(b, _)| *b == o).cloned());
        match request(rows) {
            Ok(req) => {
                if let Some((image, name)) = self.lib.existing_merge(&req.rows) {
                    return ask_question(Question::Exists { req, outside, image, name, part: false });
                }
                match own {
                    Some((image, name)) => ask_question(Question::Exists { req, outside, image, name, part: true }),
                    None => ask_question(Question::LeftOut { req, outside, left: left_names }),
                }
            }
            // nothing new can be merged: the HDR these frames belong to opens; otherwise why, naming what was left out
            Err(why) => match own {
                Some((image, _)) => self.open_merge(image),
                None => ask_question(Question::Refused(format!("{why} {}", crate::trf!("Schon zusammengefügt, nicht als Quelle verwendet: {a}.", a = left_names)))),
            },
        }
    }

    /// Start the merge of these photos (context menu, test hook; see [`request`]).
    pub(crate) fn start_hdr(&mut self, rows: Vec<SourceRow>) {
        self.start_hdr_with(rows, None);
    }

    fn start_hdr_with(&mut self, rows: Vec<SourceRow>, outside: Option<String>) {
        if !debounced() {
            return;
        }
        match request(rows) {
            Ok(req) => self.hdr_next(req, outside, Check::Exists),
            Err(why) => ask_question(Question::Refused(why)),
        }
    }

    /// The checks before a merge from `from` on (see [`Check`]): the first that has something to ask asks, otherwise
    /// the merge starts.
    pub(crate) fn hdr_next(&mut self, req: Request, outside: Option<String>, from: Check) {
        if from <= Check::Exists {
            if let Some((image, name)) = self.lib.existing_merge(&req.rows) {
                return ask_question(Question::Exists { req, outside, image, name, part: false });
            }
        }
        if from <= Check::Selection {
            if let Some(outside) = outside {
                return ask_question(Question::Selection { req, outside });
            }
        }
        // photos far apart in time or of different subjects: asked first (not refused – it may be meant)
        if from <= Check::Doubt {
            if let Some(why) = self.lib.hdr_doubt(&req.rows) {
                return ask_question(Question::Doubt { req, why });
            }
        }
        open_tool_next();
        self.lib.start_merge(req);
    }

    /// Open an existing merged photo in Develop with the tool "Zusammenfügen".
    pub(crate) fn open_merge(&mut self, image: i64) {
        let shown = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image);
        if !self.in_grid() && shown == Some(image) {
            self.tool_open = Some(crate::workspace::ToolTab::Merge);
            return;
        }
        let mut actions = Vec::new();
        self.lib.show_merge(image, &mut actions);
        open_tool_next();
        self.library_actions(actions);
    }

    /// Each frame: what the merge jobs delivered (grid previews, pixels of the open photo, success, failure).
    pub(crate) fn hdr_step(&mut self, ctx: &egui::Context) {
        // "HDR entfernen": the develop settings of the open photo go to the library first (they are part of what ⌘Z
        // brings back)
        if self.lib.merge_removals_waiting() {
            self.save_current();
            self.lib.run_merge_removals();
        }
        let asked = ASKED.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(rows) = asked {
            self.start_hdr(rows);
        }
        if let Some(reqs) = crate::hdr_batch::show(ctx) {
            self.lib.start_merge_batch(reqs);
        }
        let question = QUESTION.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(q) = question {
            let (answer, extra) = show_question(ctx, &q);
            if let Some(next) = decide(&q, answer, extra) {
                *QUESTION.lock().unwrap_or_else(|e| e.into_inner()) = None;
                match next {
                    Next::Nothing => {}
                    Next::Open(image) => self.open_merge(image),
                    Next::Continue { req, outside, from } => self.hdr_next(req, outside, from),
                }
            }
        }
        // a merge just started: Develop with the tool open, as soon as the new photo is the shown one
        if OPEN_TOOL.load(Ordering::Relaxed) && self.merge_info().is_some() {
            OPEN_TOOL.store(false, Ordering::Relaxed);
            self.tool_open = Some(crate::workspace::ToolTab::Merge);
        }
        self.merge_update_step(ctx);
        // once per start: merged photos without a stored grid preview get it from the cached merge preview (a few ms
        // each, no merge is computed; nothing recurs while scrolling)
        if self.lib.rows_loaded() && !CHECKED.swap(true, Ordering::Relaxed) {
            // merged photos the last session left unfinished (it ended while they were computed)
            self.lib.settle_unfinished_merges();
            let missing = self.lib.hdr_missing_previews();
            if !missing.is_empty() {
                std::thread::spawn(move || {
                    for (file, src) in missing {
                        if !rawdev::merged::preview_cached(&src.key) {
                            continue;
                        }
                        let (Some(meta), Some(pv)) = (rawdev::merged::read_meta(&src.key), rawdev::merged::read_preview(&src.key)) else { continue };
                        let settings = develop_settings(file);
                        if let Ok(img) = pool().install(|| rawdev::merged::render_preview_with(&src.reference(), &pv, &meta, &settings)) {
                            if let Some(rgb) = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()) {
                                push(Event::Thumbnail { file, key: src.key.clone(), img: rgb });
                            }
                        }
                    }
                });
            }
        }
        let now = ctx.input(|i| i.time);
        for e in take_events() {
            match e {
                // only the picture of the result wanted now, and only for a photo the library still has (an overtaken
                // job, or the job of a photo taken out again, must not leave its picture behind)
                Event::Thumbnail { file, key, img } => {
                    if wanted(file).is_none_or(|w| w.key == key) && self.lib.has_merged_file(file) {
                        self.lib.store_developed(ctx, file, img, true, false);
                    }
                }
                Event::Done { text, file, created } => {
                    let a = self.lib.merge_job_ended(file, false);
                    self.library_actions(a);
                    self.notes.done(text, now);
                    if created {
                        crate::hdr_live::created_done(file, now);
                    }
                }
                Event::Pixels { file, key, preview, full, meta } => self.merge_pixels(file, key, preview, full, meta),
                Event::New { file } => {
                    // an earlier merged photo may have had the same path (made and removed again): its develop history
                    // in memory is not the history of this one
                    if let Some(path) = self.lib.merged_path(file) {
                        self.history.forget_path(&path);
                    }
                }
                Event::Failed { text, removed, file, ticket } => {
                    crate::hdr_paint::recipe_changed();
                    if removed {
                        self.lib.merge_removed_back(file);
                    }
                    let a = self.lib.merge_job_ended(file, removed);
                    self.library_actions(a);
                    // (only the result of that job: a newer change of the photo stays wanted)
                    unwant_change(file, ticket);
                    if text == crate::tr("HDR abgebrochen") {
                        // a new merge cancelled before it showed anything leaves the library again: said so
                        // (as a message that stays a moment: the status line is taken over at once by the photo opening
                        // in its place – QA 11, N-1)
                        if removed {
                            self.notes.done(crate::tr("HDR abgebrochen und entfernt (Quellen bleiben)"), now);
                        } else {
                            self.status = text;
                        }
                    } else {
                        self.notes.fault(crate::activity::Fault::from_message(&text));
                    }
                }
                Event::Tuned { file, tone } => {
                    // settings of the photo the app holds in memory follow, while their sliders are still the ones
                    // the photo started with (nobody moved them): otherwise the photo would open flat, and the next
                    // save would write the neutral sliders over the automatic ones
                    let neutral = crate::hdr_auto::tone_of(&crate::Settings::default());
                    for e in self.entries.iter_mut().filter(|e| e.file == file && e.copy.is_none()) {
                        if crate::hdr_auto::tone_of(&e.settings) == neutral {
                            crate::hdr_auto::set_tone(&mut e.settings, tone);
                        }
                    }
                    // the photo at hand: its document was opened with the recipe before the automatic set its values
                    if let Some(a) = self.shown_merged().and_then(|(_, d)| d.merged.as_ref().filter(|m| m.source.file == file).and_then(|m| m.source.settings.auto_tone)) {
                        crate::hdr_auto::remember(file, Some(hdr::AutoTone { applied: Some(tone), ..a }));
                    }
                }
                Event::BatchEnded { removed, cancelled } => {
                    crate::hdr_paint::recipe_changed();
                    if removed {
                        self.lib.merge_removed();
                    }
                    if cancelled {
                        self.status = crate::tr("HDR abgebrochen").to_string();
                    }
                    // a ⌘Z of the batch that waited for its end is carried out now
                    self.lib.merge_batch_ended();
                }
            }
        }
        // a ⌘Z that waits for the first computation of a photo whose job ended without saying so (it was overtaken
        // by a change): carried out now – never an entry that waits for ever and fires at some later failure
        let a = self.lib.merge_undo_overdue();
        self.library_actions(a);
        self.merge_doc_step(ctx);
        self.merge_auto_step(ctx);
        // progress in the title bar and in the tool: redrawn while a merge runs
        if changed() || !rawdev::merged::jobs().is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}
