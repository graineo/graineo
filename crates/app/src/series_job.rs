//! Detecting series in the background, quietly: one thread at the lowest priority (efficiency cores), short
//! transactions (one per capture day), cancellable at any time.
//!
//! 1. Image signatures (`library::series::signature`) from the grid previews already in the cache – never decodes an
//!    original. Photos whose preview is not there yet are skipped and taken on a later pass.
//! 2. Grouping (`library::series::regroup`): only days whose photos changed since the last run.
//!
//! In Low Power Mode it runs slower (ten times the pause per photo) and says so; the left panel shows its state while
//! there are no series yet ([`empty_state`]).
//!
//! Runs after the library was loaded (start, import, sync, time correction, moves: every reload pokes it, a few
//! seconds later it starts) and when the vector job of the semantic search stored new vectors. The grid reads the
//! stacks again when [`generation`] changes.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use library::db::Db;
use library::preview::{self, Cache};

/// Stacks changed in the library (the grid reloads them when this moves).
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Shooting data the job read into the library (the grid reloads its rows when this moves: sorting by capture time,
/// info panel).
static ROWS: AtomicU64 = AtomicU64::new(0);

/// Moves when the job wrote shooting data into the library.
pub fn rows_generation() -> u64 {
    ROWS.load(Ordering::Relaxed)
}
/// Signatures still to compute (shown in the activity list while the job works on them).
static MISSING: AtomicUsize = AtomicUsize::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
/// What the job does now ([`Phase`] as a number) and its progress in that phase (done, total).
static PHASE: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
/// A pass has finished in this run (the library was looked at once).
static PASSED: AtomicBool = AtomicBool::new(false);

/// What the series job is doing (left panel "Serien", activity list).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// not started yet (waits for the library) or switched off
    Idle,
    /// waits for the search index (first start, restored backup), at most [`INDEX_WAIT`]
    WaitIndex,
    /// compares the content of the photos (signatures from the previews)
    Signatures,
    /// groups the photos of each capture day
    Grouping,
    /// measures sharpness and faces of the members
    Quality,
}

impl Phase {
    fn from(n: usize) -> Phase {
        match n {
            1 => Phase::WaitIndex,
            2 => Phase::Signatures,
            3 => Phase::Grouping,
            4 => Phase::Quality,
            _ => Phase::Idle,
        }
    }
}

/// The search index has priority; the series wait for it at most this long.
pub const INDEX_WAIT: Duration = Duration::from_secs(300);

fn set_phase(p: Phase, done: usize, total: usize) {
    PHASE.store(p as usize, Ordering::Relaxed);
    DONE.store(done, Ordering::Relaxed);
    TOTAL.store(total, Ordering::Relaxed);
}

/// Phase and progress (done, total) right now.
pub fn progress() -> (Phase, usize, usize) {
    (Phase::from(PHASE.load(Ordering::Relaxed)), DONE.load(Ordering::Relaxed), TOTAL.load(Ordering::Relaxed))
}

/// Has a pass looked at the whole library in this run? (Then "no series" means none were found.)
pub fn passed() -> bool {
    PASSED.load(Ordering::Relaxed)
}

/// What the row "Serien" shows its state from: detection switched on, phase, done, total, a whole pass made, the
/// computer saving power.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowState {
    pub enabled: bool,
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    pub passed: bool,
    pub low_power: bool,
}

/// The state of the detection right now, for the row "Serien". It is process-wide (the job, the setting, the power
/// state); a test that draws the row fixes it for its own thread with [`fix_row_state`], so other tests that run
/// the detection or change these states at the same time cannot change what it sees.
pub fn row_state() -> RowState {
    #[cfg(test)]
    if let Some(fixed) = FIXED_ROW_STATE.with(|f| f.get()) {
        return fixed;
    }
    let (phase, done, total) = progress();
    RowState { enabled: enabled(), phase, done, total, passed: passed(), low_power: crate::perf_mode::power() == crate::perf_mode::Power::LowPower }
}

#[cfg(test)]
thread_local! {
    static FIXED_ROW_STATE: std::cell::Cell<Option<RowState>> = const { std::cell::Cell::new(None) };
}

/// Tests: [`row_state`] answers `state` on this thread from now on (`None`: the real state again).
#[cfg(test)]
pub fn fix_row_state(state: Option<RowState>) {
    FIXED_ROW_STATE.with(|f| f.set(state));
}

/// Test hook `GRAINEO_SERIES_SLOW_MS=<ms>` (only with `GRAINEO_DATA_DIR`): extra wait per photo and per day, so the
/// progress of the search can be seen in screenshots of a small test library.
fn test_slow() {
    static MS: OnceLock<u64> = OnceLock::new();
    let ms = *MS.get_or_init(|| crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_SERIES_SLOW_MS").ok()).and_then(|v| v.parse().ok()).unwrap_or(0));
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// In Low Power Mode the job runs slower (a longer pause per photo); the texts say so.
fn throttled() -> bool {
    crate::perf_mode::power() == crate::perf_mode::Power::LowPower
}

struct Wake {
    poked: Mutex<Option<Instant>>,
    cv: Condvar,
}

fn wake() -> &'static Wake {
    static W: OnceLock<Wake> = OnceLock::new();
    W.get_or_init(|| Wake { poked: Mutex::new(Some(Instant::now())), cv: Condvar::new() })
}

/// Wait this long after the last poke (a sync reloads several times in a row).
const SETTLE: Duration = Duration::from_secs(3);
/// Days left waiting for shooting data or previews: the job looks again after this time.
const WAIT_AGAIN: Duration = Duration::from_secs(10);
/// Passes without any progress after which waiting days are grouped as they are (about two minutes).
const WAIT_ROUNDS: usize = 10;
/// Passes in a row that left days waiting without any progress.
static STALLED: AtomicUsize = AtomicUsize::new(0);
/// Pause per signature: the job never takes a core for long (fans).
const PAUSE: Duration = Duration::from_millis(4);

/// Series detection switched on (setting "seriesDetect", default on).
pub fn enabled() -> bool {
    library::settings::value("seriesDetect").as_bool().unwrap_or(true)
}

/// Stacks changed: the number moves on.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Something in the library changed (photos added, times corrected, vectors stored): group again soon.
pub fn poke() {
    let w = wake();
    *w.poked.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    w.cv.notify_all();
}

/// The user changed stacks directly (the grid already shows it): reload them, no regrouping needed.
pub fn changed() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Stop the current pass (setting switched off, quit); the thread waits for the next poke.
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Text for the activity list while working or waiting for the search index (`None`: idle).
pub fn status() -> Option<String> {
    let (phase, done, total) = progress();
    if phase == Phase::WaitIndex {
        return Some(crate::tr("Serien: warten auf den Suchindex").to_string());
    }
    if !RUNNING.load(Ordering::Relaxed) {
        return None;
    }
    let n = MISSING.load(Ordering::Relaxed);
    let t = match phase {
        Phase::Grouping if total > 0 => crate::trf!("Serien werden gesucht … {d} von {n} Tagen", d = done, n = total),
        _ if n > 0 => crate::trf!("Serien erkennen: noch {n} Fotos", n = n),
        _ => crate::tr("Serien erkennen").to_string(),
    };
    Some(if throttled() { format!("{t} · {}", crate::tr("gedrosselt: Stromsparmodus")) } else { t })
}

/// State of the series detection while the library has no series yet: a short line for the left panel ("Serien"),
/// a sentence for the middle of the grid, and the explanation (tooltip, below the sentence).
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyState {
    pub short: String,
    /// a few characters for the number column of the row "Serien" ("sucht 19/27", "keine", "aus")
    pub count: String,
    pub title: String,
    pub tip: String,
}

/// [`EmptyState`] from the state of the job. Pure (tests).
pub fn empty_state(enabled: bool, phase: Phase, done: usize, total: usize, passed: bool, throttled: bool) -> EmptyState {
    let slow = if throttled { format!(" · {}", crate::tr("gedrosselt: Stromsparmodus")) } else { String::new() };
    // the left panel is narrow: there only "gedrosselt", the tooltip says why
    let slow_short = if throttled { format!(" · {}", crate::tr("gedrosselt")) } else { String::new() };
    let how = crate::trf!(
        "Serien sind Fotos kurz nacheinander mit gleichem Motiv. Die Erkennung läuft leise im Hintergrund und nutzt nur die Vorschauen. Eigene Stapel: Fotos auswählen, Foto → Als Stapel bündeln ({g}); sichten: Foto → Serie sichten ({r}).",
        g = crate::shortcuts::display("stack_group"),
        r = crate::shortcuts::display("stack_review")
    );
    let (short, title, why) = if !enabled {
        (crate::tr("Erkennung aus").to_string(), crate::tr("Serienerkennung ist aus").to_string(), crate::tr("Einstellungen → Bibliothek → Serien automatisch erkennen").to_string())
    } else {
        match phase {
            Phase::WaitIndex => (
                crate::tr("wartet auf den Suchindex").to_string(),
                crate::tr("Serien: warten auf den Suchindex …").to_string(),
                crate::tr("Der Suchindex wird zuerst gebaut (höchstens 5 Minuten), danach beginnt die Suche nach Serien.").to_string(),
            ),
            Phase::Signatures if total > 0 => (
                crate::trf!("Suche … {d} von {n} Fotos{s}", d = done, n = total, s = &slow_short),
                crate::trf!("Serien werden gesucht … {d} von {n} Fotos{s}", d = done, n = total, s = &slow),
                crate::tr("Der Bildinhalt der Fotos wird aus den Vorschauen verglichen.").to_string(),
            ),
            Phase::Grouping if total > 0 => (
                crate::trf!("Suche … {d} von {n} Tagen{s}", d = done, n = total, s = &slow_short),
                crate::trf!("Serien werden gesucht … {d} von {n} Tagen{s}", d = done, n = total, s = &slow),
                crate::tr("Die Fotos jedes Aufnahmetags werden zu Serien gruppiert.").to_string(),
            ),
            Phase::Signatures | Phase::Grouping | Phase::Quality => (crate::trf!("Suche …{s}", s = &slow_short), crate::trf!("Serien werden gesucht …{s}", s = &slow), String::new()),
            Phase::Idle if passed => (
                crate::tr("noch keine gefunden").to_string(),
                crate::tr("Noch keine Serien gefunden").to_string(),
                crate::tr("Keine Fotos kurz nacheinander mit gleichem Motiv gefunden; Fotos ohne Vorschau kommen später dran.").to_string(),
            ),
            Phase::Idle => (crate::tr("Suche beginnt …").to_string(), crate::tr("Serien werden gesucht …").to_string(), crate::tr("Die Suche beginnt kurz nachdem die Bibliothek geladen ist.").to_string()),
        }
    };
    let tip = [why.as_str(), how.as_str()].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join("\n");
    let count = if !enabled {
        crate::tr("aus").to_string()
    } else {
        match phase {
            Phase::WaitIndex => crate::tr("wartet").to_string(),
            Phase::Signatures | Phase::Grouping if total > 0 => crate::trf!("sucht {d}/{n}", d = done, n = total),
            Phase::Idle if passed => crate::tr("keine").to_string(),
            _ => crate::tr("sucht …").to_string(),
        }
    };
    EmptyState { short, count, title, tip }
}

/// Start the thread (once; later calls do nothing).
pub fn start(cache: Arc<Cache>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new().name("serien".into()).spawn(move || {
        library::platform::background_priority();
        loop {
            wait_for_poke();
            // the search index has priority (first start, restored backup): wait until it is built, at most 5 minutes
            let t = Instant::now();
            while enabled() && crate::library_ui::INDEX_MISSING.load(Ordering::Relaxed) && t.elapsed() < INDEX_WAIT {
                set_phase(Phase::WaitIndex, 0, 0);
                std::thread::sleep(Duration::from_millis(500));
            }
            set_phase(Phase::Idle, 0, 0);
            CANCEL.store(false, Ordering::Relaxed);
            if !enabled() {
                continue;
            }
            let Ok(db) = Db::standard() else { continue };
            RUNNING.store(true, Ordering::Relaxed);
            let r = pass(&db, &cache);
            RUNNING.store(false, Ordering::Relaxed);
            MISSING.store(0, Ordering::Relaxed);
            set_phase(Phase::Idle, 0, 0);
            if r.is_ok() && !cancelled() {
                PASSED.store(true, Ordering::Relaxed);
            }
            match r {
                Ok(true) => {
                    GENERATION.fetch_add(1, Ordering::Relaxed);
                }
                Ok(false) => {}
                Err(e) => crate::log(&format!("series: {e}")),
            }
        }
    });
}

fn wait_for_poke() {
    let w = wake();
    let mut g = w.poked.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        match *g {
            Some(t) if t.elapsed() >= SETTLE => {
                *g = None;
                return;
            }
            Some(t) => {
                let left = SETTLE.saturating_sub(t.elapsed());
                g = w.cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
            }
            None => {
                g = w.cv.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        }
    }
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::Relaxed)
}

/// The preview for measuring a photo: graineo's own development or the cache, the level nearest to 1024 px, reduced
/// to at most 1024 px. Never decodes the original.
pub fn preview_for(cache: &Cache, file: i64, path: &str, size: i64, mtime: i64, orientation: Option<i64>) -> Option<image::RgbImage> {
    let key = preview::cache_key_with_orientation(preview::cache_key(path, size, mtime), orientation);
    let img = preview::read_developed_best(file, 1024).map(|(i, _)| i).or_else(|| cache.nearest_level(key, 1024).and_then(|l| cache.read(key, l)))?;
    Some(preview::downscale(img, 1024))
}

/// One pass: signatures for the previews there are, then grouping. Returns: stacks changed.
fn pass(db: &Db, cache: &Cache) -> Result<bool, String> {
    let t = Instant::now();
    let todo = library::series::files_without_signature(db);
    let small = preview::small();
    let mut done = 0usize;
    // files whose preview is not there yet: their days wait (grouped without signatures, brackets fell apart)
    let mut waiting: std::collections::HashSet<i64> = std::collections::HashSet::new();
    MISSING.store(todo.len(), Ordering::Relaxed);
    set_phase(Phase::Signatures, 0, todo.len());
    for (k, task) in todo.iter().enumerate() {
        if cancelled() {
            return Ok(false);
        }
        DONE.store(k, Ordering::Relaxed);
        test_slow();
        let key = preview::cache_key_with_orientation(preview::cache_key(&task.path, task.size, task.mtime), task.orientation);
        // the preview the grid shows: graineo's own development first, else the cache
        let img = preview::read_developed_best(task.file, small).map(|(i, _)| i).or_else(|| cache.nearest_level(key, small).and_then(|l| cache.read(key, l)));
        if let Some(img) = img {
            let sig = library::series::signature(&img);
            library::series::save_signature(db, task.file, &sig)?;
            done += 1;
            // Low Power Mode: ten times the pause (the job takes longer, the computer stays cool)
            std::thread::sleep(if throttled() { PAUSE * 10 } else { PAUSE });
        } else {
            waiting.insert(task.file);
        }
        MISSING.store(todo.len() - k - 1, Ordering::Relaxed);
    }
    set_phase(Phase::Grouping, 0, 0);
    // shooting data not read yet (right after an import the sync that reads it may not run – QA 1.5.0-test.5: the
    // series stayed in pieces until the next start): read it here, as the sync does, before grouping
    let unread: i64 = db.conn.query_row("SELECT count(*) FROM file WHERE exif = 0 AND status = 0 AND format <> 'VIDEO'", [], |r| r.get(0)).unwrap_or(0);
    if unread > 0 && !cancelled() {
        match library::exif::backfill(db, 5_000, &CANCEL) {
            Ok(n) if n > 0 => {
                crate::log(&format!("series: shooting data of {n} photos read before grouping"));
                ROWS.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {}
            Err(e) => crate::log(&format!("series: reading shooting data failed: {e}")),
        }
    }
    // days whose photos are not complete wait (shooting data, previews) – but not for ever: after
    // [`WAIT_ROUNDS`] passes without any progress they are grouped as they are
    let give_up = STALLED.load(Ordering::Relaxed) >= WAIT_ROUNDS;
    let stats = library::series::regroup_ready(db, false, &cancelled, &|d, n| {
        set_phase(Phase::Grouping, d, n);
        test_slow();
    }, (!give_up).then_some(&waiting))?;
    if stats.waiting_days > 0 && !cancelled() {
        if done == 0 && stats.regrouped_days == 0 {
            STALLED.fetch_add(1, Ordering::Relaxed);
        } else {
            STALLED.store(0, Ordering::Relaxed);
        }
        // look again in a while, in this session (the previews and shooting data come in the background)
        let _ = std::thread::Builder::new().name("serien-warten".into()).spawn(|| {
            std::thread::sleep(WAIT_AGAIN);
            poke();
        });
    } else {
        STALLED.store(0, Ordering::Relaxed);
    }
    // quality hints for the members of stacks (preview ≤ 1024 px, faces with Vision on macOS)
    let todo = library::series_quality::stack_files_without_quality(db);
    MISSING.store(todo.len(), Ordering::Relaxed);
    set_phase(Phase::Quality, 0, todo.len());
    let mut measured = 0usize;
    for (k, (file, path, size, mtime, orientation, _)) in todo.iter().enumerate() {
        if cancelled() {
            break;
        }
        if let Some(img) = preview_for(cache, *file, path, *size, *mtime, *orientation) {
            let (q, _) = library::series_quality::measure_all(&img);
            library::series_quality::save(db, *file, &q)?;
            measured += 1;
            std::thread::sleep(if throttled() { PAUSE * 50 } else { PAUSE * 5 });
        }
        MISSING.store(todo.len() - k - 1, Ordering::Relaxed);
    }
    if measured > 0 {
        crate::log(&format!("series: quality of {measured} photos measured"));
    }
    if done > 0 || stats.regrouped_days > 0 {
        crate::log(&format!(
            "series: {done} signatures, {} of {} days grouped ({} new, {} kept, {} removed stacks) in {} ms",
            stats.regrouped_days,
            stats.days,
            stats.stacks_created,
            stats.stacks_kept,
            stats.stacks_removed,
            t.elapsed().as_millis()
        ));
    }
    Ok(stats.stacks_created + stats.stacks_removed > 0 || stats.regrouped_days > 0)
}
