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
}
