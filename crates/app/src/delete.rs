//! "Delete rejected photos": first determine (in the background), then ask honestly, then
//! execute (in the background) and show the result. Local files go to the macOS Trash (on a network drive into its
//! trash folder); nothing is deleted permanently.
//!
//! Kinds of rejected images:
//! - **on disk**: file (+ sidecars, optionally camera JPEG) to the Trash, entry removed;
//! - **file missing** (state "missing", e.g. old Lightroom entries): only the entry (option, on);
//! - **from Apple Photos**: deleted via Apple Photos ("Recently Deleted").
//!
//! Volumes that are not connected are skipped (file not lost, just not there right now).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::dialog::{self, Answer, Buttons};
use crate::App;

/// A rejected image of the list (captured when the dialog opens).
#[derive(Clone, Debug)]
pub struct Candidate {
    pub image: i64,
    pub file: i64,
    pub path: PathBuf,
    pub state: u8,
    pub copy: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// (image, file, sidecars) – present
    pub disk: Vec<(i64, PathBuf, Vec<PathBuf>)>,
    /// of those on network drives: they go to the drive's trash folder instead of the macOS Trash
    pub network: usize,
    pub bytes: u64,
    /// camera JPEGs with the same name next to on-disk images
    pub jpegs: Vec<PathBuf>,
    /// image IDs whose file is missing
    pub missing: Vec<i64>,
    /// (name, reason)
    pub skipped: Vec<(String, String)>,
    /// virtual copies (entry only, the file stays with the original)
    pub copies: Vec<i64>,
    /// (image, identifier) photos from Apple Photos: deleted via Apple Photos ("Recently Deleted")
    pub apple: Vec<(i64, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub trash: usize,
    pub files: usize,
    pub removed: usize,
    /// deleted in Apple Photos
    pub apple: usize,
    pub skipped: Vec<String>,
    pub error: Vec<String>,
}

#[derive(Default)]
pub struct State {
    pub plan: Option<Plan>,
    pub running: bool,
    pub outcome: Option<Outcome>,
    /// the library read right after deleting (same thread): the list is correct as soon as the result shows
    pub loaded: Option<crate::library_ui::Loaded>,
}

/// Sidecars of an image (X3F settings, XMP), as far as present.
fn companions(p: &Path) -> Vec<PathBuf> {
    let mut v = if crate::image_kind::of(p) == crate::image_kind::ImageKind::Foveon { crate::sidecar::companion_files(p) } else { Vec::new() };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for x in [p.with_extension("xmp"), p.with_file_name(format!("{name}.xmp"))] {
        if !v.contains(&x) {
            v.push(x);
        }
    }
    v.into_iter().filter(|x| x != p && x.exists()).collect()
}

fn camera_jpeg(p: &Path) -> Option<PathBuf> {
    if crate::image_kind::of(p) == crate::image_kind::ImageKind::Done {
        return None;
    }
    ["JPG", "jpg", "JPEG", "jpeg"].iter().map(|x| p.with_extension(x)).find(|j| j != p && j.exists())
}

/// Plan in the background (file sizes, locations reachable?) – can take a while with a sleeping NAS.
pub fn plan(k: Vec<Candidate>) -> Plan {
    let db = library::db::Db::standard().ok();
    let mut p = Plan::default();
    for c in k {
        let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if c.copy {
            p.copies.push(c.image);
            continue;
        }
        // Photos from Apple Photos: never move library files to the Trash, delete via Apple Photos instead
        if library::apple_photos::in_photos_library(&c.path) {
            match library::apple_photos::uuid_from_path(&c.path) {
                Some(u) => p.apple.push((c.image, u)),
                None => p.skipped.push((name, crate::tr("Foto aus Apple Fotos ohne Kennung").into())),
            }
            continue;
        }
        // Original on an unreachable location (also if the sync has not noticed it yet): never take it for "missing" and
        // remove the entry – deleting waits until the location is connected again (no queue for deleting)
        let unreachable = c.state == 2 || (matches!(c.state, 0 | 1) && !c.path.exists() && db.as_ref().is_some_and(|d| !library::location::file_reachable(d, c.file)));
        if unreachable {
            p.skipped.push((name, crate::tr("Original nicht erreichbar – Löschen geht erst nach dem Verbinden").into()));
            continue;
        }
        match c.state {
            0 if c.path.exists() => {
                p.bytes += std::fs::metadata(&c.path).map(|m| m.len()).unwrap_or(0);
                if let Some(j) = camera_jpeg(&c.path) {
                    p.bytes += std::fs::metadata(&j).map(|m| m.len()).unwrap_or(0);
                    p.jpegs.push(j);
                }
                if c.path.parent().is_some_and(library::network_trash::is_network) {
                    p.network += 1;
                }
                p.disk.push((c.image, c.path.clone(), companions(&c.path)));
            }
            0 | 1 => p.missing.push(c.image),
            _ => p.skipped.push((name, crate::tr("Zustand unbekannt").into())),
        }
    }
    p
}

/// Execute (in the background). `stack_jpegs`: (RAW image, its camera JPEG's image) of stacked photos.
pub fn execute(p: Plan, with_jpegs: bool, remove_missing: bool, stack_jpegs: Vec<(i64, i64)>) -> Outcome {
    let mut e = Outcome::default();
    // viewing a backup: nothing on the drive or in Apple Photos is deleted from there
    if let Err(x) = library::backup_view::refuse() {
        e.error.push(x);
        return e;
    }
    // the library first: files must never go to the trash while their entries cannot be removed (they would stay
    // as "missing" without a message)
    let db = match library::db::Db::standard() {
        Ok(db) => Some(db),
        Err(x) => {
            e.error.push(crate::trf!("Bibliothek: {x}", x = x));
            return e;
        }
    };
    // RAWs whose camera JPEG went to the trash with them
    let mut jpeg_gone: std::collections::HashSet<i64> = std::collections::HashSet::new();
    // 1. disk → Trash (per image, so that one error does not stop everything)
    let mut removed: Vec<i64> = Vec::new();
    PROGRESS.1.store(p.disk.len() + p.apple.len(), std::sync::atomic::Ordering::Relaxed);
    PROGRESS.0.store(0, std::sync::atomic::Ordering::Relaxed);
    for (b, path, sidecars) in &p.disk {
        PROGRESS.0.store(removed.len() + e.error.len(), std::sync::atomic::Ordering::Relaxed);
        let mut files = vec![path.clone()];
        files.extend(sidecars.iter().cloned());
        let mut with_jpeg = false;
        if with_jpegs {
            if let Some(j) = p.jpegs.iter().find(|j| j.with_extension("") == path.with_extension("")) {
                files.push(j.clone());
                files.extend(companions(j));
                with_jpeg = true;
            }
        }
        // Network drive: no macOS Trash, the files move to the drive's trash folder (Settings → Storage)
        let network_root = path.parent().filter(|d| library::network_trash::is_network(d)).and_then(|d| db.as_ref().and_then(|db| db.root_to_path(d)));
        let outcome = match &network_root {
            Some(st) => library::network_trash::discard(Path::new(&st.path), &files, &library::card_import::today()),
            None => trash::delete_all(&files).map(|()| files.len()).map_err(|x| x.to_string()),
        };
        match outcome {
            Ok(n) => {
                e.trash += 1;
                e.files += n;
                removed.push(*b);
                if with_jpeg {
                    jpeg_gone.insert(*b);
                }
            }
            Err(x) => e.error.push(format!("{}: {x}", path.display())),
        }
    }
    // 2. missing files: entries only
    if remove_missing {
        removed.extend(p.missing.iter().copied());
    } else if !p.missing.is_empty() {
        e.skipped.push(crate::trf!("{a} Einträge fehlender Dateien behalten (Option aus)", a = p.missing.len()));
    }
    removed.extend(p.copies.iter().copied());
    // 3. Apple Photos: delete via Apple Photos (macOS asks once more itself), then entries and own sidecars
    if !p.apple.is_empty() {
        let uuids: Vec<String> = p.apple.iter().map(|(_, u)| u.clone()).collect();
        // a sync of Apple Photos that is reading right now must not put these photos back as new ones
        library::apple_photos::note_own_change();
        let answer = delete_in_apple_photos(&uuids);
        library::apple_photos::note_own_change();
        match answer {
            Ok(Some(gone)) => {
                for (b, u) in &p.apple {
                    if gone.contains(u) {
                        removed.push(*b);
                        e.apple += 1;
                        let xmp = library::apple_photos::sidecar_folder().join(format!("{u}.xmp"));
                        if xmp.exists() {
                            let _ = trash::delete(&xmp);
                        }
                    }
                }
                if gone.len() < p.apple.len() {
                    e.skipped.push(crate::trf!("{a} in Apple Fotos nicht gefunden", a = dialog::count(p.apple.len() - gone.len(), crate::tr("Foto"), crate::tr("Fotos"))));
                }
            }
            Ok(None) => e.skipped.push(crate::trf!("{a} aus Apple Fotos: in Apple Fotos abgelehnt – bleiben", a = dialog::count(p.apple.len(), crate::tr("Foto"), crate::tr("Fotos")))),
            Err(x) => e.error.push(crate::trf!("Apple Fotos: {x}", x = x)),
        }
    }
    for (n, g) in &p.skipped {
        e.skipped.push(format!("{n}: {g}"));
    }
    if let Some(db) = &db {
        // Camera JPEG in the stack: its entry goes too if the file was deleted as well
        // only where the RAW and its JPEG really went to the trash (a failed RAW keeps its JPEG entry)
        let jpegs: Vec<i64> = if with_jpegs { stack_jpegs.iter().filter(|(raw, _)| jpeg_gone.contains(raw)).map(|x| x.1).collect() } else { Vec::new() };
        match db.remove_images(&removed, &jpegs) {
            Ok(n) => e.removed = n,
            Err(x) => e.error.push(crate::trf!("Bibliothek: {x}", x = x)),
        }
        if e.apple > 0 {
            library::apple_photos::note_own_change();
        }
    }
    e
}

/// Delete via Apple Photos: `Some(deleted identifiers)` or `None` if declined in the dialog.
#[cfg(target_os = "macos")]
fn delete_in_apple_photos(uuids: &[String]) -> Result<Option<Vec<String>>, String> {
    if let Some(r) = fake_apple_delete(uuids) {
        return r;
    }
    match crate::photokit::delete(uuids)? {
        crate::photokit::Deleted::Ja(v) => Ok(Some(v)),
        crate::photokit::Deleted::Cancelled => Ok(None),
    }
}

#[cfg(not(target_os = "macos"))]
fn delete_in_apple_photos(uuids: &[String]) -> Result<Option<Vec<String>>, String> {
    if let Some(r) = fake_apple_delete(uuids) {
        return r;
    }
    Err(crate::tr("Apple Fotos gibt es nur auf dem Mac").into())
}

/// Test hook (only with `GRAINEO_DATA_DIR` and `GRAINEO_NO_PHOTOKIT`): Apple Photos is never asked; the deletion
/// "succeeds" for every identifier after `GRAINEO_PHOTOKIT_DELETE_MS` (default 1500 ms, the time the system dialog
/// takes), so the whole path – trash, Apple Photos, library, result – runs without a Photos library.
fn fake_apple_delete(uuids: &[String]) -> Option<Result<Option<Vec<String>>, String>> {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || crate::env::var_os("GRAINEO_NO_PHOTOKIT").is_none() {
        return None;
    }
    let ms = crate::env::var("GRAINEO_PHOTOKIT_DELETE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    std::thread::sleep(std::time::Duration::from_millis(ms));
    crate::log(&format!("delete: Apple Photos simulated (GRAINEO_NO_PHOTOKIT), {} photo(s)", uuids.len()));
    Some(Ok(Some(uuids.to_vec())))
}

static STATE: Mutex<Option<Arc<Mutex<State>>>> = Mutex::new(None);

/// Progress of the running deletion for the status: (photos done, photos total).
static PROGRESS: (std::sync::atomic::AtomicUsize, std::sync::atomic::AtomicUsize) = (std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicUsize::new(0));

/// "Fotos löschen · 12 von 40" for the status in the title bar.
pub fn status_text() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let (done, total) = (PROGRESS.0.load(Relaxed), PROGRESS.1.load(Relaxed));
    if total == 0 {
        crate::tr("Fotos löschen").to_string()
    } else {
        crate::trf!("Fotos löschen · {a} von {b}", a = (done + 1).min(total), b = total)
    }
}

/// Deleting is running (status in the title bar).
pub fn running() -> bool {
    state().lock().map(|z| z.running).unwrap_or(false)
}

fn state() -> Arc<Mutex<State>> {
    STATE.lock().unwrap().get_or_insert_with(Arc::default).clone()
}

impl App {
    /// Dialog "Delete rejected photos?" (destructive): first determine, then ask, then result.
    pub(crate) fn delete_dialog(&mut self, ctx: &egui::Context) {
        let z = state();
        // on open: capture candidates, plan in the background
        if self.delete_info.is_none() {
            self.delete_info = Some((0, 0, 0));
            // Selection (Del) or – without selection – all rejected photos
            let chosen: Option<std::collections::HashSet<i64>> = self.delete_selection.as_ref().map(|v| v.iter().copied().collect());
            let k: Vec<Candidate> = self
                .entries
                .iter()
                .filter(|e| match &chosen {
                    Some(g) => g.contains(&e.image),
                    None => e.meta.flag == crate::sidecar::Flag::Reject,
                })
                .map(|e| Candidate { image: e.image, file: e.file, path: e.path.clone(), state: e.state, copy: e.copy.is_some() })
                .collect();
            *z.lock().unwrap() = State::default();
            let z2 = z.clone();
            let c = ctx.clone();
            std::thread::spawn(move || {
                let p = plan(k);
                z2.lock().unwrap().plan = Some(p);
                c.request_repaint();
            });
        }
        let (plan, running, outcome) = {
            let g = z.lock().unwrap();
            (g.plan.clone(), g.running, g.outcome.clone())
        };
        // after the run: re-read the library once
        if self.delete_done_reload && outcome.is_some() {
            crate::log("delete: done, re-reading library");
            self.delete_done_reload = false;
            self.current = None;
            self.cache.clear();
            let loaded = z.lock().unwrap().loaded.take();
            self.lib.after_delete(loaded);
            self.sync_source(true);
            // continue with the right neighbour (to look at and possibly delete right away) if the active photo is gone
            if let Some(n) = self.delete_neighbor.take() {
                let gone = self.lib.active_image().is_none_or(|a| !self.entries.iter().any(|e| e.image == a));
                if gone && self.entries.iter().any(|e| e.image == n) {
                    self.lib.set_active(n);
                    self.sync_source(false);
                }
            }
        }
        // Result
        if let Some(e) = outcome {
            // plain deletion in Apple Photos without anything notable: just a notice, no result window
            if e.apple > 0 && e.trash == 0 && e.error.is_empty() && e.skipped.is_empty() {
                self.status = crate::trf!("{a} in Apple Fotos gelöscht (dort 30 Tage unter „Zuletzt gelöscht“)", a = dialog::count(e.apple, crate::tr("Foto"), crate::tr("Fotos")));
                self.confirm_delete = false;
                self.delete_selection = None;
                *z.lock().unwrap() = State::default();
                return;
            }
            let verdict = if !e.error.is_empty() { dialog::Verdict::Red } else if !e.skipped.is_empty() { dialog::Verdict::Yellow } else { dialog::Verdict::Green };
            let title = if e.error.is_empty() { if self.delete_selection.is_some() { crate::tr("Fotos gelöscht").to_string() } else { crate::tr("Abgelehnte Fotos gelöscht").to_string() } } else { crate::trf!("Gelöscht – {a}", a = dialog::count(e.error.len(), crate::tr("Fehler"), crate::tr("Fehler"))) };
            // one below the other and in Photos: how many, and where to (sidecars and library entries go without saying)
            let mut parts = vec![crate::trf!("{a} gelöscht", a = dialog::count(e.trash + e.apple, crate::tr("Foto"), crate::tr("Fotos")))];
            if e.trash > 0 && e.apple > 0 {
                parts.push(crate::trf!("{a} aus der Bibliothek → Papierkorb", a = dialog::number(e.trash)));
                parts.push(crate::trf!("{a} aus Apple Fotos → „Zuletzt gelöscht“ (30 Tage)", a = dialog::number(e.apple)));
            } else if e.apple > 0 {
                parts.push(crate::tr("aus Apple Fotos → „Zuletzt gelöscht“ (30 Tage)").to_string());
            } else if e.trash > 0 {
                parts.push(crate::tr("in den Papierkorb").to_string());
            }
            if !e.skipped.is_empty() {
                parts.push(crate::trf!("{a} übersprungen", a = dialog::number(e.skipped.len())));
            }
            let details: Vec<String> = e.error.iter().map(|f| crate::trf!("Fehler: {f}", f = f)).chain(e.skipped.iter().map(|u| crate::trf!("Übersprungen: {u}", u = u))).collect();
            let a = dialog::show(ctx, "loeschen-ergebnis", &title, dialog::WIDTH, |ui| dialog::outcome(ui, verdict, &parts.join("\n"), &details), Buttons::close());
            if a != Answer::Open {
                self.confirm_delete = false;
                self.delete_selection = None;
                *z.lock().unwrap() = State::default();
            }
            return;
        }
        let Some(p) = plan else {
            let wer = if self.delete_selection.is_some() { crate::tr("Fotos") } else { crate::tr("Abgelehnte Fotos") };
            let a = dialog::show(ctx, "loeschen", &crate::trf!("{who} werden ermittelt …", who = wer), dialog::WIDTH, |ui| dialog::progress(ui, None, crate::tr("Dateien prüfen …")), Buttons::destructive(crate::tr("In den Papierkorb"), false));
            if a == Answer::Cancel {
                self.confirm_delete = false;
                self.delete_selection = None;
            }
            return;
        };
        let n = p.disk.len() + p.missing.len() + p.copies.len() + p.apple.len();
        let only_apple = !p.apple.is_empty() && n == p.apple.len();
        let selection_mode = self.delete_selection.is_some();
        let title = match (n == 0 && p.skipped.is_empty(), selection_mode) {
            (true, true) => crate::tr("Nichts zu löschen").to_string(),
            (true, false) => crate::tr("Keine abgelehnten Fotos").to_string(),
            (false, true) => crate::trf!("{a} löschen?", a = dialog::count(n + p.skipped.len(), crate::tr("Foto"), crate::tr("Fotos"))),
            (false, false) => crate::trf!("{a} löschen?", a = dialog::count(n + p.skipped.len(), crate::tr("abgelehntes Foto"), crate::tr("abgelehnte Fotos"))),
        };
        let (mut with_jpegs, mut missing) = (self.delete_jpegs, self.delete_missing);
        let running_snapshot = running;
        // Only photos from Apple Photos: macOS asks there itself anyway (cannot be turned off) – no second prompt
        let a = if only_apple && !running {
            Answer::Primary
        } else {
            dialog::show(
            ctx,
            "loeschen",
            &title,
            dialog::WIDTH,
            |ui| {
                if n == 0 && p.skipped.is_empty() {
                    dialog::text(ui, if selection_mode { crate::tr("Es ist kein Foto ausgewählt, das sich löschen lässt.") } else { crate::tr("Abgelehnt wird mit X. In dieser Liste ist kein Foto abgelehnt.") });
                    return;
                }
                if only_apple {
                    dialog::chip(ui, &crate::trf!("{a} aus Apple Fotos → „Zuletzt gelöscht“ in Apple Fotos", a = dialog::count(p.apple.len(), crate::tr("Foto"), crate::tr("Fotos"))));
                    dialog::hint(ui, crate::tr("Fotos aus Apple Fotos werden dort gelöscht – mit iCloud-Fotos auch auf iPhone, iPad und allen anderen Geräten. Sie bleiben 30 Tage unter „Zuletzt gelöscht“ und lassen sich dort zurückholen. macOS fragt gleich noch einmal nach."));
                    ui.allocate_ui(egui::vec2(ui.available_width(), 26.0), |ui| {
                        if running_snapshot {
                            dialog::progress(ui, None, crate::tr("wartet auf Apple Fotos …"));
                        }
                    });
                    return;
                }
                dialog::text(ui, crate::tr("Dateien gehen in den Papierkorb und lassen sich von dort zurückholen. Nichts wird endgültig gelöscht."));
                // two numbers, one row each (fixed places)
                dialog::chip(ui, &crate::trf!("{a} auf der Platte · {b} → Papierkorb", a = dialog::count(p.disk.len(), crate::tr("Foto"), crate::tr("Fotos")), b = dialog::size(p.bytes)));
                dialog::chip(ui, &crate::trf!("{a}, deren Datei fehlt → nur aus der Bibliothek", a = dialog::count(p.missing.len(), crate::tr("Eintrag"), crate::tr("Einträge"))));
                if !p.apple.is_empty() {
                    dialog::chip(ui, &crate::trf!("{a} aus Apple Fotos → „Zuletzt gelöscht“ in Apple Fotos", a = dialog::count(p.apple.len(), crate::tr("Foto"), crate::tr("Fotos"))));
                    dialog::hint(ui, crate::tr("Fotos aus Apple Fotos werden dort gelöscht – mit iCloud-Fotos auch auf iPhone, iPad und allen anderen Geräten. Sie bleiben 30 Tage unter „Zuletzt gelöscht“ und lassen sich dort zurückholen. macOS fragt gleich noch einmal nach."));
                }
                if p.network > 0 {
                    dialog::hint(ui, &crate::trf!("{a} davon liegen auf Netzlaufwerken: dort gibt es keinen Papierkorb, die Dateien wandern in den Papierkorb-Ordner des Laufwerks und lassen sich unter Einstellungen → Speicher zurücklegen.", a = dialog::number(p.network)));
                }
                if !p.skipped.is_empty() {
                    let unreachable = p.skipped.iter().filter(|(_, r)| r == crate::tr("Original nicht erreichbar – Löschen geht erst nach dem Verbinden")).count();
                    if unreachable > 0 {
                        dialog::hint(ui, &crate::trf!("{a}: Original nicht erreichbar – Löschen geht erst, wenn der Speicherort verbunden ist", a = dialog::count(unreachable, crate::tr("Foto"), crate::tr("Fotos"))));
                    }
                    if p.skipped.len() > unreachable {
                        dialog::hint(ui, &crate::trf!("{a} übersprungen", a = dialog::number(p.skipped.len() - unreachable)));
                    }
                }
                dialog::options(ui, |ui| {
                    dialog::option(ui, &mut with_jpegs, crate::tr("Kamera-JPEGs mitlöschen"), &crate::trf!("{a} gefunden", a = dialog::count(p.jpegs.len(), crate::tr("gleichnamige Datei"), crate::tr("gleichnamige Dateien"))), p.jpegs.is_empty().then_some(crate::tr("Keine gleichnamigen JPEGs neben diesen Fotos")));
                    ui.add_space(2.0);
                    dialog::option(ui, &mut missing, crate::tr("Einträge fehlender Dateien entfernen"), &crate::trf!("{a} – nur die Bibliothek, keine Datei", a = dialog::count(p.missing.len(), crate::tr("Eintrag"), crate::tr("Einträge"))), p.missing.is_empty().then_some(crate::tr("Keine Einträge mit fehlender Datei")));
                });
                ui.allocate_ui(egui::vec2(ui.available_width(), 26.0), |ui| {
                    if running_snapshot {
                        dialog::progress(ui, None, crate::tr("löscht …"));
                    }
                });
            },
            if n == 0 {
                Buttons::close()
            } else if only_apple {
                Buttons::destructive(crate::tr("In Apple Fotos löschen"), !running)
            } else if !p.apple.is_empty() {
                Buttons::destructive(crate::tr("Löschen"), !running)
            } else {
                Buttons::destructive(crate::tr("In den Papierkorb"), !running)
            },
            )
        };
        self.delete_jpegs = with_jpegs;
        self.delete_missing = missing;
        // Test hook: GRAINEO_DELETE_YES=<s> confirms that many seconds after start
        let a = match crate::env::var("GRAINEO_DELETE_YES").ok().and_then(|v| v.parse::<f64>().ok()) {
            Some(t) if a == Answer::Open && !running => {
                static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                if ctx.input(|i| i.time) > t && !ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    crate::log("delete: confirmed by test hook");
                    Answer::Primary
                } else {
                    a
                }
            }
            _ => a,
        };
        match a {
            Answer::Primary if n > 0 && !running => {
                z.lock().unwrap().running = true;
                // Remember neighbour: the first photo right after the deleted ones, otherwise the last one before them
                let gone: std::collections::HashSet<i64> = p.disk.iter().map(|x| x.0).chain(p.missing.iter().copied()).chain(p.copies.iter().copied()).chain(p.apple.iter().map(|x| x.0)).collect();
                let pos: Vec<usize> = self.entries.iter().enumerate().filter(|(_, e)| gone.contains(&e.image)).map(|(i, _)| i).collect();
                self.delete_neighbor = pos.last().and_then(|&l| self.entries[l + 1..].iter().find(|e| !gone.contains(&e.image))).or_else(|| pos.first().and_then(|&f| self.entries[..f].iter().rev().find(|e| !gone.contains(&e.image)))).map(|e| e.image);
                // Stack: camera JPEG entries of the on-disk images
                let stack: Vec<(i64, i64)> = p.disk.iter().filter_map(|(b, ..)| Some((*b, self.lib.sidecar_jpeg_of(*b)?))).collect();
                let z2 = z.clone();
                let c = ctx.clone();
                let ticket = self.lib.load_ticket();
                std::thread::spawn(move || {
                    // a panic (trash, odd path) must not leave the dialog "running" for good
                    let e = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(p, with_jpegs, missing, stack)))
                        .unwrap_or_else(|_| Outcome { error: vec![crate::tr("Interner Fehler beim Löschen – bitte die Bibliothek prüfen").to_string()], ..Outcome::default() });
                    crate::log(&format!("delete: {e:?}"));
                    let loaded = library::db::Db::read_only(&library::db_path()).ok().and_then(|db| ticket.load(&db).ok());
                    let mut g = z2.lock().unwrap_or_else(|x| x.into_inner());
                    g.running = false;
                    g.loaded = loaded;
                    g.outcome = Some(e);
                    c.request_repaint();
                });
                self.delete_done_reload = true;
            }
            Answer::Cancel if !running => {
                self.confirm_delete = false;
                self.delete_selection = None;
            }
            _ => {}
        }
    }

    /// The photos a delete (or ⌫ in a collection) applies to: the selection, otherwise the active photo.
    pub(crate) fn delete_targets(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.selection().iter().filter_map(|&i| self.entries.get(i)).map(|e| e.image).collect();
        if ids.is_empty() {
            ids.extend(self.active_entry().and_then(|i| self.entries.get(i)).map(|e| e.image));
        }
        ids
    }

    /// Del / ⌫ (outside a collection), ⌘⌫ and "Move to Trash …" in the menu: move the selected photos (grid, film
    /// strip) or the active photo to the Trash after confirmation. Uses the same dialog as "Delete rejected".
    pub(crate) fn request_delete(&mut self) {
        if self.confirm_delete || (self.in_grid() && self.lib.not_in_catalog_view()) {
            return;
        }
        if self.in_grid() {
            if self.lib.series_ask_scope(crate::library_ui::ScopeKind::Delete) {
                return;
            }
            self.sync_source(true);
        }
        let ids = self.delete_targets();
        if ids.is_empty() {
            return;
        }
        self.delete_selection = Some(ids);
        self.delete_info = None;
        self.confirm_delete = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_photos_go_through_apple_photos_never_to_trash() {
        let dir = std::env::temp_dir().join(format!("graineo-delete-apple-{}", std::process::id()));
        let file = dir.join("Test.photoslibrary/originals/A/ABC-123.heic");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"heic").unwrap();
        let p = plan(vec![Candidate { image: 7, file: 1, path: file.clone(), state: 0, copy: false }]);
        assert_eq!(p.apple, vec![(7, "ABC-123".to_string())]);
        assert!(p.disk.is_empty() && p.missing.is_empty() && p.skipped.is_empty());
        assert!(file.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
