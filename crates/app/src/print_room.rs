//! The print room: its own room in the whole window (like the darkroom, Esc goes back). Left the templates, in the
//! middle the page preview ("Seite 1 von N", from the library's previews, laid out exactly as printed), right the
//! settings and, fixed at the bottom, "Drucker …" (page setup) and the main button; below, across the whole width, the
//! filmstrip of the library's current view ([`strip`]): the photos printed are the library's selection, in view order.
//! Printing renders the pages in full quality in the background ([`crate::print_job`]) and hands the PDF to the
//! system's print dialog ([`crate::print_dialog`]) or writes it to a file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use eframe::egui;
use egui::{Color32, Pos2, Rect, Vec2};
use print::layout::{caption, Align, Layout, Note, PhotoFields, PhotoSpec, RectMm, MM_PER_PT};
use print::template::{self as tpl, Background, InfoMode, Ink, Margins, Media, Orientation, Output, PageSpace, Paper, SharpenAmount, Template};

use crate::{lr, App};

#[path = "print_strip.rs"]
mod strip;

/// Key of the last settings in `settings.json`.
const LAST_KEY: &str = "printLast";
const LEFT_W: f32 = 250.0;
const RIGHT_W: f32 = 320.0;

/// A photo of the print (one of the selected photos of the view).
#[derive(Clone)]
struct RoomPhoto {
    /// position in the view (= index of `App::entries`)
    pos: usize,
    image: i64,
    file: i64,
    name: String,
    fields: PhotoFields,
}

/// The layout last computed and what it was computed from: recomputed only when one of these changes, so neither a
/// large selection nor the settings panel costs anything per frame.
struct LayoutCache {
    photos_gen: u64,
    t: Template,
    unprintable: Option<Margins>,
    info: String,
    offsets_gen: u64,
    /// print photos laid out with a stand-in shape (no thumbnail yet): (index, file); a thumbnail arriving lays out again
    missing: Vec<(usize, i64)>,
    layout: Arc<Layout>,
}

#[derive(Default)]
pub(crate) struct PrintRoom {
    pub open: bool,
    pub t: Template,
    /// name of the template last chosen (built-in key or the user's name)
    chosen: Option<String>,
    user: Vec<(PathBuf, Template)>,
    /// the print list: the selected, printable photos of the view in view order (derived from `sel`)
    photos: Vec<RoomPhoto>,
    page: usize,
    /// unprintable edges and printer of the last page setup
    unprintable: Option<Margins>,
    printer: Option<String>,
    running: Option<crate::print_job::Running>,
    message: String,
    message_is_error: bool,
    /// name typed for "Als Vorlage sichern"
    naming: Option<String>,
    /// a template of this name (or file name) exists: waiting for "replace?"
    replacing: Option<String>,
    /// shift of the visible part per photo (fill), only while the room is open
    offsets: HashMap<i64, [f64; 2]>,
    offsets_gen: u64,
    /// darkroom: film of the contact print for all photos
    film: Option<develop::film::FilmSettings>,
    /// layout last computed
    cache: Option<LayoutCache>,
    /// the sheet orientation was switched by hand in this room: a template chosen later keeps it, no suggestion
    orientation_manual: bool,
    /// suggest the sheet orientation from the photos with the next layout (once: on opening, on choosing a template)
    suggest_orientation: bool,
    /// running file output: chosen file
    pending_output: Option<Output>,
    /// image ids of the view in view order (the strip's positions), rebuilt when the view changes
    order: Vec<i64>,
    /// list state `order` was built from
    list_version: u64,
    /// the library's selection while the room is open ([`strip`]); every change is written back to the library
    sel: strip::Selection,
    /// counts changes of `sel`
    sel_gen: u64,
    /// (list state, `sel_gen`) the print list was built for
    photos_key: Option<(u64, u64)>,
    photos_gen: u64,
    /// the strip shows all photos of the view (else only the selected ones)
    show_all: bool,
    /// where the strip scrolls to (next frame) and the offset it eases towards
    follow: Option<strip::Follow>,
    strip_goal: Option<f32>,
    /// the preview goes to the page of the main photo (after the next layout)
    jump: bool,
    strip_scroll: f32,
    strip_area: Option<egui::Id>,
    cells: Option<strip::Cells>,
    /// thumbnails the strip and the preview want this frame (asked for once, after drawing)
    wanted: Vec<i64>,
    /// titles of the print photos (read in the background, only when a caption shows them)
    titles: HashMap<i64, String>,
    titles_asked: HashSet<i64>,
    titles_rx: Option<mpsc::Receiver<Vec<(i64, String)>>>,
}

impl PrintRoom {
    /// Is a job running?
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// The layout last computed.
    pub fn shown(&self) -> Option<&Layout> {
        self.cache.as_ref().map(|c| c.layout.as_ref())
    }

    pub fn set_page(&mut self, page: usize) {
        self.page = page;
    }
}

/// Exposure as "1/250 s · f/5,6 · ISO 100".
fn exposure(r: &library::db::Row) -> String {
    let mut v = Vec::new();
    if let Some(t) = r.time.filter(|t| *t > 0.0) {
        v.push(if t < 0.5 { format!("1/{} s", (1.0 / t).round()) } else { format!("{} s", (t * 10.0).round() / 10.0) });
    }
    if let Some(a) = r.aperture.filter(|a| *a > 0.0) {
        v.push(format!("f/{}", (a * 10.0).round() / 10.0));
    }
    if let Some(i) = r.iso.filter(|i| *i > 0) {
        v.push(format!("ISO {i}"));
    }
    if let Some(f) = r.focal.filter(|f| *f > 0.0) {
        v.push(format!("{} mm", f.round()));
    }
    v.join(" · ")
}

fn ink_color(i: Ink) -> Color32 {
    match i {
        Ink::Black => Color32::BLACK,
        Ink::White => crate::lr::TEXT_STRONG(),
        Ink::Gray => crate::lr::gray(128),
    }
}

/// Short display of a length ("12,7 × 17,8 cm").
fn size_text(w: f64, h: f64) -> String {
    format!("{} × {} cm", fmt1(w / 10.0), fmt1(h / 10.0))
}

fn fmt1(v: f64) -> String {
    let s = format!("{:.1}", v);
    if crate::menu::i18n::language() == crate::menu::i18n::Language::De { s.replace('.', ",") } else { s }
}

/// Text field for a length in mm.
fn mm_field(ui: &mut egui::Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>) -> bool {
    ui.add_sized([70.0, 20.0], egui::DragValue::new(value).range(range).clamp_existing_to_range(false).speed(0.5).max_decimals(1).suffix(" mm")).changed()
}

/// Row: name on the left, control on the right.
fn row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.set_min_height(22.0);
        ui.label(egui::RichText::new(label).color(lr::SLIDER_LABEL()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add).inner
    })
    .inner
}

fn switch_row(ui: &mut egui::Ui, label: &str, on: &mut bool, tip: &str) -> bool {
    let before = *on;
    let r = ui.horizontal(|ui| {
        ui.set_min_height(22.0);
        let l = ui.label(egui::RichText::new(label).color(lr::SLIDER_LABEL()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| lr::switch(ui, on));
        l
    });
    lr::hover_tip(ui, &r.inner, tip);
    before != *on
}

impl App {
    /// Open the print room. The photos to print are the library's selection (nothing selected: the active photo; in
    /// the single views the photo shown and the photos selected in the filmstrip); the strip shows all photos of the
    /// view with the selection about in the middle. Neither a selection nor an active photo: an empty print with a
    /// hint. `film`: darkroom film for all photos (contact print, its photos as the darkroom chose them), `template`:
    /// name of a built-in template to start with. Cheap at any size of the view: no photo is read here, the strip
    /// draws only its visible cells.
    pub(crate) fn open_print_room(&mut self, film: Option<develop::film::FilmSettings>, template: Option<&str>) {
        // the strip is the view the room was opened from: the grid's list (rebuilt only if it changed)
        self.sync_source(false);
        let indices = match (&film, self.current.as_ref().map(|c| c.index)) {
            (Some(_), Some(c)) => self.print_indices(c),
            _ => self.selection(),
        };
        let active = if self.in_grid() { self.lib.active_image() } else { self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable).and_then(|i| self.entries.get(i)).map(|e| e.image) };
        let chosen: Vec<i64> = indices.into_iter().filter_map(|i| self.entries.get(i)).map(|e| e.image).collect();
        let room = &mut self.print_room;
        room.user = tpl::load_all(&library::data_dir()).0;
        let last = library::settings::value(LAST_KEY);
        let last = (!last.is_null()).then(|| serde_json::to_string(&last).ok().and_then(|s| Template::from_json(&s).ok())).flatten();
        match template.and_then(|n| tpl::builtin().into_iter().find(|t| t.name == n)) {
            Some(t) => {
                room.chosen = Some(t.name.clone());
                room.t = t;
            }
            None => {
                room.chosen = None;
                room.t = last.unwrap_or_default();
            }
        }
        room.orientation_manual = false;
        room.suggest_orientation = true;
        room.show_all = library::settings::value(strip::SHOW_ALL_KEY).as_bool().unwrap_or(true);
        room.order = self.entries.iter().map(|e| e.image).collect();
        room.list_version = self.source_version;
        room.sel = strip::Selection::of_library(&chosen, active);
        room.photos.clear();
        room.photos_key = None;
        room.cache = None;
        room.cells = None;
        room.page = 0;
        room.offsets.clear();
        room.strip_scroll = 0.0;
        room.strip_goal = None;
        room.titles.clear();
        room.titles_asked.clear();
        room.titles_rx = None;
        room.film = film;
        room.naming = None;
        room.replacing = None;
        if !room.busy() {
            room.message.clear();
        }
        room.open = true;
        self.print_mark_changed(Some(strip::Follow::Center { instant: true }));
    }

    /// Can the photo at entry `i` go into the print (present, a photo that can be developed)?
    fn printable_entry(&self, i: usize) -> bool {
        self.entries.get(i).is_some_and(|e| crate::developable_state(e.state) && crate::image_kind::of(&e.path).editable())
    }

    /// The print room's photo for entry `i` (caption fields from the library row, the title once it is read).
    fn room_photo(&self, i: usize) -> RoomPhoto {
        let e = &self.entries[i];
        let filename = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| e.name.clone());
        let mut fields = PhotoFields { filename, ..Default::default() };
        if let Some(r) = self.lib.row_of_image(e.image) {
            fields.date = r.capture.as_deref().map(|c| c.split('T').next().unwrap_or(c).to_string()).unwrap_or_default();
            fields.camera = r.camera.clone().unwrap_or_default();
            fields.exposure = exposure(r);
        }
        fields.title = self.print_room.titles.get(&e.image).cloned().unwrap_or_default();
        RoomPhoto { pos: i, image: e.image, file: e.file, name: e.name.clone(), fields }
    }

    /// Every frame: follow a changed view (the selection keeps the photos still in it) and rebuild the print list
    /// when the view or the selection changed.
    fn print_refresh(&mut self, ctx: &egui::Context) {
        self.sync_source(false);
        if self.print_room.list_version != self.source_version || self.print_room.order.len() != self.entries.len() {
            let room = &mut self.print_room;
            room.list_version = self.source_version;
            room.order = self.entries.iter().map(|e| e.image).collect();
            let before = room.sel.clone();
            room.sel.tidy(&room.order);
            if room.sel != before {
                self.print_selection_changed(None);
            }
        }
        self.print_titles(ctx);
        let key = (self.print_room.list_version, self.print_room.sel_gen);
        if self.print_room.photos_key != Some(key) {
            let positions = strip::print_positions(&self.print_room.order, &self.print_room.sel, |p| self.printable_entry(p));
            let photos: Vec<RoomPhoto> = positions.into_iter().map(|i| self.room_photo(i)).collect();
            let room = &mut self.print_room;
            room.photos = photos;
            room.photos_key = Some(key);
            room.photos_gen += 1;
        }
    }

    /// Titles for the captions, only when the caption shows them: read from the library in a background thread (never
    /// in the drawing), the print list is rebuilt when they arrive.
    fn print_titles(&mut self, ctx: &egui::Context) {
        let room = &mut self.print_room;
        if let Some(rx) = &room.titles_rx {
            match rx.try_recv() {
                Ok(v) => {
                    room.titles.extend(v);
                    room.titles_rx = None;
                    room.photos_key = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => room.titles_rx = None,
                Err(mpsc::TryRecvError::Empty) => return,
            }
        }
        let info = &room.t.page.photo_info;
        let needed = info.mode == InfoMode::Title || (info.mode == InfoMode::Custom && info.text.contains("{title}"));
        if !needed {
            return;
        }
        let ask: Vec<i64> = room.photos.iter().map(|p| p.image).filter(|b| !room.titles_asked.contains(b)).collect();
        if ask.is_empty() {
            return;
        }
        room.titles_asked.extend(ask.iter().copied());
        let (tx, rx) = mpsc::channel();
        room.titles_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let titles = match library::db::Db::standard() {
                Ok(db) => ask.into_iter().filter_map(|b| db.title_of(b).map(|t| (b, t))).collect(),
                Err(_) => Vec::new(),
            };
            let _ = tx.send(titles);
            ctx.request_repaint();
        });
    }

    pub(crate) fn close_print_room(&mut self) {
        // the single views show the main photo of the print (the selection is the library's already)
        if !self.in_grid() {
            if let Some(i) = self.print_room.sel.main().and_then(|a| self.print_room.order.iter().position(|&o| o == a)) {
                if self.entries.get(i).is_some_and(|e| e.image == self.print_room.order[i]) {
                    self.select(i, false);
                    self.print_selection_changed(None);
                }
            }
        }
        let room = &mut self.print_room;
        room.open = false;
        room.cells = None;
        room.cache = None;
        room.photos.clear();
        room.titles_rx = None;
        if let Ok(v) = serde_json::to_value(&room.t) {
            library::settings::set(LAST_KEY, v);
        }
    }

    /// Photo sizes for the layout: the shape of the library preview (upright, with crop), else the shape from the
    /// library's dimensions. Also the photos laid out with a stand-in shape (no thumbnail yet).
    fn print_specs(&mut self) -> (Vec<PhotoSpec>, Vec<(usize, i64)>) {
        let mode = self.print_room.t.page.photo_info.mode;
        let pattern = self.print_room.t.page.photo_info.text.clone();
        let photos = std::mem::take(&mut self.print_room.photos);
        let mut missing = Vec::new();
        let specs = photos
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let thumb = self.lib.thumbnail(p.file).map(|t| t.size_vec2()).filter(|s| s.x > 0.0 && s.y > 0.0);
                if thumb.is_none() {
                    missing.push((k, p.file));
                }
                let aspect = thumb
                    .map(|s| s.x / s.y)
                    .or_else(|| self.entries.get(p.pos).filter(|e| e.image == p.image).map(|e| e.aspect).filter(|a| *a > 0.05 && *a < 20.0))
                    .unwrap_or(1.5);
                let (w, h) = if aspect >= 1.0 { (3000.0, 3000.0 / aspect) } else { (3000.0 * aspect, 3000.0) };
                PhotoSpec { width: w.round().max(1.0) as u32, height: h.round().max(1.0) as u32, caption: caption(mode, &pattern, &p.fields), offset: self.print_room.offsets.get(&p.image).copied().unwrap_or([0.0; 2]) }
            })
            .collect();
        self.print_room.photos = photos;
        (specs, missing)
    }

    /// The layout of the print, from the cache unless the print list, the settings, the shift of a photo or the
    /// shape of a photo (its thumbnail arrived) changed.
    fn print_layout(&mut self) -> Arc<Layout> {
        let info = self.print_page_info();
        let room = &self.print_room;
        let fresh = room.cache.as_ref().is_some_and(|c| c.photos_gen == room.photos_gen && c.t == room.t && c.unprintable == room.unprintable && c.info == info && c.offsets_gen == room.offsets_gen);
        if fresh {
            let missing = self.print_room.cache.as_ref().map(|c| c.missing.clone()).unwrap_or_default();
            if !missing.iter().any(|&(_, f)| self.lib.thumbnail(f).is_some()) {
                return self.print_room.cache.as_ref().map(|c| c.layout.clone()).expect("cache");
            }
        }
        let (specs, missing) = self.print_specs();
        let room = &mut self.print_room;
        if room.suggest_orientation && !specs.is_empty() {
            // a first suggestion, never a decision: only a template with several photos per sheet, only until the
            // orientation is switched by hand
            room.suggest_orientation = false;
            suggest_sheet_orientation(&mut room.t, room.orientation_manual, specs.iter().map(|s| s.width as f64 / s.height.max(1) as f64));
        }
        let layout = Arc::new(print::layout(&room.t, &specs, room.unprintable, &info));
        room.cache = Some(LayoutCache { photos_gen: room.photos_gen, t: room.t.clone(), unprintable: room.unprintable, info, offsets_gen: room.offsets_gen, missing, layout: layout.clone() });
        layout
    }

    /// Text of the page information line.
    fn print_page_info(&self) -> String {
        let t = &self.print_room.t;
        let date = library::backup::today();
        let space = match t.job.color.space {
            PageSpace::AdobeRgb => "Adobe RGB",
            PageSpace::Srgb => "sRGB",
        };
        let sharpen = match t.job.sharpen.amount {
            SharpenAmount::Off => crate::tr("ohne Schärfung").to_string(),
            a => crate::trf!("Schärfen {a} · {m}", a = crate::tr(amount_name(a)), m = crate::tr(media_name(t.job.sharpen.media))),
        };
        let mut v = vec![date.split('T').next().unwrap_or("").to_string(), crate::tr(&t.name).to_string(), crate::trf!("{s}, vom Drucker verwaltet", s = space), format!("{} ppi", t.job.effective_ppi()), sharpen];
        if t.job.draft {
            v.push(crate::tr("ENTWURF").into());
        }
        v.retain(|s| !s.is_empty());
        v.join(" · ")
    }

    /// Every frame (also with the room closed): progress of a running job, result of the page setup.
    pub(crate) fn print_room_poll(&mut self, ctx: &egui::Context) {
        if let Some(setup) = crate::print_dialog::poll_page_setup() {
            let room = &mut self.print_room;
            let (w, h) = setup.paper_mm;
            let (a, b) = (w.min(h), w.max(h));
            let known = tpl::PAPERS.iter().find(|p| (p.width_mm - a).abs() < 1.0 && (p.height_mm - b).abs() < 1.0);
            let was = room.t.paper.orientation;
            room.t.paper = Paper {
                id: known.map(|p| p.id.to_string()).unwrap_or_else(|| tpl::CUSTOM_PAPER.into()),
                width_mm: known.map(|p| p.width_mm).unwrap_or(a),
                height_mm: known.map(|p| p.height_mm).unwrap_or(b),
                orientation: if setup.landscape { Orientation::Landscape } else { Orientation::Portrait },
            };
            if room.t.paper.orientation != was {
                room.t.swap_grid_axes();
                room.orientation_manual = true;
            }
            room.unprintable = Some(setup.unprintable);
            room.printer = Some(setup.printer.clone()).filter(|s| !s.is_empty());
            room.t.printer = room.printer.clone().map(|name| tpl::PrinterRef { name, paper_name: setup.paper_name.clone() });
            room.t.grid.cell_mm = [0.0, 0.0];
        }
        let Some(running) = self.print_room.running.as_mut() else { return };
        let Some(result) = running.poll() else { return };
        self.print_room.running = None;
        let room = &mut self.print_room;
        match result {
            Ok(outcome) => {
                room.message_is_error = false;
                if let Some(pdf) = outcome.printer_pdf {
                    room.message = self::hand_to_printer(&pdf, &room.t);
                } else {
                    let first = outcome.files.first().map(|p| p.display().to_string()).unwrap_or_default();
                    room.message = if outcome.files.len() > 1 { crate::trf!("{n} Seiten gespeichert: {a} …", n = outcome.files.len(), a = first) } else { crate::trf!("Gespeichert: {a}", a = first) };
                    if let Some(kept) = &outcome.kept_existing {
                        let name = kept.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        room.message = format!("{} ({})", room.message, crate::trf!("{a} gibt es schon und bleibt unverändert", a = name));
                    }
                    crate::log(&format!("print: {} page(s) to file in {:.1} s", outcome.pages, outcome.seconds));
                }
            }
            Err(e) => {
                room.message_is_error = true;
                room.message = crate::trf!("Nicht gedruckt: {e}", e = e);
            }
        }
        ctx.request_repaint();
    }

    /// Start the job: photos with their settings (darkroom film if given), layout as shown.
    fn print_start(&mut self, destination: crate::print_job::Destination) {
        let (specs, _) = self.print_specs();
        let page_info = self.print_page_info();
        let photos = self.print_room.photos.clone();
        let film = self.print_room.film.clone();
        let mut job_photos = Vec::new();
        for (p, spec) in photos.iter().zip(specs) {
            let Some(i) = Some(p.pos).filter(|&i| self.entries.get(i).is_some_and(|e| e.image == p.image)) else {
                self.print_room.message = crate::trf!("{name}: nicht mehr in der Liste", name = p.name);
                self.print_room.message_is_error = true;
                return;
            };
            self.ensure_entry(i);
            let e = &self.entries[i];
            let mut settings = e.settings.clone();
            if let Some(f) = &film {
                settings.film = f.clone();
            }
            job_photos.push(crate::print_job::JobPhoto { path: e.path.clone(), settings, image: e.image, name: p.name.clone(), spec, preview_key: None });
        }
        let previews = if self.print_room.t.job.draft {
            let files: Vec<i64> = photos.iter().map(|p| p.file).collect();
            self.lib.preview_source(&files).map(|(cache, keys)| {
                for (jp, k) in job_photos.iter_mut().zip(keys) {
                    jp.preview_key = k;
                }
                cache
            })
        } else {
            None
        };
        self.save_current();
        let room = &mut self.print_room;
        if let Ok(v) = serde_json::to_value(&room.t) {
            library::settings::set(LAST_KEY, v);
        }
        room.message.clear();
        room.message_is_error = false;
        let req = crate::print_job::Request { template: room.t.clone(), photos: job_photos, unprintable: room.unprintable, page_info, destination, previews };
        room.running = Some(crate::print_job::start(req, self.egui_ctx.clone()));
    }

    /// ⌘P inside the room: the main button, if it is active.
    pub(crate) fn print_room_main_key(&mut self, ctx: &egui::Context) {
        let ready = self.print_room.shown().is_some_and(|l| !l.pages.is_empty() && !l.notes.contains(&Note::NoRoom));
        if ready && !self.print_room.busy() {
            self.print_go(ctx);
        }
    }

    /// The main button: to the printer (PDF in the temp folder) or a save dialog for the file.
    fn print_go(&mut self, ctx: &egui::Context) {
        let t = &self.print_room.t;
        match t.job.output {
            Output::Printer => {
                let dir = std::env::temp_dir().join(format!("graineo-print-{}", std::process::id()));
                let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                self.print_start(crate::print_job::Destination::Printer(dir.join(format!("print-{n}.pdf"))));
            }
            o => {
                let (ext, filter) = match o {
                    Output::Jpeg => ("jpg", crate::tr("JPEG")),
                    Output::Tiff => ("tif", crate::tr("TIFF")),
                    _ => ("pdf", crate::tr("PDF")),
                };
                let name = format!("{}.{ext}", tpl::slug(crate::tr(&t.name)));
                self.print_room.pending_output = Some(o);
                crate::file_dialog::save(ctx, "print-file", move |d| d.set_title(crate::tr("In Datei drucken")).set_file_name(name).add_filter(filter, &[ext]));
            }
        }
    }

    /// The whole window while the print room is open.
    pub(crate) fn print_room_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(path) = crate::file_dialog::fetch_one("print-file") {
            if self.print_room.pending_output.take().is_some() {
                self.print_start(crate::print_job::Destination::File(path));
            }
        }
        let typing = ctx.egui_wants_keyboard_input();
        let modal = ctx.memory(|m| m.top_modal_layer().is_some());
        if !typing && !modal && !crate::print_dialog::page_setup_open() {
            if crate::shortcuts::pressed(&ctx, "print_close") {
                if self.print_room.naming.is_some() || self.print_room.replacing.is_some() {
                    self.print_room.naming = None;
                    self.print_room.replacing = None;
                } else {
                    self.close_print_room();
                    return;
                }
            }
        }
        if !typing && !modal && !crate::print_dialog::page_setup_open() && !self.print_room.busy() {
            self.print_strip_keys(&ctx);
            if crate::shortcuts::pressed(&ctx, "print_page_prev") {
                self.print_room.page = self.print_room.page.saturating_sub(1);
            }
            if crate::shortcuts::pressed(&ctx, "print_page_next") {
                self.print_room.page += 1;
            }
        }
        self.print_refresh(&ctx);
        // the layout is pure arithmetic on the photo shapes (no pixels), cached: every change of the selection or the
        // settings shows at once without a background job, and nothing is computed again while nothing changes
        let l = self.print_layout();
        let pages = l.pages.len().max(1);
        let room = &mut self.print_room;
        if std::mem::take(&mut room.jump) {
            // the page of the main photo, else of the first photo of the print
            let positions: Vec<usize> = room.photos.iter().map(|p| p.pos).collect();
            let k = room.sel.main().and_then(|a| room.order.iter().position(|&o| o == a)).and_then(|p| strip::print_index(&positions, p)).unwrap_or(0);
            if let Some(p) = strip::page_of(&l, k) {
                room.page = p;
            }
        }
        self.print_room.page = self.print_room.page.min(pages - 1);

        egui::Panel::top("print-top").resizable(false).exact_size(lr::BAR_H + 8.0).frame(egui::Frame::new().fill(lr::PANEL_DARK()).inner_margin(egui::Margin::symmetric(12, 4))).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let g = lr::caps(ui.painter(), crate::tr("Drucken"), lr::semibold(12.0), lr::TEXT());
                let (r, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
                ui.painter().galley(r.min, g, lr::TEXT());
                ui.add_space(16.0);
                let what = crate::trf!("{n} Fotos · {p} Seiten", n = self.print_room.photos.len(), p = l.pages.len());
                ui.label(egui::RichText::new(what).color(lr::TEXT_DIM()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add(egui::Button::new(crate::tr("Schließen")));
                    lr::tip_key(ui, &r, "Druckraum schließen", "print_close");
                    if r.clicked() {
                        self.close_print_room();
                    }
                });
            });
        });
        if !self.print_room.open {
            return;
        }
        // the filmstrip across the whole width below (fixed height, no handle)
        let mut action = None;
        self.print_room.wanted.clear();
        egui::Panel::bottom("print-strip").resizable(false).exact_size(strip::STRIP_H).frame(egui::Frame::new().fill(lr::PANEL_DARK()).inner_margin(egui::Margin::symmetric(8, 3))).show(ui, |ui| {
            action = self.print_strip(ui, &l);
        });
        egui::Panel::left("print-left").resizable(false).exact_size(LEFT_W).frame(egui::Frame::new().fill(lr::PANEL()).inner_margin(egui::Margin::symmetric(10, 6))).show(ui, |ui| self.print_left(ui));
        egui::Panel::right("print-right").resizable(false).exact_size(RIGHT_W).frame(egui::Frame::new().fill(lr::PANEL()).inner_margin(egui::Margin::symmetric(10, 6))).show(ui, |ui| self.print_right(ui, &l));
        egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(lr::IMAGE_BG())).show(ui, |ui| self.print_preview(ui, &l));
        // thumbnails of the visible cells and of the page shown, asked for once (they arrive in the background)
        let wanted = std::mem::take(&mut self.print_room.wanted);
        if !wanted.is_empty() {
            self.lib.request_thumbnails(&wanted);
        }
        // changes of the selection after drawing: the next frame lays out and shows them
        if let Some(a) = action {
            if !self.print_room.busy() {
                self.print_apply(a);
            }
            ctx.request_repaint();
        }
    }

    fn print_left(&mut self, ui: &mut egui::Ui) {
        lr::category(ui, crate::tr("Vorlagen"));
        let mut choose: Option<Template> = None;
        let chosen = self.print_room.chosen.clone();
        egui::ScrollArea::vertical().id_salt("print-templates").max_height(ui.available_height() - 60.0).auto_shrink([false, true]).show(ui, |ui| {
            for t in tpl::builtin() {
                let active = chosen.as_deref() == Some(t.name.as_str());
                if lr::list_item(ui, crate::tr(&t.name), active).clicked() {
                    choose = Some(t);
                }
            }
            if !self.print_room.user.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(crate::tr("Eigene")).color(lr::TEXT_DIM()).small());
            }
            let mut remove: Option<PathBuf> = None;
            let mut update: Option<String> = None;
            for (path, t) in &self.print_room.user {
                let active = chosen.as_deref() == Some(t.name.as_str());
                let r = lr::list_item(ui, &t.name, active);
                if r.clicked() {
                    choose = Some(t.clone());
                }
                r.context_menu(|ui| {
                    if ui.button(crate::tr("Mit aktuellen Einstellungen aktualisieren")).clicked() {
                        update = Some(t.name.clone());
                        ui.close();
                    }
                    if ui.button(crate::tr("In den Papierkorb legen")).clicked() {
                        remove = Some(path.clone());
                        ui.close();
                    }
                });
            }
            if let Some(name) = update {
                // "update" is the explicit wish to replace this own template
                self.print_save_template(&name, true);
            }
            if let Some(p) = remove {
                match trash::delete(&p) {
                    Ok(()) => self.print_room.user.retain(|(q, _)| *q != p),
                    Err(e) => {
                        self.print_room.message = crate::trf!("Vorlage nicht entfernt: {e}", e = e);
                        self.print_room.message_is_error = true;
                    }
                }
            }
        });
        if let Some(t) = choose {
            let room = &mut self.print_room;
            room.chosen = Some(t.name.clone());
            // a template changes the layout, never the paper chosen in the page setup
            let manual = room.orientation_manual.then_some(room.t.paper.orientation);
            room.t = t;
            if let Some(o) = manual.filter(|_| room.t.is_multi_photo_sheet()) {
                // the orientation switched by hand stays
                room.t.set_orientation(o);
            }
            room.suggest_orientation = true;
            room.page = 0;
            room.offsets.clear();
            room.offsets_gen += 1;
        }
        ui.add_space(4.0);
        if let Some(name) = self.print_room.replacing.clone() {
            // a template of the same name (or the same file name) is never replaced without asking
            ui.label(egui::RichText::new(crate::trf!("Vorlage „{a}“ gibt es schon.", a = name)).color(lr::TEXT()));
            ui.horizontal(|ui| {
                if ui.button(crate::tr("Ersetzen")).clicked() {
                    self.print_room.replacing = None;
                    self.print_save_template(&name, true);
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    self.print_room.replacing = None;
                    self.print_room.naming = Some(name.clone());
                }
            });
        } else {
        match self.print_room.naming.clone() {
            None => {
                let r = ui.button(crate::tr("Als Vorlage sichern …"));
                lr::hover_tip(ui, &r, "Aktuelle Einstellungen als eigene Vorlage sichern");
                if r.clicked() {
                    self.print_room.naming = Some(String::new());
                }
            }
            Some(mut name) => {
                ui.horizontal(|ui| {
                    let r = ui.add_sized([LEFT_W - 100.0, 20.0], egui::TextEdit::singleline(&mut name).hint_text(crate::tr("Name")));
                    r.request_focus();
                    let ok = !name.trim().is_empty() && (ui.button(crate::tr("Sichern")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if ok {
                        let n = name.trim().to_string();
                        self.print_room.naming = None;
                        self.print_save_template(&n, false);
                    } else {
                        self.print_room.naming = Some(name.clone());
                    }
                });
            }
        }
        }
    }

    /// Save the current settings as own template `name`; an existing one of the same file name only with `replace`
    /// (otherwise the room asks first).
    fn print_save_template(&mut self, name: &str, replace: bool) {
        let mut t = self.print_room.t.clone();
        t.name = name.to_string();
        if !replace && tpl::existing(&library::data_dir(), &t.name).is_some() {
            self.print_room.replacing = Some(t.name);
            return;
        }
        match tpl::save(&library::data_dir(), &t) {
            Ok(_) => {
                self.print_room.user = tpl::load_all(&library::data_dir()).0;
                self.print_room.chosen = Some(t.name);
                self.print_room.message = crate::trf!("Vorlage „{a}“ gesichert", a = name);
                self.print_room.message_is_error = false;
            }
            Err(e) => {
                self.print_room.message = crate::trf!("Vorlage nicht gesichert: {e}", e = e);
                self.print_room.message_is_error = true;
            }
        }
    }

    fn print_right(&mut self, ui: &mut egui::Ui, l: &Layout) {
        let footer_h = 96.0;
        let avail = ui.available_rect_before_wrap();
        let top = Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - footer_h));
        let foot = Rect::from_min_max(Pos2::new(avail.min.x, avail.max.y - footer_h), avail.max);
        let busy = self.print_room.busy();
        let mut settings_ui = ui.new_child(egui::UiBuilder::new().max_rect(top).layout(egui::Layout::top_down(egui::Align::Min)));
        settings_ui.add_enabled_ui(!busy, |ui| {
            egui::ScrollArea::vertical().id_salt("print-settings").auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(RIGHT_W - 28.0);
                self.print_settings(ui, l);
            });
        });
        let mut f = ui.new_child(egui::UiBuilder::new().max_rect(foot.shrink2(Vec2::new(0.0, 6.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        f.painter().hline(foot.x_range(), foot.top(), egui::Stroke::new(1.0, crate::lr::gray(0x2a)));
        self.print_footer(&mut f, l);
        ui.allocate_rect(avail, egui::Sense::hover());
    }

    fn print_settings(&mut self, ui: &mut egui::Ui, l: &Layout) {
        let t = &mut self.print_room.t;
        let before = t.clone();
        // ── LAYOUT ──
        lr::category(ui, crate::tr("Seite und Raster"));
        let mut paper_id = t.paper.id.clone();
        row(ui, crate::tr("Papier"), |ui| {
            let shown = tpl::PAPERS.iter().find(|p| p.id == paper_id).map(|p| p.name.to_string()).unwrap_or_else(|| crate::tr("Frei").to_string());
            egui::ComboBox::from_id_salt("print-paper").selected_text(shown).width(130.0).show_ui(ui, |ui| {
                for p in tpl::PAPERS {
                    ui.selectable_value(&mut paper_id, p.id.to_string(), p.name);
                }
                ui.selectable_value(&mut paper_id, tpl::CUSTOM_PAPER.to_string(), crate::tr("Frei"));
            });
        });
        if paper_id != t.paper.id {
            match Paper::known(&paper_id, t.paper.orientation) {
                Some(p) => t.paper = p,
                None => t.paper.id = paper_id,
            }
        }
        if t.paper.id == tpl::CUSTOM_PAPER {
            row(ui, crate::tr("Breite × Höhe"), |ui| {
                mm_field(ui, &mut t.paper.height_mm, 20.0..=2000.0);
                ui.label("×");
                mm_field(ui, &mut t.paper.width_mm, 20.0..=2000.0);
            });
        }
        row(ui, crate::tr("Ausrichtung"), |ui| {
            lr::segmented_tips(
                ui,
                &mut t.paper.orientation,
                &[(Orientation::Portrait, "Hoch"), (Orientation::Landscape, "Quer")],
                &["Blatt hochkant; Reihen und Spalten werden neu aufgeteilt", "Blatt quer; Reihen und Spalten werden neu aufgeteilt"],
                8.0,
            );
        });
        if t.paper.orientation != before.paper.orientation {
            // switched by hand: the cells are divided anew for the other sheet (rows × columns swap)
            t.swap_grid_axes();
            self.print_room.orientation_manual = true;
        }
        let raised = l.notes.contains(&Note::MarginRaised);
        let minimum = self.print_room.unprintable;
        let side = |ui: &mut egui::Ui, label: &str, v: &mut f64, min: Option<f64>| {
            row(ui, label, |ui| {
                mm_field(ui, v, 0.0..=200.0);
                if let Some(m) = min.filter(|m| *m > *v + 0.01) {
                    ui.label(egui::RichText::new(crate::trf!("mind. {a} mm", a = fmt1(m))).color(lr::GOLD()).small());
                }
            });
        };
        side(ui, crate::tr("Rand oben"), &mut t.margins_mm.top, minimum.map(|m| m.top));
        side(ui, crate::tr("Rand unten"), &mut t.margins_mm.bottom, minimum.map(|m| m.bottom));
        side(ui, crate::tr("Rand links"), &mut t.margins_mm.left, minimum.map(|m| m.left));
        side(ui, crate::tr("Rand rechts"), &mut t.margins_mm.right, minimum.map(|m| m.right));
        if raised {
            ui.label(egui::RichText::new(crate::tr("Ränder auf den bedruckbaren Bereich des Druckers angehoben")).color(lr::TEXT_DIM()).small());
        }
        row(ui, crate::tr("Reihen × Spalten"), |ui| {
            ui.add_sized([44.0, 20.0], egui::DragValue::new(&mut t.grid.columns).range(1..=tpl::MAX_CELLS).clamp_existing_to_range(false).speed(0.1));
            ui.label("×");
            ui.add_sized([44.0, 20.0], egui::DragValue::new(&mut t.grid.rows).range(1..=tpl::MAX_CELLS).clamp_existing_to_range(false).speed(0.1));
        });
        row(ui, crate::tr("Abstand"), |ui| {
            mm_field(ui, &mut t.grid.gap_mm[1], 0.0..=100.0);
            ui.label("×");
            mm_field(ui, &mut t.grid.gap_mm[0], 0.0..=100.0);
        });
        // cell size: shows the size in effect; typing a value fixes it, the gap follows (6.2)
        let mut cell = [l.cell_w, l.cell_h];
        let cell_changed = row(ui, crate::tr("Zelle B × H"), |ui| {
            let b = mm_field(ui, &mut cell[1], 1.0..=2000.0);
            ui.label("×");
            let a = mm_field(ui, &mut cell[0], 1.0..=2000.0);
            a | b
        });
        if cell_changed {
            t.grid.cell_mm = cell;
        }
        // rows, columns, margins, gaps or paper changed: cells as large as possible again
        if t.grid.rows != before.grid.rows || t.grid.columns != before.grid.columns || t.margins_mm != before.margins_mm || t.grid.gap_mm != before.grid.gap_mm || t.paper != before.paper {
            t.grid.cell_mm = [0.0, 0.0];
        }

        // ── IMAGE ──
        lr::category(ui, crate::tr("Bildeinstellung"));
        switch_row(ui, crate::tr("Ausfüllen"), &mut t.image.fill, "Zelle ganz füllen, Überstand abschneiden; im Bild ziehen verschiebt den Ausschnitt");
        switch_row(ui, crate::tr("Drehen und einpassen"), &mut t.image.rotate_to_fit, "Foto um 90° drehen, wenn es dann größer wird");
        switch_row(ui, crate::tr("Ein Foto pro Seite wiederholen"), &mut t.image.repeat_one_per_page, "Jede Zelle einer Seite zeigt dasselbe Foto");
        switch_row(ui, crate::tr("Kontur"), &mut t.image.stroke.on, "Linie außen um jedes Foto");
        if t.image.stroke.on {
            row(ui, crate::tr("Konturbreite"), |ui| {
                ui.add_sized([70.0, 20.0], egui::DragValue::new(&mut t.image.stroke.width_pt).range(0.25..=10.0).clamp_existing_to_range(false).speed(0.05).max_decimals(2).suffix(" pt"));
            });
            row(ui, crate::tr("Konturfarbe"), |ui| {
                lr::segmented_pad(ui, &mut t.image.stroke.color, &[(Ink::Black, "Schwarz"), (Ink::Gray, "Grau"), (Ink::White, "Weiß")], 6.0);
            });
        }

        // ── PAGE ──
        lr::category(ui, crate::tr("Seite"));
        row(ui, crate::tr("Hintergrund"), |ui| {
            lr::segmented_pad(ui, &mut t.page.background, &[(Background::White, "Weiß"), (Background::Black, "Schwarz")], 8.0);
        });
        switch_row(ui, crate::tr("Hilfslinien"), &mut t.page.guides, "Ränder, Zellen und Maße in der Vorschau (werden nicht gedruckt)");
        switch_row(ui, crate::tr("Seitennummern"), &mut t.page.page_numbers, "Unten in der Mitte");
        switch_row(ui, crate::tr("Seiteninformationen"), &mut t.page.page_info, "Datum, Vorlage, Farbraum, Auflösung und Schärfung klein unten links");
        switch_row(ui, crate::tr("Schnittmarken"), &mut t.page.crop_marks, "Kurze Linien im Rand an den Bildkanten");
        switch_row(ui, crate::tr("Rand der Dunkelkammer mitdrucken"), &mut t.page.darkroom_border, "Papier- und Filmrand der Dunkelkammer gehören zum Bild; sonst macht das Seitenlayout den Rand");
        let modes = [(InfoMode::Off, "Aus"), (InfoMode::Filename, "Dateiname"), (InfoMode::Title, "Titel"), (InfoMode::Date, "Datum"), (InfoMode::Exposure, "Aufnahmedaten"), (InfoMode::Custom, "Eigener Text")];
        row(ui, crate::tr("Fotoinfo"), |ui| {
            let shown = modes.iter().find(|m| m.0 == t.page.photo_info.mode).map(|m| crate::tr(m.1)).unwrap_or_default();
            egui::ComboBox::from_id_salt("print-info").selected_text(shown).width(130.0).show_ui(ui, |ui| {
                for (m, n) in modes {
                    ui.selectable_value(&mut t.page.photo_info.mode, m, crate::tr(n));
                }
            });
        });
        if t.page.photo_info.mode == InfoMode::Custom {
            let r = ui.add(egui::TextEdit::singleline(&mut t.page.photo_info.text).desired_width(f32::INFINITY).hint_text("{filename} {date} {camera} {exposure} {title}"));
            lr::hover_tip(ui, &r, "Platzhalter: {filename} {title} {date} {camera} {exposure}");
        }
        if t.page.photo_info.mode != InfoMode::Off {
            row(ui, crate::tr("Schriftgrad"), |ui| {
                ui.add_sized([70.0, 20.0], egui::DragValue::new(&mut t.page.photo_info.size_pt).range(6.0..=16.0).clamp_existing_to_range(false).speed(0.1).max_decimals(1).suffix(" pt"));
            });
        }

        // ── JOB ──
        lr::category(ui, crate::tr("Druckauftrag"));
        row(ui, crate::tr("Ausgabe"), |ui| {
            lr::segmented_pad(ui, &mut t.job.output, &[(Output::Printer, "Drucker"), (Output::Pdf, "PDF"), (Output::Jpeg, "JPEG"), (Output::Tiff, "TIFF")], 6.0);
        });
        switch_row(ui, crate::tr("Entwurfsmodus"), &mut t.job.draft, "Schnell: Bibliotheksvorschau statt voller Entwicklung, 150 ppi, ohne Schärfung, 8 Bit");
        ui.add_enabled_ui(!t.job.draft, |ui| {
            let mut ppi = t.job.ppi;
            let opts: Vec<(u32, String)> = tpl::PPI_CHOICES.iter().map(|&p| (p, p.to_string())).collect();
            let opts_ref: Vec<(u32, &str)> = opts.iter().map(|(p, s)| (*p, s.as_str())).collect();
            if lr::choice(ui, "Druckauflösung", &mut ppi, &opts_ref, "print") {
                t.job.ppi = ppi;
            }
            let mut amount = t.job.sharpen.amount;
            if lr::choice(ui, "Ausdruck schärfen", &mut amount, &[(SharpenAmount::Off, "Aus"), (SharpenAmount::Low, "Niedrig"), (SharpenAmount::Standard, "Standard"), (SharpenAmount::High, "Hoch")], "print") {
                t.job.sharpen.amount = amount;
            }
            ui.add_enabled_ui(t.job.sharpen.amount != SharpenAmount::Off, |ui| {
                row(ui, crate::tr("Papier"), |ui| {
                    lr::segmented_pad(ui, &mut t.job.sharpen.media, &[(Media::Glossy, "Glanz"), (Media::Matte, "Matt")], 8.0);
                });
            });
            if matches!(t.job.output, Output::Printer | Output::Pdf | Output::Tiff) {
                switch_row(ui, crate::tr("16 Bit"), &mut t.job.bits16, "16 Bit je Kanal (PDF, TIFF); nicht jeder Druckertreiber nutzt sie");
            }
        });
        lr::choice(ui, "Farbraum der Seite", &mut t.job.color.space, &[(PageSpace::AdobeRgb, "Adobe RGB"), (PageSpace::Srgb, "sRGB")], "print");
        ui.label(egui::RichText::new(crate::tr("Farbmanagement: vom Drucker verwaltet")).color(lr::TEXT_DIM()).small());
        if t.job.output == Output::Jpeg {
            let mut q = t.job.jpeg_quality as f64;
            if lr::slider(ui, "JPEG-Qualität", &mut q, 50.0..=100.0, 92.0, 1.0) {
                t.job.jpeg_quality = q.round() as u8;
            }
        }
        ui.add_space(8.0);
        if *t != before {
            // the chosen template stays marked until another is chosen; the settings are now its variant
            self.print_room.page = self.print_room.page.min(l.pages.len().saturating_sub(1));
        }
    }

    fn print_footer(&mut self, ui: &mut egui::Ui, l: &Layout) {
        let room = &self.print_room;
        let t = &room.t;
        let ppi = t.job.effective_ppi();
        let (w, h) = l.size_px(ppi as f64);
        let mb = (w * h * if t.job.effective_bits16() { 6 } else { 3 }) as f64 / 1e6;
        let line = match &room.running {
            Some(r) => crate::trf!("Rechnet Seite {a} von {n} …", a = r.progress.0.max(1), n = r.progress.1.max(1)),
            None if !room.message.is_empty() => room.message.clone(),
            None => crate::trf!("{w} × {h} px je Seite · {mb} MB", w = w, h = h, mb = format!("{:.0}", mb)),
        };
        let color = if room.message_is_error && room.running.is_none() { Color32::from_rgb(0xe0, 0x80, 0x70) } else { lr::TEXT_DIM() };
        ui.add(egui::Label::new(egui::RichText::new(line).color(color).small()).truncate());
        ui.add_space(6.0);
        let printer = t.job.output == Output::Printer;
        let main = if printer { crate::tr("Drucken …") } else { crate::tr("In Datei drucken …") };
        let busy = room.busy();
        let can = !busy && !l.pages.is_empty() && !l.notes.contains(&Note::NoRoom);
        let mut go = false;
        let mut setup = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let avail = crate::print_dialog::page_setup_available();
            let r = ui.add_enabled(avail && !busy, egui::Button::new(crate::tr("Drucker …")));
            lr::hover_tip(ui, &r, if avail { "Drucker, Papier und Ausrichtung wählen (Seiteneinrichtung)" } else { "Auf diesem System über die PDF-Datei drucken" });
            setup = r.clicked();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if busy {
                    cancel = ui.button(crate::tr("Abbrechen")).clicked();
                    lr::main_button(ui, crate::tr("Rechnet …"), false);
                } else {
                    let r = lr::main_button(ui, main, can);
                    lr::tip_key(ui, &r, if printer { "Seiten in voller Qualität rechnen und an den Druckdialog geben" } else { "Seiten in voller Qualität in eine Datei schreiben" }, "print");
                    go = r.clicked();
                }
            });
        });
        if let Some(p) = &room.printer {
            ui.label(egui::RichText::new(crate::trf!("Drucker: {a}", a = p)).color(lr::TEXT_DIM()).small());
        }
        if setup {
            let t = self.print_room.t.clone();
            if !crate::print_dialog::begin_page_setup(&t) {
                self.print_room.message = crate::tr("Seiteneinrichtung nicht verfügbar").into();
                self.print_room.message_is_error = true;
            }
        }
        if cancel {
            if let Some(r) = &self.print_room.running {
                r.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        if go {
            let ctx = ui.ctx().clone();
            self.print_go(&ctx);
        }
    }

    fn print_preview(&mut self, ui: &mut egui::Ui, l: &Layout) {
        let full = ui.max_rect();
        let nav_h = 34.0;
        let area = Rect::from_min_max(full.min + Vec2::new(24.0, 20.0), Pos2::new(full.max.x - 24.0, full.max.y - nav_h - 8.0));
        let (pw, ph) = (l.width_mm as f32, l.height_mm as f32);
        let s = (area.width() / pw).min(area.height() / ph).max(0.01);
        let page_rect = Rect::from_center_size(area.center(), Vec2::new(pw * s, ph * s));
        let to = |x: f64, y: f64| Pos2::new(page_rect.left() + x as f32 * s, page_rect.top() + y as f32 * s);
        let rect = |r: &RectMm| Rect::from_min_max(to(r.x, r.y), to(r.right(), r.bottom()));
        let painter = ui.painter().with_clip_rect(full);
        painter.rect_filled(page_rect.translate(Vec2::new(3.0, 4.0)), 0.0, Color32::from_black_alpha(140));
        let paper = match l.background {
            Background::White => crate::lr::gray(0xf4),
            Background::Black => crate::lr::gray(0x08),
        };
        painter.rect_filled(page_rect, 0.0, paper);
        let room_page = self.print_room.page.min(l.pages.len().saturating_sub(1));
        let t = self.print_room.t.clone();
        let guides = t.page.guides;
        let guide = Color32::from_rgba_unmultiplied(0x3b, 0x7d, 0xd8, 150);
        // the printer's unprintable edges, tinted
        if let Some(u) = self.print_room.unprintable {
            let tint = Color32::from_rgba_unmultiplied(0xd0, 0x60, 0x40, 40);
            for r in [
                RectMm { x: 0.0, y: 0.0, w: l.width_mm, h: u.top },
                RectMm { x: 0.0, y: l.height_mm - u.bottom, w: l.width_mm, h: u.bottom },
                RectMm { x: 0.0, y: 0.0, w: u.left, h: l.height_mm },
                RectMm { x: l.width_mm - u.right, y: 0.0, w: u.right, h: l.height_mm },
            ] {
                painter.rect_filled(rect(&r), 0.0, tint);
            }
        }
        // only the photos of the page shown (the print list can be long)
        let on_page: Vec<(i64, i64)> = l.pages.get(room_page).map(|pg| pg.photos.iter().map(|p| self.print_room.photos.get(p.photo).map_or((0, 0), |q| (q.image, q.file))).collect()).unwrap_or_default();
        if let Some(page) = l.pages.get(room_page) {
            for (k, p) in page.photos.iter().enumerate() {
                let r = rect(&p.image);
                let (image, file) = on_page[k];
                match self.lib.thumbnail(file) {
                    Some(tex) => {
                        let [u0, v0, u1, v1] = p.crop.map(|v| v as f32);
                        // corners of the shown photo → texture coordinates (turned clockwise: (u, v) ↦ (v, 1 − u))
                        let uv = |u: f32, v: f32| if p.turned { Pos2::new(v, 1.0 - u) } else { Pos2::new(u, v) };
                        let mut mesh = egui::Mesh::with_texture(tex.id());
                        let corners = [(r.left_top(), uv(u0, v0)), (r.right_top(), uv(u1, v0)), (r.right_bottom(), uv(u1, v1)), (r.left_bottom(), uv(u0, v1))];
                        for (pos, uv) in corners {
                            mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: Color32::WHITE });
                        }
                        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                        painter.add(egui::Shape::mesh(mesh));
                    }
                    None => {
                        if image != 0 {
                            self.print_room.wanted.push(image);
                        }
                        painter.rect_filled(r, 0.0, crate::lr::gray(0x80));
                    }
                }
                // "Drehen und einpassen" turned the photo for the print: a small mark at the corner says so
                if p.turned && r.width() > 28.0 && r.height() > 28.0 {
                    let c = Pos2::new(r.right() - 11.0, r.top() + 11.0);
                    painter.circle_filled(c, 8.5, Color32::from_black_alpha(170));
                    painter.text(c, egui::Align2::CENTER_CENTER, "↻", egui::FontId::proportional(12.0), crate::lr::TEXT_STRONG());
                    let mark = ui.interact(Rect::from_center_size(c, Vec2::splat(18.0)), ui.scope_id().with(("print-turned", room_page, k)), egui::Sense::hover());
                    lr::hover_tip(ui, &mark, "Für den Druck um 90° gedreht („Drehen und einpassen“)");
                }
                // fill: drag shifts the visible part (only while the room is open)
                if t.image.fill {
                    let resp = ui.interact(r, ui.scope_id().with(("print-cell", room_page, k)), egui::Sense::drag());
                    let fx = (p.crop[2] - p.crop[0]).max(1e-6);
                    let fy = (p.crop[3] - p.crop[1]).max(1e-6);
                    let movable = fx < 0.999 || fy < 0.999;
                    if movable && resp.hovered() {
                        ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
                    }
                    if resp.dragged() && movable {
                        let d = resp.drag_delta();
                        self.print_room.offsets_gen += 1;
                        let o = self.print_room.offsets.entry(image).or_insert([0.0; 2]);
                        // offset 1 = shifted by half the overhang
                        let over_x = p.image.w * (1.0 / fx - 1.0) / 2.0;
                        let over_y = p.image.h * (1.0 / fy - 1.0) / 2.0;
                        if over_x > 1e-6 {
                            o[0] = (o[0] - d.x as f64 / s as f64 / over_x).clamp(-1.0, 1.0);
                        }
                        if over_y > 1e-6 {
                            o[1] = (o[1] - d.y as f64 / s as f64 / over_y).clamp(-1.0, 1.0);
                        }
                    }
                }
            }
            for f in &page.fills {
                let mut r = rect(&f.rect);
                if r.width() < 1.0 {
                    r = r.expand2(Vec2::new((1.0 - r.width()) / 2.0, 0.0));
                }
                if r.height() < 1.0 {
                    r = r.expand2(Vec2::new(0.0, (1.0 - r.height()) / 2.0));
                }
                painter.rect_filled(r, 0.0, ink_color(f.ink));
            }
            for tx in &page.texts {
                let size = (tx.size_pt * MM_PER_PT) as f32 * s;
                if size < 3.0 {
                    continue;
                }
                let font = egui::FontId::monospace(size);
                let max = tx.max_w as f32 * s;
                let fits = |t: &str| painter.layout_no_wrap(t.to_string(), font.clone(), crate::lr::TEXT_STRONG()).size().x <= max;
                let mut text = tx.text.clone();
                if !fits(&text) {
                    // shortened with "…" as in the print (raster.rs)
                    let mut chars: Vec<char> = tx.text.chars().collect();
                    while chars.len() > 1 && !fits(&(chars.iter().collect::<String>() + "…")) {
                        chars.pop();
                    }
                    text = chars.iter().collect::<String>() + "…";
                }
                let anchor = match tx.align {
                    Align::Left => egui::Align2::LEFT_TOP,
                    Align::Center => egui::Align2::CENTER_TOP,
                    Align::Right => egui::Align2::RIGHT_TOP,
                };
                painter.text(to(tx.x, tx.y), anchor, text, font, ink_color(tx.ink));
            }
            if guides {
                let stroke = egui::Stroke::new(1.0, guide);
                painter.rect_stroke(rect(&l.content), 0.0, stroke, egui::StrokeKind::Inside);
                let label = |r: &RectMm| {
                    let rr = rect(r);
                    painter.rect_stroke(rr, 0.0, egui::Stroke::new(1.0, guide.gamma_multiply(0.7)), egui::StrokeKind::Inside);
                    if rr.width() > 70.0 && rr.height() > 24.0 {
                        painter.text(rr.left_top() + Vec2::new(4.0, 3.0), egui::Align2::LEFT_TOP, size_text(r.w, r.h), egui::FontId::proportional(10.0), guide);
                    }
                };
                for (i, p) in page.photos.iter().enumerate() {
                    let rr = rect(&p.cell);
                    painter.rect_stroke(rr, 0.0, egui::Stroke::new(1.0, guide.gamma_multiply(0.7)), egui::StrokeKind::Inside);
                    // the size once on a photo (top left cell), it would cover the others
                    if i == 0 {
                        label(&p.cell);
                    }
                }
                for c in &page.empty_cells {
                    label(c);
                }
            }
        } else if l.notes.contains(&Note::NoRoom) {
            painter.text(page_rect.center(), egui::Align2::CENTER_CENTER, crate::tr("Kein Platz für eine Zelle: Ränder oder Abstände verkleinern"), egui::FontId::proportional(13.0), crate::lr::gray(0x60));
        } else if self.print_room.photos.is_empty() {
            painter.text(page_rect.center(), egui::Align2::CENTER_CENTER, crate::tr("Wähle Fotos im Filmstreifen"), egui::FontId::proportional(13.0), crate::lr::gray(0x60));
        }
        // page navigation
        let nav = Rect::from_min_max(Pos2::new(full.min.x, full.max.y - nav_h), full.max);
        let mut nav_ui = ui.new_child(egui::UiBuilder::new().max_rect(nav).layout(egui::Layout::left_to_right(egui::Align::Center)));
        let pages = l.pages.len().max(1);
        let label = crate::trf!("Seite {a} von {n}", a = room_page + 1, n = pages);
        let w = 260.0;
        nav_ui.add_space(((nav.width() - w) / 2.0).max(0.0));
        let prev = nav_ui.add_enabled(room_page > 0, egui::Button::new("◀").frame(false));
        lr::tip_key(&nav_ui, &prev, "Vorige Seite", "print_page_prev");
        if prev.clicked() {
            self.print_room.page = room_page.saturating_sub(1);
        }
        nav_ui.add_sized([180.0, 20.0], egui::Label::new(egui::RichText::new(label).color(lr::TEXT())));
        let next = nav_ui.add_enabled(room_page + 1 < pages, egui::Button::new("▶").frame(false));
        lr::tip_key(&nav_ui, &next, "Nächste Seite", "print_page_next");
        if next.clicked() {
            self.print_room.page = (room_page + 1).min(pages - 1);
        }
        let _ = (amount_name, media_name);
    }
}

/// Hand the finished PDF to the printer (or the test hook) and say what happened.
/// Preset the sheet orientation from the photos' aspect ratios: only a template with several photos per sheet, never
/// after the orientation was switched by hand. Returns whether the template changed.
fn suggest_sheet_orientation(t: &mut Template, manual: bool, aspects: impl IntoIterator<Item = f64>) -> bool {
    if manual || !t.is_multi_photo_sheet() {
        return false;
    }
    match tpl::suggest_orientation(aspects) {
        Some(o) if o != t.paper.orientation => {
            t.set_orientation(o);
            true
        }
        _ => false,
    }
}

fn hand_to_printer(pdf: &std::path::Path, t: &Template) -> String {
    let cleanup = || {
        let _ = std::fs::remove_file(pdf);
    };
    // test hook (own data folder only): the PDF goes to this file instead of the print dialog
    if let Some(to) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var_os("GRAINEO_PRINT_TO")) {
        let r = std::fs::copy(pdf, &to);
        cleanup();
        return match r {
            Ok(_) => crate::trf!("PDF für den Druck geschrieben: {a}", a = PathBuf::from(to).display()),
            Err(e) => crate::trf!("Nicht gedruckt: {e}", e = e),
        };
    }
    if !crate::print_dialog::dialogs_allowed() {
        cleanup();
        return crate::tr("Druckdialog im Testlauf übersprungen").into();
    }
    let title = crate::tr(&t.name).to_string();
    let r = crate::print_dialog::print_pdf(pdf, t, &title);
    if cfg!(target_os = "macos") {
        // macOS read the PDF into memory; other systems open the file in their viewer and keep it
        cleanup();
    }
    match r {
        Ok(()) => crate::tr("An den Druckdialog übergeben").into(),
        Err(e) => crate::trf!("Nicht gedruckt: {e}", e = e),
    }
}

fn amount_name(a: SharpenAmount) -> &'static str {
    match a {
        SharpenAmount::Off => "Aus",
        SharpenAmount::Low => "Niedrig",
        SharpenAmount::Standard => "Standard",
        SharpenAmount::High => "Hoch",
    }
}

fn media_name(m: Media) -> &'static str {
    match m {
        Media::Glossy => "Glanz",
        Media::Matte => "Matt",
    }
}

#[cfg(test)]
mod orientation_tests {
    use super::*;

    fn named(n: &str) -> Template {
        tpl::builtin().into_iter().find(|t| t.name == n).unwrap()
    }

    #[test]
    fn suggestion_follows_the_photos_and_leaves_manual_choice_alone() {
        let mut t = named("4 Fotos, A4");
        assert!(suggest_sheet_orientation(&mut t, false, [1.5, 1.5, 0.67]));
        assert_eq!(t.paper.orientation, Orientation::Landscape);
        assert_eq!(t.paper.size_mm(), (297.0, 210.0));
        // mostly portrait photos: back to portrait
        assert!(suggest_sheet_orientation(&mut t, false, [0.67, 0.67, 1.5]));
        assert_eq!(t.paper.orientation, Orientation::Portrait);
        // switched by hand: no suggestion
        assert!(!suggest_sheet_orientation(&mut t, true, [1.5, 1.5]));
        assert_eq!(t.paper.orientation, Orientation::Portrait);
        // single photo templates and contact sheets are left alone
        let mut one = named("1 Foto, A4, 10 mm Rand");
        assert!(!suggest_sheet_orientation(&mut one, false, [1.5]));
        let mut contact = named("Kontaktabzug A4 5 × 6");
        assert!(!suggest_sheet_orientation(&mut contact, false, [1.5, 1.5]));
    }

    #[test]
    fn cells_are_divided_for_the_sheet() {
        let mut t = named("2 Fotos, A4");
        assert_eq!((t.grid.rows, t.grid.columns), (2, 1));
        suggest_sheet_orientation(&mut t, false, [1.5, 1.5]);
        assert_eq!((t.grid.rows, t.grid.columns), (1, 2));
        let l = print::layout(&t, &[print::layout::PhotoSpec { width: 6000, height: 4000, caption: String::new(), offset: [0.0; 2] }], None, "");
        assert!(l.cell_w < l.cell_h * 1.0 + 200.0 && l.cell_w > 100.0 && l.cell_h > 150.0);
    }
}
