//! Sliders of the own highlight rebuild of Foveon photos (`develop::highlight_rebuild`): "Lichter-Farbe" and
//! "Lichter-Detail" in the colour-cast section, and the two switches in the preferences (Merrill on, Quattro off by default).

use develop::Settings;
use eframe::egui;
use std::path::Path;

use crate::lr;

/// Setting key of the former single switch "Lichter rekonstruieren (neu)" (until 1.5.0, default off); only read to
/// carry an explicit "on" over to both new switches.
pub const LEGACY_KEY: &str = "foveonHighlightRebuild";
/// Setting key of the switch for Merrill photos (default on).
pub const KEY_MERRILL: &str = "foveonHighlightRebuildMerrill";
/// Setting key of the switch for Quattro photos (dp, sd, sd H; default off: heavy on strongly overexposed photos).
pub const KEY_QUATTRO: &str = "foveonHighlightRebuildQuattro";
pub const DEFAULT_MERRILL: bool = true;
pub const DEFAULT_QUATTRO: bool = false;

/// Stored value of a switch: its own key, else an explicit "on" of the former single switch, else the default.
fn stored(v: &serde_json::Value, legacy: &serde_json::Value, default: bool) -> bool {
    v.as_bool().unwrap_or_else(|| if legacy.as_bool() == Some(true) { true } else { default })
}

pub fn merrill_on() -> bool {
    stored(&library::settings::value(KEY_MERRILL), &library::settings::value(LEGACY_KEY), DEFAULT_MERRILL)
}

pub fn quattro_on() -> bool {
    stored(&library::settings::value(KEY_QUATTRO), &library::settings::value(LEGACY_KEY), DEFAULT_QUATTRO)
}

/// At start: hand the stored switches to `develop` (`GRAINEO_FOVEON_HL` still wins for both).
pub fn apply_setting() {
    develop::highlight_rebuild::set_enabled_merrill(merrill_on());
    develop::highlight_rebuild::set_enabled_quattro(quattro_on());
}

/// Switch for Merrill in the preferences; turning it drops the cached preparations so open photos are developed again.
pub fn prefs_switch_merrill(ui: &mut egui::Ui, toggle: impl FnOnce(&mut egui::Ui, &mut bool) -> bool) {
    let mut on = merrill_on();
    if toggle(ui, &mut on) {
        library::settings::set(KEY_MERRILL, serde_json::json!(on));
        develop::highlight_rebuild::set_enabled_merrill(on);
        develop::quattro::forget_all();
    }
}

/// Switch for Quattro in the preferences (see [`prefs_switch_merrill`]).
pub fn prefs_switch_quattro(ui: &mut egui::Ui, toggle: impl FnOnce(&mut egui::Ui, &mut bool) -> bool) {
    let mut on = quattro_on();
    if toggle(ui, &mut on) {
        library::settings::set(KEY_QUATTRO, serde_json::json!(on));
        develop::highlight_rebuild::set_enabled_quattro(on);
        develop::quattro::forget_all();
    }
}

/// The two sliders; greyed out with the reason when they have no effect on this photo.
pub fn sliders(ui: &mut egui::Ui, s: &mut Settings, path: &Path) {
    use develop::quattro::HighlightState;
    let reason = if s.monochrome {
        Some(crate::tr("Nur für Farbe"))
    } else if develop::quattro::is_classic_cached(path) {
        Some(crate::tr("Nur für Merrill und Quattro"))
    } else if !develop::quattro::is_prepared_cached(path) {
        if !develop::highlight_rebuild::enabled_merrill() {
            Some(crate::tr("Lichterrekonstruktion für Merrill ist in den Einstellungen aus (Entwickeln → „Merrill: Lichter rekonstruieren“)"))
        } else if matches!(develop::highlight_post::weight(path), Some(None)) {
            Some(crate::tr("Dieses Foto hat keine rekonstruierbaren Lichter"))
        } else {
            // known rebuilt areas, or not yet developed in this session (a base from the disk cache): the first move
            // develops the photo once and learns its areas
            None
        }
    } else if !develop::highlight_rebuild::enabled_quattro() {
        Some(crate::tr("Lichterrekonstruktion für Quattro ist in den Einstellungen aus (Entwickeln → „Quattro: Lichter rekonstruieren“)"))
    } else {
        match develop::quattro::highlight_state(path) {
            Some(HighlightState::None) => Some(crate::tr("Dieses Foto hat keine rekonstruierbaren Lichter")),
            Some(HighlightState::Rebuilt { sliders: false }) => {
                Some(crate::tr("Bei diesem Foto (hohe ISO) ohne Regler, die Rekonstruktion wirkt automatisch"))
            }
            Some(HighlightState::Rebuilt { sliders: true }) => None,
            // unknown while the preparation runs; a base from the disk cache without a note (older cache) leaves the
            // sliders on: the first move prepares the photo and learns its state
            None if develop::quattro::is_preparing(path) => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                Some(crate::tr("Lichter werden rekonstruiert …"))
            }
            None => None,
        }
    };
    ui.add_space(4.0);
    ui.add_enabled_ui(reason.is_none(), |ui| {
        let mut c = s.highlight_colour as f64 * 100.0;
        if lr::slider_hint(ui, "Lichter-Farbe", &mut c, 0.0..=100.0, 50.0, 1.0, "Farbe der rekonstruierten Lichter: 0 neutral, 50 wie rekonstruiert, 100 kräftiger (nur deutliche Farben)") {
            s.highlight_colour = (c / 100.0) as f32;
        }
        let mut t = s.highlight_structure as f64 * 100.0;
        if lr::slider_hint(ui, "Lichter-Detail", &mut t, 0.0..=100.0, 50.0, 1.0, "Zeichnung in den rekonstruierten Lichtern: 0 glatt, 50 wie rekonstruiert, 100 dreifach") {
            s.highlight_structure = (t / 100.0) as f32;
        }
    });
    if let Some(r) = reason {
        ui.label(egui::RichText::new(r).small().weak());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn legacy_on_turns_both_on() {
        assert!(stored(&Value::Null, &Value::Null, DEFAULT_MERRILL));
        assert!(!stored(&Value::Null, &Value::Null, DEFAULT_QUATTRO));
        // the former switch explicitly on: both on
        assert!(stored(&Value::Null, &json!(true), DEFAULT_QUATTRO));
        // the former switch off (the old default): the new defaults
        assert!(stored(&Value::Null, &json!(false), DEFAULT_MERRILL));
        assert!(!stored(&Value::Null, &json!(false), DEFAULT_QUATTRO));
        // an own value wins
        assert!(!stored(&json!(false), &json!(true), DEFAULT_MERRILL));
    }
}
