//! Fixed footer of the editing trays (Edit, Film & Look, Crop, Retouch, Presets, Merge): one button high, below the
//! scroll area, so it never scrolls away. Left "Kopieren" and "Einfügen" (development settings), right the icon
//! "Reset all settings" with its "▾" menu (camera defaults). The same for Sigma photos and every other format.

use std::sync::atomic::Ordering;

use eframe::egui;

use crate::workspace::ToolTab;
use crate::{lr, App};

/// Height of the footer including its separator line and spacing.
pub const HEIGHT: f32 = 42.0;

/// Does tray `t` get the footer? Trays where the photo is edited; not Info, Masks, Versions or Sensor.
pub fn shown_in(t: ToolTab) -> bool {
    matches!(t, ToolTab::Edit | ToolTab::FilmLook | ToolTab::Crop | ToolTab::Retouch | ToolTab::Presets | ToolTab::Merge)
}

impl App {
    /// The footer; `enabled`: the shown photo is the active one (else greyed out like the sliders).
    pub(crate) fn edit_footer(&mut self, ui: &mut egui::Ui, enabled: bool) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        ui.separator();
        let mut copy = false;
        let mut paste = false;
        let mut reset = false;
        let mut reset_camera = false;
        let has_clip = self.clipboard.is_some();
        let camera = self.current.as_ref().and_then(|c| crate::camera_defaults::camera_of(&c.loaded));
        let has_default = camera.as_deref().and_then(crate::camera_defaults::find).is_some();
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let k = lr::secondary_button(ui, None, "Kopieren");
                lr::hover_tip(ui, &k, &crate::shortcuts::display("copy_settings"));
                copy = k.clicked();
                let e = ui.add_enabled_ui(has_clip, |ui| lr::secondary_button(ui, None, "Einfügen")).inner;
                lr::hover_tip(ui, &e, &crate::shortcuts::display("paste_settings"));
                paste = e.clicked() && has_clip;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // "▾": camera default entries (camera_defaults.rs)
                    let more = ui.add_sized([18.0, 24.0], egui::Button::new("▾").frame(false));
                    lr::hover_tip(ui, &more, crate::tr("Weitere Möglichkeiten zum Zurücksetzen"));
                    reset = lr::icon_button_hint(ui, lr::Icon::ArrowArcLeft, false, &lr::with_shortcut("Alle Einstellungen zurücksetzen", "reset_all"));
                    // test hooks for screenshots (own data folder only): GRAINEO_DIALOG=reset-menu shows the menu open,
                    // GRAINEO_DIALOG=camera-default opens "Set as default for …" once
                    let hook = if crate::env::var_os("GRAINEO_DATA_DIR").is_some() { crate::env::var("GRAINEO_DIALOG").ok() } else { None };
                    let mut popup = egui::Popup::menu(&more).align(egui::RectAlign::TOP_END);
                    if hook.as_deref() == Some("reset-menu") {
                        popup = popup.open(true);
                    }
                    if hook.as_deref() == Some("camera-default") {
                        static HOOK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                        if let Some(c) = camera.as_deref().filter(|_| !HOOK.swap(true, Ordering::Relaxed)) {
                            if let Some(cur) = self.current.as_ref() {
                                crate::camera_defaults::open_set(c, &self.entries[index].settings, &cur.loaded);
                            }
                        }
                    }
                    popup.show(|ui| {
                        ui.set_min_width(260.0);
                        if ui.add_enabled(has_default, egui::Button::new(crate::tr("Auf Kamera-Standard zurücksetzen"))).clicked() {
                            reset_camera = true;
                        }
                        ui.separator();
                        let set_text = match &camera {
                            Some(c) => crate::trf!("Als Standard für {c} festlegen …", c = c),
                            None => crate::tr("Als Kamera-Standard festlegen …").to_string(),
                        };
                        if ui.add_enabled(camera.is_some(), egui::Button::new(set_text)).clicked() {
                            if let (Some(c), Some(cur)) = (&camera, self.current.as_ref()) {
                                crate::camera_defaults::open_set(c, &self.entries[index].settings, &cur.loaded);
                            }
                        }
                        if ui.button(crate::tr("Kamera-Standards verwalten …")).clicked() {
                            crate::prefs::open_section("Kamera-Standards");
                        }
                    });
                });
            });
        });
        if reset_camera {
            self.reset_to_camera_default();
        }
        if copy {
            self.copy_dialog = true;
        }
        if paste {
            self.pending_paste = true;
        }
        if reset {
            // one undoable command (⌘Z also in the grid), see settings_group.rs
            crate::settings_group::request_reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_only_in_editing_trays() {
        assert!(shown_in(ToolTab::Edit) && shown_in(ToolTab::FilmLook) && shown_in(ToolTab::Crop));
        assert!(!shown_in(ToolTab::Info) && !shown_in(ToolTab::Masks) && !shown_in(ToolTab::Versions) && !shown_in(ToolTab::Sensor));
    }
}
