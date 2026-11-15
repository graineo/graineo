//! "Locate missing folder …" (context menu of a missing folder or root folder under FOLDERS): choose where the folder
//! is now (non-blocking folder dialog), compare in the background, confirm (with the number of photos found), then
//! switch the library over in the background ([`library::relink`]). The files themselves stay untouched.
//!
//! Test aid (only with `GRAINEO_DATA_DIR`): `GRAINEO_RELINK_TEST="folder=<old absolute path>;dest=<absolute path>;yes=<s>"`
//! plans without the folder dialog and confirms after `yes` seconds of program time (without `yes` the confirmation
//! stays open, e.g. for a screenshot).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui;
use library::db::Db;
use library::preview::Cache;
use library::relink::{self, Found, Outcome, Plan};

use crate::dialog::{self, Answer, Buttons, Verdict};
use crate::move_folder_ui::Done;

const DIALOG_ID: &str = "ordner-suchen";

type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

#[derive(Default)]
pub struct Relink {
    /// folder (ID, old path) waiting for the choice in the folder dialog
    chosen: Option<(i64, PathBuf)>,
    /// comparing in the background: (old path, chosen folder, result)
    planning: Option<(PathBuf, PathBuf, Slot<Plan>)>,
    confirm: Option<Plan>,
    run: Option<(Plan, Slot<Outcome>)>,
    test_started: bool,
    test_yes: Option<f64>,
}

/// Finished relink: result dialog and the root folder to sync afterwards.
pub struct Relinked {
    pub done: Done,
    pub sync_root: Option<PathBuf>,
}

fn folder_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

impl Relink {
    /// Context menu "Locate missing folder …": choose where it is now. The dialog starts at the nearest folder of the
    /// old path that still exists.
    pub fn request(&mut self, ctx: &egui::Context, folder: i64, old: PathBuf) {
        if self.busy() {
            return;
        }
        let start = old.ancestors().skip(1).find(|p| p.is_dir()).map(Path::to_path_buf);
        let title = crate::trf!("Wo liegt „{a}“ jetzt?", a = folder_name(&old));
        self.chosen = Some((folder, old));
        crate::file_dialog::folder(ctx, DIALOG_ID, move |d| {
            let d = d.set_title(title);
            match start {
                Some(s) => d.set_directory(s),
                None => d,
            }
        });
    }

    /// A relink is being compared, confirmed or running.
    pub fn busy(&self) -> bool {
        self.planning.is_some() || self.confirm.is_some() || self.run.is_some()
    }

    fn plan(&mut self, ctx: &egui::Context, folder: i64, old: PathBuf, target: PathBuf) {
        let slot: Slot<Plan> = Arc::new(Mutex::new(None));
        self.planning = Some((old, target.clone(), slot.clone()));
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("relink-plan".into())
            .spawn(move || {
                let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| relink::plan(&db, folder, &target));
                *slot.lock().unwrap() = Some(r);
                c.request_repaint();
            })
            .ok();
    }

    /// Test aid, see the module description.
    fn test_hook(&mut self, ctx: &egui::Context, folder_of: &dyn Fn(&Path) -> Option<i64>) {
        if self.test_started || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
            return;
        }
        let Ok(spec) = crate::env::var("GRAINEO_RELINK_TEST") else { return };
        let (mut folder, mut dest) = (None, None);
        for part in spec.split(';') {
            match part.split_once('=') {
                Some(("folder", v)) => folder = Some(PathBuf::from(v)),
                Some(("dest", v)) => dest = Some(PathBuf::from(v)),
                Some(("yes", v)) => self.test_yes = v.parse().ok(),
                _ => {}
            }
        }
        let (Some(f), Some(d)) = (folder, dest) else { return };
        let Some(id) = folder_of(&f) else { return };
        self.test_started = true;
        self.plan(ctx, id, f, d);
    }

    /// Per frame (after drawing). `cache`: the library's preview store (previews move along under the new key);
    /// `job`: a job of the library is running (relinking waits for it). Returns the result once finished.
    pub fn update(&mut self, ctx: &egui::Context, cache: Option<Arc<Cache>>, job: Option<&crate::dialog::Running>, folder_of: &dyn Fn(&Path) -> Option<i64>) -> Option<Relinked> {
        self.test_hook(ctx, folder_of);
        if let Some(target) = crate::file_dialog::fetch_one(DIALOG_ID) {
            if let Some((folder, old)) = self.chosen.take() {
                self.plan(ctx, folder, old, target);
            }
        }
        if self.planning.is_some() {
            return self.planning_dialog(ctx);
        }
        if self.confirm.is_some() {
            self.confirm_dialog(ctx, cache, job);
        }
        if self.run.is_some() {
            return self.run_dialog(ctx);
        }
        None
    }

    fn planning_dialog(&mut self, ctx: &egui::Context) -> Option<Relinked> {
        let (old, target, slot) = self.planning.as_ref()?;
        let r = slot.lock().unwrap().take();
        match r {
            Some(Ok(plan)) => {
                self.planning = None;
                self.confirm = Some(plan);
                ctx.request_repaint();
                None
            }
            Some(Err(e)) => {
                let name = folder_name(old);
                self.planning = None;
                Some(Relinked { done: Done { title: crate::tr("Ordner nicht verknüpft").into(), verdict: Verdict::Red, summary: crate::trf!("„{a}“ bleibt als fehlend markiert.", a = name), rows: Vec::new(), details: vec![e] }, sync_root: None })
            }
            None => {
                let title = crate::trf!("„{a}“ suchen", a = folder_name(old));
                let target = target.display().to_string();
                let answer = dialog::show(
                    ctx,
                    "ordner-suchen-pruefen",
                    &title,
                    dialog::WIDTH,
                    |ui| {
                        dialog::text(ui, &crate::trf!("Gewählt: {a}", a = target));
                        ui.add_space(4.0);
                        dialog::progress(ui, None, crate::tr("Fotos werden verglichen …"));
                    },
                    Buttons { cancel: crate::tr("Abbrechen"), primary: None },
                );
                // cancelling only drops the result: the comparison only reads
                if answer == Answer::Cancel {
                    self.planning = None;
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
                None
            }
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, cache: Option<Arc<Cache>>, job: Option<&crate::dialog::Running>) {
        let sync_running = job.is_some();
        let Some(plan) = self.confirm.as_ref() else { return };
        let name = folder_name(&plan.old);
        let complete = plan.complete();
        let title = if complete { crate::trf!("„{a}“ neu verknüpfen?", a = name) } else { crate::trf!("„{a}“ trotzdem neu verknüpfen?", a = name) };
        let found = crate::trf!("{a} von {b} Fotos gefunden", a = dialog::number(plan.found()), b = dialog::number(plan.total()));
        let (absent, other_size, other_time) = (plan.count(Found::Absent), plan.count(Found::OtherSize), plan.count(Found::OtherTime));
        let mut answer = dialog::show(
            ctx,
            "ordner-suchen-frage",
            &title,
            dialog::WIDTH,
            |ui| {
                ui.horizontal(|ui| {
                    dialog::chip(ui, &found);
                    crate::help::info(ui, "ordner-suchen", crate::tr("Fehlenden Ordner suchen"), &crate::help::RELINK_FOLDER);
                });
                ui.add_space(4.0);
                dialog::text(ui, &crate::trf!("Bisher: {a}", a = plan.old.display()));
                dialog::text(ui, &crate::trf!("Jetzt: {a}", a = plan.target.display()));
                if absent > 0 {
                    dialog::text(ui, &if absent == 1 { crate::tr("1 Foto ist dort nicht – es bleibt als fehlend markiert.").to_string() } else { crate::trf!("{a} Fotos sind dort nicht – sie bleiben als fehlend markiert.", a = dialog::number(absent)) });
                }
                if other_size > 0 {
                    dialog::text(ui, &if other_size == 1 { crate::tr("1 Datei heißt gleich, ist aber anders groß – sie bleibt als fehlend markiert, bis der Abgleich sie prüft.").to_string() } else { crate::trf!("{a} Dateien heißen gleich, sind aber anders groß – sie bleiben als fehlend markiert, bis der Abgleich sie prüft.", a = dialog::number(other_size)) });
                }
                if other_time > 0 {
                    dialog::hint(ui, &if other_time == 1 { crate::tr("1 Foto mit gleicher Größe, aber anderer Änderungszeit wird mit verknüpft.").to_string() } else { crate::trf!("{a} Fotos mit gleicher Größe, aber anderer Änderungszeit werden mit verknüpft.", a = dialog::number(other_time)) });
                }
                if plan.extra > 0 {
                    dialog::hint(ui, &if plan.extra == 1 { crate::tr("1 weiteres Foto im Ordner nimmt der Abgleich danach auf.").to_string() } else { crate::trf!("{a} weitere Fotos im Ordner nimmt der Abgleich danach auf.", a = dialog::number(plan.extra)) });
                }
                dialog::hint(ui, crate::tr("Die Dateien bleiben, wie sie sind; nur die Bibliothek wird umgestellt. Bewertungen, Stichwörter, Sammlungen und Entwicklung bleiben erhalten."));
                if plan.lightroom {
                    dialog::hint(ui, crate::tr("Lightroom-Ordner: Ein späterer Lightroom-Import nimmt den Ort wieder aus dem Katalog. Den Ordner deshalb auch in Lightroom suchen lassen."));
                }
                if let Some(r) = job {
                    dialog::blocked(ui, crate::tr("Das Verknüpfen beginnt danach."), r);
                }
            },
            Buttons::confirm(if complete { crate::tr("Verknüpfen") } else { crate::tr("Trotzdem verknüpfen") }, !sync_running),
        );
        if let Some(t) = self.test_yes {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            if answer == Answer::Open && !sync_running && ctx.input(|i| i.time) > t {
                answer = Answer::Primary;
            }
        }
        match answer {
            Answer::Cancel => self.confirm = None,
            Answer::Primary if !sync_running => {
                if let Some(plan) = self.confirm.take() {
                    self.start(ctx, plan, cache);
                }
            }
            _ => {
                if sync_running {
                    ctx.request_repaint_after(std::time::Duration::from_millis(500));
                }
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, plan: Plan, cache: Option<Arc<Cache>>) {
        let slot: Slot<Outcome> = Arc::new(Mutex::new(None));
        self.run = Some((plan.clone(), slot.clone()));
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("relink".into())
            .spawn(move || {
                let t = std::time::Instant::now();
                let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| relink::execute(&db, &plan, cache.as_deref()));
                if let Ok(o) = &r {
                    crate::log(&format!("relink: {} → {}: {} found, {} not found, {} previews moved in {:.2} s", plan.old.display(), plan.target.display(), o.found, o.not_found, o.previews_moved, t.elapsed().as_secs_f64()));
                }
                *slot.lock().unwrap() = Some(r);
                c.request_repaint();
            })
            .ok();
    }

    fn run_dialog(&mut self, ctx: &egui::Context) -> Option<Relinked> {
        let (plan, slot) = self.run.as_ref()?;
        let r = slot.lock().unwrap().take();
        let Some(r) = r else {
            let title = crate::trf!("„{a}“ wird neu verknüpft", a = folder_name(&plan.old));
            dialog::show(ctx, "ordner-suchen-lauf", &title, dialog::WIDTH, |ui| dialog::progress(ui, None, crate::tr("Die Bibliothek wird umgestellt …")), Buttons { cancel: crate::tr("Bitte warten"), primary: None });
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return None;
        };
        let plan = plan.clone();
        self.run = None;
        let name = folder_name(&plan.old);
        Some(match r {
            Err(e) => Relinked { done: Done { title: crate::tr("Ordner nicht verknüpft").into(), verdict: Verdict::Red, summary: crate::trf!("„{a}“ bleibt als fehlend markiert.", a = name), rows: Vec::new(), details: vec![e] }, sync_root: None },
            Ok(o) => {
                let mut rows = vec![crate::trf!("Vorschauen mitgenommen: {a}", a = dialog::number(o.previews_moved))];
                if o.not_found > 0 {
                    rows.push(if o.not_found == 1 { crate::tr("1 Foto fehlt weiterhin").to_string() } else { crate::trf!("{a} Fotos fehlen weiterhin", a = dialog::number(o.not_found)) });
                }
                let photos = dialog::count(o.found, crate::tr("Foto"), crate::tr("Fotos"));
                Relinked {
                    done: Done {
                        title: crate::tr("Ordner neu verknüpft").into(),
                        verdict: if o.not_found == 0 { Verdict::Green } else { Verdict::Yellow },
                        summary: crate::trf!("{a} wieder verknüpft: {b}", a = photos, b = plan.target.display()),
                        rows,
                        details: Vec::new(),
                    },
                    sync_root: Some(plan.target.clone()),
                }
            }
        })
    }
}
