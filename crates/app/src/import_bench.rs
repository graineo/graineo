//! Test aid `GRAINEO_IMPORT_BENCH="out=<file.json>;timeout=600;after=120"` (only with `GRAINEO_DATA_DIR`), used together with
//! `GRAINEO_LIBRARY_TEST="import=<folder>"` (opens the import window on that folder), `GRAINEO_IMPORT_MODE=copy|inplace`,
//! `GRAINEO_IMPORT_YES=<s>` (starts the import) and `GRAINEO_IMPORT_SHOW=0` (afterwards the grid shows the imported
//! photos). Records:
//!
//! - the import window: time from reading the source until the first list is there and until reading is complete
//!   (capture dates, checksums), until the first thumbnail shows in its grid and until every visible cell shows its
//!   thumbnail (only when copying: "in place" shows the list of folders instead of thumbnails);
//! - the run: photos, time, photos per second;
//! - frame intervals of the app while the window is open, split into "before the run" and "during the run";
//! - afterwards the library grid with the imported photos: time from the end of the run until the first tile and until
//!   every visible tile shows its thumbnail (for "in place" the measure of how fast new photos appear: their previews
//!   are read from the originals only now).
//!
//! The result goes into `out` (JSON), a summary line into the trace log, then the app closes. Without the variable
//! every hook returns at once.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

struct State {
    out: PathBuf,
    timeout_s: f64,
    /// longest wait for the grid after the run (s)
    after_s: f64,
    began: Instant,
    scan_started: Option<Instant>,
    listed: Option<(Instant, usize)>,
    scan_done: Option<Instant>,
    first_thumb: Option<Instant>,
    all_visible: Option<(Instant, usize)>,
    run_started: Option<(Instant, usize)>,
    run_finished: Option<(Instant, usize, usize)>,
    grid_complete: Option<(Instant, usize)>,
    grid_first: Option<Instant>,
    last_frame: Option<Instant>,
    frames_before: Vec<f64>,
    frames_run: Vec<f64>,
    frames_after: Vec<f64>,
    done: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Is the import bench switched on (cheap after the first call)?
pub fn active() -> bool {
    *ON.get_or_init(|| {
        let Some(v) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_IMPORT_BENCH").ok()) else { return false };
        let mut out = PathBuf::from("import-bench.json");
        let mut timeout_s = 600.0;
        let mut after_s = 120.0;
        for part in v.split(';') {
            match part.split_once('=') {
                Some(("out", p)) => out = PathBuf::from(p.trim()),
                Some(("timeout", t)) => timeout_s = t.trim().parse().unwrap_or(timeout_s),
                Some(("after", t)) => after_s = t.trim().parse().unwrap_or(after_s),
                _ => {}
            }
        }
        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(State {
            out,
            timeout_s,
            after_s,
            began: Instant::now(),
            scan_started: None,
            listed: None,
            scan_done: None,
            first_thumb: None,
            all_visible: None,
            run_started: None,
            run_finished: None,
            grid_complete: None,
            grid_first: None,
            last_frame: None,
            frames_before: Vec::new(),
            frames_run: Vec::new(),
            frames_after: Vec::new(),
            done: false,
        });
        true
    })
}

fn with(f: impl FnOnce(&mut State)) {
    if !active() {
        return;
    }
    if let Some(s) = STATE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        f(s);
    }
}

/// The source is being read.
pub fn scan_started() {
    with(|s| {
        s.scan_started.get_or_insert(Instant::now());
    });
}

/// Every frame of the import window: photos listed so far and whether reading the source is complete.
pub fn listed(n: usize, complete: bool) {
    with(|s| {
        let now = Instant::now();
        if n > 0 && s.listed.is_none() {
            s.listed = Some((now, n));
        }
        if n > 0 && complete && s.scan_done.is_none() {
            s.scan_done = Some(now);
            s.listed = s.listed.map(|l| (l.0, n));
        }
    });
}

/// Every frame of the import window's grid: visible cells and how many show their thumbnail; `listed` = photos found.
pub fn window_grid(listed: usize, visible: usize, with_thumb: usize) {
    with(|s| {
        let now = Instant::now();
        if listed > 0 && s.listed.is_none() {
            s.listed = Some((now, listed));
        }
        if with_thumb > 0 && s.first_thumb.is_none() {
            s.first_thumb = Some(now);
        }
        if visible > 0 && with_thumb == visible && s.all_visible.is_none() {
            s.all_visible = Some((now, visible));
        }
    });
}

/// The import starts with `files` photos.
pub fn run_started(files: usize) {
    with(|s| {
        s.run_started.get_or_insert((Instant::now(), files));
    });
}

/// The import is finished.
pub fn run_finished(copied: usize, errors: usize) {
    with(|s| {
        s.run_finished.get_or_insert((Instant::now(), copied, errors));
    });
}

/// Every frame of the app while the bench runs (frame intervals); repaints continuously. Returns `true` once the
/// report is written (the caller closes the window).
pub fn frame(ctx: &eframe::egui::Context) -> bool {
    if !active() {
        return false;
    }
    ctx.request_repaint();
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = guard.as_mut() else { return false };
    if s.done {
        return false;
    }
    let now = Instant::now();
    if let Some(l) = s.last_frame {
        let ms = now.duration_since(l).as_secs_f64() * 1000.0;
        match (s.run_started, s.run_finished) {
            (None, _) => s.frames_before.push(ms),
            (Some(_), None) => s.frames_run.push(ms),
            _ => s.frames_after.push(ms),
        }
    }
    s.last_frame = Some(now);
    let after = s.run_finished.map(|f| now.duration_since(f.0).as_secs_f64());
    let finished = (s.grid_complete.is_some() && after.is_some_and(|a| a > 1.0)) || after.is_some_and(|a| a > s.after_s) || now.duration_since(s.began).as_secs_f64() > s.timeout_s;
    if !finished {
        return false;
    }
    s.done = true;
    let json = report(s);
    if let Some(p) = s.out.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(&s.out, serde_json::to_string_pretty(&json).unwrap_or_default());
    crate::log(&format!("import-bench: {json}"));
    true
}

/// Library grid after the import: (visible tiles, with thumbnail).
pub fn grid(visible: usize, with_thumb: usize) {
    with(|s| {
        if s.run_finished.is_some() && s.grid_first.is_none() && with_thumb > 0 {
            s.grid_first = Some(Instant::now());
        }
        if s.run_finished.is_some() && s.grid_complete.is_none() && visible > 0 && visible == with_thumb {
            s.grid_complete = Some((Instant::now(), visible));
        }
    });
}

fn report(s: &State) -> serde_json::Value {
    let since = |a: Option<Instant>, b: Option<Instant>| match (a, b) {
        (Some(a), Some(b)) => Some((b.duration_since(a).as_secs_f64() * 10000.0).round() / 10.0),
        _ => None,
    };
    let frames = |v: &[f64]| {
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.total_cmp(b));
        let q = |p: f64| if s.is_empty() { 0.0 } else { (s[((s.len() - 1) as f64 * p).round() as usize] * 10.0).round() / 10.0 };
        let n = s.len().max(1) as f64;
        let over = |ms: f64| (s.iter().filter(|&&v| v > ms).count() as f64 * 1000.0 / n).round() / 10.0;
        serde_json::json!({ "n": s.len(), "mean": ((s.iter().sum::<f64>() / n) * 10.0).round() / 10.0, "median": q(0.5), "p95": q(0.95), "p99": q(0.99), "max": q(1.0), "over_16_7_pct": over(1000.0 / 60.0), "over_33_pct": over(1000.0 / 30.0) })
    };
    let run_s = match (s.run_started, s.run_finished) {
        (Some(a), Some(b)) => Some(b.0.duration_since(a.0).as_secs_f64()),
        _ => None,
    };
    let files = s.run_started.map(|r| r.1);
    serde_json::json!({
        "tool": "import-bench",
        "version": 1,
        "mode": crate::env::var("GRAINEO_IMPORT_MODE").unwrap_or_default(),
        "photos_listed": s.listed.map(|l| l.1),
        "window": {
            "list_ms": since(s.scan_started, s.listed.map(|l| l.0)),
            "read_complete_ms": since(s.scan_started, s.scan_done),
            "first_thumbnail_ms": since(s.scan_started, s.first_thumb),
            "all_visible_ms": since(s.scan_started, s.all_visible.map(|a| a.0)),
            "visible_cells": s.all_visible.map(|a| a.1),
        },
        "run": {
            "photos": files,
            "copied": s.run_finished.map(|f| f.1),
            "errors": s.run_finished.map(|f| f.2),
            "seconds": run_s.map(|v| (v * 100.0).round() / 100.0),
            "photos_per_s": match (files, run_s) { (Some(n), Some(t)) if t > 0.0 => Some((n as f64 / t * 10.0).round() / 10.0), _ => None },
        },
        "grid_after": {
            "first_tile_ms_after_run": since(s.run_finished.map(|f| f.0), s.grid_first),
            "all_visible_ms_after_run": since(s.run_finished.map(|f| f.0), s.grid_complete.map(|g| g.0)),
            "visible_tiles": s.grid_complete.map(|g| g.1),
        },
        "frames": { "before_run": frames(&s.frames_before), "during_run": frames(&s.frames_run), "after_run": frames(&s.frames_after) },
        "timed_out": s.grid_complete.is_none(),
        "process_memory_bytes": library::platform::info::process_memory(),
    })
}
