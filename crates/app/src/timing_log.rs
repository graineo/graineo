//! Local timing log of normal use (setting "Zeitprotokoll", default on): one line per opened photo in `timings.log`
//! in the logs folder of the app – time, camera family and format, ISO class, time until the first picture and until
//! the exact view, GPU or processor path, rough load of the system, memory of the app and the operating mode. Never a
//! file name or a path.
//!
//! Written quietly by one background thread (no fsync per line, the UI thread only hands the line over). At 2 MB the
//! file becomes `timings.log.1` (one previous file, the older one is dropped). The performance report appends the last
//! 100 lines, so a computer can be judged in real use. Nothing is sent anywhere.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};

/// Size at which the log is rotated.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const FILE: &str = "timings.log";
pub const PREVIOUS: &str = "timings.log.1";
/// Setting key.
const SETTING: &str = "timingLog";

/// 0 = not read yet, 1 = on, 2 = off
static ENABLED: AtomicU8 = AtomicU8::new(0);

/// Is the timing log on (setting, default on)?
pub fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = library::settings::value(SETTING).as_bool().unwrap_or(true);
            ENABLED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Switch the timing log on or off (settings window).
pub fn set_enabled(on: bool) {
    ENABLED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
    library::settings::set(SETTING, serde_json::json!(on));
}

/// Folder of the log (logs folder of the app; with `GRAINEO_DATA_DIR` its `logs` folder).
pub fn folder() -> Option<PathBuf> {
    crate::crash_log::folder().map(|(d, _)| d)
}

/// ISO class of a photo: "<=200", "<=400" … "<=6400", ">6400"; "-" when unknown.
pub fn iso_class(iso: Option<u32>) -> &'static str {
    match iso {
        None | Some(0) => "-",
        Some(i) if i <= 200 => "<=200",
        Some(i) if i <= 400 => "<=400",
        Some(i) if i <= 800 => "<=800",
        Some(i) if i <= 1600 => "<=1600",
        Some(i) if i <= 3200 => "<=3200",
        Some(i) if i <= 6400 => "<=6400",
        Some(_) => ">6400",
    }
}

/// "ISO 400" anywhere in a summary line ("1/100 s · f/2,8 · ISO 400 · 45 mm").
pub fn iso_in(summary: &str) -> Option<u32> {
    let i = summary.find("ISO ")?;
    summary[i + 4..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

/// Camera family and ISO of an opened photo (no name, no path).
pub fn family_of(loaded: &crate::jobs::Loaded) -> (String, Option<u32>) {
    match &loaded.previews.foreign {
        None => {
            let f = if loaded.cal.is_quattro() {
                "Quattro"
            } else if loaded.cal.is_classic() {
                "Classic"
            } else {
                "Merrill"
            };
            (f.to_string(), iso_in(&loaded.summary))
        }
        Some(doc) => (raw_family(doc), doc.info.exif.iso),
    }
}

/// "Bayer CR3", "X-Trans RAF", "JPEG" …: sensor kind and container format of a non-Sigma photo.
pub fn raw_family(doc: &rawdev::RawDoc) -> String {
    let ext = doc.path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    if doc.finished {
        return if ext.is_empty() { "image".into() } else { ext };
    }
    let kind = match doc.info.sensor {
        raw::Sensor::Bayer => "Bayer",
        raw::Sensor::XTrans => "X-Trans",
        raw::Sensor::Linear => "Linear",
        raw::Sensor::Mono => "Mono",
        _ => "Mosaic",
    };
    format!("{kind} {}", if doc.info.format.is_empty() { ext } else { doc.info.format.to_uppercase() })
}

struct Pending {
    family: String,
    iso: Option<u32>,
    first_ms: f64,
    sigma: bool,
    /// processor ticks of the system when the photo appeared (load while it was computed)
    ticks: Option<(u64, u64)>,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// A photo appeared in the single view after `first_ms` (since it was chosen). `exact_already`: the exact view is
/// there at once (Sigma: base from memory or disk cache). A Sigma photo waits for [`exact`]; its line is written then,
/// or – when the user moves on before – with the next photo ("exact -").
pub fn shown(loaded: &crate::jobs::Loaded, first_ms: f64, exact_already: bool) {
    if !enabled() {
        return;
    }
    let (family, iso) = family_of(loaded);
    let sigma = loaded.previews.foreign.is_none();
    let earlier = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(p) = earlier {
        emit(p, None);
    }
    let p = Pending { family, iso, first_ms, sigma, ticks: library::platform::info::system_cpu_ticks() };
    if sigma && !exact_already {
        *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    } else {
        // non-Sigma: the fitted view is final at once; Sigma from cache: exact at once
        let exact = sigma.then_some(first_ms);
        emit(p, exact);
    }
}

/// The exact view of the photo now shown arrived `ms` after it was chosen.
pub fn exact(ms: f64) {
    let p = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(p) = p {
        emit(p, Some(ms));
    }
}

/// One line: time stamp, family, ISO class, times, path, load. Pure (tests).
#[allow(clippy::too_many_arguments)]
pub fn line(at: &library::platform::LocalTime, version: &str, family: &str, iso: Option<u32>, first_ms: f64, exact_ms: Option<f64>, sigma: bool, gpu: bool, cpu_busy: Option<f64>, memory: Option<u64>) -> String {
    let off = at.offset_val;
    let sign = if off < 0 { '-' } else { '+' };
    let stamp = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}", at.year, at.month, at.tag, at.hour, at.minute, at.second, off.abs() / 3600, off.abs() % 3600 / 60);
    let family: String = family.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
    let exact = match (sigma, exact_ms) {
        (false, _) => "n/a".to_string(),
        (true, Some(e)) => format!("{e:.0}"),
        (true, None) => "-".to_string(),
    };
    let cpu = cpu_busy.map_or("-".to_string(), |c| format!("{:.0}%", c * 100.0));
    let mem = memory.map_or("-".to_string(), |m| format!("{:.0}", m as f64 / 1_048_576.0));
    format!(
        "{stamp} v={version} family={family} iso={} first_ms={first_ms:.0} exact_ms={exact} path={} system_cpu={cpu} app_mem_mb={mem}",
        iso_class(iso),
        if gpu { "gpu" } else { "cpu" }
    )
}

enum Msg {
    Line { family: String, iso: Option<u32>, first_ms: f64, exact_ms: Option<f64>, sigma: bool, gpu: bool, busy: Option<f64> },
}

fn emit(p: Pending, exact_ms: Option<f64>) {
    // which path the exact view took: a Merrill base reports it (x3fuse-core); the others follow the setting
    let gpu = pools::gpu_allowed() && (p.family != "Merrill" || exact_ms.is_none() || develop::spp::last_used_gpu());
    // how busy the whole processor was between the photo appearing and now (rough)
    let busy = p.ticks.zip(library::platform::info::system_cpu_ticks()).and_then(|(a, b)| library::platform::info::busy_share(a, b));
    let msg = Msg::Line { family: p.family, iso: p.iso, first_ms: p.first_ms, exact_ms, sigma: p.sigma, gpu, busy };
    if let Some(tx) = writer() {
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(msg);
    }
}

fn writer() -> Option<&'static Mutex<Sender<Msg>>> {
    static TX: OnceLock<Option<Mutex<Sender<Msg>>>> = OnceLock::new();
    TX.get_or_init(|| {
        let dir = folder()?;
        let (tx, rx) = channel::<Msg>();
        std::thread::Builder::new()
            .name("timing-log".into())
            .spawn(move || {
                library::platform::background_priority();
                let version = library::update::current_version();
                let mut out = Log::new(dir);
                for Msg::Line { family, iso, first_ms, exact_ms, sigma, gpu, busy } in rx {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
                    let l = line(&library::platform::local_time(now), &version, &family, iso, first_ms, exact_ms, sigma, gpu, busy, library::platform::info::process_memory());
                    // the operating mode the app computed in (`perf_mode`): "exact" or "saving"
                    out.append(&format!("{l} mode={}", crate::perf_mode::tier_key()));
                }
            })
            .ok()?;
        Some(Mutex::new(tx))
    })
    .as_ref()
}

/// The log file with rotation.
pub struct Log {
    dir: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
    max: u64,
}

impl Log {
    pub fn new(dir: PathBuf) -> Log {
        Log { dir, file: None, size: 0, max: MAX_BYTES }
    }

    #[cfg(test)]
    pub fn with_limit(dir: PathBuf, max: u64) -> Log {
        Log { dir, file: None, size: 0, max }
    }

    /// Append one line (opens the file on first use, rotates when it would exceed the limit). Errors are ignored:
    /// the log must never disturb the app.
    pub fn append(&mut self, line: &str) {
        let path = self.dir.join(FILE);
        if self.file.is_none() {
            let _ = std::fs::create_dir_all(&self.dir);
            self.size = std::fs::metadata(&path).map_or(0, |m| m.len());
            self.file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        }
        let n = line.len() as u64 + 1;
        if self.size > 0 && self.size + n > self.max {
            self.file = None;
            self.file = if std::fs::rename(&path, self.dir.join(PREVIOUS)).is_ok() {
                std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()
            } else {
                // the file cannot be renamed (Windows: held open elsewhere): start it anew instead of letting it grow;
                // if that fails too, nothing more is written (the file is None)
                std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&path).ok()
            };
            self.size = 0;
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(format!("{line}\n").as_bytes()).is_ok() {
                self.size += n;
            }
        }
    }
}

/// The last `n` lines of the log (with the previous file when the current one is shorter).
pub fn last_lines(dir: &Path, n: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for name in [PREVIOUS, FILE] {
        if let Ok(t) = std::fs::read_to_string(dir.join(name)) {
            lines.extend(t.lines().filter(|l| !l.trim().is_empty()).map(str::to_string));
        }
    }
    let start = lines.len().saturating_sub(n);
    lines.split_off(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> library::platform::LocalTime {
        library::platform::LocalTime { year: 2026, month: 11, tag: 16, hour: 9, minute: 5, second: 7, offset_val: 3600 }
    }

    #[test]
    fn line_has_times_and_no_names() {
        let l = line(&at(), "1.2.0", "Merrill", Some(400), 431.6, Some(1290.2), true, true, Some(0.354), Some(1_258_291_200));
        assert_eq!(l, "2026-11-16T09:05:07+01:00 v=1.2.0 family=Merrill iso=<=400 first_ms=432 exact_ms=1290 path=gpu system_cpu=35% app_mem_mb=1200");
        let l = line(&at(), "1.2.0", "Bayer CR3", Some(6400), 120.0, None, false, false, None, None);
        assert!(l.contains("family=Bayer_CR3 iso=<=6400 first_ms=120 exact_ms=n/a path=cpu system_cpu=- app_mem_mb=-"), "{l}");
        // left before the exact view
        assert!(line(&at(), "1.2.0", "Quattro", None, 900.0, None, true, true, None, None).contains("iso=- first_ms=900 exact_ms=-"));
        // negative offset
        let west = library::platform::LocalTime { offset_val: -5 * 3600 - 1800, ..at() };
        assert!(line(&west, "1", "Merrill", None, 1.0, None, true, true, None, None).starts_with("2026-11-16T09:05:07-05:30 "));
    }

    #[test]
    fn iso_from_summary_and_classes() {
        assert_eq!(iso_in("1/100 s · f/2,8 · ISO 400 · 45 mm"), Some(400));
        assert_eq!(iso_in("ohne Angaben"), None);
        assert_eq!(iso_class(Some(100)), "<=200");
        assert_eq!(iso_class(Some(800)), "<=800");
        assert_eq!(iso_class(Some(12800)), ">6400");
        assert_eq!(iso_class(None), "-");
    }

    #[test]
    fn log_rotates_and_keeps_one_previous_file() {
        let dir = std::env::temp_dir().join(format!("graineo-timing-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut log = Log::with_limit(dir.clone(), 100);
        for i in 0..12 {
            log.append(&format!("line {i:02} {}", "x".repeat(20)));
        }
        let cur = std::fs::read_to_string(dir.join(FILE)).unwrap();
        let prev = std::fs::read_to_string(dir.join(PREVIOUS)).unwrap();
        assert!(cur.len() as u64 <= 100 && prev.len() as u64 <= 100, "{} {}", cur.len(), prev.len());
        assert!(cur.ends_with(&format!("line 11 {}\n", "x".repeat(20))));
        // only two files, the oldest lines are gone
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        assert!(!prev.contains("line 00") && !cur.contains("line 00"));
        // the last lines come from both files in order
        let last = last_lines(&dir, 4);
        assert_eq!(last.len(), 4);
        assert!(last[3].starts_with("line 11") && last[0].starts_with("line 08"), "{last:?}");
        // a new writer continues the existing file and respects its size
        let mut again = Log::with_limit(dir.clone(), 100);
        again.append("line 12");
        assert!(std::fs::read_to_string(dir.join(FILE)).unwrap().len() as u64 <= 100);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rotation that cannot rename (Windows: the file is held open elsewhere) starts the file anew: it never grows
    /// beyond the limit.
    #[test]
    fn log_stays_bounded_when_rename_fails() {
        let dir = std::env::temp_dir().join(format!("graineo-timing-log-norename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // a non-empty folder in the place of the previous file: renaming onto it fails
        std::fs::create_dir_all(dir.join(PREVIOUS)).unwrap();
        std::fs::write(dir.join(PREVIOUS).join("x"), b"x").unwrap();
        let mut log = Log::with_limit(dir.clone(), 100);
        for i in 0..50 {
            log.append(&format!("line {i:02} {}", "x".repeat(20)));
        }
        let cur = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(cur.len() as u64 <= 100, "{}", cur.len());
        assert!(cur.ends_with(&format!("line 49 {}\n", "x".repeat(20))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
