//! CAMF inspector: view Sigma's calibration data of any X3F file – texts, property lists,
//! matrices as tables and maps (e.g. spatial gain) as heatmaps.

use std::sync::Arc;

use eframe::egui;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use x3f::{Camf, CamfEntry, CamfMatrix};

use crate::lr;

#[derive(Default)]
pub struct CamfInspector {
    pub open: bool,
    /// Image index that `camf` belongs to
    pub index: Option<usize>,
    pub camf: Option<Arc<Camf>>,
    pub error: Option<String>,
    filter: String,
    selected: Option<String>,
    /// Plane for multi-dimensional maps (outer dimensions combined)
    slice: usize,
    tex: Option<(String, usize, TextureHandle, f64, f64)>,
}

/// Short explanations of known entries.
fn explain(name: &str) -> Option<&'static str> {
    let rules: [(&str, &str); 26] = [
        ("SpatialGain", crate::tr("Flat-Field-Karte: Verstärkung je Bildort und Schicht gegen Randabfall und Farbverschiebung, je Blende/Fokus (INF = unendlich, MOD = Nahgrenze). Wird zwischen den Tabellen interpoliert.")),
        ("ColorShadingFactor", crate::tr("Radiale Feinkorrektur von Rot und Blau je Weißabgleich: R·(1 + a·r⁴ + b·r²), B·(1 + c·r⁴ + d·r²).")),
        ("WBGain", crate::tr("Kanalverstärkungen dieser Weißabgleich-Vorgabe (unten/Mitte/oben).")),
        ("CCMatrix", crate::tr("Farbmatrix dieser Weißabgleich-Vorgabe: weißabgeglichene Schichtwerte → Ausgabefarbe.")),
        ("CMCM_", crate::tr("Farbmodus-Kompensation: Matrix, mit der Sigma einen Farbmodus (Vivid, Portrait …) aus Standard ableitet.")),
        ("CMCC_", crate::tr("Kontrastkorrektur des Farbmodus (wird zum Kontrastregler addiert).")),
        ("TCColorModeSettings", crate::tr("Tonkurven-Parameter je Farbmodus (S-Kurve: Skala, Start, Ende, Steilheit, Knickpunkt).")),
        ("DarkShield", crate::tr("Abgedeckte Sensorbereiche: liefern den Schwarzwert (Dunkelrauschen) je Schicht.")),
        ("ActiveImageArea", crate::tr("Sensorbereich, der tatsächlich zum Bild gehört (links, oben, rechts, unten).")),
        ("BadPixels", crate::tr("Liste bekannter Defektpixel aus der Werkskalibrierung (werden ersetzt).")),
        ("BadCluster", crate::tr("Defekte Pixelgruppen (z. B. für das Kamera-JPEG).")),
        ("BadnessTable", crate::tr("Schwellen, ab wann ein Pixel als defekt gilt (je ISO-Stufe).")),
        ("ISONoiseSettings", crate::tr("Rauschmodell je ISO-Stufe: Parameter der Sigma-Rauschunterdrückung (Spalten = Stärken, Radien, Schwellen).")),
        ("DenoisingSlidersStep", crate::tr("Was Sigmas Rauschregler je Stufe bewirken (5 Stufen × 13 Parameter).")),
        ("ColorNoiseModel", crate::tr("Modell des Farbrauschens: wie stark das Rauschen je Schicht von der Helligkeit abhängt.")),
        ("ColorDQ", crate::tr("Stärke von Sigmas Farbrausch-Dämpfung (ColorDQ) je Schicht.")),
        ("TempGainFact", crate::tr("Korrektur der Schichtverstärkung nach Sensortemperatur.")),
        ("FNumberGainFact", crate::tr("Korrektur der Schichtverstärkung je Blende.")),
        ("SensorAdjustmentGainFact", crate::tr("Individuelle Verstärkungskorrektur dieses Sensors (Werkskalibrierung).")),
        ("ColumnOffsets", crate::tr("Spaltenversatz: korrigiert feine senkrechte Streifen des Ausleseverstärkers.")),
        ("LinLUTs", crate::tr("Linearisierungstabellen je Schicht (bei Merrill bereits in der Kamera angewandt).")),
        ("Highlight", crate::tr("Parameter der Lichter-Rekonstruktion (ab wann eine Schicht als gesättigt gilt, Überblendung).")),
        ("Despeckle", crate::tr("Parameter der Ausreißer-Entfernung (Despeckle).")),
        ("SatMap", crate::tr("Lauflängen-kodierte Karte gesättigter Pixel je Schicht (aus der Kamera).")),
        ("Histogram", crate::tr("Histogramm, das die Kamera für die Belichtungsanzeige berechnet hat.")),
        ("Capture", crate::tr("Aufnahmedaten, wie die Kamera sie für die Verarbeitung festhält.")),
    ];
    rules.iter().find(|(k, _)| name.contains(k)).map(|(_, v)| *v)
}

/// Color scale from dark violet through red/orange to light yellow (similar to "inferno").
fn heat(t: f32) -> Color32 {
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.0, 0.0, 0.02]),
        (0.2, [0.2, 0.04, 0.38]),
        (0.45, [0.6, 0.15, 0.4]),
        (0.65, [0.89, 0.35, 0.18]),
        (0.85, [0.98, 0.65, 0.04]),
        (1.0, [0.99, 1.0, 0.64]),
    ];
    let t = t.clamp(0.0, 1.0);
    let i = STOPS.iter().position(|s| s.0 >= t).unwrap_or(STOPS.len() - 1).max(1);
    let (a, b) = (STOPS[i - 1], STOPS[i]);
    let u = (t - a.0) / (b.0 - a.0).max(1e-6);
    let c: [u8; 3] = std::array::from_fn(|k| ((a.1[k] + (b.1[k] - a.1[k]) * u) * 255.0) as u8);
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Can the matrix be shown as a map? (at least two dimensions, both inner ones ≥ 4)
fn is_map(m: &CamfMatrix) -> bool {
    let s = m.shape();
    s.len() >= 2 && s[s.len() - 1] >= 4 && s[s.len() - 2] >= 4
}

impl CamfInspector {
    pub fn select(&mut self, name: &str) {
        self.selected = Some(name.to_string());
        self.slice = 0;
    }

    pub fn show(&mut self, ctx: &egui::Context, file_name: &str) {
        let mut open = self.open;
        egui::Window::new(crate::trf!("CAMF-Inspektor – {file_name}", file_name = file_name))
            .open(&mut open)
            .default_size([900.0, 600.0])
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(e) = &self.error {
                    ui.colored_label(Color32::LIGHT_RED, crate::menu::i18n::tr_msg(e).into_owned());
                    return;
                }
                let Some(camf) = self.camf.clone() else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(crate::tr("CAMF wird gelesen …"));
                    });
                    return;
                };
                ui.label(
                    egui::RichText::new(crate::trf!("{a} Einträge – Sigmas Kalibrier- und Aufnahmedaten in dieser Datei. Karten erscheinen als Heatmap.", a = camf.order.len()))
                    .small()
                    .color(lr::TEXT_DIM()),
                );
                egui::Panel::left("camf-list").resizable(true).default_size(280.0).show(ui, |ui| {
                    crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut self.filter).hint_text(crate::tr("Suchen (z. B. SpatialGain, WB, Noise) …")).desired_width(f32::INFINITY));
                    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                        let f = self.filter.to_lowercase();
                        for name in camf.order.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)) {
                            let kind = match camf.entries.get(name) {
                                Some(CamfEntry::Text(_)) => "T",
                                Some(CamfEntry::Properties(_)) => "≡",
                                Some(CamfEntry::Matrix(m)) if is_map(m) => "▦",
                                Some(CamfEntry::Matrix(_)) => "#",
                                None => "?",
                            };
                            let active = self.selected.as_deref() == Some(name.as_str());
                            if lr::list_item(ui, &format!("{kind}  {name}"), active).clicked() {
                                self.selected = Some(name.clone());
                                self.slice = 0;
                            }
                        }
                    });
                });
                egui::ScrollArea::vertical().id_salt("camf-detail").auto_shrink([false; 2]).show(ui, |ui| {
                    let Some(name) = self.selected.clone() else {
                        ui.label(crate::tr("Links einen Eintrag wählen."));
                        return;
                    };
                    ui.heading(&name);
                    if let Some(e) = explain(&name) {
                        ui.label(egui::RichText::new(e).color(lr::TEXT_DIM()));
                    }
                    ui.add_space(6.0);
                    match camf.entries.get(&name) {
                        Some(CamfEntry::Text(t)) => {
                            ui.monospace(t);
                        }
                        Some(CamfEntry::Properties(p)) => {
                            egui::Grid::new("camf-props").striped(true).show(ui, |ui| {
                                for (k, v) in p {
                                    ui.strong(k);
                                    if camf.entries.contains_key(v) {
                                        // not ui.link: it shows the pointing hand (project: arrow pointer)
                                        if lr::link_text(ui, egui::RichText::new(v), ui.visuals().hyperlink_color).clicked() {
                                            self.selected = Some(v.clone());
                                            self.slice = 0;
                                        }
                                    } else {
                                        ui.label(v);
                                    }
                                    ui.end_row();
                                }
                            });
                        }
                        Some(CamfEntry::Matrix(m)) => self.matrix(ui, &name, m),
                        None => {}
                    }
                });
            });
        self.open = open;
    }

    fn matrix(&mut self, ui: &mut egui::Ui, name: &str, m: &CamfMatrix) {
        let shape = m.shape();
        let v = m.as_f64();
        let (min, max) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        let dims: Vec<String> = m.dims.iter().map(|(n, s)| format!("{n}={s}")).collect();
        ui.label(crate::trf!("Form [{a}] · {b} Werte · Typ {c} · min {d} · max {e} · Mittel {f}", a = dims.join(", "), b = v.len(), c = m.element_type, d = fmt(min), e = fmt(max), f = fmt(mean)));
        ui.add_space(4.0);
        if is_map(m) {
            let (h, w) = (shape[shape.len() - 2], shape[shape.len() - 1]);
            let slices = v.len() / (w * h);
            if slices > 1 {
                ui.horizontal(|ui| {
                    ui.label(crate::tr("Ebene"));
                    crate::lr::slider_add(ui, egui::Slider::new(&mut self.slice, 0..=slices - 1));
                });
            }
            let slice = self.slice.min(slices - 1);
            let data = &v[slice * w * h..(slice + 1) * w * h];
            let fresh = !matches!(&self.tex, Some((n, s, ..)) if n == name && *s == slice);
            if fresh {
                let (lo, hi) = data.iter().fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
                let span = (hi - lo).max(1e-12);
                let pixels: Vec<Color32> = data.iter().map(|&x| heat(((x - lo) / span) as f32)).collect();
                let img = ColorImage::new([w, h], pixels);
                let tex = ui.ctx().load_texture(format!("camf-{name}-{slice}"), img, TextureOptions::NEAREST);
                self.tex = Some((name.to_string(), slice, tex, lo, hi));
            }
            if let Some((_, _, tex, lo, hi)) = &self.tex {
                let avail = ui.available_width().min(760.0);
                let size = egui::vec2(avail, avail * h as f32 / w as f32);
                let resp = ui.add(egui::Image::new((tex.id(), size)).sense(egui::Sense::hover()));
                // Readout of the value under the pointer: a fixed line under the image (nothing jumps)
                let readout = resp.hover_pos().map(|p| {
                    let u = (p - resp.rect.min) / resp.rect.size();
                    let (x, y) = (((u.x * w as f32) as usize).min(w - 1), ((u.y * h as f32) as usize).min(h - 1));
                    crate::trf!("Zeile {y}, Spalte {x}: {a}", y = y, x = x, a = fmt(data[y * w + x]))
                });
                ui.label(egui::RichText::new(readout.unwrap_or_else(|| "–".to_string())).small().color(crate::lr::TEXT_DIM()));
                // Color scale
                let (rect, _) = ui.allocate_exact_size(egui::vec2(size.x, 10.0), egui::Sense::hover());
                let n = 64;
                for i in 0..n {
                    let r = egui::Rect::from_min_max(
                        egui::pos2(rect.left() + rect.width() * i as f32 / n as f32, rect.top()),
                        egui::pos2(rect.left() + rect.width() * (i + 1) as f32 / n as f32, rect.bottom()),
                    );
                    ui.painter().rect_filled(r, 0.0, heat(i as f32 / (n - 1) as f32));
                }
                ui.horizontal(|ui| {
                    ui.small(fmt(*lo));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.small(fmt(*hi)));
                });
            }
            return;
        }
        // Table (inner dimension as columns)
        let cols = shape.last().copied().unwrap_or(1).max(1);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            egui::Grid::new("camf-matrix").striped(true).spacing([14.0, 2.0]).show(ui, |ui| {
                for (i, x) in v.iter().take(2000).enumerate() {
                    ui.monospace(fmt(*x));
                    if (i + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
            });
        });
        if v.len() > 2000 {
            ui.small(crate::trf!("… {a} weitere Werte", a = v.len() - 2000));
        }
    }
}

fn fmt(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e9 {
        format!("{x:.0}")
    } else if x.abs() >= 100.0 {
        format!("{x:.2}")
    } else {
        format!("{x:.5}")
    }
}
