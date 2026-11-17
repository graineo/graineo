//! Help → Create Performance Report …: the user picks a folder or up to ten photos, graineo takes one photo per camera
//! family (at most eight) and measures every stage of opening and developing them (`perf_measure`), then writes an
//! English text file and a JSON file into `perf-reports/` of the data folder (`perf_text`). Nothing is sent; the user
//! sends the files by hand. The measurement runs in a background thread and can be cancelled.
//!
//! Test hooks (own data folder only): `GRAINEO_DIALOG=perf-report` opens the window; `GRAINEO_PERF_REPORT=<folder>`
//! measures the photos of that folder without asking, writes the report and quits the app a few seconds later.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::dialog;
use crate::lr::{self, Tip};
use crate::perf_measure::{Progress, Selection};

static OPEN: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default)]
enum Phase {
    #[default]
    Idle,
    /// reading the chosen folder or files
    Choosing,
    Running,
    Done { text: PathBuf },
    Cancelled,
    Failed(String),
}

#[derive(Default)]
struct State {
    phase: Phase,
    selection: Option<Selection>,
    /// what was picked (for the anonymisation of the report)
    folder: Option<PathBuf>,
    picked: Vec<PathBuf>,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    copied: f64,
    /// test hook: quit the app once the report is written
    quit_after: bool,
    quit_at: Option<f64>,
}

fn state() -> &'static Mutex<State> {
    static S: std::sync::OnceLock<Mutex<State>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(State { copied: f64::NEG_INFINITY, ..Default::default() }))
}

const PICK_FOLDER: &str = "perf-report-folder";
const PICK_FILES: &str = "perf-report-files";

/// Open the window (Help menu, settings).
pub fn open() {
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
    if matches!(st.phase, Phase::Done { .. } | Phase::Cancelled | Phase::Failed(_)) {
        st.phase = Phase::Idle;
    }
}

pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Folder of the reports.
pub fn reports_dir() -> PathBuf {
    library::data_dir().join("perf-reports")
}

fn test_hooks(ctx: &egui::Context) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    if let Some(dir) = crate::env::var_os("GRAINEO_PERF_REPORT").map(PathBuf::from) {
        open();
        {
            let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
            st.quit_after = true;
        }
        choose(ctx, Some(dir), Vec::new(), true);
    } else if crate::env::var("GRAINEO_DIALOG").as_deref() == Ok("perf-report") {
        open();
    }
}

/// Read the chosen folder or files in the background and pick one photo per family; `start`: measure right away.
fn choose(ctx: &egui::Context, folder: Option<PathBuf>, files: Vec<PathBuf>, start: bool) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
        st.phase = Phase::Choosing;
        st.selection = None;
        st.folder = folder.clone();
        st.picked = files.clone();
        st.cancel = cancel.clone();
    }
    let display = display(ctx);
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("perf-choose".into())
        .spawn(move || {
            let sel = match &folder {
                Some(d) => crate::perf_measure::choose(crate::perf_measure::scan_folder(d, &cancel), &cancel),
                None => crate::perf_measure::choose_files(files, &cancel),
            };
            let go = start && !sel.photos.is_empty();
            {
                let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
                st.selection = Some(sel);
                st.phase = Phase::Idle;
            }
            if go {
                begin(&ctx, display);
            } else if start {
                crate::log("performance report: no photos in the folder");
                let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
                st.phase = Phase::Failed(crate::tr("Keine Fotos gefunden.").to_string());
            }
            ctx.request_repaint();
        })
        .ok();
}

fn display(ctx: &egui::Context) -> crate::perf_measure::Display {
    let scale = ctx.pixels_per_point();
    let monitor = ctx.input(|i| i.viewport().monitor_size).map(|m| [m.x, m.y]);
    let r = ctx.content_rect();
    crate::perf_measure::Display { scale, monitor, window: [r.width(), r.height()] }
}

/// Start the measurement in a background thread.
fn begin(ctx: &egui::Context, display: crate::perf_measure::Display) {
    let (sel, progress, cancel, folder, picked) = {
        let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
        let Some(sel) = st.selection.clone().filter(|s| !s.photos.is_empty()) else { return };
        st.phase = Phase::Running;
        st.cancel = Arc::new(AtomicBool::new(false));
        st.progress = Arc::new(Mutex::new(Progress { photos: sel.photos.len(), ..Default::default() }));
        (sel, st.progress.clone(), st.cancel.clone(), st.folder.clone(), st.picked.clone())
    };
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("perf-report".into())
        .spawn(move || {
            crate::log(&format!("performance report: {} photos", sel.photos.len()));
            // a panic in the measurement ends the report, not the app
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::perf_measure::run(&sel, display, &progress, &cancel)));
            let phase = match result {
                Err(_) => Phase::Failed(crate::tr("Die Messung ist mit einem Fehler abgebrochen (siehe Protokoll).").to_string()),
                Ok(r) if r.cancelled => Phase::Cancelled,
                Ok(r) => {
                    let photos: Vec<PathBuf> = sel.photos.iter().map(|(_, p)| p.clone()).chain(picked).collect();
                    let privacy = crate::perf_text::Privacy::new(&photos, folder.as_deref());
                    match write(&r, &privacy) {
                        Ok(text) => {
                            crate::log(&format!("performance report written ({:.0} s)", r.seconds));
                            Phase::Done { text }
                        }
                        Err(e) => Phase::Failed(e),
                    }
                }
            };
            state().lock().unwrap_or_else(|e| e.into_inner()).phase = phase;
            ctx.request_repaint();
        })
        .ok();
}

/// Write the text and the JSON file; returns the path of the text file.
fn write(r: &crate::perf_measure::Report, privacy: &crate::perf_text::Privacy) -> Result<PathBuf, String> {
    let dir = reports_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let base = format!("graineo-performance-{}", r.file_stamp);
    let text = dir.join(format!("{base}.txt"));
    std::fs::write(&text, crate::perf_text::text(r, privacy)).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{base}.json")), crate::perf_text::json(r, privacy)).map_err(|e| e.to_string())?;
    Ok(text)
}

/// German name of a measured stage for the progress line (ids from `perf_measure`).
fn stage_name(id: &str) -> &'static str {
    match id {
        "read" => crate::tr("Datei lesen"),
        "x3f_parse" | "decode" | "calibration" | "defects" | "open" => crate::tr("Öffnen"),
        "first_linear" | "first_develop" | "first_detail" | "first_render" => crate::tr("Erstes Bild"),
        "base_gpu" => crate::tr("Exakte Basis (Grafikkarte)"),
        "base_cpu" => crate::tr("Exakte Basis (Prozessor)"),
        "fitted" => crate::tr("Eingepasste Ansicht"),
        "full" => crate::tr("Volle Auflösung"),
        "full_gpu" => crate::tr("Volle Auflösung (Grafikkarte)"),
        "full_cpu" => crate::tr("Volle Auflösung (Prozessor)"),
        "export_jpeg" => crate::tr("JPEG-Export"),
        _ => crate::tr("Systemangaben"),
    }
}

/// Every frame: the window, while open; picks up the file dialogs.
pub fn frame(ctx: &egui::Context) {
    test_hooks(ctx);
    if let Some(dir) = crate::file_dialog::fetch_one(PICK_FOLDER) {
        choose(ctx, Some(dir), Vec::new(), false);
    }
    if let Some(files) = crate::file_dialog::fetch(PICK_FILES) {
        choose(ctx, None, files, false);
    }
    // test hook: quit a few seconds after the report is written
    {
        let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
        if st.quit_after && matches!(st.phase, Phase::Done { .. } | Phase::Failed(_) | Phase::Cancelled) {
            let now = ctx.input(|i| i.time);
            let at = *st.quit_at.get_or_insert(now + 8.0);
            if now >= at {
                st.quit_after = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
    }
    if !is_open() {
        return;
    }
    let (phase, selection, progress, copied) = {
        let st = state().lock().unwrap_or_else(|e| e.into_inner());
        let p = st.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (st.phase.clone(), st.selection.clone(), p, st.copied)
    };
    let now = ctx.input(|i| i.time);
    let running = matches!(phase, Phase::Running);
    let ready = matches!(phase, Phase::Idle) && selection.as_ref().is_some_and(|s| !s.photos.is_empty());
    let mut pick_folder = false;
    let mut pick_files = false;
    let mut copy: Option<PathBuf> = None;
    let buttons = match &phase {
        Phase::Running => dialog::Buttons { cancel: crate::tr("Messung abbrechen"), primary: None },
        Phase::Done { .. } => dialog::Buttons::close(),
        _ => dialog::Buttons::confirm(crate::tr("Starten"), ready),
    };
    let answer = dialog::show(
        ctx,
        "perf-report",
        crate::tr("Leistungsbericht erstellen"),
        560.0,
        |ui| {
            dialog::text(ui, crate::tr("graineo misst, wie schnell dieser Rechner Fotos öffnet und entwickelt: jede Stufe dreimal, mit Grafikkarte und Prozessor. Das dauert einige Minuten, der Lüfter kann dabei anlaufen."));
            dialog::hint(ui, crate::tr("Der Bericht ist eine Textdatei auf diesem Rechner, ohne Pfade, Datei-, Benutzer- und Rechnernamen. graineo sendet nichts – schick die Datei bei Bedarf selbst."));
            ui.add_space(6.0);
            match &phase {
                Phase::Running => {
                    let what = stage_name(&progress.stage);
                    let line = crate::trf!("Foto {a} von {b} · {c} · Lauf {d} von {e} · {f}", a = progress.photo.max(1), b = progress.photos, c = progress.family, d = progress.run.max(1), e = crate::perf_measure::RUNS, f = what);
                    dialog::progress(ui, Some(progress.fraction), &line);
                    dialog::hint(ui, crate::tr("Bitte währenddessen nicht mit graineo arbeiten; andere Programme verfälschen die Zeiten."));
                }
                Phase::Done { text } => {
                    dialog::outcome(ui, dialog::Verdict::Green, crate::tr("Bericht gespeichert. Schick die Textdatei (und die JSON-Datei daneben) an die Entwickler, z. B. per E-Mail oder als Anhang eines GitHub-Issues."), &[]);
                    let w = ui.available_width();
                    crate::prefs::path_text(ui, &text.to_string_lossy(), w);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let show = if cfg!(windows) { crate::tr("Im Explorer zeigen") } else { crate::tr("Im Finder zeigen") };
                        if ui.add(egui::Button::new(show).min_size(egui::vec2(120.0, 24.0)).corner_radius(0.0)).clicked() {
                            crate::system::show(text);
                        }
                        if ui.add(egui::Button::new(crate::tr("Pfad kopieren")).min_size(egui::vec2(110.0, 24.0)).corner_radius(0.0)).tip(ui, "Pfad der Textdatei in die Zwischenablage").clicked() {
                            copy = Some(text.clone());
                        }
                        if now - copied < 2.0 {
                            ui.label(egui::RichText::new(crate::tr("Kopiert")).small().color(lr::TEXT_DIM));
                        }
                    });
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let idle = !matches!(phase, Phase::Choosing);
                        if ui.add_enabled(idle, egui::Button::new(crate::tr("Ordner wählen …")).min_size(egui::vec2(130.0, 24.0)).corner_radius(0.0)).tip(ui, "Je Kamerafamilie nimmt graineo ein Foto aus dem Ordner (höchstens 8)").clicked() {
                            pick_folder = true;
                        }
                        if ui.add_enabled(idle, egui::Button::new(crate::tr("Dateien wählen …")).min_size(egui::vec2(130.0, 24.0)).corner_radius(0.0)).tip(ui, "Bis zu 10 Fotos; je Kamerafamilie zählt eines").clicked() {
                            pick_files = true;
                        }
                    });
                    ui.add_space(2.0);
                    match (&phase, &selection) {
                        (Phase::Choosing, _) => dialog::pending(ui, crate::tr("Fotos werden gesucht …")),
                        (_, Some(sel)) if sel.photos.is_empty() => dialog::warning(ui, crate::tr("Keine Fotos gefunden (X3F, RAF, CR2/CR3, ARW, NEF, DNG …, JPEG, HEIC).")),
                        (_, Some(sel)) => {
                            let fams: Vec<&str> = sel.photos.iter().map(|(f, _)| f.label()).collect();
                            dialog::chip(ui, &crate::trf!("{a} · {b}", a = dialog::count(sel.photos.len(), crate::tr("Foto"), crate::tr("Fotos")), b = fams.join(", ")));
                            let mut note = crate::trf!("Je Kamerafamilie ein Foto aus {a} gefundenen.", a = dialog::number(sel.found));
                            if sel.skipped > 0 {
                                note.push(' ');
                                note.push_str(&crate::trf!("{a} weitere Dateien nicht berücksichtigt (höchstens 10).", a = sel.skipped));
                            }
                            dialog::hint(ui, &note);
                        }
                        _ => dialog::hint(ui, crate::tr("Wähle einen Ordner oder bis zu 10 Fotos: am besten je eines von jeder Kamera, die du nutzt (Merrill, Quattro, ältere Foveon, Fuji X-Trans, andere RAWs, JPEG/HEIC).")),
                    }
                    match &phase {
                        Phase::Cancelled => dialog::warning(ui, crate::tr("Messung abgebrochen, kein Bericht geschrieben.")),
                        Phase::Failed(e) => dialog::warning(ui, e),
                        _ => {}
                    }
                }
            }
        },
        buttons,
    );
    if running {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    if pick_folder {
        crate::file_dialog::folder(ctx, PICK_FOLDER, |d| d.set_title(crate::tr("Ordner mit Fotos für den Leistungsbericht")));
    }
    if pick_files {
        crate::file_dialog::files(ctx, PICK_FILES, |d| {
            d.set_title(crate::tr("Fotos für den Leistungsbericht (bis zu 10)")).add_filter(crate::tr("Fotos"), &["x3f", "X3F", "raf", "RAF", "cr2", "CR2", "cr3", "CR3", "arw", "ARW", "nef", "NEF", "nrw", "NRW", "dng", "DNG", "orf", "ORF", "rw2", "RW2", "pef", "PEF", "jpg", "JPG", "jpeg", "JPEG", "heic", "HEIC", "tif", "TIF", "tiff", "TIFF", "png", "PNG"])
        });
    }
    let mut st = state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = copy {
        ctx.copy_text(p.to_string_lossy().into_owned());
        st.copied = now;
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
    }
    match answer {
        dialog::Answer::Primary if ready => {
            drop(st);
            begin(ctx, display(ctx));
        }
        dialog::Answer::Cancel if running => {
            // stop after the running stage; the window stays and says so
            st.cancel.store(true, Ordering::Relaxed);
        }
        dialog::Answer::Cancel => {
            st.cancel.store(true, Ordering::Relaxed);
            OPEN.store(false, Ordering::SeqCst);
        }
        _ => {}
    }
}
