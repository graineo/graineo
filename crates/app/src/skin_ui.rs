//! "Skin tones" section of the Edit panel: hue and saturation inside a fixed skin-tone region
//! (see `develop::skin`). Global only: no picker, no region selection, no skin-type classification.

use develop::Settings;
use egui::Color32;

use crate::lr;

/// Section title (also the key of its help card and of the copy/paste group).
pub const TITLE: &str = "Hauttöne";

pub fn section(ui: &mut egui::Ui, s: &mut Settings, d: &Settings) {
    let changed = s.skin_hue != d.skin_hue || s.skin_sat != d.skin_sat;
    let reset = lr::section_reset(ui, TITLE, false, changed, |ui| {
        let mut h = s.skin_hue as f64;
        if lr::slider_gradient(
            ui,
            "Hautton",
            &mut h,
            -1.0..=1.0,
            0.0,
            0.01,
            crate::tr("Links rosiger, rechts goldener – nur Hauttöne"),
            [Color32::from_rgb(0xd8, 0x86, 0x8a), Color32::from_rgb(0xd8, 0xb0, 0x5e)],
        ) {
            s.skin_hue = h as f32;
        }
        let mut v = s.skin_sat as f64;
        if lr::slider_gradient(
            ui,
            "Hautsättigung",
            &mut v,
            -1.0..=1.0,
            0.0,
            0.01,
            crate::tr("Hauttöne blasser oder kräftiger"),
            [crate::lr::ui_rgb(0xa8, 0x9c, 0x94), Color32::from_rgb(0xd8, 0x96, 0x62)],
        ) {
            s.skin_sat = v as f32;
        }
    });
    if reset {
        s.skin_hue = d.skin_hue;
        s.skin_sat = d.skin_sat;
    }
}
