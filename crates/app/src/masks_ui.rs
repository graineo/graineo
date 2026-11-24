//! Local masks: visible controls and image-space drawing, stored with development settings.
use egui::{Color32, Context, Ui};
use crate::lr;
use masks::{Adjustments, Document, Layer, Mask, Operation};
#[derive(Clone, Default)]
struct State {
    image: usize,
    /// selected mask (index of the layer)
    selected: usize,
    /// selected part of the selected mask (see `mask_parts`)
    part: usize,
    /// drawing switched on from outside (darkroom "Dodge / Burn"): the selected part is drawn, shapes anew
    draw: bool,
    show: bool,
    /// the tool of the selected part takes the pointer in the image ("the tool follows the selection"; Esc ends it)
    armed: bool,
    /// the selected gradient or ellipse was just created: a drag anywhere in the image draws it
    fresh: bool,
    /// what the selected part is (set by the panel, read by `painting`)
    tool: PartTool,
    /// brush for new strokes: radius (short side), hardness, flow, density; `None` = start values
    brush: Option<[f32; 4]>,
    /// dragged handle of a gradient or ellipse
    grab: Option<Handle>,
    /// part the stroke being painted goes into (the selected one, or the erasing part with ⌥)
    stroke_part: Option<usize>,
    /// "Auto-Maske" for the next strokes: they act only on colours like the one under their start
    auto: bool,
    /// tolerance of the auto mask (Oklab distance; `None` = start value)
    auto_tolerance: Option<f32>,
    /// a stroke with auto mask started before the image for the colour was there: its part and start point
    auto_pending: Option<(usize, [f32; 2])>,
    /// the pointer was over a handle in the last frame
    over_handle: bool,
    start: Option<[f32; 2]>,
    /// show the weights of the selected mask as a red overlay
    weights: bool,
    /// test hook GRAINEO_MASKS_TEST=subject: asked once
    test_asked: bool,
    /// click selection: clicks in the image add points (⌥: exclude)
    click: bool,
    /// the click selection creates a new mask with the next click (instead of adding to the selected one)
    click_new: bool,
    /// the click selection adds a new part with this operation to the selected mask with the next click
    click_part: Option<Operation>,
    /// row of the list under the pointer: its mask is shown in the image
    hover: Option<usize>,
    /// row being renamed and its text
    rename: Option<(usize, String)>,
    /// width / height of the photo (thumbnails), from the last image drawn
    aspect: f32,
    /// a pipette click waiting for the sample image: point, add (⇧), remove (⌥)
    pick: Option<([f32; 2], bool, bool)>,
    /// test options (`GRAINEO_MASKS_TEST=…;edge=2`) applied
    test_done: bool,
    /// a menu entry chosen in the last frame
    pending: Option<crate::mask_list_ui::RowAction>,
}

/// Kind of the selected part, for the tool in the image.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum PartTool {
    #[default]
    None,
    Brush,
    /// gradient or ellipse: handles in the image
    Shape,
    /// click selection: clicks add points
    Click,
    /// colour range: clicks take colour samples
    Pipette,
    /// depth mask: clicks take a segment in or out
    Segments,
}

/// Handles of a gradient (start, end) and an ellipse (centre, the ends of both axes).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Handle {
    Start,
    End,
    Center,
    AxisX,
    AxisY,
}

/// Start values of a colour range: tolerance and softness (Oklab units).
const COLOR_RADIUS: f32 = 0.06;
const COLOR_FEATHER: f32 = 0.06;
/// Oklab units of 100 on the sliders "Toleranz" and "Weichheit".
const COLOR_SCALE: f32 = 0.3;

/// Start values of the brush for new strokes: radius, hardness, flow, density.
const BRUSH: [f32; 4] = [0.04, 0.6, 0.2, 1.0];

/// Start value of the tolerance of the auto mask (Oklab distance where a stroke stops acting).
const AUTO_TOLERANCE: f32 = 0.15;

/// A stroke not drawn yet (placeholder point outside the photo) with the brush settings `b`.
fn new_stroke(b: [f32; 4]) -> Mask {
    Mask::Brush { points: vec![[-2.0, -2.0]], radius: b[0], hardness: b[1], flow: b[2], density: b[3], auto: None }
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
/// Does a tool of the masks take the pointer in the image (then a click or drag does not zoom or pan)?
pub fn painting(ctx: &Context) -> bool {
    let open = crate::retouch_ui::masks_open(ctx);
    ctx.data_mut(|d| {
        d.get_temp::<State>(key()).is_some_and(|s| {
            s.draw || s.click || (open && s.armed && (s.tool == PartTool::Brush || s.tool == PartTool::Pipette || s.tool == PartTool::Segments || (s.tool == PartTool::Shape && (s.fresh || s.over_handle || s.grab.is_some()))))
        })
    }) || crate::retouch_ui::active(ctx)
        || crate::remove_ui::active(ctx)
}
/// Leave the drawing tool without changing the stored mask.
pub fn finish(ctx: &Context) -> bool {
    ctx.data_mut(|d| {
        let Some(mut s) = d.get_temp::<State>(key()) else { return false };
        let active = s.draw || s.click || s.armed;
        s.draw = false;
        s.click = false;
        s.click_new = false;
        s.click_part = None;
        s.armed = false;
        s.fresh = false;
        s.grab = None;
        s.start = None;
        d.insert_temp(key(), s);
        active
    })
}
fn shape(kind: usize) -> Mask {
    match kind {
        0 => new_stroke(BRUSH),
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
        // a colour range waits for its first sample (pipette)
        _ => Mask::ColorRange { samples: Vec::new(), radius: COLOR_RADIUS, feather: COLOR_FEATHER },
    }
}
/// Slider of the mask tray: the slider of the Edit panel (double-click on name or track resets to `default`), without
/// the help cards of the global sliders of the same name. `label` is the German text (translated when drawn).
fn slider(ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, step: f64) {
    lr::slider_plain_f32(ui, label, v, range, default, step);
}
fn point(ui: &mut Ui, label: &str, p: &mut [f32; 2]) {
    ui.horizontal(|ui| {
        ui.add_space(lr::TEXT_INSET);
        ui.label(label);
        for v in p {
            crate::lr::text_field(ui, egui::DragValue::new(v).speed(0.005).range(-2.0..=2.0).clamp_existing_to_range(false));
        }
    });
}
/// Settings of the selected part: its own sliders, the numbers of a shape in the collapsed group "Genau".
/// Returns an analysis to start again (subject, people).
fn part_settings(ui: &mut Ui, s: &mut State, part: &mut Mask, path: &std::path::Path) -> Option<crate::subject_ui::Request> {
    use crate::mask_parts;
    let title = crate::tr(crate::mask_list_ui::kind_label(part));
    // the background is an inverted subject by name
    let inverted = matches!(part, Mask::Invert { .. }) && crate::mask_list_ui::kind_label(part) != "Hintergrund";
    if inverted {
        lr::category(ui, &format!("{title} · {}", crate::tr("invertiert")));
    } else {
        lr::category(ui, title);
    }
    if mask_parts::is_brush(part) {
        let mut b = s.brush.unwrap_or(BRUSH);
        let mut size = b[0] * 100.0;
        let mut hard = b[1] * 100.0;
        let mut flow = b[2] * 100.0;
        let mut density = b[3] * 100.0;
        let keys = |a: &str, b: &str| (crate::shortcuts::display(a), crate::shortcuts::display(b));
        let r = ui.scope(|ui| slider_pct(ui, "Größe (%)", &mut size, 0.1..=50.0, BRUSH[0] * 100.0, 0.1));
        let probe = ui.interact(r.response.rect, ui.scope_id().with("size_tip"), egui::Sense::hover());
        let (a, z) = keys("brush_smaller", "brush_larger");
        lr::hover_tip(ui, &probe, &crate::trf!("Größe ({a} kleiner, {b} größer)", a = a, b = z));
        let mut changed = r.inner;
        let r = ui.scope(|ui| slider_pct(ui, "Härte (%)", &mut hard, 0.0..=100.0, BRUSH[1] * 100.0, 1.0));
        let probe = ui.interact(r.response.rect, ui.scope_id().with("hardness_tip"), egui::Sense::hover());
        let (a, z) = keys("brush_softer", "brush_harder");
        lr::hover_tip(ui, &probe, &crate::trf!("Härte ({a} weicher, {b} härter)", a = a, b = z));
        changed |= r.inner;
        changed |= slider_pct(ui, "Fluss (%)", &mut flow, 1.0..=100.0, BRUSH[2] * 100.0, 1.0);
        let r = ui.scope(|ui| slider_pct(ui, "Dichte (%)", &mut density, 1.0..=100.0, BRUSH[3] * 100.0, 1.0));
        let probe = ui.interact(r.response.rect, ui.scope_id().with("density_tip"), egui::Sense::hover());
        lr::hover_tip(ui, &probe, crate::tr("Höchste Wirkung, auch beim Übermalen"));
        changed |= r.inner;
        // auto mask: a switch and its tolerance (always shown, locked while off: no jump in the tray)
        ui.horizontal(|ui| {
            ui.add_space(lr::TEXT_INSET);
            let r = lr::toggle(ui, &mut s.auto, "Auto-Maske");
            lr::hover_tip(ui, &r, crate::tr("Wirkt nur auf ähnliche Farben wie am Strichanfang"));
        });
        let mut tolerance = s.auto_tolerance.unwrap_or(AUTO_TOLERANCE) / COLOR_SCALE * 100.0;
        let t = ui.add_enabled_ui(s.auto, |ui| slider_pct(ui, "Toleranz (%)", &mut tolerance, 1.0..=100.0, AUTO_TOLERANCE / COLOR_SCALE * 100.0, 1.0));
        let probe = ui.interact(t.response.rect, ui.scope_id().with("auto_tolerance_tip"), egui::Sense::hover());
        lr::hover_tip(ui, &probe, crate::tr(if s.auto { "Wie ähnlich die Farbe sein muss" } else { "Erst „Auto-Maske“ einschalten" }));
        if t.inner {
            s.auto_tolerance = Some((tolerance / 100.0 * COLOR_SCALE).clamp(0.003, 0.3));
        }
        if changed {
            b = [(size / 100.0).clamp(0.001, 0.5), (hard / 100.0).clamp(0.0, 1.0), (flow / 100.0).clamp(0.01, 1.0), (density / 100.0).clamp(0.01, 1.0)];
            s.brush = Some(b);
            // a brush not drawn yet takes the settings at once
            let leaf = mask_parts::last_leaf(part);
            if matches!(leaf, Mask::Brush { points, .. } if mask_parts::fresh(points)) {
                *leaf = new_stroke(b);
            }
        }
        dim(ui, crate::tr("Gilt für die nächsten Striche · ⌘Z nimmt einen Strich zurück"));
        return None;
    }
    if matches!(part, Mask::Combine { .. }) {
        dim(ui, crate::tr("Aus mehreren Formen zusammengesetzt; einzelne Formen lassen sich hier nicht ändern"));
        return None;
    }
    let leaf = mask_parts::last_leaf(part);
    match leaf {
        Mask::Raster { .. } if crate::depth_ui::handles(leaf) => return crate::depth_ui::controls(ui, leaf, path),
        Mask::Raster { .. } => return crate::subject_ui::controls(ui, leaf, path),
        Mask::Linear { start, end } => {
            dim(ui, crate::tr("Anfang und Ende im Bild ziehen"));
            exact(ui, |ui| {
                point(ui, crate::tr("Anfang"), start);
                point(ui, crate::tr("Ende"), end);
            });
            if start == end {
                end[0] += 0.001;
            }
        }
        Mask::Radial { center, radii, angle, feather } => {
            slider(ui, "Weiche Kante", feather, 0.0..=1.0, 0.5, 0.01);
            exact(ui, |ui| {
                point(ui, crate::tr("Mittelpunkt"), center);
                slider(ui, "Breite", &mut radii[0], 0.001..=2.0, 0.25, 0.005);
                slider(ui, "Höhe", &mut radii[1], 0.001..=2.0, 0.2, 0.005);
                slider(ui, "Winkel", angle, -std::f32::consts::PI..=std::f32::consts::PI, 0.0, 0.01);
            });
        }
        Mask::Luminance { low, high, feather } => {
            slider(ui, "Von", low, 0.0..=2.0, 0.0, 0.01);
            slider(ui, "Bis", high, 0.0..=2.0, 0.5, 0.01);
            *high = high.max(*low);
            slider(ui, "Weiche Kante", feather, 0.0..=1.0, 0.2, 0.01);
        }
        Mask::ColorRange { samples, radius, feather } => {
            color_samples(ui, samples);
            let mut t = *radius / COLOR_SCALE * 100.0;
            if slider_pct(ui, "Toleranz", &mut t, 0.0..=100.0, COLOR_RADIUS / COLOR_SCALE * 100.0, 1.0) {
                *radius = (t / 100.0 * COLOR_SCALE).clamp(0.0, 2.0);
            }
            let mut f = *feather / COLOR_SCALE * 100.0;
            if slider_pct(ui, "Weichheit", &mut f, 0.0..=100.0, COLOR_FEATHER / COLOR_SCALE * 100.0, 1.0) {
                *feather = (f / 100.0 * COLOR_SCALE).clamp(0.0, 2.0);
            }
        }
        Mask::Color { reference, radius, feather } => {
            slider(ui, "Farbbereich", radius, 0.0..=1.0, 0.1, 0.01);
            slider(ui, "Weiche Kante", feather, 0.0..=1.0, 0.1, 0.01);
            exact(ui, |ui| {
                for (c, name) in reference.iter_mut().zip(["Rot", "Grün", "Blau"]) {
                    slider(ui, name, c, 0.0..=1.0, 0.18, 0.01);
                }
            });
        }
        _ => (),
    }
    None
}

/// The colour samples of a colour range as swatches (a click removes one), with a note while there is none.
fn color_samples(ui: &mut Ui, samples: &mut Vec<[f32; 3]>) {
    let h = 22.0;
    let w = lr::visible_width(ui);
    let (row, _) = ui.allocate_exact_size(egui::vec2(w, h + 4.0), egui::Sense::hover());
    if samples.is_empty() {
        ui.painter().text(row.left_center() + egui::vec2(lr::TEXT_INSET, 0.0), egui::Align2::LEFT_CENTER, crate::tr("Noch keine Probe: Farbe im Bild anklicken"), egui::FontId::proportional(11.0), lr::TEXT_DIM());
        return;
    }
    let mut remove = None;
    for (i, c) in samples.iter().enumerate() {
        let r = egui::Rect::from_min_size(row.min + egui::vec2(lr::TEXT_INSET + i as f32 * (h + 6.0), 2.0), egui::vec2(h, h));
        let resp = ui.interact(r, ui.scope_id().with(("color_sample", i)), egui::Sense::click());
        let srgb = |v: f32| (if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 }.clamp(0.0, 1.0) * 255.0).round() as u8;
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(srgb(c[0]), srgb(c[1]), srgb(c[2])));
        ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, if resp.hovered() { Color32::WHITE } else { Color32::from_gray(0x50) }), egui::StrokeKind::Outside);
        if resp.hovered() {
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(14.0), Color32::WHITE);
        }
        lr::hover_tip(ui, &resp, crate::tr("Probe entfernen"));
        if resp.clicked() {
            remove = Some(i);
        }
    }
    if let Some(i) = remove {
        samples.remove(i);
    }
}

/// A pipette click on a colour range: a plain click replaces the samples, ⇧ adds one (at most [`masks::SAMPLES_MAX`],
/// then the last is replaced), ⌥ removes the sample closest to the colour.
fn take_sample(samples: &mut Vec<[f32; 3]>, c: [f32; 3], add: bool, remove: bool) {
    if remove {
        let q = masks::oklab(c);
        if let Some(i) = (0..samples.len()).min_by(|&a, &b| masks::color_distance(q, masks::oklab(samples[a])).total_cmp(&masks::color_distance(q, masks::oklab(samples[b])))) {
            samples.remove(i);
        }
    } else if add {
        if samples.len() >= masks::SAMPLES_MAX {
            samples.pop();
        }
        samples.push(c);
    } else {
        *samples = vec![c];
    }
}

/// A box `r` (0..1, x0 y0 x1 y1) of the click selection: a new mask (`click_new`), a new part of the selected mask
/// (`click_part`) or the new box of the selected click selection (its points stay). Returns the layer to select.
fn boxed(local: &mut Option<Document>, s: &mut State, r: [f32; 4]) -> Option<usize> {
    use crate::mask_parts;
    let leaf = || Mask::Raster { raster: String::new(), source: masks::RasterSource::Click { points: Vec::new(), rect: Some(r) }, feather: 0.0, range: None, edge: 0.0 };
    let d = local.get_or_insert(Document { version: 2, layers: vec![] });
    let sel = if s.click_new || d.layers.is_empty() {
        if d.layers.len() >= 64 {
            return None;
        }
        d.layers.push(Layer::new(leaf(), Adjustments::default()));
        s.click_new = false;
        s.part = 0;
        d.layers.len() - 1
    } else if let Some(op) = s.click_part {
        let l = d.layers.get_mut(s.selected)?;
        let mut parts = mask_parts::split(&l.mask);
        if !parts.push(op, leaf()) {
            return None;
        }
        l.mask = mask_parts::join(&parts);
        s.part = parts.parts.len() - 1;
        s.click_part = None;
        s.tool = PartTool::Click;
        s.selected
    } else {
        let part = d.layers.get_mut(s.selected).and_then(|l| mask_parts::part_mut(&mut l.mask, s.part))?;
        let Some(Mask::Raster { raster, source: masks::RasterSource::Click { rect, .. }, .. }) = crate::subject_ui::first_raster(part) else { return None };
        *rect = Some(r);
        raster.clear();
        s.selected
    };
    d.settle_version();
    Some(sel)
}

/// Collapsed group "Genau" with the numbers of a shape (they are drawn in the image anyway).
fn exact(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    lr::section_local(ui, "mask-exact", "Genau", false, false, add);
}

fn slider_pct(ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, default: f32, step: f64) -> bool {
    lr::slider_plain_f32(ui, label, v, range, default, step)
}

/// A dimmed note line.
fn dim(ui: &mut Ui, text: &str) {
    lr::note(ui, text);
}

/// The sliders of a mask in the sections "Licht", "Kurve", "Farbe" (with colour grading), "Details" (heading
/// "Korrekturen"), each with ↺; neutral is 0 everywhere ([`masks::Adjustments::default`]), a double-click on name or
/// track resets a slider. `v4`: the sliders of document version 4 (Weiß, Schwarz, Kurve, Color Grading, Dunst) can
/// be used (not in version-1 documents, whose filters would change).
pub(crate) fn corrections(ui: &mut Ui, a: &mut Adjustments, v4: bool) {
    lr::category(ui, crate::tr("Korrekturen"));
    let locked = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        let r = ui.add_enabled_ui(v4, |ui| add(ui));
        if !v4 {
            let probe = ui.interact(r.response.rect, ui.scope_id().with("v4_why"), egui::Sense::hover());
            lr::hover_tip(ui, &probe, crate::tr("Erst „Filter auf Version 2 umstellen“ (⋯)"));
        }
    };
    // test hook option "open": all sections open (screenshots)
    let all = masks_test_opt("open").is_some();
    let light = a.exposure != 0.0 || a.contrast != 0.0 || a.highlights != 0.0 || a.shadows != 0.0 || a.whites != 0.0 || a.blacks != 0.0;
    if lr::section_local(ui, "mask-light", "Licht", true, light, |ui| {
        slider(ui, "Belichtung", &mut a.exposure, -3.0..=3.0, 0.0, 0.01);
        slider(ui, "Kontrast", &mut a.contrast, -1.0..=1.0, 0.0, 0.01);
        slider(ui, "Lichter", &mut a.highlights, -1.0..=1.0, 0.0, 0.01);
        slider(ui, "Tiefen", &mut a.shadows, -1.0..=1.0, 0.0, 0.01);
        locked(ui, &mut |ui| {
            slider(ui, "Weiß", &mut a.whites, -1.0..=1.0, 0.0, 0.01);
            slider(ui, "Schwarz", &mut a.blacks, -1.0..=1.0, 0.0, 0.01);
        });
    }) {
        (a.exposure, a.contrast, a.highlights, a.shadows, a.whites, a.blacks) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    }
    if lr::section_local(ui, "mask-curve", "Kurve", all, !a.curve.is_empty(), |ui| {
        locked(ui, &mut |ui| {
            let mut c = develop::ToneCurve { rgb: a.curve.clone(), ..Default::default() };
            if lr::tone_curve(ui, &mut c, 0, None) {
                a.curve = if masks::curve::is_identity(&c.rgb) { Vec::new() } else { c.rgb.into_iter().take(masks::CURVE_POINTS_MAX).collect() };
            }
        });
    }) {
        a.curve.clear();
    }
    let grading = [a.grade_shadows, a.grade_midtones, a.grade_highlights].iter().any(|g| g[1] > 0.0) || a.grade_balance != 0.0;
    let color = a.temperature != 0.0 || a.tint != 0.0 || a.saturation != 0.0 || grading;
    if lr::section_local(ui, "mask-color", "Farbe", all, color, |ui| {
        slider(ui, "Temperatur", &mut a.temperature, -1.0..=1.0, 0.0, 0.01);
        slider(ui, "Tönung", &mut a.tint, -1.0..=1.0, 0.0, 0.01);
        slider(ui, "Sättigung", &mut a.saturation, -1.0..=1.0, 0.0, 0.01);
        locked(ui, &mut |ui| {
            // the wheels of the global colour grading (drag: hue and strength, ⇧: strength only, double-click: neutral)
            lr::note(ui, crate::tr("Color Grading"));
            let w = ((lr::visible_width(ui) - 16.0) / 3.0).min(100.0);
            ui.horizontal(|ui| {
                ui.add_space(lr::TEXT_INSET);
                for (label, g) in [("Schatten", &mut a.grade_shadows), ("Mitte", &mut a.grade_midtones), ("Lichter", &mut a.grade_highlights)] {
                    if lr::color_wheel(ui, crate::tr(label), g, w) && g[1] == 0.0 {
                        // no strength: written as neutral (nothing of version 4 left)
                        *g = [0.0, 0.0];
                    }
                }
            });
            slider(ui, "Balance", &mut a.grade_balance, -1.0..=1.0, 0.0, 0.01);
        });
    }) {
        (a.temperature, a.tint, a.saturation) = (0.0, 0.0, 0.0);
        (a.grade_shadows, a.grade_midtones, a.grade_highlights, a.grade_balance) = ([0.0; 2], [0.0; 2], [0.0; 2], 0.0);
    }
    let details = a.clarity != 0.0 || a.texture != 0.0 || a.dehaze != 0.0 || a.sharpness != 0.0 || a.noise_reduction != 0.0;
    if lr::section_local(ui, "mask-details", "Details", all, details, |ui| {
        slider(ui, "Klarheit", &mut a.clarity, -1.0..=1.0, 0.0, 0.01);
        slider(ui, "Struktur", &mut a.texture, -1.0..=1.0, 0.0, 0.01);
        locked(ui, &mut |ui| slider(ui, "Dunst entfernen", &mut a.dehaze, -1.0..=1.0, 0.0, 0.01));
        slider(ui, "Schärfe", &mut a.sharpness, 0.0..=1.0, 0.0, 0.01);
        slider(ui, "Rauschminderung", &mut a.noise_reduction, 0.0..=1.0, 0.0, 0.01);
    }) {
        (a.clarity, a.texture, a.dehaze, a.sharpness, a.noise_reduction) = (0.0, 0.0, 0.0, 0.0, 0.0);
    }
}

/// What kind of tool part `m` needs in the image.
fn tool_of(m: &Mask) -> PartTool {
    if crate::mask_parts::is_brush(m) {
        return PartTool::Brush;
    }
    match m {
        Mask::Invert { mask } => tool_of(mask),
        Mask::Linear { .. } | Mask::Radial { .. } => PartTool::Shape,
        Mask::Raster { source: masks::RasterSource::Click { .. }, .. } => PartTool::Click,
        Mask::Raster { source: masks::RasterSource::Depth { .. }, .. } => PartTool::Segments,
        Mask::ColorRange { .. } => PartTool::Pipette,
        _ => PartTool::None,
    }
}

/// Select part `i` of `mask`: its tool takes the pointer in the image (brush paints, gradient and ellipse show handles,
/// a click selection takes clicks).
fn select_part(s: &mut State, mask: &Mask, i: usize) {
    s.part = i;
    s.fresh = false;
    s.grab = None;
    s.start = None;
    s.click_new = false;
    s.click_part = None;
    s.tool = crate::mask_parts::part(mask, i).map_or(PartTool::None, tool_of);
    s.click = s.tool == PartTool::Click;
    s.armed = s.tool != PartTool::None;
}

/// "Teile · ‹Name›": the parts of the mask with their operation (● first, + add, − subtract, ∩ intersect), a menu per
/// part, and one row "+ Hinzufügen ▾ · − Abziehen ▾ · ∩ Schneiden ▾" with the tiles. Returns an analysis to start
/// and whether the document version may have changed.
fn parts_section(ui: &mut Ui, s: &mut State, layer: &mut Layer, name: &str, path: &std::path::Path) -> (Option<crate::subject_ui::Request>, bool) {
    use crate::mask_parts;
    let mut request = None;
    let inverted = mask_parts::inverted(&layer.mask) && !crate::mask_list_ui::is_background(&layer.mask);
    let title = if inverted { crate::trf!("Teile · {name} · invertiert", name = name) } else { crate::trf!("Teile · {name}", name = name) };
    lr::category(ui, &title);
    let ops = mask_parts::operations(&layer.mask);
    s.part = s.part.min(ops.len() - 1);
    s.tool = mask_parts::part(&layer.mask, s.part).map_or(PartTool::None, tool_of);
    let parts = mask_parts::split(&layer.mask);
    let mut action: Option<(usize, PartAction)> = None;
    let w = lr::visible_width(ui);
    for (i, op) in ops.iter().enumerate() {
        let part = &parts.parts[i].1;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
        let p = ui.painter();
        if i == s.part {
            p.rect_filled(rect, 0.0, Color32::from_gray(0x30));
        } else if resp.hovered() {
            p.rect_filled(rect, 0.0, Color32::from_gray(0x22));
        }
        let sym = match op {
            None => "●",
            Some(Operation::Add) => "+",
            Some(Operation::Subtract) => "−",
            Some(Operation::Intersect) => "∩",
        };
        let c = if i == s.part { Color32::WHITE } else { lr::TEXT() };
        // the symbol (its ink, whichever font draws it) starts at the text edge of the tray
        let g = p.layout_no_wrap(sym.to_string(), egui::FontId::proportional(13.0), c);
        let ink = g.rows.first().and_then(|r| r.row.glyphs.first().map(|q| r.pos.x + q.pos.x + q.uv_rect.offset.x)).unwrap_or(0.0);
        p.galley(egui::pos2(rect.left() + lr::TEXT_INSET - ink, rect.center().y - g.size().y / 2.0), g, c);
        let mut text = crate::tr(crate::mask_list_ui::kind_label(part)).to_string();
        let detail = if mask_parts::is_brush(part) {
            crate::trf!("{n} Striche", n = mask_parts::strokes(part))
        } else if matches!(part, Mask::Invert { .. }) {
            crate::tr("invertiert").to_string()
        } else {
            String::new()
        };
        if !detail.is_empty() {
            text.push_str(" · ");
            text.push_str(&detail);
        }
        let font = egui::TextStyle::Body.resolve(ui.style());
        let shown = lr::middle_ellipsis_font(p, &text, &font, rect.width() - 60.0);
        p.text(egui::pos2(rect.left() + 28.0, rect.center().y), egui::Align2::LEFT_CENTER, shown, font, c);
        let more = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(24.0, 22.0));
        let m = ui.interact(more, ui.scope_id().with(("part_menu", i)), egui::Sense::click());
        if m.hovered() {
            ui.painter().rect_filled(more, 0.0, Color32::from_gray(0x44));
        }
        ui.painter().text(more.center(), egui::Align2::CENTER_CENTER, "⋯", egui::FontId::proportional(13.0), lr::TEXT_DIM());
        lr::hover_tip(ui, &m, crate::tr("Teil: invertieren, Verknüpfung ändern, löschen"));
        let menu = |ui: &mut Ui| -> Option<PartAction> {
            let mut a = None;
            if lr::menu_item(ui, crate::tr("Teil invertieren"), "", true) {
                a = Some(PartAction::Invert);
            }
            ui.separator();
            for (o, t) in [(Operation::Add, "Als „Hinzufügen“"), (Operation::Subtract, "Als „Abziehen“"), (Operation::Intersect, "Als „Schneiden“")] {
                if lr::menu_item_why(ui, crate::tr(t), "", i > 0 && *op != Some(o), crate::tr(if i == 0 { "Der erste Teil ist die Grundlage" } else { "" })) {
                    a = Some(PartAction::Operation(o));
                }
            }
            ui.separator();
            let why = if parts.parts.len() == 1 { "Einziger Teil: „Maske löschen“ (⋯ der Maske)" } else { "Ohne die Grundlage wirkte der nächste Teil auf das ganze Foto" };
            if lr::menu_item_why(ui, crate::tr("Teil löschen"), "", parts.can_remove(i), crate::tr(why)) {
                a = Some(PartAction::Delete);
            }
            a
        };
        egui::Popup::menu(&m).id(egui::Id::unique(("part_menu_popup", i))).show(|ui| {
            if let Some(a) = menu(ui) {
                action = Some((i, a));
            }
        });
        resp.context_menu(|ui| {
            if let Some(a) = menu(ui) {
                action = Some((i, a));
            }
        });
        if resp.clicked() && !m.hovered() {
            action = Some((i, PartAction::Select));
        }
    }
    ui.add_space(4.0);
    // add a part: the operation is the button, the kind the tile (no choice made elsewhere decides it)
    let mut add: Option<(Operation, crate::mask_list_ui::NewKind)> = None;
    let gap = ui.spacing().item_spacing.x;
    let small = 52.0;
    let bw = ((w - small - 2.0 * gap) / 2.0).floor();
    ui.horizontal(|ui| {
        for (op, label, tip, width) in [
            (Operation::Add, "+ Hinzufügen ▾", "Teil hinzufügen: vergrößert die Maske", bw),
            (Operation::Subtract, "− Abziehen ▾", "Teil abziehen: nimmt aus der Maske heraus", bw),
            (Operation::Intersect, "∩ ▾", "Mit einem Teil schneiden: wirkt nur, wo beide wirken", small),
        ] {
            let r = ui.add_sized([width, 24.0], egui::Button::new(crate::tr(label)));
            lr::hover_tip(ui, &r, crate::tr(tip));
            egui::Popup::menu(&r).id(egui::Id::unique(("add_part_menu", op as u8))).show(|ui| {
                ui.set_width(w.max(240.0));
                if let Some(k) = crate::mask_list_ui::tiles(ui, &format!("add_part_{}", op as u8), path) {
                    add = Some((op, k));
                    ui.close();
                }
            });
        }
    });
    let mut changed = false;
    if let Some((i, a)) = action {
        let mut q = parts.clone();
        match a {
            PartAction::Select => select_part(s, &layer.mask, i),
            PartAction::Invert => {
                q.invert(i);
                layer.mask = mask_parts::join(&q);
            }
            PartAction::Operation(o) => {
                if q.set_operation(i, o) {
                    layer.mask = mask_parts::join(&q);
                }
            }
            PartAction::Delete => {
                if q.remove(i) {
                    layer.mask = mask_parts::join(&q);
                    let n = mask_parts::count(&layer.mask);
                    select_part(s, &layer.mask, selection_after_delete(s.part, i, n));
                    changed = true;
                }
            }
        }
    }
    if let Some((op, kind)) = add {
        request = add_part(s, layer, op, kind);
        changed = true;
    }
    (request, changed)
}

/// Entries of a part's menu (and a click on it).
#[derive(Clone, Copy, PartialEq, Debug)]
enum PartAction {
    Select,
    Invert,
    Operation(Operation),
    Delete,
}

/// A new part of `kind` with `op` at the end of the mask, selected at once. A click selection starts with the next
/// click in the image; subject and people return the analysis to start.
fn add_part(s: &mut State, layer: &mut Layer, op: Operation, kind: crate::mask_list_ui::NewKind) -> Option<crate::subject_ui::Request> {
    use crate::mask_list_ui::NewKind as K;
    let mut parts = crate::mask_parts::split(&layer.mask);
    let (leaf, request) = match kind {
        K::Click => {
            s.click = true;
            s.click_part = Some(op);
            s.click_new = false;
            s.weights = true;
            return None;
        }
        K::Subject | K::Background | K::People => {
            let (k, source) = if kind == K::People {
                (segment::Kind::Person, masks::RasterSource::Person { instances: masks::Instances::All })
            } else {
                (segment::Kind::Subject, masks::RasterSource::Subject { instances: masks::Instances::All })
            };
            let leaf = Mask::Raster { raster: String::new(), source, feather: 0.002, range: None, edge: 0.0 };
            let leaf = if kind == K::Background { Mask::Invert { mask: Box::new(leaf) } } else { leaf };
            (leaf, Some(crate::subject_ui::Request::Analyse(k)))
        }
        K::Part(p) => (Mask::Raster { raster: String::new(), source: masks::RasterSource::PersonPart { part: p }, feather: 0.0, range: None, edge: 0.0 }, Some(crate::subject_ui::Request::Parts)),
        K::Depth => {
            (crate::depth_ui::depth_leaf(), Some(crate::subject_ui::Request::Scene(crate::depth_ui::What::Depth, crate::depth_ui::STANDARD.into())))
        }
        K::Sky => {
            (crate::depth_ui::sky_leaf(), Some(crate::subject_ui::Request::Scene(crate::depth_ui::What::Sky, crate::depth_ui::STANDARD.into())))
        }
        K::Brush => (new_stroke(s.brush.unwrap_or(BRUSH)), None),
        K::Linear => (shape(1), None),
        K::Radial => (shape(2), None),
        K::Luminance => (shape(3), None),
        K::Color => (shape(4), None),
    };
    if !parts.push(op, leaf) {
        return None;
    }
    layer.mask = crate::mask_parts::join(&parts);
    let i = parts.parts.len() - 1;
    select_part(s, &layer.mask, i);
    s.fresh = matches!(kind, K::Linear | K::Radial);
    if request.is_some() {
        s.weights = true;
    }
    request
}

/// "Stärke" of a mask (opacity, 0–100 %, double-click: 100 %) and "Kante verfeinern" (0–100 %, double-click: 0);
/// locked for version-1 documents, the refined edge also for masks with a part from an image analysis. Returns:
/// changed.
fn strength(ui: &mut Ui, layer: &mut Layer, fields: bool) -> bool {
    lr::category(ui, crate::tr("Wirkung"));
    let mut pct = layer.amount * 100.0;
    let r = ui.add_enabled_ui(fields, |ui| lr::slider_plain_f32(ui, "Stärke (%)", &mut pct, 0.0..=100.0, 100.0, 1.0));
    if !fields {
        let probe = ui.interact(r.response.rect, ui.scope_id().with("strength_why"), egui::Sense::hover());
        lr::hover_tip(ui, &probe, crate::tr("Erst „Filter auf Version 2 umstellen“ (⋯)"));
    }
    if r.inner {
        layer.amount = (pct / 100.0).clamp(0.0, 1.0);
    }
    // the edge follows the edges of the picture (drawn masks and ranges only)
    let analysis = layer.mask.has_raster();
    let open = fields && !analysis;
    let mut fine = if analysis { 0.0 } else { layer.refine * 100.0 };
    let f = ui.add_enabled_ui(open, |ui| lr::slider_plain_f32(ui, "Kante verfeinern (%)", &mut fine, 0.0..=100.0, 0.0, 1.0));
    let probe = ui.interact(f.response.rect, ui.scope_id().with("refine_why"), egui::Sense::hover());
    lr::hover_tip(
        ui,
        &probe,
        crate::tr(if !fields {
            "Erst „Filter auf Version 2 umstellen“ (⋯)"
        } else if analysis {
            "Nur für gezeichnete Masken und Bereiche; Masken aus der Bildanalyse (Motiv, Personen, Klick-Auswahl, Tiefe, Himmel) behalten ihre Kante"
        } else {
            "Die Maskenkante folgt in einem schmalen Band den Kanten im Bild (geführter Filter); mehr = breiteres Band"
        }),
    );
    if f.inner && open {
        layer.refine = (fine / 100.0).clamp(0.0, 1.0);
    }
    r.inner || f.inner
}

/// A new mask of `kind` from the tiles: selected at once; a click selection starts with the first click.
/// Returns an analysis to start (subject, people).
fn create(s: &mut State, local: &mut Option<Document>, kind: crate::mask_list_ui::NewKind) -> Option<crate::subject_ui::Request> {
    use crate::mask_list_ui::NewKind as K;
    s.rename = None;
    match kind {
        K::Subject | K::Background | K::People => {
            let k = if kind == K::People { segment::Kind::Person } else { segment::Kind::Subject };
            if crate::subject_ui::new_layer(local, k) {
                // the background: the same leaf inverted, in one step
                if kind == K::Background {
                    if let Some(l) = local.as_mut().and_then(|d| d.layers.last_mut()) {
                        l.mask = Mask::Invert { mask: Box::new(std::mem::replace(&mut l.mask, Mask::Linear { start: [0.0, 0.0], end: [1.0, 1.0] })) };
                    }
                }
                s.selected = local.as_ref().map_or(0, |d| d.layers.len() - 1);
                s.draw = false;
                s.click = false;
                s.armed = false;
                s.part = 0;
                s.weights = true;
                return Some(crate::subject_ui::Request::Analyse(k));
            }
        }
        K::Part(p) => {
            let d = local.get_or_insert(Document { version: 2, layers: vec![] });
            if d.layers.len() < 64 {
                d.layers.push(Layer::new(Mask::Raster { raster: String::new(), source: masks::RasterSource::PersonPart { part: p }, feather: 0.0, range: None, edge: 0.0 }, Adjustments::default()));
                d.settle_version();
                s.selected = d.layers.len() - 1;
                s.draw = false;
                s.click = false;
                s.armed = false;
                s.part = 0;
                s.weights = true;
                return Some(crate::subject_ui::Request::Parts);
            }
        }
        K::Depth | K::Sky => {
            let what = if kind == K::Depth { crate::depth_ui::What::Depth } else { crate::depth_ui::What::Sky };
            if crate::depth_ui::new_layer(local, what) {
                s.selected = local.as_ref().map_or(0, |d| d.layers.len() - 1);
                s.draw = false;
                s.click = false;
                // a depth mask takes clicks on segments at once
                s.armed = kind == K::Depth;
                s.tool = if kind == K::Depth { PartTool::Segments } else { PartTool::None };
                s.part = 0;
                s.weights = true;
                return Some(crate::subject_ui::Request::Scene(what, crate::depth_ui::STANDARD.into()));
            }
        }
        K::Click => {
            // the first click in the image creates the mask (never adds to the selected one)
            s.click = true;
            s.click_new = true;
            s.click_part = None;
            s.armed = false;
            s.draw = false;
            s.weights = true;
        }
        _ => {
            let n = match kind {
                K::Brush => 0,
                K::Linear => 1,
                K::Radial => 2,
                K::Luminance => 3,
                _ => 4,
            };
            let d = local.get_or_insert(Document { version: 2, layers: vec![] });
            if d.layers.len() < 64 {
                let m = match (n, s.brush) {
                    (0, Some(b)) => new_stroke(b),
                    _ => shape(n),
                };
                d.layers.push(Layer::new(m, Adjustments::default()));
                d.settle_version();
                s.selected = d.layers.len() - 1;
                // brush and gradients are drawn in the image right away
                let mask = d.layers[s.selected].mask.clone();
                select_part(s, &mask, 0);
                s.fresh = n == 1 || n == 2;
            }
        }
    }
    None
}

/// Carry out an action of the list or of a mask's menu.
fn apply_row(s: &mut State, d: &mut Document, a: crate::mask_list_ui::RowAction) {
    use crate::mask_list_ui::{LayerAction as A, RowAction as R};
    match a {
        R::Select(i) => {
            if i != s.selected {
                s.selected = i;
                s.draw = false;
                if let Some(l) = d.layers.get(i) {
                    select_part(s, &l.mask, 0);
                }
            }
        }
        R::Toggle(i) => {
            if let Some(l) = d.layers.get_mut(i) {
                if d.version >= 2 {
                    l.enabled = !l.enabled;
                }
            }
        }
        R::Renamed(i, name) => {
            if let Some(l) = d.layers.get_mut(i) {
                if d.version >= 2 {
                    l.name = name.map(|n| n.chars().filter(|c| !c.is_control()).take(masks::NAME_MAX).collect());
                }
            }
        }
        R::Menu(i, a) => {
            if i >= d.layers.len() {
                return;
            }
            match a {
                A::Rename => s.rename = Some((i, crate::mask_list_ui::display_name(&d.layers[i]))),
                A::Duplicate if d.layers.len() < 64 => {
                    let copy = d.layers[i].clone();
                    d.layers.insert(i + 1, copy);
                    s.selected = i + 1;
                }
                A::Duplicate => {}
                A::Invert => {
                    let l = &mut d.layers[i];
                    l.mask = match l.mask.clone() {
                        Mask::Invert { mask } => *mask,
                        m => Mask::Invert { mask: Box::new(m) },
                    };
                }
                A::Up if i > 0 => {
                    d.layers.swap(i, i - 1);
                    s.selected = i - 1;
                }
                A::Down if i + 1 < d.layers.len() => {
                    d.layers.swap(i, i + 1);
                    s.selected = i + 1;
                }
                A::Up | A::Down => {}
                A::ResetSliders => d.layers[i].adjustments = Adjustments::default(),
                A::Delete => {
                    d.layers.remove(i);
                    s.selected = selection_after_delete(s.selected, i, d.layers.len());
                    s.draw = false;
                    s.click = false;
                    s.armed = false;
                    s.part = 0;
                    s.start = None;
                    s.rename = None;
                }
                A::ToVersion2 => {
                    if d.version == 1 {
                        d.version = 2;
                    }
                }
            }
        }
    }
    d.settle_version();
}

/// What the image does with the pointer right now (one fixed line, empty when nothing is armed).
fn tool_line(ui: &mut Ui, s: &State, layer: Option<&Layer>) {
    let _ = layer;
    let text = if s.click && s.click_new {
        crate::tr("Klick-Auswahl: Objekt anklicken oder Rahmen ziehen – neue Maske · Esc beendet")
    } else if s.click && s.click_part.is_some() {
        crate::tr("Klick-Auswahl: Objekt anklicken oder Rahmen ziehen – neuer Teil · Esc beendet")
    } else if s.click {
        crate::tr("Klick-Auswahl: Klick ergänzt, ⌥-Klick nimmt heraus, Ziehen setzt den Rahmen")
    } else if s.draw {
        crate::tr("Im Bild zeichnen: Pinsel malt, Verlauf und Ellipse ziehen · Esc beendet")
    } else if s.armed && s.tool == PartTool::Pipette && s.pick.is_some() {
        crate::tr("Pipette: Bild wird für die Probe entwickelt …")
    } else if s.armed && s.tool == PartTool::Pipette {
        crate::tr("Pipette: Farbe anklicken · ⇧ dazu · ⌥ weg")
    } else if s.armed && s.tool == PartTool::Brush {
        crate::tr("Pinsel malt · ⌥ radiert · ⇧-Klick Linie")
    } else if s.armed && s.tool == PartTool::Segments {
        crate::tr("Tiefe: Klick nimmt ein Segment dazu oder weg · Esc beendet")
    } else if s.armed && s.tool == PartTool::Shape && s.fresh {
        crate::tr("Im Bild ziehen, um die Form zu setzen · Esc beendet")
    } else if s.armed && s.tool == PartTool::Shape {
        crate::tr("Griffe im Bild ziehen · Esc beendet")
    } else if s.tool == PartTool::Brush || s.tool == PartTool::Shape || s.tool == PartTool::Pipette || s.tool == PartTool::Segments {
        crate::tr("Teil anklicken, um im Bild zu zeichnen")
    } else {
        ""
    };
    let h = 18.0;
    let (r, _) = ui.allocate_exact_size(egui::vec2(lr::visible_width(ui), h), egui::Sense::hover());
    if !text.is_empty() {
        let font = egui::FontId::proportional(10.5);
        let shown = lr::middle_ellipsis_font(ui.painter(), text, &font, r.width() - lr::TEXT_INSET - 2.0);
        ui.painter().text(r.left_center() + egui::vec2(lr::TEXT_INSET, 0.0), egui::Align2::LEFT_CENTER, shown, font, lr::BLUE_TEXT);
    }
}

/// What the image analysis is doing (step, seconds, "Abbrechen") or its last result – always one line.
fn status_line(ui: &mut Ui, path: &std::path::Path) {
    use crate::analysis_progress::Tool;
    let last = crate::subject_ui::message(path).or_else(|| crate::face_parts::message(path));
    crate::analysis_progress::status_line(ui, path, &[Tool::Subject, Tool::Person, Tool::Click, Tool::Depth, Tool::Sky, Tool::Sample, Tool::Parts], last.as_deref());
}

/// ⌫ / Del pressed while the "Masks" tool is open and no text field has the focus (then they delete the selected
/// mask, not the photo).
fn takes_delete_keys(ctx: &Context) -> bool {
    takes_delete(ctx)
        && !ctx.egui_wants_keyboard_input()
        && ctx.input(|i| i.modifiers.is_none() && (i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)))
}

/// Do ⌫ / Del belong to the masks (the "Masks" tool is open and the photo has a mask)?
pub fn takes_delete(ctx: &Context) -> bool {
    crate::retouch_ui::masks_open(ctx) && ctx.data_mut(|d| d.get_temp::<bool>(egui::Id::unique("masks_has_layers")).unwrap_or(false))
}

/// Fingerprint of a mask for caches (thumbnails, overlay): every number and kind, without writing JSON.
pub fn stamp(m: &Mask) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    fn walk(m: &Mask, h: &mut std::collections::hash_map::DefaultHasher) {
        let f = |v: &[f32], h: &mut std::collections::hash_map::DefaultHasher| v.iter().for_each(|x| x.to_bits().hash(h));
        std::mem::discriminant(m).hash(h);
        match m {
            Mask::Brush { points, radius, hardness, flow, density, auto } => {
                points.len().hash(h);
                points.iter().for_each(|p| f(p, h));
                f(&[*radius, *hardness, *flow, *density], h);
                if let Some(a) = auto {
                    f(&[a.reference[0], a.reference[1], a.reference[2], a.tolerance], h);
                }
            }
            Mask::Linear { start, end } => f(&[start[0], start[1], end[0], end[1]], h),
            Mask::Radial { center, radii, angle, feather } => f(&[center[0], center[1], radii[0], radii[1], *angle, *feather], h),
            Mask::Luminance { low, high, feather } => f(&[*low, *high, *feather], h),
            Mask::Color { reference, radius, feather } => f(&[reference[0], reference[1], reference[2], *radius, *feather], h),
            Mask::ColorRange { samples, radius, feather } => {
                samples.len().hash(h);
                samples.iter().for_each(|c| f(c, h));
                f(&[*radius, *feather], h);
            }
            Mask::Combine { operation, left, right } => {
                (*operation as u8).hash(h);
                walk(left, h);
                walk(right, h);
            }
            Mask::Invert { mask } => walk(mask, h),
            Mask::Raster { raster, source, feather, range, edge } => {
                raster.hash(h);
                format!("{source:?}").hash(h);
                f(&[*feather, *edge], h);
                range.map(|r| f(&r, h));
                // 8 or 16 bit after the rule (a depth map never asks the 8-bit store and its library reader)
                masks::raster::present(raster, source).hash(h);
            }
        }
    }
    walk(m, &mut h);
    h.finish()
}


/// The mask section of the "Masken" tool. Returns an analysis to start for the photo at `path` (subject, people), which
/// the caller starts with the photo's settings ([`crate::subject_ui::start`]).
pub fn panel(ui: &mut Ui, local: &mut Option<Document>, index: usize, path: &std::path::Path) -> Option<crate::subject_ui::Request> {
    let mut request = crate::subject_ui::update(local, path);
    if crate::face_parts::update(local, path) && request.is_none() {
        request = Some(crate::subject_ui::Request::Parts);
    }
    let test = masks_test().as_deref() == Ok("1");
    if test && local.is_none() {
        let mut doc =
            Document::from_json(include_str!("../../masks/samples/masks-example.json")).unwrap();
        doc.version = 2;
        *local = Some(doc);
    }
    let mut s = state(ui.ctx(), index);
    if masks_test().as_deref() == Ok("subject") && !s.test_asked && local.is_none() {
        s.test_asked = true;
        s.weights = true;
        if crate::subject_ui::new_layer(local, segment::Kind::Subject) {
            request = Some(crate::subject_ui::Request::Analyse(segment::Kind::Subject));
        }
    }
    // test hook GRAINEO_MASKS_TEST=depth|sky (screenshots): a new depth or sky mask, computed at once
    if let Some(what) = masks_test().ok().and_then(|v| match v.as_str() {
        "depth" => Some(crate::depth_ui::What::Depth),
        "sky" => Some(crate::depth_ui::What::Sky),
        _ => None,
    }) {
        if !s.test_asked && local.is_none() && crate::depth_ui::new_layer(local, what) {
            s.test_asked = true;
            s.weights = true;
            s.armed = what == crate::depth_ui::What::Depth;
            request = Some(crate::subject_ui::Request::Scene(what, crate::depth_ui::STANDARD.into()));
        }
    }
    // test hook GRAINEO_MASKS_TEST=click:x,y – one click of the click selection at x, y (0..1 of the photo)
    if let Some(p) = masks_test().ok().and_then(|v| v.strip_prefix("click:").map(str::to_string)) {
        if !s.test_asked && local.is_none() && crate::subject_ui::click_available() {
            s.test_asked = true;
            s.weights = true;
            s.click = true;
            let v: Vec<f32> = p.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 2 {
                if let Some(sel) = crate::subject_ui::clicked_point(local, 0, [v[0], v[1]], false) {
                    s.selected = sel;
                }
            }
        }
    }
    // test hook GRAINEO_MASKS_TEST=background | part:<face|skin|eyes|brows|lips|mouth>: that new mask
    if !s.test_asked && local.is_none() {
        let kind = match masks_test().as_deref() {
            Ok("background") => Some(crate::mask_list_ui::NewKind::Background),
            Ok(v) => v.strip_prefix("part:").and_then(|p| masks::PersonPart::ALL.into_iter().find(|q| format!("{q:?}").eq_ignore_ascii_case(p))).map(crate::mask_list_ui::NewKind::Part),
            Err(_) => None,
        };
        if let Some(k) = kind {
            s.test_asked = true;
            if let Some(r) = create(&mut s, local, k) {
                request = Some(r);
            }
            s.weights = true;
        }
    }
    // test hook GRAINEO_MASKS_TEST=box:x0,y0,x1,y1 – a click selection with a box (0..1 of the photo)
    if let Some(p) = masks_test().ok().and_then(|v| v.strip_prefix("box:").map(str::to_string)) {
        if !s.test_asked && local.is_none() && crate::subject_ui::click_available() {
            s.test_asked = true;
            s.weights = true;
            s.click = true;
            s.click_new = true;
            let v: Vec<f32> = p.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 4 {
                if let Some(sel) = boxed(local, &mut s, [v[0], v[1], v[2], v[3]]) {
                    s.selected = sel;
                }
            }
        }
    }
    // test hook GRAINEO_MASKS_TEST="color;pick=x,y|x,y": a colour range, samples at these points (the first replaces,
    // the others add) as soon as the image for the samples is there
    if masks_test().as_deref() == Ok("color") && !s.test_asked && local.is_none() {
        s.test_asked = true;
        s.weights = true;
        create(&mut s, local, crate::mask_list_ui::NewKind::Color);
    }
    if masks_test().as_deref() == Ok("color") && !s.test_done {
        if let (Some(img), Some(points)) = (crate::mask_sample::image(index), masks_test_opt("pick")) {
            if let Some(Mask::ColorRange { samples, .. }) = local.as_mut().and_then(|d| d.layers.last_mut()).map(|l| &mut l.mask) {
                for (i, p) in points.split('|').enumerate() {
                    let v: Vec<f32> = p.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if v.len() == 2 {
                        take_sample(samples, crate::mask_sample::sample(&img, [v[0], v[1]]), i > 0, false);
                    }
                }
            }
            s.test_done = true;
        }
    }
    // test hook GRAINEO_MASKS_TEST="brush;ring=x,y" (screenshots): a brush mask of a few strokes and an erasing part,
    // the brush ready to paint (overlay by itself), its ring at x, y (0..1 of the photo) while the pointer is elsewhere
    if masks_test().as_deref() == Ok("brush") && local.is_none() && !s.test_asked {
        s.test_asked = true;
        let wave = |y: f32, phase: f32| (0..60).map(|i| [0.12 + 0.76 * i as f32 / 59.0, y + 0.05 * (i as f32 * 0.25 + phase).sin()]).collect::<Vec<_>>();
        let stroke = |points, density| Mask::Brush { points, radius: 0.05, hardness: 0.6, flow: 0.35, density, auto: None };
        let painted = Mask::Combine { operation: Operation::Add, left: Box::new(stroke(wave(0.35, 0.0), 1.0)), right: Box::new(stroke(wave(0.5, 1.5), 0.5)) };
        let erased = stroke(vec![[0.5, 0.2], [0.5, 0.65]], 1.0);
        let mut d = Document { version: 2, layers: vec![Layer::new(Mask::Combine { operation: Operation::Subtract, left: Box::new(painted), right: Box::new(erased) }, Adjustments { exposure: 0.6, ..Default::default() })] };
        d.settle_version();
        let mask = d.layers[0].mask.clone();
        *local = Some(d);
        s.selected = 0;
        select_part(&mut s, &mask, 0);
        s.brush = Some([0.05, 0.6, 0.35, 1.0]);
    }
    // test hook GRAINEO_MASKS_TEST=demo|menu-demo: three masks (named, strength 80 %, one switched off) for screenshots
    if masks_test().is_ok_and(|v| v.ends_with("demo")) && local.is_none() && !s.test_asked {
        *local = Some(demo());
        s.weights = true;
        s.test_asked = !masks_test().is_ok_and(|v| v.starts_with("menu"));
    }
    // ⇧O: overlay on, then through the kinds of showing it, then off
    if crate::shortcuts::pressed(ui.ctx(), "mask_overlay") {
        let (on, mode) = next_overlay(overlay_shown(ui.ctx(), &s, brush_ready(&s)), overlay_mode(ui.ctx()));
        set_overlay(ui.ctx(), &mut s, on);
        set_overlay_mode(ui.ctx(), mode);
    }
    if let Some(m) = masks_test_opt("overlay").and_then(|v| v.parse::<usize>().ok()).and_then(|i| Overlay::ALL.get(i).copied()) {
        set_overlay_mode(ui.ctx(), m);
    }
    // heading with the overlay switch, its kind and the help card (fixed place)
    lr::category_with(ui, "Masken", 176.0, |ui| {
        crate::help::info(ui, "local_masks", "Masken", &crate::help::LOCAL_MASKS);
        let mode = overlay_mode(ui.ctx());
        let k = ui.add_sized([20.0, ui.spacing().interact_size.y], egui::Button::new("▾"));
        lr::tip_key(ui, &k, crate::tr("Darstellung der Überlagerung wählen"), "mask_overlay");
        egui::Popup::menu(&k).id(egui::Id::unique("mask_overlay_kind")).show(|ui| {
            for m in Overlay::ALL {
                if lr::menu_item(ui, &format!("{} {}", if m == mode { "●" } else { "  " }, crate::tr(m.label())), "", true) {
                    set_overlay_mode(ui.ctx(), m);
                    set_overlay(ui.ctx(), &mut s, true);
                }
            }
        });
        let shown = overlay_shown(ui.ctx(), &s, brush_ready(&s));
        let r = ui.add(egui::Button::new(crate::tr("Überlagerung")).selected(shown));
        lr::tip_key(ui, &r, &crate::trf!("Wirkung der Maske im Bild zeigen: {kind}", kind = crate::tr(mode.label())), "mask_overlay");
        if r.clicked() {
            set_overlay(ui.ctx(), &mut s, !shown);
        }
    });
    let count = local.as_ref().map_or(0, |d| d.layers.len());
    // no mask yet: the tiles of a new mask stay open in the tray
    if count == 0 {
        s.hover = None;
        if let Some(kind) = crate::mask_list_ui::tiles(ui, "new_mask_open", path) {
            if let Some(r) = create(&mut s, local, kind) {
                request = Some(r);
            }
        }
        tool_line(ui, &s, None);
        status_line(ui, path);
        remember_layers(ui.ctx(), false);
        ui.ctx().data_mut(|d| d.insert_temp(key(), s));
        return request;
    }
    let Some(d) = local.as_mut() else { return request };
    s.selected = s.selected.min(d.layers.len() - 1);
    // the list of all masks
    let (action, hovered) = crate::mask_list_ui::list(ui, d, s.selected, &mut s.rename, s.aspect.max(0.1), index);
    s.hover = hovered;
    ui.add_space(4.0);
    // "+ Neue Maske ▾" and "⋯" of the selected mask in one row
    let mut menu_action = None;
    ui.horizontal(|ui| {
        let w = lr::visible_width(ui) - 30.0;
        let r = ui.add_sized([w, 24.0], egui::Button::new(format!("+ {} ▾", crate::tr("Neue Maske"))));
        let test_menu = masks_test().is_ok_and(|v| v.starts_with("menu")) && !s.test_asked;
        if test_menu {
            s.test_asked = true;
        }
        menu_popup(&r, egui::Id::unique("new_mask_menu"), test_menu).show(|ui| {
            ui.set_width(w.max(240.0));
            if let Some(kind) = crate::mask_list_ui::tiles(ui, "new_mask_popup", path) {
                menu_action = Some(kind);
                ui.close();
            }
        });
        let m = ui.add_sized([24.0, 24.0], egui::Button::new("⋯"));
        lr::hover_tip(ui, &m, crate::tr("Maske: umbenennen, duplizieren, invertieren, verschieben, löschen"));
        let fields = d.allows_layer_fields();
        let changed = d.layers[s.selected].adjustments != Adjustments::default();
        let (i, n) = (s.selected, d.layers.len());
        egui::Popup::menu(&m).id(egui::Id::unique("mask_layer_menu")).show(|ui| {
            if let Some(a) = crate::mask_list_ui::layer_menu(ui, i, n, fields, changed) {
                s.pending = Some(crate::mask_list_ui::RowAction::Menu(i, a));
            }
        });
    });
    let action = action.or(s.pending.take());
    if let Some(kind) = menu_action {
        if let Some(r) = create(&mut s, local, kind) {
            request = Some(r);
        }
    }
    let Some(d) = local.as_mut() else { return request };
    if let Some(a) = action {
        apply_row(&mut s, d, a);
    }
    // ⌫ / Del deletes the selected mask (the photo is not deleted while the tray is open, see `takes_delete`)
    if takes_delete_keys(ui.ctx()) && !d.layers.is_empty() {
        let i = s.selected;
        apply_row(&mut s, d, crate::mask_list_ui::RowAction::Menu(i, crate::mask_list_ui::LayerAction::Delete));
    }
    tool_line(ui, &s, d.layers.get(s.selected));
    status_line(ui, path);
    if d.layers.is_empty() {
        *local = None;
        remember_layers(ui.ctx(), false);
        ui.ctx().data_mut(|dd| dd.insert_temp(key(), s));
        return request;
    }
    s.selected = s.selected.min(d.layers.len() - 1);
    let fields_ok = d.allows_layer_fields();
    let mut settle = false;
    {
        let name = crate::mask_list_ui::display_name(&d.layers[s.selected]);
        let layer = &mut d.layers[s.selected];
        // the parts of the selected mask and how they are combined
        let (again, changed) = parts_section(ui, &mut s, layer, &name, path);
        if again.is_some() {
            request = again;
        }
        settle |= changed;
        let count = crate::mask_parts::count(&layer.mask);
        s.part = s.part.min(count - 1);
        if let Some(part) = crate::mask_parts::part_mut(&mut layer.mask, s.part) {
            if let Some(r) = part_settings(ui, &mut s, part, path) {
                request = Some(r);
            }
        }
        settle |= strength(ui, layer, fields_ok);
        corrections(ui, &mut layer.adjustments, fields_ok);
    }
    test_options(d, s.selected, &mut s.test_done);
    // colour and brightness ranges: the image they compare against (pipette, overlay)
    let auto_brush = s.auto && s.tool == PartTool::Brush;
    if request.is_none() && (auto_brush || d.layers.get(s.selected).is_some_and(|l| crate::mask_sample::needs_image(&l.mask) || l.refines())) {
        request = Some(crate::subject_ui::Request::Sample);
    }
    // also after a slider of a part (edge of a selection, colour samples …): the lowest sufficient version
    let _ = settle;
    d.settle_version();
    remember_layers(ui.ctx(), true);
    ui.ctx().data_mut(|d| d.insert_temp(key(), s));
    request
}

/// Menu popup of a button: opens on a click by itself. `force_open` (test hook) opens it at once. Never call
/// `open_memory(None)` on it: that replaces the click and the button does nothing.
fn menu_popup(button: &egui::Response, id: egui::Id, force_open: bool) -> egui::Popup<'static> {
    let popup = egui::Popup::menu(button).id(id);
    if force_open {
        popup.open_memory(egui::SetOpenCommand::Bool(true))
    } else {
        popup
    }
}

fn remember_layers(ctx: &Context, has: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("masks_has_layers"), has));
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
    d.layers.push(Layer::new(mask, Adjustments { exposure: ev.clamp(-3.0, 3.0), ..Adjustments::default() }));
    let mut s = state(ctx, index);
    s.selected = d.layers.len() - 1;
    s.part = 0;
    s.draw = true;
    s.show = true;
    ctx.data_mut(|dd| dd.insert_temp(key(), s));
    true
}

/// Position of the handles of a gradient or an ellipse (normalised image coordinates).
fn handles(m: &Mask, full: [f32; 2]) -> Vec<(Handle, [f32; 2])> {
    match m {
        Mask::Linear { start, end } => vec![(Handle::Start, *start), (Handle::End, *end)],
        Mask::Radial { center, radii, angle, .. } => {
            let short = full[0].min(full[1]);
            let (sin, cos) = angle.sin_cos();
            let at = |x: f32, y: f32| [center[0] + (cos * x - sin * y) * short / full[0], center[1] + (sin * x + cos * y) * short / full[1]];
            vec![(Handle::Center, *center), (Handle::AxisX, at(radii[0], 0.0)), (Handle::AxisY, at(0.0, radii[1]))]
        }
        _ => Vec::new(),
    }
}

/// Move handle `h` of shape `m` to `p` (normalised); the other handles stay.
fn move_handle(m: &mut Mask, h: Handle, p: [f32; 2], from: [f32; 2], full: [f32; 2]) {
    let short = full[0].min(full[1]);
    match (&mut *m, h) {
        (Mask::Linear { start, .. }, Handle::Start) => *start = p,
        (Mask::Linear { end, .. }, Handle::End) => *end = p,
        (Mask::Radial { center, .. }, Handle::Center) => {
            center[0] += p[0] - from[0];
            center[1] += p[1] - from[1];
        }
        (Mask::Radial { center, radii, angle, .. }, Handle::AxisX | Handle::AxisY) => {
            // the axis follows the pointer: its length and (for the first axis) the angle
            let d = [(p[0] - center[0]) * full[0] / short, (p[1] - center[1]) * full[1] / short];
            let len = d[0].hypot(d[1]).max(0.001);
            if h == Handle::AxisX {
                radii[0] = len;
                *angle = d[1].atan2(d[0]);
            } else {
                radii[1] = len;
                *angle = d[1].atan2(d[0]) - std::f32::consts::FRAC_PI_2;
            }
        }
        _ => {}
    }
    if let Mask::Linear { start, end } = m {
        if start == end {
            end[0] += 0.001;
        }
    }
}

/// In the image: clicks of the click selection, painting with the brush of the selected part, the handles of a
/// gradient or ellipse, outlines and the red overlay of the selected (or hovered) mask.
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
    use crate::mask_parts;
    let mut s = state(ui.ctx(), index);
    s.aspect = full[0] / full[1].max(1.0);
    let open = crate::retouch_ui::masks_open(ui.ctx());
    let to_image = |pos: egui::Pos2| -> [f32; 2] {
        let u = (pos - rect.min) / rect.size();
        let q = frame.to_src([u.x, u.y]);
        [(q[0] / full[0]).clamp(0.0, 1.0), (q[1] / full[1]).clamp(0.0, 1.0)]
    };
    let to_screen = |p: [f32; 2]| {
        let u = frame.from_src([p[0] * full[0], p[1] * full[1]]);
        rect.min + egui::vec2(u[0], u[1]) * rect.size()
    };
    if s.click && (open || s.draw) && resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p)) {
            let q = to_image(pos);
            let exclude = ui.input(|i| i.modifiers.alt);
            if s.click_new {
                // "new mask": the first click never adds to the selected mask
                if let Some(sel) = crate::subject_ui::clicked_point(local, usize::MAX, q, exclude) {
                    s.selected = sel;
                    s.part = 0;
                    s.click_new = false;
                    crate::subject_ui::clear_message();
                }
            } else if let Some(op) = s.click_part {
                // a new part of the selected mask
                if let Some(l) = local.as_mut().and_then(|d| d.layers.get_mut(s.selected)) {
                    if !exclude {
                        let leaf = Mask::Raster { raster: String::new(), source: masks::RasterSource::Click { points: vec![[q[0], q[1], 1.0]], rect: None }, feather: 0.0, range: None, edge: 0.0 };
                        let mut parts = mask_parts::split(&l.mask);
                        if parts.push(op, leaf) {
                            l.mask = mask_parts::join(&parts);
                            s.part = parts.parts.len() - 1;
                            s.click_part = None;
                            s.tool = PartTool::Click;
                            if let Some(d) = local.as_mut() {
                                d.settle_version();
                            }
                            crate::subject_ui::clear_message();
                        }
                    }
                }
            } else if let Some(part) = local.as_mut().and_then(|d| d.layers.get_mut(s.selected)).and_then(|l| mask_parts::part_mut(&mut l.mask, s.part)) {
                // more points for the selected click selection
                if let Some(Mask::Raster { raster, source: masks::RasterSource::Click { points, .. }, .. }) = crate::subject_ui::first_raster(part) {
                    if points.len() < 64 {
                        points.push([q[0], q[1], if exclude { -1.0 } else { 1.0 }]);
                        raster.clear();
                        crate::subject_ui::clear_message();
                    }
                }
            }
            ui.ctx().request_repaint();
        }
    }
    // a box dragged with the click selection: around the object (a new mask, a new part or the box of the selected
    // click selection)
    if s.click && (open || s.draw) {
        let pointer = resp.interact_pointer_pos().filter(|p| rect.contains(*p));
        if resp.drag_started() {
            s.start = pointer.map(to_image);
        }
        if let (Some(a), Some(pos)) = (s.start, pointer) {
            let b = to_image(pos);
            if resp.dragged() {
                let corners = [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]], [a[0], a[1]]].map(to_screen).to_vec();
                painter.add(egui::Shape::dashed_line(&corners, egui::Stroke::new(1.5, Color32::from_rgb(230, 180, 70)), 6.0, 4.0));
                ui.ctx().request_repaint();
            }
            if resp.drag_stopped() {
                let r = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])];
                if r[2] - r[0] > 0.01 && r[3] - r[1] > 0.01 {
                    if let Some(sel) = boxed(local, &mut s, r) {
                        s.selected = sel;
                    }
                    crate::subject_ui::clear_message();
                }
                s.start = None;
            }
        }
        if resp.drag_stopped() {
            s.start = None;
        }
    }
    if s.click && (open || s.draw) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    // pipette of a colour range: a click takes a sample of the image the masks see (waits for it if needed)
    if open && s.armed && s.tool == PartTool::Pipette {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p)) {
                let m = ui.input(|i| i.modifiers);
                s.pick = Some((to_image(pos), m.shift, m.alt));
            }
        }
        if let Some((q, add, remove)) = s.pick {
            match crate::mask_sample::pick(index, q) {
                Some(c) => {
                    if let Some(Mask::ColorRange { samples, .. }) = local.as_mut().and_then(|d| d.layers.get_mut(s.selected)).and_then(|l| mask_parts::part_mut(&mut l.mask, s.part)).map(mask_parts::last_leaf) {
                        take_sample(samples, c, add, remove);
                    }
                    s.pick = None;
                }
                None => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
    }
    // depth mask: a click takes the segment under the pointer in or out
    if open && s.armed && s.tool == PartTool::Segments {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos().filter(|p| rect.contains(*p)) {
                let q = to_image(pos);
                let part = local.as_mut().and_then(|d| d.layers.get_mut(s.selected)).and_then(|l| mask_parts::part_mut(&mut l.mask, s.part));
                if let Some(Mask::Raster { raster, source: masks::RasterSource::Depth { toggled, .. }, .. }) = part.and_then(crate::subject_ui::first_raster) {
                    crate::depth_ui::toggle_segment(raster, toggled, q);
                    ui.ctx().request_repaint();
                }
            }
        }
    }
    let Some(d) = local else {
        ui.ctx().data_mut(|d| d.insert_temp(key(), s));
        return;
    };
    // a row of the list under the pointer: its mask is shown red for a moment (without changing the selection)
    if let Some(h) = s.hover.filter(|h| *h != s.selected && open) {
        if let Some(l) = d.layers.get(h) {
            weights_overlay(ui, painter, rect, frame, full, l, index);
        }
    }
    let Some(layer) = d.layers.get_mut(s.selected) else {
        ui.ctx().data_mut(|d| d.insert_temp(key(), s));
        return;
    };
    s.part = s.part.min(mask_parts::count(&layer.mask) - 1);
    let tool = mask_parts::part(&layer.mask, s.part).map_or(PartTool::None, tool_of);
    // drawing: from the darkroom (`draw`) or the armed tool of the selected part in the open "Masks" tool
    let acting = s.draw || (open && s.armed);
    let pointer = resp.interact_pointer_pos().filter(|p| rect.contains(*p) && ui.clip_rect().contains(*p));
    if acting && tool == PartTool::Brush {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        // [ ] size, ⇧[ ⇧] hardness of the next strokes
        if let Some(b) = brush_keys(ui.ctx(), s.brush.unwrap_or(BRUSH)) {
            s.brush = Some(b);
            if let Some(leaf) = mask_parts::part_mut(&mut layer.mask, s.part).map(mask_parts::last_leaf) {
                if matches!(leaf, Mask::Brush { points, .. } if mask_parts::fresh(points)) {
                    *leaf = new_stroke(b);
                }
            }
        }
        if let Some(pos) = pointer {
            let q = to_image(pos);
            let mods = ui.input(|i| i.modifiers);
            if resp.drag_started() || resp.clicked() {
                let before = layer.mask.clone();
                let brush = s.brush.unwrap_or(BRUSH);
                // ⌥: the stroke erases (goes into the erasing part at the end of the mask)
                s.stroke_part = if mods.alt { eraser_part(&mut layer.mask, brush) } else { Some(s.part) };
                if let Some(part) = s.stroke_part.and_then(|t| mask_parts::part_mut(&mut layer.mask, t)) {
                    // ⇧-click: a straight line from the last point painted
                    let line = (mods.shift && resp.clicked()).then(|| last_point(part)).flatten();
                    match line {
                        Some((_, same, auto)) if same == new_stroke(brush) && auto == s.auto => {
                            // the same brush: the last stroke goes on (no doubled dab at the joint)
                            if let Mask::Brush { points, .. } = mask_parts::last_leaf(part) {
                                points.push(q);
                            }
                        }
                        _ => {
                            *part = mask_parts::add_stroke(part, new_stroke(brush));
                            let start = line.as_ref().map_or(q, |l| l.0);
                            // the colour under the start of the stroke (waits for the image the masks see if needed)
                            let auto = s.auto.then(|| crate::mask_sample::pick(index, start)).map(|c| match c {
                                Some(c) => Some(masks::AutoMask { reference: c, tolerance: s.auto_tolerance.unwrap_or(AUTO_TOLERANCE) }),
                                None => {
                                    s.auto_pending = s.stroke_part.map(|t| (t, start));
                                    None
                                }
                            });
                            if let Mask::Brush { points, auto: a, .. } = mask_parts::last_leaf(part) {
                                *points = line.map_or(vec![q], |(from, _, _)| vec![from, q]);
                                *a = auto.flatten();
                            }
                        }
                    }
                }
                if layer.mask.validate().is_err() {
                    layer.mask = before;
                    s.stroke_part = None;
                }
            }
            if resp.dragged() {
                if let Some(Mask::Brush { points, .. }) = s.stroke_part.and_then(|t| mask_parts::part_mut(&mut layer.mask, t)).map(mask_parts::last_leaf) {
                    if !mask_parts::fresh(points) && points.len() < 100_000 && points.last().is_none_or(|p| (p[0] - q[0]).hypot(p[1] - q[1]) > 0.001) {
                        points.push(q);
                    }
                }
                ui.ctx().request_repaint();
            }
        }
        if resp.clicked() || resp.drag_stopped() {
            s.stroke_part = None;
        }
    }
    // an auto-mask stroke waiting for the colour under its start: set once the image the masks see is there
    if let Some((t, start)) = s.auto_pending {
        match crate::mask_sample::pick(index, start) {
            Some(c) => {
                if let Some(Mask::Brush { auto, .. }) = mask_parts::part_mut(&mut layer.mask, t).map(mask_parts::last_leaf) {
                    auto.get_or_insert(masks::AutoMask { reference: c, tolerance: s.auto_tolerance.unwrap_or(AUTO_TOLERANCE) });
                }
                s.auto_pending = None;
            }
            None => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
        }
    }
    if acting && tool == PartTool::Shape {
        if let Some(leaf) = mask_parts::part_mut(&mut layer.mask, s.part).map(mask_parts::last_leaf) {
            let hs = handles(leaf, full);
            let near = |pos: egui::Pos2| hs.iter().map(|(h, p)| (*h, (to_screen(*p) - pos).length())).filter(|(_, d)| *d <= 9.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
            let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
            s.over_handle = hover.and_then(near).is_some();
            if s.over_handle || s.grab.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
            if let Some(pos) = pointer {
                let q = to_image(pos);
                if resp.drag_started() {
                    if s.draw || s.fresh {
                        // a new shape: dragged out from here
                        s.start = Some(q);
                        s.grab = None;
                    } else {
                        s.grab = near(pos);
                        s.start = Some(q);
                    }
                }
                if resp.dragged() {
                    match (s.grab, s.start) {
                        (Some(h), Some(from)) => {
                            move_handle(leaf, h, q, from, full);
                            s.start = Some(q);
                        }
                        (None, Some(anchor)) if s.draw || s.fresh => match leaf {
                            Mask::Linear { start, end } => {
                                *start = anchor;
                                *end = q;
                                if start == end {
                                    end[0] += 0.001;
                                }
                            }
                            Mask::Radial { center, radii, .. } => {
                                *center = anchor;
                                let short = full[0].min(full[1]);
                                *radii = [((q[0] - anchor[0]) * full[0] / short).abs().max(0.001), ((q[1] - anchor[1]) * full[1] / short).abs().max(0.001)];
                            }
                            _ => (),
                        },
                        _ => (),
                    }
                    ui.ctx().request_repaint();
                }
            }
            if resp.drag_stopped() {
                if s.grab.is_none() && s.start.is_some() {
                    s.fresh = false;
                }
                s.grab = None;
                s.start = None;
            }
        }
    } else {
        s.over_handle = false;
    }
    // the box of the selected click selection
    if open && tool == PartTool::Click {
        if let Some(Mask::Raster { source: masks::RasterSource::Click { rect: Some(r), .. }, .. }) = mask_parts::part_mut(&mut layer.mask, s.part).and_then(crate::subject_ui::first_raster) {
            let corners = [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]], [r[0], r[1]]].map(to_screen).to_vec();
            painter.add(egui::Shape::dashed_line(&corners, egui::Stroke::new(1.0, Color32::from_rgb(230, 180, 70)), 6.0, 4.0));
        }
    }
    // while a control outside the photo is held down (a slider, also when held still), the overlay steps aside so the
    // effect is visible; it comes back on release
    let slider_drag = control_held(ui.ctx(), rect);
    if overlay_shown(ui.ctx(), &s, acting && tool == PartTool::Brush) && !slider_drag && s.hover.is_none_or(|h| h == s.selected) {
        weights_overlay(ui, painter, rect, frame, full, layer, index);
    }
    // the segment borders of the selected depth part
    if open && tool == PartTool::Segments && crate::depth_ui::segments_shown(ui.ctx()) {
        if let Some(Mask::Raster { raster, .. }) = mask_parts::part_mut(&mut layer.mask, s.part).and_then(crate::subject_ui::first_raster) {
            segments_overlay(ui, painter, rect, frame, full, raster);
        }
    }
    // outlines of the selected part: while drawing, in the open "Masks" tool (H hides them) or in the darkroom; brush
    // strokes have none (the overlay shows what was painted, the ring where the brush is)
    if s.show || s.draw || crate::retouch_ui::mask_outlines_visible(ui.ctx()) {
        let stroke = egui::Stroke::new(1.0, Color32::from_rgb(230, 180, 70));
        if let Some(part) = mask_parts::part(&layer.mask, s.part) {
            let mut leaves = Vec::new();
            part.visit(&mut |m| {
                if matches!(m, Mask::Linear { .. } | Mask::Radial { .. }) {
                    leaves.push(m.clone());
                }
            });
            for m in leaves.iter().rev().take(200) {
                match m {
                    Mask::Linear { start, end } => {
                        painter.line_segment([to_screen(*start), to_screen(*end)], stroke);
                    }
                    Mask::Radial { center, radii, angle, .. } => {
                        let short = full[0].min(full[1]);
                        let (sin, cos) = angle.sin_cos();
                        let pts = (0..=64)
                            .map(|i| {
                                let t = i as f32 * std::f32::consts::TAU / 64.0;
                                let (x, y) = (t.cos() * radii[0], t.sin() * radii[1]);
                                to_screen([center[0] + (cos * x - sin * y) * short / full[0], center[1] + (sin * x + cos * y) * short / full[1]])
                            })
                            .collect();
                        painter.add(egui::Shape::line(pts, stroke));
                    }
                    _ => (),
                }
            }
            // handles of the selected gradient or ellipse
            if open && !s.draw && tool == PartTool::Shape {
                let mut part = part.clone();
                for (_, p) in handles(mask_parts::last_leaf(&mut part), full) {
                    painter.circle_filled(to_screen(p), 4.5, Color32::from_gray(0x20));
                    painter.circle_stroke(to_screen(p), 4.5, egui::Stroke::new(1.5, Color32::from_rgb(230, 180, 70)));
                }
            }
        }
    }
    // the brush ring at the pointer: outer ring = size, inner ring = where the soft edge begins
    if acting && tool == PartTool::Brush {
        let test_ring = || masks_test_opt("ring").map(|v| v.split(',').filter_map(|x| x.trim().parse::<f32>().ok()).collect::<Vec<_>>()).filter(|v| v.len() == 2).map(|v| to_screen([v[0], v[1]]));
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p) && ui.clip_rect().contains(*p)).or_else(test_ring) {
            let b = s.brush.unwrap_or(BRUSH);
            let q = to_image(pos);
            let short = full[0].min(full[1]);
            let rx = (to_screen([q[0] + b[0] * short / full[0], q[1]]) - to_screen(q)).length();
            let ry = (to_screen([q[0], q[1] + b[0] * short / full[1]]) - to_screen(q)).length();
            let erase = s.stroke_part.map_or(ui.input(|i| i.modifiers.alt), |t| t != s.part);
            brush_ring(&painter.with_clip_rect(rect), pos, 0.5 * (rx + ry), b[1], erase);
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(key(), s));
}

/// The brush ring at `pos`: the outer ring at `radius` (screen points) is the size, the inner ring at `radius ×
/// hardness` the start of the soft edge (left out when it would lie on the outer one); light rings with a dark rim, so
/// that they show on light and dark parts of the photo. `erase`: a "−" in the middle (⌥ erases).
fn brush_ring(painter: &egui::Painter, pos: egui::Pos2, radius: f32, hardness: f32, erase: bool) {
    let ring = |r: f32, alpha: u8| {
        painter.circle_stroke(pos, r, egui::Stroke::new(2.5, Color32::from_black_alpha(alpha / 2)));
        painter.circle_stroke(pos, r, egui::Stroke::new(1.0, Color32::from_white_alpha(alpha)));
    };
    let radius = radius.max(1.5);
    ring(radius, 230);
    let inner = radius * hardness.clamp(0.0, 1.0);
    if inner >= 1.5 && radius - inner >= 2.0 {
        ring(inner, 150);
    }
    if erase {
        for (w, c) in [(4.0, Color32::from_black_alpha(150)), (1.5, Color32::WHITE)] {
            painter.line_segment([pos - egui::vec2(5.0, 0.0), pos + egui::vec2(5.0, 0.0)], egui::Stroke::new(w, c));
        }
    }
}

/// `[` / `]`: brush smaller / larger (by a quarter), ⇧[ / ⇧]: softer / harder (by 10 %). Returns the new settings when a
/// key was pressed.
fn brush_keys(ctx: &Context, b: [f32; 4]) -> Option<[f32; 4]> {
    use crate::shortcuts::pressed;
    let mut n = b;
    if pressed(ctx, "brush_smaller") {
        n[0] /= 1.25;
    }
    if pressed(ctx, "brush_larger") {
        n[0] *= 1.25;
    }
    if pressed(ctx, "brush_softer") {
        n[1] -= 0.1;
    }
    if pressed(ctx, "brush_harder") {
        n[1] += 0.1;
    }
    n[0] = n[0].clamp(0.001, 0.5);
    n[1] = ((n[1] * 100.0).round() / 100.0).clamp(0.0, 1.0);
    (n != b).then_some(n)
}

/// The erasing part of a mask: brush strokes that take away from what the mask shows. The last part when it already is
/// one (a brush part subtracted, or added to a mask inverted as a whole), otherwise a new one at the end. Returns its
/// index (`None`: the mask tree would get too large).
fn eraser_part(mask: &mut Mask, brush: [f32; 4]) -> Option<usize> {
    use crate::mask_parts;
    let mut parts = mask_parts::split(mask);
    // in a mask inverted as a whole, adding takes away from what it shows
    let op = if parts.inverted { Operation::Add } else { Operation::Subtract };
    let n = parts.parts.len();
    if n > 1 && parts.parts[n - 1].0 == Some(op) && matches!(parts.parts[n - 1].1, Mask::Brush { .. } | Mask::Combine { .. }) && mask_parts::is_brush(&parts.parts[n - 1].1) {
        return Some(n - 1);
    }
    if !parts.push(op, new_stroke(brush)) {
        return None;
    }
    *mask = mask_parts::join(&parts);
    Some(n)
}

/// The last point painted in brush part `part`, the settings of its stroke (as a stroke not drawn yet, to compare
/// with [`new_stroke`]) and whether it has an auto mask; `None` before the first stroke.
fn last_point(part: &mut Mask) -> Option<([f32; 2], Mask, bool)> {
    match crate::mask_parts::last_leaf(part) {
        Mask::Brush { points, radius, hardness, flow, density, auto } if !crate::mask_parts::fresh(points) => {
            Some((*points.last()?, new_stroke([*radius, *hardness, *flow, *density]), auto.is_some()))
        }
        _ => None,
    }
}

fn auto_off_id() -> egui::Id {
    egui::Id::unique("mask_overlay_auto_off")
}

/// The overlay was switched off on purpose (button, ⇧O): it no longer comes on by itself while painting, until it is
/// switched on again (for all photos of the session).
fn auto_off(ctx: &Context) -> bool {
    ctx.data_mut(|d| d.get_temp::<bool>(auto_off_id()).unwrap_or(false))
}

/// Is the brush of the selected part ready to paint in the image (from the panel's point of view: the "Masks" tool is
/// open there)? Then the overlay comes on by itself.
fn brush_ready(s: &State) -> bool {
    (s.draw || s.armed) && s.tool == PartTool::Brush
}

/// Is a control outside the photo held down? The press is remembered when it starts and forgotten only when no
/// button is down any more: egui's `press_origin` is cleared by the release of any other button (or a lost event)
/// while the primary button is still down, which made the overlay reappear during a slider drag.
pub(crate) fn control_held(ctx: &Context, rect: egui::Rect) -> bool {
    let id = egui::Id::new("masks_control_held");
    let (down, pressed_at, origin) = ctx.input(|i| {
        (
            i.pointer.primary_down(),
            i.pointer.button_pressed(egui::PointerButton::Primary).then(|| i.pointer.interact_pos()).flatten(),
            i.pointer.press_origin(),
        )
    });
    let prev = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let held = held_outside(prev, down, pressed_at.map(|p| rect.contains(p)), origin.map(|p| rect.contains(p)));
    if held != prev {
        ctx.data_mut(|d| d.insert_temp(id, held));
    }
    held
}

/// State step of `control_held`: `pressed_inside` is set in the frame the primary button went down (inside the photo
/// or not), `origin_inside` is egui's own press origin, a fallback if that frame was missed.
fn held_outside(prev: bool, down: bool, pressed_inside: Option<bool>, origin_inside: Option<bool>) -> bool {
    if !down {
        return false;
    }
    match pressed_inside {
        Some(inside) => !inside,
        None => prev || origin_inside == Some(false),
    }
}

/// Is the overlay shown: switched on, or the brush is ready (`painting`) and the overlay was not switched off on
/// purpose ("auto")?
fn overlay_shown(ctx: &Context, s: &State, painting: bool) -> bool {
    s.weights || (painting && !auto_off(ctx))
}

/// Switch the overlay on or off on purpose: off stays off while painting, on stays on.
fn set_overlay(ctx: &Context, s: &mut State, on: bool) {
    s.weights = on;
    ctx.data_mut(|d| d.insert_temp(auto_off_id(), !on));
}

#[cfg(test)]
mod tests {

    /// The overlay stays hidden for the whole press on a control outside the photo, even when egui loses its press
    /// origin (another button released meanwhile); it is released only with the button, and a press in the photo
    /// never hides it.
    #[test]
    fn a_press_outside_the_photo_is_remembered_until_release() {
        use super::held_outside;
        let mut h = held_outside(false, true, Some(false), Some(false));
        assert!(h);
        // press origin lost while the primary button is still down
        h = held_outside(h, true, None, None);
        assert!(h, "must not flip back while held");
        // an old-style check would have said false here; now release ends it
        assert!(!held_outside(h, false, None, None));
        // a press in the photo (brush stroke) never counts
        assert!(!held_outside(false, true, Some(true), Some(true)));
        // a new press in the photo after one outside replaces the state
        assert!(!held_outside(true, true, Some(true), None));
    }

    /// A click on a menu button opens its popup (a regression: `open_memory(None)` had replaced the click, and
    /// "+ New mask" did nothing); the test hook opens it without a click.
    #[test]
    fn the_new_mask_button_opens_its_menu_on_a_click() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("test_menu");
        let pos = egui::pos2(40.0, 20.0);
        let frame = |ctx: &egui::Context, events: Vec<egui::Event>, force: bool| {
            let mut open = false;
            let input = egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() };
            let _ = ctx.run_ui(input, |ui| {
                let r = ui.add_sized([120.0, 24.0], egui::Button::new("+ New"));
                menu_popup(&r, id, force).show(|ui| {
                    ui.label("tiles");
                });
                open = egui::Popup::is_id_open(ui.ctx(), id);
            });
            open
        };
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        assert!(!frame(&ctx, vec![egui::Event::PointerMoved(pos)], false));
        frame(&ctx, vec![button(true)], false);
        frame(&ctx, vec![button(false)], false);
        assert!(frame(&ctx, vec![], false), "a click must open the menu");
        let ctx2 = egui::Context::default();
        assert!(frame(&ctx2, vec![], true) || frame(&ctx2, vec![], true), "the test hook opens it at once");
    }
    use super::*;
    #[test]
    fn local_xmp_roundtrip() {
        // the system's temporary folder: sidecars may be written there in every test run (with or without an own
        // data folder, wherever it lies)
        let root = std::env::temp_dir().join(format!(
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
        // version 4 (name, off, strength) goes through the sidecar unchanged
        let mut v4 = settings.local.clone().unwrap();
        v4.layers[0].name = Some("Himmel".into());
        v4.layers[0].amount = 0.6;
        if let Some(l) = v4.layers.get_mut(1) {
            l.enabled = false;
        }
        v4.settle_version();
        assert_eq!(v4.version, 4);
        let settings4 = develop::Settings { local: Some(v4), ..Default::default() };
        crate::raw_app::save_settings(&original, None, &settings4, &crate::sidecar::Meta::default()).unwrap();
        assert_eq!(crate::raw_app::load_settings(&original, None).0.local, settings4.local);
        crate::raw_app::save_settings(&original, None, &settings, &crate::sidecar::Meta::default()).unwrap();
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
                name: None,
                enabled: true,
                amount: 1.0,
                refine: 0.0,
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

#[cfg(test)]
mod brush_view_tests {
    use super::*;

    /// What a frame drew: golden line segments (outlines), circles of the brush ring, textured meshes (overlay).
    #[derive(Default, Debug)]
    struct Drawn {
        gold_lines: usize,
        rings: Vec<f32>,
        meshes: usize,
    }
    fn drawn(out: &egui::FullOutput) -> Drawn {
        fn walk(s: &egui::Shape, d: &mut Drawn) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, d)),
                egui::Shape::LineSegment { stroke, .. } if stroke.color == Color32::from_rgb(230, 180, 70) => d.gold_lines += 1,
                egui::Shape::Circle(c) if c.stroke.color == Color32::from_white_alpha(230) => d.rings.push(c.radius),
                egui::Shape::Circle(c) if c.stroke.color == Color32::from_white_alpha(150) => d.rings.push(-c.radius),
                egui::Shape::Mesh(_) => d.meshes += 1,
                _ => {}
            }
        }
        let mut d = Drawn::default();
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut d));
        d
    }
    fn frame_of(ctx: &Context, local: &mut Option<Document>, events: Vec<egui::Event>) -> egui::FullOutput {
        let frame = crate::Frame::from_settings(&develop::Settings::default(), 200, 100, 0);
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), events, ..Default::default() };
        crate::ui_test::run(ctx, raw, |ui| {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
            let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            overlay(ui, &resp, ui.painter(), rect, &frame, [200.0, 100.0], local, 0);
        })
    }
    fn button(p: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }
    fn paint(ctx: &Context, local: &mut Option<Document>, a: egui::Pos2, b: egui::Pos2) {
        frame_of(ctx, local, vec![egui::Event::PointerMoved(a), button(a, true)]);
        for k in 1..=6 {
            let p = a + (b - a) * (k as f32 / 6.0);
            frame_of(ctx, local, vec![egui::Event::PointerMoved(p)]);
        }
        frame_of(ctx, local, vec![button(b, false)]);
    }

    /// While painting: no golden centre lines of the strokes, the ring at the pointer (outer = size, inner = start of
    /// the soft edge), the overlay by itself; switched off on purpose it stays off while painting, until switched on.
    #[test]
    fn painting_shows_overlay_and_ring_but_no_centre_lines() {
        let ctx = Context::default();
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, crate::mask_list_ui::NewKind::Brush);
        // drawing switched on as by the darkroom (the "Masks" tool needs the rail), brush 10 % hard 50 %
        s.draw = true;
        s.brush = Some([0.1, 0.5, 0.2, 1.0]);
        ctx.data_mut(|d| d.insert_temp(key(), s));
        frame_of(&ctx, &mut local, Vec::new());
        paint(&ctx, &mut local, egui::pos2(40.0, 40.0), egui::pos2(150.0, 80.0));
        paint(&ctx, &mut local, egui::pos2(60.0, 80.0), egui::pos2(170.0, 30.0));
        assert_eq!(crate::mask_parts::strokes(&local.as_ref().unwrap().layers[0].mask), 2);
        let d = drawn(&frame_of(&ctx, &mut local, vec![egui::Event::PointerMoved(egui::pos2(100.0, 60.0))]));
        assert_eq!(d.gold_lines, 0, "no centre lines of brush strokes");
        assert!(d.meshes >= 1, "the overlay comes on by itself while painting: {d:?}");
        // photo 200 × 100 shown at 200 × 100 points: radius 10 % of the short side = 10 points, soft edge from 5
        assert!(d.rings.iter().any(|r| (r - 10.0).abs() < 0.5) && d.rings.iter().any(|r| (r + 5.0).abs() < 0.5), "{:?}", d.rings);
        // switched off on purpose: no overlay while painting, the ring stays
        let mut st = state(&ctx, 0);
        set_overlay(&ctx, &mut st, false);
        ctx.data_mut(|d| d.insert_temp(key(), st));
        paint(&ctx, &mut local, egui::pos2(30.0, 90.0), egui::pos2(180.0, 90.0));
        let d = drawn(&frame_of(&ctx, &mut local, vec![egui::Event::PointerMoved(egui::pos2(100.0, 60.0))]));
        assert_eq!(d.meshes, 0, "off stays off while painting");
        assert!(!d.rings.is_empty(), "the ring shows without the overlay");
        // switched on again: shown (also later while painting)
        let mut st = state(&ctx, 0);
        set_overlay(&ctx, &mut st, true);
        ctx.data_mut(|d| d.insert_temp(key(), st));
        assert!(drawn(&frame_of(&ctx, &mut local, vec![egui::Event::PointerMoved(egui::pos2(100.0, 60.0))])).meshes >= 1);
        // not painting (drawing off, overlay not switched on): no overlay, no ring
        let mut st = state(&ctx, 0);
        st.weights = false;
        st.draw = false;
        ctx.data_mut(|d| d.insert_temp(key(), st));
        ctx.data_mut(|d| d.insert_temp(auto_off_id(), false));
        let d = drawn(&frame_of(&ctx, &mut local, vec![egui::Event::PointerMoved(egui::pos2(100.0, 60.0))]));
        assert_eq!((d.meshes, d.rings.len()), (0, 0));
        // gradients keep their line
        local.as_mut().unwrap().layers.push(Layer::new(shape(1), Adjustments::default()));
        let mut st = state(&ctx, 0);
        st.selected = 1;
        st.show = true;
        ctx.data_mut(|d| d.insert_temp(key(), st));
        assert_eq!(drawn(&frame_of(&ctx, &mut local, Vec::new())).gold_lines, 1);
    }

    /// The brush settings of the panel (here: density 40 %) go into the next stroke, not into the strokes before.
    #[test]
    fn next_stroke_takes_the_density() {
        let ctx = Context::default();
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, crate::mask_list_ui::NewKind::Brush);
        s.draw = true;
        ctx.data_mut(|d| d.insert_temp(key(), s));
        frame_of(&ctx, &mut local, Vec::new());
        paint(&ctx, &mut local, egui::pos2(40.0, 40.0), egui::pos2(150.0, 80.0));
        let mut st = state(&ctx, 0);
        st.brush = Some([0.05, 0.6, 0.5, 0.4]);
        ctx.data_mut(|d| d.insert_temp(key(), st));
        paint(&ctx, &mut local, egui::pos2(40.0, 80.0), egui::pos2(150.0, 40.0));
        let mut densities = Vec::new();
        local.as_ref().unwrap().layers[0].mask.visit(&mut |m| {
            if let Mask::Brush { density, .. } = m {
                densities.push(*density);
            }
        });
        assert_eq!(densities, vec![1.0, 0.4]);
        let mut d = local.unwrap();
        d.settle_version();
        assert_eq!(d.version, 4);
        d.validate().unwrap();
    }

    fn frame_with(ctx: &Context, local: &mut Option<Document>, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> egui::FullOutput {
        let frame = crate::Frame::from_settings(&develop::Settings::default(), 200, 100, 0);
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), events: [vec![egui::Event::ModifiersChanged(modifiers)], events].concat(), ..Default::default() };
        crate::ui_test::run(ctx, raw, |ui| {
            let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
            let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            overlay(ui, &resp, ui.painter(), rect, &frame, [200.0, 100.0], local, 0);
        })
    }
    fn brush_mask(ctx: &Context) -> Option<Document> {
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, crate::mask_list_ui::NewKind::Brush);
        s.draw = true;
        ctx.data_mut(|d| d.insert_temp(key(), s));
        frame_of(ctx, &mut local, Vec::new());
        local
    }
    fn points_of(m: &Mask) -> Vec<Vec<[f32; 2]>> {
        let mut v = Vec::new();
        m.visit(&mut |n| {
            if let Mask::Brush { points, .. } = n {
                v.push(points.clone());
            }
        });
        v
    }
    /// A key pressed and let go in one frame.
    fn key_event(key: egui::Key, shift: bool) -> Vec<egui::Event> {
        let m = egui::Modifiers { shift, ..Default::default() };
        [true, false].map(|pressed| egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: m }).to_vec()
    }

    /// ⌥ while painting erases: the stroke goes into an erasing part "− Brush" at the end (made once, then reused);
    /// the selected part stays selected; the ring shows a "−"; the weights fall where it erased.
    #[test]
    fn alt_erases_into_a_subtracted_part() {
        let ctx = Context::default();
        let mut local = brush_mask(&ctx);
        paint(&ctx, &mut local, egui::pos2(20.0, 60.0), egui::pos2(200.0, 60.0));
        let alt = egui::Modifiers { alt: true, ..Default::default() };
        let out = frame_with(&ctx, &mut local, vec![egui::Event::PointerMoved(egui::pos2(100.0, 60.0))], alt);
        let minus = out.shapes.iter().any(|c| matches!(&c.shape, egui::Shape::LineSegment { points, stroke } if stroke.color == Color32::WHITE && (points[1].x - points[0].x - 10.0).abs() < 0.01));
        assert!(minus, "the ring shows a minus while ⌥ is held");
        for (a, b) in [(egui::pos2(100.0, 20.0), egui::pos2(100.0, 100.0)), (egui::pos2(140.0, 20.0), egui::pos2(140.0, 100.0))] {
            frame_with(&ctx, &mut local, vec![egui::Event::PointerMoved(a), egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: alt }], alt);
            for k in 1..=6 {
                frame_with(&ctx, &mut local, vec![egui::Event::PointerMoved(a + (b - a) * (k as f32 / 6.0))], alt);
            }
            frame_with(&ctx, &mut local, vec![egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: alt }], alt);
        }
        let m = local.as_ref().unwrap().layers[0].mask.clone();
        assert_eq!(crate::mask_parts::operations(&m), vec![None, Some(Operation::Subtract)], "one erasing part, reused");
        assert_eq!(crate::mask_parts::strokes(crate::mask_parts::part(&m, 1).unwrap()), 2, "{:?}", points_of(&m).iter().map(|p| (p.len(), p[0])).collect::<Vec<_>>());
        assert_eq!(state(&ctx, 0).part, 0, "the painting part stays selected");
        // ⌥ let go: a normal stroke goes on into the selected part
        frame_with(&ctx, &mut local, Vec::new(), egui::Modifiers::default());
        paint(&ctx, &mut local, egui::pos2(20.0, 30.0), egui::pos2(60.0, 30.0));
        let m = local.as_ref().unwrap().layers[0].mask.clone();
        assert_eq!(crate::mask_parts::strokes(crate::mask_parts::part(&m, 0).unwrap()), 2);
        // weights: painted at the left, erased at x = 100 (mask x 0.45)
        let data = vec![0.5f32; 200 * 100 * 3];
        let w = masks::rasterize(masks::Image::new(200, 100, &data).unwrap(), &m).unwrap();
        assert!(w[50 * 200 + 40] > 0.1 && w[50 * 200 + 90] < 0.02, "painted {}, erased {}", w[50 * 200 + 40], w[50 * 200 + 90]);
        // in a mask inverted as a whole the erasing part adds (takes away from what the inverted mask shows)
        let mut inv = Mask::Invert { mask: Box::new(Mask::Combine { operation: Operation::Add, left: Box::new(shape(2)), right: Box::new(shape(1)) }) };
        assert_eq!(eraser_part(&mut inv, BRUSH), Some(2));
        assert_eq!(crate::mask_parts::operations(&inv), vec![None, Some(Operation::Add), Some(Operation::Add)]);
        assert!(crate::mask_parts::inverted(&inv));
    }

    /// ⇧-click: a straight line from the last point painted (the same brush goes on in the last stroke, another brush
    /// starts a stroke at that point); a plain click is one dab.
    #[test]
    fn shift_click_draws_a_straight_line() {
        let ctx = Context::default();
        let mut local = brush_mask(&ctx);
        paint(&ctx, &mut local, egui::pos2(20.0, 30.0), egui::pos2(60.0, 30.0));
        let last = *points_of(&local.as_ref().unwrap().layers[0].mask)[0].last().unwrap();
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        let click = |ctx: &Context, local: &mut Option<Document>, p: egui::Pos2, m: egui::Modifiers| {
            frame_with(ctx, local, vec![egui::Event::PointerMoved(p), egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: m }], m);
            frame_with(ctx, local, vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: m }], m);
        };
        click(&ctx, &mut local, egui::pos2(160.0, 90.0), shift);
        let pts = points_of(&local.as_ref().unwrap().layers[0].mask);
        assert_eq!(pts.len(), 1, "same brush: the stroke goes on");
        assert_eq!(pts[0][pts[0].len() - 2], last);
        let end = pts[0][pts[0].len() - 1];
        assert!((end[0] - 0.75).abs() < 0.01 && (end[1] - 0.8).abs() < 0.01, "{end:?}");
        // another brush: a new stroke from the last point
        let mut st = state(&ctx, 0);
        st.brush = Some([0.02, 0.6, 0.5, 1.0]);
        ctx.data_mut(|d| d.insert_temp(key(), st));
        click(&ctx, &mut local, egui::pos2(160.0, 20.0), shift);
        let pts = points_of(&local.as_ref().unwrap().layers[0].mask);
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[1][0], end);
        // a plain click: one dab
        click(&ctx, &mut local, egui::pos2(30.0, 90.0), egui::Modifiers::default());
        let pts = points_of(&local.as_ref().unwrap().layers[0].mask);
        assert_eq!((pts.len(), pts[2].len()), (3, 1));
    }

    /// [ ] change the size by a quarter, ⇧[ ⇧] the hardness by 10 %; a brush not drawn yet takes it at once.
    #[test]
    fn bracket_keys_change_size_and_hardness() {
        let ctx = Context::default();
        let mut local = brush_mask(&ctx);
        let b0 = state(&ctx, 0).brush.unwrap_or(BRUSH);
        frame_of(&ctx, &mut local, key_event(egui::Key::CloseBracket, false));
        let b = state(&ctx, 0).brush.unwrap();
        assert!((b[0] - b0[0] * 1.25).abs() < 1e-6 && b[1] == b0[1]);
        frame_of(&ctx, &mut local, key_event(egui::Key::OpenBracket, false));
        frame_of(&ctx, &mut local, key_event(egui::Key::OpenBracket, false));
        let b = state(&ctx, 0).brush.unwrap();
        assert!((b[0] - b0[0] / 1.25).abs() < 1e-6);
        frame_of(&ctx, &mut local, key_event(egui::Key::OpenBracket, true));
        let b = state(&ctx, 0).brush.unwrap();
        assert!((b[1] - (b0[1] - 0.1)).abs() < 1e-6, "{b:?}");
        for _ in 0..12 {
            frame_of(&ctx, &mut local, key_event(egui::Key::CloseBracket, true));
        }
        assert_eq!(state(&ctx, 0).brush.unwrap()[1], 1.0);
        // the placeholder of the brush mask took the settings
        let Mask::Brush { radius, hardness, .. } = local.as_ref().unwrap().layers[0].mask else { panic!() };
        assert_eq!([radius, hardness], [state(&ctx, 0).brush.unwrap()[0], 1.0]);
    }

    /// "Auto-Maske": a stroke takes the colour under its start from the image the masks see (also when that image comes
    /// only after the stroke began); strokes without the switch have none; the overlay is computed on that image.
    #[test]
    fn auto_mask_strokes_take_the_colour_under_their_start() {
        let ctx = Context::default();
        let idx = 7;
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, crate::mask_list_ui::NewKind::Brush);
        s.draw = true;
        s.auto = true;
        s.image = idx;
        ctx.data_mut(|d| d.insert_temp(key(), s));
        let run = |ctx: &Context, local: &mut Option<Document>, events: Vec<egui::Event>| {
            let frame = crate::Frame::from_settings(&develop::Settings::default(), 200, 100, 0);
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), events, ..Default::default() };
            crate::ui_test::run(ctx, raw, |ui| {
                let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                overlay(ui, &resp, ui.painter(), rect, &frame, [200.0, 100.0], local, idx);
            })
        };
        let stroke = |ctx: &Context, local: &mut Option<Document>, a: egui::Pos2, b: egui::Pos2| {
            run(ctx, local, vec![egui::Event::PointerMoved(a), button(a, true)]);
            for k in 1..=5 {
                run(ctx, local, vec![egui::Event::PointerMoved(a + (b - a) * (k as f32 / 5.0))]);
            }
            run(ctx, local, vec![button(b, false)]);
        };
        // no image yet: the stroke waits for its colour
        crate::mask_sample::set_for_test(idx, None);
        run(&ctx, &mut local, Vec::new());
        stroke(&ctx, &mut local, egui::pos2(30.0, 25.0), egui::pos2(180.0, 30.0));
        assert!(state(&ctx, idx).auto_pending.is_some());
        // the image: sky (top half) over dark ground
        let (w, h) = (64usize, 32usize);
        let data = (0..w * h).flat_map(|i| if i / w < h / 2 { [0.4f32, 0.5, 0.8] } else { [0.05, 0.08, 0.03] }).collect();
        crate::mask_sample::set_for_test(idx, Some(develop::local::Captured { width: w, height: h, data }));
        run(&ctx, &mut local, Vec::new());
        assert!(state(&ctx, idx).auto_pending.is_none());
        // a second stroke starting on the ground takes the ground's colour; one without the switch none
        stroke(&ctx, &mut local, egui::pos2(30.0, 95.0), egui::pos2(180.0, 90.0));
        let mut st = state(&ctx, idx);
        st.auto = false;
        ctx.data_mut(|d| d.insert_temp(key(), st));
        stroke(&ctx, &mut local, egui::pos2(30.0, 60.0), egui::pos2(180.0, 60.0));
        let mut autos = Vec::new();
        local.as_ref().unwrap().layers[0].mask.visit(&mut |m| {
            if let Mask::Brush { auto, .. } = m {
                autos.push(auto.map(|a| (a.reference, a.tolerance)));
            }
        });
        let near = |a: Option<([f32; 3], f32)>, c: [f32; 3]| a.is_some_and(|(r, t)| t == AUTO_TOLERANCE && r.iter().zip(c).all(|(x, y)| (x - y).abs() < 1e-5));
        assert!(autos.len() == 3 && near(autos[0], [0.4, 0.5, 0.8]) && near(autos[1], [0.05, 0.08, 0.03]) && autos[2].is_none(), "{autos:?}");
        assert!(crate::mask_sample::needs_image(&local.as_ref().unwrap().layers[0].mask));
        crate::mask_sample::set_for_test(idx, None);
    }

    /// The overlay while painting a mask of 200 strokes: time of one frame (overlay computed again for the new point).
    #[test]
    fn painting_over_many_strokes_stays_quick() {
        // processor time of this thread, not wall time: other tests and builds on the machine must not make a quick
        // frame look slow (the work of a frame runs on this thread)
        fn thread_ms() -> f64 {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                extern "C" {
                    fn clock_gettime(clock: i32, tp: *mut [i64; 2]) -> i32;
                }
                const THREAD_CPUTIME: i32 = if cfg!(target_os = "macos") { 16 } else { 3 };
                let mut t = [0i64; 2];
                // SAFETY: valid pointer to a timespec-sized buffer (two 64-bit fields on these targets)
                if unsafe { clock_gettime(THREAD_CPUTIME, &mut t) } == 0 {
                    return t[0] as f64 * 1000.0 + t[1] as f64 / 1e6;
                }
            }
            static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
        }
        let ctx = Context::default();
        let strokes: Vec<Mask> = (0..200)
            .map(|i| Mask::Brush { points: (0..500).map(|j| { let t = j as f32 * 0.02 + i as f32; [0.5 + 0.4 * (t * 0.37).sin() * (t * 0.011).cos(), 0.5 + 0.4 * (t * 0.23).cos()] }).collect(), radius: 0.04, hardness: 0.6, flow: 0.2, density: 1.0, auto: None })
            .collect();
        fn tree(v: &[Mask]) -> Mask {
            if v.len() == 1 {
                return v[0].clone();
            }
            let n = v.len() / 2;
            Mask::Combine { operation: Operation::Add, left: Box::new(tree(&v[..n])), right: Box::new(tree(&v[n..])) }
        }
        let mut local = Some(Document { version: 2, layers: vec![Layer::new(tree(&strokes), Adjustments { exposure: 0.5, ..Default::default() })] });
        ctx.data_mut(|d| d.insert_temp(key(), State { draw: true, ..Default::default() }));
        frame_of(&ctx, &mut local, Vec::new());
        let a = egui::pos2(40.0, 40.0);
        frame_of(&ctx, &mut local, vec![egui::Event::PointerMoved(a), button(a, true)]);
        let mut times = Vec::new();
        for k in 1..=30 {
            let p = a + egui::vec2(k as f32 * 4.0, (k as f32 * 0.5).sin() * 20.0);
            let t = thread_ms();
            let out = frame_of(&ctx, &mut local, vec![egui::Event::PointerMoved(p)]);
            times.push(thread_ms() - t);
            assert!(drawn(&out).meshes >= 1);
        }
        let first = times[0];
        times.sort_by(f64::total_cmp);
        println!("painting over 200 strokes (overlay 512 px): first frame {first:.1} ms, median {:.2} ms, max {:.2} ms", times[15], times[29]);
        assert_eq!(crate::mask_parts::strokes(&local.as_ref().unwrap().layers[0].mask), 201);
        assert!(times[15] < 16.0, "median frame {:.1} ms", times[15]);
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

/// Red overlay of the weights of `mask` (computed small on a neutral grey image, cached while the mask is unchanged),
/// drawn as a mesh through the same transformation as the photo (crop, rotation, perspective).
/// The borders of the segments of a depth mask (segment map `id`) as thin light lines over the photo.
fn segments_overlay(ui: &Ui, painter: &egui::Painter, rect: egui::Rect, frame: &crate::Frame, full: [f32; 2], id: &str) {
    let key = egui::Id::unique("mask_segments_overlay");
    let cached = ui.ctx().data_mut(|d| d.get_temp::<(String, egui::TextureHandle)>(key));
    let tex = match cached {
        Some((i, t)) if i == id => t,
        _ => {
            let Some(map) = masks::raster::resolve_depth(id) else { return };
            let (w, h) = (map.width as usize, map.height as usize);
            let pixels: Vec<Color32> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let l = map.data[i];
                    let border = (x + 1 < w && map.data[i + 1] != l) || (y + 1 < h && map.data[i + w] != l);
                    if border { Color32::from_rgba_unmultiplied(255, 255, 255, 170) } else { Color32::TRANSPARENT }
                })
                .collect();
            let t = ui.ctx().load_texture("mask_segments", egui::ColorImage::new([w, h], pixels), egui::TextureOptions::NEAREST);
            ui.ctx().data_mut(|d| d.insert_temp(key, (id.to_string(), t.clone())));
            t
        }
    };
    texture_overlay(painter, rect, frame, full, &tex);
}

/// A texture in the mask space laid over the shown photo (following crop and rotation of `frame`).
fn texture_overlay(painter: &egui::Painter, rect: egui::Rect, frame: &crate::Frame, full: [f32; 2], tex: &egui::TextureHandle) {
    const N: u32 = 24;
    let mut mesh = egui::Mesh::with_texture(tex.id());
    for j in 0..=N {
        for i in 0..=N {
            let uv = egui::pos2(i as f32 / N as f32, j as f32 / N as f32);
            let u = frame.from_src([uv.x * full[0], uv.y * full[1]]);
            mesh.vertices.push(egui::epaint::Vertex { pos: rect.min + egui::vec2(u[0], u[1]) * rect.size(), uv, color: Color32::WHITE });
        }
    }
    for j in 0..N {
        for i in 0..N {
            let a = j * (N + 1) + i;
            mesh.add_triangle(a, a + 1, a + N + 2);
            mesh.add_triangle(a, a + N + 2, a + N + 1);
        }
    }
    painter.with_clip_rect(rect).add(egui::Shape::mesh(mesh));
}

fn weights_overlay(ui: &Ui, painter: &egui::Painter, rect: egui::Rect, frame: &crate::Frame, full: [f32; 2], layer: &Layer, index: usize) {
    use std::hash::{Hash, Hasher};
    let mask = &layer.mask;
    // colour and brightness ranges (and a refined edge) are computed on the image the mask sees, when it is there
    let content = (crate::mask_sample::needs_image(mask) || layer.refines()).then(|| crate::mask_sample::image(index)).flatten();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mode = overlay_mode(ui.ctx());
    (stamp(mask), layer.refines().then_some(layer.refine.to_bits()), full[0] as u32, full[1] as u32, content.as_ref().map(|c| std::sync::Arc::as_ptr(c) as usize), mode as u8).hash(&mut h);
    let stamp = h.finish();
    let id = egui::Id::unique("mask_weights_overlay");
    let cached = ui.ctx().data_mut(|d| d.get_temp::<(u64, egui::TextureHandle)>(id));
    let tex = match cached {
        Some((st, t)) if st == stamp => t,
        _ => {
            let scale = 512.0 / full[0].max(full[1]).max(1.0);
            let (w, hh) = (((full[0] * scale).round() as usize).max(1), ((full[1] * scale).round() as usize).max(1));
            let base = content.as_ref().map_or_else(|| vec![0.18f32; w * hh * 3], |c| crate::mask_sample::resized(c, w, hh));
            let Ok(img) = masks::Image::new(w, hh, &base) else { return };
            let view = masks::View { full: [w, hh], offset: [0, 0] };
            let Ok(wts) = layer.weights(img, view) else { return };
            let pixels: Vec<Color32> = wts.iter().map(|v| mode.pixel(*v)).collect();
            let t = ui.ctx().load_texture("mask_weights", egui::ColorImage::new([w, hh], pixels), egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|d| d.insert_temp(id, (stamp, t.clone())));
            t
        }
    };
    texture_overlay(painter, rect, frame, full, &tex);
}

/// How the overlay shows the weights of a mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Overlay {
    /// red over the selected area
    #[default]
    Red,
    /// the mask itself: white where it acts, black elsewhere
    Mask,
    /// the photo only where the mask acts, the rest covered with grey
    ImageOnGrey,
    /// red over the area the mask leaves out (as masking film)
    Rubylith,
}

impl Overlay {
    pub(crate) const ALL: [Overlay; 4] = [Overlay::Red, Overlay::Mask, Overlay::ImageOnGrey, Overlay::Rubylith];

    /// German name (translated when shown).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Overlay::Red => "Rot",
            Overlay::Mask => "Maske Schwarz-Weiß",
            Overlay::ImageOnGrey => "Bild auf Grau",
            Overlay::Rubylith => "Abdeckfolie (Rest rot)",
        }
    }

    /// Colour of a pixel of the overlay with mask weight `w` (0..1).
    pub(crate) fn pixel(self, w: f32) -> Color32 {
        let w = w.clamp(0.0, 1.0);
        match self {
            Overlay::Red => Color32::from_rgba_unmultiplied(230, 40, 40, (w * 140.0) as u8),
            Overlay::Mask => Color32::from_gray((w * 255.0).round() as u8),
            Overlay::ImageOnGrey => Color32::from_rgba_unmultiplied(128, 128, 128, ((1.0 - w) * 235.0) as u8),
            Overlay::Rubylith => Color32::from_rgba_unmultiplied(220, 30, 30, ((1.0 - w) * 150.0) as u8),
        }
    }
}

fn overlay_id() -> egui::Id {
    egui::Id::unique("mask_overlay_mode")
}

/// The kind of overlay chosen (for all photos of the session).
pub(crate) fn overlay_mode(ctx: &Context) -> Overlay {
    ctx.data_mut(|d| d.get_temp::<Overlay>(overlay_id()).unwrap_or_default())
}

fn set_overlay_mode(ctx: &Context, m: Overlay) {
    ctx.data_mut(|d| d.insert_temp(overlay_id(), m));
}

/// ⇧O: off → on (the kind chosen last) → the next kinds → off after the last kind.
fn next_overlay(on: bool, mode: Overlay) -> (bool, Overlay) {
    if !on {
        return (true, mode);
    }
    let i = Overlay::ALL.iter().position(|m| *m == mode).unwrap_or(0);
    if i + 1 < Overlay::ALL.len() {
        (true, Overlay::ALL[i + 1])
    } else {
        (false, Overlay::ALL[0])
    }
}

/// Masks of the test hook `GRAINEO_MASKS_TEST=demo`: a sky (gradient ∩ brightness, 80 %), a brush part subtracted
/// from an ellipse, a switched-off colour range.
fn demo() -> Document {
    let c = |op, l: Mask, r: Mask| Mask::Combine { operation: op, left: Box::new(l), right: Box::new(r) };
    let stroke = |y: f32| Mask::Brush { points: (0..20).map(|i| [0.3 + i as f32 * 0.02, y + (i as f32 * 0.4).sin() * 0.02]).collect(), radius: 0.03, hardness: 0.6, flow: 0.6, density: 1.0, auto: None };
    let sky = Layer {
        name: Some(crate::tr("Himmel").to_string()),
        amount: 0.8,
        ..Layer::new(c(Operation::Intersect, Mask::Linear { start: [0.5, 0.0], end: [0.5, 0.5] }, Mask::Luminance { low: 0.2, high: 2.0, feather: 0.2 }), Adjustments { exposure: -0.6, saturation: 0.2, ..Default::default() })
    };
    let ground = Layer::new(c(Operation::Subtract, Mask::Radial { center: [0.5, 0.75], radii: [0.45, 0.25], angle: 0.0, feather: 0.6 }, c(Operation::Add, stroke(0.7), stroke(0.8))), Adjustments { exposure: 0.4, clarity: 0.3, ..Default::default() });
    let color = Layer { enabled: false, ..Layer::new(Mask::Color { reference: [0.2, 0.35, 0.15], radius: 0.1, feather: 0.1 }, Adjustments { saturation: -0.5, ..Default::default() }) };
    let mut d = Document { version: 2, layers: vec![sky, ground, color] };
    d.settle_version();
    d
}

/// Test hook `GRAINEO_MASKS_TEST` (screenshots, own data folder only): the part before the first `;`.
fn masks_test() -> Result<String, std::env::VarError> {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return Err(std::env::VarError::NotPresent);
    }
    crate::env::var("GRAINEO_MASKS_TEST").map(|v| v.split(';').next().unwrap_or_default().to_string())
}

/// Option `key=value` of the test hook after the first `;` (e.g. `subject;edge=2`).
pub(crate) fn masks_test_opt(key: &str) -> Option<String> {
    crate::env::var_os("GRAINEO_DATA_DIR")?;
    let v = crate::env::var("GRAINEO_MASKS_TEST").ok()?;
    v.split(';').skip(1).find_map(|kv| kv.strip_prefix(key).and_then(|r| r.strip_prefix('=')).map(str::to_string))
}

/// Test options that change the selected mask once it is there (screenshots): `edge=<%>` sets the edge shift of the
/// first computed raster leaf.
fn test_options(d: &mut Document, selected: usize, done: &mut bool) {
    // "refine=<%>": "Kante verfeinern" of the selected mask (screenshots; kept while the test runs)
    if let (Some(r), Some(l)) = (masks_test_opt("refine").and_then(|v| v.parse::<f32>().ok()), d.layers.get_mut(selected)) {
        l.refine = (r / 100.0).clamp(0.0, 1.0);
    }
    if *done {
        return;
    }
    let Some(l) = d.layers.get_mut(selected) else { return };
    // "grade": warm highlights, cool shadows, whites, dehaze and an S curve on the selected mask
    if masks_test_opt("grade").is_some() && l.adjustments.curve.is_empty() {
        l.adjustments = Adjustments {
            whites: 0.3,
            blacks: -0.2,
            dehaze: 0.4,
            grade_shadows: [220.0, 0.6],
            grade_highlights: [40.0, 0.7],
            curve: vec![[0.0, 0.0], [0.25, 0.18], [0.75, 0.84], [1.0, 1.0]],
            ..Default::default()
        };
    }
    // depth mask: "classes=near,far" chooses the classes, "toggle=x,y|x,y" clicks segments (once the map is there)
    if masks_test_opt("classes").is_some() || masks_test_opt("toggle").is_some() {
        if let Some(Mask::Raster { raster, source: masks::RasterSource::Depth { classes, toggled, .. }, .. }) = crate::subject_ui::first_raster(&mut l.mask) {
            if !raster.is_empty() && masks::raster::resolve_depth(raster).is_some() {
                if let Some(c) = masks_test_opt("classes") {
                    *classes = masks::DepthClass::ALL.into_iter().filter(|k| c.split(',').any(|n| format!("{k:?}").eq_ignore_ascii_case(n.trim()))).collect();
                }
                for p in masks_test_opt("toggle").unwrap_or_default().split('|') {
                    let v: Vec<f32> = p.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if v.len() == 2 {
                        crate::depth_ui::toggle_segment(raster, toggled, [v[0], v[1]]);
                    }
                }
                *done = true;
            }
        }
    }
    if let Some(e) = masks_test_opt("edge").and_then(|v| v.parse::<f32>().ok()) {
        if let Some(Mask::Raster { raster, edge, .. }) = crate::subject_ui::first_raster(&mut l.mask) {
            if !raster.is_empty() {
                *edge = (e / 100.0).clamp(-masks::EDGE_MAX, masks::EDGE_MAX);
                *done = true;
            }
        }
    }
}

/// Selected row after row `deleted` was removed (`len`: rows left): the same row stays selected; when the selected
/// row itself went, the one that took its place (or the last).
fn selection_after_delete(selected: usize, deleted: usize, len: usize) -> usize {
    let s = if deleted < selected { selected - 1 } else { selected };
    s.min(len.saturating_sub(1))
}

#[cfg(test)]
mod list_tests {
    use super::*;
    use crate::mask_list_ui::{LayerAction as A, NewKind, RowAction as R};

    /// The entries of the list and of the "⋯" menu change the document as named and keep the selection on the mask.
    #[test]
    fn list_actions() {
        let mut s = State::default();
        let mut local = None;
        for k in [NewKind::Linear, NewKind::Radial, NewKind::Brush] {
            assert!(create(&mut s, &mut local, k).is_none());
        }
        let d = local.as_mut().unwrap();
        assert_eq!((d.layers.len(), s.selected, d.version), (3, 2, 2));
        apply_row(&mut s, d, R::Menu(2, A::Up));
        assert_eq!(s.selected, 1);
        assert!(matches!(d.layers[1].mask, Mask::Brush { .. }));
        apply_row(&mut s, d, R::Menu(1, A::Duplicate));
        assert_eq!((d.layers.len(), s.selected), (4, 2));
        apply_row(&mut s, d, R::Renamed(0, Some("Himmel\n".into())));
        assert_eq!(d.layers[0].name.as_deref(), Some("Himmel"));
        assert_eq!(d.version, 4);
        apply_row(&mut s, d, R::Toggle(3));
        assert!(!d.layers[3].enabled);
        apply_row(&mut s, d, R::Renamed(0, None));
        apply_row(&mut s, d, R::Toggle(3));
        assert_eq!(d.version, 2, "back to version 2 without the new fields");
        apply_row(&mut s, d, R::Menu(0, A::Invert));
        assert!(matches!(d.layers[0].mask, Mask::Invert { .. }));
        d.layers[0].adjustments.exposure = 0.5;
        apply_row(&mut s, d, R::Menu(0, A::ResetSliders));
        assert_eq!(d.layers[0].adjustments, Adjustments::default());
        apply_row(&mut s, d, R::Menu(2, A::Delete));
        assert_eq!(d.layers.len(), 3);
        assert!(s.selected < 3);
        d.validate().unwrap();
        // deleting another mask keeps the selected one selected (also when it is the last)
        d.layers[2].name = Some("C".into());
        s.selected = 2;
        apply_row(&mut s, d, R::Menu(0, A::Delete));
        assert_eq!((d.layers.len(), s.selected, d.layers[s.selected].name.as_deref()), (2, 1, Some("C")));
        // version 1: name and switch stay locked until the filters are switched to version 2
        let mut v1 = Document { version: 1, layers: vec![Layer::new(shape(2), Adjustments { exposure: 0.3, ..Default::default() })] };
        apply_row(&mut s, &mut v1, R::Toggle(0));
        apply_row(&mut s, &mut v1, R::Renamed(0, Some("x".into())));
        assert!(v1.layers[0].enabled && v1.layers[0].name.is_none() && v1.version == 1);
        apply_row(&mut s, &mut v1, R::Menu(0, A::ToVersion2));
        apply_row(&mut s, &mut v1, R::Toggle(0));
        assert_eq!((v1.version, v1.layers[0].enabled), (4, false));
    }

    /// A depth leaf counts as present after its 16-bit map (review 1.3.0, logic N4: the stamp asked the 8-bit store,
    /// which never has it, and so read the library each frame).
    #[test]
    fn stamp_sees_a_depth_map() {
        let data = std::sync::Arc::new(masks::DepthData { width: 3, height: 1, data: vec![1, 2, 3] });
        let id = data.id();
        let leaf = |raster: &str| Mask::Raster { raster: raster.into(), source: masks::RasterSource::Depth { model: "depth".into(), classes: vec![masks::DepthClass::Near], toggled: vec![] }, feather: 0.0, range: None, edge: 0.0 };
        let missing = stamp(&leaf(&id));
        masks::raster::remember_depth(data);
        assert_ne!(stamp(&leaf(&id)), missing);
    }

    /// Cases of the review: the selected row stays selected when another one goes; when it goes itself, its
    /// successor (or the new last) is selected.
    #[test]
    fn selection_after_deleting_a_row() {
        for (n, selected, deleted) in [(3, 2, 0), (3, 2, 1), (3, 1, 0), (4, 3, 1), (3, 0, 1), (3, 0, 2), (3, 2, 2), (3, 1, 1), (1, 0, 0), (2, 1, 0)] {
            let mut rows: Vec<usize> = (0..n).collect();
            let was = rows[selected];
            rows.remove(deleted);
            let now = selection_after_delete(selected, deleted, rows.len());
            if deleted != selected {
                assert_eq!(rows[now], was, "n {n}, selected {selected}, deleted {deleted}");
            } else if !rows.is_empty() {
                assert_eq!(now, selected.min(rows.len() - 1), "n {n}, deleted the selected {selected}");
            } else {
                assert_eq!(now, 0);
            }
        }
    }

    /// "Hintergrund": the subject inverted in one step, as a new mask or as a part; named so, analysis requested.
    #[test]
    fn background_is_an_inverted_subject() {
        let mut s = State::default();
        let mut local = None;
        if segment::available(segment::Kind::Subject) != segment::Support::None {
            assert_eq!(create(&mut s, &mut local, NewKind::Background), Some(crate::subject_ui::Request::Analyse(segment::Kind::Subject)));
            let l = &local.as_ref().unwrap().layers[0];
            assert!(crate::mask_list_ui::is_background(&l.mask));
            assert_eq!(crate::mask_list_ui::display_name(l), crate::tr("Hintergrund"));
            assert_eq!(crate::mask_list_ui::parts_hint(&l.mask), "");
        }
        let mut local = None;
        create(&mut s, &mut local, NewKind::Radial);
        let layer = &mut local.as_mut().unwrap().layers[0];
        assert!(add_part(&mut s, layer, Operation::Intersect, NewKind::Background).is_some());
        let p = crate::mask_parts::part(&layer.mask, 1).unwrap();
        assert!(crate::mask_list_ui::is_background(p));
        assert_eq!(crate::mask_list_ui::kind_label(p), "Hintergrund");
    }

    /// The click selection chosen as a new mask never adds to the selected mask.
    #[test]
    fn new_click_selection_is_a_new_mask() {
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, NewKind::Click);
        assert!(s.click && s.click_new && local.is_none());
        let first = crate::subject_ui::clicked_point(&mut local, usize::MAX, [0.4, 0.4], false);
        assert_eq!(first, Some(0));
        let again = crate::subject_ui::clicked_point(&mut local, usize::MAX, [0.6, 0.6], false);
        assert_eq!(again, Some(1));
        assert_eq!(crate::subject_ui::clicked_point(&mut local, 1, [0.6, 0.7], false), Some(1));
        assert_eq!(local.unwrap().layers.len(), 2);
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    /// ⇧O goes through all kinds and then off; the kinds colour full and empty weights as named.
    #[test]
    fn shift_o_cycles_and_kinds_paint_as_named() {
        let mut state = (false, Overlay::Red);
        let mut seen = Vec::new();
        for _ in 0..5 {
            state = next_overlay(state.0, state.1);
            seen.push(state);
        }
        assert_eq!(seen, vec![(true, Overlay::Red), (true, Overlay::Mask), (true, Overlay::ImageOnGrey), (true, Overlay::Rubylith), (false, Overlay::Red)]);
        // red: only where the mask acts; rubylith: only where it does not
        assert_eq!((Overlay::Red.pixel(0.0).a(), Overlay::Rubylith.pixel(1.0).a()), (0, 0));
        assert!(Overlay::Red.pixel(1.0).a() > 100 && Overlay::Rubylith.pixel(0.0).a() > 100);
        // the mask in black and white, opaque
        assert_eq!((Overlay::Mask.pixel(0.0), Overlay::Mask.pixel(1.0)), (Color32::BLACK, Color32::WHITE));
        // image on grey: the photo shows through where the mask acts
        assert_eq!(Overlay::ImageOnGrey.pixel(1.0).a(), 0);
        assert!(Overlay::ImageOnGrey.pixel(0.0).a() > 200);
    }
}

#[cfg(test)]
mod box_tests {
    use super::*;
    use masks::RasterSource;

    fn rect_of(m: &Mask) -> Option<[f32; 4]> {
        let mut r = None;
        m.visit(&mut |n| {
            if let Mask::Raster { source: RasterSource::Click { rect, .. }, .. } = n {
                r = r.or(*rect);
            }
        });
        r
    }

    /// A box of the click selection makes a new mask, a new part, or replaces the box of the selected click selection.
    #[test]
    fn a_box_makes_a_mask_a_part_or_a_new_box() {
        let mut s = State { click: true, click_new: true, ..Default::default() };
        let mut local = None;
        assert_eq!(boxed(&mut local, &mut s, [0.1, 0.2, 0.5, 0.6]), Some(0));
        let d = local.as_ref().unwrap();
        assert_eq!((d.layers.len(), d.version), (1, 4));
        assert_eq!(rect_of(&d.layers[0].mask), Some([0.1, 0.2, 0.5, 0.6]));
        assert!(!s.click_new);
        // the next box replaces it (points stay, raster pending again)
        assert_eq!(boxed(&mut local, &mut s, [0.2, 0.2, 0.4, 0.4]), Some(0));
        assert_eq!(local.as_ref().unwrap().layers.len(), 1);
        assert_eq!(rect_of(&local.as_ref().unwrap().layers[0].mask), Some([0.2, 0.2, 0.4, 0.4]));
        // as a new part with "− Abziehen"
        s.click_part = Some(Operation::Subtract);
        boxed(&mut local, &mut s, [0.3, 0.3, 0.35, 0.35]);
        let m = &local.as_ref().unwrap().layers[0].mask;
        assert_eq!(crate::mask_parts::operations(m), vec![None, Some(Operation::Subtract)]);
        assert_eq!(s.part, 1);
        local.unwrap().validate().unwrap();
    }
}

#[cfg(test)]
mod parts_tests {
    use super::*;
    use crate::mask_list_ui::NewKind;

    /// Adding parts with + − ∩: the operation comes from the button, the kind from the tile; the new part is
    /// selected and its tool armed; the rest of the tree is kept.
    #[test]
    fn parts_are_added_selected_and_armed() {
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, NewKind::Radial);
        assert!(s.armed && s.fresh && s.tool == PartTool::Shape);
        let layer = &mut local.as_mut().unwrap().layers[0];
        assert!(add_part(&mut s, layer, Operation::Subtract, NewKind::Brush).is_none());
        assert_eq!((s.part, s.tool, s.armed), (1, PartTool::Brush, true));
        assert_eq!(crate::mask_parts::operations(&layer.mask), vec![None, Some(Operation::Subtract)]);
        let r = add_part(&mut s, layer, Operation::Intersect, NewKind::Subject);
        assert_eq!(r, Some(crate::subject_ui::Request::Analyse(segment::Kind::Subject)));
        assert_eq!(s.part, 2);
        assert!(!s.armed, "a subject part has no tool in the image");
        // a click selection part starts with the next click
        assert!(add_part(&mut s, layer, Operation::Add, NewKind::Click).is_none());
        assert_eq!((s.click, s.click_part), (true, Some(Operation::Add)));
        assert_eq!(crate::mask_parts::count(&layer.mask), 3);
        let mut d = local.unwrap();
        d.settle_version();
        assert_eq!(d.version, 3);
        d.validate().unwrap();
        // selecting the first part arms the handles of the ellipse again
        select_part(&mut s, &d.layers[0].mask, 0);
        assert!(s.armed && !s.fresh && s.tool == PartTool::Shape && !s.click);
    }

    /// A gradient's handles move only their own end; the ellipse's centre moves the whole ellipse.
    #[test]
    fn handles_move_their_point() {
        let full = [300.0, 200.0];
        let mut m = Mask::Linear { start: [0.2, 0.5], end: [0.8, 0.5] };
        move_handle(&mut m, Handle::End, [0.7, 0.3], [0.8, 0.5], full);
        assert_eq!(m, Mask::Linear { start: [0.2, 0.5], end: [0.7, 0.3] });
        let mut e = Mask::Radial { center: [0.5, 0.5], radii: [0.2, 0.1], angle: 0.0, feather: 0.5 };
        move_handle(&mut e, Handle::Center, [0.6, 0.55], [0.5, 0.5], full);
        let Mask::Radial { center, radii, .. } = e else { unreachable!() };
        assert!((center[0] - 0.6).abs() < 1e-6 && (center[1] - 0.55).abs() < 1e-6);
        assert_eq!(radii, [0.2, 0.1]);
        let h = handles(&e, full);
        assert_eq!(h.len(), 3);
        // the end of the first axis lies at centre + radius (in units of the short side)
        assert!((h[1].1[0] - (0.6 + 0.2 * 200.0 / 300.0)).abs() < 1e-6);
    }

    /// Strokes go into the selected brush part, not into the other parts.
    #[test]
    fn strokes_go_into_the_selected_part() {
        let ctx = Context::default();
        let mut s = State::default();
        let mut local = None;
        create(&mut s, &mut local, NewKind::Radial);
        let layer = &mut local.as_mut().unwrap().layers[0];
        add_part(&mut s, layer, Operation::Subtract, NewKind::Brush);
        // the darkroom-style switch acts without the open tool, so the test needs no rail
        s.draw = true;
        ctx.data_mut(|d| d.insert_temp(key(), s));
        let frame = crate::Frame::from_settings(&develop::Settings::default(), 200, 100, 0);
        let run = |events: Vec<egui::Event>, local: &mut Option<Document>| {
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), events, ..Default::default() };
            let _ = crate::ui_test::run(&ctx, raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 100.0));
                    let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                    overlay(ui, &resp, ui.painter(), rect, &frame, [200.0, 100.0], local, 0);
                });
            });
        };
        run(Vec::new(), &mut local);
        let b = |p: egui::Pos2, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        for (a, z) in [(egui::pos2(40.0, 40.0), egui::pos2(90.0, 60.0)), (egui::pos2(100.0, 30.0), egui::pos2(150.0, 70.0))] {
            run(vec![egui::Event::PointerMoved(a), b(a, true)], &mut local);
            run(vec![egui::Event::PointerMoved(z)], &mut local);
            run(vec![b(z, false)], &mut local);
        }
        let m = &local.as_ref().unwrap().layers[0].mask;
        assert!(matches!(crate::mask_parts::part(m, 0), Some(Mask::Radial { .. })), "the ellipse is untouched");
        assert_eq!(crate::mask_parts::strokes(crate::mask_parts::part(m, 1).unwrap()), 2);
        m.validate().unwrap();
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// Left edge of the ink of the first glyph of every text drawn (and the right end of the text), in drawing order.
    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, f32, f32)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(String, f32, f32)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => {
                    if let Some(r) = t.galley.rows.first() {
                        if let Some(g) = r.row.glyphs.first() {
                            out.push((t.galley.job.text.clone(), t.pos.x + r.pos.x + g.pos.x + g.uv_rect.offset.x, t.pos.x + t.galley.rect.right()));
                        }
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, &mut out));
        out
    }

    /// One left edge for the Masks tray: category headings, slider names, notes, the tool line, the status line and
    /// the symbols of the parts list start at the same x; inside an open group (Licht) the slider names line up with
    /// the group title, one clear step further right. Values at the right end stay inside the tray.
    #[test]
    fn tray_rows_share_one_left_edge() {
        let ctx = egui::Context::default();
        let width = 330.0;
        let mut shapes = Vec::new();
        let mut right = 0.0;
        let path = std::path::Path::new("/nonexistent/layout-test.RAF");
        for _pass in 0..2 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 1400.0))), ..Default::default() };
            let out = crate::ui_test::run(&ctx, input, |ui| {
                ui.allocate_ui(egui::vec2(width, 1300.0), |ui| {
                    ui.set_width(width);
                    right = ui.max_rect().right();
                    let mut s = State { click: true, ..Default::default() };
                    let mut layer = demo().layers.remove(0);
                    tool_line(ui, &s, Some(&layer));
                    crate::analysis_progress::status_line(ui, path, &[crate::analysis_progress::Tool::Subject], Some("Auswahl berechnet"));
                    let _ = parts_section(ui, &mut s, &mut layer, "Himmel", path);
                    dim(ui, "Anfang und Ende im Bild ziehen");
                    strength(ui, &mut layer, true);
                    corrections(ui, &mut layer.adjustments, true);
                });
            });
            shapes = out.shapes;
        }
        let t = texts(&shapes);
        let x = |name: &str| t.iter().find(|(s, _, _)| s == name || s.starts_with(name)).unwrap_or_else(|| panic!("{name} not drawn: {:?}", t.iter().map(|t| &t.0).collect::<Vec<_>>())).1;
        let edge = x("WIRKUNG");
        for name in ["TEILE · HIMMEL", "KORREKTUREN", "STÄRKE (%)", "Anfang und Ende im Bild ziehen", "Klick-Auswahl:", "Auswahl berechnet", "●"] {
            assert!((x(name) - edge).abs() <= 1.0, "{name} starts at {}, the headings at {edge}", x(name));
        }
        let group = x("LICHT");
        assert!((x("BELICHTUNG") - group).abs() <= 1.0, "slider names in a group at {}, its title at {group}", x("BELICHTUNG"));
        assert!(group - edge >= 12.0, "group content not clearly indented: {group} vs {edge}");
        for (s, _, r) in &t {
            assert!(*r <= right + 0.5, "{s} runs past the tray ({r} > {right})");
        }
    }
}
