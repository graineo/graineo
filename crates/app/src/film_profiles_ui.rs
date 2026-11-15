//! Management of local film profiles. File work and scans run in the background.
use crate::{dialog, lr};
use eframe::egui;
use raw::film_profile::{self, Storage, Report, Options, Package};
use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Clone, Default)]
struct Snapshot {
    loaded: bool,
    running: bool,
    packages: Vec<Package>,
    options: Options,
    profile: Vec<(String, String)>,
    folder: Vec<(PathBuf, bool, bool)>,
    report: Option<Report>,
}
static STATE: Mutex<Option<Snapshot>> = Mutex::new(None);
static JOBS: AtomicUsize = AtomicUsize::new(0);
static SERIAL: Mutex<()> = Mutex::new(());
#[derive(Clone)]
enum Question {
    Package(String),
    Folder(PathBuf),
}
static QUESTION: Mutex<Option<Question>> = Mutex::new(None);

fn work(ctx: &egui::Context, f: impl FnOnce(&Storage) -> Report + Send + 'static) {
    {
        let mut lock = STATE.lock().unwrap();
        let s = lock.get_or_insert_with(Snapshot::default);
        s.running = true;
        JOBS.fetch_add(1, Ordering::SeqCst);
    }
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _serial = SERIAL.lock().unwrap();
        let a = Storage::standard();
        let mut report = f(&a);
        let (_, scan) = a.search_with_report();
        report.skipped += scan.skipped;
        report.messages.extend(
            scan.messages
                .into_iter()
                .take(100usize.saturating_sub(report.messages.len())),
        );
        let options = a.options();
        let mut folder: Vec<_> = options
            .folders
            .iter()
            .map(|p| (p.clone(), p.is_dir(), false))
            .collect();
        if options.discover {
            folder.extend(a.automatic.iter().map(|p| (p.clone(), p.is_dir(), true)));
        }
        film_profile::reread();
        let profile = film_profile::installed()
            .iter()
            .map(|p| (p.uuid.clone(), p.name.clone()))
            .collect();
        *STATE.lock().unwrap() = Some(Snapshot {
            loaded: true,
            running: JOBS.fetch_sub(1, Ordering::SeqCst) > 1,
            packages: a.packages(),
            options,
            folder,
            profile,
            report: Some(report),
        });
        ctx.request_repaint();
    });
}
fn snapshot(ctx: &egui::Context) -> Snapshot {
    let s = STATE.lock().unwrap().clone().unwrap_or_default();
    if !s.loaded && !s.running {
        work(ctx, |_| Report::default());
    }
    if !s.loaded || s.running {
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
    }
    s
}
pub fn manage() {
    crate::prefs::open_section("Looks");
}
/// Dialog ID of "Profilordner hinzufügen" (picked up in [`frame`]).
const FOLDER_DIALOG: &str = "film-profile-folder";

/// Open the system dialog through `file_dialog` (its thread, one dialog at a time); the chosen folder is added
/// in [`frame`].
pub fn add_folder(ctx: &egui::Context) {
    let title = crate::tr("Profilordner hinzufügen").to_string();
    crate::file_dialog::folder(ctx, FOLDER_DIALOG, move |d| d.set_title(title));
}
pub fn store(ctx: &egui::Context, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    manage();
    work(ctx, move |a| a.add_folder(&paths));
}
pub fn hide(ctx: &egui::Context, id: String) {
    work(ctx, move |a| outcome(a.show_profile(&id, false)));
}
fn outcome(r: Result<(), String>) -> Report {
    match r {
        Ok(()) => Report::default(),
        Err(e) => Report {
            skipped: 1,
            messages: vec![e],
            ..Default::default()
        },
    }
}

/// Once per frame: changes to the source list invalidate previews.
pub fn frame(ctx: &egui::Context) -> bool {
    let _ = snapshot(ctx);
    if let Some(p) = crate::file_dialog::fetch(FOLDER_DIALOG) {
        work(ctx, move |a| a.add_folder(&p));
    }
    let question = QUESTION.lock().unwrap().clone();
    if let Some(question) = question {
        let remove = matches!(&question, Question::Folder(_));
        let name = match &question {
            Question::Package(n) => n.clone(),
            Question::Folder(p) => p.display().to_string(),
        };
        let r = dialog::show(
            ctx,
            "film-profile-source",
            if remove {
                "Profilordner entfernen?"
            } else {
                "Look-Paket ausblenden?"
            },
            460.0,
            |ui| {
                dialog::chip(ui, &name);
                if remove {
                    dialog::text(
                        ui,
                        crate::tr("Nur der Ordnerverweis wird entfernt. Die Profildateien bleiben unverändert."),
                    );
                    dialog::hint(
                        ui,
                        crate::tr("Bilder behalten ihren Look-Verweis. Fehlt danach das Profil, warnt graineo vor dem Export."),
                    );
                } else {
                    dialog::text(
                        ui,
                        crate::tr("Das Paket verschwindet aus der Auswahl. Bereits bearbeitete Bilder nutzen ihren Look weiter."),
                    );
                }
                dialog::hint(
                    ui,
                    crate::tr("Profildateien werden weder kopiert, verschoben noch gelöscht. Abgeleitete alte Tabellen im App-Cache werden verworfen."),
                );
            },
            dialog::Buttons::confirm(if remove { "Entfernen" } else { "Ausblenden" }, true),
        );
        if r != dialog::Answer::Open {
            *QUESTION.lock().unwrap() = None;
            if r == dialog::Answer::Primary {
                work(ctx, move |a| match question {
                    Question::Folder(p) => outcome(a.remove_folder(&p)),
                    Question::Package(n) => outcome(a.show_package(&n, false)),
                });
            }
        }
    }
    let generation = film_profile::generation();
    ctx.data_mut(|d| {
        let id = egui::Id::unique("film-profile-generation");
        let old = d.get_temp::<u64>(id);
        d.insert_temp(id, generation);
        old.is_some_and(|g| g != generation)
    })
}

pub fn settings(ui: &mut egui::Ui) {
    let s = snapshot(ui.ctx());
    lr::group_label(ui, "Looks");
    ui.add(
        egui::Label::new(
            egui::RichText::new(crate::tr(film_profile::NOTICE))
                .small()
                .color(lr::TEXT_DIM),
        )
        .wrap(),
    );
    ui.add_space(8.0);
    ui.add_enabled_ui(!s.running, |ui| {
        let mut discover = s.options.discover;
        if ui
            .checkbox(&mut discover, crate::tr("Adobe-Ordner automatisch durchsuchen"))
            .changed()
        {
            work(ui.ctx(), move |a| {
                let mut o = a.options();
                o.discover = discover;
                outcome(a.write_options(&o))
            });
        }
        ui.horizontal(|ui| {
            if ui.button(crate::tr("Ordner hinzufügen …")).clicked() {
                add_folder(ui.ctx());
            }
            if ui.button(crate::tr("Jetzt neu suchen")).clicked() {
                work(ui.ctx(), |_| Report::default());
            }
        });
        ui.add_space(6.0);
        if s.loaded && s.packages.is_empty() {
            ui.label(crate::tr("Keine Looks gefunden"));
        }
        for (path, available, automatic) in &s.folder {
            egui::Frame::new()
                .fill(lr::PANEL_DARK)
                .inner_margin(8.0)
                .corner_radius(0.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string()).color(if *available {
                                lr::TEXT
                            } else {
                                lr::TEXT_DIM
                            }),
                        )
                        .wrap(),
                    );
                    ui.horizontal(|ui| {
                        ui.label(if !available {
                            crate::tr("Nicht verfügbar")
                        } else if *automatic {
                            crate::tr("Automatisch gefunden")
                        } else {
                            crate::tr("Referenzierter Ordner")
                        });
                        if !automatic && ui.button(crate::tr("Ordner entfernen …")).clicked() {
                            *QUESTION.lock().unwrap() = Some(Question::Folder(path.clone()));
                        }
                    });
                });
        }
        for package in &s.packages {
            egui::Frame::new()
                .fill(lr::PANEL_DARK)
                .inner_margin(10.0)
                .corner_radius(0.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let mut visible = package.visible;
                        if ui.checkbox(&mut visible, crate::tr("Anzeigen")).changed() {
                            let name = package.name.clone();
                            if visible {
                                work(ui.ctx(), move |a| outcome(a.show_package(&name, true)));
                            } else {
                                *QUESTION.lock().unwrap() = Some(Question::Package(name));
                            }
                        }
                        ui.label(egui::RichText::new(&package.name).strong().color(
                            if package.available > 0 {
                                lr::TEXT
                            } else {
                                lr::TEXT_DIM
                            },
                        ));
                    });
                    ui.label(crate::trf!("{a} Profile · {b} verfügbar", a = package.count, b = package.available));
                    ui.horizontal(|ui| {
                        if ui.button(crate::tr("Im Finder zeigen")).clicked() {
                            if let Some(p) = package.folders.first() {
                                crate::system::open(p);
                            }
                        }
                    });
                    if package.name == "Lookup-Tabellen" {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    crate::tr("Für sRGB-LUTs (3D, 1D-Kurve oder beides)."),
                                )
                                .small()
                                .color(lr::TEXT_DIM),
                            )
                            .wrap(),
                        );
                    }
                    ui.collapsing(crate::tr("Quellen"), |ui| {
                        for p in &package.folders {
                            ui.add(egui::Label::new(p.display().to_string()).wrap());
                        }
                    });
                });
        }
        if !s.options.hidden_profiles.is_empty() {
            ui.collapsing(crate::tr("Ausgeblendete Profile"), |ui| {
                let mut ids: Vec<_> = s.options.hidden_profiles.iter().collect();
                ids.sort();
                for id in ids {
                    ui.horizontal(|ui| {
                        ui.label(
                            s.profile
                                .iter()
                                .find(|(u, _)| u == id)
                                .map_or(id.as_str(), |(_, n)| n.as_str()),
                        );
                        if ui.button(crate::tr("Wieder anzeigen")).clicked() {
                            let id = id.clone();
                            work(ui.ctx(), move |a| outcome(a.show_profile(&id, true)));
                        }
                    });
                }
            });
        }
    });
    // A fixed status slot, also during a scan.
    ui.add_space(6.0);
    let text = if s.running {
        crate::tr("Wird gelesen …").into()
    } else if let Some(b) = &s.report {
        crate::trf!("{a} hinzugefügt · {b} bereits vorhanden · {c} übersprungen", a = b.imported, b = b.existing, c = b.skipped)
    } else {
        crate::tr("Wird gesucht …").into()
    };
    ui.label(egui::RichText::new(text).small().color(lr::TEXT_DIM));
    if let Some(b) = &s.report {
        if !b.messages.is_empty() {
            ui.collapsing("Details", |ui| {
                for m in &b.messages {
                    ui.add(egui::Label::new(m).wrap());
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_state_has_folder_link_and_hint() {
        let ctx = eframe::egui::Context::default();
        let output = crate::ui_test::run(&ctx, Default::default(), |ui| {
            super::settings(ui);
        });
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                eframe::egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("Ordner hinzufügen")));
        assert!(texts.iter().any(|t| t.contains("graineo enthält")));
        assert!(raw::film_profile::NOTICE.contains("graineo enthält keine Filmprofile"));
    }
}

#[cfg(test)]
mod visible_tests {
    #[test]
    fn long_profile_list_requests_only_visible_thumbnails() {
        use eframe::egui;
        let ctx = egui::Context::default();
        let labels = vec![(None, "Synthetic"); 189];
        let mut count = 0;
        let _ = crate::ui_test::run(&ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 400.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    count = crate::lr::thumb_grid_fav_menu(
                        ui,
                        "synthetic-profiles",
                        &labels,
                        None,
                        3,
                        &[],
                        true,
                    )
                    .visible
                    .len();
                });
            },
        );
        assert!(
            count > 0 && count < 24,
            "{count} von 189 statt nur sichtbaren Zellen"
        );
    }
}

/// Conspicuous card instead of a silent neutral preview; the stored reference stays.
pub fn missing_card(ui: &mut egui::Ui, s: &develop::Settings) {
    if let Some(text) = rawdev::film_profiles::missing(s) {
        egui::Frame::new().fill(egui::Color32::from_rgb(0x30,0x28,0x1c)).inner_margin(10.0).corner_radius(0.0).show(ui, |ui| {
            ui.set_width((ui.available_width()-20.0).max(40.0));
            ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(&text)).strong().color(egui::Color32::from_rgb(0xe8,0xbe,0x78))).wrap());
            ui.add(egui::Label::new(crate::tr("Die Vorschau enthält diesen Look nicht. Der Profilverweis bleibt gespeichert.")).wrap());
            if ui.button(crate::tr("Profile verwalten …")).clicked() { manage(); }
        });
    }
}

pub struct ExportCheck {
    job: Option<crate::jobs::Job>,
    outcome: std::sync::Arc<Mutex<Option<Vec<String>>>>,
}
pub enum ExportStep {
    Waiting,
    Aborted,
    Done(crate::jobs::Job),
}
impl ExportCheck {
    pub fn new(ctx: &egui::Context, job: crate::jobs::Job) -> Self {
        let settings = match &job {
            crate::jobs::Job::Export { items, .. } => items
                .iter()
                .map(|(_, s, _, _)| s.clone())
                .collect::<Vec<_>>(),
            _ => unreachable!(),
        };
        let outcome = std::sync::Arc::new(Mutex::new(None));
        let result = outcome.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut cache = std::collections::HashMap::new();
            let mut errors = Vec::new();
            for s in settings {
                if s.raw.look.starts_with("film:") && !s.film.is_active() {
                    let error = cache
                        .entry(s.raw.look.clone())
                        .or_insert_with(|| rawdev::film_profiles::check(&s).err());
                    if let Some(e) = error {
                        errors.push(e.clone());
                    }
                }
            }
            *result.lock().unwrap() = Some(errors);
            ctx.request_repaint();
        });
        Self {
            job: Some(job),
            outcome,
        }
    }
    pub fn step(&mut self, ctx: &egui::Context) -> ExportStep {
        let errors = self.outcome.lock().unwrap().clone();
        let Some(errors) = errors else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return ExportStep::Waiting;
        };
        if errors.is_empty() {
            return ExportStep::Done(self.job.take().unwrap());
        }
        let response = dialog::show(
            ctx,
            "film-profile-export",
            crate::tr("Ohne Look exportieren?"),
            470.0,
            |ui| {
                dialog::chip(ui, &crate::trf!("{a} betroffene Bilder", a = errors.len()));
                dialog::text(
                    ui,
                    crate::tr("Dieser Export hätte den Look nicht. Trotzdem exportieren?"),
                );
                let mut unique = errors.clone();
                unique.sort();
                unique.dedup();
                for e in unique.iter().take(5) {
                    dialog::hint(ui, e);
                }
                dialog::hint(
                    ui,
                    crate::tr("Die gespeicherten Einstellungen und Profilverweise bleiben unverändert."),
                );
            },
            dialog::Buttons::destructive(crate::tr("Trotzdem exportieren"), true),
        );
        match response {
            dialog::Answer::Primary => {
                let mut job = self.job.take().unwrap();
                if let crate::jobs::Job::Export {
                    allow_missing_profiles,
                    ..
                } = &mut job
                {
                    *allow_missing_profiles = true;
                }
                ExportStep::Done(job)
            }
            dialog::Answer::Cancel => {
                self.job = None;
                ExportStep::Aborted
            }
            dialog::Answer::Open => ExportStep::Waiting,
        }
    }
}

#[cfg(test)]
mod missing_tests {
    use super::*;
    fn settings() -> develop::Settings {
        let mut s = develop::Settings::default();
        s.raw.look = "film:NOT-INSTALLED-SYNTHETIC".into();
        s.raw.look_name = "Synthetic Film".into();
        s.raw.look_package = "Synthetic package".into();
        s
    }
    #[test]
    fn card_names_name_package_and_management() {
        let ctx = egui::Context::default();
        let s = settings();
        let output = crate::ui_test::run(&ctx, Default::default(), |ui| missing_card(ui, &s));
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("Synthetic Film (Synthetic package)"))
        );
        assert!(texts.iter().any(|t| t.contains("Profile verwalten")));
    }
    #[test]
    fn export_query_counts_and_enter_does_not_confirm() {
        use crate::jobs::Job;
        let ctx = egui::Context::default();
        let job = Job::Export {
            items: vec![],
            format: develop::export::Format::Jpeg { quality: 90 },
            engine: develop::export::Engine::Own,
            dir: std::env::temp_dir(),
            gain_map: false,
            allow_missing_profiles: false,
            as_print: false,
            color_space: graineo_color::Space::Srgb,
            options: Default::default(),
        };
        let mut export = ExportCheck {
            job: Some(job),
            outcome: std::sync::Arc::new(Mutex::new(Some(vec!["Missing profile".into(); 3]))),
        };
        let mut outcome = ExportStep::Waiting;
        // Modal windows need an initial sizing frame before their text is painted.
        let _ = crate::ui_test::run(&ctx, Default::default(), |_| {
            outcome = export.step(&ctx);
        });
        let enter = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let output = crate::ui_test::run(&ctx, enter, |_| {
            outcome = export.step(&ctx);
        });
        assert!(matches!(outcome, ExportStep::Waiting));
        assert!(export.job.is_some());
        assert!(output.shapes.iter().any(|s| matches!(&s.shape,egui::epaint::Shape::Text(t) if t.galley.text().contains("3 betroffene Bilder"))));
        let escape = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let _ = crate::ui_test::run(&ctx, escape, |_| {
            outcome = export.step(&ctx);
        });
        assert!(matches!(outcome, ExportStep::Aborted));
        assert!(export.job.is_none());
    }
    #[test]
    fn missing_look_is_copied_and_export_overwrites_nothing() {
        let s = settings();
        let copied = crate::paste::merge(
            &develop::Settings::default(),
            &s,
            &crate::paste::Groups::all(),
        );
        assert_eq!(copied.raw, s.raw);
        let root = std::path::PathBuf::from(
            crate::env::var_os("GRAINEO_DATA_DIR").expect("scratch data folder required"),
        )
        .join(format!(
            "film-reference-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("sample.png");
        image::RgbImage::from_pixel(64, 48, image::Rgb([128, 90, 60]))
            .save(&original)
            .unwrap();
        let bytes = std::fs::read(&original).unwrap();
        crate::raw_app::save_settings(
            &original,
            None,
            &copied,
            &crate::sidecar::Meta::default(),
        )
        .unwrap();
        let (loaded, _, stored) = crate::raw_app::load_settings(&original, None);
        assert!(stored);
        assert_eq!(loaded.raw, copied.raw);
        let doc = rawdev::RawDoc::open(&original, &|| true).unwrap().unwrap();
        let target = root.join("existing.jpg");
        std::fs::write(&target, b"must remain").unwrap();
        assert!(
            rawdev::export::export(
                &doc,
                &loaded,
                develop::export::Format::Jpeg { quality: 90 },
                &target
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"must remain");
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        let confirmed = rawdev::film_profiles::for_export(&loaded, true).unwrap();
        rawdev::export::export(
            &doc,
            &confirmed,
            develop::export::Format::Jpeg { quality: 90 },
            &root.join("confirmed.jpg"),
        )
        .unwrap();
        assert_eq!(loaded.raw.look, s.raw.look);
    }
}
