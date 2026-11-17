//! What the computing jobs are doing right now, for the tooltip of the turning spinner ("Foveon-Basis ·
//! Rauschminderung (Grafikkarte) · seit 4 s"). A job registers its kind when it starts and names its stage at a few
//! points; that is one short lock of a list of at most a handful of entries – nothing in the computing itself.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use eframe::egui;

struct Entry {
    id: u64,
    kind: &'static str,
    stage: Option<&'static str>,
    since: Instant,
}

static ACTIVE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);
/// ⌃⌥I: show the step next to the spinner all the time, not only on hover (to watch it without moving the mouse)
static ALWAYS: AtomicBool = AtomicBool::new(false);

/// Switches between "always show what is being computed" and "only on hover".
pub fn toggle_always() {
    ALWAYS.fetch_xor(true, Ordering::Relaxed);
}

thread_local! {
    /// the job registered by this thread (so that code deep in a job can name its stage without passing the [`Step`])
    static HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn list() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A registered job; leaves the list when dropped (also when the job panics).
pub struct Step(u64);

/// A job starts: `kind` is its translated name, `stage` the first stage if already known.
pub fn enter(kind: &'static str, stage: Option<&'static str>) -> Step {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    list().push(Entry { id, kind, stage, since: Instant::now() });
    HERE.with(|h| h.set(id));
    Step(id)
}

impl Step {
    /// The job moves on to another stage (translated text); the time "seit …" counts from the start of the job.
    pub fn stage(&self, stage: &'static str) {
        if let Some(e) = list().iter_mut().find(|e| e.id == self.0) {
            e.stage = Some(stage);
        }
    }
}

/// The job running on this thread moves on to another stage (nothing happens outside a registered job).
pub fn stage_here(stage: &'static str) {
    let id = HERE.with(|h| h.get());
    if id != 0 {
        if let Some(e) = list().iter_mut().find(|e| e.id == id) {
            e.stage = Some(stage);
        }
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        HERE.with(|h| {
            if h.get() == self.0 {
                h.set(0)
            }
        });
        list().retain(|e| e.id != self.0);
    }
}

/// Text for the tooltip: the job started last, "Art · Stufe · seit 4 s", "+n" when more are running.
pub fn describe() -> Option<String> {
    let v = list();
    let Some(e) = v.last() else {
        // nothing computes: in the operating mode "Saving" the exact view waits for the dwell – say why
        return waiting();
    };
    // what is being done: the stage if known, otherwise the kind of job
    let mut t = e.stage.unwrap_or(e.kind).to_string();
    let secs = e.since.elapsed().as_secs();
    if secs >= 1 {
        t = format!("{t} · {}", crate::trf!("seit {n} s", n = secs));
    }
    if v.len() > 1 {
        t = format!("{t}  +{}", v.len() - 1);
    }
    Some(t)
}

/// Why nothing computes while the spinner shows: the exact view waits for the dwell of the operating mode "Saving".
static WAITING: Mutex<Option<String>> = Mutex::new(None);

/// The exact view of the photo shown waits (text for the tooltip) or not (`None`).
pub fn set_waiting(text: Option<String>) {
    *WAITING.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

fn waiting() -> Option<String> {
    WAITING.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Tooltip of the spinner in the image area (`area`: where the spinner sits, top left of the image).
pub fn spinner_tip(ui: &egui::Ui, area: egui::Rect) {
    let rect = egui::Rect::from_center_size(area.left_top() + egui::vec2(18.0, 18.0), egui::vec2(30.0, 30.0));
    let resp = ui.interact(rect, egui::Id::new("kreisel-tip"), egui::Sense::hover());
    if let Some(text) = describe() {
        let test_hook = crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var_os("GRAINEO_SPINNER_TIP").is_some();
        if ALWAYS.load(Ordering::Relaxed) || test_hook {
            // shown without waiting for the pointer (⌃⌥I, or the test hook for screenshots)
            let mut tip = egui::Tooltip::always_open(ui.ctx().clone(), resp.layer_id, resp.id, resp.rect);
            tip.popup = tip.popup.align(egui::RectAlign::BOTTOM_START);
            tip.show(|ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(egui::RichText::new(&text).size(crate::lr::UI_SIZE - 0.5).color(crate::lr::TEXT))
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        } else {
            crate::lr::hover_tip(ui, &resp, &text);
            if resp.hovered() {
                // the "seit … s" counts on
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_and_cleanup() {
        let a = enter("Test-Art", None);
        let find = |kind: &str| list().iter().find(|e| e.kind == kind).map(|e| e.stage);
        assert_eq!(find("Test-Art"), Some(None));
        stage_here("Eins");
        assert_eq!(find("Test-Art"), Some(Some("Eins")));
        a.stage("Zwei");
        assert_eq!(find("Test-Art"), Some(Some("Zwei")));
        assert!(describe().is_some());
        drop(a);
        assert_eq!(find("Test-Art"), None);
        stage_here("Nichts"); // outside of a job: no effect, no panic
    }
}
