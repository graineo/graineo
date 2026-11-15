//! Section "Tonwert-Entzerrer" in Edit (category "Licht & Farbe"): one main slider "Ausgleich" (highlights are
//! never touched when brightening), below it the collapsed "Feineinstellung" with
//! nine zone sliders (−8 … 0 EV, added to the main slider), the two mask sliders and "Maske zeigen". Calculation in `develop::tone_eq`; acts for Sigma and all other cameras alike.
//!
//! The section sits in Edit and not in the tool rail: it is a group of sliders without its own interaction in the image
//! (sliders belong to a category, the rail is for tools).

use develop::tone_eq::{ToneEq, ZONES, ZONE_EV};
use develop::Settings;
use eframe::egui;

use crate::lr;

/// Title of the section (also the key of its help card and of the copy group).
pub const TITLE: &str = "Tonwert-Entzerrer";

/// Slider names of the zones (fixed, short: they are in EV, the same in every language).
const ZONE_LABELS: [&str; ZONES] = ["−8 EV", "−7 EV", "−6 EV", "−5 EV", "−4 EV", "−3 EV", "−2 EV", "−1 EV", "0 EV"];

/// Start of every zone hint (selects the help card of the zones, see `help.rs`).
pub const ZONE_HINT_START: &str = "Zone ";
/// Hint of the main slider (also selects the help card of the section, see `help.rs`).
pub const BALANCE_HINT: &str = "+ hellt Schatten auf, Lichter bleiben · − dunkelt Lichter ab, Schatten bleiben";

/// Short hint per zone (tooltip of the slider name; also selects the help card).
const ZONE_HINTS: [&str; ZONES] = [
    "Zone −8 EV: tiefste Schatten, fast Schwarz",
    "Zone −7 EV: sehr dunkle Schatten",
    "Zone −6 EV: dunkle Schatten",
    "Zone −5 EV: Schatten",
    "Zone −4 EV: dunkle Mitteltöne",
    "Zone −3 EV: Mitteltöne",
    "Zone −2 EV: helle Mitteltöne",
    "Zone −1 EV: Lichter",
    "Zone 0 EV: hellste Lichter, Weiß",
];

/// The section (the same for Sigma and other cameras).
pub fn section(ui: &mut egui::Ui, s: &mut Settings) {
    debug_assert_eq!(ZONE_EV.len(), ZONES);
    let d = ToneEq::default();
    test_preset(s);
    let changed = s.tone_eq != d || s.tone_eq_mask;
    let open = crate::env::var_os("GRAINEO_TONE_EQ_OPEN").is_some();
    let reset = lr::section_reset(ui, TITLE, open, changed, |ui| {
        let mut a = s.tone_eq.amount as f64;
        if lr::slider_hint(ui, "Ausgleich", &mut a, -2.0..=2.0, 0.0, 0.05, BALANCE_HINT) {
            s.tone_eq.amount = a as f32;
        }
        // Fine adjustment: adds to the main slider (its zones keep their own values, so a moved main slider never
        // overwrites them); collapsed by default, its place stays inside the section
        let fine = s.tone_eq.zones.iter().filter(|z| **z != 0.0).count()
            + usize::from(s.tone_eq.smoothing != d.smoothing)
            + usize::from(s.tone_eq.edges != d.edges)
            + usize::from(s.tone_eq_mask);
        let fine_open = crate::env::var_os("GRAINEO_TONE_EQ_FINE").is_some();
        lr::subsection(ui, "Feineinstellung", fine, fine_open, |ui| {
            lr::group_label(ui, "Zonen");
            for i in (0..ZONES).rev() {
                let mut v = s.tone_eq.zones[i] as f64;
                if lr::slider_hint(ui, ZONE_LABELS[i], &mut v, -2.0..=2.0, 0.0, 0.05, ZONE_HINTS[i]) {
                    s.tone_eq.zones[i] = v as f32;
                }
            }
            ui.add_space(4.0);
            if lr::group_label(ui, "Maske") {
                s.tone_eq.smoothing = d.smoothing;
                s.tone_eq.edges = d.edges;
            }
            let mut g = s.tone_eq.smoothing as f64;
            if lr::slider_hint(ui, "Glättung", &mut g, 1.0..=25.0, d.smoothing as f64, 0.5, "Größe der Maskenflächen in % der langen Bildkante") {
                s.tone_eq.smoothing = g as f32;
            }
            let mut k = s.tone_eq.edges as f64;
            if lr::slider_hint(ui, "Kantenschärfe", &mut k, 0.0..=1.0, d.edges as f64, 0.01, "Wie eng die Maske Kanten folgt – höher: weniger Lichthöfe") {
                s.tone_eq.edges = k as f32;
            }
            ui.add_space(2.0);
            let r = lr::toggle(ui, &mut s.tone_eq_mask, "Maske zeigen");
            lr::hover_tip(ui, &r, "Zonen als Graustufen: Schwarz = −8 EV, Weiß = 0 EV");
        });
    });
    if reset {
        s.tone_eq = d;
        s.tone_eq_mask = false;
    }
}

/// Test aid (own data folder only): `GRAINEO_TONE_EQ_TEST="amount=1.5;zones=…;mask=1"` sets the main slider, the
/// zones (from −8 EV to 0 EV) and "show mask" once, for screenshots.
fn test_preset(s: &mut Settings) {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || DONE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Ok(v) = crate::env::var("GRAINEO_TONE_EQ_TEST") else { return };
    DONE.store(true, std::sync::atomic::Ordering::Relaxed);
    for part in v.split(';') {
        match part.split_once('=') {
            Some(("zones", z)) => {
                for (i, x) in z.split(',').filter_map(|x| x.trim().parse::<f32>().ok()).take(ZONES).enumerate() {
                    s.tone_eq.zones[i] = x;
                }
            }
            Some(("mask", m)) => s.tone_eq_mask = m == "1",
            Some(("amount", a)) => s.tone_eq.amount = a.trim().parse().unwrap_or(0.0),
            _ => {}
        }
    }
}

/// The mask view is for the screen only: jobs that write files or thumbnails never show it.
pub fn without_mask(s: &mut Settings) {
    s.tone_eq_mask = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_zones() {
        for (i, ev) in ZONE_EV.iter().enumerate() {
            let n = ev.abs() as i32;
            assert!(ZONE_LABELS[i].starts_with(if n == 0 { "0" } else { "−" }));
            assert!(ZONE_LABELS[i].contains(&format!("{n} EV")));
            assert!(ZONE_HINTS[i].contains(ZONE_LABELS[i]));
        }
    }
}
