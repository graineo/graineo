//! Help → Report a Problem …: shows an anonymised report (version, system, graphics, the last crash, the
//! last lines of the trace log) and opens a pre-filled issue in the browser; the user sends it with their
//! own GitHub account. Nothing is ever sent by the app itself: no key in the app, no upload.
//!
//! Anonymised: home folder → `~`, every path → `~/…/Datei.ext` (source files of a backtrace keep
//! `src/file.rs:line`), photo file names → `Datei.ext`, quoted text (search words, names) → `…`, user
//! and computer name, e-mail addresses, and every keyword, person, collection and folder name of the
//! library → `‹…›`. The window shows exactly what would be sent; the user can read it first.
//!
//! Opens from the Help menu and once after a crash (the previous run ended with a panic in the main
//! thread). Test hook: `GRAINEO_DIALOG=report-bug` (screenshots; `error` is the error dialog of the library).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use eframe::egui;

use crate::dialog;
use crate::lr::{self, Tip};

/// Address for reports by e-mail. `None` until a project address exists – then the button is hidden.
/// TODO before going live: enter the project address (FAHRPLAN "Vor dem Livegang" item 6).
pub const REPORT_EMAIL: Option<&str> = None;
/// GitHub refuses longer addresses (HTTP 414); the rest goes through the clipboard.
const MAX_URL: usize = 7800;
/// Lines of the trace log in the report.
const LOG_LINES: usize = 80;
/// Lines of the last crash entry (message, place, backtrace).
const CRASH_LINES: usize = 40;

static OPEN: AtomicBool = AtomicBool::new(false);

struct State {
    /// built in the background when the window opens
    report: Option<Report>,
    crashed: bool,
    with_log: bool,
    /// time of the last "copied" (for the short confirmation)
    copied: f64,
}

static STATE: Mutex<State> = Mutex::new(State { report: None, crashed: false, with_log: true, copied: f64::NEG_INFINITY });

/// Open the window; `crashed`: shown after a crash (other title and text).
pub fn open(crashed: bool) {
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        st.report = None;
        st.crashed = crashed;
        st.with_log = true;
    }
    std::thread::Builder::new()
        .name("fehlerbericht".into())
        .spawn(|| {
            let r = Report::collect();
            STATE.lock().unwrap_or_else(|e| e.into_inner()).report = Some(r);
        })
        .ok();
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Every frame: the window, while open.
pub fn frame(ctx: &egui::Context) {
    if !is_open() {
        return;
    }
    let (report, crashed, mut with_log, copied) = {
        let st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        (st.report.clone(), st.crashed, st.with_log, st.copied)
    };
    if report.is_none() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let now = ctx.input(|i| i.time);
    let title = if crashed { "graineo wurde unerwartet beendet" } else { "Fehler melden" };
    let mut copy = false;
    let answer = dialog::show(
        ctx,
        "fehler-melden",
        title,
        560.0,
        |ui| {
            if crashed {
                dialog::text(ui, crate::tr("Beim letzten Mal ist graineo abgestürzt. Hilf mit, den Fehler zu finden: Beschreibe kurz, was du gerade gemacht hast, und sende den Bericht über GitHub."));
            } else {
                dialog::text(ui, crate::tr("Beschreibe auf GitHub kurz, was passiert ist. Der Bericht unten wird dort eingefügt."));
            }
            dialog::hint(ui, crate::tr("Ohne Pfade, Dateinamen, Personennamen und Stichwörter. graineo sendet nichts selbst – du siehst den Bericht vorher im Browser und sendest ihn mit deinem GitHub-Konto."));
            ui.add_space(4.0);
            let text = report.as_ref().map_or_else(|| crate::tr("Bericht wird erstellt …").to_string(), |r| r.text(with_log));
            egui::Frame::new().fill(egui::Color32::from_gray(0x16)).stroke(egui::Stroke::new(1.0, lr::FIELD_STROKE)).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(260.0).min_scrolled_height(260.0).auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(text).monospace().size(10.5).color(lr::TEXT)).wrap().selectable(true));
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                dialog::toggle(ui, &mut with_log);
                ui.label(egui::RichText::new(crate::tr("Protokollzeilen anhängen")).color(lr::TEXT));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let ready = report.is_some();
                let b = ui.add_enabled(ready, egui::Button::new(crate::tr("Bericht kopieren")).min_size(egui::vec2(120.0, 24.0)).corner_radius(0.0));
                if b.clone().tip(ui, "Ganzen Bericht in die Zwischenablage, zum Einfügen ins Issue (für lange Protokolle)").clicked() {
                    copy = true;
                }
                if ui.add(egui::Button::new(crate::tr("Protokolle anzeigen")).min_size(egui::vec2(120.0, 24.0)).corner_radius(0.0)).clicked() {
                    crate::crash_log::show_folder();
                }
                if let (Some(addr), Some(r)) = (REPORT_EMAIL, report.as_ref()) {
                    if ui.add(egui::Button::new(crate::tr("Per E-Mail")).min_size(egui::vec2(100.0, 24.0)).corner_radius(0.0)).tip(ui, "Öffnet dein Mailprogramm mit dem Bericht; gesendet wird erst, wenn du dort sendest").clicked() {
                        crate::system::open(mailto(addr, &r.title(), &r.text(with_log)));
                    }
                }
                if now - copied < 2.0 {
                    ui.label(egui::RichText::new(crate::tr("Kopiert")).small().color(lr::TEXT_DIM));
                }
            });
        },
        dialog::Buttons { cancel: "Schließen", primary: Some(("Auf GitHub melden", dialog::Style::Normal, report.is_some())) },
    );
    let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
    st.with_log = with_log;
    if copy {
        if let Some(r) = &report {
            ctx.copy_text(r.text(with_log));
            st.copied = now;
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
    }
    match answer {
        dialog::Answer::Primary => {
            if let Some(r) = &report {
                let url = r.issue_url(with_log);
                crate::log(&format!("report: issue page opened ({} characters)", url.len()));
                crate::system::open(&url);
            }
            OPEN.store(false, Ordering::SeqCst);
        }
        dialog::Answer::Cancel => OPEN.store(false, Ordering::SeqCst),
        dialog::Answer::Open => {}
    }
}

// ─────────────────────────────────────────────── the report

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub version: String,
    /// (label, value) lines of the system section
    pub system: Vec<(String, String)>,
    /// anonymised excerpt of the last crash entry
    pub crash: Vec<String>,
    /// anonymised last lines of the trace log
    pub log: Vec<String>,
}

impl Report {
    /// Collect everything (background thread: runs `sw_vers`, `sysctl`, reads the logs and the names of the library).
    pub fn collect() -> Report {
        let redact = Redact::collect();
        let mut log = Vec::new();
        let mut crash = Vec::new();
        if let Some((dir, name)) = crate::crash_log::folder() {
            if let Ok(t) = std::fs::read_to_string(dir.join("crash.log")) {
                if let Some(i) = t.rfind("==== ") {
                    crash = own_frames(&t[i..]).into_iter().take(CRASH_LINES).map(|l| redact.line(l)).collect();
                }
            }
            // the log of this run; after a crash the one of the run that ended (kept as "-previous")
            let mut lines: Vec<String> = Vec::new();
            for n in [crate::crash_log::previous_name(name), name.to_string()] {
                if let Ok(t) = std::fs::read_to_string(dir.join(n)) {
                    lines.extend(t.lines().map(str::to_string));
                }
            }
            let start = lines.len().saturating_sub(LOG_LINES);
            log = lines[start..].iter().map(|l| redact.line(l)).collect();
        }
        Report { version: library::update::current_version(), system: system_info(), crash, log }
    }

    pub fn title(&self) -> String {
        let os = self.system.iter().find(|(k, _)| k == "System").map(|(_, v)| v.clone()).unwrap_or_default();
        format!("Problem in graineo {} ({os})", self.version)
    }

    fn body(&self, log: &[String], crash: &[String], note: bool) -> String {
        let mut b = String::from("**What happened?**\n<!-- Please describe what you did and what went wrong. Screenshots help. -->\n\n\n**System**\n");
        for (k, v) in &self.system {
            b.push_str(&format!("- {k}: {v}\n"));
        }
        if !crash.is_empty() {
            b.push_str("\n**Last crash**\n```\n");
            for l in crash {
                b.push_str(l);
                b.push('\n');
            }
            b.push_str("```\n");
        }
        if !log.is_empty() {
            b.push_str("\n**Log (last lines, anonymised)**\n```\n");
            for l in log {
                b.push_str(l);
                b.push('\n');
            }
            b.push_str("```\n");
        }
        if note {
            b.push_str("\n_Shortened. The full report is in the clipboard (graineo → Help → Report a Problem → Copy Report)._\n");
        }
        b
    }

    /// The whole report (shown in the window, copied to the clipboard).
    pub fn text(&self, with_log: bool) -> String {
        self.body(if with_log { &self.log } else { &[] }, &self.crash, false)
    }

    /// Address of a new, pre-filled issue; long logs are cut from the oldest line on (at most [`MAX_URL`]).
    pub fn issue_url(&self, with_log: bool) -> String {
        let title = self.title();
        let make = |body: &str| format!("{}?title={}&body={}", library::update::NEW_ISSUE_URL, encode(&title), encode(body));
        let mut log: &[String] = if with_log { &self.log } else { &[] };
        let mut crash: &[String] = &self.crash;
        let mut url = make(&self.body(log, crash, false));
        while url.len() > MAX_URL {
            if !log.is_empty() {
                log = &log[(log.len() / 4).max(1)..];
            } else if crash.len() > 3 {
                crash = &crash[..crash.len() * 3 / 4];
            } else {
                // only the system section is left
                return make(&self.body(&[], &[], true));
            }
            url = make(&self.body(log, crash, true));
        }
        url
    }
}

/// A crash entry without the frames of the runtime (panic machinery, std, core): header lines and the
/// frames of the app and its libraries, each with its `at …` line.
fn own_frames(entry: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for l in entry.lines().filter(|l| !l.trim().is_empty()) {
        let t = l.trim_start();
        let frame = t.split_once(": ").filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).map(|(_, f)| f);
        if let Some(f) = frame {
            skip = ["std::", "core::", "alloc::", "<std::", "<core::", "<alloc::", "backtrace", "rust_begin_unwind", "__rust", "_main", "start", "<unknown>"].iter().any(|p| f.starts_with(p)) || f.contains("::panicking::") || f.contains("crash_log::install");
        } else if t.starts_with("at ") {
            // belongs to the frame above
        } else {
            skip = false;
        }
        if !skip {
            out.push(l);
        }
    }
    out
}

/// `mailto:` with subject and text (only when [`REPORT_EMAIL`] is set).
fn mailto(addr: &str, subject: &str, body: &str) -> String {
    let body: String = body.chars().take(1800).collect();
    format!("mailto:{addr}?subject={}&body={}", encode(subject), encode(&body))
}

/// Percent-encoding for a URL query value (everything except the unreserved characters).
pub fn encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Version, system, computer (model class, processor, memory), graphics, language.
fn system_info() -> Vec<(String, String)> {
    let mut v = vec![("graineo".to_string(), format!("{} ({}, {}{})", library::update::current_version(), std::env::consts::OS, std::env::consts::ARCH, if cfg!(debug_assertions) { ", debug" } else { "" }))];
    let out = |p: &str, a: &[&str]| std::process::Command::new(p).args(a).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
    #[cfg(target_os = "macos")]
    {
        let ver = out("/usr/bin/sw_vers", &["-productVersion"]).unwrap_or_default();
        let build = out("/usr/bin/sw_vers", &["-buildVersion"]).map(|b| format!(" ({b})")).unwrap_or_default();
        v.push(("System".into(), format!("macOS {ver}{build}")));
        let model = out("/usr/sbin/sysctl", &["-n", "hw.model"]).unwrap_or_default();
        let cpu = out("/usr/sbin/sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_default();
        let mem = out("/usr/sbin/sysctl", &["-n", "hw.memsize"]).and_then(|m| m.parse::<u64>().ok()).map(|b| format!("{} GB", (b as f64 / 1_073_741_824.0).round())).unwrap_or_default();
        v.push(("Computer".into(), [model, cpu, mem].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")));
    }
    #[cfg(windows)]
    {
        let ver = out("cmd", &["/C", "ver"]).unwrap_or_default();
        v.push(("System".into(), ver.trim().to_string()));
        let cpu = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default();
        let n = std::thread::available_parallelism().map_or(0, |n| n.get());
        v.push(("Computer".into(), format!("{cpu} · {n} threads")));
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = out;
    let gpu = if pools::gpu_allowed() { rawdev::gpu::gpu().map_or_else(|| "none usable (CPU)".to_string(), |g| g.name.clone()) } else { "off (setting)".to_string() };
    v.push(("GPU".into(), gpu));
    v.push(("Language".into(), format!("{:?}", crate::menu::i18n::language())));
    v
}

// ─────────────────────────────────────────────── anonymising

/// What must not leave the computer.
#[derive(Default, Clone)]
pub struct Redact {
    pub home: String,
    /// user name, computer name
    pub own: Vec<String>,
    /// keywords, persons, collections, folders of the library (lower case)
    pub names: Vec<String>,
}

/// Photo, sidecar and video endings: such file names become `Datei.<ext>`.
const FILE_EXT: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "png", "tif", "tiff", "dng", "x3f", "cr2", "cr3", "crw", "raf", "arw", "srf", "sr2", "nef", "nrw", "orf", "rw2", "pef", "srw", "3fr", "iiq", "erf", "mos", "gpr", "psd", "xmp", "mov", "mp4", "m4v", "avi", "mts", "webp", "avif", "gif", "bmp", "lrcat", "lrdata", "pdf", "txt", "csv", "zip",
];

impl Redact {
    fn collect() -> Redact {
        let home = sysdirs::home().map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string()).unwrap_or_default();
        let mut own = Vec::new();
        if let Some(user) = std::path::Path::new(&home).file_name() {
            own.push(user.to_string_lossy().to_string());
        }
        for v in ["USER", "USERNAME", "COMPUTERNAME"] {
            if let Ok(u) = std::env::var(v) {
                own.push(u);
            }
        }
        #[cfg(target_os = "macos")]
        for k in ["ComputerName", "LocalHostName"] {
            if let Ok(o) = std::process::Command::new("/usr/sbin/scutil").args(["--get", k]).output() {
                own.push(String::from_utf8_lossy(&o.stdout).trim().to_string());
            }
        }
        own.retain(|s| s.chars().count() >= 2);
        own.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
        let mut r = Redact { home, own, names: Vec::new() };
        r.names = library_names();
        r
    }

    /// One anonymised line.
    pub fn line(&self, l: &str) -> String {
        // very long lines are cut (char boundary)
        let mut s: String = l.chars().take(400).collect();
        if self.home.len() > 1 {
            s = s.replace(&self.home, "~");
        }
        // names first: they may contain spaces and stand inside paths or quotes
        for n in self.names.iter().chain(&self.own) {
            s = replace_word(&s, &fold(n), "‹…›");
        }
        s = quoted(&s);
        s.split_inclusive(char::is_whitespace).map(token).collect()
    }
}

/// Keyword, person, collection and folder names of the library (read-only, never in the UI thread).
fn library_names() -> Vec<String> {
    let Ok(db) = library::db::Db::read_only(&library::db_path()) else { return Vec::new() };
    let mut names = std::collections::BTreeSet::new();
    for sql in ["SELECT name FROM keyword", "SELECT name FROM collection", "SELECT name FROM root_folder", "SELECT path FROM folder"] {
        let Ok(mut st) = db.conn.prepare(sql) else { continue };
        let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) else { continue };
        for n in rows.flatten() {
            for part in n.split(['/', '\\', '|']) {
                let p = part.trim().to_lowercase();
                // short and purely numeric names (years, dates) say nothing about a person
                if p.chars().count() >= 3 && p.chars().any(char::is_alphabetic) {
                    names.insert(p);
                }
            }
            if names.len() > 50_000 {
                break;
            }
        }
    }
    // longer names first, so "Anna Maria" goes before "Anna"
    let mut v: Vec<String> = names.into_iter().collect();
    v.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
    v
}

/// Text in quotes → `…`, except short lower-case identifiers (thread names like 'main').
fn quoted(s: &str) -> String {
    const PAIRS: [(char, char); 6] = [('"', '"'), ('\'', '\''), ('„', '“'), ('“', '”'), ('«', '»'), ('‘', '’')];
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let before_ok = i == 0 || !chars[i - 1].is_alphanumeric();
        if let Some(&(_, close)) = PAIRS.iter().find(|(o, _)| *o == c).filter(|_| before_ok) {
            if let Some(len) = chars[i + 1..].iter().position(|&x| x == close) {
                let j = i + 1 + len;
                let after_ok = j + 1 >= chars.len() || !chars[j + 1].is_alphanumeric();
                if after_ok {
                    let inner: String = chars[i + 1..j].iter().collect();
                    let harmless = inner.len() <= 24 && !inner.is_empty() && inner.chars().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || "_-".contains(x));
                    out.push(c);
                    out.push_str(if harmless { &inner } else { "…" });
                    out.push(close);
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// One whitespace-separated token: paths, photo file names, e-mail addresses.
fn token(t: &str) -> String {
    let trimmed = t.trim_end();
    let space = &t[trimmed.len()..];
    // punctuation around the token stays
    let start = trimmed.find(|c: char| !"([{<\"'=:,".contains(c)).unwrap_or(trimmed.len());
    let core_end = trimmed.trim_end_matches(|c: char| "),;]}>\"'".contains(c)).len().max(start);
    let (pre, core, post) = (&trimmed[..start], &trimmed[start..core_end], &trimmed[core_end..]);
    let new = anonymise_core(core);
    format!("{pre}{new}{post}{space}")
}

fn anonymise_core(core: &str) -> String {
    if core.is_empty() {
        return String::new();
    }
    if core.contains('@') && core.contains('.') && !core.contains("://") {
        return "‹e-mail›".into();
    }
    if core.contains("://") {
        // web addresses: only scheme and host
        let host_end = core.find("://").map_or(0, |i| i + 3);
        let rest = &core[host_end..];
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        return format!("{}{host}/…", &core[..host_end]);
    }
    let is_path = core.starts_with('/') || core.starts_with("~/") || core.starts_with("./") || core.starts_with("\\\\") || (core.len() > 2 && core.as_bytes()[1] == b':' && (core.as_bytes()[2] == b'\\' || core.as_bytes()[2] == b'/'));
    let has_sep = core.contains('/') || core.contains('\\');
    let last = core.rsplit(['/', '\\']).next().unwrap_or(core);
    // source places of a backtrace: keep `src/file.rs:line:col`
    let (file, place) = match last.split_once(':') {
        Some((f, p)) if p.chars().all(|c| c.is_ascii_digit() || c == ':') => (f, Some(p)),
        _ => (last, None),
    };
    if file.ends_with(".rs") && has_sep {
        let parts: Vec<&str> = core.rsplit(['/', '\\']).take(2).collect();
        return parts.into_iter().rev().collect::<Vec<_>>().join("/");
    }
    if is_path {
        let home = core.starts_with("~/");
        let name = file_name(file);
        let place = place.map(|p| format!(":{p}")).unwrap_or_default();
        return format!("{}…/{name}{place}", if home { "~/" } else { "" });
    }
    if has_sep {
        // relative path inside a message (`a/b/c.jpg`)
        if let Some(ext) = photo_ext(file) {
            return format!("…/Datei.{ext}");
        }
        return core.to_string();
    }
    match photo_ext(core) {
        Some(ext) => format!("Datei.{ext}"),
        None => core.to_string(),
    }
}

/// A file name keeps only its ending; a folder name becomes `…`.
fn file_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 6 && ext.chars().all(|c| c.is_ascii_alphanumeric()) => format!("Datei.{ext}"),
        _ if name.is_empty() => String::new(),
        _ => "…".to_string(),
    }
}

/// Ending of a photo/sidecar/video file name (`IMG_1234.CR3` → `CR3`).
fn photo_ext(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && FILE_EXT.iter().any(|e| e.eq_ignore_ascii_case(ext))).then_some(ext)
}

/// Lower case that keeps every byte position (a letter whose lower case has another length stays).
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(x), None) if x.len_utf8() == c.len_utf8() => x,
                _ => c,
            }
        })
        .collect()
}

/// Replace `word` (folded, see [`fold`]) wherever it stands as a whole word, ignoring case.
fn replace_word(s: &str, word: &str, with: &str) -> String {
    if word.is_empty() {
        return s.to_string();
    }
    let lower = fold(s);
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(word) {
        let a = i + pos;
        let b = a + word.len();
        let before = s[..a].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after = s[b..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        out.push_str(&s[i..a]);
        if before && after {
            out.push_str(with);
        } else {
            out.push_str(&s[a..b]);
        }
        i = b;
    }
    out.push_str(&s[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact() -> Redact {
        Redact { home: "/Users/anna".into(), own: vec!["anna".into(), "Annas-MacBook-Pro".into()], names: vec!["urlaub kreta".into(), "max mustermann".into(), "max".into()] }
    }

    #[test]
    fn paths_names_and_words_do_not_leave_the_computer() {
        let r = redact();
        let cases = [
            ("open /Users/anna/Pictures/Urlaub Kreta/IMG_1234.CR3: Foveon", "open ~/…/Datei.CR3: Foveon"),
            ("move folder: 12 files renamed → /Volumes/NAS/Fotos/Max Mustermann", "move folder: 12 files renamed → …/…"),
            ("full resolution: twice (Path(\"duplikat-test.X3F\"))", "full resolution: twice (Path(\"…\"))"),
            ("bedeutungssuche: „Max am Strand“ in 12 ms", "bedeutungssuche: „…“ in 12 ms"),
            ("crash: thread 'main' at crates/app/src/main.rs:12:5: boom", "crash: thread 'main' at src/main.rs:12:5: boom"),
            ("people: named Max Mustermann", "people: named ‹…›"),
            ("collection Urlaub Kreta created", "collection ‹…› created"),
            ("mail an anna@example.org", "mail an ‹e-mail›"),
            ("download https://huggingface.co/google/x/resolve/abc/model.onnx failed", "download https://huggingface.co/… failed"),
            ("export IMG_0042.jpg done", "export Datei.jpg done"),
            ("host Annas-MacBook-Pro ready", "host ‹…› ready"),
            ("C:\\Users\\anna\\Pictures\\a.dng read", "…/Datei.dng read"),
            ("library.sqlite opened, version 0.107.0", "library.sqlite opened, version 0.107.0"),
            ("Maximum 3", "Maximum 3"),
        ];
        for (a, b) in cases {
            assert_eq!(r.line(a), b, "{a}");
        }
    }

    #[test]
    fn backtrace_keeps_only_source_files() {
        let r = redact();
        assert_eq!(r.line("             at /Users/anna/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/egui-0.37.0/src/context.rs:833:9"), "             at src/context.rs:833:9");
        assert_eq!(r.line("  12: graineo::main::h1234abcd"), "  12: graineo::main::h1234abcd");
    }

    #[test]
    fn issue_address_stays_below_the_limit() {
        let rep = Report {
            version: "0.107.0".into(),
            system: vec![("System".into(), "macOS 15.5".into()), ("GPU".into(), "Apple M2".into())],
            crash: (0..40).map(|i| format!("  {i}: frame with a long name {}", "x".repeat(80))).collect(),
            log: (0..80).map(|i| format!("[{i}] line {}", "ü".repeat(60))).collect(),
        };
        let url = rep.issue_url(true);
        assert!(url.len() <= MAX_URL, "{}", url.len());
        assert!(url.starts_with("https://github.com/graineo/graineo/issues/new?title=Problem%20in%20graineo%200.107.0"));
        assert!(url.contains("Shortened"));
        // the newest log line is kept, the oldest dropped
        assert!(url.contains(&encode("[79] line")));
        assert!(!url.contains(&encode("[0] line")));
        // short report: complete, no note
        let small = Report { log: vec!["[1] start".into()], crash: Vec::new(), ..rep.clone() };
        let u = small.issue_url(true);
        assert!(!u.contains("Shortened") && u.contains(&encode("[1] start")));
        assert!(!small.issue_url(false).contains(&encode("[1] start")));
        // only characters that are allowed in an address
        assert!(url.bytes().all(|b| b.is_ascii_graphic()));
    }

    #[test]
    fn crash_excerpt_without_runtime_frames() {
        let e = "==== 2026-11-14 · graineo 0.107.0 ====\nThread: main\nOrt: src/a.rs:1:1\nMeldung: boom\n   0: std::backtrace_rs::backtrace::trace\n             at backtrace/libunwind.rs:117:9\n   5: graineo::crash_log::install::{{closure}}\n             at src/crash_log.rs:75:9\n   7: graineo::App::update\n             at src/main.rs:12:5\n   9: core::ops::function::FnOnce::call_once\n";
        let v = own_frames(e);
        assert_eq!(v.len(), 6, "{v:?}");
        assert!(v.iter().any(|l| l.contains("graineo::App::update")) && v.iter().any(|l| l.contains("src/main.rs:12:5")));
        assert!(!v.iter().any(|l| l.contains("std::") || l.contains("libunwind") || l.contains("core::") || l.contains("crash_log.rs")));
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(encode("a b&c=ü\n"), "a%20b%26c%3D%C3%BC%0A");
    }
}
