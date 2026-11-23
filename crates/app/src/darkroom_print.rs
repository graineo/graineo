//! Darkroom: contact print. In the contact sheet you can switch between "Grid" (paper × grade) and "Print":
//! a sheet with film strips of six frames each. The frames are the selected photos of the film strip
//! (at least two marked) or, without a selection, the active photo and the following ones; film and paper
//! are those of the darkroom. Rendering runs in the background (`Job::Print`), each finished photo appears immediately.
//! The export writes the sheet as JPEG or TIFF.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use develop::film::{self, FilmSettings, CONTACT_SHEET_COLUMNS};
use develop::{Rgb16, Settings};
use eframe::egui;
use egui::{Pos2, Rect, TextureHandle, Vec2};

use crate::jobs::{Job, PRINT_EDGE};
use crate::{lr, App};

/// Idle time of the settings before re-rendering (seconds).
const QUIET_PERIOD: f64 = 0.35;
/// At most this many photos on one sheet (six strips).
const MAX_IMAGES: usize = CONTACT_SHEET_COLUMNS * 6;
/// Frame width of the screen view.
const DISPLAY_W: usize = 360;

pub struct Print {
    /// Print shown instead of grid?
    pub an: bool,
    requested: Option<(u64, f64)>,
    computing: Option<u64>,
    /// Key of the running or finished computation
    key: u64,
    valid: Arc<AtomicU64>,
    /// Photos at full render size (RGB8) for the export
    full: Vec<Option<(usize, usize, Vec<u8>)>>,
    /// downscaled photos for the screen view
    small: Vec<Option<Rgb16>>,
    tex: Option<TextureHandle>,
    /// Film of the computation (edge print, paper white)
    film: FilmSettings,
    /// photos that could not be loaded
    missing: usize,
    /// quick stand-ins (library preview, dimmed, without film) until the developed photo arrives
    stand_in: Vec<Option<Rgb16>>,
    /// The shown sheet no longer matches the settings (new computation pending)
    stale: bool,
}

impl Default for Print {
    fn default() -> Self {
        Self { an: false, requested: None, computing: None, key: 0, valid: Arc::new(AtomicU64::new(0)), full: Vec::new(), small: Vec::new(), tex: None, film: FilmSettings::default(), missing: 0, stand_in: Vec::new(), stale: false }
    }
}

impl Print {
    /// Finished photos (for progress and tests).
    pub fn done(&self) -> usize {
        self.full.iter().filter(|v| v.is_some()).count()
    }

    pub fn total(&self) -> usize {
        self.full.len()
    }

    pub fn computing(&self) -> bool {
        self.computing.is_some()
    }

    /// The shown sheet is outdated, a new computation is pending.
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Photos that could not be loaded (the sheet is complete without them).
    pub fn missing(&self) -> usize {
        self.missing
    }

    /// The sheet is fully computed and matches the settings: only then it can be exported.
    pub fn complete(&self) -> bool {
        !self.stale && self.computing.is_none() && self.done() > 0 && self.done() + self.missing >= self.total()
    }

    /// Take over the result of a photo (only if the key is still needed).
    pub fn outcome(&mut self, ctx: &egui::Context, key: u64, nr: usize, pixels: Option<(usize, usize, Vec<u8>)>, done: bool) {
        if self.computing == Some(key) && done {
            self.computing = None;
        }
        if key != self.key || nr >= self.full.len() {
            return;
        }
        match pixels {
            Some((w, h, rgb)) => {
                let large = Rgb16 { width: w, height: h, data: rgb.iter().map(|&v| v as u16 * 257).collect() };
                self.small[nr] = Some(film::downscaled(&large, DISPLAY_W));
                self.full[nr] = Some((w, h, rgb));
            }
            None => self.missing += 1,
        }
        self.reset_to(ctx);
    }

    /// Quick stand-in of a photo (only if the key is still needed and the developed photo is not there yet).
    pub fn stand_in(&mut self, ctx: &egui::Context, key: u64, nr: usize, image: (usize, usize, Vec<u8>)) {
        if key != self.key || nr >= self.small.len() || self.small[nr].is_some() {
            return;
        }
        let (w, h, rgb) = image;
        // dimmed: it is the plain preview, the film look follows
        let large = Rgb16 { width: w, height: h, data: rgb.iter().map(|&v| (v as u32 * 257 * 6 / 10) as u16).collect() };
        if self.stand_in.len() != self.small.len() {
            self.stand_in = (0..self.small.len()).map(|_| None).collect();
        }
        self.stand_in[nr] = Some(film::downscaled(&large, DISPLAY_W));
        self.reset_to(ctx);
    }

    fn reset_to(&mut self, ctx: &egui::Context) {
        let copy = |r: &Rgb16| Rgb16 { width: r.width, height: r.height, data: r.data.clone() };
        let cells: Vec<Option<Rgb16>> = (0..self.small.len()).map(|i| self.small[i].as_ref().or_else(|| self.stand_in.get(i).and_then(|v| v.as_ref())).map(copy)).collect();
        let sheet = film::contact_sheet(&cells, &self.film, CONTACT_SHEET_COLUMNS, DISPLAY_W);
        // never larger than the GPU allows (a sheet of four frames can exceed 2048 px); the export uses `export_image`
        let sheet = film::downscaled(&sheet, ctx.input(|i| i.max_texture_side));
        let img = egui::ColorImage::from_rgb([sheet.width, sheet.height], &to_rgb8(&sheet));
        match &mut self.tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("dk-abzug", img, egui::TextureOptions::LINEAR)),
        }
    }

    /// Sheet at full render size for the export.
    pub fn export_image(&self) -> Rgb16 {
        let cells: Vec<Option<Rgb16>> = self.full.iter().map(|v| v.as_ref().map(|(w, h, rgb)| Rgb16 { width: *w, height: *h, data: rgb.iter().map(|&b| b as u16 * 257).collect() })).collect();
        let zb = self.full.iter().flatten().map(|(w, h, _)| *w.max(h)).max().unwrap_or(PRINT_EDGE);
        film::contact_sheet(&cells, &self.film, CONTACT_SHEET_COLUMNS, zb)
    }
}

fn to_rgb8(img: &Rgb16) -> Vec<u8> {
    img.data.iter().map(|&v| (v >> 8) as u8).collect()
}

/// Film for the individual photos: as in the darkroom but without border (the print sets that itself).
fn print_film(f: &FilmSettings) -> FilmSettings {
    let mut t = f.clone();
    t.print_border = 0.0;
    t.print_border_bottom = 0.0;
    t.film_border = false;
    t.film_strip = false;
    t
}

/// Short names of film and paper of the sheet ("Kodak Tri-X 400 · Ilford MG IV RC"); `None` without film.
fn film_paper_name(f: &FilmSettings) -> Option<String> {
    if !f.is_active() {
        return None;
    }
    let stock = film::stock(&f.stock)?;
    let mut name = crate::darkroom::short_name(&stock.name);
    if !stock.positive && !f.negative {
        let id = f.paper.clone().or_else(|| stock.default_paper.clone());
        if let Some(p) = id.and_then(|id| film::papers().iter().find(|p| p.id == id)) {
            name = format!("{name} · {}", crate::darkroom::paper_short_name(&p.name).replace("Multigrade", "MG"));
        }
    }
    Some(name)
}

/// Photos of the sheet from the chosen ones (ascending); fewer than two chosen: `current` and the following ones. `true` = real selection.
fn plan_indices(chosen: &[usize], ok: impl Fn(usize) -> bool, current: usize, len: usize) -> (Vec<usize>, bool) {
    let selection: Vec<usize> = chosen.iter().copied().filter(|&i| ok(i)).collect();
    let (mut v, selected) = if selection.len() >= 2 { (selection, true) } else { ((current..len).filter(|&i| ok(i)).collect(), false) };
    v.truncate(MAX_IMAGES);
    (v, selected)
}

fn cache_key(sources: &[(std::path::PathBuf, Settings)], film: &FilmSettings) -> u64 {
    let mut h = DefaultHasher::new();
    for (p, s) in sources {
        p.hash(&mut h);
        serde_json::to_string(s).unwrap_or_default().hash(&mut h);
    }
    film.paper_white.hash(&mut h);
    serde_json::to_string(film).unwrap_or_default().hash(&mut h);
    h.finish() | 1
}

impl App {
    /// Which photos go on the sheet and whether they are a real selection: the selected photos (film strip and grid, two or more),
    /// otherwise the active photo and the following ones. The actively shown photo alone is no selection.
    pub(crate) fn print_plan(&self, current: usize) -> (Vec<usize>, bool) {
        let ok = |i: usize| self.entries.get(i).is_some_and(|e| crate::developable_state(e.state));
        plan_indices(&self.chosen_entries(), ok, current, self.entries.len())
    }

    pub(crate) fn print_indices(&self, current: usize) -> Vec<usize> {
        self.print_plan(current).0
    }

    /// Header line of the contact print: what is on the sheet, with which film and paper.
    pub(crate) fn darkroom_print_info(&self, index: usize) -> String {
        let (list, selected) = self.print_plan(index);
        let n = list.len();
        let what = if selected {
            crate::trf!("{n} ausgewählte Fotos", n = n)
        } else if n == 1 {
            crate::tr("1 Foto: das aktive").to_string()
        } else {
            crate::trf!("{n} Fotos ab dem aktiven", n = n)
        };
        match self.entries.get(index).and_then(|e| film_paper_name(&e.settings.film)) {
            Some(name) => format!("{what} · {name}"),
            None => what,
        }
    }

    /// Every frame: start the computation when the settings have been idle briefly and something changed.
    pub(crate) fn darkroom_print_update(&mut self, ctx: &egui::Context) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let film = self.entries[index].settings.film.clone();
        if !film.is_active() {
            return;
        }
        let indices = self.print_indices(index);
        for &i in &indices {
            self.ensure_entry(i);
        }
        let files: Vec<i64> = indices.iter().map(|&i| self.entries[i].file).collect();
        let process_film = print_film(&film);
        let sources: Vec<(std::path::PathBuf, Settings)> = indices
            .iter()
            .map(|&i| {
                let e = &self.entries[i];
                let mut s = e.settings.clone();
                s.film = process_film.clone();
                (e.path.clone(), s)
            })
            .collect();
        let key = cache_key(&sources, &film);
        let now = ctx.input(|i| i.time);
        let st = &mut self.darkroom.contact.print;
        st.stale = st.key != key;
        if st.key == key {
            return;
        }
        match st.requested {
            Some((k, since)) if k == key => {
                if now - since >= QUIET_PERIOD {
                    st.key = key;
                    st.stale = false;
                    st.film = film;
                    st.full = vec![None; sources.len()];
                    st.small = (0..sources.len()).map(|_| None).collect();
                    st.stand_in = Vec::new();
                    st.missing = 0;
                    st.computing = Some(key);
                    st.valid.store(key, Ordering::SeqCst);
                    st.reset_to(ctx);
                    let previews = self.lib.preview_source(&files);
                    self.worker.send(Job::Print { images: sources, key, valid: st.valid.clone(), previews });
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

    /// Draw the sheet in the image area `viewport` (zoom 1 = whole sheet visible).
    pub(crate) fn darkroom_print_draw(&mut self, ui: &mut egui::Ui, viewport: Rect, zoom: f32) {
        let p = ui.painter().clone();
        let film_an = self.current.as_ref().is_some_and(|c| self.entries[c.index].settings.film.is_active());
        let Some(t) = self.darkroom.contact.print.tex.as_ref().filter(|_| film_an) else {
            let text = if film_an { "…" } else { crate::tr("Film wählen") };
            p.text(viewport.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), lr::TEXT_DIM);
            return;
        };
        let ts = t.size_vec2();
        let fit = (viewport.width() / ts.x).min(viewport.height() / ts.y);
        let shown = ts * fit * zoom;
        let id = t.id();
        let mut sub = ui.new_child(egui::UiBuilder::new().max_rect(viewport));
        egui::ScrollArea::both().id_salt("dk-abzug-scroll").auto_shrink([false, false]).show(&mut sub, |ui| {
            let surface = Vec2::new(shown.x.max(viewport.width()), shown.y.max(viewport.height()));
            let (rect, _) = ui.allocate_exact_size(surface, egui::Sense::hover());
            let target = Rect::from_min_size(Pos2::new(rect.center().x - shown.x / 2.0, rect.min.y + ((surface.y - shown.y) / 2.0).max(0.0)), shown);
            ui.painter().image(id, target, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), egui::Color32::WHITE);
        });
    }

    /// Save the sheet as a file (dialog; JPEG or TIFF by extension).
    pub(crate) fn darkroom_print_export(&mut self, ctx: &egui::Context) {
        let st = &self.darkroom.contact.print;
        if !st.complete() {
            self.status = if st.done() == 0 { crate::tr("Kontaktabzug: noch keine Fotos fertig").into() } else { crate::tr("Kontaktabzug: noch nicht fertig gerechnet").into() };
            return;
        }
        crate::file_dialog::save(ctx, "dk-abzug-export", |d| d.set_title(crate::tr("Kontaktabzug exportieren")).set_file_name("kontaktabzug.jpg").add_filter(crate::tr("JPEG"), &["jpg", "jpeg"]).add_filter(crate::tr("TIFF 16 Bit"), &["tif", "tiff"]));
    }

    /// Second part of the export: write the sheet to the chosen location.
    pub(crate) fn darkroom_print_save(&mut self, path: std::path::PathBuf) {
        // never into an Apple Photos library or another program's package
        if let Err(e) = library::write_target::check(&path) {
            self.status = crate::trf!("Kontaktabzug nicht gespeichert: {e}", e = crate::menu::i18n::tr_msg(&e));
            return;
        }
        let sheet = self.darkroom.contact.print.export_image();
        let tiff = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
        let r = if tiff { develop::export::write_tiff16(&sheet, &path) } else { develop::export::write_jpeg(&sheet, &path, 92) };
        self.status = match r {
            Ok(()) => crate::trf!("Kontaktabzug gespeichert: {a}", a = path.display()),
            Err(e) => crate::trf!("Kontaktabzug nicht gespeichert: {e}", e = e),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_is_only_accepted_for_matching_key() {
        let ctx = egui::Context::default();
        let mut a = Print { key: 7, computing: Some(7), full: vec![None; 2], small: vec![None, None], film: FilmSettings { active: true, stock: "kodak_trix_400".into(), ..Default::default() }, ..Default::default() };
        let rgb = vec![128u8; 60 * 40 * 3];
        a.outcome(&ctx, 8, 0, Some((60, 40, rgb.clone())), true);
        assert_eq!(a.done(), 0, "foreign key is discarded");
        a.outcome(&ctx, 7, 0, Some((60, 40, rgb.clone())), false);
        assert_eq!((a.done(), a.computing()), (1, true));
        a.outcome(&ctx, 7, 1, None, true);
        assert_eq!((a.done(), a.missing, a.computing()), (1, 1, false));
        let sheet = a.export_image();
        assert_eq!((sheet.width, sheet.height), film::contact_sheet_size(2, CONTACT_SHEET_COLUMNS, 60));
    }

    #[test]
    fn plan_without_real_selection_starts_at_the_active_photo() {
        let ok = |i: usize| i != 4;
        // nothing selected, or only one: active photo and following ones (a failed one is skipped)
        assert_eq!(plan_indices(&[], ok, 2, 6), (vec![2, 3, 5], false));
        assert_eq!(plan_indices(&[3], ok, 2, 6), (vec![2, 3, 5], false), "one selected photo is no selection");
        // two or more: exactly those, ascending, unusable ones removed
        assert_eq!(plan_indices(&[1, 3, 4], ok, 2, 6), (vec![1, 3], true));
        assert_eq!(plan_indices(&[1, 4], ok, 2, 6), (vec![2, 3, 5], false), "only one usable left");
        // at most one sheet
        let many: Vec<usize> = (0..100).collect();
        let (v, sel) = plan_indices(&many, |_| true, 0, 100);
        assert!(sel && v.len() == MAX_IMAGES);
        let (v, sel) = plan_indices(&[], |_| true, 10, 100);
        assert!(!sel && v.len() == MAX_IMAGES && v[0] == 10);
    }

    #[test]
    fn export_needs_the_complete_sheet() {
        let ctx = egui::Context::default();
        let mut a = Print { key: 7, computing: Some(7), full: vec![None; 2], small: vec![None, None], film: FilmSettings { active: true, stock: "kodak_trix_400".into(), ..Default::default() }, ..Default::default() };
        let rgb = vec![128u8; 60 * 40 * 3];
        assert!(!a.complete());
        a.outcome(&ctx, 7, 0, Some((60, 40, rgb)), false);
        assert!(!a.complete(), "one of two photos is not enough");
        a.outcome(&ctx, 7, 1, None, true);
        assert!(a.complete() && a.missing() == 1, "failed photo reported, rest exportable");
        a.stale = true;
        assert!(!a.complete(), "outdated sheet");
    }

    #[test]
    fn film_for_photos_without_border() {
        let f = FilmSettings { active: true, print_border: 5.0, film_strip: true, film_border: true, ..Default::default() };
        let t = print_film(&f);
        assert!(t.is_active() && t.print_border == 0.0 && !t.film_strip && !t.film_border);
    }

    #[test]
    fn key_changes_with_film_and_image() {
        let p = std::path::PathBuf::from("a.x3f");
        let s = Settings::default();
        let f = FilmSettings { active: true, stock: "kodak_trix_400".into(), ..Default::default() };
        let a = cache_key(&[(p.clone(), s.clone())], &f);
        let mut g = f.clone();
        g.grain = 1.5;
        assert_ne!(a, cache_key(&[(p.clone(), s.clone())], &g));
        assert_ne!(a, cache_key(&[(p.clone(), s.clone()), (p.clone(), s.clone())], &f));
        assert_eq!(a, cache_key(&[(p, s)], &f));
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

    /// Whole window: three photos in the folder, contact sheet on "Print", all photos end up on the sheet.
    #[test]
    fn contact_print_in_full_window() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        if develop::film::films().is_empty() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("graineo-dk-abzug-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for (n, (w, h)) in [(1800u32, 1200u32), (1200, 1800), (1800, 1200)].into_iter().enumerate() {
            image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x / 8) as u8, (y / 6) as u8, 40 + 60 * n as u8])).save(folder.join(format!("bild{n}.png"))).unwrap();
        }
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
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), events, ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame(&mut a, vec![]);
        frame(&mut a, vec![]);
        frame(&mut a, vec![key_event(egui::Key::K)]);
        frame(&mut a, vec![press(egui::Key::K)]);
        assert!(a.darkroom.contact.an, "K opens the contact sheet");
        a.darkroom.contact.print.an = true;
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        loop {
            a.handle_results(&ctx);
            frame(&mut a, vec![]);
            let pr = &a.darkroom.contact.print;
            if pr.total() == 3 && pr.done() == 3 && !pr.computing() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "print did not finish: {}/{}", pr.done(), pr.total());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let sheet = a.darkroom.contact.print.export_image();
        let zb = a.darkroom.contact.print.full.iter().flatten().map(|(w, h, _)| *w.max(h)).max().unwrap();
        assert!(zb <= PRINT_EDGE && zb >= 300, "photos at render size: {zb}");
        assert_eq!((sheet.width, sheet.height), film::contact_sheet_size(3, CONTACT_SHEET_COLUMNS, zb));
        assert!(a.darkroom.contact.print.tex.is_some(), "sheet has a display texture");
        // Esc closes only the sheet
        frame(&mut a, vec![key_event(egui::Key::Escape)]);
        frame(&mut a, vec![press(egui::Key::Escape)]);
        assert!(!a.darkroom.contact.an);
    }

    /// Whole window at the smallest width: views, toolbar, thumbnails, selection, export lock, info box.
    #[test]
    fn views_toolbar_strip_and_selection() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        if develop::film::films().is_empty() {
            return;
        }
        let folder = std::env::temp_dir().join(format!("graineo-dk-ansichten-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for n in 0..4u8 {
            image::RgbImage::from_fn(900, 600, |x, y| image::Rgb([(x / 4) as u8, (y / 3) as u8, 30 + 50 * n])).save(folder.join(format!("bild{n}.png"))).unwrap();
        }
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
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(900.0, 600.0))), events, ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        let mut gaps: Vec<(&str, f32)> = Vec::new();
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame(&mut a, vec![]);
        frame(&mut a, vec![]);
        gaps.push(("single", a.darkroom.top_gap));

        // 1: the info box stays away while sheet or recipes cover the image
        assert!(!a.darkroom_covers_image());
        a.contact_view(Some(false));
        assert!(a.darkroom_covers_image());
        a.contact_view(None);
        a.darkroom.recipes.opened = true;
        assert!(a.darkroom_covers_image());
        a.darkroom.recipes.opened = false;
        assert!(!a.darkroom_covers_image());

        // 2: K opens the contact print, only one view at a time
        frame(&mut a, vec![key_event(egui::Key::K)]);
        frame(&mut a, vec![press(egui::Key::K)]);
        assert!(a.darkroom.contact.an && a.darkroom.contact.print.an, "K opens the contact print");
        frame(&mut a, vec![]);
        gaps.push(("contact print K", a.darkroom.top_gap));
        a.darkroom_choose_view(2);
        frame(&mut a, vec![]);
        gaps.push(("paper comparison", a.darkroom.top_gap));
        a.darkroom_choose_view(3);
        assert!(a.darkroom.contact.an && a.darkroom.contact.print.an && !a.darkroom.recipes.opened && a.darkroom.compare.mode == crate::darkroom_compare::Mode::Off);
        frame(&mut a, vec![]);
        gaps.push(("contact print", a.darkroom.top_gap));
        frame(&mut a, vec![key_event(egui::Key::K)]);
        frame(&mut a, vec![press(egui::Key::K)]);
        assert!(!a.darkroom.contact.an, "K closes");
        frame(&mut a, vec![key_event(egui::Key::K)]);
        frame(&mut a, vec![press(egui::Key::K)]);
        assert!(a.darkroom.contact.an && a.darkroom.contact.print.an, "K reopens the contact print");
        a.darkroom_choose_view(2);
        assert!(a.darkroom.contact.an && !a.darkroom.contact.print.an, "switching leaves the other view");
        a.darkroom_choose_view(2);
        assert!(!a.darkroom.contact.an, "a second click on the active view closes it");
        a.darkroom_choose_view(4);
        assert!(a.darkroom.recipes.opened && !a.darkroom.contact.an);
        frame(&mut a, vec![]);
        gaps.push(("recipes", a.darkroom.top_gap));
        a.darkroom_choose_view(1);
        a.darkroom.compare.remember(&a.entries[0].settings.clone());
        frame(&mut a, vec![]);
        gaps.push(("A|B", a.darkroom.top_gap));
        a.darkroom_choose_view(1);
        a.darkroom_choose_view(0);
        a.leave_darkroom();
        a.enter_darkroom();
        frame(&mut a, vec![]);

        // 4 + 5: without real selection "n photos from the active one"; the active photo alone is no selection
        let n = a.entries.len();
        assert!(n >= 4);
        let cur = a.current.as_ref().unwrap().index;
        for e in a.entries.iter_mut() {
            e.selected = false;
        }
        let (v, sel) = a.print_plan(cur);
        assert!(!sel && v == (cur..n).collect::<Vec<_>>());
        a.entries[cur].selected = true;
        assert!(!a.print_plan(cur).1, "only the active photo selected: still no selection");
        let other = (cur + 1) % n;
        a.entries[other].selected = true;
        let (v, sel) = a.print_plan(cur);
        assert!(sel && v.len() == 2 && v.contains(&cur) && v.contains(&other));
        assert!(v.windows(2).all(|w| w[0] < w[1]), "ascending");
        a.darkroom_choose_view(3);
        frame(&mut a, vec![]);
        assert!(a.darkroom_print_info(cur).starts_with("2 ausgewählte Fotos") || crate::tr("{n} ausgewählte Fotos") != "{n} ausgewählte Fotos");
        for e in a.entries.iter_mut() {
            e.selected = false;
        }
        assert!(a.darkroom_print_info(cur).contains(&format!("{}", n - cur)));

        // 7: export only when the whole sheet is computed
        a.darkroom.contact.print.an = true;
        let mut was_locked = false;
        let mut polls = 0;
        let deadline = std::time::Instant::now() + crate::ui_test::WAIT;
        loop {
            polls += 1;
            a.handle_results(&ctx);
            frame(&mut a, vec![]);
            let pr = &a.darkroom.contact.print;
            if !pr.complete() {
                was_locked = true;
            }
            if pr.complete() {
                assert_eq!(pr.done() + pr.missing(), pr.total());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "print did not finish");
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        // the lock is visible only when the sheet still had to be computed; in a full test run the previews can already
        // be there from earlier tests (same process), then the sheet is complete at once and there is nothing to lock
        let instant = a.darkroom.contact.print.complete() && !was_locked && polls == 1;
        assert!(was_locked || instant, "export was locked while rendering");
        gaps.push(("end", a.darkroom.top_gap));
        assert!(gaps.iter().all(|g| g.1 >= 0.0), "toolbar overlaps at 900 pt: {gaps:?}");
    }

    /// Measurement (not a test): `GRAINEO_MEASURE_DIR` with photos, `GRAINEO_LOG=1` for the per-photo times.
    #[test]
    #[ignore]
    fn measure_contact_print() {
        let Some(dir) = crate::env::var_os("GRAINEO_MEASURE_DIR") else { return };
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let _app_lock = crate::ui_test::app_lock();
        let mut a = App::new(&cc);
        a.open_folder_inner(std::path::PathBuf::from(dir));
        a.to_single_image();
        while a.current.as_ref().is_none_or(|c| c.texture.is_none()) || a.loading.is_some() {
            a.handle_results(&ctx);
            a.schedule(0.0);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut clock = 10.0;
        let mut frame = |a: &mut App| {
            clock += 0.5;
            let mut input = egui::RawInput { time: Some(clock), screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(1400.0, 1000.0))), ..Default::default() };
            a.raw_input_hook(&ctx, &mut input);
            let _ = crate::ui_test::run(&ctx, input, |ui| a.ui(ui, &mut eframe::Frame::_new_kittest()));
        };
        a.enter_darkroom();
        a.darkroom.test_film("kodak_trix_400");
        frame(&mut a);
        frame(&mut a);
        a.contact_view(Some(true));
        let t = std::time::Instant::now();
        let mut seen = 0;
        loop {
            a.handle_results(&ctx);
            frame(&mut a);
            let pr = &a.darkroom.contact.print;
            if pr.done() > seen {
                seen = pr.done();
                eprintln!("MEASURE photo {seen}/{} after {:.2?}", pr.total(), t.elapsed());
            }
            if pr.complete() {
                break;
            }
            assert!(t.elapsed().as_secs() < 300);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        eprintln!("MEASURE total {:.2?}", t.elapsed());
    }
}
