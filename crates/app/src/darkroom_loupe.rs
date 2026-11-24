//! Format-true grain loupe, inspired by spektrafilm (Andrea Volpato).
use crate::{help, lr, App};
use eframe::egui;
pub const STEPS: [f32; 5] = [1., 2., 4., 8., 12.];
pub fn factor(zoom: f32, one: f32, film: bool, local: bool) -> usize {
    if !film || local || zoom / one.max(0.001) < 1.99 {
        1
    } else {
        (zoom / one.max(0.001)).ceil().clamp(2., 12.) as usize
    }
}
/// Clamp the visible source area exactly to real pixel boundaries (without extra pixels at the edge).
pub fn source_region(bounds: [f32; 4], size: [usize; 2]) -> [usize; 4] {
    let [fw, fh] = size;
    let x = (bounds[0].max(0.).floor() as usize).min(fw.saturating_sub(1));
    let y = (bounds[1].max(0.).floor() as usize).min(fh.saturating_sub(1));
    let right = (bounds[2].max(0.).ceil() as usize).clamp(x + 1, fw.max(1));
    let bottom = (bounds[3].max(0.).ceil() as usize).clamp(y + 1, fh.max(1));
    [x, y, right - x, bottom - y]
}
impl App {
    /// Grain loupe data for the darkroom toolbar: shown crop in millimetres
    /// on the film and a note when only pixels are shown. The zoom levels (100 – 1200 %) are in the
    /// regular zoom menu, the + and − keys step through the same levels.
    pub(crate) fn darkroom_loupe_info(&mut self, ui: &mut egui::Ui) {
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && self.image_rect.is_some() {
            let id = egui::Id::unique("grain-loupe-demo");
            if !ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false)) {
                if let Ok(z) = crate::env::var("GRAINEO_DARKROOM_TEST_ZOOM")
                    .unwrap_or_default()
                    .parse::<f32>()
                {
                    self.set_zoom(self.one_to_one * z.clamp(1., 12.));
                    ui.ctx().data_mut(|d| d.insert_temp(id, true));
                }
            }
        }
        help::info(ui, "grain-loupe", crate::tr("Körnungs-Lupe"), &help::FILM_LOUPE);
        if let Some(c) = self.current.as_ref() {
            let f = &self.entries[c.index].settings.film;
            let mm = f.format_mm * self.image_rect.map_or(0., |r| r.width()) / (self.fit_size.x * self.zoom).max(1.);
            let suffix = if develop::local::active(&self.entries[c.index].settings) {
                crate::tr(" · lokale Korrekturen: Pixelansicht")
            } else if c.detail_grain.as_ref().is_some_and(|(t, r, _)| c.detail_grain_scale > 1 && t.size()[0] <= r[2]) {
                crate::tr(" · Pixelansicht (Speichergrenze)")
            } else {
                ""
            };
            if self.zoom > 1.001 && self.darkroom.grain_check {
                ui.label(egui::RichText::new(crate::trf!("Ausschnitt {mm} mm{suffix}", mm = format!("{:.2}", mm), suffix = suffix)).small().color(lr::TEXT_DIM()));
            }
        }
    }
    pub(crate) fn darkroom_loupe_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.darkroom_active() {
            return;
        }
        let up = ["dk_zoom_plus", "dk_zoom_equals"]
            .iter()
            .any(|id| crate::shortcuts::pressed(ctx, id));
        let down = crate::shortcuts::pressed(ctx, "dk_zoom_minus");
        let z = self.zoom / self.one_to_one.max(0.001);
        if up {
            if let Some(s) = STEPS.iter().find(|s| **s > z * 1.01) {
                self.set_zoom(self.one_to_one * *s)
            }
        }
        if down {
            match STEPS.iter().rev().find(|s| **s < z * 0.99) {
                Some(s) => self.set_zoom(self.one_to_one * *s),
                None => {
                    self.set_zoom(1.);
                    self.pan = egui::Vec2::ZERO;
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_film_above_100_changes_detail() {
        assert_eq!(factor(3., 3., true, false), 1);
        assert_eq!(factor(6., 3., true, false), 2);
        assert_eq!(factor(36., 3., true, false), 12);
        assert_eq!(factor(36., 3., false, false), 1);
        // grain check off (`film` is false then): zoom and pan stay on the whole image
        assert_eq!(factor(36., 3., false, false), 1);
        assert_eq!(factor(36., 3., true, true), 1);
    }
}
#[cfg(test)]
mod window_tests {
    use super::*;
    #[test]
    fn zoom_stages_and_real_image_region() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir =
            std::env::temp_dir().join(format!("graineo-grain-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.png");
        image::RgbImage::from_pixel(128, 96, image::Rgb([110, 110, 110]))
            .save(&path)
            .unwrap();
        let doc = rawdev::RawDoc::open(&path, &|| true).unwrap().unwrap();
        let s = develop::Settings {
            film: develop::film::FilmSettings {
                active: true,
                stock: "ilford_hp5_plus".into(),
                halation: 0.,
                couplers: 0.,
                ..Default::default()
            },
            ..Default::default()
        };
        let old = doc.render_region(&s, [112, 80, 25, 25]).unwrap();
        let same = doc.render_grain_region(&s, [112, 80, 25, 25], 1).unwrap();
        assert_eq!(old.data, same.data);
        let up = doc.render_grain_region(&s, [112, 80, 25, 25], 4).unwrap();
        assert_eq!((up.width, up.height), (64, 64));
        assert_eq!(
            up.data,
            doc.render_grain_region(&s, [112, 80, 25, 25], 4)
                .unwrap()
                .data
        );
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.darkroom.active = true;
        for z in STEPS {
            a.set_zoom(a.one_to_one * z);
            assert!((a.zoom / a.one_to_one - z).abs() < 0.001);
        }
        let out = crate::ui_test::run(
            &ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000., 80.),
                )),
                ..Default::default()
            },
            |ui| a.darkroom_loupe_info(ui),
        );
        assert!(!out.shapes.is_empty());
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn visible_region_at_photo_edge_does_not_request_extra_pixel() {
        assert_eq!(
            source_region([0., 0., 6000., 4000.], [6000, 4000]),
            [0, 0, 6000, 4000]
        );
        assert_eq!(
            source_region([5990.3, 3990.2, 6001., 4001.], [6000, 4000]),
            [5990, 3990, 10, 10]
        );
        assert_eq!(
            source_region([-10., -5., 14.3, 8.5], [100, 100]),
            [0, 0, 15, 9]
        );
    }
}
