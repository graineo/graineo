//! One import for all file types, in a window with three fixed columns: **source** on the left (cards, cameras,
//! folders, counters and filters), the **selection** in the middle (grid grouped by capture day, RAW+JPEG as one cell,
//! large view with the space bar; for "Hinzufügen" the folders), **target and "apply on import"** on the right.
//! Nothing moves when the operation changes; what does not apply keeps its place empty. A switch at the top chooses
//! the kind of source (card or camera · folder · Lightroom catalog, which has its own dialog).
//!
//! The run can continue in the background ("Im Hintergrund weiter"): the window closes, the status in the title bar
//! shows the progress, a click there opens it again. At the end the window shows the result. An import interrupted by
//! a crash or quit is offered again at the next start ([`resume`]).
//!
//! Photos dragged onto the window also open this dialog (the photo's folder as source, the dragged photos checked, the
//! others in the folder not). Submodule of `library_ui` (accesses its fields).

use std::sync::atomic::Ordering;

use super::*;
use crate::dialog::{self, Answer, Buttons};
use library::card_import::{self as ci, Candidate, CollectionTarget, Mode, Options};
use library::import_select::{self as sel, Status};

#[path = "import_apply.rs"]
mod apply;
#[path = "import_camera.rs"]
pub(crate) mod camera;
#[path = "import_grid.rs"]
mod grid;
#[path = "import_resume.rs"]
mod resume;
#[path = "import_side.rs"]
mod side;
#[path = "import_thumbs.rs"]
mod thumbs;

/// Widths of the side columns (points); the middle takes the rest.
const LEFT_W: f32 = 222.0;
const RIGHT_W: f32 = 300.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Nothing,
    Existing(i64),
    New,
}

/// One cell of the grid: a photo (RAW+JPEG = one cell, the RAW first).
pub(super) struct Cell {
    members: Vec<usize>,
    on: bool,
    day: usize,
    /// against the library (RAW+JPEG: the RAW decides), see `library::import_select`
    status: Status,
}

/// Photos of one capture day.
pub(super) struct Day {
    tag: String,
    cells: Vec<usize>,
}

#[derive(Default)]
struct Scan {
    /// right after listing: all photos, capture dates follow
    early: Option<Vec<Candidate>>,
    /// Capture dates read (read, total)
    state_key: (usize, usize),
    finished: Option<Vec<Candidate>>,
    /// the source could not be read (a camera that refused its list)
    error: Option<String>,
    /// a camera: number of files in its list when it was read (a list that changed afterwards is read again)
    camera_files: Option<usize>,
}

#[derive(Default)]
struct Run {
    state_key: ci::State,
    /// Start of copying (for speed and remaining time)
    start: Option<std::time::Instant>,
    outcome: Option<ci::Outcome>,
    /// the library read right after the import (same thread): the grid is correct as soon as the dialog says "done"
    loaded: Option<super::Loaded>,
    /// develop preset applied afterwards: (name, photos)
    preset: Option<(String, usize)>,
    /// photos with the preset: their grid previews are rendered again (image, file, path, settings)
    redevelop: Vec<(i64, i64, PathBuf, develop::Settings)>,
}

/// The result shown in the window after the run.
struct Finished {
    verdict: dialog::Verdict,
    summary: String,
    rows: Vec<String>,
    details: Vec<String>,
    /// a card the photos came from (button "Karte auswerfen")
    card: Option<PathBuf>,
    ejected: Option<Result<(), String>>,
}

pub(super) struct Import {
    source: Option<PathBuf>,
    cards: Vec<PathBuf>,
    /// watches cards being inserted or taken out while the window is open
    watch: library::card_watch::CardWatch,
    /// drives that appeared and are still being read (names)
    reading: Vec<String>,
    /// the card that was the source and has been taken out (taken again when it comes back)
    card_gone: Option<PathBuf>,
    scan: Arc<Mutex<Scan>>,
    scanning: bool,
    cand: Vec<Candidate>,
    cells: Vec<Cell>,
    days: Vec<Day>,
    /// file types switched off and "RAW+JPEG: RAW only" (setting `importRawOnly`): filtered files are not shown,
    /// not chosen and not imported
    filter: sel::Filter,
    show_existing: bool,
    show_identical: bool,
    mode: Mode,
    target: Option<PathBuf>,
    /// the remembered main folder (setting `importDest`) is not reachable right now (drive not connected): it is
    /// taken as soon as it is back, until then the dialog says so instead of choosing another place by itself
    target_missing: Option<PathBuf>,
    by_date: bool,
    backup: Option<PathBuf>,
    collection: Choice,
    new_collection: String,
    /// put the new collection into the "Imports" set
    under_imports: bool,
    /// the name of the new collection has already been prefilled once (after that it belongs to the user)
    name_prefilled: bool,
    keywords: String,
    /// develop preset applied after the import ("" = none), also for JPEG/HEIC?
    preset: String,
    preset_jpeg: bool,
    stars: u8,
    label: u8,
    copyright: String,
    /// "Hinzufügen": folders and whether they are added
    folders: Vec<(PathBuf, usize, bool)>,
    folders_on: HashMap<PathBuf, bool>,
    /// the source lies inside a root folder of the library (otherwise "Hinzufügen" makes it a new one)
    source_in_library: bool,
    run: Option<Arc<Mutex<Run>>>,
    cancel: Arc<AtomicBool>,
    /// the run continues in the background, the window is hidden
    hidden: bool,
    finished: Option<Finished>,
    /// dragged photos: only these are checked after reading
    preselection: Option<HashSet<PathBuf>>,
    /// choose the mode by itself after reading the source (as long as the user has not chosen anything)
    mode_automatic: bool,
    thumbs: thumbs::Thumbs,
    /// last clicked cell (position in the visible order) for shift-click ranges
    last_click: Option<usize>,
    /// cell under the pointer (space bar opens it large)
    hovered: Option<usize>,
    /// large view of this cell
    big: Option<usize>,
    cell_w: f32,
    /// free space at the target: (target, bytes), read in the background
    free: Arc<Mutex<Option<(PathBuf, Option<u64>)>>>,
    /// cameras connected by USB without a drive (found only while the window exists)
    cameras: camera::Cameras,
    /// why the source could not be read
    source_error: Option<String>,
    /// a camera: number of files in its list that the candidates were read from
    camera_files: Option<usize>,
    /// kind of source chosen in the switch at the top (follows the source once one is read)
    kind: SourceKind,
}

fn read_mode() -> Mode {
    Mode::from_key(library::settings::value("importMode").as_str().unwrap_or("copy"))
}

/// Kind of source, as the switch at the top of the window shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SourceKind {
    /// memory card or camera
    Card,
    Folder,
    /// Lightroom catalog: its own dialog
    Lightroom,
}

/// Mode a source starts with (as long as the user has not chosen one in this window): a card or camera is copied
/// (from a camera only copying is possible; for a card the operation chosen last time, copy or move); a folder on a
/// disk is added where it is – an archive must not be doubled by accident.
pub(super) fn default_mode(kind: SourceKind, remembered_for_cards: Mode) -> Mode {
    match kind {
        SourceKind::Card if remembered_for_cards == Mode::InPlace => Mode::CopyFiles,
        SourceKind::Card => remembered_for_cards,
        SourceKind::Folder | SourceKind::Lightroom => Mode::InPlace,
    }
}

/// Why the import button is disabled, in plain words (shown next to it); `None`: it can start.
#[allow(clippy::too_many_arguments)]
pub(super) fn block_reason(source: bool, scanning: bool, photos: usize, mode: Mode, target: bool, target_unreachable: bool, chosen: usize, folders: usize, collection_name_missing: bool) -> Option<&'static str> {
    if !source {
        return Some("Wähle zuerst eine Quelle: Karte, Kamera oder Ordner");
    }
    if scanning {
        return Some("Liest die Fotos …");
    }
    if photos == 0 {
        return Some("In dieser Quelle sind keine Fotos");
    }
    if mode == Mode::InPlace {
        if folders == 0 {
            return Some("Wähle mindestens einen Ordner");
        }
    } else {
        if !target {
            return Some("Wähle zuerst einen Zielordner");
        }
        if target_unreachable {
            return Some("Der Zielordner ist gerade nicht erreichbar");
        }
        if chosen == 0 {
            return Some("Wähle mindestens ein Foto");
        }
    }
    if collection_name_missing {
        return Some("Gib der neuen Sammlung einen Namen");
    }
    None
}

pub(super) fn is_card(p: &Path) -> bool {
    library::card_watch::is_on_card(p)
}

/// Card or camera, otherwise a folder.
pub(super) fn source_kind(p: &Path) -> SourceKind {
    if is_card(p) || library::import_source::is_camera(p) {
        SourceKind::Card
    } else {
        SourceKind::Folder
    }
}

/// Recently used source folders (setting `importRecent`, newest first, at most 5; cards are not remembered).
fn recent_sources() -> Vec<PathBuf> {
    library::settings::value("importRecent").as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect()).unwrap_or_default()
}

fn remember_source(p: &Path) {
    if is_card(p) || library::import_source::is_camera(p) {
        return;
    }
    let mut v = recent_sources();
    v.retain(|x| x != p);
    v.insert(0, p.to_path_buf());
    v.truncate(5);
    library::settings::set("importRecent", serde_json::json!(v.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>()));
}

impl Import {
    /// Is this member of a cell part of the import (file type filter, "RAW+JPEG: RAW only")?
    fn member_on(&self, cell: &Cell, i: usize) -> bool {
        self.filter.includes(&self.cand, &cell.members, i)
    }

    fn cell_duplicate(&self, c: &Cell) -> bool {
        c.status == Status::There
    }
    fn cell_probable(&self, c: &Cell) -> bool {
        c.status == Status::Probable
    }
    fn cell_identical(&self, c: &Cell) -> bool {
        c.status == Status::Identical
    }

    /// Is the cell shown with the current filters?
    fn cell_visible(&self, ci: usize) -> bool {
        let c = &self.cells[ci];
        self.filter.shows(&self.cand, &c.members) && (self.show_existing || !self.cell_duplicate(c)) && (self.show_identical || !self.cell_identical(c))
    }

    /// Can the cell be checked (a file left that passes the filters and is not in the library yet)?
    fn selectable(&self, ci: usize) -> bool {
        !self.filter.files(&self.cand, &self.cells[ci].members).is_empty()
    }

    /// Cells that are imported: shown, checked, selectable.
    fn chosen(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.cells.len()).filter(|&ci| self.cells[ci].on && self.selectable(ci) && self.cell_visible(ci))
    }

    /// Files of the chosen cells (without the files filtered out, without those already in the library).
    fn chosen_files(&self) -> Vec<Candidate> {
        self.chosen().flat_map(|ci| self.filter.files(&self.cand, &self.cells[ci].members)).map(|i| self.cand[i].clone()).collect()
    }

    /// What the import takes: photos, files per type, bytes (the footer and the button count this).
    fn chosen_tally(&self) -> sel::Tally {
        sel::tally(&self.cand, self.chosen().map(|ci| self.cells[ci].members.as_slice()), &self.filter)
    }

    /// Group the candidates into cells and days; `keep`: checks of the cells shown so far (by the path of their first file).
    fn rebuild(&mut self, keep: Option<HashMap<PathBuf, bool>>) {
        let preselection = self.preselection.clone();
        // photos: RAW+JPEG of one shot = one cell, the RAW first
        let mut cells: Vec<Cell> = sel::group(&self.cand).into_iter().map(|members| Cell { status: sel::status(&self.cand, &members), members, on: false, day: 0 }).collect();
        // days, oldest first; within a day by path
        let mut by_day: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (ci, c) in cells.iter().enumerate() {
            by_day.entry(self.cand[c.members[0]].tag.clone()).or_default().push(ci);
        }
        let days: Vec<Day> = by_day.into_iter().map(|(tag, cells)| Day { tag, cells }).collect();
        for (d, day) in days.iter().enumerate() {
            for &ci in &day.cells {
                cells[ci].day = d;
            }
        }
        self.cells = cells;
        self.days = days;
        for ci in 0..self.cells.len() {
            let c = &self.cells[ci];
            let first = self.cand[c.members[0]].path.clone();
            let fresh = sel::preselected(c.status);
            let pre = preselection.as_ref().is_none_or(|v| c.members.iter().any(|&i| v.contains(&self.cand[i].path)));
            // a cell that turned out to be already there (or uncertain) loses its check; a new one keeps the user's choice
            let on = match keep.as_ref().and_then(|k| k.get(&first)) {
                Some(&was) => was && fresh,
                None => fresh && pre,
            };
            self.cells[ci].on = on;
        }
        // folders for "Hinzufügen" (the ones whose photos are all in the library start unchecked)
        self.folders = ci::folders_of(&self.cand);
        for (p, _, all_there) in &self.folders {
            self.folders_on.entry(p.clone()).or_insert(!*all_there);
        }
        self.last_click = None;
        self.big = None;
    }

    /// Positions of the visible cells in display order (day by day).
    fn visible_order(&self) -> Vec<usize> {
        self.days.iter().flat_map(|d| d.cells.iter().copied()).filter(|&ci| self.cell_visible(ci)).collect()
    }

    fn folders_chosen(&self) -> Vec<PathBuf> {
        self.folders.iter().filter(|(p, _, _)| self.folders_on.get(p).copied().unwrap_or(false)).map(|(p, _, _)| p.clone()).collect()
    }
}

impl LibraryView {
    /// Open the import window (memory card if one is inserted, otherwise empty). A run in the background: show it.
    pub fn open_import(&mut self, ctx: &egui::Context) {
        if let Some(imp) = &mut self.import {
            imp.hidden = false;
            return;
        }
        self.import_open_with(ctx, None, None);
    }

    /// Is the import window shown (a run in the background does not count: the app stays usable)?
    pub fn import_open(&self) -> bool {
        self.import.as_ref().is_some_and(|i| !i.hidden)
    }

    /// Show the window of an import running in the background (click on the status in the title bar).
    pub fn import_show(&mut self) {
        if let Some(imp) = &mut self.import {
            imp.hidden = false;
        }
    }

    /// Short text for the status bar while an import is running.
    pub fn import_label(&self) -> Option<String> {
        let imp = self.import.as_ref()?;
        let l = imp.run.as_ref()?.lock().unwrap();
        if l.outcome.is_some() || imp.finished.is_some() {
            return None;
        }
        let s = l.state_key;
        if s.waiting {
            return Some(if camera::is_camera(imp.source.as_deref()) { crate::tr("Import wartet auf die Kamera") } else { crate::tr("Import wartet auf das Laufwerk") }.to_string());
        }
        let mut t = crate::trf!("Import · {a} von {b}", a = dialog::number((s.file + 1).min(s.files)), b = dialog::number(s.files));
        if let Some(rest) = l.start.and_then(|t0| rest_secs(&s, t0.elapsed().as_secs_f64())) {
            t += &format!(" · {}", remaining_time(rest));
        }
        Some(t)
    }

    fn import_open_with(&mut self, ctx: &egui::Context, source: Option<PathBuf>, preselection: Option<HashSet<PathBuf>>) {
        // viewing a backup: importing is refused (the button is disabled, the band says how to go back)
        if library::backup_view::active().is_some() {
            return;
        }
        // the remembered main folder (also on the NAS or an external drive); only without one the largest root folder
        let remembered = library::settings::value("importDest").as_str().filter(|p| !p.is_empty()).map(PathBuf::from);
        let target_missing = remembered.clone().filter(|p| !crate::path_check::folder_exists(p));
        let target = match &remembered {
            Some(p) => target_missing.is_none().then(|| p.clone()),
            None => self.root_rows.iter().filter(|(s, _)| crate::path_check::folder_exists(Path::new(&s.path))).max_by_key(|(_, k)| self.nodes[*k].count).map(|(s, _)| PathBuf::from(&s.path)),
        };
        // cards come from the watcher (a drive that is still being mounted must not hold up the window); with no other
        // source the first card is taken as soon as it is there
        let c2 = ctx.clone();
        let watch = library::card_watch::CardWatch::start(move || c2.request_repaint());
        let mode_test = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_IMPORT_MODE").ok()).map(|m| Mode::from_key(&m));
        // staged thumbnails of an earlier window are no longer needed (a running import keeps its window)
        thumbs::clear_staging();
        let s = |k: &str| library::settings::value(k);
        let mut imp = Import {
            source: None,
            cards: Vec::new(),
            watch,
            reading: Vec::new(),
            card_gone: None,
            scan: Arc::default(),
            scanning: false,
            cand: Vec::new(),
            cells: Vec::new(),
            days: Vec::new(),
            filter: sel::Filter { raw_only: s("importRawOnly").as_bool().unwrap_or(true), ..Default::default() },
            show_existing: false,
            show_identical: false,
            mode: mode_test.unwrap_or_else(read_mode),
            target,
            target_missing,
            by_date: s("importByDate").as_bool().unwrap_or(true),
            backup: None,
            collection: Choice::Nothing,
            new_collection: String::new(),
            under_imports: s("importUnderFolder").as_bool().unwrap_or(true),
            name_prefilled: false,
            keywords: String::new(),
            preset: s("importPreset").as_str().unwrap_or("").to_string(),
            preset_jpeg: s("importPresetJpeg").as_bool().unwrap_or(false),
            stars: 0,
            label: 0,
            copyright: s("importCopyright").as_str().unwrap_or("").to_string(),
            folders: Vec::new(),
            folders_on: HashMap::new(),
            source_in_library: false,
            run: None,
            cancel: Arc::new(AtomicBool::new(false)),
            hidden: false,
            finished: None,
            preselection,
            mode_automatic: mode_test.is_none(),
            thumbs: thumbs::Thumbs::new(ctx),
            last_click: None,
            hovered: None,
            big: None,
            cell_w: s("importCellSize").as_f64().map(|v| v as f32).unwrap_or(128.0).clamp(96.0, 220.0),
            free: Arc::default(),
            cameras: camera::Cameras::new(ctx),
            source_error: None,
            camera_files: None,
            kind: SourceKind::Card,
        };
        if let Some(q) = source {
            self.import_start_scan(&mut imp, q, ctx);
        }
        self.import = Some(imp);
    }

    fn import_start_scan(&mut self, imp: &mut Import, q: PathBuf, ctx: &egui::Context) {
        crate::import_bench::scan_started();
        imp.kind = source_kind(&q);
        imp.source = Some(q.clone());
        imp.scanning = true;
        imp.cand.clear();
        imp.cells.clear();
        imp.days.clear();
        imp.folders.clear();
        imp.folders_on.clear();
        imp.last_click = None;
        imp.big = None;
        imp.source_error = None;
        imp.card_gone = None;
        imp.camera_files = None;
        imp.source_in_library = self.db.as_ref().and_then(|db| db.root_to_path(&q)).is_some();
        let scan = Arc::new(Mutex::new(Scan::default()));
        imp.scan = scan.clone();
        let ctx = ctx.clone();
        if library::import_source::is_camera(&q) {
            camera_scan(imp, &q, scan, ctx);
            return;
        }
        std::thread::Builder::new()
            .name("import-scan".into())
            .spawn(move || {
                let (s1, c1, s2, c2) = (scan.clone(), ctx.clone(), scan.clone(), ctx.clone());
                let early = move |v: Vec<Candidate>| {
                    s1.lock().unwrap().early = Some(v);
                    c1.request_repaint();
                };
                // test aid (only with GRAINEO_DATA_DIR): GRAINEO_IMPORT_SCAN_DELAY_MS slows reading per file (screenshots of the check)
                let delay = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var("GRAINEO_IMPORT_SCAN_DELAY_MS").ok()).and_then(|v| v.parse::<u64>().ok());
                let progress = move |i: usize, n: usize| {
                    s2.lock().unwrap().state_key = (i, n);
                    c2.request_repaint();
                    if let Some(ms) = delay {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                    }
                };
                let k = Db::standard().map(|db| ci::scan_with(&db, &q, &early, &progress)).unwrap_or_default();
                scan.lock().unwrap().finished = Some(k);
                ctx.request_repaint();
            })
            .ok();
    }

    /// The import window with this folder as the source ("Not in the library" → "Import …" of a row).
    pub(super) fn import_folder(&mut self, ctx: &egui::Context, folder: PathBuf) {
        self.import_open_with(ctx, Some(folder), None);
    }

    /// Test hook: preset the source and read it.
    pub fn test_import_source(&mut self, ctx: &egui::Context, q: PathBuf) {
        self.import_open_with(ctx, Some(q), None);
    }

    /// Photos or folders dragged onto the window: already in the catalog → show them there, otherwise open the import.
    /// Returns: was any of it recognised as a photo (otherwise the drop belongs to someone else, e.g. film profiles)?
    pub fn import_store(&mut self, ctx: &egui::Context, paths: &[PathBuf]) -> bool {
        let is_photo = |p: &Path| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.') && library::format_from_extension(n).is_some());
        let files: Vec<PathBuf> = paths.iter().filter(|p| p.is_file() && is_photo(p)).cloned().collect();
        let folder: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.is_dir() && std::fs::read_dir(p).is_ok_and(|rd| rd.flatten().any(|e| e.path().is_file() && is_photo(&e.path()))))
            .cloned()
            .collect();
        if files.is_empty() && folder.is_empty() {
            return false;
        }
        if let (Some(db), true) = (&self.db, folder.is_empty()) {
            let images: Vec<i64> = files.iter().filter_map(|p| db.image_to_path(p)).collect();
            if images.len() == files.len() {
                // everything already in the catalog: jump to the first dragged photo
                self.show_folder(images[0]);
                self.set_active(images[0]);
                return true;
            }
        }
        // an import running in the background keeps its window
        if self.import.as_ref().is_some_and(|i| i.run.is_some() && i.finished.is_none()) {
            self.import_show();
            return true;
        }
        if let Some(first) = folder.first() {
            self.import_open_with(ctx, Some(first.clone()), None);
        } else {
            let source = files[0].parent().map(Path::to_path_buf);
            self.import_open_with(ctx, source, Some(files.into_iter().collect()));
        }
        true
    }

    /// Take over the results of reading the source.
    fn import_take_scan(&mut self, imp: &mut Import) {
        if !imp.scanning {
            return;
        }
        let (early, finished) = {
            let mut g = imp.scan.lock().unwrap();
            (g.early.take(), g.finished.take())
        };
        // final (with capture dates and checksums): checks of the list already shown stay, whatever turns out to be
        // already present (or uncertain) loses its check
        {
            let mut g = imp.scan.lock().unwrap();
            if let Some(e) = g.error.take() {
                imp.source_error = Some(e);
            }
            if let Some(n) = g.camera_files.take() {
                imp.camera_files = Some(n);
            }
        }
        if let (Some(k), false) = (&finished, imp.cand.is_empty()) {
            // only the checks of photos that were new in the first look count as the user's choice (one provisionally
            // "already there" by name and size starts checked if it turns out to be new)
            let keep: HashMap<PathBuf, bool> = imp.cells.iter().filter(|c| sel::preselected(c.status)).map(|c| (imp.cand[c.members[0]].path.clone(), c.on)).collect();
            imp.cand = k.clone();
            imp.rebuild(Some(keep));
            imp.scanning = false;
            imp.preselection = None;
            return;
        }
        let k = if imp.cand.is_empty() { finished.clone().or(early) } else { None };
        let Some(k) = k else { return };
        imp.scanning = finished.is_none();
        imp.cand = k;
        imp.rebuild(None);
        // Dragged photos partially selected: "Hinzufügen" would always take the whole folder, so copy right away
        if let Some(v) = &imp.preselection {
            if imp.cand.iter().any(|c| !c.duplicate && !v.contains(&c.path)) {
                imp.mode = Mode::CopyFiles;
                imp.mode_automatic = false;
            }
        }
        if imp.mode_automatic {
            if let Some(q) = imp.source.clone() {
                imp.mode = default_mode(source_kind(&q), read_mode());
            }
        }
        if !imp.scanning {
            imp.preselection = None;
        }
    }

    /// Cards inserted or taken out while the window is open: the list follows the watcher. A card that appears while no
    /// source is chosen (or the card that was taken out comes back) is read right away; the card being read or shown
    /// that goes away empties the selection (nothing of it can be imported any more) and says so. A running import
    /// is not touched (it waits for its drive by itself).
    fn import_cards(&mut self, imp: &mut Import, ctx: &egui::Context) {
        let v = imp.watch.volumes();
        imp.reading = v.reading;
        if v.cards == imp.cards {
            return;
        }
        let gone: Vec<PathBuf> = imp.cards.iter().filter(|c| !v.cards.contains(c)).cloned().collect();
        let came: Vec<PathBuf> = v.cards.iter().filter(|c| !imp.cards.contains(c)).cloned().collect();
        crate::log(&format!("import: cards {:?} → {:?}", imp.cards, v.cards));
        imp.cards = v.cards;
        if imp.run.is_some() || imp.finished.is_some() {
            return;
        }
        if let Some(root) = imp.source.as_ref().and_then(|q| gone.iter().find(|r| q.starts_with(r))).cloned() {
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            imp.scan = Arc::default();
            imp.scanning = false;
            imp.source = None;
            imp.cand.clear();
            imp.cells.clear();
            imp.days.clear();
            imp.folders.clear();
            imp.folders_on.clear();
            imp.last_click = None;
            imp.big = None;
            imp.hovered = None;
            imp.preselection = None;
            imp.source_error = Some(crate::trf!("Die Karte „{n}“ wurde entfernt.", n = name));
            imp.card_gone = Some(root);
        }
        let back = came.iter().find(|c| imp.card_gone.as_ref() == Some(*c)).cloned();
        if let Some(c) = back.or_else(|| came.first().cloned().filter(|_| imp.source.is_none())) {
            self.import_start_scan(imp, c, ctx);
        }
    }

    /// The window ("Fotos importieren"), every frame; also receives the result of the run.
    pub(super) fn import_dialog(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if crate::import_bench::frame(ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let Some(mut imp) = self.import.take() else {
            self.import_resume(ctx);
            return;
        };
        imp.thumbs.poll(ctx);
        if crate::import_bench::active() {
            crate::import_bench::listed(imp.cand.len(), !imp.scanning);
        }
        // Run finished?
        let finished = imp.run.as_ref().and_then(|l| {
            let mut g = l.lock().unwrap();
            g.outcome.take().map(|e| (e, g.loaded.take(), g.preset.take(), std::mem::take(&mut g.redevelop)))
        });
        if let Some(e) = finished {
            self.import_finished(&mut imp, e, actions);
        }
        self.import_take_scan(&mut imp);
        self.import_cards(&mut imp, ctx);
        // the camera's list changed after it was read (more files announced, or a photo taken meanwhile): read it again
        if let (false, None, Some(n), Some(library::import_source::CamState::Ready(m))) = (imp.scanning, &imp.run, imp.camera_files, camera::chosen_state(&imp)) {
            if m != n {
                if let Some(q) = imp.source.clone() {
                    crate::log(&format!("import: camera list changed ({n} → {m}), reading again"));
                    self.import_start_scan(&mut imp, q, ctx);
                }
            }
        }
        if imp.hidden {
            if imp.run.is_some() && imp.finished.is_none() {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
            self.import = Some(imp);
            return;
        }
        // Results of the system dialogs
        if let Some(d) = crate::file_dialog::fetch_one("imp-quelle") {
            remember_source(&d);
            self.import_start_scan(&mut imp, d, ctx);
        }
        if let Some(d) = crate::file_dialog::fetch_one("imp-ziel") {
            imp.target = Some(d);
            imp.target_missing = None;
        }
        if let Some(d) = crate::file_dialog::fetch_one("imp-sicherung") {
            imp.backup = Some(d);
        }
        // remembered main folder back (drive connected): take it
        if let Some(p) = imp.target_missing.clone() {
            if crate::path_check::folder_exists(&p) {
                if imp.target.is_none() {
                    imp.target = Some(p);
                }
                imp.target_missing = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs(2));
            }
        }
        let copies = imp.mode != Mode::InPlace;
        // chosen target not reachable (drive disconnected in the meantime)
        let target_gone = copies && imp.target.as_ref().is_some_and(|p| !crate::path_check::folder_exists(p));
        if target_gone {
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
        let running = imp.run.is_some() && imp.finished.is_none();
        let done = imp.finished.is_some();
        let in_place = imp.mode == Mode::InPlace;
        let chosen = imp.chosen().count();
        let folders = imp.folders_chosen().len();
        let reason = block_reason(imp.source.is_some(), imp.scanning, imp.cand.len(), imp.mode, imp.target.is_some(), target_gone, chosen, folders, imp.collection == Choice::New && imp.new_collection.trim().is_empty());
        let can = !running && !done && reason.is_none();
        let photos = |n: usize| dialog::count(n, crate::tr("Foto"), crate::tr("Fotos"));
        // the button names what happens
        let button = match imp.mode {
            Mode::InPlace => crate::trf!("{a} hinzufügen", a = dialog::count(folders, crate::tr("Ordner"), crate::tr("Ordner"))),
            Mode::CopyFiles => crate::trf!("{a} kopieren", a = photos(chosen)),
            Mode::MoveFiles => crate::trf!("{a} verschieben", a = photos(chosen)),
        };
        let mut kind_pick: Option<SourceKind> = None;
        let (w, h) = window_size(ctx);
        let collections: Vec<(i64, String)> = self.collections.iter().filter(|s| s.kind == 0 && Some(s.id) != self.quick_id).map(|s| (s.id, s.name.clone())).collect();
        // large view: Esc belongs to it (it would close the window)
        if imp.big.is_some() && !running && !done && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            imp.big = None;
        }
        let mut pick = side::Picks::default();
        let mut eject = false;
        let buttons = if running {
            Buttons { cancel: "Abbrechen", primary: Some(("Im Hintergrund weiter", dialog::Style::Normal, true)) }
        } else if done {
            Buttons { cancel: "Schließen", primary: Some(("Zuletzt importiert zeigen", dialog::Style::Normal, true)) }
        } else {
            Buttons::confirm(&button, can)
        };
        let answer = {
            let imp = &mut imp;
            dialog::show(
                ctx,
                "import",
                "Fotos importieren",
                w,
                |ui| {
                    // the kind of source, clearly at the top: card or camera · folder · Lightroom catalog (own dialog)
                    ui.add_enabled_ui(!running && !done, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(crate::tr("Quelle")).color(lr::TEXT_DIM));
                            ui.add_space(6.0);
                            let mut k = imp.kind;
                            let opts = [(SourceKind::Card, crate::tr("Karte oder Kamera")), (SourceKind::Folder, crate::tr("Ordner")), (SourceKind::Lightroom, crate::tr("Lightroom-Katalog"))];
                            if lr::segmented_blue(ui, &mut k, &opts) {
                                kind_pick = Some(k);
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let mid_w = w - LEFT_W - RIGHT_W - 2.0 * 14.0;
                        column(ui, LEFT_W, h, |ui| {
                            ui.add_enabled_ui(!running && !done, |ui| side::left(ui, imp, &mut pick));
                        });
                        divider(ui, h);
                        column(ui, mid_w, h, |ui| {
                            if let Some(f) = &imp.finished {
                                eject = result_view(ui, f);
                            } else if let Some(l) = &imp.run {
                                progress_view(ui, l, imp.mode, camera::is_camera(imp.source.as_deref()));
                            } else if in_place {
                                side::folders(ui, imp);
                            } else if imp.big.is_some() {
                                grid::big_view(ui, imp);
                            } else {
                                grid::show(ui, imp, h);
                            }
                        });
                        divider(ui, h);
                        column(ui, RIGHT_W, h, |ui| {
                            ui.add_enabled_ui(!running && !done, |ui| side::right(ui, imp, &collections, target_gone, &mut pick));
                        });
                    });
                    // footer: what is chosen; "all/none" for the grid
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.set_height(22.0);
                        ui.add_enabled_ui(!running && !done && !imp.cand.is_empty(), |ui| {
                            let t = if in_place {
                                crate::trf!("{a} von {b} Ordnern gewählt", a = dialog::number(folders), b = dialog::number(imp.folders.len()))
                            } else {
                                let total = (0..imp.cells.len()).filter(|&ci| imp.selectable(ci) && imp.cell_visible(ci)).count();
                                let t = imp.chosen_tally();
                                let mut s = crate::trf!("{a} von {b} Fotos gewählt", a = dialog::number(chosen), b = dialog::number(total));
                                if t.photos > 0 {
                                    s += &format!(" · {} · {}", files_text(&t.files), dialog::size(t.bytes));
                                }
                                s
                            };
                            ui.label(egui::RichText::new(t).color(lr::TEXT));
                            ui.add_space(12.0);
                            if ui.button(crate::tr("Alle")).clicked() {
                                grid::check_all(imp, true);
                            }
                            if ui.button(crate::tr("Keine")).clicked() {
                                grid::check_all(imp, false);
                            }
                        });
                        // a disabled import button says why, in plain words, next to it
                        if let Some(r) = reason.filter(|_| !running && !done) {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(egui::RichText::new(crate::tr(r)).color(Color32::from_rgb(0xe0, 0xb0, 0x40)));
                            });
                        }
                    });
                },
                buttons,
            )
        };
        match kind_pick {
            Some(SourceKind::Lightroom) => {
                // the Lightroom catalog has its own dialog (reads only, checks first)
                self.lr_window = true;
                if imp.run.is_none() {
                    return;
                }
            }
            Some(k) => {
                imp.kind = k;
                // to the other kind: what was read from the other kind of source goes
                if imp.source.as_deref().is_some_and(|q| source_kind(q) != k) {
                    import_clear_source(&mut imp);
                }
                match k {
                    SourceKind::Card => {
                        if let Some(c) = imp.cards.first().cloned() {
                            self.import_start_scan(&mut imp, c, ctx);
                        }
                    }
                    _ => {
                        if imp.source.is_none() {
                            pick.source = true;
                        }
                    }
                }
            }
            None => {}
        }
        if pick.source {
            crate::file_dialog::folder(ctx, "imp-quelle", |d| d.set_title(crate::tr("Quelle für den Import")));
        }
        if pick.target {
            let cur = imp.target.clone();
            crate::file_dialog::target_folder(ctx, "imp-ziel", move |d| {
                let d = d.set_title(crate::tr("Zielordner für den Import"));
                match cur {
                    Some(c) => d.set_directory(c),
                    None => d,
                }
            });
        }
        if pick.backup {
            crate::file_dialog::target_folder(ctx, "imp-sicherung", |d| d.set_title(crate::tr("Ordner für die zweite Kopie")));
        }
        if let Some(k) = pick.card {
            remember_source(&k);
            self.import_start_scan(&mut imp, k, ctx);
        }
        // "Neu einlesen": new session, then the list again
        if let Some(id) = pick.camera_reload.take() {
            imp.cameras.reload(&id);
            self.import_start_scan(&mut imp, library::import_source::camera_path(&id, ""), ctx);
        }
        // a camera chosen, or the only camera that appeared while nothing else is there
        if let Some(id) = pick.camera.take().or_else(|| camera::only_camera(&imp)) {
            self.import_start_scan(&mut imp, library::import_source::camera_path(&id, ""), ctx);
        }
        if eject {
            if let Some(f) = &mut imp.finished {
                f.ejected = f.card.as_deref().map(eject_card);
            }
        }
        self.import_test_hooks(ctx, &mut imp);
        // Test hook (only with GRAINEO_DATA_DIR): GRAINEO_IMPORT_YES=<s> confirms, GRAINEO_IMPORT_BG=<s> continues in the background
        let answer = match crate::env::var("GRAINEO_IMPORT_YES").ok().and_then(|v| v.parse::<f64>().ok()) {
            Some(t) if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && answer == Answer::Open && can => {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                if ctx.input(|i| i.time) > t {
                    Answer::Primary
                } else {
                    answer
                }
            }
            _ => answer,
        };
        let answer = match crate::env::var("GRAINEO_IMPORT_BG").ok().and_then(|v| v.parse::<f64>().ok()) {
            Some(t) if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && answer == Answer::Open && running && ctx.input(|i| i.time) > t => Answer::Primary,
            _ => answer,
        };
        // GRAINEO_IMPORT_SHOW=<s>: after the result "Zuletzt importiert zeigen" (screenshots of the grid after an import)
        let answer = match crate::env::var("GRAINEO_IMPORT_SHOW").ok().and_then(|v| v.parse::<f64>().ok()) {
            Some(t) if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && answer == Answer::Open && done && ctx.input(|i| i.time) > t => Answer::Primary,
            _ => answer,
        };
        match answer {
            Answer::Cancel if running => {
                imp.cancel.store(true, Ordering::Relaxed);
                self.import = Some(imp);
            }
            // closed (also after the result): the window and its thumbnails go away
            Answer::Cancel => {}
            Answer::Primary if running => {
                imp.hidden = true;
                self.import = Some(imp);
            }
            Answer::Primary if done => {
                self.set_source(Source::Recent);
            }
            Answer::Primary if can => {
                self.import_start(&mut imp, ctx);
                self.import = Some(imp);
            }
            _ => self.import = Some(imp),
        }
    }

    /// Start the run in the background.
    fn import_start(&mut self, imp: &mut Import, ctx: &egui::Context) {
        let candidates: Vec<Candidate> = if imp.mode == Mode::InPlace { imp.cand.clone() } else { imp.chosen_files() };
        let keywords: Vec<String> = imp.keywords.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let opt = Options {
            mode: imp.mode,
            source: imp.source.clone(),
            target: imp.target.clone(),
            by_date: imp.by_date,
            backup: imp.backup.clone(),
            collection: match imp.collection {
                Choice::Nothing => CollectionTarget::Nothing,
                Choice::Existing(id) => CollectionTarget::Existing(id),
                Choice::New => CollectionTarget::New(imp.new_collection.trim().to_string()),
            },
            parent_name: (imp.collection == Choice::New && imp.under_imports).then(|| "Importe".to_string()),
            keywords,
            folders: (imp.mode == Mode::InPlace).then(|| imp.folders_chosen()),
            stars: imp.stars,
            label: imp.label,
            copyright: imp.copyright.trim().to_string(),
            journal: true,
        };
        let set = library::settings::set;
        let camera = camera::is_camera(imp.source.as_deref());
        // remembered only for cards (copy or move); from a camera only copying is possible, a folder starts with
        // "Hinzufügen" every time
        if !camera && imp.kind == SourceKind::Card {
            set("importMode", serde_json::json!(imp.mode.key()));
        }
        if let Some(z) = &imp.target {
            set("importDest", serde_json::json!(z.to_string_lossy()));
        }
        set("importByDate", serde_json::json!(imp.by_date));
        set("importUnderFolder", serde_json::json!(imp.under_imports));
        set("importPreset", serde_json::json!(imp.preset));
        set("importPresetJpeg", serde_json::json!(imp.preset_jpeg));
        set("importCopyright", serde_json::json!(imp.copyright.trim()));
        set("importCellSize", serde_json::json!(imp.cell_w));
        let files = ci::photos(&candidates.iter().filter(|k| !k.duplicate).cloned().collect::<Vec<_>>()).len();
        let run = Arc::new(Mutex::new(Run { state_key: ci::State { files, ..Default::default() }, start: Some(std::time::Instant::now()), ..Default::default() }));
        imp.run = Some(run.clone());
        crate::import_bench::run_started(files);
        imp.big = None;
        imp.cancel.store(false, Ordering::Relaxed);
        let (cancel, ctx) = (imp.cancel.clone(), ctx.clone());
        let ticket = self.load_ticket();
        let preset = apply::chosen(&imp.preset).map(|p| (p, imp.preset_jpeg));
        let src = imp.source.as_deref().and_then(library::import_source::parts).and_then(|(id, _)| if camera { imp.cameras.open(&id) } else { None });
        std::thread::Builder::new()
            .name("import".into())
            .spawn(move || {
                let open = || Db::standard().map_err(|e| e.to_string());
                // originals of a moved photo: network drive → its own trash folder, otherwise the Trash
                let system = |d: &[PathBuf]| trash::delete_all(d).map_err(|e| e.to_string());
                let source_root = opt.source.clone();
                let dispose = move |d: &[PathBuf]| ci::dispose_originals(d, source_root.as_deref(), &library::network_trash::is_network, &system);
                let l2 = run.clone();
                let c2 = ctx.clone();
                let progress = move |s: ci::State| {
                    l2.lock().unwrap().state_key = s;
                    c2.request_repaint();
                };
                // a panic must not leave the import window "running" for good (copies and journal are safe: the
                // import journal resumes or reports at the next start)
                let e = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &src {
                    Some(s) => ci::run_import_from(&open, &**s, &candidates, &opt, &cancel, &progress, None),
                    None => ci::run_import(&open, &candidates, &opt, &dispose, &cancel, &progress),
                }))
                .unwrap_or_else(|_| ci::Outcome { catalog_error: Some(crate::tr("Interner Fehler beim Import – bitte die Bibliothek prüfen").to_string()), ..Default::default() });
                // develop preset (looks, film recipes) on the photos just imported, like pasting settings
                let changed = preset.as_ref().map(|(p, jpeg)| apply::apply(p, *jpeg, &e.files, &e.images)).unwrap_or_default();
                let applied = preset.map(|(p, _)| (p.name.clone(), changed.len()));
                let loaded = Db::read_only(&library::db_path()).ok().and_then(|db| ticket.load(&db).ok());
                let mut r = run.lock().unwrap_or_else(|x| x.into_inner());
                r.loaded = loaded;
                r.preset = applied;
                r.redevelop = changed;
                r.outcome = Some(e);
                drop(r);
                ctx.request_repaint();
            })
            .ok();
    }

    /// Run finished: re-read the catalog and show the result in the window (traffic light: green done, yellow with notes,
    /// red with errors; errors in plain text). Hidden in the background: the window comes back with the result.
    fn import_finished(&mut self, imp: &mut Import, (e, loaded, preset, redevelop): (ci::Outcome, Option<super::Loaded>, Option<(String, usize)>, Vec<(i64, i64, PathBuf, develop::Settings)>), actions: &mut Vec<Action>) {
        let k = &e.copy_file;
        let photos = |n: usize| dialog::count(n, crate::tr("Foto"), crate::tr("Fotos"));
        let files = |n: usize| dialog::count(n, crate::tr("Datei"), crate::tr("Dateien"));
        let secs = (k.duration_ms / 1000) as u64;
        let mut summary = match imp.mode {
            Mode::InPlace => crate::trf!("{a} hinzugefügt (bleiben am Ort)", a = photos(e.added)),
            Mode::MoveFiles => crate::trf!("{a} verschoben und geprüft ({b}, {c})", a = files(k.copied), b = dialog::size(k.bytes), c = dialog::duration(std::time::Duration::from_secs(secs))),
            _ => crate::trf!("{a} kopiert und geprüft ({b}, {c})", a = files(k.copied), b = dialog::size(k.bytes), c = dialog::duration(std::time::Duration::from_secs(secs))),
        };
        let errors = k.error.iter().filter(|x| x.as_str() != "abgebrochen").count();
        if errors > 0 {
            summary += &crate::trf!(" · {a} nicht importiert", a = errors);
        }
        let mut rows: Vec<String> = Vec::new();
        if k.cancelled {
            rows.push(crate::tr("Abgebrochen – schon Kopiertes ist geprüft und eingetragen.").to_string());
        }
        if k.moved > 0 {
            rows.push(crate::trf!("{a} verschoben, die Originale liegen im Papierkorb", a = photos(k.moved)));
            if imp.source.as_deref().is_some_and(is_card) {
                rows.push(crate::tr("Der Platz auf der Karte wird erst frei, wenn der Papierkorb geleert ist.").to_string());
            }
        }
        if k.duplicate > 0 {
            rows.push(crate::trf!("{a} schon in der Bibliothek, übersprungen", a = files(k.duplicate)));
        }
        let identical = imp.cells.iter().filter(|c| imp.cell_identical(c) && !c.on).count();
        if identical > 0 && imp.mode != Mode::InPlace {
            rows.push(crate::trf!("{a} identische Kopien übersprungen", a = dialog::number(identical)));
        }
        if k.backed_up > 0 {
            rows.push(crate::trf!("{a} zusätzlich als zweite Kopie gesichert", a = files(k.backed_up)));
        }
        let from_camera = camera::is_camera(imp.source.as_deref());
        if k.waited_secs > 0 {
            let d = dialog::duration(std::time::Duration::from_secs(k.waited_secs));
            rows.push(if from_camera { crate::trf!("{a} auf die Kamera gewartet", a = d) } else { crate::trf!("{a} auf das Laufwerk gewartet", a = d) });
        }
        if from_camera && k.copied > 0 {
            rows.push(crate::tr("Auf der Kamera ist alles unverändert geblieben.").to_string());
        }
        if let Some((n, m)) = &e.collection {
            rows.push(crate::trf!("in der Sammlung „{n}“ ({m})", n = n, m = m));
        }
        if let Some((n, m)) = &preset {
            rows.push(crate::trf!("Vorgabe „{n}“ auf {m} angewendet", n = crate::tr(n), m = photos(*m)));
        }
        if let Some(x) = &e.catalog_error {
            rows.push(crate::trf!("Die Fotos liegen fertig im Ziel, stehen aber noch nicht in der Bibliothek ({x}).\nSie erscheinen unter „Nicht in der Bibliothek“ und lassen sich dort mit einem Klick aufnehmen.", x = x));
        }
        rows.extend(e.notes.iter().map(|n| crate::menu::i18n::tr_msg(n).into_owned()));
        let mut details: Vec<String> = k.error.iter().filter(|x| x.as_str() != "abgebrochen").take(50).cloned().collect();
        if errors > 50 {
            details.push(crate::trf!("… und {a} weitere Fehler", a = errors - 50));
        }
        let verdict = if errors > 0 {
            dialog::Verdict::Red
        } else if e.catalog_error.is_some() || !e.notes.is_empty() || k.cancelled {
            dialog::Verdict::Yellow
        } else {
            dialog::Verdict::Green
        };
        self.not_in_catalog.trigger_research();
        match loaded {
            // read in the import thread: the new photos are there right away
            Some(l) => {
                self.adopt_load(l);
                self.import_previews(k);
            }
            None => {
                let k = k.clone();
                self.reload_then(move |b, _| b.import_previews(&k));
            }
        }
        if e.catalog_error.is_some() {
            self.set_source(Source::NotInCatalog);
        }
        // the preset shows in the grid right away: the previews of these photos are rendered again in the background
        if !redevelop.is_empty() {
            actions.push(Action::Redevelop(redevelop));
        }
        let card = imp.source.clone().filter(|q| is_card(q) && imp.mode != Mode::InPlace);
        crate::import_bench::run_finished(e.added, errors);
        imp.finished = Some(Finished { verdict, summary, rows, details, card, ejected: None });
        imp.hidden = false;
    }

    /// Previews of the photos just imported, right away and before the rest of the background: the thumbnail the import
    /// window already read (staged JPEG of the small level), otherwise from the file on the card while it is still there
    /// (same bytes, checked by the copy; reading the card is faster than the NAS), afterwards from the copy. First the
    /// small ones, then the offline copies of the photos on other drives (the development for RAWs).
    fn import_previews(&mut self, k: &ci::Report) {
        let Some(e) = &self.generator else { return };
        if k.target_files.is_empty() {
            return;
        }
        let by_path: HashMap<String, usize> = self.rows.iter().enumerate().map(|(i, z)| (library::nfc(&self.path_of(z).to_string_lossy()), i)).collect();
        let (small, large) = (preview::small(), preview::large());
        let (mut smalls, mut larges) = (Vec::new(), Vec::new());
        let mut staged_n = 0;
        // RAW+JPEG: the window read only the JPEG half; the same photo serves the RAW's small preview too
        let mut staged_pair: HashMap<(PathBuf, String), PathBuf> = HashMap::new();
        for source in &k.source_files {
            if let Some(p) = thumbs::staged(source) {
                staged_pair.insert(ci::pair_key(source), p);
            }
        }
        for (target, source) in k.target_files.iter().zip(&k.source_files) {
            let Some(&i) = by_path.get(&library::nfc(&target.to_string_lossy())) else { continue };
            let mut job = self.task(&self.rows[i], small);
            if source.is_file() {
                job.path = source.clone();
            }
            if large > small && !library::platform::on_startup_volume(target) {
                larges.push(Job { level: large, ..job.clone() });
            }
            // the thumbnail of the window: already upright, a JPEG of exactly this level
            if let Some(p) = thumbs::staged(source).or_else(|| staged_pair.get(&ci::pair_key(source)).cloned()) {
                job.path = p;
                job.format = "JPG".into();
                job.orientation = None;
                staged_n += 1;
            }
            smalls.push(job);
        }
        crate::log(&format!("import: {} small previews ({staged_n} from the import window) and {} offline copies queued", smalls.len(), larges.len()));
        smalls.extend(larges);
        e.soon(smalls);
    }

    /// Test hooks for screenshots (only with GRAINEO_DATA_DIR): GRAINEO_IMPORT_VIEW=large opens the large view of the
    /// first photo, `inplace` switches to "Hinzufügen".
    fn import_test_hooks(&mut self, ctx: &egui::Context, imp: &mut Import) {
        if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || imp.scanning || imp.cand.is_empty() {
            return;
        }
        let Ok(v) = crate::env::var("GRAINEO_IMPORT_VIEW") else { return };
        let id = egui::Id::unique("import-testansicht");
        if ctx.data(|d| d.get_temp::<bool>(id)).is_some() {
            return;
        }
        ctx.data_mut(|d| d.insert_temp(id, true));
        match v.as_str() {
            "large" => imp.big = imp.visible_order().first().copied(),
            "inplace" => {
                imp.mode = Mode::InPlace;
                imp.mode_automatic = false;
            }
            _ => {}
        }
    }
}

/// Forget the source and what was read from it (another kind of source chosen at the top).
fn import_clear_source(imp: &mut Import) {
    imp.scan = Arc::default();
    imp.scanning = false;
    imp.source = None;
    imp.cand.clear();
    imp.cells.clear();
    imp.days.clear();
    imp.folders.clear();
    imp.folders_on.clear();
    imp.last_click = None;
    imp.big = None;
    imp.hovered = None;
    imp.preselection = None;
    imp.source_error = None;
    imp.mode_automatic = true;
}

/// Read a camera as source: wait until it is connected and has listed its files (the source column shows how far it
/// is), then the photos as for a card. Ends by itself when the window closes or another source is chosen.
fn camera_scan(imp: &mut Import, q: &Path, scan: Arc<Mutex<Scan>>, ctx: egui::Context) {
    let Some((id, _)) = library::import_source::parts(q) else { return };
    let Some(src) = imp.cameras.open(&id) else {
        imp.scanning = false;
        imp.source_error = Some(crate::tr("Kamera nicht gefunden").to_string());
        return;
    };
    let weak = Arc::downgrade(&scan);
    drop(scan);
    std::thread::Builder::new()
        .name("import-kamera".into())
        .spawn(move || {
            library::preview::low_priority_thread();
            loop {
                if weak.strong_count() == 0 {
                    return;
                }
                if src.connected() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            let Some(scan) = weak.upgrade() else { return };
            let (s1, c1, s2, c2) = (scan.clone(), ctx.clone(), scan.clone(), ctx.clone());
            let early = move |v: Vec<Candidate>| {
                s1.lock().unwrap().early = Some(v);
                c1.request_repaint();
            };
            let progress = move |i: usize, n: usize| {
                s2.lock().unwrap().state_key = (i, n);
                c2.request_repaint();
            };
            scan.lock().unwrap().camera_files = src.list().ok().map(|l| l.len());
            let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| library::import_source::scan(&db, &*src, &early, &progress));
            let mut g = scan.lock().unwrap();
            match r {
                Ok(k) => g.finished = Some(k),
                Err(e) => {
                    g.error = Some(e);
                    g.finished = Some(Vec::new());
                }
            }
            drop(g);
            ctx.request_repaint();
        })
        .ok();
}

/// Size of the window content: fixed per screen, never changing with the contents (no layout jumps).
fn window_size(ctx: &egui::Context) -> (f32, f32) {
    let r = ctx.content_rect();
    ((r.width() - 80.0).clamp(900.0, 1240.0), (r.height() - 220.0).clamp(400.0, 640.0))
}

fn column(ui: &mut egui::Ui, w: f32, h: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(w);
        ui.set_height(h);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
        add(ui);
    });
}

fn divider(ui: &mut egui::Ui, h: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, h), egui::Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), egui::Stroke::new(1.0, lr::FIELD_STROKE));
}

/// Column heading in capitals (like the panels of the library).
pub(super) fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(crate::tr(text).to_uppercase()).font(lr::semibold(11.0)).color(Color32::from_gray(0x9a)));
}

/// Files per type, RAW first: "20 X3F + 2 JPG".
pub(super) fn files_text(files: &[(String, usize)]) -> String {
    files.iter().map(|(f, n)| format!("{} {f}", dialog::number(*n))).collect::<Vec<_>>().join(" + ")
}

/// Seconds still needed (by bytes), once there is a speed to go by.
fn rest_secs(s: &ci::State, secs: f64) -> Option<f64> {
    let speed = if secs > 2.0 { s.bytes as f64 / secs } else { 0.0 };
    (speed > 0.0).then(|| s.bytes_total.saturating_sub(s.bytes) as f64 / speed)
}

/// Progress of the run in the middle column.
fn progress_view(ui: &mut egui::Ui, l: &Arc<Mutex<Run>>, mode: Mode, camera: bool) {
    let (st, start) = {
        let g = l.lock().unwrap();
        (g.state_key, g.start)
    };
    heading(ui, "Import läuft");
    ui.add_space(6.0);
    if mode == Mode::InPlace {
        dialog::progress(ui, None, crate::tr("Die Ordner werden hinzugefügt …"));
        return;
    }
    if st.waiting {
        let t = if camera {
            crate::tr("Die Kamera ist nicht verbunden (Kabel, ausgeschaltet, Ruhezustand) oder das Ziel ist nicht erreichbar – wartet, bis beides wieder da ist …")
        } else {
            crate::tr("Ziel oder Quelle ist nicht erreichbar – wartet, bis das Laufwerk wieder da ist …")
        };
        dialog::progress(ui, None, t);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        return;
    }
    if st.bytes_total > 0 && st.bytes >= st.bytes_total && st.file + 1 >= st.files {
        dialog::progress(ui, None, crate::tr("Alles kopiert und geprüft – wird in die Bibliothek eingetragen …"));
        return;
    }
    // Bar by bytes (large RAWs count more than small JPEGs), plus speed and remaining time
    let fraction = if st.bytes_total > 0 { st.bytes as f32 / st.bytes_total as f32 } else { st.file as f32 / st.files.max(1) as f32 };
    let secs = start.map_or(0.0, |t| t.elapsed().as_secs_f64());
    let t = crate::trf!("Foto {a} von {b} · {c} von {d}", a = dialog::number((st.file + 1).min(st.files)), b = dialog::number(st.files), c = dialog::size(st.bytes), d = dialog::size(st.bytes_total));
    dialog::progress(ui, Some(fraction), &t);
    if let Some(rest) = rest_secs(&st, secs) {
        ui.label(egui::RichText::new(crate::trf!("{a}/s · noch etwa {b}", a = dialog::size((st.bytes as f64 / secs) as u64), b = remaining_time(rest))).color(lr::TEXT_DIM));
    }
    let mut c = crate::trf!("✓ geprüft {a} · Fehler {b}", a = dialog::number(st.verified), b = dialog::number(st.errors));
    if st.second > 0 {
        c += &crate::trf!(" · zweite Kopie {a}", a = dialog::number(st.second));
    }
    if mode == Mode::MoveFiles {
        c += &crate::trf!(" · Originale im Papierkorb: {a} (nur geprüfte)", a = dialog::number(st.trashed));
    }
    ui.label(egui::RichText::new(c).color(lr::TEXT));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
}

/// Result in the middle column. Returns: "eject card" clicked.
fn result_view(ui: &mut egui::Ui, f: &Finished) -> bool {
    heading(ui, "Import abgeschlossen");
    ui.add_space(6.0);
    let mut eject = false;
    egui::ScrollArea::vertical().id_salt("import-ergebnis").auto_shrink([false, false]).max_height(ui.available_height() - 40.0).show(ui, |ui| {
        // the notes stay visible, only the errors go into "Details"
        dialog::outcome(ui, f.verdict, &f.summary, &f.details);
        for r in &f.rows {
            dialog::text(ui, r);
        }
    });
    if let Some(card) = &f.card {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let name = card.ancestors().find(|a| a.parent() == Some(Path::new("/Volumes"))).and_then(|a| a.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if ui.add_enabled(cfg!(target_os = "macos") && f.ejected.is_none(), egui::Button::new(crate::trf!("Karte „{n}“ auswerfen", n = name))).clicked() {
                eject = true;
            }
            match &f.ejected {
                Some(Ok(())) => {
                    ui.label(egui::RichText::new(crate::tr("ausgeworfen")).color(lr::TEXT_DIM));
                }
                Some(Err(e)) => {
                    ui.label(egui::RichText::new(e).color(lr::TEXT_DIM));
                }
                None => {}
            }
        });
    }
    eject
}

/// Eject the card (volume) the photos came from.
fn eject_card(p: &Path) -> Result<(), String> {
    let volume = p.ancestors().find(|a| a.parent() == Some(Path::new("/Volumes"))).ok_or_else(|| crate::tr("keine Karte").to_string())?;
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/sbin/diskutil").arg("eject").arg(volume).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = volume;
        Err(crate::tr("Auswerfen ist hier nicht möglich").to_string())
    }
}

/// Remaining time, short: "40 s", "3 min", "1 h 10 min".
fn remaining_time(secs: f64) -> String {
    let s = secs.round().max(1.0) as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s.div_ceil(60)),
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card is copied (or moved, if chosen last time), a folder is added where it is.
    #[test]
    fn default_mode_per_source() {
        assert_eq!(default_mode(SourceKind::Card, Mode::CopyFiles), Mode::CopyFiles);
        assert_eq!(default_mode(SourceKind::Card, Mode::MoveFiles), Mode::MoveFiles);
        assert_eq!(default_mode(SourceKind::Card, Mode::InPlace), Mode::CopyFiles, "a card is never only added");
        assert_eq!(default_mode(SourceKind::Folder, Mode::CopyFiles), Mode::InPlace, "an archive is not doubled by accident");
        assert_eq!(default_mode(SourceKind::Folder, Mode::MoveFiles), Mode::InPlace);
    }

    /// The disabled import button names the first thing missing.
    #[test]
    fn block_reason_names_what_is_missing() {
        assert_eq!(block_reason(false, false, 0, Mode::CopyFiles, false, false, 0, 0, false), Some("Wähle zuerst eine Quelle: Karte, Kamera oder Ordner"));
        assert_eq!(block_reason(true, true, 0, Mode::CopyFiles, false, false, 0, 0, false), Some("Liest die Fotos …"));
        assert_eq!(block_reason(true, false, 0, Mode::CopyFiles, true, false, 0, 0, false), Some("In dieser Quelle sind keine Fotos"));
        assert_eq!(block_reason(true, false, 7, Mode::CopyFiles, false, false, 7, 0, false), Some("Wähle zuerst einen Zielordner"));
        assert_eq!(block_reason(true, false, 7, Mode::MoveFiles, true, true, 7, 0, false), Some("Der Zielordner ist gerade nicht erreichbar"));
        assert_eq!(block_reason(true, false, 7, Mode::CopyFiles, true, false, 0, 0, false), Some("Wähle mindestens ein Foto"));
        assert_eq!(block_reason(true, false, 7, Mode::CopyFiles, true, false, 7, 0, false), None);
        // adding needs no target, only folders
        assert_eq!(block_reason(true, false, 7, Mode::InPlace, false, false, 0, 2, false), None);
        assert_eq!(block_reason(true, false, 7, Mode::InPlace, false, false, 0, 0, false), Some("Wähle mindestens einen Ordner"));
        assert_eq!(block_reason(true, false, 7, Mode::InPlace, false, false, 0, 1, true), Some("Gib der neuen Sammlung einen Namen"));
    }
}
