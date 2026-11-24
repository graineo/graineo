//! Neural denoising in the noise part of the Edit panel (`rawdev::ai_denoise`): a switch, the strength, and one fixed
//! status line (what is computing, how far, how long still). Off by default; usable once the module is installed
//! (otherwise a button opens Settings → Modules). The computation runs in the background for the photo on screen
//! ([`step`]); the view is rendered again when it is there.

use std::sync::atomic::{AtomicU64, Ordering};

use develop::Settings;

use crate::lr;

/// Hint of the switch (selects the help card).
const HINT: &str = "KI-Rauschminderung auf den RAW-Daten";
/// Hint of the strength slider.
const STRENGTH_HINT: &str = "Anteil des KI-entrauschten Bildes";

/// Is the stage offered in the panel? Yes, unless `GRAINEO_AI_DENOISE=0`.
pub fn offered() -> bool {
    crate::env::var_os("GRAINEO_AI_DENOISE").is_none_or(|v| v != "0")
}

/// Switch, strength and status line (fixed heights, nothing jumps). Only for RAWs graineo decodes itself.
pub fn section(ui: &mut egui::Ui, s: &mut Settings, doc: Option<&rawdev::RawDoc>) {
    let Some(doc) = doc.filter(|d| rawdev::ai_denoise::applicable(d)) else { return };
    if !offered() {
        return;
    }
    let installed = rawdev::ai_denoise::module_ready();
    let running = rawdev::ai_denoise::status_for(&doc.path);
    let p = rawdev::ai_denoise::percent(s);
    let state = match (&running, installed) {
        (Some(st), _) => crate::trf!("rechnet {p} %", p = percent_done(st)),
        (None, false) => crate::tr("Modul fehlt").to_string(),
        (None, true) => doc.info.exif.iso.map(|i| format!("ISO {i}")).unwrap_or_default(),
    };
    let mut on = p > 0;
    ui.add_enabled_ui(installed || on, |ui| {
        if lr::choice_state(ui, "KI-Entrauschen", &state, &mut on, &[(false, "Aus"), (true, "An")], HINT) {
            s.raw.ai_denoise = if on { rawdev::ai_denoise::DEFAULT_STRENGTH } else { 0.0 };
        }
    });
    if installed {
        ui.add_enabled_ui(on, |ui| {
            let mut v = p as f64;
            if lr::slider_hint(ui, "Stärke", &mut v, 0.0..=100.0, (rawdev::ai_denoise::DEFAULT_STRENGTH * 100.0) as f64, 1.0, STRENGTH_HINT) {
                // 0 % would switch the stage off: the switch does that
                s.raw.ai_denoise = (v as f32 / 100.0).max(0.01);
            }
        });
    } else {
        let r = ui.button(crate::tr("Modul laden …"));
        lr::hover_tip(ui, &r, &crate::tr("Lädt das Modell RawNIND (GPL-3.0, 58 MB) in Einstellungen → Module"));
        if r.clicked() {
            crate::prefs::open_section("Module");
        }
    }
    // one status line of fixed height: computing (with the time still to go), an error, or nothing
    let note = match (&running, rawdev::ai_denoise::error_for(&doc.path)) {
        (Some(st), _) => {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            running_line(st)
        }
        (None, Some(e)) if on => crate::trf!("KI-Entrauschen nicht möglich: {e}", e = e),
        _ => just_done(ui, &doc.path).unwrap_or_default(),
    };
    ui.allocate_ui(egui::vec2(ui.available_width(), 30.0), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(note).small().color(lr::TEXT_DIM())).wrap());
    });
}

/// Sigma photos: the same switch and strength, after Sigma's noise reduction (`develop::ai_denoise`). The base with the
/// stage is computed like any Sigma base (the view shows the previous one until it is there); the status line shows the
/// network's progress. Only with Sigma's noise reduction on and in colour.
pub fn section_sigma(ui: &mut egui::Ui, s: &mut Settings, path: &std::path::Path) {
    if !offered() {
        return;
    }
    let installed = rawdev::ai_denoise::module_ready();
    let possible = s.denoise && !s.monochrome;
    let running = rawdev::ai_denoise::status_for(path);
    let p = rawdev::ai_denoise::percent(s);
    let state = match (&running, installed) {
        (Some(st), _) => crate::trf!("rechnet {p} %", p = percent_done(st)),
        (None, false) => crate::tr("Modul fehlt").to_string(),
        (None, true) => String::new(),
    };
    let mut on = p > 0;
    let row = ui.add_enabled_ui((installed || on) && possible, |ui| {
        if lr::choice_state(ui, "KI-Entrauschen", &state, &mut on, &[(false, "Aus"), (true, "An")], HINT) {
            s.raw.ai_denoise = if on { rawdev::ai_denoise::DEFAULT_STRENGTH } else { 0.0 };
        }
    });
    if !possible {
        // greyed out: say why (QA 11)
        let r = ui.interact(row.response.rect, ui.id().with("ai-denoise-locked"), egui::Sense::hover());
        lr::hover_tip(ui, &r, crate::tr(if s.monochrome { "KI-Entrauschen gibt es nur in Farbe." } else { "KI-Entrauschen wirkt nur mit eingeschalteter Rauschminderung." }));
    }
    if installed {
        ui.add_enabled_ui(on && possible, |ui| {
            let mut v = p as f64;
            if lr::slider_hint(ui, "Stärke", &mut v, 0.0..=100.0, (rawdev::ai_denoise::DEFAULT_STRENGTH * 100.0) as f64, 1.0, STRENGTH_HINT) {
                s.raw.ai_denoise = (v as f32 / 100.0).max(0.01);
            }
        });
    } else {
        let r = ui.button(crate::tr("Modul laden …"));
        lr::hover_tip(ui, &r, &crate::tr("Lädt das Modell RawNIND (GPL-3.0, 58 MB) in Einstellungen → Module"));
        if r.clicked() {
            crate::prefs::open_section("Module");
        }
    }
    let note = match &running {
        Some(st) => {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            running_line(st)
        }
        None if on && !possible => crate::tr("Wirkt nur mit Foveon-Detail und in Farbe").to_string(),
        None => just_done(ui, path).unwrap_or_default(),
    };
    ui.allocate_ui(egui::vec2(ui.available_width(), 30.0), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(note).small().color(lr::TEXT_DIM())).wrap());
    });
}

/// "Fertig nach 18 s" for a few seconds after the computation of this photo ended (the title bar shows it too, but a
/// later message there may replace it).
fn just_done(ui: &egui::Ui, path: &std::path::Path) -> Option<String> {
    let f = rawdev::ai_denoise::last_finished().filter(|f| f.complete && f.path == path)?;
    let left = DONE_SHOWN - f.at.elapsed().as_secs_f32();
    if left <= 0.0 {
        return None;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(left));
    Some(crate::trf!("Fertig nach {s} s", s = f.seconds.round().max(1.0) as u32))
}

/// Seconds the panel says "Fertig nach … s".
const DONE_SHOWN: f32 = 8.0;

/// Percent done, moving on during a long tile (`rawdev::ai_denoise::Status::fraction`).
fn percent_done(st: &rawdev::ai_denoise::Status) -> u32 {
    (st.fraction() * 100.0).floor() as u32
}

/// "noch etwa 9 s" / "noch etwa 2 min" (empty while the speed is not known yet).
fn remaining(st: &rawdev::ai_denoise::Status) -> Option<String> {
    let r = st.remaining_secs()?.ceil().max(1.0) as u32;
    Some(if r >= 90 { crate::trf!("noch etwa {m} min", m = r.div_ceil(60)) } else { crate::trf!("noch etwa {s} s", s = r) })
}

/// The status line of the panel while it computes.
fn running_line(st: &rawdev::ai_denoise::Status) -> String {
    match remaining(st) {
        Some(r) => crate::trf!("Rechnet einmal in voller Auflösung: {p} %, {r} – die Vorschau zeigt das Foto bis dahin ohne", p = percent_done(st), r = r),
        None => crate::trf!("Rechnet einmal in voller Auflösung: {p} % – die Vorschau zeigt das Foto bis dahin ohne", p = percent_done(st)),
    }
}

/// The line in the title bar's status while the network computes (also with the panel closed or scrolled away):
/// "KI-Entrauschen 45 % · noch etwa 9 s". The status spins and is drawn again while it runs.
pub fn activity(current: Option<&std::path::Path>) -> Option<String> {
    let st = rawdev::ai_denoise::status(current)?;
    Some(match remaining(&st) {
        Some(r) => crate::trf!("KI-Entrauschen {p} % · {r}", p = percent_done(&st), r = r),
        None => crate::trf!("KI-Entrauschen {p} %", p = percent_done(&st)),
    })
}

/// What one step of the develop view brings: render again (a computation of the photo on screen is there), and the
/// short success for the title bar when one finished.
pub struct Step {
    pub rerender: bool,
    pub done: Option<String>,
}

/// One step of the develop view: start the computation for the photo on screen when it is wanted; tell whether one
/// finished since the last call (then the view is rendered again, and "fertig" shows in the title bar). A
/// computation cancelled by switching photos says nothing.
pub fn step(doc: Option<&std::sync::Arc<rawdev::RawDoc>>, s: &Settings) -> Step {
    static SEEN: AtomicU64 = AtomicU64::new(0);
    static SEEN_DONE: AtomicU64 = AtomicU64::new(0);
    if let Some(d) = doc {
        if offered() {
            rawdev::ai_denoise::prepare(d, s);
        }
    }
    let g = rawdev::ai_denoise::generation();
    let rerender = SEEN.swap(g, Ordering::Relaxed) != g;
    let done = rawdev::ai_denoise::last_finished().filter(|f| SEEN_DONE.swap(f.seq, Ordering::Relaxed) != f.seq && f.complete).map(|f| crate::trf!("KI-Entrauschen fertig · {s} s", s = f.seconds.round().max(1.0) as u32));
    Step { rerender, done }
}
