//! Quitting (⌘Q, "Beenden" in the Dock, closing the window): all paths end in `App::on_exit`. There the current
//! photo's settings are saved and pending library writes (ratings, keywords …) get a short moment to finish.
//! Everything else running in the background (offline copies, previews, sync) simply ends with the process:
//! files are written atomically, the library is SQLite with a journal, moves have their own journal.
//!
//! A guard makes sure the process really ends: if saving or tearing down hangs (e.g. a sidecar on a
//! network drive that does not answer), the process ends after [`LIMIT`] anyway – the photo's original and
//! its sidecar stay as they were.

use std::time::{Duration, Instant};

/// Longest time from "Beenden" until the process has ended, if no library write is pending.
pub const LIMIT: Duration = Duration::from_millis(2000);
/// Library writes still on their way (quitting from the Dock, which cannot be held back): the guard waits up to this
/// long for them, as long as they are pending.
pub const WRITES_LIMIT: Duration = Duration::from_secs(10);
/// Of that, the longest wait for pending library writes in `on_exit`.
pub const WRITES: Duration = Duration::from_secs(9);

/// Closing was asked for while changes are still on their way into the library: since when (the window stays open
/// and shows the wait).
static WAITING: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
/// Close now without waiting again (everything written, or the user chose to quit without it).
static LET_GO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Every frame: a request to close (window, ⌘Q from the menu, Ctrl+Q) while library writes are pending is held back;
/// a dialog shows the wait and closes the window once everything is written. If a write fails, the user decides
/// (try again / quit without these changes / keep working). `pending`: writes on their way; `unsaved`: writes that
/// failed and wait for another try. `save` is called once when the request comes in (current photo's settings).
pub fn hold_close(ctx: &eframe::egui::Context, pending: bool, unsaved: usize, save: impl FnOnce()) {
    use eframe::egui;
    use std::sync::atomic::Ordering;
    if hold_for_merge(ctx) {
        return;
    }
    let mut waiting = WAITING.lock().unwrap_or_else(|e| e.into_inner());
    if ctx.input(|i| i.viewport().close_requested()) && !LET_GO.load(Ordering::SeqCst) && waiting.is_none() {
        save();
        let pending = pending || library::store::saving();
        if pending || unsaved > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            library::store::retry_unsaved();
            *waiting = Some(Instant::now());
            crate::log(&format!("quit: waiting for library writes (unsaved {unsaved})"));
        }
    }
    let Some(since) = *waiting else { return };
    if !pending && unsaved == 0 {
        crate::log(&format!("quit: library writes done after {} ms", since.elapsed().as_millis()));
        *waiting = None;
        LET_GO.store(true, Ordering::SeqCst);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return;
    }
    drop(waiting);
    let failed = unsaved > 0 && !pending;
    let error = library::store::last_error();
    let answer = crate::dialog::show(
        ctx,
        "beenden-warten",
        if failed { crate::tr("Änderungen nicht gespeichert") } else { crate::tr("Änderungen werden gespeichert …") },
        380.0,
        |ui| {
            if failed {
                crate::dialog::warning(ui, &crate::trf!("{n} Änderungen konnten nicht in die Bibliothek geschrieben werden.", n = unsaved));
                if let Some(e) = &error {
                    crate::dialog::hint(ui, e);
                }
            } else {
                crate::dialog::text(ui, crate::tr("graineo beendet sich, sobald alle Änderungen in der Bibliothek stehen."));
                crate::dialog::progress(ui, None, &crate::dialog::duration(since.elapsed()));
            }
        },
        crate::dialog::Buttons { cancel: crate::tr("Weiterarbeiten"), primary: Some((crate::tr("Ohne diese Änderungen beenden"), crate::dialog::Style::Destructive, failed || since.elapsed() > Duration::from_secs(10))) },
    );
    match answer {
        crate::dialog::Answer::Primary => {
            crate::log(&format!("quit: user quits with {unsaved} unsaved changes"));
            *WAITING.lock().unwrap_or_else(|e| e.into_inner()) = None;
            LET_GO.store(true, Ordering::SeqCst);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        crate::dialog::Answer::Cancel => *WAITING.lock().unwrap_or_else(|e| e.into_inner()) = None,
        crate::dialog::Answer::Open => ctx.request_repaint_after(Duration::from_millis(100)),
    }
}

/// Quitting while an HDR merge runs: `None` no question, `Some(false)` asked, `Some(true)` the user chose to wait.
static MERGE_ASK: std::sync::Mutex<Option<bool>> = std::sync::Mutex::new(None);
/// The user cancelled the merges to quit: do not ask again while they wind down.
static MERGE_LET_GO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a running merge shows in the quit question: lines (name, percent, time left) and the progress of the first.
pub struct MergeRunning {
    pub lines: Vec<String>,
    pub fraction: Option<f32>,
}

/// HDR merges running now (a batch between two brackets counts as running).
fn merge_running() -> Option<MergeRunning> {
    if let Some(r) = test_merge() {
        return Some(r);
    }
    let jobs = rawdev::merged::jobs();
    if jobs.is_empty() && !crate::hdr_batch::active() {
        return None;
    }
    let fraction = jobs.first().map(|j| merge_fraction(crate::hdr_batch::position(j.file), j.overall()));
    let mut lines = crate::hdr_ui::activities();
    if lines.is_empty() {
        lines.push(crate::tr("HDR-Reihe: nächste Aufnahme wird vorbereitet").to_string());
    }
    Some(MergeRunning { lines, fraction })
}

/// Progress of the whole run: a batch "Reihe k von n" counts its finished brackets.
fn merge_fraction(position: Option<(usize, usize)>, overall: f32) -> f32 {
    match position {
        Some((k, n)) if n > 0 => ((k.saturating_sub(1)) as f32 + overall) / n as f32,
        _ => overall,
    }
}

/// Test hook (only with `GRAINEO_DATA_DIR`): `GRAINEO_QUIT_HDR_TEST=<s>` acts as if an HDR merge ran for the first
/// `s` seconds after the start (combine with `GRAINEO_QUIT=<s>` to ask for quitting meanwhile).
fn test_merge() -> Option<MergeRunning> {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let secs: f32 = crate::env::var("GRAINEO_DATA_DIR").ok().and(crate::env::var("GRAINEO_QUIT_HDR_TEST").ok())?.parse().ok()?;
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    if t >= secs || MERGE_LET_GO.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    let p = t / secs;
    let left = Duration::from_secs_f32((secs - t).max(1.0));
    let line = crate::trf!("HDR {a} {p} % · noch etwa {t}", a = "IMG_0412-HDR", p = (p * 100.0).floor() as u32, t = crate::dialog::duration(left));
    Some(MergeRunning { lines: vec![line], fraction: Some(p) })
}

/// Before [`hold_close`]: a request to close while an HDR merge runs is held back and asked about (what runs, how
/// far; "Warten" quits once it is done, "Abbrechen und beenden" stops it and quits, Esc keeps working). Returns
/// whether the close is held this frame.
fn hold_for_merge(ctx: &eframe::egui::Context) -> bool {
    use eframe::egui;
    use std::sync::atomic::Ordering;
    let requested = ctx.input(|i| i.viewport().close_requested()) && !LET_GO.load(Ordering::SeqCst) && !MERGE_LET_GO.load(Ordering::SeqCst);
    let mut ask = MERGE_ASK.lock().unwrap_or_else(|e| e.into_inner());
    let running = if requested || ask.is_some() { merge_running() } else { None };
    if requested && (ask.is_some() || running.is_some()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if ask.is_none() {
            *ask = Some(false);
            crate::log("quit: HDR merge running, asking");
        }
    }
    let Some(waiting) = *ask else { return false };
    let Some(run) = running else {
        // done: quit if the user chose to wait, otherwise just close the question
        *ask = None;
        if waiting {
            crate::log("quit: HDR merge done, quitting");
            MERGE_LET_GO.store(true, Ordering::SeqCst);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        return true;
    };
    drop(ask);
    let title = if waiting { crate::tr("Warten auf die HDR-Berechnung …") } else { crate::tr("HDR wird noch berechnet") };
    let (answer, cancel_quit) = crate::dialog::show_extra(
        ctx,
        "beenden-hdr",
        title,
        400.0,
        |ui| {
            if waiting {
                crate::dialog::text(ui, crate::tr("graineo beendet sich, sobald die Berechnung fertig ist."));
            } else {
                crate::dialog::text(ui, crate::tr("Beim Beenden geht die laufende Berechnung verloren."));
            }
            for (i, line) in run.lines.iter().enumerate() {
                if i == 0 {
                    crate::dialog::progress(ui, run.fraction, line);
                } else {
                    crate::dialog::hint(ui, line);
                }
            }
        },
        if waiting {
            crate::dialog::Buttons { cancel: crate::tr("Weiterarbeiten"), primary: None }
        } else {
            crate::dialog::Buttons { cancel: "", primary: Some((crate::tr("Warten"), crate::dialog::Style::Normal, true)) }
        },
        Some(crate::tr("Abbrechen und beenden")),
    );
    if cancel_quit {
        crate::log("quit: user cancels the HDR merge and quits");
        *MERGE_ASK.lock().unwrap_or_else(|e| e.into_inner()) = None;
        MERGE_LET_GO.store(true, Ordering::SeqCst);
        crate::hdr_ui::cancel_all();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        return true;
    }
    match answer {
        crate::dialog::Answer::Primary => *MERGE_ASK.lock().unwrap_or_else(|e| e.into_inner()) = Some(true),
        crate::dialog::Answer::Cancel => *MERGE_ASK.lock().unwrap_or_else(|e| e.into_inner()) = None,
        crate::dialog::Answer::Open => {}
    }
    ctx.request_repaint_after(Duration::from_millis(200));
    true
}

/// `on_exit` is waiting for library writes (the guard then waits as well, up to [`WRITES_LIMIT`]).
pub static ON_EXIT_WAITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start the guard (once). Not in tests: it would end the test runner.
pub fn start_guard() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if cfg!(test) || STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let limit = crate::env::var("GRAINEO_DATA_DIR").ok().and(crate::env::var("GRAINEO_QUIT_LIMIT_MS").ok()).and_then(|v| v.parse().ok()).map_or(LIMIT, Duration::from_millis);
    let _ = std::thread::Builder::new().name("beenden".into()).spawn(move || {
        let t = Instant::now();
        std::thread::sleep(limit);
        // develop settings or ratings still being written: give them their time (never longer than WRITES_LIMIT)
        while (library::store::saving() || ON_EXIT_WAITING.load(std::sync::atomic::Ordering::SeqCst)) && t.elapsed() < WRITES_LIMIT {
            std::thread::sleep(Duration::from_millis(50));
        }
        crate::log(&format!("quit: not finished after {} ms – ending the process", limit.as_millis()));
        extern "C" {
            fn _exit(code: i32) -> !;
        }
        // SAFETY: ends the process at once, without exit handlers that might hang themselves
        unsafe { _exit(0) }
    });
}

/// Wait (polling) until `done` holds, at most `max`. Returns whether it held.
pub fn wait_until(max: Duration, mut done: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    loop {
        if done() {
            return true;
        }
        if t.elapsed() >= max {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Test hook (only with `GRAINEO_DATA_DIR`): `GRAINEO_QUIT_HANG_MS=<ms>` simulates a hanging save on quitting.
pub fn test_hang() {
    if let Some(ms) = crate::env::var("GRAINEO_DATA_DIR").ok().and(crate::env::var("GRAINEO_QUIT_HANG_MS").ok()).and_then(|v| v.parse::<u64>().ok()) {
        crate::log(&format!("quit: test hang {ms} ms"));
        std::thread::sleep(Duration::from_millis(ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wait_until_is_bounded() {
        let t = Instant::now();
        assert!(!wait_until(Duration::from_millis(50), || false));
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(wait_until(Duration::from_millis(50), || true));
        assert!(LIMIT < WRITES && WRITES < WRITES_LIMIT);
    }

    #[test]
    fn merge_fraction_counts_batch() {
        assert_eq!(merge_fraction(None, 0.4), 0.4);
        assert_eq!(merge_fraction(Some((1, 4)), 0.0), 0.0);
        assert_eq!(merge_fraction(Some((3, 4)), 0.5), 0.625);
        assert_eq!(merge_fraction(Some((4, 4)), 1.0), 1.0);
    }

    /// Nothing runs and no question is open: closing is not held.
    #[test]
    fn no_merge_no_question() {
        assert!(merge_running().is_none());
        let ctx = eframe::egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| assert!(!hold_for_merge(ui.ctx())));
    }
}
