//! What the AI tools of the Masks tray are doing right now ("Motiv", "Personen", "Klick-Auswahl", "Objekte
//! entfernen"): a job registers when it starts and names its step ("Bild wird entwickelt …", "Modell wird geladen …"),
//! the tray shows "Ermittle …" with a spinner on the button and a status line with step, seconds and "Abbrechen", the
//! image a small hint at its top edge. Cancelling stops the job between two steps (a running model step cannot be
//! interrupted; the line says so until it ends) and its result is thrown away.
//!
//! Every entry leaves the list when its [`Ticket`] is dropped, also when the job panics.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use egui::Color32;

use crate::lr;

/// The tools that compute a selection or a fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Subject,
    Person,
    Click,
    Remove,
    Depth,
    Sky,
    /// the image the colour pipette samples (development before the masks)
    Sample,
    /// parts of people (face landmarks)
    Parts,
}

impl Tool {
    /// Name of the result in the image hint (German key, translated when shown).
    fn name(self) -> &'static str {
        match self {
            Tool::Subject => "Motiv",
            Tool::Person => "Personen",
            Tool::Click => "Klick-Auswahl",
            Tool::Remove => "Füllung",
            Tool::Depth => "Tiefe",
            Tool::Sky => "Himmel",
            Tool::Sample => "Farbprobe",
            Tool::Parts => "Personen-Teile",
        }
    }
}

/// Steps (German keys, translated when shown).
pub const DEVELOP: &str = "Bild wird entwickelt …";
pub const LOAD: &str = "Modell wird geladen …";
pub const SUBJECT: &str = "Motiv wird gesucht …";
pub const PEOPLE: &str = "Personen werden gesucht …";
pub const FACES: &str = "Gesichter werden gesucht …";
pub const ENCODE: &str = "Bild wird analysiert …";
pub const DECODE: &str = "Auswahl wird berechnet …";
pub const FILL: &str = "Bereich wird gefüllt …";
pub const DEPTH: &str = "Tiefe wird geschätzt …";
pub const SKY: &str = "Himmel wird gesucht …";
pub const SEGMENTS: &str = "Segmente werden gebildet …";
const CANCELLING: &str = "Wird abgebrochen (nach diesem Schritt) …";
/// Message of a cancelled job.
pub const CANCELLED: &str = "Abgebrochen";

struct Entry {
    id: u64,
    tool: Tool,
    path: PathBuf,
    step: &'static str,
    since: Instant,
    cancel: Arc<AtomicBool>,
}

static RUNNING: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

fn list() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// A registered job; leaves the list when dropped.
pub struct Ticket {
    id: u64,
    cancel: Arc<AtomicBool>,
}

/// A job of `tool` for the photo at `path` starts with `step`.
pub fn begin(tool: Tool, path: &Path, step: &'static str) -> Ticket {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    list().push(Entry { id, tool, path: path.to_path_buf(), step, since: Instant::now(), cancel: cancel.clone() });
    Ticket { id, cancel }
}

impl Ticket {
    /// The job moves on to `step`; `Err` when it was cancelled (the job stops here).
    pub fn step(&self, step: &'static str) -> Result<(), String> {
        if self.cancelled() {
            return Err(CANCELLED.into());
        }
        if let Some(e) = list().iter_mut().find(|e| e.id == self.id) {
            e.step = step;
        }
        Ok(())
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Step callback for the model runs of `segment` (load, compute): names the step, `false` stops the run.
    pub fn model_step(&self, run: &'static str) -> impl FnMut(segment::Step) -> bool + '_ {
        move |s| self.step(if s == segment::Step::Load { LOAD } else { run }).is_ok()
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        list().retain(|e| e.id != self.id);
    }
}

/// Is `e` the error of a cancelled job (cancelled here or between two model steps)?
pub fn is_cancel(e: &str) -> bool {
    e == CANCELLED || e == segment::CANCELLED
}

/// A running job as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub tool: Tool,
    /// German key of the step (or of "cancelling")
    pub step: &'static str,
    pub seconds: u64,
    pub cancelling: bool,
}

/// The job of one of `tools` running for `path` (the one started last).
pub fn current(path: &Path, tools: &[Tool]) -> Option<Shown> {
    let v = list();
    let e = v.iter().rev().find(|e| e.path == path && tools.contains(&e.tool))?;
    let cancelling = e.cancel.load(Ordering::Relaxed);
    Some(Shown { tool: e.tool, step: if cancelling { CANCELLING } else { e.step }, seconds: e.since.elapsed().as_secs(), cancelling })
}

/// Is a job of `tool` running for `path`?
pub fn running(path: &Path, tool: Tool) -> bool {
    list().iter().any(|e| e.path == path && e.tool == tool)
}

/// Cancel the jobs of `tools` for `path` (they stop at their next step).
pub fn cancel(path: &Path, tools: &[Tool]) {
    for e in list().iter().filter(|e| e.path == path && tools.contains(&e.tool)) {
        e.cancel.store(true, Ordering::Relaxed);
    }
}

/// Text of a running job: "Motiv wird gesucht … · seit 3 s" (seconds from the first full second on).
pub fn text(s: &Shown) -> String {
    let step = crate::tr(s.step);
    if s.seconds >= 1 {
        format!("{step} · {}", crate::trf!("seit {n} s", n = s.seconds))
    } else {
        step.to_string()
    }
}

/// Button of a tool: while its job runs (`busy`) it shows a spinner and "Ermittle …" in the same place and at exactly
/// the width of the label, so nothing in the row moves (where "Ermittle …" does not fit, the spinner alone; the status
/// line names tool and step). `enabled` applies in both states (a mode switch stays usable while it computes; a start
/// button is locked by its caller).
pub fn button(ui: &mut egui::Ui, label: &str, busy: bool, enabled: bool, selected: bool) -> egui::Response {
    if !busy {
        return ui.add_enabled(enabled, egui::Button::new(crate::tr(label)).selected(selected));
    }
    let font = egui::TextStyle::Button.resolve(ui.style());
    let w_label = ui.painter().layout_no_wrap(crate::tr(label).to_string(), font.clone(), crate::lr::TEXT_STRONG()).size().x;
    let pad = ui.spacing().button_padding;
    let size = egui::vec2(w_label + 2.0 * pad.x, ui.spacing().interact_size.y);
    let r = ui.add_enabled(enabled, egui::Button::new("").min_size(size).selected(selected));
    let spin = 12.0;
    let text = crate::tr("Ermittle …");
    let g = ui.painter().layout_no_wrap(text.to_string(), font, lr::TEXT());
    let fits = g.size().x + spin + 4.0 <= w_label + 1.0;
    let w = if fits { g.size().x + spin + 4.0 } else { spin };
    let left = r.rect.center().x - w / 2.0;
    let spin_rect = egui::Rect::from_center_size(egui::pos2(left + spin / 2.0, r.rect.center().y), egui::vec2(spin, spin));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(spin_rect));
    crate::activity::calm_spinner(&mut child, spin, lr::TEXT());
    if fits {
        ui.painter().galley(egui::pos2(left + spin + 4.0, r.rect.center().y - g.size().y / 2.0), g, lr::TEXT());
    }
    r
}

/// Status line of the tools (fixed height, always present where it is called, so nothing moves): while a job of
/// `tools` runs, spinner, step and seconds and "Abbrechen"; otherwise `idle` (the last result, or empty).
pub fn status_line(ui: &mut egui::Ui, path: &Path, tools: &[Tool], idle: Option<&str>) {
    let h = ui.spacing().interact_size.y;
    let w = lr::visible_width(ui);
    ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(h);
        ui.set_max_width(w);
        // text edge of the tray (as headings, slider names and notes)
        ui.add_space(lr::TEXT_INSET);
        match current(path, tools) {
            Some(s) => {
                crate::activity::calm_spinner(ui, 12.0, lr::TEXT_DIM());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(!s.cancelling, egui::Button::new(crate::tr("Abbrechen")).small());
                    lr::hover_tip(ui, &r, "Berechnung abbrechen (ein laufender Rechenschritt endet noch)");
                    if r.clicked() {
                        cancel(path, tools);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // the tool is marked by the spinner on its button; the line names the step
                        ui.add(egui::Label::new(egui::RichText::new(text(&s)).small().color(lr::TEXT())).truncate());
                    });
                });
                // the seconds count on (a few times a second is enough)
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
            None => {
                if let Some(t) = idle.filter(|t| !t.is_empty()) {
                    ui.add(egui::Label::new(egui::RichText::new(t).small().color(lr::TEXT_DIM())).truncate());
                }
            }
        }
    });
}

/// Hint at the top edge of the image while a job runs for the photo shown: spinner, tool and step with seconds.
pub fn image_hint(ui: &egui::Ui, painter: &egui::Painter, area: egui::Rect, path: &Path) {
    let Some(s) = current(path, &[Tool::Subject, Tool::Person, Tool::Click, Tool::Remove, Tool::Depth, Tool::Sky, Tool::Sample, Tool::Parts]) else { return };
    let t = format!("{} · {}", crate::tr(s.tool.name()), text(&s));
    let galley = painter.layout_no_wrap(t, egui::FontId::proportional(12.5), crate::lr::overlay(235));
    let pad = egui::vec2(10.0, 6.0);
    let spin = 14.0;
    let size = galley.size() + pad * 2.0 + egui::vec2(spin + 8.0, 0.0);
    let rect = egui::Rect::from_min_size(egui::pos2(area.center().x - size.x / 2.0, area.top() + 12.0), size);
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(165));
    // turning arc, a few frames a second (as the spinners of the activity list)
    let now = (ui.input(|i| i.time) * 8.0).floor() / 8.0;
    let c = egui::pos2(rect.left() + pad.x + spin / 2.0, rect.center().y);
    let start = now * std::f64::consts::TAU;
    let points: Vec<egui::Pos2> = (0..16)
        .map(|i| {
            let a = start + (i as f64 / 15.0) * 4.2;
            c + (spin / 2.0 - 1.5) * egui::vec2(a.cos() as f32, a.sin() as f32)
        })
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, crate::lr::overlay(220))));
    painter.galley(egui::pos2(rect.left() + pad.x + spin + 8.0, rect.top() + pad.y), galley, crate::lr::TEXT_STRONG());
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(125));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_shows_its_step_and_leaves_when_done() {
        let path = PathBuf::from("/private/tmp/graineo-progress-test/a.jpg");
        assert!(current(&path, &[Tool::Subject]).is_none());
        {
            let t = begin(Tool::Subject, &path, DEVELOP);
            assert_eq!(current(&path, &[Tool::Subject, Tool::Person]).map(|s| s.step), Some(DEVELOP));
            // another tool or photo does not see it
            assert!(current(&path, &[Tool::Remove]).is_none());
            assert!(current(Path::new("/private/tmp/graineo-progress-test/b.jpg"), &[Tool::Subject]).is_none());
            t.step(SUBJECT).unwrap();
            assert_eq!(current(&path, &[Tool::Subject]).map(|s| s.step), Some(SUBJECT));
            assert!(running(&path, Tool::Subject));
        }
        assert!(current(&path, &[Tool::Subject]).is_none());
        assert!(!running(&path, Tool::Subject));
    }

    #[test]
    fn cancelling_stops_at_the_next_step() {
        let path = PathBuf::from("/private/tmp/graineo-progress-test/c.jpg");
        let t = begin(Tool::Remove, &path, DEVELOP);
        cancel(&path, &[Tool::Remove]);
        let s = current(&path, &[Tool::Remove]).unwrap();
        assert!(s.cancelling);
        assert_eq!(s.step, CANCELLING);
        let e = t.step(FILL).unwrap_err();
        assert!(is_cancel(&e));
        // the model run stops before loading
        let mut step = t.model_step(FILL);
        assert!(!step(segment::Step::Load));
        assert!(is_cancel(segment::CANCELLED));
        assert!(!is_cancel("model file missing"));
    }

    #[test]
    fn model_steps_are_named() {
        let path = PathBuf::from("/private/tmp/graineo-progress-test/d.jpg");
        let t = begin(Tool::Click, &path, DEVELOP);
        {
            let mut step = t.model_step(ENCODE);
            assert!(step(segment::Step::Load));
            assert_eq!(current(&path, &[Tool::Click]).unwrap().step, LOAD);
            assert!(step(segment::Step::Run));
        }
        assert_eq!(current(&path, &[Tool::Click]).unwrap().step, ENCODE);
    }
}
