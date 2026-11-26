//! Small chart, updated only from background responses.
use crate::{jobs::Job, lr, App};
use eframe::egui;
#[derive(Default)]
pub struct Curve {
    pub desired: Option<(String, f64)>,
    pub busy: bool,
    pub ready: Option<String>,
    pub data: Vec<develop::film::Series>,
    pub error: Option<String>,
}
impl App {
    pub(crate) fn darkroom_curve_panel(&mut self, ui: &mut egui::Ui, index: usize) {
        let f = self.entries[index].settings.film.clone();
        let key = serde_json::to_string(&f).unwrap_or_default();
        let now = self.now();
        let k = &mut self.darkroom.curves;
        if k.desired.as_ref().map(|x| &x.0) != Some(&key) {
            k.desired = Some((key.clone(), now));
        }
        if !k.busy && k.ready.as_ref() != Some(&key) && now - k.desired.as_ref().unwrap().1 >= 0.35
        {
            k.busy = true;
            self.worker.send(Job::ToneCurve {
                settings: f,
                key: key.clone(),
            });
        }
        crate::darkroom_extended::subgroup(ui, "Film → Papier → Abzug", |ui| crate::help::info(
                ui,
                "tone-curves",
                "Kurvendiagramm",
                &crate::help::FILM_CURVES,
            ));
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 130.), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 0., crate::lr::gray(18));
        let colors = [
            egui::Color32::from_rgb(180, 162, 115),
            crate::lr::gray(130),
            crate::lr::TEXT_STRONG(),
        ];
        for (j, c) in k.data.iter().enumerate() {
            let points = c
                .points
                .iter()
                .filter(|(x, y)| (-2.4..=2.4).contains(x) && y.is_finite())
                .map(|&(x, y)| {
                    egui::pos2(
                        r.left() + (x + 2.4) / 4.8 * r.width(),
                        r.bottom() - y.clamp(0., 1.) * r.height(),
                    )
                })
                .collect();
            p.add(egui::Shape::line(
                points,
                egui::Stroke::new(1.3, colors[j.min(2)]),
            ));
        }
        for (j, c) in k.data.iter().enumerate() {
            ui.label(egui::RichText::new(&c.name).small().color(colors[j.min(2)]));
        }
        if let Some(e) = &k.error {
            ui.label(crate::menu::i18n::tr_msg(e).into_owned());
        }
        if k.ready.as_ref() != Some(&key) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
}
