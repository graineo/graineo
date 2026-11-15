//! Lasting traces of a crash: the trace log of the previous run is kept (`run-previous.log`, test runs
//! `trace-previous.log`) instead of being overwritten at startup, and every panic – in any thread – is appended to
//! `crash.log` with time, thread, message, place and backtrace. Both lie in the logs folder of the app
//! (`~/Library/Logs/graineo`, Windows `%LOCALAPPDATA%\graineo\Logs`), with `GRAINEO_DATA_DIR` in its `logs` folder.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The crash log never grows beyond this; older entries are dropped from the front.
const LIMIT: u64 = 512 * 1024;

/// Logs folder and the name of the trace log (test runs: own data folder).
pub fn folder() -> Option<(PathBuf, &'static str)> {
    match crate::env::var_os("GRAINEO_DATA_DIR") {
        Some(dir) => Some((PathBuf::from(dir).join("logs"), "trace.log")),
        None => Some((sysdirs::app_logs_dir(library::APP)?, "run.log")),
    }
}

/// Help → Show Logs: the logs folder in the Finder (Explorer); with a crash log, that file is selected.
pub fn show_folder() {
    let Some((dir, _)) = folder() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let crash = dir.join("crash.log");
    if crash.exists() {
        crate::system::show(&crash);
    } else {
        crate::system::open(&dir);
    }
}

/// Marker: the previous run ended with a panic in the main thread (its entry of the crash log).
const NOTICE: &str = "crash-new.txt";

/// At startup: did the previous run end with a crash? Answers once (the marker is removed).
pub fn take_notice() -> bool {
    let Some((dir, _)) = folder() else { return false };
    let marker = dir.join(NOTICE);
    let crashed = marker.exists();
    if crashed {
        let _ = std::fs::remove_file(&marker);
        crate::log("crash: the previous run ended with a crash (see crash.log)");
    }
    crashed
}

/// Name of the kept log of the previous run (`run.log` → `run-previous.log`).
pub fn previous_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-previous.{ext}"),
        None => format!("{name}-previous"),
    }
}

/// At startup, before the first line is written: keep the trace log of the previous run.
pub fn keep_previous(dir: &Path, name: &str) {
    let old = dir.join(name);
    if std::fs::metadata(&old).is_ok_and(|m| m.len() > 0) {
        let _ = std::fs::rename(&old, dir.join(previous_name(name)));
    }
}

/// Install the panic hook: crash log, trace log, then the default output (console).
pub fn install() {
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unbenannt").to_string();
        let message = match (info.payload().downcast_ref::<&str>(), info.payload().downcast_ref::<String>()) {
            (Some(s), _) => s.to_string(),
            (_, Some(s)) => s.clone(),
            _ => "(keine Meldung)".to_string(),
        };
        let place = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture();
        crate::log(&format!("crash: thread '{name}' at {place}: {message}"));
        if let Some((dir, _)) = folder() {
            let text = entry(&name, &place, &message, &backtrace.to_string());
            append(&dir.join("crash.log"), &text);
            // a panic in the main thread ends the app: the next start says so
            if name == "main" {
                let _ = std::fs::write(dir.join(NOTICE), &text);
            }
        }
        before(info);
    }));
}

/// Test hook `GRAINEO_PANIC_TEST=<s>` (only with `GRAINEO_DATA_DIR`): a panic in the main thread that many seconds
/// after start – checks crash log, kept trace log and the notice at the next start.
pub fn test_panic(ctx: &eframe::egui::Context) {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let Some(t) = crate::env::var("GRAINEO_PANIC_TEST").ok().and_then(|v| v.parse::<f64>().ok()) else { return };
    if ctx.input(|i| i.time) >= t {
        panic!("Test-Absturz (GRAINEO_PANIC_TEST)");
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
}

/// One entry of the crash log.
fn entry(thread: &str, place: &str, message: &str, backtrace: &str) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let t = library::platform::local_time(now);
    let main = if thread == "main" { " (Hauptthread – beendet die App)" } else { "" };
    format!(
        "==== {:04}-{:02}-{:02} {:02}:{:02}:{:02} · graineo {} ====\nThread: {thread}{main}\nOrt: {place}\nMeldung: {message}\n{backtrace}\n\n",
        t.year,
        t.month,
        t.tag,
        t.hour,
        t.minute,
        t.second,
        env!("CARGO_PKG_VERSION"),
    )
}

/// Append to the crash log; above [`LIMIT`] only the newer half is kept (at an entry boundary).
fn append(path: &Path, text: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() + text.len() as u64 > LIMIT) {
        if let Ok(old) = std::fs::read_to_string(path) {
            let mut keep_from = old.len().saturating_sub(LIMIT as usize / 2);
            while !old.is_char_boundary(keep_from) {
                keep_from += 1;
            }
            let rest = &old[keep_from..];
            let rest = rest.find("==== ").map_or("", |i| &rest[i..]);
            let _ = std::fs::write(path, rest);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
        let _ = f.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_log_is_kept() {
        assert_eq!(previous_name("run.log"), "run-previous.log");
        assert_eq!(previous_name("trace.log"), "trace-previous.log");
        let dir = std::env::temp_dir().join(format!("graineo-crashlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.log"), "crash: vorher\n").unwrap();
        keep_previous(&dir, "run.log");
        assert!(!dir.join("run.log").exists());
        assert_eq!(std::fs::read_to_string(dir.join("run-previous.log")).unwrap(), "crash: vorher\n");
        // an empty log does not replace the kept one
        std::fs::write(dir.join("run.log"), "").unwrap();
        keep_previous(&dir, "run.log");
        assert_eq!(std::fs::read_to_string(dir.join("run-previous.log")).unwrap(), "crash: vorher\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn crash_log_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("graineo-crashlog-limit-{}", std::process::id()));
        let path = dir.join("crash.log");
        let big = "x".repeat(40_000);
        for i in 0..40 {
            append(&path, &entry("main", "a.rs:1:1", &format!("Nr. {i}"), &big));
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() as u64 <= LIMIT, "{} bytes", text.len());
        assert!(text.starts_with("==== "), "starts at an entry");
        assert!(text.contains("Meldung: Nr. 39"), "newest entry kept");
        assert!(!text.contains("Meldung: Nr. 0\n"), "oldest entry dropped");
        assert!(text.contains("Hauptthread"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
