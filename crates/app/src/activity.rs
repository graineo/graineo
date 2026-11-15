//! One status for all work, in the title bar right next to logo and app name: running work (a small spinner and
//! the most important job, "+2" when more are running), short successes ("Export fertig · 12 Fotos", a few seconds)
//! and errors of actions (red dot, "Export fehlgeschlagen" / "2 Fehler", until closed). It has a fixed width, so
//! nothing moves when it appears or disappears. A click opens a small window attached to it: the errors with cause,
//! file and buttons, then the running work.
//!
//! Only what concerns the user or slows things down shows up in the title bar (always with progress); quiet background
//! work (previews, faces, backup, settings jobs) is listed in the window only, and shows in the bar after
//! [`QUIET_SHOWN_AFTER`].
//!
//! Conditions of a single photo (damaged, Sigma-exact unavailable …) are not messages: see `photo_state`.

use eframe::egui;

use crate::{lr, App};

/// Order of importance (first = shown). Three tiers ([`Rank::tier`]): 1 started by the user (export, import,
/// moving, deleting, contact print, a check started by hand), 2 what is visible right now (the opened photo, the
/// previews of the shown folder), 3 background (sync, previews, faces, sidecars, backup).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Rank {
    Import,
    Export,
    Move,
    Delete,
    ContactPrint,
    /// work the user started in the settings or the library menu (check, rebuild …)
    Started,
    /// the opened photo is loading
    Photo,
    /// previews of the photos visible in the grid
    Visible,
    Library,
    Backup,
    Settings,
    Faces,
    Previews,
}

impl Rank {
    pub fn tier(self) -> u8 {
        match self {
            Rank::Import | Rank::Export | Rank::Move | Rank::Delete | Rank::ContactPrint | Rank::Started => 1,
            Rank::Photo | Rank::Visible => 2,
            _ => 3,
        }
    }

    /// Harmless background work that is not shown in the bar (only in the list) unless it takes very long.
    pub fn quiet(self) -> bool {
        matches!(self, Rank::Backup | Rank::Settings | Rank::Faces | Rank::Previews)
    }
}

/// Quiet work that runs longer than this shows in the bar anyway (with its progress).
pub const QUIET_SHOWN_AFTER: std::time::Duration = std::time::Duration::from_secs(300);

/// What the bar shows of all running work: everything except quiet work, which only counts after
/// [`QUIET_SHOWN_AFTER`]. `seen` remembers since when a kind of quiet work has been running.
pub fn top_tasks(all: &[Task], seen: &mut std::collections::HashMap<Rank, std::time::Instant>, now: std::time::Instant) -> Vec<Task> {
    seen.retain(|r, _| all.iter().any(|t| t.rank == *r));
    for t in all.iter().filter(|t| t.rank.quiet()) {
        seen.entry(t.rank).or_insert(now);
    }
    all.iter().filter(|t| !t.rank.quiet() || seen.get(&t.rank).is_some_and(|s| now.duration_since(*s) >= QUIET_SHOWN_AFTER)).cloned().collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub rank: Rank,
    pub text: String,
}

impl Task {
    pub fn new(rank: Rank, text: String) -> Task {
        Task { rank, text }
    }
}

/// Width of the status in the title bar (points).
const WIDTH: f32 = 420.0;

/// Messages of the library sync name the whole path of a root folder ("Abgleich /Volumes/…/Pictures/ …"):
/// shortened to the name the library shows for it ("Abgleich · Pictures (NAS)"), then translated.
pub fn short_message(msg: &str, roots: &[(&str, &str)]) -> String {
    let hit = roots
        .iter()
        .filter(|(p, _)| !p.is_empty())
        .filter_map(|(p, n)| {
            let bare = p.trim_end_matches('/');
            [*p, bare].into_iter().find(|x| !x.is_empty() && msg.contains(x)).map(|x| (x, *n))
        })
        .max_by_key(|(x, _)| x.len());
    match hit {
        Some((path, name)) => {
            if let Some(rest) = msg.strip_prefix("Abgleich ") {
                if rest.trim_end_matches(" …").trim_end_matches('/') == path.trim_end_matches('/') {
                    return crate::trf!("Abgleich · {a}", a = name);
                }
            }
            crate::menu::i18n::tr_msg(&msg.replacen(path, name, 1)).into_owned()
        }
        None => crate::menu::i18n::tr_msg(msg).into_owned(),
    }
}

/// Text in the title bar: the first task, "+n" for the others.
pub fn summary(tasks: &[Task]) -> Option<String> {
    let first = tasks.first()?;
    Some(if tasks.len() > 1 { format!("{}  +{}", first.text, tasks.len() - 1) } else { first.text.clone() })
}

impl App {
    /// All background work that is running right now, most important first.
    pub(crate) fn activities(&self) -> Vec<Task> {
        let mut v = self.lib.activities();
        // previews of the visible grid only count while the grid is shown
        let grid = self.in_grid();
        v.retain(|t| t.rank != Rank::Visible || grid);
        if self.exporting && !self.export_status.is_empty() {
            v.push(Task::new(Rank::Export, crate::trf!("Export: {a}", a = crate::menu::i18n::tr_msg(&self.export_status))));
        }
        if let Some((done, total)) = self.multi.progress() {
            v.push(Task::new(Rank::Started, crate::trf!("Einstellungen übertragen: {k} von {n}", k = crate::library_ui::thousands(done), n = crate::library_ui::thousands(total))));
        }
        let previews = self.thumbnails_waiting();
        if previews > 0 {
            v.push(Task::new(Rank::Previews, crate::trf!("Vorschauen nach Bearbeitung: noch {n}", n = crate::library_ui::thousands(previews))));
        }
        if let Some(t) = crate::prefs::work_status() {
            v.push(Task::new(Rank::Settings, t));
        }
        if let Some((done, total)) = library::sidecar_import::progress() {
            v.push(Task::new(Rank::Library, crate::trf!("Begleitdateien einlesen: noch {n}", n = crate::library_ui::thousands(total - done))));
        }
        let waiting = self.lib.sidecars_waiting();
        if waiting > 0 {
            v.push(Task::new(Rank::Previews, crate::trf!("Begleitdateien: noch {n}", n = crate::library_ui::thousands(waiting))));
        }
        if let Some((done, total)) = crate::sigma_refresh::progress() {
            let n = crate::library_ui::thousands(total - done);
            v.push(Task::new(Rank::Previews, if crate::sigma_refresh::film_stage_running() { crate::trf!("Film-Vorschauen erneuern: noch {n}", n = n) } else if crate::sigma_refresh::look_stage_running() { crate::trf!("Kamera-Look-Vorschauen erneuern: noch {n}", n = n) } else if crate::sigma_refresh::color_stage_running() { crate::trf!("Farb-Vorschauen erneuern: noch {n}", n = n) } else { crate::trf!("Foveon-Vorschauen erneuern: noch {n}", n = n) }));
        }
        if crate::delete::running() {
            v.push(Task::new(Rank::Delete, crate::delete::status_text()));
        }
        if self.lib.pairs_running() {
            v.push(Task::new(Rank::Delete, crate::tr("Doppelte JPEGs entfernen").to_string()));
        }
        if self.darkroom.contact.an && self.darkroom.contact.computing() {
            v.push(Task::new(Rank::ContactPrint, crate::tr("Kontaktabzug wird berechnet").to_string()));
        }
        if !self.in_grid() {
            if let Some(e) = self.loading.and_then(|i| self.entries.get(i)) {
                v.push(Task::new(Rank::Photo, crate::trf!("Lädt {a}", a = e.name)));
            }
        }
        v.sort_by_key(|t| t.rank);
        v
    }

    /// The status in the title bar (fixed width, empty when idle). Order: errors (red dot, stay until closed) →
    /// a short success (a few seconds) → running work. A click opens a small window attached to it with the errors
    /// (cause, file, buttons) and the running work.
    pub(crate) fn activity_status(&mut self, ui: &mut egui::Ui) {
        let all = self.activities();
        let tasks = top_tasks(&all, &mut self.notes.seen, std::time::Instant::now());
        let now = ui.input(|i| i.time);
        let done = self.notes.done.clone().filter(|(_, t)| now - t < DONE_SECS);
        if done.is_none() {
            self.notes.done = None;
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        let faults = self.notes.faults.len();
        let active = faults > 0 || !all.is_empty();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(WIDTH, 18.0), if active { egui::Sense::click() } else { egui::Sense::hover() });
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        child.spacing_mut().item_spacing.x = 6.0;
        let hover = resp.hovered();
        if faults > 0 {
            let label = if faults == 1 { self.notes.faults[0].title.clone() } else { crate::trf!("{a} Fehler", a = faults) };
            dot(&mut child, RED);
            child.add(egui::Label::new(egui::RichText::new(label).small().color(if hover { egui::Color32::WHITE } else { lr::TEXT })).truncate().selectable(false));
            if let Some(t) = summary(&tasks) {
                child.add(egui::Label::new(egui::RichText::new("·").small().color(lr::TEXT_DIM)).selectable(false));
                calm_spinner(&mut child, 9.0, lr::TEXT_DIM.gamma_multiply(0.8));
                child.add(egui::Label::new(egui::RichText::new(t).small().color(lr::TEXT_DIM)).truncate().selectable(false));
            }
        } else if let Some((text, since)) = &done {
            // fades out during the last half second
            let fade = ((DONE_SECS - (now - since)) / 0.5).clamp(0.0, 1.0) as f32;
            dot(&mut child, GREEN.gamma_multiply(fade));
            child.add(egui::Label::new(egui::RichText::new(text).small().color(lr::TEXT.gamma_multiply(fade))).truncate().selectable(false));
        } else if let Some(text) = summary(&tasks) {
            // background only: a calm, dimmed line without a spinner; work of the user or of what is visible: spinner
            let background = tasks.first().is_some_and(|t| t.rank.tier() == 3);
            if background {
                child.add(egui::Label::new(egui::RichText::new(text).small().color(if hover { lr::TEXT_DIM } else { lr::TEXT_DIM.gamma_multiply(0.75) })).truncate().selectable(false));
            } else {
                calm_spinner(&mut child, 9.0, lr::TEXT_DIM.gamma_multiply(0.8));
                child.add(egui::Label::new(egui::RichText::new(text).small().color(if hover { egui::Color32::WHITE } else { lr::TEXT })).truncate().selectable(false));
                // the spinner explains itself: the step the computing job is at
                if let Some(step) = crate::work_step::describe() {
                    lr::hover_tip(ui, &resp, &step);
                }
            }
        } else if !all.is_empty() {
            // only quiet background work: a small static dot (no redraws) leads to the list
            dot(&mut child, lr::TEXT_DIM.gamma_multiply(if hover { 0.9 } else { 0.5 }));
        }
        if !active {
            self.notes.open = false;
            return;
        }
        if resp.clicked() {
            self.notes.open = !self.notes.open;
        }
        // Test hook: GRAINEO_ACTIVITY_OPEN=1 opens the window as soon as there is an error (screenshots)
        if faults > 0 && crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var_os("GRAINEO_ACTIVITY_OPEN").is_some() && !self.notes.test_opened {
            self.notes.test_opened = true;
            self.notes.open = true;
        }
        let mut open = self.notes.open;
        let mut action = None;
        egui::Popup::from_response(&resp)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::BOTTOM_START)
            .width(WIDTH)
            .show(|ui| {
                ui.set_min_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 4.0;
                for (n, f) in self.notes.faults.iter().enumerate() {
                    if n > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        dot(ui, RED);
                        ui.label(egui::RichText::new(&f.title).font(lr::semibold(12.5)).color(egui::Color32::from_gray(0xf0)));
                    });
                    if !f.cause.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(&f.cause).font(lr::sans(12.0)).color(lr::TEXT)).wrap());
                    }
                    if !f.subject.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(&f.subject).small().color(lr::TEXT_DIM)).wrap());
                    }
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if f.retry.is_some() && ui.button(crate::tr("Erneut versuchen")).clicked() {
                            action = Some(Click::Retry(n));
                        }
                        if f.reveal.is_some() && ui.button(crate::tr("Im Finder zeigen")).clicked() {
                            action = Some(Click::Reveal(n));
                        }
                        if ui.button(crate::tr("Schließen")).clicked() {
                            action = Some(Click::Close(n));
                        }
                    });
                }
                if !all.is_empty() {
                    if faults > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                    }
                    for t in &all {
                        // an import running in the background: its window comes back from here
                        if t.rank == Rank::Import {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(&t.text).small().color(lr::TEXT));
                                if ui.small_button(crate::tr("Fenster zeigen")).clicked() {
                                    action = Some(Click::ShowImport);
                                }
                            });
                            continue;
                        }
                        ui.label(egui::RichText::new(&t.text).small().color(lr::TEXT));
                    }
                }
            });
        self.notes.open = open;
        match action {
            Some(Click::Close(n)) => {
                self.notes.faults.remove(n);
            }
            Some(Click::ShowImport) => {
                self.lib.import_show();
                self.notes.open = false;
            }
            Some(Click::Reveal(n)) => {
                if let Some(p) = self.notes.faults[n].reveal.clone() {
                    crate::system::show(&p);
                }
            }
            Some(Click::Retry(n)) => {
                let f = self.notes.faults.remove(n);
                match f.retry {
                    Some(Retry::Export(paths)) => {
                        let indices: Vec<usize> = paths.iter().filter_map(|p| self.entries.iter().position(|e| e.path == *p)).collect();
                        if !indices.is_empty() {
                            self.export(indices);
                        }
                    }
                    None => {}
                }
                self.notes.open = false;
            }
            None => {}
        }
        if self.notes.faults.is_empty() && all.is_empty() {
            self.notes.open = false;
        }
    }
}

enum Click {
    /// show the window of an import running in the background
    ShowImport,
    Retry(usize),
    Reveal(usize),
    Close(usize),
}

/// How long a success stays in the status (seconds).
const DONE_SECS: f64 = 5.0;
const RED: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x5c, 0x50);
const GREEN: egui::Color32 = egui::Color32::from_rgb(0x7c, 0xa8, 0x70);

/// Small filled dot in a row (red: error, green: done).
fn dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// What "Try again" repeats.
#[derive(Clone, Debug, PartialEq)]
pub enum Retry {
    /// export these photos again (same settings and destination)
    Export(Vec<std::path::PathBuf>),
}

/// A failed piece of work started by the user (export, moving …).
#[derive(Clone, Debug, PartialEq)]
pub struct Fault {
    /// short, e.g. "Export fehlgeschlagen"
    pub title: String,
    /// one sentence: why
    pub cause: String,
    /// affected file, folder or count
    pub subject: String,
    /// "Show in Finder"
    pub reveal: Option<std::path::PathBuf>,
    pub retry: Option<Retry>,
}

impl Fault {
    pub fn new(title: impl Into<String>, cause: impl Into<String>) -> Fault {
        Fault { title: title.into(), cause: cause.into(), subject: String::new(), reveal: None, retry: None }
    }

    /// From a free message ("Einstellungen nicht gespeichert: <Grund>"): before the first colon the title, after it
    /// the cause.
    pub fn from_message(text: &str) -> Fault {
        match text.split_once(": ") {
            Some((title, cause)) if title.chars().count() <= 48 && !cause.trim().is_empty() => Fault::new(title.trim(), first_upper(cause.trim())),
            _ => Fault::new(text.trim(), ""),
        }
    }
}

fn first_upper(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Develop settings or ratings that could not be written to the library: a fault in the status (once per error
/// text), another try every 30 s.
pub fn watch_unsaved(notes: &mut Notes) {
    static SEEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    static LAST_TRY: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let n = library::store::unsaved();
    let mut seen = SEEN.lock().unwrap_or_else(|x| x.into_inner());
    if n == 0 {
        *seen = None;
        return;
    }
    let e = library::store::last_error().unwrap_or_default();
    if seen.as_deref() != Some(e.as_str()) {
        *seen = Some(e.clone());
        notes.fault(Fault::new(crate::tr("Bearbeitung nicht gespeichert"), crate::trf!("{n} Änderungen warten, graineo versucht es erneut. {e}", n = n, e = crate::menu::i18n::tr_msg(&e))));
    }
    let mut last = LAST_TRY.lock().unwrap_or_else(|x| x.into_inner());
    if last.is_none_or(|t| t.elapsed().as_secs() >= 30) {
        *last = Some(std::time::Instant::now());
        library::store::retry_unsaved();
    }
}

/// Results of work for the status in the title bar.
#[derive(Default)]
pub struct Notes {
    /// errors, oldest first; they stay until closed
    pub faults: Vec<Fault>,
    /// last success and when it came
    pub done: Option<(String, f64)>,
    /// the window below the status is open
    pub open: bool,
    test_opened: bool,
    /// since when each kind of quiet work runs (see [`top_tasks`])
    seen: std::collections::HashMap<Rank, std::time::Instant>,
}

impl Notes {
    /// Record an error (an identical one is not repeated; at most 20 are kept).
    pub fn fault(&mut self, f: Fault) {
        self.faults.retain(|x| *x != f);
        self.faults.push(f);
        if self.faults.len() > 20 {
            self.faults.remove(0);
        }
    }

    pub fn done(&mut self, text: impl Into<String>, now: f64) {
        self.done = Some((text.into(), now));
    }

    /// A free message from the app (`App::status`): errors stay, everything else is a short success. File names
    /// and "Loading …" are not messages.
    pub fn message(&mut self, text: &str, name: &str, now: f64) {
        let s = text.trim();
        if s.is_empty() || s == name || s.starts_with("Lade ") || s.starts_with("Load ") {
            return;
        }
        let s = crate::menu::i18n::tr_msg(s).to_string();
        if is_error(&s) {
            self.fault(Fault::from_message(&s));
        } else {
            self.done(s, now);
        }
    }
}

/// Is a free message an error? ("0 errors" in count lines is not one.)
pub fn is_error(s: &str) -> bool {
    let small = s.to_lowercase().replace(" 0 fehler", "").replace(" 0 errors", "");
    ["nicht ", "fehler", "fehlgeschlagen", "error", "failed", "locked", "belegt", "unable", "could not", "not "].iter().any(|w| small.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_path_becomes_root_name() {
        let roots = [("/Volumes/nas/Photos/RAW/Pictures/", "Pictures (NAS)"), ("/Volumes/nas/Photos/", "Backup")];
        assert_eq!(short_message("Abgleich /Volumes/nas/Photos/RAW/Pictures/ …", &roots), "Abgleich · Pictures (NAS)");
        assert_eq!(short_message("Abgleich /Volumes/nas/Photos/RAW/Pictures …", &roots), "Abgleich · Pictures (NAS)");
        assert_eq!(short_message("Abgleich /Volumes/nas/Photos/RAW/Pictures/: wird gerade verschoben, später", &roots), "Abgleich Pictures (NAS): wird gerade verschoben, später");
        assert_eq!(short_message("Begleitdateien abgleichen …", &roots), "Begleitdateien abgleichen …");
    }

    #[test]
    fn messages_errors_stay_success_is_short() {
        let mut n = Notes::default();
        n.message("Lade X.X3F …", "", 1.0);
        n.message("X.X3F", "X.X3F", 1.0);
        assert!(n.faults.is_empty() && n.done.is_none());
        n.message("3 Bilder: ★★★", "", 1.0);
        assert_eq!(n.done.as_ref().map(|d| d.0.as_str()), Some("3 Bilder: ★★★"));
        n.message("Einstellungen nicht gespeichert: keine Schreibrechte", "", 2.0);
        n.message("Einstellungen nicht gespeichert: keine Schreibrechte", "", 3.0);
        assert_eq!(n.faults.len(), 1);
        assert_eq!(n.faults[0].title, "Einstellungen nicht gespeichert");
        assert_eq!(n.faults[0].cause, "Keine Schreibrechte");
        assert!(!is_error("Import fertig · 0 Fehler"));
    }

    #[test]
    fn tiers_user_visible_background() {
        assert_eq!(Rank::Export.tier(), 1);
        assert_eq!(Rank::Delete.tier(), 1);
        assert_eq!(Rank::ContactPrint.tier(), 1);
        assert_eq!(Rank::Visible.tier(), 2);
        assert_eq!(Rank::Photo.tier(), 2);
        assert_eq!(Rank::Library.tier(), 3);
        assert_eq!(Rank::Previews.tier(), 3);
        // ordering follows the tiers: anything of the user before anything visible before the background
        let mut v = vec![Task::new(Rank::Faces, "f".into()), Task::new(Rank::Visible, "v".into()), Task::new(Rank::Delete, "d".into())];
        v.sort_by_key(|t| t.rank);
        assert_eq!(v.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["d", "v", "f"]);
        let all = [Rank::Import, Rank::Export, Rank::Move, Rank::Delete, Rank::ContactPrint, Rank::Started, Rank::Photo, Rank::Visible, Rank::Library, Rank::Backup, Rank::Settings, Rank::Faces, Rank::Previews];
        assert!(all.windows(2).all(|w| w[0].tier() <= w[1].tier()));
    }

    #[test]
    fn quiet_work_only_in_list_until_five_minutes() {
        use std::time::{Duration, Instant};
        let all = vec![Task::new(Rank::Import, "Import · 1 von 2".into()), Task::new(Rank::Previews, "Vorschauen: noch 5".into()), Task::new(Rank::Faces, "Gesichter".into())];
        let mut seen = std::collections::HashMap::new();
        let t0 = Instant::now();
        let top = top_tasks(&all, &mut seen, t0);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].rank, Rank::Import);
        assert_eq!(top_tasks(&all, &mut seen, t0 + Duration::from_secs(299)).len(), 1);
        assert_eq!(top_tasks(&all, &mut seen, t0 + Duration::from_secs(301)).len(), 3);
        // work that ended is forgotten
        assert!(top_tasks(&[], &mut seen, t0 + Duration::from_secs(400)).is_empty() && seen.is_empty());
    }

    #[test]
    fn most_important_first_with_count() {
        assert_eq!(summary(&[]), None);
        let mut v = vec![Task::new(Rank::Previews, "Vorschauen: noch 12".into()), Task::new(Rank::Library, "Abgleich · Pictures".into()), Task::new(Rank::Faces, "Gesichter".into())];
        v.sort_by_key(|t| t.rank);
        assert_eq!(summary(&v).as_deref(), Some("Abgleich · Pictures  +2"));
    }
}

/// Frames per second of [`calm_spinner`].
const CALM_FPS: f64 = 8.0;

/// Turning arc like `egui::Spinner`, but drawn only a few times a second: background work often runs for minutes, and
/// the egui spinner redraws the whole window in every display frame (up to 120 per second) for it.
pub(crate) fn calm_spinner(ui: &mut egui::Ui, size: f32, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let time = (ui.input(|i| i.time) * CALM_FPS).floor() / CALM_FPS;
    let radius = rect.height().min(rect.width()) * 0.5 - 2.0;
    let n_points = (radius.round() as u32).clamp(8, 128);
    let start = time * std::f64::consts::TAU;
    let end = start + 240f64.to_radians() * time.sin();
    let points: Vec<egui::Pos2> = (0..n_points)
        .map(|i| {
            let a = egui::emath::lerp(start..=end, i as f64 / n_points as f64);
            let (s, c) = a.sin_cos();
            rect.center() + radius * egui::vec2(c as f32, s as f32)
        })
        .collect();
    ui.painter().add(egui::Shape::line(points, egui::Stroke::new(3.0, color)));
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(1.0 / CALM_FPS));
}
