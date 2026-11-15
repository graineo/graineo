//! Darkroom, phase 3: dodge and burn. As in the lab, part of the print is held back and stays lighter
//! (dodging) or gets extra exposure and thus becomes darker (burning): with brush or
//! gradient, strength in stops (−2 … +2; positive = lighter, negative = darker).
//!
//! Approximation: these are the existing local corrections ([`masks`]); the mask acts as a local
//! exposure change on the fully developed image (after film and paper), not in the enlarger
//! before the paper curve. For moderate changes of ±1 stop this comes very close to real dodging;
//! at the ends of the paper curve (deepest black, brightest white) the paper curve is no longer involved.
//! The mask structure stays the same as in "Develop" (`settings.local`).

use crate::menu::i18n::DeComma as _;
use eframe::egui;

use crate::darkroom::header;
use crate::{lr, masks_ui, App};

/// State of the section.
pub struct Dodge {
    /// 0 = brush, 1 = gradient
    pub kind: usize,
    /// Strength of the next new mask in stops
    pub strength: f32,
}

impl Default for Dodge {
    fn default() -> Self {
        Dodge { kind: 0, strength: 0.5 }
    }
}

/// Label of the strength: direction and value.
pub fn strength_text(ev: f32) -> String {
    if ev.abs() < 0.005 {
        "0,00 EV".to_string()
    } else if ev > 0.0 {
        crate::trf!("Abwedeln +{a} EV", a = format!("{ev:.2}").de_comma())
    } else {
        crate::trf!("Nachbelichten −{a} EV", a = format!("{:.2}", -ev).de_comma())
    }
}

impl App {
    /// "Dodge / Burn" section in the bottom panel.
    pub(crate) fn darkroom_dodge_panel(&mut self, ui: &mut egui::Ui, index: usize) {
        let ctx = ui.ctx().clone();
        let mut b = masks_ui::editing_state(&ctx, index);
        let count = self.entries[index].settings.local.as_ref().map_or(0, |d| d.layers.len());
        let mut new_mask = false;
        ui.horizontal_wrapped(|ui| {
            lr::segmented_pad(ui, &mut self.darkroom.dodge.kind, &[(0, crate::tr("Pinsel")), (1, crate::tr("Verlauf"))], 9.0);
            if ui.button(crate::tr("NEUE MASKE")).clicked() {
                new_mask = true;
            }
        });
        if count > 0 {
            b.selected = b.selected.min(count - 1);
            let mut remove = false;
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("dk-abw-maske").selected_text(crate::trf!("Maske {a}", a = b.selected + 1)).width(90.0).show_ui(ui, |ui| {
                    for i in 0..count {
                        ui.selectable_value(&mut b.selected, i, crate::trf!("Maske {a}", a = i + 1));
                    }
                });
                let drawing = b.draw;
                if ui.add(egui::Button::new(crate::tr("ZEICHNEN")).selected(drawing)).clicked() {
                    b.draw = !drawing;
                }
                let k = b.contours;
                if ui.add(egui::Button::new(crate::tr("KONTUR")).selected(k)).clicked() {
                    b.contours = !k;
                }
                if ui.button(crate::tr("ENTFERNEN")).clicked() {
                    remove = true;
                }
            });
            if let Some(d) = self.entries[index].settings.local.as_mut() {
                if remove && b.selected < d.layers.len() {
                    d.layers.remove(b.selected);
                    b.selected = b.selected.saturating_sub(1);
                    b.draw = false;
                }
                if let Some(l) = d.layers.get_mut(b.selected) {
                    let mut t = l.adjustments.exposure as f64;
                    if lr::slider(ui, "Stärke", &mut t, -2.0..=2.0, 0.0, 0.05) {
                        l.adjustments.exposure = t as f32;
                    }
                    ui.label(egui::RichText::new(strength_text(l.adjustments.exposure)).small().color(lr::TEXT_DIM));
                }
            }
        } else {
            let mut t = self.darkroom.dodge.strength as f64;
            if lr::slider(ui, "Stärke", &mut t, -2.0..=2.0, 0.5, 0.05) {
                self.darkroom.dodge.strength = t as f32;
            }
            ui.label(egui::RichText::new(strength_text(self.darkroom.dodge.strength)).small().color(lr::TEXT_DIM));
        }
        if new_mask {
            let (kind, ev) = (self.darkroom.dodge.kind, self.darkroom.dodge.strength);
            if masks_ui::new_exposure_layer(&ctx, &mut self.entries[index].settings.local, index, kind, ev) {
                b = masks_ui::editing_state(&ctx, index);
            }
        }
        masks_ui::set_editing(&ctx, index, b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;
    use egui::{Pos2, Rect, Vec2};

    fn app_with_image(name: &str) -> (egui::Context, App) {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-dk-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(1800, 1200, image::Rgb([110, 80, 60])).save(folder.join("sample.png")).unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(folder);
        a.to_single_image();
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (ctx, a)
    }

    fn step_frame(ctx: &egui::Context, a: &mut App, clock: &mut f64) {
        *clock += 0.5;
        let mut input = egui::RawInput { time: Some(*clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
        a.raw_input_hook(ctx, &mut input);
        let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
    }

    /// A layer with +1 EV brightens the gradient area, −1 EV darkens it, the rest stays.
    #[test]
    fn exposure_layer_acts_as_local_exposure() {
        let (w, h) = (8usize, 8usize);
        let lin = vec![0.2f32; w * h * 3];
        for (ev, factor) in [(1.0f32, 2.0f32), (-1.0, 0.5)] {
            let ctx = egui::Context::default();
            let mut local = None;
            masks_ui::new_exposure_layer(&ctx, &mut local, 0, 1, ev);
            let mut doc = local.unwrap();
            // Gradient from the top (0) to the image centre (1): fully effective at the bottom
            doc.layers[0].mask = masks::Mask::Linear { start: [0.5, 0.0], end: [0.5, 0.5] };
            let img = masks::Image::new(w, h, &lin).unwrap();
            let out = masks::apply(img, &doc).unwrap();
            let bottom = out[((h - 1) * w) * 3];
            let top = out[0];
            assert!((bottom - 0.2 * factor).abs() < 0.01, "bottom {bottom} at {ev} EV");
            assert!((top - 0.2).abs() < 0.02, "top {top}");
        }
    }

    /// In the whole window: the section draws, a new mask is created and selected, removing cleans up.
    #[test]
    fn dodge_in_full_window() {
        if develop::film::films().is_empty() {
            return;
        }
        let (ctx, mut a) = app_with_image("abwedeln");
        let mut clock = 10.0;
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        step_frame(&ctx, &mut a, &mut clock);
        step_frame(&ctx, &mut a, &mut clock);
        let i = a.current.as_ref().unwrap().index;
        assert!(a.entries[i].settings.local.is_none());
        // like a click on "New mask" (gradient, burn)
        a.darkroom.dodge.kind = 1;
        a.darkroom.dodge.strength = -0.5;
        assert!(masks_ui::new_exposure_layer(&ctx, &mut a.entries[i].settings.local, i, a.darkroom.dodge.kind, a.darkroom.dodge.strength));
        step_frame(&ctx, &mut a, &mut clock);
        step_frame(&ctx, &mut a, &mut clock);
        let d = a.entries[i].settings.local.as_ref().unwrap();
        assert_eq!(d.layers.len(), 1);
        assert_eq!(d.layers[0].adjustments.exposure, -0.5);
        let b = masks_ui::editing_state(&ctx, i);
        assert!(b.draw && b.contours && b.selected == 0);
        // the same mask structure as in "Develop": it is kept after leaving
        a.leave_darkroom();
        step_frame(&ctx, &mut a, &mut clock);
        assert_eq!(a.entries[i].settings.local.as_ref().unwrap().layers.len(), 1);
    }

    #[test]
    fn strength_text_names_the_direction() {
        assert_eq!(strength_text(0.0), "0,00 EV");
        assert_eq!(strength_text(0.5), "Abwedeln +0,50 EV");
        assert_eq!(strength_text(-1.25), "Nachbelichten −1,25 EV");
    }

    #[test]
    fn new_layer_lightens_or_darkens_and_can_be_selected() {
        let ctx = egui::Context::default();
        let mut local = None;
        assert!(masks_ui::new_exposure_layer(&ctx, &mut local, 3, 0, 0.7));
        assert!(masks_ui::new_exposure_layer(&ctx, &mut local, 3, 1, -0.4));
        let d = local.as_ref().unwrap();
        assert_eq!(d.version, 2);
        assert_eq!(d.layers.len(), 2);
        assert_eq!(d.layers[0].adjustments.exposure, 0.7);
        assert_eq!(d.layers[1].adjustments.exposure, -0.4);
        assert!(matches!(d.layers[0].mask, masks::Mask::Brush { .. }));
        assert!(matches!(d.layers[1].mask, masks::Mask::Linear { .. }));
        // the layers are valid and the last created one is selected, drawing on
        assert!(d.layers.iter().all(|l| l.mask.validate().is_ok() && l.adjustments.validate().is_ok()));
        let b = masks_ui::editing_state(&ctx, 3);
        assert_eq!(b.selected, 1);
        assert!(b.draw && b.contours);
        // another image: its own state
        assert_eq!(masks_ui::editing_state(&ctx, 4), masks_ui::Editing::default());
        // limit of 64 layers
        for _ in 0..70 {
            masks_ui::new_exposure_layer(&ctx, &mut local, 3, 0, 0.1);
        }
        assert_eq!(local.unwrap().layers.len(), 64);
    }
}
