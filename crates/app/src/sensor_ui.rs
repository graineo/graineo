//! UI of the sensor view for Bayer/X-Trans RAWs: everything is operated in the "Sensor" tray of
//! the tool rail (overlays, display, values at the pointer, charts, key figures); the image
//! itself only carries the overlays and a small status label. Computation:
//! [`crate::sensor_raw`] (in the background).

use crate::menu::i18n::DeComma as _;
use eframe::egui;
use egui::Color32;

use crate::image_kind::{self, ImageKind};
use crate::help;
use crate::lr;
use crate::sensor_raw::{Overlay, SensorRaw, DEMOSAIC, VALUE_ROWS};
use crate::{App, Tab};

impl App {
    /// Sensor view of a Bayer/X-Trans RAW (not X3F, not JPEG)?
    pub(crate) fn sensor_raw_active(&self) -> bool {
        self.tab == Tab::Sensor && self.current.as_ref().is_some_and(|c| c.loaded.previews.foreign.is_some() && image_kind::of(&self.entries[c.index].path) == ImageKind::Raw)
    }

    /// Every frame: open or pick up analyses for the active RAW; release them outside the view.
    pub(crate) fn sensor_raw_update(&mut self, ctx: &egui::Context) {
        let path = self.sensor_raw_active().then(|| self.current.as_ref().map(|c| self.entries[c.index].path.clone())).flatten();
        if path.is_none() && self.sensor_raw.has_stage() {
            let max = self.one_to_one.max(1.0) * crate::zoom::maximum_factor(false, self.darkroom.active);
            if self.zoom > max { self.set_zoom(max); }
        }
        // A whole-image texture retained from Develop would defeat crop-only inspection at high zoom.
        if path.is_some() && self.zoom > self.one_to_one.max(1.0) * 4.0 {
            if let Some(c) = self.current.as_mut() {
                let [w,h] = c.loaded.full_size;
                if c.detail.as_ref().is_some_and(|(_, r, _)| r[0] == 0 && r[1] == 0 && r[2] >= w && r[3] >= h) {
                    c.detail = None;
                }
            }
        }
        // developed by the Apple engine because graineo cannot decode the file (Nikon HE): there is no sensor data to
        // read, say why instead of waiting
        let blocked = self.current.as_ref().and_then(|c| c.loaded.previews.foreign.as_ref()).and_then(|d| d.apple.as_ref()).and_then(|a| a.graineo_blocked.clone());
        let path = if blocked.is_some() { None } else { path };
        self.sensor_raw.for_stage(path.as_deref(), ctx);
        // the photo is developed (by Apple), only the sensor view is missing: no "not readable" in that case
        self.sensor_raw.apple_only = blocked.map(|why| {
            if why.contains(raw::NIKON_HE) {
                crate::tr("Sensoransicht für HE-Dateien nicht verfügbar – entwickelt über Apples RAW-Engine").to_string()
            } else {
                crate::trf!("Sensoransicht für diese Datei nicht verfügbar – entwickelt über Apples RAW-Engine ({a})", a = crate::menu::i18n::tr_msg(&why))
            }
        });
        if let Some(factor) = ctx.data_mut(|d| d.remove_temp::<f32>(egui::Id::new("sensor-zoom-request"))) {
            let target = self.one_to_one.max(1.0) * factor;
            let now = self.now();
            let frame = (self.open_generation.load(std::sync::atomic::Ordering::SeqCst), self.fit_size, self.view_size, self.compare);
            let pan = self.pan * (target / self.zoom.max(1e-6));
            (self.zoom, self.pan) = self.navigation.zoom.start(now, (self.zoom, self.pan), (target, pan), frame, crate::zoom::reduced_motion());
            self.view_changed_at = now;
            ctx.request_repaint();
        }
        self.sensor_test_hook(ctx);
    }

    /// Test hook: `GRAINEO_SENSOR="clipping,noise;channel=3;demosaic=2;zoom=4;pointer=0.5,0.4"`
    /// switches overlays/zoom/demosaic once, as soon as the sensor data is there, and holds an
    /// imaginary mouse pointer (fraction of image width/height) for the value row.
    fn sensor_test_hook(&mut self, ctx: &egui::Context) {
        static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let Ok(spec) = crate::env::var("GRAINEO_SENSOR") else { return };
        if !self.sensor_raw.ready() {
            return;
        }
        let mut pointer = None;
        for part in spec.split(';') {
            match part.split_once('=') {
                Some(("pointer", v)) => {
                    let w: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if w.len() == 2 {
                        pointer = Some([w[0], w[1]]);
                    }
                }
                _ => {}
            }
        }
        if let (Some([fx, fy]), Some(c)) = (pointer, self.current.as_ref()) {
            let [fw, fh] = c.loaded.full_size;
            self.sensor_raw.set_hover(Some([fx * fw as f32, fy * fh as f32]), fw as f32, fh as f32);
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        if DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        for part in spec.split(';') {
            match part.split_once('=') {
                Some(("channel", v)) => self.sensor_raw.channel = v.parse().unwrap_or(3),
                Some(("demosaic", v)) => self.sensor_raw.demosaic = v.parse().unwrap_or(0),
                Some(("zoom", v)) => {
                    let f: f32 = v.parse().unwrap_or(1.0);
                    let z = self.one_to_one.max(1.0) * f;
                    self.zoom = z;
                    self.view_changed_at = self.now();
                }
                Some(_) => {}
                None => {
                    for name in part.split(',') {
                        // English name of the overlay (clipping, reserve, sharpness, moire, noise, defects, lens, mosaic)
                        if let Some(o) = Overlay::ALL.iter().find(|o| format!("{o:?}").to_lowercase().starts_with(&name.trim().to_lowercase()) && !name.trim().is_empty()) {
                            self.sensor_raw.toggle_on(*o);
                        }
                    }
                }
            }
        }
    }

    fn sensor_toggle_mosaic(&mut self, ctx: &egui::Context) {
        let on = self.sensor_raw.an.contains(&Overlay::Mosaic);
        let old = self.zoom;
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let target = self.one_to_one.max(1.0) * 8.0;
        let (z, mut pan) = crate::zoom::mosaic_pose(on, &mut self.sensor_raw.mosaic_previous, (self.zoom, self.pan), target);
        if !on {
            if let (Some(rect), Some(center), Some(pos)) = (self.image_rect, self.sensor_raw.view_center, pointer) {
                if rect.contains(pos) {
                    let rel = pos - center;
                    pan += rel - rel * (z / old);
                }
            }
        }
        self.sensor_raw.toggle_on(Overlay::Mosaic);
        let now = self.now();
        let frame = (self.open_generation.load(std::sync::atomic::Ordering::SeqCst), self.fit_size, self.view_size, self.compare);
        (self.zoom, self.pan) = self.navigation.zoom.start(now, (self.zoom, self.pan), (z, pan), frame, crate::zoom::reduced_motion());
        self.view_changed_at = now;
        ctx.request_repaint();
    }

    /// ⌥ keys of the overlays (physical keys, consumed) and J = RAW clipping.
    pub(crate) fn sensor_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.sensor_raw_active() || ctx.egui_wants_keyboard_input() {
            return;
        }
        for o in Overlay::ALL {
            if crate::shortcuts::pressed(ctx, o.key_id()) {
                if o == Overlay::Mosaic { self.sensor_toggle_mosaic(ctx); } else { self.sensor_raw.toggle_on(o); }
            }
        }
        if crate::shortcuts::pressed(ctx, "sensor_demosaic") {
            self.demosaic_next();
        }
        if let Some(i) = crate::shortcuts::which(ctx, "sensor_demosaic_n") {
            let target = i + 1;
            if self.demosaic_fits(target) {
                self.sensor_raw.demosaic = if self.sensor_raw.demosaic == target { 0 } else { target };
            }
        }
        // the ⌥ keys and J belong to the sensor view here
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(e, egui::Event::Key { key, modifiers, pressed: true, .. }
                    if modifiers.alt && !modifiers.command && !matches!(key, egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown))
            });
            if i.consume_key(egui::Modifiers::NONE, egui::Key::J) {
                self.sensor_raw.toggle_on(Overlay::Clipping);
            }
        });
    }

    fn demosaic_fits(&self, i: usize) -> bool {
        self.sensor_raw.demosaic_fits(i)
    }

    fn demosaic_next(&mut self) {
        let mut i = if self.sensor_raw.demosaic == 0 { if self.sensor_raw.sensor_type() == Some(2) { 1 } else { 2 } } else { self.sensor_raw.demosaic };
        for _ in 0..DEMOSAIC.len() {
            i = (i + 1) % DEMOSAIC.len();
            if self.demosaic_fits(i) {
                break;
            }
        }
        self.sensor_raw.demosaic = i;
    }

    /// "Sensor" tray of the tool rail for RAWs: overlays and display as lists (like the views of
    /// the X3F sensor tray), values at the pointer, RAW histogram, SNR curve, legends, key figures.
    /// Fixed rows throughout – switching something never moves the controls.
    pub(crate) fn sensor_raw_sidebar(&mut self, ui: &mut egui::Ui) {
        let ready = self.sensor_raw.ready();
        // pipeline graineo | Apple | compare (macOS only, apple_raw.rs)
        self.apple_section(ui);
        if let Some(t) = self.sensor_raw.apple_only.clone() {
            ui.label(egui::RichText::new(t).small().color(lr::TEXT_DIM()));
        } else if let Some(e) = self.sensor_raw.error.clone() {
            ui.label(egui::RichText::new(crate::trf!("Sensordaten nicht lesbar: {e}", e = e)).small().color(lr::TEXT_DIM()));
        }
        lr::section(ui, "Überlagerungen", true, |ui| {
            ui.add_enabled_ui(ready, |ui| {
                for o in Overlay::ALL {
                    if o == Overlay::Mosaic {
                        continue;
                    }
                    let on = self.sensor_raw.an.contains(&o);
                    let busy = on && self.sensor_raw.computing(o);
                    if toggle_row(ui, Some(SensorRaw::icon(o)), o.name(), &crate::shortcuts::display(o.key_id()), on, busy) {
                        self.sensor_raw.toggle_on(o);
                    }
                }
                ui.add_space(6.0);
                // always visible (locked while "Noise" is off), so nothing jumps
                ui.horizontal(|ui| {
                    lr::field_label(ui, "Rauschkanal");
                    ui.add_enabled_ui(self.sensor_raw.an.contains(&Overlay::Noise), |ui| {
                        lr::segmented_pad(ui, &mut self.sensor_raw.channel, &[(0, "R"), (1, "G"), (2, "B"), (3, "Verhältnis")], 7.0);
                    });
                });
                ui.add_space(2.0);
                let mut o = (self.sensor_raw.opacity * 100.0) as f64;
                if lr::slider(ui, "Deckkraft", &mut o, 10.0..=100.0, 80.0, 1.0) {
                    self.sensor_raw.opacity = (o / 100.0).clamp(0.1, 1.0) as f32;
                }
            });
        });
        lr::section(ui, "Darstellung", true, |ui| {
            ui.add_enabled_ui(ready, |ui| {
                let on = self.sensor_raw.an.contains(&Overlay::Mosaic);
                let busy = on && self.sensor_raw.computing(Overlay::Mosaic);
                if toggle_row(ui, Some(SensorRaw::icon(Overlay::Mosaic)), crate::tr("Mosaik mit Pixelgitter"), &crate::shortcuts::display(Overlay::Mosaic.key_id()), on, busy) {
                    self.sensor_toggle_mosaic(ui.ctx());
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    lr::group_label(ui, "Demosaik");
                    help::info(ui, "sensor-demosaik", "Demosaik-Verfahren", &help::SENSOR_DEMOSAIC);
                });
                ui.add(egui::Label::new(egui::RichText::new(crate::tr("Nur zum Vergleichen – ändert die Entwicklung nicht.")).small().color(lr::TEXT_DIM())).wrap());
                ui.add_space(2.0);
                let busy = self.sensor_raw.demosaic_running();
                for (i, (_, name)) in DEMOSAIC.iter().enumerate().skip(1) {
                    let fits = self.demosaic_fits(i);
                    let key = if i == 0 { String::new() } else { crate::shortcuts::display_index("sensor_demosaic_n", i - 1) };
                    let standard = matches!((self.sensor_raw.sensor_type(), i), (Some(2), 1) | (Some(6), 2));
                    let active = self.sensor_raw.demosaic == i || (self.sensor_raw.demosaic == 0 && standard);
                    let label = if standard { format!("{} · {}", crate::tr(name), crate::tr("Standard")) } else { crate::tr(name).to_string() };
                    let tip = if !fits && i == 1 { "RCD ist für Bayer-Sensoren. Diese Kamera hat X-Trans." } else if !fits { "Markesteijn ist für X-Trans-Sensoren. Diese Kamera hat Bayer." } else { match i { 0 => "Standardverfahren für diesen Sensor", 1 => "RCD: Standard bei Bayer", 2 => "3 Durchgänge, genauer, Standard bei X-Trans", 3 => "1 Durchgang, schneller", _ => "Einfach, weicher, Farbsäume an Kanten" } };
                    let tip = if key.is_empty() { crate::tr(tip).to_string() } else { format!("{} ({})", crate::tr(tip), key) };
                    if toggle_row_impl(ui, None, &label, &key, active, active && busy, fits, Some(&tip)) && i != self.sensor_raw.demosaic {
                        self.sensor_raw.demosaic = i;
                    }
                }
                let match_text = if self.sensor_raw.sensor_type() == Some(2) { "Diese Kamera: Bayer – passend: RCD" } else { "Diese Kamera: X-Trans – passend: Markesteijn" };
                ui.add(egui::Label::new(egui::RichText::new(crate::tr(match_text)).small().color(lr::TEXT_DIM())).wrap());
            });
        });
        lr::section(ui, "Am Zeiger", true, |ui| {
            let v = self.sensor_raw.values();
            egui::Grid::new("sensor-zeiger").num_columns(2).spacing([10.0, 3.0]).min_col_width(GRID_LABEL_W).show(ui, |ui| {
                for (i, label) in VALUE_ROWS.iter().enumerate() {
                    ui.label(egui::RichText::new(crate::tr(label)).color(lr::TEXT_DIM()));
                    let t = v.as_ref().map_or("–", |v| v[i].as_str());
                    ui.add(egui::Label::new(egui::RichText::new(t).monospace()).truncate());
                    ui.end_row();
                }
            });
        });
        lr::section(ui, "RAW-Histogramm", true, |ui| {
            let h = self.sensor_raw.histogram_ev();
            histogram_ev(ui, h.as_ref().map(|h| (h.0, &h.1)));
            ui.add_space(4.0);
            egui::Grid::new("sensor-reserve").num_columns(2).spacing([10.0, 3.0]).min_col_width(GRID_LABEL_W).show(ui, |ui| {
                for (c, name) in ["R", "G", "B"].iter().enumerate() {
                    ui.label(egui::RichText::new(crate::trf!("Reserve {a}", a = name)).color(lr::TEXT_DIM()));
                    let (text, color) = match h.as_ref().and_then(|h| h.2[c]) {
                        None => ("–".to_string(), lr::TEXT_DIM()),
                        Some(r) if r <= 0.05 => (crate::tr("ausgebrannt").to_string(), Color32::from_rgb(0xe8, 0x70, 0x70)),
                        Some(r) => (format!("{r:+.1} EV").de_comma(), if r < 0.5 { Color32::from_rgb(0xe8, 0xc0, 0x60) } else { lr::TEXT() }),
                    };
                    ui.label(egui::RichText::new(text).color(color).monospace());
                    ui.end_row();
                }
                ui.label(egui::RichText::new(crate::tr("Geclippt")).color(lr::TEXT_DIM()));
                let t = h.as_ref().and_then(|h| h.3).map_or("–".to_string(), |p| format!("{p:.2} %").de_comma());
                ui.label(egui::RichText::new(t).monospace());
                ui.end_row();
            });
        });
        lr::section(ui, "Rauschabstand (SNR)", true, |ui| {
            snr_chart(ui, self.sensor_raw.snr_curve().as_ref());
        });
        lr::section(ui, "Legende", true, |ui| {
            let l = self.sensor_raw.legends();
            if l.is_empty() {
                ui.label(egui::RichText::new("–").color(lr::TEXT_DIM()));
            }
            for (o, lines) in l {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(o.name()).strong());
                    help::info(ui, o.key_id(), o.name(), help::sensor_card(o.key_id()));
                });
                for (c, t) in lines {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 0.0, c);
                        // long legend lines wrap inside the tray (a plain label in a horizontal row would widen it)
                        ui.add(egui::Label::new(egui::RichText::new(t).small()).wrap());
                    });
                }
                ui.add_space(4.0);
            }
        });
        lr::section(ui, "Kennzahlen", true, |ui| {
            let k = self.sensor_raw.metrics();
            if k.is_empty() {
                ui.label(egui::RichText::new("–").color(lr::TEXT_DIM()));
            }
            egui::Grid::new("sensor-kennzahlen").num_columns(2).spacing([10.0, 3.0]).min_col_width(GRID_LABEL_W).max_col_width((ui.available_width() - GRID_LABEL_W - 10.0).max(GRID_LABEL_W)).show(ui, |ui| {
                for (n, v) in k {
                    ui.label(egui::RichText::new(n).color(lr::TEXT_DIM()));
                    ui.add(egui::Label::new(v).wrap());
                    ui.end_row();
                }
            });
        });
    }
}

/// Row of a switch list in the tray: optional icon, name, shortcut dimmed on the right (or "…" while
/// it computes); light fill when on, like the views of the X3F sensor tray. Returns: clicked.
fn toggle_row(ui: &mut egui::Ui, icon: Option<lr::Icon>, name: &str, key: &str, on: bool, busy: bool) -> bool {
    toggle_row_impl(ui, icon, name, key, on, busy, true, None)
}

fn toggle_row_impl(ui: &mut egui::Ui, icon: Option<lr::Icon>, name: &str, key: &str, on: bool, busy: bool, available: bool, tip: Option<&str>) -> bool {
    let enabled = ui.is_enabled() && available;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    if let Some(tip) = tip { lr::hover_tip(ui, &resp, tip); }
    let painter = ui.painter();
    if on {
        painter.rect_filled(rect, 0.0, crate::lr::gray(0x4a));
    } else if resp.hovered() && enabled {
        painter.rect_filled(rect, 0.0, crate::lr::gray(0x38));
    }
    let fg = if !enabled {
        crate::lr::gray(0x5c)
    } else if on || resp.hovered() {
        crate::lr::TEXT_STRONG()
    } else {
        lr::TEXT()
    };
    let mut x = rect.left() + 8.0;
    if let Some(i) = icon {
        lr::paint_icon(painter, egui::Rect::from_center_size(egui::pos2(x + 7.0, rect.center().y), egui::vec2(14.0, 14.0)), i, fg);
        x += 22.0;
    }
    let right = if busy { "…".to_string() } else { key.to_string() };
    let rg = painter.layout_no_wrap(right, egui::FontId::proportional(10.5), if on { crate::lr::gray(0xc8) } else { lr::TEXT_DIM() });
    let rx = rect.right() - 8.0 - rg.size().x;
    painter.galley(egui::pos2(rx, rect.center().y - rg.size().y / 2.0), rg, crate::lr::TEXT_STRONG());
    painter.with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(rx - 6.0, rect.bottom()))).text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::TextStyle::Body.resolve(ui.style()),
        fg,
    );
    resp.clicked() && enabled
}

/// Width of the label column of the value grids (same in all sections, so the values line up).
const GRID_LABEL_W: f32 = 120.0;
#[allow(non_snake_case)]
fn CHART_BG() -> Color32 {
    crate::lr::gray(20)
}
#[allow(non_snake_case)]
fn CHART_GRID() -> Color32 {
    crate::lr::gray(0x30)
}
const CHART_COLORS: [Color32; 3] = [Color32::from_rgb(255, 90, 90), Color32::from_rgb(90, 230, 90), Color32::from_rgb(100, 150, 255)];

/// Axis labels (EV) below a chart and vertical grid lines.
fn ev_axis(painter: &egui::Painter, plot: egui::Rect, ev0: f32, ev1: f32, step: i32) {
    let font = egui::FontId::proportional(9.5);
    let mut e = (ev0 / step as f32).ceil() as i32 * step;
    while e as f32 <= ev1 + 1e-3 {
        let x = plot.left() + plot.width() * (e as f32 - ev0) / (ev1 - ev0);
        painter.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], egui::Stroke::new(1.0, CHART_GRID()));
        painter.text(egui::pos2(x, plot.bottom() + 2.0), egui::Align2::CENTER_TOP, if e == 0 { "0".to_string() } else { format!("{e}") }, font.clone(), lr::TEXT_DIM());
        e += step;
    }
}

/// RAW histogram in EV below saturation (−12 … 0 EV), one curve per filter color, square-root scale.
fn histogram_ev(ui: &mut egui::Ui, h: Option<(f32, &[Vec<f64>; 3])>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 110.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let plot = egui::Rect::from_min_max(egui::pos2(rect.left() + 8.0, rect.top()), egui::pos2(rect.right() - 8.0, rect.bottom() - 14.0));
    painter.rect_filled(plot, 0.0, CHART_BG());
    let (ev0, ev1) = (-12.0f32, 0.25f32);
    ev_axis(&painter, plot, ev0, ev1 - 0.25, 2);
    // saturation
    let xs = plot.left() + plot.width() * (0.0 - ev0) / (ev1 - ev0);
    painter.line_segment([egui::pos2(xs, plot.top()), egui::pos2(xs, plot.bottom())], egui::Stroke::new(1.0, Color32::from_rgb(0x80, 0x40, 0x40)));
    let Some((ev_min, h)) = h else { return };
    let bin_ev = |i: usize| ev_min + i as f32 / 8.0;
    let max = h.iter().flat_map(|b| b.iter().enumerate().filter(|(i, _)| (ev0..ev1 - 0.2).contains(&bin_ev(*i))).map(|(_, v)| *v)).fold(1.0f64, f64::max);
    for (c, bins) in h.iter().enumerate() {
        let pts: Vec<egui::Pos2> = bins
            .iter()
            .enumerate()
            .filter(|(i, _)| (ev0..=ev1).contains(&bin_ev(*i)))
            .map(|(i, &v)| {
                let x = plot.left() + plot.width() * (bin_ev(i) + 1.0 / 16.0 - ev0) / (ev1 - ev0);
                egui::pos2(x, plot.bottom() - plot.height() * ((v / max).sqrt() as f32).min(1.0) * 0.95)
            })
            .collect();
        if pts.len() > 1 {
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.3, CHART_COLORS[c].gamma_multiply(0.85))));
        }
    }
}

/// SNR over EV below saturation: model as a line, measurements as dots, one color per filter color.
#[allow(clippy::type_complexity)]
fn snr_chart(ui: &mut egui::Ui, d: Option<&(Vec<f64>, Vec<Vec<f64>>, Vec<Vec<(f64, f64)>>)>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 130.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let plot = egui::Rect::from_min_max(egui::pos2(rect.left() + 22.0, rect.top()), egui::pos2(rect.right() - 8.0, rect.bottom() - 14.0));
    painter.rect_filled(plot, 0.0, CHART_BG());
    let (ev0, ev1) = (-14.0f32, 0.0f32);
    let db_max = 50.0f32;
    ev_axis(&painter, plot, ev0, ev1, 2);
    let font = egui::FontId::proportional(9.5);
    for db in [0, 10, 20, 30, 40, 50] {
        let y = plot.bottom() - plot.height() * db as f32 / db_max;
        painter.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], egui::Stroke::new(1.0, CHART_GRID()));
        if db % 20 == 0 {
            painter.text(egui::pos2(plot.left() - 4.0, y), egui::Align2::RIGHT_CENTER, format!("{db}"), font.clone(), lr::TEXT_DIM());
        }
    }
    painter.text(egui::pos2(plot.left() + 4.0, plot.top() + 3.0), egui::Align2::LEFT_TOP, "dB", font.clone(), lr::TEXT_DIM());
    let Some((ev, model, points)) = d else { return };
    let to = |e: f64, db: f64| egui::pos2(plot.left() + plot.width() * (e as f32 - ev0) / (ev1 - ev0), plot.bottom() - plot.height() * (db as f32 / db_max).clamp(0.0, 1.0));
    for (c, m) in model.iter().enumerate().take(3) {
        let pts: Vec<egui::Pos2> = ev.iter().zip(m).filter(|(e, _)| (ev0 as f64..=ev1 as f64).contains(e)).map(|(&e, &db)| to(e, db)).collect();
        if pts.len() > 1 {
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.2, CHART_COLORS[c].gamma_multiply(0.7))));
        }
    }
    for (c, p) in points.iter().enumerate().take(3) {
        for &(e, db) in p {
            if (ev0 as f64..=ev1 as f64).contains(&e) {
                painter.circle_stroke(to(e, db), 2.2, egui::Stroke::new(1.0, CHART_COLORS[c]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensor_raw::SensorRaw;

    fn frame(f: impl FnMut(&mut egui::Ui)) {
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 600.0))), ..Default::default() };
        crate::ui_test::run(&ctx, input, f);
    }

    /// Switch rows keep one height whether off, on, computing or locked – toggling never moves the tray.
    #[test]
    fn toggle_row_fixed_height() {
        let mut heights = Vec::new();
        frame(|ui| {
            for (on, busy) in [(false, false), (true, false), (true, true)] {
                let top = ui.cursor().top();
                toggle_row(ui, Some(lr::Icon::Warning), "Clipping", "⌥C", on, busy);
                heights.push(ui.cursor().top() - top);
            }
            ui.add_enabled_ui(false, |ui| {
                let top = ui.cursor().top();
                toggle_row(ui, None, "Markesteijn 3", "⌥2", false, false);
                heights.push(ui.cursor().top() - top);
            });
        });
        assert!(heights.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01), "{heights:?}");
    }

    /// Charts take their fixed height with and without data.
    #[test]
    fn charts_fixed_height() {
        frame(|ui| {
            let top = ui.cursor().top();
            histogram_ev(ui, None);
            let h0 = ui.cursor().top() - top;
            let bins: [Vec<f64>; 3] = [vec![1.0; 130], vec![2.0; 130], vec![0.0; 130]];
            let top = ui.cursor().top();
            histogram_ev(ui, Some((-16.0, &bins)));
            assert!((ui.cursor().top() - top - h0).abs() < 0.01);
            let top = ui.cursor().top();
            snr_chart(ui, None);
            let s0 = ui.cursor().top() - top;
            let d = (vec![-14.0, -7.0, 0.0], vec![vec![0.0, 20.0, 45.0]; 3], vec![vec![(-6.0, 25.0)]; 3]);
            let top = ui.cursor().top();
            snr_chart(ui, Some(&d));
            assert!((ui.cursor().top() - top - s0).abs() < 0.01);
        });
    }

    /// Before the sensor data is read the label says so; without a sensor only the neutral methods fit.
    #[test]
    fn status_and_methods_without_data() {
        let s = SensorRaw::default();
        let st = s.status();
        assert_eq!(st.len(), 1);
        assert!(!st[0].1);
        assert!(s.values().is_none());
        assert!(s.demosaic_fits(0) && s.demosaic_fits(4));
        assert!(!s.demosaic_fits(1) && !s.demosaic_fits(2) && !s.demosaic_fits(3));
    }
}
