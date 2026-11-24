// Windows: start as a GUI program without a console window (messages go to the trace log)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! graineo – user interface for developing Sigma Merrill RAWs.
//!
//! Layout: folder with thumbnails on the left, image in the middle (zoom, pixel inspector),
//! sliders, sensor explorer and export on the right. Heavy work runs in
//! background threads; the interface stays responsive meanwhile.
//!
//! Per image: thumbnail immediately → fast preview (≈ 0.35 s) → after a short
//! dwell, Sigma's exact base (≈ 2.6 s, "Refining …" bottom right).
//! Neighbouring images are preloaded, visited images are cached.

mod work_step;
mod highlight_ui;
mod edit_marks;
mod detail_filter;
mod detail_ui;
mod print_job;
mod print_dialog;
mod print_room;
mod camera_looks_ui;
mod learn_look_ui;
mod series_job;
mod style_scan;
mod support;
mod spp_accept;
mod preview_hold;
mod start_migration;
mod backup_view_ui;
mod workspace;
mod cell_label;
mod cell_info;
mod click_mods;
mod edge_peek;
mod sticky;
mod scroll_hold;
mod narrow_window;
mod default_app;
mod mem_budget;
mod photo_state;
mod develop_guard;
/// What the zoomed view asks for at full resolution (crop, whole image, nothing) – testable without egui
mod detail_plan;
mod filmstrip_layout;
mod large_preview;
mod threads;
mod env;
mod browse;
mod view_folder;
mod frame_watch;
mod grid_scroll_test;
mod grid_bench;
mod import_bench;
#[cfg(test)]
mod library_store_tests;
mod library_select;
mod grid_jump;
mod library_turn;
mod library_writer;
mod quit;
mod tool_rail;
mod mix_triangle;
mod file_dialog;
mod system;
mod dock_open;
mod align;
mod darkroom;
mod darkroom_loupe;
mod darkroom_extended;
mod darkroom_probe;
mod darkroom_curve;
mod darkroom_scan;
mod darkroom_lut;
mod darkroom_compare;
mod darkroom_contact;
mod path_check;
mod darkroom_print;
mod darkroom_layout;
mod panel_buttons;
mod toolbar_plan;
mod darkroom_strip;
mod darkroom_zones;
mod darkroom_marker;
mod darkroom_recipes;
mod recipe_grid;
mod darkroom_border;
mod darkroom_shelf;
mod film_previews;
mod darkroom_dodge;
mod media_library;
mod navigator;
mod zoom;
mod brand;
mod camf_view;
mod multi_edit;
mod multi_apply;
mod tone_eq_ui;
mod menu;
mod paste;
mod settings_group;
mod display_color;
mod window_geometry;
mod help;
mod wb_picker;
mod vectorscope;
// develop history of the open photo (steps, cursor, kept in the library)
mod history;
mod history_panel;
// ⌘Z / ⇧⌘Z: the most recent action of all kinds
mod undo_order;
mod jobs;
mod lr;
mod presets;
mod raw_app;
mod ai_denoise_ui;
mod hdr_auto;
mod hdr_paint;
mod hdr_batch;
mod rough_views;
mod hdr_live;
mod hdr_ui;
mod mask_list_ui;
mod mask_parts;
mod masks_ui;
mod mask_sample;
mod face_parts;
mod subject_ui;
mod depth_ui;
mod histogram_pin;
mod analysis_progress;
mod remove_ui;
mod prefs;
// choice of the search model in stages, text model included (Settings → Search)
mod prefs_search_model;
mod apple_photos_ui;
#[cfg(target_os = "macos")]
mod photokit;
mod lens_app;
mod lens_assign;
mod lens_room;
mod lens_wizard;
mod lens_grid_pdf;
mod face_frames;
mod focus_frame;
mod people_ui;
mod people_model;
mod video_ui;
mod film_profiles_ui;
mod film_look;
mod compare_view;
// Photo comparison in the centre of the library and of Develop (state, drawing)
mod photo_compare;
mod photo_compare_ui;
mod apple_raw;
#[cfg(test)] mod ui_test;
#[cfg(test)] mod list_switch_test;
#[cfg(test)] mod people_develop_test;
#[cfg(test)] mod grid_selection_test;
#[cfg(test)] mod view_folder_refresh_test;
#[cfg(test)] mod multi_edit_load_test;
mod stress;
mod shortcuts;
// ⌘C: photos to the clipboard
mod photo_copy;
// Info panel: order and grouping of the rows
mod info_rows;
mod developed_cache;
mod preview_repair;
mod sigma_refresh;
mod photo_display;
mod proof;
mod sensor_raw;
mod sensor_ui;
mod delete;
mod dialog;
mod export_probe;
mod export_names;
mod export_naming_ui;
mod export_ui;
mod export_templates;
mod test_matrix;
mod image_kind;
mod rating;
mod sidecar;
mod retouch_ui;
mod activity;
mod stack;
mod library_ui;
mod semantic_ui;
mod lightroom_state;
mod move_folder_ui;
mod relink_ui;
mod remote_access;
mod library_filter;
mod keyword_export;
mod test_image;
mod update_ui;
mod binning_ui;
mod crash_log;
mod crossfade;
mod file_migration;
mod bug_report;
mod perf_measure;
mod perf_mode;
mod perf_report;
mod perf_text;
mod timing_log;

use crate::menu::i18n::DeComma as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use develop::export::{Engine, Format};
use develop::geometry::{self, Frame};
use develop::{Settings, View};
use eframe::egui;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};

use jobs::{Job, JobResult, Loaded, Worker};

/// Expected duration of the SPP base (for the progress bar).
/// Initial value for the expected duration of the SPP base (GPU); learned afterwards
const SPP_EXPECTED_SECS: f64 = 1.3;
/// Delay on an image before the SPP base starts.
const SPP_DELAY_SECS: f64 = 0.12;
/// Preloads of neighbours up to this distance from the selected image keep running when switching
const PREFETCH_REACH: usize = 1;

/// Suggested export folder for photos in `dir`: `dir/export`, but never inside an Apple Photos library or another
/// program's package (photos from Apple Photos lie in `….photoslibrary/originals/…`) – then none is suggested.
fn export_default(dir: &std::path::Path) -> Option<PathBuf> {
    let d = dir.join("export");
    library::write_target::check(&d).is_ok().then_some(d)
}

fn main() -> eframe::Result {
    // files written under German names by earlier versions get their English names (before anything reads or logs)
    file_migration::run();
    // Crashes go to the trace log and, lastingly, to the crash log (on Windows there is no console window to show them)
    crash_log::install();
    threads::init();
    // neural denoising for Sigma bases (only acts when a photo's settings ask for it, see ai_denoise_ui.rs)
    rawdev::ai_denoise::install_sigma();
    // started again by "Sicherung ansehen" or "Zurück zur Bibliothek": the previous process ends first
    library::backup_view::wait_for_parent();
    single_instance_or_exit();
    let viewing = library::backup_view::active().is_some();
    if !viewing {
        // a restore from a backup chosen in the settings: before anything opens the library (shown in the library)
        library::backup::apply_pending_restore(&library::db_path());
        // the copy of a viewed backup (back from the view, or left after a crash during it)
        library::backup_view::cleanup();
    }
    // rasters of bitmap masks come from the library
    subject_ui::install_provider();
    // operating mode for slower computers: setting, stored measurements, system state (`perf_mode`)
    perf_mode::init();
    // background film computations (offline copies, thumbnails) stay out of the film gate while the user works
    pools::gate::FILM.set_hold_back(jobs::foreground_busy);
    // Quick Look processes for video previews left over from an earlier run (they can hang for days)
    std::thread::spawn(|| {
        let n = library::video::cleanup_orphans();
        if n > 0 {
            log(&format!("video previews: {n} leftover qlmanage processes ended"));
        }
    });
    // Sigma bases of an earlier computing method (left after an update) leave the disk cache
    std::thread::spawn(|| {
        crate::jobs::wait_for_foreground(std::time::Duration::from_secs(30));
        let n = develop::diskcache::sweep_stale();
        if n > 0 {
            log(&format!("disk cache: {n} entries of an earlier computing method removed"));
        }
    });
    // An older library is converted on a helper thread while the window shows the progress (`start_migration`); the
    // work on the library starts after it
    let migration = if viewing { None } else { start_migration::begin() };
    if migration.is_none() && !viewing {
        start_library_background();
    }
    // Test hook: GRAINEO_WINDOW="1512x900" – fixed window size instead of maximised (e.g. 14-inch screen)
    let test_window: Option<[f32; 2]> = crate::env::var("GRAINEO_WINDOW").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some([w.trim().parse().ok()?, h.trim().parse().ok()?])
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("graineo")
            .with_inner_size(test_window.unwrap_or([1500.0, 950.0]))
            .with_min_inner_size([900.0, 600.0])
            // As in common photo apps: use the whole screen at startup (without fullscreen mode)
            .with_maximized(test_window.is_none())
            .with_icon(eframe::icon_data::from_png_bytes(brand::ICON_PNG).unwrap_or_default()),
        ..Default::default()
    };
    photo_display::prefer_float_surface();
    eframe::run_native("graineo", options, Box::new(move |cc| Ok(start_migration::Start::new(cc, migration))))
}

/// The library is the main store: sidecars not read yet come in once in the background, the optional sidecars next
/// to the photos are written in the background (both at the lowest priority, stepping back while the user works)
/// (not while a backup is viewed: nothing there reads or writes next to the photos)
fn start_library_background() {
    library::sidecar_import::start_background_import(library::db_path(), || {
        crate::jobs::wait_for_foreground(std::time::Duration::from_secs(30));
    });
    library::store::start_sidecar_writer(library::db_path(), || {
        crate::jobs::wait_for_foreground(std::time::Duration::from_secs(30));
    });
}

/// Only one graineo per library: a second start with the same data folder ends at once with a clear message
/// (`library::instance_lock`; the OS lock disappears with the process, so a crash leaves nothing blocking).
fn single_instance_or_exit() {
    // a restart (backup view, update) waits for the previous process; otherwise only a short grace period
    let patience = if std::env::var_os("GRAINEO_WAIT_PID").is_some() { 15 } else { 3 };
    let db = library::real_db_path();
    match library::instance_lock::acquire(&db, std::time::Duration::from_secs(patience)) {
        Ok(library::instance_lock::Outcome::Acquired) => {}
        Ok(library::instance_lock::Outcome::Busy(pid)) => {
            // not `log`: that would rotate the trace log of the instance already running
            eprintln!("second instance: library {} is in use by process {pid:?}, exiting", db.display());
            // started with a folder or photo (Explorer menu "Mit graineo ansehen", "Open with"): the running graineo
            // shows it (open_requests), no message
            if let Some(p) = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.exists()) {
                if library::instance_lock::hand_over(&db, &p).is_ok() {
                    std::process::exit(0);
                }
            }
            // test and automatic runs (own data folder, a run that ends by itself, a screenshot run) never wait in a
            // message nobody will answer
            let unattended = ["GRAINEO_DATA_DIR", "GRAINEO_QUIT", "GRAINEO_SCREENSHOT"].iter().any(|v| crate::env::var_os(v).is_some());
            if unattended {
                std::process::exit(1);
            }
            let folder = db.parent().map(|p| p.display().to_string()).unwrap_or_default();
            let text = crate::trf!(
                "graineo läuft bereits mit dieser Bibliothek.\n\nZwei Fenster auf dieselbe Bibliothek könnten sich gegenseitig Änderungen überschreiben. Bitte das bereits geöffnete graineo verwenden.\n\nBibliothek: {folder}",
                folder = folder
            );
            second_instance_note(&text, SECOND_INSTANCE_NOTE_S);
            std::process::exit(1);
        }
        // never block the first start because of a lock problem (e.g. a file system without locks)
        Err(e) => log(&format!("instance lock not available, starting anyway: {e}")),
    }
}

/// The note of a second start with the same library. It never keeps the process alive for long: the note closes by
/// itself after [`SECOND_INSTANCE_NOTE_S`] seconds when nobody answers it (it can lie behind other windows, and the
/// process waited there for minutes – QA 9), and the process ends either way.
#[cfg(target_os = "macos")]
fn second_instance_note(text: &str, seconds: f64) {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, len: isize, encoding: u32, external: u8) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        fn CFUserNotificationDisplayAlert(timeout: f64, flags: usize, icon: *const c_void, sound: *const c_void, localization: *const c_void, header: *const c_void, message: *const c_void, default_button: *const c_void, alternate: *const c_void, other: *const c_void, response: *mut usize) -> i32;
    }
    const UTF8: u32 = 0x0800_0100;
    /// kCFUserNotificationCautionAlertLevel
    const CAUTION: usize = 2;
    // SAFETY: plain CoreFoundation calls with strings created and released here; the alert call blocks until the
    // note is answered or the timeout has passed.
    unsafe {
        let cf = |s: &str| CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0);
        let (header, message) = (cf("graineo"), cf(text));
        let mut response = 0usize;
        CFUserNotificationDisplayAlert(seconds, CAUTION, std::ptr::null(), std::ptr::null(), std::ptr::null(), header, message, std::ptr::null(), std::ptr::null(), std::ptr::null(), &mut response);
        for s in [header, message] {
            if !s.is_null() {
                CFRelease(s);
            }
        }
    }
}

/// Other systems: the system's message box on a helper thread; the process ends when it is answered, at the latest
/// after [`SECOND_INSTANCE_NOTE_S`] seconds.
#[cfg(not(target_os = "macos"))]
fn second_instance_note(text: &str, seconds: f64) {
    let text = text.to_string();
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Warning).set_title("graineo").set_description(text).set_buttons(rfd::MessageButtons::Ok).show();
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(std::time::Duration::from_secs_f64(seconds));
}

/// How long the note of a second start stays without an answer.
const SECOND_INSTANCE_NOTE_S: f64 = 60.0;

#[cfg(all(test, target_os = "macos"))]
mod second_instance_tests {
    /// The note never keeps the process waiting beyond its time (run by hand: it shows a system note for two seconds).
    #[test]
    #[ignore]
    fn note_closes_by_itself() {
        let t = std::time::Instant::now();
        super::second_instance_note("graineo test: this note closes by itself after two seconds.", 2.0);
        let s = t.elapsed().as_secs_f64();
        assert!((1.5..6.0).contains(&s), "returned after {s:.1} s");
    }
}

/// Message for an image whose file cannot be opened: a missing file and a disconnected drive
/// are different causes and are named separately.
/// Remember whether the opened photo is damaged and partly rescued (see `photo_state`); it opened, so it is not
/// "cannot be opened" any more.
fn note_damage(loaded: &Loaded) {
    let path = &loaded.path;
    photo_state::clear(path, photo_state::Kind::OpenFailed);
    match &loaded.damage {
        Some(d) => photo_state::set(path, photo_state::damaged(d)),
        None => photo_state::clear(path, photo_state::Kind::Damaged),
    }
}

/// Can a photo in this library state be developed: its file is there, or it is a merged photo (HDR) whose pixels come
/// from its sources and the cache (`library::merge`).
pub(crate) fn developable_state(state: u8) -> bool {
    state == 0 || state as i64 == library::merge::STATUS_VIRTUAL
}

pub(crate) fn state_message(state: u8) -> String {
    match state {
        1 => crate::tr("Datei fehlt – gelöscht oder verschoben").to_string(),
        2 => crate::tr("Original nicht erreichbar").to_string(),
        _ => crate::tr("Datei nicht erreichbar").to_string(),
    }
}

struct Entry {
    path: PathBuf,
    name: String,
    thumb: Option<TextureHandle>,
    settings: Settings,
    meta: sidecar::Meta,
    /// Multiple selection in the filmstrip (⌘/⇧ click)
    selected: bool,
    /// virtual copy (name), otherwise the original
    copy: Option<String>,
    /// Non-Sigma RAW: own settings in the sidecar (otherwise defaults, e.g. from Lightroom)
    stored: bool,
    /// Library image ID (shared image source) and file ID (thumbnails)
    image: i64,
    file: i64,
    format: String,
    /// 0 present, 1 missing, 2 not connected
    state: u8,
    /// Settings already read from the sidecar (otherwise on demand, see `ensure_entry`)
    loaded: bool,
    /// Aspect ratio of the upright photo from the library's dimensions (width of the filmstrip cell)
    aspect: f32,
    /// `aspect` checked against the first thumbnail shown: the library's dimensions are only a first guess (they can
    /// be the sensor's, unrotated, without a known orientation, or ignore a crop), the thumbnail shows the real shape
    aspect_checked: bool,
}


/// Settings as the sidecar keeps them (the view is not saved).
fn sidecar_settings(s: &Settings) -> Settings {
    Settings { view: develop::View::Developed, ..s.clone() }
}

/// Save an image's settings.
fn save_entry(e: &Entry) -> Result<(), String> {
    sidecar::save_entry(&e.path, e.copy.as_deref(), &e.settings, &e.meta)
}

/// The currently opened image.
struct Current {
    index: usize,
    loaded: Loaded,
    texture: Option<TextureHandle>,
    camera_jpeg: Option<TextureHandle>,
    /// Histogram of the developed preview image (8 bit, R/G/B)
    out_hist: Option<[Vec<u32>; 3]>,
    /// RGB parade or waveform of the preview image (only computed when shown)
    scope_tex: Option<TextureHandle>,
    /// RGB waveform for the header of the Develop panel (only computed when selected)
    wave_tex: Option<TextureHandle>,
    /// Clipping mask (highlights red, shadows blue) for the preview
    clip_tex: Option<TextureHandle>,
    /// Full resolution (tiled textures, image region x/y/w/h, settings): a crop around the visible part or – once
    /// rendered – the whole image, from which every zoom and pan is drawn until the settings change
    detail: Option<(DetailTex, [usize; 4], Settings)>,
    /// the sharp image of the pixels before a merged photo got new ones (`hdr_live.rs`): drawn below until `detail` is
    /// there again, so a change never shows the blurry preview at 100 %
    detail_prev: Option<(DetailTex, [usize; 4], Settings)>,
    /// 100 % view in the darkroom comparison: crop of the left side at full resolution
    detail_left: Option<(TextureHandle, [usize; 4], Settings)>,
    /// 100 % view: crop at full resolution for the preset or film currently hovered
    detail_hover: Option<(TextureHandle, [usize; 4], Settings)>,
    /// Grain loupe: enlarged crop (film grain rendered anew), kept apart from the whole image in `detail`
    detail_grain: Option<(DetailTex, [usize; 4], Settings)>,
    detail_grain_scale: usize,
    detail_left_scale: usize,
    /// Last answer of `Session::has_spp_for` (key hash of the settings, answer) for the moments when the session is
    /// locked (own pipeline rendering): then the last known answer counts instead of "base missing"
    spp_ready_cache: std::cell::Cell<Option<(u64, bool)>>,
    /// The exact base of this photo has just arrived (time): the render that follows is faded in (crossfade.rs)
    fade_armed: Option<f64>,
    /// Running crossfade fast preview -> exact image (old texture below, new one fading in)
    fade: Option<crossfade::Fade>,
}

impl Current {
    /// Sharp image for this enlargement: factor 1 is the whole image or its crop, above that the grain loupe.
    fn detail_for(&self, factor: usize) -> Option<&(DetailTex, [usize; 4], Settings)> {
        if factor <= 1 {
            self.detail.as_ref()
        } else {
            self.detail_grain.as_ref().filter(|_| self.detail_grain_scale == factor)
        }
    }
}

/// Width of the left panel (Library and Develop); 300 instead of 250 so that long folder names fit
const SOURCES_W: f32 = 300.0;
/// Zoom menu in a very narrow bar (only the percentage).
const NARROW_ZOOM_W: f32 = 72.0;
/// Width of the zoom buttons (fit, −, +), three cells of 30 pt.
const ZOOM_BUTTONS_W: f32 = 94.0;

/// Text of the zoom menu: only the percentage, also when the photo is fit (the fit button shows that).
fn zoom_label(fit: bool, fit_pct: f32, pct: f32) -> String {
    format!("{:.0} %", if fit { fit_pct } else { pct })
}

/// Fixed width of the zoom menu: enough for the longest value ("1600 %"); the monospaced font keeps every digit as wide,
/// so the field never changes its width.
fn zoom_field_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    ui.painter().layout_no_wrap("1600 %".to_string(), font, egui::Color32::WHITE).size().x.ceil() + 40.0
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Develop,
    Sensor,
    Export,
    /// Library (bibliothek_ui.rs): catalog, folders, collections, grid, loupe
    Library,
}

/// State of the refinement indicator at the bottom right of the image.
#[derive(Clone, Copy, PartialEq)]
enum Refine {
    None,
    /// Waiting for dwell or computing (since time)
    Working { since: f64 },
    /// Done (time) – "✓ SPP-exact" is shown briefly
    Done { at: f64 },
}

struct App {
    egui_ctx: egui::Context,
    worker: Worker,
    folder: Option<PathBuf>,
    entries: Vec<Entry>,
    current: Option<Current>,
    loading: Option<usize>,
    /// Preloaded or recently visited images
    cache: Vec<(usize, Loaded)>,
    /// Cancel flag per preloaded neighbour: switching images only cancels preloads that are no longer near the new image
    prefetch_flags: Vec<(usize, Arc<AtomicBool>)>,
    /// Direction of the last step through the filmstrip (+1 forward, -1 back): neighbours are preloaded in this direction first
    nav_dir: isize,
    /// Last look at the memory pressure of the system (s, `mem_budget.rs`)
    pressure_checked: f64,
    prefetch_due: Option<f64>,
    tab: Tab,
    zoom: f32,
    pan: egui::Vec2,
    show_camera_jpeg: bool,
    /// Settings last rendered for, or currently being rendered
    rendered_for: Option<(usize, Settings)>,
    rendering: bool,
    preparing: bool,
    detail_pending: bool,
    /// after a failed crop job, do not request again until this time (egui time)
    detail_blocked_until: f64,
    /// Colour mode for which an SPP base is currently being computed
    spp_pending: Option<develop::SppKey>,
    /// Photo for which the withheld fast preview was logged (once per wait)
    held_logged: Option<usize>,
    /// The last finished picture of a photo that is opened again with another size (binning): it stays on screen
    /// until the first render of the new session (see `preview_hold.rs`)
    kept_view: Option<(PathBuf, TextureHandle)>,
    /// Photo whose embedded camera picture stands in while its fast preview is withheld and nothing else is there
    /// (opened from a folder, no stored preview); see `preview_hold.rs`
    hold_jpeg: Option<usize>,
    /// SPP lens profile per file (name, `None` = none) – for "Lens corrections"
    lens_profiles: std::collections::HashMap<PathBuf, Option<String>>,
    spp_cancel: Arc<AtomicBool>,
    /// Time from which the SPP base for the current image is started
    spp_due: Option<f64>,
    /// `spp_due` waits for the settings to rest after a change that needs another base (this key, see `spp_accept.rs`)
    spp_settle: Option<develop::SppKey>,
    /// Early-started SPP base (index) whose counts still need to be applied
    spp_early: Option<usize>,
    spp_counts: Option<(usize, develop::SppKey, develop::Linear, f32)>,
    /// SPP base of the next image is being precomputed (index, cancel)
    spp_prefetching: Option<(usize, Arc<AtomicBool>)>,
    spp_seconds: Option<f32>,
    refine: Refine,
    open_generation: Arc<AtomicUsize>,
    /// Time of the last zoom/pan change
    view_changed_at: f64,
    /// Stress test of background processing (only with `GRAINEO_STRESS`)
    stress: Option<stress::Stress>,
    /// Time from slider change to image (test hook `GRAINEO_EDIT_LOG`, see `stress.rs`)
    edit_log: stress::EditLog,
    /// develop history (⌘Z / ⇧⌘Z move its cursor), see `history.rs`
    history: history::History,
    /// turns made in the library (⌘Z for all photos of one command, see `library_turn.rs`)
    turn_undo: library_turn::TurnUndo,
    turn_ops: u64,
    /// ⇧+arrow in the filmstrip: anchor and moving end (entry indices)
    strip_ext: Option<(usize, usize)>,
    /// filmstrip: the photo of the last single click and whether it was active before it (a double click on the photo
    /// that was already active closes its series)
    strip_click_was: Option<(i64, bool)>,
    /// Develop: the library changed its active photo for the strip (⌥←/→ into a series, S, ↩, a stack closed): open
    /// it once the new list is there; the value: keep the zoom
    strip_follow_lib: Option<bool>,
    /// filmstrip cells of the last frame: photo and left edge in the visible strip (a stack opened or closed stays
    /// where it stood)
    strip_seen: Vec<(i64, f32)>,
    /// bring this filmstrip cell into view (moving end of a ⇧+arrow selection)
    strip_follow: Option<usize>,
    /// Darkroom (dunkelkammer.rs)
    darkroom: darkroom::State,
    /// Last committed settings state (for undo)
    committed: Option<(usize, Settings)>,
    clipboard: Option<Settings>,
    /// Groups transferred on paste, and the copy dialog
    copy_groups: paste::Groups,
    copy_dialog: bool,
    /// "Copy settings …" from the context menu of a photo that is not open: its index (else the open photo)
    copy_from: Option<usize>,
    // View options
    presets: Vec<presets::Preset>,
    /// own presets deleted in this session: (time, place in the list, preset); ⌘Z puts the newest back
    preset_undo: Vec<(u64, usize, presets::Preset)>,
    new_preset_name: String,
    /// Confirmation dialog "Delete rejected" open
    confirm_delete: bool,
    /// Figures for the delete dialog (determined once on opening): photos, bytes, camera JPEGs
    delete_info: Option<(usize, u64, usize)>,
    /// Delete dialog: remove entries of missing files (on)
    delete_missing: bool,
    /// re-read the library after deleting
    delete_done_reload: bool,
    /// archive/restore finished: reset the current photo once the library is re-read
    reset_after_reload: bool,
    /// active after deleting: the photo to the right of the deleted ones (otherwise to the left)
    delete_neighbor: Option<i64>,
    /// virtual copy is being created in the background: source, open in single view, settings of the source
    copy_pending: Option<(i64, bool, Option<(Settings, sidecar::Meta)>)>,
    /// Deleting the selection (Del): image IDs; `None` = "Delete rejected"
    delete_selection: Option<Vec<i64>>,
    /// Rail tool that opens as soon as the image has loaded
    tool_open: Option<workspace::ToolTab>,
    /// Images last counted by the export dialog (for its title)
    export_count: usize,
    /// Export: include the originals' HDR gain map
    export_gain_map: bool,
    /// Export format "Original": the files unchanged (verified copy), see `library::original_export`
    export_original: bool,
    /// … with their XMP sidecars (ratings, keywords, edits)
    export_original_sidecars: bool,
    /// … with the camera JPEG of a RAW+JPEG pair
    export_original_pair: bool,
    /// Output colour space of the export (JPEG/TIFF only); default sRGB
    export_color_space: graineo_color::Space,
    /// Export options (bit depth, compression, linear working space, DNG extras); stored as `exportOptions`
    export_options: develop::export::Options,
    /// Export "as print": include the darkroom's paper and film border in the file
    export_as_print: bool,
    /// Limit the size on export (JPEG/TIFF) and to which size
    export_limit_size: bool,
    export_resize: develop::resize::ResizeFilter,
    /// output sharpening of finished exports (stored as `exportSharpen`)
    export_sharpen: develop::output_sharpen::OutputSharpen,
    export_size: develop::export::OutputSize,
    delete_jpegs: bool,
    /// Before/after side by side (Y)
    compare: bool,
    show_before: bool,
    /// Left side of "Unedited | Current" (remembered for the session) and its list
    before: compare_view::BeforeChoice,
    /// Previews and crops of the comparison panes that do not show the current settings
    panes: compare_view::PaneTextures,
    /// pipeline comparison graineo | Apple in the sensor tray (macOS, apple_raw.rs)
    apple: apple_raw::AppleView,
    /// Look comparison (switch "Compare" in "Film & Look")
    look_compare: compare_view::LookCompare,
    /// the app's part of the photo comparison (crops at full resolution, view), photo_compare_ui.rs
    cmp: photo_compare_ui::CompareView,
    show_clipping: bool,
    /// Face frames in single view; cache: (image, time, faces)
    show_faces: bool,
    /// Focus area of the camera in single view (⇧F)
    show_focus: bool,
    /// Placeholder from the last developed view (image, texture, grey) and the last stored state
    developed_tex: Option<(i64, egui::TextureHandle, bool)>,
    developed_stored: Option<(i64, Settings)>,
    /// thumbnails reduced in a helper thread, the newest request per image and a counter (see `thumb_from_render`)
    thumbs_done: Arc<Mutex<Vec<ThumbDone>>>,
    thumb_newest: HashMap<i64, u64>,
    /// newest request per image that stores the developed view
    thumb_store_newest: HashMap<i64, u64>,
    thumb_generation: u64,
    /// Paging through images: dwell before opening, preloading, fingerprints of stored previews (see [`browse`])
    browse: browse::Browse,
    /// Images currently being preloaded in the background (a click on one waits instead of reloading)
    preload_running: Vec<usize>,
    /// Grid selection: last preloaded image and cancel flag
    preload_image: Option<i64>,
    preload_cancel: Arc<AtomicBool>,
    faces_cache: Option<(i64, f64, Vec<library::people::FaceInfo>)>,
    /// Immediate face search for the shown photo if the background run has not reached it yet: image and result; images it already ran for
    faces_search: Option<(i64, std::sync::Arc<std::sync::Mutex<Option<Result<usize, String>>>>)>,
    faces_tried: std::collections::HashSet<i64>,
    faces_names: Vec<(i64, String)>,
    /// RGB parade or waveform in the Sensor module (remembered)
    scope: lr::Scope,
    /// Header of the Develop panel: 0 histogram, 1 RGB parade, 2 waveform, 3 vectorscope (remembered)
    dev_scope: u8,
    /// Image info in single view (top right): 0 off, 1 short, 2 detailed (key I)
    info_level: u8,
    /// results of work: errors until closed, a short success for a few seconds (status in the title bar, activity.rs)
    notes: activity::Notes,
    /// running export: the photos and the failures so far (for the result and "Try again")
    export_items: Vec<PathBuf>,
    export_failures: Vec<(Option<PathBuf>, String)>,
    /// Individual panels as in common photo apps (F5 top, F6 filmstrip, F7 left, F8 right)
    show_top: bool,
    show_filmstrip: bool,
    show_left: bool,
    /// Narrow window: automatic collapse of a side panel (once per crossing)
    narrow: narrow_window::Auto,
    show_right: bool,
    /// Lights out (L): 0 = normal, 1 = dimmed, 2 = off
    lights: u8,
    /// former screen mode (⇧F, removed; stays 0): 0 = normal, 1 = fullscreen with panels, 2 = fullscreen without
    screen_mode: u8,
    /// Fullscreen preview (F): only the image on black; remembered state to return to
    preview_full: Option<(bool, bool, bool, bool, bool)>,
    /// Image area in the loupe (for "Lights out")
    image_rect: Option<egui::Rect>,
    /// Preset preview in the navigator: requested settings and finished image
    hover_preset: Option<Settings>,
    /// Preset thumbnails (panel "Presets & History")
    preset_images: recipe_grid::Previews,
    hover_tex: Option<(Settings, TextureHandle)>,
    /// Preset last chosen by click ("category/name") – ↑/↓ moves on from there
    preset_cursor: Option<String>,
    /// Crop mode (R): edit the crop, state before opening (for Esc)
    crop_mode: Option<([f32; 4], f32, [f32; 2])>,
    /// Feedback from "Upright" (text, time) below the buttons
    upright_msg: Option<(String, f64)>,
    /// Show the perspective guide grid until (time)
    persp_grid_until: f64,
    /// "Straighten" tool in crop mode: draw a line in the image (start in the undistorted image)
    straighten: bool,
    straighten_from: Option<[f32; 2]>,
    /// Line is currently being dragged with the mouse button held (otherwise: first point set by click)
    straighten_drag: bool,
    /// Tool in crop mode (icon row on the right)
    geo_tool: align::GeoTool,
    /// Guide lines over the crop
    crop_raster: align::Raster,
    /// Rotation in quarter turns when crop mode was opened (Cancel restores it)
    crop_rotate_start: Option<u8>,
    /// Aspect ratio lock (width/height); None = free
    crop_aspect: Option<f32>,
    crop_drag: Option<CropDrag>,
    /// Toolbar below the image (T)
    show_toolbar: bool,
    /// Space between the left and right groups of the bar below the image (tests: never negative)
    toolbar_gap: f32,
    /// Measured widths of the bar groups (last frame), the width rule works with these
    toolbar_widths: std::collections::HashMap<&'static str, f32>,
    /// Zoom without the − / + buttons (they went into "MEHR")
    zoom_compact: bool,
    /// Very narrow bar: the zoom menu shows only the percentage
    zoom_narrow: bool,
    brand: brand::Brand,
    /// "Paste" from the develop column (executed after drawing)
    pending_paste: bool,
    menu: Option<menu::AppMenu>,
    /// Colour mixer tab: 0 hue, 1 saturation, 2 luminance
    hsl_tab: usize,
    /// Tone curve: selected channel (0 RGB, 1 red, 2 green, 3 blue)
    curve_channel: usize,
    /// Learned duration of the SPP base (moving average) for the progress bar
    spp_expected: f64,
    /// dwell before the exact base of the photo shown starts (s; operating mode, see `perf_mode`) – for the progress ring
    spp_wait: f64,
    /// How often the watchdog has already re-requested the base for the current image
    refine_retries: u8,
    /// Time the image was selected (for timing in the log)
    selected_at: Option<(usize, f64)>,
    /// What the sidecar holds per image (settings without the view, rating) as last read or written: leaving an
    /// unchanged image then touches no file (on the NAS each look at the sidecar took up to seconds in the UI thread)
    saved: HashMap<i64, (Settings, sidecar::Meta)>,
    /// Sidecars of the neighbours read ahead in the background (image, settings, rating, own settings present?)
    sidecars_ahead: Arc<Mutex<Vec<(i64, Settings, sidecar::Meta, bool)>>>,
    /// images whose sidecar is being read ahead
    sidecars_pending: std::collections::HashSet<i64>,
    /// Time the current image was shown
    shown_at: f64,
    /// Zoom/loupe needs Sigma's base in the session (e.g. image from the cache)
    detail_wants_spp: bool,
    /// Temporarily shown panels (top, filmstrip, left, right) and their position
    peek: [bool; 4],
    peeks: [edge_peek::Peek; 4],
    panel_rects: [Option<egui::Rect>; 4],
    /// Loupe (O) active and last mouse movement
    loupe: bool,
    loupe_moved_at: f64,
    /// "Copy section": title and settings at the time of copying
    section_clip: Option<(String, Settings)>,
    /// Small previews per film, Sigma mode and look ("Film & Look", darkroom film shelf; film_previews.rs)
    film_previews: film_previews::FilmPreviews,
    profile_export: Option<film_profiles_ui::ExportCheck>,
    /// Write test of the export target running in the background (see `export_probe.rs`).
    export_probe: Option<export_probe::ExportProbe>,
    /// Rebuilds of the image list (state → old index → new index), used to map results of older jobs
    /// onto the current list (see `Worker::epoch`)
    list_remaps: Vec<(u64, HashMap<usize, usize>)>,
    /// A profile is being hovered in the profile browser (preview in the large image)
    film_hover: bool,
    /// time the pointer left the look tiles (the hover preview stays briefly, see film_look.rs)
    film_hover_left: Option<f64>,
    /// Show the profile browser instead of the Develop panel
    profile_browser: bool,
    profile_choice: film_look::State,
    /// Favourites in the profile browser (key "sigma:<mode>" or film ID), remembered
    favorites: std::collections::HashSet<String>,
    /// Horizontal position of the filmstrip (for the mouse wheel)
    strip_scroll: f32,
    /// image of the filmstrip last scrolled into view
    /// Photo (image ID) the filmstrip was last aligned to; the ID rather than the position,
    /// so that a rebuild of the list (different position, same photo) does not jump again
    strip_active: Option<i64>,
    /// the compared photos and the list the filmstrip last brought into view for them (see `filmstrip`)
    strip_compared: u64,
    /// last seen source change of the library (filmstrip then goes to the start)
    strip_source: u64,
    /// Height of the filmstrip panel (dragged at its top edge, remembered as `filmstripHeight`)
    strip_height: f32,
    /// Cell positions of the filmstrip: (list version, entries, cell height bits) and the layout for it
    strip_layout: Option<((u64, usize, u32), filmstrip_layout::Layout)>,
    /// Test hook: after loading, open crop, the About window or the preferences
    test_open: Option<&'static str>,
    /// the print room (⌘P, `print_room.rs`)
    print_room: print_room::PrintRoom,
    /// Loupe magnification (1 = 100 %, 2 = 200 %, 4 = 400 %), scrolling switches
    loupe_scale: f32,
    /// Time of switching on/over (the hint fades out afterwards)
    loupe_hint_at: f64,
    /// accumulated scrolling until the next step
    loupe_scroll: f32,
    /// Tab key intercepted (Some(⇧)) before egui uses it to move focus
    tab_pressed: Option<bool>,
    show_shortcuts: bool,
    /// Zoom factor for 100 % and image size at "Fit" (for navigator and zoom steps)
    one_to_one: f32,
    fit_size: egui::Vec2,
    view_size: egui::Vec2,
    /// Values under the mouse pointer (for the line below the histogram)
    readout: Option<String>,
    /// White balance picker active (W)
    wb_picker: bool,
    /// size of its sample and the samples taken with ⇧-click
    wb_pick: wb_picker::State,
    /// Measurement tool active (M): drag a rectangle → statistics per layer
    measure: bool,
    measure_start: Option<egui::Pos2>,
    /// Measurement area (image index, rectangle in image pixels) and result
    measure_rect: Option<(usize, [usize; 4])>,
    stats: Option<(usize, [usize; 4], [develop::analysis::LayerStats; 3])>,
    layers_status: String,
    layers_flat: bool,
    camf: camf_view::CamfInspector,
    /// Sensor view for Bayer/X-Trans (analyses in the background)
    sensor_raw: sensor_raw::SensorRaw,
    /// Wide-gamut and HDR versions of the photo on screen (photo textures, see `photo_display.rs`)
    display: photo_display::State,
    /// Videos in single view (own layer)
    video: video_ui::Player,
    video_view: Option<video_ui::View>,
    /// Whole image at full resolution being rendered for the current image (settings, state it was sent for: image
    /// change and edit generation, sent at)
    full_job: Option<(Settings, (usize, u64), f64)>,
    /// Edit generation of the shown photo: counted up on every change of its settings; crops and whole images carry
    /// the state they were sent for and compute nothing more once it has passed (`jobs::EditStamp`)
    edit_generation: Arc<std::sync::atomic::AtomicU64>,
    /// Settings for which the whole image failed (do not request again)
    full_failed: Option<Settings>,
    /// Finished whole image that arrived during a zoom/pan: uploading its textures (≈ 250 MB at 50 MP) takes a few hundred
    /// ms on the UI thread and would freeze the gesture mid-way, so it waits until the view rests
    full_waiting: Option<FullWaiting>,
    /// Settings of the current image and since when they apply (the whole image is only rendered once they rest)
    detail_settings_since: Option<(Settings, f64)>,
    /// Zoomed view without a sharp image for the visible part since (for the log: time until sharp)
    detail_unsharp_since: Option<f64>,
    /// Loading spinner of the zoomed view: opacity and time of the last frame
    detail_spinner: (f32, f64),
    /// quiet spinners for "loading" (0) and Sigma refinement (1): (alpha, last frame, wanted since)
    quiet_spinners: [(f32, f64, Option<f64>); 2],
    /// Grid view (G) instead of loupe
    grid: bool,
    grid_size: f32,
    grid_cols: usize,
    export_format: Format,
    export_engine: Engine,
    export_dir: Option<PathBuf>,
    /// file names, subfolders and existing files of the export (last used, kept in the settings)
    export_naming: export_names::Naming,
    export_names_ui: export_naming_ui::State,
    export_status: String,
    exporting: bool,
    /// Export dialog (⌘⇧E)
    show_export: bool,
    /// Scope: 0 this image/selection, 1 filtered, 2 all
    export_scope: usize,
    export_progress: (usize, usize),
    /// Open the target folder in the Finder after export
    reveal_after_export: bool,
    status: String,
    /// Import dialog (⌘⇧I)
    /// Library (own database, Lightroom import, preview cache)
    lib: library_ui::LibraryView,
    /// State of the library list that `entries` was built from (shared image source)
    source_version: u64,
    /// active image cannot be developed (JPEG, video, unreachable): thumbnail only
    not_developable: Option<usize>,
    /// active image is a Sigma file with damaged RAW data (subset of `not_developable`): the badge says so
    damaged: Option<usize>,
    /// zoom of the preview while the original is not reachable: (image, zoom relative to fitted, shift)
    offline_zoom: (i64, f32, egui::Vec2),
    /// Tab of the right panel (Info / Edit)
    tool_tab: workspace::ToolTab,
    navigation: media_library::Navigation,
    /// last scroll input came from a trackpad (not a mouse wheel); egui smooths wheel steps over several frames
    scroll_trackpad: bool,
    /// Multiple selection: settings of the active photo (image ID) when it became active (or since the last "Apply")
    multi_base: Option<(i64, Settings)>,
    /// "Apply to the selection" in the background (multi_edit.rs, multi_apply.rs)
    multi: multi_edit::Multi,
    /// Key of the most recent job for new previews after a multi-image edit
    thumbnails_valid: Arc<std::sync::atomic::AtomicU64>,
    /// Photos whose preview should be re-rendered after a multi-image edit (image ID)
    thumbnails_pending: std::collections::BTreeSet<i64>,
    /// The one-off of `drop_rough_views_once` is done or not needed
    rough_views_checked: bool,
    /// last render result that arrived while dragging (index, settings, width, height, RGB): thumbnail/preview on release
    pending_thumb: Option<(usize, Settings, usize, usize, Vec<u8>)>,
    /// wide-gamut render still to start for the last result that arrived while dragging (index, settings): started on
    /// release – during a drag each step otherwise cost a second full render on the processor
    pending_wide: Option<(usize, Settings)>,
    /// not yet finished in the running job (carried over into a new job that replaces it)
    thumbnails_running: std::collections::BTreeSet<i64>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        photo_display::init(cc);
        Self::new_in(&cc.egui_ctx, native_window_handle(cc))
    }

    /// The app on an egui context; `hwnd`: main window on Windows (menu bar). Separate from [`App::new`] because the
    /// start screen (`start_migration`) builds the app only after the library has been converted.
    fn new_in(ctx: &egui::Context, hwnd: Option<isize>) -> Self {
        // merges an export, a print or zooming in needs take the same entrance as the merge jobs
        hdr_ui::install();
        if !cfg!(test) {
            dock_open::setup(ctx);
        }
        // Warm up the GPU for SPP noise reduction (compile shaders) so that even the
        // first image is refined quickly
        std::thread::spawn(|| {
            let r = develop::spp::gpu_adapter();
            log(&format!("SPP-GPU: {r:?}"));
        });
        lr::apply_theme(ctx);
        prefs::apply_scale(ctx);
        binning_ui::init();
        prefs::apply_gpu_setting(); // "Grafikkarte verwenden"
        highlight_ui::apply_setting();
        let worker = Worker::new(ctx.clone());
        let mut app = Self {
            egui_ctx: ctx.clone(),
            worker,
            folder: None,
            entries: Vec::new(),
            current: None,
            loading: None,
            cache: Vec::new(),
            prefetch_flags: Vec::new(),
            nav_dir: 1,
            pressure_checked: 0.0,
            prefetch_due: None,
            tab: Tab::Library,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            show_camera_jpeg: false,
            rendered_for: None,
            rendering: false,
            preparing: false,
            detail_pending: false,
            detail_blocked_until: 0.0,
            spp_pending: None,
            kept_view: None,
            held_logged: None,
            hold_jpeg: None,
            lens_profiles: Default::default(),
            spp_due: None,
            spp_settle: None,
            spp_prefetching: None,
            stress: stress::Stress::from_env(),
            edit_log: stress::EditLog::from_env(),
            spp_early: None,
            spp_counts: None,
            spp_cancel: Arc::new(AtomicBool::new(false)),
            spp_seconds: None,
            refine: Refine::None,
            open_generation: Arc::new(AtomicUsize::new(0)),
            view_changed_at: 0.0,
            history: history::History::default(),
            turn_undo: Default::default(),
            turn_ops: 0,
            strip_ext: None,
            strip_click_was: None,
            strip_follow_lib: None,
            strip_seen: Vec::new(),
            strip_follow: None,
            darkroom: darkroom::State::default(),
            committed: None,
            clipboard: None,
            copy_groups: paste::Groups::default(),
            copy_dialog: false,
            copy_from: None,
            presets: presets::load(),
            preset_undo: Vec::new(),
            new_preset_name: String::new(),
            confirm_delete: false,
            delete_info: None,
            delete_missing: true,
            delete_done_reload: false,
            reset_after_reload: false,
            delete_neighbor: None,
            copy_pending: None,
            delete_selection: None,
            tool_open: None,
            export_count: 1,
            export_gain_map: true,
            export_original: false,
            export_original_sidecars: true,
            export_original_pair: true,
            export_color_space: serde_json::from_value(library::settings::value("exportColorSpace")).unwrap_or_default(),
            export_options: serde_json::from_value(library::settings::value("exportOptions")).unwrap_or_default(),
            export_as_print: false,
            export_limit_size: library::settings::value("exportLimitSize").as_bool().unwrap_or(false),
            export_resize: library::settings::value("exportResize").as_str().and_then(develop::resize::ResizeFilter::from_key).unwrap_or_default(),
            export_sharpen: serde_json::from_value(library::settings::value("exportSharpen")).unwrap_or_default(),
            export_size: develop::export::OutputSize::from_json(library::settings::value("exportSize")).unwrap_or(develop::export::OutputSize::LongEdge(2048)),
            delete_jpegs: true,
            compare: false,
            show_before: false,
            before: Default::default(),
            panes: Default::default(),
            apple: Default::default(),
            look_compare: Default::default(),
            cmp: Default::default(),
            show_clipping: false,
            show_faces: crate::env::var_os("GRAINEO_FACES").is_some(),
            show_focus: crate::env::var_os("GRAINEO_FOCUS").is_some(),
            developed_tex: None,
            developed_stored: None,
            thumbs_done: Arc::default(),
            thumb_newest: HashMap::new(),
            thumb_store_newest: HashMap::new(),
            thumb_generation: 0,
            browse: browse::Browse::default(),
            preload_running: Vec::new(),
            preload_image: None,
            preload_cancel: Arc::new(AtomicBool::new(false)),
            faces_cache: None,
            faces_search: None,
            faces_tried: Default::default(),
            faces_names: Vec::new(),
            scope: app_file("view.txt").and_then(|f| std::fs::read_to_string(f).ok()).map(|s| lr::Scope::from_key(&s)).unwrap_or_default(),
            dev_scope: app_file("develop-view.txt")
                .and_then(|f| std::fs::read_to_string(f).ok())
                .map_or(0, |s| match legacy_names::translate(legacy_names::app::DEVELOP_SCOPE, s.trim()) {
                    "parade" => 1,
                    "waveform" => 2,
                    "vectorscope" => 3,
                    _ => 0,
                }),
            info_level: 1,
            notes: activity::Notes::default(),
            export_items: Vec::new(),
            export_failures: Vec::new(),
            show_top: true,
            show_filmstrip: true,
            show_left: true,
            narrow: if cfg!(test) { narrow_window::Auto::used_up() } else { Default::default() },
            show_right: true,
            lights: 0,
            screen_mode: 0,
            preview_full: None,
            image_rect: None,
            tab_pressed: None,
            loupe: false,
            loupe_moved_at: 0.0,
            section_clip: None,
            film_previews: Default::default(),
            profile_export: None,
            export_probe: None,
            list_remaps: Vec::new(),
            film_hover: false,
            film_hover_left: None,
            profile_browser: false,
            profile_choice: film_look::State::load(),
            favorites: film_look::load_favorites(),
            strip_scroll: 0.0,
            strip_active: None,
            strip_compared: 0,
            strip_source: 0,
            strip_height: library::settings::value("filmstripHeight").as_f64().map_or(filmstrip_layout::PANEL_DEFAULT, |h| (h as f32).clamp(filmstrip_layout::PANEL_MIN, filmstrip_layout::PANEL_MAX)),
            strip_layout: None,
            test_open: None,
            print_room: Default::default(),
            loupe_scale: 1.0,
            loupe_hint_at: 0.0,
            loupe_scroll: 0.0,
            peek: [false; 4],
            peeks: Default::default(),
            panel_rects: [None; 4],
            spp_expected: SPP_EXPECTED_SECS,
            spp_wait: SPP_DELAY_SECS,
            refine_retries: 0,
            selected_at: None,
            saved: HashMap::new(),
            sidecars_ahead: Arc::default(),
            sidecars_pending: std::collections::HashSet::new(),
            shown_at: 0.0,
            detail_wants_spp: false,
            crop_mode: None,
            upright_msg: None,
            persp_grid_until: f64::NEG_INFINITY,
            straighten: false,
            straighten_from: None,
            straighten_drag: false,
            geo_tool: Default::default(),
            crop_raster: Default::default(),
            crop_rotate_start: None,
            show_toolbar: true,
            toolbar_gap: 0.0,
            toolbar_widths: Default::default(),
            zoom_compact: false,
            zoom_narrow: false,
            brand: brand::Brand::new(),
            pending_paste: false,
            menu: if cfg!(test) { None } else { menu::AppMenu::install(ctx, hwnd) },
            hsl_tab: 1,
            curve_channel: 0,
            crop_aspect: None,
            crop_drag: None,
            hover_preset: None,
            preset_images: Default::default(),
            hover_tex: None,
            preset_cursor: None,
            show_shortcuts: false,
            one_to_one: 3.0,
            fit_size: egui::Vec2::ZERO,
            view_size: egui::Vec2::ZERO,
            readout: None,
            wb_picker: false,
            wb_pick: wb_picker::State::default(),
            measure: false,
            measure_start: None,
            measure_rect: None,
            stats: None,
            layers_status: String::new(),
            layers_flat: false,
            camf: camf_view::CamfInspector::default(),
            sensor_raw: sensor_raw::SensorRaw::default(),
            display: photo_display::State::default(),
            video: video_ui::Player::default(),
            video_view: None,
            full_job: None,
            edit_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            full_failed: None,
            full_waiting: None,
            detail_settings_since: None,
            detail_unsharp_since: None,
            detail_spinner: (0.0, 0.0),
            quiet_spinners: [(0.0, 0.0, None); 2],
            grid: false,
            grid_size: 190.0,
            grid_cols: 1,
            export_format: Format::Jpeg { quality: 92 },
            export_engine: Engine::Spp,
            export_dir: None,
            export_naming: serde_json::from_value(library::settings::value("exportNaming")).unwrap_or_default(),
            export_names_ui: Default::default(),
            export_status: String::new(),
            exporting: false,
            show_export: false,
            export_scope: 0,
            export_progress: (0, 0),
            reveal_after_export: true,
            // the previous run ended with a crash: say so once and offer the report (details in the crash log)
            status: if crash_log::take_notice() {
                bug_report::open(true);
                crate::tr("Zuletzt unerwartet beendet – Hilfe → Fehler melden …").to_string()
            } else {
                String::new()
            },
            lib: library_ui::LibraryView::new(ctx),
            source_version: u64::MAX,
            not_developable: None,
            damaged: None,
            offline_zoom: (0, 1.0, egui::Vec2::ZERO),
            tool_tab: workspace::ToolTab::Info,
            navigation: media_library::Navigation::new(ctx.clone()),
            scroll_trackpad: false,
            multi_base: None,
            multi: Default::default(),
            thumbnails_valid: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            thumbnails_pending: std::collections::BTreeSet::new(),
            rough_views_checked: false,
            pending_thumb: None,
            pending_wide: None,
            thumbnails_running: std::collections::BTreeSet::new(),
        };
        // Export defaults from the preferences (⌘, → Export)
        let ev = library::settings::value("export");
        if !ev.is_null() {
            app.export_format = match ev["format"].as_str() {
                Some("tiff") => Format::Tiff16,
                Some("dng") => Format::Dng,
                Some("jxl") => Format::JXL_DEFAULT,
                _ => Format::Jpeg { quality: ev["quality"].as_u64().unwrap_or(92).clamp(1, 100) as u8 },
            };
            app.export_engine = if ev["engine"] == "own" { Engine::Own } else { Engine::Spp };
            app.reveal_after_export = ev["reveal"].as_bool().unwrap_or(true);
        }
        // the export folder chosen last (a folder opened below suggests its own "export" folder instead)
        app.export_dir = library::settings::value("exportDir").as_str().filter(|d| !d.is_empty()).map(PathBuf::from).filter(|d| library::write_target::check(d).is_ok());
        // The image source is the library's (last chosen source). A folder or file
        // from the command line (test runs) is shown like ⌘O as "Browse folder".
        // Test hook GRAINEO_IMAGE=<path to a file> (as the scripts use it) behaves like that file on the command line;
        // GRAINEO_IMAGE=<number> stays the index in the filmstrip (below). Before 0.108 a path was silently ignored and
        // the develop view showed the last chosen photo.
        let image_path = crate::env::var("GRAINEO_IMAGE").ok().filter(|v| v.trim().parse::<usize>().is_err()).map(PathBuf::from).filter(|p| p.is_file());
        let arg = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.exists()).or(image_path);
        if let Some(p) = arg {
            app.open_path(p);
        }
        app.sync_source(true);
        // Windows: an Explorer menu entry that starts another graineo.exe (the unzipped folder was moved) follows
        if library::platform::explorer_menu::AVAILABLE {
            let label = crate::tr(prefs::EXPLORER_MENU_LABEL).to_string();
            let _ = std::thread::Builder::new().name("explorer-menu".into()).spawn(move || library::platform::explorer_menu::renew(&label));
        }
        if !app.entries.is_empty() { app.status.clear(); }
        // Prepare the GPU for full resolution of non-Sigma RAWs (compile shaders)
        std::thread::spawn(rawdev::gpu::warm_up);
        // Test hook: GRAINEO_IMAGE=<no. in filmstrip> activates this image
        let test_image = crate::env::var("GRAINEO_IMAGE").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&i| i < app.entries.len());
        if let Some(i) = test_image {
            let b = app.entries[i].image;
            app.lib.set_active(b);
        }
        // Test hook: GRAINEO_SELECTION="0,2,3" selects these images in the filmstrip and in the grid of the library (after
        // GRAINEO_IMAGE, which would clear it); the main photo is GRAINEO_IMAGE if it is one of them, otherwise the first
        if let Ok(v) = crate::env::var("GRAINEO_SELECTION") {
            let chosen: Vec<usize> = v.split(',').filter_map(|t| t.trim().parse::<usize>().ok()).filter(|&i| i < app.entries.len()).collect();
            for &i in &chosen {
                app.entries[i].selected = true;
            }
            if let Some(&first) = chosen.first() {
                let main = test_image.filter(|i| chosen.contains(i)).unwrap_or(first);
                let images: Vec<i64> = chosen.iter().map(|&i| app.entries[i].image).collect();
                app.lib.set_selection(app.entries[main].image, &images);
            }
        }
        // Test hook: start with the Sensor module or grid (GRAINEO_TAB=sensor|film|grid|crop|about|prefs|zoom|library|develop|darkroom…)
        match crate::env::var("GRAINEO_TAB").as_deref() {
            Ok("sensor") => app.tab = Tab::Sensor,
            Ok("film" | "look") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::FilmLook); app.profile_browser = true; app.profile_choice.area = film_look::Area::Look; }
            Ok("crop") => { app.tab = Tab::Develop; app.test_open = Some("crop"); }
            Ok("align") => { app.tab = Tab::Develop; app.test_open = Some("align"); }
            Ok("perspective") => { app.tab = Tab::Develop; app.test_open = Some("perspective"); }
            Ok("about") => app.brand.show_about(),
            Ok("prefs") => app.test_open = Some("prefs"),
            Ok(t) if t.starts_with("prefs:") => prefs::open_section(&t[6..]),
            Ok("zoom") => { app.tab = Tab::Develop; app.test_open = Some("zoom"); }
            Ok("print") => { app.tab = Tab::Develop; app.test_open = Some("print"); }
            Ok("grid") => app.grid = true,
            Ok("export") => app.show_export = true,
            Ok("library") => app.tab = Tab::Library,
            Ok("develop") => app.tab = Tab::Develop,
            Ok("darkroom") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
                // with GRAINEO_ZOOM=<factor>: zoomed in as with GRAINEO_TAB=zoom (screenshots of the film shelf's crops)
                if crate::env::var_os("GRAINEO_ZOOM").is_some() { app.test_open = Some("zoom"); }
            }
            Ok("darkroom-compare") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
                match crate::env::var("GRAINEO_DARKROOM_COMPARE") {
                    Ok(b) if !b.is_empty() => { app.darkroom.compare.without_film(); app.darkroom.compare.test_b = Some(b); }
                    _ => app.darkroom.compare.without_film(),
                }
            }
            Ok("darkroom-tools") if crate::env::var_os("GRAINEO_DATA_DIR").is_some() => {
                app.tab=Tab::Develop;app.darkroom.active=true;app.darkroom.advanced_open=Some(true);
                app.darkroom.probe_light.an=crate::env::var("GRAINEO_DARKROOM_TEST_PROBE").as_deref()==Ok("1");
                if let Ok(f)=crate::env::var("GRAINEO_DARKROOM_FILM"){app.darkroom.test_film(&f);}
            }
            Ok("darkroom-zones") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                app.darkroom.zones.an = true;
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
            }
            Ok("darkroom-border") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                app.darkroom.on_paper = crate::env::var("GRAINEO_DARKROOM_ON_PAPER").is_ok_and(|v| v == "1");
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
                if let Ok(r) = crate::env::var("GRAINEO_DARKROOM_BORDER") {
                    let v: Vec<f32> = r.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if v.len() >= 3 { app.darkroom.test_border(v[0], v[1], v[2]); }
                }
            }
            Ok("darkroom-dodge") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
                if let Ok(r) = crate::env::var("GRAINEO_DARKROOM_DODGE") {
                    let v: Vec<f32> = r.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if v.len() >= 2 { app.darkroom.test_dodge(v[0] as usize, v[1]); }
                }
            }
            Ok("darkroom-recipes") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                app.darkroom.recipes.opened = true;
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
            }
            Ok("darkroom-contact") => {
                app.tab = Tab::Develop;
                app.darkroom.active = true;
                app.contact_view(Some(crate::env::var("GRAINEO_DARKROOM_PRINT").is_ok_and(|v| v == "1")));
                if let Ok(f) = crate::env::var("GRAINEO_DARKROOM_FILM") { app.darkroom.test_film(&f); }
            }
            Ok("darkroom-card") => { app.tab = Tab::Develop; app.darkroom.active = true; app.darkroom.test_card("fujifilm_acros_100_ii"); }
            Ok("darkroom-paper") => { app.tab = Tab::Develop; app.darkroom.active = true; app.darkroom.test_card("ilford_multigrade_fb_classic"); }
            Ok("info") => { app.tab = Tab::Develop; app.tool_tab = workspace::ToolTab::Info; }
            Ok("edit") => { app.tab = Tab::Develop; app.tool_tab = workspace::ToolTab::Edit; }
            Ok("filmlook") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::FilmLook); }
            Ok("masks") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::Masks); }
            Ok("retouch") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::Retouch); }
            Ok("versions") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::Versions); }
            Ok("merge") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::Merge); }
            Ok("presets") => { app.tab = Tab::Develop; app.tool_open = Some(workspace::ToolTab::Presets); }
            Ok("loupe") => {
                app.tab = Tab::Library;
                app.lib.loupe = true;
            }
            // without test hook: last used module (Library or Develop)
            // (the app starts in the grid; "develop" in module.txt opens the last active photo in the single view – earlier this
            // value was saved but never applied, the app always started in the grid)
            // Never in unit tests: they share one data folder and this process-wide switch, so another test's
            // "develop" made a test's app leave the grid on its own (flaky `grid_preview_follows_the_edit`).
            _ if !cfg!(test) && std::env::args().nth(1).is_none() && file_migration::start_module().is_some_and(|m| m == "develop") => {
                // the filmstrip is filled only in the first frames: switch there (workspace::restore_view)
                workspace::RESTORE_SINGLE.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            _ => {}
        }
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
            if crate::env::var("GRAINEO_DARKROOM_LEFT").as_deref() == Ok("1") {
                app.darkroom.left_tab = darkroom_layout::LeftTab::Controls;
            }
            app.darkroom.recipes.drawer = crate::env::var("GRAINEO_DARKROOM_DRAWER").is_ok_and(|v| v == "1");
        }
        if !app.in_grid() && crate::env::var("GRAINEO_TAB").as_deref() != Ok("info") {
            app.tool_tab = workspace::ToolTab::Edit;
        }
        app
    }

    /// Open folder (⌘O): shown in the library as a source ("Browse folder") –
    /// Develop always shows exactly the library's list.
    fn open_folder(&mut self, dir: PathBuf) {
        self.open_folder_inner(dir);
    }

    /// A folder or photo from the command line or handed over by a second start: shown like ⌘O (a photo: its
    /// folder, the photo active).
    fn open_path(&mut self, p: PathBuf) {
        if p.is_dir() {
            self.open_folder_inner(p);
        } else if let Some(dir) = p.parent().filter(|_| p.is_file()) {
            self.save_current();
            self.lib.browse_folder(dir, Some(&p));
            self.folder = Some(dir.to_path_buf());
            self.export_dir = export_default(dir);
        }
    }

    /// Paths handed over by a second start (`library::instance_lock::hand_over`): a quiet thread looks for the request
    /// file once a second and wakes the window only when there is one.
    fn open_requests(&mut self, ctx: &egui::Context) {
        static WAITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        static WATCH: std::sync::Once = std::sync::Once::new();
        WATCH.call_once(|| {
            let (ctx, file) = (ctx.clone(), library::instance_lock::request_path(&library::real_db_path()));
            let _ = std::thread::Builder::new().name("open-requests".into()).spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                if file.exists() && !WAITING.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    ctx.request_repaint();
                }
            });
        });
        if !WAITING.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        if let Some(p) = library::instance_lock::take_request(&library::real_db_path()).filter(|p| p.exists()) {
            log(&format!("open request from a second start: {}", p.display()));
            self.open_path(p);
            self.sync_source(true);
            self.to_grid();
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn open_folder_inner(&mut self, dir: PathBuf) {
        self.save_current();
        self.lib.browse_folder(&dir, None);
        self.export_dir = export_default(&dir);
        self.folder = Some(dir);
        self.sync_source(true);
        if !self.entries.is_empty() { self.status.clear(); }
    }

    /// Read an entry's settings from the sidecar on demand.
    fn ensure_entry(&mut self, i: usize) {
        let opened = self.entries.get(i).is_some_and(|e| self.lib.rating_pending(e.image));
        let Some(e) = self.entries.get_mut(i) else { return };
        if e.loaded {
            return;
        }
        let (settings, meta, stored) = sidecar::load_entry(&e.path, e.copy.as_deref());
        self.apply_sidecar(i, settings, meta, stored, opened);
    }

    /// Settings and rating read from the sidecar into the entry (see [`Self::ensure_entry`]).
    fn apply_sidecar(&mut self, i: usize, settings: Settings, meta: sidecar::Meta, stored: bool, opened: bool) {
        let Some(e) = self.entries.get_mut(i) else { return };
        self.saved.insert(e.image, (sidecar_settings(&settings), meta.clone()));
        e.settings = settings;
        rawdev::film_profiles::remember_reference(&mut e.settings);
        // Rating: the sidecar is the source of truth (the library is only an index) – unless
        // a rating that was just set is still being written
        if !opened && (stored || meta != sidecar::Meta::default()) {
            e.meta = meta;
        }
        e.stored = stored;
        e.loaded = true;
    }

    /// Shared image source: filmstrip = visible list of the library, active image =
    /// active image of the library. `force`: rebuild the list even without a new state.
    fn sync_source(&mut self, force: bool) {
        let snapshot = self.lib.list_version();
        if force || snapshot != self.source_version {
            let _t = frame_watch::section("develop list");
            self.source_version = snapshot;
            // "edited" marks of the photos not loaded in this session (read in the background)
            edit_marks::refresh(&self.egui_ctx);
            let list = self.lib.list();
            let alt = std::mem::take(&mut self.entries);
            let mut alt_pos: HashMap<i64, usize> = alt.iter().enumerate().map(|(i, e)| (e.image, i)).collect();
            let mut alt_entries: Vec<Option<Entry>> = alt.into_iter().map(Some).collect();
            let mut new_from_old: HashMap<usize, usize> = HashMap::new();
            self.entries = list
                .into_iter()
                .enumerate()
                .map(|(i, q)| {
                    let meta = sidecar::Meta { rating: q.stars, flag: sidecar::Flag::from_i8(q.flag), label: q.label };
                    // a merged photo is shown without ".dng" (it is no file; the extension comes with the export)
                    let shown = library::merge::shown_name(&q.name, q.state as i64);
                    let display = match &q.copy {
                        Some(k) => format!("{shown} · {k}"),
                        None => shown,
                    };
                    if let Some(old) = alt_pos.remove(&q.image).and_then(|k| {
                        new_from_old.insert(k, i);
                        alt_entries[k].take()
                    }) {
                        return Entry { meta: if old.loaded { old.meta } else { meta }, path: q.path, name: display, format: q.format, state: q.state, aspect: if old.aspect_checked { old.aspect } else { q.aspect.unwrap_or(old.aspect) }, ..old };
                    }
                    Entry {
                        path: q.path,
                        name: display,
                        thumb: None,
                        settings: Settings::default(),
                        meta,
                        selected: false,
                        copy: q.copy,
                        stored: false,
                        image: q.image,
                        file: q.file,
                        format: q.format,
                        state: q.state,
                        loaded: false,
                        aspect: q.aspect.unwrap_or(filmstrip_layout::ASPECT_UNKNOWN),
                        aspect_checked: false,
                    }
                })
                .collect();
            // photos loaded in this session that left the list keep their "edited" mark for when they come back
            for e in alt_entries.iter().flatten().filter(|e| e.loaded) {
                edit_marks::note(e.file, e.copy.as_deref(), binning_ui::edited(&e.settings).then(|| e.settings.film.is_active()));
            }
            // Update indices (image, cache, history); discard what was lost
            let map = |i: usize| new_from_old.get(&i).copied();
            // Indices shifted or images dropped: new list state for running jobs
            if new_from_old.len() != alt_entries.len() || new_from_old.iter().any(|(a, n)| a != n) {
                let e = self.worker.epoch.fetch_add(1, Ordering::SeqCst);
                self.list_remaps.push((e, new_from_old.clone()));
                if self.list_remaps.len() > 64 {
                    self.list_remaps.remove(0);
                }
            }
            // what is loaded must belong to the photo of its new entry (same file), never only to the index
            let belongs = |entries: &[Entry], n: usize, l: &Loaded| entries.get(n).is_some_and(|e| e.path == l.path);
            if let Some(c) = self.current.take() {
                match map(c.index).filter(|&n| belongs(&self.entries, n, &c.loaded)) {
                    Some(n) => self.current = Some(Current { index: n, ..c }),
                    None => {
                        // Image is no longer in the list: settings stay saved
                        self.rendered_for = None;
                        self.display.clear();
                    }
                }
            }
            self.loading = self.loading.and_then(map);
            if self.loading.is_none() {
                self.open_generation.fetch_add(1, Ordering::SeqCst);
            }
            self.cache = std::mem::take(&mut self.cache).into_iter().filter_map(|(i, l)| map(i).filter(|&n| belongs(&self.entries, n, &l)).map(|n| (n, l))).collect();
            // everything else that refers to an image by its index follows it into the new list (or is dropped):
            // a preload still marked as running under an old index otherwise made a click on the photo now at that index
            // wait forever, and its cancel flag hit the wrong photo
            self.preload_running = std::mem::take(&mut self.preload_running).into_iter().filter_map(map).collect();
            self.prefetch_flags = std::mem::take(&mut self.prefetch_flags)
                .into_iter()
                .filter_map(|(i, flag)| match map(i) {
                    Some(n) => Some((n, flag)),
                    None => {
                        flag.store(true, Ordering::SeqCst);
                        None
                    }
                })
                .collect();
            self.browse.remap(&map);
            self.pending_thumb = self.pending_thumb.take().and_then(|(i, s, w, h, rgb)| map(i).map(|n| (n, s, w, h, rgb)));
            self.pending_wide = self.pending_wide.take().and_then(|(i, s)| map(i).map(|n| (n, s)));
            self.not_developable = self.not_developable.and_then(map);
            self.damaged = self.damaged.and_then(map);
            self.committed = self.committed.take().and_then(|(i, s)| map(i).map(|n| (n, s)));
            self.rendered_for = self.rendered_for.take().and_then(|(i, s)| map(i).map(|n| (n, s)));
            self.panes.reset();
            self.film_previews.remap(&map);
            if let Some((_, cancel)) = self.spp_prefetching.take() {
                cancel.store(true, Ordering::SeqCst);
            }
            self.spp_early = self.spp_early.and_then(map);
            self.spp_counts = None;
            self.selected_at = None;
            if self.export_dir.is_none() {
                self.export_dir = self.entries.first().and_then(|e| e.path.parent().and_then(export_default));
            }
        }
        // open the active image: in single view, in the grid for the "Edit" tab
        if self.tab != Tab::Library || (self.tool_tab.editing() && self.show_right) {
            if let Some(b) = self.lib.active_image() {
                let shown = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable);
                if shown.is_none_or(|i| self.entries.get(i).is_none_or(|e| e.image != b)) {
                    if let Some(i) = self.entries.iter().position(|e| e.image == b) {
                        self.select(i, false);
                    }
                }
            } else if self.current.is_none() && self.loading.is_none() && !self.entries.is_empty() {
                self.select(0, false);
            }
        }
    }

    /// Switch image. `keep_view`: keep zoom and crop (⌥ + arrow).
    fn select(&mut self, index: usize, keep_view: bool) {
        if index >= self.entries.len() || self.loading == Some(index) || self.current.as_ref().is_some_and(|c| c.index == index) {
            return;
        }
        let _t = frame_watch::section("select");
        {
            let _t = frame_watch::section("select: save");
            self.save_current();
        }
        {
            let _t = frame_watch::section("select: sidecar");
            self.ensure_entry(index);
        }
        // shared image source: the library shows the same active image
        self.lib.set_active(self.entries[index].image);
        if !image_kind::of(&self.entries[index].path).editable() || !developable_state(self.entries[index].state) {
            // not developable (JPEG, video …) or unreachable: thumbnail stays
            if let Some(old) = self.current.take() {
                self.cache.retain(|(i, _)| *i != old.index);
                self.cache.push((old.index, old.loaded));
            }
            self.loading = None;
            self.open_generation.fetch_add(1, Ordering::SeqCst);
            // the badge in the image says why (and a missing original is a condition of the photo, see
            // `photo_notices`) – no message
            self.status = String::new();
            self.not_developable = Some(index);
            return;
        }
        self.not_developable = None;
        self.damaged = None;
        if self.kept_view.as_ref().is_some_and(|(p, _)| *p != self.entries[index].path) {
            self.kept_view = None;
        }
        self.hold_jpeg = None;
        log(&format!("select {index}"));
        self.selected_at = Some((index, self.now()));
        self.browse.switched(self.now());
        jobs::note_browsing();
        self.browse.open_due = None;
        self.browse.full_due = None;
        self.browse.hold = None;
        self.request_large_ahead(index);
        self.try_hold(index);
        // If exactly this image is currently being preloaded (e.g. on the first click in the grid), the click waits for it instead of reloading
        let waiting = self.preload_running.contains(&index);
        self.spp_cancel.store(true, Ordering::SeqCst);
        // preloads of the new image and its near neighbours keep running (they are needed next)
        self.cancel_prefetches(|n| n.abs_diff(index) <= PREFETCH_REACH);
        self.spp_pending = None;
        // early computation of the previous image is cancelled – never leave the flag set
        self.spp_early = None;
        self.spp_counts = None;
        self.spp_due = None;
        self.spp_settle = None;
        self.spp_seconds = None;
        self.spp_wait = SPP_DELAY_SECS;
        self.refine = Refine::None;
        self.refine_retries = 0;
        self.prefetch_due = None;
        self.detail_pending = false;
        self.cancel_full_job();
        self.full_failed = None;
        self.detail_unsharp_since = None;
        if !keep_view {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        }
        // Previous image into the cache (without large full-resolution data)
        if let Some(old) = self.current.take() {
            // never wait for the session in the UI thread (a background job may be
            // holding it) – otherwise the app freezes when switching images
            if let Some(doc) = &old.loaded.previews.foreign {
                doc.release_memory();
            }
            match old.loaded.session.as_ref().map(|s| s.try_lock()) {
                Some(Ok(mut sess)) => sess.release_heavy(),
                Some(Err(_)) => {
                    let session = old.loaded.session.clone().unwrap();
                    std::thread::spawn(move || {
                        if let Ok(mut s) = session.lock() {
                            s.release_heavy();
                        }
                    });
                }
                None => {}
            }
            self.cache.retain(|(i, _)| *i != old.index);
            self.cache.push((old.index, old.loaded));
        }
        self.trim_cache(index);
        self.open_generation.fetch_add(1, Ordering::SeqCst);
        // a preloaded photo with another binning (settings changed since) is opened again
        self.cache.retain(|(i, l)| *i != index || (binning_ui::fits(l, &self.entries[index].settings) && apple_raw::engine_fits(l, &self.entries[index].settings)));
        if let Some(pos) = self.cache.iter().position(|(i, _)| *i == index) {
            let (_, loaded) = self.cache.remove(pos);
            self.loading = None;
            self.show(index, loaded);
            return;
        }
        self.loading = Some(index);
        if waiting {
            self.status = crate::trf!("Lade {a} …", a = self.entries[index].name);
            return;
        }
        // the selected image is opened: nothing else reads RAWs meanwhile (preloads compete for processor and NAS)
        self.cancel_prefetches(|n| n == index);
        // Sigma X3F chosen on its own (not while paging): opened at once, as the user waits for exactly this photo
        // (measured: the dwell cost 0.25–0.4 s until the first picture, the stored preview of other formats hides it)
        if image_kind::of(&self.entries[index].path) == image_kind::ImageKind::Foveon && !self.browse.paging(self.now()) {
            self.start_open(index);
            return;
        }
        // paging (and every other format): the stored preview is on screen; the photo is opened only after a short
        // dwell (paging on cancels it), so paging past a photo reads and computes nothing for it
        self.status = crate::trf!("Lade {a} …", a = self.entries[index].name);
        self.browse.open_due = Some((index, self.now() + browse::OPEN_DWELL));
        self.worker.ctx().request_repaint_after(std::time::Duration::from_secs_f64(browse::OPEN_DWELL));
    }

    /// Open the selected image (after the dwell, see [`browse::OPEN_DWELL`]).
    fn start_open(&mut self, index: usize) {
        // the photo the user looks at now: only its Sigma base goes into the disk cache (if the user stays with it)
        develop::diskcache::set_current(&self.entries[index].path);
        // Start the SPP base immediately, in parallel with opening (rather than after opening) – unless the user is
        // paging fast (then it starts once they stay, see `schedule`) or the operating mode waits for a longer dwell
        self.spp_counts = None;
        self.spp_early = None;
        let now = self.now();
        // (not for Classic cameras: they open with the exact view, see `jobs::classic_exact_preview`)
        if perf_mode::plan().early_base
            && !self.browse.fast(now)
            && develop::Session::uses_spp(&self.entries[index].settings)
            && image_kind::of(&self.entries[index].path) == image_kind::ImageKind::Foveon
            && self.spp_prefetching.as_ref().is_none_or(|(i, _)| *i != index)
            && !develop::quattro::is_classic_cached(&self.entries[index].path)
        {
            if let Some((_, cancel)) = self.spp_prefetching.take() {
                cancel.store(true, Ordering::SeqCst);
            }
            self.spp_cancel = Arc::new(AtomicBool::new(false));
            let mode = self.entries[index].settings.spp_key(&self.entries[index].path);
            self.spp_pending = Some(mode.clone());
            self.spp_early = Some(index);
            self.refine = Refine::Working { since: self.now() - SPP_DELAY_SECS };
            self.worker.send(Job::Spp {
                index,
                path: self.entries[index].path.clone(),
                session: None,
                mode,
                cancel: self.spp_cancel.clone(),
                prefetch: false,
            });
        }
        let e = &self.entries[index];
        self.status = crate::trf!("Lade {a} …", a = e.name);
        self.worker.send(Job::Open {
            index,
            path: e.path.clone(),
            settings: e.settings.clone(),
            generation: self.open_generation.load(Ordering::SeqCst),
            current: self.open_generation.clone(),
        });
    }

    /// Memory shortage of the system: preloaded neighbours leave the cache (`mem_budget.rs`). Looks every 2 s; in
    /// normal operation nothing changes. The photo on screen with its whole image stays in every case.
    fn relieve_memory(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.pressure_checked < 2.0 {
            return;
        }
        self.pressure_checked = now;
        let pressure = library::platform::memory_pressure();
        if pressure == library::platform::Pressure::Normal {
            return;
        }
        // a Sigma base computed ahead (≈ 1 GB, minutes when the system swaps) gives way at once (QA 10, H-1)
        if let Some((i, cancel)) = self.spp_prefetching.take() {
            cancel.store(true, Ordering::SeqCst);
            log(&format!("memory pressure {pressure:?}: Sigma base ahead of {i} cancelled"));
        }
        let Some(cur) = self.current.as_ref().map(|c| c.index) else { return };
        let cached: Vec<usize> = self.cache.iter().map(|(i, _)| *i).collect();
        let drop = mem_budget::cache_to_drop(pressure, &cached, cur);
        if drop.is_empty() {
            return;
        }
        log(&format!("memory pressure {pressure:?}: {} preloaded photo(s) released", drop.len()));
        if pressure == library::platform::Pressure::Critical {
            self.cancel_prefetches(|_| false);
        }
        for (_, l) in self.cache.iter().filter(|(i, _)| drop.contains(i)) {
            if let Some(doc) = &l.previews.foreign {
                doc.release_memory();
            }
            if let Some(Ok(mut sess)) = l.session.as_ref().map(|s| s.try_lock()) {
                sess.release_heavy();
            }
        }
        self.cache.retain(|(i, _)| !drop.contains(i));
    }

    /// Limit the cache to the nearest neighbours of the target.
    fn trim_cache(&mut self, target: usize) {
        // the photos of the comparison stay (photo_compare_ui.rs, at most four)
        let keep = self.cmp.pool.clone();
        let limit = perf_mode::plan().cache_limit.max(keep.len());
        while self.cache.len() > limit {
            let Some(far) = self
                .cache
                .iter()
                .enumerate()
                .filter(|(_, (i, _))| !keep.contains(i))
                .max_by_key(|(_, (i, _))| i.abs_diff(target))
                .map(|(pos, _)| pos)
            else {
                break;
            };
            self.cache.remove(far);
        }
    }

    /// Show a loaded image.
    fn show(&mut self, index: usize, loaded: Loaded) {
        let _t = frame_watch::section("show");
        if self.entries.get(index).is_none_or(|e| e.path != loaded.path) {
            log(&format!("show {index}: loaded photo {} does not belong to this entry, discarded", loaded.path.display()));
            return;
        }
        // the wide-gamut/HDR layer still holds the previous photo: it must never lie over this one
        self.display.clear();
        // damaged Sigma file, partly rescued: a condition of the photo (yellow line in the image info, mark in the
        // toolbar), not a message
        note_damage(&loaded);
        self.status = self.entries[index].name.clone();
        let foreign = loaded.previews.foreign.is_some();
        // Non-Sigma RAW without own settings: take the initial values from Lightroom
        if foreign && !self.entries[index].stored {
            let copy = self.entries[index].copy.clone();
            if let Some(lr_s) = resolve_lr_start(&loaded, copy.as_deref()) {
                let e = &mut self.entries[index];
                if e.settings == Settings::default() {
                    e.settings = Settings { view: e.settings.view, ..lr_s.clone() };
                }
                raw_app::set_start_values(&e.path, e.copy.as_deref(), &lr_s);
                // the original keeps its initial values out of the sidecar (see `raw_app::save_settings`): leaving it
                // unchanged writes nothing, so the sidecar need not be read either
                let now = sidecar_settings(&e.settings);
                if e.copy.is_none() && raw_app::start_value(&e.path, None).is_some_and(|st| st == now) {
                    if let Some(b) = self.saved.get_mut(&e.image).filter(|(s, _)| *s == sidecar_settings(&Settings::default())) {
                        b.0 = now;
                    }
                }
            }
        }
        let has_spp = loaded.previews.spp.is_some();
        self.shown_at = self.now();
        if let Some((i, t)) = self.selected_at.filter(|(i, _)| *i == index) {
            log(&format!("time: {i} shown after {:.0} ms{}", (self.now() - t) * 1000.0, if has_spp { ", already exact" } else { "" }));
            timing_log::shown(&loaded, (self.now() - t) * 1000.0, has_spp);
        }
        self.current = Some(Current {
            index,
            loaded,
            texture: None,
            camera_jpeg: None,
            out_hist: None,
            scope_tex: None,
            wave_tex: None,
            clip_tex: None,
            detail: None,
            detail_prev: None,
            detail_left: None,
            detail_hover: None,
            detail_grain: None,
            detail_grain_scale: 1,
            detail_left_scale: 1,
            spp_ready_cache: std::cell::Cell::new(None),
            fade_armed: None,
            fade: None,
        });
        self.panes.reset();
        self.look_compare.end();
        self.before.menu_open = false;
        self.rendered_for = None;
        self.committed = Some((index, Settings { view: View::Developed, ..self.entries[index].settings.clone() }));
        // the develop history of the photo and its neighbours comes from the library in the background
        let keys: Vec<history::Key> = (index.saturating_sub(1)..(index + 2).min(self.entries.len())).map(|i| self.history_key(i)).collect();
        self.history.request(keys);
        // already developed during preloading with the same settings: show immediately, without delay and without a change in appearance
        if let Some((vs, w, h, rgb)) = self.current.as_mut().and_then(|c| c.loaded.prerendered.take()) {
            if vs == self.entries[index].settings {
                let ctx = self.worker.ctx().clone();
                self.apply_rendered(&ctx, index, vs, w, h, rgb);
            }
        }
        let now = self.now();
        if has_spp {
            // SPP preview from the cache, or opened exact (Classic) – done; full resolution only when needed. The base
            // job started with the click is not needed any more.
            if self.spp_early == Some(index) {
                self.spp_cancel.store(true, Ordering::SeqCst);
                self.spp_early = None;
                self.spp_pending = None;
                self.spp_counts = None;
            }
            photo_state::clear(&self.entries[index].path, photo_state::Kind::SigmaExact);
            self.refine = Refine::None;
            self.prefetch_due = Some(now + 0.3);
        } else if self.spp_early == Some(index) {
            // already running since the click; apply finished counts right away
            if let Some((i, mode, counts, seconds)) = self.spp_counts.take() {
                if i == index {
                    if let Some(session) = self.current.as_ref().unwrap().loaded.session.clone() {
                        self.worker.send(Job::SppInstall { index, session, mode, counts, seconds });
                    }
                }
            }
        } else if self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == index) {
            // precomputation already running – do not restart, just wait
            self.spp_pending = Some(self.entries[index].settings.spp_key(&self.entries[index].path));
            self.refine = Refine::Working { since: now - SPP_DELAY_SECS };
        } else if develop::Session::uses_spp(&self.entries[index].settings) && !foreign {
            // the exact base after the dwell of the operating mode (`perf_mode`); the spinner says it is coming
            let wait = perf_mode::plan().base_dwell.max(SPP_DELAY_SECS);
            self.spp_due = Some(now + wait);
            self.spp_wait = wait;
            self.refine = Refine::Working { since: now };
        } else {
            // non-Sigma RAW: preload the neighbours after a short dwell (never while paging fast, see `schedule`)
            self.prefetch_due = Some(now + if foreign { browse::PREFETCH_DWELL } else { 0.3 });
            // and prepare the full resolution once the image has been looked at for a moment
            if foreign {
                let since = self.selected_at.filter(|(i, _)| *i == index).map_or(0.0, |(_, t)| now - t);
                self.browse.full_due = Some((index, now + (perf_mode::plan().full_dwell - since).max(0.3)));
            }
        }
    }

    fn now(&self) -> f64 {
        self.worker.ctx().input(|i| i.time)
    }

    fn save_current(&mut self) {
        // stress test with slider changes: nothing is saved
        if self.stress.as_ref().is_some_and(|s| s.edits_enabled()) {
            return;
        }
        if let Some(c) = &self.current {
            let Some(e) = self.entries.get(c.index) else { return };
            if !e.loaded {
                return;
            }
            // nothing changed since the sidecar was read or written: no file access (other formats; Sigma as before)
            let now = (sidecar_settings(&e.settings), e.meta.clone());
            if image_kind::of(&e.path) != image_kind::ImageKind::Foveon && self.saved.get(&e.image) == Some(&now) {
                // already saved: only steps of the develop history may be waiting (a cursor move back and forth)
                library::store::send_history();
                return;
            }
            let image = e.image;
            match save_entry(e) {
                Ok(()) => {
                    self.saved.insert(image, now);
                    // the steps of the develop history follow the saved state
                    library::store::send_history();
                }
                Err(err) => {
                    self.saved.remove(&image);
                    self.status = crate::trf!("Einstellungen nicht gespeichert: {err}", err = err);
                }
            }
        }
    }

    fn settings(&self) -> Option<&Settings> {
        self.current.as_ref().map(|c| &self.entries[c.index].settings)
    }

    /// Take over the result of the preview development: textures, histogram, clipping, thumbnail, wide-gamut display. Also for an image
    /// already developed during preloading (see [`jobs::Loaded::prerendered`]).
    fn apply_rendered(&mut self, ctx: &egui::Context, index: usize, settings: Settings, width: usize, height: usize, rgb: Vec<u8>) {
                    if self.darkroom.active && self.darkroom.zones.an && self.current.as_ref().is_some_and(|c| c.index == index) {
            self.darkroom.zones.remember(width, height, &rgb);
        }
        // the view of the open photo for ⌘C (photo_copy.rs)
        let view = self.entries.get(index).filter(|_| self.current.as_ref().is_some_and(|c| c.index == index)).map(|e| photo_copy::remember_view(e.image, e.copy.clone(), width, height, &rgb));
        if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
            c.out_hist = Some(rgb_histogram(&rgb));
            // compute parade/waveform only in the Sensor module (a few ms)
            if self.tab == Tab::Sensor {
                let img = lr::scope_image(&rgb, width, height, self.scope);
                match &mut c.scope_tex {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => c.scope_tex = Some(ctx.load_texture("scope", img, TextureOptions::LINEAR)),
                }
            } else {
                c.scope_tex = None;
            }
            // parade, waveform or vectorscope in the header of the Develop panel: on a worker (histogram_pin::request)
            if let (true, Some(px)) = (self.tab == Tab::Develop && self.dev_scope > 0, view) {
                histogram_pin::request(ctx, index, self.dev_scope, width, height, px);
            }
            let clip = clipping_mask(&rgb, width, height);
            match &mut c.clip_tex {
                Some(t) => t.set(clip, TextureOptions::NEAREST),
                None => c.clip_tex = Some(ctx.load_texture("clip", clip, TextureOptions::NEAREST)),
            }
            if crate::env::var_os("GRAINEO_LOG").is_some() {
                log(&format!("render {index} applied ({width}×{height})"));
            }
            let img = ColorImage::from_rgb([width, height], &rgb);
            // the render after the exact base arrived: new texture, the old one stays underneath for the crossfade;
            // never while a slider is dragged (live adjustment stays immediate)
            let now = ctx.input(|i| i.time);
            let armed = c.fade_armed.take().is_some_and(|t| now - t < crossfade::ARM_WINDOW) && !ctx.input(|i| i.pointer.any_down());
            c.fade = None;
            match (&mut c.texture, armed) {
                (Some(t), true) => {
                    let new = ctx.load_texture("preview", img, TextureOptions::LINEAR);
                    let old = std::mem::replace(t, new);
                    c.fade = crossfade::Fade::new(old, now);
                    ctx.request_repaint();
                }
                (Some(t), false) => t.set(img, TextureOptions::LINEAR),
                (None, _) => c.texture = Some(ctx.load_texture("preview", img, TextureOptions::LINEAR)),
            }
            // the first render of a photo opened again takes the place of the picture kept for it
            self.kept_view = None;
            self.hold_jpeg = None;
            // HDR gain map: developed image × map as photo texture (in the background)
            match c.loaded.previews.foreign.as_ref().and_then(|d| d.hdr.clone()) {
                Some(k) => self.display.new_hdr_preview(ctx, index, settings.clone(), width, height, rgb.clone(), k),
                None => {
                    // preserved gamut for displays with a wider colour space (Develop, without film; other formats and
                    // Sigma alike, once per settings state from the preview – zoom and pan compute nothing)
                    if photo_display::wide_active() && !settings.film.is_active() {
                        if let Some(job) = jobs::wide_job(index, &c.loaded, &settings) {
                            if ctx.input(|i| i.pointer.any_down()) {
                                // a slider is being dragged: only for the last step, on release
                                self.pending_wide = Some((index, settings.clone()));
                            } else {
                                self.pending_wide = None;
                                self.worker.send(job);
                            }
                        }
                    }
                }
            }
            // The thumbnail shows the development, not just the camera JPEG. While a slider
            // is being dragged, only remember the last result and apply it on release: the last result
            // usually arrives while still dragging; nothing is computed afterwards (otherwise the preview would stay stale).
            if settings.view == View::Developed && !settings.tone_eq_mask {
                if ctx.input(|i| i.pointer.any_down()) {
                    // make it visible while dragging (display only, without saving)
                    self.live_thumb(ctx, index, &settings, width, height, &rgb);
                    self.pending_thumb = Some((index, settings.clone(), width, height, rgb));
                } else {
                    self.pending_thumb = None;
                    self.thumb_from_render(ctx, index, &settings, width, height, &rgb);
                }
            }
            self.rendered_for = Some((index, settings));
        }
    }

    /// While a slider is dragged: grid and filmstrip show the change right away (smaller, display
    /// only; saved on release via [`Self::thumb_from_render`]).
    fn live_thumb(&mut self, ctx: &egui::Context, index: usize, settings: &Settings, width: usize, height: usize, rgb: &[u8]) {
        let Some(c) = self.current.as_ref().filter(|c| c.index == index && self.entries.get(index).is_some_and(|e| e.path == c.loaded.path)) else { return };
        // (never the reference frame standing in for a merged photo that is not computed yet)
        if c.loaded.previews.foreign.as_ref().is_some_and(|d| d.stands_in()) {
            return;
        }
        // (never a render from a fast preview too rough to be shown, see `thumb_from_render`)
        if !preview_hold::render_keepable(&c.loaded, settings) {
            return;
        }
        let [fw, fh] = c.loaded.full_size;
        let frame = Frame::from_settings(settings, fw, fh, c.loaded.cal.orientation);
        let small = frame_thumb(rgb, width, height, &frame, 512);
        if self.entries[index].copy.is_none() {
            let file = self.entries[index].file;
            self.lib.store_developed(ctx, file, color_image_to_rgb(&small), false, false);
        }
        match &mut self.entries[index].thumb {
            Some(t) => t.set(small, TextureOptions::LINEAR),
            None => self.entries[index].thumb = Some(ctx.load_texture(format!("thumb-{index}"), small, TextureOptions::LINEAR)),
        }
    }

    /// Thumbnail (filmstrip) and preview (grid, placeholder on opening) from a render result. The reductions run in a
    /// helper thread (three of them per render took a third of the UI thread while paging); the result is taken over
    /// in the next frame ([`Self::take_thumbs`]).
    fn thumb_from_render(&mut self, ctx: &egui::Context, index: usize, settings: &Settings, width: usize, height: usize, rgb: &[u8]) {
        // the render belongs to the current photo; it is only stored for the entry of that very photo
        if index >= self.entries.len() || self.current.as_ref().is_none_or(|c| c.index != index || c.loaded.path != self.entries[index].path) {
            return;
        }
        // A Sigma photo at high ISO rendered without its exact base (the base failed or was given up, or the
        // comparison switch shows the fast preview): that unreduced picture is never kept – no filmstrip thumbnail,
        // no grid tile, no placeholder for the next opening. What was there before stays (the camera's picture).
        if self.current.as_ref().is_some_and(|c| !preview_hold::render_keepable(&c.loaded, settings)) {
            log(&format!("render {index}: from the fast preview of a high-ISO photo, not kept as tile or placeholder"));
            return;
        }
        let image = self.entries[index].image;
        let mut job = ThumbJob { index, image, path: self.entries[index].path.clone(), generation: 0, frame: None, store: None, rgb: rgb.to_vec(), width, height };
        if let Some(c) = self.current.as_ref().filter(|c| c.index == index) {
            let [fw, fh] = c.loaded.full_size;
            job.frame = Some(Frame::from_settings(settings, fw, fh, c.loaded.cal.orientation));
            // a merged photo that is not computed yet shows its reference frame: that picture is never kept as the
            // photo's (placeholder for the next opening, grid preview) – the merge brings its own
            let stands_in = c.loaded.previews.foreign.as_ref().is_some_and(|d| d.stands_in());
            // remember the last seen development: placeholder for the next opening
            if !stands_in && self.developed_stored.as_ref().is_none_or(|(b, s)| *b != image || s != settings) {
                let e = &self.entries[index];
                let fingerprint = browse::expected(settings, e.stored, &e.path, e.copy.as_deref());
                job.store = Some((settings.clone(), fingerprint, e.copy.is_none()));
                self.developed_stored = Some((image, settings.clone()));
            }
        }
        self.thumb_generation += 1;
        job.generation = self.thumb_generation;
        self.thumb_newest.insert(image, job.generation);
        if job.store.is_some() {
            self.thumb_store_newest.insert(image, job.generation);
        }
        if cfg!(test) {
            // tests check the result right away
            let done = job.run();
            self.thumb_newest.remove(&image);
            self.thumb_store_newest.remove(&image);
            self.apply_thumb(ctx, done, true);
            return;
        }
        let out = self.thumbs_done.clone();
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("thumb".into())
            .spawn(move || {
                let done = job.run();
                out.lock().unwrap().push(done);
                c.request_repaint();
            })
            .ok();
    }

    /// Thumbnails reduced in the helper thread: take them over (only the newest per image).
    fn take_thumbs(&mut self, ctx: &egui::Context) {
        let done = std::mem::take(&mut *self.thumbs_done.lock().unwrap());
        for d in done {
            let newest = self.thumb_newest.get(&d.image) == Some(&d.generation);
            if newest {
                self.thumb_newest.remove(&d.image);
            }
            // an overtaken result still stores its developed view if no newer one does (the newer one may only be the
            // thumbnail); the thumbnail is always the newest one
            let mut d = d;
            if self.thumb_store_newest.get(&d.image) == Some(&d.generation) {
                self.thumb_store_newest.remove(&d.image);
            } else {
                d.view = None;
            }
            if newest || d.view.is_some() {
                self.apply_thumb(ctx, d, newest);
            }
        }
    }

    fn apply_thumb(&mut self, ctx: &egui::Context, d: ThumbDone, thumb: bool) {
        let Some(index) = self.entries.get(d.index).filter(|e| e.image == d.image).map(|_| d.index).or_else(|| self.entries.iter().position(|e| e.image == d.image)) else { return };
        if self.entries[index].path != d.path {
            log(&format!("thumbnail of {} does not belong to entry {index}, discarded", d.path.display()));
            return;
        }
        if let Some((view, settings, fingerprint, own)) = d.view {
            developed_cache::store(d.image, &view, fingerprint);
            // grid and filmstrip also show the edit from now on (virtual copies share the file preview with the
            // original and are therefore left out; photos from "Browse folder" without a catalog ID only get the
            // display, nothing on disk)
            if let (true, Some(view_rgb)) = (own, d.view_rgb) {
                let file = self.entries[index].file;
                // the large preview already shows this development: it stays (no new development at full resolution)
                let large_current = browse::large(&self.entries[index].path, library::preview::large()) == Some(fingerprint);
                self.lib.store_developed(ctx, file, view_rgb, file > 0, large_current);
                // the large level at full resolution, in the background once the edit rests
                let offline = self.lib.offline_developed(file);
                large_preview::schedule(file, &self.entries[index].path, &settings, fingerprint, offline);
            }
        }
        if !thumb {
            return;
        }
        match &mut self.entries[index].thumb {
            Some(t) => t.set(d.thumb, TextureOptions::LINEAR),
            None => self.entries[index].thumb = Some(ctx.load_texture(format!("thumb-{index}"), d.thumb, TextureOptions::LINEAR)),
        }
    }

    /// Whole image at full resolution → textures of the zoomed view (only for the current image with unchanged settings).
    fn apply_full(&mut self, ctx: &egui::Context, f: FullWaiting) {
        let FullWaiting { index, settings, width, height, rgb, half, seconds } = f;
        let max_side = ctx.input(|i| i.max_texture_side);
        if let Some(c) = self.current.as_mut().filter(|c| c.index == index && self.entries.get(index).is_some_and(|e| e.settings == settings)) {
            if width < c.loaded.full_size[0] || height < c.loaded.full_size[1] {
                // does not cover the image: crops stay in charge, no new attempt for these settings
                log(&format!("full resolution {index}: whole image {width}×{height} smaller than {:?}", c.loaded.full_size));
                self.full_failed = Some(settings);
                return;
            }
            let t = std::time::Instant::now();
            let tex = DetailTex::new(ctx, "detail-full", width, height, &rgb, [0, 0, width, height], 1, max_side).with_coarse(ctx, half.0, half.1, &half.2, [0, 0, width, height], max_side);
            log(&format!(
                "full resolution {index}: whole image {width}×{height} rendered in {seconds:.2} s, {} tile(s), {:.0} MB textures, prepared in {:.0?}",
                tex.tiles.len(),
                tex.bytes as f64 / 1e6,
                t.elapsed()
            ));
            c.detail = Some((tex, [0, 0, width, height], settings));
            c.detail_prev = None;
        }
    }

    /// Does a result with this origin belong to the photo now at `index` (the current image or a preloaded one)?
    fn origin_fits(&self, index: usize, origin: &jobs::Origin) -> bool {
        let Some(e) = self.entries.get(index) else { return false };
        match origin {
            jobs::Origin::Path(p) => *p == e.path,
            jobs::Origin::Data(_) => {
                let current = self.current.as_ref().filter(|c| c.index == index).map(|c| &c.loaded);
                current.into_iter().chain(self.cache.iter().filter(|(i, _)| *i == index).map(|(_, l)| l)).any(|l| l.is_origin(origin) && l.path == e.path)
                    || (current.is_some() && self.before.binning_alt.is_origin(origin, &e.path))
            }
        }
    }

    fn handle_results(&mut self, ctx: &egui::Context) {
        let _t = frame_watch::section("results");
        self.take_sidecars_ahead();
        self.poll_export_probe();
        self.take_thumbs(ctx);
        // HDR crops converted in the background: next to the 8-bit crop of the same region and settings
        for d in self.display.poll() {
            if let Some(c) = self.current.as_mut().filter(|c| c.index == d.index) {
                if let Some((t, r, s)) = c.detail.as_mut() {
                    if *r == d.region && *s == d.settings {
                        t.hdr = Some(Box::new(DetailTex::from_photo(d.tiles, d.size, d.region, None)));
                    }
                }
            }
        }
        // a whole image held back during a zoom/pan gesture: take it over once the view rests
        if self.tab == Tab::Sensor { self.full_waiting = None; }
        if self.full_waiting.is_some() {
            if ctx.input(|i| i.time) - self.view_changed_at > FULL_UPLOAD_REST {
                if let Some(f) = self.full_waiting.take() {
                    log(&format!("full resolution {}: taken over after the view came to rest", f.index));
                    self.apply_full(ctx, f);
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        while let Some((epoch, origin, mut r)) = self.worker.try_recv() {
            // Map the index onto the current list; image gone or too old → discard
            if let Some(ix) = r.index_mut() {
                let latest = self.worker.epoch.load(Ordering::SeqCst);
                if epoch < latest {
                    let mut stale = self.list_remaps.first().is_none_or(|(e, _)| *e > epoch);
                    for (e, map) in self.list_remaps.iter().filter(|(e, _)| *e >= epoch) {
                        match map.get(ix) {
                            Some(&n) => *ix = n,
                            None => {
                                stale = true;
                                break;
                            }
                        }
                        let _ = e;
                    }
                    if stale {
                        log(&format!("result of an old list state discarded ({epoch} < {latest})"));
                        // the comparison waits for this base: it asks again at once
                        if matches!(r, JobResult::SppReady { prefetch: true, .. } | JobResult::SppFailed { prefetch: true, .. }) {
                            self.compare_base_lost();
                        }
                        continue;
                    }
                }
            }
            // The result must belong to the photo now at its index (not only to the index): otherwise it is dropped;
            // results that end a waiting state still end it, without their content
            if let (Some(o), Some(ix)) = (origin.as_ref(), r.index_mut().map(|i| *i)) {
                if !self.origin_fits(ix, o) {
                    log(&format!("result for {ix} belongs to another photo ({o:?}), discarded"));
                    r = match r {
                        JobResult::Rendered { before: false, .. } => {
                            self.rendering = false;
                            continue;
                        }
                        JobResult::RegionRendered { index, .. } => JobResult::RegionFailed { index, superseded: false },
                        JobResult::FullRendered { index, settings, .. } | JobResult::DetailWide { index, settings, full: true, .. } => JobResult::FullFailed { index, settings, outdated: true, sent: jobs::NO_STAMP },
                        JobResult::DetailWide { index, .. } => JobResult::RegionFailed { index, superseded: false },
                        JobResult::RenderFailed { before: false, .. } => {
                            self.rendering = false;
                            continue;
                        }
                        JobResult::SppReady { prefetch, .. } | JobResult::SppFailed { prefetch, .. } => {
                            if prefetch && self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == ix) {
                                self.spp_prefetching = None;
                            }
                            if prefetch {
                                self.compare_base_lost();
                            }
                            continue;
                        }
                        JobResult::SppCounts { .. } => {
                            if self.spp_early.is_some_and(|i| i == ix) {
                                self.spp_early = None;
                                self.spp_pending = None;
                            }
                            continue;
                        }
                        JobResult::Prepared { .. } | JobResult::PrepareFailed { .. } => {
                            self.preparing = false;
                            continue;
                        }
                        r @ (JobResult::RegionFailed { .. } | JobResult::FullFailed { .. }) => r,
                        // a field of the comparison asks again
                        JobResult::HoverRendered { index, .. } => {
                            self.compare_develop_dropped(index);
                            continue;
                        }
                        _ => continue,
                    };
                }
            }
            match r {
                JobResult::Opened { index, loaded, prefetch } if self.entries.get(index).is_none_or(|e| e.path != loaded.path) => {
                    log(&format!("opened {index}: does not belong to this image ({}), discarded", loaded.path.display()));
                    let _ = prefetch;
                }
                JobResult::Opened { index, loaded, prefetch } => {
                    self.preload_running.retain(|i| *i != index);
                    log(&format!("opened {index} prefetch={prefetch} (loading {:?}): {}", self.loading, loaded.kind.name()));
                    // the condition of the file is known now (also when opened ahead): info panel and grid see it
                    note_damage(&loaded);
                    if self.loading == Some(index) {
                        self.loading = None;
                        self.show(index, loaded);
                    } else if prefetch && (self.current.as_ref().is_some_and(|c| c.index != index) || self.cmp.pool.contains(&index)) {
                        if !self.cache.iter().any(|(i, _)| *i == index) {
                            self.cache.push((index, loaded));
                            let target = self.current.as_ref().map_or(index, |c| c.index);
                            self.trim_cache(target);
                        }
                        self.maybe_prefetch_spp();
                    }
                    self.prefetch_flags.retain(|(i, _)| *i != index);
                    // next neighbour (one preload at a time, nearest first)
                    if prefetch && self.loading.is_none() && self.prefetch_due.is_none() && self.spp_pending.is_none() && !self.browse.fast(ctx.input(|i| i.time)) && crate::env::var_os("GRAINEO_NO_PREFETCH").is_none() {
                        self.prefetch_neighbors();
                    }
                }
                JobResult::PrefetchFailed { index } => {
                    self.preload_running.retain(|i| *i != index);
                    self.prefetch_flags.retain(|(i, _)| *i != index);
                    // a click was waiting for it, now open normally
                    if self.loading == Some(index) {
                        self.loading = None;
                        self.select(index, false);
                    }
                }
                JobResult::OpenFailed { index, error } => {
                    log(&format!("open failed {index} ({}): {error}", self.entries.get(index).map_or_else(String::new, |e| e.path.display().to_string())));
                    if self.loading == Some(index) {
                        self.loading = None;
                        // like not editable: thumbnail stays, no retry per image (loop)
                        if let Some(old) = self.current.take() {
                            self.cache.retain(|(i, _)| *i != old.index);
                            self.cache.push((old.index, old.loaded));
                        }
                        self.not_developable = Some(index);
                        self.kept_view = None;
                        // damaged Sigma file: the badge says so, the camera's preview stays visible
                        self.damaged = jobs::is_damaged(&error).then_some(index);
                    }
                    // a condition of the photo (the badge in the image and the yellow mark say so), not a message
                    if let Some(e) = self.entries.get(index) {
                        photo_state::set(&e.path, if jobs::is_damaged(&error) { photo_state::unreadable(&error) } else { photo_state::open_failed(&error) });
                    }
                }
                JobResult::Prepared { index, previews } => {
                    self.preparing = false;
                    if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
                        let spp = c.loaded.previews.spp.take();
                        c.loaded.previews = previews;
                        c.loaded.previews.spp = spp;
                        self.rendered_for = None;
                        self.panes.invalidate();
                    }
                }
                // the left side of comparisons comes from `HoverRender` (compare_view.rs)
                JobResult::Rendered { before: true, .. } => {}
                JobResult::RenderFailed { index, settings, before } => {
                    if !before {
                        self.rendering = false;
                        // no new attempt for these settings (the last image stays); the next change renders again
                        if self.current.as_ref().is_some_and(|c| c.index == index) {
                            self.rendered_for = Some((index, settings));
                        }
                    }
                    log(&format!("render {index}: not computed"));
                }
                JobResult::PrepareFailed { index, denoise_key } => {
                    self.preparing = false;
                    // the previews stay; marked as made for this noise state so that no loop of new attempts starts
                    if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
                        c.loaded.previews.denoise_key = denoise_key;
                    }
                    log(&format!("prepare {index}: not computed"));
                }
                JobResult::Rendered { index, settings, before: false, width, height, rgb } => {
                    if let Some((file_kind, ms)) = self.edit_log.rendered(&settings) {
                        log(&format!("time: edit {file_kind} rendered after {ms:.0} ms"));
                    }
                    self.rendering = false;
                    self.apply_rendered(ctx, index, settings, width, height, rgb);
                }
                JobResult::Wide { index, settings, region, tex } => {
                    // only accept if the 8-bit preview still belongs to the same settings (shown in its place)
                    if region.is_none() && self.current.as_ref().is_some_and(|c| c.index == index) && self.rendered_for.as_ref().is_some_and(|(i, s)| *i == index && *s == settings) {
                        self.display.set_wide_preview(index, settings, tex);
                    }
                }
                JobResult::DetailWide { index, settings, region, full, size, tiles, half, seconds } => {
                    if full {
                        if self.full_job.as_ref().is_some_and(|(s, _, _)| *s == settings) {
                            self.full_job = None;
                        }
                    } else {
                        self.detail_pending = false;
                    }
                    if full && self.tab == Tab::Sensor { continue; }
                    if self.entries.get(index).is_some_and(|e| e.settings != settings) {
                        // outdated (settings changed while it was computed): never shown, nothing kept – as with
                        // `RegionRendered`/`FullRendered` of the plain jobs
                        log(&format!("detail {index}: {} of old settings dropped (preserved gamut)", if full { "whole image" } else { "crop" }));
                        continue;
                    }
                    if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
                        // never replace the whole image of the same settings by a crop
                        if !full && c.detail.as_ref().is_some_and(|(_, r, ds)| *ds == settings && contains(*r, region)) {
                            continue;
                        }
                        let tex = DetailTex::from_photo(tiles, size, region, half);
                        log(&format!(
                            "{} {index}: {}×{} with preserved gamut in {seconds:.2} s, {} tile(s), {:.0} MB textures (photo textures in all {:.0} MB)",
                            if full { "full resolution" } else { "crop" },
                            size[0],
                            size[1],
                            tex.tiles.len(),
                            tex.bytes as f64 / 1e6,
                            photo_display::gpu_bytes() as f64 / 1e6
                        ));
                        c.detail = Some((tex, region, settings));
                    }
                }
                JobResult::LutExported { result } => {self.darkroom.lut.busy=false;self.darkroom.lut.message=match result{Ok(())=>"LUT gespeichert.".into(),Err(e)=>crate::trf!("LUT konnte nicht gespeichert werden: {e}", e = e)};},
                JobResult::ScanBounds { key,image,path,bounds } => {
                    if self.darkroom.scan.busy==Some(key){self.darkroom.scan.busy=None;}
                    if self.darkroom.scan.desired==Some(key) && self.darkroom_scan_key()==Some(key){
                        if let Some(i)=self.current.as_ref().map(|c|c.index).filter(|&i|self.entries[i].image==image && self.entries[i].path==path && self.entries[i].settings.film.scan_auto){
                            // not computed (crash): nothing taken over, no new attempt for this key
                            if let Some(bounds)=bounds{let f=&mut self.entries[i].settings.film;f.scan_black=bounds.0;f.scan_white=bounds.1;}
                            self.darkroom.scan.ready=Some(key);
                        }
                    }
                }
                JobResult::ToneCurve { key, data } => {
                    let k=&mut self.darkroom.curves;k.busy=false;
                    if k.desired.as_ref().is_some_and(|v|v.0==key){match data{Ok(v)=>{k.data=v;k.error=None;},Err(e)=>k.error=Some(e)}k.ready=Some(key);}
                }
                JobResult::FilmProbe { key, image, seconds } => self.darkroom.probe_light.outcome(ctx,key,image,seconds),
                JobResult::RegionFailed { index, superseded } if self.compare_region_failed(ctx, index, superseded) => {}
                JobResult::RegionFailed { superseded, .. } => {
                    self.detail_pending = false;
                    self.panes.detail_failed();
                    if !superseded {
                        // a real failure: pause before the next attempt (a superseded crop was simply replaced by a newer one)
                        self.detail_blocked_until = ctx.input(|i| i.time) + 3.0;
                        log("full-resolution crop not computed");
                    }
                }
                JobResult::FullFailed { index, settings, outdated, sent } => {
                    // only the job still awaited ends the waiting state; an outdated one (settings or image changed,
                    // list rebuilt) may be asked for again, a failed one not for these settings
                    if self.full_job.as_ref().is_some_and(|(s, st, _)| *s == settings && *st == sent) {
                        self.full_job = None;
                        if outdated {
                            log(&format!("full resolution {index}: whole image dropped (outdated)"));
                        } else {
                            self.full_failed = Some(settings);
                            log(&format!("full resolution {index}: whole image not computed"));
                        }
                    }
                }
                JobResult::FullRendered { index, settings, width, height, rgb, half, seconds } => {
                    if self.full_job.as_ref().is_some_and(|(s, _, _)| *s == settings) {
                        self.full_job = None;
                    }
                    if self.tab == Tab::Sensor { continue; }
                    let f = FullWaiting { index, settings, width, height, rgb, half, seconds };
                    if ctx.input(|i| i.time) - self.view_changed_at > FULL_UPLOAD_REST {
                        self.apply_full(ctx, f);
                    } else {
                        // the view is moving: upload later, otherwise the zoom stops for a moment
                        log(&format!("full resolution {index}: held back until the view rests"));
                        self.full_waiting = Some(f);
                        ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    }
                }
                JobResult::RegionRendered { index, settings, region, width, height, rgb, .. } if self.compare_take_region(ctx, index, &settings, region, width, height, &rgb) => {}
                JobResult::RegionRendered { index, settings, region, width, height, rgb, factor } => {
                    self.detail_pending = false;
                    let is_left = self.darkroom.active && self.darkroom.compare.left_settings(&self.entries[index].settings).as_ref() == Some(&settings);
                    let is_hover = self.hover_preset.as_ref() == Some(&settings) && self.entries.get(index).is_some_and(|e| e.settings != settings);
                    if self.panes.wants_detail(&settings) && self.current.as_ref().is_some_and(|c| c.index == index) {
                        self.panes.accept_detail(ctx, settings, region, factor, ColorImage::from_rgb([width, height], &rgb));
                    } else if let Some(c) = self.current.as_mut().filter(|c| c.index == index && is_hover && !is_left) {
                        let img = ColorImage::from_rgb([width, height], &rgb);
                        c.detail_hover = Some((ctx.load_texture("detail-hover", img, DETAIL_OPTIONS), region, settings));
                    } else if let Some(c) = self.current.as_mut().filter(|c| c.index == index && is_left) {
                        let img = ColorImage::from_rgb([width, height], &rgb);
                        c.detail_left_scale = factor;
                        c.detail_left = Some((ctx.load_texture("detail-links", img, DETAIL_OPTIONS), region, settings));
                    } else if self.entries.get(index).is_some_and(|e| e.settings != settings) {
                        // outdated crop (settings changed while it was computed): never shown, nothing uploaded
                        log(&format!("detail {index}: crop of old settings dropped"));
                    } else if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
                        // never replace the whole image of the same settings by a crop
                        if factor == 1 && c.detail.as_ref().is_some_and(|(_, r, ds)| *ds == settings && contains(*r, region)) {
                            continue;
                        }
                        let max_side = ctx.input(|i| i.max_texture_side);
                        let mut tex = DetailTex::new(ctx, "detail", width, height, &rgb, region, factor, max_side);
                        if factor == 1 {
                            // half level for views below 100 % (`detail_filter`)
                            let (hw, hh, half) = photo_display::half_rgb8(width, height, &rgb);
                            tex = tex.with_coarse(ctx, hw, hh, &half, region, max_side);
                        }
                        // HDR version of the crop (gain map) as photo textures, if the image has one
                        if let Some(k) = c.loaded.previews.foreign.as_ref().filter(|_| factor == 1).and_then(|d| d.hdr.clone()) {
                            self.display.new_hdr_detail(ctx, index, settings.clone(), region, width, height, rgb, k, c.loaded.full_size);
                        }
                        if factor > 1 {
                            // grain loupe in its own slot: the whole image stays in place
                            c.detail_grain_scale = factor;
                            c.detail_grain = Some((tex, region, settings));
                        } else {
                            c.detail = Some((tex, region, settings));
                        }
                    }
                }
                JobResult::SppReady { index, mode, preview, seconds, prefetch } => {
                    if self.stress.as_mut().is_some_and(|s| s.drop_result()) {
                        log(&format!("stress: SppReady {index} discarded (fault simulation)"));
                        continue;
                    }
                    // made for the settings of the photo now? Otherwise it belongs to settings changed meanwhile and is
                    // never shown (see `spp_accept.rs`)
                    let wanted = self.entries.get(index).map(|e| e.settings.spp_key(&e.path));
                    let fits = wanted.as_ref().is_some_and(|w| spp_accept::arrival(&mode, w) == spp_accept::Arrival::Show);
                    if prefetch {
                        if self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == index) {
                            self.spp_prefetching = None;
                        }
                        log(&format!("spp precomputed {index} in {seconds:.1}s"));
                        self.compare_base_done(index, false);
                        let is_current = self.current.as_ref().is_some_and(|c| c.index == index);
                        if !is_current {
                            if let Some((_, l)) = self.cache.iter_mut().find(|(i, _)| *i == index).filter(|_| fits) {
                                l.previews.spp = Some(preview);
                            }
                            continue;
                        }
                    }
                    if self.spp_pending.as_ref() == Some(&mode) || prefetch {
                        self.spp_pending = None;
                    }
                    if self.spp_early == Some(index) {
                        self.spp_early = None;
                    }
                    if !fits && self.current.as_ref().is_some_and(|c| c.index == index) {
                        log(&format!("spp ready {index} in {seconds:.1}s: base of old settings dropped ({})", if mode.mono { "black & white" } else { "colour" }));
                        // the base for the settings now, unless it is on its way already
                        if self.spp_pending.as_ref() != wanted.as_ref() && self.spp_due.is_none() && develop::Session::uses_spp(&self.entries[index].settings) {
                            self.request_spp(index);
                        }
                        continue;
                    }
                    log(&format!("spp ready {index} in {seconds:.1}s ({})", if mode.mono { "black & white" } else { "colour" }));
                    if let Some(ms) = self.edit_log.exact(&mode) {
                        log(&format!("time: edit noise exact after {ms:.0} ms"));
                    }
                    self.spp_expected = 0.6 * self.spp_expected + 0.4 * seconds as f64;
                    if let Some(e) = self.entries.get(index) {
                        photo_state::clear(&e.path, photo_state::Kind::SigmaExact);
                    }
                    if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
                        c.loaded.previews.spp = Some(preview);
                        c.detail = None;
                        c.detail_grain = None;
                        c.detail_hover = None;
                        c.fade = None;
                        c.fade_armed = Some(ctx.input(|i| i.time));
                        self.panes.invalidate();
                        self.rendered_for = None;
                        self.spp_seconds = Some(seconds);
                        if let Some((i, t)) = self.selected_at.filter(|(i, _)| *i == index) {
                            log(&format!("time: {i} exact after {:.0} ms", (ctx.input(|i| i.time) - t) * 1000.0));
                            timing_log::exact((ctx.input(|i| i.time) - t) * 1000.0);
                        }
                        self.refine = Refine::Done { at: ctx.input(|i| i.time) };
                        self.prefetch_due = Some(ctx.input(|i| i.time) + 0.3);
                    }
                }
                JobResult::SppCounts { index, mode, counts, seconds } => {
                    if self.spp_early != Some(index) {
                        continue;
                    }
                    // started with the click for settings changed meanwhile: not installed (the base for the settings now
                    // is on its way, see `spp_accept.rs`)
                    if self.entries.get(index).is_some_and(|e| spp_accept::arrival(&mode, &e.settings.spp_key(&e.path)) == spp_accept::Arrival::Outdated) {
                        log(&format!("spp counts {index}: base of old settings dropped"));
                        self.spp_early = None;
                        continue;
                    }
                    match self.current.as_ref().filter(|c| c.index == index) {
                        Some(c) => {
                            if let Some(session) = c.loaded.session.clone() {
                                self.worker.send(Job::SppInstall { index, session, mode, counts, seconds });
                            }
                        }
                        // Preview not opened yet: remember, show() applies it
                        None => self.spp_counts = Some((index, mode, counts, seconds)),
                    }
                }
                JobResult::SppFailed { index, error, prefetch } => {
                    log(&format!("spp failed {index} ({}): {error}", self.entries.get(index).map_or_else(String::new, |e| e.path.display().to_string())));
                    if prefetch {
                        if self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == index) {
                            self.spp_prefetching = None;
                        }
                    }
                    // the comparison's field of this photo falls back to the fast preview (a cancelled base is no
                    // failure: it is asked for again)
                    self.compare_base_done(index, error != "abgebrochen");
                    if prefetch {
                        if self.current.as_ref().is_none_or(|c| c.index != index) {
                            continue;
                        }
                    }
                    self.spp_pending = None;
                    if self.spp_early == Some(index) {
                        self.spp_early = None;
                    }
                    if self.current.as_ref().is_some_and(|c| c.index == index) {
                        self.refine = Refine::None;
                        // Failed means for this image: do not try again (otherwise endless loop "Refining …",
                        // e.g. for Sigma photos shot in black & white/sepia, for which the exact base does not exist)
                        self.refine_retries = 2;
                        let rescued = self.current.as_ref().is_some_and(|c| c.loaded.damage.is_some());
                        // a condition of the photo (yellow line in the image info, mark in the toolbar), not a message
                        let detail = if rescued {
                            // the exact base needs intact data; the rescued part is developed by graineo itself
                            crate::menu::i18n::de("Die Datei ist beschädigt. Die volle Foveon-Verarbeitung braucht unversehrte Rohdaten; den geretteten Teil rechnet graineo selbst.").to_string()
                        } else if jobs::is_damaged(&error) {
                            crate::menu::i18n::de("Datei beschädigt – graineo kann die Rohdaten nicht vollständig lesen. Gezeigt wird die Vorschau aus der Kamera.").to_string()
                        } else if error.contains("monochrome and sepia") {
                            crate::menu::i18n::de("Dieses Sigma-Foto ist in Schwarzweiß oder Sepia aufgenommen – dafür gibt es die Foveon-Basis nicht, graineo rechnet selbst.").to_string()
                        } else {
                            crate::menu::i18n::fill_de(crate::menu::i18n::de("graineo rechnet dieses Foto selbst. Ursache: {error}"), &[("error", &error)])
                        };
                        photo_state::set(&self.entries[index].path, photo_state::sigma_exact(&detail));
                    }
                }
                JobResult::CameraJpeg { index, width, height, rgb } => {
                    if let Some(c) = self.current.as_mut().filter(|c| c.index == index) {
                        let img = ColorImage::from_rgb([width, height], &rgb);
                        c.camera_jpeg = Some(ctx.load_texture("camera", img, TextureOptions::LINEAR));
                    }
                }
                JobResult::Stats { index, rect, stats } => {
                    self.stats = Some((index, rect, stats));
                }
                JobResult::LayersExported { result } => {
                    self.layers_status = match result {
                        Ok(dir) => {
                            crate::system::open(&dir);
                            crate::trf!("Gespeichert in {a}", a = dir.display())
                        }
                        Err(e) => crate::trf!("Fehler: {e}", e = e),
                    };
                }
                JobResult::SceneDistribution { index, key, bins } => {
                    if self.current.as_ref().is_some_and(|c| c.index == index) {
                        self.darkroom.marker.outcome(key, bins);
                    }
                }
                JobResult::Contact { index, key, row, fields, end, target } => {
                    if target == jobs::ContactTarget::Presets {
                        self.preset_images.outcome(ctx, key, fields, end);
                    } else if target == jobs::ContactTarget::Recipes {
                        self.darkroom.recipes.images.outcome(ctx, key, fields, end);
                    } else if self.current.as_ref().is_some_and(|c| c.index == index) {
                        self.darkroom.contact.outcome(ctx, key, row, fields, end);
                    } else if end {
                        self.darkroom.contact.task_finished(key);
                    }
                }
                JobResult::Print { key, nr, image, end } => self.darkroom.contact.print.outcome(ctx, key, nr, image, end),
                JobResult::PrintPreview { key, nr, image } => self.darkroom.contact.print.stand_in(ctx, key, nr, image),
                JobResult::Thumbnail { image, file, rgb: (w, h, px) } => {
                    self.thumbnails_running.remove(&image);
                    // w = 0: the photo could not be rendered (its old preview stays)
                    if let Some(img) = image::RgbImage::from_raw(w as u32, h as u32, px).filter(|_| w > 0 && h > 0) {
                        self.lib.turned.clear(image);
                        // only a texture that exists is renewed (the filmstrip otherwise shows the library's preview,
                        // renewed below) – thousands of edited photos must not each get a texture of their own
                        if let Some(t) = self.entries.iter_mut().find(|e| e.image == image).and_then(|e| e.thumb.as_mut()) {
                            t.set(ColorImage::from_rgb([w, h], img.as_raw()), TextureOptions::LINEAR);
                        }
                        self.lib.store_developed(ctx, file, img, file > 0, false);
                    }
                    // the next part of the previews once this one is done
                    if self.thumbnails_running.is_empty() {
                        self.compute_thumbnails();
                    }
                }
                JobResult::Variants { index, key, images } => {
                    if self.current.as_ref().is_some_and(|c| c.index == index) {
                        self.darkroom.probe.outcome(ctx, key, images);
                    }
                }
                JobResult::FilmThumbs { index, key, thumbs, end, background } => {
                    self.film_previews.receive(ctx, index, key, thumbs, end, background);
                }
                JobResult::HoverSuperseded { index, settings } => {
                    if self.entries.get(index).is_some() && self.compare_settings_of(index) == settings {
                        self.compare_develop_dropped(index);
                    }
                    // replaced by a newer hover (the newest one is still on its way: `hover_preset` stays) or crashed
                    if self.stress.as_ref().is_some_and(|s| s.hover_enabled()) {
                        stress::HOVER_SUPERSEDED.fetch_add(1, Ordering::Relaxed);
                    }
                    if self.current.as_ref().is_some_and(|c| c.index == index) {
                        self.panes.forget_request(&settings);
                        self.darkroom.compare.forget_request(&settings);
                        if self.darkroom.recipes.hover_sent.as_ref().is_some_and(|(s, _)| *s == settings) {
                            self.darkroom.recipes.hover_sent = None;
                        }
                    }
                }
                JobResult::HoverRendered { index, settings, width, height, rgb } if self.compare_take_developed(ctx, index, &settings, width, height, &rgb) => {}
                JobResult::HoverRendered { index, settings, width, height, rgb } => {
                    if self.stress.as_ref().is_some_and(|s| s.hover_enabled()) {
                        stress::HOVER_RENDERED.fetch_add(1, Ordering::Relaxed);
                    }
                    let current = self.current.as_ref().is_some_and(|c| c.index == index);
                    if current && self.darkroom.compare.active() {
                        let img = ColorImage::from_rgb([width, height], &rgb);
                        self.darkroom.compare.set_texture(ctx, settings.clone(), img);
                    }
                    if current {
                        self.panes.accept_preview(ctx, &settings, ColorImage::from_rgb([width, height], &rgb));
                    }
                    if current && self.hover_preset.as_ref() == Some(&settings) {
                        let img = ColorImage::from_rgb([width, height], &rgb);
                        match &mut self.hover_tex {
                            Some((s, t)) => {
                                t.set(img, TextureOptions::LINEAR);
                                *s = settings;
                            }
                            None => self.hover_tex = Some((settings, ctx.load_texture("hover-preset", img, TextureOptions::LINEAR))),
                        }
                    }
                }
                JobResult::Camf { index, camf } => {
                    if self.camf.index == Some(index) {
                        match camf {
                            Ok(c) => self.camf.camf = Some(c),
                            Err(e) => self.camf.error = Some(e),
                        }
                    }
                }
                JobResult::ExportProgress { done, total, message, failed } => {
                    self.export_progress = (done, total);
                    self.export_status = format!("{done}/{total} {message}");
                    if let Some(p) = failed {
                        self.export_failures.push((Some(p).filter(|p| !p.as_os_str().is_empty()), message.clone()));
                    }
                    if done == total && self.exporting {
                        self.exporting = false;
                        // result in the status in the title bar: a short success, or an error that stays until closed
                        let failures = std::mem::take(&mut self.export_failures);
                        let photos = |n: usize| if n == 1 { crate::tr("1 Foto").to_string() } else { crate::trf!("{n} Fotos", n = n) };
                        if jobs::EXPORT_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
                            self.notes.done(crate::trf!("Export {a}", a = crate::menu::i18n::tr_msg(&message)), ctx.input(|i| i.time));
                        } else if failures.is_empty() {
                            self.notes.done(crate::trf!("Export fertig · {a}", a = photos(total)), ctx.input(|i| i.time));
                        } else {
                            let all = failures.iter().any(|(p, _)| p.is_none()) || failures.len() >= total;
                            let title = if all { crate::tr("Export fehlgeschlagen").to_string() } else { crate::trf!("Export: {a} von {b} fehlgeschlagen", a = failures.len(), b = total) };
                            let retry: Vec<PathBuf> = if all { std::mem::take(&mut self.export_items) } else { failures.iter().filter_map(|(p, _)| p.clone()).collect() };
                            // the usual cause in one plain sentence, otherwise the first message
                            let denied = failures.iter().any(|(_, m)| m.contains("os error 13") || m.contains("os error 30") || m.contains("Permission denied"));
                            let cause = if denied {
                                crate::tr("Keine Schreibrechte im Zielordner.").to_string()
                            } else if failures.len() > 1 {
                                crate::trf!("{c} (und {n} weitere)", c = failures[0].1, n = failures.len() - 1)
                            } else {
                                failures[0].1.clone()
                            };
                            let dir = self.export_dir.clone();
                            self.notes.fault(activity::Fault {
                                title,
                                cause,
                                subject: dir.as_ref().map(|d| crate::trf!("{a} · Ziel: {d}", a = photos(failures.len()), d = d.display())).unwrap_or_default(),
                                reveal: dir.filter(|d| d.exists()),
                                retry: (!retry.is_empty()).then_some(activity::Retry::Export(retry)),
                            });
                        }
                        if self.reveal_after_export {
                            if let Some(d) = &self.export_dir {
                                crate::system::open(d);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Re-render or re-denoise the preview when settings have changed;
    /// start the SPP base and preloading at the right time.
    fn schedule(&mut self, now: f64) {
        // the automatic operating mode switched: say so once in the title bar (never silently, see `perf_mode`)
        if let Some(st) = perf_mode::take_change() {
            self.notes.done(perf_mode::change_text(&st), now);
        }
        // binning switched: the photo has another size, open it again (see binning_ui.rs)
        self.binning_check();
        // engine of the raw stage switched (graineo ↔ Apple, other Apple version): open the photo again (apple_raw.rs)
        self.engine_check();
        // dwell over: open the selected image now
        if let Some((i, due)) = self.browse.open_due {
            if now >= due {
                self.browse.open_due = None;
                if self.loading == Some(i) {
                    self.start_open(i);
                }
            }
        }
        // Sensor inspection stays on visible crops; do not prepare the whole RAW in the background.
        if self.tab == Tab::Sensor { self.browse.full_due = None; }
        // full resolution of the current image in the background (only while it is looked at)
        if let Some((i, due)) = self.browse.full_due {
            if self.current.as_ref().is_none_or(|c| c.index != i) {
                self.browse.full_due = None;
            } else if now >= due && self.loading.is_none() && self.preload_running.is_empty() {
                self.browse.full_due = None;
                let c = self.current.as_ref().unwrap();
                if let Some(doc) = c.loaded.previews.foreign.clone() {
                    let settings = self.entries[i].settings.clone();
                    let need = (doc.full_size[0] * doc.full_size[1]) as u64 * mem_budget::FULL_BYTES_PER_PIXEL;
                    // ahead only with room in memory; zooming prepares it anyway (the same result)
                    // (never for a merged photo whose full resolution is not in the cache: that would merge it, unasked –
                    // also right after the user cancelled just that; it is merged when it is zoomed in or exported)
                    let merge_needed = doc.merged.as_ref().is_some_and(|m| !m.pending && !rawdev::merged::cached(&m.source.key));
                    if !merge_needed && !doc.full_ready(&settings) && crate::env::var_os("GRAINEO_NO_FULL_AHEAD").is_none() && mem_budget::room_ahead("full resolution ahead", need) {
                        log(&format!("full resolution ahead {i}"));
                        self.worker.send(Job::PrepareFull { index: i, doc, settings, generation: self.open_generation.load(Ordering::SeqCst), current: self.open_generation.clone() });
                    }
                }
            }
        }
        // Watchdog: "Refining …" must never hang. If no computation is running for the current
        // image anymore (e.g. result discarded during fast switching), the
        // base is re-requested, or the indicator ends if the base is already there.
        if let (Refine::Working { since }, Some(c)) = (self.refine, self.current.as_ref()) {
            let idx = c.index;
            let in_flight = self.spp_pending.is_some()
                || self.spp_due.is_some()
                || self.spp_early == Some(idx)
                || self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == idx);
            // Time limit: whatever got lost – after this time at the latest it is
            // re-requested (at most twice), after that the fast preview stays.
            // counted from display (opening itself can take long on a busy Mac)
            let limit = SPP_DELAY_SECS + 4.0 * self.spp_expected + 15.0;
            let from = since.max(self.shown_at);
            if in_flight && self.loading.is_none() && now - from > limit && c.loaded.previews.spp.is_none() {
                log(&format!(
                    "watchdog: time limit {idx} after {:.1} s (pending {}, due {:?}, early {:?}, ahead {:?}, attempt {})",
                    now - from,
                    self.spp_pending.is_some(),
                    self.spp_due,
                    self.spp_early,
                    self.spp_prefetching.as_ref().map(|(i, _)| *i),
                    self.refine_retries
                ));
                self.spp_cancel.store(true, Ordering::SeqCst);
                if let Some((_, cancel)) = self.spp_prefetching.take() {
                    cancel.store(true, Ordering::SeqCst);
                }
                self.spp_pending = None;
                self.spp_due = None;
                self.spp_early = None;
                self.spp_counts = None;
                if self.refine_retries < 2 && develop::Session::uses_spp(&self.entries[idx].settings) {
                    self.refine_retries += 1;
                    self.refine = Refine::Working { since: now };
                    self.request_spp(idx);
                } else {
                    self.refine = Refine::None;
                    photo_state::set(&self.entries[idx].path, photo_state::sigma_exact(crate::menu::i18n::de("Die Berechnung hat zu lange gedauert. graineo zeigt die eigene Entwicklung; beim nächsten Öffnen wird es erneut versucht.")));
                }
            } else if !in_flight && now - since > 1.0 {
                if c.loaded.previews.spp.is_some() {
                    log(&format!("watchdog: refining {idx} ended (base present)"));
                    self.refine = Refine::None;
                } else if develop::Session::uses_spp(&self.entries[idx].settings) {
                    log(&format!("watchdog: SPP base {idx} re-requested"));
                    self.request_spp(idx);
                } else {
                    self.refine = Refine::None;
                }
            }
        }
        // Crop at full resolution is waiting for Sigma's base: request it if nothing is running
        // (for images from the cache usually a hit in the disk cache, ≈ 0.2 s)
        if std::mem::take(&mut self.detail_wants_spp) && self.loading.is_none() {
            // zoomed while the operating mode still waits for the dwell: the user needs the exact base now
            if self.spp_due.is_some_and(|due| due > now) && self.spp_settle.is_none() {
                log("detail: dwell for the SPP base cut short (zoom)");
                self.spp_due = Some(now);
            }
            if let Some(idx) = self.current.as_ref().map(|c| c.index) {
                let busy = self.spp_pending.is_some() || self.spp_due.is_some() || self.spp_early == Some(idx) || self.spp_prefetching.as_ref().is_some_and(|(i, _)| *i == idx);
                if !busy && self.refine_retries < 2 {
                    log(&format!("detail: SPP base for {idx} requested"));
                    self.request_spp(idx);
                }
            }
        }
        // paging fast: no exact base yet (it would be cancelled again by the next photo); it starts once the user stays
        if self.spp_due.is_some_and(|due| now >= due) && self.spp_settle.is_none() && self.zoom <= 1.0 && self.browse.fast(now) {
            self.spp_due = Some(now + 0.2);
        }
        if let Some(due) = self.spp_due {
            if now >= due && self.loading.is_none() {
                self.spp_due = None;
                if let Some(k) = self.spp_settle.take() {
                    log(&format!("spp base: settings rested, base started ({})", if k.mono { "black & white" } else { "colour" }));
                }
                if let Some(i) = self.current.as_ref().map(|c| c.index) {
                    self.request_spp(i);
                }
            }
        }
        // the exact view waits for the dwell of the operating mode: the spinner's tooltip says why and how long
        let waiting = match (self.refine, self.spp_due, &self.spp_settle) {
            (Refine::Working { .. }, Some(due), None) if due > now && self.spp_wait > SPP_DELAY_SECS => {
                let st = perf_mode::status();
                Some(crate::trf!("{t}: genaue Ansicht in {n} s, beim Zoomen sofort ({c})", t = st.tier.name(), n = (due - now).ceil().max(1.0) as u32, c = st.cause.text()))
            }
            _ => None,
        };
        work_step::set_waiting(waiting);
        // Library: preload the clicked image immediately (see `preload_selection`)
        if self.in_grid() {
            let b = self.lib.active_image();
            if b != self.preload_image {
                self.preload_image = b;
                if let Some(i) = self.active_entry() {
                    self.preload_selection(i);
                }
            }
        }
        // paging fast: no preloading (it would only compete with the images actually looked at)
        if self.prefetch_due.is_some_and(|due| now >= due) && self.browse.fast(now) {
            self.prefetch_due = Some(now + 0.2);
        }
        if let Some(due) = self.prefetch_due {
            if now >= due && self.loading.is_none() && self.spp_pending.is_none() {
                self.prefetch_due = None;
                if crate::env::var_os("GRAINEO_NO_PREFETCH").is_none() {
                    self.prefetch_neighbors();
                    self.maybe_prefetch_spp();
                }
            }
        }
        let Some(c) = &self.current else { return };
        // neural denoising of the photo on screen: computed in the background, rendered again once it is there
        let ai = crate::ai_denoise_ui::step(c.loaded.previews.foreign.as_ref(), &self.entries[c.index].settings);
        if let Some(text) = ai.done {
            self.notes.done(text, now);
        }
        if ai.rerender {
            if let Some(c) = self.current.as_mut() {
                c.detail = None;
                c.detail_grain = None;
                c.detail_hover = None;
            }
            self.panes.invalidate();
            self.rendered_for = None;
        }
        let Some(c) = &self.current else { return };
        let s = self.entries[c.index].settings.clone();
        self.edit_log.observe(c.index, &self.entries[c.index].path, &s);
        // Colour mode changed: recompute the SPP base (converted linearly until then)
        if develop::Session::uses_spp(&s) && c.loaded.session.is_some() {
            let index = c.index;
            let wanted = s.spp_key(&self.entries[index].path);
            // the base on screen cannot stand in for the new settings (colour ↔ black & white): the fast preview with
            // the new settings until their base is there, never the picture of the old ones (see `spp_accept.rs`)
            if let Some(k) = c.loaded.previews.spp.as_ref().and_then(|p| p.spp.as_ref()).map(|(k, _)| k).filter(|k| !spp_accept::stands_in(k, &wanted)) {
                log(&format!("spp base {index} ({}) does not fit the new settings: fast preview until the new base", if k.mono { "black & white" } else { "colour" }));
                if let Some(c) = self.current.as_mut() {
                    c.loaded.previews.spp = None;
                    c.detail = None;
                    c.detail_grain = None;
                    c.detail_hover = None;
                }
                self.panes.invalidate();
                self.rendered_for = None;
                self.spp_settle = Some(wanted.clone());
                self.spp_due = None;
            }
            // a base still being computed for settings changed meanwhile: cancelled right away (it would compute for
            // nothing; finished anyway, it stays in the disk cache)
            if self.spp_pending.as_ref().is_some_and(|k| *k != wanted) {
                log(&format!("spp base {index}: settings changed while computing, old job cancelled"));
                self.spp_cancel.store(true, Ordering::SeqCst);
                self.spp_pending = None;
                if self.spp_early == Some(index) {
                    self.spp_early = None;
                    self.spp_counts = None;
                }
                if let Some((_, cancel)) = self.spp_prefetching.take_if(|(i, _)| *i == index) {
                    cancel.store(true, Ordering::SeqCst);
                }
                self.spp_settle.get_or_insert_with(|| wanted.clone());
                self.spp_due = None;
            }
            let Some(c) = &self.current else { return };
            let shown = c.loaded.previews.spp.as_ref().and_then(|p| p.spp.as_ref()).map(|(k, _)| k.clone());
            let now = self.now();
            match spp_accept::step(self.spp_pending.is_some(), shown.as_ref(), self.spp_settle.as_ref(), &wanted) {
                spp_accept::Step::Wait => {}
                spp_accept::Step::BackToShown => {
                    log(&format!("spp base {index}: settings back to the base shown, nothing started"));
                    self.spp_settle = None;
                    self.spp_due = None;
                }
                // start once the settings have rested (fast preview with the new settings until then), never for each
                // step of a quick back and forth
                spp_accept::Step::Settle => {
                    if self.spp_settle.as_ref() != Some(&wanted) || self.spp_due.is_none() {
                        self.spp_settle = Some(wanted);
                        self.spp_due = Some(now + spp_accept::SETTLE_SECS);
                        if matches!(self.refine, Refine::None | Refine::Done { .. }) {
                            self.refine = Refine::Working { since: now };
                        }
                    }
                }
                // Noise slider back to default, or image from memory without SPP
                // (no longer once the watchdog has given up on this image)
                spp_accept::Step::Plain => {
                    if self.spp_due.is_none() && matches!(self.refine, Refine::None) && self.refine_retries < 2 {
                        self.request_spp(index);
                        return;
                    }
                }
            }
        }
        let Some(c) = &self.current else { return };
        if self.preparing || self.rendering {
            return;
        }
        // high ISO without its exact base: the fast preview is too rough to be shown, the picture on screen stays
        // until the base is there (see `preview_hold.rs`)
        if self.holds_fast_preview(c, &s) {
            if self.held_logged != Some(c.index) {
                self.held_logged = Some(c.index);
                log(&format!("render {}: fast preview withheld (high ISO), waiting for the exact base", c.index));
            }
            return;
        }
        self.held_logged = None;
        if let (Some(session), true) = (c.loaded.session.clone(), c.loaded.previews.denoise_key != s.denoise_key() && s.view == View::Developed) {
            self.preparing = true;
            self.worker.send(Job::Prepare { index: c.index, session, settings: s });
            return;
        }
        if self.rendered_for.as_ref() != Some(&(c.index, s.clone())) {
            if crate::env::var_os("GRAINEO_LOG").is_some() {
                let from = match c.loaded.previews.spp.as_ref().filter(|_| develop::Session::uses_spp(&s)).and_then(|p| p.spp.as_ref()) {
                    Some((k, _)) => format!("exact base ({})", if k.mono { "black & white" } else { "colour" }),
                    None => "fast preview".into(),
                };
                log(&format!("render {} sent ({}) from {from}", c.index, if s.monochrome { "black & white" } else { "colour" }));
            }
            self.rendering = true;
            self.worker.send(Job::Render {
                index: c.index,
                previews: c.loaded.previews.clone(),
                cal: c.loaded.cal.clone(),
                settings: s,
                before: false,
            });
        }
    }

    /// Is the render from the fast preview withheld for the photo on screen with these settings (high ISO, exact base
    /// on its way)? See `preview_hold.rs`.
    fn holds_fast_preview(&self, c: &Current, s: &Settings) -> bool {
        preview_hold::enabled() && c.loaded.session.is_some() && c.loaded.previews.foreign.is_none() && preview_hold::hold(preview_hold::state(&c.loaded.previews, &c.loaded.cal, s, self.refine_retries >= 2))
    }

    /// Do pictures derived from the photo on screen (film and preset tiles, hover and comparison previews, contact
    /// rows) wait with these settings? They wait like the main picture while its fast preview is withheld: none of
    /// them is ever made from the rough data; each asks again in a later frame.
    pub(crate) fn derived_waits(&self, s: &Settings) -> bool {
        self.current.as_ref().is_some_and(|c| self.holds_fast_preview(c, s))
    }

    /// Compute Sigma's exact base for the current image in the background.
    fn request_spp(&mut self, index: usize) {
        let Some(c) = self.current.as_ref().filter(|c| c.index == index) else { return };
        let Some(session) = c.loaded.session.clone() else { return };
        let mode = self.entries[index].settings.spp_key(&self.entries[index].path);
        self.spp_settle = None;
        self.spp_cancel.store(true, Ordering::SeqCst);
        self.spp_cancel = Arc::new(AtomicBool::new(false));
        self.cancel_prefetches(|_| false);
        if let Some((i, cancel)) = self.spp_prefetching.take() {
            if i != index {
                cancel.store(true, Ordering::SeqCst);
            } else {
                // computed ahead and still running: wait for it (made for other settings, it is dropped on arrival and
                // asked for again)
                self.spp_prefetching = Some((i, cancel));
                self.spp_pending = Some(mode);
                return;
            }
        }
        self.spp_pending = Some(mode.clone());
        let since = match self.refine {
            Refine::Working { since } => since,
            _ => self.now(),
        };
        self.refine = Refine::Working { since };
        self.worker.send(Job::Spp {
            index,
            path: self.entries[index].path.clone(),
            session: Some(session),
            mode,
            cancel: self.spp_cancel.clone(),
            prefetch: false,
        });
    }

    /// Precompute the SPP base of the next image as soon as the current one is done and the
    /// next one has been preloaded – when paging on, it is then SPP-exact immediately.
    fn maybe_prefetch_spp(&mut self) {
        if self.spp_prefetching.is_some() || self.spp_pending.is_some() || self.spp_due.is_some() {
            return;
        }
        // operating mode "Saving": nothing computed ahead (`perf_mode`)
        if !perf_mode::plan().base_ahead {
            return;
        }
        let Some(cur) = self.current.as_ref().map(|c| c.index) else { return };
        let vis = self.visible();
        let Some(next) = next_in_direction(&vis, cur, self.nav_dir) else { return };
        if !self.entries[next].loaded {
            return;
        }
        let settings = &self.entries[next].settings;
        if !develop::Session::uses_spp(settings) {
            return;
        }
        let Some((_, loaded)) = self.cache.iter().find(|(i, _)| *i == next) else { return };
        if loaded.previews.spp.is_some() {
            return;
        }
        let Some(session) = loaded.session.clone() else { return };
        // a base with the neural denoising is not computed ahead (seconds of full load for a photo perhaps never
        // shown); it starts, with its progress in the title bar, when the photo is shown
        if settings.spp_key(&self.entries[next].path).ai > 0 {
            return;
        }
        let need = (loaded.full_size[0] * loaded.full_size[1]) as u64 * mem_budget::BASE_BYTES_PER_PIXEL;
        if !mem_budget::room_ahead("Sigma base ahead", need) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.spp_prefetching = Some((next, cancel.clone()));
        log(&format!("spp precompute {next}"));
        self.worker.send(Job::Spp {
            index: next,
            path: self.entries[next].path.clone(),
            session: Some(session),
            mode: settings.spp_key(&self.entries[next].path),
            cancel,
            prefetch: true,
        });
    }

    /// Selection in the grid (first click): load and fully develop the image in the background right away, so that the double click
    /// shows it without waiting and without a change in appearance.
    fn preload_selection(&mut self, n: usize) {
        if crate::env::var_os("GRAINEO_NO_PREFETCH").is_some() || n >= self.entries.len() {
            return;
        }
        // the preload of the previous selection is obsolete
        self.preload_cancel.store(true, Ordering::SeqCst);
        self.preload_cancel = Arc::new(AtomicBool::new(false));
        if self.cache.iter().any(|(c, _)| *c == n) || self.current.as_ref().is_some_and(|c| c.index == n) || self.preload_running.contains(&n) {
            return;
        }
        self.ensure_entry(n);
        let e = &self.entries[n];
        if image_kind::of(&e.path).editable() && developable_state(e.state) {
            self.preload_running.push(n);
            self.worker.send(Job::Prefetch { index: n, path: e.path.clone(), settings: e.settings.clone(), cancel: self.preload_cancel.clone() });
        }
    }

    /// New cancel flag for preloading image `n`.
    fn prefetch_flag(&mut self, n: usize) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.prefetch_flags.retain(|(i, _)| *i != n);
        self.prefetch_flags.push((n, flag.clone()));
        flag
    }

    /// Cancel all preloads of neighbours except those for which `keep` holds.
    fn cancel_prefetches(&mut self, keep: impl Fn(usize) -> bool) {
        // the photos of the comparison keep loading
        let pool = self.cmp.pool.clone();
        self.prefetch_flags.retain(|(n, flag)| {
            let k = keep(*n) || pool.contains(n);
            if !k {
                flag.store(true, Ordering::SeqCst);
            }
            k
        });
    }

    /// Preload neighbouring images: in the direction of travel one, then one behind. Only one preload
    /// at a time – the next one starts when the previous one has arrived (see `JobResult::Opened`),
    /// so the nearest neighbour is always ready first.
    fn prefetch_neighbors(&mut self) {
        let Some(i) = self.current.as_ref().map(|c| c.index) else { return };
        let d = self.nav_dir;
        // with the large previews decoded ahead one RAW in the direction of travel is enough (and the one behind; not
        // in the operating mode "Saving")
        let behind = perf_mode::plan().neighbours_behind;
        let order = [i as isize + d, i as isize - d];
        let order = &order[..if behind { 2 } else { 1 }];
        for n in order.iter().copied().filter(|&n| n >= 0 && (n as usize) < self.entries.len()).map(|n| n as usize) {
            if self.preload_running.contains(&n) {
                return; // one preload at a time; this one reports back and continues the chain
            }
            if !self.cache.iter().any(|(c, _)| *c == n) && image_kind::of(&self.entries[n].path).editable() && developable_state(self.entries[n].state) {
                if !mem_budget::room_ahead("preloading the neighbour", mem_budget::OPEN_BYTES) {
                    return;
                }
                self.ensure_entry(n);
                self.preload_running.push(n);
                let cancel = self.prefetch_flag(n);
                self.worker.send(Job::Prefetch { index: n, path: self.entries[n].path.clone(), settings: self.entries[n].settings.clone(), cancel });
                return;
            }
        }
    }

    /// The large preview of the selected image shows exactly its settings (fingerprint): keep it on screen when the
    /// image has loaded (see the hold in [`browse::Browse`]).
    /// Drop the whole-image job at full resolution (image or settings changed).
    fn cancel_full_job(&mut self) {
        // the job itself sees the image change (`open_generation`) and drops itself
        self.full_job = None;
    }

    fn try_hold(&mut self, index: usize) {
        let e = &self.entries[index];
        if e.copy.is_some() || image_kind::of(&e.path) == image_kind::ImageKind::Foveon {
            return;
        }
        let (path, image, stored, settings) = (e.path.clone(), e.image, e.stored, e.settings.clone());
        let expected = self.browse.expected(index, &settings, stored, &path, None);
        if browse::large(&path, library::preview::large()) != Some(expected) {
            return;
        }
        if let Some((t, true)) = self.lib.large_preview(image) {
            self.browse.hold = Some((index, t, expected));
        }
    }

    /// Read the sidecars of the next images in the direction of travel (and the one behind) in the background: on the
    /// NAS each read took up to seconds, and selecting an image read its sidecar in the UI thread.
    fn read_sidecars_ahead(&mut self, index: usize, d: isize) {
        let jobs: Vec<(i64, PathBuf, Option<String>)> = (1..=browse::LARGE_AHEAD)
            .map(|k| index as isize + k * d)
            .chain([index as isize - d])
            .filter(|&n| n >= 0 && (n as usize) < self.entries.len())
            .map(|n| &self.entries[n as usize])
            .filter(|e| !e.loaded && !self.sidecars_pending.contains(&e.image))
            .map(|e| (e.image, e.path.clone(), e.copy.clone()))
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.sidecars_pending.extend(jobs.iter().map(|j| j.0));
        let out = self.sidecars_ahead.clone();
        let ctx = self.worker.ctx().clone();
        std::thread::Builder::new()
            .name("sidecars-ahead".into())
            .spawn(move || {
                for (image, path, copy) in jobs {
                    let (settings, meta, stored) = sidecar::load_entry(&path, copy.as_deref());
                    out.lock().unwrap().push((image, settings, meta, stored));
                }
                ctx.request_repaint();
            })
            .ok();
    }

    /// Sidecars read ahead: into the entries not read yet.
    fn take_sidecars_ahead(&mut self) {
        let done = std::mem::take(&mut *self.sidecars_ahead.lock().unwrap());
        for (image, settings, meta, stored) in done {
            self.sidecars_pending.remove(&image);
            let Some(i) = self.entries.iter().position(|e| e.image == image && !e.loaded) else { continue };
            let opened = self.lib.rating_pending(image);
            self.apply_sidecar(i, settings, meta, stored, opened);
        }
    }

    /// Decode the large previews of this image and ahead in the direction of travel (and one behind), so that paging
    /// shows them at once.
    fn request_large_ahead(&mut self, index: usize) {
        let d = self.nav_dir;
        if self.browse.ahead_for == Some((index, d)) {
            return;
        }
        self.browse.ahead_for = Some((index, d));
        self.read_sidecars_ahead(index, d);
        let images: Vec<i64> = (0..=browse::LARGE_AHEAD)
            .map(|k| index as isize + k * d)
            .chain([index as isize - d])
            .filter(|&n| n >= 0 && (n as usize) < self.entries.len())
            .map(|n| &self.entries[n as usize])
            .filter(|e| e.copy.is_none())
            .map(|e| e.image)
            .collect();
        self.lib.prefetch_large(&images);
    }

    fn export(&mut self, indices: Vec<usize>) {
        let Some(dir) = self.export_dir.clone() else { return };
        // never into an Apple Photos library or another program's package (checked again right before writing)
        if let Err(e) = library::write_target::check(&dir) {
            self.notes.fault(activity::Fault { title: crate::tr("Export fehlgeschlagen").into(), cause: menu::i18n::tr_msg(&e).into_owned(), subject: dir.display().to_string(), reveal: None, retry: None });
            self.export_dir = None;
            return;
        }
        // writable? the message comes before the run, not as an error in the middle of it. The write test runs in
        // the background (a hanging network drive must not freeze the window); `poll_export_probe` goes on.
        if self.export_probe.is_some() {
            return;
        }
        self.exporting = true;
        jobs::EXPORT_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        self.export_status = crate::tr("Zielordner wird geprüft …").into();
        self.export_probe = Some(export_probe::ExportProbe::start(&self.egui_ctx, dir, indices));
    }

    /// Once per frame: the write test of the export target is done -> the export goes on (or is refused).
    fn poll_export_probe(&mut self) {
        let Some(p) = &self.export_probe else { return };
        if jobs::EXPORT_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
            self.export_probe = None;
            self.exporting = false;
            self.export_status = "Abgebrochen".into();
            return;
        }
        match p.poll() {
            export_probe::Poll::Waiting => {}
            export_probe::Poll::Failed(e) => {
                let dir = p.dir.clone();
                self.export_probe = None;
                self.exporting = false;
                self.export_status.clear();
                self.notes.fault(activity::Fault { title: crate::tr("Export fehlgeschlagen").into(), cause: menu::i18n::tr_msg(&e).into_owned(), subject: sysdirs::display_path(&dir), reveal: None, retry: None });
                self.export_dir = None;
            }
            export_probe::Poll::Ready => {
                let Some(p) = self.export_probe.take() else { return };
                self.exporting = false;
                self.export_status.clear();
                self.export_after_probe(p.indices, p.dir);
            }
        }
    }

    fn export_after_probe(&mut self, indices: Vec<usize>, dir: PathBuf) {
        self.save_current();
        // an export target inside a library folder (e.g. the suggested `<photo folder>/export`): the sync must not take
        // the exported files in as new photos (in the background, the library may be busy)
        if library::backup_view::active().is_none() {
            let target = dir.clone();
            let _ = std::thread::Builder::new().name("export-exclude".into()).spawn(move || {
                match library::db::Db::open(&library::db_path()).map_err(|e| e.to_string()).and_then(|db| library::manage::exclude_export_folder(&db, &target)) {
                    Ok(true) => log(&format!("export: {} kept out of the library sync", target.display())),
                    Ok(false) => {}
                    Err(e) => log(&format!("export: could not keep {} out of the sync: {e}", target.display())),
                }
            });
        }
        if self.export_original {
            // the files unchanged: every reachable photo (also finished images and videos), with the stacked camera JPEG
            let written = self.export_written(&indices);
            let names = self.export_job_names(&written);
            let items: Vec<(PathBuf, i64, Option<i64>)> = written
                .into_iter()
                .map(|i| {
                    let e = &self.entries[i];
                    (e.path.clone(), e.image, self.export_original_pair.then(|| self.lib.sidecar_jpeg_of(e.image)).flatten())
                })
                .collect();
            self.export_items = items.iter().map(|(p, ..)| p.clone()).collect();
            self.export_failures.clear();
            self.exporting = true;
            jobs::EXPORT_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
            self.export_status = crate::tr("Export startet …").into();
            self.worker.send(Job::ExportOriginal { items, dir, sidecars: self.export_original_sidecars, names });
            return;
        }
        for &i in &indices {
            self.ensure_entry(i);
        }
        let written = self.export_written(&indices);
        let names = self.export_job_names(&written);
        let items = written
            .into_iter()
            .map(|i| {
                let mut st = self.entries[i].settings.clone();
                // The size limit only applies to finished images (JPEG/TIFF), DNG stays at full resolution
                st.output_size = (self.export_limit_size && self.export_format.is_finished_image()).then_some(self.export_size);
                st.output_filter = self.export_resize;
                st.output_sharpen = if self.export_format.is_finished_image() { self.export_sharpen } else { Default::default() };
                tone_eq_ui::without_mask(&mut st);
                (self.entries[i].path.clone(), st, self.entries[i].image, self.entries[i].meta.clone())
            })
            .collect::<Vec<_>>();
        self.export_items = items.iter().map(|(p, ..)| p.clone()).collect();
        self.export_failures.clear();
        self.exporting = true;
        jobs::EXPORT_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        self.export_status = crate::tr("Look-Profile werden geprüft …").into();
        self.profile_export = Some(film_profiles_ui::ExportCheck::new(&self.egui_ctx, Job::Export { items, format: self.export_format, engine: self.export_engine, dir, gain_map: self.export_gain_map, allow_missing_profiles: false, as_print: self.export_as_print, color_space: self.export_color_space, options: self.export_options.clone(), names }));
    }

    /// Identity of entry `i` in the develop history.
    fn history_key(&self, i: usize) -> history::Key {
        history::Key { path: self.entries[i].path.clone(), copy: self.entries[i].copy.clone() }
    }

    /// Record settings changes as steps of the develop history as soon as no mouse button is pressed.
    fn track_history(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.current else {
            self.history.poll();
            return;
        };
        let index = c.index;
        let current = Settings { view: View::Developed, ..self.entries[index].settings.clone() };
        if ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let key = self.history_key(index);
        match &self.committed {
            Some((i, s)) if *i == index && *s != current => {
                let before = s.clone();
                let own = history::take_reset_gesture();
                if let Some(t) = self.history.record_with(&key, index, &before, &current, own) {
                    self.status = t;
                }
                self.lib.rating_not_recent();
                self.turn_undo.other_action();
                self.committed = Some((index, current));
            }
            // changed by a command that sets `committed` itself (several photos, rotation): a step of its own
            Some((i, s)) if *i == index => self.history.follow(&key, index, s),
            _ => self.committed = Some((index, current)),
        }
    }

    /// ⌘Z / ⇧⌘Z on the open photo: move the cursor of its develop history one step.
    fn undo(&mut self, redo: bool) {
        let Some(index) = self.current.as_ref().map(|c| c.index) else { return };
        let key = self.history_key(index);
        let now = Settings { view: View::Developed, ..self.entries[index].settings.clone() };
        // a change not recorded yet (e.g. ⌘Z while typing in a field) becomes a step first
        if let Some((i, s)) = &self.committed {
            if *i == index && *s != now {
                let before = s.clone();
                self.history.record(&key, index, &before, &now);
                self.committed = Some((index, now.clone()));
            }
        }
        let from = self.history.cursor_of(&key);
        if let Some(target) = self.history.undo(&key, index, &now, redo) {
            let view = self.entries[index].settings.view;
            self.entries[index].settings = Settings { view, ..target.clone() };
            self.committed = Some((index, target));
            // merged photo: strength and output of its recipe follow the step
            hdr_auto::history_moved(from);
        }
    }

    /// Apply copied groups to the current photo or all selected ones.
    fn paste_settings(&mut self) {
        let Some(src) = self.clipboard.clone() else { return };
        let groups = self.copy_groups;
        // every selected photo (grid: the selection, Develop: filmstrip selection + open photo); one ⌘Z for all
        let (targets, unreachable) = self.without_unreachable(self.selection());
        let pasted = src.clone();
        let cmd = history::Command::new(history::Hint::paste(history::clipboard_source().as_deref()));
        let n = self.settings_command(&targets, cmd, move |s| paste::merge(s, &pasted, &groups));
        if let Some(w) = rawdev::film_profiles::missing(&src) {
            self.status = crate::trf!("Eingefügt – {w}", w = w);
        } else if n > 1 {
            self.status = crate::trf!("In {n} Fotos eingefügt (⌘Z nimmt es zurück)", n = n);
        }
        if unreachable > 0 {
            self.status = crate::trf!("Eingefügt; {a} übersprungen: Original nicht erreichbar", a = unreachable);
        }
    }

    /// Split off the photos whose original is on an unreachable location (they keep their settings).
    /// Returns: (the others, number of unreachable ones).
    fn without_unreachable(&self, indices: Vec<usize>) -> (Vec<usize>, usize) {
        let (off, on): (Vec<usize>, Vec<usize>) = indices.into_iter().partition(|&i| self.entries.get(i).is_some_and(|e| e.state == 2));
        (on, off.len())
    }

    /// Copy: with ⌥ everything immediately, otherwise the selection dialog.
    fn copy_settings(&mut self, all: bool) {
        if self.current.is_none() {
            return;
        }
        if all {
            self.copy_groups = paste::Groups::all();
            self.clipboard = self.settings().cloned();
            history::copied_from(self.current.as_ref().map(|c| self.entries[c.index].name.clone()));
            if let Some(w) = self.clipboard.as_ref().and_then(rawdev::film_profiles::missing) { self.status = crate::trf!("Kopiert – {w}", w = w); }
        } else {
            self.copy_dialog = true;
        }
    }

    /// Hover preview (preset, film, look) to show in place of the current image: while a hover is going on, the last
    /// finished preview stays until the next one is rendered (no flash of the current state in between).
    fn hover_shown(&self) -> Option<&(Settings, TextureHandle)> {
        self.hover_tex.as_ref().filter(|_| self.hover_preset.is_some())
    }

    fn toggle_camera_jpeg(&mut self) {
        self.show_camera_jpeg = !self.show_camera_jpeg;
        if let Some(c) = &self.current {
            if self.show_camera_jpeg && c.camera_jpeg.is_none() {
                self.worker.send(Job::CameraJpeg { index: c.index, path: self.entries[c.index].path.clone() });
            }
        }
    }

    /// Fullscreen preview (F): only the image, on black, without panels.
    fn toggle_preview_full(&mut self, ctx: &egui::Context) {
        match self.preview_full.take() {
            Some((top, film, left, right, fullscreen)) => {
                (self.show_top, self.show_filmstrip, self.show_left, self.show_right) = (top, film, left, right);
                if !fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                }
            }
            None => {
                self.preview_full = Some((self.show_top, self.show_filmstrip, self.show_left, self.show_right, self.screen_mode > 0));
                (self.show_top, self.show_filmstrip, self.show_left, self.show_right) = (false, false, false, false);
                self.grid = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
        }
    }

    /// "Auto" (tone, ⌘U): determine the tone sliders from the preview – the same
    /// preview that is displayed. Undo works through the normal history.
    fn auto_tone_current(&mut self) {
        let Some(c) = &self.current else { return };
        history::hint(c.index, history::Hint::auto());
        let (previews, cal) = (c.loaded.previews.clone(), c.loaded.cal.clone());
        if let Some(doc) = &previews.foreign {
            doc.auto_tone(&mut self.entries[c.index].settings);
            return;
        }
        auto_tone(&previews, &cal, &mut self.entries[c.index].settings);
    }

    /// Execute a command from the Mac menu.
    fn run_cmd(&mut self, cmd: menu::Cmd, ctx: &egui::Context) {
        use menu::Cmd::*;
        // In the grid the photo list follows only on demand (see `update`)
        if self.in_grid() {
            self.sync_source(false);
        }
        if crate::env::var_os("GRAINEO_NATIVE_KEYS").is_some() {
            log(&format!("menu command: {cmd:?}"));
        }
        let has = self.current.is_some();
        // the print room only chooses what to print: deleting or removing from a collection does not belong there
        if self.print_room.open && matches!(cmd, TrashSelected | DeleteRejected | RemoveFromCollection) {
            return;
        }
        match cmd {
            Settings => {
                prefs::open();
                ctx.request_repaint();
            }
            CheckUpdates => update_ui::check_now(ctx),
            UiLarger => prefs::step_scale(ctx, 1),
            UiSmaller => prefs::step_scale(ctx, -1),
            UiReset => prefs::step_scale(ctx, 0),
            Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            OpenFolder => {
                file_dialog::folder(ctx, "ordner-oeffnen", |d| d);
            }
            Import => self.lib.open_import(ctx),
            Lightroom => library_ui::request_lightroom(),
            Export => self.open_export(),
            VirtualCopy => self.create_virtual_copy(None),
            MergeHdr => self.open_hdr(),
            RereadMetadata => {
                let mut a = Vec::new();
                self.lib.reread_metadata(ctx, &mut a);
                self.library_actions(a);
            }
            // ⇧S and the menu entry: as the button "all series" wherever the filmstrip or the grid shows the list
            // (no switch to the grid); where it does nothing the status line says why
            StackToggleAll => {
                let a = self.lib.stack_menu_command(library_ui::StackMenu::ToggleAll);
                self.library_actions(a);
            }
            StackReview | StackGroup | StackDissolve => {
                // series and stacks live in the grid: from a single view the grid comes first
                if !self.in_grid() {
                    self.to_grid();
                }
                let c = match cmd {
                    StackReview => library_ui::StackMenu::Review,
                    StackGroup => library_ui::StackMenu::Group,
                    StackDissolve => library_ui::StackMenu::Dissolve,
                    _ => library_ui::StackMenu::ToggleAll,
                };
                let a = self.lib.stack_menu_command(c);
                self.library_actions(a);
            }
            DeleteRejected => self.confirm_delete = true,
            TrashSelected if !self.confirm_delete => self.request_delete(),
            TrashSelected => {}
            RemoveFromCollection => {
                if self.lib.removable_collection().is_some() {
                    let ids = self.delete_targets();
                    let a = self.lib.remove_from_collection_external(ids);
                    self.library_actions(a);
                } else {
                    self.status = crate::tr("Nur in einer Sammlung möglich").to_string();
                }
            }
            // ⌘Z / ⇧⌘Z: the most recent action of all kinds (undo_order.rs); the people view keeps its own
            Undo if self.in_grid() && people_ui::is_open() => people_ui::request(people_ui::Request::Undo),
            Undo => self.undo_latest(),
            Redo if self.in_grid() && people_ui::is_open() => people_ui::request(people_ui::Request::Redo),
            Redo => self.redo_latest(),
            CopyPhoto => self.copy_photo_command(ctx),
            CopySettings => self.copy_settings(false),
            PasteSettings => self.paste_settings(),
            TransferSettings => self.transfer_command(),
            AutoTone => self.auto_tone_current(),
            ResetSettings => self.reset_command(),
            ApplyToAll => self.apply_to_all(),
            Crop if has => self.toggle_crop(true),
            RotateLeft => self.rotate_command(false),
            RotateRight => self.rotate_command(true),
            Pick | Reject | Unflag => {
                let f = match cmd {
                    Pick => 1,
                    Reject => -1,
                    _ => 0,
                };
                self.rate(rating::Change::Flag(f), false);
            }
            JumpToSelection => {
                if self.in_grid() {
                    self.lib.request_jump();
                }
            }
            Grid => self.to_grid(),
            Loupe | Develop => self.to_single_image(),
            Sensor => {
                self.to_single_image();
                self.tab = Tab::Sensor;
            }
            TabInfo => {
                self.tool_tab = workspace::ToolTab::Info;
                self.show_right = true;
            }
            TabEdit if self.in_people() => self.choose_tool(workspace::ToolTab::Edit),
            TabEdit => {
                self.tool_tab = workspace::ToolTab::Edit;
                self.show_right = true;
            }
            ShowInFinder => self.show_in_finder(),
            ShowInLibrary => self.show_in_folder(),
            SelectAll => {
                // in a text field: select the text; otherwise all photos of the view
                if ctx.egui_wants_keyboard_input() {
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key { key: egui::Key::A, physical_key: Some(egui::Key::A), pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
                    });
                } else if self.print_room.open {
                    // the print room: all photos of its filmstrip
                    if !ctx.memory(|m| m.top_modal_layer().is_some()) {
                        self.print_select_all();
                    }
                } else if self.in_grid() && people_ui::is_open() {
                    people_ui::request(people_ui::Request::SelectAll);
                } else {
                    self.lib.select_all();
                }
            }
            SelectNone if self.in_grid() && people_ui::is_open() => people_ui::request(people_ui::Request::SelectNone),
            SelectNone if self.print_room.open => self.print_select_none(),
            SelectNone => {
                self.lib.clear_selection();
                self.entries.iter_mut().for_each(|e| e.selected = false);
                // the single views always keep their active photo
                if !self.in_grid() {
                    self.lib.keep_active();
                }
            }
            NewCollection => self.lib.ask_new_collection(library::manage::COLLECTION, None),
            NewSmartCollection => self.lib.ask_new_collection(library::manage::SMART, None),
            NewCollectionSet => self.lib.ask_new_collection(library::manage::SET, None),
            ManageImports => self.lib.ask_imports_external(),
            ArrangeKeywords => {
                self.tab = Tab::Library;
                self.lib.open_arrange(None);
            }
            // in the print room ⌘P is its main button
            Print if self.print_room.open => self.print_room_main_key(ctx),
            Print => self.open_print_room(None, None),
            ContactPrint => self.open_contact_print(),
            LensRoom => lens_assign::request(lens_assign::Request::Room),
            CameraLooks => camera_looks_ui::open(self.lib.learn_candidates(), self.current_learn_key()),
            Compare => {
                self.compare = !self.compare;
                self.end_look_compare();
            }
            Before => {
                self.show_before = !self.show_before;
                self.end_look_compare();
                self.show_camera_jpeg = false;
            }
            CameraJpeg => self.toggle_camera_jpeg(),
            Clipping => self.show_clipping = !self.show_clipping,
            Info => self.info_next(),
            Panels => self.toggle_panels(false),
            Lights => self.cycle_lights(),
            FullPreview => self.toggle_preview_full(ctx),
            Loupe100 => {
                self.loupe = !self.loupe;
                self.loupe_hint_at = f64::NEG_INFINITY;
            }
            Shortcuts => self.show_shortcuts = !self.show_shortcuts,
            // Help → Show Logs: the logs folder in the Finder (trace log, crash log)
            ShowLogs => crash_log::show_folder(),
            // Help → Report a Problem: anonymised report, pre-filled issue in the browser
            ReportBug => bug_report::open(false),
            // Help → Create Performance Report: measures this computer, writes a local text file
            PerfReport => perf_report::open(),
            // Help → Support graineo …: only opens the browser, the app sends nothing
            Support => support::open(),
            About => self.brand.show_about(),
            // colour proof: display only – the layer draws again with another uniform, nothing is computed
            ProofToggle => {
                proof::toggle();
                ctx.request_repaint();
            }
            GamutWarning => {
                proof::toggle_warning();
                ctx.request_repaint();
            }
            Navigator => {
                navigator::toggle();
                ctx.request_repaint();
            }
            ProofFull | ProofSrgb | ProofP3 | ProofAdobe | ProofProPhoto => {
                if let Some(p) = cmd.proof() {
                    proof::set(p);
                }
                ctx.request_repaint();
            }
            _ => {}
        }
    }

    /// Apply the current image's settings to all photos in the folder (one ⌘Z; many photos in the background).
    fn apply_to_all(&mut self) {
        let Some(cur) = self.current.as_ref().map(|c| c.index) else { return };
        let s = develop::Settings { view: View::Developed, ..self.entries[cur].settings.clone() };
        let targets: Vec<usize> = (0..self.entries.len()).filter(|&i| i != cur && image_kind::of(&self.entries[i].path).editable() && developable_state(self.entries[i].state)).collect();
        let cmd = history::Command::new(history::Hint::sync(&self.entries[cur].name, None));
        self.settings_command(&targets, cmd, move |_| s.clone());
    }

    /// Keyboard shortcuts as in common photo apps (only when no input field has focus).
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some()) && ctx.egui_wants_keyboard_input();
        if typing {
            return;
        }
        self.darkroom_loupe_shortcuts(ctx);
        let (cmd, shift, alt) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift, i.modifiers.alt));
        let pressed = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        // Esc back to the grid – only if no tool needs Esc for itself (state before anything else)
        if pressed(egui::Key::Escape) && (masks_ui::finish(ctx) | remove_ui::finish(ctx)) {
            self.navigation.click.cancel();
            return;
        }
        // Esc first closes the list of the comparison's left side, then ends the look comparison (without a change)
        if pressed(egui::Key::Escape) && self.before.menu_open {
            self.before.menu_open = false;
            return;
        }
        if pressed(egui::Key::Escape) && self.look_compare.on {
            log("look comparison: ended with Esc");
            self.end_look_compare();
            return;
        }
        let esc_free = !masks_ui::painting(ctx) && !retouch_ui::has_selection(ctx) && self.crop_mode.is_none() && !self.wb_picker && !self.measure && self.preview_full.is_none() && !self.loupe && !self.compare && !self.profile_browser;
        // Esc only ever leaves the outermost temporary layer: first the app fullscreen (F), only
        // then (next Esc) the single view. Hidden panels (Tab/⇧Tab) stay as they are.
        if esc_free && pressed(egui::Key::Escape) {
            if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                self.screen_mode = 0;
            } else {
                self.to_grid();
            }
            return;
        }
        // ←/→ and rating keys: in event order in `shortcuts_ordered` (wertung.rs);
        // here only if that did not take them (e.g. crop)
        if !cmd && pressed(egui::Key::ArrowRight) {
            self.step(true, alt);
        } else if !cmd && pressed(egui::Key::ArrowLeft) {
            self.step(false, alt);
        }
        if self.grid && !cmd && (pressed(egui::Key::ArrowDown) || pressed(egui::Key::ArrowUp)) {
            let down = pressed(egui::Key::ArrowDown);
            let vis = self.visible();
            if let Some(pos) = self.current.as_ref().and_then(|c| vis.iter().position(|&v| v == c.index)) {
                let target = if down { (pos + self.grid_cols).min(vis.len() - 1) } else { pos.saturating_sub(self.grid_cols) };
                if target != pos {
                    self.select(vis[target], false);
                }
            }
        }
        // Common shortcuts: G = grid, E/↩ = loupe, D = Develop
        // Workspace: G = grid (Esc below), E/I = tabs (tasten_arbeitsbereich), D = Edit
        if !cmd && pressed(egui::Key::G) {
            self.to_grid();
        }
        if !cmd && pressed(egui::Key::D) {
            self.tool_tab = workspace::ToolTab::Edit;
            self.show_right = true;
        }
        if cmd && pressed(egui::Key::Z) {
            if shift {
                self.redo_latest();
            } else {
                self.undo_latest();
            }
        }
        if cmd && shift && pressed(egui::Key::C) {
            self.copy_settings(alt);
        }
        // ⇧⌘V / ⌥⇧⌘V / ⌥⇧⌘R / ⇧⌘R are menu shortcuts (paste, transfer, reset, rotate): handled as menu commands only
        if cmd && pressed(egui::Key::Slash) {
            self.show_shortcuts = !self.show_shortcuts;
        }
        if shortcuts::pressed(ctx, "export") { self.open_export(); }
        if shortcuts::pressed(ctx, "merge_hdr") { self.open_hdr(); }
        if self.current.is_some() && self.tab == Tab::Develop {
            if shortcuts::pressed(ctx, "snapshot") { self.take_snapshot(); }
            if shortcuts::pressed(ctx, "swap_sides") && self.compare_kind().is_some() { self.swap_sides(); }
        }
        if cmd && pressed(egui::Key::Backspace) {
            self.confirm_delete = true;
        }
        if cmd {
            return;
        }
        if shift && pressed(egui::Key::R) {
            self.toggle_camera_jpeg();
        }
        // Crop: R opens/applies, ↩ applies, Esc discards, X swaps the orientation
        if !shift && pressed(egui::Key::R) && self.current.is_some() {
            self.toggle_crop(true);
        }
        // A: straighten (draw a line along the horizon), also from within the image
        if !shift && pressed(egui::Key::A) && self.current.is_some() {
            self.open_geo_tool(align::GeoTool::Align);
        }
        // the filmstrip's stacks (library_series.rs): S opens / closes the active photo's stack, ↩ opens a collapsed
        // one (the keyboard's double click); ↩ does nothing else here outside the crop
        if self.crop_mode.is_none() && !self.lib.compare_open() {
            if shortcuts::pressed(ctx, "stack_toggle_single") {
                let a = self.lib.toggle_active_stack();
                self.library_actions(a);
                self.strip_follow_lib = Some(false);
            }
            if shortcuts::pressed(ctx, "stack_open_enter") && self.lib.enter_expand() {
                self.strip_follow_lib = Some(false);
            }
        }
        if self.crop_mode.is_some() {
            if pressed(egui::Key::Enter) {
                self.toggle_crop(true);
            } else if pressed(egui::Key::Escape) {
                if self.straighten_from.is_some() {
                    // first discard a started line, only then the crop mode
                    self.straighten_from = None;
                } else {
                    self.toggle_crop(false);
                }
            }
            if pressed(egui::Key::X) {
                if let Some(r) = self.crop_aspect {
                    self.set_crop_aspect(Some(1.0 / r));
                }
            }
        }
        if pressed(egui::Key::O) && self.current.is_some() {
            self.loupe = !self.loupe;
            self.loupe_hint_at = f64::NEG_INFINITY;
            self.grid = false;
        }
        if pressed(egui::Key::M) && self.current.is_some() {
            self.measure = !self.measure;
            self.tab = Tab::Sensor;
        }
        if pressed(egui::Key::W) && self.current.is_some() {
            self.wb_picker = !self.wb_picker;
            self.tab = Tab::Develop;
        }
        // size of the picker's sample (only while it is on)
        if self.wb_picker {
            if shortcuts::pressed(ctx, "eyedropper_smaller") {
                self.wb_pick.resize(false);
            }
            if shortcuts::pressed(ctx, "eyedropper_larger") || shortcuts::pressed(ctx, "eyedropper_larger_us") {
                self.wb_pick.resize(true);
            }
        }
        if !cmd && pressed(egui::Key::H) && self.current.is_some() {
            retouch_ui::toggle_overlays(ctx);
        }
        if shortcuts::pressed(ctx, shortcuts::RETOUCH) && self.current.is_some() {
            self.choose_tool(workspace::ToolTab::Retouch);
        }
        if pressed(egui::Key::Backslash) {
            self.show_before = !self.show_before;
            self.end_look_compare();
            self.show_camera_jpeg = false;
        }
        if !shift && pressed(egui::Key::V) {
            if let Some(i) = self.current.as_ref().map(|c| c.index) {
                let s = &mut self.entries[i].settings;
                s.monochrome = !s.monochrome;
            }
        }
        if pressed(egui::Key::J) {
            self.show_clipping = !self.show_clipping;
        }
        if shortcuts::pressed(ctx, "focus_frame") {
            self.toggle_focus();
        }

        // L = lights out: dimmed → off → normal
        if pressed(egui::Key::L) {
            self.cycle_lights();
        }
        // F (in `workspace_shortcuts`): the whole app fullscreen. Panels independently with Tab/⇧Tab;
        // Esc first leaves fullscreen, then the single view. (The former ⇧F screen modes are gone.)
        if self.preview_full.is_some() && pressed(egui::Key::Escape) {
            self.toggle_preview_full(ctx);
        }
        // Video in single view: space starts/pauses
        if pressed(egui::Key::Space) && self.not_developable.and_then(|i| self.entries.get(i)).is_some_and(|e| e.format == "VIDEO") {
            self.video.toggle_on();
        } else if (pressed(egui::Key::Z) || pressed(egui::Key::Space)) && !self.zoom_to_focus() {
            self.toggle_zoom();
        }
        // without ⌘: ⌘Y/⇧⌘Y belong to the colour proof
        if !cmd && pressed(egui::Key::Y) {
            self.compare = !self.compare;
            self.end_look_compare();
        }
        // Rating and flagging; with ⇧ then move to the next image
        // (not in crop mode – there X belongs to the orientation)
        if self.crop_mode.is_some() {
            return;
        }
        if pressed(egui::Key::T) {
            self.show_toolbar = !self.show_toolbar;
        }
        // Rating and flagging: `shortcuts_ordered` (immediate, active image, event order)
        let _ = shift;
    }

    /// Images that pass the filter (order as in the folder).
    /// Images of the filmstrip: the library's whole list (filtering happens there).
    fn visible(&self) -> Vec<usize> {
        (0..self.entries.len()).collect()
    }

    /// Next/previous visible image.
    fn step(&mut self, forward: bool, keep_view: bool) {
        let vis = self.visible();
        // also from a non-developable or unreachable image to its neighbour
        let Some(cur) = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable) else {
            if let Some(&first) = vis.first() {
                self.select(first, false);
            }
            return;
        };
        // After extending a selection, an ordinary arrow starts at its moving end.
        let extended_head = self.strip_ext.take().and_then(|(anchor, head)| {
            if anchor != cur { return None; }
            let (a, h) = (vis.iter().position(|&v| v == anchor)?, vis.iter().position(|&v| v == head)?);
            let expected: std::collections::HashSet<usize> = crate::library_select::span(a, h).map(|p| vis[p]).collect();
            let selected: std::collections::HashSet<usize> = vis.iter().copied().filter(|&i| self.entries[i].selected || i == cur).collect();
            (expected == selected).then_some(head)
        });
        let from = extended_head.unwrap_or(cur);
        let next = if forward { vis.iter().find(|&&i| i > from) } else { vis.iter().rev().find(|&&i| i < from) };
        self.nav_dir = if forward { 1 } else { -1 };
        if let Some(&n) = next {
            if extended_head.is_some() {
                self.entries.iter_mut().for_each(|e| e.selected = false);
                self.lib.set_selection(self.entries[n].image, &[self.entries[n].image]);
                self.strip_follow = Some(n);
            }
            self.select(n, keep_view);
        } else if let Some(head) = extended_head {
            self.entries.iter_mut().for_each(|e| e.selected = false);
            self.lib.set_selection(self.entries[head].image, &[self.entries[head].image]);
            self.strip_follow = Some(head);
            self.select(head, keep_view);
        }
    }

    /// Develop: after a stack command the library may have another active photo (the cover of a closed stack, the
    /// member a ⌥ step went to): open it once the new list is in the strip.
    fn follow_lib_active(&mut self) {
        let Some(keep_view) = self.strip_follow_lib else { return };
        if self.lib.list_pending() {
            return;
        }
        self.strip_follow_lib = None;
        if self.in_grid() {
            return;
        }
        let Some(a) = self.lib.active_image() else { return };
        let now = self.loading.or(self.current.as_ref().map(|c| c.index)).and_then(|i| self.entries.get(i)).map(|e| e.image);
        if now == Some(a) {
            return;
        }
        if let Some(k) = self.entries.iter().position(|e| e.image == a) {
            self.entries.iter_mut().for_each(|e| e.selected = false);
            self.select(k, keep_view);
        }
    }

    /// Remember the last used module (Library or Develop) – for the next start.
    fn remember_module(&mut self) {
        let latest = if self.tab == Tab::Library { "library" } else { "develop" };
        thread_local!(static RECENT: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") });
        if RECENT.with(|z| z.get()) != latest {
            RECENT.with(|z| z.set(latest));
            if let Some(f) = app_file("module.txt") {
                let _ = std::fs::write(f, latest);
            }
        }
    }

    /// Indices of the really selected photos (filmstrip and library grid), without the forcibly counted active one.
    pub(crate) fn chosen_entries(&self) -> Vec<usize> {
        let raster: std::collections::HashSet<i64> = self.lib.selection().into_iter().collect();
        (0..self.entries.len()).filter(|&i| self.entries[i].selected || raster.contains(&self.entries[i].image)).collect()
    }

    /// Indices of the selection (at least the current image).
    fn selection(&self) -> Vec<usize> {
        if self.in_grid() {
            let target = self.lib.target_images();
            return self.entries.iter().enumerate().filter_map(|(i, e)| target.contains(&e.image).then_some(i)).collect();
        }
        let mut v = self.chosen_entries();
        if let Some(c) = self.loading.or(self.not_developable).or(self.current.as_ref().map(|c| c.index)) {
            if !v.contains(&c) {
                v.push(c);
                v.sort();
            }
        }
        v
    }

    /// Fit ↔ 100 % (Z, space).
    fn toggle_focus(&mut self) {
        self.show_focus = !self.show_focus;
        focus_frame::reset_status();
    }

    /// Z with the focus area shown: 100 % centred on the area the camera reports (false: no area, or zoomed in).
    fn zoom_to_focus(&mut self) -> bool {
        if !self.show_focus || self.zoom > 1.01 {
            return false;
        }
        let Some(path) = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.path.clone()) else { return false };
        let Some(u) = focus_frame::target(&path, self.now()) else { return false };
        self.navigation.zoom.cancel();
        self.zoom = self.one_to_one.max(1.0);
        self.pan = egui::vec2((0.5 - u[0]) * self.fit_size.x, (0.5 - u[1]) * self.fit_size.y) * self.zoom;
        self.view_changed_at = self.now();
        true
    }

    fn toggle_zoom(&mut self) {
        self.navigation.zoom.cancel();
        if self.zoom > 1.01 {
            self.zoom = 1.0;
        } else {
            self.zoom = self.one_to_one.max(1.0);
        }
        self.pan = egui::Vec2::ZERO;
        self.view_changed_at = self.now();
    }

    /// One step of the stress test (see `stress.rs`).
    fn stress_step(&mut self, ctx: &egui::Context) {
        let Some(st) = self.stress.as_mut() else { return };
        ctx.request_repaint();
        let now = ctx.input(|i| i.time);
        let cur = self.current.as_ref().map(|c| c.index);
        // zoom guard: is the whole image at full resolution in place for the current settings?
        let full_ready = self.current.as_ref().is_some_and(|c| {
            self.entries.get(c.index).is_some_and(|e| {
                c.detail.as_ref().is_some_and(|(_, r, ds)| *ds == e.settings && r[2] >= c.loaded.full_size[0] && r[3] >= c.loaded.full_size[1])
            })
        });
        let busy = matches!(self.refine, Refine::Working { .. }) || self.loading.is_some() || self.detail_pending || self.rendering || self.preparing || self.spp_pending.is_some();
        st.set_full_ready(full_ready && !busy);
        match st.step(now, cur, self.entries.len()) {
            stress::Action::Wait => {}
            stress::Action::ApplyMask => {
                if let Some(i) = cur {
                    self.entries[i].settings.local = masks::Document::from_json(
                        r#"{"version":2,"layers":[{"mask":{"type":"radial","center":[0.5,0.5],"radii":[0.45,0.45],"angle":0.0,"feather":0.5},"adjustments":{"exposure":0.5}}]}"#,
                    )
                    .ok();
                    log("stress: mask applied (radial, exposure +0.5)");
                }
            }
            stress::Action::Select(i) => {
                log(&format!("stress: select {i}"));
                self.select(i, false);
            }
            stress::Action::Zoom(z, [dx, dy]) => {
                self.zoom = z;
                self.pan = egui::vec2(dx, dy) * self.fit_size * z;
                self.view_changed_at = self.now();
            }
            stress::Action::Pan([dx, dy]) => {
                self.pan = egui::vec2(dx, dy) * self.fit_size * self.zoom;
                self.view_changed_at = self.now();
            }
            stress::Action::Fit => {
                self.zoom = 1.0;
                self.pan = egui::Vec2::ZERO;
                self.view_changed_at = self.now();
            }
            stress::Action::Edit(kind) => {
                if let Some(i) = cur {
                    let s = &mut self.entries[i].settings;
                    match kind {
                        stress::EditKind::Exposure(v) => s.exposure = v as f64,
                        stress::EditKind::NoiseStep(d) => s.nr_luma = (s.nr_luma + 0.25 * d as f64).clamp(develop::NR_OFF, 1.0),
                    }
                }
            }
            stress::Action::Hover(k) => {
                // like the pointer over the film list: hover preview of film k (settings differ per step)
                let films = develop::film::films();
                if let (Some(c), Some(f)) = (&self.current, films.get(k % films.len().max(1))) {
                    let mut h = self.entries[c.index].settings.clone();
                    h.film.active = true;
                    h.film.stock = f.id.clone();
                    h.exposure += 0.001 * (k + 1) as f64;
                    if self.hover_preset.as_ref() != Some(&h) {
                        stress::HOVER_SENT.fetch_add(1, Ordering::Relaxed);
                        self.worker.send(Job::HoverRender { index: c.index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: h.clone(), supersede: true });
                        self.hover_preset = Some(h);
                        self.film_hover = true;
                    }
                }
            }
            stress::Action::Check { waited, timeout } => {
                let refining = matches!(self.refine, Refine::Working { .. });
                let busy = refining || self.loading.is_some() || self.detail_pending || self.rendering || self.preparing;
                if st.hover_enabled() {
                    // the storm ends when every sent preview has come back (rendered or superseded)
                    let open = stress::HOVER_SENT.load(Ordering::Relaxed) > stress::HOVER_RENDERED.load(Ordering::Relaxed) + stress::HOVER_SUPERSEDED.load(Ordering::Relaxed);
                    if open && !timeout {
                        return;
                    }
                    let line = stress::hover_report();
                    log(&line);
                    eprintln!("{line}");
                    if open {
                        eprintln!("STRESS HANGS after {waited:.1} s idle: hover previews missing");
                        std::process::exit(1);
                    }
                }
                if !busy || timeout {
                    let state = format!("current {a}, refining {refining}, loading {b}, detail {c}, render {d}, preparation {e}, spp_pending {f}, spp_due {g}, spp_early {h}, precompute {i}, SPP preview {j}", a = format!("{:?}", cur), refining = refining, b = format!("{:?}", self.loading), c = self.detail_pending, d = self.rendering, e = self.preparing, f = self.spp_pending.is_some(), g = format!("{:?}", self.spp_due), h = format!("{:?}", self.spp_early), i = format!("{:?}", self.spp_prefetching.as_ref().map(|(i, _)| *i)), j = self.current.as_ref().is_some_and(|c| c.loaded.previews.spp.is_some()));
                    // duplicate whole images, outdated jobs dropped, gates still held or awaited
                    let planning = jobs::planning_report();
                    log(&planning);
                    eprintln!("{planning}");
                    if busy {
                        eprintln!("STRESS HANGS after {waited:.1} s idle: {state}");
                        std::process::exit(1);
                    }
                    if st.zoom_enabled() {
                        let line = stress::zoom_report();
                        log(&line);
                        eprintln!("{line}");
                    }
                    eprintln!("STRESS OK: idle after {waited:.1} s ({state})");
                    std::process::exit(0);
                }
            }
        }
    }

    /// Zoom percentages offered for the current view (only those above the fit).
    fn zoom_steps(&self) -> Vec<f32> {
        let fit_pct = 100.0 / self.one_to_one.max(1e-3);
        let zoom_steps: &[f32] = if self.tab == Tab::Sensor { &[25.0,50.0,100.0,200.0,400.0,800.0,1600.0,3200.0] } else if self.darkroom_active() { &[25.0,50.0,100.0,200.0,400.0,800.0,1200.0] } else { &[25.0,50.0,100.0,200.0,400.0] };
        zoom_steps.iter().copied().filter(|p| *p > fit_pct * 1.02).collect()
    }

    /// Image zoom one step in (`dir` > 0) or out (`dir` < 0, below the first step: fit); `dir` == 0: fit.
    /// Used by the − / + buttons of the toolbar and by ⌘+ / ⌘− / ⌘0.
    fn zoom_step(&mut self, dir: i32) {
        let o = self.one_to_one.max(1e-3);
        let pct = self.zoom / o * 100.0;
        let steps = self.zoom_steps();
        let target = match dir {
            0 => None,
            d if d < 0 => steps.iter().rev().copied().find(|p| *p < pct * 0.99),
            _ => match steps.iter().copied().find(|p| *p > pct * 1.01) {
                Some(p) => Some(p),
                None => return,
            },
        };
        match target {
            None => {
                self.set_zoom(1.0);
                self.pan = egui::Vec2::ZERO;
            }
            Some(p) => self.set_zoom((o * p / 100.0).max(1.0)),
        }
    }

    /// ⌘+ / ⌘− / ⌘0 (Ctrl on Windows and Linux) zoom the photo in Develop and in the comparison; the darkroom keeps them
    /// for its grain loupe. ⇧⌘+ / ⇧⌘− / ⇧⌘0 change the UI size everywhere (`prefs::scale_shortcuts`).
    fn zoom_shortcuts(&mut self, ctx: &egui::Context) {
        use crate::shortcuts::pressed;
        if prefs::scale_shortcuts(ctx) || self.darkroom_active() || !(self.tab == Tab::Develop || self.compare_shown()) {
            return;
        }
        if pressed(ctx, "image_zoom_in") || pressed(ctx, "image_zoom_in_us") {
            self.zoom_step(1);
        } else if pressed(ctx, "image_zoom_out") {
            self.zoom_step(-1);
        } else if pressed(ctx, "image_zoom_fit") {
            self.zoom_step(0);
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.navigation.zoom.cancel();
        let old = self.zoom;
        self.zoom = zoom.max(1.0);
        self.pan *= self.zoom / old;
        self.view_changed_at = self.now();
    }

    /// Navigator: thumbnail with crop frame; clicking/dragging moves the visible area.
    fn navigator(&mut self, ui: &mut egui::Ui) {
        // Fixed height so nothing jumps when switching images; while the preview loads,
        // the filmstrip thumbnail stands in its place.
        const H: f32 = 150.0;
        let w = ui.available_width();
        let pending = self.loading.and_then(|i| self.entries[i].thumb.clone());
        let hover = self.hover_shown().map(|(_, t)| t.clone());
        let pending = hover.or(pending);
        let tex = pending.or_else(|| self.current.as_ref().and_then(|c| c.texture.clone().or_else(|| self.entries[c.index].thumb.clone())));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, H), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, lr::PANEL_DARK);
        let Some(tex) = tex else {
            self.zoom_presets(ui);
            return;
        };
        // The navigator shows the crop, in crop mode the whole image
        let (full, frame) = match self.current.as_ref().filter(|_| self.loading.is_none()) {
            Some(c) => {
                let [fw, fh] = c.loaded.full_size;
                let st = &self.entries[c.index].settings;
                let cam = c.loaded.cal.orientation;
                let f = if self.crop_mode.is_some() { Frame::full_settings(st, fw, fh, cam) } else { Frame::from_settings(st, fw, fh, cam) };
                ([0.0, 0.0, fw as f32, fh as f32], f)
            }
            None => {
                let t = tex.size_vec2();
                ([0.0, 0.0, t.x, t.y], Frame::full(t.x as usize, t.y as usize, 0))
            }
        };
        let s = egui::vec2(frame.w, frame.h);
        let size = s * (w / s.x).min(H / s.y);
        let img = egui::Rect::from_center_size(rect.center(), size);
        // the preview with preserved gamut / in HDR of the render shown, if this is its 8-bit preview
        let id = self.current.as_ref().filter(|c| self.loading.is_none() && c.texture.as_ref().is_some_and(|t| t.id() == tex.id())).and_then(|c| {
            let rendered = self.rendered_for.as_ref().filter(|(i, _)| *i == c.index).map(|(_, s)| s);
            self.display.preview_id(c.index, rendered)
        });
        paint_src_quad(&painter.with_clip_rect(img), id.map_or(tex.id(), |(id, _)| id), full, &frame, img, Color32::WHITE);
        if self.zoom > 1.01 && self.fit_size.x > 0.0 {
            let full = self.fit_size * self.zoom;
            let view = self.view_size.max(egui::Vec2::splat(1.0));
            let center = egui::vec2(0.5, 0.5) - self.pan / full;
            let half = (view / full) * 0.5;
            let r = egui::Rect::from_min_max(
                img.min + (center - half).clamp(egui::Vec2::ZERO, egui::Vec2::splat(1.0)) * img.size(),
                img.min + (center + half).clamp(egui::Vec2::ZERO, egui::Vec2::splat(1.0)) * img.size(),
            );
            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5, Color32::WHITE), egui::StrokeKind::Middle);
            if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
                let u = ((p - img.min) / img.size()).clamp(egui::Vec2::ZERO, egui::Vec2::splat(1.0));
                self.pan = (egui::vec2(0.5, 0.5) - u) * full;
                self.view_changed_at = self.now();
            }
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let u = ((p - img.min) / img.size()).clamp(egui::Vec2::ZERO, egui::Vec2::splat(1.0));
                self.zoom = self.one_to_one.max(1.0);
                self.pan = (egui::vec2(0.5, 0.5) - u) * self.fit_size * self.zoom;
                self.view_changed_at = self.now();
            }
        }
        self.zoom_presets(ui);
    }

    fn zoom_presets(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let steps = zoom::preset_steps(self.tab == Tab::Sensor);
        let mut sel = if self.zoom <= 1.01 {
            0
        } else {
            steps.iter().position(|f| (self.zoom - self.one_to_one * f).abs() < 0.01).map_or(usize::MAX, |p| p + 1)
        };
        let changed = if self.tab == Tab::Sensor {
            let first = lr::segmented(ui, &mut sel, &[(0, "Einpassen"), (1, "100 %"), (2, "200 %"), (3, "400 %")]);
            let second = lr::segmented(ui, &mut sel, &[(4, "800 %"), (5, "1600 %"), (6, "3200 %")]);
            first || second
        } else {
            lr::segmented(ui, &mut sel, &[(0, "Einpassen"), (1, "100 %"), (2, "200 %"), (3, "400 %")])
        };
        if changed {
            if sel == 0 {
                self.set_zoom(1.0);
                self.pan = egui::Vec2::ZERO;
            } else {
                self.set_zoom(self.one_to_one * steps[sel - 1]);
            }
        }
    }
}

impl eframe::App for App {
    /// As in common photo apps, Tab belongs to the panels: the key is intercepted before egui
    /// uses it to jump through the controls (except when typing in a text field).
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        test_image::input(ctx, raw_input); // Test hook: GRAINEO_INPUT
        // a click counts with the modifier keys held at the press (⌘/⇧-click, ⇧-frame), see click_mods.rs
        click_mods::complete(raw_input);
        // the user is at work: background work stays at its usual pace (see `jobs::idle`)
        // (moving the pointer with a button held counts too: dragging a slider)
        let dragging = ctx.input(|i| i.pointer.any_down());
        if raw_input.events.iter().any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. } | egui::Event::Text(_)) || (dragging && matches!(e, egui::Event::PointerMoved(_)))) {
            jobs::note_input();
        }
        test_image::native_keys(ctx); // Test hook: GRAINEO_NATIVE_KEYS (macOS menu shortcuts)
        test_image::log_keys(raw_input);
        // Photos (or folders with photos) dropped on the window open the import; otherwise
        // looks (valid .cube LUTs, look profiles) are offered for the look list
        let dropped: Vec<PathBuf> = raw_input.dropped_files.iter().map(|f| f.path().to_path_buf()).collect();
        // the lens assistant (step "Aufnahmen") takes a drop for itself (lens_wizard.rs)
        if lens_wizard::take_dropped(&dropped) {
            raw_input.dropped_files.clear();
        }
        let dropped: Vec<PathBuf> = raw_input.dropped_files.iter().map(|f| f.path().to_path_buf()).collect();
        if !dropped.is_empty() && self.lib.import_store(ctx, &dropped) {
            raw_input.dropped_files.clear();
        }
        // the rest: looks are checked in the background and added only after asking (never the settings by themselves)
        let rest: Vec<PathBuf> = raw_input.dropped_files.drain(..).map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect();
        film_profiles_ui::dropped(ctx, rest);
        // Rating keys that are later taken from the events must not
        // leave an old click job either.
        if raw_input.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })) {
            self.navigation.click.cancel();
            self.navigation.zoom.cancel();
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        raw_input.events.retain(|e| match e {
            egui::Event::Key { key: egui::Key::Tab, pressed, modifiers, .. } => {
                if *pressed {
                    self.tab_pressed = Some(modifiers.shift);
                }
                false
            }
            _ => true,
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        frame_watch::begin(ui.ctx()); // Test hook: frame times with GRAINEO_FRAME_LOG
        frame_watch::repaint_causes(ui.ctx()); // Test hook: GRAINEO_REPAINT_LOG
        // End-of-pass callbacks are kept by egui until the program ends: register them once, not every frame
        // (registered per frame they piled up – after an hour most of the UI thread went into calling them)
        static END_PASS: std::sync::Once = std::sync::Once::new();
        END_PASS.call_once(|| {
            ui.ctx().on_end_pass("i18n-audit", std::sync::Arc::new(|c| menu::i18n::audit_image(c)));
            ui.ctx().on_end_pass("zoom-test-frame", std::sync::Arc::new(|_| zoom_test_frame(false, 0.0)));
            ui.ctx().on_end_pass("frame-watch", std::sync::Arc::new(|_| frame_watch::end()));
        });
        zoom_test_frame(true, self.zoom); // Test hook: frame times with GRAINEO_ZOOM_TEST
        remote_access::frame(self, ui.ctx()); // browser access: start by setting, changes made in the browser
        self.open_requests(&ui.ctx().clone()); // a folder or photo handed over by a second start
        // narrow window: collapse a side panel once when the window falls below the threshold
        if self.preview_full.is_none() {
            let dark = self.darkroom_active();
            let (left, right) = if dark { (!self.darkroom.left_collapsed, !self.darkroom.right_closed) } else { (self.show_left, self.show_right) };
            match self.narrow.step(ui.ctx().content_rect().width(), left, right) {
                Some(narrow_window::Side::Left) if dark => self.darkroom.left_collapsed = true,
                Some(narrow_window::Side::Right) if dark => self.darkroom.right_closed = true,
                Some(narrow_window::Side::Left) => self.show_left = false,
                Some(narrow_window::Side::Right) => self.show_right = false,
                None => {}
            }
        }
        // closing while changes are still on their way into the library: wait visibly (quit.rs)
        {
            let pending = !self.lib.writes_finished() || library::store::saving() || library_turn::writing() || self.multi_busy() || subject_ui::storing();
            if self.turn_ops != self.lib.ops {
                // a move or another library operation came after the last turn: ⌘Z first undoes that
                self.turn_ops = self.lib.ops;
                self.turn_undo.other_action();
            }
            let ctx = ui.ctx().clone();
            quit::hold_close(&ctx, pending, library::store::unsaved(), || self.save_current());
        }
        // develop settings that could not be saved: visible, and tried again every half minute
        activity::watch_unsaved(&mut self.notes);
        // Results of the system dialogs (run on a helper thread, see dateidialog.rs)
        if let Some(dir) = file_dialog::fetch_one("ordner-oeffnen") {
            self.open_folder(dir);
        }
        // a write target in an Apple Photos library or another program's package was refused in the dialog
        if let Some((p, why)) = file_dialog::take_refused() {
            self.notes.fault(activity::Fault { title: crate::tr("Ziel nicht verwendet").into(), cause: menu::i18n::tr_msg(&why).into_owned(), subject: p.display().to_string(), reveal: None, retry: None });
        }
        self.darkroom_file_dialogs();
        self.tool_rail_follow();
        // Photos dropped on the app icon in the Dock ("Open with")
        let opened = dock_open::fetch();
        if !opened.is_empty() {
            let c = ui.ctx().clone();
            self.lib.import_store(&c, &opened);
            c.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        display_color::apply(_frame); // display is colour-managed (sRGB → monitor profile)
        window_geometry::check(_frame, ui.ctx());
        window_geometry::test_step(ui.ctx());
        photo_display::frame(_frame, self.display.hdr_shown());
        self.video_view = None;
        // the hover preview ended: its image goes too (a new hover never starts with an old picture)
        if self.hover_preset.is_none() {
            self.hover_tex = None;
        }
        let ctx = ui.ctx().clone();
        if film_profiles_ui::frame(&ctx) | learn_look_ui::generation_changed(&ctx) {
            self.film_previews.forget();
            self.hover_preset = None; self.film_hover = false;
            self.rendered_for = None;
        }
        if let Some(current) = &self.current {
            if current.loaded.previews.foreign.is_some() { rawdev::film_profiles::remember_reference(&mut self.entries[current.index].settings); }
        }
        if let Some(export) = &mut self.profile_export {
            match export.step(&ctx) {
                film_profiles_ui::ExportStep::Waiting => {},
                film_profiles_ui::ExportStep::Aborted => { self.profile_export = None; self.exporting = false; self.export_status = "Abgebrochen".into(); },
                film_profiles_ui::ExportStep::Done(job) => { self.profile_export = None; self.export_status = crate::tr("Export startet …").into(); self.worker.send(job); },
            }
        }
        prefs::ui(&ctx); // Preferences (⌘,): disk cache
        self.lens_ui(&ctx); // lens assignment, assistant "Objektiv hinzufügen", window OBJEKTIVE
        test_image::step(&ctx); // Test hook: GRAINEO_SCREENSHOT
        test_image::keys(&ctx); // Test hook: GRAINEO_KEYS
        // Test hook: GRAINEO_FULL_PREVIEW=1 switches on the fullscreen preview (F) once after 5 s
        static PREVIEW_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if crate::env::var_os("GRAINEO_FULL_PREVIEW").is_some() && self.current.is_some() && ctx.input(|i| i.time) > 5.0 && !PREVIEW_TEST.swap(true, std::sync::atomic::Ordering::Relaxed) {
            self.toggle_preview_full(&ctx);
        }
        // Test hook: GRAINEO_SENSOR_VIEW=raw|top|middle|bottom|chroma|clipping (only with GRAINEO_DATA_DIR) shows this
        // sensor view once the image is open (screenshots of the sensor views)
        static SENSOR_VIEW_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && self.current.is_some() && !SENSOR_VIEW_TEST.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(v) = crate::env::var("GRAINEO_SENSOR_VIEW") {
                let view = match v.as_str() {
                    "raw" => Some(View::RawComposite),
                    "top" => Some(View::Layer(2)),
                    "middle" => Some(View::Layer(1)),
                    "bottom" => Some(View::Layer(0)),
                    "chroma" => Some(View::Chroma),
                    "clipping" => Some(View::Clipping),
                    _ => None,
                };
                if let (Some(view), Some(i)) = (view, self.current.as_ref().map(|c| c.index)) {
                    SENSOR_VIEW_TEST.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.entries[i].settings.view = view;
                }
            }
        }
        // Splash window: ready as soon as the first image is shown or no folder is open
        let ready = self.in_grid() || self.entries.is_empty() || (self.current.is_some() && self.loading.is_none());
        let status = if self.entries.is_empty() { crate::tr("Bereit").to_string() } else { crate::tr("Lade Ordner …").to_string() };
        self.brand.splash(&ctx, ready, &status);
        self.handle_results(&ctx);
        zoom::demo(self, &ctx);
        // Prepare the library per frame (grid: its keys too), then the shared image source
        self.test_sequence(&ctx);
        self.test_matrix(&ctx);
        // open dialog (egui modal): its keys belong to it alone
        // likewise the People view in the library: its keys (↩, ⌫, arrows, N …) belong to it
        // likewise "RAW+JPEG doppelt" (its list and review take arrows and space)
        let dialog_open = ctx.memory(|m| m.top_modal_layer().is_some()) || camera_looks_ui::is_open() || (self.in_grid() && (people_ui::is_open() || self.lib.pairs_view_open())) || self.print_room.open;
        // Menu shortcuts that reach egui: on Windows always (the menu bar there only shows them), on the Mac when
        // AppKit did not match the key (e.g. punctuation on some keyboard layouts). A key the menu took never
        // reaches egui. Taken (consumed) before every other key handler, so nothing runs twice.
        let menu_keys = menu::shortcut_without_menu(&ctx);
        // A key in the grid (outside a text field) may act on the photo list: bring it up to date first
        if self.in_grid() && !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))) {
            self.sync_source(false);
        }
        self.sensor_raw_update(&ctx);
        self.apple_update(&ctx);
        if !dialog_open {
            // the comparison first: it takes the keys it handles (photo_compare_ui.rs)
            self.compare_keys(&ctx);
            self.compare_entry_keys(&ctx);
            self.sensor_shortcuts(&ctx);
            self.shortcuts_ordered(&ctx);
        } else if self.in_people() && !ctx.memory(|m| m.top_modal_layer().is_some()) {
            // the people view: stars, flags and labels act on the chosen face's photo
            self.shortcuts_ordered(&ctx);
        }
        let grid_shortcuts = self.in_grid() && !self.lib.import_open() && !ctx.egui_wants_keyboard_input() && !dialog_open;
        // ⇧S everywhere the grid or the filmstrip shows the list (grid, loupe, comparison, Develop, darkroom), as the
        // button "all series"; where it does nothing (view "Serien", no stacks) the status line says why
        if !self.lib.import_open() && !ctx.egui_wants_keyboard_input() && !dialog_open && shortcuts::pressed(&ctx, "stack_toggle_all") {
            let a = self.lib.stack_menu_command(library_ui::StackMenu::ToggleAll);
            self.library_actions(a);
        }
        self.lib.can_paste = self.clipboard.is_some();
        if !self.copy_dialog {
            self.copy_from = None;
        }
        let a = {
            let _t = frame_watch::section("library");
            self.lib.prepare(&ctx, grid_shortcuts)
        };
        self.library_actions(a);
        // the comparison wished for (button above the grid, N, context menu, series)
        self.compare_requests();
        // "Not in the library" is a list of folders, not photos: chosen in a single-image view it opens in the
        // library (otherwise the centre only said "no image open")
        if !self.in_grid() && self.lib.not_in_catalog_view() {
            self.to_grid();
        }
        self.workspace_shortcuts(&ctx);
        // In the grid the photo list of Develop/filmstrip (all photos shown: 100–800 ms with 66 000)
        // no longer follows every search result – it stalled typing and the fade-in. Keys, menu commands and actions
        // of the grid and leaving it (workspace.rs) bring it up to date first.
        // (with the Edit tool open in the grid the active photo is developed: there it follows as before)
        if !self.in_grid() || (self.tool_tab.editing() && self.show_right) || self.lib.compare_open() {
            self.sync_source(false);
        }
        self.follow_lib_active();
        self.restore_view();
        self.stress_step(&ctx);
        self.relieve_memory(&ctx);
        if self.current.is_some() && self.loading.is_none() {
            match self.test_open.take() {
                Some(t @ ("crop" | "align" | "perspective")) => {
                    self.toggle_crop(true);
                    self.set_geo_tool(match t {
                        "align" => align::GeoTool::Align,
                        "perspective" => align::GeoTool::Perspective,
                        _ => align::GeoTool::Crop,
                    });
                    // Test hook: GRAINEO_PERSP="vertical,horizontal[,angle]" sets the perspective
                    if let (Ok(v), Some(c)) = (crate::env::var("GRAINEO_PERSP"), self.current.as_ref()) {
                        let n: Vec<f32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
                        let ([fw, fh], cam, i) = (c.loaded.full_size, c.loaded.cal.orientation, c.index);
                        geometry::set_transform(&mut self.entries[i].settings, n.get(2).copied().unwrap_or(0.0), n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0), fw, fh, cam);
                        self.persp_grid_until = f64::INFINITY;
                    }
                }
                Some("about") => self.brand.show_about(),
                Some("prefs") => prefs::open(),
                // Test hook: GRAINEO_TAB=print opens the print room (GRAINEO_PRINT_TEMPLATE=<built-in name>, GRAINEO_PRINT_PAGE=<n>)
                Some("print") => {
                    let name = crate::env::var("GRAINEO_PRINT_TEMPLATE").ok();
                    self.open_print_room(None, name.as_deref());
                    self.print_room_test_hooks();
                    if let Some(p) = crate::env::var("GRAINEO_PRINT_PAGE").ok().and_then(|v| v.parse::<usize>().ok()) {
                        self.print_room.set_page(p.saturating_sub(1));
                    }
                }
                // Test hook: GRAINEO_ZOOM=<factor> magnifies beyond 100 % (e.g. 2 = 200 %)
                Some("zoom") if self.one_to_one > 1.0 => self.set_zoom(self.one_to_one * crate::env::var("GRAINEO_ZOOM").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0)),
                Some("zoom") => self.test_open = Some("zoom"),
                _ => {}
            }
        }
        if std::mem::take(&mut self.pending_paste) {
            self.paste_settings();
        }
        let mut commands = self.menu.as_ref().map(|m| m.poll()).unwrap_or_default();
        commands.extend(menu_keys);
        for cmd in commands {
            self.run_cmd(cmd, &ctx);
        }
        self.zoom_shortcuts(&ctx);
        self.print_room_poll(&ctx);
        if !self.print_room.open {
            self.darkroom_shortcuts(&ctx);
        }
        self.darkroom_compare_update();
        self.darkroom_strip_update(&ctx);
        self.darkroom_contact_update(&ctx);
        self.darkroom_marker_update();
        // retouch circles: visible while the "Retouch" tool is open, mask outlines while "Masks" is open (H inverts)
        {
            let tool = (!self.in_grid() && self.show_right && !self.darkroom_active()).then(|| self.tool_rail_active());
            retouch_ui::set_panel_open(&ctx, tool == Some(workspace::ToolTab::Retouch), tool == Some(workspace::ToolTab::Masks));
            hdr_paint::set_open(&ctx, tool == Some(workspace::ToolTab::Merge) && self.merge_info().is_some());
        }
        if !self.lib.import_open() && self.tab != Tab::Library && !dialog_open {
            self.shortcuts(&ctx);
        }

        // Linux: the menu bar in the window (macOS and Windows: system menu bar), not in the fullscreen preview (F)
        if self.preview_full.is_none() {
            if let Some(m) = &self.menu {
                m.bar(ui);
            }
        }
        // the print room takes the whole window (Esc goes back to the view before)
        if self.print_room.open {
            self.print_room_ui(ui);
            return;
        }
        // viewing a backup: fixed band at the very top (above everything, also in the darkroom)
        backup_view_ui::band(ui);
        let full = self.preview_full.is_some();
        // Edge strips as in common photo apps: a narrow strip with a triangle at each
        // edge shows and hides the panel by click; hovering a hidden
        // panel shows it temporarily until the mouse leaves it again.
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let darkroom = self.darkroom_active();
        let edges = !full && !darkroom;
        if edges {
            let (show, peek) = (self.show_top, self.peek[0]);
            if edge_strip(ui, "edge-top", Edge::Top, show || peek) {
                self.show_top = !show;
                log("edge strip top toggled");
                self.peek[0] = false;
                self.peeks[0].reset();
            }
        }
        if darkroom {
            // same height as in Develop: narrow edge strip (without toggle) and the module bar in the
            // same panel as elsewhere – so nothing jumps when switching and the panels look the same everywhere
            // In the fullscreen preview (F) Develop has no edge strip either: the panel starts at the very top
            if !full {
                egui::Panel::top("dk-rand").show_separator_line(false).resizable(false).exact_size(12.0).frame(egui::Frame::new().fill(lr::PANEL_DARK)).show(ui, |_| {});
            }
            egui::Panel::top("top").resizable(false).show(ui, |ui| self.top_bar(ui));
        } else if self.show_top || self.peek[0] {
            let r = egui::Panel::top("top").resizable(false).show(ui, |ui| self.top_bar(ui)).response.rect;
            self.panel_rects[0] = Some(r);
        }
        // a folder only being viewed (⌘O): fixed band below the navigation, the same place in grid, Develop and darkroom
        self.lib.view_folder_band(ui, full, darkroom && self.darkroom.image_only);
        // no status bar any more: image info in the image (I), messages and background work in the one status in the title bar,
        // export and shortcuts in the menu (File, Help)
        // update notice (overlay at the bottom right)
        if let Some(t) = update_ui::frame(ui.ctx(), self.message_bottom()) {
            self.status = t;
        }
        self.take_status(ui.ctx());
        self.remember_module();
        let raster = self.in_grid();
        // Filmstrip only in single view (in the grid the centre already shows all images); the comparison in the
        // library has it too (it is the selection)
        let no_strip = raster && !self.lib.compare_open();
        if edges && !no_strip {
            let (show, peek) = (self.show_filmstrip, self.peek[1]);
            if edge_strip(ui, "edge-bottom", Edge::Bottom, show || peek) {
                self.show_filmstrip = !show;
                log("edge strip filmstrip toggled");
                self.peek[1] = false;
                self.peeks[1].reset();
            }
        }
        if darkroom && !full && !self.darkroom.image_only {
            let shown = self.show_filmstrip;
            if edge_strip(ui, "dk-edge-bottom", Edge::Bottom, shown) {
                self.show_filmstrip = !shown;
            }
        }
        let strip_shown = !no_strip && (self.show_filmstrip || self.peek[1]) && !(darkroom && self.darkroom.image_only);
        let t_strip = lr::slide(&ctx, "filmstrip", strip_shown);
        if !no_strip && t_strip > 0.001 && t_strip < 0.999 {
            egui::Panel::bottom("filmstrip-gleiten").resizable(false).exact_size(self.strip_height * t_strip).frame(egui::Frame::side_top_panel(ui.style())).show(ui, |_| {});
        } else if strip_shown {
            // Height adjustable by dragging the top edge, from a narrow row of photos (about 48 pt) to large
            // thumbnails; the height is remembered once the drag ends
            let frame = egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin { left: 6, right: 6, top: 4, bottom: 3 });
            let r = egui::Panel::bottom("filmstrip")
                .resizable(true)
                .frame(frame)
                .default_size(self.strip_height)
                .size_range(filmstrip_layout::PANEL_MIN..=filmstrip_layout::PANEL_MAX)
                .show(ui, |ui| self.filmstrip(ui))
                .response
                .rect;
            self.panel_rects[1] = Some(r);
            if (r.height() - self.strip_height).abs() > 0.5 && !ui.input(|i| i.pointer.any_down()) {
                self.strip_height = r.height();
                library::settings::set("filmstripHeight", serde_json::json!((r.height() * 2.0).round() / 2.0));
            }
        }
        // Edge strips as in Develop: a click collapses and expands the left or right side of the darkroom
        let darkroom_edges = darkroom && !full && !self.darkroom.image_only;
        if edges {
            let (show, peek) = (self.show_left, self.peek[2]);
            if edge_strip(ui, "edge-left", Edge::Left, show || peek) {
                self.show_left = !show;
                log("edge strip left toggled");
                self.peek[2] = false;
                self.peeks[2].reset();
            }
            let (show, peek) = (self.show_right, self.peek[3]);
            if edge_strip(ui, "edge-right", Edge::Right, show || peek) {
                self.show_right = !show;
                log("edge strip right toggled");
                self.peek[3] = false;
                self.peeks[3].reset();
            }
        }
        if darkroom_edges {
            let collapsed = self.darkroom.left_collapsed;
            if edge_strip(ui, "dk-edge-left", Edge::Left, !collapsed) {
                self.darkroom.left_collapsed = !collapsed;
            }
            let collapsed = self.darkroom.right_closed;
            if edge_strip(ui, "dk-edge-right", Edge::Right, !collapsed) {
                self.darkroom.right_closed = !collapsed;
            }
        }
        // Side panels with fixed bounds so they never push the image aside:
        // sources on the left, Info/Edit on the right
        let column_w = darkroom_layout::column_width(ctx.content_rect().width());
        self.darkroom.column_rects = [None, None];
        let t_left = lr::slide(&ctx, "dk-left", darkroom && !self.darkroom.image_only && !self.darkroom.left_collapsed);
        if darkroom && t_left > 0.001 {
            let w = tool_rail::WIDTH + column_w;
            let r = egui::Panel::left("dk-links").resizable(false).exact_size(w * t_left).frame(egui::Frame::new().fill(lr::PANEL)).show(ui, |ui| {
                lr::slid(ui, egui::vec2(w, 0.0), true, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let h = ui.available_height();
                        let (rail, _) = ui.allocate_exact_size(egui::vec2(tool_rail::WIDTH, h), egui::Sense::hover());
                        ui.painter().rect_filled(rail, 0.0, lr::PANEL_DARK);
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rail).layout(egui::Layout::top_down(egui::Align::Min)), |ui| self.darkroom_left_rail(ui));
                        let col = egui::Rect::from_min_size(egui::pos2(rail.right() + 8.0, rail.top() + 6.0), egui::vec2(column_w - 16.0, h - 12.0));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(col).layout(egui::Layout::top_down(egui::Align::Min)), |ui| self.darkroom_left(ui));
                    });
                });
            }).response.rect;
            self.darkroom.column_rects[0] = Some(r);
        }
        let t_right = lr::slide(&ctx, "dk-right", darkroom && !self.darkroom.image_only && !self.darkroom.right_closed);
        if darkroom && t_right > 0.001 {
            let r = egui::Panel::right("dk-rechts").resizable(false).exact_size(column_w * t_right).show(ui, |ui| {
                lr::slid(ui, egui::vec2(column_w - 16.0, 0.0), false, |ui| self.darkroom_right(ui));
            }).response.rect;
            self.darkroom.column_rects[1] = Some(r);
        }
        self.darkroom.handle_rect = None;
        if darkroom && !self.darkroom.image_only {
            egui::Panel::top("dk-griff").show_separator_line(false).resizable(false).exact_size(darkroom_layout::HANDLE_H).show(ui, |ui| self.darkroom_handle(ui));
            let strip_h = self.darkroom_strip_animated(&ctx);
            if strip_h > 0.5 {
                egui::Panel::top("dk-filmlooks").resizable(false).exact_size(strip_h).show(ui, |ui| self.darkroom_look_strip(ui));
            }
        }
        let t_sources = lr::slide(&ctx, "sources", !darkroom && (self.show_left || self.peek[2]));
        if !darkroom && t_sources > 0.001 {
            let r = egui::Panel::left("quellen").resizable(false).exact_size(SOURCES_W * t_sources).show(ui, |ui| {
                lr::slid(ui, egui::vec2(SOURCES_W - 16.0, 0.0), true, |ui| { lr::bounded(ui, |ui| self.lib.sources(ui)); });
                if std::mem::take(&mut self.lib.export_requested) {
                    self.open_export();
                }
            }).response.rect;
            self.panel_rects[2] = Some(r);
        }
        let t_tools = lr::slide(&ctx, "tools", !darkroom && (self.show_right || self.peek[3]));
        if !darkroom && t_tools > 0.001 && t_tools < 0.999 {
            // while it glides only the frame moves; the panel content follows once it is in place
            egui::Panel::right("rechts-gleiten").resizable(false).exact_size((340.0 + tool_rail::WIDTH) * t_tools).frame(egui::Frame::new().fill(lr::PANEL)).show(ui, |_| {});
        } else if !darkroom && t_tools >= 0.999 {
            let mut actions = Vec::new();
            // the tool rail on the far right, the selected tool's panel to its left
            let tool_rail = egui::Panel::right("rechts-schiene").show_separator_line(false)
                .resizable(false)
                .exact_size(tool_rail::WIDTH)
                .frame(egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(0, 0)))
                .show(ui, |ui| self.tool_rail(ui))
                .response
                .rect;
            let r = egui::Panel::right("rechts").resizable(false).exact_size(340.0).show(ui, |ui| actions = lr::bounded(ui, |ui| self.right_panel(ui))).response.rect;
            self.panel_rects[3] = Some(r.union(tool_rail));
            self.library_actions(actions);
        }
        self.test_helpers(&ctx);
        self.multi_follow(&ctx);
        if self.pending_thumb.is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if let Some((index, s, w, h, rgb)) = self.pending_thumb.take() {
                // only if it is still the current result
                if self.rendered_for.as_ref().is_some_and(|(i, r)| *i == index && *r == s) {
                    self.thumb_from_render(&ctx, index, &s, w, h, &rgb);
                }
            }
        }
        if self.pending_wide.is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if let Some((index, s)) = self.pending_wide.take() {
                // only if it is still the current result of the image shown
                let job = self.current.as_ref().filter(|c| c.index == index).and_then(|c| jobs::wide_job(index, &c.loaded, &s));
                if let Some(job) = job.filter(|_| self.rendered_for.as_ref().is_some_and(|(i, r)| *i == index && *r == s)) {
                    self.worker.send(job);
                }
            }
        }
        // Temporarily shown panels: dwell on the strip opens, a short grace period outside strip and panel closes
        // (state machine and zone in edge_peek.rs; independent of the gliding size of the panel)
        {
            let now = ctx.input(|i| i.time);
            let down = ctx.input(|i| i.pointer.any_down());
            let pinned = [self.show_top, self.show_filmstrip, self.show_left, self.show_right];
            let extent = [self.panel_rects[0].map_or(90.0, |r| r.height()), self.strip_height, SOURCES_W, 340.0 + tool_rail::WIDTH];
            for k in 0..4 {
                let strip = ctx.data_mut(|d| d.remove_temp::<Option<egui::Rect>>(egui::Id::new(("edge-rect", k)))).flatten();
                let (Some(strip), true) = (strip, edges) else {
                    self.peeks[k] = Default::default();
                    self.peek[k] = false;
                    continue;
                };
                let zone = edge_peek::zone(strip, k, extent[k], self.panel_rects[k]);
                let on_strip = pointer.is_some_and(|p| strip.contains(p));
                let in_zone = pointer.is_some_and(|p| zone.contains(p));
                let open = self.peeks[k].step(edge_peek::Input { now, on_strip, in_zone, pinned: pinned[k], pointer_down: down });
                self.peek[k] = open;
                if self.peeks[k].pending() {
                    ctx.request_repaint_after(std::time::Duration::from_millis(40));
                }
            }
        }
        // also while the image is still loading (or cannot be developed): the bar stays in its place, so the image area keeps
        // its height and the picture does not jump when the RAW arrives
        let toolbar_image = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable);
        // the comparison has its tools in the same bar, in the library too (photo_compare_ui.rs)
        let comparing = self.compare_shown();
        if (comparing && !full) || (self.show_toolbar && !full && !raster && !(darkroom && self.darkroom.image_only) && toolbar_image.is_some()) {
            egui::Panel::bottom("toolbar").resizable(false).frame(egui::Frame::new().fill(lr::PANEL).inner_margin(egui::Margin::symmetric(10, 5))).show(ui, |ui| self.toolbar(ui));
        }
        // Sensor view (Bayer/X-Trans): operated in the "Sensor" tray of the tool rail, nothing above or below the image
        if raster {
            // hovered preset/film: the cell of the selected photo already shows the effect
            self.lib.hover_preview = self.current.as_ref().and_then(|c| {
                let (_, t) = self.hover_shown()?;
                Some((self.entries.get(c.index)?.image, t.clone()))
            });
            let mut actions = Vec::new();
            if self.lib.compare_open() && !self.in_people() {
                // the comparison instead of the grid (photo_compare_ui.rs)
                egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(lr::IMAGE_BG).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| self.compare_center(ui));
            } else {
                egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(lr::IMAGE_BG).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| actions = self.lib.grid_center(ui));
            }
            self.library_actions(actions);
            let a = self.lib.after(&ctx);
            self.library_actions(a);
        } else {
            let a = self.lib.after(&ctx);
            self.library_actions(a);
        }
        let bg = if full { Color32::BLACK } else { lr::IMAGE_BG };
        self.image_rect = None;
        let central = if raster {
            ctx.content_rect()
        } else if self.grid {
            egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(bg).inner_margin(8.0)).show(ui, |ui| self.grid_view(ui)).response.rect
        } else if self.compare_in_develop() && !full {
            // the comparison instead of the single image (photo_compare_ui.rs)
            egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(bg).inner_margin(egui::Margin::symmetric(8, 6))).show(ui, |ui| self.compare_center(ui)).response.rect
        } else {
            egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(bg)).show(ui, |ui| self.viewer(ui)).response.rect
        };
        // Lights out (L): dim or blacken everything except the image
        if self.lights > 0 {
            let keep = self.image_rect.map_or(central, |r| r.intersect(central));
            let screen = ctx.content_rect();
            let alpha = if self.lights == 1 { 205 } else { 255 };
            let shade = Color32::from_black_alpha(alpha);
            let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("lights-out"));
            let p = ctx.layer_painter(layer);
            for r in [
                egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, keep.min.y)),
                egui::Rect::from_min_max(egui::pos2(screen.min.x, keep.max.y), screen.max),
                egui::Rect::from_min_max(egui::pos2(screen.min.x, keep.min.y), egui::pos2(keep.min.x, keep.max.y)),
                egui::Rect::from_min_max(egui::pos2(keep.max.x, keep.min.y), egui::pos2(screen.max.x, keep.max.y)),
            ] {
                p.rect_filled(r, 0.0, shade);
            }
        }
        // pale way-back button in the corner while the top bar is hidden or the lights are dimmed
        self.way_back_button(&ctx);
        let sheet_area = self.darkroom.column_rects.iter().flatten().chain(self.darkroom.handle_rect.iter()).fold(central, |a, b| a.union(*b));
        self.darkroom_contact_draw(&ctx, sheet_area);
        self.darkroom_drawer(&ctx);
        self.darkroom_card(&ctx);
        self.darkroom_light_points(&ctx);
        self.darkroom_scan_update();
        self.darkroom_lut_dialog(&ctx);
        self.darkroom_recipes_window(&ctx);
        let copy_source = self.copy_from.and_then(|i| self.entries.get(i)).map(|e| &e.settings).or(self.settings());
        let copy_warning = copy_source.and_then(rawdev::film_profiles::missing);
        self.settings_group_frame(&ctx);
        self.photo_copy_frame(&ctx);
        if self.copy_dialog && paste::dialog(&ctx, &mut self.copy_dialog, &mut self.copy_groups, copy_warning.as_deref()) {
            let from = self.copy_from.or(self.current.as_ref().map(|c| c.index));
            history::copied_from(from.and_then(|i| self.entries.get(i)).map(|e| e.name.clone()));
            self.clipboard = self.copy_from.take().and_then(|i| self.entries.get(i)).map(|e| e.settings.clone()).or_else(|| self.settings().cloned());
            self.status = self.clipboard.as_ref().and_then(rawdev::film_profiles::missing).map_or_else(|| crate::tr("Einstellungen kopiert – ⌘⇧V fügt sie ein").into(), |w| crate::trf!("Kopiert – {w}", w = w));
        }
        if self.camf.open {
            let name = self.camf.index.and_then(|i| self.entries.get(i)).map(|e| e.name.clone()).unwrap_or_default();
            self.camf.show(&ctx, &name);
        }
        let mut open = self.show_shortcuts;
        shortcuts::window(&ctx, &mut open);
        self.show_shortcuts = open;
        if self.show_export {
            self.export_dialog(&ctx);
        }
        // "HDR zusammenfügen …": dialog, finished previews and results of the merge jobs (hdr_ui.rs)
        self.hdr_step(&ctx);
        // folder chosen for a waiting export, "overwrite?" (export_naming_ui.rs)
        self.export_naming_step(&ctx);
        if self.confirm_delete {
            self.delete_dialog(&ctx);
        } else {
            self.delete_info = None;
        }
        self.drop_rough_views_once();
        preview_repair::maybe_start(&self.lib);
        sigma_refresh::maybe_start(&self.lib);
        sigma_refresh::maybe_start_learned(&self.lib);
        camera_looks_ui::test_hook(|| self.lib.learn_candidates());
        camera_looks_ui::auto_learn();
        camera_looks_ui::show(&ctx);
        stack::test_helper();
        crash_log::test_panic(&ctx);
        bug_report::frame(&ctx);
        perf_report::frame(&ctx);
        if stack::reload_needed() {
            self.lib.discard_all_textures();
        }
        // the library is re-read in the background: then open the current photo afresh (new paths/states)
        if self.reset_after_reload && !self.lib.reloading() {
            self.reset_after_reload = false;
            self.current = None;
            self.cache.clear();
            self.sync_source(true);
        }

        self.track_history(&ctx);
        self.history_test_hook();
        let now = ui.input(|i| i.time);
        self.schedule(now);
        if self.spp_due.is_some() || self.prefetch_due.is_some() || self.browse.open_due.is_some() || self.browse.full_due.is_some() || !matches!(self.refine, Refine::None) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Test mode: move to the next image automatically every N seconds (GRAINEO_AUTOTEST=seconds)
        if let Some(secs) = crate::env::var("GRAINEO_AUTOTEST").ok().and_then(|v| v.parse::<f64>().ok()) {
            if let Some(c) = self.current.as_ref().map(|c| c.index) {
                if self.loading.is_none() && !self.rendering && now - self.view_changed_at > secs && c + 1 < self.entries.len() {
                    self.view_changed_at = now;
                    self.select(c + 1, false);
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let v = self.video_view.take();
        self.video.draw(_frame, &ctx, v);
    }

    /// ⌘Q, Dock and closing the window: save, wait for pending library writes (window and ⌘Q show the wait before,
    /// see [`quit::hold_close`]; from the Dock up to [`quit::WRITES`]), then end within [`quit::LIMIT`] (with pending
    /// writes [`quit::WRITES_LIMIT`]) even if something hangs (see `quit.rs`).
    fn on_exit(&mut self) {
        let t = std::time::Instant::now();
        log("quit: start");
        quit::start_guard();
        quit::test_hang();
        self.save_current();
        quit::ON_EXIT_WAITING.store(true, std::sync::atomic::Ordering::SeqCst);
        let written = quit::wait_until(quit::WRITES.saturating_sub(t.elapsed()), || self.lib.writes_finished() && !library::store::saving() && !subject_ui::storing());
        quit::ON_EXIT_WAITING.store(false, std::sync::atomic::Ordering::SeqCst);
        log(&format!("quit: saved after {} ms{}", t.elapsed().as_millis(), if written { "" } else { ", library writes still pending" }));
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        // Module selection on the right (takes precedence), tools on the left in the remaining space;
        // in a narrow window the left side is clipped instead of overlapping.
        let full = ui.available_rect_before_wrap().with_max_y(ui.available_rect_before_wrap().top() + 24.0);
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(full).layout(egui::Layout::right_to_left(egui::Align::Center)));
        {
            let ui = &mut right;
            // the switches for panels and lights at the very right, the module tabs to their left (fixed places)
            self.panel_buttons(ui);
            self.module_bar(ui);
        }
        let left_rect = full.with_max_x(right.min_rect().left() - 16.0);
        let mut left = ui.new_child(egui::UiBuilder::new().max_rect(left_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        left.set_clip_rect(left_rect.intersect(ui.clip_rect()));
        {
            let ui = &mut left;
            self.brand.header(ui);
            // one status for all work (running, done, failed), right next to the name (fixed width, empty when idle)
            self.activity_status(ui);
            // opened from the people view: the way back to it, at a fixed place (also Esc, G, double click)
            if !self.in_grid() && self.lib.people_pending() {
                ui.add_space(12.0);
                let b = ui.button(crate::tr("← Personen"));
                lr::tip_key(ui, &b, "Zurück zu Personen: dieselbe Person, dasselbe Gesicht", "grid_esc");
                if b.clicked() {
                    self.to_grid();
                }
            }
            // active colour proof or gamut warning: short note (no explanation, no gold; the ?-card is in the settings)
            if let Some(note) = proof::note() {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(note).color(lr::TEXT()));
            }
            // no file or folder name here: the filmstrip header and the info box already show it
        }
        ui.allocate_rect(full, egui::Sense::hover());
    }

    /// Left column: navigator and presets.
    fn left_panel_lists(&mut self, ui: &mut egui::Ui) {
        let mut apply: Option<Settings> = None;
        let mut delete: Option<usize> = None;
        let cur = self.settings().cloned();
        // ↑/↓ moves to the previous/next preset after one has been clicked
        let all: Vec<(String, presets::Preset)> = presets::presets_library()
            .into_iter()
            .flat_map(|c| c.presets.into_iter().map(move |p| (format!("{}/{}", c.name, p.name), p)))
            .chain(self.presets.iter().filter(|p| p.scope != presets::Scope::Film).map(|p| (format!("Eigene/{}", p.name), p.clone())))
            .collect();
        let typing = ui.ctx().egui_wants_keyboard_input();
        let mut moved = false;
        if let (Some(key), false, false) = (self.preset_cursor.clone(), typing, self.grid) {
            let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
            if up || down {
                // only as long as the image still carries exactly this preset (afterwards changed by hand)
                let still = |pos: usize| cur.as_ref().is_some_and(|c| all[pos].1.apply(c) == *c);
                if let Some(pos) = all.iter().position(|(k, _)| *k == key).filter(|&p| still(p)) {
                    let next = if down { (pos + 1).min(all.len() - 1) } else { pos.saturating_sub(1) };
                    if next != pos {
                        if let Some(c) = &cur {
                            apply = Some(all[next].1.apply(c));
                            if let Some(i) = self.current.as_ref().map(|c| c.index) {
                                history::hint(i, history::Hint::preset(&all[next].1.name));
                            }
                        }
                        self.preset_cursor = Some(all[next].0.clone());
                        moved = true;
                    }
                }
            }
        }
        let mut clicked_key: Option<String> = None;
        let sigma = self.current.as_ref().is_some_and(|c| c.loaded.previews.foreign.is_none());
        self.update_preset_previews(ui.ctx(), sigma);
        // all presets of the library in fixed order (index = Quelle::Mitgeliefert), without film looks
        let lib: Vec<(&'static str, presets::Preset)> = presets::presets_library().into_iter().flat_map(|c| { let n = c.name; c.presets.into_iter().map(move |p| (n, p)) }).collect();
        let mut delete_own: Option<usize> = None;
        // Hovering a preset: effect immediately in the large image or in the grid cell of the selected photo
        let mut hovered_recipe: Option<darkroom_recipes::Source> = None;
        // Preset the image currently carries (for highlighting in the grid)
        let active: Option<darkroom_recipes::Source> = cur.as_ref().and_then(|c| {
            let neutral = |p: &presets::Preset| p.scope == presets::Scope::Toning && p.settings.grade_shadows[1] == 0.0 && p.settings.grade_highlights[1] == 0.0 && p.settings.grade_midtones[1] == 0.0;
            lib.iter().position(|(_, p)| !neutral(p) && p.apply(c) == *c).map(darkroom_recipes::Source::BuiltIn)
                .or_else(|| self.presets.iter().position(|p| p.scope != presets::Scope::Film && p.apply(c) == *c).map(darkroom_recipes::Source::Own))
        });
        lr::section(ui, "Vorgaben", true, |ui| {
            let mut selected: Option<darkroom_recipes::Source> = None;
            let mut groups: Vec<&'static str> = Vec::new();
            for (n, _) in &lib {
                if !groups.contains(n) { groups.push(*n); }
            }
            for (gi, name) in groups.iter().enumerate() {
                let sigma_only = *name == "Foveon-Spezial";
                if sigma_only && !sigma { continue; }
                let count = lib.iter().filter(|(n, _)| n == name).count();
                lr::subsection(ui, name, count, true, |ui| {
                    if let Some((q, _)) = recipe_grid::raster(ui, &self.preset_images, &|e| e.group as usize == gi, active, 105.0, false, &mut hovered_recipe) {
                        selected = Some(q);
                    }
                });
            }
            let own_count = self.presets.iter().filter(|p| p.scope != presets::Scope::Film).count();
            lr::subsection(ui, "Eigene", own_count, true, |ui| {
                if own_count == 0 {
                    ui.label(egui::RichText::new(crate::tr("Noch keine – unten benennen und mit + speichern.")).small().color(lr::TEXT_DIM()));
                } else if let Some((q, _)) = recipe_grid::raster(ui, &self.preset_images, &|e| e.group == 99, active, 105.0, false, &mut hovered_recipe) {
                    selected = Some(q);
                }
                if let Some(darkroom_recipes::Source::Own(i)) = active {
                    if ui.button(crate::tr("VORGABE LÖSCHEN")).clicked() {
                        delete_own = Some(i);
                    }
                }
            });
            ui.horizontal(|ui| {
                let edit = crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut self.new_preset_name).hint_text(crate::tr("Neue Vorgabe …")).desired_width(ui.available_width() - 40.0));
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let can = !self.new_preset_name.trim().is_empty() && self.current.is_some();
                let plus = ui.add_enabled(can, egui::Button::new("+"));
                lr::hover_tip(ui, &plus, crate::tr("Aktuelle Einstellungen als Vorgabe speichern (↩)"));
                if plus.clicked() || (can && enter) {
                    if let Some(s) = self.settings().cloned() {
                        self.presets.push(presets::Preset {
                            name: self.new_preset_name.trim().to_string(),
                            settings: presets::own_preset_settings(Settings { view: View::Developed, ..s }),
                            scope: presets::Scope::Look,
                            hint: crate::tr("Eigene Vorgabe").into(),
                        });
                        presets::save(&self.presets);
                        self.new_preset_name.clear();
                    }
                }
            });
            // a click applies the preset
            if let (Some(q), Some(c)) = (selected, cur.as_ref()) {
                let (key, preset) = match q {
                    darkroom_recipes::Source::BuiltIn(i) => lib.get(i).map(|(n, p)| (format!("{}/{}", n, p.name), p.clone())),
                    darkroom_recipes::Source::Own(i) => self.presets.get(i).map(|p| (format!("Eigene/{}", p.name), p.clone())),
                }.unzip();
                if let (Some(key), Some(p)) = (key, preset) {
                    apply = Some(p.apply(c));
                    clicked_key = Some(key);
                    if let Some(i) = self.current.as_ref().map(|c| c.index) {
                        history::hint(i, history::Hint::preset(&p.name));
                    }
                }
            }
        });
        let hovered: Option<Settings> = match (hovered_recipe, cur.as_ref()) {
            (Some(darkroom_recipes::Source::BuiltIn(i)), Some(c)) => lib.get(i).map(|(_, p)| p.apply(c)),
            (Some(darkroom_recipes::Source::Own(i)), Some(c)) => self.presets.get(i).map(|p| p.apply(c)),
            // a step hovered in the list "Verlauf" (last frame): its state as the preview
            _ => self.history.take_hover(),
        };
        if let Some(i) = delete_own {
            delete = Some(i);
        }
        // Request a preview as soon as another preset is hovered (never from a fast preview too rough to be shown)
        let hovered = hovered.filter(|h| !self.derived_waits(h));
        match hovered {
            Some(h) => {
                if self.hover_preset.as_ref() != Some(&h) {
                    if let Some(c) = &self.current {
                        self.worker.send(Job::HoverRender { index: c.index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: h.clone(), supersede: true });
                    }
                    self.hover_preset = Some(h);
                }
            }
            None => {
                // If a film is being hovered (section "Film"), the preview belongs there
                if !self.film_hover {
                    self.hover_preset = None;
                }
            }
        }
        if let Some(k) = clicked_key {
            self.preset_cursor = Some(k);
        }
        if let Some(i) = delete {
            self.delete_own_preset(i);
        }
        if let (Some(s), Some(i)) = (apply, self.current.as_ref().map(|c| c.index)) {
            self.entries[i].settings = s;
            self.hover_preset = None;
        }
        self.history_section(ui);
    }

    /// Left part of the filmstrip header: source, count, delete rejected, selection.
    fn strip_header_left(&mut self, ui: &mut egui::Ui, library: bool, n: usize, sel: &[usize]) {
        // The "Nicht in der Bibliothek" overview has folders, not photos of the list
        // a series compared as a whole: the strip holds only its members, its header says so ("Reihe · 8 Bilder")
        let title = self.compare_shown().then(|| self.lib.compare_series_word()).flatten().map_or_else(|| self.lib.source_title(), str::to_string);
        let count = if let Some(t) = self.lib.not_in_catalog_strip() {
            t
        } else if n == 1 {
            crate::menu::i18n::fill_names(crate::tr("{a} · 1 Bild"), &[("a", &title)])
        } else {
            crate::menu::i18n::fill_names(crate::tr("{a} · {b} Bilder"), &[("a", &title), ("b", &n.to_string())])
        };
        ui.add(egui::Label::new(egui::RichText::new(count).small()).truncate());
        if let Some(t) = self.compare_strip_note() {
            ui.label(egui::RichText::new(t).small());
        } else if !library {
            let rejected = self.entries.iter().filter(|e| e.meta.flag == sidecar::Flag::Reject).count();
            if rejected > 0 && n < 5000 && ui.small_button(crate::trf!("{rejected} abgelehnte löschen …", rejected = rejected)).clicked() {
                self.confirm_delete = true;
            }
            if sel.len() >= 2 && ui.small_button(crate::tr("Kontaktabzug erstellen …")).clicked() {
                self.open_contact_print();
            }
            if sel.len() > 1 {
                ui.label(egui::RichText::new(crate::trf!("· {a} ausgewählt", a = sel.len())).small());
            }
        }
        // multiple selection: settings of the main photo onto the others (settings_group.rs), grid and Develop
        if sel.len() > 1 {
            let b = ui.small_button(crate::tr("Übertragen …"));
            lr::tip_key(ui, &b, "Einstellungen vom Haupt-Foto auf die Auswahl übertragen", "sync_settings");
            if b.clicked() {
                self.open_transfer_dialog();
            }
            if !library && ui.small_button(crate::tr("Auswahl aufheben")).clicked() {
                self.entries.iter_mut().for_each(|e| e.selected = false);
            }
        }
    }

    /// Filmstrip at the bottom: thumbnails with stars and flags, filter, selection, sync.
    /// Filmstrip: the visible list of the library (shared image source), the same in Library
    /// and Develop. Only the visible part is drawn; thumbnails come from the
    /// library's preview cache (edited images show their development).
    fn filmstrip(&mut self, ui: &mut egui::Ui) {
        let library = self.tab == Tab::Library;
        let n = self.entries.len();
        let sel = self.selection();
        let active = if library {
            // the active photo hidden in a collapsed stack: the stack's cell stands for it
            self.lib.active_image().map(|b| self.lib.tile_for(b)).and_then(|b| self.entries.iter().position(|e| e.image == b))
        } else {
            self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable)
        };
        // Header: one compact row of fixed height (source, count, selection, active photo; filter on the right)
        let header_w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(header_w, filmstrip_layout::HEADER_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_min_size(egui::vec2(header_w, filmstrip_layout::HEADER_H));
            ui.set_max_height(filmstrip_layout::HEADER_H);
            // Filmstrip filter (on the right, with a funnel – unlike the flag below the
            // image, which changes the photo itself); the same filter as in the library
            ui.spacing_mut().item_spacing.x = 6.0;
            let f = self.lib.flag_filter();
            let cells = [
                (lr::Cell::Flag(1), f & 1 != 0, "Filter: markierte zeigen"),
                (lr::Cell::Flag(0), f & 2 != 0, "Filter: unmarkierte zeigen"),
                (lr::Cell::Flag(-1), f & 4 != 0, "Filter: abgelehnte zeigen"),
            ];
            if let Some(k) = lr::cell_group_sized(ui, "streifen-filter", &cells, 26.0, filmstrip_layout::HEADER_H - 2.0).0 {
                self.lib.toggle_flag_filter(k);
            }
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, filmstrip_layout::HEADER_H), egui::Sense::hover());
            lr::paint_icon(ui.painter(), egui::Rect::from_center_size(r.center(), egui::vec2(13.0, 13.0)), lr::Icon::Funnel, if f != 0 { Color32::WHITE } else { lr::TEXT_DIM() });
            // expand / collapse all series (⇧S), the same button as above the grid (library_series.rs)
            if library_ui::series_all_button(ui, self.lib.series_all(), 26.0, filmstrip_layout::HEADER_H - 2.0) {
                self.lib.series_all_click();
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().button_padding.y = 1.0;
                self.strip_header_left(ui, library, n, &sel);
                if let Some(e) = active.and_then(|a| self.entries.get(a)) {
                    ui.add(egui::Label::new(egui::RichText::new(format!("/ {}", e.name)).small().color(lr::TEXT_DIM())).truncate());
                }
            });
        });
        ui.add_space(2.0);
        let (cmd, shift) = { let m = click_mods::at_press(ui.ctx()); (m.command, m.shift) };
        let mut clicked: Option<usize> = None;
        // Cells as wide as the photo, their height from the filmstrip height; below a certain height only the photos
        // (no stars row). Positions as prefix sums, rebuilt only when the list or the height changes.
        let cells_h = (ui.available_height() - filmstrip_layout::SCROLLBAR).max(1.0);
        let key = (self.source_version, n, cells_h.to_bits());
        if self.strip_layout.as_ref().is_none_or(|(k, _)| *k != key) {
            let layout = filmstrip_layout::Layout::new(self.entries.iter().map(|e| e.aspect), cells_h);
            self.strip_layout = Some((key, layout));
        }
        let Some((_, layout)) = self.strip_layout.take() else { return };
        let (thumb_h, meta_h) = (layout.thumb_h, layout.meta_h);
        // Mouse wheel over the filmstrip scrolls horizontally (trackpad swiping works anyway)
        let wheel = if ui.rect_contains_pointer(ui.available_rect_before_wrap()) {
            ui.input_mut(|i| {
                let d = i.smooth_scroll_delta;
                if d.y != 0.0 && d.x.abs() < d.y.abs() {
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    d.y
                } else {
                    0.0
                }
            })
        } else {
            0.0
        };
        let view_w = ui.available_width();
        let mut offset = self.strip_scroll;
        if wheel != 0.0 {
            offset = (offset - wheel).clamp(0.0, (layout.total() - view_w).max(0.0));
        }
        // Source changed (e.g. "All photos"): go to the start without scrolling, instead of jumping to the active
        // photo, which sits somewhere in the middle of the new list. Only when a photo is explicitly
        // to be shown (Show in folder) does it still jump to the photo.
        let mut jumped = false;
        if library && self.strip_source != self.lib.source_nr {
            self.strip_source = self.lib.source_nr;
            if !self.lib.open_to_active() {
                offset = 0.0;
                jumped = true;
                self.strip_active = active.map(|a| self.entries[a].image);
            }
        }
        // keep the active image in view (after switching), centred, never past the end of the strip; also after
        // a change of view (library ↔ Develop) or a new list, where the same photo can sit elsewhere
        let moved = filmstrip_layout::view_or_list_changed(ui.ctx(), library, self.source_version);
        // a stack opened or closed (double click, number, S, ⇧S, ↩, ⌥←/→): the stack stays where it stood in the
        // visible strip instead of the active photo being centred
        if moved && self.lib.take_strip_keep() {
            if let Some(a) = active {
                let image = self.entries[a].image;
                let key = self.lib.stack_key(image);
                let in_group = |b: i64| if key.is_some() { self.lib.stack_key(b) == key } else { b == image };
                let before = self.strip_seen.iter().filter(|(b, _)| in_group(*b)).map(|(_, x)| *x).reduce(f32::min);
                let now = self.entries.iter().position(|e| in_group(e.image));
                if let (Some(x), Some(i)) = (before, now) {
                    offset = filmstrip_layout::keep_at(&layout, i, x, view_w);
                    self.strip_active = Some(image);
                    jumped = true;
                }
            }
        }
        if let Some(a) = active {
            let image = self.entries[a].image;
            if self.strip_active != Some(image) || moved && !jumped {
                self.strip_active = Some(image);
                offset = layout.bring_into_view(a, offset, view_w);
                jumped = true;
            }
        }
        // comparing: after a new list (a series opened in the strip) the compared photos come into view together,
        // the middle one centred (QA: the 7th of seven was cut off at the edge)
        let images = if self.compare_shown() { self.lib.compare.as_ref().map(|c| c.images.clone()).unwrap_or_default() } else { Vec::new() };
        let compared_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (&images, self.source_version, n).hash(&mut h);
            h.finish()
        };
        if !images.is_empty() && compared_key != self.strip_compared {
            self.strip_compared = compared_key;
            let at: Vec<usize> = self.entries.iter().enumerate().filter(|(_, e)| images.contains(&e.image)).map(|(i, _)| i).collect();
            if let (Some(&a), Some(&b)) = (at.first(), at.last()) {
                let (left, right) = (layout.x(a), layout.x(b) + layout.width(b));
                if left < offset || right > offset + view_w {
                    // centre the span (when it is wider than the strip, its start stays in view)
                    let max = (layout.total() - view_w).max(0.0);
                    offset = ((left + right - view_w) / 2.0).min(left).clamp(0.0, max);
                    jumped = true;
                }
            }
        }
        // ⇧+arrow: the moving end of the selection stays in view
        if let Some(h) = self.strip_follow.take() {
            offset = layout.bring_into_view(h, offset, view_w);
            jumped = true;
        }
        // A jump goes exactly there: the scroll area keeps a coasting speed and the scroll range of the
        // last frame; a target past that old range (the list grew, e.g. from the library to Develop) was taken as
        // over-scrolling and sprang back slowly to the old end instead of to the photo. Fresh state, then the offset.
        if jumped {
            filmstrip_layout::forget_scroll_motion(ui.ctx());
        }
        let area = egui::ScrollArea::horizontal().id_salt(filmstrip_layout::AREA).auto_shrink([false; 2]).horizontal_scroll_offset(offset);
        let mut wanted: Vec<i64> = Vec::new();
        let mut context: Option<(usize, crate::library_ui::Menu)> = None;
        let mut quick_click: Option<usize> = None;
        let mut number_click: Option<i64> = None;
        let mut double: Option<usize> = None;
        let mut seen: Vec<(i64, f32)> = Vec::new();
        let mut number_hit = false;
        let mut menu_toggle: Option<i64> = None;
        // cells whose shape the first thumbnail corrects (unknown or rotated dimensions, crop): once per photo
        let mut checked: Vec<usize> = Vec::new();
        let mut learned: Vec<(usize, f32)> = Vec::new();
        let pad = filmstrip_layout::PAD;
        // with a multiple selection the unselected thumbnails recede a little (three steps: active, selected, receding)
        // while comparing in the library the strip shows the grid's selection (it is the comparison's source)
        let lib_sel: std::collections::HashSet<i64> = if library && self.compare_shown() { self.lib.target_images().into_iter().collect() } else { Default::default() };
        let multi = self.entries.iter().filter(|e| e.selected || lib_sel.contains(&e.image)).count() >= 2;
        // the strip keeps its own band below the cells for the bar (filmstrip_layout::SCROLLBAR), no extra one
        ui.spacing_mut().scroll.floating_allocated_width = 0.0;
        // position display: a very faint track shows the whole strip, the handle (a little more visible than in lists,
        // since the band is only a few points high) the visible part; thin, widens under the pointer, no gold
        {
            let s = &mut ui.spacing_mut().scroll;
            s.floating_width = 3.0;
            s.dormant_background_opacity = 0.07;
            s.active_background_opacity = 0.10;
            s.interact_background_opacity = 0.14;
            s.dormant_handle_opacity = 0.40;
            s.active_handle_opacity = 0.55;
            s.interact_handle_opacity = 0.70;
        }
        let strip = area.show_viewport(ui, |ui, vp| {
            let h = ui.available_height();
            ui.set_min_size(egui::vec2(layout.total(), h));
            let origin = ui.min_rect().min;
            let cell_h = thumb_h + 2.0 * pad + meta_h;
            for i in layout.visible(vp.min.x, vp.max.x) {
                let r = egui::Rect::from_min_size(origin + egui::vec2(layout.x(i), 0.0), egui::vec2(layout.width(i), cell_h));
                let resp = ui.interact(r, ui.scope_id().with(("strip", i)), egui::Sense::click_and_drag());
                let e = &self.entries[i];
                seen.push((e.image, layout.x(i) - vp.min.x));
                let is_active = active == Some(i);
                let picked = e.selected || lib_sel.contains(&e.image);
                // Selection as a ring with a gap to the photo, so it does not merge with the picture
                let fill = if resp.hovered() && !is_active && !picked { Color32::from_gray(0x3c) } else { Color32::TRANSPARENT };
                let recede = multi && !is_active && !picked && !resp.hovered();
                ui.painter().rect_filled(r, 0.0, fill);
                if is_active {
                    ui.painter().rect_stroke(r.expand(1.0), 0.0, egui::Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Inside);
                } else if picked {
                    ui.painter().rect_stroke(r, 0.0, egui::Stroke::new(1.0, Color32::from_gray(0xc6)), egui::StrokeKind::Inside);
                    if self.strip_ext.is_some_and(|(anchor, head)| active == Some(anchor) && head == i) {
                        ui.painter().line_segment([r.left_bottom() + egui::vec2(3.0, -2.0), r.right_bottom() + egui::vec2(-3.0, -2.0)], egui::Stroke::new(2.0, Color32::from_gray(0xb0)));
                    }
                }
                let img_area = egui::Rect::from_min_size(r.min + egui::vec2(pad, pad), egui::vec2(r.width() - 2.0 * pad, thumb_h));
                let tex = e.thumb.clone().or_else(|| self.lib.thumbnail(e.file));
                let e = &self.entries[i];
                match tex {
                    Some(t) => {
                        // fitted into the cell (a crop can differ from the library's dimensions)
                        // a photo just turned shows the turn at once, until its re-rendered thumbnail arrives
                        let quarters = self.lib.turned.quarters(e.image);
                        let sz = library_turn::turned_size(t.size_vec2(), quarters);
                        if !e.aspect_checked && sz.x > 0.0 && sz.y > 0.0 {
                            checked.push(i);
                            // small differences come from rounding of the thumbnail size
                            if (sz.x / sz.y / e.aspect - 1.0).abs() > 0.03 {
                                learned.push((i, sz.x / sz.y));
                            }
                        }
                        let k = (img_area.width() / sz.x).min(img_area.height() / sz.y);
                        let ir = egui::Rect::from_center_size(img_area.center(), sz * k);
                        // rejected photos dimmed (translucent), as in the library
                        let tint = if e.meta.flag == sidecar::Flag::Reject {
                            Color32::WHITE.gamma_multiply(if recede { 0.25 } else { 0.3 })
                        } else if e.state == 1 || e.state == 2 {
                            Color32::from_gray(if recede { 90 } else { 110 })
                        } else if recede {
                            Color32::from_gray(210)
                        } else {
                            Color32::WHITE
                        };
                        library_turn::paint_image(ui.painter(), t.id(), ir, quarters, tint);
                        // merged photo (HDR): the same mark as in the grid
                        if e.state as i64 == library::merge::STATUS_VIRTUAL && thumb_h >= 40.0 {
                            hdr_live::paint_mark(ui.painter(), ir);
                        }
                        if let Some(note) = library_ui::virtual_note(e.file, e.state as i64).filter(|_| resp.hovered()) {
                            lr::hover_tip(ui, &resp, &format!("{} · {note}", e.name));
                        }
                        // not loaded in this session: from the stored settings of the library (`edit_marks`)
                        let edited_film = if e.loaded { binning_ui::edited(&e.settings).then(|| e.settings.film.is_active()) } else { edit_marks::get(e.file, e.copy.as_deref()) };
                        let edited = thumb_h >= 64.0 && edited_film.is_some();
                        // the series number always sits at the same corner (3 pt from the cell edge); other marks in
                        // that corner (edited) stand above it
                        let strip_mark = if thumb_h >= 40.0 { self.lib.strip_mark(e.image) } else { None };
                        if edited {
                            let above = if strip_mark.is_some() { 21.0 } else { 0.0 };
                            lr::edit_badge_film(ui.painter(), ir.right_bottom() - egui::vec2(0.0, above), edited_film.unwrap_or(false));
                        }
                        // place in its series ("2/5"; collapsed "5"), as on the grid tile: in the comparison the members of
                        // the compared series stand open here, the number says which photo of the series it is
                        // From a cell height that leaves room for it the number is a button as on the grid tile (open /
                        // close; greyed with the reason while the comparison holds the series open); a click on it
                        // keeps the photo selected. Smaller cells only show it.
                        if thumb_h >= 40.0 {
                            if let Some(m) = strip_mark {
                                let button = thumb_h >= STRIP_NUMBER_BUTTON_H;
                                let corner = ir.right_bottom() - egui::vec2(3.0, 3.0);
                                let nr = library_ui::strip_number_rect(ui.painter(), corner, &m.text, button);
                                if button {
                                    // greyed: the click is taken all the same (it must not reach the cell and swap the
                                    // photo into a field of the comparison, QA 1.5.0-test.7)
                                    let nresp = ui.interact(nr, ui.scope_id().with(("strip-number", e.image)), egui::Sense::click());
                                    if nresp.clicked() || nresp.double_clicked() {
                                        number_hit = true;
                                    }
                                    library_ui::paint_strip_number(ui.painter(), nr, &m.text, m.usable && nresp.hovered(), !m.usable);
                                    lr::hover_tip(ui, &nresp, &m.tip);
                                    if m.usable && nresp.clicked() {
                                        number_click = Some(e.image);
                                    }
                                } else {
                                    library_ui::paint_strip_number(ui.painter(), nr, &m.text, false, false);
                                }
                            }
                        }
                        // quick collection: plaque at the top right, only when in it (hover offers an empty ring for adding);
                        // a click toggles it like the circle in the grid
                        if thumb_h >= 40.0 {
                            let inside = self.lib.in_quick(e.image);
                            let badge = ui.interact(lr::quick_badge_rect(ir), ui.scope_id().with(("strip-quick", e.image)), egui::Sense::click());
                            // a photo of a viewed folder: no ring to add (the quick collection cannot hold it), the reason on hover
                            let possible = self.lib.quick_possible(e.image);
                            if inside || (possible && (resp.hovered() || badge.hovered())) {
                                lr::quick_badge(ui.painter(), ir, inside, badge.hovered());
                            }
                            if possible {
                                lr::tip_key(ui, &badge, if inside { "Aus der Schnellsammlung entfernen" } else { "Zur Schnellsammlung hinzufügen" }, "quick");
                            } else {
                                lr::hover_tip(ui, &badge, crate::tr(library_ui::QUICK_REFUSED));
                            }
                            if badge.clicked() {
                                quick_click = Some(i);
                            }
                        }
                    }
                    None => {
                        wanted.push(e.image);
                        ui.painter().rect_filled(img_area, 0.0, Color32::from_gray(0x16));
                        ui.painter().text(img_area.center(), egui::Align2::CENTER_CENTER, &e.format, egui::FontId::proportional(10.0), Color32::from_gray(0x82));
                    }
                }
                // stars and flag only from a certain height on (the row grows in, see `filmstrip_layout::metrics`)
                if meta_h >= filmstrip_layout::META - 3.0 {
                    let y = img_area.bottom() + meta_h / 2.0 + 0.5;
                    let text = Color32::from_gray(0xdd);
                    // stars and flag in fixed zones; narrow cells "★5" (`cell_label.rs`)
                    let zone = r.width() - 2.0 * pad - 2.0 - crate::cell_label::FLAG_W - crate::cell_label::GAP;
                    if let Some(stars) = crate::cell_label::stars(e.meta.rating, zone, &crate::cell_label::measurer(ui.painter(), 10.0)) {
                        ui.painter().text(egui::pos2(r.left() + pad + 2.0, y), egui::Align2::LEFT_CENTER, stars, egui::FontId::proportional(10.0), if e.meta.rating > 0 || is_active { text } else { Color32::from_gray(0x78) });
                    }
                    // flag: filled tick / cross, nothing without a flag
                    let f = match e.meta.flag {
                        sidecar::Flag::Pick => 1,
                        sidecar::Flag::Reject => -1,
                        sidecar::Flag::None => 0,
                    };
                    if f != 0 {
                        lr::paint_flag(ui.painter(), egui::pos2(r.right() - pad - 7.0, y), 12.0, f, true, text);
                    }
                }
                // marks of the comparison (in it, reference, tournament)
                self.compare_strip_mark(ui.painter(), r, e.image);
                if resp.double_clicked() && !cmd && !shift {
                    double = Some(i);
                } else if resp.clicked() && !cmd && !shift {
                    self.strip_click_was = Some((e.image, is_active));
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
                let present = e.state == 0;
                let (image, file) = (e.image, e.file);
                let small = thumb_h < STRIP_NUMBER_BUTTON_H;
                resp.context_menu(|ui| {
                    // a cell too small for the number button: open / close its stack first in the menu
                    if small && self.lib.strip_toggle_menu(ui, image) {
                        menu_toggle = Some(image);
                        ui.close();
                    }
                    if let Some(m) = self.lib.photo_menu_for(ui, image, present) {
                        context = Some((i, m));
                    }
                });
                // drag onto a collection on the left
                if resp.drag_started() {
                    let sel = self.selection();
                    let images: Vec<i64> = if sel.len() > 1 && sel.contains(&i) { sel.iter().map(|&k| self.entries[k].image).collect() } else { vec![image] };
                    resp.dnd_set_drag_payload(crate::library_ui::PhotoDrag { images, file });
                }
            }
        });
        filmstrip_layout::remember_area(ui.ctx(), strip.id);
        self.strip_seen = seen;
        let mut scroll = strip.state.clamped_offset().x;
        for i in checked {
            self.entries[i].aspect_checked = true;
        }
        if learned.is_empty() {
            self.strip_layout = Some((key, layout));
        } else {
            // new cell widths: keep the active photo (or the first visible one) where it is, so the strip does not move
            let visible = layout.visible(scroll, scroll + view_w);
            let anchor = active.filter(|a| visible.contains(a)).unwrap_or(visible.start);
            for &(i, a) in &learned {
                self.entries[i].aspect = a;
            }
            let new = filmstrip_layout::Layout::new(self.entries.iter().map(|e| e.aspect), cells_h);
            scroll = (scroll + new.x(anchor) - layout.x(anchor)).clamp(0.0, (new.total() - view_w).max(0.0));
            self.strip_layout = Some((key, new));
            ui.ctx().request_repaint();
        }
        if let Some((i, m)) = context {
            self.photo_command(ui.ctx(), i, m);
        }
        self.strip_scroll = scroll;
        if let Some(i) = quick_click {
            // like the grid: on a selected photo the whole selection, otherwise only this photo
            let sel = self.selection();
            let target: Vec<i64> = if sel.len() > 1 && sel.contains(&i) { sel.iter().map(|&k| self.entries[k].image).collect() } else { vec![self.entries[i].image] };
            if let Some(t) = self.lib.quick_toggle(&target) {
                self.status = t;
            }
            clicked = None;
        }
        if number_hit {
            // a click on the number (also a greyed one) never reaches the cell
            clicked = None;
            double = None;
        }
        if let Some(image) = number_click.or(menu_toggle) {
            // the number is its own button: the photo selected stays (Develop: after closing, the cover opens)
            let actions = self.lib.strip_mark_click(image);
            self.library_actions(actions);
            self.strip_follow_lib = Some(false);
            clicked = None;
        }
        if !wanted.is_empty() && (!library || self.compare_shown()) {
            self.lib.request_thumbnails(&wanted);
        }
        if let Some(i) = double {
            // double click: a collapsed series opens; on the photo that was already
            // active its open series closes. The click before it has selected the photo already.
            let image = self.entries[i].image;
            let was_active = self.strip_click_was.is_some_and(|(b, a)| b == image && a);
            self.strip_click_was = None;
            if let Some(active) = self.lib.strip_double_click(image, was_active) {
                clicked = None;
                // Develop: the photo left in the strip (the cover after closing) is opened
                if !library && self.current.as_ref().is_none_or(|c| self.entries[c.index].image != active) {
                    if let Some(k) = self.entries.iter().position(|e| e.image == active) {
                        self.select(k, false);
                    }
                }
            }
        }
        if let Some(i) = clicked {
            if self.compare_strip_click(i, cmd, shift) {
                // the comparison took it (photo_compare_ui.rs)
            } else if library {
                self.lib.set_active(self.entries[i].image);
            } else {
                let vis = self.visible();
                let alt = ui.input(|i| i.modifiers.alt);
                self.click_entry(i, &vis, cmd, shift, alt);
            }
        }
    }

    /// Click on an image in filmstrip or grid: ⌘ = toggle selection,
    /// ⇧ = select range, otherwise open the image.
    fn click_entry(&mut self, i: usize, vis: &[usize], cmd: bool, shift: bool, alt: bool) {
        if cmd {
            let current = self.loading.or(self.current.as_ref().map(|c| c.index));
            let selected: Vec<usize> = vis.iter().copied().filter(|&v| self.entries[v].selected || Some(v) == current).collect();
            match cmd_click_on_active(i, current, &selected) {
                CmdClick::Toggle => {
                    self.entries[i].selected = !self.entries[i].selected;
                    if let Some(c) = current {
                        self.entries[c].selected = true;
                    }
                }
                // ⌘-click on the active photo of a multiple selection: it leaves the selection, the next selected one
                // becomes active (single views always keep an active photo)
                CmdClick::Next(n) => {
                    self.entries[i].selected = false;
                    self.entries[n].selected = true;
                    self.select(n, alt);
                }
                CmdClick::Nothing => {}
            }
        } else if shift {
            if let Some(c) = self.current.as_ref().map(|c| c.index) {
                let (a, b) = (c.min(i), c.max(i));
                for &v in vis {
                    if v >= a && v <= b {
                        self.entries[v].selected = true;
                    }
                }
            }
        } else {
            self.entries.iter_mut().for_each(|e| e.selected = false);
            self.select(i, alt);
        }
    }

    /// Grid view (G): cells with number, name, stars,
    /// flag; double click opens the loupe.
    fn grid_view(&mut self, ui: &mut egui::Ui) {
        let vis = self.visible();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::trf!("Raster · {a} von {b} Bildern", a = vis.len(), b = self.entries.len())).color(lr::TEXT_DIM()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().slider_width = 120.0;
                let r = ui.add(egui::Slider::new(&mut self.grid_size, 120.0..=360.0).show_value(false));
                lr::hover_tip(ui, &r, crate::tr("Vorschaugröße"));
                ui.label(egui::RichText::new(crate::tr("Miniaturen")).small().color(lr::TEXT_DIM()));
                if lr::list_item(ui, crate::tr("Lupe (E)"), false).clicked() {
                    self.grid = false;
                }
            });
        });
        ui.add_space(4.0);
        let cell = self.grid_size;
        let cols = ((ui.available_width() / cell).floor() as usize).max(1);
        self.grid_cols = cols;
        let cell = ui.available_width() / cols as f32;
        let rows = vis.len().div_ceil(cols);
        let (cmd, shift, alt) = { let m = click_mods::at_press(ui.ctx()); (m.command, m.shift, m.alt) };
        let current = self.current.as_ref().map(|c| c.index).or(self.loading);
        let mut clicked = None;
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, cell, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for col in 0..cols {
                        let Some(&i) = vis.get(row * cols + col) else { break };
                        let e = &self.entries[i];
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(cell, cell), egui::Sense::click());
                        let p = ui.painter();
                        let inner = rect.shrink(3.0);
                        let active = current == Some(i);
                        let fill = if active {
                            Color32::from_gray(0x4a)
                        } else if e.selected {
                            Color32::from_gray(0x34)
                        } else if resp.hovered() {
                            Color32::from_gray(0x26)
                        } else {
                            Color32::from_gray(0x1a)
                        };
                        p.rect_filled(inner, 0.0, fill);
                        // large, pale number at the top left
                        let n = vis.iter().position(|&v| v == i).unwrap_or(0) + 1;
                        p.text(inner.left_top() + egui::vec2(6.0, 2.0), egui::Align2::LEFT_TOP, n.to_string(), egui::FontId::proportional(cell * 0.16), Color32::from_gray(0x33));
                        let img_area = egui::Rect::from_min_max(inner.min + egui::vec2(10.0, 22.0), inner.max - egui::vec2(10.0, 30.0));
                        if let Some(t) = &e.thumb {
                            let s = t.size_vec2();
                            let k = (img_area.width() / s.x).min(img_area.height() / s.y);
                            let r = egui::Rect::from_center_size(img_area.center(), s * k);
                            p.rect_filled(r.expand(1.0), 0.0, Color32::from_gray(0x18));
                            p.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        // stars, name and flag in fixed zones, never overlapping (`cell_label.rs`)
                        let bottom = inner.left_bottom() + egui::vec2(8.0, -8.0);
                        let zone_right = inner.right() - 8.0 - crate::cell_label::FLAG_W - crate::cell_label::GAP;
                        let mut name_left = bottom.x;
                        if let Some(stars) = crate::cell_label::stars(e.meta.rating, zone_right - bottom.x, &crate::cell_label::measurer(&p, 11.0)) {
                            let g = p.text(bottom, egui::Align2::LEFT_BOTTOM, stars, egui::FontId::proportional(11.0), if e.meta.rating > 0 { Color32::from_gray(0xdd) } else { Color32::from_gray(0x74) });
                            name_left = g.right() + crate::cell_label::GAP;
                        }
                        // flag: filled tick / cross
                        let f = match e.meta.flag {
                            sidecar::Flag::Pick => 1,
                            sidecar::Flag::Reject => -1,
                            sidecar::Flag::None => 0,
                        };
                        if f != 0 {
                            lr::paint_flag(&p, inner.right_bottom() + egui::vec2(-15.0, -15.0), 15.0, f, true, Color32::from_gray(0xe8));
                        }
                        let edited_film = if e.loaded { binning_ui::edited(&e.settings).then(|| e.settings.film.is_active()) } else { edit_marks::get(e.file, e.copy.as_deref()) };
                        if let Some(film) = edited_film {
                            lr::edit_badge_film(&p, inner.right_top() + egui::vec2(-4.0, 22.0), film);
                        }
                        if resp.hovered() || active {
                            let measure = crate::cell_label::measurer(&p, 10.5);
                            let room = zone_right - name_left;
                            if room >= measure("a…b.JPG") {
                                let name = crate::cell_label::middle_with_extension(&e.name, room, &measure);
                                p.text(egui::pos2(name_left, bottom.y - 1.0), egui::Align2::LEFT_BOTTOM, name, egui::FontId::proportional(10.5), lr::TEXT_DIM());
                            }
                        }
                        if active && self.loading == Some(i) {
                            resp.scroll_to_me(None);
                        }
                        if resp.double_clicked() {
                            open = Some(i);
                        } else if resp.clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            }
        });
        if let Some(i) = clicked {
            self.click_entry(i, &vis, cmd, shift, alt);
        }
        if let Some(i) = open {
            if current != Some(i) {
                self.select(i, false);
            }
            self.grid = false;
        }
    }

    /// Placeholder from the last developed view of this image (see [`developed_cache`]).
    /// Last developed view of an image as a texture, with whether it is grey ([`browse::looks_grey`]).
    fn developed_placeholder(&mut self, ctx: &egui::Context, image: i64) -> Option<(egui::TextureHandle, bool)> {
        if let Some((b, t, grey)) = &self.developed_tex {
            if *b == image {
                return Some((t.clone(), *grey));
            }
        }
        let view = developed_cache::load(image)?;
        let grey = browse::looks_grey(view.as_raw(), 4);
        let t = ctx.load_texture(format!("entwickelt-{image}"), view, TextureOptions::LINEAR);
        self.developed_tex = Some((image, t.clone(), grey));
        Some((t, grey))
    }

    /// Zoom of the preview of a photo whose original is not reachable: a click switches between fitted and the
    /// preview's own resolution (one preview pixel per screen pixel, never more), dragging moves it, a double click
    /// goes back to the grid. Returns the image rectangle.
    #[allow(clippy::too_many_arguments)]
    fn offline_preview_zoom(&mut self, ui: &egui::Ui, resp: &egui::Response, avail: egui::Rect, fitted: egui::Rect, image: i64, s: egui::Vec2, fit: f32) -> egui::Rect {
        if self.offline_zoom.0 != image {
            self.offline_zoom = (image, 1.0, egui::Vec2::ZERO);
        }
        let ppp = ui.ctx().pixels_per_point();
        let z_max = (1.0 / (fit * ppp)).max(1.0);
        let generation = self.open_generation.load(Ordering::SeqCst);
        let free = self.image_click_free(ui.ctx());
        let (_, mut zoom, mut pan) = self.offline_zoom;
        match self.navigation.click.answer(resp, generation, free, avail) {
            Some(media_library::Geste::Raster) => {
                self.to_grid();
                return fitted;
            }
            Some(media_library::Geste::Zoom(p)) => {
                if zoom > 1.0 {
                    zoom = 1.0;
                    pan = egui::Vec2::ZERO;
                } else if z_max > 1.01 {
                    // the clicked point stays where it is
                    let uv = ((p - fitted.min) / fitted.size()).clamp(egui::Vec2::ZERO, egui::Vec2::splat(1.0));
                    let size = fitted.size() * z_max;
                    zoom = z_max;
                    pan = p - avail.center() + size * 0.5 - uv * size;
                }
            }
            None => {}
        }
        zoom = zoom.min(z_max);
        let size = s * fit * zoom;
        if zoom > 1.0 {
            if resp.dragged() {
                pan += resp.drag_delta();
            }
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            }
        }
        let limit = ((size - avail.size()) * 0.5).max(egui::Vec2::ZERO);
        pan = pan.clamp(-limit, limit);
        self.offline_zoom = (image, zoom, pan);
        egui::Rect::from_center_size(avail.center() + pan, size)
    }

    fn viewer(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(avail, egui::Sense::click_and_drag());
        let painter = ui.painter_at(avail);
        let full_uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let now = ui.input(|i| i.time);

        // Placeholder: thumbnail of the selected image until the preview is ready
        let shown = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable);
        let tex = match &self.current {
            Some(c) if self.loading.is_none() => {
                if self.show_camera_jpeg {
                    c.camera_jpeg.as_ref().or(c.texture.as_ref())
                } else if let Some((_, t)) = self.hover_shown() {
                    // preset hovered: effect directly in the large image
                    Some(t)
                } else {
                    // no render yet because the fast preview is withheld (`preview_hold.rs`): the last finished
                    // picture of this photo (opened again with another size), else the camera's embedded picture
                    // where nothing else can stand in
                    c.texture
                        .as_ref()
                        .or_else(|| self.kept_view.as_ref().filter(|(p, _)| *p == c.loaded.path).map(|(_, t)| t))
                        .or_else(|| c.camera_jpeg.as_ref().filter(|_| self.hold_jpeg == Some(c.index)))
                }
            }
            _ => None,
        };
        hover_test_log(self.current.as_ref().filter(|_| self.loading.is_none()).map(|c| match tex {
            Some(t) if c.texture.as_ref().is_some_and(|ct| ct.id() == t.id()) => "current".to_string(),
            Some(_) => self.hover_tex.as_ref().map_or("other".to_string(), |(s, _)| if s.film.is_active() { format!("hover {}", s.film.stock) } else { format!("hover look {}", s.raw.look) }),
            None => "none".to_string(),
        }));
        let Some(tex) = tex.cloned() else {
            // Original on an unreachable location: the preview can be zoomed up to its own resolution
            let offline = shown.filter(|_| self.loading.is_none()).and_then(|i| self.entries.get(i)).filter(|e| e.state == 2).map(|e| e.image);
            // A file that is still loading or unreachable can also be closed again.
            if offline.is_none() && shown.is_some() && resp.double_clicked_by(egui::PointerButton::Primary) && self.image_click_free(ui.ctx()) {
                self.to_grid();
                return;
            }
            if offline.is_none() {
                self.navigation.click.cancel();
            }
            let fallback_kind = shown.and_then(|i| {
                let (thumb, image) = (self.entries.get(i)?.thumb.clone(), self.entries[i].image);
                let e = &self.entries[i];
                // opened again with another size (binning): the last finished picture stays while the photo loads
                if let Some((_, t)) = self.kept_view.as_ref().filter(|(p, _)| *p == e.path) {
                    return Some((t.clone(), KEPT_VIEW));
                }
                let (path, copy, stored, settings) = (e.path.clone(), e.copy.clone(), e.stored, e.settings.clone());
                let expected = self.browse.expected(i, &settings, stored, &path, copy.as_deref());
                // the large preview (offline copy) first: it shows the development, sharp up to its level; virtual
                // copies share the file preview with the original, for them the last developed view of the copy itself
                let large = if copy.is_none() { self.lib.large_preview(image).filter(|(_, l)| *l).map(|(t, _)| t) } else { None };
                let large_fresh = large.is_some() && browse::large(&path, library::preview::large()) == Some(expected);
                if let Some(t) = large.clone().filter(|_| large_fresh) {
                    // it shows exactly the current settings: it stays on screen when the image has loaded
                    if self.loading == Some(i) && self.browse.hold.is_none() && image_kind::of(&path) != image_kind::ImageKind::Foveon {
                        self.browse.hold = Some((i, t.clone(), expected));
                    }
                    return Some((t, "large (current)"));
                }
                // An outdated view stands in only when it is of the same kind as the development that follows: a photo
                // switched to black & white elsewhere (several at once, a preset, pasted settings) must not flash up in
                // its old colours (nor the other way round)
                let loading = self.loading == Some(i);
                let wanted_grey = browse::grey_expected(&settings);
                let developed_fresh = browse::developed(image) == Some(expected);
                // while an image is loading: the view graineo last developed of it – it looks like the image that
                // follows (same development and crop); before an outdated large preview
                if loading && (large.is_none() || developed_fresh) {
                    if crate::env::var_os("GRAINEO_LOG").is_some() && self.browse.expected_logged != Some(i) {
                        self.browse.expected_logged = Some(i);
                        log(&format!("fingerprint {i}: large texture {}, expected {expected:016x}, large {:?}, developed {:?}, stored {stored}, grey wanted {wanted_grey}", large.is_some(), browse::large(&path, library::preview::large()).map(|v| format!("{v:016x}")), browse::developed(image).map(|v| format!("{v:016x}"))));
                    }
                    if let Some((t, grey)) = self.developed_placeholder(ui.ctx(), image) {
                        if browse::may_stand_in(developed_fresh, grey, wanted_grey) {
                            return Some((t, "developed"));
                        }
                    }
                }
                // an outdated large preview (developed again only a while after an edit) may show the other kind: while
                // the photo loads, the grid's preview stands in instead (it follows every edit at once)
                if let Some(t) = large.filter(|_| !loading) {
                    if crate::env::var_os("GRAINEO_LOG").is_some() && self.browse.expected_logged != Some(i) {
                        self.browse.expected_logged = Some(i);
                        log(&format!("fingerprint {i}: expected {expected:016x}, large {:?}, developed {:?}, stored {stored}", browse::large(&path, library::preview::large()).map(|v| format!("{v:016x}")), browse::developed(image).map(|v| format!("{v:016x}"))));
                    }
                    return Some((t, "large"));
                }
                if let Some(t) = thumb {
                    return Some((t, "thumbnail"));
                }
                if let Some((t, _)) = self.developed_placeholder(ui.ctx(), image).filter(|&(_, grey)| browse::may_stand_in(developed_fresh, grey, wanted_grey)) {
                    return Some((t, "developed"));
                }
                let small = if loading { self.lib.small_view(image) } else { self.lib.large_view(image) };
                small.map(|t| (t, "small"))
            });
            if let (Some(i), Some((t, kind))) = (shown, fallback_kind.as_ref()) {
                let since = self.selected_at.filter(|(s, _)| *s == i).map(|(_, t)| now - t);
                self.browse.log_view(i, kind, t.id(), since);
            }
            let kept = fallback_kind.as_ref().is_some_and(|(_, k)| *k == KEPT_VIEW);
            let fallback = fallback_kind.map(|(t, _)| t);
            // the fast preview is withheld and nothing can stand in (opened from a folder, no stored preview): ask
            // for the camera's embedded picture once; it is drawn like the photo (orientation, crop)
            if fallback.is_none() && self.loading.is_none() {
                if let Some(c) = self.current.as_ref().filter(|c| self.hold_jpeg != Some(c.index) && self.holds_fast_preview(c, &self.entries[c.index].settings)) {
                    let index = c.index;
                    self.hold_jpeg = Some(index);
                    if c.camera_jpeg.is_none() {
                        self.worker.send(Job::CameraJpeg { index, path: self.entries[index].path.clone() });
                    }
                }
            }
            // Video: Apple's player as a layer above the image area, our own control bar below it
            if let Some(e) = self.not_developable.filter(|_| self.loading.is_none()).and_then(|i| self.entries.get(i)).filter(|e| e.format == "VIDEO" && e.state == 0) {
                let path = e.path.clone();
                let image = egui::Rect::from_min_max(avail.min, egui::pos2(avail.max.x, avail.max.y - video_ui::BAR));
                let bar = egui::Rect::from_min_max(egui::pos2(avail.min.x, image.max.y), avail.max);
                if let Some(t) = fallback.as_ref() {
                    let s = t.size_vec2();
                    let fit = (image.width() / s.x).min(image.height() / s.y);
                    painter.image(t.id(), egui::Rect::from_center_size(image.center(), s * fit), full_uv, Color32::from_gray(200));
                }
                let free = !prefs::opened() && !ui.ctx().memory(|m| m.top_modal_layer().is_some()) && !ui.ctx().any_popup_open() && !egui::Popup::is_any_open(ui.ctx()) && !help::card_open(ui.ctx());
                if free {
                    self.video_view = Some(video_ui::View { rect: image, path: path.clone() });
                }
                if let Some(h) = self.video.hint() {
                    badge(&painter, image, &h, None);
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                } else if !self.video.ready_for(&path) {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
                self.video.bar(ui, bar);
                return;
            }
            let now_t = ui.input(|i| i.time);
            let text = match self.not_developable.filter(|_| self.loading.is_none()).and_then(|i| self.entries.get(i)) {
                Some(_) if self.damaged.is_some() && self.damaged == self.not_developable => crate::tr("Datei beschädigt – Vorschau aus der Kamera").to_string(),
                Some(e) if e.state != 0 && self.lib.work_running() => crate::tr("Bibliothek wird gerade abgeglichen …").to_string(),
                Some(e) if e.state as i64 == library::merge::STATUS_VIRTUAL => photo_state::merged_open_error(&e.path).unwrap_or_else(|| state_message(e.state)),
                Some(e) if e.state != 0 => state_message(e.state),
                Some(e) => photo_state::unsupported_text(&e.path).unwrap_or_else(|| crate::trf!("{a} – lässt sich hier nicht entwickeln", a = e.format)),
                None => crate::tr("Wird geladen …").to_string(),
            };
            if offline.is_some() && fallback.is_none() {
                // no preview at all: no zoom, a double click still goes back
                if resp.double_clicked_by(egui::PointerButton::Primary) && self.image_click_free(ui.ctx()) {
                    self.to_grid();
                    return;
                }
                self.navigation.click.cancel();
            }
            if self.info_level > 0 && shown.is_some() && !self.darkroom_covers_image() {
                // the info box is there right away, also while the photo is still loading
                self.info_overlay(&painter, avail);
            }
            match fallback {
                Some(thumb) => {
                    let s = thumb.size_vec2();
                    let fit = (avail.width() / s.x).min(avail.height() / s.y);
                    let mut rect = egui::Rect::from_center_size(avail.center(), s * fit);
                    if let Some(image) = offline {
                        rect = self.offline_preview_zoom(ui, &resp, avail, rect, image, s, fit);
                        if self.tab == Tab::Library {
                            return;
                        }
                    }
                    // a stand-in is dimmed; the photo's own last picture keeps its brightness (no flicker)
                    painter.image(thumb.id(), rect, full_uv, if kept { Color32::WHITE } else { Color32::from_gray(200) });
                    let loading_only = self.not_developable.filter(|_| self.loading.is_none()).is_none();
                    if loading_only {
                        // still loading: the same quiet spinner as for full resolution, no words
                        quiet_spinner(&mut self.quiet_spinners[0], ui.ctx(), &painter, rect.intersect(avail), true, None, now_t);
                        if self.quiet_spinners[0].0 > 0.0 {
                            work_step::spinner_tip(ui, rect.intersect(avail));
                        }
                    } else {
                        // fixed place (bottom right of the view), also while zoomed
                        badge(&painter, if offline.is_some() { avail } else { rect.intersect(avail) }, &text, None);
                    }
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
                None if shown.is_some() => {
                    quiet_spinner(&mut self.quiet_spinners[0], ui.ctx(), &painter, avail, true, None, now_t);
                    if self.quiet_spinners[0].0 > 0.0 {
                        work_step::spinner_tip(ui, avail);
                    }
                }
                None => {
                    painter.text(avail.center(), egui::Align2::CENTER_CENTER, crate::tr("Kein Bild geöffnet"), egui::FontId::proportional(16.0), Color32::GRAY);
                }
            }
            return;
        };
        let click_free = self.image_click_free(ui.ctx());
        let generation = self.open_generation.load(Ordering::SeqCst);
        // the preview with preserved gamut or in HDR of exactly the render shown (photo texture), else the 8-bit preview
        let wide_id = self.current.as_ref().filter(|c| c.texture.as_ref().is_some_and(|t| t.id() == tex.id())).and_then(|c| {
            let rendered = self.rendered_for.as_ref().filter(|(i, _)| *i == c.index).map(|(_, s)| s);
            self.display.preview_id(c.index, rendered)
        });
        if wide_id.is_some_and(|(_, hdr)| hdr) {
            photo_display::hdr_drawn();
        }
        let tex_id = wide_id.map_or(tex.id(), |(id, _)| id);
        // Faces of the image (briefly cached so that changes in "People" become visible soon)
        if let Some((face, name)) = face_frames::fetch() {
            self.lib.name_face(face, &name);
            self.faces_cache = None;
        }
        if let Some(face) = face_frames::fetch_discarded() {
            self.lib.discard_manual_face(face);
            self.faces_cache = None;
        }
        if let Some((face_rect, anchor)) = face_frames::fetch_frames() {
            let image = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image);
            if let Some(image) = image {
                // create in the background; the name field appears as soon as the face is registered
                self.lib.manual_face(image, face_rect, move |id| face_frames::new_name_field(id, anchor));
            }
        }
        if self.show_faces {
            let image = self.current.as_ref().and_then(|c| self.entries.get(c.index)).map(|e| e.image);
            if let Some(image) = image {
                // Not searched yet (the background run has not got this far): search this photo now and suggest names
                if let Some((b, res)) = &self.faces_search {
                    let done = res.lock().unwrap().take();
                    if let Some(r) = done {
                        let b = *b;
                        self.faces_search = None;
                        self.faces_cache = None;
                        if let Err(e) = r {
                            log(&format!("faces in photo {b}: {e}"));
                            // a condition of the photo, not a message
                            if let Some(en) = self.entries.iter().find(|en| en.image == b) {
                                photo_state::set(&en.path, photo_state::faces(&e));
                            }
                        }
                    }
                }
                if self.faces_search.is_none() && !self.faces_tried.contains(&image) && self.lib.faces_version(image) == 0 {
                    self.faces_tried.insert(image);
                    let res = std::sync::Arc::new(std::sync::Mutex::new(None));
                    self.faces_search = Some((image, res.clone()));
                    let c = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let r = match library::db::Db::standard() {
                            Ok(db) => library::people::check_image(&db, image),
                            Err(e) => Err(e.to_string()),
                        };
                        *res.lock().unwrap() = Some(r);
                        c.request_repaint();
                    });
                }
                if self.faces_search.as_ref().is_some_and(|(b, _)| *b == image) {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                }
                if self.faces_cache.as_ref().is_none_or(|(b, t, _)| *b != image || now - *t > 3.0) {
                    self.faces_cache = Some((image, now, self.lib.faces_of(image)));
                    self.faces_names = self.lib.person_names();
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
            }
        }
        // Comparisons: "Unedited | Current" (Y), the left side alone (backslash) and the look comparison (compare_view.rs)
        if self.compare_view(ui, avail, &resp, &painter, generation, click_free) {
            return;
        }
        // fade over: release the old texture
        if let Some(c) = self.current.as_mut().filter(|c| c.fade.as_ref().is_some_and(|f| f.alpha(now).is_none())) {
            c.fade = None;
        }
        let c = self.current.as_ref().unwrap();
        // Crop (crop/straighten); in crop mode the whole image
        let [fw_px, fh_px] = c.loaded.full_size;
        let cam = c.loaded.cal.orientation;
        let frame = if self.crop_mode.is_some() {
            Frame::full_settings(&self.entries[c.index].settings, fw_px, fh_px, cam)
        } else {
            // a hovered preset or history step shows its own crop
            Frame::from_settings(self.hover_shown().map_or(&self.entries[c.index].settings, |(h, _)| h), fw_px, fh_px, cam)
        };
        let img_size = egui::vec2(frame.w, frame.h);
        // Darkroom: border and "On paper" need space around the photo
        let mut sheet = if self.darkroom_active() { self.darkroom_sheet(c.index) } else { None };
        let total = sheet.as_ref().map_or(img_size, |b| b.total(img_size));
        // crop mode: a margin around the photo, so the grips at its corners and edges never sit at the window edge
        let pad = if self.crop_mode.is_some() { 2.0 * CROP_MARGIN } else { 0.0 };
        let fit = ((avail.width() - pad).max(1.0) / total.x).min((avail.height() - pad).max(1.0) / total.y);

        // Zoom with mouse wheel / trackpad, pan by dragging, single click = fit or 100 %
        let full_w = fw_px as f32;
        let one_to_one = frame.w / (img_size.x * fit) / ui.ctx().pixels_per_point();
        self.one_to_one = one_to_one;
        self.fit_size = img_size * fit;
        self.view_size = avail.size();
        let zoom_frame = (generation, self.fit_size, avail.size(), false);
        if let Some((z,p)) = self.navigation.zoom.step(now, zoom_frame, zoom::direct_gesture(ui.ctx(), click_free)) {
            self.zoom = z; self.pan = p; self.view_changed_at = now;
            zoom::log_lines(ui.ctx(), now, (z,p));
        }
        if let Some((z, p)) = zoom_test_step(self.zoom, self.pan, one_to_one) {
            (self.zoom, self.pan) = (z, p);
            self.view_changed_at = now;
        }
        if crate::env::var_os("GRAINEO_ZOOM_TEST").is_some() {
            // every frame (like a real gesture), so that zoom test ramps run smoothly and frame times can be measured
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.crop_mode.is_some() {
            // Crop always fit
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        } else if resp.hovered() && self.loupe && self.zoom <= 1.05 && !self.show_camera_jpeg {
            // Loupe: scrolling/pinching switches 100 → 200 → 400 % (in steps)
            let (dy, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            self.loupe_scroll += dy + (pinch - 1.0) * 400.0;
            if now - self.loupe_hint_at < 0.3 {
                // after a step wait briefly so that one swipe does not jump two steps
                self.loupe_scroll = 0.0;
            }
            let step = if self.loupe_scroll > 40.0 {
                2.0
            } else if self.loupe_scroll < -40.0 {
                0.5
            } else {
                1.0
            };
            if step != 1.0 {
                self.loupe_scroll = 0.0;
                let s = (self.loupe_scale * step).clamp(1.0, 4.0);
                if s != self.loupe_scale {
                    self.loupe_scale = s;
                    self.loupe_hint_at = now;
                }
            }
        } else if resp.hovered() {
            // Mac convention: pinching or ⌘ + scrolling (egui turns it into zoom_delta) and
            // ⌥ + scrolling zoom; two-finger swipe / mouse wheel pans the zoomed image
            let (pinch, swipe, alt) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
            if let Some(t) = zoom::scroll_source(ui.ctx()) {
                self.scroll_trackpad = t;
            }
            let mut factor = pinch;
            if matches!(self.tab, Tab::Develop | Tab::Sensor) {
                // Develop and sensor view (with and without darkroom): mouse wheel zooms; trackpad swipe pans once zoomed in
                // (fitted it does nothing); pinch and ⌘ + scrolling zoom around the fingers; pan by dragging
                let (f, p) = zoom::develop_gesture(pinch, swipe, self.scroll_trackpad, alt, self.zoom > 1.01);
                factor = f;
                if p != egui::Vec2::ZERO {
                    self.pan += p;
                    self.view_changed_at = now;
                }
            } else if alt {
                factor *= 1.0 + (swipe.y + swipe.x) * 0.002;
            } else if self.zoom > 1.01 && swipe != egui::Vec2::ZERO {
                self.pan += swipe;
                self.view_changed_at = now;
            }
            if factor != 1.0 {
                let old = self.zoom;
                self.zoom = (self.zoom * factor).clamp(1.0, one_to_one.max(1.0) * zoom::maximum_factor(self.tab == Tab::Sensor, self.darkroom.active));
                if let Some(m) = resp.hover_pos() {
                    let rel = m - (avail.center() + self.pan);
                    self.pan += rel - rel * (self.zoom / old);
                }
                self.view_changed_at = now;
            }
        }
        // Tools get all image gestures first. Navigation separates single and double click.
        // Darkroom comparison: a drag on the divider moves the line, not the image
        self.darkroom.compare.check_drag(&resp, avail);
        // A zoomed image can be panned: open hand on hover, closed hand while dragging
        if self.zoom > 1.01 && click_free && self.crop_mode.is_none() && !self.darkroom.compare.dragging {
            if resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            } else if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        if resp.dragged() && click_free && !self.darkroom.compare.dragging {
            self.pan += resp.drag_delta();
            self.view_changed_at = now;
        }
        let hit = egui::Rect::from_center_size(avail.center() + self.pan, img_size * fit * self.zoom).intersect(avail);
        match self.navigation.click.answer(&resp, generation, click_free, hit) {
            Some(media_library::Geste::Raster) => { self.to_grid(); return; }
            Some(media_library::Geste::Zoom(m)) => {
                let target = if self.zoom > 1.01 { (1.0,egui::Vec2::ZERO) } else {
                    let z=one_to_one.max(1.0);
                    // a click on the camera's focus area: 100 % centred on it
                    match focus_frame::hit(m, now).filter(|_| self.show_focus) {
                        Some(c) => (z, -(c-avail.center())*z),
                        None => { let rel=m-avail.center(); (z,rel-rel*z) }
                    }
                };
                (self.zoom,self.pan)=self.navigation.zoom.start(now,(self.zoom,self.pan),target,zoom_frame,zoom::reduced_motion());
                ui.ctx().request_repaint(); self.view_changed_at=now;
            }
            None => {}
        }
        // fitted stays fitted: a gesture that ended just above "fit" goes back to it
        if zoom::settle_fit(&mut self.zoom, &mut self.pan, zoom::view_busy(ui.ctx(), &self.navigation.zoom)) {
            self.view_changed_at = now;
        }
        let size = img_size * fit * self.zoom;
        // Clamp panning: no empty border next to the image; if an axis fits entirely
        // in the window, it is centred (half a point of rounding counts as fitting).
        let excess = (size - avail.size()) * 0.5;
        self.pan.x = if excess.x > 0.5 { self.pan.x.clamp(-excess.x, excess.x) } else { 0.0 };
        self.pan.y = if excess.y > 0.5 { self.pan.y.clamp(-excess.y, excess.y) } else { 0.0 };
        let offset = sheet.as_ref().map_or(egui::Vec2::ZERO, |b| b.offset(img_size, fit * self.zoom));
        let rect = egui::Rect::from_center_size(avail.center() + self.pan + offset, size);
        let full_src = [0.0, 0.0, fw_px as f32, fh_px as f32];
        let clipped = painter.with_clip_rect(rect.intersect(avail));
        let dim = self.crop_mode.is_some();
        if let Some(b) = &mut sheet {
            // the image info box keeps its place free of perforation and edge print
            b.free_zone = self.info_zone(&painter, avail);
            b.background(&painter, avail, rect);
        }
        let tint = sheet.as_ref().map_or(Color32::WHITE, |b| b.tint());
        let image_tint = if dim { Color32::from_gray(85) } else { tint };
        // Stored preview with matching fingerprint: stays on screen until a slider moves or the view is zoomed (the
        // interactive image is ready underneath, but switching to it would only make the picture jump)
        let holding = match self.browse.hold.clone().filter(|(i, _, _)| *i == c.index) {
            Some((_, t, fp)) => {
                let e = &self.entries[c.index];
                let expected_now = self.browse.expected(c.index, &e.settings, e.stored, &e.path, e.copy.as_deref());
                let ts = t.size_vec2();
                let same_shape = ((ts.x / ts.y) / (rect.width() / rect.height()) - 1.0).abs() < 0.015;
                let plain = self.zoom <= 1.01 && self.crop_mode.is_none() && !self.compare && !self.show_before && !self.show_camera_jpeg && !self.darkroom.active && self.hover_tex.is_none() && sheet.is_none() && e.settings.view == View::Developed && !self.show_clipping;
                if fp == expected_now && same_shape && plain {
                    Some(t)
                } else {
                    log(&format!("hold {} released ({})", c.index, if fp != expected_now { "settings" } else if !same_shape { "shape" } else { "view" }));
                    None
                }
            }
            None => None,
        };
        if holding.is_none() && self.browse.hold.as_ref().is_some_and(|(i, _, _)| *i == c.index) {
            self.browse.hold = None;
        }
        // merged photo: a single exposure held down in the tool "Zusammenfügen" is shown instead (hdr_paint.rs)
        let peek = match (self.merge_info(), c.loaded.previews.foreign.as_ref()) {
            (Some(mi), Some(doc)) => hdr_paint::peek_texture(ui.ctx(), doc, &mi, &self.entries[c.index].settings),
            _ => None,
        };
        let holding = holding.filter(|_| peek.is_none());
        match (holding.as_ref(), self.crop_mode.map(|_| Frame::from_settings(&self.entries[c.index].settings, fw_px, fh_px, c.loaded.cal.orientation).angle)) {
            (Some(t), _) => {
                clipped.image(t.id(), rect, full_uv, image_tint);
            }
            (None, Some(angle)) if angle.abs() > 0.001 => paint_src_quad_rotated(&painter.with_clip_rect(avail), tex_id, full_src, &frame, rect, image_tint, &ViewTransform::new(rect.center(), angle)),
            _ => {
                // crossfade fast preview -> exact image: the old picture below, the new one over it with rising alpha;
                // only in the plain fitted view of the current photo (not hover, camera JPEG or zoom)
                let fading = c.fade.as_ref().filter(|_| self.hover_tex.is_none() && !self.show_camera_jpeg && !self.show_before && !self.compare);
                match fading.and_then(|f| f.alpha(now).map(|a| (f, a))) {
                    Some((f, a)) => {
                        paint_src_quad(&clipped, f.old.id(), full_src, &frame, rect, image_tint);
                        paint_src_quad(&clipped, tex_id, full_src, &frame, rect, image_tint.gamma_multiply(a));
                        ui.ctx().request_repaint();
                    }
                    None => paint_src_quad(&clipped, peek.unwrap_or(tex_id), full_src, &frame, rect, image_tint),
                }
            }
        }
        if self.darkroom.active && self.tab == Tab::Develop {
            self.darkroom.compare.draw(&painter, avail, rect, full_src, &frame, &self.entries[c.index].settings, None, tint);
            self.darkroom.zones.draw(&painter, avail, rect, &frame, c.loaded.full_size, ui.input(|i| i.pointer.hover_pos()));
        }
        self.image_rect = Some(rect.intersect(avail));
        photo_display::note_image_rect(rect.intersect(avail), ui.ctx().pixels_per_point());
        if self.show_faces && self.crop_mode.is_none() && !self.show_camera_jpeg && !self.show_before {
            if let Some((_, _, faces)) = &self.faces_cache {
                let r = self.entries[c.index].settings.rotate % 4;
                face_frames::draw(ui, avail, rect, &frame, r, faces, &self.faces_names);
            }
            if self.faces_search.is_some() {
                badge(&painter, rect.intersect(avail), crate::tr("Gesichter werden gesucht …"), None);
            }
        }
        if self.show_focus && self.crop_mode.is_none() && !self.show_camera_jpeg {
            if let Some(status) = focus_frame::draw(ui, avail, rect, &frame, &self.entries[c.index].path) {
                badge(&painter, rect.intersect(avail), &status, None);
            }
        }
        let overlay_clip = self.show_clipping && !self.show_camera_jpeg && !self.show_before;

        // Detail at full resolution as soon as the preview is no longer sufficient when zooming
        let settings = &self.entries[c.index].settings;
        let photo_path = &self.entries[c.index].path;
        let preview_res = c.loaded.previews.developed.lin.width as f32 / full_w;
        // screen pixels per image pixel: `rect` is the crop frame (its width `frame.w` in image pixels, upright), not the whole
        // sensor width – with a crop or an upright portrait photo `rect.width() / full_w` would be too small. Used for the
        // moment the full resolution is needed, the grain loupe factor and drawing.
        let display_scale = rect.width() * ui.ctx().pixels_per_point() / frame.w.max(1.0);
        let screen_px_per_image_px = display_scale;
        // Pipeline inspection stays with the preview (rather blurry than extra load)
        let zoomed_in = !self.show_camera_jpeg && self.crop_mode.is_none() && !settings.view.is_stage() && self.zoom > 1.05 && screen_px_per_image_px > preview_res * 1.1;
        // merged photo, a single exposure held: its sharp crop (rendered ahead for every frame while the view rests)
        if zoomed_in {
            if let (Some(mi), Some(doc)) = (self.merge_info(), c.loaded.previews.foreign.as_ref()) {
                let region = hdr_paint::visible_region(rect, avail, &frame, [fw_px, fh_px]);
                let resting = now - self.view_changed_at > 0.4 && !self.detail_pending;
                if let Some((t, r)) = hdr_paint::peek_detail(ui.ctx(), doc, &mi, settings, region, [fw_px, fh_px], resting) {
                    paint_src_quad(&clipped, t, [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32], &frame, rect, image_tint);
                }
            }
        }
        let needs_detail = zoomed_in && peek.is_none();
        let mut detail_loading = false;
        // Full resolution only from Sigma's base if the image uses it: without it the
        // crop would have to re-render the whole image with our own processing – for seconds
        // under the session lock while the finished base waits for it. Until then the
        // zoomed preview stays (with the small loading spinner). Rendering from the base holds no session lock any
        // more; is the session locked nevertheless (own pipeline), the last known answer counts, not "base missing".
        let detail_ready = |c: &Current, settings: &Settings, gave_up: bool| {
            match &c.loaded.session {
                None => true,
                Some(sess) => {
                    !develop::Session::uses_spp(settings) || gave_up || {
                        let key = develop::session::spp_key_hash(&settings.spp_key(photo_path));
                        match sess.try_lock() {
                            Ok(s) => {
                                let ready = s.has_spp_for(settings);
                                c.spp_ready_cache.set(Some((key, ready)));
                                ready
                            }
                            Err(_) => c.spp_ready_cache.get().is_some_and(|(k, ready)| k == key && ready),
                        }
                    }
                }
            }
        };
        let gave_up = self.refine_retries >= 2;
        // Settings rest since …; a whole image for other settings is no longer wanted (never mix old and new settings)
        if self.detail_settings_since.as_ref().is_none_or(|(s, _)| s != settings) {
            self.detail_settings_since = Some((settings.clone(), now));
            // a new settings state: crops and the whole image of the old one compute nothing more
            self.edit_generation.fetch_add(1, Ordering::SeqCst);
        }
        if self.full_job.as_ref().is_some_and(|(s, _, _)| s != settings) {
            self.full_job = None;
        }

        if needs_detail {
            // visible part of the crop → bounding rectangle in the source image
            let vis = rect.intersect(avail);
            self.sensor_raw.view_center = Some(avail.center());
            let to_src = |p: egui::Pos2| {
                let u = (p - rect.min) / rect.size();
                frame.to_src([u.x, u.y])
            };
            let pts = [vis.left_top(), vis.right_top(), vis.right_bottom(), vis.left_bottom()].map(to_src);
            let (x0, x1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            let (x0, y0) = (x0.max(0.0).floor(), y0.max(0.0).floor());
            let (x1, y1) = (x1.min(fw_px as f32).ceil(), y1.min(fh_px as f32).ceil());
            let region = darkroom_loupe::source_region([x0,y0,x1,y1],[fw_px,fh_px]);
            let factor = darkroom_loupe::factor(screen_px_per_image_px, 1.0, self.darkroom.active && self.darkroom.grain_check && settings.film.is_active(), develop::local::active(settings));
            // Hovered preset/film: effect in the zoomed image too, with its own crop at full resolution
            let hover_s: Option<Settings> = self.hover_shown().map(|(h, _)| h.clone()).filter(|h| h != settings);
            if let Some(hs) = &hover_s {
                if let Some((dtex, r, ds)) = &c.detail_hover {
                    if ds == hs {
                        let src = [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32];
                        paint_src_quad(&clipped, dtex.id(), src, &frame, rect, image_tint);
                    }
                }
            } else {
                if let Some((dtex, _, ds)) = c.detail_prev.as_ref().filter(|(_, r, ds)| ds == settings && c.detail.as_ref().is_none_or(|(_, dr, dds)| dds != settings || !contains(*dr, *r))) {
                    if ds == settings {
                        dtex.paint(&clipped, &frame, rect, image_tint, display_scale);
                    }
                }
                if let Some((dtex, _, ds)) = &c.detail {
                    if ds == settings {
                        dtex.paint(&clipped, &frame, rect, image_tint, display_scale);
                    }
                }
                if factor > 1 {
                    if let Some((dtex, _, ds)) = c.detail_for(factor) {
                        if ds == settings {
                            dtex.paint(&clipped, &frame, rect, image_tint, display_scale);
                        }
                    }
                }
            }
            // What to compute at full resolution (`detail_plan`): a crop around the visible part, the whole image once
            // per settings state (afterwards every zoom and pan draws from it), or nothing. Not the whole image for the
            // film grain loupe (enlarged crops, factor > 1) and gain-map HDR (its EDR layer works on crops).
            let ready = |s: &Settings| detail_ready(c, s, gave_up);
            let decision = detail_plan::decide(&detail_plan::Facts {
                region,
                full: [fw_px, fh_px],
                factor,
                since_view_change: now - self.view_changed_at,
                settings_rest: self.detail_settings_since.as_ref().map_or(0.0, |(_, t)| now - t),
                has_gain_map: c.loaded.previews.foreign.as_ref().is_some_and(|d| d.hdr.is_some()),
                allow_full: self.tab != Tab::Sensor,
                settings,
                hover: hover_s.as_ref(),
                hover_is_preset: hover_s.is_some() && self.hover_preset == hover_s,
                detail: c.detail_for(factor).map(|(_, r, s)| detail_plan::Have { region: *r, settings: s }),
                whole: c.detail.as_ref().map(|(_, r, s)| detail_plan::Have { region: *r, settings: s }),
                detail_hover: c.detail_hover.as_ref().map(|(_, r, s)| detail_plan::Have { region: *r, settings: s }),
                detail_pending: self.detail_pending,
                blocked: now < self.detail_blocked_until,
                full_job: self.full_job.as_ref().map(|(s, _, _)| s),
                full_waiting: self.full_waiting.is_some(),
                full_failed: self.full_failed.as_ref(),
                ready: &ready,
            });
            let stale = decision.stale;
            detail_loading = stale;
            match decision.request {
                detail_plan::Request::Nothing => {}
                detail_plan::Request::WaitForSpp => {
                    self.detail_wants_spp = true;
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
                detail_plan::Request::Later(s) => ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(s)),
                detail_plan::Request::Region { hover, region } => {
                    self.detail_pending = true;
                    let (generation, edit) = (self.open_generation.load(Ordering::SeqCst), jobs::EditStamp::current(&self.edit_generation));
                    if hover {
                        // hover crops land in `detail_hover` (8 bit, `RegionRendered`), never as wide photo textures
                        let rs = hover_s.clone().unwrap_or_else(|| settings.clone());
                        self.worker.send(Job::RenderRegion { factor, index: c.index, session: c.loaded.session.clone(), foreign: c.loaded.previews.foreign.clone(), settings: rs, region, generation, current: self.open_generation.clone(), edit });
                    } else {
                        // with preserved gamut where possible (`RenderDetailWide`), else the plain crop
                        self.worker.send(detail_job(c, settings, Some(region), factor, generation, self.open_generation.clone(), edit));
                    }
                }
                detail_plan::Request::Full { nearly_all } => {
                    let edit = jobs::EditStamp::current(&self.edit_generation);
                    let generation = self.open_generation.load(Ordering::SeqCst);
                    self.full_job = Some((settings.clone(), edit.sent(generation), now));
                    log(&format!("full resolution {}: whole image requested {}", c.index, if nearly_all { "(visible part is nearly all of it)" } else { "in the background" }));
                    // with preserved gamut up to `WIDE_FULL_MAX_PIXELS` (`RenderDetailWide`), else the 8-bit whole image
                    self.worker.send(detail_job(c, settings, None, 1, generation, self.open_generation.clone(), edit));
                }
            }
            // log: time until the visible part is sharp
            match (stale, self.detail_unsharp_since) {
                (true, None) => {
                    self.detail_unsharp_since = Some(now);
                    log(&format!("detail {}: visible part not sharp yet (zoom {:.2})", c.index, self.zoom));
                }
                (false, Some(t)) => {
                    self.detail_unsharp_since = None;
                    log(&format!("detail {}: sharp after {:.0} ms", c.index, (now - t) * 1000.0));
                }
                _ => {}
            }
            // Darkroom comparison at 100 %: re-render the left side at full resolution and lay it over the
            // detail image of the right side, so that the line and comparison do not disappear
            if self.darkroom.active && self.tab == Tab::Develop && self.darkroom.compare.active() {
                if let Some(ls) = self.darkroom.compare.left_settings(settings) {
                    let left_stale = c.detail_left.as_ref().is_none_or(|(_, r, ds)| c.detail_left_scale != factor || *ds != ls || *r != region && !contains(*r, region));
                    if left_stale && !stale && !self.detail_pending && now >= self.detail_blocked_until && now - self.view_changed_at > 0.25 {
                        self.detail_pending = true;
                        self.worker.send(Job::RenderRegion { factor, index: c.index, session: c.loaded.session.clone(), foreign: c.loaded.previews.foreign.clone(), settings: ls.clone(), region, generation: self.open_generation.load(Ordering::SeqCst), current: self.open_generation.clone(), edit: jobs::EditStamp::current(&self.edit_generation) });
                    }
                    let left_detail = c.detail_left.as_ref().filter(|(_, _, ds)| *ds == ls).map(|(t, r, _)| (t.id(), [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32]));
                    self.darkroom.compare.draw(&painter, avail, rect, full_src, &frame, settings, left_detail, tint);
                }
            }
            if self.detail_pending {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        } else {
            self.detail_unsharp_since = None;
            if now - self.view_changed_at < 0.3 {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        // Wide gamut and HDR are part of the drawing above (photo textures, see `photo_display.rs`): fit, zoomed, loupe,
        // with tools and dialogs over the image – nothing to place separately
        let sdr_detail = c.detail.as_ref().filter(|(_, _, ds)| needs_detail && ds == settings);
        let view_kind = match (holding.is_some(), sdr_detail) {
            (true, _) => "large (held)",
            (false, Some((t, _, _))) if t.wide => "detail (wide)",
            (false, Some(_)) => "detail",
            (false, None) if wide_id.is_some_and(|(_, hdr)| hdr) => "hdr",
            (false, None) if wide_id.is_some() => "wide",
            (false, None) => "interactive",
        };
        {
            let since = self.selected_at.filter(|(s, _)| *s == c.index).map(|(_, t)| now - t);
            let id = holding.as_ref().map_or(tex.id(), |t| t.id());
            self.browse.log_view(c.index, view_kind, id, since);
        }

        if overlay_clip && !(self.tab == Tab::Sensor && self.sensor_raw.ready()) {
            if let Some(clip) = &c.clip_tex {
                paint_src_quad(&clipped, clip.id(), full_src, &frame, rect, Color32::WHITE);
            }
        }
        // Sensor view (Bayer/X-Trans): overlays, mosaic, demosaic crop, values at the pointer
        if self.tab == Tab::Sensor && c.loaded.previews.foreign.is_some() && self.sensor_raw.ready() && !self.show_camera_jpeg && !self.show_before {
            let vis = rect.intersect(avail);
            let to_src = |p: egui::Pos2| {
                let u = (p - rect.min) / rect.size();
                frame.to_src([u.x, u.y])
            };
            let pts = [vis.left_top(), vis.right_top(), vis.right_bottom(), vis.left_bottom()].map(to_src);
            let (x0, x1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            let (x0, y0) = (x0.max(0.0), y0.max(0.0));
            let (x1, y1) = (x1.min(fw_px as f32), y1.min(fh_px as f32));
            let px_per_px = rect.width() * ui.ctx().pixels_per_point() / frame.w;
            let (fw, fh) = (fw_px as f32, fh_px as f32);
            let ctx2 = ui.ctx().clone();
            self.sensor_raw.overlay(&ctx2, fw, fh, [x0, y0, x1 - x0, y1 - y0], px_per_px, |t, src, tint| paint_src_quad(&clipped, t.id(), src, &frame, rect, tint));
            self.sensor_raw.paint_pixels(&clipped, [x0, y0, x1-x0, y1-y0], fw, fh, |p| {
                let u = frame.from_src(p);
                rect.min + egui::vec2(u[0], u[1]) * rect.size()
            });
            self.sensor_raw.set_hover(resp.hover_pos().filter(|p| vis.contains(*p)).map(to_src), fw, fh);
            if let (Some(pos), Some(tip)) = (resp.hover_pos().filter(|p| vis.contains(*p)), self.sensor_raw.pixel_tip()) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                self.sensor_raw.paint_hover_pixel(&clipped, fw, fh, |p| {
                    let u = frame.from_src(p);
                    rect.min + egui::vec2(u[0], u[1]) * rect.size()
                });
                // Reuse the image response so the tooltip never intercepts image gestures.
                let mut anchor = resp.clone();
                anchor.id = resp.id.with("sensor-pixel-tip");
                anchor.rect = egui::Rect::from_min_size(pos + egui::vec2(8.0, 8.0), egui::vec2(1.0, 1.0));
                lr::hover_tip(ui, &anchor, &tip);
            } else {
                ui.ctx().data_mut(|d| d.remove_temp::<f64>(resp.id.with("sensor-pixel-tip").with("tipp-start")));
            }
        }
        if self.tab == Tab::Sensor && c.loaded.previews.foreign.is_some() && !self.show_camera_jpeg && !self.show_before {
            let vis = rect.intersect(avail);
            let sensor_z = self.sensor_raw.zoom_image();
            let wanted = if self.sensor_raw.ready() && self.sensor_raw.an.contains(&sensor_raw::Overlay::Mosaic) && sensor_z < 3.99 { Some(8.0) } else if self.sensor_raw.ready() && sensor_z < 0.99 { Some(1.0) } else { None };
            let status = self.sensor_raw.paint_status(&ui.painter().with_clip_rect(vis), vis, if wanted.is_some() { 115.0 } else { 0.0 });
            if let (Some(factor), Some(status)) = (wanted, status) {
                let button = egui::Rect::from_min_size(egui::pos2(status.right() + 4.0, status.center().y - 11.0), egui::vec2(105.0, 22.0));
                if ui.put(button, egui::Button::new(crate::tr("Hineinzoomen"))).clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("sensor-zoom-request"), factor));
                }
            }
        }

        // Loupe (O): square at the mouse pointer showing 100 % at full resolution –
        // the rest stays fit. While full resolution loads, a magnified preview.
        // also in the zoomed image: the loupe then magnifies beyond the current step
        if self.loupe && self.crop_mode.is_none() && !self.show_camera_jpeg {
            if let Some(m) = resp.hover_pos().filter(|p| rect.contains(*p)) {
                let ppp = ui.ctx().pixels_per_point();
                let half = 130.0; // points
                let u = (m - rect.min) / rect.size();
                // Image at 100 %: 1 image pixel = 1 screen pixel (× loupe step); if the image is already
                // magnified more, at least twice the size of the view
                let current_scale = (rect.width() * ppp / frame.w).max(1e-3);
                let scale = if current_scale * 1.5 > self.loupe_scale { (2f32).powf((current_scale * 2.0).log2().ceil()).min(16.0) } else { self.loupe_scale };
                let size100 = egui::vec2(frame.w, frame.h) / ppp * scale;
                let rect100 = egui::Rect::from_min_size(m - egui::vec2(u.x, u.y) * size100, size100);
                let lens = egui::Rect::from_center_size(m, egui::vec2(half * 2.0, half * 2.0)).intersect(avail.shrink(2.0));
                let lp = painter.with_clip_rect(lens);
                lp.rect_filled(lens, 0.0, Color32::BLACK);
                paint_src_quad(&lp, tex_id, full_src, &frame, rect100, Color32::WHITE);
                // required crop in the source image
                let p = frame.to_src([u.x, u.y]);
                // some margin for rotation, more with perspective (locally stretched up to ≈ 2×)
                let r = half * ppp * if frame.has_perspective() { 2.5 } else { 1.5 } / scale;
                let (x0, y0) = ((p[0] - r).max(0.0).floor(), (p[1] - r).max(0.0).floor());
                let (x1, y1) = ((p[0] + r).min(fw_px as f32).ceil(), (p[1] + r).min(fh_px as f32).ceil());
                let need = darkroom_loupe::source_region([x0,y0,x1,y1],[fw_px,fh_px]);
                let mut sharp = false;
                if let Some((dtex, dr, ds)) = &c.detail {
                    if ds == settings {
                        dtex.paint(&lp, &frame, rect100, tint, f32::INFINITY);
                        sharp = contains(*dr, need);
                    }
                }
                if !sharp && !self.detail_pending && now >= self.detail_blocked_until && now - self.loupe_moved_at > 0.08 && !detail_ready(c, settings, gave_up) {
                    self.detail_wants_spp = true;
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                } else if !sharp && !self.detail_pending && now >= self.detail_blocked_until && now - self.loupe_moved_at > 0.08 {
                    self.detail_pending = true;
                    // generous region so that small mouse movements do not need a rebuild
                    let r2 = r * 2.0;
                    let (x0, y0) = ((p[0] - r2).max(0.0).floor(), (p[1] - r2).max(0.0).floor());
                    let (x1, y1) = ((p[0] + r2).min(fw_px as f32).ceil(), (p[1] + r2).min(fh_px as f32).ceil());
                    let region = darkroom_loupe::source_region([x0,y0,x1,y1],[fw_px,fh_px]);
                    self.worker.send(detail_job(c, settings, Some(region), 1, self.open_generation.load(Ordering::SeqCst), self.open_generation.clone(), jobs::EditStamp::current(&self.edit_generation)));
                }
                if ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    self.loupe_moved_at = now;
                }
                painter.rect_stroke(lens, 0.0, egui::Stroke::new(2.0, Color32::from_white_alpha(200)), egui::StrokeKind::Outside);
                painter.rect_stroke(lens.expand(2.0), 0.0, egui::Stroke::new(1.0, Color32::from_black_alpha(160)), egui::StrokeKind::Outside);
                // Step at the bottom left of the loupe, shortly after switching on also the hint
                let pill = |at: egui::Pos2, align: egui::Align2, text: &str, alpha: f32| {
                    let g = painter.layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.5), Color32::from_white_alpha((235.0 * alpha) as u8));
                    let r = align.anchor_size(at, g.size());
                    painter.rect_filled(r.expand2(egui::vec2(6.0, 3.0)), 0.0, Color32::from_black_alpha((150.0 * alpha) as u8));
                    painter.galley(r.min, g, Color32::WHITE);
                };
                pill(lens.left_bottom() + egui::vec2(10.0, -8.0), egui::Align2::LEFT_BOTTOM, &format!("{:.0} %", scale * 100.0), 1.0);
                if !sharp {
                    pill(lens.right_bottom() - egui::vec2(10.0, 8.0), egui::Align2::RIGHT_BOTTOM, crate::tr("lädt …"), 0.8);
                }
                if self.loupe_hint_at == f64::NEG_INFINITY {
                    self.loupe_hint_at = now;
                }
                let age = now - self.loupe_hint_at;
                if age < 3.5 {
                    let a = (1.0 - ((age - 2.5) / 1.0).clamp(0.0, 1.0)) as f32;
                    pill(lens.center_bottom() + egui::vec2(0.0, 12.0), egui::Align2::CENTER_TOP, crate::tr("Scrollen: 100 · 200 · 400 %   ·   O schließt die Lupe"), a);
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
        if self.info_level > 0 && !self.darkroom_covers_image() {
            // in the corner of the image area, not in the photo
            self.info_overlay(&painter, avail);
        }

        // the photo is shown: the loading spinner fades out
        quiet_spinner(&mut self.quiet_spinners[0], ui.ctx(), &painter, rect.intersect(avail), false, None, now);
        // Full resolution still loading: a small, quiet spinner at the top left of the image area – only after 300 ms,
        // faded in and out (the image getting sharper is the actual signal; short loads show nothing)
        let area = rect.intersect(avail);
        {
            let want = detail_loading && self.zoom > 1.05 && self.detail_unsharp_since.is_some_and(|t| now - t > 0.3);
            let dt = (now - self.detail_spinner.1).clamp(0.0, 0.1) as f32;
            let a = (self.detail_spinner.0 + if want { dt / 0.25 } else { -dt / 0.2 }).clamp(0.0, 1.0);
            self.detail_spinner = (a, now);
            if a > 0.0 {
                spinner(&painter, area.left_top() + egui::vec2(18.0, 18.0), 6.5, now, a);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            } else if detail_loading && self.zoom > 1.05 {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
            // Test hook GRAINEO_SPINNER_TEST=1 (only with GRAINEO_DATA_DIR): the spinner on very light, mid-grey and
            // very dark patches and on the image itself, for screenshots of its visibility
            if crate::env::var_os("GRAINEO_SPINNER_TEST").is_some() && crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
                for (i, g) in [250u8, 128, 8].into_iter().enumerate() {
                    let c = area.left_top() + egui::vec2(18.0 + 40.0 * (i as f32 + 1.0), 18.0);
                    painter.rect_filled(egui::Rect::from_center_size(c, egui::vec2(36.0, 36.0)), 0.0, Color32::from_gray(g));
                    spinner(&painter, c, 6.5, now, 1.0);
                }
                spinner(&painter, area.center(), 6.5, now, 1.0);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
            }
        }
        if self.quiet_spinners[0].0 > 0.0 || self.quiet_spinners[1].0 > 0.0 || self.detail_spinner.0 > 0.0 {
            work_step::spinner_tip(ui, area);
        }
        // the AI tools of the Masks tray at work (subject, people, click selection, removal): hint at the top edge
        if let Some(e) = self.current.as_ref().and_then(|c| self.entries.get(c.index)) {
            analysis_progress::image_hint(ui, &painter, area, &e.path);
        }
        // Refinement indicator at the bottom right
        if self.show_camera_jpeg {
            badge(&painter, area, crate::tr("Referenz: Kamera-JPEG – ⇧R zum Umschalten"), None);
        } else if self.look_compare.on {
            badge(&painter, area, crate::tr("Looks zum Vergleichen anklicken (bis zu drei) · Esc beendet"), None);
        } else {
            match self.refine {
                Refine::Working { since } => {
                    // The delay before the start counts too; slower near the end (in case it takes longer)
                    let t = ((now - since) / (self.spp_wait.max(SPP_DELAY_SECS) + self.spp_expected)).max(0.0);
                    let p = (if t < 0.9 { t } else { 0.9 + 0.09 * (1.0 - (-(t - 0.9) * 4.0).exp()) }) as f32;
                    // quiet spinner with a progress ring instead of words
                    quiet_spinner(&mut self.quiet_spinners[1], ui.ctx(), &painter, area, true, Some(p), now);
                }
                Refine::Done { at } => {
                    // done: the bar fills up and fades out quietly
                    let age = now - at;
                    if age < 0.35 {
                        quiet_spinner(&mut self.quiet_spinners[1], ui.ctx(), &painter, area, false, Some(1.0), now);
                        ui.ctx().request_repaint();
                    } else {
                        self.refine = Refine::None;
                    }
                }
                Refine::None => {}
            }
        }

        // Values under the mouse pointer → line below the histogram
        self.readout = None;
        if let Some(pos) = resp.hover_pos().filter(|p| rect.contains(*p) && self.crop_mode.is_none()) {
            let u = (pos - rect.min) / rect.size();
            let src = frame.to_src([u.x, u.y]);
            let p = &c.loaded.previews.sensor;
            let (px, py) = ((src[0] / fw_px as f32 * p.lin.width as f32) as usize, (src[1] / fh_px as f32 * p.lin.height as f32) as usize);
            if px < p.lin.width && py < p.lin.height {
                let v = p.lin.pixel(px, py);
                let sat = c.loaded.saturation;
                self.readout = Some(if c.loaded.previews.foreign.is_some() {
                    // Camera RGB (white-balanced as shot), percent of the white point
                    format!("R {:>5.1} %  G {:>5.1} %  B {:>5.1} %  · {}, {}", v[0] * 100.0, v[1] * 100.0, v[2] * 100.0, px * p.factor, py * p.factor)
                } else {
                    crate::trf!("unten {a}  Mitte {b}  oben {c}  · {d}, {e}", a = format!("{:>4.0}", v[0] * sat), b = format!("{:>4.0}", v[1] * sat), c = format!("{:>4.0}", v[2] * sat), d = px * p.factor, e = py * p.factor)
                });
                if self.wb_picker {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    let index = c.index;
                    let s = &self.entries[index].settings;
                    let lv = wb_picker::levels(&c.loaded);
                    self.wb_pick.follow(index);
                    let side = self.wb_pick.side();
                    let lin = &p.lin;
                    let sample = wb_picker::sample(&|x, y| lin.pixel(x, y), lin.width, lin.height, px, py, side, &lv);
                    // ⇧: this spot together with the samples taken before (⇧-click)
                    let shift = ui.input(|i| i.modifiers.shift);
                    let mut all = if shift { self.wb_pick.taken.clone() } else { Vec::new() };
                    all.push(sample.mean);
                    let mean = wb_picker::combine(&all).unwrap_or(sample.mean);
                    let outcome = if sample.verdict.usable() { wb_picker::outcome(&c.loaded, s, mean) } else { wb_picker::Outcome::None };
                    let probe = wb_picker::Probe { sample, outcome, samples: all.len() };
                    if crate::env::var_os("GRAINEO_LOG").is_some() && resp.clicked() {
                        eprintln!("wb picker: preview {px},{py} of {}x{}, side {side}, {:?} → {:?}", lin.width, lin.height, probe.sample, probe.outcome);
                    }
                    // the loupe: magnified image around the pointer with the sampled square and what a click sets
                    // each preview pixel is one flat block: all four corners sample the texel's middle (sharp, no smoothing)
                    wb_picker::paint_loupe(&painter, avail, pos, (px, py), (lin.width, lin.height), side, &probe, |lp, r, x, y| {
                        let uv = egui::pos2((x as f32 + 0.5) / lin.width as f32, (y as f32 + 0.5) / lin.height as f32);
                        let mut mesh = egui::Mesh::with_texture(tex_id);
                        mesh.add_rect_with_uv(r, egui::Rect::from_min_max(uv, uv), Color32::WHITE);
                        lp.add(egui::Shape::mesh(mesh));
                    });
                    if resp.clicked() && probe.sample.verdict.usable() && probe.outcome != wb_picker::Outcome::None {
                        history::hint(index, history::Hint::white_balance_picker());
                        wb_picker::apply(&probe.outcome, &mut self.entries[index].settings);
                        if shift {
                            // ⇧-click: one more sample, the picker stays on
                            self.wb_pick.taken = all;
                        } else {
                            self.wb_picker = false;
                        }
                    }
                }
            }
        }
        resp.context_menu(|ui| self.image_context_menu(ui));
        self.measure_overlay(ui, &resp, &painter, rect, &frame);
        if self.crop_mode.is_some() {
            self.crop_overlay(ui, &resp, &painter, rect, tex_id);
        }
        if self.wb_picker {
            badge(&painter, avail, crate::tr("Weißabgleich-Pipette: neutrale Stelle anklicken · ⇧ weitere Probe · −/+ Größe · Esc beendet"), None);
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.wb_picker = false;
            }
        } else {
            self.wb_pick.taken.clear();
            self.wb_pick.image = None;
        }
        if self.crop_mode.is_none() && !self.compare && !self.show_before && !self.show_camera_jpeg && !self.measure && !self.wb_picker {
            let index=self.current.as_ref().unwrap().index;
            masks_ui::overlay(ui,&resp,&painter,rect,&frame,[fw_px as f32,fh_px as f32],&mut self.entries[index].settings.local,index);
            { let st = &mut self.entries[index].settings; retouch_ui::overlay(ui,&resp,&painter,rect,&frame,[fw_px as f32,fh_px as f32],&mut st.spots,&mut st.red_eyes,index); remove_ui::overlay(ui,&resp,&painter,rect,&frame,[fw_px as f32,fh_px as f32],&mut st.removals); }
            if let Some(mi) = self.merge_info() { hdr_paint::overlay(ui,&resp,&painter,rect,&frame,[fw_px as f32,fh_px as f32],&mi,avail); }
        }
        // Navigator: the whole photo small at the bottom right while it is larger than the view (navigator.rs)
        let nav = navigator::View { view: avail, image: rect, offset, allowed: self.crop_mode.is_none() && !self.darkroom_covers_image(), interactive: click_free };
        if let Some(pan) = navigator::show(ui, &nav, |p, r, tint| paint_src_quad(p, peek.unwrap_or(tex_id), full_src, &frame, r, tint)) {
            if pan != self.pan {
                self.pan = pan;
                self.view_changed_at = now;
                ui.ctx().request_repaint();
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        // Content never wider than the panel: overlong lines are clipped instead of widening the whole
        // panel (otherwise everything slides out to the left)
        let w = ui.available_width();
        let mut surface = egui::ScrollArea::vertical().auto_shrink([false; 2]).max_width(w);
        // Test hook: GRAINEO_SIDEBAR_SCROLL=<points> also scrolls the Sensor tray (screenshots of the whole tray)
        if let Some(px) = crate::scroll_hold::first_offset(ui, "controls", self.current.is_some()) {
            surface = surface.vertical_scroll_offset(px);
        }
        surface.show(ui, |ui| crate::sticky::scope(ui, |ui| match self.tab {
            Tab::Develop => self.develop_controls(ui, workspace::ToolTab::Edit),
            Tab::Sensor => self.sensor_controls(ui),
            Tab::Export => {
                self.export_controls(ui, false);
            }
            Tab::Library => {}
        }));
    }

    fn develop_controls(&mut self, ui: &mut egui::Ui, split: workspace::ToolTab) {
        if self.current.is_none() {
            ui.label(crate::tr("Kein Bild geöffnet"));
            return;
        }
        // Film & Look shows the selection right away (without an intermediate step with histogram and card)
        if split == workspace::ToolTab::FilmLook {
            self.profile_browser_ui(ui);
            return;
        }
        self.develop_sections(ui, split);
        // the metadata of the file are in the Info panel ("Aufnahmedetails"), not repeated here
    }

    fn develop_sections(&mut self, ui: &mut egui::Ui, split: workspace::ToolTab) {
        use workspace::ToolTab;
        if self.current.is_none() {
            return;
        }
        // Display at the top (histogram header with clipping triangles, shooting data and values under the mouse
        // pointer); above the scroll area when pinned (histogram_pin.rs)
        let hist_drag = self.histogram_display(ui);
        // several photos selected in the grid: "Apply to n photos" right below the histogram
        self.multi_band_slot(ui);
        // Film & Look: profile cards and – for film – its fine sliders; in "Edit" only one line
        if split == ToolTab::Edit {
            self.profile_row(ui);
        }
        let Some(c) = &self.current else { return };

        let foreign = c.loaded.previews.foreign.clone();
        let wb_shot = c.loaded.cal.wb_name.clone();
        let wb_names: Vec<String> = c.loaded.cal.wb_presets.iter().map(|p| p.name.clone()).collect();
        let mut wb_picker = self.wb_picker;
        let index = c.index;
        let uses_spp = develop::Session::uses_spp(&self.entries[index].settings);
        // Sigma's sharpening needs the detail image of the SPP base (Merrill only)
        let sigma_sharp_ok = uses_spp && !c.loaded.cal.is_prepared();
        let auto_src = (c.loaded.previews.clone(), c.loaded.cal.clone());
        let binning_cal = c.loaded.cal.clone();
        let binning_default = binning_ui::default_for(&binning_cal);
        // Lens correction as in SPP: Merrill only (SPP base), profile from the lens data used by SPP
        let lens_ok = !c.loaded.cal.is_prepared();
        let lens_path = self.entries[index].path.clone();
        let lens_profile = if lens_ok {
            self.lens_profiles.entry(lens_path.clone()).or_insert_with(|| develop::lens::profile_name(&lens_path)).clone()
        } else {
            None
        };
        // camera and lens for the lens assignment (own profiles); none for finished images (JPEG, HEIC …)
        let lens_ident = match &foreign {
            Some(doc) if doc.finished => None,
            Some(doc) => {
                let (maker, model, lens) = doc.lens_identity();
                Some(lens_assign::Identity::new(maker, model, lens))
            }
            None if lens_ok => Some(lens_assign::Identity::foveon(&c.loaded.cal.camera)),
            None => None,
        };
        // Context menu of a section header: copy or paste only this section
        if let Some((title, act)) = ui.ctx().data_mut(|d| d.remove_temp::<(String, u8)>(egui::Id::unique(lr::SECTION_ACTION))) {
            if let Some(g) = paste::for_section(&title) {
                match act {
                    1 => self.section_clip = Some((title, self.entries[index].settings.clone())),
                    _ => {
                        if let Some((t, src)) = &self.section_clip {
                            if *t == title {
                                self.entries[index].settings = paste::merge(&self.entries[index].settings, src, &g);
                            }
                        }
                    }
                }
            }
        }
        let clip_title = self.section_clip.as_ref().map(|(t, _)| t.clone());
        ui.ctx().data_mut(|d| match clip_title {
            Some(t) => {
                d.insert_temp(egui::Id::unique(lr::SECTION_CLIP), t);
            }
            None => {
                d.remove::<String>(egui::Id::unique(lr::SECTION_CLIP));
            }
        });
        let s = &mut self.entries[index].settings;
        if let Some((region, dx)) = hist_drag {
            let delta = region.slider_delta(dx as f64);
            match region {
                lr::HistRegion::Shadows => s.shadow = (s.shadow + delta).clamp(-2.0, 2.0),
                lr::HistRegion::Exposure => s.exposure = (s.exposure + delta).clamp(-3.0, 3.0),
                lr::HistRegion::Highlights => s.highlight = (s.highlight + delta).clamp(-2.0, 2.0),
            }
        }
        let d = Settings::default();
        if s.view != View::Developed {
            ui.colored_label(Color32::YELLOW, crate::tr("Sensoransicht aktiv – Regler wirken erst wieder in der Entwicklung."));
            if ui.button(crate::tr("Zur Entwicklung")).clicked() {
                s.view = View::Developed;
            }
        }

        if split == ToolTab::Masks {
            masks_ui::import_hint(ui, s.raw.lightroom_masks.as_ref());
            let path = self.entries[index].path.clone();
            let s = &mut self.entries[index].settings;
            match masks_ui::panel(ui, &mut s.local, index, &path) {
                Some(subject_ui::Request::Analyse(kind)) => subject_ui::start(ui.ctx(), kind, &path, s),
                Some(subject_ui::Request::Click(points, rect)) => subject_ui::start_click(ui.ctx(), &path, s, points, rect),
                Some(subject_ui::Request::Scene(what, model)) => depth_ui::start(ui.ctx(), what, &model, &path, s),
                Some(subject_ui::Request::Sample) => mask_sample::ensure(ui.ctx(), index, &path, s),
                Some(subject_ui::Request::Parts) => face_parts::start(ui.ctx(), &path, s),
                None => {}
            }
            return;
        }
        if split == ToolTab::Retouch {
            let path = self.entries[index].path.clone();
            let s = &mut self.entries[index].settings;
            retouch_ui::panel(ui, &mut s.spots, &mut s.red_eyes, index);
            if let Some(i) = remove_ui::panel(ui, &mut s.removals, &path) {
                remove_ui::start(ui.ctx(), &path, s, i);
            }
            return;
        }
        lr::category(ui, crate::tr("Licht & Farbe"));
        let mut bp = s.bypass;
        if let Some(doc) = &foreign {
            // Non-Sigma RAW: own basic settings (Lightroom-compatible sliders), note about initial values
            raw_app::lightroom_hint(ui, s);
            raw_app::defaults(ui, s, doc, &mut wb_picker);
            bp = s.bypass;
        } else {
            let basic_changed = s.monochrome != d.monochrome
                || !s.wb_is_default()
                || s.color_mode != d.color_mode
                || s.profile_amount != d.profile_amount
                || s.exposure != d.exposure
                || s.contrast != d.contrast
                || s.highlight != d.highlight
                || s.shadow != d.shadow
                || s.fill_light != d.fill_light
                || s.dehaze != d.dehaze
                || s.noise_brake != d.noise_brake
                || s.vibrance != d.vibrance
                || s.clarity != d.clarity
                || s.saturation != d.saturation;
            let mut reset_tone = false;
            let brake_key = egui::Id::unique(("noise-brake-shown", &lens_path));
            let reset_basic = lr::section_switch(ui, "Grundeinstellungen", true, basic_changed, &mut bp.basic, |ui| {
                lr::choice(ui, "Behandlung", &mut s.monochrome, &[(false, crate::tr("Farbe")), (true, crate::tr("Schwarzweiß"))], "");
                // White balance: one drop-down list (as shot, camera presets) with the picker beside it
                {
                    let label = |n: &str| -> &'static str { match n {
                        "Auto" => crate::tr("Automatisch"),
                        "AutoLSP" => crate::tr("Automatisch (natürlich)"),
                        "Sunlight" => crate::tr("Tageslicht"),
                        "Shade" => crate::tr("Schatten"),
                        "Overcast" => crate::tr("Bewölkt"),
                        "Incandescent" => crate::tr("Glühlampe"),
                        "Fluorescent" => crate::tr("Leuchtstofflampe"),
                        "Flash" => crate::tr("Blitz"),
                        "Custom" => crate::tr("Eigener Weißabgleich (Kamera)"),
                        _ => "",
                    } };
                    let icon = |n: &str| -> lr::Icon { match n {
                        "Auto" | "AutoLSP" => lr::Icon::Auto,
                        "Sunlight" => lr::Icon::Sun,
                        "Shade" => lr::Icon::Shade,
                        "Overcast" => lr::Icon::Cloud,
                        "Incandescent" => lr::Icon::Bulb,
                        "Fluorescent" => lr::Icon::Tube,
                        "Flash" => lr::Icon::Flash,
                        _ => lr::Icon::Custom,
                    } };
                    let custom = s.wb_mult != [1.0; 3];
                    let shot = crate::trf!("Wie Aufnahme ({a})", a = match label(&wb_shot) { "" => wb_shot.as_str(), l => l });
                    let mut items: Vec<(lr::Icon, &str)> = vec![(lr::Icon::Camera, shot.as_str())];
                    for n in &wb_names {
                        items.push((icon(n), match label(n) { "" => n.as_str(), l => l }));
                    }
                    let selected = if custom {
                        None
                    } else {
                        match &s.white_balance {
                            None => Some(0),
                            Some(n) => wb_names.iter().position(|m| m == n).map(|i| i + 1),
                        }
                    };
                    let (picked, picker) = lr::icon_select(ui, "wb", "Weißabgleich", &items, selected, (lr::Icon::Picker, "Eigener"), lr::Icon::Picker, wb_picker, &lr::with_shortcut("Pipette: neutrale Bildstelle anklicken", "eyedropper"));
                    if picker {
                        wb_picker = !wb_picker;
                    }
                    if let Some(i) = picked {
                        s.white_balance = if i == 0 { None } else { Some(wb_names[i - 1].clone()) };
                        s.wb_mult = [1.0; 3];
                        wb_picker = false;
                    }
                }
                let mut t = s.wb_temp as f64;
                if lr::slider_gradient(ui, "Temperatur", &mut t, -1.0..=1.0, 0.0, 0.01, "Feinabstimmung relativ zur Vorgabe: − kühler (blau), + wärmer (gelb)", lr::GRADIENT_TEMP) {
                    s.wb_temp = t as f32;
                }
                let mut g = s.wb_tint as f64;
                if lr::slider_gradient(ui, "Tönung", &mut g, -1.0..=1.0, 0.0, 0.01, "− grüner, + magenta – z. B. gegen den Merrill-Grünstich in Hauttönen", lr::GRADIENT_TINT) {
                    s.wb_tint = g as f32;
                }
                ui.add_space(4.0);
                // Heading on the left, "Auto" to the right of it as in common photo apps (fixed place)
                ui.horizontal(|ui| {
                    reset_tone = lr::group_label(ui, "Tonwert");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // keep clear of the overlaid scroll bar at the right edge
                        ui.add_space(8.0);
                        let b = egui::Button::new(egui::RichText::new(crate::tr("Automatisch")).small()).min_size(egui::vec2(0.0, 18.0));
                        let auto = ui.add(b);
                        if auto.clicked() {
                            auto_tone(&auto_src.0, &auto_src.1, s);
                        }
                    });
                });
                lr::slider_gradient(ui, "Belichtung", &mut s.exposure, -3.0..=3.0, 0.0, 0.05, "In Blendenstufen – verschiebt die Tonkurve der Kamera", lr::GRADIENT_EXPOSURE);
                // With film, the film curve replaces Sigma's tone curve: contrast, highlights, shadows have no effect
                let film_on = s.film.is_active();
                ui.add_enabled_ui(!film_on, |ui| {
                    lr::slider(ui, "Kontrast", &mut s.contrast, -2.0..=2.0, 0.0, 0.05);
                    lr::slider(ui, "Lichter", &mut s.highlight, -2.0..=2.0, 0.0, 0.05);
                    lr::slider(ui, "Tiefen", &mut s.shadow, -2.0..=2.0, 0.0, 0.05);
                });
                let mut fill = s.fill_light as f64;
                if lr::slider_hint(ui, "Fill Light (Retinex)", &mut fill, -2.0..=2.0, 0.0, 0.05, "Aufhellung (Retinex): hebt dunkle Bildteile lokal an, ohne die Lichter zu verändern.") {
                    s.fill_light = fill as f32;
                }
                // Sigma brakes the noise only when brightening with Fill Light (develop: brake with fill_light > 0)
                let lifted = s.fill_light > 0.0;
                raw_app::noise_brake(ui, &mut s.noise_brake, lifted, brake_key, "");
                ui.add_space(4.0);
                if lr::group_label(ui, "Präsenz") {
                    s.clarity = d.clarity;
                    s.dehaze = d.dehaze;
                    s.vibrance = d.vibrance;
                    s.saturation = d.saturation;
                }
                let mut cl = s.clarity as f64;
                if lr::slider_hint(ui, "Klarheit", &mut cl, -1.0..=1.0, 0.0, 0.01, "Lokaler Kontrast in den Mitteltönen – betont den Foveon-Mikrokontrast und den „3D“-Eindruck") {
                    s.clarity = cl as f32;
                }
                let mut dh = s.dehaze as f64;
                if lr::slider_hint(ui, "Dunst entfernen", &mut dh, -1.0..=1.0, 0.0, 0.01, "Dunst und Schleier herausrechnen (Dark Channel), negativ: Dunst hinzufügen") {
                    s.dehaze = dh as f32;
                }
                ui.add_enabled_ui(!s.monochrome, |ui| {
                    let mut v = s.vibrance as f64;
                    if lr::slider_gradient(ui, "Dynamik", &mut v, -1.0..=1.0, 0.0, 0.01, "Hebt blasse Farben stärker an als bereits kräftige – schont Hauttöne und Himmel", lr::GRADIENT_SAT) {
                        s.vibrance = v as f32;
                    }
                    let mut sat = s.saturation as f64;
                    if lr::slider_gradient(ui, "Sättigung", &mut sat, -1.0..=1.0, 0.0, 0.01, "Alle Farben gleichmäßig kräftiger oder blasser", lr::GRADIENT_SAT) {
                        s.saturation = sat as f32;
                    }
                });
            });

            if reset_basic {
                s.monochrome = d.monochrome;
                s.color_mode = d.color_mode.clone();
                s.profile_amount = d.profile_amount;
                s.white_balance = None;
                s.wb_mult = d.wb_mult;
                s.wb_temp = 0.0;
                s.wb_tint = 0.0;
                reset_tone = true;
                s.dehaze = d.dehaze;
                s.vibrance = d.vibrance;
                s.saturation = d.saturation;
                s.clarity = d.clarity;
            }
            if reset_tone {
                s.exposure = d.exposure;
                s.contrast = d.contrast;
                s.highlight = d.highlight;
                s.shadow = d.shadow;
                s.fill_light = d.fill_light;
                s.noise_brake = d.noise_brake;
            }
        }

        // Colour casts: corrective, therefore right after the basic settings and before the creative tone curve
        // (in the processing they also act before the curve). Collapsed by default.
        // (only the Sigma path has neutral highlights; other cameras have no colour-cast section)
        let casts_changed = s.highlight_neutralize != d.highlight_neutralize || s.highlight_colour != d.highlight_colour || s.highlight_structure != d.highlight_structure;
        let reset_casts = foreign.is_none() && lr::section_switch(ui, "Farbstiche", crate::env::var_os("GRAINEO_CASTS_OPEN").is_some(), casts_changed, &mut bp.casts, |ui| {
            ui.add_enabled_ui(!s.monochrome, |ui| {
                let mut hn = s.highlight_neutralize as f64;
                if lr::slider_hint(ui, "Lichter neutral", &mut hn, 0.0..=1.0, 0.0, 0.05, "Entfärbt sehr helle Bereiche (Highlight-Neutralisierung), gegen farbige Säume in ausgebrannten Lichtern.") {
                    s.highlight_neutralize = hn as f32;
                }
            });
            crate::highlight_ui::sliders(ui, s, &lens_path);
        });
        if reset_casts {
            s.highlight_neutralize = d.highlight_neutralize;
            s.highlight_colour = d.highlight_colour;
            s.highlight_structure = d.highlight_structure;
        }

        // Tone equalizer: exposure per brightness zone (all cameras), before the creative tone curve
        tone_eq_ui::section(ui, s);

        // Tone curve (point curve) on the output values
        let curve_changed = !s.tone_curve.is_identity() || s.tone_curve != d.tone_curve;
        let mut ch = self.curve_channel;
        let out_hist = c.out_hist.as_ref();
        let reset_curve = lr::section_switch(ui, "Gradationskurve", false, curve_changed, &mut bp.curve, |ui| {
            lr::choice(ui, "Kanal", &mut ch, &[(0, "RGB"), (1, "Rot"), (2, crate::tr("Grün")), (3, "Blau")], "");
            lr::tone_curve(ui, &mut s.tone_curve, ch, out_hist);
        });
        self.curve_channel = ch;
        if reset_curve {
            s.tone_curve = d.tone_curve.clone();
        }

        if s.monochrome && foreign.is_none() {
            let mix_changed = s.mix != d.mix;
            let reset_mix = lr::section_reset(ui, "S/W-Mischung (Sensorschichten)", true, mix_changed, |ui| {
                // Triangle as in SPP: one point sets all three shares (sum 1), snaps to corners, edge midpoints, centre and edges
                mix_triangle::show(ui, &mut s.mix);
                ui.add_space(4.0);
                // As with Sigma, the three shares always sum to 1: one slider moves the other two along in their previous ratio
                for (i, label) in ["Unten (rot)", "Mitte (grün)", "Oben (blau)"].iter().enumerate() {
                    let mut v = s.mix[i] as f64;
                    if lr::slider_gradient(ui, label, &mut v, MIX_MIN as f64..=MIX_MAX as f64, [0.33, 0.33, 0.34][i], 0.01, "Anteil dieser Schicht am Grauwert (0 bis 1); die Summe bleibt 1, die beiden anderen Regler folgen", [lr::GRADIENT_RED, lr::GRADIENT_GREEN, lr::GRADIENT_BLUE][i]) {
                        s.mix = set_mix(s.mix, i, v as f32);
                    }
                }
                // Filters as in front of black & white film, as colour dots (sum = without filter)
                let filters: Vec<(String, String, [f32; 3])> = presets::library()
                    .into_iter()
                    .flat_map(|c| c.presets)
                    .filter(|p| p.scope == presets::Scope::Mix)
                    .map(|p| (p.name.clone(), p.hint.clone(), p.settings.mix))
                    .chain([("Nur oben".to_string(), "Nur die obere Schicht – wie orthochromatischer Film".to_string(), [0.0, 0.0, 1.0])])
                    .collect();
                let chip = |name: &str| -> Color32 { match name {
                    n if n.starts_with("Gelb") => Color32::from_rgb(0xe6, 0xcf, 0x3a),
                    n if n.starts_with("Orange") => Color32::from_rgb(0xe4, 0x8a, 0x2e),
                    n if n.starts_with("Rot") => Color32::from_rgb(0xd9, 0x47, 0x3f),
                    n if n.starts_with("Grün") => Color32::from_rgb(0x4f, 0xbf, 0x5a),
                    n if n.starts_with("Blau") => Color32::from_rgb(0x3f, 0x73, 0xe0),
                    "Nur oben" => Color32::from_rgb(0x7f, 0x9c, 0xd8),
                    _ => Color32::from_gray(0x9a),
                } };
                let tips: Vec<String> = filters.iter().map(|(n, h, _)| if h.is_empty() { n.clone() } else { format!("{n}: {h}") }).collect();
                let items: Vec<(Color32, &str)> = filters.iter().zip(&tips).map(|((n, _, _), t)| (chip(n), t.as_str())).collect();
                let selected = filters.iter().position(|(_, _, m)| s.mix == *m);
                ui.add_space(2.0);
                lr::field_label(ui, crate::tr("Filter"));
                if let Some(i) = lr::color_chips(ui, "bw-filter", &items, selected) {
                    s.mix = filters[i].2;
                }
            });
            if reset_mix {
                s.mix = d.mix;
            }
        }

        if !s.monochrome {
            let hsl_changed = s.hsl.iter().flatten().any(|&v| v != 0.0);
            let mut tab = self.hsl_tab;
            let reset_hsl = lr::section_switch(ui, "Farbmischer", false, hsl_changed, &mut bp.mixer, |ui| {
                lr::segmented_full(ui, &mut tab, &[(0, "Farbton"), (1, crate::tr("Sättigung")), (2, "Luminanz")]);
                ui.add_space(4.0);
                let names = ["Rot", "Orange", "Gelb", "Grün", "Aquamarin", "Blau", "Lila", "Magenta"];
                let hint = match tab {
                    0 => crate::tr("Verschiebt den Farbton dieses Bereichs (± 30°)"),
                    1 => crate::tr("Sättigung nur dieses Farbbereichs – z. B. Aquamarin/Grün senken gegen Farbreflexe auf Haut"),
                    _ => crate::tr("Helligkeit nur dieses Farbbereichs – z. B. Blau senken für dunkleren Himmel"),
                };
                for (i, name) in names.iter().enumerate() {
                    let h = develop::look::HSL_CENTERS[i];
                    let col = |h: f32, s: f32, v: f32| Color32::from(egui::ecolor::Hsva::new(h.rem_euclid(360.0) / 360.0, s, v, 1.0));
                    let grad = match tab {
                        0 => [col(h - 30.0, 0.7, 0.75), col(h + 30.0, 0.7, 0.75)],
                        1 => [Color32::from_gray(0x70), col(h, 0.8, 0.8)],
                        _ => [col(h, 0.7, 0.25), col(h, 0.35, 0.95)],
                    };
                    let mut v = s.hsl[i][tab] as f64;
                    if lr::slider_gradient(ui, name, &mut v, -1.0..=1.0, 0.0, 0.01, hint, grad) {
                        s.hsl[i][tab] = v as f32;
                    }
                }
            });
            self.hsl_tab = tab;
            if reset_hsl {
                s.hsl = d.hsl;
            }
        } else if foreign.is_some() {
            // Black & white mixer: brightness per colour range when converting to grey
            if lr::group_label(ui, crate::tr("Schwarzweiß-Mischer")) {
                s.gray_mix = [0.0; 8];
            }
            let names = ["Rot", "Orange", "Gelb", "Grün", "Aquamarin", "Blau", "Lila", "Magenta"];
            for (i, name) in names.iter().enumerate() {
                let h = develop::look::HSL_CENTERS[i];
                let col = |h: f32, sat: f32, v: f32| Color32::from(egui::ecolor::Hsva::new(h.rem_euclid(360.0) / 360.0, sat, v, 1.0));
                let mut v = s.gray_mix[i] as f64;
                if lr::slider_gradient(ui, name, &mut v, -1.0..=1.0, 0.0, 0.01, crate::tr("Helligkeit dieses Farbbereichs im Schwarzweißbild – z. B. Blau senken für einen dunkleren Himmel"), [col(h, 0.7, 0.25), col(h, 0.35, 0.95)]) {
                    s.gray_mix[i] = v as f32;
                }
            }
        }

        // Calibration (only RAWs of other cameras): primaries of the profile and shadow tint
        if foreign.is_some() {
            let calib_changed = s.raw.calibration.iter().any(|v| *v != 0.0);
            let reset_calib = lr::section_reset(ui, "Kalibrierung", false, calib_changed, |ui| {
                let groups = [("Rot", 0usize, lr::GRADIENT_RED), ("Grün", 2, lr::GRADIENT_GREEN), ("Blau", 4, lr::GRADIENT_BLUE)];
                for (name, i, grad) in groups {
                    lr::group_label(ui, name);
                    let mut h = s.raw.calibration[i] as f64;
                    if lr::slider_gradient(ui, "Farbton", &mut h, -1.0..=1.0, 0.0, 0.01, "Dreht diese Grundfarbe des Profils zur nächsten hin (Rot → Gelb, Grün → Cyan, Blau → Violett); wirkt auf alle Farben, die sie enthalten", grad) {
                        s.raw.calibration[i] = h as f32;
                    }
                    let mut v = s.raw.calibration[i + 1] as f64;
                    if lr::slider_gradient(ui, "Sättigung", &mut v, -1.0..=1.0, 0.0, 0.01, "Buntheit dieser Grundfarbe des Profils", grad) {
                        s.raw.calibration[i + 1] = v as f32;
                    }
                }
                lr::group_label(ui, "Schatten");
                let mut t = s.raw.calibration[6] as f64;
                if lr::slider_gradient(ui, "Tönung", &mut t, -1.0..=1.0, 0.0, 0.01, "Farbstich in den Tiefen: links grüner, rechts magenta", lr::GRADIENT_TINT) {
                    s.raw.calibration[6] = t as f32;
                }
            });
            if reset_calib {
                s.raw.calibration = [0.0; 7];
            }
        }

        let grade_changed = s.grade_shadows[1] > 0.0 || s.grade_midtones[1] > 0.0 || s.grade_highlights[1] > 0.0 || s.grade_balance != 0.0;
        let reset_grade = lr::section_switch(ui, "Color Grading", false, grade_changed, &mut bp.grading, |ui| {
            let w = ((ui.available_width() - 16.0) / 3.0).min(110.0);
            ui.horizontal(|ui| {
                lr::color_wheel(ui, "Schatten", &mut s.grade_shadows, w);
                lr::color_wheel(ui, "Mitte", &mut s.grade_midtones, w);
                lr::color_wheel(ui, "Lichter", &mut s.grade_highlights, w);
            });
            let mut b = s.grade_balance as f64;
            if lr::slider_hint(ui, "Balance", &mut b, -1.0..=1.0, 0.0, 0.01, "− mehr Schatten-Tonung, + mehr Lichter-Tonung") {
                s.grade_balance = b as f32;
            }
        });
        if reset_grade {
            s.grade_shadows = d.grade_shadows;
            s.grade_midtones = d.grade_midtones;
            s.grade_highlights = d.grade_highlights;
            s.grade_balance = 0.0;
        }

        lr::category(ui, crate::tr("Gestaltung"));
        let fx_changed = s.fade != 0.0 || s.vignette != 0.0 || s.grain_amount != 0.0;
        let reset_fx = lr::section_switch(ui, "Effekte", false, fx_changed, &mut bp.effects, |ui| {
            let mut v = s.vignette as f64;
            if lr::slider_hint(ui, "Vignette", &mut v, -1.0..=1.0, 0.0, 0.01, "− Ränder abdunkeln, + aufhellen") {
                s.vignette = v as f32;
            }
            ui.add_enabled_ui(s.vignette != 0.0, |ui| {
                let mut m = s.vignette_mid as f64;
                if lr::slider_hint(ui, "Mittelpunkt", &mut m, 0.0..=1.0, 0.5, 0.01, "Wie weit die Vignette zur Mitte reicht") {
                    s.vignette_mid = m as f32;
                }
            });
            ui.add_space(2.0);
            let mut g = s.grain_amount as f64;
            if lr::slider_hint(ui, "Korn", &mut g, 0.0..=1.0, 0.0, 0.01, "Filmkorn – am besten in 100 % beurteilen") {
                s.grain_amount = g as f32;
            }
            ui.add_enabled_ui(s.grain_amount > 0.0, |ui| {
                let mut gs = s.grain_size as f64;
                if lr::slider_hint(ui, "Korngröße", &mut gs, 0.5..=3.0, 1.0, 0.05, "Klein = feinkörniger Film (Pan F, Acros), groß = Push/High-ISO (Tri-X, Delta 3200)") {
                    s.grain_size = gs as f32;
                }
                let mut gr = s.grain_rough as f64;
                if lr::slider_hint(ui, "Rauigkeit", &mut gr, 0.0..=1.0, 0.5, 0.01, "Gleichmäßiges ↔ unregelmäßiges Korn") {
                    s.grain_rough = gr as f32;
                }
            });
            ui.add_space(2.0);
            let mut f = s.fade as f64;
            if lr::slider_hint(ui, "Matte Tiefen", &mut f, 0.0..=0.3, 0.0, 0.005, "Hebt den Schwarzpunkt an – der ausgeblichene Look alter Abzüge") {
                s.fade = f as f32;
            }
        });
        s.bypass = bp;
        if reset_fx {
            s.fade = 0.0;
            s.vignette = 0.0;
            s.vignette_mid = d.vignette_mid;
            s.grain_amount = 0.0;
            s.grain_size = d.grain_size;
            s.grain_rough = d.grain_rough;
        }

        // Layer mixer: Foveon layers freely onto R/G/B instead of Sigma's colour matrix (colour only;
        // in black & white the B&W mix applies), see develop::layers. Operated like a
        // classic channel mixer: choose an output channel, shares of the three layers in %.
        // Off by default (mainly for infrared conversions), can be enabled
        // under Preferences → Develop; an image with a mix always shows it so that it can be reset.
        let layers_visible = prefs::layer_mixer_on() || s.layer_matrix.is_some() || crate::env::var_os("GRAINEO_LAYERS_OPEN").is_some();
        if !s.monochrome && foreign.is_none() && layers_visible {
            use develop::layers;
            let layers_changed = s.layer_matrix.is_some();
            let row_id = egui::Id::unique("schichtmischer-ausgabe");
            let mut row: usize = ui.ctx().data(|dm| dm.get_temp(row_id)).unwrap_or(0);
            let reset_layers = lr::section_switch(ui, "Schichtmischer", crate::env::var_os("GRAINEO_LAYERS_OPEN").is_some(), layers_changed, &mut bp.layers, |ui| {
                ui.add_space(2.0);
                let mut sel = layers::PRESETS.iter().position(|(_, m)| *m == s.layer_matrix).unwrap_or(usize::MAX);
                if lr::choice(ui, "Vorgabe", &mut sel, &[(0, crate::tr("Kamera")), (1, crate::tr("Identität")), (2, "IR NRG"), (3, "R↔B")], "") {
                    s.layer_matrix = layers::PRESETS[sel].1;
                }
                ui.add_space(4.0);
                let on = s.layer_matrix.is_some();
                ui.add_enabled_ui(on, |ui| {
                    lr::choice(ui, "Ausgabe", &mut row, &[(0, "Rot"), (1, crate::tr("Grün")), (2, "Blau")], "");
                    let mut m = s.layer_matrix.unwrap_or(layers::IDENTITY);
                    let mut changed = false;
                    for (i, label) in ["aus Unten", "aus Mitte", "aus Oben"].iter().enumerate() {
                        let mut v = (m[row][i] * 100.0).round() as f64;
                        let tip = crate::tr("Anteil dieser Schicht am gewählten Ausgabekanal in Prozent");
                        if lr::slider_gradient(ui, label, &mut v, -100.0..=200.0, (layers::IDENTITY[row][i] * 100.0) as f64, 1.0, tip, [lr::GRADIENT_RED, lr::GRADIENT_GREEN, lr::GRADIENT_BLUE][i]) {
                            m[row][i] = v as f32 / 100.0;
                            changed = true;
                        }
                    }
                    let sum = (m[row].iter().sum::<f32>() * 100.0).round();
                    ui.label(egui::RichText::new(crate::trf!("Summe {sum} %", sum = format!("{:.0}", sum))).small().color(if sum == 100.0 { lr::TEXT_DIM() } else { Color32::from_rgb(0xe0, 0xb0, 0x60) }));
                    if changed && on {
                        s.layer_matrix = Some(m);
                    }
                });
            });
            ui.ctx().data_mut(|dm| dm.insert_temp(row_id, row));
            if reset_layers {
                s.layer_matrix = d.layer_matrix;
            }
        }

        // Remove fringing (Lightroom: Lens Corrections → Manual), a subgroup of the
        // lens corrections; applies to all cameras on the finished image (develop::defringe)
        let fringe_changed = s.defringe_purple_amount != d.defringe_purple_amount
            || s.defringe_purple_hue != d.defringe_purple_hue
            || s.defringe_green_amount != d.defringe_green_amount
            || s.defringe_green_hue != d.defringe_green_hue;
        lr::category(ui, crate::tr("Optik & Details"));
        // Lens corrections as in SIGMA Photo Pro: for Foveon cameras only chromatic
        // aberration (lens profile or manual red/blue).
        // Applies in the SPP base (recomputation ≈ 3 s), hence fixed rows without jumps.
        let lens_changed = s.ca_mode != d.ca_mode || s.ca_profile != d.ca_profile || s.ca_red != d.ca_red || s.ca_blue != d.ca_blue
            || s.raw.lens != d.raw.lens || s.raw.lens_distortion != d.raw.lens_distortion || s.raw.lens_vignette != d.raw.lens_vignette || s.raw.lens_ca != d.raw.lens_ca || s.raw.lens_image != d.raw.lens_image || s.raw.lens_profile != d.raw.lens_profile;
        let reset_lens = lr::section_reset(ui, "Objektivkorrekturen", crate::env::var_os("GRAINEO_LENS_OPEN").is_some(), lens_changed || fringe_changed, |ui| {
            if let Some(doc) = &foreign {
                raw_app::lens(ui, s, doc);
                if let Some(id) = &lens_ident {
                    lens_assign::section(ui, s, id);
                }
            } else {
                use develop::lens::CaMode;
                if lr::group_label(ui, "Chromatische Aberration") {
                    s.ca_mode = d.ca_mode;
                    s.ca_profile = d.ca_profile;
                    s.ca_red = d.ca_red;
                    s.ca_blue = d.ca_blue;
                }
                ui.add_enabled_ui(lens_ok && !s.monochrome, |ui| {
                    lr::choice(ui, "Korrektur", &mut s.ca_mode, &[(CaMode::Off, "Aus"), (CaMode::Profile, crate::tr("Profil")), (CaMode::Manual, "Manuell")], "");
                    ui.add_enabled_ui(s.ca_mode == CaMode::Profile, |ui| {
                        let mut v = s.ca_profile as f64;
                        if lr::slider_hint(ui, "Stärke", &mut v, 0.0..=2.0, 1.0, 0.1, "Objektivprofil: Sigmas Korrekturdaten für dieses Objektiv, Stärke 0 … 2 (1 = Standard)") {
                            s.ca_profile = ((v * 10.0).round() / 10.0) as f32;
                        }
                    });
                    ui.add_enabled_ui(s.ca_mode == CaMode::Manual, |ui| {
                        let mut r = s.ca_red as f64;
                        if lr::slider_gradient(ui, "Rot", &mut r, -2.0..=2.0, 0.0, 0.1, "Manuell: zieht den Rotkanal zur Bildmitte (+) oder zum Rand (−), gegen Farbsäume an den Bildrändern", [Color32::from_rgb(0x30, 0x90, 0x90), Color32::from_rgb(0xc0, 0x40, 0x40)]) {
                            s.ca_red = ((r * 10.0).round() / 10.0) as f32;
                        }
                        let mut b = s.ca_blue as f64;
                        if lr::slider_gradient(ui, "Blau", &mut b, -2.0..=2.0, 0.0, 0.1, "Manuell: zieht den Blaukanal zur Bildmitte (+) oder zum Rand (−), gegen Farbsäume an den Bildrändern", [Color32::from_rgb(0xb0, 0xa0, 0x30), Color32::from_rgb(0x40, 0x60, 0xd0)]) {
                            s.ca_blue = ((b * 10.0).round() / 10.0) as f32;
                        }
                    });
                });
                // Always exactly one hint line
                let note = if !lens_ok {
                    crate::tr("Nur für Merrill-Kameras (Sigma-Objektivdaten)").to_string()
                } else if s.monochrome {
                    crate::tr("In Monochrom keine Korrektur (Sigma-Objektivdaten)").to_string()
                } else {
                    match &lens_profile {
                        Some(name) => crate::trf!("Profil: {name}", name = name),
                        None => crate::tr("Kein Objektivprofil (Sigma-Objektivdaten nicht gefunden)").to_string(),
                    }
                };
                ui.add(egui::Label::new(egui::RichText::new(note).small().color(lr::TEXT_DIM())).truncate());
                ui.add_space(4.0);
                if let Some(id) = &lens_ident {
                    lens_assign::section(ui, s, id);
                }
            }
            let purple = |v: f32| Color32::from(egui::ecolor::Hsva::new(develop::defringe::purple_hue_deg(v) / 360.0, 0.65, 0.85, 1.0));
            let green = |v: f32| Color32::from(egui::ecolor::Hsva::new(develop::defringe::green_hue_deg(v) / 360.0, 0.65, 0.8, 1.0));
            if lr::group_label(ui, "Violette Säume") {
                s.defringe_purple_amount = d.defringe_purple_amount;
                s.defringe_purple_hue = d.defringe_purple_hue;
            }
            let mut a = s.defringe_purple_amount as f64 * 20.0;
            if lr::slider_hint(ui, "Betrag", &mut a, 0.0..=20.0, 0.0, 1.0, "Entfärbt violette und magentafarbene Säume an harten Kontrastkanten (Äste gegen Himmel, Gegenlicht) – am besten in 100 % beurteilen") {
                s.defringe_purple_amount = (a / 20.0) as f32;
            }
            ui.add_enabled_ui(s.defringe_purple_amount > 0.0, |ui| {
                lr::hue_range(ui, "Farbton", &mut s.defringe_purple_hue, d.defringe_purple_hue, develop::defringe::MIN_SPAN, purple, "Welche Farbtöne als violetter Saum gelten");
            });
            ui.add_space(2.0);
            if lr::group_label(ui, "Grüne Säume") {
                s.defringe_green_amount = d.defringe_green_amount;
                s.defringe_green_hue = d.defringe_green_hue;
            }
            ui.push_id("defringe-green", |ui| {
                let mut g = s.defringe_green_amount as f64 * 20.0;
                if lr::slider_hint(ui, "Betrag", &mut g, 0.0..=20.0, 0.0, 1.0, "Entfärbt grüne Säume an harten Kontrastkanten – Blätter und andere grüne Flächen bleiben erhalten") {
                    s.defringe_green_amount = (g / 20.0) as f32;
                }
                ui.add_enabled_ui(s.defringe_green_amount > 0.0, |ui| {
                    lr::hue_range(ui, "Farbton", &mut s.defringe_green_hue, d.defringe_green_hue, develop::defringe::MIN_SPAN, green, "Welche Farbtöne als grüner Saum gelten");
                });
            });
        });
        if reset_lens {
            s.defringe_purple_amount = d.defringe_purple_amount;
            s.defringe_purple_hue = d.defringe_purple_hue;
            s.defringe_green_amount = d.defringe_green_amount;
            s.defringe_green_hue = d.defringe_green_hue;
            s.ca_mode = d.ca_mode;
            s.ca_profile = d.ca_profile;
            s.ca_red = d.ca_red;
            s.ca_blue = d.ca_blue;
            s.raw.lens = d.raw.lens;
            s.raw.lens_distortion = d.raw.lens_distortion;
            s.raw.lens_vignette = d.raw.lens_vignette;
            s.raw.lens_ca = d.raw.lens_ca;
            s.raw.lens_image = d.raw.lens_image;
            s.raw.lens_profile = d.raw.lens_profile.clone();
        }

        let nr_changed = s.denoise != d.denoise || s.detail_fine != d.detail_fine || s.detail_structure != d.detail_structure || s.nr_luma != d.nr_luma || s.nr_chroma != d.nr_chroma || s.quattro_detail != d.quattro_detail || s.binning != binning_default || s.sharpen != d.sharpen || s.sigma_sharpness != d.sigma_sharpness
            || s.raw.nr_luma != d.raw.nr_luma || s.raw.nr_luma_detail != d.raw.nr_luma_detail || s.raw.nr_color != d.raw.nr_color || s.raw.nr_color_detail != d.raw.nr_color_detail || s.raw.false_color != d.raw.false_color || s.raw.ai_denoise != d.raw.ai_denoise;
        let raw_nr = foreign.as_ref().map(|doc| doc.preview.profile.is_some());
        let reset_nr = lr::section_reset(ui, "Details", true, nr_changed, |ui| {
            if lr::group_label(ui, "Schärfen") {
                s.sharpen = 0.0;
                s.sigma_sharpness = 0.0;
            }
            let sharp_row = ui.add_enabled_ui(sigma_sharp_ok, |ui| {
                if raw_nr.is_some() {
                    return;
                }
                let mut v = s.sigma_sharpness as f64;
                if lr::slider_hint(ui, "Foveon-Schärfe", &mut v, -2.0..=2.0, 0.0, 0.1, "Foveon-Schärfung: 0 = Grundschärfung, −2 = ungeschärft, +2 = kräftig. Exakt in 100 %, die eingepasste Ansicht zeigt eine Näherung") {
                    s.sigma_sharpness = v as f32;
                }
            });
            if !sigma_sharp_ok && raw_nr.is_none() {
                // greyed out: say why (binning_ui.rs)
                let r = ui.interact(sharp_row.response.rect, ui.id().with("sigma-sharpness-locked"), egui::Sense::hover());
                // below the row, as every tooltip (QA 11; it names "Zusätzlich schärfen", which it covers only while hovered)
                lr::hover_tip(ui, &r, binning_ui::sigma_sharpness_locked_tip(binning_cal.is_prepared()));
            }
            if raw_nr.is_none() {
                ui.label(egui::RichText::new(crate::tr("Zusätzlich schärfen")).small().color(lr::TEXT_DIM()));
            }
            let mut sh = s.sharpen as f64;
            if lr::slider_hint(ui, "Betrag", &mut sh, 0.0..=2.0, 0.0, 0.01, "Zusätzlich zur Foveon-Schärfung – am besten in 100 % beurteilen (in der eingepassten Ansicht kaum sichtbar)") {
                s.sharpen = sh as f32;
            }
            ui.add_enabled_ui(s.sharpen > 0.0, |ui| {
                let mut r = s.sharpen_radius as f64;
                if lr::slider_hint(ui, "Radius", &mut r, 0.5..=3.0, 1.0, 0.05, "Klein = feine Details (Foveon-typisch), groß = gröbere Kanten") {
                    s.sharpen_radius = r as f32;
                }
                let mut m = s.sharpen_mask as f64;
                if lr::slider_hint(ui, "Maskieren", &mut m, 0.0..=1.0, 0.0, 0.01, "Schärft nur Kanten, glatte Flächen (Himmel, Haut) bleiben ruhig") {
                    s.sharpen_mask = m as f32;
                }
            });
            ui.add_space(4.0);
            if let Some(possible) = raw_nr {
                raw_app::noise(ui, s, possible, foreign.as_ref().is_some_and(|d| d.info.sensor == raw::Sensor::XTrans), foreign.as_deref());
            } else {
                // Sigma's "noise reduction" is also Sigma's detail level (base and detail layer,
                // base sharpness) – switching it off therefore also removes the typical Merrill sharpness
                lr::group_label(ui, "Rauschen & Foveon-Detail");
                // two band sliders instead of the switch (detail_ui.rs); photos stored with the switch off keep it
                if !crate::detail_ui::sliders(ui, s) {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut s.denoise, crate::tr("Foveon-Detail"));
                        help::info(ui, "sigma-detail", "Foveon-Detail", &help::SIGMA_DETAIL);
                    });
                }
                // Quattro: adjustable from capture ISO 400 on (full chain); below it the noise reduction runs in the
                // preparation (see `binning_ui::noise_sliders_locked`). Greyed out, the stored values stay and still apply.
                let locked = binning_ui::noise_sliders_locked(&binning_cal, &lens_path);
                // Classic below ISO 400: no adjustable noise reduction in the processing (greyed out, with the reason)
                let classic_low = binning_ui::classic_noise_locked(binning_cal.model, binning_cal.iso);
                let sliders = ui.add_enabled_ui(s.denoise && locked.is_none() && !classic_low, |ui| {
                    lr::slider(ui, "Luminanz", &mut s.nr_luma, 0.0..=1.0, 0.5, 0.25);
                    lr::slider(ui, "Farbe", &mut s.nr_chroma, 0.0..=1.0, 0.5, 0.25);
                });
                if locked.is_some() || classic_low {
                    let r = ui.interact(sliders.response.rect, ui.id().with("nr-locked"), egui::Sense::hover());
                    let tip = match locked {
                        Some(lock) if !classic_low => binning_ui::noise_locked_tip(lock, s.nr_luma != 0.5 || s.nr_chroma != 0.5),
                        _ => binning_ui::CLASSIC_NOISE_TIP,
                    };
                    lr::hover_tip(ui, &r, tip);
                }
                // Quattro: detail step of the chain from capture ISO 400 on (binning_ui.rs)
                binning_ui::detail_slider(ui, &binning_cal, &lens_path, s);
                // Quattro at high ISO: binning per photo (binning_ui.rs)
                binning_ui::switch(ui, &binning_cal, s);
                // neural denoising after Sigma's noise reduction (ai_denoise_ui.rs)
                crate::ai_denoise_ui::section_sigma(ui, s, &lens_path);
            }
        });
        if reset_nr {
            s.sigma_sharpness = d.sigma_sharpness;
            s.sharpen = d.sharpen;
            s.sharpen_radius = d.sharpen_radius;
            s.sharpen_mask = d.sharpen_mask;
            s.denoise = d.denoise;
            s.detail_fine = d.detail_fine;
            s.detail_structure = d.detail_structure;
            s.nr_luma = d.nr_luma;
            s.nr_chroma = d.nr_chroma;
            s.quattro_detail = d.quattro_detail;
            s.binning = binning_default;
            s.raw.nr_luma = d.raw.nr_luma;
            s.raw.nr_luma_detail = d.raw.nr_luma_detail;
            s.raw.nr_color = d.raw.nr_color;
            s.raw.nr_color_detail = d.raw.nr_color_detail;
            s.raw.false_color = d.raw.false_color;
            s.raw.ai_denoise = d.raw.ai_denoise;
        }

        ui.add_space(6.0);
        ui.add_space(4.0);
        let mut copy = false;
        let mut paste = false;
        let has_clip = self.clipboard.is_some();
        ui.columns(2, |cols| {
            let k = cols[0].add_sized([cols[0].available_width(), 24.0], egui::Button::new(crate::tr("Kopieren")));
            lr::hover_tip(&cols[0], &k, &crate::shortcuts::display("copy_settings"));
            copy = k.clicked();
            let e = cols[1].add_enabled(has_clip, egui::Button::new(crate::tr("Einfügen")).min_size(egui::vec2(cols[1].available_width(), 24.0)));
            lr::hover_tip(&cols[1], &e, &crate::shortcuts::display("paste_settings"));
            paste = e.clicked();
        });
        let reset_button = ui.add_sized([ui.available_width(), 24.0], egui::Button::new(crate::tr("Alle Einstellungen zurücksetzen")));
        lr::hover_tip(ui, &reset_button, &crate::shortcuts::display("reset_all"));
        let reset = reset_button.clicked();
        if copy {
            self.copy_dialog = true;
        }
        if paste {
            self.pending_paste = true;
        }
        if reset {
            // one undoable command (⌘Z also in the grid), see settings_group.rs
            crate::settings_group::request_reset();
        }
        self.wb_picker = wb_picker;
    }

    /// Measurement area: drag a rectangle (with the tool active) and draw the current area.
    fn measure_overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, frame: &Frame) {
        let Some(c) = &self.current else { return };
        let (index, [fw, fh]) = (c.index, c.loaded.full_size);
        let to_img = |p: egui::Pos2| {
            let u = ((p - rect.min) / rect.size()).clamp(egui::Vec2::ZERO, egui::Vec2::splat(1.0));
            let q = frame.to_src([u.x, u.y]);
            [q[0].clamp(0.0, fw as f32 - 1.0) as usize, q[1].clamp(0.0, fh as f32 - 1.0) as usize]
        };
        let to_screen = |x: usize, y: usize| {
            let u = frame.from_src([x as f32, y as f32]);
            rect.min + egui::vec2(u[0], u[1]) * rect.size()
        };
        if self.measure {
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            if resp.drag_started() {
                self.measure_start = resp.interact_pointer_pos();
            }
            if let (Some(a), Some(b)) = (self.measure_start, resp.interact_pointer_pos()) {
                if resp.dragged() {
                    painter.rect_stroke(egui::Rect::from_two_pos(a, b), 0.0, egui::Stroke::new(1.5, lr::ACCENT), egui::StrokeKind::Middle);
                }
                if resp.drag_stopped() {
                    let (p, q) = (to_img(a), to_img(b));
                    let (x, y) = (p[0].min(q[0]), p[1].min(q[1]));
                    let (w, h) = (p[0].abs_diff(q[0]).max(2), p[1].abs_diff(q[1]).max(2));
                    self.measure_rect = Some((index, [x, y, w, h]));
                    self.measure_start = None;
                    if let Some(session) = c.loaded.session.clone() {
                        self.worker.send(Job::Stats { index, session, rect: [x, y, w, h] });
                    }
                }
            }
            badge(painter, rect.intersect(ui.clip_rect()), crate::tr("Messfeld: Rechteck aufziehen · M beendet"), None);
        }
        if let Some((i, [x, y, w, h])) = self.measure_rect {
            // only while the tool is active (the values stay in the sensor section)
            if i == index && self.measure && self.measure_start.is_none() {
                let pts = vec![to_screen(x, y), to_screen(x + w, y), to_screen(x + w, y + h), to_screen(x, y + h)];
                painter.add(egui::Shape::closed_line(pts.clone(), egui::Stroke::new(2.5, Color32::BLACK)));
                painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.2, Color32::WHITE)));
            }
        }
    }

    /// Toolbar below the image: view, flag, rating, overlays.
    /// Same building blocks as the library's filter bar (`lr::cell_group`, height `lr::BAR_H`,
    /// 10 px between groups) and the same order: flags before stars.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        use lr::Cell;
        if self.compare_shown() {
            self.compare_toolbar(ui);
            return;
        }
        use toolbar_plan::{Group, GAP};
        let Some(i) = self.loading.or(self.current.as_ref().map(|c| c.index)).or(self.not_developable).filter(|&i| i < self.entries.len()) else { return };
        // view, rotation and zoom need the loaded image: dimmed until it is there (same places)
        let loaded = self.loading.is_none() && self.current.as_ref().is_some_and(|c| c.index == i);
        let darkroom = self.darkroom_active();
        let darkroom_comparing = darkroom && self.darkroom.compare.mode != crate::darkroom_compare::Mode::Off;
        // One measured width rule: only the zoom menu always stays, the others give way whole, in a fixed rank;
        // widths are those measured in the last frame (estimates only for the very first one)
        let available = ui.available_width();
        let notices: Vec<help::NoticeItem> = self.photo_notices(i).into_iter().map(|n| help::NoticeItem { title: n.title, text: n.detail, layers: n.layers }).collect();
        let measured = |app: &Self, g: Group, guess: f32| app.toolbar_widths.get(g.key()).copied().unwrap_or(guess);
        // very narrow: the menu shows only the percentage, so menu and "MEHR" still fit
        let wide_combo = if darkroom { 116.0 } else { 140.0 };
        let notice_w = self.toolbar_widths.get("notice").copied().unwrap_or(24.0);
        let wide_combo = self.toolbar_widths.get("zoom-menu").copied().unwrap_or(wide_combo);
        self.zoom_narrow = available - 20.0 < wide_combo + notice_w + toolbar_plan::MORE_W + 2.0 * GAP;
        let combo = if self.zoom_narrow { NARROW_ZOOM_W } else { self.toolbar_widths.get("zoom-menu").copied().unwrap_or(wide_combo) };
        // the condition mark always keeps its place (nothing moves when it appears)
        let fixed = [combo, self.toolbar_widths.get("notice").copied().unwrap_or(24.0)];
        let mut groups = vec![
            (Group::View, measured(self, Group::View, 120.0)),
            (Group::ZoomButtons, measured(self, Group::ZoomButtons, 56.0)),
            (Group::Flag, measured(self, Group::Flag, 60.0)),
            (Group::Stars, measured(self, Group::Stars, 110.0)),
            (Group::Labels, measured(self, Group::Labels, 110.0)),
            (Group::Rotate, measured(self, Group::Rotate, 60.0)),
            (Group::Overlays, measured(self, Group::Overlays, 120.0)),
        ];
        if darkroom {
            let guess = self.darkroom_tools_width(ui);
            groups.push((Group::Darkroom, measured(self, Group::Darkroom, guess)));
        }
        // a small reserve for spacing egui adds itself (e.g. around the zoom menu)
        let shown = toolbar_plan::plan(available - 20.0, &fixed, &groups);
        let hidden: Vec<Group> = groups.iter().map(|g| g.0).filter(|g| !shown.contains(g)).collect();
        let has = |g: Group| shown.contains(&g);
        self.zoom_compact = !has(Group::ZoomButtons);
        let mut widths: Vec<(&'static str, f32)> = Vec::new();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            let comparing_photos = self.compare_in_develop();
            let view = if comparing_photos { 4 } else if darkroom_comparing || self.compare { 1 } else if self.show_before { 2 } else if self.show_camera_jpeg { 3 } else { 0 };
            let left = if loaded { self.before_pane().1.label } else { crate::tr("Standardentwicklung").to_string() };
            let key = |k: &str| crate::shortcuts::display(k);
            // fifth cell: the photo comparison (photo_compare_ui.rs), from 2 photos selected in the filmstrip
            let can_compare = comparing_photos || self.selection().len() >= 2;
            let compare_tip = if comparing_photos { lr::with_shortcut("Vergleich beenden", "compare_open") } else if can_compare { lr::with_shortcut("Mit den gewählten Fotos vergleichen", "compare_open") } else { crate::tr("Im Filmstreifen weitere Fotos mit ⌘-Klick wählen").to_string() };
            let tips = [crate::tr("Aktuell").to_string(), format!("{left} | {} ({})", crate::tr("Aktuell"), key("compare")), format!("{left} ({})", key("before")), lr::with_shortcut("Kamera-JPEG", "camera_jpeg"), compare_tip];
            let icons: &[lr::Icon] = if darkroom { &[lr::Icon::Image, lr::Icon::SplitH, lr::Icon::Before, lr::Icon::Camera] } else { &[lr::Icon::Image, lr::Icon::SplitH, lr::Icon::Before, lr::Icon::Camera, lr::Icon::SquaresFour] };
            let cells: Vec<_> = icons
                .iter()
                .enumerate()
                .map(|(n, ic)| (Cell::Symbol(*ic, if n == 4 && !can_compare { Color32::from_gray(0x60) } else { lr::TEXT() }), n == view, tips[n].as_str()))
                .collect();
            let view_click = if has(Group::View) {
                let r = ui.add_enabled_ui(loaded, |ui| {
                    if !loaded {
                        ui.multiply_opacity(0.35);
                    }
                    lr::cell_group(ui, "ansicht", &cells, 30.0)
                });
                widths.push((Group::View.key(), r.response.rect.width()));
                r.inner
            } else {
                None
            };
            if view_click == Some(4) {
                if comparing_photos {
                    self.compare_close_back();
                } else if can_compare {
                    self.compare_open_with(Default::default());
                }
            } else if let Some(v) = view_click {
                if comparing_photos {
                    self.compare_close();
                }
                if darkroom && (v == 1) != darkroom_comparing {
                    self.darkroom_choose_view(1);
                }
                if v != view {
                    self.compare = v == 1 && !darkroom;
                    self.show_before = v == 2;
                    if v == 1 || v == 2 {
                        self.end_look_compare();
                    }
                    if (v == 3) != self.show_camera_jpeg {
                        self.toggle_camera_jpeg();
                    }
                }
            }
            let flag = self.entries[i].meta.flag;
            let rating = self.entries[i].meta.rating;
            let label = self.entries[i].meta.label;
            let color = |e: u8| { let c = library::label::COLORS[e as usize]; Color32::from_rgb(c[0], c[1], c[2]) };
            let names: Vec<String> = [crate::tr("Etikett Rot"), crate::tr("Etikett Gelb"), crate::tr("Etikett Grün"), crate::tr("Etikett Blau"), crate::tr("Etikett Lila")]
                .iter()
                .enumerate()
                .map(|(i, n)| if i < 4 { format!("{n} ({})", i + 6) } else { n.to_string() })
                .collect();
            let (t_pick, t_rej) = (lr::with_shortcut("Als Auswahl markieren", "pick"), lr::with_shortcut("Als abgelehnt markieren", "reject"));
            if has(Group::Flag) {
                let cells = [
                    (Cell::Flag(1), flag == sidecar::Flag::Pick, t_pick.as_str()),
                    (Cell::Flag(-1), flag == sidecar::Flag::Reject, t_rej.as_str()),
                ];
                let (c, r) = lr::cell_group_rect(ui, "markierung", &cells, 30.0);
                widths.push((Group::Flag.key(), r.width()));
                match c {
                    Some(0) => self.toolbar_flag(flag, sidecar::Flag::Pick),
                    Some(_) => self.toolbar_flag(flag, sidecar::Flag::Reject),
                    None => {}
                }
            }
            if has(Group::Stars) {
                let star_tips: Vec<String> = (1..=5u8).map(|n| format!("{} ({n})", if n == 1 { crate::tr("1 Stern").to_string() } else { crate::trf!("{n} Sterne", n = n) })).collect();
                let cells: Vec<_> = (1..=5u8)
                    .map(|n| {
                        let an = n <= rating;
                        (Cell::Text(if an { "★" } else { "☆" }, 14.0, if an { Color32::from_rgb(0xe8, 0xc5, 0x5a) } else { Color32::from_gray(0x8a) }), false, star_tips[n as usize - 1].as_str())
                    })
                    .collect();
                let (c, r) = lr::cell_group_rect(ui, "sterne", &cells, 22.0);
                widths.push((Group::Stars.key(), r.width()));
                if let Some(k) = c {
                    let n = k as u8 + 1;
                    self.rate(rating::Change::Stars(if rating == n { 0 } else { n }), false);
                }
            }
            if has(Group::Labels) {
                let cells: Vec<_> = (1..=5u8).map(|e| (Cell::Color(color(e)), label == e, names[e as usize - 1].as_str())).collect();
                let (c, r) = lr::cell_group_rect(ui, "etiketten", &cells, 22.0);
                widths.push((Group::Labels.key(), r.width()));
                if let Some(k) = c {
                    let e = k as u8 + 1;
                    self.rate(rating::Change::Label(if label == e { 0 } else { e }), false);
                }
            }
            let left_end = ui.cursor().left();
            let right = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                let zoom = ui.add_enabled_ui(loaded, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if !loaded {
                        ui.multiply_opacity(0.35);
                    }
                    self.zoom_menu(ui);
                });
                // measured: the menu alone, and the − / + buttons as their own group
                let zw = zoom.response.rect.width();
                if self.zoom_narrow {
                    // the narrow width is fixed, nothing to learn
                } else if self.zoom_compact {
                    widths.push(("zoom-menu", zw));
                } else {
                    widths.push(("zoom-menu", (zw - ZOOM_BUTTONS_W).max(60.0)));
                    widths.push((Group::ZoomButtons.key(), ZOOM_BUTTONS_W));
                }
                if has(Group::Rotate) {
                    let (t_l, t_r) = (lr::with_shortcut("Nach links drehen", "rotate_left"), lr::with_shortcut("Nach rechts drehen", "rotate_right"));
                    let cells = [(Cell::Symbol(lr::Icon::RotCcw, lr::TEXT()), false, t_l.as_str()), (Cell::Symbol(lr::Icon::RotCw, lr::TEXT()), false, t_r.as_str())];
                    let r = ui.add_enabled_ui(loaded, |ui| {
                        if !loaded {
                            ui.multiply_opacity(0.35);
                        }
                        lr::cell_group(ui, "drehen", &cells, 30.0)
                    });
                    widths.push((Group::Rotate.key(), r.response.rect.width()));
                    match r.inner {
                        Some(0) => self.rotate_image(false),
                        Some(_) => self.rotate_image(true),
                        None => {}
                    }
                }
                // Conditions of this photo (damaged, Sigma-exact unavailable …): a yellow mark; a click explains them.
                let before = ui.cursor().right();
                help::notice_mark(ui, "werkzeugleiste", &self.entries[i].name, &notices, photo_state::YELLOW);
                widths.push(("notice", (before - ui.cursor().right() - GAP).max(0.0)));
                help::test_card(ui.ctx());
                if has(Group::Overlays) {
                    let has_hdr = self.toolbar_has_hdr();
                    let t_info = lr::with_shortcut("Bildinfos im Bild: kurz → ausführlich → aus", "info_overlay");
                    let t_clip = lr::with_shortcut("Clipping-Warnung", "clipping");
                    let t_hdr = if has_hdr { "HDR-Anzeige" } else { "" };
                    let t_focus = lr::with_shortcut("Fokusfeld der Kamera zeigen", "focus_frame");
                    let cells = [
                        (Cell::Symbol(lr::Icon::Smiley, lr::TEXT()), self.show_faces, "Gesichter anzeigen"),
                        (Cell::Symbol(lr::Icon::Crosshair, lr::TEXT()), self.show_focus, t_focus.as_str()),
                        (Cell::Symbol(lr::Icon::Warning, lr::TEXT()), self.show_clipping, t_clip.as_str()),
                        (Cell::Symbol(lr::Icon::Sun, if has_hdr { lr::TEXT() } else { Color32::from_gray(0x50) }), has_hdr && self.display.hdr_on, t_hdr),
                        (Cell::Symbol(lr::Icon::Info, lr::TEXT()), self.info_level > 0, t_info.as_str()),
                    ];
                    let (c, r) = lr::cell_group_rect(ui, "anzeigen", &cells, 30.0);
                    widths.push((Group::Overlays.key(), r.width()));
                    match c {
                        Some(0) => self.show_faces = !self.show_faces,
                        Some(1) => self.toggle_focus(),
                        Some(2) => self.show_clipping = !self.show_clipping,
                        Some(3) if has_hdr => self.display.hdr_on = !self.display.hdr_on,
                        Some(4) => self.info_next(),
                        _ => {}
                    }
                }
                if has(Group::Darkroom) {
                    let r = ui.horizontal(|ui| self.darkroom_tools(ui));
                    widths.push((Group::Darkroom.key(), r.response.rect.width()));
                }
                if !hidden.is_empty() {
                    self.toolbar_more(ui, i, &hidden);
                }
            });
            self.toolbar_gap = right.response.rect.left() - left_end;
            for (k, w) in widths {
                self.toolbar_widths.insert(k, w);
            }
            if darkroom {
                self.darkroom.top_gap = self.toolbar_gap;
            }
        });
    }

    fn toolbar_has_hdr(&self) -> bool {
        self.current.as_ref().is_some_and(|c| c.loaded.previews.foreign.as_ref().is_some_and(|d| d.hdr.is_some())) && self.display.hdr_possible != Some(false)
    }

    fn toolbar_flag(&mut self, flag: sidecar::Flag, wanted: sidecar::Flag) {
        let f = if flag == wanted { sidecar::Flag::None } else { wanted };
        self.rate(rating::Change::Flag(f.as_i8()), false);
    }

    /// "MEHR": the hidden groups, one function per row with symbol and name; switched-on toggles get a check mark.
    fn toolbar_more(&mut self, ui: &mut egui::Ui, i: usize, hidden: &[toolbar_plan::Group]) {
        use toolbar_plan::Group;
        let resp = ui.add(egui::Button::new(crate::tr("MEHR")).min_size(egui::vec2(toolbar_plan::MORE_W, lr::BAR_H)).fill(lr::GROUP_FILL));
        egui::Popup::menu(&resp).align(egui::RectAlign::TOP_START).show(|ui| {
            ui.set_min_width(230.0);
            let item = |ui: &mut egui::Ui, icon: Option<lr::Icon>, swatch: Option<Color32>, name: &str, on: bool| -> bool {
                let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 0.0, Color32::from_gray(0x36));
                }
                let icon_rect = egui::Rect::from_center_size(egui::pos2(r.left() + 14.0, r.center().y), egui::vec2(15.0, 15.0));
                if let Some(ic) = icon {
                    lr::paint_icon(ui.painter(), icon_rect, ic, lr::TEXT());
                }
                if let Some(c) = swatch {
                    ui.painter().rect_filled(icon_rect.shrink(2.0), 0.0, c);
                }
                ui.painter().text(egui::pos2(r.left() + 30.0, r.center().y), egui::Align2::LEFT_CENTER, name, egui::TextStyle::Body.resolve(ui.style()), lr::TEXT());
                if on {
                    ui.painter().text(egui::pos2(r.right() - 10.0, r.center().y), egui::Align2::RIGHT_CENTER, "✓", egui::TextStyle::Body.resolve(ui.style()), lr::TEXT());
                }
                resp.clicked()
            };
            let meta = self.entries[i].meta.clone();
            for g in hidden {
                match g {
                    Group::Flag => {
                        if item(ui, Some(lr::Icon::CheckCircle), None, crate::tr("Als Auswahl markieren"), meta.flag == sidecar::Flag::Pick) {
                            self.toolbar_flag(meta.flag, sidecar::Flag::Pick);
                        }
                        if item(ui, Some(lr::Icon::XCircle), None, crate::tr("Als abgelehnt markieren"), meta.flag == sidecar::Flag::Reject) {
                            self.toolbar_flag(meta.flag, sidecar::Flag::Reject);
                        }
                    }
                    Group::Stars => {
                        for n in 1..=5u8 {
                            if item(ui, Some(lr::Icon::Star), None, &crate::trf!("{n} Sterne", n = n), meta.rating == n) {
                                self.rate(rating::Change::Stars(if meta.rating == n { 0 } else { n }), false);
                            }
                        }
                    }
                    Group::Labels => {
                        let names = [crate::tr("Etikett Rot"), crate::tr("Etikett Gelb"), crate::tr("Etikett Grün"), crate::tr("Etikett Blau"), crate::tr("Etikett Lila")];
                        for e in 1..=5u8 {
                            let c = library::label::COLORS[e as usize];
                            if item(ui, None, Some(Color32::from_rgb(c[0], c[1], c[2])), names[e as usize - 1], meta.label == e) {
                                self.rate(rating::Change::Label(if meta.label == e { 0 } else { e }), false);
                            }
                        }
                    }
                    Group::Rotate => {
                        if item(ui, Some(lr::Icon::RotCcw), None, crate::tr("Nach links drehen"), false) {
                            self.rotate_image(false);
                        }
                        if item(ui, Some(lr::Icon::RotCw), None, crate::tr("Nach rechts drehen"), false) {
                            self.rotate_image(true);
                        }
                    }
                    Group::Overlays => {
                        if item(ui, Some(lr::Icon::Smiley), None, crate::tr("Gesichter anzeigen"), self.show_faces) {
                            self.show_faces = !self.show_faces;
                        }
                        if item(ui, Some(lr::Icon::Crosshair), None, crate::tr("Fokusfeld der Kamera zeigen"), self.show_focus) {
                            self.toggle_focus();
                        }
                        if item(ui, Some(lr::Icon::Warning), None, crate::tr("Clipping-Warnung"), self.show_clipping) {
                            self.show_clipping = !self.show_clipping;
                        }
                        if self.toolbar_has_hdr() && item(ui, Some(lr::Icon::Sun), None, crate::tr("HDR-Anzeige"), self.display.hdr_on) {
                            self.display.hdr_on = !self.display.hdr_on;
                        }
                        if item(ui, Some(lr::Icon::Info), None, crate::tr("Bildinfos im Bild"), self.info_level > 0) {
                            self.info_next();
                        }
                    }
                    Group::Darkroom => self.darkroom_tools_menu(ui, &item),
                    Group::View => {
                        let comparing = self.compare || (self.darkroom_active() && self.darkroom.compare.mode != crate::darkroom_compare::Mode::Off);
                        if item(ui, Some(lr::Icon::SplitH), None, crate::tr("Vergleichen"), comparing) {
                            if self.darkroom_active() {
                                self.darkroom_choose_view(1);
                            } else {
                                self.compare = !self.compare;
                                self.show_before = false;
                                self.end_look_compare();
                            }
                        }
                        if !self.darkroom_active() && item(ui, Some(lr::Icon::Before), None, crate::tr("Vorher"), self.show_before) {
                            self.show_before = !self.show_before;
                            self.compare = false;
                        }
                    }
                    Group::ZoomButtons => {
                        if item(ui, Some(lr::Icon::ZoomOut), None, crate::tr("Einpassen"), self.zoom <= 1.001) {
                            self.set_zoom(1.0);
                            self.pan = egui::Vec2::ZERO;
                        }
                        if item(ui, Some(lr::Icon::ZoomIn), None, "100 %", (self.zoom - self.one_to_one).abs() < 0.01) {
                            self.set_zoom(self.one_to_one.max(1.0));
                        }
                    }
                }
                ui.separator();
            }
        });
    }

    /// Zoom in the toolbar: − / percent menu / + (as in x3fuse). The percentages
    /// refer to 100 % = 1 image pixel per screen pixel.
    fn zoom_menu(&mut self, ui: &mut egui::Ui) {
        let comparing = self.compare_shown();
        self.zoom_menu_in(ui, comparing);
    }

    /// The zoom group; `comparing`: the comparison's own toolbar draws it (it holds the comparison taken out of the
    /// library meanwhile, so `compare_shown()` would say no – QA 5: "100 %" while every field was being computed).
    pub(crate) fn zoom_menu_in(&mut self, ui: &mut egui::Ui, comparing: bool) {
        use lr::Cell;
        let o = self.one_to_one.max(1e-3);
        let pct = self.zoom / o * 100.0;
        let fit_pct = 100.0 / o;
        let steps = self.zoom_steps();
        let to = |app: &mut Self, p: Option<f32>| match p {
            None => {
                app.set_zoom(1.0);
                app.pan = egui::Vec2::ZERO;
            }
            Some(p) => app.set_zoom((o * p / 100.0).max(1.0)),
        };
        // right to left: [fit − +] | menu – menu and buttons as two fields 4 px apart (fit is its own
        // button, highlighted while the photo is fit; the menu shows only the percentage at a fixed width)
        let fit_tip = lr::with_shortcut("Einpassen", "zoom");
        let (out_tip, in_tip) = (lr::with_shortcut("Verkleinern", "image_zoom_out"), lr::with_shortcut("Vergrößern", "image_zoom_in"));
        let cells = [(Cell::Symbol(lr::Icon::FrameCorners, lr::TEXT()), self.zoom <= 1.001, fit_tip.as_str()), (Cell::Symbol(lr::Icon::ZoomOut, lr::TEXT()), false, out_tip.as_str()), (Cell::Symbol(lr::Icon::ZoomIn, lr::TEXT()), false, in_tip.as_str())];
        let zoom_tip = lr::with_shortcut("Einpassen ↔ 100 %", "zoom");
        match if self.zoom_compact { None } else { lr::cell_group(ui, "zoom", &cells, 30.0) } {
            Some(0) => self.zoom_step(0),
            Some(1) => self.zoom_step(-1),
            Some(_) => self.zoom_step(1),
            None => {}
        }
        if !self.zoom_compact {
            ui.add_space(-6.0);
        }
        // the comparison: fitted, the focused field's own scale ("–" while it is being computed)
        let label = if comparing && self.zoom <= 1.001 { self.cmp.fit_pct.map_or_else(|| "– %".to_string(), |p| format!("{p:.0} %")) } else { zoom_label(self.zoom <= 1.001, fit_pct, pct) };
        ui.scope(|ui| {
            lr::field_style(ui);
            let r = egui::ComboBox::from_id_salt("zoom-menu").selected_text(label).width(if self.zoom_narrow { NARROW_ZOOM_W - 12.0 } else { zoom_field_width(ui) }).show_ui(ui, |ui| {
                if ui.selectable_label(self.zoom <= 1.001, crate::trf!("Einpassen ({a} %)", a = format!("{fit_pct:.0}"))).clicked() {
                    to(self, None);
                }
                for p in &steps {
                    if ui.selectable_label((pct - p).abs() < 0.5, format!("{p:.0} %")).clicked() {
                        to(self, Some(*p));
                    }
                }
            });
            lr::hover_tip(ui, &r.response, &zoom_tip);
        });
    }

    /// Crop mode: frame with rule-of-thirds grid and handles. Dragging inside moves, at corners
    /// and edges resizes, outside rotates.
    fn crop_overlay(&mut self, ui: &egui::Ui, resp: &egui::Response, painter: &egui::Painter, rect: egui::Rect, tex: egui::TextureId) {
        let Some(c) = &self.current else { return };
        let (index, [fw, fh]) = (c.index, c.loaded.full_size);
        let s = &self.entries[index].settings;
        let crop = Frame::from_settings(s, fw, fh, c.loaded.cal.orientation);
        let (ow, oh) = crop.oriented();
        // uprighted image ↔ screen (the whole image is fit, rotated back by the fine angle: the frame stays level)
        let view_rect = ViewTransform::new(rect.center(), crop.angle);
        let scr0 = |p: [f32; 2]| rect.min + egui::vec2(p[0] / ow, p[1] / oh) * rect.size();
        let scr = |p: [f32; 2]| view_rect.forward(scr0(p));
        let src = |q: egui::Pos2| {
            let u = (view_rect.back(q) - rect.min) / rect.size();
            [u.x * ow, u.y * oh]
        };
        let corners = crop.corners_ori().map(scr);
        // Crop bright over the darkened image (with perspective as a fine grid)
        let n = if crop.has_perspective() { 24 } else { 1 };
        let mut mesh = egui::Mesh::with_texture(tex);
        for j in 0..=n {
            for i in 0..=n {
                let u = [i as f32 / n as f32, j as f32 / n as f32];
                let q = crop.to_src(u);
                mesh.vertices.push(egui::epaint::Vertex { pos: scr(crop.to_ori(u)), uv: egui::pos2(q[0] / fw as f32, q[1] / fh as f32), color: Color32::WHITE });
            }
        }
        for j in 0..n {
            for i in 0..n {
                let a = (j * (n + 1) + i) as u32;
                let b = a + n as u32 + 1;
                mesh.add_triangle(a, a + 1, b + 1);
                mesh.add_triangle(a, b + 1, b);
            }
        }
        painter.add(egui::Shape::mesh(mesh));
        let at = |u: [f32; 2]| scr(crop.to_ori(u));
        let now = ui.input(|i| i.time);
        if now < self.persp_grid_until || self.geo_tool == align::GeoTool::Perspective {
            // Guide grid while adjusting perspective/angle:
            // square cells over the whole crop, for comparison with edges in the image
            let nx = 12usize;
            let ny = ((nx as f32 * crop.h / crop.w).round() as usize).max(2);
            let stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(70));
            for k in 1..nx {
                let t = k as f32 / nx as f32;
                painter.line_segment([at([t, 0.0]), at([t, 1.0])], stroke);
            }
            for k in 1..ny {
                let t = k as f32 / ny as f32;
                painter.line_segment([at([0.0, t]), at([1.0, t])], stroke);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            // selected grid (thirds, golden ratio, diagonals)
            for (a, b) in self.crop_raster.lines() {
                painter.line_segment([at(a), at(b)], egui::Stroke::new(1.0, Color32::from_white_alpha(90)));
            }
        }
        painter.add(egui::Shape::closed_line(corners.to_vec(), egui::Stroke::new(1.5, Color32::WHITE)));
        let handles: [([f32; 2], [f32; 2]); 8] = [
            ([0.0, 0.0], [-1.0, -1.0]),
            ([1.0, 0.0], [1.0, -1.0]),
            ([1.0, 1.0], [1.0, 1.0]),
            ([0.0, 1.0], [-1.0, 1.0]),
            ([0.5, 0.0], [0.0, -1.0]),
            ([1.0, 0.5], [1.0, 0.0]),
            ([0.5, 1.0], [0.0, 1.0]),
            ([0.0, 0.5], [-1.0, 0.0]),
        ];
        // grip zones (screen points): a corner wins over an edge; an edge grips along its whole length
        let grip = |p: egui::Pos2| -> Option<[f32; 2]> {
            if let Some((_, side)) = handles[..4].iter().find(|(u, _)| at(*u).distance(p) < CROP_CORNER_GRIP) {
                return Some(*side);
            }
            let edges = [(0, 1, [0.0, -1.0]), (1, 2, [1.0, 0.0]), (2, 3, [0.0, 1.0]), (3, 0, [-1.0, 0.0])];
            edges.iter().find(|(a, b, _)| distance_to_segment(p, corners[*a], corners[*b]) < CROP_EDGE_GRIP).map(|e| e.2)
        };
        let hovered_grip = if self.crop_drag.is_none() { resp.hover_pos().and_then(grip) } else { None };
        let active_grip = match self.crop_drag {
            Some(CropDrag::Resize { side, .. }) => Some(side),
            _ => hovered_grip,
        };
        // the grip under the pointer (or being dragged) is highlighted: a thicker edge, a larger corner
        if let Some([sx, sy]) = active_grip {
            if sx != 0.0 && sy != 0.0 {
                painter.rect_filled(egui::Rect::from_center_size(at([(sx + 1.0) / 2.0, (sy + 1.0) / 2.0]), egui::vec2(13.0, 13.0)), 0.0, Color32::WHITE);
            } else {
                let (a, b) = if sx != 0.0 { let x = (sx + 1.0) / 2.0; ([x, 0.0], [x, 1.0]) } else { let y = (sy + 1.0) / 2.0; ([0.0, y], [1.0, y]) };
                painter.line_segment([at(a), at(b)], egui::Stroke::new(3.0, Color32::WHITE));
            }
        }
        for (u, side) in &handles {
            let corner = side[0] != 0.0 && side[1] != 0.0;
            let size = if corner { 9.0 } else { 7.0 };
            painter.rect_filled(egui::Rect::from_center_size(at(*u), egui::vec2(size, size)), 0.0, Color32::WHITE);
        }

        // Straighten: place a line along the horizon or an edge → angle. Two clicks
        // or dragging; with ⌘ held also without the tool selected. On the handles
        // of the frame, moving/resizing still applies.
        let cmd = ui.input(|i| i.modifiers.command);
        let near_handle = hovered_grip.is_some();
        if self.straighten_from.is_some() || ((self.straighten || cmd) && self.crop_drag.is_none() && !near_handle) {
            let text = if self.straighten_from.is_some() {
                crate::tr("Zweiten Punkt anklicken (oder Maustaste loslassen) · Esc verwirft die Linie")
            } else {
                crate::tr("Ausrichten: Punkt am Horizont oder an einer Kante anklicken, dann den zweiten (oder ziehen) · Esc bricht ab")
            };
            badge(painter, rect, text, None);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
            let mut just_set = false;
            if resp.drag_started() && self.straighten_from.is_none() {
                self.straighten_from = resp.interact_pointer_pos().map(src);
                self.straighten_drag = true;
                just_set = true;
            } else if resp.clicked() && self.straighten_from.is_none() {
                self.straighten_from = resp.interact_pointer_pos().map(src);
                self.straighten_drag = false;
                just_set = true;
            }
            if let (Some(a), Some(p)) = (self.straighten_from, resp.interact_pointer_pos().or(resp.hover_pos())) {
                let b = src(p);
                let long = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt() > 0.02 * ow.max(oh);
                let stroke = egui::Stroke::new(1.5, Color32::WHITE);
                painter.line_segment([scr(a), p], egui::Stroke::new(3.5, Color32::from_black_alpha(120)));
                painter.line_segment([scr(a), p], stroke);
                painter.circle_filled(scr(a), 3.0, Color32::WHITE);
                if long {
                    let w = align::line_angles(a, b);
                    let center = scr(a).lerp(p, 0.5) + egui::vec2(0.0, -12.0);
                    let g = painter.layout_no_wrap(format!("{w:+.1}°").de_comma(), egui::FontId::proportional(12.0), Color32::WHITE);
                    painter.rect_filled(egui::Rect::from_center_size(center, g.size() + egui::vec2(8.0, 4.0)), 0.0, Color32::from_black_alpha(170));
                    painter.galley(center - g.size() / 2.0, g, Color32::WHITE);
                }
                let done = !just_set && ((self.straighten_drag && resp.drag_stopped()) || (!self.straighten_drag && (resp.clicked() || resp.drag_stopped())));
                if done && long {
                    // The angle is the slope of the line in the undistorted image (where the frame is unrotated)
                    let angle = align::line_angles(a, b);
                    let next = Frame { angle, ..crop }.constrain();
                    let (c2, a2) = next.to_settings();
                    let s = &mut self.entries[index].settings;
                    s.crop = c2;
                    s.crop_angle = a2;
                    self.persp_grid_until = ui.input(|i| i.time) + 0.8;
                    self.upright_msg = Some((crate::trf!("Begradigt: {a2}°", a2 = format!("{:+.2}", a2)), ui.input(|i| i.time)));
                    self.straighten_from = None;
                    self.straighten_drag = false;
                    self.straighten = false;
                } else if done {
                    // too short (e.g. almost the same spot twice): the line stays open until a sensible point comes
                    self.straighten_drag = false;
                }
                ui.ctx().request_repaint();
            }
            return;
        }

        // Interaction
        let hover = resp.hover_pos();
        let hit = |p: egui::Pos2| -> CropDrag {
            if let Some(side) = grip(p) {
                return CropDrag::Resize { side, start: crop };
            }
            let l = crop.from_ori(src(p));
            if (0.0..=1.0).contains(&l[0]) && (0.0..=1.0).contains(&l[1]) {
                CropDrag::Move { start: crop, from: src(p) }
            } else {
                let m = scr([crop.cx, crop.cy]);
                CropDrag::Rotate { start: crop, from: [p.x, p.y], center: [m.x, m.y] }
            }
        };
        if let Some(p) = hover.filter(|_| self.crop_drag.is_none()) {
            let cursor = match hit(p) {
                CropDrag::Resize { side: [sx, sy], .. } if sx * sy > 0.0 => egui::CursorIcon::ResizeNwSe,
                CropDrag::Resize { side: [sx, sy], .. } if sx * sy < 0.0 => egui::CursorIcon::ResizeNeSw,
                CropDrag::Resize { side: [sx, _], .. } if sx != 0.0 => egui::CursorIcon::ResizeHorizontal,
                CropDrag::Resize { .. } => egui::CursorIcon::ResizeVertical,
                CropDrag::Move { .. } => egui::CursorIcon::Grab,
                CropDrag::Rotate { center, .. } => {
                    rotate_cursor(painter, p, egui::pos2(center[0], center[1]));
                    egui::CursorIcon::None
                }
            };
            ui.ctx().set_cursor_icon(cursor);
        }
        // double click into the crop: done (like Enter)
        if resp.double_clicked() && resp.interact_pointer_pos().is_some_and(|p| matches!(hit(p), CropDrag::Move { .. })) {
            self.toggle_crop(true);
            return;
        }
        if resp.drag_started() {
            // where the button went down decides, not where the pointer is once the drag registers (a few points later):
            // otherwise a grip pressed exactly at the corner was often missed
            let origin = ui.input(|i| i.pointer.press_origin());
            self.crop_drag = origin.or(resp.interact_pointer_pos()).map(hit);
        }
        if resp.drag_stopped() {
            self.crop_drag = None;
        }
        let (Some(drag), Some(p)) = (self.crop_drag, resp.interact_pointer_pos().filter(|_| resp.dragged())) else { return };
        let q = src(p);
        let aspect = self.crop_aspect.or_else(|| match drag {
            // Shift while resizing: aspect ratio is kept
            CropDrag::Resize { start, .. } if ui.input(|i| i.modifiers.shift) => Some(start.w / start.h),
            _ => None,
        });
        let mut new_format: Option<f32> = None;
        if let CropDrag::Rotate { center, .. } = drag {
            // While rotating, the frame stays level, with a fine grid and angle display
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            rotate_cursor(painter, p, egui::pos2(center[0], center[1]));
            self.persp_grid_until = ui.input(|i| i.time) + 0.4;
        }
        let next = match drag {
            CropDrag::Move { start, from } => Frame { cx: start.cx + q[0] - from[0], cy: start.cy + q[1] - from[1], ..start },
            CropDrag::Rotate { start, from, center } => {
                // on screen around the centre of the frame: dragging clockwise rotates the image clockwise
                let center = egui::pos2(center[0], center[1]);
                let a0 = (from[1] - center.y).atan2(from[0] - center.x);
                let a1 = (p.y - center.y).atan2(p.x - center.x);
                let mut d = (a1 - a0).to_degrees();
                if d > 180.0 {
                    d -= 360.0;
                } else if d < -180.0 {
                    d += 360.0;
                }
                Frame { angle: (start.angle - d).clamp(-45.0, 45.0), ..start }
            }
            CropDrag::Resize { side: [sx, sy], start } => {
                // in the rotated frame: the opposite side/corner stays fixed
                let l = start.from_ori(q);
                let (lx, ly) = ((l[0] - 0.5) * start.w, (l[1] - 0.5) * start.h);
                let (fx, fy) = (-sx * start.w / 2.0, -sy * start.h / 2.0);
                let mut w = if sx != 0.0 { (sx * (lx - fx)).max(32.0) } else { start.w };
                let mut h = if sy != 0.0 { (sy * (ly - fy)).max(32.0) } else { start.h };
                if let Some(mut r) = aspect {
                    // fixed ratio: dragging a corner beyond the square flips between landscape and portrait
                    if self.crop_aspect.is_some() && sx != 0.0 && sy != 0.0 {
                        r = if w >= h { r.max(1.0 / r) } else { r.min(1.0 / r) };
                        new_format = Some(r);
                    }
                    if sx != 0.0 && (sy == 0.0 || w / r >= h) {
                        h = w / r;
                    } else {
                        w = h * r;
                    }
                }
                // new centre in the local frame, back to the source image
                let cxl = if sx != 0.0 { fx + sx * w / 2.0 } else { 0.0 };
                let cyl = if sy != 0.0 { fy + sy * h / 2.0 } else { 0.0 };
                let tmp = Frame { w: start.w, h: start.h, ..start };
                let center = tmp.to_ori([cxl / start.w + 0.5, cyl / start.h + 0.5]);
                Frame { cx: center[0], cy: center[1], w, h, ..start }
            }
        };
        let next = next.constrain();
        let (crop, angle) = next.to_settings();
        let s = &mut self.entries[index].settings;
        s.crop = crop;
        s.crop_angle = angle;
        if let Some(r) = new_format.filter(|r| self.crop_aspect.is_some_and(|a| (a - r).abs() > 1e-4)) {
            self.crop_aspect = Some(r);
        }
        if matches!(drag, CropDrag::Rotate { .. }) {
            let t = format!("{:+.1}°", angle).de_comma();
            let g = painter.layout_no_wrap(t, egui::FontId::proportional(12.0), Color32::WHITE);
            let pos = p + egui::vec2(16.0, 14.0);
            painter.rect_filled(egui::Rect::from_min_size(pos - egui::vec2(4.0, 2.0), g.size() + egui::vec2(8.0, 4.0)), 0.0, Color32::from_black_alpha(170));
            painter.galley(pos, g, Color32::WHITE);
        }
    }

    /// Open/close crop mode (R). `keep = false` restores the previous crop.
    fn toggle_crop(&mut self, keep: bool) {
        let Some(i) = self.current.as_ref().map(|c| c.index) else { return };
        match self.crop_mode.take() {
            Some((crop, angle, [pv, ph])) => {
                if !keep {
                    let s = &mut self.entries[i].settings;
                    s.crop = crop;
                    s.crop_angle = angle;
                    s.perspective_v = pv;
                    s.perspective_h = ph;
                    if let Some(r) = self.crop_rotate_start {
                        s.rotate = r;
                    }
                }
                self.crop_rotate_start = None;
            }
            None => {
                let s = &self.entries[i].settings;
                self.crop_mode = Some((s.crop, s.crop_angle, [s.perspective_v, s.perspective_h]));
                self.crop_rotate_start = Some(s.rotate);
                self.geo_tool = align::GeoTool::Crop;
                self.upright_msg = None;
                self.grid = false;
                self.tab = Tab::Develop;
            }
        }
        self.crop_drag = None;
        self.straighten = false;
        self.straighten_from = None;
        self.straighten_drag = false;
    }

    /// Select a tool in crop mode; "Straighten" is immediately armed for the horizon line.
    fn set_geo_tool(&mut self, tool: align::GeoTool) {
        self.geo_tool = tool;
        self.straighten = tool == align::GeoTool::Align;
        self.straighten_from = None;
        self.straighten_drag = false;
    }

    /// Open crop mode (if still closed) and select the tool.
    fn open_geo_tool(&mut self, tool: align::GeoTool) {
        if self.crop_mode.is_none() {
            self.toggle_crop(true);
        }
        if self.crop_mode.is_some() {
            self.set_geo_tool(tool);
        }
    }

    /// Rotate the image in 90° steps; the crop rotates with it.
    fn rotate_image(&mut self, clockwise: bool) {
        let Some(i) = self.current.as_ref().map(|c| c.index) else { return };
        // the crop turns with it (see `library_turn::turn_settings`)
        library_turn::turn_settings(&mut self.entries[i].settings, clockwise);
    }

    /// Set the aspect ratio and adapt the crop to it (centre stays).
    fn set_crop_aspect(&mut self, aspect: Option<f32>) {
        self.crop_aspect = aspect;
        let Some(c) = &self.current else { return };
        let (i, [fw, fh]) = (c.index, c.loaded.full_size);
        let Some(r) = aspect else { return };
        let cam = c.loaded.cal.orientation;
        let f = Frame::from_settings(&self.entries[i].settings, fw, fh, cam);
        let area = f.w * f.h;
        let w = (area * r).sqrt();
        let f = Frame { w, h: w / r, ..f }.constrain();
        let (crop, angle) = f.to_settings();
        self.entries[i].settings.crop = crop;
        self.entries[i].settings.crop_angle = angle;
    }

    /// Small previews of "Film & Look" (Sigma's colour modes, camera looks, films) for the current photo and settings
    /// state: returns the textures shown now and requests the `requested` ones that are missing (see film_previews.rs).
    fn profile_thumbs(&mut self, ui: &egui::Ui, requested: &[String]) -> HashMap<String, TextureHandle> {
        self.film_previews_request(ui, requested, &[])
    }

    /// Like [`Self::profile_thumbs`]; afterwards the tiles of `fill` are computed quietly in the background (darkroom
    /// film shelf: the rows not scrolled to yet).
    pub(crate) fn film_previews_request(&mut self, ui: &egui::Ui, requested: &[String], fill: &[String]) -> HashMap<String, TextureHandle> {
        let Some(c) = &self.current else { return HashMap::new() };
        let index = c.index;
        let settings = &self.entries[index].settings;
        let path = self.entries[index].path.clone();
        let key = film_previews::state_key(&path, settings);
        let now = ui.input(|i| i.time);
        // no tile from a fast preview too rough to be shown: nothing is asked for until the exact base is there
        let hold = self.holds_fast_preview(c, settings);
        let (requested, fill): (&[String], &[String]) = if hold { (&[], &[]) } else { (requested, fill) };
        let plan = self.film_previews.plan(index, &path, key, requested, fill, now);
        for (modes, background) in [(plan.front, false), (plan.back, true)] {
            if !modes.is_empty() {
                self.worker.send(Job::FilmThumbs { index, previews: c.loaded.previews.clone(), cal: c.loaded.cal.clone(), settings: settings.clone(), key, modes, valid: self.film_previews.valid(), background });
            }
        }
        if plan.busy || hold {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.film_previews.shown()
    }

    /// Panel "Crop & Straighten": at the top the icon row of the tools (crop,
    /// straighten, perspective), below it the settings of the selected tool.
    fn crop_panel(&mut self, ui: &mut egui::Ui) {
        use align::GeoTool;
        let Some(c) = &self.current else { return };
        let (index, [fw, fh]) = (c.index, c.loaded.full_size);
        let cam = c.loaded.cal.orientation;
        let active = self.crop_mode.is_some();
        let s = &self.entries[index].settings;
        let changed = s.crop != Settings::default().crop || s.crop_angle != 0.0 || s.perspective_v != 0.0 || s.perspective_h != 0.0;
        let mut toggle = false;
        let mut cancel = false;
        let mut aspect: Option<Option<f32>> = None;
        let mut reset = false;
        let mut angle = s.crop_angle as f64;
        let mut angle_changed = false;
        let mut pv = s.perspective_v as f64;
        let mut ph = s.perspective_h as f64;
        let mut persp_changed = false;
        let mut persp_reset = false;
        let mut upright: Option<develop::upright::Mode> = None;
        let straighten = self.straighten;
        let mut straighten_click = false;
        let mut straighten_free = false;
        let mut open_tool: Option<GeoTool> = None;
        let mut rotate90: Option<bool> = None;
        let mut raster = self.crop_raster;
        let tool = self.geo_tool;
        let now = ui.input(|i| i.time);
        let msg = self.upright_msg.clone().filter(|(_, t)| now - t < 6.0).map(|(m, _)| m);
        let (ow, oh) = Frame::full(fw, fh, geometry::quarters(s, cam)).oriented();
        let original = ow / oh;
        let current_aspect = self.crop_aspect;
        let reset_hit = lr::panel(ui, |ui| {
            // Tools as an icon row, like the white balance presets; a click opens it and shows its settings below
            let tools = [GeoTool::Crop, GeoTool::Align, GeoTool::Perspective];
            let items = [
                (lr::Icon::Crop, crate::tr("Freistellen"), crate::tr("Ausschnitt und Format (R)")),
                // "Ausrichten" alone means aligning exposures or fields elsewhere ("Align"); here it is the horizon
                (lr::Icon::Ruler, crate::menu::i18n::german_or("Ausrichten", crate::tr("Ausrichten (Horizont)")), crate::tr("Horizont begradigen (A)")),
                (lr::Icon::Perspective, crate::tr("Perspektive"), crate::tr("Stürzende Linien entzerren")),
            ];
            let selected = active.then(|| tools.iter().position(|t| *t == tool)).flatten();
            if let Some(i) = lr::icon_grid(ui, "geo-tool", &items, selected, 3) {
                open_tool = Some(tools[i]);
            }
            ui.add_space(4.0);
            if !active {
                if changed {
                    let f = Frame::from_settings(s, fw, fh, cam);
                    let persp = if f.has_perspective() { crate::tr(" · Perspektive") } else { "" };
                    ui.label(egui::RichText::new(format!("{:.0} × {:.0} px · {:+.1}°{persp}", f.w, f.h, f.angle)).small().color(lr::TEXT_DIM()));
                }
                return;
            }
            match tool {
                GeoTool::Crop => {
                    // Aspect ratios as a grid (given in landscape), orientation via landscape/portrait
                    let norm = |r: f32| if r >= 1.0 { r } else { 1.0 / r };
                    let landscape = current_aspect.map_or(original >= 1.0, |a| a >= 1.0);
                    let presets: [(Option<f32>, &str); 10] = [
                        (Some(norm(original)), crate::tr("Original")),
                        (None, crate::tr("Frei")),
                        (Some(1.0), "1:1"),
                        (Some(5.0 / 4.0), "5:4"),
                        (Some(7.0 / 6.0), "7:6"),
                        (Some(7.0 / 5.0), "7:5"),
                        (Some(3.0 / 2.0), "3:2"),
                        (Some(16.0 / 9.0), "16:9"),
                        (Some(21.0 / 9.0), "21:9"),
                        (Some(65.0 / 24.0), "XPan"),
                    ];
                    let selected = presets.iter().position(|(r, _)| match (r, current_aspect) {
                        (None, None) => true,
                        (Some(a), Some(b)) => (a - norm(b)).abs() < 1e-3,
                        _ => false,
                    });
                    ui.horizontal(|ui| {
                        lr::field_label(ui, crate::tr("Format"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut l = landscape;
                            if lr::orientation_toggle(ui, &mut l) {
                                if let Some(r) = current_aspect.or(Some(original)) {
                                    aspect = Some(Some(1.0 / r));
                                }
                            }
                            // Lock: hold the current aspect ratio or release it again
                            let locked = current_aspect.is_some();
                            let (icon, tip) = if locked { (lr::Icon::Lock, "Format gesperrt – klicken zum Freigeben") } else { (lr::Icon::LockOpen, "Format frei – klicken zum Sperren") };
                            if lr::icon_button_hint(ui, icon, locked, tip) {
                                aspect = Some(if locked {
                                    None
                                } else {
                                    let f = Frame::from_settings(s, fw, fh, cam);
                                    Some(f.w / f.h)
                                });
                            }
                        });
                    });
                    if let Some(i) = lr::aspect_grid(ui, &presets, selected, landscape) {
                        aspect = Some(presets[i].0.map(|r| if landscape || r == 1.0 { r } else { 1.0 / r }));
                    }
                    ui.add_space(2.0);
                    lr::group_label(ui, "Raster");
                    lr::segmented_pad(ui, &mut raster, &align::Raster::ALL, 8.0);
                    ui.add_space(4.0);
                    // Rotating and straightening belong to cropping: angle, draw line, 90° rotation
                    angle_changed = lr::slider_hint(ui, "Winkel", &mut angle, -45.0..=45.0, 0.0, 0.05, "Ausrichten in Grad – ⌥ beim Ziehen für Feinarbeit; auch außerhalb des Rahmens ziehen dreht das Bild");
                    ui.horizontal(|ui| {
                        lr::field_label(ui, crate::tr("Drehen"));
                        if lr::icon_button(ui, lr::Icon::RotCcw, false) {
                            rotate90 = Some(false);
                        }
                        if lr::icon_button(ui, lr::Icon::RotCw, false) {
                            rotate90 = Some(true);
                        }
                        if lr::icon_button_hint(ui, lr::Icon::Ruler, straighten, crate::tr("Horizont begradigen: Linie entlang einer Kante ziehen")) {
                            straighten_free = true;
                        }
                    });
                    ui.add_space(6.0);
                }
                GeoTool::Align => {
                    // Draw line: two clicks or drag; next to it automatic upright
                    use develop::upright::Mode;
                    let items = [
                        (lr::Icon::Ruler, crate::tr("Linie"), crate::tr("Zwei Punkte am Horizont oder an einer Kante anklicken (oder eine Linie ziehen) – daraus wird der Winkel")),
                        (lr::Icon::Auto, crate::tr("Auto"), crate::tr("Ausgewogen aufrichten: Winkel, stürzende Linien und gemäßigt waagerecht")),
                        (lr::Icon::ArrowsH, crate::tr("Waagerecht"), crate::tr("Nur den Winkel: Horizont und waagerechte Kanten gerade richten")),
                        (lr::Icon::ArrowsV, crate::tr("Senkrecht"), crate::tr("Winkel und stürzende Linien: senkrechte Kanten parallel stellen")),
                    ];
                    match lr::icon_grid(ui, "upright", &items, straighten.then_some(0), 4) {
                        Some(0) => straighten_click = true,
                        Some(1) => upright = Some(Mode::Auto),
                        Some(2) => upright = Some(Mode::Level),
                        Some(3) => upright = Some(Mode::Vertical),
                        _ => {}
                    }
                    ui.add_space(2.0);
                    angle_changed = lr::slider_hint(ui, "Winkel", &mut angle, -45.0..=45.0, 0.0, 0.05, "Ausrichten in Grad – ⌥ beim Ziehen für Feinarbeit");
                    ui.horizontal(|ui| {
                        lr::field_label(ui, crate::tr("Drehen"));
                        if lr::icon_button(ui, lr::Icon::RotCcw, false) {
                            rotate90 = Some(false);
                        }
                        if lr::icon_button(ui, lr::Icon::RotCw, false) {
                            rotate90 = Some(true);
                        }
                    });
                    // fixed line for the feedback (no jumping)
                    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::hover());
                    if let Some(m) = &msg {
                        ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, m, egui::TextStyle::Small.resolve(ui.style()), lr::TEXT_DIM());
                    }
                }
                GeoTool::Perspective => {
                    persp_reset = lr::group_label(ui, "Perspektive");
                    let hint_v = crate::tr("Perspektive senkrecht: stürzende Linien – negativ verbreitert oben (Blick nach oben)");
                    let hint_h = "Perspektive waagerecht: negativ verbreitert links, positiv rechts";
                    persp_changed |= lr::slider_hint(ui, "Senkrecht", &mut pv, -1.0..=1.0, 0.0, 0.01, hint_v);
                    persp_changed |= lr::slider_hint(ui, "Waagerecht", &mut ph, -1.0..=1.0, 0.0, 0.01, hint_h);
                    ui.add_space(2.0);
                }
            }
            ui.horizontal(|ui| {
                reset = ui.button(crate::tr("Zurücksetzen")).clicked();
                // shortcuts in the text instead of a hover tip; "Fertig" not gold (the window's main button is "Export")
                cancel = ui.button(format!("{}  Esc", crate::tr("Abbrechen"))).clicked();
                toggle = ui.button(format!("{}  ↩", crate::tr("Fertig"))).clicked();
            });
        });
        if reset || reset_hit {
            let d = Settings::default();
            let s = &mut self.entries[index].settings;
            s.crop = d.crop;
            s.crop_angle = 0.0;
            s.perspective_v = 0.0;
            s.perspective_h = 0.0;
        }
        if persp_reset && !(reset || reset_hit) {
            let s = &mut self.entries[index].settings;
            let a = s.crop_angle;
            geometry::set_transform(s, a, 0.0, 0.0, fw, fh, cam);
        }
        if persp_changed {
            let s = &mut self.entries[index].settings;
            let a = s.crop_angle;
            geometry::set_transform(s, a, pv as f32, ph as f32, fw, fh, cam);
            self.persp_grid_until = now + 0.8;
        }
        if straighten_free {
            self.straighten = !self.straighten;
            self.straighten_from = None;
            self.straighten_drag = false;
        }
        if straighten_click {
            self.geo_tool = GeoTool::Align;
            self.straighten = true;
            self.straighten_from = None;
            self.straighten_drag = false;
        }
        if let Some(mode) = upright {
            self.straighten = false;
            let s = &self.entries[index].settings;
            let base = Frame { angle: s.crop_angle, ..Frame::full_settings(s, fw, fh, cam) };
            let t = std::time::Instant::now();
            let segs = develop::upright::detect(&c.loaded.previews.developed.lin, &base);
            let r = develop::upright::solve(&segs, &base, mode);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            eprintln!("upright {mode:?}: {} lines, {ms:.0} ms → {r:?}", segs.len());
            let text = match r {
                Ok(u) => {
                    let s = &mut self.entries[index].settings;
                    geometry::set_transform(s, u.angle, u.pv, u.ph, fw, fh, cam);
                    self.persp_grid_until = now + 1.2;
                    crate::trf!("{a}° · senkrecht {b} · waagerecht {c} ({d} Linien)", a = format!("{:+.1}", u.angle), b = format!("{:+.2}", u.pv), c = format!("{:+.2}", u.ph), d = u.lines)
                }
                Err(e) => e.to_string(),
            };
            self.upright_msg = Some((text, now));
        }
        if angle_changed {
            self.persp_grid_until = now + 0.8;
            let s = &self.entries[index].settings;
            let f = Frame { angle: angle as f32, ..Frame::from_settings(s, fw, fh, cam) }.constrain();
            let (crop, a) = f.to_settings();
            let s = &mut self.entries[index].settings;
            s.crop = crop;
            s.crop_angle = a;
        }
        if let Some(a) = aspect {
            self.set_crop_aspect(a);
        }
        self.crop_raster = raster;
        if let Some(cw) = rotate90 {
            self.rotate_image(cw);
            if let Some(r) = self.crop_aspect {
                self.set_crop_aspect(Some(1.0 / r));
            }
        }
        if let Some(t) = open_tool {
            if t != tool || !active {
                self.open_geo_tool(t);
            } else if t == GeoTool::Align {
                self.set_geo_tool(t);
            }
        }
        if toggle {
            self.toggle_crop(true);
        }
        if cancel {
            self.toggle_crop(false);
        }
    }

    fn sensor_controls(&mut self, ui: &mut egui::Ui) {
        // Sigma/Foveon: the section "Pipeline" stays visible (greyed out, only graineo), as for the other RAWs
        if self.current.as_ref().is_some_and(|c| c.loaded.session.is_some()) {
            self.apple_section(ui);
        }
        let Some(c) = &self.current else {
            ui.label(crate::tr("Kein Bild geöffnet"));
            return;
        };
        let index = c.index;
        let path = self.entries[index].path.clone();
        let Some(session) = c.loaded.session.clone() else {
            if image_kind::of(&path) == image_kind::ImageKind::Raw {
                self.sensor_raw_sidebar(ui);
            } else {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(crate::tr("Die Sensor-Ansicht gibt es für RAW-Dateien (X3F, Bayer, X-Trans) – ein fertiges Bild hat kein Sensormosaik mehr.")).color(lr::TEXT_DIM()));
            }
            return;
        };
        let headroom = develop::analysis::ettr_headroom(&c.loaded.histograms);
        let hist = c.loaded.histograms.clone();
        let info = c.loaded.info.clone();
        let scope_tex = c.scope_tex.clone();
        // no parade/waveform yet (just switched to the Sensor module) → re-render once
        if scope_tex.is_none() && !self.rendering {
            self.rendered_for = None;
        }
        let mut scope = self.scope;
        let has_spp = c.loaded.previews.spp.is_some();
        let prepared = c.loaded.cal.is_prepared();
        let damaged = c.loaded.damage.is_some();
        let s = &mut self.entries[index].settings;

        lr::section(ui, "Ansicht", true, |ui| {
            // One row of fixed height: `with_layout(right_to_left(Center))` would take the whole remaining height of the
            // sidebar (the "?" centred vertically in it) and push the list of views out of sight.
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 18.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                help::info(ui, "sensor-ansichten", "Ansichten", &help::SENSOR_VIEWS)
            });
            // Test hook for screenshots: GRAINEO_SENSOR_VIEW=top|middle|bottom|raw[,rescued] selects a view once, a few
            // seconds after the start (when the photo's own settings have been read)
            static VIEW_ENV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if ui.input(|i| i.time) > 4.0 && !VIEW_ENV.swap(true, std::sync::atomic::Ordering::Relaxed) {
                if let Ok(v) = crate::env::var("GRAINEO_SENSOR_VIEW") {
                    s.view = match v.split(',').next().unwrap_or("") {
                        "top" => View::Layer(2),
                        "middle" => View::Layer(1),
                        "bottom" => View::Layer(0),
                        "raw" => View::RawComposite,
                        _ => s.view,
                    };
                    s.view_rescue = v.contains("rescued");
                }
            }
            for (v, label) in [
                (View::Developed, crate::tr("Entwickeltes Bild")),
                (View::Layer(2), crate::tr("Obere Schicht")),
                (View::Layer(1), crate::tr("Mittlere Schicht")),
                (View::Layer(0), crate::tr("Untere Schicht")),
                (View::RawComposite, crate::tr("Alle drei roh als R/G/B")),
                (View::Chroma, crate::tr("Farbanteil der Schichten")),
                (View::Clipping, crate::tr("Raw-Clipping je Schicht")),
            ] {
                if lr::list_item(ui, label, s.view == v).clicked() {
                    s.view = v;
                }
            }
            ui.add_space(4.0);
            let mut ev = s.view_gain_ev as f64;
            if lr::slider_hint(ui, "Helligkeit", &mut ev, -2.0..=4.0, 1.0, 0.1, "Nur Anzeige der Sensoransichten, in Blendenstufen") {
                s.view_gain_ev = ev as f32;
            }
            // damaged file: which rows of the shown layer were read, estimated or filled grey
            if damaged {
                ui.add_space(4.0);
                ui.add_enabled_ui(s.view != View::Developed && !s.view.is_stage(), |ui| {
                    lr::toggle(ui, &mut s.view_rescue, crate::tr("Gerettete Bereiche zeigen"));
                    if s.view_rescue {
                        ui.horizontal(|ui| {
                            for (color, text) in [(Color32::from_rgb(0xe0, 0xc0, 0x40), crate::tr("geschätzt")), (Color32::from_gray(0x9a), crate::tr("grau gefüllt"))] {
                                let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                                ui.painter().rect_filled(r, 0.0, color);
                                ui.label(egui::RichText::new(text).small().color(lr::TEXT_DIM()));
                                ui.add_space(8.0);
                            }
                        });
                    }
                });
            }
        });

        lr::section(ui, "Verarbeitungsstufen", true, |ui| {
            use develop::inspect::Stage;
            // Test hook for screenshots: GRAINEO_STAGE=<1…7>[d] selects a stage once (d = difference)
            static STAGE_ENV: std::sync::Once = std::sync::Once::new();
            STAGE_ENV.call_once(|| {
                if let Ok(v) = crate::env::var("GRAINEO_STAGE") {
                    let n: usize = v.trim_end_matches('d').parse().unwrap_or(0);
                    if let Some(&stage) = n.checked_sub(1).and_then(|i| Stage::ALL.get(i)) {
                        s.view = View::Stage { stage, diff: v.ends_with('d'), gain: 3 };
                    }
                }
            });
            let cur = match s.view {
                View::Stage { stage, diff, gain } => Some((stage, diff, gain)),
                _ => None,
            };
            let (diff, gain) = cur.map_or((false, 3), |(_, d, g)| (d, g));
            for st in Stage::ALL {
                if lr::list_item(ui, crate::tr(st.label()), cur.is_some_and(|c| c.0 == st)).clicked() {
                    s.view = View::Stage { stage: st, diff: diff && st.diff_label().is_some(), gain };
                }
            }
            ui.add_space(4.0);
            // after a click in the list the new stage applies (otherwise it would be
            // overwritten below with the previous one)
            let cur = match s.view {
                View::Stage { stage, diff, gain } => Some((stage, diff, gain)),
                _ => None,
            };
            // Fixed layout (toggle and sliders always in the same place, locked if needed),
            // so that nothing jumps when changing the stage
            let (stage, mut diff, gain) = cur.unwrap_or((Stage::Final, false, gain));
            let has_diff = cur.is_some() && stage.diff_label().is_some();
            let mut g = gain as f64;
            ui.add_enabled_ui(has_diff, |ui| {
                lr::segmented(ui, &mut diff, &[(false, crate::tr("Stufe")), (true, crate::tr("Differenz"))]);
                ui.add_space(2.0);
                ui.add_enabled_ui(diff, |ui| {
                    lr::slider_hint(ui, "Verstärkung", &mut g, 0.0..=8.0, 3.0, 1.0, "Differenz × 2ⁿ (3 = × 8); grau = keine Änderung");
                });
            });
            if cur.is_some() {
                s.view = View::Stage { stage, diff: diff && has_diff, gain: g.round().clamp(0.0, 8.0) as u8 };
                let text = if diff && has_diff { stage.diff_label().unwrap_or_default() } else { stage.explain() };
                ui.label(egui::RichText::new(crate::tr(text)).small().color(lr::TEXT()));
                let (note, exact) = develop::inspect::exactness(stage, has_spp, prepared);
                let note = crate::tr(note);
                let color = if exact { Color32::from_rgb(0x8c, 0xc8, 0x8c) } else { lr::TEXT_DIM() };
                ui.label(egui::RichText::new(if exact { format!("● {note}") } else { format!("○ {note}") }).small().color(color));
            } else {
            }
        });

        lr::section(ui, "Parade & Waveform", true, |ui| {
            lr::segmented(ui, &mut scope, &lr::Scope::ALL.map(|m| (m, m.label())));
            ui.add_space(2.0);
            lr::scope_view(ui, scope_tex.as_ref(), scope);
        });

        lr::section(ui, "Rohhistogramm & Belichtung", true, |ui| {
            histogram(ui, &hist, 96.0, true);
            ui.add_space(2.0);
            ui.label(egui::RichText::new(crate::tr("Reserve bis Sättigung")).color(lr::TEXT_DIM()));
            egui::Grid::new("ettr").num_columns(3).spacing([18.0, 2.0]).show(ui, |ui| {
                for (c, name) in [(0, "unten"), (1, "Mitte"), (2, "oben")] {
                    let h = headroom[c];
                    let (text, color) = if h.is_nan() {
                        ("–".to_string(), lr::TEXT_DIM())
                    } else if h <= 0.05 {
                        ("ausgebrannt".to_string(), Color32::from_rgb(0xe8, 0x70, 0x70))
                    } else {
                        (format!("{h:+.1} EV"), if h < 0.5 { Color32::from_rgb(0xe8, 0xc0, 0x60) } else { lr::TEXT() })
                    };
                    ui.label(name);
                    ui.label(egui::RichText::new(text).color(color).monospace());
                    ui.end_row();
                }
            });
        });

        let mut measure = self.measure;
        lr::section(ui, "Messfeld (Rauschen & SNR)", true, |ui| {
            ui.horizontal(|ui| {
                lr::toggle(ui, &mut measure, crate::tr("Messfeld aufziehen"));
            });
            match &self.stats {
                Some((i, [_, _, w, h], st)) if *i == index => {
                    ui.label(egui::RichText::new(crate::trf!("{w} × {h} Pixel, Rohdaten nach Schwarzabzug (Sensoreinheiten)", w = w, h = h)).small().color(lr::TEXT_DIM()));
                    egui::Grid::new("stats").num_columns(6).striped(true).spacing([10.0, 2.0]).show(ui, |ui| {
                        for t in ["", crate::tr("Mittel"), crate::tr("Std."), crate::tr("Rauschen"), crate::tr("SNR"), crate::tr("Clip")] {
                            ui.label(egui::RichText::new(t).small().color(lr::TEXT_DIM()));
                        }
                        ui.end_row();
                        for (c, name) in [(2, "oben"), (1, "Mitte"), (0, "unten")] {
                            let l = &st[c];
                            ui.label(name);
                            ui.monospace(format!("{:.1}", l.mean));
                            ui.monospace(format!("{:.2}", l.std));
                            ui.monospace(format!("{:.2}", l.noise));
                            ui.monospace(if l.snr_db().is_nan() { "–".into() } else { format!("{:.1} dB", l.snr_db()) });
                            ui.monospace(format!("{:.1} %", l.clipped * 100.0));
                            ui.end_row();
                        }
                    });
                    if st[2].noise > 0.0 {
                        ui.label(
                            egui::RichText::new(crate::trf!("Rauschen unten/oben: × {a} · Mitte/oben: × {b}", a = format!("{:.2}", st[0].noise / st[2].noise), b = format!("{:.2}", st[1].noise / st[2].noise)))
                            .small()
                            .color(lr::TEXT_DIM()),
                        );
                    }
                    ui.label(
                        egui::RichText::new(crate::tr("„Rauschen“ = Differenz waagerechter Nachbarn / √2 – unabhängig von Verläufen im Feld. Für saubere Werte eine gleichmäßige Fläche messen."))
                            .small()
                            .color(lr::TEXT_DIM()),
                    );
                }
                _ => {
                    ui.label(egui::RichText::new(crate::tr("Noch kein Messfeld – z. B. Himmel oder eine graue Fläche aufziehen.")).small().color(lr::TEXT_DIM()));
                }
            }
        });
        if measure != self.measure {
            self.measure = measure;
        }

        lr::section(ui, "Kalibrierdaten (CAMF)", true, |ui| {
            ui.label(
                egui::RichText::new(crate::tr("Sigmas Werkskalibrierung in jeder Datei: Flat-Field-Karten, Farbmatrizen, Weißabgleich-Tabellen, Rauschmodelle, Defektpixel."))
                    .small()
                    .color(lr::TEXT_DIM()),
            );
            // Test hook: GRAINEO_CAMF=<entry> opens the inspector directly
            let auto = crate::env::var("GRAINEO_CAMF").ok().filter(|_| self.camf.index.is_none());
            if let Some(name) = &auto {
                self.camf.select(name);
            }
            if ui.button(crate::tr("CAMF-Inspektor öffnen …")).clicked() || auto.is_some() {
                self.camf.open = true;
                if self.camf.index != Some(index) {
                    self.camf.index = Some(index);
                    self.camf.camf = None;
                    self.camf.error = None;
                    self.worker.send(Job::Camf { index, path: path.clone() });
                }
            }
        });

        lr::section(ui, "Schichten exportieren", false, |ui| {
            ui.label(
                egui::RichText::new(crate::tr("Drei lineare 16-Bit-Graustufen-TIFFs (unten, Mitte, oben) in voller Auflösung – für eigene Mischungen, Analysen oder Astro-Stacking."))
                    .small()
                    .color(lr::TEXT_DIM()),
            );
            ui.checkbox(&mut self.layers_flat, crate::tr("Flat-Field anwenden (Spatial Gain + Farbschatten)"));
            if ui.button(crate::tr("Schichten speichern …")).clicked() {
                let start = path.parent().unwrap_or(std::path::Path::new("/")).to_path_buf();
                file_dialog::target_folder(ui.ctx(), "schichten-speichern", move |d| d.set_directory(start));
            }
            if let Some(dir) = file_dialog::fetch_one("schichten-speichern") {
                self.layers_status = crate::tr("Wird gespeichert …").into();
                self.worker.send(Job::ExportLayers { session: session.clone(), dir, flat_field: self.layers_flat });
            }
            if !self.layers_status.is_empty() {
                ui.label(egui::RichText::new(crate::menu::i18n::tr_msg(&self.layers_status)).small());
            }
        });

        lr::section(ui, "Aufnahme", false, |ui| info_grid(ui, &info));
        if scope != self.scope {
            self.scope = scope;
            if let Some(f) = app_file("view.txt") {
                let _ = std::fs::write(f, scope.key());
            }
            if let Some(c) = &mut self.current {
                c.scope_tex = None;
            }
            self.rendered_for = None;
        }
    }
}

/// File that holds the last opened folder.
/// Small file in the app folder under "Application Support" (folder is created).
fn app_file(name: &str) -> Option<PathBuf> {
    // same location as the library (test runs: GRAINEO_DATA_DIR)
    let dir = library::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join(name))
}

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Narrow edge strip with a triangle (points inwards when the panel is hidden).
/// Click: toggle. Hovering a hidden panel for a while reports "show
/// temporarily" via the context data. Returns: clicked.
/// Fixed width, not resizable (otherwise egui shows a handle with a resize cursor at the edge).
fn edge_strip(ui: &mut egui::Ui, id: &str, edge: Edge, shown: bool) -> bool {
    let frame = egui::Frame::new().fill(lr::PANEL_DARK);
    let size = 12.0;
    let body = |ui: &mut egui::Ui| -> bool {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.interact(rect, ui.scope_id().with(id), egui::Sense::click());
        let hovered = resp.hovered();
        let c = rect.center();
        let s = 4.0;
        // the triangle points in the direction the panel moves
        let inward = !shown;
        let (a, b, d) = match (edge, inward) {
            (Edge::Left, true) | (Edge::Right, false) => (c + egui::vec2(-s * 0.6, -s), c + egui::vec2(-s * 0.6, s), c + egui::vec2(s * 0.6, 0.0)),
            (Edge::Left, false) | (Edge::Right, true) => (c + egui::vec2(s * 0.6, -s), c + egui::vec2(s * 0.6, s), c + egui::vec2(-s * 0.6, 0.0)),
            (Edge::Top, true) | (Edge::Bottom, false) => (c + egui::vec2(-s, -s * 0.6), c + egui::vec2(s, -s * 0.6), c + egui::vec2(0.0, s * 0.6)),
            (Edge::Top, false) | (Edge::Bottom, true) => (c + egui::vec2(-s, s * 0.6), c + egui::vec2(s, s * 0.6), c + egui::vec2(0.0, -s * 0.6)),
        };
        // the dwell logic lives in edge_peek.rs; here only the place of the strip is recorded
        let side = match edge { Edge::Top => 0, Edge::Bottom => 1, Edge::Left => 2, Edge::Right => 3 };
        if id.starts_with("edge-") {
            ui.data_mut(|d| d.insert_temp(egui::Id::new(("edge-rect", side)), Some(rect)));
        }
        let col = if hovered { Color32::from_gray(0x9a) } else { Color32::from_gray(0x3a) };
        ui.painter().add(egui::Shape::convex_polygon(vec![a, b, d], col, egui::Stroke::NONE));
        resp.clicked()
    };
    match edge {
        Edge::Top => egui::Panel::top(id).show_separator_line(false).resizable(false).exact_size(size).frame(frame).show(ui, body).inner,
        Edge::Bottom => egui::Panel::bottom(id).show_separator_line(false).resizable(false).exact_size(size).frame(frame).show(ui, body).inner,
        Edge::Left => egui::Panel::left(id).show_separator_line(false).resizable(false).exact_size(size).frame(frame).show(ui, body).inner,
        Edge::Right => egui::Panel::right(id).show_separator_line(false).resizable(false).exact_size(size).frame(frame).show(ui, body).inner,
    }
}

/// Bounds of a layer share in the B&W mix.
const MIX_MIN: f32 = 0.0;
const MIX_MAX: f32 = 1.0;

/// Set layer share `i` to `v`; the other two take the remainder in their previous ratio, the sum stays 1.
fn set_mix(alt: [f32; 3], i: usize, v: f32) -> [f32; 3] {
    let v = v.clamp(MIX_MIN, MIX_MAX);
    let (j, k) = ((i + 1) % 3, (i + 2) % 3);
    let rest = 1.0 - v;
    let so = alt[j] + alt[k];
    let mut nj = if so.abs() > 0.02 { alt[j] * rest / so } else { rest / 2.0 };
    nj = nj.clamp(MIX_MIN, MIX_MAX);
    let nk = (rest - nj).clamp(MIX_MIN, MIX_MAX);
    let nj = (rest - nk).clamp(MIX_MIN, MIX_MAX);
    let mut m = [0.0; 3];
    m[i] = v;
    m[j] = nj;
    m[k] = nk;
    m
}

/// Free space around the photo in crop mode (points per side).
const CROP_MARGIN: f32 = 22.0;
/// Grip radius of a crop corner (points).
const CROP_CORNER_GRIP: f32 = 20.0;
/// Grip distance of a crop edge (points, along its whole length).
const CROP_EDGE_GRIP: f32 = 9.0;

/// Distance of `p` from the segment `a`–`b` (screen points).
fn distance_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + ab * t)
}

/// Ongoing crop gesture.
#[derive(Clone, Copy)]
enum CropDrag {
    Move { start: Frame, from: [f32; 2] },
    /// `from`/`center`: pointer start and frame centre on screen
    Rotate { start: Frame, from: [f32; 2], center: [f32; 2] },
    /// `side`: −1/0/+1 per axis (corner or edge)
    Resize { side: [f32; 2], start: Frame },
}

/// Rotate cursor (arc with two arrowheads) at `p`, curved around the frame centre `center`.
fn rotate_cursor(painter: &egui::Painter, p: egui::Pos2, center: egui::Pos2) {
    let d = p - center;
    let radius = d.length().max(24.0);
    let mid_angle = d.y.atan2(d.x);
    let half = (15.0 / radius).min(0.7);
    let point = |t: f32| center + egui::vec2((mid_angle + t).cos(), (mid_angle + t).sin()) * radius;
    // the arc lies around the pointer, not around the centre: shift by `p`
    let offset = p - point(0.0);
    let arc: Vec<egui::Pos2> = (0..=12).map(|i| point(-half + 2.0 * half * i as f32 / 12.0) + offset).collect();
    let tip = |end: egui::Pos2, before: egui::Pos2| {
        let r = (end - before).normalized();
        let n = egui::vec2(-r.y, r.x);
        [end + r * 5.0, end - r * 3.0 + n * 5.0, end - r * 3.0 - n * 5.0]
    };
    let (a, b) = (tip(arc[0], arc[1]), tip(arc[12], arc[11]));
    for (width, color) in [(4.5, Color32::from_black_alpha(220)), (1.8, Color32::WHITE)] {
        painter.add(egui::Shape::line(arc.clone(), egui::Stroke::new(width, color)));
        for t in [a, b] {
            painter.add(egui::Shape::closed_line(t.to_vec(), egui::Stroke::new(width, color)));
        }
    }
    painter.add(egui::Shape::convex_polygon(a.to_vec(), Color32::WHITE, egui::Stroke::NONE));
    painter.add(egui::Shape::convex_polygon(b.to_vec(), Color32::WHITE, egui::Stroke::NONE));
}

/// Draws a texture showing the source rectangle `src` (x, y, w, h in source pixels)
/// as it appears in the crop `frame`, which is mapped onto `rect` (screen).
/// Test hook `GRAINEO_ZOOM_TEST="1@6,fit@9,2@10,pan:300:-120@11"` (only with `GRAINEO_DATA_DIR`): sets the zoom of the
/// single view after the given seconds – a number is a multiple of 100 %, `fit` fits the image, `pan:dx:dy` pans by points,
/// `report` logs zoom and pan –
/// for measuring zoom and pan sequences without mouse control. `ramp:<n>:<s>` zooms smoothly (every frame, like a
/// pinch or scroll gesture) to `n` × 100 % within `s` seconds. While the hook is active, frames slower than 25 ms
/// are logged ("zoom test frame"). Returns the new zoom and pan when a step is due.
fn zoom_test_step(zoom: f32, pan: egui::Vec2, one: f32) -> Option<(f32, egui::Vec2)> {
    type Ramp = (std::time::Instant, f32, f32, f64);
    type Steps = (std::time::Instant, Vec<(String, f64)>, Option<Ramp>);
    static STEPS: Mutex<Option<Steps>> = Mutex::new(None);
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return None;
    }
    let spec = crate::env::var("GRAINEO_ZOOM_TEST").ok()?;
    let mut g = STEPS.lock().unwrap();
    let (start, list, ramp) = g.get_or_insert_with(|| {
        let list = spec.split(',').filter_map(|t| t.rsplit_once('@').and_then(|(k, s)| Some((k.trim().to_string(), s.trim().parse().ok()?)))).collect();
        (std::time::Instant::now(), list, None)
    });
    let t = start.elapsed().as_secs_f64();
    if let Some((r0, from, to, dur)) = *ramp {
        let u = (r0.elapsed().as_secs_f64() / dur).clamp(0.0, 1.0) as f32;
        let z = (from.ln() + (to.ln() - from.ln()) * u).exp().max(1.0);
        if u >= 1.0 {
            *ramp = None;
            log(&format!("zoom test: ramp to {to:.3} done after {t:.2} s"));
        }
        return Some((z, pan * (z / zoom.max(1e-3))));
    }
    let pos = list.iter().position(|(_, s)| t >= *s)?;
    let (what, _) = list.remove(pos);
    log(&format!("zoom test: {what} after {t:.1} s"));
    let parts: Vec<&str> = what.split(':').collect();
    match parts.as_slice() {
        ["fit"] => Some((1.0, egui::Vec2::ZERO)),
        ["report"] => {
            log(&format!("zoom test: report zoom {zoom:.3} pan {:.1} {:.1}", pan.x, pan.y));
            None
        }
        ["pan", dx, dy] => Some((zoom, pan + egui::vec2(dx.parse().ok()?, dy.parse().ok()?))),
        ["ramp", n, s] => {
            *ramp = Some((std::time::Instant::now(), zoom, (n.parse::<f32>().ok()? * one).max(1.0), s.parse().ok()?));
            None
        }
        [n] => Some(((n.parse::<f32>().ok()? * one).max(1.0), pan)),
        _ => None,
    }
}

/// Test hook (with `GRAINEO_ZOOM_TEST`): logs frames slower than 25 ms – gap since the previous frame start and the
/// share of the app's own frame code (the rest is painting, uploading textures and waiting for the display).
fn zoom_test_frame(begin: bool, zoom: f32) {
    type Last = (std::time::Instant, f64);
    static LAST: Mutex<Option<Last>> = Mutex::new(None);
    if crate::env::var_os("GRAINEO_ZOOM_TEST").is_none() || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let now = std::time::Instant::now();
    let mut g = LAST.lock().unwrap();
    if begin {
        if let Some((prev, ui_ms)) = *g {
            let gap = (now - prev).as_secs_f64() * 1000.0;
            if gap > 25.0 {
                log(&format!("zoom test frame: {gap:.0} ms since the previous frame (its frame code {ui_ms:.0} ms), zoom {zoom:.3}"));
            }
        }
        *g = Some((now, 0.0));
    } else if let Some((b, _)) = *g {
        *g = Some((b, (now - b).as_secs_f64() * 1000.0));
    }
}

/// Seconds without zoom/pan before a finished whole image is uploaded (see `App::full_waiting`).
const FULL_UPLOAD_REST: f64 = 0.25;

/// Whole image at full resolution from the worker, not yet uploaded as textures.
struct FullWaiting {
    index: usize,
    settings: Settings,
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    half: (usize, usize, Vec<u8>),
    seconds: f32,
}

/// Full-resolution image of the zoomed view, split into tiles no larger than the GPU allows (`max_texture_side`).
/// The whole image also carries a level at half the edge length for views below ~75 % (pyramid: less aliasing
/// than minifying the full level, a quarter of the texels).
/// Largest whole image (pixels) kept with preserved gamut at full resolution: 10 bytes per pixel with the half level
/// (≈ 450 MB of textures at 45 MP); larger images get the 8-bit whole image.
const WIDE_FULL_MAX_PIXELS: usize = 45_000_000;

/// The job for the sharp zoomed view: the whole image (`region = None`) or a crop; with preserved gamut where the
/// photo and its settings have it (and at 100 %: `factor` 1), else the 8-bit jobs.
fn detail_job(c: &Current, settings: &Settings, region: Option<[usize; 4]>, factor: usize, generation: usize, current: Arc<AtomicUsize>, edit: jobs::EditStamp) -> Job {
    let [fw, fh] = c.loaded.full_size;
    let (index, session, foreign, settings) = (c.index, c.loaded.session.clone(), c.loaded.previews.foreign.clone(), settings.clone());
    let wide = photo_display::wide_active() && factor == 1 && jobs::wide_detail_possible(&c.loaded, &settings) && (region.is_some() || fw * fh <= WIDE_FULL_MAX_PIXELS);
    match region {
        _ if wide => Job::RenderDetailWide { index, session, foreign, settings, region, generation, current, edit },
        Some(region) => Job::RenderRegion { index, session, foreign, settings, region, factor, generation, current, edit },
        None => Job::RenderFull { index, session, foreign, settings, generation, current, edit },
    }
}

/// Crops shown zoomed in: pixels stay pixels when magnified, bilinear when reduced (nearest would skip texels).
const DETAIL_OPTIONS: TextureOptions = TextureOptions { magnification: egui::TextureFilter::Nearest, minification: egui::TextureFilter::Linear, ..TextureOptions::NEAREST };

/// Texture of a detail tile: 8 bit (egui, fallback without wgpu render state) or a photo texture (8 bit or with
/// preserved gamut / HDR, `photo_display.rs`) that also has a bilinear binding.
enum TileTex {
    Egui(TextureHandle),
    Photo(Arc<photo_display::PhotoTex>),
}

impl TileTex {
    /// `smooth`: bilinear magnification (photo textures; egui textures stay nearest)
    fn id(&self, smooth: bool) -> egui::TextureId {
        match self {
            TileTex::Egui(t) => t.id(),
            TileTex::Photo(t) if smooth => t.id_smooth(),
            TileTex::Photo(t) => t.id(),
        }
    }
}

struct DetailTex {
    /// texture and its part of the source image (x, y, w, h in image pixels)
    tiles: Vec<(TileTex, [f32; 4])>,
    /// texture pixels (width, height) of this level
    size: [usize; 2],
    /// texture pixels per image pixel of this level (crops of the grain loupe: > 1, half level: about 0.5)
    texel: f32,
    /// bytes of all levels
    bytes: usize,
    coarse: Option<Box<DetailTex>>,
    /// HDR version of the same crop (gain map), drawn instead while HDR is shown
    hdr: Option<Box<DetailTex>>,
    /// the tiles hold values beyond sRGB (photo textures)
    wide: bool,
}

impl DetailTex {
    /// `rgb` (`width`×`height`, 8 bit) shows `region` of the source image, `factor` texture pixels per image pixel.
    fn new(ctx: &egui::Context, name: &str, width: usize, height: usize, rgb: &[u8], region: [usize; 4], factor: usize, max_side: usize) -> Self {
        let f = factor.max(1) as f32;
        let to_src = |r: [usize; 4]| [region[0] as f32 + r[0] as f32 / f, region[1] as f32 + r[1] as f32 / f, r[2] as f32 / f, r[3] as f32 / f];
        // as photo textures with a nearest and a bilinear binding (`detail_filter`), else as egui textures
        if let Some(t) = photo_display::tiles_rgb8(width, height, rgb, max_side.min(photo_display::max_texture_side())) {
            let bytes = t.iter().map(|(t, _)| t.bytes()).sum();
            let tiles = t.into_iter().map(|(t, r)| (TileTex::Photo(t), to_src(r))).collect();
            return DetailTex { tiles, size: [width, height], texel: f, bytes, coarse: None, hdr: None, wide: false };
        }
        let edge = max_side.max(256);
        let mut tiles = Vec::new();
        for ty in (0..height).step_by(edge) {
            for tx in (0..width).step_by(edge) {
                let (tw, th) = ((width - tx).min(edge), (height - ty).min(edge));
                let img = if (tw, th) == (width, height) {
                    ColorImage::from_rgb([width, height], rgb)
                } else {
                    let mut part = Vec::with_capacity(tw * th * 3);
                    for y in ty..ty + th {
                        part.extend_from_slice(&rgb[(y * width + tx) * 3..(y * width + tx + tw) * 3]);
                    }
                    ColorImage::from_rgb([tw, th], &part)
                };
                tiles.push((TileTex::Egui(ctx.load_texture(name, img, DETAIL_OPTIONS)), to_src([tx, ty, tw, th])));
            }
        }
        DetailTex { tiles, size: [width, height], texel: f, bytes: width * height * 4, coarse: None, hdr: None, wide: false }
    }

    /// From photo textures (`photo_display::tiles`) of `region` (image pixels, at full resolution), optionally with the
    /// half level of the same region.
    fn from_photo(tiles: photo_display::Tiles, size: [usize; 2], region: [usize; 4], half: Option<(usize, usize, photo_display::Tiles)>) -> Self {
        let map = |tiles: photo_display::Tiles, (fx, fy): (f32, f32)| -> Vec<(TileTex, [f32; 4])> {
            tiles.into_iter().map(|(t, r)| (TileTex::Photo(t), [region[0] as f32 + r[0] as f32 * fx, region[1] as f32 + r[1] as f32 * fy, r[2] as f32 * fx, r[3] as f32 * fy])).collect()
        };
        let texel = |w: usize| w as f32 / region[2].max(1) as f32;
        let coarse = half.map(|(hw, hh, t)| {
            let bytes = t.iter().map(|(t, _)| t.bytes()).sum();
            Box::new(DetailTex { tiles: map(t, (region[2] as f32 / hw as f32, region[3] as f32 / hh as f32)), size: [hw, hh], texel: texel(hw), bytes, coarse: None, hdr: None, wide: true })
        });
        let bytes = tiles.iter().map(|(t, _)| t.bytes()).sum::<usize>() + coarse.as_ref().map_or(0, |c| c.bytes);
        DetailTex { tiles: map(tiles, (region[2] as f32 / size[0].max(1) as f32, region[3] as f32 / size[1].max(1) as f32)), size, texel: texel(size[0]), bytes, coarse, hdr: None, wide: true }
    }

    /// Add the level at half resolution (`rgb`: `width`×`height`) for the same region.
    fn with_coarse(mut self, ctx: &egui::Context, width: usize, height: usize, rgb: &[u8], region: [usize; 4], max_side: usize) -> Self {
        let mut c = DetailTex::new(ctx, "detail-half", width, height, rgb, [0, 0, width, height], 1, max_side);
        // texture pixels → image pixels of `region`
        let (fx, fy) = (region[2] as f32 / width as f32, region[3] as f32 / height as f32);
        for (_, src) in &mut c.tiles {
            *src = [region[0] as f32 + src[0] * fx, region[1] as f32 + src[1] * fy, src[2] * fx, src[3] * fy];
        }
        c.texel = width as f32 / region[2].max(1) as f32;
        self.bytes += c.bytes;
        self.coarse = Some(Box::new(c));
        self
    }

    fn size(&self) -> [usize; 2] {
        self.size
    }

    /// `scale`: screen pixels per image pixel. Full and half level are chosen, blended and filtered by
    /// `detail_filter::plan` (no aliasing in reduced views, exact pixels at 100 %).
    fn paint(&self, painter: &egui::Painter, frame: &Frame, rect: egui::Rect, tint: Color32, scale: f32) {
        if let Some(h) = self.hdr.as_ref().filter(|_| photo_display::hdr_frame()) {
            return h.paint(painter, frame, rect, tint, scale);
        }
        let transformed = frame.angle.abs() > 1e-4 || frame.has_perspective();
        let plan = detail_filter::plan(scale / self.texel.max(1e-6), self.coarse.is_some(), transformed);
        if plan.full {
            for (t, src) in &self.tiles {
                paint_src_quad(painter, t.id(plan.smooth), *src, frame, rect, tint);
            }
        }
        if let Some(c) = self.coarse.as_ref().filter(|_| plan.half > 0.0) {
            // the half level is always bilinear (it is a prefiltered reduction, never shown as pixels)
            let tint = if plan.full { tint.gamma_multiply(plan.half) } else { tint };
            for (t, src) in &c.tiles {
                paint_src_quad(painter, t.id(true), *src, frame, rect, tint);
            }
        }
    }
}

/// Kind of placeholder in the viewer: the last finished picture of the photo itself (see `App::kept_view`).
const KEPT_VIEW: &str = "kept";

/// Small loading spinner without text, `alpha` 0…1. Two-tone so it stays visible on any image: a very faint dark disc,
/// a dark halo (slightly wider) under the ring and the turning arc, the arc itself light.
/// Quiet spinner at the top left of the image area (state slot 0: loading, 1: Sigma refinement): appears only after 300 ms
/// of waiting, fades in and out; `progress` adds a ring that fills up.
fn quiet_spinner(state: &mut (f32, f64, Option<f64>), ctx: &egui::Context, painter: &egui::Painter, area: egui::Rect, want: bool, progress: Option<f32>, now: f64) {
    {
        let (alpha, last, since) = *state;
        let since = if want { Some(since.unwrap_or(now)) } else { None };
        let show = want && since.is_some_and(|t| now - t > 0.3);
        let dt = (now - last).clamp(0.0, 0.1) as f32;
        let a = (alpha + if show { dt / 0.25 } else { -dt / 0.2 }).clamp(0.0, 1.0);
        *state = (a, now, since);
        if a > 0.0 {
            let c = area.left_top() + egui::vec2(18.0, 18.0);
            spinner(painter, c, 6.5, now, a);
            if let Some(p) = progress {
                let n = (40.0 * p.clamp(0.0, 1.0)) as usize;
                if n > 0 {
                    let pts: Vec<egui::Pos2> = (0..=n).map(|i| {
                        let t = -std::f32::consts::FRAC_PI_2 + i as f32 / 40.0 * std::f32::consts::TAU;
                        c + egui::vec2(t.cos(), t.sin()) * 10.5
                    }).collect();
                    painter.add(egui::Shape::line(pts.clone(), egui::Stroke::new(3.0, Color32::from_black_alpha((90.0 * a) as u8))));
                    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, Color32::from_white_alpha((200.0 * a) as u8))));
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if want {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Filmstrip cell height from which a series number is a button (smaller cells only show it).
const STRIP_NUMBER_BUTTON_H: f32 = 56.0;

fn spinner(painter: &egui::Painter, center: egui::Pos2, radius: f32, now: f64, alpha: f32) {
    let dark = |a: f32| Color32::from_black_alpha((255.0 * a * alpha) as u8);
    let light = |a: f32| Color32::from_white_alpha((255.0 * a * alpha) as u8);
    painter.circle_filled(center, radius + 3.5, dark(0.20));
    painter.circle_stroke(center, radius, egui::Stroke::new(3.5, dark(0.35)));
    painter.circle_stroke(center, radius, egui::Stroke::new(2.0, light(0.18)));
    let start = (now * 5.0) as f32;
    let points: Vec<egui::Pos2> = (0..=20).map(|i| {
        let t = start + i as f32 / 20.0 * std::f32::consts::PI * 0.6;
        center + egui::vec2(t.cos(), t.sin()) * radius
    }).collect();
    painter.add(egui::Shape::line(points.clone(), egui::Stroke::new(3.5, dark(0.35))));
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, light(0.75))));
}

/// Visible region `r` with a margin of `share` of its size on every side, clamped to the image (`size`).
fn region_with_margin(r: [usize; 4], share: f32, size: [usize; 2]) -> [usize; 4] {
    let (mx, my) = ((r[2] as f32 * share) as usize, (r[3] as f32 * share) as usize);
    let (x0, y0) = (r[0].saturating_sub(mx), r[1].saturating_sub(my));
    let (x1, y1) = ((r[0] + r[2] + mx).min(size[0]), (r[1] + r[3] + my).min(size[1]));
    [x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1)]
}

fn paint_src_quad(painter: &egui::Painter, tex: egui::TextureId, src: [f32; 4], frame: &Frame, rect: egui::Rect, tint: Color32) {
    let [x, y, w, h] = src;
    // Without perspective the mapping is affine: two triangles are exact. With perspective
    // (projective) a fine mesh, so the texture does not kink along the diagonals.
    let n = if frame.has_perspective() { 32 } else { 1 };
    let mut mesh = egui::Mesh::with_texture(tex);
    for j in 0..=n {
        for i in 0..=n {
            let (tu, tv) = (i as f32 / n as f32, j as f32 / n as f32);
            let u = frame.from_src([x + w * tu, y + h * tv]);
            mesh.vertices.push(egui::epaint::Vertex { pos: rect.min + egui::vec2(u[0], u[1]) * rect.size(), uv: egui::pos2(tu, tv), color: tint });
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = (j * (n + 1) + i) as u32;
            let b = a + n as u32 + 1;
            mesh.add_triangle(a, a + 1, b + 1);
            mesh.add_triangle(a, b + 1, b);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// View in crop mode: the whole image is rotated back by the fine angle around the centre of the image area, so that the
/// frame stays axis-aligned and the image rotates behind it. `forward`: image → screen, `back` the reverse.
#[derive(Clone, Copy)]
struct ViewTransform {
    center: egui::Pos2,
    s: f32,
    c: f32,
}

impl ViewTransform {
    fn new(center: egui::Pos2, angle_deg: f32) -> Self {
        let a = (-angle_deg).to_radians();
        Self { center, s: a.sin(), c: a.cos() }
    }
    fn forward(&self, p: egui::Pos2) -> egui::Pos2 {
        let d = p - self.center;
        self.center + egui::vec2(self.c * d.x - self.s * d.y, self.s * d.x + self.c * d.y)
    }
    fn back(&self, q: egui::Pos2) -> egui::Pos2 {
        let d = q - self.center;
        self.center + egui::vec2(self.c * d.x + self.s * d.y, -self.s * d.x + self.c * d.y)
    }
}

/// Like [`paint_src_quad`], but with the corners additionally rotated by the crop view.
fn paint_src_quad_rotated(painter: &egui::Painter, tex: egui::TextureId, src: [f32; 4], frame: &Frame, rect: egui::Rect, tint: Color32, view_rect: &ViewTransform) {
    let [x, y, w, h] = src;
    let n = if frame.has_perspective() { 32 } else { 1 };
    let mut mesh = egui::Mesh::with_texture(tex);
    for j in 0..=n {
        for i in 0..=n {
            let (tu, tv) = (i as f32 / n as f32, j as f32 / n as f32);
            let u = frame.from_src([x + w * tu, y + h * tv]);
            let pos = view_rect.forward(rect.min + egui::vec2(u[0], u[1]) * rect.size());
            mesh.vertices.push(egui::epaint::Vertex { pos, uv: egui::pos2(tu, tv), color: tint });
        }
    }
    for j in 0..n {
        for i in 0..n {
            let a = (j * (n + 1) + i) as u32;
            let b = a + n as u32 + 1;
            mesh.add_triangle(a, a + 1, b + 1);
            mesh.add_triangle(a, b + 1, b);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Thumbnail with crop and rotation: first downscale (area average), then sample through the
/// crop mapping. `rgb` shows the whole sensor image.
fn frame_thumb(rgb: &[u8], w: usize, h: usize, frame: &Frame, edge: usize) -> ColorImage {
    let small = downscale_rgb(rgb, w, h, edge * 2);
    let [sw, sh] = small.size;
    let k = (edge as f32 / frame.w.max(frame.h)).min(1.0);
    let (tw, th) = (((frame.w * k).round() as usize).max(1), ((frame.h * k).round() as usize).max(1));
    let mut pixels = Vec::with_capacity(tw * th);
    for y in 0..th {
        for x in 0..tw {
            let p = frame.to_src([(x as f32 + 0.5) / tw as f32, (y as f32 + 0.5) / th as f32]);
            let sx = ((p[0] / frame.sw * sw as f32) as usize).min(sw - 1);
            let sy = ((p[1] / frame.sh * sh as f32) as usize).min(sh - 1);
            pixels.push(small.pixels[sy * sw + sx]);
        }
    }
    ColorImage::new([tw, th], pixels)
}

/// Reductions of a render result for the thumbnail and the stored previews (see `App::thumb_from_render`).
struct ThumbJob {
    index: usize,
    image: i64,
    /// file of the photo the render belongs to
    path: PathBuf,
    generation: u64,
    /// crop of the current image (otherwise the whole render is reduced)
    frame: Option<Frame>,
    /// store the developed view: settings, fingerprint, original (not a virtual copy)
    store: Option<(Settings, u64, bool)>,
    rgb: Vec<u8>,
    width: usize,
    height: usize,
}

struct ThumbDone {
    index: usize,
    image: i64,
    path: PathBuf,
    generation: u64,
    /// developed view (1280 px) with settings, fingerprint and whether it belongs to the original
    view: Option<(ColorImage, Settings, u64, bool)>,
    /// the view as RGB for the library's preview store (originals only)
    view_rgb: Option<image::RgbImage>,
    thumb: ColorImage,
}

impl ThumbJob {
    fn run(self) -> ThumbDone {
        let (rgb, w, h) = (&self.rgb, self.width, self.height);
        let view = match (&self.frame, self.store) {
            (Some(frame), Some((settings, fp, own))) => Some((frame_thumb(rgb, w, h, frame, 1280), settings, fp, own)),
            _ => None,
        };
        let view_rgb = view.as_ref().filter(|v| v.3).map(|v| color_image_to_rgb(&v.0));
        let thumb = match &self.frame {
            Some(frame) => frame_thumb(rgb, w, h, frame, 400),
            None => downscale_rgb(rgb, w, h, 400),
        };
        ThumbDone { index: self.index, image: self.image, path: self.path, generation: self.generation, view, view_rgb, thumb }
    }
}

/// egui image → RGB image (for the library's preview store).
fn color_image_to_rgb(ci: &ColorImage) -> image::RgbImage {
    let [w, h] = ci.size;
    let raw: Vec<u8> = ci.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
    image::RgbImage::from_raw(w as u32, h as u32, raw).unwrap_or_default()
}

/// Downscales an RGB image by area averaging to at most `edge` pixels on the long side.
fn downscale_rgb(rgb: &[u8], w: usize, h: usize, edge: usize) -> ColorImage {
    let f = w.max(h).div_ceil(edge).max(1);
    let (tw, th) = (w / f, h / f);
    let mut out = Vec::with_capacity(tw * th * 3);
    for ty in 0..th {
        for tx in 0..tw {
            let mut acc = [0u32; 3];
            for y in ty * f..ty * f + f {
                for x in tx * f..tx * f + f {
                    let i = (y * w + x) * 3;
                    for c in 0..3 {
                        acc[c] += rgb[i + c] as u32;
                    }
                }
            }
            let n = (f * f) as u32;
            out.extend(acc.map(|a| (a / n) as u8));
        }
    }
    ColorImage::from_rgb([tw, th], &out)
}

/// Subtle badge at the bottom right of the image area, optionally with a progress bar (0…1).
fn badge(painter: &egui::Painter, area: egui::Rect, text: &str, progress: Option<f32>) -> egui::Rect {
    badge_alpha(painter, area, text, progress, 1.0)
}

fn badge_alpha(painter: &egui::Painter, area: egui::Rect, text: &str, progress: Option<f32>, alpha: f32) -> egui::Rect {
    let galley = painter.layout_no_wrap(crate::menu::i18n::tr_msg(text).into_owned(), egui::FontId::proportional(12.5), Color32::from_white_alpha((235.0 * alpha) as u8));
    let pad = egui::vec2(10.0, 6.0);
    let bar = if progress.is_some() { 5.0 } else { 0.0 };
    let size = galley.size() + pad * 2.0 + egui::vec2(0.0, bar);
    let min = area.right_bottom() - size - egui::vec2(12.0, 12.0);
    let rect = egui::Rect::from_min_size(min, size);
    navigator::note_corner(painter.ctx(), rect);
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha((165.0 * alpha) as u8));
    painter.galley(rect.min + pad, galley, Color32::WHITE);
    if let Some(p) = progress {
        let track = egui::Rect::from_min_max(
            egui::pos2(rect.left() + pad.x, rect.bottom() - pad.y - 2.0),
            egui::pos2(rect.right() - pad.x, rect.bottom() - pad.y + 1.0),
        );
        painter.rect_filled(track, 0.0, Color32::from_white_alpha((50.0 * alpha) as u8));
        let done = egui::Rect::from_min_max(track.min, egui::pos2(track.left() + track.width() * p.clamp(0.0, 1.0), track.bottom()));
        painter.rect_filled(done, 0.0, Color32::from_rgba_unmultiplied(120, 170, 255, (230.0 * alpha) as u8));
    }
    rect
}

/// Clipping mask as in common photo apps (J): blown highlights red, crushed shadows blue.
fn clipping_mask(rgb: &[u8], width: usize, height: usize) -> ColorImage {
    let pixels = rgb
        .chunks_exact(3)
        .map(|p| {
            if p.iter().any(|&v| v >= 254) {
                Color32::from_rgb(255, 40, 40)
            } else if p.iter().all(|&v| v <= 1) {
                Color32::from_rgb(40, 90, 255)
            } else {
                Color32::TRANSPARENT
            }
        })
        .collect();
    ColorImage { size: [width, height], source_size: egui::vec2(width as f32, height as f32), pixels }
}

/// Histogram of the 8-bit preview image per channel.
fn rgb_histogram(rgb: &[u8]) -> [Vec<u32>; 3] {
    let mut h: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0; 256]);
    for p in rgb.chunks_exact(3) {
        h[0][p[0] as usize] += 1;
        h[1][p[1] as usize] += 1;
        h[2][p[2] as usize] += 1;
    }
    h
}

fn histogram(ui: &mut egui::Ui, h: &[Vec<u32>; 3], height: f32, sqrt: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(20));
    // Do not use the outermost bins (pure black/white) for scaling
    let max = h.iter().flat_map(|v| v[1..v.len() - 1].iter()).copied().max().unwrap_or(1).max(1) as f32;
    let colors = [
        Color32::from_rgba_unmultiplied(255, 80, 80, 150),
        Color32::from_rgba_unmultiplied(80, 255, 80, 150),
        Color32::from_rgba_unmultiplied(90, 140, 255, 150),
    ];
    for (c, bins) in h.iter().enumerate() {
        let n = bins.len();
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(n + 2);
        pts.push(rect.left_bottom());
        for (i, &v) in bins.iter().enumerate() {
            let f = v as f32 / max;
            let f = if sqrt { f.sqrt() } else { f };
            let x = rect.left() + rect.width() * i as f32 / (n - 1) as f32;
            pts.push(egui::pos2(x, rect.bottom() - rect.height() * f.min(1.0)));
        }
        pts.push(rect.right_bottom());
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.3, colors[c])));
    }
    // Clipping hint: share of pure highlights/shadows
    let total: u32 = h[1].iter().sum::<u32>().max(1);
    let hi = h.iter().map(|v| v[v.len() - 1]).max().unwrap_or(0) as f32 / total as f32 * 100.0;
    if hi > 0.1 {
        painter.text(rect.right_top() + egui::vec2(-6.0, 4.0), egui::Align2::RIGHT_TOP, crate::trf!("Lichter {hi} %", hi = format!("{:.1}", hi)), egui::FontId::proportional(10.0), Color32::from_gray(170));
    }
}

/// Is region `inner` (x, y, w, h) entirely inside `outer`?
fn contains(outer: [usize; 4], inner: [usize; 4]) -> bool {
    inner[0] >= outer[0] && inner[1] >= outer[1] && inner[0] + inner[2] <= outer[0] + outer[2] && inner[1] + inner[3] <= outer[1] + outer[3]
}

/// Shorten paths in the log to the file name: "open /Users/x/Photos/a.RAF: …" → "open a.RAF: …".
/// A path starts at the start of a word with "/", a UNC prefix or a drive ("C:", then a separator); it runs over
/// single spaces as long as the next word still holds a separator (folders with spaces: "/Users/x/My Photos/a.RAF")
/// and ends before trailing punctuation, which stays.
fn shorten_paths(msg: &str) -> String {
    let is_sep = |c: char| c == '/' || c == '\\';
    // a path starts at byte `i` of `msg`
    let starts = |i: usize| -> bool {
        let before_ok = msg[..i].chars().next_back().is_none_or(|c| c.is_whitespace() || "([\"'=:".contains(c));
        let rest = &msg[i..];
        let mut ch = rest.chars();
        let drive = matches!((ch.next(), ch.next(), ch.next()), (Some(d), Some(':'), Some(s)) if d.is_ascii_alphabetic() && is_sep(s));
        before_ok && (rest.starts_with('/') || rest.starts_with("\\\\") || drive)
    };
    let word_end = |i: usize| msg[i..].find(char::is_whitespace).map_or(msg.len(), |e| i + e);
    let trim = |t: &str| t.trim_end_matches(|c| ":,;)]\"'".contains(c)).len();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < msg.len() {
        if !starts(i) {
            let c = msg[i..].chars().next().expect("inside the text");
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let mut end = word_end(i);
        // over single spaces while the next word is still part of the path (holds a separator, starts no new path)
        while trim(&msg[i..end]) == end - i && msg[end..].starts_with(' ') && !msg[end + 1..].starts_with(char::is_whitespace) {
            let next = end + 1;
            let next_end = word_end(next);
            if next_end == next || starts(next) || !msg[next..next_end].contains(is_sep) {
                break;
            }
            end = next_end;
        }
        let token = &msg[i..end];
        let core = &token[..trim(token)];
        out.push_str(core.rsplit(is_sep).next().unwrap_or(""));
        out.push_str(&token[core.len()..]);
        i = end;
    }
    out
}

/// Trace log: with `GRAINEO_LOG` on stderr (complete), additionally in
/// `~/Library/Logs/graineo/run.log` (recreated at every start, at most ≈ 2 MB) – so that
/// a hang can be traced afterwards. The file on disk only contains file names,
/// no folder paths; detailed (full paths) only with `GRAINEO_LOG` or in the test data folder.
/// Lightroom start values of an image: Upright from Lightroom becomes our own straightening in the same mode
/// (Lightroom's solution is not read). A virtual copy starts from its own Lightroom settings, if it has any.
fn resolve_lr_start(loaded: &Loaded, copy: Option<&str>) -> Option<Settings> {
    let own = copy.and_then(|k| loaded.previews.foreign.as_ref().and_then(|d| rawdev::lightroom::copy_settings(d, k)));
    let mut lr_s = own.or_else(|| loaded.lr_start.clone())?;
    if let Some(mode) = std::mem::take(&mut lr_s.raw.upright_from_lr).and_then(|m| match m {
        1 | 4 => Some(develop::upright::Mode::Auto),
        2 => Some(develop::upright::Mode::Level),
        3 => Some(develop::upright::Mode::Vertical),
        _ => None,
    }) {
        let ([fw, fh], cam) = (loaded.full_size, loaded.cal.orientation);
        let base = Frame { angle: lr_s.crop_angle, ..Frame::full_settings(&lr_s, fw, fh, cam) };
        let segs = develop::upright::detect(&loaded.previews.developed.lin, &base);
        match develop::upright::solve(&segs, &base, mode) {
            Ok(u) => geometry::set_transform(&mut lr_s, u.angle, u.pv, u.ph, fw, fh, cam),
            Err(e) => log(&format!("Upright from Lightroom: {e}")),
        }
    }
    Some(lr_s)
}

/// Test hook `GRAINEO_HOVER_TEST` (see `film_look::hover_test`): logs every change of what the large image shows
/// (current state or hover preview), with the time since the first call.
fn hover_test_log(shown: Option<String>) {
    static LAST: Mutex<Option<(std::time::Instant, String)>> = Mutex::new(None);
    if crate::env::var_os("GRAINEO_HOVER_TEST").is_none() || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return;
    }
    let Some(shown) = shown else { return };
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let start = g.as_ref().map_or_else(std::time::Instant::now, |(t, _)| *t);
    if g.as_ref().is_none_or(|(_, s)| *s != shown) {
        log(&format!("hover test {:.3}s: image shows {shown}", start.elapsed().as_secs_f64()));
        *g = Some((start, shown));
    }
}

fn log(msg: &str) {
    use std::io::Write;
    static FILE: std::sync::OnceLock<Option<Mutex<(std::fs::File, usize)>>> = std::sync::OnceLock::new();
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 100_000_000;
    if crate::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("[{t}] {msg}");
    }
    let file = FILE.get_or_init(|| {
        let (dir, name) = crash_log::folder()?;
        std::fs::create_dir_all(&dir).ok()?;
        // the previous run's log stays (a crash there is otherwise lost at the next start)
        crash_log::keep_previous(&dir, name);
        std::fs::File::create(dir.join(name)).ok().map(|f| Mutex::new((f, 0)))
    });
    if let Some(m) = file {
        if let Ok(mut g) = m.lock() {
            if g.1 < 2_000_000 {
                // Unless explicitly enabled, the log only contains file names, no folders (they reveal names)
                let verbose = crate::env::var_os("GRAINEO_LOG").is_some() || crate::env::var_os("GRAINEO_DATA_DIR").is_some();
                let msg = if verbose {
                    let home = sysdirs::home().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
                    if home.len() > 1 { msg.replace(&home, "~") } else { msg.to_string() }
                } else {
                    shorten_paths(msg)
                };
                let line = format!("[{t}] {msg}\n");
                g.1 += line.len();
                let _ = g.0.write_all(line.as_bytes());
            }
        }
    }
}

/// Apply the automatic look to `s` (SPP preview if the settings use it).
fn auto_tone(previews: &jobs::Previews, cal: &develop::Calibration, s: &mut Settings) -> bool {
    let preview = match &previews.spp {
        Some(spp) if develop::Session::uses_spp(s) => spp,
        _ => &previews.developed,
    };
    match develop::auto::auto_look(preview, cal, s) {
        Some(x) => {
            develop::auto_look::apply(&x, s);
            true
        }
        None => false,
    }
}

/// Shooting data as a two-column list (name dimmed, value on the right), values copyable.
fn info_grid(ui: &mut egui::Ui, info: &[(String, String)]) {
    for (k, v) in info {
        lr::kv_row(ui, crate::tr(k), crate::tr(v), false);
    }
}

/// Display name of a Sigma colour mode (CAMF name → localised name or Sigma's spelling).
fn mode_label(m: &str) -> String {
    match m {
        "Standard" => "Standard",
        "Vivid" => crate::tr("Lebendig"),
        "Neutral" => "Neutral",
        "Portrait" => crate::tr("Porträt"),
        "Landscape" => crate::tr("Landschaft"),
        "Monochrome" => crate::tr("Monochrom"),
        "FCBlue" => "FOV Classic Blue",
        "FCYellow" => "FOV Classic Yellow",
        "SunsetRed" => "Sunset Red",
        "ForestGreen" => "Forest Green",
        "TealAndOrange" => "Teal & Orange",
        "" => crate::tr("Wie Aufnahme"),
        develop::settings::LINEAR_MODE => crate::tr("Linear (Szene)"),
        other => other,
    }
    .to_string()
}

/// The photo to precompute the Sigma base for: the next visible one in the direction of travel (`dir` > 0 forward,
/// otherwise backward), `None` at the edge. `visible` is in ascending order.
fn next_in_direction(visible: &[usize], current: usize, dir: isize) -> Option<usize> {
    if dir >= 0 {
        visible.iter().copied().find(|&i| i > current)
    } else {
        visible.iter().copied().rev().find(|&i| i < current)
    }
}

#[cfg(test)]
mod direction_tests {
    use super::next_in_direction;

    #[test]
    fn spp_prefetch_follows_direction() {
        let vis = [0, 2, 3, 7, 9];
        assert_eq!(next_in_direction(&vis, 3, 1), Some(7));
        assert_eq!(next_in_direction(&vis, 3, -1), Some(2));
        assert_eq!(next_in_direction(&vis, 4, 1), Some(7));
        assert_eq!(next_in_direction(&vis, 4, -1), Some(3));
        assert_eq!(next_in_direction(&vis, 9, 1), None);
        assert_eq!(next_in_direction(&vis, 0, -1), None);
        assert_eq!(next_in_direction(&[], 0, 1), None);
    }
}

#[cfg(test)]
mod log_tests {
    use super::shorten_paths;

    #[test]
    fn log_names_only_file_names() {
        assert_eq!(shorten_paths("öffnen /Users/name/Fotos/IMG_1.CR3: Kamera-RAW"), "öffnen IMG_1.CR3: Kamera-RAW");
        assert_eq!(shorten_paths("export /Users/c/a.jpg: Metadaten ok"), "export a.jpg: Metadaten ok");
        assert_eq!(shorten_paths("Fehler (/Volumes/nas/x/y.RAF)"), "Fehler (y.RAF)");
        // Fractions and ratios stay
        assert_eq!(shorten_paths("Zeile 3/4, Stufe 1/2"), "Zeile 3/4, Stufe 1/2");
        assert_eq!(shorten_paths("ohne Pfad"), "ohne Pfad");
        // folders with spaces and Windows paths: no folder name is left
        assert_eq!(shorten_paths("focus /Users/x/Meine Fotos/a.CR3: 3 field(s)"), "focus a.CR3: 3 field(s)");
        assert_eq!(shorten_paths("öffnen C:\\Users\\x\\Mein Ordner\\foto.raw: ok"), "öffnen foto.raw: ok");
        assert_eq!(shorten_paths("Fehler (D:/Bilder/Neue Fotos/b.NEF)"), "Fehler (b.NEF)");
        assert_eq!(shorten_paths("netz \\\\nas\\Fotos\\c.ARW fertig"), "netz c.ARW fertig");
        // a file name with a space keeps its second word; two paths stay two
        assert_eq!(shorten_paths("öffnen /a/b/Mein Foto.jpg"), "öffnen Mein Foto.jpg");
        assert_eq!(shorten_paths("von /a/b.jpg nach /c/d.jpg"), "von b.jpg nach d.jpg");
        assert_eq!(shorten_paths("kopiert /a/b.jpg /c/d.jpg"), "kopiert b.jpg d.jpg");
        assert_eq!(shorten_paths("Verhältnis 16:9, Fläche 3/4"), "Verhältnis 16:9, Fläche 3/4");
    }
}

/// Translate a template with values: `trf!("{n} Fotos", n = n)` (German like `format!`, English from `locales/en-ui.tsv`).
#[macro_export]
macro_rules! trf {
    ($t:literal $(, $k:ident = $v:expr)* $(,)?) => {
        $crate::menu::i18n::fill($crate::menu::i18n::tr($t), &[ $( (stringify!($k), &($v).to_string()) ),* ])
    };
}

/// Short form for display texts: `crate::tr("Deutsch")` (see `i18n/ui.rs`).
pub(crate) use menu::i18n::tr;

#[cfg(test)]
mod view_tests {
    use super::*;

    #[test]
    fn tilted_frame_is_upright_in_crop_view() {
        let f = Frame { cx: 300.0, cy: 200.0, w: 400.0, h: 250.0, angle: 12.0, q: 0, sw: 800.0, sh: 500.0, pv: 0.0, ph: 0.0 };
        let view_rect = ViewTransform::new(egui::pos2(400.0, 250.0), f.angle);
        let c = f.corners_ori().map(|p| view_rect.forward(egui::pos2(p[0], p[1])));
        assert!((c[0].y - c[1].y).abs() < 1e-3 && (c[3].y - c[2].y).abs() < 1e-3, "top/bottom horizontal: {c:?}");
        assert!((c[0].x - c[3].x).abs() < 1e-3 && (c[1].x - c[2].x).abs() < 1e-3, "left/right vertical: {c:?}");
        // and back
        let p = egui::pos2(123.0, 321.0);
        let q = view_rect.back(view_rect.forward(p));
        assert!((q - p).length() < 1e-3);
    }
}

#[cfg(test)]
mod mix_tests {
    use super::*;

    #[test]
    fn sum_stays_one() {
        let mut m = [0.33, 0.33, 0.34];
        for (i, v) in [(2, 0.97), (0, -0.5), (1, 1.4), (2, 0.0), (0, 1.0), (1, 0.5)] {
            m = set_mix(m, i, v);
            assert!((m.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{m:?}");
            assert!(m.iter().all(|x| (MIX_MIN - 1e-4..=MIX_MAX + 1e-4).contains(x)), "{m:?}");
        }
    }
}

/// Windows: identifier (HWND) of the main window for the menu bar; none elsewhere.
fn native_window_handle(cc: &eframe::CreationContext) -> Option<isize> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match cc.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cc;
        None
    }
}

/// ⌘-click in a single view (filmstrip): what happens to the selection.
#[derive(Debug, PartialEq)]
enum CmdClick {
    /// a photo other than the active one: add to / remove from the selection
    Toggle,
    /// the active photo of a multiple selection: leaves it, this one becomes active
    Next(usize),
    /// the active photo as the only one: nothing (there is always an active photo)
    Nothing,
}

/// `selected`: the selected photos in display order, the active one included.
fn cmd_click_on_active(i: usize, current: Option<usize>, selected: &[usize]) -> CmdClick {
    if current != Some(i) {
        return CmdClick::Toggle;
    }
    let pos = selected.iter().position(|&v| v == i);
    let after = pos.and_then(|p| selected[p + 1..].first().copied());
    let before = pos.and_then(|p| selected[..p].last().copied());
    match after.or(before) {
        Some(n) => CmdClick::Next(n),
        None => CmdClick::Nothing,
    }
}

#[cfg(test)]
mod cmd_click_tests {
    use super::*;
    #[test]
    fn cmd_click_rules_of_the_single_views() {
        // another photo: toggle
        assert_eq!(cmd_click_on_active(3, Some(1), &[1]), CmdClick::Toggle);
        // the active one of several: the next selected becomes active, at the end the previous one
        assert_eq!(cmd_click_on_active(2, Some(2), &[1, 2, 5]), CmdClick::Next(5));
        assert_eq!(cmd_click_on_active(5, Some(5), &[1, 2, 5]), CmdClick::Next(2));
        // the only one: nothing
        assert_eq!(cmd_click_on_active(2, Some(2), &[2]), CmdClick::Nothing);
    }
}

/// The zoom field shows only the percentage and keeps one width at every zoom step.
#[cfg(test)]
mod zoom_field_tests {
    use super::*;

    #[test]
    fn zoom_field_keeps_its_width_at_every_step() {
        let ctx = egui::Context::default();
        lr::apply_theme(&ctx);
        let mut widths = Vec::new();
        for (fit, fit_pct, pct) in [(true, 23.0, 23.0), (true, 7.0, 7.0), (false, 23.0, 100.0), (false, 23.0, 400.0), (false, 23.0, 1600.0)] {
            let label = zoom_label(fit, fit_pct, pct);
            assert!(!label.contains(crate::tr("Einpassen")), "{label}");
            let mut w = 0.0;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 200.0))), ..Default::default() };
            let _ = crate::ui_test::run(&ctx, input, |ui| {
                lr::field_style(ui);
                let field = zoom_field_width(ui);
                let r = egui::ComboBox::from_id_salt("zoom-test").selected_text(label.clone()).width(field).show_ui(ui, |_| {});
                w = r.response.rect.width();
                let text = ui.painter().layout_no_wrap(label.clone(), egui::TextStyle::Button.resolve(ui.style()), egui::Color32::WHITE).size().x;
                assert!(text + 16.0 <= field, "{label} does not fit {field}");
            });
            widths.push(w);
        }
        assert!(widths.windows(2).all(|p| (p[0] - p[1]).abs() < 0.5), "{widths:?}");
    }
}
