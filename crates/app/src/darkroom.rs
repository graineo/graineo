//! Darkroom: a room of its own for the real
//! films of the film simulation with paper, development and print. The photo is shown large in the centre, the
//! library is hidden. On the left "Negative" (film shelf, development, grain, taking filter),
//! on the right "Paper" (paper shelf with characteristic curves, grade, tone), at the bottom "Print" (brightness), at the top a
//! narrow bar. Each film and paper has an info card via "?" with a character text,
//! characteristic curves and spectral sensitivity from the profile data ([`develop::film::map`]).
//!
//! The darkroom changes the same settings (`settings.film`) as "Develop"; there is
//! no separate render path.

use crate::menu::i18n::DeComma as _;
use std::collections::HashMap;

use develop::film::{self, Category, FilmSettings, InfoCard, Series};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, Vec2};

use crate::{dialog, lr, App, Tab};

/// State of the darkroom.
#[derive(Default)]
pub struct State {
    /// Darkroom shown instead of "Develop"?
    pub active: bool,
    pub advanced_open: Option<bool>,
    /// Thumbnail strip at the bottom (default: off)
    /// Tools of the two columns
    pub left_tab: crate::darkroom_layout::LeftTab,
    /// Handle row above the image (drawer position) and the two columns (overlays cover them too)
    pub handle_rect: Option<Rect>,
    pub column_rects: [Option<Rect>; 2],
    /// Grain loupe on request (off: zoom and pan are drawn from the whole image, nothing is rendered again)
    pub grain_check: bool,
    /// Image only: all side panels gone, only the top bar stays
    pub image_only: bool,
    pub left_collapsed: bool,
    pub right_closed: bool,
    /// Info card open: profile ID of the film or paper
    map: Option<String>,
    /// Cache of the info cards (the computation is not quite cheap)
    cards: HashMap<String, Option<InfoCard>>,
    /// Subtitles of the shelf rows (type and speed)
    subtitle: HashMap<String, String>,
    /// language generation of the two caches above (they hold translated text)
    lang_gen: u64,
    /// Test hook (GRAINEO_DARKROOM_FILM): film selected on the first draw
    test_film: Option<String>,
    /// Compare (phase 2, dunkelkammer_vergleich.rs)
    pub compare: crate::darkroom_compare::Compare,
    /// Test strip (phase 2, dunkelkammer_streifen.rs)
    pub probe: crate::darkroom_strip::Strip,
    /// Zone system display and metering point (phase 2, dunkelkammer_zonen.rs)
    pub zones: crate::darkroom_zones::Zones,
    /// Live markers in the info card (phase 2, dunkelkammer_marker.rs)
    pub marker: crate::darkroom_marker::Marker,
    /// Contact sheet as a grid (dunkelkammer_kontakt.rs)
    pub contact: crate::darkroom_contact::Contact,
    /// Recipe book (phase 3, dunkelkammer_rezepte.rs)
    pub recipes: crate::darkroom_recipes::Recipes,
    /// "On paper" viewing mode (phase 3, dunkelkammer_rand.rs): display only
    pub on_paper: bool,
    pub lut: crate::darkroom_lut::Lut,
    pub scan: crate::darkroom_scan::Scan,
    pub curves: crate::darkroom_curve::Curve,
    pub probe_light: crate::darkroom_probe::Probe,
    /// Dodge and burn (phase 3, dunkelkammer_abwedeln.rs)
    pub dodge: crate::darkroom_dodge::Dodge,
    /// Test hook (GRAINEO_DARKROOM_DODGE): type and strength of a mask created on the first draw
    test_dodge: Option<(usize, f32)>,
    /// Free space in the toolbar between the left group (views, comparison actions) and the right one (negative = overlap)
    pub top_gap: f32,
    /// Test hook (GRAINEO_DARKROOM_BORDER): paper border, bottom border, film border set on the first draw
    test_border_ui: Option<(f32, f32, f32)>,
}

impl State {
    /// The caches hold translated text: drop them after a language switch.
    fn refresh_language(&mut self) {
        let g = crate::menu::i18n::generation();
        if g != self.lang_gen {
            self.lang_gen = g;
            self.cards.clear();
            self.subtitle.clear();
        }
    }

    /// Info card (cached).
    fn fetch_card(&mut self, id: &str) -> Option<InfoCard> {
        self.refresh_language();
        self.cards.entry(id.to_string()).or_insert_with(|| film::card(id).map(card_translated)).clone()
    }

    /// ID of the open info card.
    pub fn card_open(&self) -> Option<String> {
        self.map.clone()
    }

    /// Test hook: select the film on the first draw.
    pub fn test_film(&mut self, id: &str) {
        self.test_film = Some(id.to_string());
    }

    /// Test hook: create the dodge mask on the first draw.
    pub fn test_dodge(&mut self, kind: usize, ev: f32) {
        self.test_dodge = Some((kind, ev));
    }

    /// Test hook: set the border on the first draw.
    pub fn test_border(&mut self, paper: f32, bottom: f32, film: f32) {
        self.test_border_ui = Some((paper, bottom, film));
    }

    /// Test hook (GRAINEO_TAB=darkroom-card): info card open from the start.
    pub fn test_card(&mut self, id: &str) {
        self.map = Some(id.to_string());
    }

    /// Subtitle of a film or paper row: speed and a short characterisation (without one: type and speed) (cached).
    fn subtitle(&mut self, f: &film::StockInfo) -> String {
        self.refresh_language();
        if let Some(t) = self.subtitle.get(&f.id) {
            return t.clone();
        }
        let kind = f.category.label();
        let iso = self.fetch_card(&f.id).and_then(|k| k.iso);
        let t = match (iso, crate::darkroom_shelf::character(&f.id)) {
            (Some(iso), Some(c)) => format!("ISO {iso:.0} · {}", crate::tr(c)),
            (None, Some(c)) => crate::tr(c).to_string(),
            (Some(iso), None) => format!("{} · ISO {iso:.0}", crate::tr(kind)),
            (None, None) => crate::tr(kind).to_string(),
        };
        self.subtitle.insert(f.id.clone(), t.clone());
        t
    }
}

/// The cards come with German texts from the profile data; translate them here once into the UI language
/// (texts without a dictionary entry stay German).
fn card_translated(mut k: InfoCard) -> InfoCard {
    let t = |s: &String| crate::tr(s).to_string();
    k.name = t(&k.name);
    k.type_name = t(&k.type_name);
    k.additional = t(&k.additional);
    k.text = t(&k.text);
    k.source = t(&k.source);
    k.process = t(&k.process);
    k.period = t(&k.period);
    k.suitable = k.suitable.iter().map(t).collect();
    k.less_suitable = k.less_suitable.iter().map(t).collect();
    k.tips = k.tips.iter().map(t).collect();
    k.facts = k.facts.iter().map(|(a, b)| (t(a), t(b))).collect();
    // composite hint: translate fixed building blocks, numbers stay
    for split in ["Orthochromatisch: nicht rot-empfindlich", "Panchromatisch, bis ins nahe Infrarot empfindlich", "Panchromatisch", "(Empfindlichkeit reicht bis etwa", "; Rot gegenüber Grün", "Blenden)"] {
        k.spectral_note = k.spectral_note.replace(split, crate::tr(split));
    }
    for r in k.characteristic_curves.iter_mut() {
        if let Some(rest) = r.name.strip_prefix("Grad ") {
            r.name = format!("{} {rest}", crate::tr("Grad"));
        }
    }
    k
}

/// Groups of the film shelf: black & white first.
const GROUPS: [(Category, &str); 4] = [
    (Category::BlackWhite, "Schwarzweiß-Filme"),
    (Category::ColorNegative, "Farbnegative"),
    (Category::Slide, "Dia-Filme"),
    (Category::Cine, "Kinofilme"),
];

/// Wavelength range of the spectra, rounded to 50 nm.
fn spectrum_range(r: &[Series]) -> (f32, f32) {
    let (mut from, mut to) = (f32::MAX, f32::MIN);
    for p in r.iter().flat_map(|r| r.points.iter()) {
        from = from.min(p.0);
        to = to.max(p.0);
    }
    if from > to {
        return (380.0, 700.0);
    }
    (((from - 10.0) / 50.0).floor() * 50.0, ((to + 10.0) / 50.0).ceil() * 50.0)
}

/// Shorter display name for the shelf rows: without additions like "Black and White Negative Film" and without
/// the developer after the comma.
pub(crate) fn paper_short_name(name: &str) -> String {
    name.split(['(', ',']).next().unwrap_or(name).trim().to_string()
}

pub(crate) fn short_name(name: &str) -> String {
    let front = name.split(',').next().unwrap_or(name);
    let pipeline = ["Black", "and", "White", "Negative", "Film", "Professional", "(TX)", "(TMX)", "(TMY)"];
    let t: Vec<&str> = front.split_whitespace().filter(|w| !pipeline.contains(w)).collect();
    if t.is_empty() { name.to_string() } else { t.join(" ") }
}

/// Color of the i-th of n curves; the active one bright, the others muted.
fn curve_color(i: usize, n: usize, active: bool, spectrum: bool) -> Color32 {
    if spectrum {
        return match i {
            0 => Color32::from_rgb(0xd8, 0x6a, 0x5c),
            1 => Color32::from_rgb(0x72, 0xb8, 0x6a),
            _ => Color32::from_rgb(0x6a, 0x96, 0xd8),
        };
    }
    if active {
        return Color32::from_rgb(0xf0, 0xd4, 0x8a);
    }
    let t = if n > 1 { i as f32 / (n - 1) as f32 } else { 0.5 };
    let g = (0x62 as f32 + t * 0x30 as f32) as u8;
    Color32::from_rgb(g, g, (g as f32 * 1.05).min(255.0) as u8)
}

fn decimal_comma(x: f32, digits: usize) -> String {
    format!("{x:.digits$}").de_comma()
}

/// Draw characteristic curves or spectra into a rectangle. `x`/`y` are the displayed ranges,
/// `axes` switches on grid and labels (large display), `active` highlights one curve.
fn draw(p: &egui::Painter, r: Rect, series: &[Series], x: (f32, f32), y: (f32, f32), axes: bool, active: Option<usize>, spectrum: bool) {
    p.rect_filled(r, 0.0, lr::PANEL_DARK());
    p.rect_stroke(r, 0.0, Stroke::new(1.0, crate::lr::gray(0x2a)), egui::StrokeKind::Inside);
    let (margin_left, margin_bottom) = if axes { (30.0, 16.0) } else { (3.0, 3.0) };
    let field = Rect::from_min_max(r.min + Vec2::new(margin_left, 4.0), r.max - Vec2::new(5.0, margin_bottom));
    let map_px = |px: f32, py: f32| Pos2::new(field.left() + (px - x.0) / (x.1 - x.0) * field.width(), field.bottom() - (py - y.0) / (y.1 - y.0) * field.height());
    if axes {
        let font = egui::FontId::proportional(10.0);
        let step_x = if x.1 - x.0 > 100.0 { 50.0 } else { 1.0 };
        let mut gx = (x.0 / step_x).ceil() * step_x;
        while gx <= x.1 {
            let a = map_px(gx, y.0);
            p.line_segment([Pos2::new(a.x, field.top()), a], Stroke::new(1.0, crate::lr::gray(0x1e)));
            p.text(Pos2::new(a.x, field.bottom() + 8.0), egui::Align2::CENTER_CENTER, format!("{gx:.0}"), font.clone(), lr::TEXT_DIM());
            gx += step_x;
        }
        let step_y = if y.1 - y.0 > 2.5 { 1.0 } else { 0.5 };
        let mut gy = (y.0 / step_y).ceil() * step_y;
        while gy <= y.1 + 1e-4 {
            let a = map_px(x.0, gy);
            p.line_segment([a, Pos2::new(field.right(), a.y)], Stroke::new(1.0, crate::lr::gray(0x1e)));
            p.text(Pos2::new(field.left() - 4.0, a.y), egui::Align2::RIGHT_CENTER, decimal_comma(gy, if step_y < 1.0 { 1 } else { 0 }), font.clone(), lr::TEXT_DIM());
            gy += step_y;
        }
    }
    let n = series.len();
    // active curve last so it is on top
    let mut order: Vec<usize> = (0..n).filter(|i| Some(*i) != active).collect();
    order.extend(active);
    for i in order {
        let is_active = Some(i) == active || (n == 1 && !spectrum);
        let color = curve_color(i, n, is_active, spectrum);
        let width = if is_active || spectrum { 1.6 } else { 1.0 };
        let pts: Vec<Pos2> = series[i].points.iter().filter(|(px, py)| *px >= x.0 - 1.0 && *px <= x.1 + 1.0 && *py >= y.0 - 5.0).map(|&(px, py)| {
            let a = map_px(px, py.clamp(y.0, y.1));
            Pos2::new(a.x.clamp(field.left(), field.right()), a.y.clamp(field.top(), field.bottom()))
        }).collect();
        if pts.len() > 1 {
            p.add(egui::Shape::line(pts, Stroke::new(width, color)));
        }
    }
}

/// "?" button: circle with question mark (as in the help cards). Returns: clicked?
fn question_mark(ui: &mut egui::Ui, center: Pos2, id: egui::Id) -> bool {
    let r = Rect::from_center_size(center, Vec2::splat(18.0));
    let resp = ui.interact(r, id, egui::Sense::click());
    let color = if resp.hovered() { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() };
    lr::paint_icon(ui.painter(), Rect::from_center_size(center, Vec2::splat(13.0)), lr::Icon::Info, color);
    resp.clicked()
}

/// Room on the right of list rows for the scroll bar.
const LIST_SCROLL_GAP: f32 = 0.0;

/// Calm list row: no box; selected a little lighter with a fine mark, hover barely lighter, a thin line below.
fn list_row_background(p: &egui::Painter, r: Rect, active: bool, hovered: bool) {
    if active {
        p.rect_filled(r, 0.0, crate::lr::gray(0x26));
        p.rect_filled(Rect::from_min_size(r.min, Vec2::new(2.0, r.height())), 0.0, crate::lr::gray(0xc8));
    } else if hovered {
        p.rect_filled(r, 0.0, crate::lr::gray(0x1f));
    }
    p.line_segment([Pos2::new(r.left(), r.bottom() - 0.5), Pos2::new(r.right(), r.bottom() - 0.5)], Stroke::new(1.0, crate::lr::gray(0x2a)));
}

/// Head of a darkroom column in the style of the tool headers in Develop.
pub(crate) fn column_title(ui: &mut egui::Ui, title: &str) {
    column_head(ui, title, |_| false);
}

/// Fixed column head: title and an optional control on one centre line, same height as the tool headers in Develop.
pub(crate) fn column_head(ui: &mut egui::Ui, title: &str, control: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(Vec2::new(w, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_size(Vec2::new(w, 24.0));
        let g = lr::caps(ui.painter(), title, lr::semibold(11.5), crate::lr::gray(0xd8));
        let (r, _) = ui.allocate_exact_size(Vec2::new(g.size().x, 24.0), egui::Sense::hover());
        ui.painter().galley(Pos2::new(r.left(), r.center().y - g.size().y / 2.0), g, crate::lr::TEXT_STRONG());
        ui.add_space(10.0);
        control(ui)
    })
    .inner
}

/// Why a film has no paper to choose: named for what really applies.
pub(crate) fn no_paper_text(f: &FilmSettings) -> String {
    if !f.is_active() {
        return crate::tr("Kein Film gewählt.").to_string();
    }
    if f.negative {
        return crate::tr("Die Negativansicht zeigt den Film ohne Papier.").to_string();
    }
    match film::stock(&f.stock) {
        Some(s) if s.positive => crate::trf!("{film} ist ein Umkehrfilm: kein Papier nötig. Für Papierwahl einen Negativfilm wählen.", film = short_name(&s.name)),
        _ => crate::tr("Für diesen Film gibt es keine Papierwahl.").to_string(),
    }
}

/// Shown instead of an empty film list when the spektrafilm data were not found (paths are in the log).
pub(crate) fn film_data_missing_hint(ui: &mut egui::Ui) {
    if film::data_dir().is_none() {
        ui.add(egui::Label::new(egui::RichText::new(crate::tr("Filmdaten nicht gefunden – der Ordner „spektrafilm“ muss neben dem Programm liegen.")).color(lr::TEXT_DIM())).wrap());
        ui.add_space(6.0);
    }
}

#[allow(non_snake_case)]
fn CARD_LINE() -> Color32 {
    crate::lr::gray(0x33)
}
/// Running text in the info modal stays at a comfortable line length.
const CARD_MEASURE: f32 = 520.0;

/// Info modal: section heading with a fine line below.
fn card_heading(ui: &mut egui::Ui, title: &str) {
    let g = lr::caps_spaced(ui.painter(), crate::tr(title), egui::FontId::proportional(10.5), crate::lr::gray(0xbc), 1.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), egui::Sense::hover());
    ui.painter().galley(Pos2::new(r.left(), r.top() + 2.0), g, crate::lr::TEXT_STRONG());
    ui.painter().line_segment([Pos2::new(r.left(), r.bottom() - 0.5), Pos2::new(r.right(), r.bottom() - 0.5)], Stroke::new(1.0, CARD_LINE()));
    ui.add_space(5.0);
}

/// Info modal: list item with a hanging indent.
fn card_item(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (r, _) = ui.allocate_exact_size(Vec2::new(14.0, 16.0), egui::Sense::hover());
        ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, "·", egui::FontId::proportional(12.0), lr::TEXT_DIM());
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(CARD_MEASURE));
            card_text(ui, text, crate::lr::gray(0xd0), 12.0);
        });
    });
    ui.add_space(5.0);
}

/// Title of the info modal: the name without the film type it repeats from the subtitle.
fn card_title(k: &InfoCard) -> String {
    let mut name = k.name.split(',').next().unwrap_or(&k.name).trim().to_string();
    for suffix in ["Black and White Negative Film", "Black and White Reversal Film", "Color Negative Film", "Colour Negative Film", "Negative Film", "Reversal Film", "Professional"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.trim().to_string();
        }
    }
    if name.is_empty() { k.name.clone() } else { name }
}

/// Info modal: text in the app's font, always left aligned.
fn card_text(ui: &mut egui::Ui, text: &str, color: Color32, size: f32) {
    ui.add(egui::Label::new(egui::RichText::new(text).font(egui::FontId::proportional(size)).color(color)).wrap());
}

/// Info modal: list under a heading.
fn card_list(ui: &mut egui::Ui, title: &str, entries: &[String]) {
    if entries.is_empty() {
        return;
    }
    card_heading(ui, title);
    for e in entries {
        card_item(ui, e);
    }
}

/// Info modal: aligned key/value rows.
fn card_rows(ui: &mut egui::Ui, rows: &[(String, String)]) {
    const KEY_W: f32 = 128.0;
    let w = ui.available_width();
    for (n, (key, value)) in rows.iter().enumerate() {
        let top = ui.cursor().top();
        ui.add_space(5.0);
        ui.horizontal_top(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(KEY_W, 16.0), egui::Sense::hover());
            ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, key, egui::FontId::proportional(11.0), lr::TEXT_DIM());
            ui.vertical(|ui| {
                ui.set_max_width(ui.available_width());
                card_text(ui, value, crate::lr::gray(0xd8), 11.5);
            });
        });
        ui.add_space(5.0);
        let y = ui.cursor().top() - 0.5;
        if n + 1 < rows.len() {
            ui.painter().line_segment([Pos2::new(ui.min_rect().left(), y), Pos2::new(ui.min_rect().left() + w, y)], Stroke::new(1.0, CARD_LINE()));
        }
        let _ = top;
    }
}

/// Select a film (like a click in the shelf): paper, development, grade and push to the film's defaults.
pub(crate) fn set_film(s: &mut develop::Settings, id: Option<String>) {
    match id {
        Some(id) => {
            s.film.active = true;
            s.film.stock = id;
            s.film.paper = None;
            s.film.film_dev = 0.0;
            s.film.paper_grade = -1.0;
            s.film.push = 0.0;
        }
        None => s.film.active = false,
    }
}

/// Row in the film shelf: the whole photo small with this film, name, characterisation, sign of the kind of film
/// (opens the info card). Returns: (clicked, sign clicked, visible, hovered).
/// `waiting`: the tile of this film is still being computed (a placeholder instead of an empty, black field).
fn shelf_row(ui: &mut egui::Ui, stock: Option<&film::StockInfo>, id: &str, tex: Option<&egui::TextureHandle>, waiting: bool, name: &str, sub: &str, active: bool) -> (bool, bool, bool, bool) {
    use crate::darkroom_shelf::{FIELD, ROW_H, SIGN_W};
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width() - LIST_SCROLL_GAP, ROW_H), egui::Sense::click());
    let visible = ui.is_rect_visible(r);
    if visible {
        let p = ui.painter();
        list_row_background(p, r, active, resp.hovered());
        let ppp = ui.ctx().pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let thumb_rect = Rect::from_min_size(Pos2::new(snap(r.left() + 4.0), snap(r.top() + (ROW_H - FIELD.y) / 2.0)), FIELD);
        p.rect_filled(thumb_rect, 0.0, lr::PANEL_DARK());
        if let Some(t) = tex {
            crate::darkroom_shelf::paint_tile(p, thumb_rect, t, ppp);
        } else if waiting {
            crate::darkroom_shelf::paint_waiting(p, thumb_rect, ui.input(|i| i.time));
        }
        let text_x = thumb_rect.right() + 10.0;
        let width = r.right() - text_x - SIGN_W;
        let mut job = egui::text::LayoutJob::simple_singleline(name.to_string(), egui::TextStyle::Body.resolve(ui.style()), if active { crate::lr::TEXT_STRONG() } else { lr::TEXT() });
        job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let g = p.layout_job(job);
        p.galley(Pos2::new(text_x, r.top() + 5.0), g, crate::lr::TEXT_STRONG());
        // the characterisation may take two lines
        let mut job = egui::text::LayoutJob::simple(sub.to_string(), egui::FontId::proportional(10.0), lr::TEXT_DIM(), width);
        job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
        let g = p.layout_job(job);
        p.galley(Pos2::new(text_x, r.top() + 21.0), g, lr::TEXT_DIM());
    }
    let q = match stock {
        Some(s) => crate::darkroom_shelf::sign_button(ui, r, ui.scope_id().with(("dk-info", id)), crate::darkroom_shelf::format_of(s), crate::darkroom_shelf::color_of(s)),
        None => question_mark(ui, Pos2::new(r.right() - 8.0 - 9.0, r.center().y), ui.scope_id().with(("dk-info", id))),
    };
    (resp.clicked() && !q, q, visible, resp.hovered() && !q)
}

/// Section "Foveon" (Sigma-Analog): black-and-white film exposed directly from the three sensor layers, grain instead
/// of sensor noise. Always shown with an active film (fixed place, no layout jump); greyed out with the reason as tooltip
/// where it does not apply (other cameras, colour film).
fn foveon_section(ui: &mut egui::Ui, s: &mut develop::Settings, cal: Option<&develop::Calibration>) {
    let reason = develop::analog::unavailable_reason(cal, s);
    let grain_reason = develop::analog::grain_balance_reason(cal, s);
    let d = FilmSettings::default();
    let changed = s.film.layer_exposure != d.layer_exposure || s.film.grain_balance != d.grain_balance;
    let weights = cal.and_then(|c| develop::analog::weights(c, s));
    if lr::section_reset(ui, "Foveon", true, changed, |ui| {
        ui.horizontal(|ui| {
            lr::group_label(ui, "Sigma-Analog");
            crate::help::info(ui, "film-foveon", "Sigma-Analog", &crate::help::FILM_FOVEON);
        });
        let row = |ui: &mut egui::Ui, on: &mut bool, label: &str, tip: &str, reason: Option<&str>| {
            let r = ui.add_enabled_ui(reason.is_none(), |ui| {
                ui.horizontal(|ui| {
                    lr::switch(ui, on);
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(crate::tr(label)).color(if reason.is_none() { lr::TEXT() } else { lr::TEXT_DIM() }));
                })
                .response
            });
            let hover = ui.interact(r.inner.rect, ui.id().with(("foveon-tip", label)), egui::Sense::hover());
            lr::hover_tip(ui, &hover, reason.unwrap_or(tip));
        };
        row(ui, &mut s.film.layer_exposure, "Aus den Sensorschichten belichten", "Der Film sieht direkt die drei Foveon-Schichten: filmgetreue Grautöne", reason);
        row(ui, &mut s.film.grain_balance, "Korn gleicht Sensorrauschen aus", "Weniger Filmkorn, wo das Foto schon rauscht; bei Farbfilm auch weniger Farbrauschen", grain_reason);
        // the weights of the layers, so that the effect stays visible (always one line: no layout jump)
        let text = match weights {
            Some(w) => crate::trf!("oben {o} · Mitte {m} · unten {u}", o = decimal_comma(w.w[2], 2), m = decimal_comma(w.w[1], 2), u = decimal_comma(w.w[0], 2)),
            None => " ".to_string(),
        };
        let r = ui.add(egui::Label::new(egui::RichText::new(text).font(egui::FontId::monospace(10.5)).color(lr::ACCENT())).truncate());
        lr::hover_tip(ui, &r, "Anteil der drei Sensorschichten an der Belichtung des Films");
    }) {
        s.film.layer_exposure = d.layer_exposure;
        s.film.grain_balance = d.grain_balance;
    }
}

/// Slider (value as f32, default `def`).
fn slider(ui: &mut egui::Ui, label: &str, x: &mut f32, r: std::ops::RangeInclusive<f64>, def: f32, step: f64) {
    let mut t = *x as f64;
    if lr::slider(ui, label, &mut t, r, def as f64, step) {
        *x = t as f32;
    }
}

/// Narrow section heading (capitals, letter-spaced).
pub(crate) fn header(ui: &mut egui::Ui, text: &str) {
    let g = lr::caps(ui.painter(), text, lr::semibold(lr::UI_SIZE), crate::lr::gray(0xe2));
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), egui::Sense::hover());
    ui.painter().galley(Pos2::new(r.left(), r.center().y - g.size().y / 2.0), g, crate::lr::gray(0xe2));
}

impl App {
    /// Is the darkroom active (only in single image view with an open photo)?
    pub(crate) fn darkroom_active(&self) -> bool {
        self.darkroom.active && self.tab == Tab::Develop && self.current.is_some()
    }

    /// Open the darkroom (from Develop or the library).
    pub(crate) fn enter_darkroom(&mut self) {
        if !self.has_photo() {
            return;
        }
        if self.active_unreachable() {
            self.status = crate::tr("Dunkelkammer: Original nicht erreichbar – geht wieder, sobald der Speicherort verbunden ist").into();
            return;
        }
        if self.in_grid() {
            self.to_single_image();
        }
        self.tab = Tab::Develop;
        self.profile_browser = false;
        self.darkroom.active = true;
        self.darkroom.image_only = false;
    }

    /// Back to "Develop".
    pub(crate) fn leave_darkroom(&mut self) {
        self.darkroom.compare.adopt();
        self.darkroom.active = false;
        self.darkroom.image_only = false;
        self.contact_view(None);
        self.darkroom.recipes.opened = false;
        self.darkroom.recipes.drawer = false;
        self.darkroom.map = None;
        // the shared drawing tool (dodge) must not keep changing masks in the Develop image
        crate::masks_ui::finish(&self.egui_ctx);
    }

    /// Darkroom keys, before all other shortcuts: ⇧D on/off, F6 thumbnail strip, Tab "Image only",
    /// Esc back to Develop (the key is consumed so that Esc does not also jump to the grid).
    pub(crate) fn darkroom_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.lib.import_open() || ctx.memory(|m| m.top_modal_layer().is_some()) || self.tab == Tab::Library {
            return;
        }
        if crate::shortcuts::pressed(ctx, "darkroom") {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::D));
            if self.darkroom.active { self.leave_darkroom() } else { self.enter_darkroom() }
            return;
        }
        if !self.darkroom_active() {
            return;
        }
        let none_mods = egui::Modifiers::NONE;
        if ctx.input_mut(|i| i.consume_key(none_mods, egui::Key::F6)) {
            self.show_filmstrip = !self.show_filmstrip;
        }
        if crate::shortcuts::pressed(ctx, "dk_contact") {
            self.contact_view_toggle();
        }
        // Tab is already intercepted by the app's input hook (Tab belongs to the panels)
        if let Some(shift) = self.tab_pressed.take() {
            self.toggle_panels(shift);
        }
        if self.crop_mode.is_none() && !self.measure && !self.wb_picker && ctx.input_mut(|i| i.consume_key(none_mods, egui::Key::Escape)) {
            // first the drawer and the contact sheet, then the darkroom
            if self.darkroom.recipes.drawer {
                self.darkroom.recipes.drawer = false;
            } else if self.darkroom.contact.an {
                self.contact_view(None);
            } else {
                self.leave_darkroom();
            }
        }
    }

    /// Select the darkroom view (0 Single, 1 Compare, 2 Paper comparison, 3 Contact print, 4 Recipes); only one at a time.
    pub(crate) fn darkroom_choose_view(&mut self, target: usize) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let sheet = self.darkroom.contact.an;
        let war = (self.darkroom.compare.mode != crate::darkroom_compare::Mode::Off, sheet && !self.darkroom.contact.print.an, sheet && self.darkroom.contact.print.an, self.darkroom.recipes.opened);
        // switch everything off, then switch on the requested view
        match self.darkroom.compare.mode {
            crate::darkroom_compare::Mode::Off => {}
            crate::darkroom_compare::Mode::NoFilm => self.darkroom.compare.adopt(),
            crate::darkroom_compare::Mode::AB => {
                let mut s = self.entries[index].settings.clone();
                self.darkroom.compare.switch_off(&mut s);
                self.entries[index].settings = s;
            }
        }
        self.contact_view(None);
        self.darkroom.recipes.opened = false;
        self.darkroom.recipes.message.clear();
        match target {
            1 if !war.0 => self.darkroom.compare.without_film(),
            2 if !war.1 => self.contact_view(Some(false)),
            3 if !war.2 => self.contact_view(Some(true)),
            4 if !war.3 => self.darkroom.recipes.opened = true,
            _ => {}
        }
    }

    /// The settings of the open photo (mutable).
    fn darkroom_settings(&mut self) -> Option<&mut develop::Settings> {
        let i = self.current.as_ref()?.index;
        self.entries.get_mut(i).map(|e| &mut e.settings)
    }

    /// Left side "Negative": film shelf, development, grain, taking filter.
    pub(crate) fn darkroom_left(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        if let Some(id) = self.darkroom.test_film.take() {
            if let Some(s) = self.darkroom_settings() {
                s.film.active = true;
                s.film.stock = id;
                s.film.paper = None;
            }
        }
        if let Some((p, u, f)) = self.darkroom.test_border_ui.take() {
            if let Some(s) = self.darkroom_settings() {
                s.film.print_border = p;
                s.film.print_border_bottom = u;
                s.film.film_border = f > 0.5 && f < 1.5;
                s.film.film_strip = f > 1.5;
            }
        }
        if let Some((kind, ev)) = self.darkroom.test_dodge.take() {
            let ctx = ui.ctx().clone();
            crate::masks_ui::new_exposure_layer(&ctx, &mut self.entries[index].settings.local, index, kind, ev);
            crate::masks_ui::finish(&ctx);
        }
        use crate::darkroom_layout::LeftTab;
        ui.add_space(4.0);
        if self.darkroom.left_tab == LeftTab::Controls {
            column_title(ui, crate::tr("REGLER"));
            ui.add_space(2.0);
            ui.separator();
            egui::ScrollArea::vertical().id_salt("dk-regler").auto_shrink([false, false]).show(ui, |ui| crate::sticky::scope(ui, |ui| {
                if !self.entries[index].settings.film.is_active() {
                    ui.label(egui::RichText::new(crate::tr("Film wählen")).color(lr::TEXT_DIM()));
                } else {
                    self.darkroom_film_sliders(ui, index);
                    self.darkroom_extended_panel(ui, index);
                }
                ui.add_space(12.0);
            }));
            return;
        }
        // Fixed head: film simulation off, data sheet or tuned; the last film stays remembered
        let f = &mut self.entries[index].settings.film;
        let mut mode = if !f.is_active() { 0u8 } else if f.look_datasheet { 1 } else { 2 };
        let changed = column_head(ui, crate::tr("FILM"), |ui| lr::segmented_pad(ui, &mut mode, &[(0, crate::tr("Aus")), (1, crate::tr("Datenblatt")), (2, crate::tr("Abgestimmt"))], 6.0));
        if changed {
            let f = &mut self.entries[index].settings.film;
            if mode == 0 {
                f.active = false;
            } else {
                if f.stock.is_empty() {
                    if let Some(first) = film::films().first() {
                        f.stock = first.id.clone();
                    }
                }
                f.active = true;
                f.look_datasheet = mode == 1;
            }
        }
        ui.add_space(2.0);
        ui.separator();
        let thumbs = self.film_previews_request(ui, &[], &[]);
        let failed = self.film_previews.failed().clone();
        let subs: HashMap<String, String> = film::films().iter().map(|f| (f.id.clone(), self.darkroom.subtitle(f))).collect();
        let mut visible: Vec<String> = Vec::new();
        let mut hover: Option<String> = None;
        let mut pick: Option<String> = None;
        let mut info: Option<String> = None;
        let current_id = self.entries[index].settings.film.is_active().then(|| self.entries[index].settings.film.stock.clone());
        let mut area = egui::ScrollArea::vertical().id_salt("dk-regal").auto_shrink([false, false]);
        if let Some(y) = crate::darkroom_shelf::test_scroll() {
            area = area.vertical_scroll_offset(y);
        }
        area.show(ui, |ui| crate::sticky::scope(ui, |ui| {
            ui.add_space(6.0);
            film_data_missing_hint(ui);
            {
                for (cat, title) in GROUPS {
                    let films: Vec<_> = film::films().iter().filter(|f| f.category == cat).collect();
                    if films.is_empty() {
                        continue;
                    }
                    ui.add_space(4.0);
                    lr::group_label(ui, title);
                    for f in films {
                        let sub = subs.get(&f.id).cloned().unwrap_or_default();
                        let (click, q, visible_, hovered) = shelf_row(ui, Some(f), &f.id, thumbs.get(&f.id), !failed.contains(&f.id), &short_name(&f.name), &sub, current_id.as_deref() == Some(f.id.as_str()));
                        if visible_ {
                            visible.push(f.id.clone());
                        }
                        if click {
                            pick = Some(f.id.clone());
                        }
                        if hovered && !current_id.as_deref().is_some_and(|a| a == f.id) {
                            hover = Some(f.id.clone());
                        }
                        if q {
                            info = Some(f.id.clone());
                        }
                    }
                }
            }
            ui.add_space(6.0);
        }));
        // visible rows first, then the other films quietly (the shelf is scrolled through)
        let all: Vec<String> = GROUPS.iter().flat_map(|(cat, _)| film::films().iter().filter(move |f| f.category == *cat)).map(|f| f.id.clone()).collect();
        self.film_previews_request(ui, &visible, &all);
        // Hovering a film: quick preview in the large image (as with presets) without selecting it
        let waits = self.derived_waits(&self.entries[index].settings);
        match hover.filter(|_| pick.is_none() && !waits) {
            Some(id) => {
                let mut h = self.entries[index].settings.clone();
                set_film(&mut h, Some(id));
                if self.hover_preset.as_ref() != Some(&h) {
                    if let Some(c) = &self.current {
                        self.worker.send(crate::jobs::Job::HoverRender { index: c.index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: h.clone(), supersede: true });
                    }
                    self.hover_preset = Some(h);
                }
                self.film_hover = true;
            }
            None => {
                if self.film_hover {
                    self.hover_preset = None;
                    self.film_hover = false;
                }
            }
        }
        if let Some(w) = pick {
            if let Some(s) = self.darkroom_settings() {
                set_film(s, Some(w));
            }
        }
        if info.is_some() {
            self.darkroom.map = info;
        }
    }

    /// Film sliders: development, push/pull, contrast, grain, halation, taking filter.
    fn darkroom_film_sliders(&mut self, ui: &mut egui::Ui, index: usize) {
        let d = FilmSettings::default();
        let cal = self.current.as_ref().map(|c| c.loaded.cal.clone());
        let s = &mut self.entries[index].settings;
        if !s.film.is_active() {
            return;
        }
        foveon_section(ui, s, cal.as_deref());
        let info = film::stock(&s.film.stock).cloned();
        let ei_text = film::development_info(&s.film).map(|i| match (i.ei, i.minutes) {
            (Some(ei), Some(min)) => format!("EI {ei:.0} · {} min", decimal_comma(min as f32, 1).trim_end_matches(",0")),
            (Some(ei), None) => format!("EI {ei:.0}"),
            (None, Some(min)) => format!("{} min", decimal_comma(min as f32, 1).trim_end_matches(",0")),
            _ => String::new(),
        });
        let f = &mut self.entries[index].settings.film;
        let develop_changed = f.film_dev != d.film_dev || f.push != d.push || f.film_gamma != d.film_gamma || f.exposure != d.exposure;
        if lr::section_reset(ui, "Entwicklung", true, develop_changed, |ui| {
            if let Some(si) = info.as_ref().filter(|i| i.family.len() > 1) {
                lr::group_label(ui, "Entwicklungszeit (min)");
                let l: Vec<(f32, String)> = std::iter::once((0.0, "Std".to_string())).chain(si.family.iter().map(|&t| (t, format!("{t}")))).collect();
                let o: Vec<(f32, &str)> = l.iter().map(|(t, n)| (*t, n.as_str())).collect();
                lr::segmented(ui, &mut f.film_dev, &o);
            }
            slider(ui, "Push / Pull", &mut f.push, -2.0..=3.0, 0.0, 0.25);
            if let Some(t) = ei_text.as_deref().filter(|t| !t.is_empty()) {
                ui.label(egui::RichText::new(t).font(egui::FontId::monospace(10.5)).color(lr::ACCENT()));
            }
            slider(ui, "Filmkontrast", &mut f.film_gamma, 0.5..=1.5, 1.0, 0.01);
            slider(ui, "Filmbelichtung", &mut f.exposure, -3.0..=3.0, 0.0, 0.05);
        }) {
            f.film_dev = d.film_dev;
            f.push = d.push;
            f.film_gamma = d.film_gamma;
            f.exposure = d.exposure;
        }
        let grain_changed = f.grain != d.grain || f.grain_size != d.grain_size || f.grain_model != d.grain_model || f.halation != d.halation || f.format_mm != d.format_mm;
        if lr::section_reset(ui, "Korn & Lichthof", true, grain_changed, |ui| {
            lr::group_label(ui, "Format");
            lr::segmented(ui, &mut f.format_mm, &[(35.0, "35 mm"), (60.0, "6×6"), (120.0, "4×5")]);
            slider(ui, "Korn", &mut f.grain, 0.0..=2.0, 1.0, 0.01);
            slider(ui, "Korngröße", &mut f.grain_size, 0.5..=3.0, 1.0, 0.01);
            // grain model: always visible (no layout jump), greyed out without grain
            ui.add_enabled_ui(f.grain > 0.0, |ui| {
                let mut model = film::grain_model(f);
                let tips = ["Korngröße überall gleich, wie im Filmmodell spektrafilm", "Korn wie bei echtem Film: gröber in den Schatten, feiner in den Lichtern (an Filmscans gemessen)"];
                if lr::choice_tips(ui, "Korngröße je Helligkeit", &mut model, &[(film::GRAIN_EVEN, "Gleichmäßig"), (film::GRAIN_FILM, "Wie Film")], &tips, "") {
                    f.grain_model = model.to_string();
                }
            });
            slider(ui, "Lichthof", &mut f.halation, 0.0..=3.0, 1.0, 0.01);
        }) {
            f.grain = d.grain;
            f.grain_size = d.grain_size;
            f.grain_model = d.grain_model.clone();
            f.halation = d.halation;
            f.format_mm = d.format_mm;
        }
        let tint_changed = f.green_tune != d.green_tune || f.blue_tune != d.blue_tune;
        if lr::section_reset(ui, "Farbton", true, tint_changed, |ui| {
            ui.horizontal(|ui| {
                lr::group_label(ui, "Grün und Blau");
                crate::help::info(ui, "film-farbton", "Farbton", &crate::help::FILM_HUE);
            });
            slider(ui, "Grün", &mut f.green_tune, -1.0..=1.0, 0.0, 0.05);
            slider(ui, "Blau", &mut f.blue_tune, -1.0..=1.0, 0.0, 0.05);
        }) {
            f.green_tune = d.green_tune;
            f.blue_tune = d.blue_tune;
        }
        if lr::section_reset(ui, "Aufnahmefilter", true, !f.taking_filter.is_empty(), |ui| {
            if lr::list_item(ui, crate::tr("Standard"), f.taking_filter.is_empty()).clicked() {
                f.taking_filter.clear();
            }
            if lr::list_item(ui, crate::tr("Keiner"), f.taking_filter == "none").clicked() {
                f.taking_filter = "none".to_string();
            }
            for fi in film::filter_list() {
                if lr::list_item(ui, fi.name, f.taking_filter == fi.id).clicked() {
                    f.taking_filter = fi.id.to_string();
                }
            }
            if !f.taking_filter.is_empty() && f.taking_filter != "none" {
                slider(ui, "Filterstärke", &mut f.taking_filter_amount, 0.0..=1.0, 1.0, 0.01);
            }
        }) {
            f.taking_filter.clear();
            f.taking_filter_amount = d.taking_filter_amount;
        }
    }

    /// Right side "Paper": paper shelf with characteristic curves, grade, tone, matte, contrast.
    pub(crate) fn darkroom_right(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let s = self.entries[index].settings.clone();
        let film_info = s.film.is_active().then(|| film::stock(&s.film.stock).cloned()).flatten();
        let mut pick: Option<Option<String>> = None;
        let mut info: Option<String> = None;
        ui.add_space(4.0);
        column_title(ui, crate::tr("ABZUG"));
        ui.add_space(2.0);
        ui.separator();
        egui::ScrollArea::vertical().id_salt("dk-abzug").auto_shrink([false, false]).show(ui, |ui| crate::sticky::scope(ui, |ui| {
            let paper_info = film_info.as_ref().filter(|fi| !fi.positive && !s.film.negative);
            let Some(fi) = paper_info else {
                lr::section(ui, "Papier", true, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(no_paper_text(&s.film)).color(lr::TEXT_DIM())).wrap());
                });
                self.darkroom_right_rest(ui, index);
                return;
            };
            let effective = s.film.paper.clone().or_else(|| fi.default_paper.clone());
            lr::section(ui, "Papier", true, |ui| {
                if ui.button(crate::tr("ALLE PAPIERE ZEIGEN")).clicked() {
                    self.contact_view(Some(false));
                }
                ui.add_space(4.0);
                let mut papers: Vec<&film::StockInfo> = film::papers().iter().filter(|p| p.monochrome == fi.monochrome || (fi.monochrome && p.category == Category::Paper)).collect();
                papers.sort_by_key(|p| !p.monochrome);
                let mut color_group = false;
                for p in papers {
                    if fi.monochrome && !p.monochrome && !color_group {
                        color_group = true;
                        lr::group_label(ui, "Farbpapier (getönt)");
                    }
                    let map = self.darkroom.fetch_card(&p.id);
                    let active = effective.as_deref() == Some(p.id.as_str());
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width() - LIST_SCROLL_GAP, 64.0), egui::Sense::click());
                    let pa = ui.painter();
                    list_row_background(pa, r, active, resp.hovered());
                    let curve = Rect::from_min_size(r.min + Vec2::new(5.0, 5.0), Vec2::new(86.0, 54.0));
                    if let Some(k) = &map {
                        let center = k.characteristic_curves.len() / 2;
                        draw(pa, curve, &k.characteristic_curves, (-1.5, 1.5), (0.0, 2.4), false, Some(center), false);
                    }
                    let tx = r.left() + 100.0;
                    let width = r.right() - tx - crate::darkroom_shelf::SIGN_W;
                    let mut job = egui::text::LayoutJob::simple(paper_short_name(&p.name), egui::TextStyle::Body.resolve(ui.style()), if active { crate::lr::TEXT_STRONG() } else { lr::TEXT() }, width);
                    job.wrap.max_rows = 2;
                    let g = pa.layout_job(job);
                    let name_h = g.size().y;
                    pa.galley(Pos2::new(tx, r.top() + 5.0), g, crate::lr::TEXT_STRONG());
                    // characterisation below the name, up to two lines (fits the row also below a two-line name)
                    let sub = self.darkroom.subtitle(p);
                    let mut job = egui::text::LayoutJob::simple(sub, egui::FontId::proportional(10.0), lr::TEXT_DIM(), width);
                    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
                    let g = pa.layout_job(job);
                    pa.galley(Pos2::new(tx, r.top() + 6.0 + name_h), g, lr::TEXT_DIM());
                    let q = crate::darkroom_shelf::sign_button(ui, r, ui.scope_id().with(("dk-papier-info", &p.id)), crate::darkroom_shelf::format_of(p), crate::darkroom_shelf::color_of(p));
                    if q {
                        info = Some(p.id.clone());
                    } else if resp.clicked() {
                        // the film's default paper stays "Default" (follows the film)
                        pick = Some((fi.default_paper.as_deref() != Some(p.id.as_str())).then(|| p.id.clone()));
                    }
                }
            });
            self.darkroom_paper_sliders(ui, index, fi);
            self.darkroom_right_rest(ui, index);
            ui.add_space(12.0);
        }));
        if let Some(w) = pick {
            if let Some(s) = self.darkroom_settings() {
                s.film.paper = w;
                s.film.paper_grade = -1.0;
            }
        }
        if info.is_some() {
            self.darkroom.map = info;
        }
    }

    /// Paper sliders: grade, contrast, tone, matte.
    fn darkroom_paper_sliders(&mut self, ui: &mut egui::Ui, index: usize, fi: &film::StockInfo) {
        let d = FilmSettings::default();
        let f = &mut self.entries[index].settings.film;
        let paper_id = f.paper.clone().or_else(|| fi.default_paper.clone());
        let paper = paper_id.as_deref().and_then(film::stock).cloned();
        let color_paper = fi.monochrome && paper.as_ref().is_some_and(|p| !p.monochrome);
        let changed = f.paper_grade != d.paper_grade || f.paper_gamma != d.paper_gamma || f.paper_tone != d.paper_tone || f.paper_matte != d.paper_matte || (color_paper && (f.print_warmth != d.print_warmth || f.print_tint != d.print_tint));
        if lr::section_reset(ui, "Grad und Papierton", true, changed, |ui| {
            if let Some(pi) = paper.as_ref().filter(|p| p.family.len() > 1) {
                {
                    lr::group_label(ui, "Grad");
                    let multigrade = pi.id.starts_with("ilford_multigrade");
                    let l: Vec<(f32, String)> = std::iter::once((-1.0, "Std".to_string()))
                        .chain(pi.family.iter().map(|&t| (t, if multigrade && t == 0.0 { "00".to_string() } else if multigrade && t == 0.5 { "0".to_string() } else { format!("{t}") })))
                        .collect();
                    // wraps so that long grade lists fit the column
                    ui.horizontal_wrapped(|ui| {
                        for (value, name) in &l {
                            if ui.selectable_label(f.paper_grade == *value, name.as_str()).clicked() {
                                f.paper_grade = *value;
                            }
                        }
                    });
                }
            }
            slider(ui, "Papierkontrast", &mut f.paper_gamma, 0.5..=1.5, 1.0, 0.01);
            if fi.monochrome {
                ui.add_enabled_ui(!color_paper, |ui| {
                    slider(ui, "Papierton", &mut f.paper_tone, -1.0..=1.0, 0.0, 0.01);
                });
                // Black & white film on color paper: tone via the enlarger's yellow/magenta filters
                ui.add_enabled_ui(color_paper, |ui| {
                    ui.horizontal(|ui| {
                        lr::group_label(ui, "Abzugsfarbe");
                        crate::help::info(ui, "film-abzugsfarbe", "Abzugsfarbe", &crate::help::FILM_PRINT_COLOR);
                    });
                    slider(ui, "Warm ↔ Kühl", &mut f.print_warmth, -2.0..=2.0, 0.0, 0.01);
                    slider(ui, "Grün ↔ Magenta", &mut f.print_tint, -2.0..=2.0, 0.0, 0.01);
                });
            }
            slider(ui, "Papier matt", &mut f.paper_matte, 0.0..=6.0, 0.0, 0.05);
        }) {
            f.paper_grade = d.paper_grade;
            f.paper_gamma = d.paper_gamma;
            f.paper_tone = d.paper_tone;
            f.paper_matte = d.paper_matte;
            if color_paper {
                f.print_warmth = d.print_warmth;
                f.print_tint = d.print_tint;
            }
        }
    }

    /// Right column below the paper: print with test strip, dodging, border.
    fn darkroom_right_rest(&mut self, ui: &mut egui::Ui, index: usize) {
        let d = FilmSettings::default();
        let color = self.entries[index].settings.film.is_active() && film::stock(&self.entries[index].settings.film.stock).is_some_and(|f| !f.monochrome && !f.positive);
        let f = &self.entries[index].settings.film;
        let changed = f.print_exposure != d.print_exposure || (color && f.print_warmth != d.print_warmth);
        if lr::section_reset(ui, "Abzug", true, changed, |ui| {
            let f = &mut self.entries[index].settings.film;
            let active = f.is_active();
            ui.add_enabled_ui(active, |ui| {
                slider(ui, "Abzughelligkeit", &mut f.print_exposure, -2.0..=2.0, 0.0, 0.05);
                if color {
                    slider(ui, "Abzug wärmer", &mut f.print_warmth, -1.0..=1.0, 0.0, 0.01);
                }
            });
            ui.add_space(6.0);
            self.darkroom_strip_draw(ui, index);
        }) {
            let f = &mut self.entries[index].settings.film;
            f.print_exposure = d.print_exposure;
            if color {
                f.print_warmth = d.print_warmth;
            }
        }
        lr::section(ui, "Abwedeln / Nachbelichten", true, |ui| self.darkroom_dodge_panel(ui, index));
        self.darkroom_border_sliders(ui, index);
    }

    /// Info card of a film or paper (dialog): character text, key data, characteristic curves, spectrum.
    pub(crate) fn darkroom_card(&mut self, ctx: &egui::Context) {
        let Some(id) = self.darkroom.map.clone() else { return };
        let Some(k) = self.darkroom.fetch_card(&id) else {
            self.darkroom.map = None;
            return;
        };
        // Which curve currently applies to this photo (development time or paper grade)?
        let active = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| &e.settings.film).and_then(|f| {
            let fam = film::stock(&id).map(|i| i.family.clone()).unwrap_or_default();
            if fam.is_empty() {
                return None;
            }
            let target = if k.is_paper {
                if f.paper_grade >= 0.0 { f.paper_grade } else if id.starts_with("ilford_multigrade") { 3.0 } else { fam[(fam.len() - 1) / 2] }
            } else if f.stock == id && f.film_dev > 0.0 {
                f.film_dev
            } else {
                fam[(fam.len() - 1) / 2]
            };
            fam.iter().enumerate().min_by(|a, b| (a.1 - target).abs().total_cmp(&(b.1 - target).abs())).map(|(i, _)| i)
        });
        let marker = &self.darkroom.marker;
        let shift = self.darkroom_marker_shift();
        const COLUMN_GAP: f32 = 40.0;
        const SECTION_GAP: f32 = 14.0;
        let modal_w = (ctx.content_rect().width() - 80.0).clamp(640.0, 1040.0);
        let left_w = ((modal_w - COLUMN_GAP) * 0.54).floor();
        let right_w = modal_w - COLUMN_GAP - left_w - 16.0;
        let max_h = (ctx.content_rect().height() - 180.0).max(240.0);
        let title = card_title(&k);
        let a = dialog::show(ctx, "dk-karte", &title, modal_w, |ui| {
            egui::ScrollArea::vertical().id_salt("dk-karte-inhalt").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(Vec2::new(left_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(left_w);
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.spacing_mut().item_spacing.y = 2.0;
                let mut sub = Vec::new();
                for t in [&k.maker, &k.type_name] {
                    if !t.is_empty() {
                        sub.push(t.clone());
                    }
                }
                if let Some(iso) = k.iso {
                    sub.push(format!("ISO {iso:.0}"));
                }
                if !sub.is_empty() {
                    card_text(ui, &sub.join(" · "), lr::TEXT_DIM(), 11.5);
                }
                if !k.text.is_empty() {
                    ui.add_space(6.0);
                    ui.scope(|ui| {
                        ui.set_max_width(ui.available_width().min(CARD_MEASURE));
                        card_text(ui, &k.text, crate::lr::gray(0xe0), 12.5);
                    });
                }
                ui.add_space(SECTION_GAP);

                // Data block
                card_heading(ui, "Daten");
                let mut rows: Vec<(String, String)> = Vec::new();
                if k.name != card_title(&k) {
                    rows.push((crate::tr("Name").to_string(), k.name.clone()));
                }
                let process = [k.process.as_str(), k.period.as_str()].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" · ");
                if !process.is_empty() {
                    rows.push((crate::tr("Prozess").to_string(), process));
                }
                if let Some(rms) = k.rms {
                    rows.push((crate::tr("Körnigkeit (RMS)").to_string(), crate::trf!("{rms} (Datenblatt, Dichte 1,0, Blende 48 µm)", rms = format!("{:.0}", rms))));
                } else if !k.is_paper {
                    rows.push((crate::tr("Körnigkeit (RMS)").to_string(), crate::tr("im Datenblatt nicht angegeben").to_string()));
                }
                rows.push((crate::tr("Höchste Dichte").to_string(), decimal_comma(k.dmax, 1)));
                if !k.spectral_note.is_empty() {
                    rows.push((crate::tr("Spektral").to_string(), k.spectral_note.clone()));
                }
                if !k.additional.is_empty() {
                    rows.push((crate::tr("Angaben").to_string(), k.additional.clone()));
                }
                rows.push((crate::tr("Profil").to_string(), if k.custom { crate::tr("eigen (aus Datenblättern)").to_string() } else { crate::tr("spektrafilm").to_string() }));
                if !k.source.is_empty() {
                    rows.push((crate::tr("Quelle").to_string(), crate::trf!("{a} (Herstellerdatenblatt)", a = k.source)));
                }
                card_rows(ui, &rows);

                if !k.suitable.is_empty() || !k.less_suitable.is_empty() {
                    ui.add_space(SECTION_GAP);
                    let half = ((left_w - 20.0) / 2.0).floor();
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 20.0;
                        let top = ui.cursor().top();
                        let mid = ui.cursor().left() + half + 10.0;
                        ui.painter().line_segment([Pos2::new(mid, top + 4.0), Pos2::new(mid, top + 4.0 + 18.0 * (k.suitable.len().max(k.less_suitable.len()) as f32 + 1.4))], Stroke::new(1.0, CARD_LINE()));
                        for (title, list) in [("Gut für", &k.suitable), ("Weniger geeignet", &k.less_suitable)] {
                            ui.allocate_ui_with_layout(Vec2::new(half, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                                ui.set_width(half);
                                ui.spacing_mut().item_spacing.y = 2.0;
                                card_list(ui, title, list);
                            });
                        }
                    });
                }
                if !k.tips.is_empty() {
                    ui.add_space(SECTION_GAP);
                    card_list(ui, "Tipps für graineo", &k.tips);
                }
                if !k.facts.is_empty() {
                    ui.add_space(SECTION_GAP);
                    let facts: Vec<String> = k.facts.iter().map(|(t, _)| t.clone()).collect();
                    card_list(ui, "Wissenswertes", &facts);
                }
                let mut sources: Vec<&str> = Vec::new();
                for (_, q) in &k.facts {
                    if !q.is_empty() && !sources.contains(&q.as_str()) {
                        sources.push(q);
                    }
                }
                if !sources.is_empty() {
                    ui.add_space(SECTION_GAP);
                    card_heading(ui, "Quellen");
                    for q in sources {
                        card_text(ui, q, lr::TEXT_DIM(), 10.0);
                    }
                }
            });
            ui.add_space(COLUMN_GAP);
            ui.allocate_ui_with_layout(Vec2::new(right_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(right_w);
                ui.spacing_mut().item_spacing.x = 6.0;
            card_heading(ui, if k.is_paper { "Kennlinie je Papiergrad" } else { "Kennlinie je Entwicklungszeit" });
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 170.0), egui::Sense::hover());
            draw(ui.painter(), r, &k.characteristic_curves, (-3.0, 3.0), (0.0, (k.dmax * 1.05).max(1.0).min(4.0).ceil().max(2.0)), true, active, false);
            if !k.is_paper {
                // Live markers: distribution of the photo on the characteristic curve
                marker.draw(ui.painter(), r.shrink2(Vec2::new(0.0, 0.0)), (-3.0, 3.0), shift);
            }
            ui.horizontal_wrapped(|ui| {
                for (i, rh) in k.characteristic_curves.iter().enumerate() {
                    let c = curve_color(i, k.characteristic_curves.len(), Some(i) == active, false);
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, c);
                    ui.label(egui::RichText::new(&rh.name).small().color(if Some(i) == active { crate::lr::TEXT_STRONG() } else { lr::TEXT_DIM() }));
                    ui.add_space(6.0);
                }
            });
            ui.label(egui::RichText::new(crate::tr("Belichtung (log, 0 = Mittelgrau) → Dichte")).small().color(lr::TEXT_DIM()));
            if !k.is_paper && marker.filled() > 0 {
                ui.label(egui::RichText::new(crate::tr("Balken: wie viel vom Foto bei dieser Belichtung auf den Film fällt")).small().color(lr::ACCENT()));
            }
            ui.add_space(8.0);
            if !k.spectrum.is_empty() {
                card_heading(ui, "Spektralempfindlichkeit");
                let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 130.0), egui::Sense::hover());
                let (from, to) = spectrum_range(&k.spectrum);
                let lowest = k.spectrum.iter().flat_map(|r| r.points.iter().map(|p| p.1)).fold(0.0f32, f32::min);
                draw(ui.painter(), r, &k.spectrum, (from, to), ((lowest - 0.5).floor().max(-4.0), 0.0), true, None, true);
                if k.spectrum.len() > 1 {
                    ui.horizontal(|ui| {
                        for (i, rh) in k.spectrum.iter().enumerate() {
                            let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_filled(dot.center(), 3.5, curve_color(i, k.spectrum.len(), false, true));
                            ui.label(egui::RichText::new(&rh.name).small().color(lr::TEXT_DIM()));
                            ui.add_space(6.0);
                        }
                    });
                }
                ui.label(egui::RichText::new(crate::tr("Wellenlänge (nm) → relative Empfindlichkeit (log)")).small().color(lr::TEXT_DIM()));
            }
            });
            }));
        }, dialog::Buttons::close());
        if a != dialog::Answer::Open {
            self.darkroom.map = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names() {
        assert_eq!(short_name("Ilford HP5 Plus Black and White Negative Film, ILFOTEC HC"), "Ilford HP5 Plus");
        assert_eq!(short_name("Kodak T-MAX 400 Professional Negative Film (TMY), T-MAX developer"), "Kodak T-MAX 400");
        assert_eq!(paper_short_name("Ilford Multigrade FB Classic (Barytpapier, glänzend), Kontrastgrade 00–5"), "Ilford Multigrade FB Classic");
        assert_eq!(paper_short_name("Kodak Print Film 2302"), "Kodak Print Film 2302");
    }

    #[test]
    fn spectrum_range_rounds_to_50() {
        let r = vec![Series { name: "x".into(), points: vec![(385.0, -1.0), (655.0, -2.0)] }];
        assert_eq!(spectrum_range(&r), (350.0, 700.0));
        assert_eq!(spectrum_range(&[]), (380.0, 700.0));
    }

    #[test]
    fn curve_colors_distinguish_active() {
        assert_ne!(curve_color(1, 4, true, false), curve_color(1, 4, false, false));
        assert_ne!(curve_color(0, 3, false, true), curve_color(2, 3, false, true));
    }

    #[test]
    fn card_cache_returns_same_card() {
        let mut z = State::default();
        let a = z.fetch_card("fujifilm_acros_100_ii");
        let b = z.fetch_card("fujifilm_acros_100_ii");
        assert_eq!(a.is_some(), b.is_some());
        if let Some(k) = a {
            assert_eq!(k.iso, Some(100.0));
            // no missing values (zeros) in the spectrum
            assert!(k.spectrum[0].points.iter().all(|p| p.1 < 0.0 || p.1 == 0.0));
            let (from, to) = spectrum_range(&k.spectrum);
            assert!(from >= 350.0 && to <= 750.0, "{from}–{to}");
        }
        assert!(z.fetch_card("gibt-es-nicht").is_none());
    }

    #[test]
    fn plot_and_row_draw_without_panic() {
        let ctx = egui::Context::default();
        let series = vec![
            Series { name: "a".into(), points: (0..40).map(|i| (-3.0 + i as f32 * 0.15, (i as f32 * 0.07).min(2.0))).collect() },
            Series { name: "b".into(), points: (0..40).map(|i| (-3.0 + i as f32 * 0.15, (i as f32 * 0.05).min(1.8))).collect() },
        ];
        let mut click = (false, false, false, false);
        let output = crate::ui_test::run(&ctx, Default::default(), |ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(300.0, 120.0), egui::Sense::hover());
            draw(ui.painter(), r, &series, (-3.0, 3.0), (0.0, 2.0), true, Some(1), false);
            let (r2, _) = ui.allocate_exact_size(Vec2::new(80.0, 50.0), egui::Sense::hover());
            draw(ui.painter(), r2, &series, (-1.5, 1.5), (0.0, 2.4), false, None, false);
            click = shelf_row(ui, None, "test", None, false, "Testfilm mit sehr langem Namen, der abgeschnitten werden muss", "Schwarzweiß · ISO 100", true);
        });
        assert!(!output.shapes.is_empty());
        // without a mouse click neither row nor "?" is triggered
        assert!(!click.0 && !click.1);
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use eframe::App as _;

    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    fn key_event(key: egui::Key, mods: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: mods }
    }

    /// Whole window: enter the darkroom, toggles, select a film, leave.
    #[test]
    fn darkroom_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-dunkelkammer-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbImage::from_pixel(1800, 1200, image::Rgb([110, 80, 60])).save(folder.join("sample.png")).unwrap();
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
            let mut input = egui::RawInput {
                time: Some(clock),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))),
                events,
                ..Default::default()
            };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        frame(&mut a, vec![]);
        frame(&mut a, vec![]);
        assert!(!a.darkroom_active(), "without a call it stays in Develop");

        // enter: side panels left and right, image narrower
        a.enter_darkroom();
        frame(&mut a, vec![]);
        frame(&mut a, vec![]);
        assert!(a.darkroom_active() && a.tab == Tab::Develop);
        let wide = a.image_rect.expect("image in the darkroom").width();
        assert!(wide <= 1400.0 - 2.0 * crate::darkroom_layout::column_width(1400.0) + 1.0, "image width {wide}");
        assert!(wide > 100.0, "image must still be visible: {wide}");

        // the filmstrip is always there (F6 hides it as in Develop); Tab: image only
        assert!(a.show_filmstrip);
        frame(&mut a, vec![key_event(egui::Key::Tab, egui::Modifiers::NONE)]);
        frame(&mut a, vec![press(egui::Key::Tab)]);
        assert!(a.darkroom.image_only, "Tab: image only");
        let wide_only = a.image_rect.expect("image").width();
        assert!(wide_only > wide + 200.0, "without side panels the image is wider: {wide_only} vs {wide}");
        frame(&mut a, vec![key_event(egui::Key::Tab, egui::Modifiers::NONE)]);
        frame(&mut a, vec![press(egui::Key::Tab)]);
        assert!(!a.darkroom.image_only);

        // select a film: the same settings as in "Develop"
        if !develop::film::films().is_empty() {
            a.darkroom.test_film("kodak_trix_400");
            frame(&mut a, vec![]);
            frame(&mut a, vec![]);
            let i = a.current.as_ref().unwrap().index;
            let f = &a.entries[i].settings.film;
            assert!(f.is_active() && f.stock == "kodak_trix_400");
        }

        // ⇧D leaves, ⇧D enters again, Esc leaves without jumping to the grid
        frame(&mut a, vec![key_event(egui::Key::D, egui::Modifiers::SHIFT)]);
        assert!(!a.darkroom.active, "⇧D leaves the darkroom");
        frame(&mut a, vec![press(egui::Key::D)]);
        frame(&mut a, vec![key_event(egui::Key::D, egui::Modifiers::SHIFT)]);
        assert!(a.darkroom_active(), "⇧D opens the darkroom");
        frame(&mut a, vec![press(egui::Key::D)]);
        frame(&mut a, vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)]);
        assert!(!a.darkroom.active && !a.in_grid(), "Esc goes back to Develop, not to the grid");
        // to the grid: the darkroom has ended
        a.enter_darkroom();
        a.to_grid();
        assert!(!a.darkroom.active);
    }

    /// Compare in the whole window: without film, A | B with remember, swap, keep and restore.
    #[test]
    fn compare_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        if develop::film::films().is_empty() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("graineo-dk-vergleich-{}", std::process::id()));
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
        let mut frame = |a: &mut App| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        let wait_until = |a: &mut App, frame: &mut dyn FnMut(&mut App), to: &dyn Fn(&App) -> bool| {
            let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
            while !to(a) {
                frame(a);
                a.handle_results(&ctx);
                a.schedule(0.0);
                assert!(std::time::Instant::now() < deadline, "timeout waiting for the comparison");
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        };
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame(&mut a);
        frame(&mut a);
        let i = a.current.as_ref().unwrap().index;
        assert!(a.entries[i].settings.film.is_active());

        // without film: the left side is rendered, the split line is in the middle
        a.darkroom.compare.without_film();
        wait_until(&mut a, &mut frame, &|a| {
            let s = &a.entries[i].settings;
            a.darkroom.compare.left_settings(s).is_some_and(|l| a.darkroom.compare.ready(&l))
        });
        assert_eq!(a.darkroom.compare.split, 0.5);

        // A | B: remember A, select another film, the left side follows A
        let before = a.entries[i].settings.clone();
        let s = before.clone();
        a.darkroom.compare.remember(&s);
        a.entries[i].settings.film.stock = "ilford_hp5_plus".into();
        wait_until(&mut a, &mut frame, &|a| {
            let s = &a.entries[i].settings;
            a.darkroom.compare.left_settings(s).is_some_and(|l| l.film.stock == "kodak_trix_400" && a.darkroom.compare.ready(&l))
        });
        // swap: B becomes Trix, A becomes HP5
        let mut s = a.entries[i].settings.clone();
        a.darkroom.compare.swap(&mut s);
        a.entries[i].settings = s;
        assert_eq!(a.entries[i].settings.film.stock, "kodak_trix_400");
        assert_eq!(a.darkroom.compare.a.as_ref().unwrap().stock, "ilford_hp5_plus");
        // Compare off: state from the start of the comparison
        a.entries[i].settings.film.stock = "agfa_apx_100".into();
        let mut s = a.entries[i].settings.clone();
        a.darkroom.compare.switch_off(&mut s);
        a.entries[i].settings = s;
        assert_eq!(a.entries[i].settings.film.stock, "kodak_trix_400");
        assert!(!a.darkroom.compare.active());
        frame(&mut a);
    }

    /// Test strip in the whole window: fields are rendered, the clicked value lands in the settings.
    #[test]
    fn test_strip_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        if develop::film::films().is_empty() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("graineo-dk-streifen-{}", std::process::id()));
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
        let mut frame = |a: &mut App| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame(&mut a);
        frame(&mut a);
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.darkroom.probe.fields() < 7 {
            frame(&mut a);
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline, "test strip did not finish");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert_eq!(a.darkroom.probe.fields(), 7);
        // Grade row: the default paper (Multigrade) has seven grades
        a.darkroom.probe.kind = crate::darkroom_strip::Kind::Grad;
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        loop {
            frame(&mut a);
            a.handle_results(&ctx);
            a.schedule(0.0);
            if a.darkroom.probe.fields() == 7 && a.darkroom.probe.kind == crate::darkroom_strip::Kind::Grad {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "grade strip did not finish");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        // Paper row: the photo on every black & white paper
        let n_papers = develop::film::papers().iter().filter(|p| p.monochrome).count();
        a.darkroom.probe.kind = crate::darkroom_strip::Kind::Paper;
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        loop {
            frame(&mut a);
            a.handle_results(&ctx);
            a.schedule(0.0);
            if a.darkroom.probe.fields() == n_papers && a.darkroom.probe.kind == crate::darkroom_strip::Kind::Paper {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "paper strip did not finish");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    /// Zone display in the whole window: after switching it on, the distribution of the finished image is available.
    #[test]
    fn zones_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let folder = std::env::temp_dir().join(format!("graineo-dk-zonen-{}", std::process::id()));
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
        let mut frame = |a: &mut App| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        a.enter_darkroom();
        frame(&mut a);
        assert!(!a.darkroom.zones.an);
        a.darkroom.zones.an = true;
        a.rendered_for = None;
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.darkroom.zones.distribution().iter().sum::<f32>() < 99.0 {
            frame(&mut a);
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline, "no zone distribution");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        frame(&mut a);
    }

    /// Live markers: with a film's info card open, the distribution of the photo is computed.
    #[test]
    fn live_marker_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        if develop::film::films().is_empty() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("graineo-dk-marker-{}", std::process::id()));
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
        let mut frame = |a: &mut App| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1400.0, 1000.0))), ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        a.enter_darkroom();
        frame(&mut a);
        a.darkroom.test_card("kodak_trix_400");
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        while a.darkroom.marker.filled() == 0 {
            frame(&mut a);
            a.handle_results(&ctx);
            a.schedule(0.0);
            assert!(std::time::Instant::now() < deadline, "no scene distribution");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        frame(&mut a);
    }
}
