//! "Library" module: catalog, folders, collections and keywords on the left,
//! grid with filter bar or large view (loupe) in the middle,
//! metadata on the right. Data: `crates/library` (own SQLite database, preview cache).
//!
//! Ratings apply immediately (grid, filmstrip); [`crate::rating::Writer`] writes the database and
//! sidecars in the background in key order.

use crate::menu::i18n::DeComma as _;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use library::db::{Db, Folder, Collection, Root, Keyword, Row};
use library::preview::{self, Job, Cache, Generator};
use eframe::egui;
use egui::{Color32, TextureHandle};

#[path = "not_in_catalog_ui.rs"]
mod not_in_catalog_ui;
#[path = "import_ui.rs"]
mod import_ui;
#[path = "library_facets_job.rs"]
mod library_facets_job;
#[path = "library_filter_bar.rs"]
mod library_filter_bar;
#[path = "library_load.rs"]
mod library_load;
#[path = "library_manage.rs"]
mod library_manage;
#[path = "collection_order_ui.rs"]
mod collection_order_ui;
#[path = "collection_drag.rs"]
mod collection_drag;
#[path = "library_pairs.rs"]
mod library_pairs;
// Cleaning up duplicates (group view, suggestion, Trash)
#[path = "library_dupes.rs"]
mod library_dupes;
// Typed completion in the search field
#[path = "search_suggest.rs"]
mod search_suggest;
// Search in the background, soft transitions of the grid
#[path = "search_job.rs"]
mod search_job;
#[path = "search_motion.rs"]
mod search_motion;
use search_suggest::Suggestion;
/// Field table of the search card.
pub fn search_field_table(ui: &mut egui::Ui) {
    search_suggest::field_table(ui);
}
// which view the centre shows, its chip and how to leave it; "+" for folders
#[path = "library_views.rs"]
mod library_views;
pub use library_manage::PhotoDrag;
pub use library_load::Loaded;
use crate::library_filter::Facets;

/// Selection of the column filters and the filters themselves, kept across a rebuild (see `take_facets_state`).
type FacetsState = (crate::library_filter::Remembered, Option<Box<Facets>>);
use crate::lr;
use library::similar::Vectors;
use library::search;

/// What the app should do after an image of the library.
pub enum Action {
    Selection,
    Export,
    /// open the active image in single-image view (double-click, Return, D)
    Develop,
    /// ratings changed (image, stars, flag, label) → update the filmstrip
    Rated(Vec<crate::rating::Value>),
    Status(String),
    /// create a virtual copy of this image (the app takes over settings and opens the copy)
    VirtualCopy(i64),
    /// delete the selected photos (confirmation, trash)
    Delete,
    /// virtual copy created in the background (source, new image ID)
    CopyCreated(i64, Result<i64, String>),
    /// faces of an image changed (re-read the single image)
    FacesChanged,
    /// "Copy settings …" of this photo (context menu)
    CopySettings(i64),
    /// paste the copied settings onto the selection (context menu)
    PasteSettings,
    /// turn the selected photos by 90° (buttons above the grid; `true` = clockwise)
    Turn(bool),
    /// "Ordner" in the Info panel clicked: Bibliothek, source = the photo's folder, the photo active and in view
    ShowFolder(i64),
    /// import: render the grid previews of these photos again (image, file, path, settings)
    Redevelop(Vec<(i64, i64, PathBuf, develop::Settings)>),
}

/// Image source on the left (catalog, folder, collection, keyword).
#[derive(Clone, PartialEq, Debug)]
enum Source {
    All,
    Recent,
    /// file is really missing (folder reachable, file gone)
    Missing,
    /// volume not connected (file not reachable, not lost)
    Offline,
    /// folder including subfolders (root ID, relative path)
    Folder(i64, String),
    Collection(i64),
    /// quick collection
    Quick,
    Keyword(i64),
    /// smart collection: saved search query
    Smart(i64),
    /// similar to this file (CLIP image vectors)
    Similar(i64),
    /// duplicates (identical or very similar), grouped
    Duplicates,
    /// folders on disk whose photos the catalog doesn't know
    NotInCatalog,
    /// only view a folder (⌘O), not in the library: own rows with negative IDs
    Browse(PathBuf),
    /// By date: capture day, month or year ("2026", "2026-09", "2026-09-19"), empty = without date.
    /// Camera photos and Apple Photos mixed; just a view, no file is moved.
    Date(String),
}

/// One year of the "By date" list.
struct DateYear {
    key: String,
    count: usize,
    months: Vec<DateMonth>,
}

struct DateMonth {
    /// "2026-09"
    key: String,
    count: usize,
    /// ("2026-09-19", count), newest first
    days: Vec<(String, usize)>,
}

/// "2026-09" → "September"
pub(crate) fn month_name(key: &str) -> String {
    const MONTHS: [&str; 12] = ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"];
    key.get(5..7).and_then(|m| m.parse::<usize>().ok()).and_then(|m| MONTHS.get(m.wrapping_sub(1))).map(|m| crate::tr(m).to_string()).unwrap_or_else(|| key.to_string())
}

/// "2026-09-19" → "Sa 19."
fn tag_name(key: &str) -> String {
    let parts: Vec<i64> = key.split('-').filter_map(|t| t.parse().ok()).collect();
    let [j, m, t] = parts[..] else { return key.to_string() };
    // Weekday after Sakamoto (0 = Sunday)
    const MONTH_OFFSET: [i64; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let jj = if m < 3 { j - 1 } else { j };
    let w = (jj + jj / 4 - jj / 100 + jj / 400 + MONTH_OFFSET.get((m - 1).clamp(0, 11) as usize).copied().unwrap_or(0) + t).rem_euclid(7);
    const DAYS: [&str; 7] = ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"];
    format!("{} {t}.", crate::tr(DAYS[w as usize]))
}

/// Write job for the quick collection (a background thread processes them in order).
enum QuickJob {
    /// photos in (`true`) or out
    Set(Vec<i64>, bool),
    Clear,
    /// save as an ordinary collection with this name
    Save(String),
}

/// Confirmation for the quick collection.
#[derive(Clone, PartialEq)]
enum QuickQuery {
    Off,
    Clear,
    /// name of the new collection, clear afterwards?
    Save(String, bool),
}

/// Folder ID of the "View folder" rows (not in the database).
const VIEW_FOLDER: i64 = -1;

/// Setting: ratings in a folder only being viewed are written next to the photos as sidecars (XMP). Default off: then
/// rating there is refused with the reason. It covers ratings only: a develop edit of such a photo is always saved
/// as a sidecar next to it (otherwise the edit would be lost silently); the originals are never touched.
const VIEW_SIDECARS_KEY: &str = "viewFolderSidecars";

pub fn view_folder_sidecars() -> bool {
    library::settings::value(VIEW_SIDECARS_KEY).as_bool().unwrap_or(false)
}

/// Why rating is refused in a viewed folder (status line, tooltips).
pub const RATING_REFUSED: &str = "Zum Bewerten zur Bibliothek hinzufügen oder den Schalter „Bewertungen als Begleitdatei (XMP)“ einschalten";

/// ID for a photo of a viewed folder (not in the library): negative and unique for the whole session. Numbered per
/// folder (−2, −3 …) the photos of two viewed folders shared their IDs – the app then took the first folder's loaded
/// photo, settings and preview for the photo of the second one at the same place.
fn view_id() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-2);
    NEXT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
}

/// Pending or failed "Similar photos" request.
#[derive(Clone)]
enum SimilarState {
    Load,
    Computing,
    Error(String),
}

/// Displayed file name: for photos from Apple Photos the original name (`IMG_1234.HEIC`) instead of the identifier.
fn display_name(z: &Row) -> String {
    crate::apple_photos_ui::original_name(z.file).unwrap_or_else(|| z.name.clone())
}


/// An image of the shared image source (Library ↔ Develop).
#[derive(Clone, Debug)]
pub struct SourceImage {
    pub image: i64,
    pub file: i64,
    pub path: PathBuf,
    pub name: String,
    pub copy: Option<String>,
    pub format: String,
    pub state: u8,
    pub stars: u8,
    pub flag: i8,
    pub label: u8,
    /// aspect ratio of the upright photo (width of its filmstrip cell), if the library knows the dimensions
    pub aspect: Option<f32>,
}

#[derive(Clone, Copy, PartialEq)]
enum SortOrder {
    CaptureTime,
    Name,
    Stars,
}

/// Flag selection (bits): picked, unflagged, rejected. 0 = all.
const FLAG_PICKED: u8 = 1;
const FLAG_NONE: u8 = 2;
const FLAG_REJECTED: u8 = 4;

/// Search field and attributes (flags, stars, labels); the metadata columns are in [`Facets`].
#[derive(Clone, PartialEq, Default)]
struct Filter {
    text: String,
    /// bits `FLAGGE_*`; 0 = no restriction
    flags: u8,
    /// at least this many stars; 0 = no restriction
    stars_min: u8,
    /// bit n = label n (1 red … 5 purple); 0 = no restriction
    labels: u8,
}

impl Filter {
    /// Image matches flags, stars and labels.
    fn attributes_match(&self, flag: i8, stars: u8, label: u8) -> bool {
        let flag = match flag {
            1 => FLAG_PICKED,
            -1 => FLAG_REJECTED,
            _ => FLAG_NONE,
        };
        (self.flags == 0 || self.flags & flag != 0)
            && stars >= self.stars_min
            && (self.labels == 0 || (label > 0 && self.labels & (1 << label) != 0))
    }

    fn attributes_active(&self) -> bool {
        self.flags != 0 || self.stars_min != 0 || self.labels != 0
    }

    fn clear_attributes(&mut self) {
        self.flags = 0;
        self.stars_min = 0;
        self.labels = 0;
    }
}

/// Node in the folder tree.
#[derive(Default)]
struct Node {
    root: i64,
    path: String,
    name: String,
    children: Vec<usize>,
    count: usize,
    /// photos below that are on disk but not in the catalog
    foreign: usize,
    /// folder exists only on disk (not in the catalog), e.g. an intermediate folder
    disk_only: bool,
    /// this folder itself contains photos the catalog doesn't know (count)
    disk: Option<usize>,
}

/// Background work (Lightroom import, sync, add folder).
#[derive(Default)]
struct Work {
    running: bool,
    /// kind of the running job ("lightroom", "lr_uebergabe", "pruefen" …)
    kind: &'static str,
    message: String,
    /// share done when the total is known (handover of the edits), otherwise `None` (indeterminate bar)
    fraction: Option<f32>,
    /// the handover of the edits has started in this job (the Lightroom dialog shows the progress there)
    handover_seen: bool,
    outcome: Option<Result<serde_json::Value, String>>,
    /// the run has committed new capture dates (columns and filters should already show them)
    interim: bool,
    /// start of the running job (for "running for …" in dialogs that wait for it)
    started: Option<std::time::Instant>,
    /// duration of the last run of the same kind (sync), for the waiting hint when the share is not known
    last: Option<std::time::Duration>,
}

/// Is this an automatic sync (started by the app, quiet, in the background)?
fn automatic(kind: &str) -> bool {
    matches!(kind, "sync" | "sync_full")
}

/// Why buttons wait for a running job of the library (sentence after "Gerade läuft: …").
pub const BLOCKED_WHY: &str = "Bis dahin sind die Knöpfe gesperrt, damit nicht zwei Aufträge gleichzeitig in die Bibliothek schreiben.";

/// Running job of the library for dialogs outside the library view (settings); updated every frame.
static JOB_NOW: Mutex<Option<crate::dialog::Running>> = Mutex::new(None);

/// The running job of the library (sync, Lightroom import, check …), if any – for the explanation of locked buttons.
pub fn running_job() -> Option<crate::dialog::Running> {
    JOB_NOW.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Maximum number of thumbnail textures held at once (≈ 200 KB each on the GPU).
const TEXTURES_MAX: usize = 700;
/// Maximum number of new textures per frame – keeps scrolling smooth.
const TEXTURES_PER_FRAME: usize = 48;
/// at most this many textures of the large level (single image and the ones decoded ahead while paging: the current,
/// three ahead, one behind and the last few seen – ≈ 17 MB each at 2560 px, ≈ 43 MB at 4000 px)
const LARGE_TEXTURES_MAX: usize = 8;
/// grid measurement logged once per run
static GRID_COMPLETE_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tidy away previews of levels no longer in use (background thread, lowest priority; stops when the levels change
/// again – the change then tidies once more).
fn tidy_previews(cache: &preview::Cache, why: &str) {
    library::platform::background_priority();
    let (small, large) = (preview::small(), preview::large());
    let t = std::time::Instant::now();
    let (derived, removed) = cache.tidy_levels(small, large, &|| (preview::small(), preview::large()) == (small, large));
    crate::log(&format!("previews tidied ({why}): {derived} derived from larger levels, {removed} of other levels removed, {:.1} s", t.elapsed().as_secs_f64()));
}

pub struct LibraryView {
    /// "Exportieren …" clicked in the left column (the app opens the export dialog)
    pub export_requested: bool,
    db: Option<Db>,
    /// last interim reload during a run
    interim_loaded: Option<std::time::Instant>,
    /// write ratings in the background (database, sidecars)
    writer: crate::rating::Writer,
    /// previous values per rating (⌘Z), newest last
    rating_undo: Vec<Vec<crate::rating::Value>>,
    /// when each rating change was made (parallel to `rating_undo`, [`crate::undo_order::tick`])
    rating_at: Vec<u64>,
    /// when the last move / rename of this session finished (⌘Z of the grid takes it back, see `undo_order.rs`)
    pub(crate) move_at: u64,
    /// the last undoable action was a rating
    last_was_rating: bool,
    /// show RAW+JPEG pairs as one image (setting `rawJpegStacks`, on by default)
    stack: bool,
    /// kind: 0 photos only (default), 1 videos only, 2 all (setting `libraryKind`)
    kind: u8,
    pub error: Option<String>,
    /// shared with the background search; changes through `Arc::make_mut`
    rows: Arc<Vec<Row>>,
    index: HashMap<i64, usize>,
    /// left the people view for Develop: the grid's source, active photo, selection and "nothing selected" from before
    people_return: Option<(Source, Option<i64>, HashSet<i64>, bool)>,
    folder_paths: Arc<HashMap<i64, String>>,
    roots: Vec<Root>,
    folder: Vec<Folder>,
    nodes: Vec<Node>,
    /// root node per root folder (root, node index)
    root_rows: Vec<(Root, usize)>,
    open_folders: HashSet<(i64, String)>,
    /// By date: year → month → day with count; plus the number of photos without date
    date_tree: Vec<DateYear>,
    without_date: usize,
    open_dates: HashSet<String>,
    collections: Vec<Collection>,
    /// members of every level of `collections` in the panel's order (built when the list changes, `set_collections`)
    collection_tree: library::collection_order::Tree,
    open_collections: HashSet<i64>,
    /// open sets as last written to the settings (`None` until restored after the first load)
    open_collections_saved: Option<HashSet<i64>>,
    /// dragging collections in the left panel (collection_drag.rs)
    collection_drag: collection_drag::DragState,
    keywords: Vec<Keyword>,
    /// flat list of the visible keyword rows (depth, index), rebuilt on change
    last_run: Option<i64>,
    /// counts of the catalog entries (recent, archived, missing, not connected), with time, number of rows and last run
    catalog_counts: Option<(std::time::Instant, usize, Option<i64>, [usize; 3])>,
    source: Source,
    source_ids: Option<HashSet<i64>>,
    filter: Filter,
    /// evaluated search query (text) and hits per row
    query_for: Option<String>,
    query: search::Query,
    query_matches: Option<Vec<bool>>,
    search_time_ms: f64,
    facets: Option<Facets>,
    /// column filters being built in the background (see `library_facets_job`)
    facets_job: Option<library_facets_job::FacetsJob>,
    columns_open: bool,
    /// search only in the selected folder/collection (default: whole library)
    search_in_source: bool,
    /// face detection in the background: status text, cancel, started?
    people_status: Arc<Mutex<String>>,
    people_cancel: Arc<std::sync::atomic::AtomicBool>,
    people_started: bool,
    /// Notice before the first model download (face recognition): shown / answered "Later" for this session
    face_model_notice: bool,
    face_model_later: bool,
    /// number of names a human should review (refreshed every 10 s)
    review_n: usize,
    /// number of name suggestions
    suggestion_n: usize,
    review_checked: Option<std::time::Instant>,
    /// the people view changed names: re-read the photos when it is left
    people_stale: bool,
    /// counts of names to review and suggestions, made in the background (see `people_maintenance`)
    people_counts: Arc<Mutex<Option<(usize, usize)>>>,
    /// open work around people (photos without face search, outdated suggestions), measured with the counts
    people_work: Arc<Mutex<library::people_state::Work>>,
    /// a face run or suggestion refresh is running in the background
    people_active: Arc<AtomicBool>,
    /// end of the last background run, and when the open work was first seen (waiting time before a new run)
    people_ended: Arc<Mutex<Option<std::time::Instant>>>,
    people_waiting_since: Option<std::time::Instant>,
    /// background face work is switched on (then the open work is shown in the status line)
    people_follow: bool,
    people_counting: Arc<AtomicBool>,
    /// files whose sidecar write waits for their location (`library::location`), and when it was counted
    pending_sidecars: usize,
    pending_checked: Option<std::time::Instant>,
    /// start face detection no earlier than this point in time (after program start)
    people_not_before: Option<std::time::Instant>,
    sort_order: SortOrder,
    /// sort direction reversed (arrow next to the sort order)
    sort_reversed: bool,
    /// version of the visible list (incremented on every refilter) – Develop syncs against it
    list_version: u64,
    /// fingerprint of the visible list as Develop sees it (see `list_fingerprint`)
    list_fingerprint: u64,
    /// photos of the selected source before all filters (header: "118 of 130")
    source_total: usize,
    /// folder the left panel should scroll to once (root, path)
    scroll_to_folder: Option<(i64, String)>,
    remembered_active: Option<i64>,
    /// "View folder" (⌘O): folder and its rows (kept on reload)
    viewed: Option<(PathBuf, Vec<Row>)>,
    /// what "View folder" found (for the reason of an empty grid)
    viewed_listing: Option<crate::view_folder::Listing>,
    /// width of the centre between the sidebars (grid and bars end there)
    centre_width: f32,
    smart_window: bool,
    smart_name: String,
    /// "Save as smart collection": in this set (`None`: top level)
    smart_parent: Option<i64>,
    smart_counts: HashMap<i64, Option<usize>>,
    suggestion: usize,
    /// names for the completion in the search field (built in the background)
    suggest: Arc<Mutex<search_suggest::Shared>>,
    /// Background search and the fade of the grid
    search_job: search_job::SearchJob,
    motion: search_motion::Motion,
    vectors: Arc<Mutex<Option<Result<Vectors, String>>>>,
    vectors_running: bool,
    similar_order: HashMap<i64, usize>,
    /// "Similar photos" is pending or failed for this file; shown right at the button
    similar_state: Option<(i64, SimilarState)>,
    /// Info with several selected: (selection sorted, active photo) → rows that differ
    meta_comparison: Option<((Vec<i64>, Option<i64>), HashSet<&'static str>)>,
    /// test: last stored preview (file, mean brightness)
    #[cfg(test)]
    pub last_stored: Option<(i64, f32)>,
    /// result of the image vector computation for a single photo (helper thread)
    similar_result: Arc<Mutex<Option<(i64, Result<Vec<f32>, String>)>>>,
    visible: Vec<usize>,
    /// the visible list (images) before the last load was adopted: keeps the active photo's place
    visible_before: Option<Vec<i64>>,
    /// loading in the background (see `library_load`)
    loader: library_load::Loader,
    /// preview keys of the rows as loaded (file, key), made by the load in the background (see `key_at`)
    row_keys: Vec<(i64, u64)>,
    refilter: bool,
    cameras: Vec<(String, usize)>,
    formats: Vec<(String, usize)>,
    years: BTreeMap<u16, [usize; 13]>,
    selection: HashSet<i64>,
    /// 90° turn asked for with a button above the grid (handed on as `Action::Turn`)
    turn_request: Option<bool>,
    /// counts library operations (move, rename, remove …): after one, ⌘Z belongs to it, not to an earlier turn
    pub(crate) ops: u64,
    /// quarter turns still to show on tiles until their re-rendered preview arrives
    pub(crate) turned: crate::library_turn::Turned,
    /// ⇧+arrow selection in progress (anchor and moving end)
    sel_ext: Option<crate::library_select::Ext>,
    /// rubber band on free space and the scroll position it asks for (autoscroll)
    band: Option<crate::library_select::Band>,
    band_scroll: Option<f32>,
    /// test aid `band=x0,y0,x1,y1` (content coordinates): a rubber band that stays held
    band_test: Option<(egui::Pos2, egui::Pos2)>,
    /// tests: screen rectangle of every grid cell drawn in the last frame (photo id, rect)
    #[cfg(test)]
    pub(crate) test_cells: Vec<(i64, egui::Rect)>,
    /// Photos (image ID) in the cells drawn in the last frame of the grid (their previews are renewed first)
    pub(crate) on_screen: Vec<i64>,
    /// Grid: nothing selected (click on free space, ⌘D, Esc). `active` stays as the last active photo,
    /// which actions without a selection use; the single views always show it.
    nothing_selected: bool,
    /// copied settings are there (the app sets it per frame; context menu "Paste settings")
    pub can_paste: bool,
    /// managing the library (remove, rename, move, delete; ⌘Z of it), see `library_manage`
    manage: library_manage::Manage,
    /// RAW+JPEG twice in the library, see `library_pairs`
    pairs: library_pairs::PairsState,
    dupes: library_dupes::DupesState,
    manage_message: Option<String>,
    /// active image (image ID)
    active: Option<i64>,
    pub loupe: bool,
    cell_size: f32,
    columns: usize,
    to_active: bool,
    /// "jump to the selection" (key C, button, file name in the info): the active photo goes to the middle of the grid
    jump_request: bool,
    /// frames in which the grid still checks that the active photo is in view (after `to_active`)
    active_settle: u8,
    // Previews
    generator: Option<Generator>,
    textures: HashMap<(i64, u32), (TextureHandle, u64)>,
    /// preview of a hovered preset or film for the selected photo (replaces its thumbnail in the grid)
    pub hover_preview: Option<(i64, TextureHandle)>,
    clock: u64,
    requested: Vec<(i64, u32)>,
    queue: Vec<preview::Outcome>,
    meta_pending: Vec<(i64, preview::Meta)>,
    background_set: bool,
    /// protection state the background list was built with (rebuilt when it changes)
    background_protection: u64,
    // large view
    zoom100: bool,
    pan: egui::Vec2,
    // background work
    work: Arc<Mutex<Work>>,
    pub lr_window: bool,
    /// state of the Lightroom dialog (catalog, last import, last handover) – read when opening and after each run
    lr_state: Option<crate::lightroom_state::LrState>,
    /// "Prüfen" of the Lightroom dialog: (catalog, state of the read-only check)
    lr_check: Arc<Mutex<Option<(PathBuf, crate::lightroom_state::Check)>>>,
    /// the last Lightroom run could not read this catalog (catalog, error)
    lr_failed: Option<(PathBuf, String)>,
    /// catalog of the Lightroom run going on (for its error)
    lr_job_catalog: Option<PathBuf>,
    /// catalogs found in Lightroom's folder (`None` while still looking), looked for once
    lr_found: Arc<Mutex<Option<Vec<PathBuf>>>>,
    lr_search_started: bool,
    /// Lightroom has the catalog open (catalog, open, looked at)
    lr_in_use: Option<(PathBuf, bool, std::time::Instant)>,
    /// collection set the catalog's collections go into (`None`: not set yet, the catalog's name)
    lr_set_name: Option<String>,
    /// confirmation "Rebuild library?" open
    rebuild_confirm: bool,
    /// a rating in a viewed folder was refused (the app says why)
    rating_refused: bool,
    /// confirmation "remove missing entries?" ("Missing photos" view) and result of the background run
    missing_confirm: bool,
    missing_result: Arc<Mutex<Option<Result<usize, String>>>>,
    /// quick collection: image IDs (the display updates immediately, the database follows in the background)
    quick: HashSet<i64>,
    /// ID of the quick collection in the database, if already created (only for hiding it in "Collections")
    quick_id: Option<i64>,
    quick_query: QuickQuery,
    quick_tx: Option<std::sync::mpsc::Sender<QuickJob>>,
    /// jobs not yet written
    quick_pending: Arc<std::sync::atomic::AtomicUsize>,
    /// feedback from the writer: (new collection created, error text)
    quick_feedback_queue: Arc<Mutex<Vec<(bool, Option<String>)>>>,
    /// to wake up the UI when the writer is done
    quick_ctx: Option<egui::Context>,
    /// other write operations of the UI (copy, faces, keywords, move …) in the background
    db_writer: crate::library_writer::Writer,
    report: Option<serde_json::Value>,
    /// measurement: grid duration per frame (ms, moving average)
    pub raster_ms: f64,
    pub start_ms: f64,
    /// file whose large preview was last requested (single image)
    large_requested: Option<i64>,
    /// regenerating previews in the background (cleanup running)
    preview_cleanup: Arc<AtomicBool>,
    /// creation time (measurement of the grid previews)
    opened_at: std::time::Instant,
    grid_y: f32,
    grid_remembered: Option<GridState>,
    grid_back_y: Option<f32>,
    /// back from single image view: keep this photo at the same height on screen for a few frames,
    /// even if the layout or the list settles only after the first frame (image, distance from the top, frames left)
    grid_anchor: Option<(i64, f32, u8)>,
    /// row pitch (cell height plus spacing) of the last drawn grid frame
    grid_cell: f32,
    /// visible height of the grid in the last drawn frame
    grid_view_h: f32,
    /// source changed: grid back to the top without scrolling (unless it should show a specific photo)
    grid_top: bool,
    /// counts source changes; the filmstrip then also jumps to the start
    pub source_nr: u64,
    /// test aid scrolling: start, frames, longest frame (ms)
    measurement: Option<(std::time::Instant, u64, f64)>,
    /// test aid GRAINEO_GRID_SCROLL_TEST: scrolling at a fixed speed with measurement
    scroll_test: Option<crate::grid_scroll_test::ScrollTest>,
    // Milestone 3
    dup: Arc<Mutex<Option<Vec<library::duplicates::Group>>>>,
    dup_running: bool,
    dup_order: HashMap<i64, (usize, usize)>,
    kw_input: String,
    /// extra details read from the opened file (Develop): exposure program, focus, colour mode, sensor … – shown in the
    /// Info panel below the metadata (image ID, rows)
    pub(crate) file_details: Option<(i64, Vec<(String, String)>)>,
    /// chosen suggestion below the keyword field (0 = none, n = n-th)
    kw_pick: usize,
    kw_sets: Vec<(String, Vec<String>)>,
    /// dialog "Save keyword set": name, keywords
    kw_set_dialog: Option<(String, Vec<String>)>,
    kw_set: usize,
    dialog: Option<Dialog>,
    /// result of a running time correction (background)
    time_result: Arc<Mutex<Option<Result<library::time_correction::Report, String>>>>,
    watcher: Option<library::watch::Watcher>,
    watched: Vec<PathBuf>,
    last_auto_sync: Option<std::time::Instant>,
    changed_since: Option<std::time::Instant>,
    /// root folders with changes since `changed_since` (FSEvents); `None` in it: unknown where → all
    changed_roots: HashSet<Option<PathBuf>>,
    /// last check whether the daily full sync is due
    full_checked: Option<std::time::Instant>,
    last_round: std::time::Instant,
    net_round: std::time::Instant,
    reachable: HashMap<i64, bool>,
    import: Option<import_ui::Import>,
    backup_status: Arc<Mutex<Option<String>>>,
    // semantic search
    semantic: Arc<Mutex<Option<Result<Arc<SemState>, String>>>>,
    sem_loading: bool,
    /// file → rank in the semantic list (for the current query)
    sem_rank: HashMap<i64, usize>,
    sem_for: String,
    /// strictness for which `sem_rank` was built
    sem_applied: f32,
    sem_ms: f64,
    /// start of the "Similar in meaning" part in `visible`
    sem_from: Option<usize>,
    /// relevance values and strictness of the semantic part (semantic_ui.rs)
    sem_view: crate::semantic_ui::View,
    vector_job: Arc<AtomicBool>,
    vector_round: Option<std::time::Instant>,
    /// `library::search_model::round` at the last check (stage switched: reload the search state)
    search_model_round: u64,
    /// `library::semantic::text_model_round` at the last check (switching the text model)
    text_model_round: u64,
    /// "Not in catalog": search, display, add
    not_in_catalog: not_in_catalog_ui::NotInCatalogState,
    /// "Move to …" for folders (target, confirmation, progress)
    move_folder: crate::move_folder_ui::MoveFolder,
    relink: crate::relink_ui::Relink,
}

/// Loaded models and vectors of the semantic search.
pub struct SemState {
    // encoder of the active search model stage (library::search_model, Settings → Search)
    pub(crate) enc: library::search_model::QueryEncoder,
    pub(crate) image: Vectors,
    pub(crate) text: Option<Vectors>,
}

/// "Import from Lightroom …" (settings): opens the Lightroom dialog in the next frame.
static LIGHTROOM_REQUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_lightroom() {
    LIGHTROOM_REQUEST.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Maintenance from the settings (Library → Maintenance): "pruefen" (check the library) or "neuaufbau"
/// (asks first); runs in the library view in the next frame.
static MAINTENANCE_REQUEST: Mutex<Option<&'static str>> = Mutex::new(None);

pub fn request_maintenance(kind: &'static str) {
    *MAINTENANCE_REQUEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(kind);
}

/// Small windows of the library.
enum Dialog {
    Rename { file: i64, name: String },
    /// Correct capture time: files, capture time of the active photo, kind (0 set to time, 1 hours,
    /// 2 restore), input of the correct time, hours
    /// `files`: the files to correct (with the camera JPEGs of RAW+JPEG stacks), `photos`: how many photos that is
    Time { files: Vec<i64>, photos: usize, reference: Option<String>, kind: u8, input: String, hours: i32 },
    RenameFolder { folder: i64, name: String },
    Error(String),
    /// Completion of an operation (e.g. import) in a dialog instead of a notice: status light, summary, further
    /// lines (visible), details (collapsed)
    Outcome { title: String, verdict: crate::dialog::Verdict, summary: String, rows: Vec<String>, details: Vec<String> },
}


/// Last component of a path for messages.
fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Load the library at startup before the first frame: test hooks that need the rows at once (filters, image number,
/// selection, a folder or file on the command line).
fn foreground_start() -> bool {
    ["GRAINEO_LIBRARY_TEST", "GRAINEO_IMAGE", "GRAINEO_SELECTION"].iter().any(|k| std::env::var_os(k).is_some())
        || std::env::args().nth(1).is_some_and(|a| !a.starts_with("-psn"))
}

/// `v` (indices into `rows`) in the order "newest first, without date at the end; same time by name, original before
/// its copies" – if `v` is in the opposite order of the capture times (as `Db::rows` delivers): the groups of equal
/// capture time are turned round, their inside stays. `None`: not in that order (sort instead).
fn newest_first(rows: &[Row], v: &[usize]) -> Option<Vec<usize>> {
    let key = |i: usize| {
        let z = &rows[i];
        (z.capture.is_none(), z.capture.as_deref(), z.name.as_str(), z.copy.is_some())
    };
    if !v.windows(2).all(|w| key(w[0]) <= key(w[1])) {
        return None;
    }
    let dated = v.iter().take_while(|&&i| rows[i].capture.is_some()).count();
    let mut out = Vec::with_capacity(v.len());
    let mut end = dated;
    while end > 0 {
        let capture = rows[v[end - 1]].capture.as_deref();
        let mut start = end - 1;
        while start > 0 && rows[v[start - 1]].capture.as_deref() == capture {
            start -= 1;
        }
        out.extend_from_slice(&v[start..end]);
        end = start;
    }
    out.extend_from_slice(&v[dated..]);
    Some(out)
}

/// Path of a row's file.
fn row_path(folder_paths: &HashMap<i64, String>, z: &Row) -> String {
    format!("{}{}", folder_paths.get(&z.folder).map(String::as_str).unwrap_or(""), z.name)
}

/// Preview key of a row (path, size, time, orientation).
fn row_key(folder_paths: &HashMap<i64, String>, z: &Row) -> u64 {
    preview::cache_key_with_orientation(preview::cache_key(&row_path(folder_paths, z), z.size.unwrap_or(0), z.mtime.unwrap_or(0)), z.orientation)
}

/// Test aid `GRAINEO_RELOAD_TEST=<seconds>` (only with `GRAINEO_DATA_DIR`): re-read the library every n seconds, like the
/// interim reloads during a sync – for measuring frame times.
fn reload_test_due() -> bool {
    static EVERY: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let every = EVERY.get_or_init(|| {
        crate::env::var_os("GRAINEO_DATA_DIR")?;
        crate::env::var("GRAINEO_RELOAD_TEST").ok()?.parse::<f64>().ok().filter(|s| *s > 0.0)
    });
    let Some(every) = every else { return false };
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    match *last {
        Some(t) if (now - t).as_secs_f64() < *every => false,
        None => {
            *last = Some(now);
            false
        }
        _ => {
            *last = Some(now);
            crate::log("reload test: re-reading library");
            true
        }
    }
}

/// Connection of the UI thread: waits only briefly for write locks of the background runs
/// (instead of freezing the UI for up to 5 s in the busy timeout). Background connections keep waiting long.
fn ui_connection(db: Db) -> Db {
    let _ = db.conn.busy_timeout(std::time::Duration::from_millis(250));
    db
}

impl LibraryView {
    pub fn new(ctx: &egui::Context) -> Self {
        let t = std::time::Instant::now();
        // one-time: English file and folder names (old location → new location)
        for m in library::migrate() {
            crate::log(&format!("umbenannt: {m}"));
        }
        let mut b = Self {
            export_requested: false,
            db: None,
            writer: crate::rating::Writer::new(Some(library::db_path())),
            rating_undo: Vec::new(),
            rating_at: Vec::new(),
            move_at: 0,
            last_was_rating: false,
            stack: library::settings::value("rawJpegStacks").as_bool().unwrap_or(true),
            kind: library::settings::value("libraryKind").as_u64().map_or(0, |v| v.min(2) as u8),
            error: None,
            rows: Arc::new(Vec::new()),
            people_return: None,
            index: HashMap::new(),
            folder_paths: Arc::new(HashMap::new()),
            roots: Vec::new(),
            folder: Vec::new(),
            nodes: Vec::new(),
            root_rows: Vec::new(),
            open_folders: HashSet::new(),
            date_tree: Vec::new(),
            without_date: 0,
            open_dates: HashSet::new(),
            collections: Vec::new(),
            collection_tree: Default::default(),
            open_collections: HashSet::new(),
            open_collections_saved: None,
            collection_drag: Default::default(),
            keywords: Vec::new(),
            last_run: None,
            catalog_counts: None,
            source: Source::All,
            source_ids: None,
            filter: Filter::default(),
            query_for: None,
            query: search::Query::default(),
            query_matches: None,
            search_time_ms: 0.0,
            facets: None,
            facets_job: None,
            columns_open: false,
            search_in_source: false,
            people_status: Arc::new(Mutex::new(String::new())),
            people_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            people_started: false,
            face_model_notice: false,
            face_model_later: false,
            review_n: 0,
            suggestion_n: 0,
            review_checked: None,
            people_stale: false,
            people_counts: Arc::default(),
            people_work: Arc::default(),
            people_active: Arc::default(),
            people_ended: Arc::default(),
            people_waiting_since: None,
            people_follow: false,
            people_counting: Arc::default(),
            pending_sidecars: 0,
            pending_checked: None,
            people_not_before: None,
            sort_order: SortOrder::CaptureTime,
            sort_reversed: false,
            centre_width: f32::INFINITY,
            list_version: 0,
            list_fingerprint: 0,
            source_total: 0,
            scroll_to_folder: None,
            remembered_active: None,
            viewed: None,
            viewed_listing: None,
            smart_window: false,
            smart_name: String::new(),
            smart_parent: None,
            smart_counts: HashMap::new(),
            suggestion: 0,
            search_job: Default::default(),
            motion: Default::default(),
            suggest: Default::default(),
            vectors: Arc::new(Mutex::new(None)),
            vectors_running: false,
            similar_order: HashMap::new(),
            similar_state: None,
            meta_comparison: None,
            #[cfg(test)]
            last_stored: None,
            similar_result: Arc::new(Mutex::new(None)),
            visible: Vec::new(),
            visible_before: None,
            loader: Default::default(),
            row_keys: Vec::new(),
            interim_loaded: None,
            refilter: true,
            cameras: Vec::new(),
            formats: Vec::new(),
            years: BTreeMap::new(),
            selection: HashSet::new(),
            turn_request: None,
            ops: 0,
            turned: Default::default(),
            sel_ext: None,
            band: None,
            band_scroll: None,
            band_test: None,
            #[cfg(test)]
            test_cells: Vec::new(),
            on_screen: Vec::new(),
            nothing_selected: false,
            can_paste: false,
            manage: Default::default(),
            pairs: Default::default(),
            dupes: Default::default(),
            manage_message: None,
            active: None,
            loupe: false,
            cell_size: 180.0,
            columns: 1,
            to_active: false,
            jump_request: false,
            active_settle: 0,
            generator: None,
            textures: HashMap::new(),
            hover_preview: None,
            clock: 0,
            requested: Vec::new(),
            queue: Vec::new(),
            meta_pending: Vec::new(),
            background_set: false,
            background_protection: 0,
            zoom100: false,
            pan: egui::Vec2::ZERO,
            work: Arc::new(Mutex::new(Work::default())),
            lr_window: false,
            lr_state: None,
            lr_check: Arc::new(Mutex::new(None)),
            lr_failed: None,
            lr_job_catalog: None,
            lr_found: Arc::new(Mutex::new(None)),
            lr_search_started: false,
            lr_in_use: None,
            lr_set_name: None,
            rebuild_confirm: false,
            rating_refused: false,
            missing_confirm: false,
            missing_result: Arc::new(Mutex::new(None)),
            quick: HashSet::new(),
            quick_id: None,
            quick_query: QuickQuery::Off,
            quick_tx: None,
            quick_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            quick_feedback_queue: Arc::new(Mutex::new(Vec::new())),
            quick_ctx: None,
            db_writer: Default::default(),
            report: None,
            raster_ms: 0.0,
            start_ms: 0.0,
            large_requested: None,
            preview_cleanup: Arc::new(AtomicBool::new(false)),
            opened_at: std::time::Instant::now(),
            grid_y: 0.0,
            grid_remembered: None,
            grid_back_y: None,
            grid_anchor: None,
            grid_cell: 0.0,
            grid_view_h: 0.0,
            grid_top: false,
            source_nr: 0,
            measurement: None,
            scroll_test: crate::grid_scroll_test::ScrollTest::from_env(),
            dup: Arc::new(Mutex::new(None)),
            dup_running: false,
            dup_order: HashMap::new(),
            kw_input: String::new(),
            file_details: None,
            kw_pick: 0,
            kw_sets: load_kw_sets(),
            kw_set_dialog: None,
            kw_set: 0,
            dialog: None,
            time_result: Arc::new(Mutex::new(None)),
            watcher: None,
            watched: Vec::new(),
            last_auto_sync: None,
            changed_since: None,
            changed_roots: HashSet::new(),
            full_checked: None,
            last_round: std::time::Instant::now(),
            net_round: std::time::Instant::now(),
            reachable: HashMap::new(),
            import: None,
            backup_status: Arc::new(Mutex::new(None)),
            semantic: Arc::new(Mutex::new(None)),
            sem_loading: false,
            sem_rank: HashMap::new(),
            sem_for: String::new(),
            sem_applied: -1.0,
            sem_ms: 0.0,
            sem_from: None,
            sem_view: crate::semantic_ui::View::new(),
            vector_job: Arc::new(AtomicBool::new(false)),
            vector_round: None,
            search_model_round: u64::MAX,
            text_model_round: library::semantic::text_model_round(),
            not_in_catalog: Default::default(),
            move_folder: Default::default(),
            relink: Default::default(),
        };
        // folder and file moves interrupted at the last run: finish or revert them in the background; the first sync
        // waits for it
        crate::move_folder_ui::resume_at_start();
        match Db::standard() {
            Ok(db) => b.db = Some(ui_connection(db)),
            Err(e) => b.error = Some(crate::trf!("Bibliothek nicht zu öffnen: {e}", e = e)),
        }
        // restore from a backup carried out at this start (see `library::backup::apply_pending_restore`)
        if let Some(r) = library::backup::take_restore_message() {
            let (verdict, summary) = match r {
                Ok(r) => (
                    crate::dialog::Verdict::Green,
                    match &r.kept {
                        // file names only: both lie in the data folder or the backup folder, full paths fill the dialog
                        Some(k) => crate::trf!("{n} Fotos aus {a} wiederhergestellt; die bisherige Bibliothek liegt als {b}", n = r.images, a = file_name(&r.backup), b = file_name(k)),
                        None => crate::trf!("{n} Fotos aus {a} wiederhergestellt", n = r.images, a = file_name(&r.backup)),
                    },
                ),
                Err(e) => (crate::dialog::Verdict::Yellow, crate::trf!("Nicht wiederhergestellt, die Bibliothek ist unverändert: {e}", e = e)),
            };
            b.dialog = Some(Dialog::Outcome { title: crate::tr("Bibliothek wiederherstellen").to_string(), verdict, summary, rows: Vec::new(), details: Vec::new() });
        }
        b.quick_ctx = Some(ctx.clone());
        b.loader.foreground = cfg!(test) || foreground_start();
        let c = ctx.clone();
        let cache = Arc::new(Cache::standard());
        crate::prefs::register_previews(cache.clone());
        // offline copies of RAWs show the development (standard development for unedited photos), not the camera JPEG
        preview::set_developer(Box::new(|path, level| {
            let t = std::time::Instant::now();
            let (r, tm) = crate::large_preview::render_offline_timed(path, level);
            match &r {
                Ok(img) => crate::log(&format!(
                    "offline copy {}: {}×{} in {:.1} s (waited {:.1} s for paging/opening, read {:.1} s, developed {:.1} s)",
                    path.display(),
                    img.width(),
                    img.height(),
                    t.elapsed().as_secs_f32(),
                    tm.waited,
                    tm.open,
                    tm.develop
                )),
                Err(e) => crate::log(&format!("offline copy {}: {e}", path.display())),
            }
            r
        }));
        // 4 threads: an offline copy in the background occupies one for its whole development (two while idle), the
        // others decode visible tiles and the large previews ahead while paging
        b.generator = Some(Generator::new(cache, 4, Box::new(move |visible| if visible { c.request_repaint() } else { c.request_repaint_after(std::time::Duration::from_secs(1)) })));
        // the window shows at once; the rows follow from the background (test runs: right away)
        if b.loader.foreground {
            b.reload_now();
        } else {
            b.reload();
        }
        crate::log(&format!("library: opened in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0));
        b
    }

    /// After the first load: last source and active photo, test filters, sync or search index in the background.
    fn after_first_load(&mut self) {
        let Some(ctx) = self.quick_ctx.clone() else { return };
        self.restore_open_collections();
        self.restore_source();
        self.test_filter(&ctx);
        self.start_ms = self.opened_at.elapsed().as_secs_f64() * 1000.0;
        crate::log(&format!("library: {} images ready after {:.0} ms", self.rows.len(), self.start_ms));
        // sync with the disk at startup (read-only, ~0.5 s), the result reloads silently
        // without a search index (first version 2): AI data, capture dates and index in the background
        let without_index = self.db.as_ref().is_some_and(|d| d.info("search_index").is_none());
        if self.db.is_some() && !self.rows.is_empty() && without_index {
            self.start_job(&ctx, "index", None);
        } else if self.db.is_some() && !self.rows.is_empty() && crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_none() {
            self.start_job(&ctx, "sync", None);
        }
    }

    /// Test aid `GRAINEO_LIBRARY_TEST="search=beach;columns=1;lens=EF20-35mm f/2.8L;aperture=1-4;folder=2024/;collection=Name;cell=240;active=12;sort=stars;loupe=1;quick=1"`.
    fn test_filter(&mut self, ctx: &egui::Context) {
        let Ok(spec) = crate::env::var("GRAINEO_LIBRARY_TEST") else { return };
        for part in spec.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            match k.trim() {
                "search" | "text" => {
                    self.filter.text = v.to_string();
                    self.search_job.sync_next = true;
                }
                // Focus in the search field (suggestion list for screenshots)
                "focus" => search_suggest::TEST_FOCUS.store(v == "1", Ordering::Relaxed),
                "picked" => self.filter.flags = FLAG_PICKED,
                "kind" => self.kind = v.parse().unwrap_or(0).min(2),
                "date" => {
                    for n in [4, 7] {
                        if v.len() >= n {
                            self.open_dates.insert(v[..n].to_string());
                        }
                    }
                    self.set_source(Source::Date(v.to_string()));
                }
                "cell" => self.cell_size = v.parse().unwrap_or(self.cell_size),
                "columns" => self.columns_open = v == "1",
                "sort" => self.sort_order = match v { "name" => SortOrder::Name, "stars" => SortOrder::Stars, _ => SortOrder::CaptureTime },
                "lens" | "camera" => {
                    let k = if k == "lens" { 1 } else { 0 };
                    self.facets_now();
                    if let Some(f) = &mut self.facets {
                        f.choose_name(k, v);
                    }
                    self.columns_open = true;
                }
                "aperture" | "focal" | "iso" => {
                    let r = ["aperture", "focal", "iso"].iter().position(|x| *x == k).unwrap_or(0);
                    self.facets_now();
                    if let (Some(f), Some((a, e))) = (&mut self.facets, v.split_once('-')) {
                        f.choose_range(&self.rows, r, a.parse().unwrap_or(1.0), e.parse().unwrap_or(32.0));
                    }
                    self.columns_open = true;
                }
                "folder" => {
                    if let Some(o) = self.folder.iter().find(|o| o.path == v) {
                        let (s, p) = (o.root, o.path.clone());
                        let mut q = String::new();
                        for t in p.split('/').filter(|t| !t.is_empty()) {
                            self.open_folders.insert((s, q.clone()));
                            q.push_str(t);
                            q.push('/');
                        }
                        self.set_source(Source::Folder(s, p));
                    }
                }
                "collection" => {
                    if let Some(x) = self.collections.iter().find(|x| x.name == v).cloned() {
                        self.open_parents_of(x.id);
                        self.set_source(if x.kind == 2 { Source::Smart(x.id) } else { Source::Collection(x.id) });
                    }
                }
                // all sets open (screenshots of the order)
                "open_collections" if v == "all" => {
                    self.open_collections.extend(self.collections.iter().filter(|c| c.kind == 1).map(|c| c.id));
                }
                // a drag held still for screenshots: "drag=Source>Target:0.5" (names; 0.5 = middle of the target row)
                "drag" => self.collection_drag.test = collection_drag::test_drag(&self.collections, v),
                "active" => {
                    self.apply_filter();
                    if let Some(&i) = v.parse::<usize>().ok().and_then(|n| self.visible.get(n)) {
                        self.active = Some(self.rows[i].image);
                        self.to_active = true;
                    }
                }
                "loupe" => self.loupe = v == "1",
                // the context menu of the active photo, drawn open next to it (screenshots)
                "menu" => TEST_PHOTO_MENU.store(v == "1", Ordering::Relaxed),
                // like ⌫ in a collection on the active photo
                "remove" if v == "1" => {
                    let target = self.target();
                    let mut a = Vec::new();
                    self.remove_from_collection(target, &mut a);
                }
                // rubber band held from (x0, y0) to (x1, y1), content coordinates of the grid
                "band" => {
                    let n: Vec<f32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                    if n.len() == 4 {
                        self.band_test = Some((egui::pos2(n[0], n[1]), egui::pos2(n[2], n[3])));
                    }
                }
                // like the B key on the active image
                "quick" if v == "1" => {
                    let target = self.target();
                    let _ = self.quick_toggle(&target);
                }
                "import" => self.test_import_source(ctx, PathBuf::from(v)),
                "lr-window" => self.lr_window = true,
                "check" => {
                    if let Some(a) = self.db.as_ref().and_then(|db| library::reconcile::reconcile(db, None, &|_| {}).ok()) {
                        self.dialog = Some(sync_outcome(crate::tr("Bibliothek geprüft"), &serde_json::json!({"kind": "check", "sync": a.json()})));
                    }
                }
                "duplicates" => self.show_duplicates(),
                // like "Save these photos as a smart collection" in the status row (dialog with the proposed name)
                "smartsave" => {
                    self.smart_name = self.smart_name_proposal();
                    self.smart_window = true;
                }
                // add a folder to the library like "+" in FOLDERS (test libraries under /private/tmp)
                "add" => self.start_job(&ctx, "folder", Some(PathBuf::from(v))),
                // "Similar photos" of the active photo (chip test); `active=<n>` before it picks the photo
                "similar" => {
                    self.apply_filter();
                    if self.active.is_none() {
                        self.active = self.visible.first().map(|&i| self.rows[i].image);
                    }
                    self.show_similar(&ctx);
                }
                _ => {}
            }
        }
        self.refilter = true;
    }

    /// Start building the column filters when needed (background thread, never waits): until they are there the filter
    /// bar shows its frame with a note ([`Self::poll_facets`] takes the result).
    fn facets_ready(&mut self) {
        if self.facets.is_some() || self.facets_job.is_some() {
            return;
        }
        let Some(path) = self.db.as_ref().map(|d| d.path.clone()) else { return };
        self.start_facets((Default::default(), None), library_facets_job::Assignments::FromDb(path));
    }

    /// Start (or restart) the background build; the remembered selection is restored on the result, the previous
    /// filters are kept if nothing they were built from changed.
    fn start_facets(&mut self, (remembered, previous): FacetsState, source: library_facets_job::Assignments) {
        self.facets = None;
        self.facets_job = library_facets_job::FacetsJob::start(self.rows.clone(), self.keywords.clone(), source, remembered, previous, self.columns_open, self.quick_ctx.clone());
    }

    /// The selection of the column filters (also while they are being rebuilt) and the filters themselves for a
    /// rebuild: the filters are taken away; `None` if they are not in use at all.
    fn take_facets_state(&mut self) -> Option<FacetsState> {
        match self.facets.take() {
            Some(f) => {
                self.facets_job = None;
                Some((f.remember(), Some(Box::new(f))))
            }
            None => self.facets_job.take().map(|j| (j.remembered, j.previous)),
        }
    }

    /// Every frame: take the finished column filters (restores the selection, then the grid is filtered again).
    fn poll_facets(&mut self) {
        let Some(job) = &mut self.facets_job else { return };
        // the new filters, or `None`: the inputs are unchanged, the previous ones stay
        let built = match job.poll() {
            library_facets_job::Poll::Pending => return,
            library_facets_job::Poll::Lost => {
                self.facets_job = None;
                self.refilter = true;
                return;
            }
            library_facets_job::Poll::Unchanged => None,
            library_facets_job::Poll::Ready(f) => Some(f),
        };
        let Some(mut job) = self.facets_job.take() else { return };
        if job.rows_len != self.rows.len() {
            // the list changed meanwhile: build again for the current one
            let source = library_facets_job::Assignments::FromDb(self.db.as_ref().map(|d| d.path.clone()).unwrap_or_default());
            self.start_facets((job.remembered, job.previous), source);
            return;
        }
        let Some(mut f) = built.or_else(|| job.previous.take()) else {
            self.refilter = true;
            return;
        };
        // the same names again (also on the kept filters: the selection may have been cleared meanwhile)
        f.reset();
        f.restore(&job.remembered);
        self.facets = Some(*f);
        self.refilter = true;
    }

    /// Test setup only: the column filters right now (the rows must be loaded).
    fn facets_now(&mut self) {
        if self.facets.is_some() {
            return;
        }
        let Some(db) = &self.db else { return };
        let keywords = library_facets_job::read_assignments(db);
        self.facets_job = None;
        self.facets = Some(Facets::build(&self.rows, &self.keywords, &keywords));
        self.refilter = true;
    }

    /// Tell the preview store which photos lie on other drives than the startup volume or are not reachable right
    /// now (their previews are never evicted). The drives are checked in the background (per root folder, never
    /// inside a mounted volume); after the first check of a run, previews of earlier levels are tidied away.
    fn update_protection(&self) {
        let Some(e) = &self.generator else { return };
        let root_of: HashMap<i64, i64> = self.folder.iter().map(|o| (o.id, o.root)).collect();
        let root_path: HashMap<i64, &String> = self.roots.iter().map(|s| (s.id, &s.path)).collect();
        // place per row: the path of its root folder, otherwise the folder's own path (browsed folder)
        let mut places: Vec<String> = Vec::new();
        let mut place_index: HashMap<String, usize> = HashMap::new();
        let mut place_of_folder: HashMap<i64, usize> = HashMap::new();
        let mut rows: Vec<(u64, i64, usize, bool)> = Vec::with_capacity(self.rows.len());
        for (n, z) in self.rows.iter().enumerate() {
            let folder = z.folder;
            let i = *place_of_folder.entry(folder).or_insert_with(|| {
                let place = root_of.get(&folder).and_then(|r| root_path.get(r).copied()).or_else(|| self.folder_paths.get(&folder)).cloned().unwrap_or_default();
                *place_index.entry(place.clone()).or_insert_with(|| {
                    places.push(place);
                    places.len() - 1
                })
            });
            rows.push((self.key_at(n), z.file, i, matches!(z.status, 2 | 3)));
        }
        let cache = e.cache().clone();
        static GENERATION: AtomicU64 = AtomicU64::new(0);
        static TIDIED: AtomicBool = AtomicBool::new(false);
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let tidy = !TIDIED.swap(true, Ordering::SeqCst);
        let running = self.preview_cleanup.clone();
        if tidy {
            running.store(true, Ordering::SeqCst);
        }
        std::thread::spawn(move || {
            let on_this_mac: Vec<bool> = places.iter().map(|p| p.is_empty() || library::platform::on_startup_volume(Path::new(p))).collect();
            let (mut other, mut unreachable) = (HashSet::new(), HashSet::new());
            let (mut offline_files, mut local_files) = (HashSet::new(), HashSet::new());
            for (key, file, place, gone) in rows {
                if gone {
                    unreachable.insert(key);
                    offline_files.insert(file);
                } else if !on_this_mac[place] {
                    other.insert(key);
                    offline_files.insert(file);
                } else {
                    local_files.insert(file);
                }
            }
            // a newer reload already knows better
            if GENERATION.load(Ordering::SeqCst) == generation || tidy {
                crate::prefs::set_offline_photo_count(offline_files.len());
                cache.set_protection(other, unreachable);
            }
            if tidy {
                tidy_previews(&cache, "Start");
                // developed large previews of photos on this Mac (no offline copy here)
                let small = preview::small();
                let n: usize = local_files.into_iter().filter(|f| !offline_files.contains(f)).map(|f| preview::remove_developed_large(f, small)).sum();
                if n > 0 {
                    crate::log(&format!("previews: {n} developed large previews of photos on this Mac removed"));
                }
                running.store(false, Ordering::SeqCst);
            }
        });
    }

    /// Levels changed or "regenerate" requested in the settings, large previews developed: apply (cleanup in
    /// the background, then visible ones first, then the rest).
    fn preview_requests(&mut self) {
        // Sigma previews renewed in the background (see `sigma_refresh`): drop the old textures, they load again
        for file in crate::sigma_refresh::take_finished() {
            self.textures.retain(|k, _| k.0 != file);
            self.requested.retain(|k| k.0 != file);
            if self.large_requested == Some(file) {
                self.large_requested = None;
            }
        }
        for file in crate::large_preview::take_finished() {
            let large = preview::large();
            if let (Some(e), Some(z)) = (&self.generator, self.rows.iter().find(|z| z.file == file)) {
                e.cache().remove(self.key(z), large);
            }
            self.textures.remove(&(file, large));
            if self.large_requested == Some(file) {
                self.large_requested = None;
            }
        }
        let changed = preview::take_levels_changed();
        let regenerate = preview::take_regenerate();
        if !(changed || regenerate) {
            return;
        }
        let Some(e) = &self.generator else { return };
        let cache = e.cache().clone();
        let done = self.preview_cleanup.clone();
        done.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            if regenerate {
                let n = cache.clear_unprotected();
                crate::log(&format!("previews: {n} removed (to be regenerated)"));
            } else {
                tidy_previews(&cache, "Stufen geändert");
            }
            done.store(false, Ordering::SeqCst);
        });
        self.discard_all_textures();
        self.large_requested = None;
        self.background_set = false;
    }

    fn build_tree(&mut self) {
        self.build_tree_with(None);
    }

    /// Folder tree and date list; `dates` already counted (by a load in the background) or counted here.
    fn build_tree_with(&mut self, dates: Option<(Vec<DateYear>, usize)>) {
        (self.date_tree, self.without_date) = dates.unwrap_or_else(|| library_load::dates_of(&self.rows));
        self.nodes.clear();
        self.root_rows.clear();
        let mut per_folder: HashMap<i64, usize> = HashMap::new();
        for z in self.rows.iter() {
            *per_folder.entry(z.folder).or_default() += 1;
        }
        let mut pos: HashMap<(i64, String), usize> = HashMap::new();
        for s in &self.roots {
            let i = self.nodes.len();
            self.nodes.push(Node { root: s.id, path: String::new(), name: s.name.clone(), ..Default::default() });
            pos.insert((s.id, String::new()), i);
            self.root_rows.push((s.clone(), i));
        }
        let mut folder = self.folder.clone();
        folder.sort_by(|a, b| a.path.cmp(&b.path));
        // Apple Photos: don't show Apple's internal subfolders (0–F), only the root with the total count
        let apple: Vec<i64> = self.roots.iter().filter(|s| s.origin == library::apple_photos::ORIGIN).map(|s| s.id).collect();
        for o in &folder {
            if apple.contains(&o.root) {
                if let Some(&w) = pos.get(&(o.root, String::new())) {
                    self.nodes[w].count += per_folder.get(&o.id).copied().unwrap_or(0);
                }
                continue;
            }
            // create a node for every path component (including intermediate folders without their own entry)
            let mut parent = match pos.get(&(o.root, String::new())) {
                Some(&p) => p,
                None => continue,
            };
            let mut path = String::new();
            for part in o.path.split('/').filter(|t| !t.is_empty()) {
                path.push_str(part);
                path.push('/');
                let key = (o.root, path.clone());
                let i = match pos.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = self.nodes.len();
                        self.nodes.push(Node { root: o.root, path: path.clone(), name: part.to_string(), ..Default::default() });
                        self.nodes[parent].children.push(i);
                        pos.insert(key, i);
                        i
                    }
                };
                parent = i;
            }
            let n = per_folder.get(&o.id).copied().unwrap_or(0);
            // add the count to all ancestors
            let mut p = String::new();
            if let Some(&w) = pos.get(&(o.root, String::new())) {
                self.nodes[w].count += n;
            }
            for part in o.path.split('/').filter(|t| !t.is_empty()) {
                p.push_str(part);
                p.push('/');
                if let Some(&i) = pos.get(&(o.root, p.clone())) {
                    self.nodes[i].count += n;
                }
            }
        }
        // children alphabetically
        let names: Vec<String> = self.nodes.iter().map(|k| k.name.to_lowercase()).collect();
        for k in &mut self.nodes {
            k.children.sort_by(|a, b| names[*a].cmp(&names[*b]));
        }
        self.not_in_catalog_in_tree();
    }

    /// "By date" list: years, expanded months, within them days.
    fn date_list(&mut self, ui: &mut egui::Ui) {
        let tree = std::mem::take(&mut self.date_tree);
        for j in &tree {
            if self.date_row(ui, &j.key, j.key.clone(), j.count, 0, true) {
                for mo in &j.months {
                    if self.date_row(ui, &mo.key, month_name(&mo.key), mo.count, 1, true) {
                        for (t, n) in &mo.days {
                            self.date_row(ui, t, tag_name(t), *n, 2, false);
                        }
                    }
                }
            }
        }
        if self.without_date > 0 {
            self.date_row(ui, "", crate::tr("Ohne Datum").into(), self.without_date, 0, false);
        }
        if tree.is_empty() && self.without_date == 0 && !self.first_loading() {
            ui.label(egui::RichText::new(crate::tr("Noch keine Fotos")).color(lr::TEXT_DIM));
        }
        self.date_tree = tree;
    }

    /// One row of the date list; returns: expanded?
    fn date_row(&mut self, ui: &mut egui::Ui, key: &str, text: String, n: usize, depth: usize, collapsible: bool) -> bool {
        let open = self.open_dates.contains(key);
        let active = self.source_marked(&Source::Date(key.to_string()));
        let (click, triangle, _) = Self::list_row(ui, &text, Some(n), active, depth, collapsible.then_some(open), false);
        if triangle {
            if open {
                self.open_dates.remove(key);
            } else {
                self.open_dates.insert(key.to_string());
            }
        }
        if click {
            self.set_source(Source::Date(key.to_string()));
            if collapsible {
                self.open_dates.insert(key.to_string());
            }
        }
        self.open_dates.contains(key)
    }

    /// IDs of the current source (collection/keyword) from the database.
    fn source_changed(&mut self) {
        let Some(db) = &self.db else { return };
        self.source_ids = match &self.source {
            Source::Collection(id) => Some(db.images_of_collection(*id).unwrap_or_default().into_iter().collect()),
            Source::Quick => Some(self.quick.clone()),
            Source::Keyword(id) => Some(db.images_with_keyword(*id).unwrap_or_default().into_iter().collect()),
            _ => None,
        };
        self.refilter = true;
    }

    /// Text search across the whole library although a folder/collection is selected?
    fn search_global(&self) -> bool {
        !self.search_in_source && !self.filter.text.trim().is_empty() && matches!(self.source, Source::Folder(..) | Source::Collection(_) | Source::Keyword(_))
    }

    fn set_source(&mut self, q: Source) {
        self.sel_ext = None;
        crate::people_ui::close();
        self.pairs.open = false;
        if self.source != q {
            // whoever searches the whole library and then clicks a folder leaves the search
            if self.search_global() {
                self.filter.text.clear();
            }
            self.search_in_source = false;
            self.source = q;
            remember_source(&self.source);
            self.source_changed();
            self.loupe = false;
            // to the start instead of the active photo, which may be somewhere in the middle of the new list
            self.to_active = false;
            self.active_settle = 0;
            self.grid_top = true;
            self.grid_back_y = None;
            self.source_nr += 1;
        }
    }

    /// Show a finished search.
    fn take_search(&mut self, done: search_job::Done) {
        // a new query starts at the top of the grid (instead of jumping to wherever the active photo landed)
        if self.query_for.as_deref() != Some(done.text.as_str()) && !self.to_active {
            self.grid_top = true;
        }
        self.query = done.query;
        self.query_matches = done.matches;
        self.search_time_ms = done.ms;
        self.query_for = Some(done.text);
        self.refilter = true;
        self.motion.pending = true;
    }

    /// Only the search text was typed: the search follows in the background, the grid stays.
    fn search_typed(&mut self) {
        let text = self.filter.text.trim().to_string();
        if text.is_empty() {
            self.refilter = true;
        } else if self.query_for.as_deref() == Some(text.as_str()) {
            // back to the text shown: nothing to search
            self.search_job.cancel();
        } else {
            self.search_job.wish(&text);
        }
    }

    fn evaluate_query(&self, a: &search::Query) -> Option<Vec<bool>> {
        let db = self.db.as_ref()?;
        let k = search::Context { db, rows: &self.rows, folder_paths: &self.folder_paths };
        Some(search::evaluate(&k, a))
    }

    fn apply_filter(&mut self) {
        if !self.refilter {
            return;
        }
        // column filters with a selection are being rebuilt: wait for them (the grid would flash unfiltered)
        if self.facets.is_none() && self.facets_job.as_ref().is_some_and(|j| j.selection_active()) {
            return;
        }
        self.refilter = false;
        let _t = crate::frame_watch::section("library: filter");
        // re-evaluate the search query (full text + fields) only when the text changed
        // Evaluated in the background (search_job.rs); until the result is there, the last one stays
        let text = self.filter.text.trim().to_string();
        if self.query_for.as_deref() != Some(text.as_str()) {
            if text.is_empty() {
                self.search_job.cancel();
                self.query = search::Query::default();
                self.query_matches = None;
                self.search_time_ms = 0.0;
                self.query_for = Some(text);
            } else if std::mem::take(&mut self.search_job.sync_next) {
                if let Some(db) = &self.db {
                    let done = search_job::SearchJob::now(db, &text, &self.rows, &self.folder_paths);
                    self.take_search(done);
                }
            } else {
                self.search_job.wish(&text);
            }
        }
        if let Source::Smart(id) = &self.source {
            let rule = self.collections.iter().find(|s| s.id == *id).and_then(|s| s.search.clone());
            self.source_ids = None;
            let matches = rule.map(|r| self.evaluate_query(&search::parse(&r)).unwrap_or_default());
            self.source_ids = Some(
                matches
                    .map(|v| v.iter().enumerate().filter(|(_, ok)| **ok).map(|(i, _)| self.rows[i].image).collect())
                    .unwrap_or_default(),
            );
        }
        let folder_ids: Option<HashSet<i64>> = match &self.source {
            Source::Folder(root, path) => Some(self.folder.iter().filter(|o| o.root == *root && o.path.starts_with(path.as_str())).map(|o| o.id).collect()),
            _ => None,
        };
        let run = self.last_run;
        let search_global = self.search_global();
        // semantic extension (only with free words and an installed text encoder); only hits for the query shown
        let sem_current = self.update_semantic();
        let empty_rank = HashMap::new();
        let sem_rank = if sem_current { std::mem::take(&mut self.sem_rank) } else { empty_rank };
        let fields: Option<Vec<bool>> = if sem_rank.is_empty() {
            None
        } else {
            let fields_only = search::Query { clauses: self.query.clauses.iter().filter(|k| k.iter().all(|t| matches!(t.kind, search::Kind::Field(..)))).cloned().collect() };
            self.evaluate_query(&fields_only)
        };
        let t_loop = crate::frame_watch::section("filter: rows");
        let mut v2: Vec<usize> = Vec::new();
        let mut v: Vec<usize> = Vec::with_capacity(self.rows.len());
        let mut total = 0usize;
        for (i, z) in self.rows.iter().enumerate() {
            let source_ok = match &self.source {
                _ if search_global => z.folder != VIEW_FOLDER,
                Source::All => z.folder != VIEW_FOLDER,
                Source::Browse(_) => z.folder == VIEW_FOLDER,
                Source::Recent => run.is_some() && z.run == run,
                Source::Missing => z.status == 1,
                Source::Offline => z.status == 2,
                Source::Duplicates => self.dup_order.contains_key(&z.file),
                Source::NotInCatalog => false,
                Source::Folder(..) => folder_ids.as_ref().is_some_and(|s| s.contains(&z.folder)),
                Source::Date(d) if d.is_empty() => z.folder != VIEW_FOLDER && z.capture.as_deref().is_none_or(|a| a.len() < 10),
                Source::Date(d) => z.folder != VIEW_FOLDER && z.capture.as_deref().is_some_and(|a| a.len() >= 10 && a.starts_with(d.as_str())),
                Source::Similar(_) => self.similar_order.contains_key(&z.file),
                Source::Collection(_) | Source::Quick | Source::Keyword(_) | Source::Smart(_) => self.source_ids.as_ref().is_some_and(|s| s.contains(&z.image)),
            };
            if !source_ok {
                continue;
            }
            // camera JPEGs in a stack are not photos of their own and don't count
            if !(self.stack && z.jpeg_for.is_some()) {
                total += 1;
            }
            // stack: a RAW's camera JPEG is part of its image
            // kind: photos, videos or both
            if (self.kind == 0 && z.format == "VIDEO") || (self.kind == 1 && z.format != "VIDEO") {
                continue;
            }
            if self.stack && z.jpeg_for.is_some() {
                continue;
            }
            let mark_ok = self.filter.attributes_match(z.flag, z.stars, z.label);
            if !mark_ok {
                continue;
            }
            if self.query_matches.as_ref().is_some_and(|t| !t[i]) {
                // not in the full text: maybe "similar in meaning" (fields must still match)
                if sem_rank.contains_key(&z.file) && fields.as_ref().is_none_or(|f| f[i]) {
                    v2.push(i);
                }
                continue;
            }
            v.push(i);
        }
        drop(t_loop);
        let t_order = crate::frame_watch::section("filter: columns");
        // column filters (count even when there is no selection – for the display)
        if let Some(f) = self.facets.as_mut().filter(|f| f.rows_len() == self.rows.len()) {
            if f.active() || self.columns_open {
                v = f.apply(&self.rows, &v);
            }
        }
        drop(t_order);
        let t_order = crate::frame_watch::section("filter: order");
        // order
        match (&self.source, self.sort_order) {
            (Source::Similar(_), _) => v.sort_by_key(|&i| self.similar_order.get(&self.rows[i].file).copied().unwrap_or(usize::MAX)),
            (Source::Duplicates, _) => v.sort_by_key(|&i| self.dup_order.get(&self.rows[i].file).copied().unwrap_or((usize::MAX, 0))),
            (Source::Collection(id), SortOrder::CaptureTime) => {
                if let Some(db) = &self.db {
                    let order: HashMap<i64, usize> = db.images_of_collection(*id).unwrap_or_default().into_iter().enumerate().map(|(i, b)| (b, i)).collect();
                    v.sort_by_key(|&i| order.get(&self.rows[i].image).copied().unwrap_or(usize::MAX));
                }
            }
            (_, SortOrder::Name) => v.sort_by(|&a, &b| self.rows[a].name.to_lowercase().cmp(&self.rows[b].name.to_lowercase())),
            (_, SortOrder::Stars) => v.sort_by_key(|&i| std::cmp::Reverse(self.rows[i].stars)),
            // capture time: newest first, without date at the end;
            // same time by name, original before its copies
            (_, SortOrder::CaptureTime) => {
                // the rows come from the database oldest first (`Db::rows`): then only the order of the capture times is
                // turned round (linear); otherwise (e.g. rows of a viewed folder in between) sorted
                match newest_first(&self.rows, &v) {
                    Some(w) => v = w,
                    // By a rank computed once per load (comparing texts took 10–60 ms per search)
                    None => {
                        let rank = self.search_job.capture_rank(&self.rows);
                        v.sort_unstable_by_key(|&i| rank[i]);
                    }
                }
            }
        }
        if self.sort_reversed && !matches!(self.source, Source::Similar(_) | Source::Duplicates) {
            v.reverse();
        }
        // part 2: semantic hits (column filters apply here too), sorted by meaning
        if let Some(f) = self.facets.as_mut().filter(|f| f.rows_len() == self.rows.len()) {
            if f.active() && !v2.is_empty() {
                v2 = f.apply(&self.rows, &v2);
            }
        }
        v2.sort_by_key(|&i| sem_rank.get(&self.rows[i].file).copied().unwrap_or(usize::MAX));
        // the separator row (with the strictness slider) also stands when the threshold cuts everything off but a lenient setting would show hits
        self.sem_from = (!v2.is_empty() || (sem_current && self.sem_view.row)).then_some(v.len());
        if sem_current {
            self.sem_rank = sem_rank;
        }
        v.extend(v2);
        // if the active image drops out (e.g. rejected while rejected photos are hidden), the
        // next one takes its place instead of jumping to the start
        let old_pos = match self.visible_before.take() {
            Some(before) => self.active.and_then(|a| before.iter().position(|&b| b == a)),
            None => self.active.and_then(|a| self.visible.iter().position(|&i| self.rows.get(i).is_some_and(|z| z.image == a))),
        };
        self.visible = v;
        self.source_total = total;
        drop(t_order);
        let _t_print = crate::frame_watch::section("filter: fingerprint");
        // Develop re-reads the list only when what it shows changed (a reload with the same photos costs it nothing)
        let fingerprint = self.list_fingerprint();
        if fingerprint != self.list_fingerprint {
            self.list_fingerprint = fingerprint;
            self.list_version += 1;
            // a new search result: the grid opens like a curtain (search_motion.rs)
            if self.motion.pending {
                self.motion.start(std::time::Instant::now());
            }
        }
        self.motion.pending = false;
        if self.active.is_none_or(|a| !self.visible.iter().any(|&i| self.rows[i].image == a)) {
            let pos = old_pos.unwrap_or(0).min(self.visible.len().saturating_sub(1));
            self.active = self.visible.get(pos).map(|&i| self.rows[i].image);
        }
        self.background_set = false;
    }

    /// Fingerprint of everything [`Self::list`] hands to Develop: order, paths (via the preview key), names, states, ratings.
    fn list_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.visible.len().hash(&mut h);
        for &i in &self.visible {
            let z = &self.rows[i];
            (z.image, z.file, self.key_at(i), z.folder, &z.name, &z.copy, &z.format).hash(&mut h);
            (z.status, z.stars, z.flag, z.label).hash(&mut h);
        }
        // never 0 (the start value): the first list always counts as new
        h.finish() | 1
    }

    /// Hit count of a smart collection (evaluated once, `None` = not translatable).
    fn smart_count(&mut self, id: i64) -> Option<usize> {
        if let Some(n) = self.smart_counts.get(&id) {
            return *n;
        }
        let rule = self.collections.iter().find(|s| s.id == id).and_then(|s| s.search.clone());
        let n = rule.and_then(|r| self.evaluate_query(&search::parse(&r))).map(|v| v.iter().filter(|x| **x).count());
        self.smart_counts.insert(id, n);
        n
    }

    /// "Similar photos" for the active image. Loads the image vectors the first time and computes a
    /// missing vector right away; while that takes, the status is shown at the button (`similar_state`),
    /// afterwards it continues by itself (`similar_continue`).
    fn show_similar(&mut self, ctx: &egui::Context) {
        let Some(&i) = self.active.and_then(|a| self.index.get(&a)) else { return };
        let file = self.rows[i].file;
        self.similar_state = None;
        let loaded = self.vectors.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|r| r.as_ref().map(|v| v.has_real(file)).map_err(|e| e.clone()));
        match loaded {
            None => {
                if !self.vectors_running {
                    self.vectors_running = true;
                    let target = self.vectors.clone();
                    let c = ctx.clone();
                    std::thread::spawn(move || {
                        let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| Vectors::load(&db));
                        *target.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        c.request_repaint();
                    });
                }
                self.similar_state = Some((file, SimilarState::Load));
            }
            Some(Err(e)) => self.similar_state = Some((file, SimilarState::Error(e))),
            Some(Ok(false)) => self.compute_image_vector(file, ctx),
            Some(Ok(true)) => {
                let list = self.vectors.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|r| r.as_ref().ok()).map(|v| v.similar_items(file, 300)).unwrap_or_default();
                self.similar_order = std::iter::once(file).chain(list.iter().map(|(d, _)| *d)).enumerate().map(|(r, d)| (d, r)).collect();
                self.enter_special_source();
                self.source = Source::Similar(file);
                self.source_ids = None;
                self.loupe = false;
                self.refilter = true;
                self.to_active = true;
            }
        }
    }

    /// Compute the image vector of a single photo right away (image module from "Modules").
    fn compute_image_vector(&mut self, file: i64, ctx: &egui::Context) {
        let Some(dir) = library::semantic::image_folder() else {
            self.similar_state = Some((file, SimilarState::Error(crate::tr("Dafür fehlt das Bildmodul (Einstellungen → Module)").into())));
            return;
        };
        self.similar_state = Some((file, SimilarState::Computing));
        let target = self.similar_result.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = (|| {
                let db = Db::standard().map_err(|e| e.to_string())?;
                let (path, format) = library::similar::source_of(&db, file).ok_or_else(|| crate::tr("Foto nicht in der Bibliothek").to_string())?;
                let (img, _) = library::preview::load(&path, &format, 512).map_err(|_| crate::tr("Die Vorschau dieses Fotos ist nicht lesbar").to_string())?;
                let enc = library::semantic::ImageEncoder::load(&dir)?;
                let v = enc.embed(&img);
                let _ = library::similar::save(&db, file, &v);
                Ok(v)
            })();
            *target.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, r));
            c.request_repaint();
        });
    }

    /// Per frame: continue a pending "Similar photos" as soon as the vectors or the image vector are there;
    /// discard the status if another photo has become active in the meantime.
    fn similar_continue(&mut self, ctx: &egui::Context) {
        let Some((file, state)) = self.similar_state.clone() else { return };
        let active = self.active.and_then(|a| self.index.get(&a)).map(|&i| self.rows[i].file);
        if active != Some(file) {
            self.similar_state = None;
            return;
        }
        match state {
            SimilarState::Load => {
                if self.vectors.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    self.show_similar(ctx);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
            }
            SimilarState::Computing => match { let r = self.similar_result.lock().unwrap_or_else(|e| e.into_inner()).take(); r } {
                Some((d, Ok(v))) if d == file => {
                    if let Some(Ok(vectors)) = self.vectors.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                        vectors.insert_into(file, &v);
                    }
                    if v.iter().any(|x| x.abs() > 1e-6) {
                        self.show_similar(ctx);
                    } else {
                        self.similar_state = Some((file, SimilarState::Error(crate::tr("Die Vorschau dieses Fotos ist nicht lesbar").into())));
                    }
                }
                Some((d, Err(e))) if d == file => self.similar_state = Some((file, SimilarState::Error(e))),
                _ => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
            },
            SimilarState::Error(_) => {}
        }
    }

    /// Metadata rows of the info for a photo (label, value).
    fn meta_rows(&self, z: &Row) -> Vec<(&'static str, String)> {
        self.meta_rows_with(z, true)
    }

    /// The most important first, nothing twice (`info_rows.rs`). `file`: complete with the details of
    /// the opened file (only for the photo they belong to; the comparison of a multiple selection leaves them out).
    fn meta_rows_with(&self, z: &Row, file: bool) -> Vec<(&'static str, String)> {
        let path = self.path_of(z);
        let details: &[(String, String)] = match self.file_details.as_ref().filter(|(img, _)| file && *img == z.image) {
            Some((_, d)) => d,
            None => &[],
        };
        let from_file = |k: &str| crate::info_rows::detail(details, k).map(str::to_string);
        let mut rows: Vec<(&'static str, String)> = vec![(crate::tr("Dateiname"), display_name(z))];
        if let Some(k) = &z.copy {
            rows.push(("Kopie", k.clone()));
        }
        // the file names the time as EXIF does ("2013:03:15 15:19:47"): shown like the library's
        let exif_time = |t: String| match (t.get(0..4), t.get(5..7), t.get(8..10), t.get(11..19)) {
            (Some(y), Some(m), Some(d), Some(h)) if t.as_bytes().get(4) == Some(&b':') => capture_date(&format!("{y}-{m}-{d}T{h}")),
            _ => t,
        };
        if let Some(a) = z.capture.as_ref().map(|a| capture_date(a)).or_else(|| from_file("Aufnahme").map(exif_time)) {
            rows.push((crate::tr("Aufnahme"), a));
        }
        if let Some(k) = z.camera.clone().or_else(|| from_file("Kamera")) {
            rows.push((crate::tr("Kamera"), k));
        }
        // the lens by name (the X3F details name only the focal and aperture range: those stay below)
        let lens = z.lens.clone().or_else(|| from_file("Objektiv").filter(|o| !o.contains("mm · f/")));
        if let Some(o) = lens {
            rows.push((crate::tr("Objektiv"), o));
        }
        let exposure = if z.time.is_some() || z.aperture.is_some() || z.iso.is_some() || z.focal.is_some() {
            crate::info_rows::exposure_line(
                z.time.map(|v| crate::library_filter::value_text(3, v)),
                z.aperture.map(|v| crate::library_filter::value_text(0, v)),
                z.iso.map(|v| format!("ISO {v}")),
                z.focal.map(|v| format!("{} mm", v.round())),
                crate::info_rows::detail(details, "Brennweite"),
            )
        } else {
            from_file("Belichtung").and_then(|b| crate::info_rows::exposure_line(None, Some(b.de_comma()), None, None, crate::info_rows::detail(details, "Brennweite")))
        };
        if let Some(b) = exposure {
            rows.push((crate::tr("Belichtung"), b));
        }
        if let (Some(w), Some(h)) = (z.width, z.height) {
            rows.push((crate::tr("Maße"), format!("{w} × {h}")));
        }
        if let Some(g) = z.size {
            rows.push((crate::tr("Größe"), format!("{:.1} MB", g as f64 / 1e6).de_comma()));
        }
        rows.push(("Format", self.format_text(z)));
        rows.push((crate::tr("Ordner"), path.parent().map(|p| p.display().to_string()).unwrap_or_default()));
        // the condition only when something is wrong
        let state = match z.status {
            1 => Some(crate::tr("fehlt").to_string()),
            2 => Some(match volume_name(&path) {
                Some(v) => crate::trf!("Speicherort „{v}“ nicht erreichbar", v = v),
                None => crate::tr("Speicherort nicht erreichbar").into(),
            }),
            // conditions of the photo found on opening (damaged, Sigma-exact unavailable …, see `photo_state`)
            _ => Some(crate::photo_state::get(&path).into_iter().map(|n| n.state).collect::<Vec<_>>().join(" · ")).filter(|t| !t.is_empty()),
        };
        if let Some(t) = state {
            rows.push((crate::tr("Zustand"), t));
        }
        if self.db.as_ref().is_some_and(|d| library::location::is_pending(d, z.file)) {
            rows.push((crate::tr("Begleitdatei"), crate::tr("wartet auf Speicherort").into()));
        }
        if let Some(e) = self.db.as_ref().and_then(|d| d.conn.query_row("SELECT label_lr FROM image WHERE id=?1", [z.image], |r| r.get::<_, Option<String>>(0)).ok().flatten()) {
            rows.push((crate::tr("LR-Etikett"), e));
        }
        if let Some(t) = self.db.as_ref().and_then(|d| d.title_of(z.image)) {
            rows.push((crate::tr("Titel"), t));
        }
        if let Some(t) = self.db.as_ref().and_then(|d| d.image_text_of(z.image)) {
            rows.push(("Bildtext", t));
        }
        if let Some((lat, lon)) = self.db.as_ref().and_then(|d| d.position_of(z.image)) {
            rows.push((crate::tr("GPS-Position"), format!("{lat:.5}, {lon:.5}")));
        }
        if let Some(c) = self.db.as_ref().and_then(|d| d.copyright_of(z.image)) { rows.push((crate::tr("Copyright"), c)); } // copyright from the import
        rows
    }

    /// Labels of the metadata rows in which the selected photos differ (empty for
    /// one photo). Computed only when the selection or active photo changes; at most 500 photos.
    fn meta_differing(&mut self) -> HashSet<&'static str> {
        let target = self.target();
        if target.len() < 2 {
            return HashSet::new();
        }
        let mut sorted = target.clone();
        sorted.sort_unstable();
        let key = (sorted, self.active);
        if let Some((k, v)) = &self.meta_comparison {
            if *k == key {
                return v.clone();
            }
        }
        let mut values: HashMap<&'static str, String> = HashMap::new();
        let mut differing: HashSet<&'static str> = HashSet::new();
        let mut first = true;
        for b in target.iter().take(500) {
            let Some(&i) = self.index.get(b) else { continue };
            let rows = self.meta_rows_with(&self.rows[i], false);
            let here: HashMap<&'static str, String> = rows.into_iter().collect();
            if first {
                values = here;
                first = false;
                continue;
            }
            for (k, v) in &values {
                if here.get(k) != Some(v) {
                    differing.insert(*k);
                }
            }
            for k in here.keys() {
                if !values.contains_key(k) {
                    differing.insert(*k);
                }
            }
        }
        self.meta_comparison = Some((key, differing.clone()));
        differing
    }

    /// File type for Info, unambiguous: "X3F · Foveon RAW", "JPEG · camera JPEG for X.X3F" …
    fn format_text(&self, z: &Row) -> String {
        let name_of = |b: i64| self.index.get(&b).map(|&i| self.rows[i].name.clone()).unwrap_or_default();
        let kind = match crate::image_kind::from_extension(z.format.as_str()) {
            crate::image_kind::ImageKind::Foveon => crate::trf!("{a} · Foveon-RAW", a = z.format.as_str()),
            crate::image_kind::ImageKind::Raw => format!("{} · RAW", z.format.as_str()),
            crate::image_kind::ImageKind::Done if z.format == "JPG" => "JPEG".to_string(),
            _ => z.format.as_str().to_string(),
        };
        if let Some(r) = z.jpeg_for {
            return crate::trf!("JPEG · Kamera-JPEG zu {a}", a = name_of(r));
        }
        match z.sidecar_jpeg {
            Some(j) => crate::trf!("{kind} + Kamera-JPEG ({a})", kind = kind, a = name_of(j)),
            None => kind,
        }
    }

    /// File of the image.
    fn path_of(&self, z: &Row) -> PathBuf {
        PathBuf::from(row_path(&self.folder_paths, z))
    }

    /// Discard all grid textures (the stack recomputed previews): the visible ones are requested again.
    pub fn discard_all_textures(&mut self) {
        self.textures.clear();
        self.requested.clear();
    }

    fn key(&self, z: &Row) -> u64 {
        row_key(&self.folder_paths, z)
    }

    /// Preview key of row `i`: computed by the load in the background, otherwise here.
    fn key_at(&self, i: usize) -> u64 {
        let z = &self.rows[i];
        match self.row_keys.get(i) {
            Some(&(file, key)) if file == z.file => key,
            _ => self.key(z),
        }
    }

    fn task(&self, z: &Row, level: u32) -> Job {
        Job { file: z.file, path: self.path_of(z), format: z.format.clone(), key: self.key(z), level: level, orientation: z.orientation }
    }

    /// Counts of the catalog entries in the left panel: recent import, archived, missing, not connected. Counted at most
    /// once a second (four passes over every photo of the library in each frame cost 5–10 ms per frame).
    fn catalog_counts(&mut self) -> [usize; 3] {
        if let Some((t, n, run, c)) = self.catalog_counts {
            if n == self.rows.len() && run == self.last_run && t.elapsed() < std::time::Duration::from_secs(1) {
                return c;
            }
        }
        let mut c = [0; 3];
        for z in self.rows.iter() {
            c[0] += usize::from(self.last_run.is_some() && z.run == self.last_run);
            c[1] += usize::from(z.status == 1);
            c[2] += usize::from(z.status == 2);
        }
        self.catalog_counts = Some((std::time::Instant::now(), self.rows.len(), self.last_run, c));
        c
    }

    /// Take over finished thumbnails as textures.
    fn results(&mut self, ctx: &egui::Context) {
        let Some(e) = &self.generator else { return };
        // a second offline copy only while nothing happens (paging and editing keep the processor)
        e.set_heavy_limit(if crate::jobs::idle() { 2 } else { 1 });
        self.queue.extend(e.fetch());
        // at most one large preview per frame: each is up to 16 MP – converting and uploading several in one frame
        // (the large previews decoded ahead arrive together) stalled the window while paging
        let mut n = 0;
        let mut large_seen = false;
        while n < self.queue.len().min(TEXTURES_PER_FRAME) {
            let q = &self.queue[n];
            let large = q.image.is_some() && q.level > preview::small();
            if large && large_seen {
                break;
            }
            large_seen |= large;
            n += 1;
        }
        for r in self.queue.drain(..n).collect::<Vec<_>>() {
            if let Some(m) = r.meta {
                if m.capture.is_some() || m.camera.is_some() || m.width.is_some() {
                    self.meta_pending.push((r.file, m));
                }
            }
            // an offline copy made in the background (e.g. the development replacing the camera JPEG): show it anew
            if r.image.is_none() && r.error.is_none() && r.level == preview::large() && r.level > preview::small() && self.textures.remove(&(r.file, r.level)).is_some() && self.large_requested == Some(r.file) {
                self.large_requested = None;
            }
            if let Some(img) = r.image {
                let ci = egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], img.as_raw());
                let tex = ctx.load_texture(format!("bib-{}-{}", r.file, r.level), ci, egui::TextureOptions::LINEAR);
                self.clock += 1;
                self.textures.insert((r.file, r.level), (tex, self.clock));
            }
            self.requested.retain(|k| *k != (r.file, r.level));
        }
        if !self.queue.is_empty() {
            ctx.request_repaint();
        }
        // collected extra knowledge into the database (empty fields only)
        if self.meta_pending.len() >= 64 || (!self.meta_pending.is_empty() && self.queue.is_empty()) {
            // written by the background writer (the UI never writes or waits for the database: before 0.108 this ran
            // here and afterwards left the UI connection at a 3 s busy timeout for every later read)
            let pending = std::mem::take(&mut self.meta_pending);
            let n = pending.len();
            self.in_background(
                move |db| {
                    let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
                    for (d, m) in &pending {
                        db.complete_file(*d, m.width, m.height, m.capture.as_deref(), m.camera.as_deref()).map_err(|e| e.to_string())?;
                    }
                    tx.commit().map_err(|e| e.to_string())
                },
                move |_, _, r| {
                    if let Err(e) = r {
                        crate::log(&format!("library: {n} completions (size, date, camera) not written: {e}"));
                    }
                },
            );
        }
        // large previews are big textures (up to full resolution): keep only the last few
        let large = preview::large();
        if large > preview::small() {
            let mut big: Vec<((i64, u32), u64)> = self.textures.iter().filter(|(k, _)| k.1 == large).map(|(k, v)| (*k, v.1)).collect();
            if big.len() > LARGE_TEXTURES_MAX {
                big.sort_by_key(|e| std::cmp::Reverse(e.1));
                for (k, _) in big.into_iter().skip(LARGE_TEXTURES_MAX) {
                    self.textures.remove(&k);
                }
            }
        }
        // LRU of the textures
        if self.textures.len() > TEXTURES_MAX {
            let mut all: Vec<((i64, u32), u64)> = self.textures.iter().map(|(k, v)| (*k, v.1)).collect();
            all.sort_by_key(|e| e.1);
            for (k, _) in all.iter().take(self.textures.len() - TEXTURES_MAX * 9 / 10) {
                self.textures.remove(k);
            }
        }
    }

    /// Get the texture for an image (records the use); prefers the requested level.
    fn texture(&mut self, file: i64, level: u32) -> Option<TextureHandle> {
        self.clock += 1;
        let clock = self.clock;
        for s in [level, preview::large(), preview::small()] {
            if let Some(t) = self.textures.get_mut(&(file, s)) {
                t.1 = clock;
                return Some(t.0.clone());
            }
        }
        None
    }

    /// Background generation: everything present without a thumbnail, visible order first.
    fn background(&mut self) {
        let Some(e) = &self.generator else { return };
        let protection = e.cache().protection_generation();
        if self.background_set && protection == self.background_protection {
            return;
        }
        self.background_set = true;
        self.background_protection = protection;
        // The order runs over every photo of the library (66 000: 100–200 ms) – in a thread, after
        // every new search result it blocked the window
        static ROUND: AtomicU64 = AtomicU64::new(0);
        let round = ROUND.fetch_add(1, Ordering::SeqCst) + 1;
        let (cache, generator) = (e.cache().clone(), e.clone());
        let (rows, folders, row_keys, visible) = (self.rows.clone(), self.folder_paths.clone(), self.row_keys.clone(), self.visible.clone());
        std::thread::spawn(move || {
            let key_at = |i: usize| match row_keys.get(i) {
                Some(&(file, key)) if file == rows[i].file => key,
                _ => row_key(&folders, &rows[i]),
            };
            let mut seen = HashSet::new();
            let mut list = Vec::new();
            let mut larges = Vec::new();
            let (small, large) = (preview::small(), preview::large());
            for i in visible.iter().copied().chain(0..rows.len()) {
                if ROUND.load(Ordering::SeqCst) != round {
                    return;
                }
                let z = &rows[i];
                if z.status != 0 || !seen.insert(z.file) {
                    continue;
                }
                // the key once per photo (it is the same for both levels); the job only where one is needed
                let key = key_at(i);
                if !cache.has(key, small) {
                    list.push(Job { file: z.file, path: PathBuf::from(row_path(&folders, z)), format: z.format.clone(), key, level: small, orientation: z.orientation });
                }
                larges.push((i, key));
            }
            // then the offline copy (large level), in the same order, only for photos on other drives (needed once the
            // drive is disconnected); photos on this Mac get none. Before the drives are known (protection 0), none at all.
            // `rows` is the whole library (not only the list shown), so the order only puts the visible ones first.
            // Offline copies already developed are not queued at all: the counter "Previews: x left" then shows the
            // work really left (checking the mark reads the start of each file).
            for (i, key) in larges {
                if protection > 0 && cache.is_protected(key) {
                    let z = &rows[i];
                    let a = Job { file: z.file, path: PathBuf::from(row_path(&folders, z)), format: z.format.clone(), key: row_key(&folders, z), level: large, orientation: z.orientation };
                    if !cache.has(a.key, large) || (preview::develops(&a.format) && !cache.is_developed(a.key, a.level)) {
                        list.push(a);
                    }
                }
            }
            if ROUND.load(Ordering::SeqCst) != round {
                return;
            }
            generator.background(list);
            // keys of all photos whose offline copy is developed (RAW/X3F on other drives, reachable or not): progress
            // counter "Offline copies developed: x of y" in the settings
            let mut seen = HashSet::new();
            let keys: Vec<u64> = if protection > 0 {
                rows.iter().filter(|z| seen.insert(z.file) && preview::develops(&z.format)).map(|z| row_key(&folders, z)).filter(|k| cache.is_protected(*k)).collect()
            } else {
                Vec::new()
            };
            crate::prefs::set_offline_develop_keys(keys);
        });
        let on = crate::env::var("GRAINEO_PREVIEW_BACKGROUND").map_or(true, |v| v != "0") && library::settings::value("previewsBackground").as_bool().unwrap_or(true);
        e.background_on(on && !self.rows.is_empty());
    }

    // ───────────────────────────────────────────── Background work

    fn start_job(&mut self, ctx: &egui::Context, kind: &'static str, arg: Option<PathBuf>) {
        {
            let mut a = self.work.lock().unwrap_or_else(|e| e.into_inner());
            if a.running {
                return;
            }
            *a = Work { running: true, kind, message: crate::tr("Beginne …").into(), fraction: None, handover_seen: false, outcome: None, interim: false, started: Some(std::time::Instant::now()), last: None };
        }
        let work = self.work.clone();
        let c = ctx.clone();
        if matches!(kind, "lightroom" | "lr_handover") {
            self.lr_job_catalog = arg.clone();
        }
        // the catalog's collections go into this set (empty: at the top level as in Lightroom)
        let lr_options = library::lightroom::Options { collection_set: self.lr_set_name.clone().filter(|n| !n.trim().is_empty()) };
        std::thread::spawn(move || {
            // automatic syncs nobody waits for: lowest priority (efficiency cores), they pause while the user works;
            // what the user started runs at "utility"
            library::platform::set_priority(if automatic(kind) { library::platform::Priority::Background } else { library::platform::Priority::Utility });
            let step = |fraction: Option<f32>, s: &str| {
                let mut a = work.lock().unwrap_or_else(|e| e.into_inner());
                a.message = s.to_string();
                a.fraction = fraction;
                a.handover_seen |= fraction.is_some();
                c.request_repaint();
            };
            let report = |s: &str| step(None, s);
            // sync: text of the step without resetting the share, and the share of the folders done
            let sync_text = |s: &str| {
                work.lock().unwrap_or_else(|e| e.into_inner()).message = s.to_string();
                c.request_repaint();
            };
            let sync_count = |done: usize, total: usize| {
                work.lock().unwrap_or_else(|e| e.into_inner()).fraction = Some(done as f32 / total.max(1) as f32);
                c.request_repaint();
            };
            let sync_pause = || {
                crate::jobs::wait_for_foreground(std::time::Duration::from_secs(30));
            };
            if crate::move_folder_ui::resuming() {
                step(None, crate::tr("Unterbrochenes Verschieben wird abgeschlossen …"));
                crate::move_folder_ui::wait_for_resume();
            }
            if automatic(kind) || kind == "check" {
                work.lock().unwrap_or_else(|e| e.into_inner()).last = crate::lightroom_state::newest_sync_duration(&library::data_dir().join("reports"));
            }
            let outcome = (|| -> Result<serde_json::Value, String> {
                let db = Db::standard().map_err(|e| e.to_string())?;
                match kind {
                    "lightroom" => {
                        let k = arg.ok_or_else(|| crate::tr("Kein Lightroom-Katalog gewählt").to_string())?;
                        let t = std::time::Instant::now();
                        let b = library::lightroom::import_with(&db, &k, &lr_options, &report)?;
                        let ti = t.elapsed().as_millis();
                        let a = library::reconcile::reconcile(&db, None, &report)?;
                        let mut v = serde_json::json!({"kind": "lightroom", "import": b.json(), "sync": a.json(), "import_duration_ms": ti});
                        // the develop data goes into the library right away: afterwards nothing needs the catalog
                        match lightroom_handover(&db, &k, &step) {
                            Ok(u) => v["handover"] = u,
                            Err(e) => v["handover_error"] = e.into(),
                        }
                        // sidecars of the newly imported photos are read once in the background
                        library::sidecar_import::wake();
                        Ok(v)
                    }
                    "lr_handover" => {
                        let k = arg.ok_or_else(|| crate::tr("Kein Lightroom-Katalog gewählt").to_string())?;
                        let u = lightroom_handover(&db, &k, &step)?;
                        return Ok(serde_json::json!({"kind": "lr_handover", "handover": u}));
                    }
                    "folder" => {
                        let o = arg.ok_or(crate::tr("kein Ordner"))?;
                        let (_, a) = library::reconcile::add_root(&db, &o, &report)?;
                        // "+" in FOLDERS is no import from the import window: not "Zuletzt importiert" (`Db::last_import`)
                        let _ = db.mark_run(a.run, "root_folder");
                        Ok(serde_json::json!({"kind": "folder", "folder": o, "sync": a.json()}))
                    }
                    "index" => Ok(serde_json::json!({"kind": "index"})),
                    "check" => {
                        let t = std::time::Instant::now();
                        let a = library::reconcile::run(&db, None, &library::reconcile::Options { depth: library::reconcile::Depth::Full, write_now: None, progress: &sync_text, count: &sync_count, pause: &|| {} })?;
                        Ok(serde_json::json!({"kind": "check", "sync": a.json(), "duration_ms": t.elapsed().as_millis()}))
                    }
                    "rebuild" => {
                        let target = db.path.with_file_name("library-rebuild.sqlite");
                        let w = library::backup::roots_file(&db.path);
                        let k = db.info("lightroom_catalog").map(PathBuf::from);
                        let t = std::time::Instant::now();
                        let mut v = library::backup::rebuild(&target, &w, k, &report)?;
                        v["kind"] = "rebuild".into();
                        v["file"] = serde_json::json!(target);
                        v["duration_ms"] = serde_json::json!(t.elapsed().as_millis());
                        return Ok(v);
                    }
                    _ => {
                        // one root folder (FSEvents/network drive) or all. Quick (unchanged folders are not checked
                        // file by file); a full check once a day while the user is away, or when asked for
                        let root = arg.as_deref().and_then(|p| db.root_to_path(p)).map(|s| s.id);
                        let depth = if kind == "sync_full" { library::reconcile::Depth::Full } else { library::reconcile::Depth::Quick };
                        let a = library::reconcile::run(&db, root, &library::reconcile::Options { depth, write_now: None, progress: &sync_text, count: &sync_count, pause: &sync_pause })?;
                        Ok(serde_json::json!({"kind": "sync", "sync": a.json()}))
                    }
                }
                .and_then(|mut v| {
                    // post-processing: missing capture dates, search index
                    let new = v["sync"]["newly_added"].as_u64().unwrap_or(0) > 0;
                    if automatic(kind) {
                        sync_pause();
                    }
                    report("Aufnahmedaten lesen …");
                    let _ = library::exif::backfill_with(&db, 100_000, &std::sync::atomic::AtomicBool::new(false), &|_| {
                        work.lock().unwrap_or_else(|e| e.into_inner()).interim = true;
                        c.request_repaint();
                    });
                    report(crate::tr("Texte in den Dateien lesen …"));
                    let _ = library::textmeta::backfill(&db, 100_000, &std::sync::atomic::AtomicBool::new(false));
                    // The search index is rebuilt completely (over 10 s write lock) – after automatic
                    // syncs at most every 10 minutes
                    static LAST_INDEX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                    let now_s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    let index_due = now_s.saturating_sub(LAST_INDEX.load(std::sync::atomic::Ordering::Relaxed)) >= 600;
                    if !automatic(kind) || (new && index_due) {
                        LAST_INDEX.store(now_s, std::sync::atomic::Ordering::Relaxed);
                        report("Suchindex aufbauen …");
                        let (n, ms) = library::search::build_index(&db)?;
                        v["index"] = serde_json::json!({"images": n, "ms": ms});
                    }
                    Ok(v)
                })
            })();
            // report as a file (for "Show report")
            if let Ok(v) = &outcome {
                let dir = library::data_dir().join("reports");
                let _ = std::fs::create_dir_all(&dir);
                let name = format!("{}-{}.json", v["kind"].as_str().unwrap_or("run"), library::now());
                let _ = std::fs::write(dir.join(name), serde_json::to_string_pretty(v).unwrap_or_default());
            }
            let mut a = work.lock().unwrap_or_else(|e| e.into_inner());
            a.running = false;
            a.outcome = Some(outcome);
            c.request_repaint();
        });
    }

    fn check_work(&mut self) -> Option<Action> {
        let (finished, interim, kind) = {
            let mut a = self.work.lock().unwrap_or_else(|e| e.into_inner());
            (a.outcome.take(), std::mem::take(&mut a.interim), a.kind)
        };
        // a Lightroom run that could not read the catalog: the dialog shows it (with the path and the reason) and
        // keeps its import button disabled, instead of only a status line
        if let (Some(Err(e)), true) = (&finished, matches!(kind, "lightroom" | "lr_handover")) {
            if let Some(k) = self.lr_job_catalog.clone().filter(|_| e.contains("Katalog nicht lesbar") || e.contains("not a database") || e.contains("no such table")) {
                self.lr_failed = Some((k, e.clone()));
                self.lr_state = None;
                self.lr_window = true;
            }
        }
        // interim state of the capture dates: reload at most every 5 s
        if interim && finished.is_none() && self.interim_loaded.is_none_or(|t| t.elapsed().as_secs() >= 5) {
            self.interim_loaded = Some(std::time::Instant::now());
            // the list follows when the load is adopted (`adopt_load` filters again)
            self.reload();
        }
        match finished? {
            Ok(v) => {
                let quiet = v["kind"] == "sync" || v["kind"] == "index";
                if v["kind"] == "rebuild" {
                    return Some(self.swap_rebuild(&v));
                }
                // silent sync without changes: don't reload (no flicker)
                let a = &v["sync"];
                let nothing = v["kind"] == "sync"
                    && ["newly_added", "relinked", "sidecar_new", "from_sidecar"].iter().all(|k| a[*k].as_u64().unwrap_or(0) == 0)
                    && a["missing"].as_u64() == Some(self.rows.iter().filter(|z| z.status == 1).count() as u64);
                if !nothing {
                    self.reload();
                }
                // silent sync without change: no message
                if nothing {
                    return None;
                }
                let text = if v["kind"] == "index" {
                    crate::trf!("Suchindex fertig ({a} Bilder, {b} ms)", a = v["index"]["images"], b = v["index"]["ms"])
                } else if quiet {
                    let a = &v["sync"];
                    crate::menu::i18n::sync_status(a["existing"].as_u64(), a["missing"].as_u64(), a["newly_added"].as_u64())
                } else if v["kind"] == "lightroom" || v["kind"] == "lr_handover" {
                    // the Lightroom dialog shows the new state and the result of the run
                    let handover = v["kind"] == "lr_handover";
                    self.report = Some(v);
                    self.lr_state = None;
                    self.lr_failed = None;
                    // the check described the state before the run: done now
                    *self.lr_check.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    self.lr_window = true;
                    if handover { crate::tr("Lightroom-Bearbeitungen übernommen").to_string() } else { crate::tr("Bibliothek aktualisiert").to_string() }
                } else {
                    // check of the library, added folder: result in its own dialog
                    self.dialog = Some(sync_outcome(if v["kind"] == "check" { crate::tr("Bibliothek geprüft") } else { crate::tr("Bibliothek aktualisiert") }, &v));
                    crate::tr("Bibliothek aktualisiert").to_string()
                };
                Some(Action::Status(text))
            }
            // "database is locked": another part wrote longer than the timeout; the sync catches up later
            Err(e) if e.contains("locked") || e.contains("busy") => Some(Action::Status(crate::tr("Bibliothek war gerade belegt – der Vorgang wurde abgebrochen, der automatische Abgleich läuft später erneut").into())),
            Err(e) => Some(Action::Status(crate::trf!("Bibliothek: {e}", e = e))),
        }
    }

    // ───────────────────────────────────────────── Rating

    /// Target images: the selection (if the active image belongs to it), otherwise the active image.
    fn target(&self) -> Vec<i64> {
        match self.active {
            Some(a) if self.selection.contains(&a) && self.selection.len() > 1 => self.selection.iter().copied().collect(),
            Some(a) => vec![a],
            None => Vec::new(),
        }
    }

    fn rate(&mut self, stars: Option<u8>, flag: Option<i8>, label: Option<u8>, actions: &mut Vec<Action>) {
        let target = self.target();
        let new = self.rate_images(&target, stars, flag, label);
        if self.take_rating_refused() {
            actions.push(Action::Status(crate::tr(RATING_REFUSED).to_string()));
        }
        if !new.is_empty() {
            actions.push(Action::Rated(new));
        }
    }

    /// Set the rating of these images immediately (grid, filter); database and sidecars in the
    /// background. Label: pressing the same key again removes it.
    /// Returns: new values (for the filmstrip).
    pub fn rate_images(&mut self, target: &[i64], stars: Option<u8>, flag: Option<i8>, label: Option<u8>) -> Vec<crate::rating::Value> {
        let label = label.map(|e| {
            let all_same = target.iter().all(|b| self.index.get(b).is_some_and(|&i| self.rows[i].label == e));
            if all_same {
                0
            } else {
                e
            }
        });
        // stack: the camera JPEG gets the same rating (both sidecars)
        let mut target = target.to_vec();
        if self.stack {
            let extra: Vec<i64> = target.iter().filter_map(|b| self.index.get(b).and_then(|&i| self.rows[i].sidecar_jpeg)).collect();
            for b in extra {
                if !target.contains(&b) {
                    target.push(b);
                }
            }
        }
        // a viewed folder is only looked at: ratings there only with the switch (sidecars next to the photos)
        if !view_folder_sidecars() && target.iter().any(|b| self.index.get(b).is_some_and(|&i| self.rows[i].folder == VIEW_FOLDER)) {
            self.rating_refused = true;
            return Vec::new();
        }
        let (mut old, mut new) = (Vec::new(), Vec::new());
        for b in &target {
            let Some(&i) = self.index.get(b) else { continue };
            let z = &mut Arc::make_mut(&mut self.rows)[i];
            old.push((*b, z.stars, z.flag, z.label));
            if let Some(s) = stars {
                z.stars = s;
            }
            if let Some(m) = flag {
                z.flag = m;
            }
            if let Some(e) = label {
                z.label = e;
            }
            new.push((*b, z.stars, z.flag, z.label));
        }
        if new.is_empty() || old == new {
            return new;
        }
        self.rating_undo.push(old);
        self.rating_at.push(crate::undo_order::tick());
        if self.rating_undo.len() > 200 {
            self.rating_undo.remove(0);
            self.rating_at.remove(0);
        }
        self.last_was_rating = true;
        self.write_rating(&new);
        // filter again, but the active image stays until you move on
        self.refilter = true;
        new
    }

    /// Undo the last rating (⌘Z right after it). Returns: the restored values.
    pub fn undo_rating_change(&mut self) -> Option<Vec<crate::rating::Value>> {
        if !self.last_was_rating {
            return None;
        }
        let old = self.rating_undo.pop()?;
        self.rating_at.pop();
        for &(b, s, m, e) in &old {
            if let Some(&i) = self.index.get(&b) {
                let z = &mut Arc::make_mut(&mut self.rows)[i];
                (z.stars, z.flag, z.label) = (s, m, e);
            }
        }
        self.last_was_rating = !self.rating_undo.is_empty();
        self.write_rating(&old);
        self.refilter = true;
        Some(old)
    }

    /// When the rating change ⌘Z would take back was made (`None`: none left).
    pub fn rating_stamp(&self) -> Option<u64> {
        self.rating_at.last().copied()
    }

    /// ⌘Z chose the rating as the most recent action: take it back even if other actions were flagged since.
    pub fn rating_recent(&mut self) {
        self.last_was_rating = !self.rating_undo.is_empty();
    }

    /// When the step ⌘Z of the grid takes back was made: the management (collections, keywords …) or the last move.
    pub fn external_stamp(&self) -> u64 {
        self.manage.undo_at.last().copied().unwrap_or(0).max(self.move_at)
    }

    /// Another undoable action (slider, move) came after the last rating.
    pub fn rating_not_recent(&mut self) {
        self.last_was_rating = false;
    }

    /// Is this image still being written? (Then the rating in memory applies.)
    pub fn rating_pending(&self, image: i64) -> bool {
        self.writer.writing(image)
    }

    /// Sidecars still to write next to the photos (0 while writing them is off).
    pub fn sidecars_waiting(&self) -> usize {
        self.pending_sidecars
    }

    pub fn ratings_written(&self) -> bool {
        self.writer.all_written()
    }

    /// Ratings and other library writes all carried out (for quitting)?
    pub fn writes_finished(&self) -> bool {
        self.writer.all_written() && self.db_writer.all_written()
    }

    /// Write jobs: library (database + sidecars of existing files) or, for
    /// "View folder", the sidecar directly.
    fn write_rating(&mut self, values: &[crate::rating::Value]) {
        use crate::rating::Task;
        self.loader.rated(values.iter().map(|w| w.0));
        let mut lib_entries = Vec::new();
        let mut sidecar = Vec::new();
        for w in values {
            let Some(&i) = self.index.get(&w.0) else { continue };
            let z = &self.rows[i];
            if z.folder == VIEW_FOLDER {
                // nothing next to the photos of a folder only being viewed unless the switch allows it
                if !view_folder_sidecars() {
                    continue;
                }
                let a = Task::File { image: w.0, path: self.path_of(z), format: z.format.clone(), copy: z.copy.clone(), value: (w.1, w.2, w.3) };
                self.writer.send(a);
                continue;
            }
            lib_entries.push(*w);
            // location not reachable (2): the sidecar write waits in the queue (`library::location`);
            // missing file: database only
            if matches!(z.status, 0 | 2) && !sidecar.contains(&z.file) {
                sidecar.push(z.file);
            }
        }
        if !lib_entries.is_empty() && self.db.is_some() {
            self.writer.send(Task::Library { values: lib_entries, sidecar });
        }
    }

    /// Double-click: single image (even non-developable files show the large preview there).
    fn open(&mut self, image: i64, actions: &mut Vec<Action>) {
        if !self.index.contains_key(&image) {
            return;
        }
        actions.push(Action::Develop);
    }

    /// Step in the grid (arrow keys, ⇧ after rating).
    pub fn step_external(&mut self, d: isize) {
        self.step(d);
    }

    /// Columns of the grid (↑/↓ jump one row).
    pub fn column_count(&self) -> usize {
        self.columns.max(1)
    }

    /// What counts as selected now: the selection, or just the active photo (a single photo is not stored in
    /// `selection`), or nothing.
    fn selected_ids(&self) -> HashSet<i64> {
        if self.nothing_selected {
            HashSet::new()
        } else if self.selection.is_empty() {
            self.active.into_iter().collect()
        } else {
            self.selection.clone()
        }
    }

    fn position_of(&self, image: i64) -> Option<usize> {
        self.visible.iter().position(|&i| self.rows[i].image == image)
    }

    /// ⇧+arrow: extend the selection from the anchor (the active photo when it started) towards the arrow;
    /// the opposite direction shrinks it back. `d`: ±1 (←/→) or ±columns (↑/↓). The anchor stays active;
    /// the moving end is scrolled into view.
    pub fn extend_external(&mut self, d: isize) {
        use crate::library_select as sel;
        let n = self.visible.len();
        if n == 0 {
            return;
        }
        let now = self.selected_ids();
        let kept = self.sel_ext.as_ref().filter(|e| e.made == now && self.active == Some(e.anchor)).and_then(|e| Some((self.position_of(e.anchor)?, self.position_of(e.head)?)));
        let (anchor, head) = kept.unwrap_or_else(|| {
            let a = self.active.and_then(|a| self.position_of(a)).unwrap_or(0);
            let chosen: Vec<usize> = now.iter().filter_map(|&b| self.position_of(b)).collect();
            sel::start_state(a, &chosen)
        });
        let head = sel::next_head(head, d, n);
        let images: Vec<i64> = sel::span(anchor, head).map(|p| self.rows[self.visible[p]].image).collect();
        let (anchor_image, head_image) = (self.rows[self.visible[anchor]].image, self.rows[self.visible[head]].image);
        let made: HashSet<i64> = images.iter().copied().collect();
        self.selection = if images.len() > 1 { made.clone() } else { HashSet::new() };
        self.active = Some(anchor_image);
        self.nothing_selected = false;
        self.to_active = true;
        self.zoom100 = false;
        self.pan = egui::Vec2::ZERO;
        self.sel_ext = Some(sel::Ext { anchor: anchor_image, head: head_image, made });
    }

    /// Rubber band on free space of the grid: press and drag selects the photos the frame touches (⇧ adds, ⌘ toggles).
    /// Works from the position in rows and columns, not from what was drawn. Returns `true` while a band is dragged
    /// or when one just ended (then the release is no click on free space).
    fn band_update(&mut self, ui: &egui::Ui, view: egui::Rect, geo: crate::library_select::GridGeometry) -> bool {
        use crate::library_select as sel;
        let (pressed, down, pos, dt) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.latest_pos(), i.stable_dt.min(0.1)));
        let mods = crate::click_mods::at_press(ui.ctx());
        let offset = self.grid_y;
        let to_content = |p: egui::Pos2| egui::pos2(p.x - view.left(), p.y - view.top() + offset);
        let (down, pos) = match self.band_test {
            Some((a, b)) => {
                if self.band.is_none() {
                    self.band = Some(sel::Band { origin: a, pointer: egui::pos2(view.left() + b.x, view.top() + b.y - offset), mode: sel::BandMode::Replace, before: HashSet::new(), moved: false });
                }
                (true, self.band.as_ref().map(|x| x.pointer))
            }
            None => (down, pos),
        };
        if self.band.is_none() && pressed {
            if let Some(p) = pos {
                // Free space starts a normal band; Shift also permits starting on a photo.
                if view.contains(p) && p.x < view.right() - 14.0 && ui.rect_contains_pointer(view) && (mods.shift || geo.position_at(to_content(p)).is_none()) {
                    let mode = if mods.shift { sel::BandMode::Add } else { sel::band_mode(false, mods.command) };
                    self.band = Some(sel::Band { origin: to_content(p), pointer: p, mode, before: self.selected_ids(), moved: false });
                }
            }
        }
        let Some(mut b) = self.band.take() else { return false };
        if !down {
            // released: the selection stays as the last frame made it
            return b.moved;
        }
        if let Some(p) = pos {
            b.pointer = p;
        }
        let end = to_content(b.pointer);
        if !b.moved && (end - b.origin).length() > sel::BAND_THRESHOLD {
            b.moved = true;
        }
        if b.moved {
            self.sel_ext = None;
            let r = egui::Rect::from_two_pos(b.origin, end);
            let touched: Vec<i64> = geo.touched(r).into_iter().filter_map(|p| self.visible.get(p)).map(|&i| self.rows[i].image).collect();
            let result = sel::band_result(&b.before, &touched, b.mode);
            if result.len() > 1 {
                if self.active.is_none_or(|a| !result.contains(&a)) {
                    self.active = result.iter().filter_map(|&x| self.position_of(x).map(|p| (p, x))).min().map(|(_, x)| x);
                }
                self.selection = result;
                self.nothing_selected = false;
            } else if let Some(&only) = result.iter().next() {
                self.selection.clear();
                self.active = Some(only);
                self.nothing_selected = false;
            } else {
                self.selection.clear();
                self.nothing_selected = true;
            }
            // frame (clipped to the view), scrolls with the content
            let screen = egui::Rect::from_min_max(
                egui::pos2(view.left() + r.min.x, view.top() + r.min.y - offset),
                egui::pos2(view.left() + r.max.x, view.top() + r.max.y - offset),
            );
            let p = ui.painter_at(view);
            p.rect_filled(screen, 0.0, Color32::from_white_alpha(0x1c));
            p.rect_stroke(screen, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0xb4)), egui::StrokeKind::Inside);
            // near or beyond the top/bottom edge the view follows, calmly
            let speed = sel::autoscroll_speed(b.pointer.y, view.top(), view.bottom());
            if speed != 0.0 {
                self.band_scroll = Some((offset + speed * dt).max(0.0));
                ui.ctx().request_repaint();
            }
        }
        self.band = Some(b);
        true
    }

    fn step(&mut self, d: isize) {
        let current = self.sel_ext.as_ref().filter(|e| self.active == Some(e.anchor) && e.made == self.selected_ids()).map(|e| e.head).or(self.active);
        let pos = current.and_then(|a| self.visible.iter().position(|&i| self.rows[i].image == a));
        self.sel_ext = None;
        let new = match pos {
            Some(p) => (p as isize + d).clamp(0, self.visible.len() as isize - 1) as usize,
            None => 0,
        };
        if let Some(&i) = self.visible.get(new) {
            self.active = Some(self.rows[i].image);
            self.nothing_selected = false;
            self.selection.clear();
            self.to_active = true;
            self.zoom100 = false;
            self.pan = egui::Vec2::ZERO;
        }
    }

    // ───────────────────────────────────────────── Keys

    /// ⌘A in the menu: select all visible photos.
    pub fn select_all(&mut self) {
        self.sel_ext = None;
        self.selection = self.visible.iter().map(|&i| self.rows[i].image).collect();
        self.nothing_selected = false;
        if self.active.is_none_or(|a| !self.selection.contains(&a)) {
            self.active = self.visible.first().map(|&i| self.rows[i].image);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (cmd, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let p = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        if cmd {
            if p(egui::Key::Quote) {
                self.menu(Menu::VirtualCopy, ctx, actions);
            }
            if p(egui::Key::Z) && !shift {
                match self.undo_rating_change() {
                    Some(old) => {
                        actions.push(Action::Status(crate::trf!("Rückgängig: Bewertung ({a} {b})", a = old.len(), b = if old.len() == 1 { crate::tr("Foto") } else { crate::tr("Fotos") })));
                        actions.push(Action::Rated(old));
                    }
                    None => self.undo(actions),
                }
            }
            if p(egui::Key::A) {
                self.select_all();
            }
            if p(egui::Key::D) {
                self.clear_selection();
            }
            return;
        }
        // Esc in the grid, when nothing else needs it (no fullscreen to leave): nothing selected any more
        // Esc first leaves "Similar photos" / "Duplicates" (the chip shows it), then clears the selection
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if p(egui::Key::Escape) && !fullscreen && self.leave_special_source() {
        } else if p(egui::Key::Escape) && !self.nothing_selected && !fullscreen {
            self.clear_selection();
        }
        // arrows and rating keys: in event order in `App::shortcuts_ordered`
        // single image: Return/Space/D (workspace without modules, the app switches the view)
        if p(egui::Key::Enter) || p(egui::Key::Space) || p(egui::Key::D) {
            if self.active.is_some_and(|a| self.index.contains_key(&a)) {
                actions.push(Action::Develop);
                // The key is used up: the single image view handles its keys in the same frame, where Space
                // would otherwise at once switch to 100 % (a crop from the middle instead of the whole photo).
                ctx.input_mut(|i| {
                    for k in [egui::Key::Enter, egui::Key::Space, egui::Key::D] {
                        i.consume_key(egui::Modifiers::NONE, k);
                    }
                });
            }
        }
    }

    // ───────────────────────────────────────────── Parts for the workspace

    /// Per frame before drawing: fetch background work, grid keys, filter the list.
    pub fn prepare(&mut self, ctx: &egui::Context, shortcuts: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(a) = self.check_work() {
            actions.push(a);
        }
        for f in self.writer.error() {
            actions.push(Action::Status(f));
        }
        self.quick_ctx.get_or_insert_with(|| ctx.clone());
        if LIGHTROOM_REQUEST.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.lr_window = true;
        }
        *JOB_NOW.lock().unwrap_or_else(|e| e.into_inner()) = self.running();
        let request = MAINTENANCE_REQUEST.lock().unwrap_or_else(|e| e.into_inner()).take();
        match request {
            // a job is running: the check starts right after it (instead of being dropped)
            Some("check") if self.work.lock().unwrap_or_else(|e| e.into_inner()).running => *MAINTENANCE_REQUEST.lock().unwrap_or_else(|e| e.into_inner()) = request,
            Some("check") => self.start_job(ctx, "check", None),
            Some("rebuild") => self.rebuild_confirm = true,
            _ => {}
        }
        self.quick_feedback(&mut actions);
        for finished in self.db_writer.fetch(ctx) {
            finished(self, &mut actions);
        }
        self.poll_load(&mut actions);
        self.poll_facets();
        {
            let _t = crate::frame_watch::section("library: textures");
            self.results(ctx);
        }
        if !self.preview_cleanup.load(Ordering::SeqCst) {
            self.preview_requests();
        }
        if shortcuts {
            self.shortcuts(ctx, &mut actions);
        }
        self.similar_continue(ctx);
        crate::apple_photos_ui::update(ctx);
        crate::apple_photos_ui::held_dialog(ctx);
        if crate::apple_photos_ui::reload_needed() {
            // photos cropped in Apple Photos: previews that may show the original go (made again when shown)
            if let Some(g) = &self.generator {
                for key in crate::apple_photos_ui::take_outdated_previews() {
                    g.cache().remove_all(key);
                }
            }
            // the list follows when the load is adopted (`adopt_load` filters again)
            self.reload();
        }
        if reload_test_due() {
            // the list follows when the load is adopted (`adopt_load` filters again)
            self.reload();
        }
        // A finished background search replaces the shown result in one go
        if let Some(done) = self.search_job.poll(ctx, &self.rows, &self.folder_paths) {
            if self.filter.text.trim() == done.text {
                self.take_search(done);
            }
        }
        self.apply_filter();
        self.background();
        actions
    }

    /// Left panel: sources (catalog, folders, collections, keywords).
    pub fn sources(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let _t = crate::frame_watch::section("library: sources");
        self.left_panel(ui, &ctx);
    }

    /// "Info" tab: rating, metadata, AI, keywords, collections.
    pub fn info(&mut self, ui: &mut egui::Ui, in_grid: bool) -> Vec<Action> {
        let mut actions = Vec::new();
        let _t = crate::frame_watch::section("library: info");
        if in_grid && crate::people_ui::is_open() {
            // people view: navigator with the chosen face, below it the details of its photo
            lr::section(ui, "Navigator", true, |ui| crate::people_ui::navigator(ui));
            let shown = crate::people_ui::focus_image().filter(|i| self.index.contains_key(i));
            // the library's own choice stays as it is (the grid finds it again when the people view closes)
            let kept = (self.active, std::mem::take(&mut self.selection), self.jump_request);
            self.active = shown;
            self.right(ui, &mut actions);
            self.active = kept.0;
            self.selection = kept.1;
            self.jump_request = kept.2;
            return actions;
        }
        self.right(ui, &mut actions);
        actions
    }

    /// The photo of the face chosen in the people view – while it is open the current photo of the app (ratings, tool
    /// rail, Develop). `None`: the view is closed or no single face is chosen.
    pub fn people_focus(&self) -> Option<i64> {
        crate::people_ui::is_open().then(crate::people_ui::focus_image).flatten().filter(|i| self.index.contains_key(i))
    }

    /// From the people view to Develop: the chosen face's photo becomes the active one, shown in its folder (the
    /// filmstrip there); the people view and the grid as they were come back with [`Self::resume_people`].
    pub fn people_to_develop(&mut self) {
        if !crate::people_ui::is_open() {
            return;
        }
        let focus = self.people_focus();
        let back = (self.source.clone(), self.active, self.selection.clone(), self.nothing_selected);
        // leave the view first (it notes whether the large photo was shown), then choose the folder
        crate::people_ui::suspend();
        if let Some(image) = focus {
            self.show_folder(image);
            // column filters or a search that hide the photo: off, so that Develop shows exactly this photo
            if self.position_of(image).is_none() {
                self.reset_all_filters();
                self.filter.text.clear();
                self.apply_filter();
                self.active = Some(image);
                self.to_active = true;
            }
        }
        crate::people_ui::suspend();
        self.people_return = Some(back);
    }

    /// Tests: stars of a photo as the library holds them.
    #[cfg(test)]
    pub fn stars_of(&self, image: i64) -> Option<u8> {
        self.index.get(&image).map(|&i| self.rows[i].stars)
    }

    /// Left for Develop from the people view and not yet back?
    pub fn people_pending(&self) -> bool {
        crate::people_ui::resume_pending()
    }

    /// Back to the grid from Develop: the people view comes back as it was if Develop was opened from it – the same
    /// person, face, filters and scroll position; below it the grid's source and choice from before.
    pub fn resume_people(&mut self) {
        if !crate::people_ui::resume_pending() {
            self.people_return = None;
            return;
        }
        if let Some((source, active, selection, nothing)) = self.people_return.take() {
            // choosing the source closes the people view and forgets the way back: mark it again right after
            self.set_source(source);
            self.apply_filter();
            self.active = active;
            self.selection = selection;
            self.nothing_selected = nothing;
            self.to_active = true;
            crate::people_ui::suspend();
        }
        if crate::people_ui::resume() {
            self.pairs.open = false;
            self.loupe = false;
        }
    }

    /// Centre in the grid: filter bar and grid.
    pub fn grid_center(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        self.centre_width = ui.available_width();
        let clip = ui.clip_rect().intersect(ui.max_rect().expand2(egui::vec2(8.0, 6.0)));
        ui.set_clip_rect(clip);
        self.loupe = false;
        // RAW+JPEG twice: own view instead of the photo grid
        if self.pairs.open {
            self.pairs_center(ui);
            return actions;
        }
        // people: own view instead of the photo grid
        if crate::people_ui::is_open() {
            let people = crate::people_ui::view(ui, self.db.as_ref());
            // names changed: the photos are re-read once the view is left (not after every answer)
            self.people_stale |= people.changed;
            if people.counts_changed {
                self.review_checked = None;
            }
            if self.people_stale && (people.search.is_some() || people.open.is_some()) {
                self.people_stale = false;
                self.reload();
            }
            if let Some(t) = people.search {
                self.filter.text = t;
                self.search_in_source = false;
                self.refilter = true;
                self.review_checked = None;
            }
            // Develop with the chosen face's photo: `App::to_single_image` → `people_to_develop`
            if people.open.is_some() {
                actions.push(Action::Develop);
            }
            return actions;
        }
        if std::mem::take(&mut self.people_stale) {
            self.reload();
        }
        if self.source == Source::NotInCatalog {
            self.not_in_catalog_center(ui);
            return actions;
        }
        // Duplicates as groups side by side (library_dupes.rs); "Als Raster" shows the grid
        if self.source == Source::Duplicates && !self.dupes.grid {
            self.dupes_center(ui);
            return actions;
        }
        {
            let _t = crate::frame_watch::section("library: filter bar");
            self.filter_bar(ui);
        }
        self.not_in_catalog_banner(ui);
        self.dupes_banner(ui);
        {
            let _t = crate::frame_watch::section("library: grid");
            self.raster(ui, &mut actions);
        }
        actions
    }

    /// After drawing: windows and maintenance.
    pub fn after(&mut self, ctx: &egui::Context) -> Vec<Action> {
        let mut actions = Vec::new();
        self.lr_dialog(ctx);
        self.rebuild_dialog(ctx);
        self.missing_dialog(ctx);
        self.quick_dialogs(ctx);
        self.smart_dialog(ctx);
        self.kw_set_dialog(ctx);
        self.face_model_dialog(ctx);
        self.dialogs(ctx, &mut actions);
        self.manage_dialogs(ctx, &mut actions);
        self.pairs_dialogs(ctx, &mut actions);
        self.dupes_dialogs(ctx, &mut actions);
        self.paint_drag(ctx);
        let cache = self.generator.as_ref().map(|g| g.cache().clone());
        let job = self.running();
        let (roots, folders) = (&self.roots, &self.folder);
        let folder_of = |p: &Path| -> Option<i64> {
            let s = format!("{}/", library::nfc(p.to_string_lossy().trim_end_matches('/')));
            folders.iter().find(|o| roots.iter().any(|r| r.id == o.root && format!("{}{}", r.path, o.path) == s)).map(|o| o.id)
        };
        let moved = self.move_folder.update(ctx, cache.clone(), job.as_ref(), &folder_of);
        let relinked = self.relink.update(ctx, cache, job.as_ref(), &folder_of);
        if self.move_folder.take_reload() && moved.is_none() {
            // a move job paused or stopped: the folders done so far are at the target
            self.reload();
        }
        if let Some(d) = moved {
            self.reload();
            self.dialog = Some(Dialog::Outcome { title: d.title, verdict: d.verdict, summary: d.summary, rows: d.rows, details: d.details });
        }
        if self.dialog.is_none() {
            if let Some(m) = crate::move_folder_ui::take_resumed().filter(|m| !m.is_empty()) {
                self.reload();
                self.dialog = Some(Dialog::Outcome {
                    title: crate::tr("Unterbrochene Vorgänge").to_string(),
                    verdict: crate::dialog::Verdict::Yellow,
                    summary: crate::tr("Beim letzten Beenden unterbrochenes Verschieben oder Umbenennen wurde abgeschlossen bzw. zurückgenommen.").to_string(),
                    rows: m,
                    details: Vec::new(),
                });
            }
        }
        if let Some(r) = relinked {
            let d = r.done;
            self.reload();
            // sync the root folder: photos still missing, new ones in the folder
            if let Some(p) = r.sync_root.filter(|_| crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_none()) {
                if self.work.lock().unwrap_or_else(|e| e.into_inner()).running {
                    self.changed_since.get_or_insert(std::time::Instant::now());
                    self.changed_roots.insert(Some(p));
                } else {
                    self.start_job(ctx, "sync", Some(p));
                }
            }
            self.dialog = Some(Dialog::Outcome { title: d.title, verdict: d.verdict, summary: d.summary, rows: d.rows, details: d.details });
        }
        if let Some(target) = crate::file_dialog::fetch_one("bib-verschieben") {
            let mut files: Vec<i64> = self.target().iter().filter_map(|b| self.index.get(b).map(|&i| self.rows[i].file)).collect();
            files.sort();
            files.dedup();
            self.operation(move |db| library::relocate::plan_files(db, &files, &target, None), &mut actions);
        }
        self.import_dialog(ctx, &mut actions);
        self.not_in_catalog_done(ctx, &mut actions);
        self.maintenance(ctx);
        self.semantic_maintenance(ctx);
        self.remember_active();
        actions
    }

    /// "Show in library folder": source = the image's folder, expand the tree and scroll
    /// there, activate the image. Images outside the library ("View folder") stay there.
    pub fn show_folder(&mut self, image: i64) {
        let Some(&i) = self.index.get(&image) else { return };
        let folder = self.rows[i].folder;
        if let Some(o) = self.folder.iter().find(|o| o.id == folder).cloned() {
            let mut q = String::new();
            for t in o.path.split('/').filter(|t| !t.is_empty()) {
                self.open_folders.insert((o.root, q.clone()));
                q.push_str(t);
                q.push('/');
            }
            self.set_source(Source::Folder(o.root, o.path.clone()));
            self.scroll_to_folder = Some((o.root, o.path));
        }
        self.apply_filter();
        self.active = Some(image);
        self.nothing_selected = false;
        self.selection.clear();
        self.to_active = true;
    }

    /// ⌘D / Edit → Select None: in the grid nothing is selected any more; the last active photo stays for actions
    /// without a selection and for the single views.
    /// Context menu of a photo outside the grid (filmstrip, single image): the same entries as in the grid.
    pub fn photo_menu_for(&self, ui: &mut egui::Ui, image: i64, present: bool) -> Option<Menu> {
        photo_menu(ui, self.quick.contains(&image), present, self.can_paste, self.removable_collection().as_deref())
    }

    /// Carry out a photo command from outside the grid (on the active photo / the selection set before).
    pub fn menu_external(&mut self, m: Menu, ctx: &egui::Context) -> Vec<Action> {
        let mut a = Vec::new();
        self.menu(m, ctx, &mut a);
        a
    }

    /// Selection from outside (filmstrip): `active` and the photos an action applies to.
    pub fn set_selection(&mut self, active: i64, images: &[i64]) {
        self.sel_ext = None;
        self.active = Some(active);
        self.nothing_selected = false;
        self.selection = if images.len() > 1 { images.iter().copied().collect() } else { HashSet::new() };
    }

    pub fn clear_selection(&mut self) {
        self.sel_ext = None;
        self.selection.clear();
        self.nothing_selected = true;
    }

    /// Nothing selected in the grid?
    pub fn nothing_selected(&self) -> bool {
        self.nothing_selected
    }

    /// The single views always have an active photo: leaving the grid ends "nothing selected".
    pub fn keep_active(&mut self) {
        self.nothing_selected = false;
    }

    /// Images an action refers to: the selection, otherwise the active image.
    pub fn target_images(&self) -> Vec<i64> {
        self.target()
    }

    /// Selected images (image IDs) in the grid; empty = only the active one.
    pub fn selection(&self) -> Vec<i64> {
        self.selection.iter().copied().collect()
    }

    // ───────────────────────────────────────────── UI

    /// Row of the left panel: indentation, optional disclosure triangle, name, count on the right.
    /// Returns: (name clicked, triangle clicked).
    fn list_row(ui: &mut egui::Ui, text: &str, count: Option<usize>, active: bool, depth: usize, open: Option<bool>, dimmed: bool) -> (bool, bool, egui::Response) {
        Self::list_row_text(ui, text, count.map(thousands), active, depth, open, dimmed)
    }

    /// Like [`Self::list_row`] with freely chosen text on the right (e.g. "+179").
    fn list_row_text(ui: &mut egui::Ui, text: &str, right: Option<String>, active: bool, depth: usize, open: Option<bool>, dimmed: bool) -> (bool, bool, egui::Response) {
        Self::list_row_icon(ui, None, text, right, active, depth, open, dimmed)
    }

    /// Special row of the BIBLIOTHEK section with a small symbol in front (folders, collections and
    /// keywords have none): symbol in a fixed column, dimmed, in the marked row in the colour of the text.
    pub(super) fn special_row(ui: &mut egui::Ui, icon: lr::Icon, text: &str, count: Option<usize>, active: bool, dimmed: bool) -> (bool, bool, egui::Response) {
        Self::list_row_icon(ui, Some(icon), text, count.map(thousands), active, 0, None, dimmed)
    }

    #[allow(clippy::too_many_arguments)]
    fn list_row_icon(ui: &mut egui::Ui, icon: Option<lr::Icon>, text: &str, right: Option<String>, active: bool, depth: usize, open: Option<bool>, dimmed: bool) -> (bool, bool, egui::Response) {
        Self::list_row_sense(ui, egui::Sense::click(), icon, text, right, active, depth, open, dimmed)
    }

    /// [`Self::list_row_icon`] with its own sense (collections: click and drag).
    #[allow(clippy::too_many_arguments)]
    fn list_row_sense(ui: &mut egui::Ui, sense: egui::Sense, icon: Option<lr::Icon>, text: &str, right: Option<String>, active: bool, depth: usize, open: Option<bool>, dimmed: bool) -> (bool, bool, egui::Response) {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), sense);
        if !ui.is_rect_visible(rect) {
            return (false, false, resp);
        }
        let x0 = rect.left() + 4.0 + depth as f32 * 12.0;
        let triangle = egui::Rect::from_center_size(egui::pos2(x0 + 5.0, rect.center().y), egui::vec2(14.0, 18.0));
        let over_triangle = open.is_some() && resp.hover_pos().is_some_and(|p| triangle.contains(p));
        let painter = ui.painter();
        if active {
            painter.rect_filled(rect, 0.0, Color32::from_gray(0x4a));
        } else if resp.hovered() {
            painter.rect_filled(rect, 0.0, Color32::from_gray(0x2c));
        }
        if let Some(o) = open {
            let c = triangle.center();
            let pts = if o { vec![c + egui::vec2(-3.5, -2.0), c + egui::vec2(3.5, -2.0), c + egui::vec2(0.0, 2.5)] } else { vec![c + egui::vec2(-2.0, -3.5), c + egui::vec2(2.5, 0.0), c + egui::vec2(-2.0, 3.5)] };
            painter.add(egui::Shape::convex_polygon(pts, if over_triangle { Color32::WHITE } else { Color32::from_gray(0x8a) }, egui::Stroke::NONE));
        }
        let color = if dimmed { Color32::from_gray(0x8c) } else if active || resp.hovered() { Color32::WHITE } else { lr::TEXT };
        let font = egui::TextStyle::Body.resolve(ui.style());
        let right_free = 52.0;
        // symbol (special rows only) in the marker column of the rows – where the other sections show the triangle of
        // a folder –, so its left edge keeps the same indent as their content; the names follow flush
        let tx = if let Some(icon) = icon {
            let r = egui::Rect::from_min_size(egui::pos2(x0, rect.center().y - 7.0), egui::vec2(14.0, 14.0));
            lr::paint_icon(ui.painter(), r, icon, if active { color } else { lr::TEXT_DIM });
            x0 + 20.0
        } else {
            x0 + 14.0
        };
        let max_w = rect.right() - right_free - tx;
        let galley = ui.painter().layout(text.to_string(), font.clone(), color, f32::INFINITY);
        let painter = ui.painter_at(egui::Rect::from_min_max(egui::pos2(tx, rect.top()), egui::pos2(tx + max_w.max(10.0), rect.bottom())));
        painter.galley(egui::pos2(tx, rect.center().y - galley.size().y / 2.0), galley, color);
        if let Some(t) = right {
            ui.painter().text(rect.right_center() - egui::vec2(6.0, 0.0), egui::Align2::RIGHT_CENTER, t, egui::TextStyle::Small.resolve(ui.style()), lr::TEXT_DIM);
        }
        let click = resp.clicked();
        (click && !over_triangle, click && over_triangle, resp)
    }

    fn left_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // fixed at the bottom: Import (also "Add folder": there "Hinzufügen"). Importing from Lightroom is in the menu
        // (File → Import from Lightroom …), on the tiles of the empty library and in the settings (Library).
        egui::Panel::bottom("bib-links-unten").resizable(false).frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 8))).show(ui, |ui| {
            // Import (the one gold button) and Export side by side, equally wide
            let w = ((ui.available_width() - 6.0) / 2.0).floor();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                // a view in place of the grid (People, RAW+JPEG twice) has its own gold button
                // viewing a backup: importing is refused there, so the button is disabled (not gold), with the reason
                let can_import = library::backup_view::active().is_none();
                let r = ui.add_enabled_ui(can_import, |ui| lr::arrow_button_gold(ui, crate::tr("Importieren …"), w, true, can_import && !self.grid_replaced())).inner;
                if !can_import {
                    // a disabled widget reports no hover: the reason on a hover area over the same place
                    let reason = ui.interact(r.rect, r.id.with("reason"), egui::Sense::hover());
                    lr::hover_tip(ui, &reason, crate::tr("Gesperrt, solange du eine Sicherung ansiehst – erst zurück zur Bibliothek"));
                }
                if can_import && r.clicked() {
                    self.open_import(ctx);
                }
                // nothing to export in an empty library: disabled, with the reason
                let can_export = !self.rows.is_empty();
                let r = ui.add_enabled_ui(can_export, |ui| lr::arrow_button(ui, crate::tr("Exportieren …"), w, false)).inner;
                if !can_export {
                    // a disabled widget reports no hover: the reason on a hover area over the same place
                    let reason = ui.interact(r.rect, r.id.with("grund"), egui::Sense::hover());
                    lr::hover_tip(ui, &reason, crate::tr("Noch keine Fotos zum Exportieren"));
                }
                if can_export && r.clicked() {
                    self.export_requested = true;
                }
            });
        });
        let w = ui.available_width();
        let [n_recent, n_missing, n_offline] = self.catalog_counts();
        egui::ScrollArea::vertical().id_salt("bib-links-scroll").auto_shrink([false; 2]).max_width(w).show(ui, |ui| crate::sticky::scope(ui, |ui| {
            // inside the scroll area: never wider than its viewport (it keeps a band free for the bar)
            ui.set_max_width(ui.available_width().min(w));
            self.collection_drag.content_top = ui.cursor().min.y;
            if let Some(f) = &self.error {
                ui.label(egui::RichText::new(crate::menu::i18n::tr_msg(f)).color(Color32::from_rgb(230, 120, 100)));
            }
            // an empty library shows only what already means something (no rows of zeros); the fixed places return
            // with the first photo
            let empty = self.library_is_empty();
            let n_all = self.library_photo_count();
            lr::section(ui, "Bibliothek", true, |ui| {
                for (q, icon, name, n) in [
                    (Source::All, lr::Icon::SquaresFour, crate::tr("Alle Fotos"), n_all),
                    (Source::Quick, lr::Icon::PlusCircle, crate::tr("Schnellsammlung +"), self.quick.len()),
                    (Source::Recent, lr::Icon::TrayArrowDown, crate::tr("Zuletzt importiert"), n_recent),
                    (Source::Missing, lr::Icon::ImageBroken, crate::tr("Fehlende Fotos"), n_missing),
                    (Source::Offline, lr::Icon::LinkBreak, crate::tr("Nicht verbunden"), n_offline),
                ] {
                    if empty && matches!(q, Source::Missing | Source::Offline) {
                        continue;
                    }
                    let active = self.source_marked(&q);
                    let is_quick = q == Source::Quick;
                    let (click, _, resp) = Self::special_row(ui, icon, name, Some(n).filter(|_| !empty), active, false);
                    if is_quick {
                        resp.context_menu(|ui| self.quick_context(ui));
                        if let Some(images) = Self::drop_target(ui, &resp, true) {
                            let t = self.drop_on_quick(images);
                            self.manage_message = Some(t);
                        }
                    }
                    if q == Source::Recent {
                        resp.context_menu(|ui| {
                            if lr::menu_item(ui, crate::tr("Importe verwalten …"), "", true) {
                                self.ask_imports();
                            }
                        });
                    }
                    if click {
                        self.set_source(q);
                    }
                }
                if empty {
                    return;
                }
                let n_dup = self.dup.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|g| g.iter().map(|x| x.files.len()).sum::<usize>());
                if Self::special_row(ui, lr::Icon::Copy, crate::tr("Duplikate"), n_dup, self.source_marked(&Source::Duplicates), false).0 {
                    self.show_duplicates();
                }
                self.pairs_list_row(ui);
                let review = if self.review_n + self.suggestion_n > 0 { Some(self.review_n + self.suggestion_n) } else { None };
                if Self::special_row(ui, lr::Icon::User, crate::tr("Personen"), review, crate::people_ui::is_open(), false).0 {
                    // to where something needs doing
                    let tab = if self.review_n > 0 {
                        crate::people_ui::ToolTab::Check
                    } else if self.suggestion_n > 0 {
                        crate::people_ui::ToolTab::Suggestions
                    } else {
                        crate::people_ui::ToolTab::People
                    };
                    if !crate::people_ui::is_open() {
                        self.open_people(tab);
                    }
                }
                // the number counts photos (like every other row); the folders are in the tooltip
                let counts = self.not_in_catalog_count();
                let row = Self::special_row(ui, lr::Icon::FolderDashed, crate::tr("Nicht in der Bibliothek"), counts.map(|(_, photos)| photos), self.source_marked(&Source::NotInCatalog), false);
                if let Some((folders, photos)) = counts {
                    lr::hover_tip(ui, &row.2, &crate::trf!("Fotos, die noch nicht in der Bibliothek sind: {a} · Ordner: {b}", a = crate::dialog::number(photos), b = crate::dialog::number(folders)));
                }
                if row.0 {
                    self.set_source(Source::NotInCatalog);
                    self.not_in_catalog.trigger_research();
                }
            });
            if !empty {
                lr::section(ui, "Nach Datum", false, |ui| self.date_list(ui));
            }
            // "+" on the right of the ORDNER header: add a folder to the library (library_views.rs)
            let (folders_top, folders_right) = (ui.cursor().min.y, ui.max_rect().right());
            lr::section(ui, "Ordner", true, |ui| {
                let root_rows = self.root_rows.clone();
                for (s, k) in root_rows {
                    let reachable = crate::path_check::folder_exists(std::path::Path::new(&s.path));
                    self.folder_node(ui, k, 0, !reachable);
                }
                if self.root_rows.is_empty() && !self.first_loading() {
                    ui.label(egui::RichText::new(crate::tr("Noch keine Ordner")).color(lr::TEXT_DIM));
                }
            });
            self.add_folder_button(ui, folders_top, folders_right);
            // "+" on the right of the SAMMLUNGEN header: create collection, smart collection, set
            let header_top = ui.cursor().min.y;
            let header_right = ui.max_rect().right();
            // own order per level (collection_order_ui.rs), dragging (collection_drag.rs)
            lr::section(ui, "Sammlungen", true, |ui| self.collection_list(ui));
            self.collections_header_menu(ui, egui::Rect::from_min_max(egui::pos2(ui.max_rect().left(), header_top), egui::pos2(header_right, header_top + 28.0)));
            self.remember_open_collections();
            {
                let resp = Self::header_plus(ui, header_top, header_right, "bib-sammlungen-plus", "Sammlung erstellen");
                egui::Popup::menu(&resp).show(|ui| {
                    if lr::menu_item(ui, crate::tr("Sammlung erstellen …"), &crate::shortcuts::display("new_collection"), true) {
                        self.ask_new_collection(library::manage::COLLECTION, None);
                    }
                    if lr::menu_item(ui, crate::tr("Smart-Sammlung erstellen …"), "", true) {
                        self.ask_new_collection(library::manage::SMART, None);
                    }
                    if lr::menu_item(ui, crate::tr("Sammlungssatz erstellen …"), "", true) {
                        self.ask_new_collection(library::manage::SET, None);
                    }
                });
            }
        }));
    }

    fn folder_node(&mut self, ui: &mut egui::Ui, k: usize, depth: usize, offline: bool) {
        let (root, path, name, n, has_children, disk_only, disk, foreign) = {
            let node = &self.nodes[k];
            (node.root, node.path.clone(), node.name.clone(), node.count, !node.children.is_empty(), node.disk_only, node.disk, node.foreign)
        };
        let key = (root, path.clone());
        let open = self.open_folders.contains(&key);
        let active = if disk_only {
            match &self.source {
                Source::Browse(p) if !self.grid_replaced() => self.root_rows.iter().find(|(s, _)| s.id == root).is_some_and(|(s, _)| p == &PathBuf::from(format!("{}{}", s.path, path.trim_end_matches('/')))),
                _ => false,
            }
        } else {
            self.source_marked(&Source::Folder(root, path.clone()))
        };
        let name_for_menu = name.clone();
        let display = name;
        // folder only on disk: dimmed, "+count" on the right instead of the catalog count
        let (number, dimmed) = if disk_only { (Some(format!("+{}", thousands(disk.unwrap_or(foreign)))), true) } else { (Some(thousands(n)), offline) };
        let (click, triangle, resp) = Self::list_row_text(ui, &display, number, active, depth, has_children.then_some(open), dimmed);
        // root folder on an unreachable location: small "not connected" mark (crossed-out circle) before the count
        if offline && depth == 0 && ui.is_rect_visible(resp.rect) {
            let c = egui::pos2(resp.rect.right() - 48.0, resp.rect.center().y);
            let stroke = egui::Stroke::new(1.2, Color32::from_gray(0x8c));
            ui.painter().circle_stroke(c, 4.5, stroke);
            ui.painter().line_segment([c + egui::vec2(-3.2, 3.2), c + egui::vec2(3.2, -3.2)], stroke);
        }
        if self.scroll_to_folder.as_ref() == Some(&key) {
            resp.scroll_to_me(Some(egui::Align::Center));
            self.scroll_to_folder = None;
        }
        if disk_only && disk.is_some() {
            resp.context_menu(|ui| self.not_in_catalog_menu(ui, root, &path));
        } else if !disk_only {
            let folder_id = self.folder.iter().find(|o| o.root == root && o.path == path).map(|o| o.id);
            let apple = self.roots.iter().any(|x| x.id == root && x.origin == library::apple_photos::ORIGIN);
            resp.context_menu(|ui| {
                // groups: show – file commands – remove (destructive last)
                if lr::menu_item(ui, crate::tr("Im Finder zeigen"), "", true) {
                    if let Some(s) = self.roots.iter().find(|x| x.id == root) {
                        crate::system::show(format!("{}{}", s.path, path));
                    }
                }
                ui.separator();
                if depth > 0 && lr::menu_item(ui, crate::tr("Ordner umbenennen …"), "", folder_id.is_some()) {
                    if let Some(o) = folder_id {
                        let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
                        self.dialog = Some(Dialog::RenameFolder { folder: o, name });
                    }
                }
                // between locations (to the NAS and back): copy with check, previews prepared beforehand
                let can_move = folder_id.is_some() && !offline && !apple && !self.move_folder.busy();
                if lr::menu_item(ui, crate::tr("Verschieben nach …"), "", can_move) {
                    if let (Some(o), Some(s)) = (folder_id, self.roots.iter().find(|x| x.id == root)) {
                        let source = PathBuf::from(format!("{}{}", s.path, path).trim_end_matches('/'));
                        self.move_folder.request(ui.ctx(), o, source);
                    }
                }
                // moved or renamed in the Finder (the folder is gone, its volume is connected): link it again
                if let (Some(o), Some(s), false) = (folder_id, self.roots.iter().find(|x| x.id == root), apple) {
                    let here = PathBuf::from(format!("{}{}", s.path, path).trim_end_matches('/'));
                    let root_there = depth == 0 || !offline;
                    let volume = library::relink::volume_root(&here).is_none_or(|v| crate::path_check::folder_exists(&v));
                    if root_there && volume && !crate::path_check::da(&here) {
                        let free = !self.relink.busy() && !self.move_folder.busy() && !library::move_job::active();
                        if lr::menu_item(ui, crate::tr("Fehlenden Ordner suchen …"), "", free) {
                            self.relink.request(ui.ctx(), o, here);
                        }
                    }
                }
                ui.separator();
                // database only, the files stay; never for the Photos library (its sync brings everything back)
                let free = !apple && !self.move_folder.busy() && !library::move_job::active();
                if lr::menu_item(ui, crate::tr("Aus der Bibliothek entfernen …"), "", free) {
                    self.ask_remove_folder(root, path.clone(), name_for_menu.clone());
                }
            });
        }
        if triangle {
            if open {
                self.open_folders.remove(&key);
            } else {
                self.open_folders.insert(key.clone());
            }
        }
        if click {
            if disk_only {
                if disk.is_some() {
                    self.view_not_in_catalog(root, &path);
                }
            } else {
                self.set_source(Source::Folder(root, path));
            }
            if has_children && !open {
                self.open_folders.insert(key.clone());
            }
        }
        if self.open_folders.contains(&key) {
            let children = self.nodes[k].children.clone();
            for c in children {
                self.folder_node(ui, c, depth + 1, offline);
            }
        }
    }

    /// "View folder" without photos: says why instead of an empty grid.
    fn browse_empty(&mut self, ui: &mut egui::Ui) {
        let Some(reason) = self.viewed_listing.as_ref().and_then(|l| l.empty_reason()) else { return };
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(crate::tr("Keine Fotos in diesem Ordner")).size(16.0).color(lr::TEXT));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(reason).color(lr::TEXT_DIM));
        });
    }

    /// "Ordner nur ansehen": a quiet bar above the grid of a viewed folder – originals are never changed, edits go
    /// to sidecars (XMP) next to the photo; the way into the library and back; and the switch whether ratings may
    /// go next to the photos as sidecars as well (default off).
    fn view_folder_bar(&mut self, ui: &mut egui::Ui) {
        let Source::Browse(dir) = self.source.clone() else { return };
        let ctx = ui.ctx().clone();
        let (mut add, mut close) = (false, false);
        egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_height(24.0);
                let lead = ui.label(egui::RichText::new(crate::tr("Nur ansehen – Originale unverändert")).color(lr::TEXT));
                lr::hover_tip(ui, &lead, crate::tr("Änderungen (Entwicklung) werden als Begleitdatei (XMP) neben das Foto gespeichert, sobald du etwas änderst. Zur Bibliothek hinzufügen speichert sie stattdessen in der Bibliothek."));
                ui.add_space(14.0);
                let mut on = view_folder_sidecars();
                if crate::dialog::toggle(ui, &mut on) {
                    library::settings::set(VIEW_SIDECARS_KEY, serde_json::json!(on));
                }
                let sw = ui.label(egui::RichText::new(crate::tr("Bewertungen als Begleitdatei (XMP) neben die Fotos schreiben")).small().color(lr::TEXT_DIM));
                lr::hover_tip(ui, &sw, crate::tr("Gilt nur für Bewertungen. Entwicklungen werden immer als Begleitdatei neben das Foto gespeichert."));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tr("Schließen")).clicked() {
                        close = true;
                    }
                    if ui.button(crate::tr("Zur Bibliothek hinzufügen …")).clicked() {
                        add = true;
                    }
                });
            });
        });
        if add {
            self.import_folder(&ctx, dir);
        } else if close {
            self.close_view_folder();
        }
    }

    /// Leave a viewed folder: its photos go out of the list, back to the library.
    fn close_view_folder(&mut self) {
        let all = Arc::make_mut(&mut self.rows);
        all.retain(|z| z.folder != VIEW_FOLDER);
        self.index = self.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        self.viewed = None;
        self.viewed_listing = None;
        self.selection.clear();
        self.active = None;
        self.set_source(Source::All);
        self.refilter = true;
    }

    /// Ratings in a viewed folder are refused (the switch is off): says so instead of silently doing nothing.
    pub fn take_rating_refused(&mut self) -> bool {
        std::mem::take(&mut self.rating_refused)
    }

    /// No photo in the library (the photos of a folder only being viewed do not count).
    fn library_is_empty(&self) -> bool {
        !self.rows.iter().any(|z| z.folder != VIEW_FOLDER)
    }

    /// Photos in the library ("Alle Fotos"): without those of a folder only being viewed.
    fn library_photo_count(&self) -> usize {
        self.rows.iter().filter(|z| z.folder != VIEW_FOLDER).count()
    }

    /// Empty library (first start): three equally large tiles for the three ways in – import photos, import from
    /// Lightroom, only view a folder – and below them (Mac) a quiet line for Apple Photos. No gold here: the gold
    /// button is "Importieren …" on the left.
    fn library_empty(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(48.0);
        ui.vertical_centered(|ui| {
            // "Willkommen bei" and the name as the word mark (brand font, aperture in the "o"), never in the UI font
            ui.label(egui::RichText::new(crate::tr("Willkommen bei")).size(14.0).color(lr::TEXT_DIM));
            ui.add_space(2.0);
            match crate::brand::wordmark(ui.ctx()) {
                Some(t) => {
                    let w = 168.0;
                    ui.add(egui::Image::new((t.id(), egui::vec2(w, w * t.size()[1] as f32 / t.size()[0] as f32))));
                }
                None => {
                    ui.label(egui::RichText::new("graineo").font(lr::brand(30.0)).color(Color32::from_rgb(0xf2, 0xef, 0xe6)));
                }
            }
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::tr("Wie möchtest du anfangen?")).color(lr::TEXT_DIM));
        });
        ui.add_space(18.0);
        let tiles = [
            (lr::Icon::TrayArrowDown, crate::tr("Fotos importieren"), crate::tr("Von Karte, Kamera oder aus einem Ordner."), crate::tr("Du wählst, ob die Fotos bleiben, kopiert oder verschoben werden."), "import"),
            (lr::Icon::Images, crate::tr("Aus Lightroom importieren"), crate::tr("Fotos, Sterne, Sammlungen und Bearbeitungen."), crate::tr("Der Katalog wird nur gelesen."), ""),
            (lr::Icon::FolderOpen, crate::tr("Ordner nur ansehen"), crate::tr("Fotos durchblättern, ohne Import."), crate::tr("Verändert nichts."), "open_folder"),
        ];
        let gap = 14.0;
        let w = ((ui.available_width() - 2.0 * gap - 40.0) / 3.0).clamp(170.0, 250.0);
        let h = 150.0;
        let mut clicked = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.add_space(((ui.available_width() - 3.0 * w - 2.0 * gap) / 2.0).max(0.0));
            for (i, (icon, title, line, hint, key)) in tiles.iter().enumerate() {
                let (rect, r) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
                let p = ui.painter();
                // test aid (screenshots): GRAINEO_TILE_HOVER=<n> shows tile n as hovered
                let forced = crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_TILE_HOVER").ok().and_then(|v| v.parse::<usize>().ok()) == Some(i);
                let hot = r.hovered() || forced;
                // titles stay in the light text colour; the symbol in the app's blue marks the tile as clickable, on
                // hover a blue frame and a slightly lighter ground (the Apple Photos line stays the only blue text)
                p.rect_filled(rect, 0.0, if hot { Color32::from_gray(0x2a) } else { lr::GROUP_FILL });
                p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, if hot { lr::BLUE } else { lr::FIELD_STROKE }), egui::StrokeKind::Inside);
                let inner = rect.shrink2(egui::vec2(14.0, 14.0));
                lr::paint_icon(p, egui::Rect::from_min_size(inner.min, egui::vec2(22.0, 22.0)), *icon, lr::BLUE_TEXT);
                let mut y = inner.min.y + 32.0;
                for (text, font, color) in [(*title, lr::semibold(13.0), lr::TEXT), (*line, egui::FontId::proportional(lr::UI_SIZE), lr::TEXT_DIM), (*hint, egui::FontId::proportional(lr::UI_SIZE - 1.0), lr::TEXT_DIM)] {
                    let g = p.layout(text.to_string(), font, color, inner.width());
                    let gh = g.size().y;
                    p.galley(egui::pos2(inner.min.x, y), g, color);
                    y += gh + 5.0;
                }
                if !key.is_empty() {
                    lr::tip_key(ui, &r, title, key);
                }
                if r.clicked() {
                    clicked = Some(i);
                }
            }
        });
        match clicked {
            Some(0) => self.open_import(&ctx),
            Some(1) => self.lr_window = true,
            Some(2) => crate::file_dialog::folder(&ctx, "ordner-oeffnen", |d| d),
            _ => {}
        }
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(crate::tr("Ordner und Fotos kannst du auch hierher ziehen.")).small().color(lr::TEXT_DIM));
            // Apple Photos: a quiet line, no tile; asking for access only after the click (in the settings)
            #[cfg(target_os = "macos")]
            {
                ui.add_space(4.0);
                // the app's blue (switch, progress), as a clickable line with an arrow – not a button, not gold
                let r = lr::link_text(ui, egui::RichText::new(format!("{}  →", crate::tr("Du nutzt Apple Fotos? Bibliothek verbinden …"))), lr::BLUE_TEXT);
                lr::hover_tip(ui, &r, "Öffnet Einstellungen › Bibliothek › Apple Fotos");
                if r.clicked() {
                    crate::prefs::open_section("Bibliothek");
                }
            }
        });
    }

    /// Dialog "Save keyword set": enter a name; the keywords are shown as marks below.
    fn kw_set_dialog(&mut self, ctx: &egui::Context) {
        use crate::dialog;
        let Some((name, words)) = &mut self.kw_set_dialog else { return };
        let title = if words.len() == 1 { crate::tr("Stichwortsatz mit 1 Stichwort sichern").to_string() } else { crate::trf!("Stichwortsatz mit {a} Stichwörtern sichern", a = dialog::number(words.len())) };
        let ok = !name.trim().is_empty();
        let list = words.iter().map(|w| w.rsplit('|').next().unwrap_or(w).to_string()).collect::<Vec<_>>().join(" · ");
        let a = dialog::show(
            ctx,
            "stichwortsatz",
            &title,
            dialog::WIDTH,
            |ui| {
                dialog::text(ui, crate::tr("Ein Satz setzt diese Stichwörter später mit einem Klick auf die ausgewählten Fotos."));
                dialog::input(ui, crate::tr("Name"), name);
                ui.add_space(2.0);
                dialog::chip(ui, &list);
            },
            dialog::Buttons::confirm(crate::tr("Sichern"), ok),
        );
        match a {
            dialog::Answer::Primary if ok => {
                if let Some((n, w)) = self.kw_set_dialog.take() {
                    self.kw_sets.push((n.trim().to_string(), w));
                    self.kw_set = self.kw_sets.len() - 1;
                    save_kw_sets(&self.kw_sets);
                }
            }
            dialog::Answer::Cancel => self.kw_set_dialog = None,
            _ => {}
        }
    }

    /// Notice before the one-time model download of the face recognition (in the app window, not a system dialog).
    /// "Load" starts the download and the search, "Later" does nothing until the next start.
    fn face_model_dialog(&mut self, ctx: &egui::Context) {
        use crate::dialog;
        if !self.face_model_notice {
            return;
        }
        let mb = library::people::model_download_bytes().div_ceil(1_000_000) as usize;
        let a = dialog::show(
            ctx,
            "gesichtsmodell",
            crate::tr("Gesichtserkennung vorbereiten"),
            dialog::WIDTH,
            |ui| {
                dialog::text(ui, &crate::trf!("graineo lädt einmalig {a} MB für die Gesichtserkennung. Danach arbeitet sie ohne Internet.", a = dialog::number(mb)));
                dialog::text(ui, crate::tr("Mit „Später“ wird nichts geladen; die Frage kommt beim nächsten Start wieder. Abschalten: Einstellungen → Entwickeln → Personen → „Gesichter automatisch erkennen“."));
            },
            dialog::Buttons { cancel: "Später", primary: Some(("Laden", dialog::Style::Normal, true)) },
        );
        match a {
            dialog::Answer::Primary => {
                self.face_model_notice = false;
                library::people::allow_download();
                self.spawn_people_work(ctx, true);
            }
            dialog::Answer::Cancel => {
                self.face_model_notice = false;
                self.face_model_later = true;
            }
            dialog::Answer::Open => {}
        }
    }

    /// Window "Save as smart collection": enter a name, save search text + column selection.
    fn smart_dialog(&mut self, ctx: &egui::Context) {
        if !self.smart_window {
            return;
        }
        // all filters of the view as one rule (search, metadata columns, flags, stars, labels; library_filter_bar.rs)
        let search_text = self.smart_rule().unwrap_or_default();
        let mut open = true;
        let n = self.visible.len();
        let title = crate::trf!("Smart-Sammlung mit {a} sichern", a = crate::dialog::count(n, crate::tr("Treffer"), crate::tr("Treffern")));
        let name_ok = !self.smart_name.trim().is_empty();
        // an existing smart collection is never replaced from here: a taken name gets " (2)" etc.
        let actual_name = if name_ok { self.db.as_ref().and_then(|d| d.unique_smart_name(self.smart_name.trim()).ok()) } else { None };
        let renamed = actual_name.as_deref().filter(|n| *n != self.smart_name.trim()).map(str::to_string);
        // the set to save into (as for "New collection …"); a set that is gone falls back to the top level
        let sets = self.set_choices();
        if !sets.iter().any(|(id, _)| *id == self.smart_parent) {
            self.smart_parent = None;
        }
        let answer = crate::dialog::show(
            ctx,
            "smart",
            &title,
            crate::dialog::WIDTH,
            |ui| {
                crate::dialog::text(ui, crate::tr("Die Sammlung merkt sich die Regel, nicht die Fotos: neue passende Fotos kommen von selbst hinzu."));
                crate::dialog::input(ui, crate::tr("Name"), &mut self.smart_name);
                if let Some(n) = &renamed {
                    crate::dialog::text(ui, &crate::trf!("Der Name ist schon vergeben, gesichert wird als „{n}“.", n = n));
                }
                ui.add_space(4.0);
                Self::set_choice(ui, "smart-collection-set", &sets, &mut self.smart_parent);
                ui.add_space(4.0);
                ui.label(egui::RichText::new(crate::tr("Regel")).small().color(lr::TEXT_DIM));
                crate::dialog::chip(ui, &search_text);
            },
            crate::dialog::Buttons::confirm(crate::tr("Sichern"), name_ok),
        );
        let save = answer == crate::dialog::Answer::Primary;
        if answer == crate::dialog::Answer::Cancel {
            open = false;
        }
        if save && !self.smart_name.trim().is_empty() {
            let name = self.smart_name.trim().to_string();
            let parent = self.smart_parent;
            let started = self.in_background(
                move |db| {
                    let (id, _) = db.save_smart_new(&name, &search_text).map_err(|e| e.to_string())?;
                    // in the chosen set, at its end (new ones at the top level come last there anyway)
                    if parent.is_some() {
                        library::collection_order::place(db, id, parent, None)?;
                    }
                    // optional copy in the root folder (only with "write sidecars"); the library is the main store
                    if let Err(e) = library::collection_file::write_if_enabled(db) {
                        crate::log(&format!("Smart-Sammlungen nicht in Datei geschrieben: {e}"));
                    }
                    Ok(id)
                },
                |b, actions, r| match r {
                    Ok(id) => {
                        if let Some(db) = &b.db {
                            b.set_collections(db.collections().unwrap_or_default());
                        }
                        b.open_parents_of(id);
                        b.collection_drag.scroll_to = Some(id);
                        b.collection_drag.flash(id);
                        b.smart_counts.clear();
                        b.filter.text.clear();
                        if let Some(f) = &mut b.facets {
                            f.reset();
                        }
                        b.set_source(Source::Smart(id));
                    }
                    Err(e) => actions.push(Action::Status(crate::trf!("Smart-Sammlung nicht gesichert: {e}", e = e))),
                },
            );
            if started {
                self.smart_name.clear();
                open = false;
            }
        }
        self.smart_window = open;
    }

    fn source_name(&self) -> String {
        match &self.source {
            Source::All => crate::tr("Alle Fotos").into(),
            Source::Recent => crate::tr("Zuletzt importiert").into(),
            Source::Missing => crate::tr("Fehlende Fotos").into(),
            Source::Offline => crate::tr("Nicht verbunden").into(),
            Source::Browse(p) => crate::menu::i18n::fill_names(crate::tr("Ordner ansehen · {a}"), &[("a", &p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())]),
            Source::NotInCatalog => crate::tr("Nicht in der Bibliothek").into(),
            Source::Date(d) if d.is_empty() => crate::tr("Ohne Datum").into(),
            Source::Date(d) => match d.len() {
                4 => d.clone(),
                // `get` instead of slicing: a date from a test hook or an odd capture time never panics while drawing
                7 => format!("{} {}", month_name(d), d.get(..4).unwrap_or(d)),
                _ => format!("{} {} {}", tag_name(d), month_name(d.get(..7).unwrap_or(d)), d.get(..4).unwrap_or(d)),
            },
            Source::Duplicates => match self.dup.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                Some(g) => crate::trf!("Duplikate · {a} gleich, {b} ähnlich (Gruppen)", a = g.iter().filter(|x| x.kind == library::duplicates::Kind::Identical).count(), b = g.iter().filter(|x| x.kind == library::duplicates::Kind::Similar).count()),
                None => crate::tr("Duplikate · werden gesucht …").into(),
            },
            Source::Folder(s, p) => {
                let root = self.roots.iter().find(|x| x.id == *s).map(|x| x.name.clone()).unwrap_or_default();
                if p.is_empty() {
                    root
                } else {
                    format!("{root} / {}", p.trim_end_matches('/'))
                }
            }
            Source::Collection(id) => self.collections.iter().find(|s| s.id == *id).map(|s| s.name.clone()).unwrap_or_default(),
            Source::Quick => crate::tr("Schnellsammlung").into(),
            Source::Smart(id) => self
                .collections
                .iter()
                .find(|s| s.id == *id)
                .map(|s| match &s.search {
                    Some(q) => format!("{} ⚙ {q}", s.name),
                    None => crate::menu::i18n::fill_names(crate::tr("{a} ⚙ nicht übersetzbar"), &[("a", &s.name)]),
                })
                .unwrap_or_default(),
            Source::Similar(d) => crate::menu::i18n::fill_names(crate::tr("Ähnlich zu {a}"), &[("a", &self.rows.iter().find(|z| z.file == *d).map(|z| z.name.clone()).unwrap_or_default())]),
            Source::Keyword(id) => self.keywords.iter().find(|s| s.id == *id).map(|s| crate::menu::i18n::fill_names(crate::tr("Stichwort {a}"), &[("a", &s.name)])).unwrap_or_default(),
        }
    }

    fn header_row(&mut self, ui: &mut egui::Ui) {
        // fixed width (ends at the right panel): size/previews/work on the right, the
        // text on the left truncated – nothing slides under the sidebar
        let place = if self.search_global() {
            crate::tr("Gesamte Bibliothek").to_string()
        } else if matches!(self.source, Source::All) {
            // "All photos" follows the kind filter (photos / videos / both); the source in the side panel keeps its name
            crate::tr(count_words(self.kind).0).to_string()
        } else {
            self.source_name()
        };
        // count in the selected source; "of" only when a filter or the search hides something
        let mut left_text = if self.first_loading() {
            crate::tr("Bibliothek wird geladen …").to_string()
        } else if self.visible.len() == self.source_total && self.source_total == 1 {
            match self.kind {
                1 => crate::menu::i18n::fill_names(crate::tr("{a} · 1 Video"), &[("a", &place)]),
                2 => crate::menu::i18n::fill_names(crate::tr("{a} · 1 Medium"), &[("a", &place)]),
                _ => crate::menu::i18n::fill_names(crate::tr("{a} · 1 Foto"), &[("a", &place)]),
            }
        } else if self.visible.len() == self.source_total {
            match self.kind {
                1 => crate::menu::i18n::fill_names(crate::tr("{a} · {b} Videos"), &[("a", &place), ("b", &thousands(self.visible.len()))]),
                2 => crate::menu::i18n::fill_names(crate::tr("{a} · {b} Medien"), &[("a", &place), ("b", &thousands(self.visible.len()))]),
                _ => crate::menu::i18n::fill_names(crate::tr("{a} · {b} Fotos"), &[("a", &place), ("b", &thousands(self.visible.len()))]),
            }
        } else {
            crate::menu::i18n::fill_names(crate::tr("{a} · {b} von {c}"), &[("a", &place), ("b", &thousands(self.visible.len())), ("c", &thousands(self.source_total))])
        };
        if self.search_job.busy() && !self.filter.text.trim().is_empty() {
            left_text.push_str(crate::tr(" · sucht …"));
        } else if self.query_matches.is_some() {
            left_text.push_str(&crate::trf!(" · Suche {a} ms", a = format!("{:.0}", self.search_time_ms.max(1.0))));
            if let Some(n1) = self.sem_from {
                left_text.push_str(&crate::trf!(" · {a} Wort-Treffer, {b} ähnlich in der Bedeutung ({c} ms)", a = n1, b = self.visible.len() - n1, c = format!("{:.0}", self.sem_ms)));
            } else if self.sem_loading {
                left_text.push_str(crate::tr(" · Bedeutungssuche lädt …"));
            }
            if !self.sem_loading && self.sem_view.busy(&self.sem_for) {
                left_text.push_str(crate::tr(" · Bedeutungssuche läuft …"));
            }
        }
        if self.selection.len() > 1 {
            left_text.push_str(&crate::trf!(" · {a} ausgewählt", a = thousands(self.selection.len())));
        }
        let width = self.centre_width.min(ui.available_width());
        ui.allocate_ui_with_layout(egui::vec2(width, 20.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_max_width(width);
            // turn and jump act on the selection: right of the size slider (library_filter_bar.rs)
            self.selection_actions(ui);
            ui.add_space(10.0);
            if !self.loupe {
                ui.spacing_mut().slider_width = 110.0;
                let r = ui.add(egui::Slider::new(&mut self.cell_size, 110.0..=380.0).show_value(false));
                lr::hover_tip(ui, &r, crate::tr("Vorschaugröße"));
            }
            // only the notice that previews are paused for lack of space (the progress is shown at the top in the title bar)
            if self.generator.as_ref().is_some_and(|e| e.cache().paused()) {
                let w = (ui.available_width() * 0.4).clamp(60.0, 230.0);
                ui.add_sized([w, 18.0], egui::Label::new(egui::RichText::new(crate::tr("Vorschauen pausiert: wenig Platz")).small().color(lr::TEXT_DIM)).truncate());
            }
            if matches!(self.source, Source::Missing) && !self.visible.is_empty() && ui.add(egui::Button::new(egui::RichText::new(crate::tr("Einträge entfernen …")).small())).clicked() {
                self.missing_confirm = true;
            }
            // background work (sync, faces …) shows only in the title bar next to the logo (activity.rs)
            // count, and while something filters "Save these photos as a smart collection" (library_filter_bar.rs)
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| self.status_left(ui, left_text));
        });
        ui.add_space(4.0);
    }

    /// Is a sync/import of the library running right now? (Then states like "missing" may still be outdated.)
    pub fn work_running(&self) -> bool {
        self.work.lock().unwrap_or_else(|e| e.into_inner()).running
    }

    /// The library's background work for the status in the title bar (activity.rs): sync/import (paths shortened to the
    /// name of the root folder), face search, previews with counter, import, moving a folder.
    /// The running job as dialogs show it when their buttons wait for it ([`crate::dialog::blocked`]).
    pub fn running(&self) -> Option<crate::dialog::Running> {
        let a = self.work.lock().unwrap_or_else(|e| e.into_inner());
        if !a.running {
            return None;
        }
        let roots: Vec<(&str, &str)> = self.roots.iter().map(|r| (r.path.as_str(), r.name.as_str())).collect();
        let step = crate::activity::short_message(&a.message, &roots);
        let step = step.trim_end_matches('…').trim_end_matches('.').trim();
        let label = match a.kind {
            "sync" | "sync_full" => crate::tr("Abgleich"),
            "check" => crate::tr("Bibliothek prüfen"),
            "lightroom" => crate::tr("Lightroom-Import"),
            "lr_handover" => crate::tr("Lightroom-Bearbeitungen übernehmen"),
            "folder" => crate::tr("Ordner hinzufügen"),
            "index" => crate::tr("Suchindex aufbauen"),
            "rebuild" => crate::tr("Bibliothek neu aufbauen"),
            _ => crate::tr("Bibliothek"),
        };
        // "Abgleich · Pictures (NAS)": the step already names the sync
        let rest = step.strip_prefix(&format!("{} · ", crate::tr("Abgleich"))).unwrap_or(step);
        let what = if rest.is_empty() || rest == label { label.to_string() } else { format!("{label} · {rest}") };
        Some(crate::dialog::Running { what, since: a.started.map(|t| t.elapsed()).unwrap_or_default(), fraction: a.fraction, last: a.last })
    }

    pub fn activities(&self) -> Vec<crate::activity::Task> {
        use crate::activity::{Rank, Task};
        let mut v = Vec::new();
        if let Some(t) = self.import_label() {
            v.push(Task::new(Rank::Import, t));
        }
        if library::move_folder::busy() {
            v.push(Task::new(Rank::Move, crate::move_folder_ui::status_text()));
        }
        {
            let a = self.work.lock().unwrap_or_else(|e| e.into_inner());
            if a.running {
                let roots: Vec<(&str, &str)> = self.roots.iter().map(|r| (r.path.as_str(), r.name.as_str())).collect();
                let mut t = crate::activity::short_message(&a.message, &roots);
                // known share (sync: folders done): percent and the time still needed
                if let (Some(f), Some(since)) = (a.fraction.filter(|f| *f > 0.02 && *f < 1.0), a.started.map(|s| s.elapsed())) {
                    t.push_str(&crate::trf!(" · {p} % · noch etwa {t}", p = (f * 100.0).floor() as u32, t = crate::dialog::duration(since.mul_f32((1.0 - f) / f))));
                }
                // a check or rebuild started by the user counts as their work; the automatic sync is background
                v.push(Task::new(if automatic(a.kind) { Rank::Library } else { Rank::Started }, t));
            }
        }
        v.extend(crate::prefs::cache_task());
        v.extend(crate::prefs_search_model::task());
        if self.backup_status.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(crate::tr("läuft")) {
            v.push(Task::new(Rank::Backup, crate::tr("Sicherung der Bibliothek").to_string()));
        }
        let st = self.people_status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !st.is_empty() {
            v.push(Task::new(Rank::Faces, crate::menu::i18n::tr_msg(&st).into_owned()));
        } else if self.people_follow {
            // open work that has not started yet (waiting time, or other work first): says what is still to do
            let w = *self.people_work.lock().unwrap_or_else(|e| e.into_inner());
            if w.photos > 0 {
                v.push(Task::new(Rank::Faces, crate::trf!("Gesichter aktualisieren: noch {n}", n = thousands(w.photos))));
            } else if w.suggestions {
                v.push(Task::new(Rank::Faces, crate::tr("Namensvorschläge folgen").to_string()));
            }
        }
        if let Some(t) = crate::apple_photos_ui::status() {
            v.push(Task::new(Rank::Library, t));
        }
        // previews of the photos visible in the grid that are still missing (what the user is looking at)
        let level = preview::small();
        let visible_missing = self.requested.iter().filter(|k| k.1 == level && !self.textures.contains_key(k)).count();
        if visible_missing > 0 {
            v.push(Task::new(Rank::Visible, crate::trf!("Vorschauen hier: noch {a}", a = thousands(visible_missing))));
        }
        if let Some(e) = self.generator.as_ref() {
            let open = e.open_set();
            if !e.cache().paused() && open > 0 && e.background_running() {
                v.push(Task::new(Rank::Previews, crate::trf!("Vorschauen: noch {a}", a = thousands(open))));
            }
        }
        v
    }

    fn raster(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let t0 = std::time::Instant::now();
        self.header_row(ui);
        self.view_folder_bar(ui);
        // "View folder" without photos: the reason below the header row (which keeps its place)
        if self.visible.is_empty() && !self.refilter && matches!(self.source, Source::Browse(_)) {
            self.browse_empty(ui);
        }
        // first start / empty library: a calm hint with the way in instead of an empty middle
        if self.library_is_empty() && self.db.is_some() && !self.refilter && !self.first_loading() && !matches!(self.source, Source::Browse(_)) {
            self.library_empty(ui);
        }
        // a filter hides everything: centred in the empty grid, below the status row (library_filter_bar.rs)
        if self.no_matches_shown() {
            self.no_matches(ui);
        }
        if let Some(clockwise) = self.turn_request.take() {
            actions.push(Action::Turn(clockwise));
        }
        if crate::shortcuts::pressed(ui.ctx(), "jump_to_selection") {
            self.jump_request = true;
        }
        // ⌥⌘↑ / ⌥⌘↓: the collection shown moves within its level (collection_order_ui.rs)
        self.collection_order_keys(ui.ctx());
        let width = self.centre_width.min(ui.available_width());
        let columns = ((width / self.cell_size).floor() as usize).max(1);
        self.columns = columns;
        let cell = width / columns as f32;
        // rows: full-text hits, then (if any) a separator row and "Similar in meaning"
        let n1 = self.sem_from.unwrap_or(self.visible.len());
        let r1 = n1.div_ceil(columns);
        let r2 = (self.visible.len() - n1).div_ceil(columns);
        let rows_n = r1 + if self.sem_from.is_some() { 1 + r2 } else { 0 };
        let start_of = |order: usize| -> Option<usize> {
            if order < r1 {
                Some(order * columns)
            } else if order == r1 {
                None
            } else {
                Some(n1 + (order - r1 - 1) * columns)
            }
        };
        // always the small level (exactly two levels; 1024 px for large cells is a choice in the settings)
        let level = preview::small();
        // modifier keys as held at the press: with a slow frame ⌘/⇧ may already be let go when the release arrives
        let (cmd, shift) = { let m = crate::click_mods::at_press(ui.ctx()); (m.command, m.shift) };
        let mut clicked = None;
        let mut double_click = None;
        let mut menu: Option<(Menu, i64)> = None;
        let mut quick_click: Option<i64> = None;
        let mut cell_hovered = false;
        let mut shown: Vec<usize> = Vec::new();
        #[cfg(test)]
        self.test_cells.clear();
        let mut area = egui::ScrollArea::vertical().id_salt("bib-raster").auto_shrink([false; 2]);
        // test aid: GRAINEO_LIBRARY_SCROLL=<points per frame> scrolls by itself and measures
        if let Some(px) = crate::env::var("GRAINEO_LIBRARY_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
            area = area.vertical_scroll_offset(self.grid_y + px);
            self.scroll_measure(ui.ctx());
        }
        let mut scroll_test_y = None;
        if let Some(t) = self.scroll_test.as_mut() {
            scroll_test_y = t.offset(std::time::Instant::now(), self.opened_at.elapsed().as_secs_f32(), !self.visible.is_empty());
            if let Some(y) = scroll_test_y {
                area = area.vertical_scroll_offset(y);
            }
            ui.ctx().request_repaint();
        }
        if std::mem::take(&mut self.grid_top) && !self.to_active {
            area = area.vertical_scroll_offset(0.0);
            self.grid_y = 0.0;
        }
        if let Some(y) = self.grid_back_y.take() {
            // position from back then; if the active photo is not in view with it (the grid has changed in the meantime),
            // the "to the active photo" check below still applies
            area = area.vertical_scroll_offset(y);
        }
        // back from single image view: the photo opened stays at the same height on screen. Applied for a
        // few frames, because the list or the layout may still settle after the first frame; any scrolling ends it.
        // row pitch of show_rows: cell height plus the spacing between rows
        let pitch = cell + ui.spacing().item_spacing.y;
        // The separator row is low (grid_jump::SEP_H), the rows after it move up
        let layout = crate::grid_jump::Rows::new(n1, columns, self.sem_from.is_some(), pitch, ui.spacing().item_spacing.y);
        // grid row of a position in the list (after the hits, the separator row comes first)
        let row_of = |pos: usize| if pos < n1 { pos / columns } else { r1 + 1 + (pos - n1) / columns };
        if let Some((image, dy, left)) = self.grid_anchor.take() {
            let scrolled = ui.input(|i| i.smooth_scroll_delta.y != 0.0);
            if !scrolled {
                match self.visible.iter().position(|&i| self.rows[i].image == image) {
                    Some(pos) => {
                        let y = (layout.top(row_of(pos)) - dy).max(0.0);
                        area = area.vertical_scroll_offset(y);
                        if left > 1 {
                            self.grid_anchor = Some((image, dy, left - 1));
                        }
                    }
                    // list not there yet: wait (counts down, so it never waits forever)
                    None if left > 1 => self.grid_anchor = Some((image, dy, left - 1)),
                    None => self.to_active = true,
                }
            }
        }
        // Scroll the active photo into view. Checked again for a few frames: right after switching from the
        // single view the list or the layout (columns, panels) may still change, and a single scroll would then
        // leave the photo outside. Scrolling by the user ends it.
        if std::mem::take(&mut self.jump_request) {
            // explicit jump: always to the middle (also when the photo is already in view), the selection stays
            if let Some(pos) = self.active.and_then(|a| self.visible.iter().position(|&i| self.rows[i].image == a)) {
                let row = row_of(pos);
                area = area.vertical_scroll_offset(crate::grid_jump::center_offset(layout.top(row), cell, ui.available_height()));
            }
            self.active_settle = 0;
        }
        if std::mem::take(&mut self.to_active) {
            self.active_settle = 8;
        }
        if self.active_settle > 0 {
            if ui.input(|i| i.smooth_scroll_delta.y != 0.0) {
                self.active_settle = 0;
            } else {
                self.active_settle -= 1;
                let follow = self.sel_ext.as_ref().filter(|e| self.active == Some(e.anchor) && self.selection.contains(&e.head)).map(|e| e.head).or(self.active);
                if let Some(pos) = follow.and_then(|a| self.visible.iter().position(|&i| self.rows[i].image == a)) {
                    // row pitch as in show_rows (cell plus spacing); with the cell height alone the target drifted
                    // by the spacing per row (in long lists by whole screens)
                    let y = layout.top(row_of(pos));
                    let view_h = ui.available_height();
                    let old = self.grid_y;
                    if y < old || y + cell > old + view_h {
                        area = area.vertical_scroll_offset((y - view_h / 2.0 + cell / 2.0).max(0.0));
                    }
                }
            }
            ui.ctx().request_repaint();
        }
        // While returning to a remembered place, the scroll area still has the state of the last grid frame:
        // kinetic velocity from scrolling before leaving would let the grid coast on, and with the content
        // height of an older layout the rubber band would pull it back towards that old edge.
        // Both off while anchoring (decay 0 also clears the stored velocity).
        let kinetic = ui.spacing().scroll.kinetic;
        if self.grid_anchor.is_some() {
            ui.spacing_mut().scroll.kinetic.rubber_band = false;
            ui.spacing_mut().scroll.kinetic.decay_time = 0.0;
        }
        if let Some(y) = self.band_scroll.take().filter(|_| self.band.is_some()) {
            area = area.vertical_scroll_offset(y);
        }
        // Opacity of the rows (curtain after a new result, dimmed while a newer search is pending)
        let (now, outdated) = (std::time::Instant::now(), self.search_job.outdated_since());
        let grid_y = self.grid_y;
        let out = crate::grid_jump::show_rows(area, ui, layout, rows_n, |ui, range| {
            for order in range {
                let Some(start) = start_of(order) else {
                    // separator row
                    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), crate::grid_jump::SEP_H), egui::Sense::hover());
                    let y = r.top() + 28.0;
                    ui.painter().line_segment([egui::pos2(r.left() + 4.0, y), egui::pos2(r.right() - 4.0, y)], egui::Stroke::new(1.0, Color32::from_gray(0x44)));
                    let g = lr::caps(ui.painter(), crate::tr("Ähnlich in der Bedeutung"), lr::semibold(11.0), Color32::from_gray(0xc8));
                    let gr = egui::Rect::from_min_size(egui::pos2(r.left() + 16.0, y - g.size().y / 2.0), g.size()).expand(6.0);
                    ui.painter().rect_filled(gr, 0.0, lr::IMAGE_BG);
                    ui.painter().galley(gr.min + egui::vec2(6.0, 6.0), g, Color32::from_gray(0xc8));
                    if self.visible.len() > n1 {
                        ui.painter().text(egui::pos2(r.left() + 16.0, y + 22.0), egui::Align2::LEFT_TOP, crate::trf!("{a} Fotos nach Bild- und Beschreibungsinhalt, ohne passendes Wort", a = self.visible.len() - n1), egui::FontId::proportional(10.5), lr::TEXT_DIM);
                    } else {
                        ui.painter().text(egui::pos2(r.left() + 16.0, y + 22.0), egui::Align2::LEFT_TOP, crate::tr("Keine ausreichend ähnlichen Fotos"), egui::FontId::proportional(10.5), lr::TEXT_DIM);
                    }
                    let (changed, released) = crate::semantic_ui::strictness_slider(ui, r, y + 28.0, &mut self.sem_view.strict);
                    if changed {
                        self.refilter = true;
                    }
                    if released {
                        crate::semantic_ui::save(self.sem_view.strict);
                    }
                    continue;
                };
                let end = if start < n1 { n1 } else { self.visible.len() };
                let alpha = self.motion.alpha(((layout.top(order) - grid_y).max(0.0) / pitch) as usize, now, outdated);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if alpha < 1.0 {
                        ui.multiply_opacity(alpha);
                    }
                    for sp in 0..columns {
                        let pos = start + sp;
                        if pos >= end {
                            break;
                        }
                        let Some(&i) = self.visible.get(pos) else { break };
                        shown.push(i);
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(cell, cell), egui::Sense::click_and_drag());
                        let z = self.rows[i].clone();
                        #[cfg(test)]
                        self.test_cells.push((z.image, rect));
                        let tex = match &self.hover_preview {
                            Some((b, t)) if *b == z.image => Some(t.clone()),
                            _ => self.texture(z.file, level),
                        };
                        let badge = self.paint_cell(ui, rect, &resp, &z, tex.as_ref(), pos + 1);
                        cell_hovered |= resp.hovered() || resp.context_menu_opened();
                        resp.context_menu(|ui| {
                            if let Some(m) = photo_menu(ui, self.quick.contains(&z.image), z.status == 0, self.can_paste, self.removable_collection().as_deref()) {
                                menu = Some((m, z.image));
                            }
                        });
                        // drag onto a collection on the left: the whole selection if this photo belongs to it
                        if resp.drag_started() && !shift && self.band.is_none() {
                            resp.dnd_set_drag_payload(self.drag_payload(z.image, z.file));
                        }
                        if badge {
                            quick_click = Some(z.image);
                        } else if resp.double_clicked() {
                            double_click = Some(z.image);
                        } else if resp.clicked() {
                            clicked = Some(z.image);
                        }
                    }
                });
            }
        });
        ui.spacing_mut().scroll.kinetic = kinetic;
        if self.motion.moving(now, outdated) {
            ui.ctx().request_repaint();
        }
        self.grid_y = out.state.clamped_offset().y;
        if let (Some(t), Some(wanted)) = (self.scroll_test.as_mut(), scroll_test_y) {
            let large = preview::large();
            let tiles: Vec<(i64, bool)> = shown.iter().map(|&i| self.rows[i].file).map(|f| (f, self.textures.contains_key(&(f, level)) || self.textures.contains_key(&(f, large)))).collect();
            let bytes: u64 = self.textures.values().map(|(t, _)| t.size()[0] as u64 * t.size()[1] as u64 * 4).sum();
            if let Some(report) = t.frame(std::time::Instant::now(), &tiles, bytes, self.textures.len(), wanted, self.grid_y) {
                crate::log(&format!("{report}; level {level} px, cell {:.0} pt × {:.1} = {:.0} px, {columns} columns", cell, ui.ctx().pixels_per_point(), cell * ui.ctx().pixels_per_point()));
                self.scroll_test = None;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.grid_cell = pitch;
        self.on_screen = shown.iter().map(|&i| self.rows[i].image).collect();
        if TEST_PHOTO_MENU.load(Ordering::Relaxed) {
            self.test_photo_menu(ui);
        }
        self.grid_view_h = out.inner_rect.height();
        let geometry = crate::library_select::GridGeometry { columns, first: n1, total: self.visible.len(), separated: self.sem_from.is_some(), cell, pitch, sep_pitch: layout.sep_pitch };
        let band_busy = self.band_update(ui, out.inner_rect, geometry);
        // request thumbnails: visible ones first, then one row before and two after
        if let (Some(&first), Some(&last)) = (shown.first(), shown.last()) {
            let pos_a = self.visible.iter().position(|&i| i == first).unwrap_or(0);
            let pos_b = self.visible.iter().position(|&i| i == last).unwrap_or(0);
            let mut wanted: Vec<(i64, u32)> = Vec::new();
            let mut jobs = Vec::new();
            let region = shown.iter().copied().chain(self.visible[(pos_b + 1).min(self.visible.len())..(pos_b + 1 + columns * 2).min(self.visible.len())].iter().copied()).chain(self.visible[pos_a.saturating_sub(columns)..pos_a].iter().copied());
            for i in region {
                let z = &self.rows[i];
                if self.textures.contains_key(&(z.file, level)) || wanted.contains(&(z.file, level)) {
                    continue;
                }
                // missing/archived files: only from the cache
                if z.status != 0 && !self.generator.as_ref().is_some_and(|e| e.cache().has(self.key(z), level)) {
                    continue;
                }
                wanted.push((z.file, level));
                jobs.push(self.task(z, level));
            }
            if wanted != self.requested {
                if let Some(e) = &self.generator {
                    e.visible(jobs);
                }
                self.requested = wanted;
            }
        }
        if let Some(b) = quick_click {
            // badge: if the image belongs to the selection, it applies to the whole selection, otherwise only to this image
            let target = if self.selection.contains(&b) && self.selection.len() > 1 { self.selection.iter().copied().collect() } else { vec![b] };
            if let Some(t) = self.quick_toggle(&target) {
                actions.push(Action::Status(t));
            }
        }
        if let Some(b) = clicked.filter(|_| !band_busy) {
            self.click(b, cmd, shift);
            actions.push(Action::Selection);
        } else if !band_busy && quick_click.is_none() && double_click.is_none() && !cell_hovered && !self.nothing_selected && !shift && ui.input(|i| i.pointer.primary_clicked()) && ui.rect_contains_pointer(out.inner_rect) {
            // click on free space between or after the photos: nothing selected any more
            self.clear_selection();
        }
        if let Some(b) = double_click {
            self.active = Some(b);
            self.open(b, actions);
        }
        if let Some((m, b)) = menu {
            if !self.selection.contains(&b) {
                self.selection.clear();
            }
            self.active = Some(b);
            self.menu(m, ui.ctx(), actions);
        }
        // measurement: time from opening the library until the grid shows its first tiles (once per run)
        static GRID_FIRST_LOGGED: AtomicBool = AtomicBool::new(false);
        if !shown.is_empty() && !GRID_FIRST_LOGGED.swap(true, Ordering::Relaxed) {
            crate::log(&format!("grid: first {} tiles after {:.0} ms", shown.len(), self.opened_at.elapsed().as_secs_f64() * 1000.0));
        }
        // measurement: time from opening the library until all visible tiles have their preview (once per run)
        if !shown.is_empty() && !GRID_COMPLETE_LOGGED.load(std::sync::atomic::Ordering::Relaxed) && shown.iter().all(|&i| self.textures.contains_key(&(self.rows[i].file, level))) {
            GRID_COMPLETE_LOGGED.store(true, std::sync::atomic::Ordering::Relaxed);
            crate::log(&format!("grid: all {} visible previews after {:.0} ms", shown.len(), self.opened_at.elapsed().as_secs_f64() * 1000.0));
        }
        // measurement: draw time per visible image (moving average)
        if !shown.is_empty() {
            let ms = t0.elapsed().as_secs_f64() * 1000.0 / shown.len() as f64;
            self.raster_ms = if self.raster_ms == 0.0 { ms } else { self.raster_ms * 0.95 + ms * 0.05 };
        }
    }

    fn scroll_measure(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let m = self.measurement.get_or_insert((now, 0, 0.0));
        m.1 += 1;
        m.2 = m.2.max(ctx.input(|i| i.unstable_dt) as f64 * 1000.0);
        let duration = now.duration_since(m.0).as_secs_f64();
        if duration >= 3.0 {
            crate::log(&format!(
                "grid measurement: {:.0} frames/s, longest {:.1} ms, drawing {:.3} ms per tile, y {:.0}, textures {}",
                m.1 as f64 / duration,
                m.2,
                self.raster_ms,
                self.grid_y,
                self.textures.len()
            ));
            self.measurement = None;
        }
        ctx.request_repaint();
    }

    fn click(&mut self, b: i64, cmd: bool, shift: bool) {
        self.sel_ext = None;
        if std::mem::take(&mut self.nothing_selected) {
            // from "nothing selected" every click starts a new selection at the clicked photo
            self.selection.clear();
            self.active = Some(b);
            return;
        }
        if cmd {
            let selected = self.selection.contains(&b) || self.active == Some(b);
            if selected {
                // ⌘-click on a selected photo deselects it; if it was the active one, another selected one becomes active;
                // the last one: nothing is selected any more (the photo stays the last active one)
                if let Some(a) = self.active {
                    self.selection.insert(a);
                }
                if self.selection.len() <= 1 {
                    self.selection.clear();
                    self.active = Some(b);
                    self.nothing_selected = true;
                } else {
                    self.selection.remove(&b);
                    if self.active == Some(b) {
                        let next = self.visible.iter().map(|&i| self.rows[i].image).find(|x| self.selection.contains(x));
                        self.active = next.or_else(|| self.selection.iter().next().copied());
                    }
                    if self.selection.len() == 1 {
                        self.selection.clear();
                    }
                }
            } else {
                if let Some(a) = self.active {
                    self.selection.insert(a);
                }
                self.selection.insert(b);
                self.active = Some(b);
            }
        } else if shift {
            let pa = self.active.and_then(|a| self.visible.iter().position(|&i| self.rows[i].image == a));
            let pb = self.visible.iter().position(|&i| self.rows[i].image == b);
            if let (Some(x), Some(y)) = (pa, pb) {
                for &i in &self.visible[x.min(y)..=x.max(y)] {
                    self.selection.insert(self.rows[i].image);
                }
            }
        } else {
            self.selection.clear();
            self.active = Some(b);
        }
    }

    /// Returns: quick collection badge (circle above the image) clicked.
    fn paint_cell(&self, ui: &egui::Ui, rect: egui::Rect, resp: &egui::Response, z: &Row, tex: Option<&TextureHandle>, number: usize) -> bool {
        let p = ui.painter();
        let inner = rect.shrink(3.0);
        let active = !self.nothing_selected && self.active == Some(z.image);
        let selected = !self.nothing_selected && self.selection.contains(&z.image);
        let mut fill = if active {
            Color32::from_gray(0x4a)
        } else if selected {
            Color32::from_gray(0x36)
        } else if resp.hovered() {
            Color32::from_gray(0x26)
        } else {
            Color32::from_gray(0x1a)
        };
        // color label tints the cell
        if z.label > 0 {
            let c = library::label::COLORS[z.label as usize];
            let mix = |a: u8, b: u8| ((a as u16 * 3 + b as u16) / 4) as u8;
            let g = fill.r();
            fill = Color32::from_rgb(mix(g, c[0]), mix(g, c[1]), mix(g, c[2]));
        }
        p.rect_filled(inner, 0.0, fill);
        if active {
            p.rect_stroke(inner, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0x9a)), egui::StrokeKind::Inside);
        } else if self.sel_ext.as_ref().is_some_and(|e| self.active == Some(e.anchor) && e.head == z.image && selected) {
            p.line_segment([inner.left_bottom() + egui::vec2(3.0, -2.0), inner.right_bottom() + egui::vec2(-3.0, -2.0)], egui::Stroke::new(2.0, Color32::from_gray(0xb0)));
        }
        // Fixed header row outside the photo area (`cell_label.rs`): state and number on the left, the
        // name with its marks in the middle, quick collection on the right in its own zone; nothing overlaps.
        let header = egui::Rect::from_min_size(inner.min + egui::vec2(5.0, 1.0), egui::vec2(inner.width() - 10.0, 22.0));
        let warning = matches!(z.status, 1 | 2);
        let number_x = header.left() + 2.0 + if warning { 20.0 } else { 0.0 };
        {
            use crate::cell_label as cl;
            let measure = cl::measurer(p, cl::SIZE);
            let zone_right = header.right() - cl::QUICK_W - cl::GAP;
            let number_text = number.to_string();
            let number_w = measure(&number_text).max(12.0);
            let score = self.sem_view.score.get(&z.file).filter(|_| self.sem_from.is_some_and(|n1| number > n1)).copied();
            // the search score next to the number only where `cell_value` draws it
            let score_w = if score.is_some() && header.right() - 25.0 - (number_x + number_w + 4.0) >= 74.0 { 38.0 } else { 0.0 };
            let copy_no = z.copy.as_deref().map(cl::copy_number);
            let copy_w = copy_no.map_or(0.0, |n| cl::copy_width(n, &measure));
            let pair = self.stack && z.sidecar_jpeg.is_some();
            let pair_w = if pair { measure("+JPG") } else { 0.0 };
            let name = display_name(z);
            let h = cl::header(zone_right - number_x, number_w + score_w, &name, copy_w, pair_w, &measure);
            let mut x = number_x;
            if h.number {
                p.text(egui::pos2(number_x, header.center().y), egui::Align2::LEFT_CENTER, number_text, egui::FontId::proportional(cl::SIZE), Color32::from_gray(0x8c));
                if let Some(pc) = score.filter(|_| score_w > 0.0) {
                    crate::semantic_ui::cell_value(ui, header, number_x + number_w, pc);
                }
                x += number_w + score_w + cl::GAP;
            }
            if let Some(n) = &h.name {
                let color = if active { Color32::from_gray(0xd0) } else { Color32::from_gray(0xa0) };
                let g = p.text(egui::pos2(x, header.center().y), egui::Align2::LEFT_CENTER, n, egui::FontId::proportional(cl::SIZE), color);
                if h.shortened {
                    let tip = ui.interact(g, egui::Id::unique(("zellname", z.image)), egui::Sense::hover());
                    lr::hover_tip(ui, &tip, &name);
                }
                x = g.right() + cl::GAP;
            }
            if h.copy {
                let r = cl::paint_copy(p, egui::pos2(x, header.center().y), copy_no.flatten(), Color32::from_gray(0xb8));
                let tip = ui.interact(r.expand(2.0), egui::Id::unique(("zellkopie", z.image)), egui::Sense::hover());
                lr::hover_tip(ui, &tip, &cl::copy_tip(z.copy.as_deref().unwrap_or_default()));
                x = r.right() + cl::GAP;
            }
            if h.pair {
                let g = p.text(egui::pos2(x, header.center().y), egui::Align2::LEFT_CENTER, "+JPG", egui::FontId::proportional(cl::SIZE), Color32::from_gray(0x8a));
                let tip = ui.interact(g, egui::Id::unique(("zellpaar", z.image)), egui::Sense::hover());
                lr::hover_tip(ui, &tip, "RAW+JPEG zusammen (ein Foto)");
            }
        }
        let image = egui::Rect::from_min_max(inner.min + egui::vec2(8.0, 25.0), inner.max - egui::vec2(8.0, 22.0));
        match tex {
            Some(t) => {
                // a photo just turned shows the turn at once, until its re-rendered preview arrives
                let quarters = self.turned.quarters(z.image);
                let s = crate::library_turn::turned_size(t.size_vec2(), quarters);
                let k = (image.width() / s.x).min(image.height() / s.y);
                let r = egui::Rect::from_center_size(image.center(), s * k);
                p.rect_filled(r.expand(1.0), 0.0, Color32::from_gray(0x10));
                // rejected photos dimmed (translucent), as in the filmstrip
                let tint = if z.flag == -1 {
                    Color32::WHITE.gamma_multiply(0.3)
                } else if z.status == 1 || z.status == 2 {
                    Color32::from_gray(110)
                } else {
                    Color32::WHITE
                };
                crate::library_turn::paint_image(p, t.id(), r, quarters, tint);
                // video: duration at the bottom left of the image ("▶ 0:42")
                if z.format == "VIDEO" {
                    let text = library::video::duration(&self.path_of(z)).map_or("▶".to_string(), |d| format!("▶ {}", library::video::duration_text(d)));
                    let g = p.layout_no_wrap(text, egui::FontId::proportional(10.5), Color32::WHITE);
                    let field = egui::Rect::from_min_size(r.left_bottom() + egui::vec2(4.0, -g.size().y - 8.0), g.size() + egui::vec2(8.0, 4.0));
                    p.rect_filled(field, 0.0, Color32::from_black_alpha(170));
                    p.galley(field.min + egui::vec2(4.0, 2.0), g, Color32::WHITE);
                }
                // virtual copy: folded corner at the bottom left
                if z.copy.is_some() {
                    let c = r.left_bottom();
                    p.add(egui::Shape::convex_polygon(vec![c, c + egui::vec2(0.0, -12.0), c + egui::vec2(12.0, 0.0)], Color32::from_gray(0xd8), egui::Stroke::NONE));
                }
            }
            None => {
                let placeholder = egui::Rect::from_center_size(image.center(), egui::vec2(image.width() * 0.8, image.width() * 0.55).min(image.size()));
                p.rect_filled(placeholder, 0.0, Color32::from_gray(0x22));
                let t = if z.status != 0 { "" } else { "…" };
                p.text(placeholder.center(), egui::Align2::CENTER_CENTER, t, egui::FontId::proportional(11.0), Color32::from_gray(0x82));
            }
        }
        // state: missing / volume missing
        let badge = match z.status {
            1 => Some(Color32::from_rgb(220, 110, 90)),
            2 => Some(Color32::from_rgb(200, 160, 90)),
            _ => None,
        };
        if let Some(c) = badge {
            let at = egui::pos2(header.left() + 8.0, header.center().y);
            p.circle_stroke(at, 6.0, egui::Stroke::new(1.0, c));
            p.text(at, egui::Align2::CENTER_CENTER, "!", egui::FontId::proportional(10.0), c);
        }
        // bottom: stars on the left, flag on the right, each in its own zone (narrow: "★5")
        let bottom = inner.left_bottom() + egui::vec2(8.0, -6.0);
        let stars_w = inner.right() - 8.0 - crate::cell_label::FLAG_W - crate::cell_label::GAP - bottom.x;
        if let Some(stars) = crate::cell_label::stars(z.stars, stars_w, &crate::cell_label::measurer(p, crate::cell_label::SIZE)) {
            p.text(bottom, egui::Align2::LEFT_BOTTOM, stars, egui::FontId::proportional(crate::cell_label::SIZE), if z.stars > 0 { Color32::from_gray(0xdd) } else { Color32::from_gray(0x70) });
        }
        // flag: tick for a pick, cross for a rejected one (its image is dimmed), the dashed circle of
        // "none" only under the pointer; at the bottom right, away from the quick-collection dot in the header
        let flag_at = inner.right_bottom() + egui::vec2(-14.0, -12.0);
        if z.flag != 0 || resp.hovered() {
            lr::paint_flag(&p, flag_at, 15.0, z.flag as i8, z.flag != 0, if z.flag != 0 { Color32::from_gray(0xe8) } else { Color32::from_gray(0x80) });
        }
        // quick collection: circle on the right of the fixed header row (filled = in, empty = not in)
        let inside = self.quick.contains(&z.image);
        let click;
        {
            let middle = egui::pos2(header.right() - 9.0, header.center().y);
            let matches = ui.interact(egui::Rect::from_center_size(middle, egui::vec2(20.0, 20.0)), egui::Id::unique(("schnell-plakette", z.image)), egui::Sense::click());
            click = matches.clicked();
            lr::tip_key(ui, &matches, if inside { "Aus der Schnellsammlung entfernen" } else { "Zur Schnellsammlung hinzufügen" }, "quick");
            let hovered = matches.hovered();
            let p = ui.painter();
            if inside {
                p.circle_filled(middle, 6.0, if hovered { Color32::WHITE } else { Color32::from_gray(0xe4) });
                p.circle_stroke(middle, 6.0, egui::Stroke::new(1.0, Color32::from_gray(0x20)));
            } else {
                p.circle_stroke(middle, 6.0, egui::Stroke::new(1.3, if hovered { Color32::WHITE } else if resp.hovered() { Color32::from_gray(0x90) } else { Color32::from_gray(0x58) }));
            }
        }
        click
    }

    fn right(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        // content never wider than the panel (otherwise it grows to the left over the centre)
        let w = ui.available_width();
        egui::ScrollArea::vertical().id_salt("bib-rechts-scroll").auto_shrink([false; 2]).max_width(w).show(ui, |ui| crate::sticky::scope(ui, |ui| {
            // inside the scroll area: never wider than its viewport (it keeps a band free for the bar)
            ui.set_max_width(ui.available_width().min(w));
            let Some(&i) = self.active.and_then(|a| self.index.get(&a)) else {
                lr::section(ui, "Metadaten", true, |ui| {
                    ui.label(egui::RichText::new(crate::tr("Kein Foto gewählt")).color(lr::TEXT_DIM));
                });
                return;
            };
            let z = self.rows[i].clone();
            // a viewed folder: rating only with the switch of its bar; locked controls say why
            let locked = z.folder == VIEW_FOLDER && !view_folder_sidecars();
            lr::section(ui, "Bewertung", true, |ui| {
                if locked {
                    ui.add(egui::Label::new(egui::RichText::new(crate::tr(RATING_REFUSED)).small().color(lr::TEXT_DIM)).wrap());
                }
                ui.add_enabled_ui(!locked, |ui| {
                ui.horizontal(|ui| {
                    lr::field_label(ui, crate::tr("Sterne"));
                    for n in 1..=5u8 {
                        let on = z.stars >= n;
                        let r = ui.add(egui::Label::new(egui::RichText::new(if on { "★" } else { "☆" }).size(15.0).color(if on { Color32::from_gray(0xe8) } else { Color32::from_gray(0x90) })).selectable(false).sense(egui::Sense::click()));
                        lr::hover_tip(ui, &r, &format!("{} ({n})", if n == 1 { crate::tr("1 Stern").to_string() } else { crate::trf!("{n} Sterne", n = n) }));
                        if r.clicked() {
                            self.rate(Some(if z.stars == n { 0 } else { n }), None, None, actions);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    lr::field_label(ui, crate::tr("Markierung"));
                    // Tick / circle / cross, the active one filled on a light ground
                    let tips = [lr::with_shortcut("Als Auswahl markieren", "pick"), lr::with_shortcut("Markierung aufheben", "unflag"), lr::with_shortcut("Ablehnen", "reject")];
                    let cells = [(lr::Cell::Flag(1), z.flag == 1, tips[0].as_str()), (lr::Cell::Flag(0), z.flag == 0, tips[1].as_str()), (lr::Cell::Flag(-1), z.flag == -1, tips[2].as_str())];
                    if let Some(k) = lr::cell_group_sized(ui, "info-markierung", &cells, 30.0, 24.0).0 {
                        let m = [1i8, 0, -1][k];
                        if m != z.flag {
                            self.rate(None, Some(m), None, actions);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    lr::field_label(ui, crate::tr("Etikett"));
                    // the same rectangular cells as the filter bar and the import window
                    let names = ["Etikett Rot", "Etikett Gelb", "Etikett Grün", "Etikett Blau", "Etikett Lila"];
                    let tips: Vec<String> = (1..=5u8).map(|e| if e <= 4 { format!("{} ({})", crate::tr(names[e as usize - 1]), e + 5) } else { crate::tr(names[e as usize - 1]).to_string() }).collect();
                    let cells: Vec<_> = (1..=5u8).map(|e| {
                        let c = library::label::COLORS[e as usize];
                        (lr::Cell::Color(Color32::from_rgb(c[0], c[1], c[2])), z.label == e, tips[e as usize - 1].as_str())
                    }).collect();
                    if let Some(i) = lr::cell_group_sized(ui, "info-etikett", &cells, 26.0, 22.0).0 {
                        self.rate(None, None, Some(i as u8 + 1), actions);
                    }
                });
                });
            });
            lr::section(ui, "Metadaten", true, |ui| {
                let mut rows = self.meta_rows(&z);
                // several selected: fields that differ show "‹verschieden›" instead of the active photo's value
                let differing = self.meta_differing();
                for (k, v) in rows.iter_mut() {
                    if differing.contains(*k) {
                        *v = crate::tr("‹verschieden›").to_string();
                    }
                }
                let name_key = crate::tr("Dateiname");
                let folder_key = crate::tr("Ordner");
                // the folder leads to its photos in the Bibliothek, only for photos in the library (a folder only
                // viewed with ⌘O has no library folder)
                let in_library = z.image > 0 && self.folder.iter().any(|o| o.id == z.folder);
                let folder_tip = lr::with_shortcut("In der Bibliothek zeigen", "show_in_library_folder");
                for (k, v) in rows {
                    let folder = k == folder_key && in_library && !differing.contains(k);
                    let clickable = k == name_key && !differing.contains(k) || folder;
                    let tip = if folder { folder_tip.as_str() } else { "" };
                    if lr::kv_row_tip(ui, k, &v, clickable, tip).clicked() {
                        if folder {
                            actions.push(Action::ShowFolder(z.image));
                        } else if clickable {
                            self.jump_request = true;
                        }
                    }
                }
            });
            // details from the opened file (only in Develop, once the photo is loaded): what the library does not know
            // Only what METADATEN does not show, in groups (technique · sensor · camera)
            if let Some((_, details)) = self.file_details.as_ref().filter(|(img, _)| *img == z.image) {
                let groups = crate::info_rows::groups(details);
                if groups.iter().any(|g| !g.is_empty()) {
                    lr::section(ui, "Aufnahmedetails", false, |ui| {
                        let mut first = true;
                        for (n, group) in groups.iter().enumerate().filter(|(_, g)| !g.is_empty()) {
                            if !first {
                                ui.add_space(6.0);
                            }
                            if n == 1 {
                                ui.label(egui::RichText::new(crate::tr("Sensor")).small().color(lr::TEXT_DIM));
                            }
                            first = false;
                            for (k, v) in group {
                                let r = lr::kv_row(ui, crate::tr(k), crate::tr(v), false);
                                if k == "Startwerte" {
                                    lr::hover_tip(ui, &r, "SIGMA Photo Pro speichert seine Einstellungen in der X3F-Datei. Weil du dieses Foto in graineo noch nicht bearbeitet hast, startet graineo mit diesen Werten.\nDeine eigenen Änderungen überschreiben sie; Zurücksetzen stellt neutral.");
                                }
                            }
                        }
                    });
                }
            }
            // keywords with their id: removing and the keyword sets act on exactly the keyword meant
            let (keywords, collections) = self.db.as_ref().map_or((Vec::new(), Vec::new()), |d| (library::keywords::shown_of(&d.conn, z.image).unwrap_or_default(), d.collections_of(z.image).unwrap_or_default()));
            // AI keywords in Lightroom (written by an AI tool, mostly without a parent term, partly under
            // "KI-Stichworte"/"KI-Analyse") visibly separated: same words as in the AI description
            let ai = self.db.as_ref().and_then(|d| d.ai_of(z.file));
            let ai_set: HashSet<String> = ai.as_ref().map(|k| k.keywords.iter().map(|w| search::fold_text(w)).collect()).unwrap_or_default();
            let (ai_kw, keywords): (Vec<(i64, String)>, Vec<(i64, String)>) =
                keywords.into_iter().partition(|(_, s)| s.starts_with("KI-") || ai_set.contains(&search::fold_text(s.rsplit(" › ").next().unwrap_or(s))));
            let words = search::free_words(&self.query);
            let mut search_word: Option<String> = None;
            lr::section(ui, "KI", true, |ui| {
                match &ai {
                    None => {
                        ui.label(egui::RichText::new(crate::tr("Keine KI-Beschreibung")).color(lr::TEXT_DIM));
                    }
                    Some(k) => {
                        let q = k.quality.map_or("–", |q| ["Ausschuss", "mittel", "gut"][q as usize % 3]);
                        lr::kv_row(ui, crate::tr("Qualität"), crate::tr(q), false);
                        lr::kv_row(ui, crate::tr("Technik"), &k.technical.map_or("–".into(), |v| format!("{v:.1}").de_comma()), false);
                        lr::kv_row(ui, crate::tr("Aussortieren"), &k.delete.map_or("–".into(), |v| format!("{v:.1}").de_comma()), false);
                        ui.add_space(4.0);
                        for t in [&k.scene, &k.reason].into_iter().flatten() {
                            highlighted(ui, t, &words);
                            ui.add_space(3.0);
                        }
                        let chips: Vec<String> = if k.keywords.is_empty() { ai_kw.iter().map(|(_, s)| s.rsplit(" › ").next().unwrap_or(s).to_string()).collect() } else { k.keywords.clone() };
                        if let Some(w) = chips_row(ui, &chips) {
                            search_word = Some(w);
                        }
                        if k.nonsense {
                            ui.label(egui::RichText::new(crate::tr("KI-Beschreibung als unsicher markiert")).small().color(lr::TEXT_DIM));
                        }
                        ui.label(egui::RichText::new(k.model.clone().unwrap_or_default()).small().color(Color32::from_gray(0x9a)));
                    }
                }
                ui.add_space(2.0);
                let state = self.similar_state.as_ref().map(|(_, s)| s.clone());
                let waiting = matches!(state, Some(SimilarState::Load | SimilarState::Computing));
                if ui.add_enabled(!waiting, egui::Button::new(crate::tr("Ähnliche Fotos"))).clicked() {
                    self.show_similar(ui.ctx());
                }
                // status right here, not down in the status bar
                match state {
                    Some(SimilarState::Load) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new().size(11.0));
                            ui.label(egui::RichText::new(crate::tr("Bildvektoren werden geladen …")).small().color(lr::TEXT_DIM));
                        });
                    }
                    Some(SimilarState::Computing) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new().size(11.0));
                            ui.label(egui::RichText::new(crate::tr("Bildvektor für dieses Foto wird berechnet …")).small().color(lr::TEXT_DIM));
                        });
                    }
                    Some(SimilarState::Error(e)) => {
                        ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(&e)).small().color(lr::TEXT_DIM)).wrap());
                    }
                    None => {}
                }
            });
            if let Some(w) = search_word {
                let q = if w.contains(' ') { format!("\"{w}\"") } else { w };
                let t = self.filter.text.trim().to_string();
                self.filter.text = if t.is_empty() { q } else { format!("{t} {q}") };
                self.refilter = true;
            }
            // people in the photo: confirmed names, suggestions to confirm, faces without names
            let faces = self.db.as_ref().map(|d| library::people::faces_to_image(&d.conn, z.image)).unwrap_or_default();
            let mut suggestion_answer: Option<(i64, bool)> = None;
            let mut person_search: Option<String> = None;
            if !faces.is_empty() {
                lr::section(ui, "Personen", true, |ui| {
                    let mut without = 0;
                    for g in &faces {
                        match g.status.as_str() {
                            "confirmed" => {
                                // name as a link: click searches all photos of this person
                                let r = lr::link_text(ui, egui::RichText::new(&g.name), lr::TEXT);
                                if r.clicked() {
                                    person_search = Some(g.name.clone());
                                }
                            }
                            "suggested" => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(egui::RichText::new(crate::trf!("Vorschlag: {a}", a = g.name)).color(Color32::from_rgb(0xe0, 0xc2, 0x4a)));
                                    ui.label(egui::RichText::new(format!("{:.2}", g.confidence)).small().color(lr::TEXT_DIM));
                                    if ui.small_button(crate::tr("Ja")).clicked() {
                                        suggestion_answer = Some((g.id, true));
                                    }
                                    if ui.small_button(crate::tr("Nein")).clicked() {
                                        suggestion_answer = Some((g.id, false));
                                    }
                                });
                            }
                            _ => without += 1,
                        }
                    }
                    if without > 0 {
                        ui.label(egui::RichText::new(crate::trf!("{a} Gesichter ohne Namen", a = without)).small().color(lr::TEXT_DIM));
                    }
                });
            }
            if let Some(n) = person_search {
                // exactly the faces confirmed as this person (live), not a free-text search of the name (index)
                self.filter.text = format!("person:\"{}\"", n.replace('"', ""));
                self.refilter = true;
            }
            if let Some((id, accept)) = suggestion_answer {
                self.in_background(move |d| library::people::answer_suggestion(d, id, accept), |b, _, _| b.review_checked = None);
                self.review_checked = None;
            }
            let mut kw_remove: Option<(i64, String)> = None;
            // chosen keyword (id) or typed text (resolved in the background)
            let mut kw_add: Option<(Option<i64>, String)> = None;
            lr::section(ui, "Stichwörter", true, |ui| {
                let n = self.target().len();
                if n > 1 {
                    ui.label(egui::RichText::new(crate::trf!("Änderungen gelten für {n} Fotos", n = n)).small().color(lr::TEXT_DIM));
                }
                if keywords.is_empty() && ai_kw.is_empty() {
                    ui.label(egui::RichText::new(crate::tr("Keine")).color(lr::TEXT_DIM));
                }
                // keyword chips with × to remove; keywords that are also words of the AI description come
                // after them, dimmed (a keyword added here that the AI also named must not disappear)
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                    for ((id, s), dim) in keywords.iter().map(|s| (s, false)).chain(ai_kw.iter().map(|s| (s, true))) {
                        if chip_remove(ui, s, dim) {
                            kw_remove = Some((*id, s.clone()));
                        }
                    }
                });
                // add with completion ("Orte > Italien > Rom" for hierarchy). The suggestions depend on the focus
                // at the start of the frame: a click on a suggestion takes the focus from the field in this frame
                // (egui), and the list must still be there to receive that click.
                let id = egui::Id::unique("library-keyword-input");
                let focused = ui.memory(|m| m.has_focus(id));
                let t = search::fold_text(self.kw_input.trim());
                // a name in several branches ("Paris" below places and people) is not taken from the typed text alone:
                // all of them come first in the list and one has to be chosen
                let same_name = if t.contains(['>', '›', '|']) { 0 } else { self.keywords.iter().filter(|s| search::fold_text(&s.name) == t).count() };
                let suggestions: Vec<(i64, String)> = if focused && (t.chars().count() >= 2 || same_name > 1) {
                    let mut v: Vec<&Keyword> = self.keywords.iter().filter(|s| search::fold_text(&s.name).starts_with(t.as_str())).collect();
                    v.sort_by(|a, b| (search::fold_text(&b.name) == t).cmp(&(search::fold_text(&a.name) == t)).then(b.count.cmp(&a.count)));
                    v.into_iter().take(same_name.max(6)).map(|s| (s.id, self.kw_path(s.id))).collect()
                } else {
                    Vec::new()
                };
                // ↓/↑ choose a suggestion, ↩ takes it (or the typed text when none is chosen)
                if suggestions.is_empty() {
                    self.kw_pick = 0;
                } else {
                    ui.input_mut(|i| {
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            self.kw_pick = (self.kw_pick + 1).min(suggestions.len());
                        }
                        if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            self.kw_pick = self.kw_pick.saturating_sub(1);
                        }
                    });
                    self.kw_pick = self.kw_pick.min(suggestions.len());
                }
                let r = crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut self.kw_input).id(id).hint_text(crate::tr("Stichwort hinzufügen")).desired_width(ui.available_width() - 4.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.kw_input.trim().is_empty() {
                    match suggestions.get(self.kw_pick.wrapping_sub(1)) {
                        Some((id, path)) => kw_add = Some((Some(*id), path.replace('|', " › "))),
                        None if same_name > 1 => {
                            // ambiguous: the field keeps the text, a suggestion has to be chosen
                            ui.memory_mut(|m| m.request_focus(id));
                        }
                        None => kw_add = Some((None, self.kw_input.trim().to_string())),
                    }
                }
                if same_name > 1 && self.kw_pick == 0 {
                    ui.label(egui::RichText::new(crate::trf!("„{a}“ gibt es mehrfach – bitte wählen", a = self.kw_input.trim())).small().color(lr::TEXT_DIM));
                }
                for (n, (kid, path)) in suggestions.iter().enumerate() {
                    if lr::list_item(ui, &path.replace('|', " › "), self.kw_pick == n + 1).clicked() {
                        kw_add = Some((Some(*kid), path.replace('|', " › ")));
                    }
                }
                if kw_add.is_some() {
                    // the field stays ready for the next keyword
                    self.kw_input.clear();
                    self.kw_pick = 0;
                    ui.memory_mut(|m| m.request_focus(id));
                }
                // keyword sets: one click sets or removes the keyword
                if !self.kw_sets.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        lr::field_label(ui, crate::tr("Satz"));
                        let names: Vec<String> = self.kw_sets.iter().map(|x| x.0.clone()).collect();
                        egui::ComboBox::from_id_salt("bib-sw-satz").selected_text(names.get(self.kw_set).cloned().unwrap_or_default()).width(150.0).show_ui(ui, |ui| {
                            for (i, n) in names.iter().enumerate() {
                                ui.selectable_value(&mut self.kw_set, i, n);
                            }
                        });
                    });
                    let set_words = self.kw_sets.get(self.kw_set).map(|x| x.1.clone()).unwrap_or_default();
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                        for w in &set_words {
                            // the keyword of the photo this set word means: the same path, or for a plain name the same leaf
                            let segs = library::keywords::split_typed(w);
                            let leaf = segs.last().cloned().unwrap_or_default();
                            let shown = segs.join(library::keywords::SHOWN_SEPARATOR);
                            let have = keywords.iter().find(|(_, p)| search::fold_text(p) == search::fold_text(&shown) || segs.len() == 1 && search::fold_text(p.rsplit(" › ").next().unwrap_or(p)) == search::fold_text(&leaf));
                            if ui.selectable_label(have.is_some(), &leaf).clicked() {
                                match have {
                                    Some(h) => kw_remove = Some(h.clone()),
                                    None => kw_add = Some((None, w.clone())),
                                }
                            }
                        }
                    });
                }
                if ui.add_enabled(!keywords.is_empty(), egui::Button::new(crate::tr("Als Stichwortsatz sichern …")).small()).clicked() {
                    let n = crate::trf!("Satz {a}", a = self.kw_sets.len() + 1);
                    self.kw_set_dialog = Some((n, keywords.iter().map(|(_, s)| s.replace(" › ", "|")).collect()));
                }
            });
            if kw_add.is_some() || kw_remove.is_some() {
                let target = self.target();
                let label = match (&kw_add, &kw_remove) {
                    (Some((_, t)), _) => crate::trf!("Stichwort „{a}“ zugewiesen", a = t),
                    (None, Some((_, t))) => crate::trf!("Stichwort „{a}“ entfernt", a = t),
                    _ => String::new(),
                };
                // database and sidecars in the background, by id (exactly the keyword meant); ⌘Z takes it back
                self.in_background(
                    move |db| {
                        let mut add = Vec::new();
                        match kw_add {
                            Some((Some(id), _)) => add.push(id),
                            Some((None, text)) => match library::keywords::resolve(db, &text, true)? {
                                library::keywords::Resolved::Existing(id) | library::keywords::Resolved::Created(id) => add.push(id),
                                library::keywords::Resolved::Missing => {}
                                library::keywords::Resolved::Ambiguous(paths) => return Err(library::keywords::ambiguous_message(&text, &paths)),
                            },
                            None => {}
                        }
                        let remove: Vec<i64> = kw_remove.into_iter().map(|x| x.0).collect();
                        library::keywords::change(db, &target, &add, &remove)
                    },
                    move |b, actions, r| {
                        match r {
                            Ok((n, snap)) => {
                                let text = crate::trf!("{a} ({n} Begleitdatei(en) geschrieben)", a = label, n = n);
                                actions.push(Action::Status(library_manage::keyword_status(&text, &snap)));
                                if !snap.is_empty() {
                                    b.manage.push(library_manage::Undo::Snap { label, snap, keywords: true, smart: false });
                                }
                            }
                            Err(e) => actions.push(Action::Status(crate::trf!("Stichwörter: {e}", e = e))),
                        }
                        b.reload_keywords();
                    },
                );
            }
            lr::section(ui, "Sammlungen", false, |ui| {
                if collections.is_empty() {
                    ui.label(egui::RichText::new(crate::tr("Keine")).color(lr::TEXT_DIM));
                }
                for s in collections {
                    ui.label(s);
                }
            });
        }));
    }

    /// Confirmation and background run "remove missing entries" ("Missing photos" view).
    fn missing_dialog(&mut self, ctx: &egui::Context) {
        // result of the background run "remove missing entries"
        let finished = self.missing_result.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = finished {
            if let Err(e) = &r {
                eprintln!("Remove missing entries: {e}");
            }
            self.reload();
        }
        if self.missing_confirm {
            let ids: Vec<i64> = self.visible.iter().map(|&i| &self.rows[i]).filter(|z| z.status == 1 && z.folder != VIEW_FOLDER).map(|z| z.image).collect();
            let a = crate::dialog::show(
                ctx,
                "fehlend-raus",
                &crate::trf!("{a} entfernen?", a = crate::dialog::count(ids.len(), crate::tr("fehlenden Eintrag"), crate::tr("fehlende Einträge"))),
                crate::dialog::WIDTH,
                |ui| {
                    crate::dialog::text(ui, crate::tr("Zu diesen Einträgen gibt es keine Datei mehr. Entfernt werden nur die Einträge aus der Bibliothek, es wird keine Datei angefasst. Der Lightroom-Import legt sie nicht wieder an."));
                    crate::dialog::chip(ui, &crate::dialog::count(ids.len(), crate::tr("Eintrag"), crate::tr("Einträge")));
                },
                crate::dialog::Buttons::destructive(crate::tr("Einträge entfernen"), !ids.is_empty()),
            );
            match a {
                crate::dialog::Answer::Primary => {
                    self.missing_confirm = false;
                    let result = self.missing_result.clone();
                    std::thread::spawn(move || {
                        let r = Db::open(&library::db_path()).map_err(|e| e.to_string()).and_then(|db| db.remove_images(&ids, &[]).map_err(|e| e.to_string()));
                        *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    });
                }
                crate::dialog::Answer::Cancel => self.missing_confirm = false,
                crate::dialog::Answer::Open => {}
            }
        }
    }

    // ───────────────────────────────────────────── Quick collection

    /// Read the quick collection from the database – not while jobs are still in flight
    /// (the display is then already more current than the database).
    fn quick_read(&mut self) {
        if self.quick_pending.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            return;
        }
        let Some(db) = &self.db else { return };
        self.quick_id = db.quick_id();
        self.quick = self.quick_id.map(|id| db.images_of_collection(id).unwrap_or_default().into_iter().collect()).unwrap_or_default();
    }

    /// Job for the writer thread (started the first time); the UI never waits for the database.
    fn quick_send(&mut self, a: QuickJob) {
        use std::sync::atomic::Ordering;
        let feedback = self.quick_feedback_queue.clone();
        let open = self.quick_pending.clone();
        let ctx = self.quick_ctx.clone();
        let tx = self.quick_tx.get_or_insert_with(|| {
            let (tx, rx) = std::sync::mpsc::channel::<QuickJob>();
            std::thread::spawn(move || {
                while let Ok(a) = rx.recv() {
                    let new_collection = matches!(a, QuickJob::Save(_));
                    let r = Db::open(&library::db_path()).map_err(|e| e.to_string()).and_then(|db| {
                        match a {
                            QuickJob::Set(b, add) => db.quick_set(&b, add).map(|_| ()),
                            QuickJob::Clear => db.quick_clear().map(|_| ()),
                            QuickJob::Save(n) => db.quick_as_collection(&n).map(|_| ()),
                        }
                        .map_err(|e| e.to_string())
                    });
                    open.fetch_sub(1, Ordering::SeqCst);
                    feedback.lock().unwrap_or_else(|e| e.into_inner()).push((new_collection && r.is_ok(), r.err()));
                    if let Some(c) = &ctx {
                        c.request_repaint();
                    }
                }
            });
            tx
        });
        self.quick_pending.fetch_add(1, Ordering::SeqCst);
        self.loader.quick_sent += 1;
        let _ = tx.send(a);
    }

    /// Fetch finished jobs: report errors, re-read if needed (after "save as collection" the whole library).
    fn quick_feedback(&mut self, actions: &mut Vec<Action>) {
        if self.quick_pending.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            return;
        }
        let feedback = std::mem::take(&mut *self.quick_feedback_queue.lock().unwrap_or_else(|e| e.into_inner()));
        if feedback.is_empty() {
            return;
        }
        let mut new_collection = false;
        let mut error = false;
        for (new, f) in feedback {
            new_collection |= new;
            if let Some(f) = f {
                error = true;
                actions.push(Action::Status(crate::trf!("Schnellsammlung: {f}", f = f)));
            }
        }
        if new_collection {
            self.reload();
        } else if error {
            // the display had run ahead: back to the state of the database
            self.quick_read();
            if self.source == Source::Quick {
                self.source_changed();
            }
        } else if self.quick_id.is_none() {
            // the first write created the collection: remember the ID for the display
            self.quick_read();
        }
    }

    /// B: put photos into the quick collection; if all are already in, take them out again.
    /// The display updates immediately. Returns: text for the status line.
    /// Is the photo in the quick collection?
    pub fn in_quick(&self, image: i64) -> bool {
        self.quick.contains(&image)
    }

    pub fn quick_toggle(&mut self, target: &[i64]) -> Option<String> {
        if target.is_empty() {
            return None;
        }
        let add = !target.iter().all(|b| self.quick.contains(b));
        if add {
            self.quick.extend(target.iter().copied());
        } else {
            for b in target {
                self.quick.remove(b);
            }
        }
        if self.source == Source::Quick {
            self.source_changed();
        }
        self.quick_send(QuickJob::Set(target.to_vec(), add));
        let n = target.len();
        Some(match (add, n) {
            (true, 1) => crate::tr("Zur Schnellsammlung hinzugefügt").to_string(),
            (true, n) => crate::trf!("{n} Fotos zur Schnellsammlung hinzugefügt", n = n),
            (false, 1) => "Aus der Schnellsammlung entfernt".to_string(),
            (false, n) => crate::trf!("{n} Fotos aus der Schnellsammlung entfernt", n = n),
        })
    }

    /// ⌘B: show the quick collection.
    pub fn show_quick(&mut self) {
        self.set_source(Source::Quick);
    }

    /// ⇧⌘B (clear) or ⌥⌘B (save as collection): open the confirmation.
    pub fn quick_ask(&mut self, save: bool) {
        if self.quick.is_empty() {
            return;
        }
        self.quick_query = if save { QuickQuery::Save(format!("Schnellsammlung {}", &library::backup::today()[..7]), true) } else { QuickQuery::Clear };
    }

    fn quick_context(&mut self, ui: &mut egui::Ui) {
        let empty = self.quick.is_empty();
        if lr::menu_item(ui, crate::tr("Als Sammlung sichern …"), &crate::shortcuts::display("quick_save"), !empty) {
            self.quick_ask(true);
        }
        ui.separator();
        if lr::menu_item(ui, crate::tr("Schnellsammlung leeren …"), &crate::shortcuts::display("quick_clear"), !empty) {
            self.quick_ask(false);
        }
    }

    /// Confirmations "Clear quick collection?" and "Save as collection".
    fn quick_dialogs(&mut self, ctx: &egui::Context) {
        use crate::dialog;
        let n = self.quick.len();
        match self.quick_query.clone() {
            QuickQuery::Off => {}
            QuickQuery::Clear => {
                let a = dialog::show(
                    ctx,
                    "schnell-leeren",
                    crate::tr("Schnellsammlung leeren?"),
                    dialog::WIDTH,
                    |ui| {
                        dialog::text(ui, crate::tr("Die Fotos bleiben in der Bibliothek, nur die Auswahl in der Schnellsammlung wird aufgehoben."));
                        dialog::chip(ui, &dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")));
                    },
                    dialog::Buttons::destructive(crate::tr("Leeren"), n > 0),
                );
                match a {
                    dialog::Answer::Primary => {
                        self.quick_query = QuickQuery::Off;
                        self.quick.clear();
                        if self.source == Source::Quick {
                            self.source_changed();
                        }
                        self.quick_send(QuickJob::Clear);
                    }
                    dialog::Answer::Cancel => self.quick_query = QuickQuery::Off,
                    dialog::Answer::Open => {}
                }
            }
            QuickQuery::Save(mut name, mut clear) => {
                let buttons = dialog::Buttons::confirm(crate::tr("Sichern"), n > 0 && !name.trim().is_empty());
                let a = dialog::show(
                    ctx,
                    "schnell-sichern",
                    crate::tr("Schnellsammlung als Sammlung sichern"),
                    dialog::WIDTH,
                    |ui| {
                        dialog::input(ui, crate::tr("Name der neuen Sammlung"), &mut name);
                        dialog::chip(ui, &dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")));
                        dialog::options(ui, |ui| {
                            dialog::option(ui, &mut clear, crate::tr("Schnellsammlung danach leeren"), crate::tr("Aus: die Fotos bleiben zusätzlich in der Schnellsammlung"), None);
                        });
                    },
                    buttons,
                );
                match a {
                    dialog::Answer::Primary => {
                        self.quick_query = QuickQuery::Off;
                        self.quick_send(QuickJob::Save(name.trim().to_string()));
                        if clear {
                            self.quick.clear();
                            if self.source == Source::Quick {
                                self.source_changed();
                            }
                            self.quick_send(QuickJob::Clear);
                        }
                    }
                    dialog::Answer::Cancel => self.quick_query = QuickQuery::Off,
                    dialog::Answer::Open => self.quick_query = QuickQuery::Save(name, clear),
                }
            }
        }
    }

    /// "Aus Lightroom importieren": one flow – choose the catalog (one found in the Pictures folder is suggested),
    /// check it (reads only: what would come and what is already there), import (writes; the edits come along in the
    /// same run). The gold main button is always the step needed now ([`crate::lightroom_state::Step`]); a catalog that
    /// cannot be read says why in the dialog, and the import button stays disabled with the reason.
    fn lr_dialog(&mut self, ctx: &egui::Context) {
        use crate::dialog;
        use crate::lightroom_state::{date, default_set_name, failure_text, Check, LrState, Step};
        if !self.lr_window {
            self.lr_state = None;
            return;
        }
        let (running, kind, message, fraction, handover_seen) = {
            let a = self.work.lock().unwrap_or_else(|e| e.into_inner());
            (a.running, a.kind, a.message.clone(), a.fraction, a.handover_seen)
        };
        let handover_now = HANDOVER_RUNNING.load(std::sync::atomic::Ordering::SeqCst);
        let lr_running = running && matches!(kind, "lightroom" | "lr_handover");
        // other work blocks the buttons: what runs, why, for how long and how much longer
        let running_other = self.running().filter(|_| !lr_running);
        // state once when opening and after every run (no file access per frame)
        let st = self.lr_state.get_or_insert_with(|| self.db.as_ref().map(LrState::read).unwrap_or_default()).clone();
        // without a chosen catalog: list Lightroom's folder once in the background (only names, nothing is opened)
        if st.catalog.is_none() && !self.lr_search_started {
            self.lr_search_started = true;
            let (found, c) = (self.lr_found.clone(), ctx.clone());
            std::thread::spawn(move || {
                let v = crate::lightroom_state::search_dir().map(|d| library::lightroom::find_catalogs(&d)).unwrap_or_default();
                *found.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
                c.request_repaint();
            });
        }
        let suggestion = self.lr_found.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|v| v.first().cloned()).filter(|_| st.catalog.is_none());
        let check: Option<Check> = self.lr_check.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(k, _)| Some(k) == st.catalog.as_ref()).map(|(_, c)| c.clone());
        let failed: Option<String> = self.lr_failed.as_ref().filter(|(k, _)| Some(k) == st.catalog.as_ref()).map(|(_, e)| e.clone());
        let step = st.step(check.as_ref(), failed.is_some(), suggestion.is_some(), lr_running);
        // Lightroom open (lock file, changes in the WAL): looked at every two seconds, only file metadata
        if let Some(k) = st.catalog.as_ref().filter(|_| st.catalog_found) {
            let stale = self.lr_in_use.as_ref().is_none_or(|(p, _, t)| p != k || t.elapsed().as_secs() >= 2);
            if stale {
                self.lr_in_use = Some((k.clone(), library::lightroom::catalog_in_use(k), std::time::Instant::now()));
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
        let in_use = self.lr_in_use.as_ref().is_some_and(|(p, u, _)| Some(p) == st.catalog.as_ref() && *u);
        if self.lr_set_name.is_none() {
            if let Some(k) = st.catalog.as_ref().or(suggestion.as_ref()) {
                self.lr_set_name = Some(default_set_name(k));
            }
        }
        let (mut choose, mut check_now, mut import, mut handover, mut show_report) = (false, false, false, false, false);
        let report = self.report.clone().filter(|v| v["kind"] == "lightroom" || v["kind"] == "lr_handover");
        let mut set_name = self.lr_set_name.clone().unwrap_or_default();
        let set_before = set_name.clone();
        let answer = dialog::show(
            ctx,
            "lightroom",
            crate::tr("Aus Lightroom importieren"),
            520.0,
            |ui| {
                dialog::text(ui, crate::tr("graineo liest deinen Lightroom-Katalog nur. Lightroom und deine Fotos bleiben unverändert."));
                ui.add_space(4.0);
                // ── Catalog: the chosen (or found) catalog in its own field, the button to change it beside it
                lr_section_head(ui, crate::tr("Katalog"), None);
                match (&st.catalog, &suggestion) {
                    (Some(k), _) if !st.catalog_found => lr_status_row(ui, Some(dialog::Verdict::Red), &failure_text("", k), |_| {}),
                    (None, Some(_)) => dialog::hint(ui, crate::tr("Gefunden in deinem Lightroom-Ordner:")),
                    _ => {}
                }
                let shown = st.catalog.as_ref().or(suggestion.as_ref()).map(|k| k.display().to_string());
                let empty = if st.catalog.is_none() && suggestion.is_none() && self.lr_found.lock().unwrap_or_else(|e| e.into_inner()).is_none() { "Sucht nach Katalogen …" } else { "Noch nichts gewählt" };
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // the main button already chooses: no second button for the same
                        if step != Step::Choose && ui.add_enabled(!lr_running, egui::Button::new(crate::tr(if st.catalog.is_some() { "Ändern …" } else { "Anderen wählen …" }))).clicked() {
                            choose = true;
                        }
                        lr::value_field(ui, shown.as_deref(), empty);
                    });
                });
                if st.catalog.is_none() && suggestion.is_none() && self.lr_found.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    let dir = crate::lightroom_state::search_dir().map(|d| d.display().to_string()).unwrap_or_default();
                    dialog::hint(ui, &crate::trf!("In „{a}“ ist keiner.", a = dir));
                }
                if in_use {
                    dialog::warning(ui, crate::tr("Lightroom ist geöffnet. Am besten vorher beenden, sonst fehlen die neuesten Änderungen."));
                } else {
                    dialog::hint(ui, crate::tr("Lightroom vorher beenden (sonst fehlen die neuesten Änderungen)."));
                }
                lr_separator(ui);
                // ── Import: check (reads only), then import; the edits come along
                lr_section_head(ui, crate::tr("Importieren"), Some(("lr-import", &crate::help::LR_IMPORT)));
                if lr_running {
                    let cancellable = kind == "lr_handover" || handover_now || handover_seen;
                    lr_progress(ui, fraction, &message, cancellable && handover_now);
                } else if let (Some(e), Some(k)) = (failed.as_ref().or(match &check {
                    Some(Check::Failed(e)) => Some(e),
                    _ => None,
                }), st.catalog.as_ref())
                {
                    lr_status_row(ui, Some(dialog::Verdict::Red), &failure_text(e, k), |ui| {
                        if ui.add_enabled(!running, egui::Button::new(crate::tr("Erneut prüfen"))).clicked() {
                            check_now = true;
                        }
                    });
                    dialog::hint(ui, crate::tr("Importieren geht erst mit einem lesbaren Katalog. Wähle einen anderen oder prüfe ihn erneut."));
                    egui::CollapsingHeader::new(egui::RichText::new(crate::tr("Details")).small().color(lr::TEXT_DIM)).id_salt("lr-fehler").default_open(false).show(ui, |ui| {
                        dialog::hint(ui, e);
                    });
                } else {
                    match &check {
                        Some(Check::Running) => {
                            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), LR_ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| dialog::pending(ui, crate::tr("Liest den Katalog …")));
                        }
                        Some(Check::Done(p)) => {
                            let photos = |n: usize| dialog::count(n, crate::tr("Foto"), crate::tr("Fotos"));
                            lr_status_row(ui, Some(dialog::Verdict::Green), &crate::trf!("{a}, davon {b} neu", a = photos(p.images), b = dialog::number(p.new_images)), |_| {});
                            let mut parts = vec![
                                dialog::count(p.collections + p.smart_collections, crate::tr("Sammlung"), crate::tr("Sammlungen")),
                                dialog::count(p.keywords, crate::tr("Stichwort"), crate::tr("Stichwörter")),
                                edited_photos(p.edited),
                            ];
                            if p.collection_sets > 0 {
                                parts.insert(1, dialog::count(p.collection_sets, crate::tr("Sammlungssatz"), crate::tr("Sammlungssätze")));
                            }
                            dialog::text(ui, &parts.join(" · "));
                            if let Some(v) = p.catalog_version.filter(|_| p.newer_format()) {
                                dialog::warning(ui, &newer_format_text(v));
                            }
                            if !p.catalog_gaps.is_empty() {
                                dialog::hint(ui, &crate::trf!("Im Katalog fehlt: {a}. Diese Angaben bleiben leer.", a = p.catalog_gaps.join(", ")));
                            }
                            if !p.roots_unreachable.is_empty() {
                                dialog::warning(ui, &crate::trf!("{a} gerade nicht erreichbar: {b}", a = dialog::count(p.roots_unreachable.len(), crate::tr("Ordner"), crate::tr("Ordner")), b = p.roots_unreachable.join(", ")));
                            }
                            if !p.roots_network.is_empty() {
                                dialog::hint(ui, &crate::trf!("{a} im Netzwerk, nicht geprüft: {b}", a = dialog::count(p.roots_network.len(), crate::tr("Ordner"), crate::tr("Ordner")), b = p.roots_network.join(", ")));
                            }
                            dialog::hint(ui, crate::tr("Die Bearbeitungen kommen gleich mit. Erst „Importieren“ schreibt in die Bibliothek."));
                        }
                        _ => {
                            let (verdict, text) = match &st.import {
                                Some(i) => (Some(dialog::Verdict::Green), crate::trf!("Zuletzt importiert: {d} · {n}", d = date(i.time), n = dialog::count(i.photos, crate::tr("Foto"), crate::tr("Fotos")))),
                                None => (None, crate::tr("Noch nicht importiert").to_string()),
                            };
                            lr_status_row(ui, verdict.filter(|_| st.catalog_found), &text, |ui| {
                                if st.import.is_some() && st.catalog_found && ui.add_enabled(!running, egui::Button::new(crate::tr("Erneut prüfen"))).clicked() {
                                    check_now = true;
                                }
                            });
                            if st.import.is_some() {
                                // the edits came along; a cancelled handover can be continued
                                let text = match &st.handover {
                                    Some(h) if h.cancelled => crate::trf!("Bearbeitungen: abgebrochen am {d} · {n} übernommen", d = date(h.time), n = dialog::number(h.taken)),
                                    Some(h) => crate::trf!("Bearbeitungen: {n} übernommen · {d}", n = dialog::number(h.taken), d = date(h.time)),
                                    None => crate::tr("Bearbeitungen: noch nicht übernommen").to_string(),
                                };
                                dialog::hint(ui, &text);
                            } else {
                                dialog::hint(ui, crate::tr("„Prüfen“ liest den Katalog und zeigt, was kommt. Geschrieben wird erst beim Importieren; die Bearbeitungen kommen gleich mit."));
                            }
                        }
                    }
                }
                // collection set: the catalog's collections go into one set (empty: at the top as in Lightroom)
                if st.catalog_found || suggestion.is_some() {
                    ui.add_space(4.0);
                    ui.add_enabled_ui(!lr_running, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(crate::tr("Sammlungssatz")).color(lr::TEXT_DIM));
                            let r = crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut set_name).hint_text(crate::tr("leer: ganz oben")).desired_width(240.0));
                            lr::hover_tip(ui, &r, "Die Sammlungen aus Lightroom kommen in diesen Sammlungssatz");
                        });
                    });
                }
                // ── Other work blocks the buttons: what runs, why, for how long and how much longer
                if let Some(r) = running_other.as_ref() {
                    lr_separator(ui);
                    dialog::blocked(ui, BLOCKED_WHY, r);
                }
                // ── Result of the last run in this session (details collapsed)
                if let Some(v) = report.as_ref().filter(|_| !lr_running) {
                    lr_separator(ui);
                    let u = &v["handover"];
                    let i = &v["import"];
                    let conflicts = i["conflicts"].as_array().map_or(0, |x| x.len()) + v["sync"]["conflicts"].as_array().map_or(0, |x| x.len());
                    let open = u["unreachable"].as_u64().unwrap_or(0) + u["errors"].as_u64().unwrap_or(0) + u["conflicts"].as_u64().unwrap_or(0);
                    let mut parts = Vec::new();
                    if !i.is_null() {
                        parts.push(crate::trf!("{n} neu importiert", n = dialog::count(i["new_images"].as_u64().unwrap_or(0) as usize, crate::tr("Foto"), crate::tr("Fotos"))));
                    }
                    if !u.is_null() {
                        // the same count as "bearbeitet" when checking: photos Lightroom marks as edited
                        let edited = u["edited"].as_u64().or_else(|| u["written"].as_u64()).unwrap_or(0) as usize;
                        parts.push(crate::trf!("{n} übernommen", n = edited_photos(edited)));
                    }
                    if let Some(e) = v["handover_error"].as_str() {
                        parts.push(crate::trf!("Bearbeitungen: {e}", e = e));
                    }
                    if conflicts as u64 + open > 0 {
                        parts.push(crate::trf!("{n} offen", n = conflicts as u64 + open));
                    }
                    let verdict = if conflicts as u64 + open > 0 || u["cancelled"] == true || !v["handover_error"].is_null() { dialog::Verdict::Yellow } else { dialog::Verdict::Green };
                    dialog::outcome(ui, verdict, &format!("{}: {}", crate::tr("Fertig"), parts.join(" · ")), &[]);
                    if let Some(t) = not_taken_text(v) {
                        dialog::hint(ui, &crate::trf!("Nicht übernommen: {a}", a = t));
                    }
                    if let Some(n) = i["catalog_version"].as_i64().filter(|_| i["newer_format"] == true) {
                        dialog::warning(ui, &newer_format_text(n));
                    }
                    egui::CollapsingHeader::new(egui::RichText::new(crate::tr("Details")).small().color(lr::TEXT_DIM)).id_salt("lr-details").default_open(false).show(ui, |ui| {
                        report_table(ui, v);
                        if ui.button(crate::tr("Bericht im Finder zeigen")).clicked() {
                            show_report = true;
                        }
                    });
                }
            },
            dialog::Buttons {
                // running: closing does not stop the run (it goes on in the background); the handover has its own "Abbrechen"
                cancel: if lr_running { crate::tr("Im Hintergrund fortsetzen") } else { crate::tr("Schließen") },
                primary: match step {
                    Step::Choose => Some((crate::tr("Katalog wählen …"), dialog::Style::Normal, !lr_running)),
                    Step::CheckSuggestion | Step::Check => Some((crate::tr("Prüfen"), dialog::Style::Normal, !running)),
                    Step::Import => Some((crate::tr("Importieren"), dialog::Style::Normal, !running)),
                    Step::Handover => Some((crate::tr("Bearbeitungen übernehmen"), dialog::Style::Normal, !running)),
                    // shown, but disabled: the dialog says why
                    Step::Blocked => Some((crate::tr("Importieren"), dialog::Style::Normal, false)),
                    Step::Wait if !lr_running => Some((crate::tr("Prüfen"), dialog::Style::Normal, false)),
                    Step::Wait | Step::Done => None,
                },
            },
        );
        // still unset (no catalog known yet): stays unset, so the catalog's name comes in as soon as there is one
        if self.lr_set_name.is_some() || set_name != set_before {
            self.lr_set_name = Some(set_name);
        }
        match answer {
            dialog::Answer::Cancel => self.lr_window = false,
            dialog::Answer::Primary => match step {
                Step::Choose => choose = true,
                Step::CheckSuggestion => {
                    if let Some(s) = suggestion.clone() {
                        self.lr_use_catalog(ctx, s);
                    }
                }
                Step::Check => check_now = true,
                Step::Import => import = true,
                Step::Handover => handover = true,
                Step::Blocked | Step::Wait | Step::Done => {}
            },
            dialog::Answer::Open => {}
        }
        if choose {
            crate::file_dialog::file(ctx, "lr-katalog-wahl", |d| d.add_filter(crate::tr("Lightroom-Katalog"), &["lrcat"]));
        }
        if let Some(p) = crate::file_dialog::fetch_one("lr-katalog-wahl") {
            self.lr_set_name = None;
            self.lr_use_catalog(ctx, p);
        }
        if check_now {
            if let Some(k) = st.catalog.clone() {
                self.lr_start_check(ctx, k);
            }
        }
        if import {
            self.report = None;
            self.lr_failed = None;
            self.start_job(ctx, "lightroom", st.catalog.clone());
        }
        if handover {
            self.report = None;
            self.lr_failed = None;
            self.start_job(ctx, "lr_handover", st.catalog.clone());
        }
        if show_report {
            // reading the folder (many reports, slow disk) stays out of the drawing thread
            std::thread::spawn(|| {
                let dir = library::data_dir().join("reports");
                let newest = std::fs::read_dir(&dir).ok().and_then(|rd| rd.flatten().map(|e| e.path()).max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok()));
                if let Some(p) = newest {
                    crate::system::show(p);
                }
            });
        }
    }

    /// Take this catalog: remembered in the background (never wait for the write lock in the UI), then checked right
    /// away (reads only).
    fn lr_use_catalog(&mut self, ctx: &egui::Context, catalog: PathBuf) {
        self.lr_failed = None;
        let k = catalog.clone();
        self.in_background(move |db| db.set_info("lightroom_catalog", &k.to_string_lossy()).map_err(|e| e.to_string()), |s, _, _| s.lr_state = None);
        // shown as chosen right away (the state is read again once it is stored)
        if let Some(st) = &mut self.lr_state {
            st.catalog_found = catalog.exists();
            st.catalog = Some(catalog.clone());
        }
        self.lr_start_check(ctx, catalog);
    }

    /// "Prüfen": read the catalog and compare it with the library, without writing to either.
    fn lr_start_check(&mut self, ctx: &egui::Context, catalog: PathBuf) {
        self.lr_failed = None;
        *self.lr_check.lock().unwrap_or_else(|e| e.into_inner()) = Some((catalog.clone(), crate::lightroom_state::Check::Running));
        let (slot, c) = (self.lr_check.clone(), ctx.clone());
        std::thread::spawn(move || {
            library::platform::set_priority(library::platform::Priority::Utility);
            let r = Db::read_only(&library::db_path()).map_err(|e| e.to_string()).and_then(|db| library::lightroom::preview(&db, &catalog));
            let v = match r {
                Ok(p) => crate::lightroom_state::Check::Done(p),
                Err(e) => crate::lightroom_state::Check::Failed(e),
            };
            let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
            // a newer check (another catalog) replaces this one
            if g.as_ref().is_some_and(|(k, _)| *k == catalog) {
                *g = Some((catalog, v));
            }
            drop(g);
            c.request_repaint();
        });
    }

    /// Confirmation "Rebuild library?" (requested from the settings, Library → Maintenance).
    fn rebuild_dialog(&mut self, ctx: &egui::Context) {
        if !self.rebuild_confirm {
            return;
        }
        let n = self.rows.iter().filter(|z| z.folder != VIEW_FOLDER).count();
        let busy = self.running();
        let a = crate::dialog::show(
            ctx,
            "rebuild",
            crate::tr("Bibliothek neu aufbauen?"),
            crate::dialog::WIDTH,
            |ui| {
                crate::dialog::text(ui, crate::tr("Erzeugt die Bibliothek frisch aus Ordnern, Begleitdateien und Lightroom-Katalog. Die bisherige Datei bleibt daneben liegen."));
                crate::dialog::chip(ui, &crate::trf!("heute {a}", a = crate::dialog::count(n, crate::tr("Foto"), crate::tr("Fotos"))));
                if let Some(r) = &busy {
                    crate::dialog::blocked(ui, BLOCKED_WHY, r);
                }
            },
            crate::dialog::Buttons::confirm(crate::tr("Neu aufbauen"), busy.is_none()),
        );
        match a {
            crate::dialog::Answer::Primary if busy.is_none() => {
                self.rebuild_confirm = false;
                self.start_job(ctx, "rebuild", None);
            }
            crate::dialog::Answer::Cancel => self.rebuild_confirm = false,
            _ => {}
        }
    }
}

/// Section heading of the Lightroom dialog: capitals, optionally "?" with a help card.
fn lr_section_head(ui: &mut egui::Ui, title: &str, help: Option<(&str, &crate::help::Card)>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let g = lr::caps(ui.painter(), title, lr::semibold(10.5), lr::TEXT_DIM);
        let (r, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
        ui.painter().galley(r.min, g, lr::TEXT_DIM);
        if let Some((salt, card)) = help {
            crate::help::info(ui, salt, title, card);
        }
    });
}

/// One line of state (status dot, text) with the section's own button on the right – fixed height, so that
/// the progress taking its place does not move anything.
fn lr_status_row(ui: &mut egui::Ui, verdict: Option<crate::dialog::Verdict>, text: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), LR_ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
        if let Some(v) = verdict {
            let c = match v {
                crate::dialog::Verdict::Green => Color32::from_rgb(0x4c, 0xc2, 0x6a),
                crate::dialog::Verdict::Yellow => Color32::from_rgb(0xe0, 0xb0, 0x40),
                crate::dialog::Verdict::Red => Color32::from_rgb(0xe0, 0x5c, 0x50),
            };
            ui.painter().circle_filled(r.center(), 4.0, c);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(crate::menu::i18n::tr_msg(text)).color(lr::TEXT)).truncate());
            });
        });
    });
}

const LR_ROW_H: f32 = 40.0;

/// Progress in place of the state line (the section heading names the action): bar with the real share when the
/// total is known, "Cancel" next to it when the step can be cancelled, below it the current step.
/// Wording of the count line for the kind filter (0 photos, 1 videos, 2 both): the name of "all" and what is counted.
fn count_words(kind: u8) -> (&'static str, &'static str) {
    match kind {
        1 => ("Alle Videos", "Videos"),
        2 => ("Alle Medien", "Medien"),
        _ => ("Alle Fotos", "Fotos"),
    }
}

fn lr_progress(ui: &mut egui::Ui, fraction: Option<f32>, message: &str, cancellable: bool) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), LR_ROW_H), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if cancellable && ui.small_button(crate::tr("Abbrechen")).clicked() {
                HANDOVER_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 0.0, Color32::from_gray(0x33));
            let blue = Color32::from_rgb(0x3b, 0x7d, 0xd8);
            match fraction {
                Some(f) => {
                    ui.painter().rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(r.width() * f.clamp(0.0, 1.0), r.height())), 0.0, blue);
                }
                None => {
                    // indeterminate: a short segment moving through
                    let t = ui.input(|i| i.time) as f32;
                    let x = r.left() + (t * 0.6).fract() * (r.width() + 80.0) - 80.0;
                    ui.painter().rect_filled(egui::Rect::from_x_y_ranges(x.max(r.left())..=(x + 80.0).min(r.right()), r.y_range()), 0.0, blue);
                    ui.ctx().request_repaint();
                }
            }
        });
        let share = fraction.map(|f| format!(" · {} %", (f * 100.0).floor() as u32)).unwrap_or_default();
        ui.add(egui::Label::new(egui::RichText::new(format!("{}{share}", crate::menu::i18n::tr_msg(message))).small().color(lr::TEXT_DIM)).truncate());
    });
}

fn lr_separator(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 9.0), egui::Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, egui::Stroke::new(1.0, Color32::from_gray(0x2c)));
}

/// Cancels a running handover of the Lightroom edits (after the current photo; a new run continues).
static HANDOVER_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static HANDOVER_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Take over the Lightroom develop data into the sidecars ([`library::lr_handover`]), with protocol in the reports folder.
fn lightroom_handover(db: &Db, catalog: &std::path::Path, step: &dyn Fn(Option<f32>, &str)) -> Result<serde_json::Value, String> {
    use std::sync::atomic::Ordering;
    HANDOVER_CANCEL.store(false, Ordering::SeqCst);
    HANDOVER_RUNNING.store(true, Ordering::SeqCst);
    let r = library::lr_handover::run_with(
        db,
        catalog,
        true,
        &|done, total| step((total > 0).then(|| done as f32 / total as f32), &crate::trf!("Lightroom-Bearbeitungen übernehmen … {a} von {b}", a = thousands(done), b = thousands(total))),
        &HANDOVER_CANCEL,
        &rawdev::lr_masks::not_taken,
    );
    HANDOVER_RUNNING.store(false, Ordering::SeqCst);
    let r = r?;
    let mut v = r.json();
    // Lightroom keeps AI edits beside the catalog (`NAME.lrcat-data`); a catalog copied without that folder has lost
    // them in Lightroom itself as well
    if r.not_taken.get(rawdev::lr_masks::AI_MASKS).is_some_and(|n| *n > 0) {
        let mut data = catalog.as_os_str().to_owned();
        data.push("-data");
        v["ai_data_missing"] = (!std::path::Path::new(&data).is_dir()).into();
    }
    if let Some(p) = library::lr_handover::save_protocol(&r) {
        v["log"] = serde_json::json!(p);
    }
    Ok(v)
}

/// "11 bearbeitete Fotos": photos (originals and virtual copies) Lightroom marks as edited – one term and one count
/// when checking and after the handover.
fn edited_photos(n: usize) -> String {
    crate::dialog::count(n, crate::tr("bearbeitetes Foto"), crate::tr("bearbeitete Fotos"))
}

fn newer_format_text(version: i64) -> String {
    crate::trf!(
        "Neueres Katalogformat als getestet ({a}, getestet: {b}). graineo importiert trotzdem; was fehlt, steht im Bericht.",
        a = version,
        b = library::lightroom::TESTED_DB_VERSION
    )
}

/// What the catalog has and graineo did not take over (import and handover of one run), as one line; `None` if
/// nothing. Kinds of the develop settings: [`rawdev::lr_masks::not_taken`].
fn not_taken_text(v: &serde_json::Value) -> Option<String> {
    let i = &v["import"]["not_taken"];
    let u = &v["handover"]["not_taken"];
    let get = |x: &serde_json::Value, k: &str| x[k].as_u64().unwrap_or(0) as usize;
    let kinds: [(usize, &str, &str); 9] = [
        (get(u, rawdev::lr_masks::AI_MASKS), crate::tr("KI-Maske"), crate::tr("KI-Masken")),
        (get(u, rawdev::lr_masks::RANGE_MASKS), crate::tr("Bereichsmaske"), crate::tr("Bereichsmasken")),
        (get(u, rawdev::lr_masks::SUBTRACTIONS), crate::tr("Maske mit Subtraktion"), crate::tr("Masken mit Subtraktion")),
        (get(u, rawdev::lr_masks::OTHER_MASKS), crate::tr("andere Maske"), crate::tr("andere Masken")),
        (get(u, rawdev::lr_masks::POINT_COLOR), crate::tr("Punktfarbe"), crate::tr("Punktfarben")),
        (get(u, rawdev::lr_masks::REMOVE_SPOTS), crate::tr("Entfernen-Punkt"), crate::tr("Entfernen-Punkte")),
        (get(i, "synonyms"), crate::tr("Stichwort-Synonym"), crate::tr("Stichwort-Synonyme")),
        (get(i, "stacks"), crate::tr("Stapel"), crate::tr("Stapel")),
        (get(i, "unknown_labels"), crate::tr("Etikett ohne Farbe (nur Text)"), crate::tr("Etiketten ohne Farbe (nur Text)")),
    ];
    let mut parts: Vec<String> = kinds.iter().filter(|(n, ..)| *n > 0).map(|(n, one, many)| crate::dialog::count(*n, one, many)).collect();
    if v["handover"]["ai_data_missing"] == true {
        parts.push(crate::tr("KI-Daten des Katalogs fehlen (Ordner „.lrcat-data“)").to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn report_table(ui: &mut egui::Ui, v: &serde_json::Value) {
    let i = &v["import"];
    let a = &v["sync"];
    let n = |x: &serde_json::Value| x.as_u64().map_or("–".to_string(), |n| thousands(n as usize));
    // long values wrap inside the dialog width instead of widening the window
    let value_width = (ui.available_width() - 190.0).max(160.0);
    egui::Grid::new("lr-bericht").num_columns(2).spacing([16.0, 3.0]).max_col_width(value_width).striped(true).show(ui, |ui| {
        let mut row = |k: &str, w: String| {
            ui.label(egui::RichText::new(k).color(lr::TEXT_DIM));
            ui.add(egui::Label::new(w).wrap());
            ui.end_row();
        };
        if !i.is_null() {
            row(crate::tr("Fotos"), crate::trf!("{a} (davon {b} neu, {c} virtuelle Kopien)", a = n(&i["images"]), b = n(&i["new_images"]), c = n(&i["virtual_copies"])));
            let formats = i["per_format"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k} {}", n(v))).collect::<Vec<_>>().join(" · ")).unwrap_or_default();
            row(crate::tr("Dateien"), format!("{} – {formats}", n(&i["files"])));
            row(crate::tr("Ordner"), crate::trf!("{a} in {b} Stammordnern", a = n(&i["folder"]), b = n(&i["roots"])));
            row(crate::tr("Bewertungen"), crate::trf!("{a} mit Sternen · {b} markiert · {c} abgelehnt · {d} mit Etikett", a = n(&i["with_stars"]), b = n(&i["flagged"]), c = n(&i["rejected"]), d = n(&i["with_label"])));
            row(crate::tr("Stichwörter"), crate::trf!("{a} ({b} Zuordnungen)", a = n(&i["keywords"]), b = n(&i["keyword_assignments"])));
            row(crate::tr("Sammlungen"), crate::trf!("{a} · {b} Sätze · {c} Smart (nur Name)", a = n(&i["collections"]), b = n(&i["collection_sets"]), c = n(&i["smart_collections"])));
            row(crate::tr("Titel · GPS"), crate::trf!("{a} mit Titel · {b} mit GPS-Position", a = n(&i["with_title"]), b = n(&i["with_position"])));
            if let Some(o) = i["unknown_labels"].as_object().filter(|o| !o.is_empty()) {
                row(crate::tr("Etiketten ohne Farbe"), o.iter().map(|(k, v)| format!("„{k}“ {}", n(v))).collect::<Vec<_>>().join(" · "));
            }
            let conflicts_n = i["conflicts"].as_array().map_or(0, |a| a.len());
            row(crate::tr("Konflikte"), conflicts_n.to_string());
            if let Some(version) = i["catalog_version"].as_i64() {
                let newer = if i["newer_format"] == true { crate::trf!(" – neuer als getestet ({a})", a = library::lightroom::TESTED_DB_VERSION) } else { String::new() };
                row(crate::tr("Katalogformat"), format!("{version}{newer}"));
            }
            if let Some(g) = i["catalog_gaps"].as_array().filter(|g| !g.is_empty()) {
                row(crate::tr("Im Katalog fehlt"), g.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "));
            }
            row(crate::tr("Dauer"), format!("{} ms", n(&v["import_duration_ms"])));
        }
        let u = &v["handover"];
        if !u.is_null() {
            row(
                crate::tr("Lightroom-Bearbeitungen"),
                crate::trf!(
                    "{a} geschrieben · {b} schon vorhanden · {c} Foto nicht gefunden · {d} Ziel nicht erreichbar",
                    a = n(&u["written"]),
                    b = n(&u["already"]),
                    c = n(&u["not_found"]),
                    d = n(&u["unreachable"])
                ),
            );
            row(crate::tr("Davon Kopien"), n(&u["copies"]));
            row(crate::tr("Bearbeitet (in Lightroom)"), n(&u["edited"]));
            row(crate::tr("Fehler"), format!("{} · {}", n(&u["errors"]), crate::trf!("{a} Konflikte", a = n(&u["conflicts"]))));
        }
        if let Some(t) = not_taken_text(v) {
            row(crate::tr("Nicht übernommen"), t);
        }
        if !a.is_null() {
            row(crate::tr("Vorhanden"), n(&a["existing"]));
            row(crate::tr("Fehlend"), n(&a["missing"]));
            let off = a["offline"].as_array().map(|l| l.iter().map(|o| format!("{} ({})", o["path"].as_str().unwrap_or(""), n(&o["files"]))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            row(crate::tr("Nicht verbunden"), if off.is_empty() { "–".into() } else { off });
            row(crate::tr("Neu von der Platte"), n(&a["newly_added"]));
            row(crate::tr("Begleit-JPEGs (RAW+JPEG)"), n(&a["sidecars"]));
            row(crate::tr("Ordner nicht in der Bibliothek"), crate::trf!("{a} mit {b} Bildern", a = a["unknown_folders"].as_object().map_or(0, |o| o.len()), b = n(&a["unknown_folders_images"])));
            row(crate::tr("Begleitdateien"), crate::trf!("{a} neu geschrieben ({b} waren defekt) · {c} aus Datei übernommen · {d} in Datei geschrieben · {e} offen (Anlegen aus)", a = n(&a["sidecar_new"]), b = n(&a["sidecar_broken"]), c = n(&a["from_sidecar"]), d = n(&a["in_sidecar"]), e = n(&a["sidecar_would"])));
            row(crate::tr("Wieder verknüpft"), n(&a["relinked"]));
        }
    });
}

/// Result dialog of a check of the library (or a rebuild, an added folder): new, missing, conflicts; the counts
/// below, the noteworthy sidecars as details.
fn sync_outcome(title: &str, v: &serde_json::Value) -> Dialog {
    let a = &v["sync"];
    let n = |x: &serde_json::Value| thousands(x.as_u64().unwrap_or(0) as usize);
    let conflicts = a["conflicts"].as_array().map_or(0, |x| x.len());
    let missing = a["missing"].as_u64().unwrap_or(0);
    let verdict = if conflicts > 0 || missing > 0 { crate::dialog::Verdict::Yellow } else { crate::dialog::Verdict::Green };
    let summary = crate::trf!("{new} neu, {missing} fehlen, {conflicts} Konflikte", new = n(&a["newly_added"]), missing = missing, conflicts = conflicts);
    let mut rows = vec![
        crate::trf!("Vorhanden: {n}", n = n(&a["existing"])),
        crate::trf!("Begleitdateien: {a} neu geschrieben · {b} aus Datei übernommen · {c} in Datei geschrieben", a = n(&a["sidecar_new"]), b = n(&a["from_sidecar"]), c = n(&a["in_sidecar"])),
    ];
    if let Some(off) = a["offline"].as_array().filter(|l| !l.is_empty()) {
        rows.push(crate::trf!("Nicht verbunden: {n}", n = off.iter().filter_map(|o| o["path"].as_str()).collect::<Vec<_>>().join(", ")));
    }
    let mut details: Vec<String> = a["conflicts"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    details.extend(a["sidecar_list"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(str::to_string)).take(50).collect::<Vec<_>>()).unwrap_or_default());
    Dialog::Outcome { title: title.to_string(), verdict, summary, rows, details }
}

/// Small capital label before a filter (not clickable).
/// Raw tokens of the search field (keep quoted text together).
fn raw_tokens(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    for c in t.chars() {
        if c == '"' {
            q = !q;
            cur.push(c);
        } else if c.is_whitespace() && !q {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Label and color of a chip: field (blue), NOT (red), word (grey).
fn chip_text(t: &str) -> (String, Color32) {
    let (not, r) = match t.strip_prefix('-') {
        Some(r) if !r.is_empty() => (true, r),
        _ => (false, t),
    };
    let r = r.trim_matches('"');
    let field = r.split_once(':').filter(|(k, _)| library::search_fields::field_id(k).is_some());
    let text = match field {
        Some((k, v)) => format!("{}{} {}", if not { crate::tr("nicht ") } else { "" }, k, v.trim_matches('"').replace("<=", "≤ ").replace(">=", "≥ ")),
        None => format!("{}{r}", if not { crate::tr("nicht ") } else { "" }),
    };
    let color = if not {
        Color32::from_rgb(0x7a, 0x34, 0x34)
    } else if field.is_some() {
        Color32::from_rgb(0x2e, 0x4c, 0x74)
    } else {
        Color32::from_gray(0x44)
    };
    (text, color)
}

/// Text with highlighted search hits (wrapping).
fn highlighted(ui: &mut egui::Ui, text: &str, words: &[String]) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let mut pos = 0;
    let normal = egui::TextFormat { font_id: font.clone(), color: lr::TEXT, ..Default::default() };
    let mark = egui::TextFormat { font_id: font, color: Color32::BLACK, background: Color32::from_rgb(0xe0, 0xc2, 0x4a), ..Default::default() };
    for (a, e) in search::highlight(text, words) {
        job.append(&text[pos..a], 0.0, normal.clone());
        job.append(&text[a..e], 0.0, mark.clone());
        pos = e;
    }
    job.append(&text[pos..], 0.0, normal);
    ui.label(job);
}

/// Clickable keyword chips (wrapping). Returns: clicked word.
fn chips_row(ui: &mut egui::Ui, words: &[String]) -> Option<String> {
    let mut clicked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for w in words {
            let g = ui.painter().layout_no_wrap(w.clone(), egui::FontId::proportional(10.5), lr::TEXT);
            let (r, resp) = ui.allocate_exact_size(g.size() + egui::vec2(14.0, 6.0), egui::Sense::click());
            ui.painter().rect_filled(r, 0.0, if resp.hovered() { Color32::from_gray(0x50) } else { Color32::from_gray(0x33) });
            ui.painter().galley(r.min + egui::vec2(7.0, 3.0), g, if resp.hovered() { Color32::WHITE } else { lr::TEXT });
            if resp.clicked() {
                clicked = Some(w.clone());
            }
        }
    });
    clicked
}

/// Display text of the selected flags ("Markiert · Ohne Flagge").
fn flags_text(f: u8) -> String {
    let mut t = Vec::new();
    for (bit, name) in [(FLAG_PICKED, "Markiert"), (FLAG_NONE, "Ohne Flagge"), (FLAG_REJECTED, "Abgelehnt")] {
        if f & bit != 0 {
            t.push(crate::tr(name));
        }
    }
    t.join(" · ")
}

/// Display text of the selected labels ("Rot, Blau").
fn labels_text(bits: u8) -> String {
    let mut t = Vec::new();
    for (n, name) in [(1, "Rot"), (2, "Gelb"), (3, "Grün"), (4, "Blau"), (5, "Lila")] {
        if bits & (1 << n) != 0 {
            t.push(crate::tr(name));
        }
    }
    t.join(", ")
}

fn header(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    let g = lr::caps(ui.painter(), text, egui::FontId::proportional(10.0), Color32::from_gray(0x8a));
    let (rect, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
    ui.painter().galley(rect.min, g, Color32::from_gray(0x8a));
}

/// 44275 → "44 275"
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// "2024-05-11T14:03:22" → "11.05.2024 14:03"
fn capture_date(a: &str) -> String {
    if a.len() >= 16 {
        format!("{}.{}.{} {}", &a[8..10], &a[5..7], &a[0..4], &a[11..16])
    } else {
        a.to_string()
    }
}


struct GridState {
    source: Source,
    active: Option<i64>,
    selection: HashSet<i64>,
    y: f32,
    /// distance of the active photo's row from the top of the visible grid (points)
    anchor_dy: Option<f32>,
}

/// Test aid `GRAINEO_LIBRARY_TEST="menu=1"`: the context menu of the active photo stays drawn open.
static TEST_PHOTO_MENU: AtomicBool = AtomicBool::new(false);

impl LibraryView {
    /// The context menu of the active photo, drawn open over the grid (test aid, see [`TEST_PHOTO_MENU`]).
    fn test_photo_menu(&self, ui: &mut egui::Ui) {
        let Some(a) = self.active else { return };
        let present = self.index.get(&a).is_some_and(|&i| self.rows[i].status == 0);
        let at = ui.max_rect().left_top() + egui::vec2(160.0, 120.0);
        egui::Area::new(egui::Id::new("test-photo-menu")).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_width(300.0);
                let _ = photo_menu(ui, self.quick.contains(&a), present, self.can_paste, self.removable_collection().as_deref());
            });
        });
    }
}

/// Actions from the context menu of a photo (grid, filmstrip, single image).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Menu {
    Export,
    /// add to or remove from the quick collection
    Quick,
    VirtualCopy,
    Rename,
    Move,
    Finder,
    Similar,
    /// show the image's folder in the sources on the left
    ShowFolder,
    /// delete the selection (trash, with confirmation)
    Delete,
    /// correct the capture time of the selection
    Time,
    CopySettings,
    PasteSettings,
    /// remove from the library (database only, the files stay)
    RemoveFromLibrary,
    /// take out of the collection shown (the photo stays in the library)
    RemoveFromCollection,
}

/// Context menu of a photo (grid; the filmstrip and the single image use the same order): frequent at the top,
/// file commands together, destructive last; shortcuts right-aligned. Returns the chosen command.
/// `collection`: name of the collection shown, if photos can be taken out of it (then ⌫ does that, ⌘⌫ the trash).
fn photo_menu(ui: &mut egui::Ui, in_quick: bool, present: bool, can_paste: bool, collection: Option<&str>) -> Option<Menu> {
    use crate::shortcuts::display;
    let mut chosen = None;
    let mut item = |ui: &mut egui::Ui, m: Menu, text: &str, key: String, enabled: bool| {
        if lr::menu_item(ui, text, &key, enabled) {
            chosen = Some(m);
        }
    };
    item(ui, Menu::Export, crate::tr("Exportieren …"), display("export"), true);
    ui.separator();
    let quick = if in_quick { crate::tr("Aus der Schnellsammlung entfernen") } else { crate::tr("Zur Schnellsammlung hinzufügen") };
    item(ui, Menu::Quick, quick, display("quick"), true);
    item(ui, Menu::VirtualCopy, crate::tr("Virtuelle Kopie anlegen"), display("virtual_copy"), true);
    item(ui, Menu::CopySettings, crate::tr("Einstellungen kopieren …"), display("copy_settings"), true);
    item(ui, Menu::PasteSettings, crate::tr("Einstellungen einfügen"), display("paste_settings"), can_paste);
    ui.separator();
    item(ui, Menu::Similar, crate::tr("Ähnliche Fotos"), String::new(), true);
    item(ui, Menu::Time, crate::tr("Aufnahmezeit bearbeiten …"), String::new(), true);
    ui.separator();
    item(ui, Menu::Rename, crate::tr("Umbenennen …"), String::new(), present);
    item(ui, Menu::Move, crate::tr("Verschieben nach …"), String::new(), present);
    item(ui, Menu::ShowFolder, crate::tr("Im Bibliotheksordner zeigen"), display("show_in_library_folder"), true);
    item(ui, Menu::Finder, crate::tr("Im Finder zeigen"), display("show_in_finder"), present);
    ui.separator();
    if let Some(name) = collection {
        item(ui, Menu::RemoveFromCollection, &crate::trf!("Aus „{a}“ entfernen", a = name), "⌫".into(), true);
    }
    item(ui, Menu::RemoveFromLibrary, crate::tr("Aus der Bibliothek entfernen …"), String::new(), true);
    // in a collection ⌫ only takes the photo out of it; ⌘⌫ moves it to the trash everywhere
    let trash_key = if collection.is_some() { display("trash_selected") } else { "⌫".into() };
    item(ui, Menu::Delete, crate::tr("In den Papierkorb …"), trash_key, present);
    chosen
}


/// Saved keyword sets (`keyword-sets.json` in Application Support).
fn kw_sets_file() -> PathBuf {
    library::data_dir().join("keyword-sets.json")
}

fn load_kw_sets() -> Vec<(String, Vec<String>)> {
    let v: serde_json::Value = std::fs::read_to_string(kw_sets_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(serde_json::json!({}));
    v["keywordSets"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| Some((x["name"].as_str()?.to_string(), x["keywords"].as_array()?.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()))).collect())
        .unwrap_or_default()
}

fn save_kw_sets(s: &[(String, Vec<String>)]) {
    let v = serde_json::json!({"keywordSets": s.iter().map(|(n, k)| serde_json::json!({"name": n, "keywords": k})).collect::<Vec<_>>()});
    let _ = std::fs::write(kw_sets_file(), serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// Chip with × (remove keyword). Returns: × clicked.
fn chip_remove(ui: &mut egui::Ui, text: &str, dim: bool) -> bool {
    let colour = if dim { lr::TEXT_DIM } else { lr::TEXT };
    let g = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(10.5), colour);
    let (r, _) = ui.allocate_exact_size(g.size() + egui::vec2(24.0, 6.0), egui::Sense::hover());
    let cross = egui::Rect::from_center_size(egui::pos2(r.right() - 9.0, r.center().y), egui::vec2(14.0, 14.0));
    let resp = ui.interact(cross, ui.scope_id().with(("sw-chip", text)), egui::Sense::click());
    ui.painter().rect_filled(r, 0.0, if dim { Color32::from_gray(0x26) } else { Color32::from_gray(0x33) });
    ui.painter().galley(r.min + egui::vec2(7.0, 3.0), g, colour);
    ui.painter().text(cross.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(11.0), if resp.hovered() { Color32::WHITE } else { lr::TEXT_DIM });
    resp.clicked()
}

impl LibraryView {
    /// Exposure line from the library ("1/60 s · f/2.2 · ISO 1600 · 50 mm"): shown in the info overlay while the photo
    /// itself is still loading, so the box is there right away.
    pub(crate) fn exposure_line(&self, image: i64) -> Option<String> {
        let r = self.rows.iter().find(|r| r.image == image)?;
        let mut parts = Vec::new();
        if let Some(t) = r.time.filter(|t| *t > 0.0) {
            parts.push(if t >= 0.5 { format!("{} s", ((t * 10.0).round() / 10.0).to_string().de_comma()) } else { format!("1/{} s", (1.0 / t).round()) });
        }
        if let Some(a) = r.aperture.filter(|a| *a > 0.0) {
            parts.push(format!("f/{}", ((a * 10.0).round() / 10.0)));
        }
        if let Some(iso) = r.iso.filter(|v| *v > 0) {
            parts.push(format!("ISO {iso}"));
        }
        if let Some(f) = r.focal.filter(|f| *f > 0.0) {
            parts.push(format!("{} mm", f.round()));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

impl LibraryView {
    /// keyword path "A|B|C".
    fn kw_path(&self, id: i64) -> String {
        let mut v = Vec::new();
        let mut cur = Some(id);
        for _ in 0..20 {
            let Some(c) = cur else { break };
            let Some(s) = self.keywords.iter().find(|s| s.id == c) else { break };
            v.push(s.name.clone());
            cur = s.parent;
        }
        v.reverse();
        v.join("|")
    }

    fn menu(&mut self, m: Menu, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(&i) = self.active.and_then(|a| self.index.get(&a)) else { return };
        let z = self.rows[i].clone();
        match m {
            Menu::Export => actions.push(Action::Export),
            Menu::Delete => actions.push(Action::Delete),
            Menu::Time => {
                let (files, photos) = capture_time_files(&self.target(), |b| self.index.get(&b).map(|&j| &self.rows[j]), self.stack);
                let input = z.capture.as_deref().map(time_display).unwrap_or_default();
                self.dialog = Some(Dialog::Time { files, photos, reference: z.capture.clone(), kind: 0, input, hours: 0 });
            }
            Menu::VirtualCopy => actions.push(Action::VirtualCopy(z.image)),
            Menu::Rename => {
                let stem = Path::new(&z.name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                self.dialog = Some(Dialog::Rename { file: z.file, name: stem });
            }
            Menu::Move => {
                let start = self.path_of(&z).parent().map(|p| p.to_path_buf());
                crate::file_dialog::target_folder(ctx, "bib-verschieben", move |d| match start {
                    Some(s) => d.set_directory(s),
                    None => d,
                });
            }
            Menu::Finder => {
                crate::system::show(self.path_of(&z));
            }
            Menu::ShowFolder => self.show_folder(z.image),
            Menu::Quick => {
                let target = self.target();
                if let Some(t) = self.quick_toggle(&target) {
                    actions.push(Action::Status(t));
                }
            }
            Menu::Similar => {
                self.show_similar(ctx);
            }
            Menu::CopySettings => actions.push(Action::CopySettings(z.image)),
            Menu::PasteSettings => actions.push(Action::PasteSettings),
            Menu::RemoveFromLibrary => {
                let target = self.target();
                self.ask_remove_photos(target);
            }
            Menu::RemoveFromCollection => {
                let target = self.target();
                self.remove_from_collection(target, actions);
            }
        }
    }

    /// Plan and run an operation (move/rename); errors as a window.
    fn operation(&mut self, plan: impl FnOnce(&Db) -> Result<library::relocate::Operation, String> + Send + 'static, actions: &mut Vec<Action>) {
        self.last_was_rating = false;
        self.ops += 1;
        // moving files (to the NAS: copy and verify) takes time: in the background, the UI stays usable
        // the list is re-read in the same job: correct the moment the move is done
        let ticket = self.load_ticket();
        let started = self.in_background(
            move |db| plan(db).and_then(|mut v| library::relocate::execute_step(db, &mut v).map(|_| (v, ticket.load(db).ok()))),
            |b, actions, r| match r {
                Ok((v, loaded)) => {
                    actions.push(Action::Status(crate::trf!("{a} – ⌘Z macht es rückgängig", a = v.title)));
                    b.move_at = crate::undo_order::tick();
                    b.adopt_or_reload(loaded);
                }
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            },
        );
        if started {
            actions.push(Action::Status(crate::tr("Wird verschoben …").into()));
        }
    }

    /// Undo the last move/rename operation.
    /// Revert the last move/rename (menu "Undo" in the grid).
    pub fn undo_external(&mut self) -> Vec<Action> {
        let mut a = Vec::new();
        self.undo(&mut a);
        a
    }

    fn undo(&mut self, _actions: &mut Vec<Action>) {
        // the newest step of managing the library (remove, rename, move, drop onto a collection) first
        if self.manage_undo() {
            return;
        }
        // the move of this session is taken back (older journals count as old)
        self.move_at = 0;
        // in the background; the message comes with the result, after a move still running (jobs run in order)
        let ticket = self.load_ticket();
        self.in_background(
            move |db| {
                let Some(mut v) = library::relocate::journals().into_iter().find(|v| v.status == "done") else { return Ok(None) };
                library::relocate::undo(db, &mut v).map(|_| Some((v.title, ticket.load(db).ok())))
            },
            |b, actions, r| match r {
                Ok(Some((title, loaded))) => {
                    actions.push(Action::Status(crate::trf!("Rückgängig: {a}", a = title)));
                    b.adopt_or_reload(loaded);
                }
                Ok(None) => actions.push(Action::Status(crate::tr("Nichts rückgängig zu machen").into())),
                Err(e) => b.dialog = Some(Dialog::Error(e)),
            },
        );
    }

    /// Search for duplicates (in the background, image vectors are loaded once for it) and show them.
    fn show_duplicates(&mut self) {
        self.enter_special_source();
        self.source = Source::Duplicates;
        self.source_ids = None;
        self.loupe = false;
        self.refilter = true;
        if self.dup.lock().unwrap_or_else(|e| e.into_inner()).is_some() || self.dup_running {
            self.build_dup_order();
            return;
        }
        self.dup_running = true;
        let target = self.dup.clone();
        std::thread::spawn(move || {
            let r = Db::standard().ok().and_then(|db| {
                let v = Vectors::load(&db).ok();
                library::duplicates::find(&db, v.as_ref()).ok()
            });
            *target.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.unwrap_or_default());
        });
    }

    fn build_dup_order(&mut self) {
        self.dup_order.clear();
        if let Some(g) = self.dup.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            for (gi, gr) in g.iter().enumerate() {
                for (j, d) in gr.files.iter().enumerate() {
                    self.dup_order.entry(*d).or_insert((gi, j));
                }
            }
        }
        self.refilter = true;
    }

    fn dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::dialog;
        // time correction done: re-read and show the result
        let finished = self.time_result.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(e) = finished {
            self.reload();
            self.dialog = Some(match e {
                Ok(b) => {
                    let mut rows = Vec::new();
                    if b.apple > 0 {
                        rows.push(crate::trf!("{a} aus Apple Fotos unverändert", a = dialog::count(b.apple, crate::tr("Foto"), crate::tr("Fotos"))));
                    }
                    if b.without_time > 0 {
                        rows.push(crate::trf!("{a} ohne Aufnahmezeit", a = dialog::count(b.without_time, crate::tr("Foto"), crate::tr("Fotos"))));
                    }
                    let verdict = if b.error.is_empty() { dialog::Verdict::Green } else { dialog::Verdict::Yellow };
                    Dialog::Outcome { title: crate::tr("Aufnahmezeit korrigiert").into(), verdict, summary: crate::trf!("{a} geändert", a = dialog::count(b.changed, crate::tr("Foto"), crate::tr("Fotos"))), rows, details: b.error }
                }
                Err(e) => Dialog::Error(e),
            });
        }
        let Some(d) = self.dialog.take() else { return };
        let mut d = d;
        let old_name = match &d {
            Dialog::Rename { file, .. } => self.rows.iter().find(|z| z.file == *file).map(|z| z.name.clone()).unwrap_or_default(),
            Dialog::RenameFolder { folder, .. } => self.folder.iter().find(|o| o.id == *folder).map(|o| o.path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()).unwrap_or_default(),
            Dialog::Error(_) | Dialog::Outcome { .. } | Dialog::Time { .. } => String::new(),
        };
        let title = match &d {
            Dialog::Rename { .. } => crate::trf!("„{old_name}“ umbenennen", old_name = old_name),
            Dialog::RenameFolder { .. } => crate::trf!("Ordner „{old_name}“ umbenennen", old_name = old_name),
            Dialog::Error(_) => crate::tr("Vorgang nicht ausgeführt").to_string(),
            Dialog::Outcome { title, .. } => title.clone(),
            Dialog::Time { photos, .. } => crate::trf!("Aufnahmezeit korrigieren · {a}", a = dialog::count(*photos, crate::tr("Foto"), crate::tr("Fotos"))),
        };
        let buttons = match &d {
            Dialog::Rename { name, .. } | Dialog::RenameFolder { name, .. } => dialog::Buttons::confirm(crate::tr("Umbenennen"), !name.trim().is_empty()),
            Dialog::Error(_) | Dialog::Outcome { .. } => dialog::Buttons::close(),
            Dialog::Time { reference, kind, input, hours, .. } => {
                let enabled = match kind {
                    0 => time_offset(reference.as_deref(), input).is_some_and(|d| d != 0),
                    1 => *hours != 0,
                    _ => true,
                };
                dialog::Buttons::confirm(if *kind == 2 { crate::tr("Wiederherstellen") } else { crate::tr("Korrigieren") }, enabled)
            }
        };
        let answer = dialog::show(
            ctx,
            "umbenennen",
            &title,
            dialog::WIDTH,
            |ui| match &mut d {
                Dialog::Rename { name, .. } => {
                    dialog::input(ui, crate::tr("Neuer Name (ohne Endung)"), name);
                    dialog::hint(ui, crate::tr("Begleitdateien (XMP, Einstellungen, Kamera-JPEG) bekommen denselben Namen."));
                }
                Dialog::RenameFolder { name, .. } => {
                    dialog::input(ui, crate::tr("Neuer Name"), name);
                    dialog::hint(ui, crate::tr("Der Ordner wird auf der Platte umbenannt; die Bibliothek folgt."));
                }
                Dialog::Error(t) => {
                    let mut rows = t.lines();
                    let first = rows.next().unwrap_or("").to_string();
                    let rest: Vec<String> = rows.map(str::to_string).filter(|l| !l.trim().is_empty()).collect();
                    dialog::outcome(ui, dialog::Verdict::Red, &first, &rest);
                }
                Dialog::Time { photos, reference, kind, input, hours, .. } => {
                    lr::segmented(ui, kind, &[(0u8, crate::tr("Auf Datum und Uhrzeit")), (1u8, crate::tr("Um Stunden verschieben")), (2u8, crate::tr("Ursprüngliche Zeit"))]);
                    ui.add_space(6.0);
                    match *kind {
                        0 => {
                            let old = reference.as_deref().map(time_display).unwrap_or_else(|| crate::tr("ohne Aufnahmezeit").into());
                            dialog::hint(ui, &crate::trf!("Aktives Foto, jetzt: {old}", old = old));
                            dialog::input(ui, crate::tr("Richtige Aufnahmezeit (TT.MM.JJJJ hh:mm:ss)"), input);
                            match time_offset(reference.as_deref(), input) {
                                Some(d) => dialog::hint(ui, &crate::trf!("Abstand {d} – alle {n} um denselben Abstand", d = offset_text(d), n = dialog::number(*photos))),
                                None => dialog::hint(ui, crate::tr("Zeit nicht lesbar")),
                            }
                        }
                        1 => {
                            ui.horizontal(|ui| {
                                crate::lr::text_field(ui, egui::DragValue::new(hours).range(-48..=48).suffix(" h"));
                                ui.label(egui::RichText::new(crate::tr("z. B. für eine andere Zeitzone")).small().color(lr::TEXT_DIM));
                            });
                        }
                        _ => dialog::hint(ui, crate::tr("Korrigierte Fotos bekommen wieder die Zeit der Kamera.")),
                    }
                    ui.add_space(4.0);
                    dialog::hint(ui, crate::tr("Die Originale bleiben unverändert; die Zeit steht in der Begleitdatei (auch für Lightroom). Fotos aus Apple Fotos bleiben unverändert."));
                }
                Dialog::Outcome { verdict, summary, rows, details, .. } => {
                    dialog::outcome(ui, *verdict, summary, details);
                    for z in rows.iter() {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.add(egui::Label::new(egui::RichText::new(z.as_str()).color(lr::TEXT_DIM)).wrap());
                        });
                    }
                }
            },
            buttons,
        );
        let go = answer == dialog::Answer::Primary && !matches!(d, Dialog::Error(_) | Dialog::Outcome { .. });
        if go {
            if let Dialog::Time { files, reference, kind, input, hours, .. } = &d {
                let offset = match kind {
                    0 => time_offset(reference.as_deref(), input),
                    1 => Some(*hours as i64 * 3600),
                    _ => None,
                };
                let (files, kind) = (files.clone(), *kind);
                let outcome = self.time_result.clone();
                let c = ctx.clone();
                std::thread::spawn(move || {
                    let e = Db::standard().map_err(|e| e.to_string()).and_then(|db| {
                        let sidecar = library::reconcile::write_allowed(&db);
                        match (kind, offset) {
                            (2, _) => library::time_correction::restore(&db, &files, sidecar),
                            (_, Some(a)) => library::time_correction::relocate(&db, &files, a, sidecar),
                            _ => Err(crate::tr("Zeit nicht lesbar").into()),
                        }
                    });
                    *outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    c.request_repaint();
                });
                return;
            }
        }
        let open = answer == dialog::Answer::Open;
        if go {
            match d {
                Dialog::Rename { file, name } => {
                    let dir = self.db.as_ref().and_then(|db| db.file_path(file)).and_then(|p| p.parent().map(|x| x.to_path_buf()));
                    if let Some(dir) = dir {
                        self.operation(move |db| library::relocate::plan_files(db, &[file], &dir, Some(name.trim())), actions);
                    }
                }
                Dialog::RenameFolder { folder, name } => {
                    self.operation(move |db| library::relocate::plan_folder(db, folder, name.trim()), actions);
                }
                Dialog::Error(_) | Dialog::Outcome { .. } | Dialog::Time { .. } => {}
            }
            return;
        }
        if open {
            self.dialog = Some(d);
        }
    }

    /// Background maintenance per frame: duplicate result, FSEvents (local), network drives, mounting,
    /// interrupted operations, daily backup.
    /// Start face detection in the background (once per program start, unless switched off) and refresh the number of
    /// names to review.
    fn people_maintenance(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        if let Some((review, suggestions)) = self.people_counts.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.review_n = review;
            self.suggestion_n = suggestions;
        }
        // counted in a thread with its own connection: both scan the faces (100 000 and more), which took up to seconds
        // in the UI thread every 10 s – also while paging in Develop
        if self.db.is_some() && self.review_checked.is_none_or(|t| now.duration_since(t).as_secs() >= 10) && !self.people_counting.swap(true, Ordering::SeqCst) {
            self.review_checked = Some(now);
            let (out, running, c, work) = (self.people_counts.clone(), self.people_counting.clone(), ctx.clone(), self.people_work.clone());
            let spawned = std::thread::Builder::new().name("people-counts".into()).spawn(move || {
                library::preview::low_priority_thread();
                // one read-only connection for the whole session instead of opening the library every 10 s
                static COUNT_DB: Mutex<Option<Db>> = Mutex::new(None);
                let mut g = COUNT_DB.lock().unwrap_or_else(|e| e.into_inner());
                if g.as_ref().is_none_or(|d| d.path != library::db_path()) {
                    *g = Db::read_only(&library::db_path()).or_else(|_| Db::standard()).ok();
                }
                if let Some(db) = g.as_ref() {
                    let counts = (library::people::check_count(&db.conn), library::people::suggestion_count(&db.conn));
                    // open work: count of photos without search (the exact list only when the cheap count changed)
                    static KNOWN: Mutex<Option<(usize, usize)>> = Mutex::new(None);
                    *work.lock().unwrap_or_else(|e| e.into_inner()) = library::people_state::measure(db, &mut KNOWN.lock().unwrap_or_else(|e| e.into_inner()));
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(counts);
                    c.request_repaint();
                }
                running.store(false, Ordering::SeqCst);
            });
            if spawned.is_err() {
                self.people_counting.store(false, Ordering::SeqCst);
            }
        }
        let forced = crate::env::var_os("GRAINEO_FACE_RUN").is_some();
        let env_ok = forced || (crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_none() && crate::env::var_os("GRAINEO_FACE_OFF").is_none());
        let allowed = env_ok && (forced || library::people::automatic_enabled());
        self.people_follow = allowed;
        if self.db.is_none() || self.work.lock().unwrap_or_else(|e| e.into_inner()).running {
            return;
        }
        if !allowed {
            // automatic face search switched off: the one-time steps after an update (box repair, Lightroom
            // revision) still come once, quietly, a while after the start
            if env_ok && !self.people_started {
                let not_before = *self.people_not_before.get_or_insert(now + std::time::Duration::from_secs(25));
                if now >= not_before {
                    self.people_started = true;
                    self.spawn_people_once(ctx);
                } else {
                    ctx.request_repaint_after(not_before - now);
                }
            }
            return;
        }
        if self.people_started {
            self.people_follow_up(ctx, now);
            return;
        }
        let not_before = *self.people_not_before.get_or_insert(now + std::time::Duration::from_secs(if forced { 3 } else { 25 }));
        if now < not_before {
            return;
        }
        self.people_started = true;
        self.spawn_people_work(ctx, true);
    }

    /// After the first run: run again when there is open work – new photos without face search (from an import or the
    /// Apple Photos sync), or suggestions that are out of date after confirmations. A waiting time first, so that a series
    /// of confirmations does not start a computation each time.
    fn people_follow_up(&mut self, ctx: &egui::Context, now: std::time::Instant) {
        // while someone answers in the people view, the suggestions stay as they are (they would change under their hands)
        if crate::people_ui::active_within(120) {
            ctx.request_repaint_after(std::time::Duration::from_secs(10));
            return;
        }
        let work = *self.people_work.lock().unwrap_or_else(|e| e.into_inner());
        if self.people_active.load(Ordering::SeqCst) || work.none() {
            if work.none() {
                self.people_waiting_since = None;
            }
            return;
        }
        let since = *self.people_waiting_since.get_or_insert(now);
        let after_run = self.people_ended.lock().unwrap_or_else(|e| e.into_inner()).is_none_or(|t| now.duration_since(t).as_secs() >= 60);
        let wait = if work.photos > 0 { 30 } else { 15 };
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
        if now.duration_since(since).as_secs() >= wait && after_run {
            self.people_waiting_since = None;
            self.spawn_people_work(ctx, work.photos > 0);
        }
    }

    /// Background thread for the one-time steps after an update only (automatic face search switched off).
    fn spawn_people_once(&mut self, ctx: &egui::Context) {
        let (status, cancel, c) = (self.people_status.clone(), self.people_cancel.clone(), ctx.clone());
        let (active, ended) = (self.people_active.clone(), self.people_ended.clone());
        active.store(true, Ordering::SeqCst);
        let _ = std::thread::Builder::new().name("people-once".into()).spawn(move || {
            library::platform::background_priority();
            let report = |t: String| {
                *status.lock().unwrap_or_else(|e| e.into_inner()) = t;
                c.request_repaint();
            };
            if let Ok(db) = library::db::Db::standard() {
                if library::people::once_pending(&db) {
                    for n in library::people::once_after_update(&db, &cancel, &report) {
                        crate::log(&n);
                    }
                }
            }
            active.store(false, Ordering::SeqCst);
            *ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
            report(String::new());
        });
    }

    /// Background thread: `search` = face search of the open photos (then suggestions), otherwise only the suggestions.
    fn spawn_people_work(&mut self, ctx: &egui::Context, search: bool) {
        // The search needs the face model: if it is not there yet, ask before the first download.
        if search && !library::people::model_present() {
            // only when there are photos to search (an empty library needs no model); later the follow-up asks
            if !self.face_model_later && self.people_work.lock().unwrap_or_else(|e| e.into_inner()).photos > 0 {
                self.face_model_notice = true;
                ctx.request_repaint();
            }
            return;
        }
        let (status, cancel, c) = (self.people_status.clone(), self.people_cancel.clone(), ctx.clone());
        let (active, ended) = (self.people_active.clone(), self.people_ended.clone());
        active.store(true, Ordering::SeqCst);
        report_start(&status, &c, search);
        std::thread::spawn(move || {
            let report = |t: String| {
                *status.lock().unwrap_or_else(|e| e.into_inner()) = t;
                c.request_repaint();
            };
            if let Ok(db) = library::db::Db::standard() {
                if search {
                    // the one-time steps after an update first, logged at once (the search after them can take long)
                    for n in library::people::once_after_update(&db, &cancel, &report) {
                        crate::log(&n);
                    }
                    match library::people::run(&db, &cancel, &report) {
                        Ok(b) => {
                            for n in &b.notes {
                                crate::log(n);
                            }
                            crate::log(&format!("faces: {} photos scanned, {} faces, {} errors", b.images, b.faces, b.error));
                        }
                        Err(e) => {
                            crate::log(&format!("Gesichter: {e}"));
                            active.store(false, Ordering::SeqCst);
                            *ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
                            report(crate::trf!("Gesichter: {e}", e = e));
                            return;
                        }
                    }
                } else {
                    library::platform::background_priority();
                    let r = library::people::assign(&db.conn, &library::people::Thresholds::default());
                    crate::log(&format!("people: suggestions recomputed: {r:?}"));
                }
            }
            active.store(false, Ordering::SeqCst);
            *ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
            report(String::new());
        });
    }

    fn maintenance(&mut self, ctx: &egui::Context) {
        self.people_maintenance(ctx);
        // waiting sidecar writes (cheap count, read only)
        if self.pending_checked.is_none_or(|t| t.elapsed().as_secs() >= 3) {
            self.pending_checked = Some(std::time::Instant::now());
            self.pending_sidecars = self.db.as_ref().map_or(0, |db| library::store::sidecars_waiting(&db.conn));
        }
        self.update_not_in_catalog(ctx);
        if self.dup_running && self.dup.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            self.dup_running = false;
            self.build_dup_order();
        }
        if crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_some() {
            return;
        }
        let now = std::time::Instant::now();
        // watchers for all local, reachable root folders
        let local_roots: Vec<PathBuf> = self.roots.iter().filter(|s| s.origin != library::apple_photos::ORIGIN).map(|s| PathBuf::from(&s.path)).filter(|p| crate::path_check::folder_exists(p) && library::watch::local(p)).collect();
        if local_roots != self.watched {
            self.watcher = library::watch::Watcher::start_scan(&local_roots, 1.0);
            self.watched = local_roots;
        }
        if let Some(b) = &self.watcher {
            let ev = b.fetch();
            for p in ev.iter().filter(|p| library::watch::relevant(p)) {
                self.changed_since.get_or_insert(now);
                // only the root folder with the change is synced (not every root, e.g. on the network drive)
                let s = library::nfc(&p.to_string_lossy());
                let root = self.watched.iter().filter(|r| s.starts_with(&*r.to_string_lossy())).max_by_key(|r| r.as_os_str().len()).cloned();
                self.changed_roots.insert(root);
            }
        }
        // batched: sync 2 s after the first change
        // at most every minute: ongoing mass changes (e.g. archiving) shouldn't keep the database busy all the time
        let idle = self.last_auto_sync.is_none_or(|t| now.duration_since(t).as_secs() >= 60);
        if self.changed_since.is_some_and(|t| now.duration_since(t).as_secs_f64() > 2.0) && idle && !self.work.lock().unwrap_or_else(|e| e.into_inner()).running && !library::move_folder::busy() {
            self.changed_since = None;
            self.last_auto_sync = Some(now);
            let roots = std::mem::take(&mut self.changed_roots);
            let one = if roots.len() == 1 { roots.into_iter().next().flatten() } else { None };
            self.start_job(ctx, "sync", one);
        }
        // once a day a full sync (every file's size and time), only while the user is away: the quick sync does not
        // notice files overwritten in place in an otherwise unchanged folder
        if self.full_checked.is_none_or(|t| now.duration_since(t).as_secs() >= 60) && crate::jobs::idle() && !self.work.lock().unwrap_or_else(|e| e.into_inner()).running && !library::move_folder::busy() {
            self.full_checked = Some(now);
            if self.db.as_ref().is_some_and(library::reconcile::full_due) {
                self.start_job(ctx, "sync_full", None);
            }
        }
        if now.duration_since(self.last_round).as_secs() >= 15 {
            self.last_round = now;
            // volume (re)mounted → sync right away
            let mut new_available = None;
            for s in &self.roots {
                let da = crate::path_check::folder_exists(Path::new(&s.path));
                if self.reachable.insert(s.id, da) == Some(false) && da {
                    new_available = Some(PathBuf::from(&s.path));
                }
            }
            // network drives: check every 15 minutes, one root folder per round (all of them in turn)
            let net_due = now.duration_since(self.net_round).as_secs() >= 900;
            let net: Option<PathBuf> = if net_due {
                static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let nets: Vec<PathBuf> = self.roots.iter().map(|s| PathBuf::from(&s.path)).filter(|p| crate::path_check::folder_exists(p) && !library::watch::local(p)).collect();
                (!nets.is_empty()).then(|| nets[NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % nets.len()].clone())
            } else {
                None
            };
            if net_due {
                self.net_round = now;
            }
            if let Some(p) = new_available.or(net) {
                self.start_job(ctx, "sync", Some(p));
            }
            // daily backup (background, own connection)
            if self.db.as_ref().is_some_and(library::backup::due) && self.backup_status.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                *self.backup_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::tr("läuft").into());
                let st = self.backup_status.clone();
                std::thread::spawn(move || {
                    let r = Db::standard().map_err(|e| e.to_string()).and_then(|db| library::backup::create_backup(&db));
                    *st.lock().unwrap_or_else(|e| e.into_inner()) = Some(match r {
                        Ok((p, n)) => format!("{} ({:.0} MB)", p.display(), n as f64 / 1e6),
                        Err(e) => crate::trf!("Fehler: {e}", e = e),
                    });
                });
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(2000));
    }

    /// After the rebuild: the new file takes the place of the old one at the next start (the old one is kept next to
    /// it). Swapping it while the app runs lost writes: the background writers (ratings, keywords, people) kept their
    /// connection to the renamed old file, and a failed rename left an empty new library.
    fn swap_rebuild(&mut self, v: &serde_json::Value) -> Action {
        let new = PathBuf::from(v["file"].as_str().unwrap_or(""));
        let db_path = self.db.as_ref().map(|d| d.path.clone()).unwrap_or_else(library::db_path);
        match library::backup::schedule_restore(&db_path, &new) {
            Ok(_) => {
                self.dialog = Some(sync_outcome(crate::tr("Bibliothek neu aufgebaut"), v));
                Action::Status(crate::tr("Bibliothek neu aufgebaut – graineo setzt sie beim nächsten Start ein (die alte bleibt daneben erhalten). Jetzt neu starten.").into())
            }
            Err(e) => Action::Status(crate::trf!("Neuaufbau nicht verwendbar: {e}", e = e)),
        }
    }

    /// Camera JPEG of a stack (image ID), if stacked.
    pub fn sidecar_jpeg_of(&self, image: i64) -> Option<i64> {
        self.stack.then(|| self.index.get(&image).and_then(|&i| self.rows[i].sidecar_jpeg)).flatten()
    }

    /// After deleting: re-read the library (or take the list the job loaded right after deleting); "View folder"
    /// re-reads its folder.
    pub fn after_delete(&mut self, loaded: Option<Loaded>) {
        if let (Source::Browse(_), Some((dir, _))) = (&self.source, self.viewed.clone()) {
            self.source = Source::All;
            self.browse_folder(&dir, None);
        } else {
            self.adopt_or_reload(loaded);
        }
        self.apply_filter();
    }

    /// Test aid (`GRAINEO_DIALOG`): open a dialog of the library.
    pub fn test_dialog(&mut self, ctx: &egui::Context, name: &str) {
        match name {
            // managing the library (screenshots): first subfolder / first collection / the grid photos
            "remove-folder" => {
                if let Some(o) = self.folder.iter().find(|o| !o.path.is_empty()).cloned() {
                    let name = o.path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
                    self.ask_remove_folder(o.root, o.path, name);
                }
            }
            "remove-photos" => {
                let v: Vec<i64> = self.visible.iter().take(3).map(|&i| self.rows[i].image).collect();
                self.ask_remove_photos(v);
            }
            "new-collection" => {
                self.selection = self.visible.iter().take(2).map(|&i| self.rows[i].image).collect();
                self.active = self.selection.iter().next().copied();
                self.ask_new_collection(library::manage::COLLECTION, None);
            }
            "imports" => self.ask_imports(),
            "delete-collection" => {
                if let Some(c) = self.collections.iter().find(|c| c.kind == 1).cloned() {
                    self.ask_delete(library_manage::Target::Collection(c.id), c.name);
                }
            }
            "smart" => {
                self.filter.text = "kamera:merrill".into();
                self.refilter = true;
                self.smart_window = true;
            }
            "lightroom" => self.lr_window = true,
            // the read-only check of the catalog of the (test) library, as after choosing it
            "lr-check" => {
                self.lr_window = true;
                if let Some(k) = self.db.as_ref().and_then(library::lightroom_catalog) {
                    self.lr_start_check(ctx, k);
                }
            }
            "rebuild" => self.rebuild_confirm = true,
            // start the import / the handover of the edits with the catalog of the (test) library
            "lr-import" | "lr-handover" => {
                self.lr_window = true;
                let k = self.db.as_ref().and_then(library::lightroom_catalog);
                self.start_job(ctx, if name == "lr-import" { "lightroom" } else { "lr_handover" }, k);
            }
            // screenshots of the progress (a tiny test catalog is done too fast): only the displayed state, no work
            "lr-running-import" | "lr-running-handover" => {
                self.lr_window = true;
                let mut a = self.work.lock().unwrap_or_else(|e| e.into_inner());
                a.running = true;
                if name == "lr-running-import" {
                    (a.kind, a.message, a.fraction) = ("lightroom", "39 165 Fotos übernehmen …".into(), None);
                } else {
                    (a.kind, a.message, a.fraction) = ("lr_handover", "Lightroom-Bearbeitungen übernehmen … 1 600 von 39 165".into(), Some(1600.0 / 39165.0));
                    HANDOVER_RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            // screenshots of the explanation of locked buttons: a sync of the first root folder runs (only the displayed
            // state, no work) – in the Lightroom dialog and in "Rebuild library?"
            "lr-locked" | "rebuild-locked" => {
                if name == "lr-locked" {
                    self.lr_window = true;
                } else {
                    self.rebuild_confirm = true;
                }
                let root = self.roots.first().map(|r| r.path.clone()).unwrap_or_default();
                let mut a = self.work.lock().unwrap_or_else(|e| e.into_inner());
                a.running = true;
                (a.kind, a.message, a.fraction) = ("sync", format!("Abgleich {root} …"), Some(0.42));
                a.started = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(75));
            }
            "card" => self.open_import(ctx),
            "rename" => {
                if let Some(z) = self.active.and_then(|a| self.index.get(&a)).map(|&i| self.rows[i].clone()) {
                    let stem = Path::new(&z.name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    self.dialog = Some(Dialog::Rename { file: z.file, name: stem });
                }
            }
            "import-done" => {
                self.dialog = Some(Dialog::Outcome {
                    title: crate::tr("Import abgeschlossen").into(),
                    verdict: crate::dialog::Verdict::Green,
                    summary: crate::trf!("{a} kopiert und geprüft", a = crate::dialog::count(58, crate::tr("Foto"), crate::tr("Fotos"))),
                    rows: vec![crate::trf!("in der Sammlung „{n}“ ({m})", n = "Herbst 2026", m = 58)],
                    details: Vec::new(),
                })
            }
            "error" => self.dialog = Some(Dialog::Error(crate::tr("Ziel existiert schon: DP2M5436.X3F\n/Users/…/2012-05-30/DP2M5436.X3F\n/Users/…/2012-05-30/DP2M5436.xmp").into())),
            "keyword-set" => self.kw_set_dialog = Some(("Satz 1".into(), vec!["Natur|Baum".into(), "Familie".into(), "Urlaub|Ostsee".into()])),
            _ => {}
        }
    }

}


impl LibraryView {
    /// Compute the semantic ranking for the current query (loads model and vectors in the
    /// background the first time). Only with free words, an installed text encoder and the setting on.
    /// Returns: the semantic hits at hand belong to the query shown (never hits of an older query).
    fn update_semantic(&mut self) -> bool {
        let words: Vec<String> = self
            .query
            .clauses
            .iter()
            .flatten()
            .filter(|t| !t.negated)
            .filter_map(|t| match &t.kind {
                search::Kind::Word { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        let text = words.join(" ");
        let on = library::settings::value("semanticSearch").as_bool().unwrap_or(true);
        // active search model (stage) available? (library::search_model)
        if text.is_empty() || !on || library::search_model::active().folder().is_none() {
            self.sem_rank.clear();
            self.sem_view.clear();
            self.sem_for.clear();
            return false;
        }
        if self.sem_for == text {
            // only the strictness changed: filter the values at hand, no recomputation
            if self.sem_applied != self.sem_view.strict {
                self.sem_rank = self.sem_view.apply();
                self.sem_applied = self.sem_view.strict;
            }
            return true;
        }
        // never compute here: remember the wish; `semantic_poll` starts the calculation (debounced, in the background)
        self.sem_view.wish(&text);
        false
    }

    /// Per frame: pick up a finished calculation, start the wished one after the typing pause,
    /// load model and vectors in the background the first time. Never computes or waits itself.
    fn semantic_poll(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.sem_view.take_result() {
            self.sem_rank = self.sem_view.set(r.list, r.lib_size);
            self.sem_applied = self.sem_view.strict;
            self.sem_ms = r.ms;
            self.sem_for = r.text;
            self.refilter = true;
        }
        let Some(text) = self.sem_view.wanted().map(str::to_string) else { return };
        if self.sem_for == text || self.sem_view.started_for(&text) {
            return;
        }
        let state = match self.semantic.try_lock() {
            Ok(g) => match g.as_ref() {
                Some(Ok(z)) => Some(Some(z.clone())),
                Some(Err(_)) => {
                    drop(g);
                    self.sem_view.clear();
                    return;
                }
                None => Some(None),
            },
            Err(_) => None,
        };
        let Some(state) = state else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let Some(z) = state else {
            if !self.sem_loading {
                self.sem_loading = true;
                let target = self.semantic.clone();
                std::thread::spawn(move || {
                    let r = (|| -> Result<std::sync::Arc<SemState>, String> {
                        let db = Db::standard().map_err(|e| e.to_string())?;
                        // vectors and encoder of the active stage (library::search_model)
                        let stage = library::search_model::active();
                        let enc = library::search_model::QueryEncoder::load(stage)?;
                        let (mut image, text) = library::search_model::load_vectors(&db, stage)?;
                        if let Some(n) = crate::env::var("GRAINEO_SEM_FAKE").ok().and_then(|v| v.parse::<usize>().ok()) {
                            image.pad_for_test(n);
                        }
                        Ok(std::sync::Arc::new(SemState { enc, image, text }))
                    })();
                    *target.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                });
            }
            return;
        };
        match self.sem_view.pause_left() {
            Some(left) => ctx.request_repaint_after(left),
            None => self.sem_view.start(z, text, ctx.clone()),
        }
    }

    /// Per frame: take over finished semantics, image/text vectors for new photos in the background.
    fn semantic_maintenance(&mut self, ctx: &egui::Context) {
        self.semantic_poll(ctx);
        if self.sem_loading && self.semantic.try_lock().is_ok_and(|g| g.is_some()) {
            self.sem_loading = false;
            self.sem_for.clear();
            self.refilter = true;
        }
        if self.sem_loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        // search model stage (prefs_search_model.rs): reload after a switch; read the photos in for a new stage
        // right away (the user chose it), for new photos of an active SigLIP stage with the normal round below
        let round = library::search_model::round();
        if round != self.search_model_round {
            if self.search_model_round != u64::MAX && !self.sem_loading {
                *self.semantic.lock().unwrap_or_else(|e| e.into_inner()) = None;
                // the running query belongs to the old model: ask again with the new one
                self.sem_view.clear();
                self.sem_for.clear();
                self.refilter = true;
            }
            self.search_model_round = round;
            if library::search_model::target().is_some() {
                library::search_model::start(|| {});
            }
        }
        // text model switched in the settings (M-CLIP): reload encoder and description vectors, and
        // compute the descriptions of the new model right away
        let round = library::semantic::text_model_round();
        if round != self.text_model_round {
            self.text_model_round = round;
            if !self.sem_loading {
                *self.semantic.lock().unwrap_or_else(|e| e.into_inner()) = None;
                self.sem_view.clear();
                self.sem_for.clear();
                self.refilter = true;
            }
            self.vector_round = None;
        }
        // vectors for photos without a vector (image encoder) or AI descriptions without a text vector
        let due = self.vector_round.is_none_or(|t| t.elapsed().as_secs() > 600);
        let on = library::settings::value("imageEncoderBackground").as_bool().unwrap_or(true);
        if due && on && !self.vector_job.load(std::sync::atomic::Ordering::Relaxed) && crate::env::var_os("GRAINEO_LIBRARY_NO_SYNC").is_none() {
            self.vector_round = Some(std::time::Instant::now());
            let (image_dir, text_model) = (library::semantic::image_folder(), library::semantic::description_model());
            if image_dir.is_none() && text_model.is_none() {
                return;
            }
            let run = self.vector_job.clone();
            run.store(true, std::sync::atomic::Ordering::Relaxed);
            let sem = self.semantic.clone();
            std::thread::spawn(move || {
                // low priority (efficiency cores)
                library::platform::background_priority();
                let n = backfill_vectors(image_dir, text_model);
                // vectors of an active SigLIP stage for new photos (library::search_model)
                library::search_model::start(|| {});
                if n > 0 {
                    // re-read the loaded vectors on the next search
                    *sem.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
                crate::log(&format!("vectors: {n} recomputed"));
                run.store(false, std::sync::atomic::Ordering::Relaxed);
            });
        }
    }
}

/// Compute missing image and description vectors (throttled). Returns: count.
fn backfill_vectors(image_dir: Option<PathBuf>, text_model: Option<library::semantic::TextModel>) -> usize {
    let Ok(db) = Db::standard() else { return 0 };
    let mut n = 0;
    if let Some(d) = image_dir {
        if let Ok(enc) = library::semantic::ImageEncoder::load(&d) {
            loop {
                let list = library::similar::without_vector(&db, 32);
                if list.is_empty() {
                    break;
                }
                for (file, path, format) in list {
                    let v = library::preview::load(&path, &format, 512).ok().map(|(img, _)| enc.embed(&img));
                    // an entry even on error, so the file isn't tried again and again
                    let v = v.unwrap_or_else(|| vec![0.0; 512]);
                    let _ = library::similar::save(&db, file, &v);
                    n += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
    }
    if let Some(model) = text_model {
        // description vectors (standard model if installed; crates/library: semantic_backfill)
        n += library::semantic_backfill::descriptions(&db, model);
    }
    n
}

/// Name of the volume of a path under `/Volumes/NAME/…` (otherwise `None`, e.g. startup volume).
fn volume_name(p: &std::path::Path) -> Option<String> {
    let mut c = p.components();
    c.next()?; // "/"
    match (c.next()?.as_os_str().to_str()?, c.next()?.as_os_str().to_str()?) {
        ("Volumes", v) => Some(v.to_string()),
        _ => None,
    }
}

/// Remember the source for the next start (`settings.json`, key "librarySource").
fn remember_source(q: &Source) {
    let t = match q {
        Source::All => "all".to_string(),
        Source::Recent => "recent".into(),
        Source::Missing => "missing".into(),
        Source::Offline => "offline".into(),
        Source::Folder(s, p) => format!("folder:{s}:{p}"),
        Source::Collection(id) => format!("collection:{id}"),
        Source::Quick => "quick".into(),
        Source::Keyword(id) => format!("keyword:{id}"),
        Source::Smart(id) => format!("smart:{id}"),
        Source::Browse(p) => format!("browse:{}", p.display()),
        Source::NotInCatalog => "uncatalogued".into(),
        Source::Date(d) => format!("date:{d}"),
        Source::Similar(_) | Source::Duplicates => return,
    };
    library::settings::set("librarySource", serde_json::json!(t));
}

fn read_source() -> Option<Source> {
    let v = library::settings::value("librarySource");
    let t = v.as_str()?;
    let (k, r) = t.split_once(':').unwrap_or((t, ""));
    Some(match k {
        "all" => Source::All,
        "recent" => Source::Recent,
        "missing" => Source::Missing,
        "offline" => Source::Offline,
        "uncatalogued" => Source::NotInCatalog,
        "quick" => Source::Quick,
        "folder" => {
            let (s, p) = r.split_once(':')?;
            Source::Folder(s.parse().ok()?, p.to_string())
        }
        "collection" => Source::Collection(r.parse().ok()?),
        "keyword" => Source::Keyword(r.parse().ok()?),
        "smart" => Source::Smart(r.parse().ok()?),
        "browse" => Source::Browse(PathBuf::from(r)),
        "date" => Source::Date(r.to_string()),
        _ => return None,
    })
}

// ───────────────────────────────────────────── Shared image source (Develop)

impl LibraryView {
    /// Version of the visible list; when it changes, Develop rebuilds the filmstrip.
    /// Flag filter (bits picked/unflagged/rejected, 0 = all) – also usable above the filmstrip.
    pub fn flag_filter(&self) -> u8 {
        self.filter.flags
    }

    /// Toggle one bit of the flag filter (0 = picked, 1 = unflagged, 2 = rejected).
    pub fn toggle_flag_filter(&mut self, n: usize) {
        self.filter.flags ^= [FLAG_PICKED, FLAG_NONE, FLAG_REJECTED][n.min(2)];
        self.refilter = true;
    }

    pub fn list_version(&self) -> u64 {
        self.list_version
    }

    /// The visible list (source, search, column filters, sort order) for the filmstrip.
    pub fn list(&self) -> Vec<SourceImage> {
        self.visible
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .map(|z| {
                SourceImage {
                    image: z.image,
                    file: z.file,
                    path: self.path_of(z),
                    name: display_name(z),
                    copy: z.copy.clone(),
                    format: z.format.clone(),
                    state: z.status,
                    stars: z.stars,
                    flag: z.flag,
                    label: z.label,
                    aspect: crate::filmstrip_layout::aspect(z.width, z.height, z.orientation),
                }
            })
            .collect()
    }

    /// Name of the source (for the header of the filmstrip).
    pub fn source_title(&self) -> String {
        self.source_name()
    }

    /// Selection and scroll position belong to the grid, not to the filmstrip.
    pub fn remember_grid(&mut self) {
        let anchor_dy = self.active.and_then(|a| self.visible.iter().position(|&i| self.rows[i].image == a)).filter(|_| self.grid_cell > 0.0)
            .map(|pos| (pos / self.columns.max(1)) as f32 * self.grid_cell - self.grid_y)
            // only if the photo was fully in view; otherwise the grid scrolls it into view on return
            .filter(|&dy| dy >= -1.0 && dy + self.grid_cell <= self.grid_view_h + 1.0);
        self.grid_remembered = Some(GridState { source: self.source.clone(), active: self.active, selection: self.selection.clone(), y: self.grid_y, anchor_dy });
    }
    /// Back to the grid: the photo last shown in single-image view (`self.active`) stays active. If it was
    /// the same as when leaving the grid, the selection and scroll position from back then apply; otherwise
    /// the grid scrolls to it (instead of jumping to the photo from back then or to the start).
    pub fn restore_grid(&mut self) {
        let visible: HashSet<_> = self.visible.iter().map(|&i| self.rows[i].image).collect();
        let now = self.active.filter(|a| visible.contains(a));
        let Some(z) = self.grid_remembered.take().filter(|z| z.source == self.source) else {
            self.grid_back_y = None;
            self.grid_anchor = None;
            self.to_active = now.is_some();
            return;
        };
        match now {
            Some(a) if z.active != Some(a) => {
                self.selection.clear();
                self.to_active = true;
                self.grid_back_y = None;
                self.grid_anchor = None;
            }
            _ => {
                self.selection = z.selection.intersection(&visible).copied().collect();
                self.active = now.or(z.active.filter(|a| visible.contains(a))).or_else(|| self.visible.first().map(|&i| self.rows[i].image));
                self.grid_y = z.y;
                self.grid_back_y = Some(z.y);
                self.grid_anchor = match (self.active, z.anchor_dy) {
                    (Some(a), Some(dy)) if z.active == Some(a) => Some((a, dy, 30)),
                    _ => None,
                };
                self.to_active = self.grid_anchor.is_none();
            }
        }
    }
    pub fn active_image(&self) -> Option<i64> {
        self.active
    }
    /// tests: the selected photos as the grid shows them (active one included; empty when nothing is selected)
    #[cfg(test)]
    pub(crate) fn selected_for_test(&self) -> HashSet<i64> {
        if self.nothing_selected { HashSet::new() } else { self.selected_ids() }
    }
    /// Should the grid jump to the active photo right away (e.g. "Show in folder")?
    pub fn open_to_active(&self) -> bool {
        self.to_active
    }

    /// Set the active image from outside (Develop pages through) – the grid scrolls along.
    /// Create a virtual copy of `image` in the background; then reload the list and report `Action::CopyCreated`.
    /// `false`: no library.
    pub fn create_copy_for(&mut self, image: i64) -> bool {
        self.create_copy_named(image, None)
    }

    /// [`Self::create_copy_for`] with a name for the copy (taken if free, else "Kopie N").
    pub fn create_copy_named(&mut self, image: i64, name: Option<String>) -> bool {
        let ticket = self.load_ticket();
        self.in_background(
            move |db| library::sync::create_copy(db, image, name.as_deref()).map(|c| (c, ticket.load(db).ok())),
            move |b, actions, r| {
                let r = match r {
                    Ok((c, loaded)) => {
                        b.adopt_or_reload(loaded);
                        Ok(c)
                    }
                    Err(e) => Err(e),
                };
                actions.push(Action::CopyCreated(image, r));
            },
        )
    }

    /// Write operation in the background (the UI never waits for the database or for files); `finished`
    /// runs in the next frame afterwards. `false`: no library open.
    fn in_background<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&Db) -> Result<R, String> + Send + 'static,
        finished: impl FnOnce(&mut LibraryView, &mut Vec<Action>, Result<R, String>) + Send + 'static,
    ) -> bool {
        let Some(path) = self.db.as_ref().map(|d| d.path.clone()) else { return false };
        self.db_writer.task(path, work, finished);
        true
    }

    /// Re-read the keyword list (after a change), update facets and filters.
    fn reload_keywords(&mut self) {
        let Some(db) = &self.db else { return };
        self.keywords = db.keywords().unwrap_or_default();
        if let Some(g) = self.take_facets_state() {
            if let Some(path) = self.db.as_ref().map(|d| d.path.clone()) {
                self.start_facets(g, library_facets_job::Assignments::FromDb(path));
            }
        }
        self.query_for = None;
        self.refilter = true;
    }

    pub fn set_active(&mut self, image: i64) {
        self.nothing_selected = false;
        if self.active != Some(image) {
            self.sel_ext = None;
            self.active = Some(image);
            self.selection.clear();
            self.to_active = true;
        }
    }

    /// Remember the active image for the next start (only on change).
    fn remember_active(&mut self) {
        if self.active.is_some() && self.active != self.remembered_active {
            self.remembered_active = self.active;
            library::settings::set("libraryActive", serde_json::json!(self.active));
        }
    }

    /// Preview cache and keys of the given files (for a background thread reading stand-in images); `None` without cache.
    pub fn preview_source(&self, files: &[i64]) -> Option<(std::sync::Arc<preview::Cache>, Vec<Option<u64>>)> {
        let e = self.generator.as_ref()?;
        let keys = files.iter().map(|f| self.rows.iter().position(|z| z.file == *f).map(|i| self.key_at(i))).collect();
        Some((e.cache().clone(), keys))
    }

    /// Thumbnail of an image (from the library's preview cache), if already loaded.
    pub fn thumbnail(&mut self, file: i64) -> Option<TextureHandle> {
        self.texture(file, preview::small())
    }

    /// After editing: the developed view of a photo becomes its preview in the grid and in the
    /// filmstrip, immediately (textures) and permanently (developed grid preview as with the stack;
    /// the old entry in the preview store is removed afterwards so the old image doesn't
    /// appear again after a restart).
    /// `large_current`: the large preview (offline copy or developed large level) already shows exactly this development
    /// (fingerprint) – it stays. Before, every look at a photo threw it away and it was developed again at full
    /// resolution (3–10 s each time).
    pub fn store_developed(&mut self, ctx: &egui::Context, file: i64, img: image::RgbImage, persistent: bool, large_current: bool) {
        // the small level from the render (≈ 1280 px); the large level is developed separately at full resolution
        // ([`crate::large_preview`]), until then the small developed level stands in for it
        let (small_level, large_level) = (preview::small(), preview::large());
        let small = preview::downscale(img, small_level);
        for (level, image) in [(small_level, &small)] {
            let ci = egui::ColorImage::from_rgb([image.width() as usize, image.height() as usize], image.as_raw());
            self.clock += 1;
            match self.textures.get_mut(&(file, level)) {
                Some((t, u)) => {
                    t.set(ci, egui::TextureOptions::LINEAR);
                    *u = self.clock;
                }
                None => {
                    let tex = ctx.load_texture(format!("bib-{file}-{level}"), ci, egui::TextureOptions::LINEAR);
                    self.textures.insert((file, level), (tex, self.clock));
                }
            }
        }
        // the large texture shows the previous development: requested again
        if !large_current {
            self.textures.remove(&(file, large_level));
            self.requested.retain(|k| *k != (file, large_level));
        }
        #[cfg(test)]
        {
            self.last_stored = Some((file, small.pixels().map(|p| (p.0[0] as f32 + p.0[1] as f32 + p.0[2] as f32) / 3.0).sum::<f32>() / small.pixels().len().max(1) as f32));
        }
        if !persistent {
            return;
        }
        let key = self.rows.iter().find(|z| z.file == file).map(|z| self.key(z));
        let cache = self.generator.as_ref().map(|e| e.cache().clone());
        std::thread::spawn(move || {
            if preview::write_developed(file, small_level, &small) {
                // outdated: the developed large level of the previous edit, earlier small levels and every level
                // from the camera file
                // (the large level stays if it shows this development)
                let earlier: Vec<u32> = [256, 512, 1024].into_iter().filter(|&l| l != small_level).chain((!large_current).then_some(large_level)).collect();
                preview::remove_developed(file, &earlier);
                if let (Some(key), Some(cache)) = (key, cache) {
                    if large_current {
                        cache.remove(key, small_level);
                    } else {
                        cache.remove_all(key);
                    }
                }
            }
        });
    }

    /// Large preview of an image (for non-developable images in Develop); requests it.
    /// Names of all people (for suggestions when naming in the single view).
    pub fn person_names(&self) -> Vec<(i64, String)> {
        self.db.as_ref().map(|d| library::people::person_names(&d.conn)).unwrap_or_default()
    }

    /// Name a face (existing or new person); an empty name removes the name. Afterwards it counts as decided by a human.
    /// In the background; errors come as a status message.
    pub fn name_face(&mut self, face: i64, name: &str) {
        let name = name.to_string();
        self.in_background(
            move |db| {
                if name.trim().is_empty() {
                    library::people::apply_answer(db, face, library::people::Answer::RemoveName)
                } else {
                    library::people::apply_answer_with_name(db, face, &name)
                }
            },
            |b, actions, r| {
                if let Err(e) = r {
                    actions.push(Action::Status(e));
                }
                b.reload();
                actions.push(Action::FacesChanged);
            },
        );
    }

    /// Hand-drawn frame (normalised x, y, width, height on the upright image): create a face in the background;
    /// `then` gets (on the main thread) the ID of the new face.
    pub fn manual_face(&mut self, image: i64, r: [f32; 4], then: impl FnOnce(i64) + Send + 'static) {
        self.in_background(
            move |db| library::people::manual_face(db, image, r[0], r[1], r[2], r[3]),
            |_, actions, r| {
                match r {
                    Ok(id) => then(id),
                    Err(e) => actions.push(Action::Status(e)),
                }
                actions.push(Action::FacesChanged);
            },
        );
    }

    pub fn discard_manual_face(&mut self, face: i64) {
        self.in_background(move |db| library::people::discard_manual_face(db, face), |_, actions, _| actions.push(Action::FacesChanged));
    }

    /// Has the photo already been searched for faces (1), not yet (0), or did the search fail (2)?
    pub fn faces_version(&self, image: i64) -> i64 {
        self.db.as_ref().map(|d| library::people::search_state(&d.conn, image)).unwrap_or(1)
    }

    /// Faces of a photo (frames normalised to the upright image) for the single view.
    pub fn faces_of(&self, image: i64) -> Vec<library::people::FaceInfo> {
        self.db.as_ref().map(|d| library::people::faces_to_image(&d.conn, image)).unwrap_or_default()
    }

    pub fn large_view(&mut self, image: i64) -> Option<TextureHandle> {
        let small = self.large_ready(image)?;
        Some(small)
    }

    /// Large preview of an image if it is already loaded; otherwise it is requested (before everything else) and
    /// the best texture present so far is returned. `bool`: it is the large level.
    pub fn large_preview(&mut self, image: i64) -> Option<(TextureHandle, bool)> {
        let &i = self.index.get(&image)?;
        let z = self.rows[i].clone();
        let large = preview::large();
        if let Some(t) = self.textures.get_mut(&(z.file, large)) {
            self.clock += 1;
            t.1 = self.clock;
            return Some((t.0.clone(), true));
        }
        // offline copy only for photos on other drives; on this Mac the small one stands in until the original is loaded
        if !self.generator.as_ref().is_some_and(|e| e.cache().is_protected(self.key(&z))) {
            return self.texture(z.file, preview::small()).map(|t| (t, false));
        }
        if self.large_requested != Some(z.file) {
            if let Some(e) = &self.generator {
                e.urgent(self.task(&z, large));
            }
            self.large_requested = Some(z.file);
        }
        self.texture(z.file, preview::small()).map(|t| (t, false))
    }

    /// Check for [`crate::large_preview::schedule`]: does the offline copy of this file in the preview store show the
    /// development (not the camera JPEG)? Reads the start of the file only when called.
    /// Preview store and the preview jobs (small level) of these files – for the one-off check of the developed previews
    /// ([`crate::preview_repair`]). `None` while the preview store is not open yet.
    pub fn preview_check_jobs(&self, files: &HashSet<i64>) -> Option<(Arc<preview::Cache>, Vec<preview::Job>)> {
        // only once the library has been read (in the background): before that the rows are empty
        if self.db.is_none() || self.first_loading() || self.reloading() {
            return None;
        }
        let cache = self.generator.as_ref()?.cache().clone();
        let jobs = self.rows.iter().filter(|z| z.copy.is_none() && z.folder != VIEW_FOLDER && files.contains(&z.file)).map(|z| self.task(z, preview::small())).collect();
        Some((cache, jobs))
    }

    /// Photos that may need a new preview (see `sigma_refresh`): X3F of the Sigma cameras affected by the changes of
    /// 0.95.0, and the other RAWs (camera look no longer automatic), originals only. `None` until the library has been
    /// read.
    pub fn sigma_refresh_candidates(&self) -> Option<(Arc<preview::Cache>, Vec<crate::sigma_refresh::Candidate>)> {
        if self.db.is_none() || self.first_loading() || self.reloading() {
            return None;
        }
        let cache = self.generator.as_ref()?.cache().clone();
        let mut cands: Vec<crate::sigma_refresh::Candidate> = self
            .rows
            .iter()
            .filter(|z| z.copy.is_none() && z.folder != VIEW_FOLDER && library::is_raw(&z.format))
            .filter_map(|z| {
                let group = if z.format == "X3F" { crate::sigma_refresh::group_of(z.camera.as_deref()?, z.iso)? } else { crate::sigma_refresh::Group::CameraLook };
                Some(crate::sigma_refresh::Candidate { image: z.image, file: z.file, path: self.path_of(z), key: self.key(z), group, camera: z.camera.clone().unwrap_or_default() })
            })
            .collect();
        // film grain (any format): which ones are affected is decided from their settings
        cands.extend(self.rows.iter().filter(|z| z.copy.is_none() && z.folder != VIEW_FOLDER).map(|z| crate::sigma_refresh::Candidate {
            image: z.image,
            file: z.file,
            path: self.path_of(z),
            key: self.key(z),
            group: crate::sigma_refresh::Group::FilmGrain,
            camera: z.camera.clone().unwrap_or_default(),
        }));
        Some((cache, cands))
    }

    pub fn offline_developed(&self, file: i64) -> impl FnOnce() -> bool + use<> {
        let job = self.rows.iter().find(|z| z.file == file).map(|z| (self.key(z), preview::large()));
        let cache = self.generator.as_ref().map(|e| e.cache().clone());
        move || match (job, cache) {
            (Some((key, level)), Some(c)) => c.has(key, level) && c.is_developed(key, level),
            _ => false,
        }
    }

    /// Decode the large previews (offline copies) of these images ahead, while paging in the single view: the next
    /// images then appear sharp at once. Only photos with an offline copy; replaces the previous request.
    pub fn prefetch_large(&mut self, images: &[i64]) {
        let large = preview::large();
        let Some(e) = &self.generator else { return };
        let mut jobs = Vec::new();
        for b in images {
            let Some(&i) = self.index.get(b) else { continue };
            let z = &self.rows[i];
            let present = e.cache().has(self.key(z), large) || preview::has_developed(z.file, large);
            // only offline copies that exist (never decode an original on the NAS just for paging)
            if self.textures.contains_key(&(z.file, large)) || !e.cache().is_protected(self.key(z)) || !present {
                continue;
            }
            jobs.push(self.task(z, large));
        }
        e.ahead(jobs);
    }

    fn large_ready(&mut self, image: i64) -> Option<TextureHandle> {
        self.large_preview(image).map(|(t, _)| t)
    }

    /// Request thumbnails for these images (visible part of the filmstrip).
    pub fn request_thumbnails(&mut self, images: &[i64]) {
        let mut wanted = Vec::new();
        let mut jobs = Vec::new();
        for b in images {
            let Some(&i) = self.index.get(b) else { continue };
            let z = &self.rows[i];
            let k = (z.file, preview::small());
            if self.textures.contains_key(&k) || wanted.contains(&k) {
                continue;
            }
            if z.status != 0 && !self.generator.as_ref().is_some_and(|e| e.cache().has(self.key(z), k.1)) {
                continue;
            }
            wanted.push(k);
            jobs.push(self.task(z, k.1));
        }
        if !wanted.is_empty() && wanted != self.requested {
            if let Some(e) = &self.generator {
                e.visible(jobs);
            }
            self.requested = wanted;
        }
    }

    /// View folder (⌘O): if it is in the library, its folder source; otherwise a
    /// temporary source with the folder's images (not added to the library).
    /// `active`: activate this image.
    pub fn browse_folder(&mut self, dir: &std::path::Path, active: Option<&std::path::Path>) {
        let target = format!("{}/", library::nfc(&dir.to_string_lossy()).trim_end_matches('/'));
        let known = self.folder.iter().find(|o| self.folder_paths.get(&o.id).is_some_and(|p| library::nfc(p) == target)).map(|o| (o.root, o.path.clone()));
        match known {
            Some((s, p)) => {
                self.set_source(Source::Folder(s, p));
            }
            None => {
                // with subfolders: names are paths relative to the folder
                let listing = crate::view_folder::list(dir);
                let names = listing.names.clone();
                self.viewed_listing = Some(listing);
                // rating and virtual copies from the sidecar (the truth), read only
                let raw_stems: HashSet<String> = names
                    .iter()
                    .filter(|n| library::format_from_extension(n).is_some_and(library::is_raw))
                    .filter_map(|n| n.rsplit_once('.').map(|(s, _)| s.to_lowercase()))
                    .collect();
                let mut rows: Vec<Row> = Vec::new();
                for n in names {
                    let Some(f) = library::format_from_extension(&n) else { continue };
                    let path = dir.join(&n);
                    let raw_beside = !library::is_raw(f) && n.rsplit_once('.').is_some_and(|(s, _)| raw_stems.contains(&s.to_lowercase()));
                    let values = match library::sidecar::read(&path, f, raw_beside).0 {
                        library::xmp::ReadResult::Ok(w) => w,
                        _ => library::xmp::Values::default(),
                    };
                    let m = std::fs::metadata(&path).ok();
                    let basis = Row {
                        image: 0,
                        file: 0,
                        folder: VIEW_FOLDER,
                        name: n.clone(),
                        format: f.to_string(),
                        copy: None,
                        stars: values.stars,
                        flag: values.flag,
                        label: values.label,
                        status: 0,
                        camera: None,
                        capture: None,
                        run: None,
                        size: m.as_ref().map(|m| m.len() as i64),
                        mtime: m.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64),
                        width: None,
                        height: None,
                        orientation: None,
                        lens: None,
                        aperture: None,
                        focal: None,
                        iso: None,
                        time: None,
                        ai_quality: None,
                        changed: None,
                        imported: None,
                        from_lightroom: false,
                        sidecar_jpeg: None,
                        jpeg_for: None,
                    };
                    let file = view_id();
                    rows.push(Row { image: file, file: file, ..basis.clone() });
                    for k in library::sidecar::read_copies(values.copies_json.as_deref()) {
                        let id = view_id();
                        rows.push(Row { image: id, file: file, copy: Some(k.name), stars: k.stars, flag: k.flag, label: k.label, ..basis.clone() });
                    }
                }
                library::db::mark_pairs(&mut rows);
                self.insert_viewed(dir, rows);
                self.source = Source::All; // forces the switch below
                self.set_source(Source::Browse(dir.to_path_buf()));
                // search and column filters apply to the library: in a foreign folder they would
                // silently hide everything (no keywords, no capture dates in the database)
                if self.filter != Filter::default() || self.facets.as_ref().is_some_and(|f| f.active()) {
                    crate::log("view folder: filter reset");
                    self.filter = Filter::default();
                    if let Some(f) = &mut self.facets {
                        f.reset();
                    }
                }
                self.refilter = true;
            }
        }
        self.apply_filter();
        if let Some(a) = active {
            let target = library::nfc(&a.to_string_lossy());
            // a stack's camera JPEG also finds its RAW
            let same = |z: &Row| library::nfc(&self.path_of(z).to_string_lossy()) == target;
            let matches = self.visible.iter().map(|&i| &self.rows[i]).find(|z| same(z) || z.sidecar_jpeg.and_then(|j| self.index.get(&j)).is_some_and(|&j| same(&self.rows[j]))).map(|z| z.image);
            if let Some(b) = matches {
                self.set_active(b);
            }
        }
        // activate the first image if the active one is not in the list
        if !self.active.is_some_and(|a| self.visible.iter().any(|&i| self.rows[i].image == a)) {
            if let Some(&i) = self.visible.first() {
                let b = self.rows[i].image;
                self.set_active(b);
            }
        }
        crate::log(&format!("view folder {}: {} files, {} visible", dir.display(), self.viewed.as_ref().map_or(0, |a| a.1.len()), self.visible.len()));
    }

    /// Swap the rows of "View folder" without re-reading the library (that took half a second
    /// in the foreground for large libraries).
    fn insert_viewed(&mut self, dir: &std::path::Path, rows: Vec<Row>) {
        let all = Arc::make_mut(&mut self.rows);
        all.retain(|z| z.folder != VIEW_FOLDER);
        all.extend(rows.iter().cloned());
        Arc::make_mut(&mut self.folder_paths).insert(VIEW_FOLDER, format!("{}/", dir.display()));
        self.index = self.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
        self.viewed = Some((dir.to_path_buf(), rows));
        // column filters compute per row → rebuild (selection is kept via the names)
        if let Some(g) = self.take_facets_state() {
            if let Some(path) = self.db.as_ref().map(|d| d.path.clone()) {
                self.start_facets(g, library_facets_job::Assignments::FromDb(path));
            }
        }
        self.query_for = None;
        self.refilter = true;
    }

    /// At startup: restore the last selected source and active image.
    /// A source that no longer exists (folder, collection or keyword removed in the meantime) falls back to "All photos";
    /// a remembered folder is expanded in the tree so that it can be seen as selected.
    fn restore_source(&mut self) {
        match read_source() {
            Some(Source::Browse(p)) if p.is_dir() => self.browse_folder(&p, None),
            Some(Source::Browse(_)) | None => {}
            Some(q) => {
                let q = self.existing_source(q);
                if let Source::Folder(r, p) = &q {
                    let mut part = String::new();
                    for t in p.split('/').filter(|t| !t.is_empty()) {
                        self.open_folders.insert((*r, part.clone()));
                        part.push_str(t);
                        part.push('/');
                    }
                    self.scroll_to_folder = Some((*r, p.clone()));
                }
                if let Source::Collection(id) | Source::Smart(id) = &q {
                    self.open_parents_of(*id);
                }
                self.source = q;
                self.source_changed();
            }
        }
        // the photo only if it still exists in the library (otherwise the grid starts at the top)
        if let Some(b) = library::settings::value("libraryActive").as_i64().filter(|b| self.index.contains_key(b)) {
            self.active = Some(b);
            self.remembered_active = Some(b);
            self.to_active = true;
        }
    }

    /// `q` if it still exists in the library, otherwise "All photos".
    fn existing_source(&self, q: Source) -> Source {
        let exists = match &q {
            Source::Folder(r, p) => self.roots.iter().any(|x| x.id == *r) && (p.is_empty() || self.folder.iter().any(|o| o.root == *r && o.path == *p)),
            Source::Collection(id) | Source::Smart(id) => self.collections.iter().any(|x| x.id == *id),
            Source::Keyword(id) => self.db.as_ref().is_some_and(|db| db.images_with_keyword(*id).is_ok_and(|v| !v.is_empty())),
            _ => true,
        };
        if exists {
            q
        } else {
            crate::log("library: remembered source no longer exists, showing all photos");
            Source::All
        }
    }
}



#[cfg(test)]
mod media_library_tests {
    use super::*;
    #[test]
    fn arrow_extension_keeps_main_photo_and_steps_from_head() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-selection-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..6 {
            image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(dir.join(format!("image-{i}.png"))).unwrap();
        }
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        library.browse_folder(&dir, None);
        let ids: Vec<_> = library.visible.iter().map(|&i| library.rows[i].image).collect();
        library.active = Some(ids[2]);
        library.nothing_selected = false;
        library.selection.clear();
        library.extend_external(1);
        library.extend_external(1);
        assert_eq!(library.active, Some(ids[2]));
        assert_eq!(library.selected_ids(), ids[2..=4].iter().copied().collect());
        assert_eq!(library.sel_ext.as_ref().map(|e| e.head), Some(ids[4]));
        library.extend_external(-1);
        assert_eq!(library.active, Some(ids[2]));
        assert_eq!(library.selected_ids(), ids[2..=3].iter().copied().collect());
        library.step_external(1);
        assert_eq!(library.active, Some(ids[4]));
        assert_eq!(library.selected_ids(), HashSet::from([ids[4]]));
        assert!(library.sel_ext.is_none());
        library.click(ids[1], false, true);
        assert_eq!(library.active, Some(ids[4]));
        assert_eq!(library.selected_ids(), ids[1..=4].iter().copied().collect());
    }

    #[test]
    fn shift_drag_on_a_photo_adds_to_selection() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-band-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(dir.join(format!("image-{i}.png"))).unwrap();
        }
        let ctx = egui::Context::default();
        let mut library = LibraryView::new(&ctx);
        library.browse_folder(&dir, None);
        let ids: Vec<_> = library.visible.iter().map(|&i| library.rows[i].image).collect();
        assert_eq!(ids.len(), 3);
        library.active = Some(ids[2]);
        library.nothing_selected = false;
        library.selection.clear();
        let geometry = crate::library_select::GridGeometry { columns: 4, first: 3, total: 3, separated: false, cell: 100.0, pitch: 100.0, sep_pitch: 100.0 };
        let view = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        let mut frame = |time, mut events: Vec<egui::Event>, modifiers| {
            events.insert(0, egui::Event::ModifiersChanged(modifiers));
            let input = egui::RawInput { time: Some(time), screen_rect: Some(view), events, ..Default::default() };
            let _ = crate::ui_test::run(&ctx, input, |ui| { library.band_update(ui, view, geometry); });
        };
        let start = egui::pos2(10.0, 50.0);
        let end = egui::pos2(190.0, 70.0);
        frame(0.0, vec![egui::Event::PointerMoved(start)], shift);
        frame(0.1, vec![egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: shift }], shift);
        frame(0.2, vec![egui::Event::PointerMoved(end)], shift);
        frame(0.3, vec![egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: shift }], shift);
        drop(frame);
        assert_eq!(library.active, Some(ids[2]));
        assert_eq!(library.selected_ids(), ids.iter().copied().collect());
        let input = egui::RawInput {
            time: Some(0.5), screen_rect: Some(view),
            events: vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE), egui::Event::PointerMoved(start), egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }],
            ..Default::default()
        };
        let _ = crate::ui_test::run(&ctx, input, |ui| { library.band_update(ui, view, geometry); });
        assert!(library.band.is_none(), "ordinary drag on a photo stays available for drag and drop");
    }

    #[test]
    fn grid_shift_drag_selects_and_plain_drag_keeps_payload() {
        assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
        let dir = std::env::temp_dir().join(format!("graineo-drag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80])).save(dir.join(format!("image-{i}.png"))).unwrap();
        }
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut library = LibraryView::new(&ctx);
        library.browse_folder(&dir, None);
        library.centre_width = 580.0;
        library.cell_size = 140.0;
        let ids: Vec<_> = library.visible.iter().map(|&i| library.rows[i].image).collect();
        library.active = Some(ids[2]);
        library.nothing_selected = false;
        library.selection.clear();
        let tex = ctx.load_texture("drag-test", egui::ColorImage::filled([24, 16], Color32::GRAY), Default::default());
        let file = library.rows[library.visible[0]].file;
        library.textures.insert((file, preview::small()), (tex.clone(), 0));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0));
        let mut frame = |time, events| {
            let input = egui::RawInput { time: Some(time), screen_rect: Some(screen), events, ..Default::default() };
            let mut actions = Vec::new();
            crate::ui_test::run(&ctx, input, |root| { egui::CentralPanel::default().show(root, |ui| library.raster(ui, &mut actions)); })
        };
        let output = frame(0.0, vec![]);
        let rect = output.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Mesh(m) if m.texture_id == tex.id() => Some(egui::Rect::from_min_max(
                egui::pos2(m.vertices.iter().map(|v| v.pos.x).fold(f32::INFINITY, f32::min), m.vertices.iter().map(|v| v.pos.y).fold(f32::INFINITY, f32::min)),
                egui::pos2(m.vertices.iter().map(|v| v.pos.x).fold(f32::NEG_INFINITY, f32::max), m.vertices.iter().map(|v| v.pos.y).fold(f32::NEG_INFINITY, f32::max)),
            )),
            _ => None,
        }).expect("first synthetic photo is visible");
        let start = rect.center();
        let end = start + egui::vec2(145.0, 12.0);
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        frame(0.1, vec![egui::Event::ModifiersChanged(shift), egui::Event::PointerMoved(start)]);
        frame(0.2, vec![egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: shift }]);
        frame(0.3, vec![egui::Event::PointerMoved(end)]);
        assert!(!egui::DragAndDrop::has_any_payload(&ctx), "Shift drag must not start photo drag and drop");
        frame(0.4, vec![egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: shift }]);
        frame(0.5, vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE), egui::Event::PointerMoved(start)]);
        frame(0.6, vec![egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        frame(0.7, vec![egui::Event::PointerMoved(end)]);
        assert!(egui::DragAndDrop::has_payload_of_type::<PhotoDrag>(&ctx), "plain drag must keep photo drag and drop");
        frame(0.8, vec![egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
        drop(frame);
        assert_eq!(library.active, Some(ids[2]));
        assert_eq!(library.selected_ids(), ids.into_iter().collect());
    }

    #[test]
    fn grid_return_and_clickable_header() {
        assert!(
            crate::env::var_os("GRAINEO_DATA_DIR").is_some(),
            "UI tests need their own data folder"
        );
        let dir = std::env::temp_dir().join(format!("graineo-grid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..12 {
            image::RgbImage::from_pixel(24, 16, image::Rgb([60, 100, 80]))
                .save(dir.join(format!("image-{i:02}.png")))
                .unwrap();
        }
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut b = LibraryView::new(&ctx);
        b.browse_folder(&dir, None);
        let ids: Vec<_> = b.visible.iter().map(|&i| b.rows[i].image).collect();
        assert_eq!(ids.len(), 12);
        b.active = Some(ids[3]);
        b.selection = HashSet::from([ids[2], ids[3]]);
        b.grid_y = 310.0;
        b.remember_grid();
        // paged on in single-image view: the photo last shown stays active, the grid scrolls there
        b.set_active(ids[5]);
        b.grid_y = 0.0;
        b.restore_grid();
        assert_eq!(b.active, Some(ids[5]));
        assert!(b.selection.is_empty());
        assert_eq!(b.grid_back_y, None);
        assert!(b.to_active);
        // stayed on the same photo: selection and scroll position from back then
        b.active = Some(ids[3]);
        b.selection = HashSet::from([ids[2], ids[3]]);
        b.grid_y = 310.0;
        b.remember_grid();
        b.grid_y = 0.0;
        b.restore_grid();
        assert_eq!(b.active, Some(ids[3]));
        assert_eq!(b.selection, HashSet::from([ids[2], ids[3]]));
        assert_eq!(b.grid_back_y, Some(310.0));
        // the active photo must be in view afterwards: the check stays on (only applies if it isn't visible)
        assert!(b.to_active);
        b.remember_grid();
        b.source = Source::Quick;
        b.active = Some(ids[0]);
        b.restore_grid();
        assert_eq!(
            b.active,
            Some(ids[0]),
            "Neue Quelle darf nicht überschrieben werden"
        );
        b.source = Source::Browse(dir.clone());
        b.selection.clear();
        b.grid_y = 0.0;
        b.grid_back_y = Some(0.0);
        // A real texture makes it testable that the icons lie fully above the photo.
        let texture = ctx.load_texture(
            "synthetic-grid-photo",
            egui::ColorImage::filled([24, 16], Color32::from_gray(150)),
            Default::default(),
        );
        let ti = b.visible[0];
        let file = b.rows[ti].file;
        b.textures
            .insert((file, preview::small()), (texture.clone(), 0));
        b.quick.insert(ids[0]);
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 440.0));
        let mut run = |time, events| {
            let mut actions = Vec::new();
            let out = crate::ui_test::run(&ctx,
                egui::RawInput {
                    screen_rect: Some(rect),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| b.raster(ui, &mut actions));
                },
            );
            (out, actions)
        };
        let (frame, _) = run(0.0, vec![]);
        let circle = frame
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Circle(c) if c.radius == 6.0 => Some(c.center),
                _ => None,
            })
            .unwrap();
        let photo_top = frame
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(m) if m.texture_id == texture.id() => Some(
                    m.vertices
                        .iter()
                        .map(|v| v.pos.y)
                        .fold(f32::INFINITY, f32::min),
                ),
                _ => None,
            })
            .unwrap();
        assert!(
            circle.y + 10.0 < photo_top,
            "quick collection must not lie on the photo"
        );
        let event = |pressed| egui::Event::PointerButton {
            pos: circle,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        run(1.0, vec![egui::Event::PointerMoved(circle), event(true)]);
        let (_, a) = run(1.02, vec![event(false)]);
        assert!(a
            .iter()
            .all(|a| !matches!(a, Action::Develop | Action::Selection)));
        run(1.1, vec![event(true)]);
        let (_, a) = run(1.12, vec![event(false)]);
        assert!(
            a.iter()
                .all(|a| !matches!(a, Action::Develop | Action::Selection)),
            "double-click on the circle must not open the photo"
        );
        drop(run);
        assert!(
            b.quick.contains(&ids[0]),
            "two clicks toggle off and on again"
        );
        // After viewing another image, the real ScrollArea returns so that the active photo is in view.
        b.active = Some(ids[ids.len() - 1]);
        b.grid_y = 310.0;
        b.remember_grid();
        b.grid_y = 0.0;
        b.restore_grid();
        let _ = crate::ui_test::run(&ctx,
            egui::RawInput {
                screen_rect: Some(rect),
                time: Some(2.0),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| b.raster(ui, &mut Vec::new()));
            },
        );
        // the last photo lies below the remembered viewport: the grid keeps scrolling to it instead of stopping at 310
        assert!(b.grid_y > 310.0, "raster_y = {}", b.grid_y);
        // If the active photo is not in view at the remembered position (the grid has changed),
        // the grid scrolls to it instead of staying at the wrong place.
        b.active = Some(ids[0]);
        b.remember_grid();
        b.grid_y = 0.0;
        b.restore_grid();
        let _ = crate::ui_test::run(&ctx,
            egui::RawInput {
                screen_rect: Some(rect),
                time: Some(3.0),
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| b.raster(ui, &mut Vec::new()));
            },
        );
        assert!(b.grid_y < 310.0 - 1.0, "active photo must be in view, raster_y = {}", b.grid_y);
        // Back from single image view while the grid layout changes (other column count in the first
        // frame): the photo opened stays at the same height on screen instead of the grid jumping.
        let frame = |b: &mut LibraryView, t: f64| {
            let _ = crate::ui_test::run(&ctx,
                egui::RawInput { screen_rect: Some(rect), time: Some(t), ..Default::default() },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| b.raster(ui, &mut Vec::new()));
                },
            );
        };
        b.cell_size = 180.0;
        b.active = Some(ids[6]);
        b.grid_back_y = Some(300.0);
        frame(&mut b, 4.0);
        let (cell, columns) = (b.grid_cell, b.columns);
        let dy = (6 / columns) as f32 * cell - b.grid_y;
        b.remember_grid();
        b.cell_size = 290.0;
        b.restore_grid();
        assert!(!b.to_active, "anchor instead of scrolling to the photo");
        for k in 0..6 {
            frame(&mut b, 5.0 + k as f64 * 0.05);
        }
        let row_y = (6 / b.columns) as f32 * b.grid_cell;
        assert_ne!(b.columns, columns, "test needs a changed column count");
        assert!((row_y - b.grid_y - dy).abs() < 1.0 || b.grid_y == 0.0 && row_y < dy + 1.0, "photo moved on screen: before {dy}, now {} (grid_y {}, pitch {}, columns {} -> {})", row_y - b.grid_y, b.grid_y, b.grid_cell, columns, b.columns);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Files whose capture time "Edit capture time …" changes for these images: once per file (virtual copies share
/// the capture time), and with RAW+JPEG stacks also the stacked camera JPEG (as with the rating), so that the pair
/// keeps the same time. Returns the files and the number of photos (stacked JPEGs not counted).
fn capture_time_files<'a>(target: &[i64], row: impl Fn(i64) -> Option<&'a Row>, stack: bool) -> (Vec<i64>, usize) {
    let mut files: Vec<i64> = Vec::new();
    let mut extra: Vec<i64> = Vec::new();
    for &b in target {
        let Some(z) = row(b) else { continue };
        if !files.contains(&z.file) {
            files.push(z.file);
        }
        if stack {
            if let Some(j) = z.sidecar_jpeg.and_then(&row) {
                extra.push(j.file);
            }
        }
    }
    let photos = files.len();
    for f in extra {
        if !files.contains(&f) {
            files.push(f);
        }
    }
    (files, photos)
}

#[cfg(test)]
mod date_tests {
    #[test]
    fn count_line_follows_the_kind_filter() {
        assert_eq!(super::count_words(0).0, "Alle Fotos");
        assert_eq!(super::count_words(1).0, "Alle Videos");
        assert_eq!(super::count_words(2).0, "Alle Medien");
        assert_eq!(super::count_words(9).0, "Alle Fotos");
    }

    use super::*;

    #[test]
    fn weekday_and_month() {
        assert_eq!(tag_name("2024-05-05"), "So 5.");
        assert_eq!(tag_name("2026-11-01"), "So 1.");
        assert_eq!(tag_name("2000-02-29"), "Di 29.");
        assert_eq!(month_name("2026-09"), "September");
        assert_eq!(month_name("2026-13"), "2026-13");
    }

    fn row(image: i64, capture: Option<&str>, name: &str, copy: Option<&str>) -> Row {
        Row {
            image,
            file: image,
            folder: 1,
            name: name.into(),
            format: "JPG".into(),
            copy: copy.map(Into::into),
            stars: 0,
            flag: 0,
            label: 0,
            status: 0,
            camera: None,
            capture: capture.map(Into::into),
            run: None,
            size: None,
            mtime: None,
            width: None,
            height: None,
            orientation: None,
            lens: None,
            aperture: None,
            focal: None,
            iso: None,
            time: None,
            ai_quality: None,
            changed: None,
            imported: None,
            from_lightroom: false,
            sidecar_jpeg: None,
            jpeg_for: None,
        }
    }

    /// "Edit capture time …" on a RAW of a RAW+JPEG stack also corrects the stacked camera JPEG (as the rating does);
    /// without stacks, or for a virtual copy, only the photo's own file.
    #[test]
    fn capture_time_takes_the_stacked_jpeg_along() {
        let mut raw = row(1, Some("2024-01-01T10:00:00"), "A.CR3", None);
        raw.sidecar_jpeg = Some(2);
        let mut jpeg = row(2, Some("2024-01-01T10:00:00"), "A.JPG", None);
        jpeg.jpeg_for = Some(1);
        let mut copy = row(3, Some("2024-01-01T10:00:00"), "A.CR3", Some("Kopie 1"));
        copy.file = 1;
        copy.sidecar_jpeg = Some(2);
        let other = row(4, None, "B.JPG", None);
        let rows = [raw, jpeg, copy, other];
        let find = |b: i64| rows.iter().find(|z| z.image == b);
        assert_eq!(capture_time_files(&[1], find, true), (vec![1, 2], 1));
        assert_eq!(capture_time_files(&[1], find, false), (vec![1], 1));
        // original and virtual copy share the file: once each, still one photo
        assert_eq!(capture_time_files(&[1, 3, 4], find, true), (vec![1, 4, 2], 2));
    }

    /// The linear "newest first" gives the same order as sorting.
    #[test]
    fn newest_first_like_sorting() {
        let (a, b, c) = (Some("2024-01-01T10:00:00"), Some("2024-01-01T10:00:00"), Some("2025-03-02T08:00:00"));
        let mut rows = vec![
            row(1, a, "A.JPG", None),
            row(2, b, "B.JPG", None),
            row(3, b, "B.JPG", Some("Kopie 1")),
            row(4, c, "C.JPG", None),
            row(5, None, "X.JPG", None),
            row(6, None, "Y.JPG", Some("K")),
        ];
        rows.sort_by(|a, b| a.capture.is_none().cmp(&b.capture.is_none()).then_with(|| a.capture.cmp(&b.capture)).then_with(|| a.name.cmp(&b.name)).then_with(|| a.copy.cmp(&b.copy)));
        let sorted = |v: &[usize]| {
            let mut v = v.to_vec();
            v.sort_by(|&x, &y| {
                let (za, zb) = (&rows[x], &rows[y]);
                za.capture.is_none().cmp(&zb.capture.is_none()).then_with(|| zb.capture.cmp(&za.capture)).then_with(|| za.name.cmp(&zb.name)).then_with(|| za.copy.is_some().cmp(&zb.copy.is_some()))
            });
            v
        };
        for v in [vec![0, 1, 2, 3, 4, 5], vec![1, 2, 4], vec![0, 3], vec![5], vec![]] {
            assert_eq!(newest_first(&rows, &v), Some(sorted(&v)), "{v:?}");
        }
        assert_eq!(newest_first(&rows, &[3, 0]), None);
    }
}

/// 'YYYY-MM-DDThh:mm:ss' → 'DD.MM.YYYY hh:mm:ss'
fn time_display(t: &str) -> String {
    match (t.get(0..4), t.get(5..7), t.get(8..10), t.get(11..19)) {
        (Some(j), Some(m), Some(d), Some(z)) => format!("{d}.{m}.{j} {z}"),
        _ => t.to_string(),
    }
}

/// Input 'DD.MM.YYYY hh:mm[:ss]' (or 'YYYY-MM-DD hh:mm:ss') → 'YYYY-MM-DDThh:mm:ss'
fn time_input(t: &str) -> Option<String> {
    let t = t.trim();
    let (capture_date, clock) = t.split_once(' ').map(|(a, b)| (a.trim(), b.trim())).unwrap_or((t, "00:00:00"));
    let clock = if clock.matches(':').count() == 1 { format!("{clock}:00") } else { clock.to_string() };
    let parts: Vec<&str> = clock.split(':').collect();
    let clock = if parts.len() == 3 { format!("{:0>2}:{:0>2}:{:0>2}", parts[0], parts[1], parts[2]) } else { return None };
    let iso = if capture_date.contains('.') {
        let d: Vec<&str> = capture_date.split('.').collect();
        if d.len() != 3 {
            return None;
        }
        format!("{:0>4}-{:0>2}-{:0>2}T{clock}", d[2], d[1], d[0])
    } else {
        format!("{capture_date}T{clock}")
    };
    library::time_correction::seconds(&iso).map(library::time_correction::text)
}

/// Difference in seconds between the time of the active photo and the entered one.
fn time_offset(reference: Option<&str>, input: &str) -> Option<i64> {
    let old = library::time_correction::seconds(reference?)?;
    let new = library::time_correction::seconds(&time_input(input)?)?;
    Some(new - old)
}

/// "+1 Tag 2 h 5 min 3 s"
fn offset_text(d: i64) -> String {
    let sign = if d < 0 { "−" } else { "+" };
    let a = d.unsigned_abs();
    let (t, h, m, s) = (a / 86_400, a % 86_400 / 3600, a % 3600 / 60, a % 60);
    let mut v = Vec::new();
    if t > 0 {
        v.push(crate::trf!("{t} {e}", t = t, e = if t == 1 { crate::tr("Tag") } else { crate::tr("Tage") }));
    }
    if h > 0 {
        v.push(format!("{h} h"));
    }
    if m > 0 {
        v.push(format!("{m} min"));
    }
    if s > 0 || v.is_empty() {
        v.push(format!("{s} s"));
    }
    format!("{sign}{}", v.join(" "))
}

/// Status text at the start of a background run (before its first report).
fn report_start(status: &Arc<Mutex<String>>, ctx: &egui::Context, search: bool) {
    *status.lock().unwrap_or_else(|e| e.into_inner()) = if search { "Gesichter werden gesucht …" } else { "Namensvorschläge werden aktualisiert …" }.to_string();
    ctx.request_repaint();
}
