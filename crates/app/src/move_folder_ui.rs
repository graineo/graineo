//! "Move to …" for a folder of the library (context menu under FOLDERS): choose the target (non-blocking folder
//! dialog), plan in the background, confirm, then move – all in the background with progress.
//!
//! - Same volume, new folder: one rename ([`library::move_folder`]).
//! - Everything else (another location, or merging into an existing folder of the same name, or the contents into the
//!   chosen folder itself): a job folder by folder ([`library::move_job`]) – with progress (files, GB, remaining time),
//!   pause and resume (also after a restart), the Mac kept awake while it runs, and a pause with a hint when the Mac
//!   reaches the minimum free space.
//!
//! Test aid (only with `GRAINEO_DATA_DIR`): `GRAINEO_MOVE_TEST="folder=<absolute path>;dest=<absolute path>;
//! contents=1;yes=<s>;pause=<s>;resume=<s>"` plans the move without the folder dialog, confirms it after `yes` seconds
//! of program time, pauses it after `pause` seconds and resumes it after `resume` seconds.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use library::db::Db;
use library::move_folder::{self, Outcome, Phase, Plan, Progress};
use library::move_job::{self, Job, Stop};
use library::preview::Cache;

use crate::dialog::{self, Answer, Buttons, Verdict};

const DIALOG_ID: &str = "ordner-verschieben";
/// Assumed for the estimate: copy over the network / locally (bytes per second), developing one RAW (s).
const NET_RATE: f64 = 90e6;
const LOCAL_RATE: f64 = 250e6;
const DEVELOP_SECS: f64 = 2.0;

/// Files done / total of the running move for the status in the title bar.
static STATUS: std::sync::Mutex<(usize, usize)> = std::sync::Mutex::new((0, 0));

/// "Ordner verschieben · 12 von 40" (without a known count: "Ordner verschieben").
pub fn status_text() -> String {
    let (done, total) = *STATUS.lock().unwrap_or_else(|e| e.into_inner());
    if total == 0 {
        crate::tr("Ordner verschieben").to_string()
    } else {
        crate::trf!("Ordner verschieben · {a} von {b}", a = (done + 1).min(total), b = total)
    }
}

fn set_status(done: usize, total: usize) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = (done, total);
}

#[derive(Default)]
struct Run {
    progress: Progress,
    cancelling: bool,
    result: Option<Result<Outcome, String>>,
}

/// A running job (in its thread).
struct JobRun {
    progress: move_job::Progress,
    started: Instant,
    /// bytes counted when this run started (for the speed)
    base_bytes: Option<u64>,
    pausing: bool,
    result: Option<Result<(Job, Stop), String>>,
}

/// Finished move for the library view: re-read and show the result.
pub struct Done {
    pub title: String,
    pub verdict: Verdict,
    pub summary: String,
    pub rows: Vec<String>,
    pub details: Vec<String>,
}

enum Confirm {
    Rename(Plan),
    Job(move_job::Plan),
}

type Slot<T> = Arc<Mutex<Option<T>>>;

#[derive(Default)]
pub struct MoveFolder {
    /// folder (ID) waiting for the choice of the target
    chosen: Option<i64>,
    /// folder, chosen place, contents into it (for planning again when the option changes)
    request: Option<(i64, PathBuf, bool)>,
    planning: Option<Slot<Result<Confirm, String>>>,
    confirm: Option<Confirm>,
    run: Option<(Arc<Mutex<Run>>, Plan)>,
    job_run: Option<(Arc<Mutex<JobRun>>, Job)>,
    /// paused job (from this session or the last one)
    paused: Option<Job>,
    show_paused: bool,
    confirm_stop: bool,
    stopping: Option<Slot<Result<Job, String>>>,
    loaded: bool,
    reload: bool,
    cancel: Arc<AtomicBool>,
    test_started: bool,
    test_yes: Option<f64>,
    test_pause: Option<f64>,
    test_resume: Option<f64>,
    test_contents: bool,
}

/// Moving a folder needs the trash of the app: local volumes the macOS Trash, network drives the trash folder of the
/// root folder (no macOS Trash there).
pub fn dispose(list: &[PathBuf], root: &Path) -> Result<(), String> {
    let network = list.first().is_some_and(|p| library::network_trash::is_network(p));
    if network {
        library::network_trash::discard(root, list, &library::card_import::today()).map(|_| ())
    } else {
        trash::delete_all(list).map_err(|e| e.to_string())
    }
}

/// Interrupted operations are being finished in the background (set from start until done).
static RESUMING: AtomicBool = AtomicBool::new(false);
/// Messages of the finished resumption, fetched once by the library view.
static RESUMED: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// At startup: finish or revert interrupted folder moves and file moves/renames, register a paused job (the sync
/// leaves its folders alone). Runs in a background thread (file operations on the NAS can take minutes); every sync
/// waits for it ([`wait_for_resume`]), the messages come as a dialog ([`take_resumed`]).
pub fn resume_at_start() {
    if let Some(j) = move_job::load_open() {
        crate::log(&format!("move job: paused – {} ({} of {} folders)", j.title, j.dirs_done(), j.dirs.len()));
    }
    let open = library::relocate::journals().into_iter().any(|v| {
        (v.kind == move_folder::KIND && matches!(v.status.as_str(), "running" | "planned" | "rolling_back" | "cleanup"))
            || (v.kind.is_empty() && matches!(v.status.as_str(), "running" | "planned" | "rolling_back" | "undoing"))
    });
    if !open {
        return;
    }
    RESUMING.store(true, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("wiederaufnahme".into())
        .spawn(|| {
            struct Done;
            impl Drop for Done {
                fn drop(&mut self) {
                    RESUMING.store(false, Ordering::SeqCst);
                }
            }
            let _done = Done;
            let messages = match std::panic::catch_unwind(|| match Db::standard() {
                Ok(db) => {
                    let mut m = move_folder::resume(&db, &dispose);
                    m.extend(library::relocate::resume(&db));
                    m
                }
                Err(e) => vec![e.to_string()],
            }) {
                Ok(m) => m,
                Err(_) => vec![crate::tr("Wiederaufnahme abgebrochen (interner Fehler)").to_string()],
            };
            for m in &messages {
                crate::log(&format!("resume: {m}"));
            }
            *RESUMED.lock().unwrap_or_else(|e| e.into_inner()) = Some(messages);
        })
        .map_err(|e| crate::log(&format!("resume: thread not started: {e}")))
        .ok();
}

/// Interrupted operations are still being finished.
pub fn resuming() -> bool {
    RESUMING.load(Ordering::SeqCst)
}

/// For background jobs that touch the library (sync): wait until the resumption at startup is done.
pub fn wait_for_resume() {
    while resuming() {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Messages of the resumption at startup (once, when it is done).
pub fn take_resumed() -> Option<Vec<String>> {
    RESUMED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

fn remaining_time(secs: f64) -> String {
    let s = secs.round().max(1.0) as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s.div_ceil(60)),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

impl MoveFolder {
    /// Context menu "Move to …": choose the target (or show the paused job).
    pub fn request(&mut self, ctx: &egui::Context, folder: i64, source: PathBuf) {
        if self.busy() {
            return;
        }
        if self.paused.is_some() {
            self.show_paused = true;
            return;
        }
        self.chosen = Some(folder);
        let start = source.parent().map(Path::to_path_buf);
        crate::file_dialog::target_folder(ctx, DIALOG_ID, move |d| {
            let d = d.set_title(crate::tr("Ziel für den Ordner wählen"));
            match start {
                Some(s) => d.set_directory(s),
                None => d,
            }
        });
    }

    /// A move is being planned, confirmed or running.
    pub fn busy(&self) -> bool {
        self.planning.is_some() || self.confirm.is_some() || self.run.is_some() || self.job_run.is_some() || self.stopping.is_some()
    }

    /// Parts of the library changed (a job paused or stopped): the view should re-read it.
    pub fn take_reload(&mut self) -> bool {
        std::mem::take(&mut self.reload)
    }

    fn plan(&mut self, ctx: &egui::Context, folder: i64, dest: PathBuf, contents: bool, cache: Option<Arc<Cache>>) {
        let slot = Arc::new(Mutex::new(None));
        self.planning = Some(slot.clone());
        self.request = Some((folder, dest.clone(), contents));
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("move-folder-plan".into())
            .spawn(move || {
                let r = (|| -> Result<Confirm, String> {
                    let db = Db::standard().map_err(|e| e.to_string())?;
                    if !contents {
                        // same volume into a new folder: one rename
                        if let Ok(p) = move_folder::plan(&db, folder, &dest) {
                            if p.same_volume {
                                return Ok(Confirm::Rename(p));
                            }
                        }
                    }
                    let cache = cache.unwrap_or_else(|| Arc::new(Cache::standard()));
                    move_job::plan(&db, folder, &dest, contents, Some(&cache)).map(Confirm::Job)
                })();
                *slot.lock().unwrap() = Some(r);
                c.request_repaint();
            })
            .ok();
    }

    /// Test aid, see the module description.
    fn test_hook(&mut self, ctx: &egui::Context, cache: Option<Arc<Cache>>, folder_of: &dyn Fn(&Path) -> Option<i64>) {
        if self.test_started || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let Ok(spec) = crate::env::var("GRAINEO_MOVE_TEST") else { return };
        let mut folder = None;
        let mut dest = None;
        for part in spec.split(';') {
            match part.split_once('=') {
                Some(("folder", v)) => folder = Some(PathBuf::from(v)),
                Some(("dest", v)) => dest = Some(PathBuf::from(v)),
                Some(("yes", v)) => self.test_yes = v.parse().ok(),
                Some(("pause", v)) => self.test_pause = v.parse().ok(),
                Some(("resume", v)) => self.test_resume = v.parse().ok(),
                Some(("contents", v)) => self.test_contents = v == "1",
                _ => {}
            }
        }
        if self.paused.is_some() {
            self.test_started = true;
            return;
        }
        let (Some(f), Some(d)) = (folder, dest) else { return };
        let Some(id) = folder_of(&f) else { return };
        self.test_started = true;
        let contents = self.test_contents;
        self.plan(ctx, id, d, contents, cache);
    }

    /// Per frame (after drawing). `cache`: the library's preview store (previews move along under the new key);
    /// `job`: a job of the library is running (a move waits for it). Returns the result once the move is finished.
    pub fn update(&mut self, ctx: &egui::Context, cache: Option<Arc<Cache>>, job: Option<&crate::dialog::Running>, folder_of: &dyn Fn(&Path) -> Option<i64>) -> Option<Done> {
        if !self.loaded {
            self.loaded = true;
            self.paused = move_job::jobs().into_iter().find(Job::open);
            self.show_paused = self.paused.is_some();
        }
        self.test_hook(ctx, cache.clone(), folder_of);
        if let Some(dest) = crate::file_dialog::fetch_one(DIALOG_ID) {
            if let Some(folder) = self.chosen.take() {
                self.plan(ctx, folder, dest, false, cache.clone());
            }
        }
        if let Some(slot) = &self.planning {
            let r = slot.lock().unwrap().take();
            match r {
                Some(Ok(plan)) => {
                    self.planning = None;
                    self.confirm = Some(plan);
                }
                Some(Err(e)) => {
                    self.planning = None;
                    self.confirm = None;
                    return Some(Done { title: crate::tr("Ordner nicht verschoben").into(), verdict: Verdict::Red, summary: e, rows: Vec::new(), details: Vec::new() });
                }
                None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        if self.planning.is_some() && self.confirm.is_none() {
            // planning reads every folder once (over the network that can take a minute)
            let answer = dialog::show(
                ctx,
                "ordner-verschieben-plan",
                crate::tr("Verschieben vorbereiten"),
                dialog::WIDTH,
                |ui| {
                    dialog::progress(ui, None, crate::tr("Ordner und Ziel werden verglichen …"));
                },
                Buttons { cancel: crate::tr("Abbrechen"), primary: None },
            );
            if answer == Answer::Cancel {
                self.planning = None;
                self.request = None;
            }
        }
        if self.confirm.is_some() {
            if let Some(d) = self.confirm_dialog(ctx, cache.clone(), job) {
                return Some(d);
            }
        }
        if self.run.is_some() {
            return self.progress_dialog(ctx);
        }
        if self.job_run.is_some() {
            return self.job_dialog(ctx);
        }
        if let Some(d) = self.stop_result() {
            return Some(d);
        }
        if self.paused.is_some() && self.stopping.is_none() {
            self.paused_dialog(ctx, cache, job);
        }
        None
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, cache: Option<Arc<Cache>>, job: Option<&crate::dialog::Running>) -> Option<Done> {
        let sync_running = job.is_some();
        let replanning = self.planning.is_some();
        let mut contents = self.request.as_ref().is_some_and(|r| r.2);
        let dest_name = self.request.as_ref().and_then(|r| r.1.file_name().map(|n| n.to_string_lossy().to_string())).unwrap_or_default();
        let (title, primary) = match self.confirm.as_ref()? {
            Confirm::Rename(p) => (crate::trf!("„{a}“ verschieben?", a = p.name), crate::tr("Verschieben")),
            Confirm::Job(p) if p.merge => (crate::trf!("„{a}“ zusammenführen?", a = p.name), crate::tr("Zusammenführen")),
            Confirm::Job(p) => (crate::trf!("„{a}“ verschieben?", a = p.name), crate::tr("Verschieben")),
        };
        let mut toggled = false;
        let confirm = self.confirm.as_ref()?;
        let mut answer = dialog::show(
            ctx,
            "ordner-verschieben-frage",
            &title,
            dialog::WIDTH,
            |ui| match confirm {
                Confirm::Rename(plan) => {
                    ui.horizontal(|ui| {
                        dialog::chip(ui, &dialog::count(plan.photos.len(), crate::tr("Foto"), crate::tr("Fotos")));
                        dialog::chip(ui, &dialog::size(plan.bytes));
                        crate::help::info(ui, "ordner-verschieben", crate::tr("Verschieben nach …"), &crate::help::MOVE_FOLDER);
                    });
                    ui.add_space(4.0);
                    dialog::text(ui, &crate::trf!("Ziel: {a}", a = plan.target.display()));
                    dialog::text(ui, crate::tr("Gleiches Laufwerk: Der Ordner wird nur umbenannt."));
                    if let Some(r) = job {
                        dialog::blocked(ui, crate::tr("Das Verschieben beginnt danach."), r);
                    }
                }
                Confirm::Job(plan) => {
                    let g = &plan.figures;
                    ui.horizontal(|ui| {
                        dialog::chip(ui, &dialog::count(g.photos, crate::tr("Foto"), crate::tr("Fotos")));
                        dialog::chip(ui, &dialog::size(g.bytes));
                        crate::help::info(ui, "ordner-verschieben", crate::tr("Verschieben nach …"), &crate::help::MOVE_FOLDER);
                    });
                    ui.add_space(4.0);
                    dialog::text(ui, &crate::trf!("Ziel: {a}", a = plan.target.display()));
                    if plan.merge {
                        dialog::text(ui, crate::tr("Den Ordner gibt es dort schon. Die Dateien werden in die vorhandenen Ordner einsortiert; nichts wird überschrieben."));
                    } else if plan.same_volume {
                        dialog::text(ui, crate::tr("Gleiches Laufwerk: Die Dateien werden umbenannt, nicht kopiert."));
                    } else {
                        dialog::text(ui, crate::tr("Jede Datei wird kopiert und geprüft; erst dann kommt das Original in den Papierkorb."));
                    }
                    ui.add_space(2.0);
                    dialog::hint(ui, &crate::trf!("Neu kopieren: {a} Dateien ({b})", a = dialog::number(g.copy_files), b = dialog::size(g.copy_bytes)));
                    if g.same_files > 0 {
                        dialog::hint(ui, &crate::trf!("Gleichnamig am Ziel: {a} Dateien ({b}) – bei gleicher Prüfsumme gelten sie als vorhanden, sonst bleiben sie hier", a = dialog::number(g.same_files), b = dialog::size(g.same_bytes)));
                    }
                    if g.conflict_files > 0 {
                        dialog::warning(ui, &crate::trf!("{a} Dateien bleiben hier: Am Ziel liegt eine andere Datei gleichen Namens", a = dialog::number(g.conflict_files)));
                    }
                    if g.merge_rows > 0 {
                        dialog::hint(ui, &crate::trf!("{a} Fotos kennt die Bibliothek schon am Ziel: Die Einträge werden zusammengeführt", a = dialog::number(g.merge_rows)));
                    }
                    if g.missing_rows > 0 {
                        dialog::hint(ui, &crate::trf!("{a} fehlende Fotos bleiben am alten Ort in der Bibliothek", a = dialog::number(g.missing_rows)));
                    }
                    if !g.kept_folders.is_empty() || g.kept_files > 0 {
                        let mut names = g.kept_folders.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
                        if g.kept_folders.len() > 4 {
                            names.push_str(", …");
                        }
                        dialog::hint(ui, &crate::trf!("Bleiben liegen (Lightroom-Katalog, Mediathek von Apple Fotos, Pakete anderer Programme): {a}", a = if names.is_empty() { crate::trf!("{a} Katalogdateien", a = dialog::number(g.kept_files)) } else { names }));
                    }
                    if plan.offline {
                        dialog::hint(ui, &crate::trf!("Offline-Kopien vorbereiten: {a} Fotos ({b} RAWs entwickeln), etwa {c} auf diesem Mac", a = dialog::number(g.offline_missing), b = dialog::number(g.offline_develop), c = dialog::size(g.offline_bytes)));
                        if let Some(f) = g.free_local {
                            dialog::hint(ui, &crate::trf!("Frei auf diesem Mac: {a}, davon bleiben {b} frei", a = dialog::size(f), b = dialog::size(g.min_free)));
                        }
                        if let Some(short) = g.offline_shortfall() {
                            dialog::warning(ui, &crate::trf!("Der Platz reicht nicht für alle Offline-Kopien (es fehlen etwa {a}). Das Verschieben hält an, wenn nur noch {b} frei sind. Dann den Papierkorb leeren – dort liegen die schon verschobenen Originale – und fortsetzen.", a = dialog::size(short), b = dialog::size(g.min_free)));
                        }
                    }
                    if let Some(f) = g.free_target.filter(|_| !plan.same_volume) {
                        dialog::hint(ui, &crate::trf!("Frei am Ziel: {a}", a = dialog::size(f)));
                    }
                    let network = !library::watch::local(&plan.target);
                    let secs = move_job::estimate_secs(g, if network { NET_RATE } else { LOCAL_RATE }, DEVELOP_SECS, plan.same_volume);
                    dialog::hint(ui, &crate::trf!("Geschätzte Dauer: etwa {a}. Der Mac bleibt so lange wach; anhalten und später fortsetzen geht jederzeit.", a = remaining_time(secs)));
                    if let Some(r) = job {
                        dialog::blocked(ui, crate::tr("Das Verschieben beginnt danach."), r);
                    }
                    let parent_name = if plan.contents { dest_name.clone() } else { plan.target.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default() };
                    dialog::options(ui, |ui| {
                        let label = crate::trf!("Inhalt direkt in „{a}“ einsortieren", a = parent_name);
                        let expl = crate::trf!("statt als Unterordner „{a}“", a = plan.name);
                        if dialog::option(ui, &mut contents, &label, &expl, replanning.then(|| crate::tr("Wird neu berechnet …"))) {
                            toggled = true;
                        }
                    });
                }
            },
            Buttons::confirm(primary, !sync_running && !replanning),
        );
        if toggled {
            if let Some((folder, dest, _)) = self.request.clone() {
                self.plan(ctx, folder, dest, contents, cache.clone());
            }
            return None;
        }
        if let Some(t) = self.test_yes {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            if answer == Answer::Open && !sync_running && !replanning && ctx.input(|i| i.time) > t {
                answer = Answer::Primary;
            }
        }
        match answer {
            Answer::Cancel => {
                self.confirm = None;
                self.planning = None;
            }
            Answer::Primary if !sync_running && !replanning => match self.confirm.take()? {
                Confirm::Rename(plan) => self.start(ctx, plan, cache),
                Confirm::Job(plan) => {
                    let job = Job::new(&plan);
                    if let Err(e) = job.save() {
                        return Some(Done { title: crate::tr("Ordner nicht verschoben").into(), verdict: Verdict::Red, summary: e, rows: Vec::new(), details: Vec::new() });
                    }
                    self.start_job(ctx, job, cache);
                }
            },
            _ => {
                if sync_running {
                    ctx.request_repaint_after(std::time::Duration::from_millis(500));
                }
            }
        }
        None
    }

    // ───────────────────────────── rename on the same volume

    fn start(&mut self, ctx: &egui::Context, plan: Plan, cache: Option<Arc<Cache>>) {
        let run = Arc::new(Mutex::new(Run::default()));
        self.cancel.store(false, Ordering::SeqCst);
        self.run = Some((run.clone(), plan.clone()));
        let (cancel, c) = (self.cancel.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("move-folder".into())
            .spawn(move || {
                let r2 = run.clone();
                let c2 = c.clone();
                let progress = move |p: Progress| {
                    set_status(p.done, p.total);
                    r2.lock().unwrap().progress = p;
                    c2.request_repaint();
                };
                let result = (|| -> Result<Outcome, String> {
                    let db = Db::standard().map_err(|e| e.to_string())?;
                    let cache = cache.unwrap_or_else(|| Arc::new(Cache::standard()));
                    let out = move_folder::execute(&db, &plan, Some(&cache), &cancel, &progress, &dispose)?;
                    crate::log(&format!("move folder: {} files renamed → {}", out.files, plan.target.display()));
                    Ok(out)
                })();
                run.lock().unwrap().result = Some(result);
                c.request_repaint();
            })
            .ok();
    }

    fn progress_dialog(&mut self, ctx: &egui::Context) -> Option<Done> {
        let (run, plan) = self.run.as_ref()?;
        let finished = run.lock().unwrap().result.take();
        if let Some(r) = finished {
            let plan = plan.clone();
            self.run = None;
            return Some(outcome(&plan, r));
        }
        let (p, cancelling) = {
            let g = run.lock().unwrap();
            (g.progress.clone(), g.cancelling)
        };
        let title = crate::trf!("„{a}“ wird verschoben", a = plan.name);
        let answer = dialog::show(
            ctx,
            "ordner-verschieben-lauf",
            &title,
            dialog::WIDTH,
            |ui| {
                dialog::text(ui, &crate::trf!("Ziel: {a}", a = plan.target.display()));
                ui.add_space(4.0);
                if cancelling {
                    dialog::progress(ui, None, crate::tr("Wird abgebrochen …"));
                    return;
                }
                match p.phase {
                    Phase::Switch => dialog::progress(ui, None, crate::tr("Die Bibliothek wird umgestellt …")),
                    _ => dialog::progress(ui, None, crate::tr("Der Ordner wird umbenannt …")),
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            },
            Buttons { cancel: crate::tr("Abbrechen"), primary: None },
        );
        if answer == Answer::Cancel && matches!(p.phase, Phase::Prepare | Phase::Copy) {
            self.cancel.store(true, Ordering::SeqCst);
            run.lock().unwrap().cancelling = true;
        }
        None
    }

    // ───────────────────────────── jobs (between locations, merging)

    fn start_job(&mut self, ctx: &egui::Context, job: Job, cache: Option<Arc<Cache>>) {
        let run = Arc::new(Mutex::new(JobRun { progress: Default::default(), started: Instant::now(), base_bytes: None, pausing: false, result: None }));
        self.cancel.store(false, Ordering::SeqCst);
        self.paused = None;
        self.show_paused = false;
        self.job_run = Some((run.clone(), job.clone()));
        let (cancel, c) = (self.cancel.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("move-job".into())
            .spawn(move || {
                let awake = library::platform::KeepAwake::new();
                let mut job = job;
                let r2 = run.clone();
                let c2 = c.clone();
                let progress = move |p: &move_job::Progress| {
                    set_status(p.files, p.files_total);
                    let mut r = r2.lock().unwrap();
                    if r.base_bytes.is_none() {
                        r.base_bytes = Some(p.bytes);
                        r.started = Instant::now();
                    }
                    r.progress = p.clone();
                    drop(r);
                    c2.request_repaint();
                };
                let result = (|| -> Result<Stop, String> {
                    let db = Db::standard().map_err(|e| e.to_string())?;
                    let cache = cache.unwrap_or_else(|| Arc::new(Cache::standard()));
                    let space = move_job::space_of(&cache);
                    let t = Instant::now();
                    let stop = move_job::run(&db, &mut job, &cache, &cancel, &progress, &dispose, &space)?;
                    crate::log(&format!(
                        "move job: {:?} after {:.0} s, awake {} – {} of {} folders, {} copied ({:.1} MB), {} present, {} merged, {} conflicts → {}",
                        stop,
                        t.elapsed().as_secs_f64(),
                        awake.active(),
                        job.dirs_done(),
                        job.dirs.len(),
                        job.report.files_copied,
                        job.report.bytes_copied as f64 / 1e6,
                        job.report.files_present,
                        job.report.rows_merged,
                        job.report.conflict_files,
                        job.target.display()
                    ));
                    Ok(stop)
                })();
                drop(awake);
                run.lock().unwrap().result = Some(result.map(|s| (job, s)));
                c.request_repaint();
            })
            .ok();
    }

    fn job_dialog(&mut self, ctx: &egui::Context) -> Option<Done> {
        let (run, job) = self.job_run.as_ref()?;
        let finished = run.lock().unwrap().result.take();
        if let Some(r) = finished {
            let job0 = job.clone();
            self.job_run = None;
            self.reload = true;
            return match r {
                Ok((job, Stop::Finished)) => Some(job_outcome(&job)),
                Ok((job, Stop::Paused(_))) => {
                    self.paused = Some(job);
                    self.show_paused = true;
                    None
                }
                Err(e) => {
                    // not started (e.g. the library could not be opened): it stays paused
                    let mut j = move_job::jobs().into_iter().find(|j| j.id == job0.id).unwrap_or(job0);
                    j.message = e;
                    self.paused = Some(j);
                    self.show_paused = true;
                    None
                }
            };
        }
        let (p, started, base, pausing) = {
            let g = run.lock().unwrap();
            (g.progress.clone(), g.started, g.base_bytes, g.pausing)
        };
        if let Some(t) = self.test_pause {
            if ctx.input(|i| i.time) > t && !pausing {
                self.test_pause = None;
                self.cancel.store(true, Ordering::SeqCst);
                run.lock().unwrap().pausing = true;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let title = if job.merge { crate::trf!("„{a}“ wird zusammengeführt", a = job.name) } else { crate::trf!("„{a}“ wird verschoben", a = job.name) };
        let answer = dialog::show(
            ctx,
            "ordner-verschieben-auftrag",
            &title,
            dialog::WIDTH,
            |ui| {
                dialog::text(ui, &crate::trf!("Ziel: {a}", a = job.target.display()));
                ui.add_space(4.0);
                if pausing {
                    dialog::progress(ui, None, crate::tr("Wird angehalten …"));
                    return;
                }
                let total = p.bytes_total.max(1);
                let f = p.bytes as f32 / total as f32;
                let line = crate::trf!(
                    "Ordner {a} von {b} · Datei {c} von {d} · {e} von {f}",
                    a = dialog::number((p.dir + 1).min(p.dirs.max(1))),
                    b = dialog::number(p.dirs),
                    c = dialog::number(p.files.min(p.files_total)),
                    d = dialog::number(p.files_total),
                    e = dialog::size(p.bytes),
                    f = dialog::size(p.bytes_total)
                );
                let secs = started.elapsed().as_secs_f64();
                let done_now = p.bytes.saturating_sub(base.unwrap_or(p.bytes));
                dialog::progress(ui, Some(f), &line);
                // own line (fixed height): speed and remaining time once they can be told
                let speed_line = if secs > 10.0 && done_now > 0 {
                    let speed = done_now as f64 / secs;
                    let rest = p.bytes_total.saturating_sub(p.bytes) as f64 / speed;
                    crate::trf!("{a}/s · noch etwa {b}", a = dialog::size(speed as u64), b = remaining_time(rest))
                } else {
                    crate::tr("Restzeit wird ermittelt …").to_string()
                };
                dialog::hint(ui, &speed_line);
                let phase = match p.phase {
                    move_job::Phase::Check => crate::tr("Vergleichen"),
                    move_job::Phase::Previews => crate::tr("Vorschauen und Offline-Kopien vorbereiten"),
                    move_job::Phase::Copy => crate::tr("Kopieren und prüfen"),
                    move_job::Phase::Switch => crate::tr("Bibliothek umstellen"),
                    move_job::Phase::Trash => crate::tr("Alte Dateien in den Papierkorb"),
                };
                dialog::hint(ui, &format!("{phase} · {}", if p.rel.is_empty() { job.name.clone() } else { p.rel.trim_end_matches('/').to_string() }));
                if p.prepared > 0 {
                    dialog::hint(ui, &crate::trf!("Vorschauen vorbereitet: {a} Fotos", a = dialog::number(p.prepared)));
                }
                dialog::hint(ui, crate::tr("Der Mac bleibt wach, solange das Verschieben läuft. Anhalten geht jederzeit; fortsetzen später, auch nach einem Neustart."));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            },
            Buttons { cancel: crate::tr("Anhalten"), primary: None },
        );
        if answer == Answer::Cancel && !pausing {
            self.cancel.store(true, Ordering::SeqCst);
            run.lock().unwrap().pausing = true;
        }
        None
    }

    fn paused_dialog(&mut self, ctx: &egui::Context, cache: Option<Arc<Cache>>, library_job: Option<&crate::dialog::Running>) {
        let sync_running = library_job.is_some();
        if !self.show_paused {
            return;
        }
        let Some(job) = self.paused.clone() else { return };
        if self.confirm_stop {
            let answer = dialog::show(
                ctx,
                "ordner-verschieben-beenden",
                crate::tr("Verschieben beenden?"),
                dialog::WIDTH,
                |ui| {
                    dialog::text(ui, crate::tr("Fertige Ordner bleiben am Ziel. Was im angefangenen Ordner schon kopiert ist, wird dort wieder entfernt; alles Übrige bleibt am alten Ort."));
                    dialog::hint(ui, &crate::trf!("{a} von {b} Ordnern fertig", a = dialog::number(job.dirs_done()), b = dialog::number(job.dirs.len())));
                },
                Buttons::destructive(crate::tr("Beenden"), true),
            );
            match answer {
                Answer::Cancel => self.confirm_stop = false,
                Answer::Primary => {
                    self.confirm_stop = false;
                    let slot = Arc::new(Mutex::new(None));
                    self.stopping = Some(slot.clone());
                    let c = ctx.clone();
                    let mut j = job;
                    std::thread::Builder::new()
                        .name("move-job-stop".into())
                        .spawn(move || {
                            let r = (|| {
                                let db = Db::standard().map_err(|e| e.to_string())?;
                                let cache = cache.unwrap_or_else(|| Arc::new(Cache::standard()));
                                move_job::stop(&db, &mut j, &cache, &dispose)?;
                                Ok(j)
                            })();
                            *slot.lock().unwrap() = Some(r);
                            c.request_repaint();
                        })
                        .ok();
                }
                Answer::Open => {}
            }
            return;
        }
        let space = job.message.starts_with(move_job::PAUSED_FOR_SPACE);
        let mut stop_clicked = false;
        let mut answer = dialog::show(
            ctx,
            "ordner-verschieben-angehalten",
            crate::tr("Verschieben angehalten"),
            dialog::WIDTH,
            |ui| {
                ui.horizontal(|ui| {
                    dialog::chip(ui, &crate::trf!("{a} von {b} Ordnern", a = dialog::number(job.dirs_done()), b = dialog::number(job.dirs.len())));
                    dialog::chip(ui, &crate::trf!("{a} von {b}", a = dialog::size(job.report.bytes_done), b = dialog::size(job.bytes_total)));
                    crate::help::info(ui, "ordner-verschieben-angehalten", crate::tr("Verschieben nach …"), &crate::help::MOVE_FOLDER);
                });
                ui.add_space(4.0);
                dialog::text(ui, &crate::trf!("„{a}“ nach {b}", a = job.name, b = job.target.display()));
                dialog::hint(ui, &crate::trf!("Verschoben: {a} Fotos", a = dialog::number(job.report.photos_moved)));
                if space {
                    let free = cache.as_ref().and_then(|c| c.free_space()).unwrap_or(0);
                    let min = cache.as_ref().map_or(library::preview::MIN_FREE, |c| c.min_free);
                    dialog::warning(ui, &crate::trf!("Auf diesem Mac sind nur noch {a} frei; {b} bleiben immer frei. Papierkorb leeren – dort liegen die schon verschobenen Originale –, dann fortsetzen.", a = dialog::size(free), b = dialog::size(min)));
                } else if job.message == move_job::PAUSED_BY_USER {
                    dialog::text(ui, crate::tr("Angehalten. Fortsetzen geht jederzeit, auch nach einem Neustart."));
                } else if !job.message.is_empty() {
                    dialog::warning(ui, &job.message);
                    dialog::hint(ui, crate::tr("Ist das Laufwerk getrennt: verbinden, dann fortsetzen."));
                }
                dialog::hint(ui, crate::tr("Abgleich und Lightroom-Import warten, bis das Verschieben fertig oder beendet ist."));
                if let Some(r) = library_job {
                    dialog::blocked(ui, crate::tr("Das Verschieben geht danach weiter."), r);
                }
                ui.add_space(4.0);
                if ui.add(egui::Button::new(crate::tr("Verschieben beenden …")).corner_radius(0.0)).clicked() {
                    stop_clicked = true;
                }
            },
            Buttons { cancel: crate::tr("Später"), primary: Some((crate::tr("Fortsetzen"), dialog::Style::Normal, !sync_running)) },
        );
        if stop_clicked {
            self.confirm_stop = true;
            return;
        }
        if let Some(t) = self.test_resume {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            if answer == Answer::Open && !sync_running && ctx.input(|i| i.time) > t {
                self.test_resume = None;
                answer = Answer::Primary;
            }
        }
        match answer {
            Answer::Cancel => self.show_paused = false,
            Answer::Primary if !sync_running => {
                if let Some(j) = self.paused.take() {
                    self.start_job(ctx, j, cache);
                }
            }
            _ => {}
        }
    }

    fn stop_result(&mut self) -> Option<Done> {
        let r = self.stopping.as_ref()?.lock().unwrap().take()?;
        self.stopping = None;
        self.reload = true;
        match r {
            Ok(job) => {
                self.paused = None;
                self.show_paused = false;
                let mut d = job_outcome(&job);
                d.title = crate::tr("Verschieben beendet").into();
                d.verdict = Verdict::Yellow;
                d.summary = crate::trf!("{a} von {b} Ordnern sind am Ziel; der Rest liegt unverändert am alten Ort.", a = dialog::number(job.dirs_done()), b = dialog::number(job.dirs.len()));
                Some(d)
            }
            Err(e) => Some(Done { title: crate::tr("Verschieben nicht beendet").into(), verdict: Verdict::Red, summary: e, rows: Vec::new(), details: Vec::new() }),
        }
    }
}

fn outcome(plan: &Plan, r: Result<Outcome, String>) -> Done {
    let photos = dialog::count(plan.photos.len(), crate::tr("Foto"), crate::tr("Fotos"));
    match r {
        Err(e) if e == library::relocate::CANCELLED => Done {
            title: crate::tr("Verschieben abgebrochen").into(),
            verdict: Verdict::Yellow,
            summary: crate::trf!("Nichts verschoben – „{a}“ liegt unverändert am alten Ort.", a = plan.name),
            rows: Vec::new(),
            details: Vec::new(),
        },
        Err(e) => Done {
            title: crate::tr("Ordner nicht verschoben").into(),
            verdict: Verdict::Red,
            summary: crate::trf!("„{a}“ liegt unverändert am alten Ort.", a = plan.name),
            rows: Vec::new(),
            details: vec![e],
        },
        Ok(o) => {
            let verdict = if o.notes.is_empty() { Verdict::Green } else { Verdict::Yellow };
            Done { title: crate::tr("Ordner verschoben").into(), verdict, summary: crate::trf!("{a} nach {b}", a = photos, b = plan.target.display()), rows: o.notes, details: Vec::new() }
        }
    }
}

fn job_outcome(job: &Job) -> Done {
    let r = &job.report;
    let mut rows = Vec::new();
    rows.push(crate::trf!("Kopiert und geprüft: {a} Dateien ({b})", a = dialog::number(r.files_copied), b = dialog::size(r.bytes_copied)));
    if r.files_present > 0 {
        rows.push(crate::trf!("Am Ziel schon vorhanden (gleiche Prüfsumme): {a} Dateien ({b})", a = dialog::number(r.files_present), b = dialog::size(r.bytes_present)));
    }
    if r.rows_merged > 0 {
        rows.push(crate::trf!("Doppelte Einträge zusammengeführt: {a}", a = dialog::number(r.rows_merged)));
    }
    if r.conflict_files > 0 {
        rows.push(crate::trf!("Nicht verschoben, weil am Ziel eine andere Datei gleichen Namens liegt: {a} Dateien (siehe Details)", a = dialog::number(r.conflict_files)));
    }
    if r.missing_rows > 0 {
        rows.push(crate::trf!("Fehlende Fotos blieben am alten Ort: {a}", a = dialog::number(r.missing_rows)));
    }
    rows.push(crate::trf!("Vorschauen: {a} neu, {b} mitgenommen", a = dialog::number(r.previews_made), b = dialog::number(r.previews_moved)));
    if !r.without_preview.is_empty() {
        rows.push(crate::trf!("Ohne Vorschau (Datei nicht lesbar): {a}", a = dialog::number(r.without_preview.len())));
    }
    rows.push(crate::trf!("Dauer: {a}", a = remaining_time(r.secs)));
    rows.extend(r.notes.iter().cloned());
    let verdict = if r.conflict_files == 0 && r.without_preview.is_empty() && r.notes.is_empty() { Verdict::Green } else { Verdict::Yellow };
    let mut details: Vec<String> = r.conflicts.clone();
    details.extend(r.without_preview.iter().map(|p| crate::trf!("ohne Vorschau: {a}", a = p)));
    Done {
        title: if job.merge { crate::tr("Ordner zusammengeführt").into() } else { crate::tr("Ordner verschoben").into() },
        verdict,
        summary: crate::trf!("{a} nach {b}", a = dialog::count(r.photos_moved, crate::tr("Foto"), crate::tr("Fotos")), b = job.target.display()),
        rows,
        details,
    }
}
