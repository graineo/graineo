//! Masks tool: the list of masks (eye, thumbnail, name, strength), the tiles of a new mask or part and the menus of a
//! mask. Only drawing and the chosen action; `masks_ui` changes the document.
use std::path::Path;

use egui::{Color32, Ui};
use masks::{Adjustments, Document, Layer, Mask, RasterSource};

use crate::lr;
use crate::mask_parts;

/// What a tile creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    Subject,
    /// the subject inverted: everything but the subject
    Background,
    People,
    Click,
    Brush,
    Linear,
    Radial,
    Luminance,
    Color,
    Depth,
    Sky,
    /// a part of people (face landmarks, macOS)
    Part(masks::PersonPart),
}

impl NewKind {
    /// German name (translated when drawn).
    pub fn label(self) -> &'static str {
        match self {
            NewKind::Subject => "Motiv",
            NewKind::Background => "Hintergrund",
            NewKind::People => "Personen",
            NewKind::Click => "Klick-Auswahl",
            NewKind::Brush => "Pinsel",
            NewKind::Linear => "Verlauf",
            NewKind::Radial => "Radial",
            NewKind::Luminance => "Helligkeit",
            NewKind::Color => "Farbe",
            NewKind::Depth => "Tiefe",
            NewKind::Sky => "Himmel",
            NewKind::Part(p) => crate::face_parts::label(p),
        }
    }

    fn icon(self) -> lr::Icon {
        match self {
            NewKind::Subject => lr::Icon::UserFocus,
            NewKind::Background => lr::Icon::ImageSquare,
            NewKind::People => lr::Icon::Users,
            NewKind::Click => lr::Icon::CursorClick,
            NewKind::Brush => lr::Icon::PaintBrush,
            NewKind::Linear => lr::Icon::Gradient,
            NewKind::Radial => lr::Icon::Circle,
            NewKind::Luminance => lr::Icon::SunDim,
            NewKind::Color => lr::Icon::Palette,
            NewKind::Depth => lr::Icon::Perspective,
            NewKind::Sky => lr::Icon::Cloud,
            NewKind::Part(p) => match p {
                masks::PersonPart::Face => lr::Icon::Smiley,
                masks::PersonPart::Skin => lr::Icon::Drop,
                masks::PersonPart::Eyes => lr::Icon::Eye,
                masks::PersonPart::Brows => lr::Icon::Minus,
                masks::PersonPart::Lips => lr::Icon::CircleHalf,
                masks::PersonPart::Mouth => lr::Icon::SquaresFour,
            },
        }
    }

    fn tip(self) -> &'static str {
        match self {
            NewKind::Subject => "Hauptmotiv auswählen (Bildanalyse des Systems, ändert keine Pixel)",
            NewKind::Background => "Alles außer dem Hauptmotiv (Motiv, invertiert)",
            NewKind::People => "Personen auswählen (Bildanalyse des Systems, ändert keine Pixel)",
            NewKind::Click => "Objekt im Bild anklicken oder einen Rahmen darum ziehen; ⌥-Klick nimmt Bereiche heraus (rechnet auf dem Rechner)",
            NewKind::Brush => "Im Bild malen",
            NewKind::Linear => "Linearer Verlauf: im Bild ziehen",
            NewKind::Radial => "Radialer Verlauf (Ellipse): im Bild ziehen",
            NewKind::Luminance => "Bereich nach Helligkeit",
            NewKind::Color => "Bereich nach Farbe (Pipette)",
            NewKind::Depth => "Ganze Segmente nach ihrer Entfernung vom Objektiv (vorne, Mitte, hinten; Tiefenbild aus dem Foto oder geschätzt, ändert keine Pixel)",
            NewKind::Sky => "Himmel automatisch auswählen (Tiefe und Klick-Auswahl, ändert keine Pixel)",
            NewKind::Part(p) => crate::face_parts::tip(p),
        }
    }

    /// Can the kind be created now? `Err`: why not (tooltip of the locked tile).
    pub fn possible(self, path: &Path) -> Result<(), &'static str> {
        match self {
            NewKind::Subject | NewKind::Background | NewKind::People => {
                let k = if self == NewKind::People { segment::Kind::Person } else { segment::Kind::Subject };
                if segment::available(k) == segment::Support::None {
                    Err(if cfg!(target_os = "macos") { "Braucht macOS 14 oder neuer" } else { "Gibt es nur unter macOS" })
                } else if crate::subject_ui::running(path).is_some() || crate::subject_ui::analysing() {
                    Err("Erst möglich, wenn die laufende Bildanalyse fertig ist")
                } else {
                    Ok(())
                }
            }
            NewKind::Click if !crate::subject_ui::click_available() => Err("Braucht das Modul „Klick-Auswahl“ (Einstellungen → Module)"),
            NewKind::Depth => crate::depth_ui::depth_possible(),
            NewKind::Sky => crate::depth_ui::sky_possible(),
            NewKind::Part(_) => crate::face_parts::available().and_then(|_| if crate::face_parts::running(path) { Err("Erst möglich, wenn die laufende Bildanalyse fertig ist") } else { Ok(()) }),
            _ => Ok(()),
        }
    }
}

/// The tiles in three groups "Automatisch", "Formen", "Bereiche"; returns the chosen kind. Locked tiles stay in place
/// and tell why in their tooltip.
pub fn tiles(ui: &mut Ui, salt: &str, path: &Path) -> Option<NewKind> {
    let mut chosen = None;
    use masks::PersonPart as P;
    let groups: [(&str, &[NewKind]); 4] = [
        ("Automatisch", &[NewKind::Subject, NewKind::Background, NewKind::Sky, NewKind::People, NewKind::Click]),
        ("Formen", &[NewKind::Brush, NewKind::Linear, NewKind::Radial]),
        ("Bereiche", &[NewKind::Luminance, NewKind::Color, NewKind::Depth]),
        ("Personen-Teile", &[NewKind::Part(P::Face), NewKind::Part(P::Skin), NewKind::Part(P::Eyes), NewKind::Part(P::Brows), NewKind::Part(P::Lips), NewKind::Part(P::Mouth)]),
    ];
    for (g, (title, kinds)) in groups.iter().enumerate() {
        if g > 0 {
            ui.add_space(4.0);
        }
        let font = egui::FontId::proportional(10.0);
        let t = lr::caps_spaced(ui.painter(), title, font, lr::TEXT_DIM, 0.8);
        let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), t.size().y + 4.0), egui::Sense::hover());
        ui.painter().galley(egui::pos2(r.left() + lr::TEXT_INSET, r.top() + 1.0), t, lr::TEXT_DIM);
        let w = ui.available_width();
        let gap = 4.0;
        let cell_w = (w - 2.0 * gap) / 3.0;
        // three tiles per row; a group with more wraps into further rows
        let rows = kinds.len().div_ceil(3);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 40.0 * rows as f32 + gap * (rows - 1) as f32), egui::Sense::hover());
        for (i, kind) in kinds.iter().enumerate() {
            let (col, row) = (i % 3, i / 3);
            let cell = egui::Rect::from_min_size(rect.min + egui::vec2(col as f32 * (cell_w + gap), row as f32 * (40.0 + gap)), egui::vec2(cell_w, 40.0));
            let possible = kind.possible(path);
            let ok = possible.is_ok();
            let resp = ui.interact(cell, ui.scope_id().with((salt, i, g)), if ok { egui::Sense::click() } else { egui::Sense::hover() });
            let p = ui.painter();
            let (fill, stroke) = if resp.hovered() && ok { (Color32::from_gray(0x26), Color32::from_gray(0x50)) } else { (Color32::from_gray(0x1c), Color32::from_gray(0x30)) };
            p.rect_filled(cell, 0.0, fill);
            p.rect_stroke(cell, 0.0, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
            let color = if !ok { Color32::from_gray(0x5c) } else if resp.hovered() { Color32::WHITE } else { lr::TEXT };
            lr::paint_icon(p, egui::Rect::from_center_size(cell.center_top() + egui::vec2(0.0, 14.0), egui::vec2(17.0, 17.0)), kind.icon(), color);
            let g = p.layout_no_wrap(crate::tr(kind.label()).to_string(), egui::FontId::proportional(10.5), color);
            p.galley(egui::pos2(cell.center().x - g.size().x.min(cell_w - 4.0) / 2.0, cell.bottom() - 4.0 - g.size().y), g, color);
            lr::hover_tip(ui, &resp, crate::tr(possible.err().unwrap_or(kind.tip())));
            if resp.clicked() && ok {
                chosen = Some(*kind);
            }
        }
    }
    chosen
}

/// German name of the kind of a part (translated when drawn).
pub fn kind_label(m: &Mask) -> &'static str {
    match m {
        Mask::Brush { .. } => "Pinsel",
        Mask::Linear { .. } => "Verlauf",
        Mask::Radial { .. } => "Radial",
        Mask::Luminance { .. } => "Helligkeit",
        Mask::Color { .. } | Mask::ColorRange { .. } => "Farbbereich",
        Mask::Invert { mask } if matches!(**mask, Mask::Raster { source: RasterSource::Subject { .. }, .. }) => "Hintergrund",
        Mask::Invert { mask } => kind_label(mask),
        Mask::Combine { .. } if mask_parts::is_brush(m) => "Pinsel",
        Mask::Combine { .. } => "Zusammengesetzt",
        Mask::Raster { source, .. } => match source {
            RasterSource::Subject { .. } => "Motiv",
            RasterSource::Person { .. } => "Personen",
            RasterSource::Click { .. } => "Klick-Auswahl",
            RasterSource::PersonPart { part } => crate::face_parts::label(*part),
            RasterSource::Painted => "Bitmap",
            RasterSource::Depth { .. } => "Tiefe",
            RasterSource::Scene { .. } => "Himmel",
        },
    }
}

/// Is the mask the background: only a subject selection, inverted?
pub fn is_background(m: &Mask) -> bool {
    matches!(m, Mask::Invert { mask } if matches!(**mask, Mask::Raster { source: RasterSource::Subject { .. }, .. }))
}

/// Shown name of a mask: its own name, otherwise the kind of its first part ("Hintergrund" for an inverted subject).
pub fn display_name(l: &Layer) -> String {
    match &l.name {
        Some(n) => n.clone(),
        None if is_background(&l.mask) => crate::tr("Hintergrund").to_string(),
        None => crate::tr(mask_parts::part(&l.mask, 0).map_or("Maske", kind_label)).to_string(),
    }
}

/// Short note what a mask consists of ("2 Teile", "3 Striche", "invertiert").
pub fn parts_hint(m: &Mask) -> String {
    let n = mask_parts::count(m);
    let mut t = if n > 1 {
        crate::trf!("{n} Teile", n = n)
    } else {
        match mask_parts::part(m, 0) {
            Some(p) if mask_parts::is_brush(p) => crate::trf!("{n} Striche", n = mask_parts::strokes(p)),
            _ => String::new(),
        }
    };
    if !is_background(m) && (mask_parts::inverted(m) || mask_parts::part(m, 0).is_some_and(|p| matches!(p, Mask::Invert { .. })) && n == 1) {
        if !t.is_empty() {
            t.push_str(" · ");
        }
        t.push_str(crate::tr("invertiert"));
    }
    t
}

/// What a click in the list asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum RowAction {
    Select(usize),
    /// eye: switch the mask on or off
    Toggle(usize),
    /// an entry of the menu of mask `i`
    Menu(usize, LayerAction),
    /// finished renaming: the new name (`None`: back to the name of its kind)
    Renamed(usize, Option<String>),
}

/// Entries of the "⋯" menu of a mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerAction {
    Rename,
    Duplicate,
    Invert,
    Up,
    Down,
    ResetSliders,
    Delete,
    /// version-1 document: switch the filters to version 2 (only then name, eye and strength)
    ToVersion2,
}

/// Height of a row of the list, rows the list shows without scrolling.
pub const ROW_H: f32 = 26.0;
pub const ROWS: usize = 5;

/// The "⋯" menu of mask `i` of `n`; `fields`: name, eye and strength possible (not with version 1).
pub fn layer_menu(ui: &mut Ui, i: usize, n: usize, fields: bool, sliders_changed: bool) -> Option<LayerAction> {
    let mut a = None;
    let why_v1 = crate::tr("Erst „Filter auf Version 2 umstellen“");
    if lr::menu_item_why(ui, crate::tr("Umbenennen …"), "", fields, why_v1) {
        a = Some(LayerAction::Rename);
    }
    if lr::menu_item(ui, crate::tr("Duplizieren"), "", n < 64) {
        a = Some(LayerAction::Duplicate);
    }
    if lr::menu_item(ui, crate::tr("Maske invertieren"), "", true) {
        a = Some(LayerAction::Invert);
    }
    ui.separator();
    if lr::menu_item(ui, crate::tr("Nach oben"), "", i > 0) {
        a = Some(LayerAction::Up);
    }
    if lr::menu_item(ui, crate::tr("Nach unten"), "", i + 1 < n) {
        a = Some(LayerAction::Down);
    }
    if !fields {
        ui.separator();
        if lr::menu_item(ui, crate::tr("Filter auf Version 2 umstellen"), "", true) {
            a = Some(LayerAction::ToVersion2);
        }
    }
    ui.separator();
    if lr::menu_item(ui, crate::tr("Regler zurücksetzen"), "", sliders_changed) {
        a = Some(LayerAction::ResetSliders);
    }
    if lr::menu_item(ui, crate::tr("Maske löschen"), "⌫", true) {
        a = Some(LayerAction::Delete);
    }
    a
}

/// The list of masks (fixed height, scrolls in itself): eye, thumbnail, name, what it consists of, strength.
/// `rename`: the row being renamed and its text. Returns the action and the row under the pointer.
pub fn list(ui: &mut Ui, doc: &Document, selected: usize, rename: &mut Option<(usize, String)>, aspect: f32, index: usize) -> (Option<RowAction>, Option<usize>) {
    let mut action = None;
    let mut hovered = None;
    let fields = doc.allows_layer_fields();
    let w = lr::visible_width(ui);
    let frame_h = ROW_H * ROWS as f32 + 2.0;
    let (outer, _) = ui.allocate_exact_size(egui::vec2(w, frame_h), egui::Sense::hover());
    ui.painter().rect_filled(outer, 0.0, lr::PANEL_DARK);
    ui.painter().rect_stroke(outer, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x2c)), egui::StrokeKind::Inside);
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt("mask_list").max_rect(outer.shrink(1.0)).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::ScrollArea::vertical().id_salt("mask_list_scroll").auto_shrink([false; 2]).max_height(frame_h - 2.0).show(&mut child, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, l) in doc.layers.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
            let p = ui.painter().clone();
            if i == selected {
                p.rect_filled(rect, 0.0, Color32::from_gray(0x34));
            } else if resp.hovered() {
                p.rect_filled(rect, 0.0, Color32::from_gray(0x22));
            }
            if resp.hovered() {
                hovered = Some(i);
            }
            // eye
            let eye = egui::Rect::from_min_size(rect.min, egui::vec2(24.0, ROW_H));
            let er = ui.interact(eye, ui.scope_id().with(("eye", i)), if fields { egui::Sense::click() } else { egui::Sense::hover() });
            let ec = if !fields { Color32::from_gray(0x50) } else if er.hovered() { Color32::WHITE } else { lr::TEXT_DIM };
            lr::paint_icon(&p, egui::Rect::from_center_size(eye.center(), egui::vec2(14.0, 14.0)), if l.enabled { lr::Icon::Eye } else { lr::Icon::EyeSlash }, ec);
            lr::hover_tip(ui, &er, if fields { crate::tr(if l.enabled { "Maske ausschalten (bleibt erhalten)" } else { "Maske einschalten" }) } else { crate::tr("Erst „Filter auf Version 2 umstellen“ (⋯)") });
            if er.clicked() && fields {
                action = Some(RowAction::Toggle(i));
            }
            // thumbnail
            let th = egui::Rect::from_min_size(egui::pos2(eye.right() + 2.0, rect.center().y - 8.0), egui::vec2(24.0, 16.0));
            let tex = thumbnail(ui.ctx(), i, &l.mask, aspect, index);
            p.rect_filled(th, 0.0, Color32::from_gray(0x0c));
            if let Some(t) = tex {
                p.image(t.id(), th, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            p.rect_stroke(th, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x3a)), egui::StrokeKind::Outside);
            // strength on the right
            let text_c = if !l.enabled { Color32::from_gray(0x6a) } else if i == selected { Color32::WHITE } else { lr::TEXT };
            let pct = format!("{} %", (l.amount * 100.0).round() as i32);
            let font = egui::TextStyle::Body.resolve(ui.style());
            let gp = p.layout_no_wrap(pct, font.clone(), if l.enabled { lr::TEXT_DIM } else { Color32::from_gray(0x5a) });
            let pr = egui::pos2(rect.right() - 8.0 - gp.size().x, rect.center().y - gp.size().y / 2.0);
            p.galley(pr, gp, lr::TEXT_DIM);
            let name_x = th.right() + 8.0;
            let name_w = (pr.x - 8.0 - name_x).max(10.0);
            if rename.as_ref().is_some_and(|r| r.0 == i) {
                let r = egui::Rect::from_min_size(egui::pos2(name_x - 3.0, rect.top() + 3.0), egui::vec2(name_w + 3.0, ROW_H - 6.0));
                let mut text = rename.as_ref().map(|r| r.1.clone()).unwrap_or_default();
                let te = ui.put(r, egui::TextEdit::singleline(&mut text).char_limit(masks::NAME_MAX).id_salt(("mask_rename", i)));
                if !te.has_focus() && !te.lost_focus() {
                    te.request_focus();
                }
                let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if te.lost_focus() || cancel {
                    let t = text.trim().to_string();
                    action = Some(RowAction::Renamed(i, if cancel { l.name.clone() } else if t.is_empty() { None } else { Some(t) }));
                    *rename = None;
                } else {
                    *rename = Some((i, text));
                }
            } else {
                let name = display_name(l);
                let shown = lr::middle_ellipsis_font(&p, &name, &font, name_w);
                let g = p.layout_no_wrap(shown, font.clone(), text_c);
                let gw = g.size().x;
                p.galley(egui::pos2(name_x, rect.center().y - g.size().y / 2.0), g, text_c);
                let hint = parts_hint(&l.mask);
                let rest = name_w - gw - 8.0;
                if !hint.is_empty() && rest > 24.0 {
                    let small = egui::FontId::proportional(10.0);
                    let hint = lr::middle_ellipsis_font(&p, &hint, &small, rest);
                    let g = p.layout_no_wrap(hint, small, Color32::from_gray(0x80));
                    p.galley(egui::pos2(name_x + gw + 8.0, rect.center().y - g.size().y / 2.0 + 1.0), g, Color32::from_gray(0x80));
                }
            }
            if resp.clicked() && !er.hovered() {
                action = Some(RowAction::Select(i));
            }
            if resp.double_clicked() && !er.hovered() && fields {
                action = Some(RowAction::Menu(i, LayerAction::Rename));
            }
            resp.context_menu(|ui| {
                let changed = l.adjustments != Adjustments::default();
                if let Some(a) = layer_menu(ui, i, doc.layers.len(), fields, changed) {
                    action = Some(RowAction::Menu(i, a));
                }
            });
        }
    });
    (action, hovered)
}

/// Small picture of the weights of a mask (light = full effect) on a neutral grey image (colour and brightness ranges:
/// on the image they compare against, when it is there), cached while the mask is unchanged; `aspect` = width / height
/// of the photo, `index` the photo. One texture per row (`slot`), replaced when the mask changes.
pub fn thumbnail(ctx: &egui::Context, slot: usize, mask: &Mask, aspect: f32, index: usize) -> Option<egui::TextureHandle> {
    let content = crate::mask_sample::needs_image(mask).then(|| crate::mask_sample::image(index)).flatten();
    let stamp = crate::masks_ui::stamp(mask) ^ aspect.to_bits() as u64 ^ content.as_ref().map_or(0, |c| std::sync::Arc::as_ptr(c) as u64).rotate_left(17);
    let id = egui::Id::unique(("mask_thumbnail", slot));
    if let Some((s, t)) = ctx.data_mut(|d| d.get_temp::<(u64, egui::TextureHandle)>(id)) {
        if s == stamp {
            return Some(t);
        }
    }
    let (w, h) = if aspect >= 1.5 { (48usize, ((48.0 / aspect).round() as usize).max(1)) } else { (((32.0 * aspect).round() as usize).max(1), 32usize) };
    let base = content.as_ref().map_or_else(|| vec![0.18f32; w * h * 3], |c| crate::mask_sample::resized(c, w, h));
    let img = masks::Image::new(w, h, &base).ok()?;
    let weights = masks::rasterize(img, mask).ok()?;
    let pixels: Vec<Color32> = weights.iter().map(|v| Color32::from_gray((v.clamp(0.0, 1.0) * 220.0) as u8 + 16)).collect();
    let t = ctx.load_texture("mask_thumbnail", egui::ColorImage::new([w, h], pixels), egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, (stamp, t.clone())));
    Some(t)
}
