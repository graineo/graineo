//! An import interrupted by a crash or quit (journal without end, see `library::import_journal`): offered once after the
//! start – "Rest importieren" opens the import window with the same source and settings and only the missing photos
//! checked; files already copied are recognised as being in the library (or found in the target by their checksum).

use std::sync::Mutex;

use super::*;
use library::import_journal::{self as journal, Unfinished};

enum State {
    /// not looked yet
    Fresh,
    /// reading the journals in the background
    Looking,
    Found(Unfinished),
    Done,
}

static STATE: Mutex<State> = Mutex::new(State::Fresh);

impl LibraryView {
    /// Look for an interrupted import (once per start, in the background) and offer its rest.
    pub(super) fn import_resume(&mut self, ctx: &egui::Context) {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            State::Fresh => {
                // test runs only when asked for (screenshots of other views must not get this window)
                if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var_os("GRAINEO_IMPORT_RESUME").is_none() {
                    *g = State::Done;
                    return;
                }
                *g = State::Looking;
                let ctx = ctx.clone();
                std::thread::Builder::new()
                    .name("import-journal".into())
                    .spawn(move || {
                        let found = journal::unfinished().into_iter().next();
                        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = match found {
                            Some(u) => State::Found(u),
                            None => State::Done,
                        };
                        ctx.request_repaint();
                    })
                    .ok();
            }
            State::Looking | State::Done => {}
            State::Found(u) => {
                let u = u.clone();
                drop(g);
                self.resume_dialog(ctx, u);
            }
        }
    }

    fn resume_dialog(&mut self, ctx: &egui::Context, u: Unfinished) {
        let rest: Vec<PathBuf> = u.rest().into_iter().map(|p| p.path).collect();
        let opt = ci::Options::from_json(&u.settings);
        // a camera: the window waits for it (it is opened as soon as it is connected)
        let from_camera = opt.source.as_deref().is_some_and(library::import_source::is_camera);
        let source_there = from_camera || opt.source.as_deref().is_some_and(crate::path_check::folder_exists);
        let photos = |n: usize| dialog::count(n, crate::tr("Datei"), crate::tr("Dateien"));
        let answer = dialog::show(
            ctx,
            "import-unterbrochen",
            "Import unterbrochen",
            dialog::WIDTH,
            |ui| {
                dialog::text(ui, &crate::trf!("Der letzte Import wurde unterbrochen: {a} von {b} fertig.", a = dialog::number(u.done.len()), b = photos(u.planned.len())));
                if from_camera {
                    dialog::hint(ui, crate::tr("Quelle: Kamera – bitte anschließen und einschalten, das Import-Fenster wartet auf sie."));
                } else if let Some(q) = &opt.source {
                    dialog::hint(ui, &crate::trf!("Quelle: {q}", q = q.display()));
                }
                if !source_there {
                    dialog::warning(ui, crate::tr("Die Quelle ist gerade nicht verbunden (Karte einlegen oder Laufwerk verbinden)."));
                }
            },
            Buttons { cancel: "Verwerfen", primary: Some(("Rest importieren", dialog::Style::Normal, source_there)) },
        );
        match answer {
            Answer::Open => {
                if !source_there {
                    ctx.request_repaint_after(std::time::Duration::from_secs(2));
                }
            }
            Answer::Cancel => {
                journal::dismiss(&u);
                *STATE.lock().unwrap_or_else(|e| e.into_inner()) = State::Done;
            }
            Answer::Primary => {
                journal::dismiss(&u);
                *STATE.lock().unwrap_or_else(|e| e.into_inner()) = State::Done;
                self.import_open_with(ctx, opt.source.clone(), Some(rest.into_iter().collect()));
                if let Some(imp) = &mut self.import {
                    imp.mode = opt.mode;
                    imp.mode_automatic = false;
                    imp.target = opt.target.clone().or(imp.target.take());
                    imp.target_missing = None;
                    imp.by_date = opt.by_date;
                    imp.backup = opt.backup.clone();
                    imp.keywords = opt.keywords.join(", ");
                    imp.stars = opt.stars;
                    imp.label = opt.label;
                    imp.copyright = opt.copyright.clone();
                    match &opt.collection {
                        CollectionTarget::Nothing => imp.collection = Choice::Nothing,
                        CollectionTarget::Existing(id) => imp.collection = Choice::Existing(*id),
                        CollectionTarget::New(n) => {
                            // the collection may exist by now (created at the end of the interrupted run)
                            match self.collections.iter().find(|c| c.kind == 0 && c.name == *n) {
                                Some(c) => imp.collection = Choice::Existing(c.id),
                                None => {
                                    imp.collection = Choice::New;
                                    imp.new_collection = n.clone();
                                    imp.name_prefilled = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
