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
        // a cargo test program gets a temp folder from `app_logs_dir`, never the real one
        None if sysdirs::test_program_dir().is_some() => Some((sysdirs::app_logs_dir(library::APP)?, "trace.log")),
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
        // A panic in the main thread must really end the process: unwinding through the event loop, other
        // threads or a hanging GPU driver kept an invisible process alive (holding the instance lock), so every
        // new start gave up. Terminate right here, without atexit handlers or joining threads.
        if name == "main" {
            hard_exit(101);
        }
    }));
}

/// Ends the process at once (no destructors, no atexit handlers, no waiting for other threads or the driver).
/// The OS frees the instance lock with the process.
pub fn hard_exit(code: i32) -> ! {
    crate::log(&format!("exit: process ends now (code {code})"));
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        TerminateProcess(GetCurrentProcess(), code as u32);
    }
    #[cfg(unix)]
    unsafe {
        unsafe extern "C" {
            fn _exit(code: i32) -> !;
        }
        _exit(code);
    }
    #[allow(unreachable_code)]
    std::process::exit(code)
}

/// The GPU device of the window was lost (driver reset, graphics switch, sleep). Set by wgpu's callback.
static DEVICE_LOST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Called by wgpu when the window's device is lost (not on a normal shutdown).
pub fn note_device_lost(reason: String) {
    crate::log(&format!("display: GPU device lost: {reason}"));
    if let Ok(mut d) = DEVICE_LOST.lock() {
        *d = Some(reason);
    }
}

/// Each frame: after a lost device the window cannot draw anymore (eframe cannot rebuild its painter). Instead
/// of a frozen or invisible window: crash-log entry and notice for the next start, a short message, then end.
pub fn check_device_lost() {
    let Some(reason) = DEVICE_LOST.lock().ok().and_then(|d| d.clone()) else { return };
    if let Some((dir, _)) = folder() {
        let text = entry("main", "GPU", &format!("GPU device lost: {reason}"), "");
        append(&dir.join("crash.log"), &text);
        let _ = std::fs::write(dir.join(NOTICE), &text);
    }
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("graineo")
            .set_description(crate::tr(
                "Die Grafikkarte wurde zurückgesetzt (Grafikwechsel oder Energiesparen). graineo wird beendet und kann sofort neu gestartet werden; Bearbeitungen sind gespeichert.",
            ))
            .show();
    }
    hard_exit(1);
}

/// Test hook `GRAINEO_STAGING_FAIL_TEST=<from>-<to>` (only with `GRAINEO_DATA_DIR`): between these seconds the egui
/// renderer behaves as if the GPU buffers could not be filled (as with a lost device): frames are skipped, no panic.
/// `GRAINEO_DEVICE_LOST_TEST=<s>`: reports a lost device after s seconds (the app ends with a crash-log entry).
pub fn test_gpu_failures(ctx: &eframe::egui::Context) {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let now = ctx.input(|i| i.time);
    if let Some((a, b)) = crate::env::var("GRAINEO_STAGING_FAIL_TEST").ok().and_then(|v| {
        let (a, b) = v.split_once('-')?;
        Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?))
    }) {
        let on = now >= a && now < b;
        eframe::egui_wgpu::TEST_STAGING_FAILURE.store(on, std::sync::atomic::Ordering::Relaxed);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    if let Some(t) = crate::env::var("GRAINEO_DEVICE_LOST_TEST").ok().and_then(|v| v.parse::<f64>().ok()) {
        if now >= t {
            note_device_lost("test (GRAINEO_DEVICE_LOST_TEST)".into());
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
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

/// Start log (`start.log` next to `crash.log`): one line per step of the start up to the window, and the reason of every
/// end before it (second instance, window could not be created). Appended and closed per line, so it survives an end
/// without a message; it never rotates the trace log of a graineo already running (unlike `crate::log`).
pub fn start_note(text: &str) {
    let Some((dir, _)) = folder() else { return };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let t = library::platform::local_time(now);
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} · graineo {} · pid {} · {text}\n",
        t.year,
        t.month,
        t.tag,
        t.hour,
        t.minute,
        t.second,
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    append_line(&dir.join("start.log"), &line);
}

/// Append one line; beyond [`START_LIMIT`] the file keeps only its newer half (at a line boundary).
fn append_line(path: &Path, line: &str) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > START_LIMIT) {
        if let Ok(old) = std::fs::read_to_string(path) {
            let mut from = old.len() / 2;
            while !old.is_char_boundary(from) {
                from += 1;
            }
            let rest = &old[from..];
            let rest = rest.find('\n').map_or("", |i| &rest[i + 1..]);
            let _ = std::fs::write(path, rest);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

const START_LIMIT: u64 = 128 * 1024;

/// A message the user sees although there is no window (yet). Windows: a message box in front of other windows (a box
/// without owner otherwise can open behind the window that has the focus, and the start looks like "nothing happens");
/// elsewhere the system's message box. Blocks until answered.
pub fn start_message(text: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (t, c) = (wide(text), wide("graineo"));
        // SAFETY: two zero-terminated UTF-16 strings that live across the call
        unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TOPMOST) };
    }
    #[cfg(not(windows))]
    {
        let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Warning).set_title("graineo").set_description(text).set_buttons(rfd::MessageButtons::Ok).show();
    }
}

#[cfg(test)]
mod start_log_tests {
    #[test]
    fn start_log_keeps_its_newer_half() {
        let dir = std::env::temp_dir().join(format!("graineo-start-log-{}", std::process::id()));
        let p = dir.join("start.log");
        for i in 0..6000 {
            super::append_line(&p, &format!("line {i:05} ……………………\n"));
        }
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.len() as u64 <= super::START_LIMIT + 100);
        assert!(s.ends_with("line 05999 ……………………\n"));
        assert!(s.starts_with("line "), "starts at a whole line");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
