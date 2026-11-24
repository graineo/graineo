//! Foveon detail: two band sliders (fine detail, structure) instead of the former switch, for every Foveon class.
//! Merrill: bands of the residual inside Sigma's noise reduction (`x3f_core` texture bands); Quattro and Classic:
//! the same bands on the noise-reduced base (`develop::foveon_texture`). The noise sliders act independently of them.

use crate::lr;
use develop::Settings;

/// Tooltip of the fine detail slider (also selects its help card, see `help::for_slider`).
pub const FINE_TIP: &str = "Feinste Foveon-Ebene: Poren, Hauttextur, Restrauschen. 100 = Sigmas Detail, weniger = glattere Haut";
/// Tooltip of the structure slider (also selects its help card).
pub const STRUCTURE_TIP: &str = "Gröbere Foveon-Ebene: Wimpern, Haare, Augen, Kanten. 100 = Sigmas Detail";

/// Translation key of the Merrill "Struktur" band: the plain key "Struktur" already names the texture slider
/// of the other cameras (en "Texture"), the band needs its own word in every other language.
pub const STRUCTURE_KEY: &str = "Struktur (Detailband)";

/// Translation key of the Merrill "Feindetail" band ("Feindetail" alone is a camera picture style).
pub const FINE_KEY: &str = "Feindetail (Detailband)";

/// Displayed name of the Merrill fine detail band ("Feindetail" in German).
pub fn fine_label() -> &'static str {
    crate::menu::i18n::german_or("Feindetail", crate::tr(FINE_KEY))
}

/// Displayed name of the Merrill structure band ("Struktur" in German).
pub fn structure_label() -> &'static str {
    crate::menu::i18n::german_or("Struktur", crate::tr(STRUCTURE_KEY))
}

/// Draws the two band sliders for a Foveon photo. Returns `false` where the former switch is shown instead: photos
/// stored with the switch off (they keep their look until it is switched on again).
pub fn sliders(ui: &mut egui::Ui, s: &mut Settings) -> bool {
    if !s.denoise {
        return false;
    }
    let row = ui.add_enabled_ui(!s.monochrome, |ui| {
        let mut fine = f64::from(s.detail_fine) * 100.0;
        let mut structure = f64::from(s.detail_structure) * 100.0;
        let mut changed = false;
        if lr::slider_hint(ui, fine_label(), &mut fine, 0.0..=200.0, 100.0, 5.0, FINE_TIP) {
            s.detail_fine = (fine / 100.0) as f32;
            changed = true;
        }
        if lr::slider_hint(ui, structure_label(), &mut structure, 0.0..=200.0, 100.0, 5.0, STRUCTURE_TIP) {
            s.detail_structure = (structure / 100.0) as f32;
            changed = true;
        }
        changed
    });
    if s.monochrome {
        let r = ui.interact(row.response.rect, ui.id().with("detail-mono"), egui::Sense::hover());
        lr::hover_tip(ui, &r, "Wirkt nur in Farbe");
    }
    true
}
