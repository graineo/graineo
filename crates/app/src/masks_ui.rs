//! Local masks: visible controls and image-space drawing, stored with development settings.
use egui::{Color32, Context, Ui};
use crate::lr;
use masks::{Adjustments, Document, Layer, Mask, Operation};
#[derive(Clone, Default)]
struct State {
    image: usize,
    selected: usize,
    draw: bool,
    show: bool,
    kind: usize,
    operation: usize,
    start: Option<[f32; 2]>,
}
fn key() -> egui::Id {
    egui::Id::unique("local_mask_editor")
}
fn state(ctx: &Context, index: usize) -> State {
    let mut s = ctx.data_mut(|d| d.get_temp::<State>(key()).unwrap_or_default());
    if s.image != index {
        s = State {
            image: index,
            ..Default::default()
        };
    }
    s
}
pub fn painting(ctx: &Context) -> bool {
    ctx.data_mut(|d| d.get_temp::<State>(key()).is_some_and(|s| s.draw)) || crate::retouch_ui::active(ctx)
}
/// Leave the drawing tool without changing the stored mask.
pub fn finish(ctx: &Context) -> bool {
    ctx.data_mut(|d| {
        let Some(mut s) = d.get_temp::<State>(key()) else { return false };
        let active = s.draw;
        s.draw = false;
        s.start = None;
        d.insert_temp(key(), s);
        active
    })
}
fn shape(kind: usize) -> Mask {
    match kind {
        0 => Mask::Brush {
            points: vec![[-2.0, -2.0]],
            radius: 0.04,
            hardness: 0.6,
            flow: 0.2,
        },
        1 => Mask::Linear {
            start: [0.25, 0.5],
            end: [0.75, 0.5],
        },
        2 => Mask::Radial {
            center: [0.5, 0.5],
            radii: [0.25, 0.2],
            angle: 0.0,
            feather: 0.5,
        },
        3 => Mask::Luminance {
            low: 0.0,
            high: 0.5,
            feather: 0.2,
        },
        _ => Mask::Color {
            reference: [0.18; 3],
            radius: 0.1,
            feather: 0.1,
        },
    }
}
fn slider(ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>) {
    ui.add(egui::Slider::new(v, range).text(label));
}
fn point(ui: &mut Ui, label: &str, p: &mut [f32; 2]) {
    ui.horizontal(|ui| {
        ui.label(label);
        for v in p {
            crate::lr::text_field(ui, egui::DragValue::new(v).speed(0.005).range(-2.0..=2.0));
        }
    });
}
fn edit(ui: &mut Ui, m: &mut Mask) {
    match m {
        Mask::Brush {
            radius,
            hardness,
            flow,
            ..
        } => {
            slider(ui, crate::tr("Radius"), radius, 0.001..=0.5);
            slider(ui, crate::tr("Härte"), hardness, 0.0..=1.0);
            slider(ui, crate::tr("Fluss"), flow, 0.0..=1.0);
        }
        Mask::Linear { start, end } => {
            point(ui, crate::tr("Anfang"), start);
            point(ui, crate::tr("Ende"), end);
            if start == end {
                end[0] += 0.001;
            }
        }
        Mask::Radial {
            center,
            radii,
            angle,
            feather,
        } => {
            point(ui, crate::tr("Mitte"), center);
            slider(ui, crate::tr("Breite"), &mut radii[0], 0.001..=2.0);
            slider(ui, crate::tr("Höhe"), &mut radii[1], 0.001..=2.0);
            slider(
                ui,
                crate::tr("Winkel"),
                angle,
                -std::f32::consts::PI..=std::f32::consts::PI,
            );
            slider(ui, crate::tr("Weiche Kante"), feather, 0.0..=1.0);
        }
        Mask::Luminance { low, high, feather } => {
            slider(ui, crate::tr("Von"), low, 0.0..=2.0);
            slider(ui, crate::tr("Bis"), high, 0.0..=2.0);
            *high = high.max(*low);
            slider(ui, crate::tr("Weiche Kante"), feather, 0.0..=1.0);
        }
        Mask::Color {
            reference,
            radius,
            feather,
        } => {
            for (c, name) in reference.iter_mut().zip([crate::tr("Rot"), crate::tr("Grün"), crate::tr("Blau")]) {
                slider(ui, name, c, 0.0..=1.0);
            }
            slider(ui, crate::tr("Farbbereich"), radius, 0.0..=1.0);
            slider(ui, crate::tr("Weiche Kante"), feather, 0.0..=1.0);
        }
        Mask::Invert { mask } => edit(ui, mask),
        Mask::Combine { left, right, .. } => {
            egui::CollapsingHeader::new(crate::tr("Vorige Form"))
                .id_salt("left")
                .show(ui, |ui| edit(ui, left));
            edit(ui, right);
        }
    }
}
/// Names of the mask kinds (selection for new masks).
const KINDS: [&str; 5] = ["Pinsel", "Linearer Verlauf", "Ellipse", "Helligkeit", "Farbbereich"];

/// Display name of a mask: kind of its last shape, marked when inverted or combined.
fn mask_name(m: &Mask) -> String {
    fn kind(m: &Mask) -> &'static str {
        match m {
            Mask::Brush { .. } => KINDS[0],
            Mask::Linear { .. } => KINDS[1],
            Mask::Radial { .. } => KINDS[2],
            Mask::Luminance { .. } => KINDS[3],
            Mask::Color { .. } => KINDS[4],
            Mask::Invert { mask } => kind(mask),
            Mask::Combine { right, .. } => kind(right),
        }
    }
    let mut t = crate::tr(kind(m)).to_string();
    if matches!(m, Mask::Combine { .. }) || matches!(m, Mask::Invert { mask } if matches!(**mask, Mask::Combine { .. })) {
        t.push_str(" +");
    }
    if matches!(m, Mask::Invert { .. }) {
        t.push_str(&format!(" · {}", crate::tr("invertiert")));
    }
    t
}

pub fn panel(ui: &mut Ui, local: &mut Option<Document>, index: usize) {
    let test = crate::env::var("GRAINEO_MASKS_TEST").as_deref() == Ok("1");
    if test && local.is_none() {
        let mut doc =
            Document::from_json(include_str!("../../masks/samples/masks-example.json")).unwrap();
        doc.version = 2;
        *local = Some(doc);
    }
    let mut s = state(ui.ctx(), index);
    egui::CollapsingHeader::new(crate::tr("LOKALE KORREKTUREN"))
        .default_open(true)
        .show(ui, |ui| {
            // new mask: kind and button in one row
            ui.horizontal(|ui| {
                crate::help::info(ui, "local_masks", "Lokale Korrekturen", &crate::help::LOCAL_MASKS);
                egui::ComboBox::from_id_salt("local_kind")
                    .selected_text(crate::tr(KINDS[s.kind]))
                    .show_ui(ui, |ui| {
                        for (i, name) in KINDS.iter().enumerate() {
                            ui.selectable_value(&mut s.kind, i, crate::tr(name));
                        }
                    });
                if ui.button(crate::tr("Neue Maske")).clicked() {
                    let d = local.get_or_insert(Document { version: 2, layers: vec![] });
                    if d.layers.len() < 64 {
                        d.layers.push(Layer { mask: shape(s.kind), adjustments: Adjustments::default() });
                        s.selected = d.layers.len() - 1;
                        // brush and gradients are drawn in the image right away
                        s.draw = s.kind <= 2;
                    }
                }
            });
            let count = local.as_ref().map_or(0, |d| d.layers.len());
            if count == 0 {
                ui.label(egui::RichText::new(crate::tr("Keine Masken")).small().color(lr::TEXT_DIM));
                return;
            }
            let Some(d) = local.as_mut() else { return };
            if d.version == 1 && ui.button(crate::tr("Filter auf Version 2 umstellen")).clicked() {
                d.version = 2;
            }
            s.selected = s.selected.min(d.layers.len() - 1);
            // existing masks: choose one (all listed with their kind), draw in the image, remove
            let mut remove = false;
            ui.horizontal(|ui| {
                let names: Vec<String> = d.layers.iter().map(|l| mask_name(&l.mask)).collect();
                egui::ComboBox::from_id_salt("local_layer")
                    .selected_text(crate::trf!("Maske {a} von {b} · {c}", a = s.selected + 1, b = count, c = names[s.selected]))
                    .show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut s.selected, i, crate::trf!("Maske {a} · {c}", a = i + 1, c = n));
                        }
                    });
            });
            ui.horizontal(|ui| {
                let drawing = s.draw;
                if ui.add(egui::Button::new(crate::tr("Im Bild zeichnen")).selected(drawing)).clicked() {
                    s.draw = !drawing;
                    s.start = None;
                }
                if ui.button(crate::tr("Maske entfernen")).clicked() {
                    remove = true;
                }
            });
            if remove {
                d.layers.remove(s.selected);
                s.selected = s.selected.saturating_sub(1);
                s.draw = false;
                s.start = None;
                if d.layers.is_empty() {
                    return;
                }
            }
            let layer = &mut d.layers[s.selected];
            ui.horizontal(|ui| {
                if ui.button(crate::tr("Invertieren")).clicked() {
                    layer.mask = match layer.mask.clone() {
                        Mask::Invert { mask } => *mask,
                        m => Mask::Invert { mask: Box::new(m) },
                    };
                }
                if ui.button(crate::tr("Zurücksetzen")).clicked() {
                    layer.adjustments = Adjustments::default();
                }
            });
            const OPERATIONS: [&str; 3] = ["Addieren", "Subtrahieren", "Schneiden"];
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("local_operation")
                    .selected_text(crate::tr(OPERATIONS[s.operation]))
                    .show_ui(ui, |ui| {
                        for (i, t) in OPERATIONS.iter().enumerate() {
                            ui.selectable_value(&mut s.operation, i, crate::tr(t));
                        }
                    });
                if ui.button(crate::tr("Form ergänzen")).clicked() {
                    let candidate = Mask::Combine {
                        operation: [Operation::Add, Operation::Subtract, Operation::Intersect][s.operation],
                        left: Box::new(layer.mask.clone()),
                        right: Box::new(shape(s.kind)),
                    };
                    if candidate.validate().is_ok() {
                        layer.mask = candidate;
                    }
                }
            });
            edit(ui, &mut layer.mask);
            let a = &mut layer.adjustments;
            slider(ui, crate::tr("Belichtung"), &mut a.exposure, -3.0..=3.0);
            for (label, v) in [
                (crate::tr("Kontrast"), &mut a.contrast),
                (crate::tr("Lichter"), &mut a.highlights),
                (crate::tr("Tiefen"), &mut a.shadows),
                (crate::tr("Temperatur"), &mut a.temperature),
                (crate::tr("Tönung"), &mut a.tint),
                (crate::tr("Sättigung"), &mut a.saturation),
                (crate::tr("Klarheit"), &mut a.clarity),
                (crate::tr("Struktur"), &mut a.texture),
            ] {
                slider(ui, label, v, -1.0..=1.0);
            }
            slider(ui, crate::tr("Schärfe"), &mut a.sharpness, 0.0..=1.0);
            slider(ui, crate::tr("Rauschminderung"), &mut a.noise_reduction, 0.0..=1.0);
        });
    ui.ctx().data_mut(|d| d.insert_temp(key(), s));
}

/// State of mask editing for the darkroom ("Dodge / Burn").
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Editing {
    /// selected mask
    pub selected: usize,
    /// draw in the image (brush or drag a gradient)
    pub draw: bool,
    /// show outlines
    pub contours: bool,
}

/// Current editing state for image `index`.
pub fn editing_state(ctx: &Context, index: usize) -> Editing {
    let s = state(ctx, index);
    Editing { selected: s.selected, draw: s.draw, contours: s.show }
}

/// Set the editing state (selected mask, drawing, outlines).
pub fn set_editing(ctx: &Context, index: usize, b: Editing) {
    let mut s = state(ctx, index);
    s.selected = b.selected;
    s.draw = b.draw;
    s.show = b.contours;
    ctx.data_mut(|d| d.insert_temp(key(), s));
}

/// New layer for local print brightness: `kind` 0 = brush, 1 = gradient (top to bottom);
/// `ev` > 0 brightens (dodge), < 0 darkens (burn). The layer is selected and
/// drawing in the image is switched on. Returns: created?
pub fn new_exposure_layer(ctx: &Context, local: &mut Option<Document>, index: usize, kind: usize, ev: f32) -> bool {
    let d = local.get_or_insert(Document { version: 2, layers: vec![] });
    if d.layers.len() >= 64 {
        return false;
    }
    if d.version == 1 {
        d.version = 2;
    }
    let mask = match kind {
        0 => shape(0),
        _ => Mask::Linear { start: [0.5, 0.15], end: [0.5, 0.55] },
    };
    d.layers.push(Layer { mask, adjustments: Adjustments { exposure: ev.clamp(-3.0, 3.0), ..Adjustments::default() } });
    let mut s = state(ctx, index);
    s.selected = d.layers.len() - 1;
    s.kind = kind.min(1);
    s.draw = true;
    s.show = true;
    ctx.data_mut(|dd| dd.insert_temp(key(), s));
    true
}

fn leaf(m: &mut Mask) -> &mut Mask {
    match m {
        Mask::Invert { mask } => leaf(mask),
        Mask::Combine { right, .. } => leaf(right),
        m => m,
    }
}
pub fn overlay(
    ui: &Ui,
    resp: &egui::Response,
    painter: &egui::Painter,
    rect: egui::Rect,
    frame: &crate::Frame,
    full: [f32; 2],
    local: &mut Option<Document>,
    index: usize,
) {
    let mut s = state(ui.ctx(), index);
    let Some(d) = local else {
        return;
    };
    let Some(layer) = d.layers.get_mut(s.selected) else {
        return;
    };
    let to_screen = |p: [f32; 2]| {
        let u = frame.from_src([p[0] * full[0], p[1] * full[1]]);
        rect.min + egui::vec2(u[0], u[1]) * rect.size()
    };
    if s.draw && (resp.drag_started() || resp.clicked()) {
        if let Mask::Brush {
            radius,
            hardness,
            flow,
            points,
        } = leaf(&mut layer.mask)
        {
            if points.iter().any(|p| p[0] >= 0.0 && p[1] >= 0.0) {
                let new = Mask::Brush {
                    points: vec![[-2.0, -2.0]],
                    radius: *radius,
                    hardness: *hardness,
                    flow: *flow,
                };
                fn collect(m: Mask, out: &mut Vec<Mask>) {
                    match m {
                        Mask::Combine {
                            operation: Operation::Add,
                            left,
                            right,
                        } => {
                            collect(*left, out);
                            collect(*right, out);
                        }
                        m => out.push(m),
                    }
                }
                fn balance(v: &[Mask]) -> Mask {
                    if v.len() == 1 {
                        return v[0].clone();
                    }
                    let n = v.len() / 2;
                    Mask::Combine {
                        operation: Operation::Add,
                        left: Box::new(balance(&v[..n])),
                        right: Box::new(balance(&v[n..])),
                    }
                }
                fn append(m: &Mask, new: &Mask) -> Mask {
                    match m {
                        Mask::Invert { mask } => Mask::Invert {
                            mask: Box::new(append(mask, new)),
                        },
                        Mask::Combine {
                            operation,
                            left,
                            right,
                        } if *operation != Operation::Add => Mask::Combine {
                            operation: *operation,
                            left: left.clone(),
                            right: Box::new(append(right, new)),
                        },
                        m => {
                            let mut forms = Vec::new();
                            collect(m.clone(), &mut forms);
                            forms.push(new.clone());
                            balance(&forms)
                        }
                    }
                }
                let candidate = append(&layer.mask, &new);
                if candidate.validate().is_ok() {
                    layer.mask = candidate;
                }
            }
        }
    }
    let m = leaf(&mut layer.mask);
    if s.draw {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        if let Some(pos) = resp
            .interact_pointer_pos()
            .filter(|p| rect.contains(*p) && ui.clip_rect().contains(*p))
        {
            let u = (pos - rect.min) / rect.size();
            let q = frame.to_src([u.x, u.y]);
            let p = [
                (q[0] / full[0]).clamp(0.0, 1.0),
                (q[1] / full[1]).clamp(0.0, 1.0),
            ];
            if resp.drag_started() || resp.clicked() {
                s.start = Some(p);
                if let Mask::Brush { points, .. } = m {
                    points.clear();
                    points.push(p);
                }
            }
            if resp.dragged() || resp.clicked() {
                match m {
                    Mask::Brush { points, .. } => {
                        if points.len() < 100_000
                            && points
                                .last()
                                .is_none_or(|q| (q[0] - p[0]).hypot(q[1] - p[1]) > 0.001)
                        {
                            points.push(p);
                        }
                    }
                    Mask::Linear { start, end } => {
                        if let Some(anchor) = s.start {
                            *start = anchor;
                            *end = p;
                            if start == end {
                                end[0] += 0.001;
                            }
                        }
                    }
                    Mask::Radial { center, radii, .. } => {
                        if let Some(anchor) = s.start {
                            *center = anchor;
                            let short = full[0].min(full[1]);
                            *radii = [
                                ((p[0] - anchor[0]) * full[0] / short).abs().max(0.001),
                                ((p[1] - anchor[1]) * full[1] / short).abs().max(0.001),
                            ];
                        }
                    }
                    _ => (),
                }
                ui.ctx().request_repaint();
            }
        }
    }
    // outlines: while drawing, with the overlays of the "Masks" tool (H) or switched on in the darkroom
    if s.show || s.draw || crate::retouch_ui::overlays_visible(ui.ctx()) {
        let stroke = egui::Stroke::new(1.0, Color32::from_rgb(230, 180, 70));
        match m {
            Mask::Brush { points, .. } => {
                for w in points.windows(2) {
                    painter.line_segment([to_screen(w[0]), to_screen(w[1])], stroke);
                }
                if let Some(p) = points.first() {
                    painter.circle_stroke(to_screen(*p), 4.0, stroke);
                }
            }
            Mask::Linear { start, end } => {
                painter.line_segment([to_screen(*start), to_screen(*end)], stroke);
            }
            Mask::Radial {
                center,
                radii,
                angle,
                ..
            } => {
                let short = full[0].min(full[1]);
                let (sin, cos) = angle.sin_cos();
                let pts = (0..=64)
                    .map(|i| {
                        let t = i as f32 * std::f32::consts::TAU / 64.0;
                        let x = t.cos() * radii[0];
                        let y = t.sin() * radii[1];
                        to_screen([
                            center[0] + (cos * x - sin * y) * short / full[0],
                            center[1] + (sin * x + cos * y) * short / full[1],
                        ])
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, stroke));
            }
            _ => (),
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(key(), s));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_xmp_roundtrip() {
        let root = std::path::PathBuf::from(
            crate::env::var("GRAINEO_DATA_DIR").expect("own test folder missing"),
        );
        assert!(root.starts_with("/private/tmp/"));
        let root = root.join(format!(
            "mask-xmp-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("sample.jpg");
        std::fs::write(&original, b"synthetic-original").unwrap();
        let xmp = library::xmp::path_to(&original);
        let foreign="<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:custom=\"urn:test\" custom:Keep=\"yes\"/></rdf:RDF></x:xmpmeta>";
        std::fs::write(&xmp, foreign).unwrap();
        let mut doc =
            Document::from_json(include_str!("../../masks/samples/masks-example.json")).unwrap();
        doc.version = 2;
        let settings = develop::Settings {
            local: Some(doc),
            ..Default::default()
        };
        crate::raw_app::save_settings(
            &original,
            None,
            &settings,
            &crate::sidecar::Meta::default(),
        )
        .unwrap();
        let (loaded, _, present) = crate::raw_app::load_settings(&original, None);
        assert!(present);
        assert_eq!(loaded.local, settings.local);
        assert!(std::fs::read_to_string(&xmp)
            .unwrap()
            .contains("custom:Keep=\"yes\""));
        assert_eq!(std::fs::read(original).unwrap(), b"synthetic-original");
        let mut unknown: serde_json::Value =
            serde_json::from_str(&library::xmp::read_develop(&xmp).unwrap().unwrap())
                .unwrap();
        unknown["local"]["version"] = 999.into();
        library::xmp::write_develop(&xmp, Some(&unknown.to_string())).unwrap();
        let before = std::fs::read(&xmp).unwrap();
        assert!(crate::raw_app::save_settings(
            &root.join("sample.jpg"),
            None,
            &settings,
            &crate::sidecar::Meta::default()
        )
        .is_err());
        assert_eq!(std::fs::read(&xmp).unwrap(), before);
    }
}

#[cfg(test)]
mod render_tests {
    #[test]
    #[ignore = "Fotokopie im Scratch erforderlich"]
    fn local_preview_detail_export_match() {
        let path = std::path::PathBuf::from(
            std::env::var("MASKS_PHOTO").expect("photo copy in scratch required"),
        );
        assert!(path.starts_with("/private/tmp/graineo-masks"));
        let doc = rawdev::RawDoc::open(&path, &|| true).unwrap().unwrap();
        let mut local =
            masks::Document::from_json(include_str!("../../masks/samples/masks-v2-example.json"))
                .unwrap();
        local.layers.truncate(1);
        let s = develop::Settings {
            local: Some(local),
            ..Default::default()
        };
        let full = doc.render_full(&s).unwrap();
        let region = [10, 10, 30, 25];
        let detail = doc.render_region(&s, region).unwrap();
        let expected = develop::local::crop(&full, region);
        assert_eq!(detail.data, expected.data);
        let preview = doc.render_preview(&s);
        assert_eq!((preview.width, preview.height), (full.width, full.height));
        let max = preview
            .data
            .iter()
            .zip(&full.data)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        println!("preview/export max. 16-bit deviation: {max}");
        assert!(max <= 2);
    }
}

#[cfg(test)]
mod drawing_tests {
    use super::*;
    #[test]
    fn finishing_removes_only_the_drawing_tool() {
        let ctx=Context::default();
        ctx.data_mut(|d| d.insert_temp(key(), State { draw:true, start:Some([0.3,0.4]), ..Default::default() }));
        assert!(painting(&ctx));
        assert!(finish(&ctx));
        assert!(!painting(&ctx));
        assert!(!finish(&ctx));
        assert!(state(&ctx,0).start.is_none());
    }
    #[test]
    fn strokes_keep_previous_and_inversion() {
        let ctx = Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(
                key(),
                State {
                    draw: true,
                    ..Default::default()
                },
            )
        });
        let mut local = Some(Document {
            version: 2,
            layers: vec![Layer {
                mask: shape(0),
                adjustments: Adjustments::default(),
            }],
        });
        let frame = crate::Frame::from_settings(&develop::Settings::default(), 200, 100, 0);
        let run = |events: Vec<egui::Event>, local: &mut Option<Document>| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let _ = crate::ui_test::run(&ctx, raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let rect =
                        egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
                    let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                    overlay(
                        ui,
                        &resp,
                        ui.painter(),
                        rect,
                        &frame,
                        [200.0, 100.0],
                        local,
                        0,
                    );
                });
            });
        };
        run(Vec::new(), &mut local); // Register hit-test rectangles before the first pointer press.
        let click = |p: egui::Pos2, pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        for (start, end) in [
            (egui::pos2(30.0, 30.0), egui::pos2(90.0, 60.0)),
            (egui::pos2(120.0, 30.0), egui::pos2(160.0, 70.0)),
        ] {
            run(
                vec![egui::Event::PointerMoved(start), click(start, true)],
                &mut local,
            );
            run(vec![egui::Event::PointerMoved(end)], &mut local);
            run(vec![click(end, false)], &mut local);
            let layer = &mut local.as_mut().unwrap().layers[0];
            layer.mask.validate().unwrap();
        }
        assert!(matches!(
            local.as_ref().unwrap().layers[0].mask,
            Mask::Combine { .. }
        ));
        let previous = local.as_ref().unwrap().layers[0].mask.clone();
        local.as_mut().unwrap().layers[0].mask = Mask::Invert {
            mask: Box::new(previous),
        };
        let p = egui::pos2(80.0, 40.0);
        let q = egui::pos2(110.0, 65.0);
        run(
            vec![egui::Event::PointerMoved(p), click(p, true)],
            &mut local,
        );
        run(vec![egui::Event::PointerMoved(q)], &mut local);
        run(vec![click(q, false)], &mut local);
        assert!(matches!(
            local.as_ref().unwrap().layers[0].mask,
            Mask::Invert { .. }
        ));
        local.unwrap().validate().unwrap();
    }
}

/// The import report stays visible even when the tool group is collapsed.
pub fn import_hint(ui: &mut Ui, report: Option<&develop::settings::MaskImportReport>) {
    let Some(r) = report else { return };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.strong(crate::tr("LIGHTROOM-MASKEN"));
        if r.total == 0 {
            ui.label(crate::tr("Maskendaten nicht übernommen"));
        } else {
            ui.label(crate::trf!("{a} von {b} Masken übernommen", a = r.imported, b = r.total));
        }
        if r.imported != 0 { ui.label(crate::tr("Wirkung näherungsweise")); }
        if !r.unsupported.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(215, 181, 115), crate::tr("Nicht vollständig übernommen"));
            egui::CollapsingHeader::new(crate::tr("Details anzeigen")).show(ui, |ui| {
                for (key, count) in &r.unsupported { ui.label(format!("{}: {count}×", import_label(key))); }
            });
        } else if r.skipped != 0 {
            ui.label(crate::trf!("{a} deaktiviert", a = r.skipped));
        }
    });
}

fn import_label(key: &str) -> &str {
    match key {
        "LocalSharpness" => crate::tr("Negative Schärfe (Weichzeichnen)"),
        "LocalBlacks2012" => crate::tr("Lokaler Schwarzwert"),
        "LocalWhites2012" => crate::tr("Lokaler Weißwert"),
        "LocalBrightness" => crate::tr("Ältere lokale Helligkeit"),
        "CorrectionRangeMask" => crate::tr("Bereichsauswahl"),
        "MaskBlendMode" => crate::tr("Verknüpfung der Teilmasken"),
        "MaskValue" => crate::tr("Abweichende Maskendichte"),
        "Mask/Subject" => crate::tr("KI-Motivmaske"),
        "Mask/Sky" => crate::tr("KI-Himmelmaske"),
        "AutoMask" => crate::tr("Automatische Kantenmaske"),
        _ => crate::tr("Weitere nicht unterstützte Maskendaten"),
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    #[test]
    fn incomplete_import_warning_is_visible_without_a_local_document() {
        let ctx = egui::Context::default();
        let report = develop::settings::MaskImportReport {
            version: 1, total: 1, skipped: 1,
            unsupported: [("Mask/Subject".into(), 1)].into_iter().collect(),
            ..Default::default()
        };
        let out = crate::ui_test::run(&ctx, egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show_inside(ui, |ui| import_hint(ui, Some(&report)));
        });
        assert!(out.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::epaint::Shape::Text(text) if text.galley.text().contains("Nicht vollständig übernommen")
        )));
        assert_eq!(import_label("Mask/Subject"), crate::tr("KI-Motivmaske"));
    }
}
