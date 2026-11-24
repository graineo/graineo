//! Middle column of the import window: the grid grouped by capture day (head row with its own check: all / none /
//! partly), RAW+JPEG as one cell, and the large view of one photo (space bar). Only the rows in view are drawn, so
//! 2 000 photos on a card cost no more than a screen full.

use super::*;

/// Height of a day's head row.
const HEAD_H: f32 = 26.0;
/// Height of the bar above the grid.
const BAR_H: f32 = 26.0;

enum RowKind {
    Head(usize),
    /// cells of a day: positions `from..to` in that day's visible list
    Cells(usize, usize, usize),
}

struct Row {
    y: f32,
    h: f32,
    kind: RowKind,
}

/// Check or uncheck every visible, selectable cell (footer "Alle"/"Keine", ⌘A).
pub(super) fn check_all(imp: &mut Import, on: bool) {
    for ci in imp.visible_order() {
        if imp.selectable(ci) {
            imp.cells[ci].on = on;
        }
    }
}

/// Weekday of a date `YYYY-MM-DD` (0 = Monday).
fn weekday(tag: &str) -> Option<usize> {
    let y: i32 = tag.get(..4)?.parse().ok()?;
    let m: i32 = tag.get(5..7)?.parse().ok()?;
    let d: i32 = tag.get(8..10)?.parse().ok()?;
    // Sakamoto (0 = Sunday)
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    let w = (y + y / 4 - y / 100 + y / 400 + T[(m - 1).clamp(0, 11) as usize] + d).rem_euclid(7);
    Some(((w + 6) % 7) as usize)
}

/// "Sa 02.05.2026" from `2026-05-02`.
pub(super) fn day_label(tag: &str) -> String {
    const NAMES: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];
    let date = match (tag.get(..4), tag.get(5..7), tag.get(8..10)) {
        (Some(y), Some(m), Some(d)) => match (y.parse(), m.parse(), d.parse()) {
            (Ok(y), Ok(m), Ok(d)) => crate::menu::i18n::format::date(crate::menu::i18n::language(), y, m, d).unwrap_or_else(|| tag.to_string()),
            _ => tag.to_string(),
        },
        _ => tag.to_string(),
    };
    match weekday(tag) {
        Some(w) => format!("{} {date}", crate::tr(NAMES[w])),
        None => date,
    }
}

/// Short kind of a cell: "CR3+JPG", "X3F", "JPG".
fn cell_kind(imp: &Import, c: &Cell) -> String {
    let parts: Vec<&str> = c.members.iter().filter(|&&i| imp.member_on(c, i)).map(|&i| imp.cand[i].format.as_str()).collect();
    parts.join("+")
}

/// The file a cell shows: the JPEG half of RAW+JPEG (its embedded image is the same photo, read much faster than a
/// RAW's preview), otherwise the first file that is part of the import.
pub(super) fn picture_of(imp: &Import, c: &Cell) -> usize {
    c.members.iter().copied().find(|&i| library::import_select::is_jpeg_like(&imp.cand[i].format)).or_else(|| c.members.iter().copied().find(|&i| imp.member_on(c, i))).unwrap_or(c.members[0])
}

pub(super) fn cell_name(imp: &Import, c: &Cell) -> String {
    imp.cand[c.members[0]].path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The grid (with its bar: date range, cell size). "RAW+JPG: nur RAW" is in the left column with the file types.
pub(super) fn show(ui: &mut egui::Ui, imp: &mut Import, height: f32) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        // the days that have a photo shown (a day of photos already there is hidden with them)
        let shown: Vec<&Day> = imp.days.iter().filter(|d| d.cells.iter().any(|&ci| imp.cell_visible(ci))).collect();
        let range = match (shown.first(), shown.last()) {
            (Some(a), Some(b)) if a.tag != b.tag => format!("{} – {}", day_label(&a.tag), day_label(&b.tag)),
            (Some(a), _) => day_label(&a.tag),
            _ => String::new(),
        };
        ui.label(egui::RichText::new(range.to_uppercase()).font(lr::semibold(11.0)).color(Color32::from_gray(0x9a)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut w = imp.cell_w;
            let r = ui.add_sized([90.0, 18.0], egui::Slider::new(&mut w, 96.0..=220.0).show_value(false));
            lr::hover_tip(ui, &r, "Größe der Miniaturen");
            imp.cell_w = w;
        });
    });
    let width = ui.available_width();
    let height = height - BAR_H - 6.0;
    if imp.cand.is_empty() {
        egui::Frame::new().fill(lr::PANEL_DARK).show(ui, |ui| {
            ui.set_min_size(egui::vec2(width, height));
            ui.centered_and_justified(|ui| {
                let camera = camera::chosen_state(imp).filter(|_| imp.cand.is_empty());
                let t: String = if let Some(e) = &imp.source_error {
                    crate::menu::i18n::tr_msg(e).into_owned()
                } else if let Some(st) = camera.as_ref().filter(|s| !matches!(s, library::import_source::CamState::Ready(_))) {
                    camera::state_hint(st)
                } else if camera == Some(library::import_source::CamState::Ready(0)) && !imp.scanning {
                    camera::state_hint(&library::import_source::CamState::Ready(0))
                } else if imp.scanning {
                    crate::tr("liest die Fotos …").to_string()
                } else if imp.source.is_some() {
                    crate::tr("In dieser Quelle sind keine Fotos gefunden worden.").to_string()
                } else {
                    if camera::enabled() { crate::tr("Karte einlegen, Kamera anschließen oder einen Ordner wählen.") } else { crate::tr("Karte einlegen oder einen Ordner wählen.") }.to_string()
                };
                ui.label(egui::RichText::new(t).color(lr::TEXT_DIM()));
            });
        });
        return;
    }
    let cell_w = imp.cell_w;
    let cell_h = (cell_w * 0.9).round();
    let columns = ((width - 12.0) / cell_w).floor().max(1.0) as usize;
    // visible cells per day and the rows (heads and cell rows) with their positions
    let per_day: Vec<Vec<usize>> = imp.days.iter().map(|d| d.cells.iter().copied().filter(|&ci| imp.cell_visible(ci)).collect()).collect();
    let mut rows: Vec<Row> = Vec::new();
    let mut y = 0.0;
    for (d, cells) in per_day.iter().enumerate() {
        if cells.is_empty() {
            continue;
        }
        rows.push(Row { y, h: HEAD_H, kind: RowKind::Head(d) });
        y += HEAD_H;
        for from in (0..cells.len()).step_by(columns) {
            rows.push(Row { y, h: cell_h, kind: RowKind::Cells(d, from, (from + columns).min(cells.len())) });
            y += cell_h;
        }
    }
    let total = y + 4.0;
    // order of the visible cells for shift-click ranges
    let order: Vec<usize> = per_day.iter().flatten().copied().collect();
    let pos_of: HashMap<usize, usize> = order.iter().enumerate().map(|(p, &ci)| (ci, p)).collect();
    let mut click: Option<(usize, bool)> = None;
    let mut open_big: Option<usize> = None;
    let mut day_toggle: Option<usize> = None;
    let mut hovered = None;
    let mut wanted: Vec<(PathBuf, String)> = Vec::new();
    egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(4, 4)).show(ui, |ui| {
        ui.set_min_size(egui::vec2(width - 8.0, height - 8.0));
        egui::ScrollArea::vertical().id_salt("import-raster").auto_shrink([false, false]).max_height(height - 8.0).show_viewport(ui, |ui, viewport| {
            ui.set_height(total);
            let origin = ui.min_rect().min;
            for row in rows.iter().filter(|r| r.y + r.h >= viewport.min.y && r.y <= viewport.max.y) {
                match row.kind {
                    RowKind::Head(d) => {
                        let rect = egui::Rect::from_min_size(origin + egui::vec2(0.0, row.y), egui::vec2(width - 12.0, HEAD_H));
                        if day_head(ui, imp, d, &per_day[d], rect) {
                            day_toggle = Some(d);
                        }
                    }
                    RowKind::Cells(d, from, to) => {
                        for (n, &ci) in per_day[d][from..to].iter().enumerate() {
                            let rect = egui::Rect::from_min_size(origin + egui::vec2(n as f32 * cell_w, row.y), egui::vec2(cell_w, cell_h));
                            let resp = ui.interact(rect, ui.scope_id().with(("imp-zelle", ci)), egui::Sense::click());
                            wanted.push(paint_cell(ui, imp, ci, rect, resp.hovered()));
                            if resp.hovered() {
                                hovered = Some(ci);
                            }
                            if resp.double_clicked() {
                                open_big = Some(ci);
                            } else if resp.clicked() && imp.selectable(ci) {
                                click = Some((pos_of[&ci], ui.input(|i| i.modifiers.shift)));
                            }
                        }
                    }
                }
            }
        });
    });
    imp.hovered = hovered;
    if crate::import_bench::active() {
        crate::import_bench::window_grid(imp.cand.len(), wanted.len(), wanted.iter().filter(|(p, _)| imp.thumbs.tex.contains_key(p)).count());
    }
    // the helpers take the newest request first: requested from the bottom up, the top left cell comes first
    for (p, f) in wanted.iter().rev() {
        imp.thumbs.request(p, f);
    }
    if let Some(d) = day_toggle {
        let cells = &per_day[d];
        let all = cells.iter().filter(|&&ci| imp.selectable(ci)).all(|&ci| imp.cells[ci].on);
        for &ci in cells {
            if imp.selectable(ci) {
                imp.cells[ci].on = !all;
            }
        }
    }
    if let Some((pos, shift)) = click {
        let ci = order[pos];
        let new = !imp.cells[ci].on;
        match (shift, imp.last_click.filter(|&l| l < order.len())) {
            (true, Some(l)) => {
                for &cj in &order[l.min(pos)..=l.max(pos)] {
                    if imp.selectable(cj) {
                        imp.cells[cj].on = new;
                    }
                }
            }
            _ => imp.cells[ci].on = new,
        }
        imp.last_click = Some(pos);
    }
    // keys: space = large view of the photo under the pointer, P/X = check/uncheck it, ⌘A = all
    let ctx = ui.ctx().clone();
    if let Some(ci) = imp.hovered {
        if crate::shortcuts::pressed(&ctx, "single_photo_space") {
            open_big = Some(ci);
        }
        if imp.selectable(ci) {
            if crate::shortcuts::pressed(&ctx, "pick") {
                imp.cells[ci].on = true;
            }
            if crate::shortcuts::pressed(&ctx, "reject") {
                imp.cells[ci].on = false;
            }
        }
    }
    if crate::shortcuts::pressed(&ctx, "select_all") {
        check_all(imp, true);
    }
    if let Some(ci) = open_big {
        imp.big = Some(ci);
    }
}

/// Head row of a day: own check (all / none / partly), weekday and date, number and size. Returns: check clicked.
fn day_head(ui: &mut egui::Ui, imp: &Import, d: usize, cells: &[usize], rect: egui::Rect) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(("imp-tag", d)), egui::Sense::click());
    let p = ui.painter();
    let selectable: Vec<usize> = cells.iter().copied().filter(|&ci| imp.selectable(ci)).collect();
    let on = selectable.iter().filter(|&&ci| imp.cells[ci].on).count();
    let check = egui::Rect::from_min_size(rect.left_center() + egui::vec2(4.0, -7.5), egui::vec2(15.0, 15.0));
    p.rect_stroke(check, 0.0, egui::Stroke::new(1.0, Color32::from_gray(if resp.hovered() { 0xe0 } else { 0xb0 })), egui::StrokeKind::Inside);
    if !selectable.is_empty() && on == selectable.len() {
        p.rect_filled(check.shrink(3.5), 0.0, Color32::from_gray(0xe8));
    } else if on > 0 {
        p.rect_filled(egui::Rect::from_center_size(check.center(), egui::vec2(7.0, 2.0)), 0.0, Color32::from_gray(0xe8));
    }
    let bytes: u64 = cells.iter().flat_map(|&ci| imp.filter.files(&imp.cand, &imp.cells[ci].members)).map(|i| imp.cand[i].size).sum();
    let t = format!("{} · {} · {}", day_label(&imp.days[d].tag).to_uppercase(), dialog::count(cells.len(), crate::tr("Foto"), crate::tr("Fotos")), dialog::size(bytes));
    p.text(check.right_center() + egui::vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, t, lr::semibold(11.5), lr::TEXT());
    p.hline(rect.x_range(), rect.bottom() - 1.0, egui::Stroke::new(1.0, Color32::from_gray(0x26)));
    lr::hover_tip(ui, &resp, "Alle Fotos dieses Tages an- oder abwählen");
    resp.clicked() && !selectable.is_empty()
}

/// One cell: thumbnail, check, kind ("CR3+JPG"), name, marks (in the library, probably there, identical).
fn paint_cell(ui: &mut egui::Ui, imp: &mut Import, ci: usize, rect: egui::Rect, hovered: bool) -> (PathBuf, String) {
    let c = &imp.cells[ci];
    let first = picture_of(imp, c);
    let (path, format) = (imp.cand[first].path.clone(), imp.cand[first].format.clone());
    let duplicate = imp.cell_duplicate(c);
    let probable = imp.cell_probable(c);
    let identical = imp.cell_identical(c);
    let selectable = imp.selectable(ci);
    // a photo already there whose JPEG half is missing can still be checked (only that half is imported)
    let partly = duplicate && selectable;
    let c = &imp.cells[ci];
    let on = c.on && selectable;
    let kind = cell_kind(imp, c);
    let name = cell_name(imp, c);
    let p = ui.painter();
    let inner = rect.shrink(3.0);
    p.rect_filled(inner, 0.0, if hovered { Color32::from_gray(0x24) } else { Color32::from_gray(0x1a) });
    let image = egui::Rect::from_min_size(inner.min + egui::vec2(4.0, 4.0), egui::vec2(inner.width() - 8.0, inner.height() - 26.0));
    match imp.thumbs.tex.get(&path) {
        Some(t) => {
            let sz = t.size_vec2();
            let f = (image.width() / sz.x).min(image.height() / sz.y);
            let tint = if duplicate || !on { Color32::from_gray(120) } else { Color32::WHITE };
            p.image(t.id(), egui::Rect::from_center_size(image.center(), sz * f), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
        }
        None => {
            p.text(image.center(), egui::Align2::CENTER_CENTER, &format, egui::FontId::proportional(10.5), Color32::from_gray(0x82));
        }
    }
    // check (light, not gold: gold is the main button only)
    if selectable {
        let check = egui::Rect::from_min_size(inner.min + egui::vec2(6.0, 6.0), egui::vec2(15.0, 15.0));
        p.rect_filled(check, 0.0, Color32::from_black_alpha(150));
        p.rect_stroke(check, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0xb0)), egui::StrokeKind::Inside);
        if on {
            p.rect_filled(check.shrink(3.5), 0.0, Color32::from_gray(0xe8));
        }
    }
    // marks top right
    let mark = if partly {
        Some((crate::tr("RAW in der Bibliothek"), Color32::from_rgb(0x8f, 0xc0, 0x8a)))
    } else if duplicate {
        Some((crate::tr("in der Bibliothek"), Color32::from_rgb(0x8f, 0xc0, 0x8a)))
    } else if probable {
        Some((crate::tr("vermutlich da"), Color32::from_rgb(0xe0, 0xb0, 0x40)))
    } else if identical {
        Some((crate::tr("≡ identisch"), Color32::from_rgb(0xa0, 0xb4, 0xd0)))
    } else {
        None
    };
    if let Some((t, col)) = mark {
        let g = p.layout_no_wrap(t.to_string(), egui::FontId::proportional(10.0), col);
        let r = egui::Rect::from_min_size(inner.right_top() + egui::vec2(-g.size().x - 8.0, 5.0), g.size() + egui::vec2(4.0, 2.0));
        p.rect_filled(r, 0.0, Color32::from_black_alpha(170));
        p.galley(r.min + egui::vec2(2.0, 1.0), g, col);
    }
    // name and kind
    let color = if duplicate { lr::TEXT_DIM() } else { lr::TEXT() };
    let clip = p.with_clip_rect(egui::Rect::from_min_max(egui::pos2(inner.left() + 4.0, inner.bottom() - 20.0), egui::pos2(inner.right() - 4.0, inner.bottom())));
    let k = clip.layout_no_wrap(kind, egui::FontId::proportional(10.0), lr::TEXT_DIM());
    let kw = k.size().x;
    clip.galley(egui::pos2(inner.right() - 5.0 - kw, inner.bottom() - 17.0), k, lr::TEXT_DIM());
    let n = clip.layout_no_wrap(name, egui::FontId::proportional(10.5), color);
    let clip_name = p.with_clip_rect(egui::Rect::from_min_max(egui::pos2(inner.left() + 4.0, inner.bottom() - 20.0), egui::pos2(inner.right() - 10.0 - kw, inner.bottom())));
    clip_name.galley(egui::pos2(inner.left() + 5.0, inner.bottom() - 18.0), n, color);
    (path, format)
}

/// Large view of one photo (replaces the grid; the side columns stay): the embedded preview at about 1 600 px,
/// paging with ← →, check with P/X or the box in the head, space bar back to the grid.
pub(super) fn big_view(ui: &mut egui::Ui, imp: &mut Import) {
    let Some(ci) = imp.big else { return };
    let order = imp.visible_order();
    let pos = order.iter().position(|&c| c == ci).unwrap_or(0);
    let ctx = ui.ctx().clone();
    let mut go: Option<usize> = None;
    let mut back = false;
    // head: back, previous, name and kind, capture time, check, next
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BAR_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let r = ui.button("‹");
        lr::hover_tip(ui, &r, &lr::with_shortcut("Voriges Foto", "arrows"));
        if r.clicked() && pos > 0 {
            go = Some(pos - 1);
        }
        let c = &imp.cells[ci];
        let k = &imp.cand[c.members[0]];
        let when = k.capture.as_deref().map(|a| format!(" · {} {}", day_label(&k.tag), a.get(11..19).unwrap_or(""))).unwrap_or_default();
        let size: u64 = c.members.iter().filter(|&&i| imp.member_on(c, i)).map(|&i| imp.cand[i].size).sum();
        ui.label(egui::RichText::new(format!("{} · {}{} · {}", cell_name(imp, c), cell_kind(imp, c), when, dialog::size(size))).color(lr::TEXT()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.button("›");
            lr::hover_tip(ui, &r, &lr::with_shortcut("Nächstes Foto", "arrows"));
            if r.clicked() && pos + 1 < order.len() {
                go = Some(pos + 1);
            }
            let r = ui.button(crate::tr("Raster"));
            lr::hover_tip(ui, &r, &lr::with_shortcut("Zurück zum Raster", "single_photo_space"));
            if r.clicked() {
                back = true;
            }
            if imp.selectable(ci) {
                let mut on = imp.cells[ci].on;
                let r = ui.checkbox(&mut on, crate::tr("importieren"));
                lr::hover_tip(ui, &r, &format!("{} · {}", lr::with_shortcut("Anwählen", "pick"), lr::with_shortcut("Abwählen", "reject")));
                imp.cells[ci].on = on;
            }
        });
    });
    // the photo, fitted; the neighbours are loaded ahead
    let c = &imp.cells[ci];
    let first = picture_of(imp, c);
    let mut wanted = vec![(imp.cand[first].path.clone(), imp.cand[first].format.clone())];
    for n in [pos + 1, pos.wrapping_sub(1)] {
        if let Some(&cj) = order.get(n) {
            let cell = &imp.cells[cj];
            let f = picture_of(imp, cell);
            wanted.push((imp.cand[f].path.clone(), imp.cand[f].format.clone()));
        }
    }
    imp.thumbs.request_big(&wanted);
    let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, lr::IMAGE_BG);
    let path = &wanted[0].0;
    let tex = imp.thumbs.big.get(path).or_else(|| imp.thumbs.tex.get(path));
    match tex {
        Some(t) => {
            let sz = t.size_vec2();
            let f = ((rect.width() - 16.0) / sz.x).min((rect.height() - 16.0) / sz.y);
            p.image(t.id(), egui::Rect::from_center_size(rect.center(), sz * f), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            p.text(rect.center(), egui::Align2::CENTER_CENTER, crate::tr("lädt …"), egui::FontId::proportional(12.0), lr::TEXT_DIM());
        }
    }
    if !imp.thumbs.big.contains_key(path) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    // keys
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) && pos + 1 < order.len() {
        go = Some(pos + 1);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) && pos > 0 {
        go = Some(pos - 1);
    }
    if crate::shortcuts::pressed(&ctx, "single_photo_space") {
        back = true;
    }
    if imp.selectable(ci) {
        if crate::shortcuts::pressed(&ctx, "pick") {
            imp.cells[ci].on = true;
        }
        if crate::shortcuts::pressed(&ctx, "reject") {
            imp.cells[ci].on = false;
        }
    }
    if back {
        imp.big = None;
    } else if let Some(n) = go {
        imp.big = order.get(n).copied();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_of_dates() {
        assert_eq!(weekday("2026-05-02"), Some(5)); // Saturday
        assert_eq!(weekday("2026-11-12"), Some(3)); // Thursday
        assert_eq!(weekday("2024-02-29"), Some(3));
        assert_eq!(weekday("x"), None);
    }
}
