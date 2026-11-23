//! "Metadaten neu einlesen" (menu Foto, context menu of the grid): reads the shooting data of the selection – without
//! a selection of the folder or collection shown – again from the files (`library::metadata_reread`).
//!
//! First a quick look whether sidecars lie beside the photos (background); only then a dialog asks whether rating,
//! label and keywords should be taken over from them as well (off by default). The reading runs in its own thread at
//! background priority, shows its progress in the status (cancellable from the list) and ends with one line
//! "N Fotos gelesen, M geändert, K nicht erreichbar". ⌘Z puts the previous values back.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static DONE: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);

/// What a background step left for the interface.
pub(super) enum Finished {
    /// sidecars looked for: the files and how many of them have one
    Checked { files: Vec<i64>, photos: usize, sidecars: usize },
    Done(Result<library::metadata_reread::Outcome, String>),
}

static RESULT: Mutex<Option<Finished>> = Mutex::new(None);

/// Progress of a running reading (done, total).
pub fn progress() -> Option<(usize, usize)> {
    RUNNING.load(Ordering::Relaxed).then(|| (DONE.load(Ordering::Relaxed), TOTAL.load(Ordering::Relaxed)))
}

/// Cancel the running reading (from the activity list); what was read so far stays and ⌘Z covers it.
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// The result line ("12 Fotos gelesen, 3 geändert, 1 nicht erreichbar").
pub fn summary(o: &library::metadata_reread::Outcome) -> String {
    let mut t = crate::trf!("{a} gelesen, {m} geändert", a = crate::dialog::count(o.read, crate::tr("Foto"), crate::tr("Fotos")), m = crate::dialog::number(o.changed));
    if o.unreachable > 0 {
        t.push_str(&crate::trf!(", {k} nicht erreichbar", k = crate::dialog::number(o.unreachable)));
    }
    if o.cancelled {
        t.push_str(&format!(" ({})", crate::tr("abgebrochen")));
    }
    t
}

fn put(f: Finished, ctx: &egui::Context) {
    *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    ctx.request_repaint();
}

impl LibraryView {
    /// Files the command acts on: the selection (one photo when only one is active), without a selection all
    /// photos shown (folder, collection). Virtual copies count once (one file).
    pub(super) fn reread_files(&self) -> (Vec<i64>, usize) {
        let images: Vec<i64> = if self.nothing_selected || self.active.is_none() { self.visible.iter().filter_map(|&i| self.rows.get(i)).map(|z| z.image).collect() } else { self.target() };
        let mut files: Vec<i64> = images.iter().filter_map(|b| self.index.get(b).map(|&i| &self.rows[i])).filter(|z| z.status != 3).map(|z| z.file).collect();
        let photos = images.len();
        files.sort_unstable();
        files.dedup();
        (files, photos)
    }

    /// "Metadaten neu einlesen" chosen: look for sidecars in the background, then ask or start.
    pub fn reread_metadata(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if RUNNING.load(Ordering::Relaxed) {
            actions.push(Action::Status(crate::tr("Metadaten werden bereits eingelesen.").to_string()));
            return;
        }
        let (files, photos) = self.reread_files();
        if files.is_empty() {
            actions.push(Action::Status(crate::tr("Keine Fotos zum Einlesen.").to_string()));
            return;
        }
        let Some(path) = self.db.as_ref().map(|d| d.path.clone()) else { return };
        let c = ctx.clone();
        RUNNING.store(true, Ordering::Relaxed);
        DONE.store(0, Ordering::Relaxed);
        TOTAL.store(files.len(), Ordering::Relaxed);
        CANCEL.store(false, Ordering::Relaxed);
        std::thread::spawn(move || {
            library::platform::background_priority();
            let sidecars = Db::open(&path).map_err(|e| e.to_string()).and_then(|db| library::metadata_reread::with_sidecar(&db, &files, &CANCEL)).unwrap_or(0);
            RUNNING.store(false, Ordering::Relaxed);
            put(Finished::Checked { files, photos, sidecars }, &c);
        });
    }

    fn start_reread(&mut self, files: Vec<i64>, sidecar: bool, ctx: &egui::Context) {
        let Some(path) = self.db.as_ref().map(|d| d.path.clone()) else { return };
        let c = ctx.clone();
        RUNNING.store(true, Ordering::Relaxed);
        DONE.store(0, Ordering::Relaxed);
        TOTAL.store(files.len(), Ordering::Relaxed);
        CANCEL.store(false, Ordering::Relaxed);
        std::thread::spawn(move || {
            library::platform::background_priority();
            let r = Db::open(&path).map_err(|e| e.to_string()).and_then(|db| {
                library::metadata_reread::run(&db, &files, sidecar, &CANCEL, &|d, _| {
                    DONE.store(d, Ordering::Relaxed);
                    c.request_repaint();
                })
            });
            RUNNING.store(false, Ordering::Relaxed);
            put(Finished::Done(r), &c);
        });
    }

    /// Per frame: results of the background steps; the question about sidecars.
    pub(super) fn reread_poll(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let finished = RESULT.lock().unwrap_or_else(|e| e.into_inner()).take();
        match finished {
            Some(Finished::Checked { files, photos, sidecars }) => {
                if CANCEL.load(Ordering::Relaxed) {
                    return;
                }
                if sidecars == 0 {
                    self.start_reread(files, false, ctx);
                } else {
                    self.dialog = Some(Dialog::Reread { files, photos, sidecars, take: false });
                }
            }
            Some(Finished::Done(Ok(o))) => {
                let line = summary(&o);
                if !o.undo.is_empty() {
                    self.manage.push(library_manage::Undo::Reread { label: crate::tr("Metadaten neu einlesen").to_string(), undo: o.undo.clone() });
                }
                if o.changed > 0 {
                    self.reload();
                    self.reload_keywords();
                }
                actions.push(Action::Status(line));
            }
            Some(Finished::Done(Err(e))) => self.dialog = Some(Dialog::Error(e)),
            None => {}
        }
        if RUNNING.load(Ordering::Relaxed) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// The question, drawn by `dialogs`; returns whether the dialog stays open.
    pub(super) fn reread_dialog(&mut self, ctx: &egui::Context, d: Dialog) -> Option<Dialog> {
        let Dialog::Reread { files, photos, sidecars, mut take } = d else { return Some(d) };
        let title = crate::trf!("Metadaten neu einlesen · {a}", a = crate::dialog::count(photos, crate::tr("Foto"), crate::tr("Fotos")));
        let answer = crate::dialog::show(
            ctx,
            "metadaten-neu-einlesen",
            &title,
            crate::dialog::WIDTH,
            |ui| {
                crate::dialog::text(ui, crate::tr("Liest Datum, Kamera, Objektiv, Belichtung, Ort, Maße und Ausrichtung neu aus den Dateien."));
                crate::dialog::hint(ui, &crate::trf!("Neben {a} liegt eine XMP-Begleitdatei.", a = crate::dialog::count(sidecars, crate::tr("Foto"), crate::tr("Fotos"))));
                crate::dialog::options(ui, |ui| {
                    crate::dialog::option(ui, &mut take, crate::tr("Bewertung, Stichwörter, Etikett aus XMP"), crate::tr("Ersetzt die Werte der Bibliothek; Entwicklung bleibt."), None);
                });
                crate::dialog::hint(ui, &crate::trf!("Dateien bleiben unverändert; {k} macht es rückgängig.", k = crate::shortcuts::display("undo")));
            },
            crate::dialog::Buttons::confirm(crate::tr("Einlesen"), true),
        );
        match answer {
            crate::dialog::Answer::Primary => {
                self.start_reread(files, take, ctx);
                None
            }
            crate::dialog::Answer::Cancel => None,
            crate::dialog::Answer::Open => Some(Dialog::Reread { files, photos, sidecars, take }),
        }
    }
}
