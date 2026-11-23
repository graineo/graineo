//! Darkroom: contact sheet as a grid. Like a real darkroom sheet with many small
//! prints: rows are the papers, columns the contrast grades; each cell shows your photo with exactly
//! that combination. A click applies paper and grade together and leaves the sheet open so you can
//! keep comparing. Slide film and direct scan have no paper: then it is a row across
//! the film's speeds (EI) or development times.
//!
//! Rendering runs in the background (`Job::Contact`), row by row, only once the settings have been
//! idle for a moment; finished rows appear immediately, a progress indicator shows while rendering.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use develop::Settings;
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Vec2};

use crate::darkroom_strip::grade_name;
use crate::jobs::Job;
use crate::{help, lr, App};

/// Idle time of the settings before the sheet is re-rendered (seconds).
const QUIET_PERIOD: f64 = 0.35;

#[derive(Default)]
pub struct Contact {
    /// Sheet open?
    pub an: bool,
    /// Library ID; also protects against switching to another list with the same index.
    image: Option<i64>,
    /// requested key and since when (UI time)
    requested: Option<(u64, f64)>,
    /// key currently being rendered
    computing: Option<u64>,
    /// key of the finished sheet
    done: Option<u64>,
    /// Shape (rows and columns) of the shown cells; if it changes, the old images are discarded
    form: u64,
    tex: HashMap<(usize, usize), TextureHandle>,
    /// Cell magnification (1 = everything fits the window, more = the sheet scrolls)
    pub zoom: f32,
    /// Contact print (film strip) instead of grid, see `darkroom_print.rs`
    pub print: crate::darkroom_print::Print,
    /// last used of the two sheet views (K opens it again): true = contact print
    pub last_print: bool,
}

/// One cell of the sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    /// Contrast grade or development time/EI; −1 = paper without grades (default)
    pub value: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Label (short paper name or "EI"/"time")
    pub name: String,
    /// Paper ID; `None` for slide film
    pub paper: Option<String>,
    pub fields: Vec<Field>,
}

/// Layout of the sheet for the current settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// Slide film/direct scan: row across speeds instead of papers
    pub slide: bool,
    pub rows: Vec<Row>,
    pub columns: usize,
    /// currently selected cell (row, column)
    pub current: Option<(usize, usize)>,
    /// Default paper of the film (stored in the settings as "none selected")
    pub standard: Option<String>,
}

/// Row name of a paper: without manufacturer and without bracketed additions (the row wraps itself).
fn row_name(name: &str) -> String {
    let n = name.split(['(', ',']).next().unwrap_or(name).trim();
    n.trim_start_matches("Ilford ").trim_start_matches("Kodak Professional ").trim_start_matches("Kodak ").trim_start_matches("Fujifilm ").to_string()
}

fn nearest(values: &[f32], actual: f32) -> Option<usize> {
    values.iter().enumerate().min_by(|a, b| (a.1 - actual).abs().total_cmp(&(b.1 - actual).abs())).map(|(i, _)| i)
}

/// Sheet for the settings; `Err` with a short note when there is nothing to show.
pub fn model(s: &Settings) -> Result<Model, &'static str> {
    if !s.film.is_active() {
        return Err(crate::tr("Film wählen"));
    }
    let film = develop::film::stock(&s.film.stock).ok_or(crate::tr("Film wählen"))?;
    if film.positive || s.film.negative {
        if film.family.len() < 2 {
            return Err(crate::tr("Für diesen Film gibt es keine Varianten"));
        }
        let fields: Vec<Field> = film.family.iter().map(|&v| Field { name: format!("{v}"), value: v }).collect();
        let actual = develop::film::development_info(&s.film).and_then(|e| e.ei.or(e.minutes.map(|m| m as f32))).unwrap_or(0.0);
        let current = nearest(&film.family, actual).map(|i| (0, i));
        let name = if film.positive { "EI" } else { crate::tr("Minuten") };
        return Ok(Model { slide: true, columns: fields.len(), rows: vec![Row { name: name.into(), paper: None, fields }], current, standard: None });
    }
    let standard = film.default_paper.clone().ok_or(crate::tr("Kein Papier"))?;
    let effective_paper = s.film.paper.clone().unwrap_or_else(|| standard.clone());
    let mut rows = Vec::new();
    let mut current = None;
    for p in develop::film::papers().iter().filter(|p| p.monochrome == film.monochrome) {
        let multi = p.id.starts_with("ilford_multigrade");
        let fields: Vec<Field> = if p.family.len() > 1 {
            p.family.iter().map(|&g| Field { name: grade_name(multi, g), value: g }).collect()
        } else {
            vec![Field { name: "Std".into(), value: -1.0 }]
        };
        if p.id == effective_paper {
            let col = if fields.len() > 1 {
                let actual = if s.film.paper_grade >= 0.0 {
                    s.film.paper_grade
                } else {
                    develop::film::default_paper_grade(&film.id, &p.id).unwrap_or(p.family[(p.family.len() - 1) / 2])
                };
                nearest(&p.family, actual).unwrap_or(0)
            } else {
                0
            };
            current = Some((rows.len(), col));
        }
        rows.push(Row { name: row_name(&p.name), paper: Some(p.id.clone()), fields });
    }
    if rows.is_empty() {
        return Err(crate::tr("Kein Papier"));
    }
    let columns = rows.iter().map(|z| z.fields.len()).max().unwrap_or(1);
    Ok(Model { slide: false, rows, columns, current, standard: Some(standard) })
}

impl Model {
    /// Settings of a cell: everything as current, only paper and grade (or EI) change.
    pub fn setting(&self, s: &Settings, row: usize, column: usize) -> Option<Settings> {
        let z = self.rows.get(row)?;
        let f = z.fields.get(column)?;
        let mut t = s.clone();
        if self.slide {
            t.film.film_dev = f.value;
        } else {
            let id = z.paper.clone()?;
            t.film.paper = (self.standard.as_deref() != Some(id.as_str())).then_some(id);
            t.film.paper_grade = f.value;
        }
        Some(t)
    }

    /// Shape of the sheet (for the image cache).
    fn form(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.slide.hash(&mut h);
        for z in &self.rows {
            z.name.hash(&mut h);
            z.fields.len().hash(&mut h);
        }
        h.finish()
    }
}

/// Image key: everything except paper, grade and EI, which change from cell to cell.
fn cache_key(image: i64, s: &Settings, m: &Model) -> u64 {
    let mut n = s.clone();
    n.film.paper = None;
    n.film.paper_grade = -1.0;
    if m.slide {
        n.film.film_dev = 0.0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    image.hash(&mut h);
    m.form().hash(&mut h);
    serde_json::to_string(&n).unwrap_or_default().hash(&mut h);
    h.finish()
}

impl Contact {
    /// A contact sheet is being computed (status in the title bar).
    pub fn computing(&self) -> bool {
        self.computing.is_some() || self.print.computing()
    }

    /// Number of finished cells (tests).
    #[cfg(test)]
    pub fn fields(&self) -> usize {
        self.tex.len()
    }

    /// Take over a finished row (only if the key is still needed).
    pub fn outcome(&mut self, ctx: &egui::Context, key: u64, row: usize, fields: Vec<(usize, usize, usize, Vec<u8>)>, done: bool) {
        if done { self.task_finished(key); }
        if self.requested.map(|w| w.0) != Some(key) {
            return;
        }
        for (column, w, h, rgb) in fields {
            let img = egui::ColorImage::from_rgb([w, h], &rgb);
            self.tex.insert((row, column), ctx.load_texture(format!("dk-kontakt-{row}-{column}"), img, egui::TextureOptions::LINEAR));
        }
        if done {
            self.done = Some(key);
        }
    }

    /// A photo that has since been left must also release the single running job.
    pub fn task_finished(&mut self, key: u64) {
        if self.computing == Some(key) { self.computing = None; }
    }

    fn image_change(&mut self, image: i64) {
        if self.image == Some(image) { return; }
        self.image = Some(image);
        self.requested = None;
        self.done = None;
        self.tex.clear();
        // The old job may finish; do not start a second one until then.
    }

}

impl App {
    /// Open the sheet in a view (`print`: contact print, otherwise paper comparison), or close it (`None`).
    /// The contact print shows the thumbnail strip while it is open and restores the previous state afterwards.
    pub(crate) fn contact_view(&mut self, view: Option<bool>) {
        let k = &mut self.darkroom.contact;
        match view {
            Some(print) => {
                k.an = true;
                k.print.an = print;
                k.last_print = print;
            }
            None => {
                k.an = false;
            }
        }
    }

    /// K: open or close the contact print.
    pub(crate) fn contact_view_toggle(&mut self) {
        let view = (!self.darkroom.contact.an || !self.darkroom.contact.print.an).then_some(true);
        self.contact_view(view);
    }

    /// Command "Create contact print …": from anywhere into the darkroom with the sheet open.
    pub(crate) fn open_contact_print(&mut self) {
        if self.in_grid() {
            self.to_single_image();
        }
        if !self.darkroom_active() {
            self.enter_darkroom();
        }
        self.contact_view(Some(true));
    }

    /// Does the sheet or the recipe book cover the image area (then the photo info box stays away)?
    pub(crate) fn darkroom_covers_image(&self) -> bool {
        self.darkroom_active() && (self.darkroom.contact.an || self.darkroom.recipes.opened)
    }

    /// Every frame: request rows when the settings have been idle briefly and something changed.
    pub(crate) fn darkroom_contact_update(&mut self, ctx: &egui::Context) {
        if !self.darkroom_active() || !self.darkroom.contact.an {
            return;
        }
        if self.darkroom.contact.print.an {
            self.darkroom_print_update(ctx);
            return;
        }
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let image = self.entries[index].image;
        self.darkroom.contact.image_change(image);
        let s = self.entries[index].settings.clone();
        let Ok(m) = model(&s) else { return };
        let now = ctx.input(|i| i.time);
        let key = cache_key(image, &s, &m);
        // no row from a fast preview too rough to be shown: asked again once the exact base is there
        let waits = self.derived_waits(&s);
        let st = &mut self.darkroom.contact;
        if st.form != m.form() {
            st.form = m.form();
            st.tex.clear();
            st.done = None;
        }
        if st.done == Some(key) || st.computing == Some(key) {
            st.requested = Some((key, st.requested.map_or(now, |w| if w.0 == key { w.1 } else { now })));
            return;
        }
        match st.requested {
            Some((k, since)) if k == key => {
                if now - since >= QUIET_PERIOD && st.computing.is_none() && !waits {
                    st.computing = Some(key);
                    let Some(c) = self.current.as_ref() else { return };
                    let rows: Vec<(usize, Vec<(usize, Settings)>)> = (0..m.rows.len())
                        .map(|z| (z, (0..m.rows[z].fields.len()).filter_map(|sp| m.setting(&s, z, sp).map(|t| (sp, t))).collect()))
                        .collect();
                    self.worker.send(Job::Contact { index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), rows, key, target: crate::jobs::ContactTarget::Paper });
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(120));
                }
            }
            _ => {
                st.requested = Some((key, now));
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
    }

    /// Click on a cell: apply paper and grade (or EI), the sheet stays open.
    pub(crate) fn darkroom_contact_choose(&mut self, index: usize, row: usize, column: usize) {
        let Some(e) = self.entries.get(index) else { return };
        let Ok(m) = model(&e.settings) else { return };
        if let Some(t) = m.setting(&e.settings, row, column) {
            self.entries[index].settings.film = t.film;
        }
    }

    /// The sheet over the image area `rect`.
    pub(crate) fn darkroom_contact_draw(&mut self, ctx: &egui::Context, rect: Rect) {
        if !self.darkroom_active() || !self.darkroom.contact.an {
            return;
        }
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let s = self.entries[index].settings.clone();
        let model = model(&s);
        let mut pick: Option<(usize, usize)> = None;
        let mut close = false;
        egui::Area::new(egui::Id::NULL.with("dk-kontakt")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
            ui.set_clip_rect(rect);
            // swallows clicks and drags so the image underneath stays still
            ui.allocate_rect(rect, egui::Sense::click_and_drag());
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 0.0, lr::IMAGE_BG);
            let margin = 14.0;
            // Header: title, progress, help, close
            let header = Rect::from_min_size(rect.min + Vec2::new(margin, 8.0), Vec2::new(rect.width() - 2.0 * margin, 22.0));
            let total = model.as_ref().map_or(0, |m| m.rows.iter().map(|z| z.fields.len()).sum::<usize>());
            let done = self.darkroom.contact.tex.len().min(total);
            let computing = model.is_ok() && (self.darkroom.contact.computing.is_some() || done < total || self.darkroom.contact.done.is_none());
            let print_on = self.darkroom.contact.print.an;
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(header).layout(egui::Layout::left_to_right(egui::Align::Center)));
            child.label(egui::RichText::new(if print_on { crate::tr("KONTAKTABZUG") } else { crate::tr("PAPIERVERGLEICH") }).color(Color32::from_gray(0xe2)).strong());
            help::info(&mut child, "dk-kontakt", if print_on { crate::tr("Kontaktabzug") } else { crate::tr("Papiervergleich") }, &help::DR_CONTACT);
            if print_on {
                // fixed place for the status; text changes, nothing moves
                let a = &self.darkroom.contact.print;
                let text = if a.computing() {
                    crate::trf!("RECHNET {a}/{b}", a = a.done(), b = a.total())
                } else if a.stale() {
                    crate::tr("RECHNET …").to_string()
                } else if a.missing() > 0 {
                    crate::trf!("{n} Fotos fehlen", n = a.missing())
                } else {
                    String::new()
                };
                child.label(egui::RichText::new(text).color(lr::TEXT_DIM));
            } else if model.is_ok() && computing {
                child.label(egui::RichText::new(crate::trf!("RECHNET {done}/{total}", done = done, total = total)).color(lr::TEXT_DIM));
            }
            let mut export = false;
            let mut print = false;
            let mut zoom = self.darkroom.contact.zoom.clamp(1.0, 4.0);
            let zoom_controls = |ui: &mut egui::Ui, zoom: &mut f32| {
                if ui.add_enabled(*zoom > 1.001, egui::Button::new(crate::tr("EINPASSEN")).frame(false)).clicked() {
                    *zoom = 1.0;
                }
                let r = ui.add(egui::Slider::new(zoom, 1.0..=4.0).show_value(false).step_by(0.05));
                lr::hover_tip(ui, &r, crate::tr("Felder vergrößern (⌘ + Mausrad oder Zusammenziehen)"));
                ui.label(egui::RichText::new(crate::tr("GRÖSSE")).color(lr::TEXT_DIM));
            };
            let export_ready = self.darkroom.contact.print.complete();
            child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(crate::tr("SCHLIESSEN")).frame(false)).clicked() {
                    close = true;
                }
                ui.add_space(12.0);
                if print_on {
                    if ui.add_enabled(export_ready, egui::Button::new(crate::tr("EXPORTIEREN …")).frame(false)).clicked() {
                        export = true;
                    }
                    ui.add_space(8.0);
                    let r = ui.add(egui::Button::new(crate::tr("DRUCKEN …")).frame(false));
                    lr::hover_tip(ui, &r, "Im Druckraum in voller Auflösung drucken (Filmstreifen-Vorlage)");
                    if r.clicked() {
                        print = true;
                    }
                } else {
                    zoom_controls(ui, &mut zoom);
                }
            });
            if print_on {
                // second line: what is on the sheet (left, shortened when narrow), size controls (right)
                let line2 = Rect::from_min_size(header.min + Vec2::new(0.0, 22.0), Vec2::new(header.width(), 22.0));
                let mut row = ui.new_child(egui::UiBuilder::new().max_rect(line2).layout(egui::Layout::right_to_left(egui::Align::Center)));
                zoom_controls(&mut row, &mut zoom);
                let info = self.darkroom_print_info(index);
                let left = Rect::from_min_max(line2.min, Pos2::new(row.min_rect().left() - 10.0, line2.max.y));
                if left.width() > 20.0 {
                    let mut l = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::left_to_right(egui::Align::Center)));
                    l.add(egui::Label::new(egui::RichText::new(info).color(lr::TEXT_DIM)).truncate());
                }
            }
            // Pinch or ⌘ + mouse wheel zooms when the mouse is over the sheet
            if ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| rect.contains(p)) {
                let factor = ctx.input(|i| i.zoom_delta());
                if factor != 1.0 {
                    zoom = (zoom * factor).clamp(1.0, 4.0);
                }
            }
            self.darkroom.contact.zoom = zoom;
            if export {
                self.darkroom_print_export(ctx);
            }
            if print {
                let film = self.entries[index].settings.film.clone();
                self.open_print_room(Some(film), Some(print::template::FILM_STRIP_TEMPLATE));
                return;
            }
            if print_on {
                let viewport = Rect::from_min_max(Pos2::new(rect.left() + margin, rect.top() + 58.0), Pos2::new(rect.right() - margin, rect.bottom() - margin));
                self.darkroom_print_draw(ui, viewport, zoom);
                return;
            }
            let m = match &model {
                Ok(m) => m,
                Err(hint) => {
                    p.text(rect.center(), egui::Align2::CENTER_CENTER, *hint, egui::FontId::proportional(13.0), lr::TEXT_DIM);
                    return;
                }
            };
            // Grid: at zoom 1 everything fits the window, above that the sheet scrolls (horizontally and vertically)
            let label_b = 112.0;
            let header_h = if m.columns > 1 { 18.0 } else { 0.0 };
            let gap = 8.0;
            let viewport = Rect::from_min_max(Pos2::new(rect.left() + margin, rect.top() + 36.0), Pos2::new(rect.right() - margin, rect.bottom() - margin));
            let (nz, ns) = (m.rows.len() as f32, m.columns as f32);
            let cw1 = ((viewport.width() - label_b - gap * (ns - 1.0)) / ns).max(20.0);
            let ch1 = ((viewport.height() - header_h - gap * (nz - 1.0)) / nz).clamp(30.0, 340.0);
            // Aspect ratio of the photo from a finished cell
            let ar = self.darkroom.contact.tex.values().next().map_or(1.5, |t| t.size_vec2().x / t.size_vec2().y.max(1.0));
            let (iw1, ih1) = if cw1 / ch1 > ar { (ch1 * ar, ch1) } else { (cw1, cw1 / ar) };
            let (zw, zh, bw, bh) = (cw1 * zoom, ch1 * zoom, iw1 * zoom, ih1 * zoom);
            let content = Vec2::new(label_b + ns * zw + gap * (ns - 1.0), header_h + nz * zh + gap * (nz - 1.0));
            let mut sub = ui.new_child(egui::UiBuilder::new().max_rect(viewport));
            egui::ScrollArea::both().id_salt("dk-kontakt-scroll").auto_shrink([false, false]).show(&mut sub, |ui| {
                let (surface, _) = ui.allocate_exact_size(content.max(viewport.size()), egui::Sense::hover());
                let p = ui.painter().clone();
                let grid = Rect::from_min_size(surface.min + Vec2::new(label_b, header_h), Vec2::new(content.x - label_b, content.y - header_h));
                let top = grid.top();
                // Column headers from the widest row
                let wide = m.rows.iter().max_by_key(|z| z.fields.len()).map(|z| z.fields.clone()).unwrap_or_default();
                if header_h > 0.0 {
                    for (sp, f) in wide.iter().enumerate() {
                        let x = grid.left() + sp as f32 * (zw + gap) + zw / 2.0;
                        p.text(Pos2::new(x, top - header_h / 2.0), egui::Align2::CENTER_CENTER, &f.name, egui::FontId::monospace(11.0), lr::TEXT_DIM);
                    }
                }
                for (z, row) in m.rows.iter().enumerate() {
                    let y = grid.top() + z as f32 * (zh + gap);
                    let differs = row.fields.len() > 1 && row.fields.iter().zip(&wide).any(|(a, b)| a.name != b.name);
                    let header_rect = Rect::from_min_size(Pos2::new(surface.left(), y), Vec2::new(label_b - 8.0, zh));
                    let mut job = egui::text::LayoutJob::simple(row.name.clone(), egui::FontId::monospace(11.0), Color32::from_gray(0xd0), header_rect.width());
                    job.wrap.max_rows = 3;
                    let g = p.layout_job(job);
                    p.galley(header_rect.left_top() + Vec2::new(0.0, 4.0), g, Color32::WHITE);
                    for (sp, f) in row.fields.iter().enumerate() {
                        let cell = Rect::from_min_size(Pos2::new(grid.left() + sp as f32 * (zw + gap), y), Vec2::new(zw, zh));
                        let image = Rect::from_min_size(cell.min, Vec2::new(bw, bh));
                        let resp = ui.interact(image, egui::Id::NULL.with(("dk-kontakt", z, sp)), egui::Sense::click());
                        p.rect_filled(image, 0.0, Color32::from_gray(0x14));
                        if let Some(t) = self.darkroom.contact.tex.get(&(z, sp)) {
                            p.image(t.id(), image, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        } else {
                            p.text(image.center(), egui::Align2::CENTER_CENTER, "…", egui::FontId::monospace(12.0), lr::TEXT_DIM);
                        }
                        let active = m.current == Some((z, sp));
                        let outline = if active { Stroke::new(2.0, lr::ACCENT) } else if resp.hovered() { Stroke::new(1.0, Color32::from_gray(0x78)) } else { Stroke::new(1.0, Color32::from_gray(0x2e)) };
                        p.rect_stroke(image, 0.0, outline, egui::StrokeKind::Inside);
                        if differs || row.fields.len() == 1 {
                            p.text(image.left_bottom() + Vec2::new(5.0, -5.0), egui::Align2::LEFT_BOTTOM, &f.name, egui::FontId::monospace(10.5), Color32::from_gray(0xe0));
                        }
                        if resp.clicked() {
                            pick = Some((z, sp));
                        }
                    }
                }
            });
        });
        if let Some((z, sp)) = pick {
            self.darkroom_contact_choose(index, z, sp);
        }
        if close {
            self.contact_view(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trix() -> Settings {
        let mut s = Settings::default();
        s.film.active = true;
        s.film.stock = "kodak_trix_400".into();
        s
    }

    #[test]
    fn row_name_without_maker_and_suffixes() {
        assert_eq!(row_name("Ilford Multigrade FB Classic (Barytpapier, glänzend)"), "Multigrade FB Classic");
        assert_eq!(row_name("Ilford Multigrade IV RC (glossy), contrast grades"), "Multigrade IV RC");
        assert_eq!(row_name("Kodak Print Film 2302"), "Print Film 2302");
    }

    #[test]
    fn no_film_no_sheet() {
        assert_eq!(model(&Settings::default()), Err("Film wählen"));
    }

    #[test]
    fn sheet_with_papers_and_grades() {
        if develop::film::stock("kodak_trix_400").is_none() {
            return;
        }
        let m = model(&trix()).expect("black & white film has papers");
        assert!(!m.slide && m.rows.len() >= 2, "{} rows", m.rows.len());
        let mg = m.rows.iter().position(|z| z.paper.as_deref() == Some("ilford_multigrade_iv_rc")).expect("Multigrade RC");
        let names: Vec<_> = m.rows[mg].fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["00", "0", "1", "2", "3", "4", "5"]);
        assert_eq!(m.columns, 7);
        // Default: Multigrade RC with grade 3
        assert_eq!(m.current, Some((mg, 4)));
        assert_eq!(m.standard.as_deref(), Some("ilford_multigrade_iv_rc"));
    }

    #[test]
    fn cell_applies_paper_and_grade_together() {
        if develop::film::stock("kodak_trix_400").is_none() {
            return;
        }
        let s = trix();
        let m = model(&s).unwrap();
        let fb = m.rows.iter().position(|z| z.paper.as_deref() == Some("ilford_multigrade_fb_classic"));
        let Some(fb) = fb else { return };
        let t = m.setting(&s, fb, 5).unwrap();
        assert_eq!(t.film.paper.as_deref(), Some("ilford_multigrade_fb_classic"));
        assert_eq!(t.film.paper_grade, 4.0);
        // Default paper is stored as "none selected"
        let std = m.rows.iter().position(|z| z.paper == m.standard).unwrap();
        let u = m.setting(&s, std, 2).unwrap();
        assert_eq!(u.film.paper, None);
        assert_eq!(u.film.paper_grade, 1.0);
        // everything else stays
        assert_eq!(t.film.stock, "kodak_trix_400");
        assert!(m.setting(&s, 99, 0).is_none());
    }

    #[test]
    fn slide_film_has_one_row_across_speed() {
        let mut s = Settings::default();
        s.film.active = true;
        s.film.stock = "agfa_scala_200x".into();
        if develop::film::stock("agfa_scala_200x").is_none() {
            return;
        }
        let m = model(&s).unwrap();
        assert!(m.slide && m.rows.len() == 1);
        let names: Vec<_> = m.rows[0].fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["100", "200", "400", "800", "1600"]);
        assert_eq!(m.current, Some((0, 1)), "default is EI 200");
        let t = m.setting(&s, 0, 3).unwrap();
        assert_eq!(t.film.film_dev, 800.0);
        assert_eq!(t.film.paper, None);
    }

    #[test]
    fn key_ignores_paper_and_grade() {
        if develop::film::stock("kodak_trix_400").is_none() {
            return;
        }
        let mut s = trix();
        let m = model(&s).unwrap();
        let a = cache_key(0, &s, &m);
        s.film.paper = Some("kodak_2302".into());
        s.film.paper_grade = 5.0;
        assert_eq!(a, cache_key(0, &s, &m), "paper and grade are not part of the key");
        s.film.grain = 1.7;
        assert_ne!(a, cache_key(0, &s, &m), "different grain needs new images");
    }

    #[test]
    fn push_changes_image_key_for_slide_too() {
        let mut s = trix();
        s.film.stock = "agfa_scala_200x".into();
        let m = model(&s).unwrap();
        let before = cache_key(1, &s, &m);
        s.film.push = 1.0;
        assert_ne!(before, cache_key(1, &s, &m));
        assert_ne!(before, cache_key(2, &trix(), &m));
    }

    #[test]
    fn image_change_discards_old_cells_and_old_completion_releases_new_sheet() {
        let ctx = egui::Context::default();
        let mut k = Contact::default();
        k.image_change(1);
        k.requested = Some((7, 0.0));
        k.computing = Some(7);
        k.outcome(&ctx, 7, 0, vec![(0, 2, 2, vec![128; 12])], false);
        assert_eq!(k.fields(), 1);
        k.image_change(2);
        assert_eq!(k.fields(), 0);
        assert_eq!(k.computing, Some(7));
        k.requested = Some((9, 1.0));
        k.task_finished(7);
        assert!(k.computing.is_none());
        assert_eq!(k.requested, Some((9, 1.0)));
        k.outcome(&ctx, 7, 0, vec![(0, 2, 2, vec![128; 12])], true);
        assert_eq!(k.fields(), 0);
    }

    #[test]
    fn result_is_only_accepted_for_matching_key() {
        let ctx = egui::Context::default();
        let mut k = Contact::default();
        k.requested = Some((7, 0.0));
        k.computing = Some(7);
        let rgb = vec![128u8; 4 * 3 * 3];
        k.outcome(&ctx, 8, 0, vec![(0, 4, 3, rgb.clone())], true);
        assert_eq!(k.fields(), 0, "foreign key is discarded");
        k.outcome(&ctx, 7, 0, vec![(0, 4, 3, rgb.clone()), (1, 4, 3, rgb.clone())], false);
        assert_eq!(k.fields(), 2);
        assert!(k.done.is_none() && k.computing == Some(7));
        k.outcome(&ctx, 7, 1, vec![(0, 4, 3, rgb)], true);
        assert_eq!(k.fields(), 3);
        assert_eq!(k.done, Some(7));
        assert!(k.computing.is_none());
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use eframe::App as _;

    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    /// Whole window: open the sheet with K, render cells, pick a cell (paper + grade), Esc closes only the sheet.
    #[test]
    fn contact_sheet_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        if develop::film::films().is_empty() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("graineo-dk-kontakt-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_fn(1800, 1200, |x, y| image::Rgb([(x / 8) as u8, (y / 6) as u8, 90])).save(folder.join("sample.png")).unwrap();
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
        let mut clock = 10.0;
        let mut frame = |a: &mut App, events: Vec<egui::Event>| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), events, ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame(&mut a, vec![]);
        frame(&mut a, vec![]);
        assert!(a.darkroom_active());
        assert!(!a.darkroom.contact.an);

        // K opens the sheet
        frame(&mut a, vec![key_event(egui::Key::K)]);
        frame(&mut a, vec![press(egui::Key::K)]);
        assert!(a.darkroom.contact.an, "K opens the contact sheet");
        a.contact_view(Some(false)); // the paper grid is checked here
        // wait until the cells are rendered
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        loop {
            a.handle_results(&ctx);
            frame(&mut a, vec![]);
            let i = a.current.as_ref().unwrap().index;
            let total: usize = model(&a.entries[i].settings).map(|m| m.rows.iter().map(|z| z.fields.len()).sum()).unwrap_or(0);
            if a.darkroom.contact.done.is_some() && a.darkroom.contact.fields() >= total && total > 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "cells did not finish: {}", a.darkroom.contact.fields());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }

        // Pick a cell: paper AND grade together, the sheet stays open
        let i = a.current.as_ref().unwrap().index;
        let m = model(&a.entries[i].settings).unwrap();
        let fb = m.rows.iter().position(|z| z.paper.as_deref() == Some("ilford_multigrade_fb_classic")).expect("FB paper");
        let id = egui::Id::NULL.with(("dk-kontakt", fb, 5usize));
        let rect = ctx.read_response(id).expect("visible paper cell").rect;
        let pos = rect.center();
        let click = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&mut a, vec![egui::Event::PointerMoved(pos), click(true)]);
        frame(&mut a, vec![click(false)]);
        let f = &a.entries[i].settings.film;
        assert_eq!(f.paper.as_deref(), Some("ilford_multigrade_fb_classic"));
        assert_eq!(f.paper_grade, 4.0);
        assert!(a.darkroom.contact.an, "the sheet stays open");
        assert_eq!(model(&a.entries[i].settings).unwrap().current, Some((fb, 5)));

        // Esc closes only the sheet, the darkroom stays
        frame(&mut a, vec![key_event(egui::Key::Escape)]);
        frame(&mut a, vec![press(egui::Key::Escape)]);
        assert!(!a.darkroom.contact.an, "Esc closes the sheet");
        assert!(a.darkroom_active(), "the darkroom stays open");
        // a second Esc leaves the darkroom
        frame(&mut a, vec![key_event(egui::Key::Escape)]);
        assert!(!a.darkroom.active);
    }
}
