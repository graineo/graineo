//! "Alle Reihen zusammenfügen …": several exposure brackets at once.
//!
//! The dialog lists the brackets found in the selection (detected brackets of the library, other stacks that differ in
//! exposure) with one switch each and the options every merge shares (motion, output, alignment, stack, keyword).
//! "Zusammenfügen" creates one merged photo per bracket in the library (`LibraryView::start_merge_batch`) and computes
//! them **one after the other** in a single background job: the title bar shows "Reihe 3 von 12", cancelling stops the
//! running merge and takes the not yet computed ones out again, ⌘Z takes the whole batch back (`Undo::MergeBatch`).
//! Every result is made exactly as the result of a single merge (same recipe, cache and grid preview).

use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::dialog::{self, Answer, Buttons, Part};
use crate::hdr_ui::{Request, SourceRow};
use crate::lr;

/// One bracket offered in the dialog.
#[derive(Clone, Debug)]
pub struct Group {
    /// the photos, dark → bright
    pub rows: Vec<SourceRow>,
    /// switched on for merging
    pub on: bool,
    /// there is a merged photo in this stack already (switched off at first)
    pub merged: bool,
    /// why this bracket cannot be merged
    pub reason: Option<String>,
}

/// Exposure brackets in a set of photos: shots one after the other (same camera, size and focal length, at most 12 s
/// apart plus the exposure time) whose exposure differs from every earlier one of the bracket by at least 0.4 EV; a
/// repeated exposure starts the next bracket. Brackets have at least two photos and one EV of spread, at most nine photos.
/// Independent of the series of the library: those compare image content and split wide brackets (−3 … +3 EV) apart.
pub fn brackets(mut rows: Vec<SourceRow>) -> Vec<Vec<SourceRow>> {
    rows.retain(|r| r.capture.is_some() && r.exif_ev().is_some());
    rows.sort_by(|a, b| (&a.camera, a.capture, &a.name).cmp(&(&b.camera, b.capture, &b.name)));
    let same = |a: &SourceRow, b: &SourceRow| {
        let focal = match (a.focal, b.focal) {
            (Some(x), Some(y)) => (x - y).abs() <= 0.01 * x.max(y),
            _ => true,
        };
        a.camera == b.camera && (a.width, a.height) == (b.width, b.height) && focal
    };
    let mut out: Vec<Vec<SourceRow>> = Vec::new();
    for r in rows {
        let ev = r.exif_ev().unwrap_or(0.0);
        let joins = out.last().is_some_and(|g| {
            let last = &g[g.len() - 1];
            let gap = r.capture.unwrap_or(0) - last.capture.unwrap_or(0);
            let max_gap = 12.0 + last.time.unwrap_or(0.0);
            same(&g[0], &r) && gap as f64 <= max_gap && g.len() < hdr::MAX_FRAMES && g.iter().all(|m| (m.exif_ev().unwrap_or(0.0) - ev).abs() >= 0.4)
        });
        if joins {
            out.last_mut().map(|g| g.push(r));
        } else {
            out.push(vec![r]);
        }
    }
    out.retain(|g| {
        let evs: Vec<f64> = g.iter().filter_map(|r| r.exif_ev()).collect();
        g.len() >= hdr::MIN_FRAMES && evs.iter().copied().fold(f64::MIN, f64::max) - evs.iter().copied().fold(f64::MAX, f64::min) >= 1.0
    });
    out
}

impl Group {
    /// A bracket (from [`brackets`]) for the dialog; `merged`: there is a merged photo in its stack already.
    pub fn new(mut rows: Vec<SourceRow>, merged: bool) -> Option<Group> {
        if rows.len() < hdr::MIN_FRAMES {
            return None;
        }
        rows.sort_by(|a, b| a.exif_ev().unwrap_or(0.0).total_cmp(&b.exif_ev().unwrap_or(0.0)).then(a.name.cmp(&b.name)));
        let mut g = Group { rows, on: false, merged, reason: None };
        g.reason = crate::hdr_ui::refusal(&g.rows);
        g.on = g.reason.is_none() && !merged;
        Some(g)
    }

    /// Capture time of the first shot of the bracket in seconds (order of the list and of the computations).
    pub fn time(&self) -> f64 {
        self.rows.iter().filter_map(|r| r.capture).min().map_or(0.0, |c| c as f64)
    }

    /// Exposure spread in EV (from the shooting data).
    fn spread(&self) -> Option<f64> {
        let evs: Vec<f64> = self.rows.iter().filter_map(|r| r.exif_ev()).collect();
        let lo = evs.iter().copied().fold(f64::MAX, f64::min);
        let hi = evs.iter().copied().fold(f64::MIN, f64::max);
        (!evs.is_empty()).then_some(hi - lo)
    }

    /// "IMG_0101 … IMG_0103" (first and last name in file order).
    fn title(&self) -> String {
        let stem = |n: &str| n.rsplit_once('.').map_or(n.to_string(), |(a, _)| a.to_string());
        let mut names: Vec<&str> = self.rows.iter().map(|r| r.name.as_str()).collect();
        names.sort();
        match (names.first(), names.last()) {
            (Some(a), Some(b)) if names.len() > 1 => format!("{} … {}", stem(a), stem(b)),
            (Some(a), _) => stem(a),
            _ => String::new(),
        }
    }

    /// The grey line below the title.
    fn detail(&self) -> String {
        if let Some(why) = &self.reason {
            return why.clone();
        }
        let n = self.rows.len();
        let spread = self.spread().map(|s| format!(" · {} EV", crate::menu::i18n::format::decimal(crate::menu::i18n::language(), s, if s.fract().abs() < 0.05 { 0 } else { 1 }))).unwrap_or_default();
        let done = if self.merged { format!(" · {}", crate::tr("schon zusammengefügt")) } else { String::new() };
        format!("{}{spread}{done}", crate::trf!("{n} Bilder", n = n))
    }
}

struct BatchDialog {
    groups: Vec<Group>,
    ghost: hdr::ghost::Level,
    output: hdr::fusion::Output,
    align: bool,
    in_stack: bool,
    keyword: String,
    opened: std::time::Instant,
}

static DIALOG: Mutex<Option<BatchDialog>> = Mutex::new(None);

/// Keys shared with the dialog of a single merge (the settings are remembered together).
const KEY_STACK: &str = "hdrInStack";
const KEY_KEYWORD: &str = "hdrKeyword";

/// Why "Alle Reihen zusammenfügen …" does nothing while a batch runs.
pub const RUNNING: &str = "Es werden gerade Reihen zusammengefügt – weitere Reihen danach";

/// Is a merge of a cancelled batch that was not computed yet taken out of the library again? Not when something was
/// computed for it meanwhile (the user opened it) or it carries work of the user: cancelling a batch stops
/// computations, it never takes work away. Such a photo stays (computed when it is opened) and belongs to the ⌘Z step
/// of the batch like the finished ones.
pub(crate) fn abandoned_on_cancel(has_result: bool, has_work: bool) -> bool {
    !has_result && !has_work
}

/// Open the dialog for these brackets.
pub fn open(groups: Vec<Group>) {
    // one batch at a time: a second one would share the progress, the cancel flag and the ⌘Z step of the first
    if active() {
        crate::hdr_live::say(crate::tr(RUNNING));
        return;
    }
    let mut g = DIALOG.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_some() {
        return;
    }
    *g = Some(BatchDialog {
        groups,
        ghost: hdr::ghost::Level::default(),
        output: hdr::fusion::Output::default(),
        align: true,
        in_stack: library::settings::value(KEY_STACK).as_bool().unwrap_or(true),
        keyword: library::settings::value(KEY_KEYWORD).as_str().map(str::to_string).unwrap_or_else(|| crate::hdr_ui::DEFAULT_KEYWORD.to_string()),
        opened: std::time::Instant::now(),
    });
}

#[cfg(test)]
pub(crate) fn is_open() -> bool {
    DIALOG.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

fn close() {
    DIALOG.lock().unwrap_or_else(|e| e.into_inner()).take();
}

fn options_box(ui: &mut egui::Ui, d: &mut BatchDialog) {
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
    egui::Frame::new().fill(egui::Color32::from_gray(0x19)).stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(0x30))).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        use hdr::ghost::Level;
        let levels = [(Level::Off, "Aus"), (Level::Standard, "Standard"), (Level::Fine, "Fein")];
        let tips = ["Alle Aufnahmen überall verrechnen (für ruhige Motive)", "Bewegte Stellen aus der Referenz, weich überblendet", "Jede Aufnahme einzeln dort ausschließen, wo sie abweicht"];
        lr::choice_tips(ui, "Bewegung", &mut d.ghost, &levels, &tips, "");
        let outputs = [(hdr::fusion::Output::Linear, "Linear"), (hdr::fusion::Output::Fusion, "Fusion")];
        let output_tips = ["Szenenlinear, die Lichter bleiben über Weiß erhalten (für die eigene Entwicklung)", "Natürlicher Look: die Belichtungen werden überblendet, der Umfang auf 0 bis 1 verdichtet"];
        lr::choice_tips(ui, "Ausgabe", &mut d.output, &outputs, &output_tips, "");
        let in_stack = d.in_stack;
        dialog::option(ui, &mut d.in_stack, crate::tr("Ergebnis in den Stapel legen"), crate::tr(if in_stack { "Oben im Stapel des Referenzbilds" } else { "Eigenes Foto neben den Quellen, ohne Stapel" }), None);
        dialog::option(ui, &mut d.align, crate::tr("Ausrichten"), crate::tr("Gleicht Verschiebung, Drehung und Maßstab zwischen den Aufnahmen aus"), None);
        ui.add_space(2.0);
        ui.label(egui::RichText::new(crate::tr("Stichwort für das Ergebnis")).small().color(lr::TEXT_DIM()));
        lr::edit_field(ui, egui::TextEdit::singleline(&mut d.keyword).desired_width(ui.available_width()).hint_text(crate::tr("leer = keins")));
    });
}

/// One row of the list: switch, title, grey line.
fn group_row(ui: &mut egui::Ui, g: &mut Group) {
    let locked = g.reason.is_some();
    ui.add_enabled_ui(!locked, |ui| {
        ui.horizontal(|ui| {
            let mut off = false;
            let on: &mut bool = if locked { &mut off } else { &mut g.on };
            lr::switch(ui, on);
            let title = g.title();
            if ui.add(egui::Label::new(egui::RichText::new(title).color(lr::TEXT())).truncate().selectable(false).sense(egui::Sense::click())).clicked() && !locked {
                g.on = !g.on;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(44.0);
            ui.add(egui::Label::new(egui::RichText::new(g.detail()).small().color(lr::TEXT_DIM())).truncate());
        });
    });
}

/// Draw the dialog (if open). Returns one request per switched-on bracket when the user chose "Zusammenfügen".
pub fn show(ctx: &egui::Context) -> Option<Vec<Request>> {
    let mut g = DIALOG.lock().unwrap_or_else(|e| e.into_inner());
    let d = g.as_mut()?;
    let width = (ctx.content_rect().width() - 2.0 * 24.0 - 40.0).clamp(420.0, 640.0);
    let n_on = d.groups.iter().filter(|g| g.on).count();
    let all = d.groups.len();
    // the title names the function; the number of brackets that will run is on the button
    let title = crate::tr("Alle Reihen zusammenfügen").to_string();
    let label = if n_on == 1 { crate::tr("Eine Reihe zusammenfügen").to_string() } else { crate::trf!("{n} Reihen zusammenfügen", n = n_on) };
    let answer = dialog::show_scrolling(
        ctx,
        "hdr-batch",
        &title,
        width,
        34.0,
        |ui, part| match part {
            Part::Header => {}
            Part::Body => {
                ui.spacing_mut().item_spacing.y = 6.0;
                for grp in d.groups.iter_mut() {
                    group_row(ui, grp);
                    ui.add_space(2.0);
                }
                ui.add_space(8.0);
                options_box(ui, d);
            }
            Part::Footer => {
                if n_on == 0 {
                    dialog::warning(ui, crate::tr("Keine Reihe eingeschaltet."));
                } else if d.in_stack {
                    dialog::hint(ui, &crate::trf!("{a} von {b} Reihen · eine nach der anderen, Ergebnisse oben im Stapel", a = n_on, b = all));
                } else {
                    dialog::hint(ui, &crate::trf!("{a} von {b} Reihen · eine nach der anderen, Ergebnisse neben den Quellen", a = n_on, b = all));
                }
            }
        },
        Buttons::confirm(&label, n_on > 0),
    );
    // ↩ is ignored in the first moments (the key press that opened the dialog)
    let clicked = ctx.input(|i| i.pointer.any_released());
    let answer = if answer == Answer::Primary && !clicked && d.opened.elapsed() < std::time::Duration::from_millis(500) { Answer::Open } else { answer };
    // test hook: GRAINEO_HDR_AUTO=1 confirms the dialog after a few seconds (screenshots of the job)
    let auto = crate::env::var_os("GRAINEO_HDR_AUTO").is_some_and(|v| v == "1") && crate::env::var_os("GRAINEO_DATA_DIR").is_some() && d.opened.elapsed() > std::time::Duration::from_secs(4);
    let answer = if answer == Answer::Open && auto { Answer::Primary } else { answer };
    match answer {
        Answer::Primary if n_on > 0 => {
            library::settings::set(KEY_STACK, serde_json::json!(d.in_stack));
            library::settings::set(KEY_KEYWORD, serde_json::json!(d.keyword.trim()));
            let settings = hdr::Settings { reference: None, align: d.align, ghost: d.ghost, output: d.output, ..hdr::Settings::new_merge() };
            let reqs = d.groups.iter().filter(|g| g.on).map(|g| Request { rows: g.rows.clone(), settings: settings.clone(), stack: d.in_stack, keyword: d.keyword.trim().to_string() }).collect();
            drop(g);
            close();
            Some(reqs)
        }
        Answer::Cancel => {
            drop(g);
            close();
            None
        }
        _ => None,
    }
}

// ─────────────────────────────────────────────── the batch job

/// One merge of a batch, already created in the library.
pub struct Item {
    pub src: rawdev::merged::MergedSource,
    pub name: String,
    pub undo: Option<library::series::Change>,
}

#[derive(Default)]
struct State {
    /// files of the batch in order, with the position of the one being computed
    files: Vec<i64>,
    current: Option<usize>,
    /// a batch is being created in the library (its job has not started yet)
    starting: bool,
    cancel: Option<Arc<AtomicBool>>,
}

static STATE: Mutex<State> = Mutex::new(State { files: Vec::new(), current: None, starting: false, cancel: None });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Position `(k, n)` ("Reihe k von n") of the running merge of `file` when it belongs to a batch of more than one.
pub fn position(file: i64) -> Option<(usize, usize)> {
    let s = state();
    let n = s.files.len();
    (n > 1 && s.current.is_some_and(|c| s.files.get(c) == Some(&file))).then(|| (s.current.unwrap_or(0) + 1, n))
}

/// Is a batch running (or being created in the library)?
pub fn active() -> bool {
    let s = state();
    s.starting || s.current.is_some()
}

/// A batch is about to be created: from now on it counts as running (a second one asked for before the library has
/// written the first would share its state). `false`: one runs already.
pub fn reserve() -> bool {
    let mut s = state();
    if s.starting || s.current.is_some() {
        return false;
    }
    s.starting = true;
    true
}

/// The batch reserved was not started (nothing could be created).
pub fn release() {
    state().starting = false;
}

/// Stop the batch: the running merge ends (the caller cancels it), the ones not yet computed are taken out again.
pub fn cancel() {
    if let Some(c) = &state().cancel {
        c.store(true, Ordering::Relaxed);
    }
}

/// Is `file` waiting for its turn or running in a batch?
pub fn pending(file: i64) -> bool {
    let s = state();
    s.current.is_some() && s.files.iter().position(|f| *f == file).is_some_and(|p| s.current.is_some_and(|c| p >= c))
}

/// Test: a new session (the state of a batch whose session ended is forgotten).
#[cfg(test)]
pub(crate) fn test_new_session() {
    *state() = State::default();
    close();
}

/// Test: the files of the batch and the position of the one being computed.
#[cfg(test)]
pub(crate) fn test_state() -> (Vec<i64>, Option<usize>) {
    let s = state();
    (s.files.clone(), s.current)
}

/// Compute the merges one after the other (background thread). Every merge asks for its turn anew
/// ([`crate::hdr_ui::turn`]): a change of another photo the user makes meanwhile is computed before the next bracket.
pub fn run(items: Vec<Item>) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut s = state();
        s.files = items.iter().map(|i| i.src.file).collect();
        s.current = Some(0);
        s.starting = false;
        s.cancel = Some(cancel.clone());
    }
    crate::hdr_ui::mark_changed();
    crate::hdr_ui::spawn(move || {
        #[cfg(test)]
        let session = crate::hdr_ui::test_session();
        // Whatever happens to this thread (a panic as well): the batch does not count as running for ever, and the
        // app hears that it ended (a ⌘Z waiting for it goes on).
        struct Ended {
            removed: bool,
            cancelled: bool,
            #[cfg(test)]
            session: u64,
        }
        impl Drop for Ended {
            fn drop(&mut self) {
                // (test: a batch of a session that ended leaves no trace, as if its process had gone)
                #[cfg(test)]
                if self.session != crate::hdr_ui::test_session() {
                    return;
                }
                {
                    let mut s = state();
                    s.current = None;
                    s.cancel = None;
                }
                crate::hdr_ui::mark_changed();
                crate::hdr_ui::push(crate::hdr_ui::Event::BatchEnded { removed: self.removed, cancelled: self.cancelled });
            }
        }
        let mut ended = Ended {
            removed: false,
            cancelled: false,
            #[cfg(test)]
            session,
        };
        for (k, item) in items.iter().enumerate() {
            #[cfg(test)]
            if session != crate::hdr_ui::test_session() {
                return;
            }
            if cancel.load(Ordering::Relaxed) {
                ended.cancelled = true;
                // not started: out of the library again, as when a single merge is cancelled – unless the user worked
                // on it meanwhile or something was computed for it (it stays; ⌘Z of the batch takes it out as well)
                let file = item.src.file;
                if abandoned_on_cancel(rawdev::merged::has_cached(file) || crate::hdr_ui::job_alive(file), crate::hdr_ui::carries_work(file)) {
                    ended.removed |= crate::hdr_ui::discard(&item.src, item.undo.as_ref());
                }
                continue;
            }
            state().current = Some(k);
            crate::hdr_ui::mark_changed();
            // (a panic outside the computation itself ends this merge, not the batch)
            let end = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::hdr_ui::job(&item.src, &item.name, item.undo.as_ref(), false))).unwrap_or(crate::hdr_ui::JobEnd::Failed);
            if end == crate::hdr_ui::JobEnd::Failed {
                if cancel.load(Ordering::Relaxed) {
                    ended.cancelled = true;
                }
                // (it took itself out of the library unless it shows a picture or carries work)
                ended.removed = true;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn row(name: &str, t: f64) -> SourceRow {
        SourceRow { file: 1, image: 1, path: PathBuf::from(format!("/nicht/da/{name}")), name: name.into(), format: "CR2".into(), camera: Some("Canon".into()), time: Some(t), iso: Some(100), aperture: Some(8.0), focal: None, capture: None, width: Some(100), height: Some(80) }
    }

    fn shot(name: &str, capture: i64, t: f64) -> SourceRow {
        SourceRow { capture: Some(capture), ..row(name, t) }
    }

    /// Cancelling a batch takes the merges not yet computed out again – never one the user opened (something was
    /// computed for it) or worked on.
    #[test]
    fn cancel_of_a_batch_keeps_merges_with_results_or_work() {
        assert!(abandoned_on_cancel(false, false));
        assert!(!abandoned_on_cancel(true, false));
        assert!(!abandoned_on_cancel(false, true));
        assert!(!abandoned_on_cancel(true, true));
    }

    #[test]
    fn finds_brackets_in_a_run_of_shots() {
        // two brackets of three shots (0, −2, +2 EV order of the camera), then a single shot, then a burst of equal exposures
        let rows = vec![
            shot("a1", 100, 0.01),
            shot("a2", 100, 0.0025),
            shot("a3", 101, 0.04),
            shot("b1", 105, 0.01),
            shot("b2", 105, 0.0025),
            shot("b3", 106, 0.04),
            shot("c1", 400, 0.01),
            shot("d1", 600, 0.01),
            shot("d2", 600, 0.01),
            shot("d3", 601, 0.01),
        ];
        let b = brackets(rows);
        let names: Vec<Vec<String>> = b.iter().map(|g| g.iter().map(|r| r.name.clone()).collect()).collect();
        assert_eq!(names, vec![vec!["a1", "a2", "a3"], vec!["b1", "b2", "b3"]]);
    }

    #[test]
    fn long_exposures_keep_the_chain_and_big_gaps_break_it() {
        // 1 s, 0.125 s, 8 s apart by the exposure times; a new scene a minute later with the same settings
        let rows = vec![shot("m1", 0, 1.0), shot("m2", 0, 0.125), shot("m3", 4, 4.0), shot("m4", 13, 8.0), shot("n1", 120, 1.0), shot("n2", 121, 0.125)];
        let names: Vec<usize> = brackets(rows).iter().map(|g| g.len()).collect();
        assert_eq!(names, vec![4, 2]);
    }

    #[test]
    fn other_cameras_and_sizes_do_not_chain() {
        let mut b = shot("b", 101, 0.04);
        b.camera = Some("Nikon".into());
        assert!(brackets(vec![shot("a", 100, 0.01), b]).is_empty());
        let mut c = shot("c", 101, 0.04);
        c.width = Some(200);
        assert!(brackets(vec![shot("a", 100, 0.01), c]).is_empty());
        // one EV of spread is the least
        assert!(brackets(vec![shot("a", 100, 0.01), shot("b", 100, 0.0141)]).is_empty());
    }

    #[test]
    fn group_refuses_unreachable_sources() {
        let g = Group::new(vec![row("a.CR2", 0.01), row("b.CR2", 0.04), row("c.CR2", 0.16)], false).unwrap();
        assert_eq!(g.rows[0].name, "a.CR2");
        assert!(g.reason.is_some(), "sources are not on disk");
        assert!(!g.on);
        assert!(Group::new(vec![row("a.CR2", 0.01)], false).is_none());
    }

    #[test]
    fn title_and_detail() {
        let g = Group::new(vec![row("IMG_0103.CR2", 0.16), row("IMG_0101.CR2", 0.01), row("IMG_0102.CR2", 0.04)], false).unwrap();
        assert_eq!(g.title(), "IMG_0101 … IMG_0103");
        assert!(g.detail().len() > 0);
    }

    /// The dialog lists the brackets with the main button, a refused one stays off; Esc closes it.
    #[test]
    fn dialog_lists_brackets() {
        close();
        let ctx = egui::Context::default();
        let g1 = Group::new(vec![row("IMG_0101.CR2", 0.01), row("IMG_0102.CR2", 0.04), row("IMG_0103.CR2", 0.16)], false).unwrap();
        let g2 = Group::new(vec![row("IMG_0201.CR2", 0.01), row("IMG_0202.CR2", 0.04)], false).unwrap();
        open(vec![g1, g2]);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 900.0));
        let mut texts = Vec::new();
        for _ in 0..3 {
            let out = crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                assert!(show(ui.ctx()).is_none());
            });
            texts = out.shapes.iter().filter_map(|s| if let egui::epaint::Shape::Text(t) = &s.shape { Some(t.galley.text().to_string()) } else { None }).collect();
        }
        let all = texts.join("\n");
        assert!(all.contains("IMG_0101 … IMG_0103"), "{all}");
        assert!(all.contains("IMG_0201 … IMG_0202"), "{all}");
        let key = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        crate::ui_test::run(&ctx, egui::RawInput { screen_rect: Some(screen), events: vec![key], ..Default::default() }, |ui| {
            show(ui.ctx());
        });
        assert!(!is_open());
    }

    #[test]
    fn position_counts_only_real_batches() {
        let _app_lock = crate::ui_test::app_lock();
        {
            let mut s = state();
            s.files = vec![10, 11, 12];
            s.current = Some(1);
        }
        assert_eq!(position(11), Some((2, 3)));
        assert_eq!(position(10), None);
        assert!(pending(12) && pending(11) && !pending(10));
        *state() = State::default();
        assert_eq!(position(11), None);
        // B13: a batch counts as running from the moment it is asked for, not only once the library has created it
        assert!(!active());
        assert!(reserve());
        assert!(active(), "reserved: a second one is refused");
        assert!(!reserve());
        release();
        assert!(!active());
    }
}
