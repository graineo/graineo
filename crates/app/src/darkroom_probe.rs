//! Self-generated light points as a background preview, no file on disk.
use crate::{jobs::Job, lr, App};
use eframe::egui;
use std::hash::{Hash, Hasher};
#[derive(Default)]
pub struct Probe {
    pub an: bool,
    desired: Option<(u64, f64)>,
    busy: Option<u64>,
    ready: Option<u64>,
    tex: Option<egui::TextureHandle>,
    status: String,
}
impl Probe {
    pub fn outcome(
        &mut self,
        ctx: &egui::Context,
        key: u64,
        image: Result<develop::Rgb16, String>,
        seconds: f32,
    ) {
        if self.busy == Some(key) {
            self.busy = None;
        }
        if self.desired.map(|v| v.0) != Some(key) {
            return;
        }
        match image {
            Ok(im) => {
                self.tex = Some(ctx.load_texture(
                    "darkroom-light-points",
                    egui::ColorImage::from_rgb([im.width, im.height], &im.to_rgb8()),
                    egui::TextureOptions::NEAREST,
                ));
                self.ready = Some(key);
                self.status = format!("{seconds:.3} s");
            }
            Err(e) => {
                self.status = e;
                self.ready = Some(key);
            }
        }
    }
}
impl App {
    pub(crate) fn darkroom_light_points(&mut self, ctx: &egui::Context) {
        if !self.darkroom_active() || !self.darkroom.probe_light.an {
            return;
        }
        let Some(f) = self
            .current
            .as_ref()
            .and_then(|c| self.entries.get(c.index))
            .map(|e| e.settings.film.clone())
        else {
            return;
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(&f).unwrap_or_default().hash(&mut h);
        let key = h.finish();
        let now = self.now();
        let p = &mut self.darkroom.probe_light;
        if p.desired.map(|v| v.0) != Some(key) {
            p.desired = Some((key, now));
        }
        if p.busy.is_none() && p.ready != Some(key) && now - p.desired.unwrap().1 >= 0.35 {
            p.busy = Some(key);
            self.worker.send(Job::FilmProbe { settings: f, key });
        }
        let width = (ctx.content_rect().width() - 100.).clamp(320., 1200.);
        let response =
            egui::Modal::new(egui::Id::unique("darkroom-light-points")).show(ctx, |ui| {
                ui.set_width(width);
                lr::group_label(ui, "Probe · Lichtpunkte");
                let (labels, _) =
                    ui.allocate_exact_size(egui::vec2(width, 20.), egui::Sense::hover());
                for i in 0..9 {
                    ui.painter().text(
                        egui::pos2(
                            labels.left() + (i + 1) as f32 * width / 10.,
                            labels.center().y,
                        ),
                        egui::Align2::CENTER_CENTER,
                        format!("{:+} EV", i - 2),
                        egui::FontId::proportional(11.5),
                        lr::TEXT_DIM(),
                    );
                }
                let (r, _) =
                    ui.allocate_exact_size(egui::vec2(width, width * 0.5), egui::Sense::hover());
                if let Some(tex) = &p.tex {
                    ui.painter().image(
                        tex.id(),
                        r,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                        egui::Color32::WHITE,
                    );
                } else {
                    ui.painter().rect_filled(r, 0., egui::Color32::BLACK);
                }
                ui.label(if p.ready == Some(key) {
                    crate::menu::i18n::tr_msg(&p.status).into_owned()
                } else {
                    crate::tr("Wird berechnet …").to_string()
                });
                if ui.button(crate::tr("SCHLIESSEN")).clicked() {
                    p.an = false;
                }
            });
        if response.should_close() {
            self.darkroom.probe_light.an = false;
        }
        if self.darkroom.probe_light.ready != Some(key) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}
