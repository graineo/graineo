//! Film/paper simulation and color looks have separate selection areas.
//! Settings and computation stay the same as before this UI split.
use super::{App, Job, Settings, app_file, film_profiles_ui, help, lr, mode_label, raw_app};
use eframe::egui::{self, Color32, TextureHandle};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Area {
    #[default]
    Film,
    Look,
}
#[derive(Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct State {
    version: u32,
    #[serde(rename = "area")]
    pub area: Area,
    #[serde(rename = "openLookPackages")]
    open_packages: HashSet<String>,
    #[serde(skip)]
    search: String,
    #[serde(skip)]
    save_allowed: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            area: Area::Film,
            open_packages: HashSet::new(),
            search: String::new(),
            save_allowed: true,
        }
    }
}
impl State {
    pub fn load() -> Self {
        app_file("profile-browser.json").map_or_else(Self::default, |p| Self::from_file(&p))
    }
    fn from_file(p: &Path) -> Self {
        match std::fs::read(p) {
            Ok(b) => serde_json::from_slice::<Self>(&b)
                .ok()
                .filter(|s| s.version == 1)
                .unwrap_or(Self {
                    save_allowed: false,
                    ..Self::default()
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(_) => Self {
                save_allowed: false,
                ..Self::default()
            },
        }
    }
    fn save(&self) {
        if let Some(p) = app_file("profile-browser.json") {
            let _ = self.to_file(&p);
        }
    }
    fn to_file(&self, p: &Path) -> Result<(), String> {
        if !self.save_allowed {
            return Ok(());
        }
        write_atomic(p, &serde_json::to_vec(self).map_err(|e| e.to_string())?)
    }
}
fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(crate::tr("Auswahlzustand ist eine Verknüpfung").into());
    }
    let parent = p.parent().ok_or(crate::tr("Ordner fehlt"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let tmp = parent.join(format!("browser-state-{}-{stamp}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
pub fn load_favorites() -> HashSet<String> {
    match (app_file("favorites.json"), app_file("favoriten.txt")) {
        (Some(p), Some(old)) => favorites_from(&p, &old),
        _ => HashSet::new(),
    }
}
fn favorites_from(p: &Path, old: &Path) -> HashSet<String> {
    if let Some(v) = std::fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
    {
        return v.into_iter().collect();
    }
    std::fs::read_to_string(old)
        .ok()
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
fn save_favorites(v: &HashSet<String>) -> Result<(), String> {
    let p = app_file("favorites.json").ok_or(crate::tr("Favoritenordner nicht verfügbar"))?;
    favorites_to_file(&p, v)
}
fn favorites_to_file(p: &Path, v: &HashSet<String>) -> Result<(), String> {
    if p.exists() {
        let b = std::fs::read(p).map_err(|e| e.to_string())?;
        serde_json::from_slice::<Vec<String>>(&b)
            .map_err(|_| crate::tr("Defekte Favoriten bleiben unangetastet").to_string())?;
    }
    let mut v: Vec<_> = v.iter().collect();
    v.sort();
    write_atomic(p, &serde_json::to_vec(&v).map_err(|e| e.to_string())?)
}
#[derive(Clone)]
struct Entry {
    key: String,
    name: String,
    choice: Choice,
    available: bool,
}
/// A look or film from the browser, applied to the other settings of the photo.
#[derive(Clone)]
pub(crate) enum Choice {
    Film(String),
    Look(String),
    /// Sigma colour mode (empty: the camera's mode); switches black and white off
    Sigma(String),
    /// Sigma black and white (Foveon layers)
    SigmaMono,
    /// film look (recipe): the whole film part
    Recipe(develop::film::FilmSettings),
}
impl Choice {
    pub(crate) fn apply(&self, s: &Settings) -> Settings {
        let mut t = s.clone();
        match self {
            Self::Film(id) => {
                t.film.active = true;
                t.film.stock = id.clone();
                t.film.paper = None;
            }
            Self::Look(n) => {
                t.film.active = false;
                t.raw.look = n.clone();
                // linear scene rendering: start values without white balancing (D65 keeps the light of the scene),
                // without noise reduction and extra sharpening; every slider stays free afterwards
                if n == rawdev::learn::LINEAR_LOOK {
                    t.raw.wb_mode = "custom".into();
                    t.raw.temperature = 6504.0;
                    t.raw.tint = 0.0;
                    t.raw.nr_luma = 0.0;
                    t.raw.nr_color = 0.0;
                    t.sharpen = 0.0;
                }
            }
            Self::Sigma(m) => {
                t.film.active = false;
                t.color_mode = if m.is_empty() { None } else { Some(m.clone()) };
                t.monochrome = false;
                // linear scene rendering: daylight white balance (keeps the light of the scene), noise sliders off,
                // Foveon sharpening at its minimum (unsharpened), no extra sharpening
                if m == develop::settings::LINEAR_MODE {
                    t.white_balance = Some("Sunlight".into());
                    t.wb_mult = [1.0; 3];
                    t.wb_temp = 0.0;
                    t.wb_tint = 0.0;
                    t.nr_luma = develop::settings::NR_OFF;
                    t.nr_chroma = develop::settings::NR_OFF;
                    t.sigma_sharpness = -2.0;
                    t.sharpen = 0.0;
                }
            }
            Self::SigmaMono => {
                t.film.active = false;
                t.monochrome = true;
            }
            Self::Recipe(f) => t.film = f.clone(),
        }
        t
    }
}

/// Name of the film or look the settings show (as in the "Selected look" card).
pub(crate) fn current_look_name(s: &Settings, doc: Option<&rawdev::RawDoc>, camera_mode: &str) -> String {
    if s.film.is_active() {
        film_card(s).1
    } else {
        look_card(s, doc, camera_mode).1
    }
}
struct Group {
    name: String,
    entries: Vec<Entry>,
    package: bool,
}
fn film_groups() -> Vec<Group> {
    use develop::film::{self, Category};
    let mut groups = Vec::new();
    for cat in [
        Category::BlackWhite,
        Category::ColorNegative,
        Category::Slide,
        Category::Cine,
    ] {
        let mut films: Vec<_> = film::films().iter().filter(|f| f.category == cat).collect();
        films.sort_by_key(|f| f.name.to_lowercase());
        let entries = films
            .into_iter()
            .map(|f| Entry {
                key: f.id.clone(),
                name: f.name.clone(),
                choice: Choice::Film(f.id.clone()),
                available: true,
            })
            .collect::<Vec<_>>();
        if !entries.is_empty() {
            groups.push(Group {
                name: crate::trf!("Filme · {a}", a = crate::tr(cat.label())),
                entries,
                package: true,
            });
        }
    }
    groups
}
/// Tile of the linear scene rendering (RAW look or Sigma colour mode)?
fn is_linear_key(key: &str) -> bool {
    key == format!("look:{}", rawdev::learn::LINEAR_LOOK) || key == format!("sigma:{}", develop::settings::LINEAR_MODE)
}
/// Tile key of Sigma's black and white (apart from `sigma:<mode>`, the colour modes).
pub(crate) const SIGMA_MONO_KEY: &str = "sigma-mono";
fn look_groups(
    s: &Settings,
    doc: Option<&rawdev::RawDoc>,
    camera_mode: &str,
    modes: &[String],
) -> Vec<Group> {
    let mut groups = Vec::new();
    if let Some(doc) = doc {
        let profile = raw::film_profile::installed();
        let availability: HashMap<_, _> = profile
            .iter()
            .map(|p| {
                (
                    p.uuid.as_str(),
                    p.available && !raw::film_profile::load_failed(&p.uuid),
                )
            })
            .collect();
        let entries = raw_app::profile_entries(doc)
            .into_iter()
            .map(|(key, name, look)| Entry {
                key,
                name,
                choice: Choice::Look(look),
                available: true,
            })
            .collect();
        groups.push(Group {
            name: crate::tr("Kamera").into(),
            entries,
            package: false,
        });
        for (name, entries) in raw_app::film_profile_groups() {
            let entries = entries
                .into_iter()
                .map(|(key, name, look)| {
                    let available = look
                        .strip_prefix("film:")
                        .and_then(|id| availability.get(id))
                        .copied()
                        .unwrap_or(false);
                    Entry {
                        key,
                        name,
                        choice: Choice::Look(look),
                        available,
                    }
                })
                .collect();
            groups.push(Group {
                name,
                entries,
                package: true,
            });
        }
    } else {
        // Sigma: the camera's colour modes; a photo set to black and white shows that entry first (selected) and the
        // colour modes in colour (a click switches black and white off)
        let mono = s.monochrome.then(|| Entry {
            key: SIGMA_MONO_KEY.into(),
            name: crate::tr("Schwarzweiß").into(),
            choice: Choice::SigmaMono,
            available: true,
        });
        let entries = mono
            .into_iter()
            .chain(std::iter::once("").chain(modes.iter().map(String::as_str)).chain(std::iter::once(develop::settings::LINEAR_MODE)).map(|m| Entry {
                key: format!("sigma:{m}"),
                name: if m.is_empty() {
                    crate::trf!("{a} (Kamera)", a = mode_label(camera_mode))
                } else {
                    mode_label(m)
                },
                choice: Choice::Sigma(m.into()),
                available: true,
            }))
            .collect();
        groups.push(Group {
            name: crate::tr("Kamera").into(),
            entries,
            package: false,
        });
    }
    groups
}
fn film_card(s: &Settings) -> (String, String, String) {
    if !s.film.is_active() {
        return (String::new(), crate::tr("Aus").into(), String::new());
    }
    let f = develop::film::stock(&s.film.stock);
    let paper = f
        .filter(|f| !f.positive && !s.film.negative)
        .and_then(|f| s.film.paper.clone().or(f.default_paper.clone()))
        .and_then(|id| develop::film::stock(&id).map(|p| p.name.clone()));
    let sub = match (f, paper) {
        (Some(f), Some(p)) => format!("{} · {p}", f.category.label()),
        (Some(f), None) => f.category.label().into(),
        _ => String::new(),
    };
    (
        s.film.stock.clone(),
        f.map_or(s.film.stock.clone(), |f| f.name.clone()),
        sub,
    )
}
fn look_card(
    s: &Settings,
    doc: Option<&rawdev::RawDoc>,
    camera_mode: &str,
) -> (String, String, String) {
    let mut base = s.clone();
    base.film.active = false;
    if let Some(doc) = doc {
        return raw_app::profile_card(doc, &base);
    }
    if s.monochrome {
        return (
            SIGMA_MONO_KEY.into(),
            crate::tr("Schwarzweiß").into(),
            crate::tr("Foveon · Sensorschichten").into(),
        );
    }
    let m = s.color_mode.clone().unwrap_or_default();
    (
        format!("sigma:{m}"),
        if m.is_empty() {
            crate::trf!("{a} (Kamera)", a = mode_label(camera_mode))
        } else {
            mode_label(&m)
        },
        if s.is_linear() { crate::tr("Ohne Kurve, Look, Schärfen, Entrauschen") } else { crate::tr("Farbmodus der Kamera") }.into(),
    )
}
/// Head of "Film & Look": thumbnail, name (up to two lines), subline and an optional dimmed note –
/// pure information without a frame. On the right, top-aligned, the symbol buttons darkroom and look
/// comparison. Fixed height (one row more only with a note), so nothing below jumps.
/// Returns: (darkroom clicked, comparison clicked).
fn look_header(
    ui: &mut egui::Ui,
    presentation: &(String, String, String),
    note: Option<&str>,
    thumbs: &HashMap<String, TextureHandle>,
    compare_on: bool,
) -> (bool, bool) {
    const THUMB: egui::Vec2 = egui::vec2(60., 40.);
    const BUTTONS_W: f32 = 26. * 2. + 4.;
    let (key, title, sub) = presentation;
    ui.add_space(4.);
    // fixed height: room for two name rows and the subline (plus the note), whatever the name length
    let row = |font: egui::FontId| ui.painter().layout_no_wrap("Ag".into(), font, crate::lr::TEXT_STRONG()).size().y;
    let small_row = row(egui::FontId::proportional(10.));
    let text_h = 2. * row(lr::semibold(lr::UI_SIZE)) + 2. + small_row + if note.is_some() { small_row + 1. } else { 0. };
    let height = text_h.max(THUMB.y);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let p = ui.painter();
    let img = egui::Rect::from_min_size(rect.min, THUMB);
    p.rect_filled(img, 3., lr::PANEL_DARK());
    if let Some(t) = thumbs.get(key) {
        let sz = t.size_vec2();
        let k = (img.width() / sz.x).min(img.height() / sz.y);
        p.image(
            t.id(),
            egui::Rect::from_center_size(img.center(), sz * k),
            egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.)),
            egui::Color32::WHITE,
        );
    } else {
        p.text(img.center(), egui::Align2::CENTER_CENTER, if key.is_empty() { "—" } else { "…" }, lr::semibold(lr::UI_SIZE), lr::TEXT_DIM());
    }
    let x = img.right() + 10.;
    let text_w = (rect.right() - BUTTONS_W - 8. - x).max(40.);
    let line = |text: String, font: egui::FontId, color: Color32, rows: usize| {
        let mut job = egui::text::LayoutJob::simple(text, font, color, text_w);
        job.wrap.max_rows = rows;
        ui.painter().layout_job(job)
    };
    let name = line(title.clone(), lr::semibold(lr::UI_SIZE), crate::lr::TEXT_STRONG(), 2);
    let mut y = rect.top();
    let (name_h, name_rows) = (name.size().y, name.rows.len());
    p.galley(egui::pos2(x, y), name, crate::lr::TEXT_STRONG());
    y += name_h + 2.;
    // name and subline share three rows: a short name leaves the subline room for a second row
    let sub_rows = 3usize.saturating_sub(name_rows).max(1);
    for (text, rows) in [(sub.replace("Kodak Professional ", "Kodak "), sub_rows), (note.unwrap_or_default().to_string(), 1)] {
        if text.is_empty() {
            continue;
        }
        let g = line(text, egui::FontId::proportional(10.), lr::TEXT_DIM(), rows);
        let h = g.size().y;
        p.galley(egui::pos2(x, y), g, lr::TEXT_DIM());
        y += h + 1.;
    }
    // symbol buttons: comparison on the far right (fixed place), darkroom to its left
    let compare_rect = egui::Rect::from_min_size(egui::pos2(rect.right() - lr::DRAWN_ICON_BUTTON.x, rect.top()), lr::DRAWN_ICON_BUTTON);
    let darkroom_rect = compare_rect.translate(egui::vec2(-(lr::DRAWN_ICON_BUTTON.x + 4.), 0.));
    let darkroom = lr::drawn_icon_button_at(ui, darkroom_rect, "film-look-darkroom", lr::DrawnIcon::Enlarger, false, crate::tr("In der Dunkelkammer bearbeiten"));
    let compare = lr::drawn_icon_button_at(ui, compare_rect, "film-look-compare", lr::DrawnIcon::TwoFields, compare_on, crate::tr("Looks vergleichen (⌘-Klick auf Kacheln)"));
    ui.add_space(6.);
    (darkroom, compare)
}
fn package_header(ui: &mut egui::Ui, name: &str, count: usize, open: bool, available: bool) -> bool {
    let (rect, r) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.), egui::Sense::click());
    package_header_paint(ui.painter(), rect, name, count, open, available, r.hovered(), r.has_focus());
    // sticks to the top while the packs scroll; a click on that copy counts like a click here
    let id = ui.id().with(("paket", name));
    let owned = name.to_string();
    crate::sticky::register(crate::sticky::RANK_CATEGORY, id, rect, move |p, bar, hovered| {
        package_header_paint(p, bar, &owned, count, open, available, hovered, false);
    });
    r.clicked() || crate::sticky::take_click(ui, id)
}
#[allow(clippy::too_many_arguments)]
fn package_header_paint(p: &egui::Painter, rect: egui::Rect, name: &str, count: usize, open: bool, available: bool, hovered: bool, focus: bool) {
    p.rect_filled(
        rect,
        3.,
        crate::lr::gray(if hovered { 0x26 } else { 0x1c }),
    );
    if focus {
        p.rect_stroke(
            rect,
            3.,
            egui::Stroke::new(1., lr::TEXT()),
            egui::StrokeKind::Inside,
        );
    }
    let color = if available {
        lr::TEXT()
    } else {
        crate::lr::gray(0x68)
    };
    let clip = p.with_clip_rect(egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.right() - 35., rect.bottom()),
    ));
    let g = lr::caps(
        &clip,
        &format!("{} {name}", if open { "▾" } else { "▸" }),
        lr::semibold(10.5),
        color,
    );
    clip.galley(rect.min + egui::vec2(4., 5.), g, color);
    p.text(
        rect.right_center() - egui::vec2(5., 0.),
        egui::Align2::RIGHT_CENTER,
        count.to_string(),
        lr::semibold(10.5),
        color,
    );
}
#[derive(Default)]
struct LookAction {
    chosen: Option<Settings>,
    /// clicked tile (key, name, choice) – for the look comparison
    picked: Option<(String, String, Choice)>,
    hover: Option<Settings>,
    star_toggle: Option<String>,
    hide: Option<String>,
    state_dirty: bool,
}
fn group_ui(
    ui: &mut egui::Ui,
    state: &mut State,
    g: &Group,
    current: &str,
    basis: &Settings,
    thumbs: &HashMap<String, TextureHandle>,
    favs: &HashSet<String>,
    requested: &mut Vec<String>,
    numbers: &dyn Fn(&str) -> Option<usize>,
) -> LookAction {
    let mut action = LookAction::default();
    let query = state.search.trim().to_lowercase();
    let items: Vec<_> = g
        .entries
        .iter()
        .filter(|e| {
            query.is_empty()
                || format!("{} {}", g.name, e.name)
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();
    if items.is_empty() {
        return action;
    }
    ui.add_space(4.);
    let mut open = !g.package || !query.is_empty() || state.open_packages.contains(&g.name);
    if g.package {
        if package_header(
            ui,
            &g.name,
            items.len(),
            open,
            items.iter().any(|e| e.available),
        ) && query.is_empty()
        {
            if open {
                state.open_packages.remove(&g.name);
            } else {
                state.open_packages.insert(g.name.clone());
            }
            open = !open;
            action.state_dirty = true;
        }
    } else {
        lr::category(ui, &g.name);
    }
    if !open {
        return action;
    }
    let list: Vec<_> = items
        .iter()
        .map(|e| (thumbs.get(&e.key).cloned(), e.name.as_str()))
        .collect();
    let stars: Vec<_> = items.iter().map(|e| favs.contains(&e.key)).collect();
    let availability: Vec<_> = items.iter().map(|e| e.available).collect();
    let selected = items.iter().position(|e| e.key == current);
    let r = lr::thumb_grid_fav_available(
        ui,
        &crate::trf!("profil-{a}", a = g.name),
        &list,
        selected,
        3,
        &stars,
        items.iter().all(|e| e.key.starts_with("look:film:")),
        &availability,
    );
    requested.extend(
        r.visible
            .iter()
            .filter(|&&i| items[i].available)
            .map(|&i| items[i].key.clone()),
    );
    for (i, rect) in &r.rects {
        if let Some(n) = numbers(&items[*i].key) {
            crate::compare_view::pick_mark(ui.painter(), rect.min, n);
        }
    }
    // Tile tips hang on the whole tile (image and name), so they sit below the name instead of covering it.
    for (i, cell) in &r.cells {
        if is_linear_key(&items[*i].key) {
            let hover = ui.interact(*cell, ui.id().with(("linear-tip", &items[*i].key)), egui::Sense::hover());
            lr::hover_tip(ui, &hover, "Sensordaten ohne Kurve, Look, Schärfen, Entrauschen – flacher Ausgangspunkt");
        }
    }
    if let Some(i) = r.clicked {
        action.chosen = Some(items[i].choice.apply(basis));
        action.picked = Some((items[i].key.clone(), items[i].name.clone(), items[i].choice.clone()));
    }
    if let Some(i) = r.hovered {
        action.hover = Some(items[i].choice.apply(basis));
    }
    if let Some(i) = r.star {
        action.star_toggle = Some(items[i].key.clone());
    }
    if let Some(i) = r.hide {
        action.hide = items[i].key.strip_prefix("look:film:").map(str::to_string);
    }
    action
}
impl App {
    /// A row in "Edit": current film or look; a click opens the "Film & Look" tool.
    pub(super) fn profile_row(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &self.current else { return };
        let s = self.entries[c.index].settings.clone();
        let (foreign, camera_mode) = (c.loaded.previews.foreign.clone(), c.loaded.camera_mode.clone());
        let name = if s.film.is_active() { crate::trf!("Film · {a}", a = film_card(&s).1) } else { crate::trf!("Look · {a}", a = look_card(&s, foreign.as_deref(), &camera_mode).1) };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
        let p = ui.painter();
        p.rect_filled(rect, 0.0, if resp.hovered() { crate::lr::gray(0x2a) } else { crate::lr::gray(0x1e) });
        let g = lr::caps(p, crate::tr("Film & Look"), egui::FontId::proportional(10.0), lr::TEXT_DIM());
        p.galley(egui::pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0), g, lr::TEXT_DIM());
        // mark "Kamera-Standard" when the photo's camera has own start values (camera_defaults.rs), tooltip with the values
        let default_entry = crate::camera_defaults::camera_of(&c.loaded).and_then(|k| crate::camera_defaults::find(&k));
        let mut name_right = rect.right() - 26.0;
        if let Some(e) = &default_entry {
            // compact mark: camera icon in a thin frame (the look name keeps its room)
            let mark = egui::Rect::from_center_size(egui::pos2(rect.right() - 40.0, rect.center().y), egui::vec2(20.0, 18.0));
            p.rect_stroke(mark, 0.0, egui::Stroke::new(1.0, crate::lr::gray(0x5a)), egui::StrokeKind::Inside);
            lr::paint_icon(p, mark.shrink(3.0), lr::Icon::Camera, lr::TEXT_DIM());
            name_right = mark.left() - 6.0;
            let tip = ui.interact(mark, ui.scope_id().with("camera-default-mark"), egui::Sense::hover());
            lr::hover_tip(ui, &tip, &crate::camera_defaults::tooltip(e));
        }
        let p = ui.painter();
        let clip = p.with_clip_rect(egui::Rect::from_min_max(egui::pos2(rect.left() + 100.0, rect.top()), egui::pos2(name_right, rect.bottom())));
        clip.text(egui::pos2(rect.left() + 100.0, rect.center().y), egui::Align2::LEFT_CENTER, name, egui::TextStyle::Body.resolve(ui.style()), lr::TEXT());
        lr::paint_icon(p, egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(12.0, 12.0)), lr::Icon::Chevron, lr::TEXT_DIM());
        if resp.clicked() {
            self.choose_tool(crate::workspace::ToolTab::FilmLook);
        }
        ui.add_space(4.0);
    }

    /// "Intensity" of the active film or look; greyed out (same place) when the rendering has none.
    pub(super) fn profile_amount_ui(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &self.current else { return };
        let s = &mut self.entries[c.index].settings;
        let raw_look = c
            .loaded
            .previews
            .foreign
            .as_ref()
            .is_some_and(|doc| doc.look_name(s).is_some());
        let adjustable = develop::profile::adjustable_names(s, &c.loaded.camera_mode) || raw_look;
        let mut v = if adjustable { s.profile_amount as f64 * 100. } else { 100. };
        let changed = ui
            .add_enabled_ui(adjustable, |ui| lr::slider_hint(ui, "Intensität", &mut v, 0.0..=100.0, 100., 1., "Stärke der aktiven Wiedergabe"))
            .inner;
        if changed && adjustable {
            s.profile_amount = (v / 100.) as f32;
        }
    }
    /// Film looks (user and built-in, one grid) – a click puts the look's film on the photo,
    /// the other sliders stay. The search filters by look and film name.
    /// Returns: (clicked, hovered) – clicked as (key, name, choice), hovered as finished settings.
    fn film_looks_ui(&mut self, ui: &mut egui::Ui, s: &Settings) -> (Option<(String, String, Choice)>, Option<Settings>) {
        self.update_recipe_previews(ui.ctx());
        let query = self.profile_choice.search.trim().to_lowercase();
        let matches_filter = |e: &crate::recipe_grid::Entry| {
            query.is_empty() || format!("{} {}", e.name, e.film_name).to_lowercase().contains(&query)
        };
        let images = &self.darkroom.recipes.images;
        let shown: Vec<_> = images.entries.iter().filter(|e| matches_filter(e)).collect();
        if shown.is_empty() {
            return (None, None);
        }
        ui.add_space(4.);
        lr::category(ui, crate::tr("Filmlooks"));
        let selected = shown.iter().position(|e| s.film.is_active() && s.film == e.film);
        let subs: Vec<String> = shown.iter().map(|e| crate::recipe_grid::sub_line(e)).collect();
        let tiles: Vec<_> = shown
            .iter()
            .zip(&subs)
            .map(|(e, sub)| lr::Tile { tex: images.texture(e), name: &e.name, sub, star: None, available: true })
            .collect();
        let r = lr::tile_grid(ui, "film-looks", &tiles, selected, 3, false);
        for (i, rect) in &r.rects {
            if let Some(n) = self.look_compare.number(&recipe_key(shown[*i].source)) {
                crate::compare_view::pick_mark(ui.painter(), rect.min, n);
            }
        }
        let clicked = r.clicked.map(|i| shown[i]).map(|e| (recipe_key(e.source), e.name.clone(), Choice::Recipe(e.film.clone())));
        let hover = r.hovered.map(|i| {
            let mut t = s.clone();
            t.film = shown[i].film.clone();
            t
        });
        (clicked, hover)
    }
    pub(super) fn profile_browser_ui(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &self.current else {
            self.profile_browser = false;
            return;
        };
        let index = c.index;
        let camera_mode = c.loaded.camera_mode.clone();
        let modes: Vec<String> = c.loaded.cal.modes.iter().map(|m| m.name.clone()).collect();
        let foreign = c.loaded.previews.foreign.clone();
        let s = self.entries[index].settings.clone();
        ui.add_space(4.);
        let mut state_changed = false;
        let thumbs = self.profile_thumbs(ui, &[]);
        let film_an = s.film.is_active();
        let presentation = if film_an {
            film_card(&s)
        } else {
            look_card(&s, foreign.as_deref(), &camera_mode)
        };
        let mut look_settings = s.clone();
        look_settings.film.active = false;
        let look_ready = foreign.is_none() || rawdev::film_profiles::missing(&look_settings).is_none();
        let note = (!film_an && !look_ready).then(|| crate::tr("Nicht verfügbar"));
        self.look_compare.panel_frame = ui.ctx().cumulative_frame_nr();
        let (darkroom_clicked, compare_clicked) = look_header(ui, &presentation, note, &thumbs, self.look_compare.on);
        // Film sliders (paper, print, grain …) exist only in the darkroom
        if darkroom_clicked {
            self.enter_darkroom();
            return;
        }
        self.profile_amount_ui(ui);
        ui.add_space(4.);
        // fixed place; without an exportable look the button is dimmed and the tooltip says why
        let lut = self.look_lut_available();
        let lut_button = ui.add(egui::Button::new(egui::RichText::new(crate::tr("LOOK ALS LUT SPEICHERN …")).color(if lut.is_ok() { lr::TEXT() } else { lr::TEXT_DIM() })));
        lr::hover_tip(ui, &lut_button, match lut {
            Ok(()) => "Gewählten Look als 3D-LUT (.cube) speichern",
            Err(reason) => reason,
        });
        if lut_button.clicked() && lut.is_ok() {
            self.film_look_lut_open();
        }
        ui.add_space(4.);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 16. - ui.spacing().item_spacing.x;
            lr::search_field(ui, &mut self.profile_choice.search, "film-look", crate::tr("Look oder Film suchen"), w);
            if self.look_compare.on {
                help::info(ui, "look-compare-help", crate::tr("Looks vergleichen"), &help::LOOK_COMPARE);
            } else {
                help::info(ui, "film-look-browser-help", crate::tr("Film und Look"), &help::FILM_AND_LOOK);
            }
        });
        if compare_clicked {
            if self.look_compare.on {
                crate::log("look comparison: switched off");
                self.end_look_compare();
            } else {
                self.start_look_compare();
            }
        }
        // ⌘-click on a tile: into the comparison even without the switch
        let cmd = ui.input(|i| i.modifiers.command);
        let picking = self.look_compare.on || cmd;
        let mut picked: Option<(String, String, Choice)> = None;

        let mut requested = Vec::new();
        if !presentation.0.is_empty() && (film_an || look_ready) {
            requested.push(presentation.0.clone());
        }
        crate::darkroom::film_data_missing_hint(ui);
        // Order: camera or Sigma, film looks (grid), plain films (collapsed), look packs
        let mut groups = look_groups(&s, foreign.as_deref(), &camera_mode, &modes);
        let after_camera = groups.len().min(1);
        for (n, g) in film_groups().into_iter().enumerate() {
            groups.insert(after_camera + n, g);
        }
        let mut apply = None;
        let first_film_group = after_camera;
        let favs = self.favorites.clone();
        let favorites: Vec<_> = groups
            .iter()
            .flat_map(|g| g.entries.iter())
            .filter(|e| favs.contains(&e.key))
            .cloned()
            .collect();
        let mut first_film_group = first_film_group;
        if !favorites.is_empty() {
            first_film_group += 1;
            groups.insert(
                0,
                Group {
                    name: "Favoriten".into(),
                    entries: favorites,
                    package: false,
                },
            );
        }
        let current = presentation.0.clone();
        let (mut hovered, mut star) = (None, None);
        for (gi, g) in groups.iter().enumerate() {
            if gi == first_film_group {
                let (look_clicked, look_hovered) = self.film_looks_ui(ui, &s);
                if let Some((key, name, choice)) = look_clicked {
                    apply = Some(choice.apply(&s));
                    picked = Some((key, name, choice));
                }
                if look_hovered.is_some() {
                    hovered = look_hovered;
                }
            }
            let picks = &self.look_compare;
            let numbers = |k: &str| picks.number(k);
            let action = group_ui(
                ui,
                &mut self.profile_choice,
                g,
                &current,
                &s,
                &thumbs,
                &favs,
                &mut requested,
                &numbers,
            );
            if let Some(t) = action.chosen {
                apply = Some(t);
            }
            if action.picked.is_some() {
                picked = action.picked;
            }
            if let Some(t) = action.hover {
                hovered = Some(t);
            }
            if let Some(k) = action.star_toggle {
                star = Some(k);
            }
            if let Some(id) = action.hide {
                film_profiles_ui::hide(ui.ctx(), id);
            }
            state_changed |= action.state_dirty;
        }
        if groups.iter().all(|g| {
            !g.entries.iter().any(|e| {
                self.profile_choice.search.trim().is_empty()
                    || format!("{} {}", g.name, e.name)
                        .to_lowercase()
                        .contains(&self.profile_choice.search.trim().to_lowercase())
            })
        }) {
            ui.label(crate::tr("Keine Treffer"));
        }
        self.profile_thumbs(ui, &requested);
        if state_changed {
            self.profile_choice.save();
        }
        match (picking, picked) {
            (true, Some((key, name, choice))) => {
                crate::log(&format!("look comparison: tile {key} picked/removed"));
                self.toggle_compare_pick(crate::compare_view::Pick { key, name, choice });
            }
            (_, picked) => {
                if let Some(mut t) = apply {
                    if let Some((_, name, choice)) = &picked {
                        crate::history::hint(index, crate::history::Hint::film(name, !matches!(choice, Choice::Film(_) | Choice::Recipe(_))));
                    }
                    rawdev::film_profiles::remember_reference(&mut t);
                    self.entries[index].settings = t;
                }
            }
        }
        if !self.look_compare.test_done {
            self.look_compare.test_done = true;
            if let Some(keys) = crate::env::var("GRAINEO_LOOK_COMPARE").ok().filter(|_| crate::env::var_os("GRAINEO_DATA_DIR").is_some()) {
                // test hook: keys of the tiles (as in GRAINEO_HOVER_TEST), comma separated
                for k in keys.split(',').map(str::trim) {
                    if let Some(e) = groups.iter().flat_map(|g| g.entries.iter()).find(|e| e.key == k) {
                        self.toggle_compare_pick(crate::compare_view::Pick { key: e.key.clone(), name: e.name.clone(), choice: e.choice.clone() });
                    } else if let Some(e) = self.darkroom.recipes.images.entries.iter().find(|e| recipe_key(e.source) == k || e.name == k) {
                        let pick = crate::compare_view::Pick { key: recipe_key(e.source), name: e.name.clone(), choice: Choice::Recipe(e.film.clone()) };
                        self.toggle_compare_pick(pick);
                    }
                }
            }
            // test hook: GRAINEO_LOOK_APPLY=<tile key or film look name> puts this look on the photo
            if let Some(k) = crate::env::var("GRAINEO_LOOK_APPLY").ok().filter(|_| crate::env::var_os("GRAINEO_DATA_DIR").is_some()) {
                // `sigma-mono` also on a photo in colour (where the tile is not offered): sets it to black and white
                let mono = (k == SIGMA_MONO_KEY && foreign.is_none()).then_some(Choice::SigmaMono);
                let choice = mono.or_else(|| groups.iter().flat_map(|g| g.entries.iter()).find(|e| e.key == k).map(|e| e.choice.clone())).or_else(|| {
                    self.darkroom.recipes.images.entries.iter().find(|e| recipe_key(e.source) == k || e.name == k).map(|e| Choice::Recipe(e.film.clone()))
                });
                if let Some(c) = choice {
                    self.entries[index].settings = c.apply(&s);
                }
            }
        }
        // test hook: GRAINEO_LOOK_LUT=1 opens the dialog "Look als LUT speichern" (after GRAINEO_LOOK_APPLY)
        if !self.darkroom.lut.hook_done && crate::env::var_os("GRAINEO_LOOK_LUT").is_some() && crate::env::var_os("GRAINEO_DATA_DIR").is_some() && self.look_compare.test_done {
            self.darkroom.lut.hook_done = true;
            self.film_look_lut_open();
        }
        // learned look of this camera and style, and the window to learn one (at the bottom of the look panel)
        if !film_an && crate::learn_look_ui::panel_section(ui, foreign.as_deref()) {
            crate::camera_looks_ui::open(self.lib.learn_candidates(), foreign.as_ref().and_then(|d| d.learn_key.clone()));
        }
        // during the look comparison hovering shows nothing in the image (the fields are the preview)
        if self.look_compare.on {
            hovered = None;
        }
        if let Some(k) = star {
            if !self.favorites.remove(&k) {
                self.favorites.insert(k);
            }
            if let Err(e) = save_favorites(&self.favorites) {
                self.status = e;
            }
        }
        if let Some(h) = hover_test(&groups, &s) {
            hovered = h;
        }
        // never a hover preview from a fast preview too rough to be shown
        let hovered = hovered.filter(|h| !self.derived_waits(h));
        match hovered {
            Some(h) => {
                self.film_hover_left = None;
                if self.hover_preset.as_ref() != Some(&h) {
                    if let Some(c) = &self.current {
                        self.worker.send(Job::HoverRender {
                            index: c.index,
                            previews: c.loaded.previews.clone(),
                            cal: c.loaded.cal.clone(),
                            settings: h.clone(),
                            supersede: true,
                        });
                    }
                    self.hover_preset = Some(h);
                }
                self.film_hover = true;
            }
            None => {
                // Back to the current state only after a short pause: moving quickly from tile to tile (through the gap
                // between them) keeps the last preview until the next one is rendered, without a flash of the current state.
                if self.film_hover {
                    let now = ui.input(|i| i.time);
                    let left = *self.film_hover_left.get_or_insert(now);
                    if now - left >= HOVER_LINGER {
                        self.hover_preset = None;
                        self.film_hover = false;
                        self.film_hover_left = None;
                    } else {
                        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(HOVER_LINGER - (now - left) + 0.01));
                    }
                }
            }
        }
    }
}

/// Seconds the hover preview stays after the pointer left the tiles.
const HOVER_LINGER: f64 = 0.22;

/// Key of a film look tile (for the comparison marks).
fn recipe_key(q: crate::darkroom_recipes::Source) -> String {
    format!("recipe:{q:?}")
}

/// Test hook `GRAINEO_HOVER_TEST="kodak_portra_400@6,-@6.1,ilford_hp5_plus@6.15,-@7"` (only with `GRAINEO_DATA_DIR`):
/// pretends that the tile with this key is hovered from the given second on (`-`: pointer between or outside the
/// tiles), for measuring fast hover sequences without mouse control. `None`: the hook does not decide.
fn hover_test(groups: &[Group], s: &Settings) -> Option<Option<Settings>> {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    crate::env::var_os("GRAINEO_DATA_DIR")?;
    let spec = crate::env::var("GRAINEO_HOVER_TEST").ok()?;
    let t = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64();
    let key = spec
        .split(',')
        .filter_map(|p| p.split_once('@'))
        .filter_map(|(k, at)| Some((k.trim(), at.trim().parse::<f64>().ok()?)))
        .filter(|(_, at)| *at <= t)
        .last()?
        .0;
    Some(groups.iter().flat_map(|g| g.entries.iter()).find(|e| e.key == key).map(|e| e.choice.apply(s)))
}

#[cfg(test)]
#[path = "film_look_tests.rs"]
mod tests;
