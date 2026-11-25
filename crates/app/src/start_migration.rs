//! Start with an older library: the conversion to the current schema (about half a minute for 65 000 photos) runs on
//! a helper thread, the window meanwhile shows the start card with the step and a progress bar (the user interface
//! stays alive, never a spinning wheel). Only when the library is ready the app itself is built; if the conversion
//! fails, the window says so and names the backup – the app does not start on a half converted library.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::Instant;

use eframe::egui;
use library::db::MigrationStep;

use crate::brand::{Brand, Hold};

/// The running conversion (helper thread and its result).
pub struct Run {
    rx: Receiver<Result<(), String>>,
    db: PathBuf,
    begin: Instant,
}

/// Starts the conversion on a helper thread if opening the library would start one (older schema).
pub fn begin() -> Option<Run> {
    let db = library::db_path();
    let from = library::db::migration_needed(&db)?;
    crate::log(&format!("library schema {from}: conversion to {} on a helper thread", library::db::VERSION));
    let (tx, rx) = channel();
    let path = db.clone();
    let spawned = std::thread::Builder::new().name("library-migration".into()).spawn(move || {
        let _ = tx.send(library::db::Db::open(&path).map(|_| ()).map_err(|e| e.to_string()));
    });
    if let Err(e) = spawned {
        // no thread: the app opens the library itself (blocking, as before)
        crate::log(&format!("library migration thread not started: {e}"));
        return None;
    }
    Some(Run { rx, db, begin: Instant::now() })
}

enum Stage {
    Converting(Run),
    Failed { error: String, backup: PathBuf },
    Ready(Box<crate::App>),
}

/// What eframe runs: the conversion screen first, then the app (without a pending conversion directly the app).
pub struct Start {
    stage: Stage,
    ctx: egui::Context,
    hwnd: Option<isize>,
    brand: Brand,
    /// progress bar as shown (never backwards, creeps ahead within a step)
    shown: f32,
    last: Option<Instant>,
}

impl Start {
    pub fn new(cc: &eframe::CreationContext, run: Option<Run>) -> Box<dyn eframe::App> {
        let Some(run) = run else {
            return Box::new(crate::App::new(cc));
        };
        crate::photo_display::init(cc);
        let hwnd = crate::native_window_handle(cc);
        crate::lr::apply_theme(&cc.egui_ctx);
        crate::prefs::apply_scale(&cc.egui_ctx);
        crate::theme::load(&cc.egui_ctx);
        cc.egui_ctx.on_end_pass("frame-watch", std::sync::Arc::new(|_| crate::frame_watch::end()));
        Box::new(Self { stage: Stage::Converting(run), ctx: cc.egui_ctx.clone(), hwnd, brand: Brand::new(), shown: 0.0, last: None })
    }

    fn step_text(step: MigrationStep) -> String {
        match step {
            MigrationStep::Backup => crate::tr("Sicherung wird angelegt …"),
            MigrationStep::Convert => crate::tr("Tabellen werden umgestellt …"),
            MigrationStep::Check => crate::tr("Prüfung …"),
        }
        .to_string()
    }

    /// Conversion screen; the result when the conversion is over.
    fn converting(&mut self, ui: &mut egui::Ui) -> Option<Result<(), String>> {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let Stage::Converting(run) = &self.stage else { return None };
        match run.rx.try_recv() {
            Ok(r) => {
                crate::log(&format!("library conversion over after {:.1} s", run.begin.elapsed().as_secs_f32()));
                return Some(r);
            }
            Err(TryRecvError::Disconnected) => return Some(Err("Hilfsthread beendet".into())),
            Err(TryRecvError::Empty) => {}
        }
        let (step, real) = library::db::migration_progress().unwrap_or((MigrationStep::Backup, 0.0));
        let now = Instant::now();
        let dt = self.last.map_or(0.0, |t| (now - t).as_secs_f32().min(0.2));
        self.last = Some(now);
        // the steps report only now and then: the bar creeps ahead a little between two reports
        self.shown = self.shown.max(real);
        self.shown += ((real + 0.08).min(0.97) - self.shown).max(0.0) * dt * 0.3;
        ui.painter().rect_filled(ui.max_rect(), 0.0, crate::lr::ui_rgb(0x0b, 0x0c, 0x0d));
        self.brand.show_hold(
            ctx,
            Hold {
                title: crate::tr("Bibliothek wird auf das neue Format umgestellt …").to_string(),
                step: Self::step_text(step),
                fraction: self.shown,
                note: crate::tr("Das dauert beim ersten Start etwa eine halbe Minute. Deine Fotos werden nicht verändert; eine Sicherung bleibt erhalten.").to_string(),
            },
        );
        None
    }

    fn failed(ui: &mut egui::Ui, error: &str, backup: &std::path::Path) {
        {
            let ctx = ui.ctx().clone();
            ui.painter().rect_filled(ui.max_rect(), 0.0, crate::lr::ui_rgb(0x0b, 0x0c, 0x0d));
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.28).max(20.0));
                ui.set_max_width(640.0);
                ui.label(egui::RichText::new(crate::tr("Die Bibliothek konnte nicht umgestellt werden")).size(16.0));
                ui.add_space(10.0);
                if backup.exists() {
                    ui.label(crate::tr("Es wurde nichts verändert, deine Fotos und die Bibliothek sind unversehrt. Eine Sicherung der Bibliothek liegt hier:"));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(backup.display().to_string()).monospace());
                } else {
                    // the backup itself failed: there is none, and nothing was touched
                    ui.label(crate::tr("Es wurde nichts verändert, deine Fotos und die Bibliothek sind unversehrt."));
                }
                ui.add_space(10.0);
                ui.label(egui::RichText::new(crate::trf!("Meldung: {e}", e = error)).weak());
                ui.add_space(16.0);
                if ui.button(crate::tr("graineo beenden")).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
    }
}

impl eframe::App for Start {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Stage::Ready(app) = &mut self.stage {
            app.raw_input_hook(ctx, raw_input);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Stage::Ready(app) = &mut self.stage {
            app.ui(ui, frame);
            return;
        }
        crate::frame_watch::begin(&ctx);
        crate::test_image::step(&ctx); // Test hook: GRAINEO_SCREENSHOT
        let db = match &self.stage {
            Stage::Converting(run) => Some(run.db.clone()),
            _ => None,
        };
        if let (Some(result), Some(db)) = (self.converting(ui), db) {
            match result {
                Ok(()) => {
                    crate::start_library_background();
                    self.stage = Stage::Ready(Box::new(crate::App::new_in(&self.ctx, self.hwnd)));
                    ctx.request_repaint();
                }
                Err(error) => {
                    crate::log(&format!("library conversion failed: {error}"));
                    self.stage = Stage::Failed { error, backup: library::db::migration_backup_path(&db) };
                }
            }
        }
        if let Stage::Failed { error, backup } = &self.stage {
            Self::failed(ui, error, backup);
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    fn on_exit(&mut self) {
        if let Stage::Ready(app) = &mut self.stage {
            app.on_exit();
        }
    }
}
