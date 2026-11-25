//! Assistant "Objektiv hinzufügen" in four steps: name the lens (manual lenses and adapters have no name in the
//! file), choose what to measure (with a printable grid template, `lens_grid_pdf`), choose the
//! test shots (grid for distortion and colour fringes, flat fields for vignetting and colour cast; the second flat
//! field of a pair with the camera turned by 180°), measure in the background and save the result as an own profile
//! (`lensprof::store`), optionally assigning it at once.
//!
//! The measurement itself is `raw::measure_run::measure_with` (all formats, X3F through `rawdev::lens_shots`).

use crate::menu::i18n::DeComma as _;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use eframe::egui;
use raw::measure_run::{self as run, MeasureHints, Outcome, Role};

use crate::dialog::{self, Answer, Buttons, Verdict};
use crate::lens_assign::{self, Identity, Request};
use crate::lr;

const ID: &str = "objektiv-assistent";
const PICK_FOLDER: &str = "objektiv-assistent-ordner";
const PICK_GRID: &str = "objektiv-assistent-gitter";
const PICK_FLAT: &str = "objektiv-assistent-weissfeld";

/// What the user wants to measure (step 2); decides which shots step 3 asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Goal {
    /// distortion and colour fringes: photo of a grid
    Geometry,
    /// vignetting and colour cast: photo of an even white or grey area per aperture
    Flat,
    Both,
}

impl Goal {
    fn grid(self) -> bool {
        self != Goal::Flat
    }
    fn flat(self) -> bool {
        self != Goal::Geometry
    }
}

enum Message {
    Progress(usize, usize, String),
    Done(Result<Outcome, String>),
}

enum Phase {
    Lens,
    What,
    Shots,
    Running { done: usize, total: usize, name: String },
    Outcome(Box<Outcome>),
    Error(String),
}

/// What the user says about a file; `Auto` lets the measurement decide by content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Auto,
    Grid,
    Flat,
    /// second flat field of a pair (camera turned by 180°); belongs to the flat field above it
    FlatRotated,
}

struct Row {
    path: PathBuf,
    kind: Kind,
    /// aperture typed in when the file carries none ("f/2.8", "2,8")
    aperture: String,
}

struct State {
    phase: Phase,
    name: String,
    focal: String,
    rows: Vec<Row>,
    receiver: Option<Receiver<Message>>,
    abort: Arc<AtomicBool>,
    preview: Option<(egui::TextureHandle, egui::TextureHandle)>,
    /// profile id and version after saving, or the error
    saved: Option<Result<lensprof::ProfileRef, String>>,
    assign_rule: bool,
    assign_photo: bool,
    show_shots: bool,
    goal: Goal,
    paper: crate::lens_grid_pdf::Paper,
    orientation: crate::lens_grid_pdf::Orientation,
    /// result of "Vorlage drucken (PDF)": error text, if any
    pdf_error: Option<String>,
}

thread_local! {
    static Z: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Open the assistant at the first step.
pub fn open() {
    Z.with(|z| {
        *z.borrow_mut() = Some(State {
            phase: Phase::Lens,
            name: String::new(),
            focal: String::new(),
            rows: Vec::new(),
            receiver: None,
            abort: Arc::new(AtomicBool::new(false)),
            preview: None,
            saved: None,
            assign_rule: true,
            assign_photo: true,
            show_shots: false,
            goal: Goal::Both,
            paper: crate::lens_grid_pdf::Paper::A4,
            orientation: crate::lens_grid_pdf::Orientation::Portrait,
            pdf_error: None,
        })
    });
}

/// Test hook (`GRAINEO_DIALOG=lens-wizard`): with `GRAINEO_LENS_WIZARD_FOLDER` the shots of that folder are listed;
/// `GRAINEO_LENS_WIZARD_NAME` names the lens, `GRAINEO_LENS_WIZARD_STEP=2|3` jumps on (3 measures right away),
/// `GRAINEO_LENS_WIZARD_GOAL=grid|flatfield` chooses the goal (otherwise both).
pub fn start_test(ctx: &egui::Context) {
    open();
    let step = crate::env::var("GRAINEO_LENS_WIZARD_STEP").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(1);
    Z.with(|z| {
        let mut g = z.borrow_mut();
        let Some(s) = g.as_mut() else { return };
        if let Ok(n) = crate::env::var("GRAINEO_LENS_WIZARD_NAME") {
            s.name = n;
        }
        if let Ok(o) = crate::env::var("GRAINEO_LENS_WIZARD_FOLDER") {
            add_paths(s, run::files_in_folder(std::path::Path::new(&o)));
        }
        if step >= 2 {
            s.phase = Phase::What;
        }
        if let Ok(g) = crate::env::var("GRAINEO_LENS_WIZARD_GOAL") {
            s.goal = match g.as_str() {
                "grid" => Goal::Geometry,
                "flatfield" => Goal::Flat,
                _ => Goal::Both,
            };
            s.phase = Phase::Shots;
        }
        if step >= 3 && !s.rows.is_empty() {
            start(s, ctx);
        }
    });
}

/// Photos or folders dropped on the window while step 3 ("Aufnahmen") is open: they are added (folders with their RAW
/// files, the kind recognised by content). Called from the raw input hook, before the import takes a drop; true if
/// the assistant took them.
pub fn take_dropped(paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    Z.with(|z| {
        let mut g = z.borrow_mut();
        let Some(s) = g.as_mut().filter(|s| matches!(s.phase, Phase::Shots)) else { return false };
        add_paths(s, dropped_shots(paths));
        true
    })
}

/// RAW files among dropped paths; a folder stands for its RAW files (not recursive, like "Ordner …").
fn dropped_shots(paths: &[PathBuf]) -> Vec<PathBuf> {
    use crate::image_kind::{from_extension, ImageKind};
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            out.extend(run::files_in_folder(p));
        } else if matches!(from_extension(p.extension().and_then(|e| e.to_str()).unwrap_or("")), ImageKind::Raw | ImageKind::Foveon) {
            out.push(p.clone());
        }
    }
    out
}

fn add_paths(s: &mut State, paths: Vec<PathBuf>) {
    add_paths_as(s, paths, Kind::Auto);
}

/// Add shots with a known kind (buttons "Gitteraufnahmen …" / "Weißfelder …").
fn add_paths_as(s: &mut State, paths: Vec<PathBuf>, kind: Kind) {
    for p in paths {
        if !s.rows.iter().any(|r| r.path == p) {
            s.rows.push(Row { path: p, kind, aperture: String::new() });
        }
    }
}

fn parse_number(t: &str) -> Option<f32> {
    let t = t.trim().trim_start_matches("f/").trim_start_matches('f').trim_start_matches('/').replace(',', ".");
    t.parse::<f32>().ok().filter(|v| v.is_finite() && *v > 0.0)
}

/// Pairs of flat fields: each `FlatRotated` belongs to the closest `Flat` above it that has no partner yet.
fn pairs(rows: &[Row]) -> (Vec<(PathBuf, PathBuf)>, Vec<PathBuf>) {
    let mut open: Vec<&Row> = Vec::new();
    let mut pairs = Vec::new();
    let mut orphans = Vec::new();
    for r in rows {
        match r.kind {
            Kind::Flat => open.push(r),
            Kind::FlatRotated => match open.pop() {
                Some(a) => pairs.push((a.path.clone(), r.path.clone())),
                None => orphans.push(r.path.clone()),
            },
            _ => {}
        }
    }
    (pairs, orphans)
}

/// Why "Messen" cannot start yet.
fn blocked(s: &State) -> Option<&'static str> {
    if s.rows.is_empty() {
        return Some("Noch keine Aufnahmen gewählt");
    }
    // what the chosen measurement needs ("automatisch" may be either)
    if s.goal.grid() && !s.rows.iter().any(|r| matches!(r.kind, Kind::Grid | Kind::Auto)) {
        return Some("Für Verzeichnung und Farbsäume fehlt eine Gitteraufnahme");
    }
    if s.goal.flat() && !s.rows.iter().any(|r| matches!(r.kind, Kind::Flat | Kind::Auto)) {
        return Some("Für Vignettierung und Farbstich fehlt ein Weißfeld");
    }
    if !pairs(&s.rows).1.is_empty() {
        return Some("Ein Weißfeld „180° gedreht“ braucht ein Weißfeld darüber");
    }
    if s.rows.iter().any(|r| !r.aperture.trim().is_empty() && parse_number(&r.aperture).is_none()) {
        return Some("Eine Blende ist keine Zahl");
    }
    None
}

fn hints(s: &State) -> MeasureHints {
    let (pairs, _) = pairs(&s.rows);
    let mut h = MeasureHints { lens_name: Some(s.name.trim().to_string()), focal_length: parse_number(&s.focal), pairs, ..Default::default() };
    let mut apertures: HashMap<PathBuf, f32> = HashMap::new();
    for r in &s.rows {
        if let Some(n) = parse_number(&r.aperture) {
            apertures.insert(r.path.clone(), n);
        }
        match r.kind {
            Kind::Grid => {
                h.roles.insert(r.path.clone(), Role::Grid);
            }
            Kind::Flat | Kind::FlatRotated => {
                h.roles.insert(r.path.clone(), Role::FlatField);
            }
            Kind::Auto => {}
        }
    }
    h.apertures = apertures;
    h
}

fn start(s: &mut State, ctx: &egui::Context) {
    let (tx, rx) = channel();
    let files: Vec<PathBuf> = s.rows.iter().map(|r| r.path.clone()).collect();
    let hints = hints(s);
    s.abort = Arc::new(AtomicBool::new(false));
    let abort = s.abort.clone();
    s.receiver = Some(rx);
    s.phase = Phase::Running { done: 0, total: files.len(), name: String::new() };
    s.preview = None;
    s.saved = None;
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("objektiv-messen".into())
        .spawn(move || {
            // the user waits for the result, but the Mac should stay quiet: not the fastest class
            library::platform::set_priority(library::platform::Priority::Utility);
            let tx2 = tx.clone();
            let ctx2 = ctx.clone();
            let r = run::measure_with(
                &files,
                &rawdev::lens_shots::AnySource,
                &hints,
                &move |i, n, name| {
                    let _ = tx2.send(Message::Progress(i, n, name.to_string()));
                    ctx2.request_repaint();
                },
                &abort,
            );
            let _ = tx.send(Message::Done(r));
            ctx.request_repaint();
        })
        .ok();
}

fn num1(v: f64) -> String {
    format!("{v:.1}").de_comma()
}

fn num2(v: f64) -> String {
    format!("{v:.2}").de_comma()
}

fn percent(v: f32) -> String {
    format!("{:+.1} %", (v - 1.0) * 100.0).de_comma()
}

fn image(ctx: &egui::Context, name: &str, b: (usize, usize, &[u8])) -> egui::TextureHandle {
    ctx.load_texture(name, egui::ColorImage::from_rgb([b.0, b.1], b.2), egui::TextureOptions::LINEAR)
}

fn kind_text(k: Kind) -> &'static str {
    match k {
        Kind::Auto => "automatisch",
        Kind::Grid => "Gitter",
        Kind::Flat => "Weißfeld",
        Kind::FlatRotated => "Weißfeld, Kamera 180° gedreht",
    }
}

/// Small sketch of what to photograph: checkerboard (grid) or an even, softly lit area (flat field).
fn sketch(ui: &mut egui::Ui, grid: bool) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(54.0, 40.0), egui::Sense::hover());
    let p = ui.painter();
    if grid {
        p.rect_filled(r, 0.0, crate::lr::gray(0xd8));
        let q = r.width() / 9.0;
        for j in 0..((r.height() / q).floor() as usize) {
            for i in 0..9 {
                if (i + j) % 2 == 0 {
                    p.rect_filled(egui::Rect::from_min_size(r.min + egui::vec2(i as f32 * q, j as f32 * q), egui::vec2(q, q)), 0.0, crate::lr::gray(0x20));
                }
            }
        }
    } else {
        // light falling off to the corners: what the flat field measures
        for k in 0..6 {
            let f = 1.0 - k as f32 / 6.0;
            p.rect_filled(r.shrink(k as f32 * 3.0), 0.0, crate::lr::gray((0xa0 as f32 + 0x50 as f32 * (1.0 - f)) as u8));
        }
    }
    p.rect_stroke(r, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0x50)), egui::StrokeKind::Inside);
}

/// One selectable card of step 2: sketch(es), title, short "how to photograph it". Fixed height.
fn goal_card(ui: &mut egui::Ui, goal: &mut Goal, value: Goal, title: &str, how: &str) {
    let on = *goal == value;
    let frame = egui::Frame::new()
        .fill(if on { crate::lr::gray(0x26) } else { crate::lr::gray(0x19) })
        .stroke(egui::Stroke::new(1.0, if on { crate::lr::gray(0x8a) } else { crate::lr::gray(0x30) }))
        .inner_margin(egui::Margin::symmetric(10, 8));
    let r = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(52.0);
            ui.horizontal(|ui| {
                // choice mark (the whole card is the click area)
                let (m, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_stroke(m.center(), 6.0, egui::Stroke::new(1.0, crate::lr::gray(if on { 0xd0 } else { 0x70 })));
                if on {
                    ui.painter().circle_filled(m.center(), 3.0, lr::TEXT());
                }
                if value.grid() {
                    sketch(ui, true);
                }
                if value.flat() {
                    sketch(ui, false);
                }
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(crate::tr(title)).color(lr::TEXT()).strong());
                    ui.add(egui::Label::new(egui::RichText::new(crate::tr(how)).small().color(lr::TEXT_DIM())).wrap());
                });
            });
        })
        .response;
    if ui.interact(r.rect, ui.id().with(("ziel", title)), egui::Sense::click()).clicked() {
        *goal = value;
    }
}

/// Call every frame in the main window.
pub fn ui(ctx: &egui::Context) {
    Z.with(|zz| {
        let mut guard = zz.borrow_mut();
        let Some(s) = guard.as_mut() else { return };
        if let Some(p) = crate::file_dialog::fetch_one(PICK_FOLDER) {
            add_paths(s, run::files_in_folder(&p));
        }
        if let Some(p) = crate::file_dialog::fetch(PICK_GRID) {
            add_paths_as(s, p, Kind::Grid);
        }
        if let Some(p) = crate::file_dialog::fetch(PICK_FLAT) {
            add_paths_as(s, p, Kind::Flat);
        }
        let mut finished: Option<Result<Outcome, String>> = None;
        if let Some(rx) = &s.receiver {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Message::Progress(i, n, name) => s.phase = Phase::Running { done: i, total: n, name },
                    Message::Done(r) => finished = Some(r),
                }
            }
        }
        if let Some(r) = finished {
            s.receiver = None;
            match r {
                Ok(e) => {
                    s.preview = e.geo.iter().find_map(|g| g.preview.as_ref()).map(|(w, h, a, b)| (image(ctx, "ov-vorher", (*w, *h, a)), image(ctx, "ov-nachher", (*w, *h, b))));
                    s.phase = Phase::Outcome(Box::new(e));
                }
                Err(m) if m == "abgebrochen" => s.phase = Phase::Shots,
                Err(m) => s.phase = Phase::Error(m),
            }
        }
        if matches!(s.phase, Phase::Running { .. }) {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let name_ok = !s.name.trim().is_empty();
        let focal_ok = s.focal.trim().is_empty() || parse_number(&s.focal).is_some();
        let block = blocked(s);
        let has_profile = matches!(&s.phase, Phase::Outcome(e) if e.profile.is_some()) && s.saved.is_none();
        let buttons = match &s.phase {
            Phase::Lens => Buttons::confirm("Weiter", name_ok && focal_ok),
            Phase::What => Buttons::confirm("Weiter", true),
            Phase::Shots => Buttons::confirm("Messen", block.is_none()),
            Phase::Running { .. } => Buttons { cancel: "Abbrechen", primary: None },
            Phase::Outcome(_) => Buttons { cancel: "Schließen", primary: Some(("Als Profil speichern", dialog::Style::Normal, has_profile)) },
            Phase::Error(_) => Buttons { cancel: "Schließen", primary: Some(("Zurück", dialog::Style::Normal, true)) },
        };
        let mut pick_grid = false;
        let mut pick_flat = false;
        let mut pick_folder = false;
        let mut back = false;
        let mut print = false;
        let title = match &s.phase {
            Phase::Lens => crate::tr("Objektiv hinzufügen · 1 / 4 Objektiv"),
            Phase::What => crate::tr("Objektiv hinzufügen · 2 / 4 Was messen?"),
            Phase::Shots => crate::tr("Objektiv hinzufügen · 3 / 4 Aufnahmen"),
            _ => crate::tr("Objektiv hinzufügen · 4 / 4 Messen"),
        };
        let goal = s.goal;
        let answer = dialog::show(
            ctx,
            ID,
            title,
            660.0,
            |ui| match &mut s.phase {
                Phase::Lens => {
                    dialog::hint(ui, "Auch für manuelle Objektive und Adapter ohne Angabe in der Datei.");
                    ui.add_space(4.0);
                    dialog::input(ui, "Name des Objektivs (zum Beispiel „Helios 44-2 58mm“)", &mut s.name);
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(crate::tr("Brennweite in mm (nur nötig, wenn die Datei keine nennt)")).small().color(lr::TEXT_DIM()));
                    lr::edit_field(ui, egui::TextEdit::singleline(&mut s.focal).desired_width(120.0));
                    if !focal_ok {
                        dialog::hint(ui, "Die Brennweite ist keine Zahl.");
                    }
                }
                Phase::What => {
                    ui.horizontal(|ui| {
                        if ui.button(crate::tr("Zurück")).clicked() {
                            back = true;
                        }
                        ui.label(egui::RichText::new(s.name.trim().to_string()).small().color(lr::TEXT_DIM()));
                        crate::help::info(ui, "objektiv-aufnahmen", "Aufnahmen für ein neues Objektiv", &crate::help::LENS_SHOTS);
                    });
                    ui.add_space(4.0);
                    goal_card(ui, &mut s.goal, Goal::Geometry, "Verzeichnung und Farbsäume", "Gedrucktes Gitter, das das ganze Bild bis in die Ecken füllt; Kamera parallel, Stativ, f/8.");
                    ui.add_space(4.0);
                    goal_card(ui, &mut s.goal, Goal::Flat, "Vignettierung und Farbstich", "Gleichmäßig helle Fläche, unscharf, nichts ausgebrannt; je Blende eine Aufnahme, dazu dieselbe mit 180° gedrehter Kamera.");
                    ui.add_space(4.0);
                    goal_card(ui, &mut s.goal, Goal::Both, "Beides", "Gitter- und Weißfeldaufnahmen zusammen; ergibt ein vollständiges Profil.");
                    ui.add_space(6.0);
                    // grid template to print (only when a grid is needed; the row keeps its height)
                    ui.add_enabled_ui(s.goal.grid(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(crate::tr("Gitter-Vorlage")).small().color(lr::TEXT_DIM()));
                            lr::segmented(ui, &mut s.paper, &[(crate::lens_grid_pdf::Paper::A4, "A4"), (crate::lens_grid_pdf::Paper::A3, "A3")]);
                            lr::segmented(ui, &mut s.orientation, &[(crate::lens_grid_pdf::Orientation::Portrait, crate::tr("Hoch")), (crate::lens_grid_pdf::Orientation::Landscape, crate::tr("Quer"))]);
                            let b = ui.button(crate::tr("Vorlage drucken (PDF)"));
                            lr::hover_tip(ui, &b, crate::tr("Öffnet ein Schachbrett zum Drucken; in Originalgröße (100 %) drucken und die 100-mm-Strecke nachmessen"));
                            if b.clicked() {
                                print = true;
                            }
                        });
                    });
                    match &s.pdf_error {
                        Some(e) => dialog::warning(ui, &crate::trf!("Die Vorlage ließ sich nicht anlegen: {e}", e = e)),
                        None => dialog::hint(ui, "Immer: RAW, ISO 100, Bildstabilisator und Objektivkorrektur der Kamera aus."),
                    }
                }
                Phase::Shots => {
                    ui.horizontal(|ui| {
                        if ui.button(crate::tr("Zurück")).clicked() {
                            back = true;
                        }
                        ui.label(egui::RichText::new(s.name.trim().to_string()).small().color(lr::TEXT_DIM()));
                    });
                    ui.horizontal(|ui| {
                        if goal.grid() && ui.button(crate::tr("Gitteraufnahmen …")).clicked() {
                            pick_grid = true;
                        }
                        if goal.flat() && ui.button(crate::tr("Weißfelder …")).clicked() {
                            pick_flat = true;
                        }
                        let f = ui.button(crate::tr("Ordner …"));
                        lr::hover_tip(ui, &f, crate::tr("Alle Aufnahmen eines Ordners; Gitter und Weißfeld werden am Inhalt erkannt"));
                        if f.clicked() {
                            pick_folder = true;
                        }
                        crate::help::info(ui, "objektiv-weissfeld", "Aufnahmen für ein neues Objektiv", &crate::help::LENS_SHOTS);
                    });
                    if goal.flat() {
                        dialog::hint(ui, "Weißfeld-Paar: dieselbe Fläche noch einmal mit um 180° gedrehter Kamera; diese Aufnahme direkt unter ihr Weißfeld stellen und „Weißfeld, Kamera 180° gedreht“ wählen.");
                    }
                    ui.add_space(2.0);
                    let mut remove: Option<usize> = None;
                    // drop area: photos or folders dragged onto the window land here (`take_dropped`)
                    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
                    let area = egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, if hovering { lr::TEXT() } else { crate::lr::gray(0x30) }))
                        .fill(if hovering { crate::lr::gray(0x24) } else { egui::Color32::TRANSPARENT })
                        .inner_margin(egui::Margin::same(4));
                    area.show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(250.0).min_scrolled_height(250.0).auto_shrink([false, false]).show(ui, |ui| {
                        if s.rows.is_empty() || hovering {
                            ui.add_space(if s.rows.is_empty() { 100.0 } else { 0.0 });
                            ui.vertical_centered(|ui| {
                                let t = if hovering { crate::tr("Loslassen: Fotos und Ordner kommen in die Liste") } else { crate::tr("Fotos oder Ordner hierher ziehen oder über die Knöpfe oben hinzufügen") };
                                ui.label(egui::RichText::new(t).small().color(if hovering { lr::TEXT() } else { lr::TEXT_DIM() }));
                            });
                        }
                        for (i, r) in s.rows.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                let name = r.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                                ui.add_sized([180.0, 18.0], egui::Label::new(egui::RichText::new(name).small().color(lr::TEXT())).truncate());
                                egui::ComboBox::from_id_salt(("objektiv-art", i)).width(170.0).selected_text(crate::tr(kind_text(r.kind))).show_ui(ui, |ui| {
                                    for k in [Kind::Auto, Kind::Grid, Kind::Flat, Kind::FlatRotated] {
                                        ui.selectable_value(&mut r.kind, k, crate::tr(kind_text(k)));
                                    }
                                });
                                ui.label(egui::RichText::new("f/").small().color(lr::TEXT_DIM()));
                                lr::edit_field(ui, egui::TextEdit::singleline(&mut r.aperture).desired_width(44.0));
                                if ui.small_button("✕").clicked() {
                                    remove = Some(i);
                                }
                            });
                        }
                    });
                    });
                    if let Some(i) = remove {
                        s.rows.remove(i);
                    }
                    // fixed line: why "Messen" is locked
                    let why = block.map(crate::tr).map(str::to_string).unwrap_or_else(|| crate::trf!("{n} Aufnahmen · Blende nur eintragen, wenn die Datei keine nennt", n = s.rows.len()));
                    ui.label(egui::RichText::new(why).small().color(if block.is_some() { lr::TEXT_DIM() } else { lr::TEXT() }));
                }
                Phase::Running { done, total, name } => {
                    dialog::text(ui, "Die Aufnahmen werden ausgewertet. Das dauert etwa eine Sekunde pro Foto, bei Sigma-X3F etwas länger.");
                    ui.add_space(6.0);
                    dialog::progress(ui, Some(*done as f32 / (*total).max(1) as f32), &crate::trf!("{a} von {b} · {c}", a = (*done + 1).min(*total), b = total, c = name));
                }
                Phase::Error(m) => {
                    dialog::outcome(ui, Verdict::Red, "Es konnte nichts gemessen werden", &[m.clone()]);
                }
                Phase::Outcome(e) => outcome_ui(ui, e, s.preview.as_ref(), &mut s.show_shots, &s.saved, &mut s.assign_rule, &mut s.assign_photo),
            },
            buttons,
        );
        if pick_grid {
            crate::file_dialog::files(ctx, PICK_GRID, |d| d.set_title(crate::tr("Gitteraufnahmen")));
        }
        if pick_flat {
            crate::file_dialog::files(ctx, PICK_FLAT, |d| d.set_title(crate::tr("Weißfeldaufnahmen")));
        }
        if print {
            s.pdf_error = crate::lens_grid_pdf::write_and_open(s.paper, s.orientation).err();
        }
        if pick_folder {
            crate::file_dialog::folder(ctx, PICK_FOLDER, |d| d.set_title(crate::tr("Ordner mit Aufnahmen")));
        }
        if back {
            s.phase = match s.phase {
                Phase::Shots => Phase::What,
                _ => Phase::Lens,
            };
        }
        let mut close = false;
        match (&s.phase, answer) {
            (Phase::Lens, Answer::Primary) => s.phase = Phase::What,
            (Phase::What, Answer::Primary) => s.phase = Phase::Shots,
            (Phase::Shots, Answer::Primary) => start(s, ctx),
            (Phase::Outcome(_), Answer::Primary) => save(s),
            (Phase::Error(_), Answer::Primary) => s.phase = Phase::Shots,
            (Phase::Running { .. }, Answer::Cancel) => s.abort.store(true, Ordering::Relaxed),
            (_, Answer::Cancel) => close = true,
            _ => {}
        }
        if close {
            *guard = None;
        }
    });
}

/// Save the result as an own profile; the wished assignments go to the main window.
fn save(s: &mut State) {
    let Phase::Outcome(e) = &s.phase else { return };
    match run::save(e) {
        Ok(path) => {
            let id = path.file_stem().and_then(|f| f.to_str()).unwrap_or("").to_string();
            let reference = lensprof::store::list().into_iter().find(|p| p.id == id).map(|p| p.reference());
            match reference {
                Some(r) => {
                    let mut ident = Identity::new(e.maker.clone(), e.model.clone(), e.lens.clone());
                    ident.foveon = e.sensor == lensprof::own::SensorKind::Foveon;
                    let manual = ident.lens.is_none();
                    if s.assign_rule && manual || s.assign_photo {
                        lens_assign::request(Request::Apply { profile: r.to_string(), ident, rule: s.assign_rule && manual, photo: s.assign_photo });
                    }
                    s.saved = Some(Ok(r));
                }
                None => s.saved = Some(Err(crate::tr("Das gespeicherte Profil wurde nicht gefunden").to_string())),
            }
        }
        Err(err) => s.saved = Some(Err(err.to_string())),
    }
}

fn verdict_geo(v: &raw::measure::Distortion) -> Verdict {
    if v.rms_after < 0.8 && v.coverage >= 0.8 && v.spread >= 0.7 {
        Verdict::Green
    } else {
        Verdict::Yellow
    }
}

fn verdict_score(score: u8) -> Verdict {
    match lensprof::quality::Level::of(score) {
        lensprof::quality::Level::Good => Verdict::Green,
        lensprof::quality::Level::Usable => Verdict::Yellow,
        lensprof::quality::Level::Uncertain => Verdict::Red,
    }
}

fn part_name(p: lensprof::own::QualityPart) -> &'static str {
    use lensprof::own::QualityPart::*;
    match p {
        Distortion => "Verzeichnung",
        LateralCa => "Farbsäume",
        Vignette => "Vignettierung",
        Cast => "Farbstich",
    }
}

fn channel_names(sensor: lensprof::own::SensorKind) -> [&'static str; 3] {
    if sensor == lensprof::own::SensorKind::Foveon {
        ["obere Schicht", "mittlere Schicht", "untere Schicht"]
    } else {
        ["Rot", "Grün", "Blau"]
    }
}

#[allow(clippy::too_many_arguments)]
fn outcome_ui(
    ui: &mut egui::Ui,
    e: &Outcome,
    preview: Option<&(egui::TextureHandle, egui::TextureHandle)>,
    show_shots: &mut bool,
    saved: &Option<Result<lensprof::ProfileRef, String>>,
    assign_rule: &mut bool,
    assign_photo: &mut bool,
) {
    let name = e.name.clone().unwrap_or_else(|| format!("{} {} · {}", e.maker, e.model, e.lens.as_deref().unwrap_or("?")));
    dialog::chip(ui, &format!("{name} · {} {}", e.maker, e.model));
    if let Some(q) = &e.quality {
        let parts: Vec<String> = q.parts.iter().map(|(p, v)| format!("{} {v}", crate::tr(part_name(*p)))).collect();
        dialog::outcome(ui, verdict_score(q.score), &crate::trf!("Güte {a} · {b}", a = q.score, b = crate::tr(lensprof::quality::Level::of(q.score).text())), &[parts.join(" · ")]);
    }
    ui.add_space(4.0);
    egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
        let rmax = {
            let (w, h) = (3.0f64, 2.0f64);
            (w * w + h * h).sqrt() / 2.0 / w
        };
        for g in &e.geo {
            let f = run::focal_length_text(g.focal_length);
            match &g.distortion {
                Ok(v) => {
                    let corner = (raw::measure::ratio_val(&v.k, rmax) - 1.0) * 100.0;
                    let kind = if corner < -0.05 { crate::tr("tonnenförmig") } else if corner > 0.05 { crate::tr("kissenförmig") } else { crate::tr("praktisch keine") };
                    let heading = crate::trf!("{a}: Verzeichnung {b}, {c} % in der Ecke", a = f, b = kind, c = num1(corner.abs()));
                    let mut details = vec![
                        crate::trf!("Koeffizienten k1 {a} · k2 {b} · k3 {c}", a = format!("{:+.5}", v.k[0]), b = format!("{:+.5}", v.k[1]), c = format!("{:+.5}", v.k[2])),
                        crate::trf!("Restfehler der Linien {a} → {b} Bildpunkte · {c} Linien · Abdeckung {d} %", a = num2(v.rms_before), b = num2(v.rms_after), c = v.chains, d = (v.spread * 100.0).round()),
                        crate::trf!("Aufnahmen: {a}", a = g.images.join(", ")),
                    ];
                    for (n, reason) in &g.rejected {
                        details.push(crate::trf!("nicht verwendet: {a} – {b}", a = n, b = crate::tr(reason)));
                    }
                    if let Some(q) = &g.lateral_ca {
                        details.push(crate::trf!("Farbquerfehler am Rand: Rot {a} · Blau {b} Bildpunkte", a = num2(q.red_edge), b = num2(q.blue_edge)));
                    }
                    dialog::outcome(ui, verdict_geo(v), &heading, &details);
                }
                Err(m) => dialog::outcome(ui, Verdict::Red, &crate::trf!("{a}: Verzeichnung nicht gemessen", a = f), &[m.clone()]),
            }
            ui.add_space(2.0);
        }
        for g in &e.vig {
            let f = run::focal_length_text(g.focal_length);
            let n = g.aperture.map_or(String::new(), |n| format!(" f/{}", num1(n)));
            match &g.vignette {
                Ok(v) => {
                    let ev = v.corner.max(1e-3).log2();
                    let heading = crate::trf!("{a}{b}: Vignettierung {c} EV in der Ecke", a = f, b = n, c = num1(ev.abs()));
                    let verdict = if v.rest < 0.02 && v.angle_spread < 0.04 { Verdict::Green } else { Verdict::Yellow };
                    dialog::outcome(ui, verdict, &heading, &[crate::trf!("Aufnahmen: {a}", a = g.images.join(", "))]);
                }
                Err(m) => dialog::outcome(ui, Verdict::Red, &crate::trf!("{a}{b}: Vignettierung nicht gemessen", a = f, b = n), &[m.clone()]),
            }
            ui.add_space(2.0);
        }
        let names = channel_names(e.sensor);
        for g in &e.cast {
            let f = run::focal_length_text(g.focal_length);
            let n = g.aperture.map_or(String::new(), |n| format!(" f/{}", num1(n)));
            match &g.cast {
                Ok(c) => {
                    let q = &c.quality;
                    let corner: Vec<String> = (0..3).map(|k| format!("{} {}", crate::tr(names[k]), percent(q.corner[k]))).collect();
                    let heading = crate::trf!("{a}{b}: Farbstich, Güte {c}", a = f, b = n, c = q.score);
                    let mut details = vec![crate::trf!("In der Ecke: {a}", a = corner.join(" · ")), crate::trf!("Aufnahmen: {a}", a = g.images.join(", "))];
                    details.push(match q.pair_rms {
                        Some(p) => crate::trf!("Paar: Abweichung der beiden Aufnahmen {a} % · {b} Staubfelder ignoriert", a = num2(p as f64 * 100.0), b = q.dust_cells),
                        None => crate::tr("Nur eine Aufnahme: ohne Paar höchstens Güte 70").to_string(),
                    });
                    dialog::outcome(ui, verdict_score(q.score), &heading, &details);
                }
                Err(m) => dialog::outcome(ui, Verdict::Red, &crate::trf!("{a}{b}: Farbstich nicht gemessen", a = f, b = n), &[m.clone()]),
            }
            ui.add_space(2.0);
        }
        if !e.cast.is_empty() && e.sensor != lensprof::own::SensorKind::Foveon {
            dialog::hint(ui, "Der Farbstich wird bei Nicht-Sigma-RAWs noch nicht angewandt; Verzeichnung, Farbsäume und Vignettierung wirken.");
        }
        if !e.cast.is_empty() && e.sensor == lensprof::own::SensorKind::Foveon {
            dialog::hint(ui, "Der Farbstich wird bei Sigma-X3F noch nicht angewandt; Verzeichnung und Vignettierung wirken im Foveon-Pfad ebenfalls noch nicht.");
        }
        for h in &e.notes {
            dialog::hint(ui, &format!("⚠  {h}"));
        }
        if let Some((before, after)) = preview {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::tr("Vorher / Nachher")).small().color(lr::TEXT_DIM()));
            let b = (ui.available_width() - 8.0) / 2.0;
            ui.horizontal(|ui| {
                for t in [before, after] {
                    let sz = t.size_vec2();
                    ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(b, b * sz.y / sz.x)));
                }
            });
            dialog::hint(ui, "Links die Aufnahme, rechts mit dem gemessenen Profil entzerrt. Gerade Linien müssen gerade sein.");
        }
        ui.add_space(4.0);
        let unused = e.shots.iter().filter(|a| matches!(a.role, Role::Unusable(_))).count();
        let title = crate::trf!("Aufnahmen ({a}, davon {b} nicht verwendet)", a = e.shots.len(), b = unused);
        egui::CollapsingHeader::new(egui::RichText::new(title).small().color(lr::TEXT_DIM())).id_salt("ov-aufnahmen").default_open(*show_shots).show(ui, |ui| {
            for a in &e.shots {
                let role = match &a.role {
                    Role::Grid => crate::tr("Gitter").to_string(),
                    Role::FlatField => if a.rotated { crate::tr("Weißfeld 180°").to_string() } else { crate::tr("Fläche").to_string() },
                    Role::Unusable(g) => crate::trf!("nicht verwendet: {a}", a = crate::menu::i18n::tr_msg(g)),
                };
                let f = a.focal_length.map_or(String::new(), |f| format!(" · {f:.0} mm"));
                let n = a.aperture.map_or(String::new(), |n| format!(" f/{n:.1}"));
                ui.add(egui::Label::new(egui::RichText::new(format!("{}{f}{n}: {role}", a.name)).small().color(lr::TEXT_DIM())).wrap());
            }
        });
        ui.add_space(4.0);
        match saved {
            None => {
                if e.profile.is_some() {
                    ui.label(egui::RichText::new(crate::tr("DANACH ZUWEISEN")).small().color(lr::TEXT_DIM()));
                    if e.lens.as_deref().map(str::trim).unwrap_or("").is_empty() {
                        ui.checkbox(assign_rule, crate::tr("allen Fotos dieser Kamera ohne Objektivangabe (Vorgabe)"));
                    }
                    ui.checkbox(assign_photo, crate::tr("dem geöffneten Foto"));
                }
            }
            Some(Ok(r)) => {
                dialog::outcome(ui, Verdict::Green, "Profil gespeichert", &[crate::trf!("{a} · Version {b}", a = e.name.clone().unwrap_or_default(), b = r.version)]);
            }
            Some(Err(m)) => {
                dialog::outcome(ui, Verdict::Red, "Speichern fehlgeschlagen", &[m.clone()]);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drop in step 3 adds RAW files and the RAW files of dropped folders; other steps leave the drop to the import.
    #[test]
    fn drop_adds_raws_only_in_the_shots_step() {
        let dir = std::env::temp_dir().join(format!("graineo-objektiv-drop-{}", std::process::id()));
        let sub = dir.join("weissfeld");
        std::fs::create_dir_all(&sub).unwrap();
        for f in ["a.CR2", "b.jpg"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::write(sub.join("c.X3F"), b"x").unwrap();
        let dropped = vec![dir.join("a.CR2"), dir.join("b.jpg"), sub.clone()];
        open();
        assert!(!take_dropped(&dropped), "step 1 does not take a drop");
        Z.with(|z| z.borrow_mut().as_mut().unwrap().phase = Phase::Shots);
        assert!(take_dropped(&dropped));
        let names: Vec<String> = Z.with(|z| z.borrow().as_ref().unwrap().rows.iter().map(|r| r.path.file_name().unwrap().to_string_lossy().to_string()).collect());
        assert_eq!(names, vec!["a.CR2", "c.X3F"]);
        assert!(take_dropped(&dropped));
        assert_eq!(Z.with(|z| z.borrow().as_ref().unwrap().rows.len()), 2, "no duplicates");
        Z.with(|z| *z.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
