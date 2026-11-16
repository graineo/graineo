//! Background work. Each task runs on a kept worker thread of its lane ([`crate::threads`]) and reports its
//! result back via a channel; the UI is repainted afterwards.
//!
//! Where jobs wait for each other: at the gates of `pools::gate` (visible before soon before idle; a newer waiter
//! with the same key supersedes an older one). Whether a job or its result still counts, four things decide:
//! - `generation`/`current` (`App::open_generation`): the image changed – the job is dropped;
//! - [`EditStamp`] (`App::edit_generation`): the settings of the shown photo changed – crop and whole image compute
//!   nothing more and are not shown;
//! - `cancel` (preloading, Sigma base): checked inside the computation itself, so it can stop half-way;
//! - `epoch` and [`Origin`]: the list was rebuilt – indices are mapped, results of another photo dropped.
//!
//! Every job the app waits for sends an end message when it is superseded, outdated or crashes
//! ([`terminal_result`]), so that no waiting state stays set.

use crate::menu::i18n::DeComma as _;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// "Abbrechen" next to the export progress: the export stops before the next photo (the one being written is finished).
pub static EXPORT_CANCEL: AtomicBool = AtomicBool::new(false);
/// Previews re-rendered after a multi-selection edit (measurements, load tests).
pub static THUMBS_RENDERED: AtomicU64 = AtomicU64::new(0);
use std::sync::{Arc, Mutex};

use develop::export::{Engine, Format};
use develop::session::Preview;
use develop::{Session, Settings, View};

/// Longest edge of the preview in pixels.
pub const PREVIEW_EDGE: usize = 2400;
const HISTOGRAM_BINS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactTarget { Paper, Recipes, Presets }

/// Longest edge of the photos in the contact print (frame width on export).
pub const PRINT_EDGE: usize = 800;

pub enum Job {
    ExportLut { settings:develop::film::FilmSettings,name:String,path:PathBuf },
    /// LUT of the look chosen for a RAW photo (33³, the table of the development)
    ExportLookLut { doc:Arc<rawdev::RawDoc>,settings:Settings,name:String,path:PathBuf },
    ScanBounds { key:u64, image:i64,path:PathBuf,previews:Previews,cal:Arc<develop::Calibration>,settings:Settings },
    ToneCurve { settings: develop::film::FilmSettings, key: String },
    FilmProbe { settings: develop::film::FilmSettings, key: u64 },
    /// `generation` is incremented on the next image change; outdated loads abort.
    Open { index: usize, path: PathBuf, settings: Settings, generation: usize, current: Arc<AtomicUsize> },
    /// Prepare the neighbouring image (aborted as soon as `cancel` is set)
    Prefetch { index: usize, path: PathBuf, settings: Settings, cancel: Arc<std::sync::atomic::AtomicBool> },
    Prepare { index: usize, session: Arc<Mutex<Session>>, settings: Settings },
    /// `before`: before view (default settings) instead of the current development
    Render { index: usize, previews: Previews, cal: Arc<develop::Calibration>, settings: Settings, before: bool },
    /// Preview (`region: None`) or crop at full resolution with preserved gamut for display (photo texture, see
    /// `photo_display.rs`)
    RenderWide { index: usize, doc: Arc<rawdev::RawDoc>, settings: Settings, region: Option<[usize; 4]> },
    /// Sigma preview with preserved gamut for display (photo texture), from the same preview as [`Job::Render`]
    RenderWideSigma { index: usize, previews: Previews, cal: Arc<develop::Calibration>, settings: Settings },
    CameraJpeg { index: usize, path: PathBuf },
    /// Compute Sigma's exact base (≈ 3 s, without locking the session)
    /// `prefetch`: compute ahead for the next image
    /// `session = None`: start immediately on image change (in parallel with opening) – the result
    /// arrives as [`JobResult::SppCounts`] and is installed later with [`Job::SppInstall`].
    Spp { index: usize, path: PathBuf, session: Option<Arc<Mutex<Session>>>, mode: develop::SppKey, cancel: Arc<std::sync::atomic::AtomicBool>, prefetch: bool },
    SppInstall { index: usize, session: Arc<Mutex<Session>>, mode: develop::SppKey, counts: develop::Linear, seconds: f32 },
    /// Crop at full resolution: x, y, width, height in image pixels
    /// `generation`/`current` as with `Open`: after an image change the job is dropped; `edit`: after a settings change
    /// it computes nothing more (answer `RegionFailed { superseded: true }`)
    /// Non-Sigma RAW: `session = None`, computed from `foreign`
    RenderRegion { index: usize, session: Option<Arc<Mutex<Session>>>, foreign: Option<Arc<rawdev::RawDoc>>, settings: Settings, region: [usize; 4], factor: usize, generation: usize, current: Arc<AtomicUsize>, edit: EditStamp },
    /// Whole image at full resolution for the zoomed view: rendered once per settings state, then the view draws
    /// every zoom and pan from it. Same path as the export (`render_full`). `edit`: the settings state it was sent
    /// for – once the settings changed it is dropped (`FullFailed { outdated: true }`), before or after computing;
    /// `generation`/`current` as with `RenderRegion`.
    RenderFull { index: usize, session: Option<Arc<Mutex<Session>>>, foreign: Option<Arc<rawdev::RawDoc>>, settings: Settings, generation: usize, current: Arc<AtomicUsize>, edit: EditStamp },
    /// Zoomed view with preserved gamut (see `photo_display.rs`): the whole image (`region: None`, like
    /// [`Job::RenderFull`]) or a crop (like [`Job::RenderRegion`] at 100 %), uploaded as photo textures. Where there
    /// is no wide output (film, monochrome, Classic, a crop with local adjustments) it runs as the plain job and
    /// answers like it. `edit`, `generation`/`current` as with the plain jobs (outdated: dropped before or after
    /// computing).
    RenderDetailWide { index: usize, session: Option<Arc<Mutex<Session>>>, foreign: Option<Arc<rawdev::RawDoc>>, settings: Settings, region: Option<[usize; 4]>, generation: usize, current: Arc<AtomicUsize>, edit: EditStamp },
    /// Prepare the full resolution of a non-Sigma RAW in the background (after a dwell, so that zooming is sharp at
    /// once); dropped when the image changes.
    PrepareFull { index: usize, doc: Arc<rawdev::RawDoc>, settings: Settings, generation: usize, current: Arc<AtomicUsize> },
    /// per image: file, settings, library image ID (keywords), rating
    /// `gain_map`: pass the HDR gain map of the originals (iPhone etc.) on to JPEGs where the geometry fits
    Export { items: Vec<(PathBuf, Settings, i64, crate::sidecar::Meta)>, format: Format, engine: Engine, dir: PathBuf, gain_map: bool, allow_missing_profiles: bool, as_print: bool, color_space: graineo_color::Space, options: develop::export::Options },
    /// Export "Original": the files unchanged, verified copies (path, image ID, image ID of the stacked camera JPEG
    /// to take along); `sidecars`: an XMP next to each copy
    ExportOriginal { items: Vec<(PathBuf, i64, Option<i64>)>, dir: PathBuf, sidecars: bool },
    /// Metering area statistics on the raw data (rectangle in the active image area)
    Stats { index: usize, session: Arc<Mutex<Session>>, rect: [usize; 4] },
    /// Write the three layers as linear 16-bit TIFFs
    ExportLayers { session: Arc<Mutex<Session>>, dir: PathBuf, flat_field: bool },
    /// Preview of a preset on hover (for the navigator, downscaled). `supersede`: a newer hover preview of the same
    /// photo replaces this one while it waits (pointer moving over the film list: only the newest is computed, the
    /// others answer [`JobResult::HoverSuperseded`]); `false` for previews every one of which is wanted (compare panes,
    /// left side of the darkroom comparison).
    HoverRender { index: usize, previews: Previews, cal: Arc<develop::Calibration>, settings: Settings, supersede: bool },
    /// Read the CAMF block for the inspector
    Camf { index: usize, path: PathBuf },
    /// Small whole-image previews of the photo per film, Sigma colour mode or camera look ("Film & Look" tiles and the
    /// darkroom's film shelf, see `film_previews.rs`): one tile after the other, each sent at once; stops once
    /// `valid` != `key`. `background`: the quiet fill of tiles not visible yet (lane "back").
    FilmThumbs { index: usize, previews: Previews, cal: Arc<develop::Calibration>, settings: Settings, key: u64, modes: Vec<String>, valid: Arc<AtomicU64>, background: bool },
    /// Darkroom test strip: the same photo with several settings, one small image each
    Variants { index: usize, previews: Previews, cal: Arc<develop::Calibration>, list: Vec<(String, Settings)>, key: u64 },
    /// Darkroom contact sheet: the same photo with the settings of several rows, one
    /// larger image per cell; finished rows come back individually (progress)
    Contact { index: usize, previews: Previews, cal: Arc<develop::Calibration>, rows: Vec<(usize, Vec<(usize, Settings)>)>, key: u64, target: ContactTarget },
    /// Darkroom live markers: distribution of scene brightness in stops relative to middle grey
    SceneDistribution { index: usize, previews: Previews, cal: Arc<develop::Calibration>, settings: Settings, key: u64 },
    /// Contact print: several photos of the library (file, settings with the darkroom film), each
    /// is loaded and developed and comes back individually; `valid` carries the key of the current print
    /// (a newer key aborts this job)
    Print { images: Vec<(PathBuf, Settings)>, key: u64, valid: Arc<std::sync::atomic::AtomicU64>, previews: Option<(Arc<library::preview::Cache>, Vec<Option<u64>>)> },
    /// New previews after a multi-edit: develop each photo (image ID, file ID, path, settings)
    /// with crop and rotation; a newer job (`valid` ≠ `key`) supersedes this one
    /// `large`: also renew the large preview of photos on other drives (a few edited photos; not for thousands)
    Thumbnails { images: Vec<(i64, i64, PathBuf, Settings)>, key: u64, valid: Arc<std::sync::atomic::AtomicU64>, large: bool },
}

/// Preview of the denoised image and of the unprocessed sensor image.
#[derive(Clone)]
pub struct Previews {
    pub developed: Arc<Preview>,
    pub sensor: Arc<Preview>,
    pub denoise_key: Option<(u8, u8)>,
    /// Preview from Sigma's exact base, once computed
    pub spp: Option<Arc<Preview>>,
    /// Raw data (shared with the session) for the "Raw data" stage of the pipeline inspection
    pub raw: Arc<x3f::RawImage>,
    /// Non-Sigma RAW (CR2/CR3/RAF/DNG/ARW …): own chain (crates/rawdev); the fields above
    /// are then only placeholders (preview of the linear camera image for straightening/display)
    pub foreign: Option<Arc<rawdev::RawDoc>>,
}

/// A loaded image – for display or for caching.
pub struct Loaded {
    /// Sigma session; `None` for non-Sigma RAWs (see [`Previews::foreign`])
    pub session: Option<Arc<Mutex<Session>>>,
    pub cal: Arc<develop::Calibration>,
    pub previews: Previews,
    pub histograms: [Vec<u32>; 3],
    pub info: Vec<(String, String)>,
    pub full_size: [usize; 2],
    pub camera_mode: String,
    pub saturation: f32,
    /// Short info for the info line
    pub summary: String,
    /// Sigma file without own edit in graineo: starts with the settings SIGMA Photo Pro stored in the X3F
    pub start_values_from_spp: bool,
    /// Non-Sigma RAW: initial values from Lightroom (catalog or XMP), if present
    pub lr_start: Option<Settings>,
    /// Already developed while preloading: (settings, width, height, RGB) – then the image appears without delay when clicked
    pub prerendered: Option<(Settings, usize, usize, Vec<u8>)>,
    /// loaded file and chosen path (from [`crate::image_kind`]) – the app uses this to check that the
    /// result belongs to the image at this index
    pub path: PathBuf,
    pub kind: crate::image_kind::ImageKind,
    /// Damaged Sigma file, partly rescued: shares of the rescue (see [`crate::photo_state::damaged`])
    pub damage: Option<develop::damaged::Damage>,
}

/// Message for a damaged, partly rescued Sigma file (see `develop::damaged`).
pub fn damage_note(d: &develop::damaged::Damage) -> String {
    let pct = |x: f64| ((x * 100.0).round() as i64).to_string();
    let mut parts = vec![crate::trf!("{a} % gerettet", a = pct(d.intact))];
    if d.estimated > 0.0 {
        parts.push(crate::trf!("{a} % aus den übrigen Schichten geschätzt", a = pct(d.estimated)));
    }
    if d.missing > 0.0 {
        parts.push(crate::trf!("{a} % fehlen (grau)", a = pct(d.missing)));
    }
    crate::trf!(
        "{head} – {parts}. Teile der Datei fehlen oder sind überschrieben, z. B. durch eine fehlerhafte Speicherkarte.",
        head = damaged_head(),
        parts = parts.join(", ")
    )
}

/// Message for a Sigma file whose RAW data is damaged (see [`x3f::Error::Damaged`]). Starts with
/// [`damaged_head`], so that [`is_damaged`] recognises it.
fn damaged_text(plane: usize, first_row: Option<usize>, rows: usize) -> String {
    // German source text: the message is kept (photo condition, log) and translated where it is shown (`tr_msg`)
    use crate::menu::i18n::{de, fill_de as fill};
    let detail = match first_row {
        Some(row) => fill(de("Schicht {p} ab Zeile {r} von {n} unlesbar"), &[("p", &(plane + 1).to_string()), ("r", &(row + 1).to_string()), ("n", &rows.to_string())]),
        None => fill(de("Schicht {p} unvollständig"), &[("p", &(plane + 1).to_string())]),
    };
    fill(de("{head} ({detail}) – Teile der Datei fehlen oder sind überschrieben, z. B. durch eine fehlerhafte Speicherkarte. Gezeigt wird die Vorschau aus der Kamera."), &[("head", de("Datei beschädigt")), ("detail", &detail)])
}

fn damaged_head() -> String {
    crate::tr("Datei beschädigt").to_string()
}

/// Is `error` the message of a damaged Sigma file (also x3fuse-core's decoding errors of the exact base)?
pub fn is_damaged(error: &str) -> bool {
    error.starts_with("Datei beschädigt") || error.starts_with(&damaged_head()) || error.contains("invalid Huffman code") || error.contains("truncated entropy stream")
}

fn load(path: &PathBuf, settings: &Settings, keep_going: impl Fn() -> bool) -> Result<Option<Loaded>, String> {
    use crate::image_kind::ImageKind;
    // Apple Photos, photo cropped or rotated there: Apple's current version (as in the grid; the face boxes refer to it);
    // sidecar and list path stay with the photo of the Photos library
    if let Some(current) = library::apple_photos::current_version(path) {
        crate::log(&format!("open {}: current version from Apple Photos {}", path.display(), current.display()));
        crate::raw_app::register_raw(&current, path);
        return crate::raw_app::load(&current, settings, &keep_going);
    }
    // Apple Photos: original only in iCloud → working copy in the cache; sidecar and list path
    // stay with the photo of the Photos library
    if library::apple_photos::in_photos_library(path) && !path.is_file() {
        let original_copy = crate::apple_photos_ui::original(path)?;
        if !keep_going() {
            return Ok(None);
        }
        crate::raw_app::register_raw(&original_copy, path);
        return crate::raw_app::load(&original_copy, settings, &keep_going);
    }
    let kind = crate::image_kind::of(path);
    crate::log(&format!("open {}: {}", path.display(), kind.name()));
    match kind {
        ImageKind::Raw | ImageKind::Done => return crate::raw_app::load(path, settings, &keep_going),
        ImageKind::Other => return Err(crate::trf!("{a}: lässt sich nicht bearbeiten", a = path.display())),
        ImageKind::Foveon => {}
    }
    let session = Session::open_for(path, settings).map_err(|e| match e {
        x3f::Error::Damaged { plane, first_row, rows } => damaged_text(plane, first_row, rows),
        e => e.to_string(),
    })?;
    if !keep_going() {
        return Ok(None);
    }
    // Classic: the exact view alongside the own previews (both from the finished preparation)
    let (mut previews, exact) = rayon::join(|| previews(&session, settings), || classic_exact_preview(&session, settings));
    if !keep_going() {
        return Ok(None);
    }
    previews.spp = exact;
    let histograms = develop::layer_histograms(&previews.sensor.lin, HISTOGRAM_BINS);
    let p = &session.props;
    let get = |k: &str| p.get(k).unwrap_or("–").to_string();
    let program = match p.get("PMODE").unwrap_or("") {
        "P" => "Programmautomatik".to_string(),
        "A" => "Zeitautomatik (A)".to_string(),
        "S" => "Blendenautomatik (S)".to_string(),
        "M" => "Manuell".to_string(),
        "" => "–".to_string(),
        other => other.to_string(),
    };
    // the lens itself is named in METADATEN; here only its aperture range
    let aperture_range = match p.get("LENSARANGE") {
        Some(a) if !a.trim().is_empty() => format!("f/{}", a.trim().replace(" to ", " – ")).de_comma(),
        _ => "–".to_string(),
    };
    let mut info = vec![
        ("Kamera".into(), get("CAMMODEL")),
        ("Aufnahme".into(), p.get("TIME").and_then(|t| t.trim().parse::<i64>().ok()).map_or("–".to_string(), camera_time)),
        ("Belichtung".into(), format!("{} s · f/{} · ISO {}", get("SH_DESC"), get("AP_DESC"), get("ISO")).de_comma()),
        ("Programm".into(), program),
        ("Korrektur".into(), p.get("EXPCOMP").filter(|v| !v.trim().is_empty()).map_or("–".into(), |v| format!("{} EV", v.trim()).de_comma())),
        ("Brennweite".into(), format!("{} mm ({} mm KB)", get("FLENGTH"), get("FLEQ35MM")).de_comma()),
        ("Blendenbereich".into(), aperture_range),
        ("Fokus · Blitz".into(), format!("{} · {}", get("AFMODE"), match p.get("FLASH").unwrap_or("–") { "OFF" => "aus", "ON" => "an", o => o })),
        ("Weißabgleich".into(), get("WB_DESC")),
        ("Farbmodus Kamera".into(), get("CM_DESC")),
        ("Sensortemperatur".into(), format!("{} °C", get("IMAGERTEMP"))),
        if session.cal.is_quattro() {
            ("Aufbereitung".into(), "x3fuse-core (Quattro 1:1:4, untere Schichten hochgerechnet)".into())
        } else if session.cal.is_classic() {
            ("Aufbereitung".into(), crate::tr("x3fuse-core (ältere Foveon-Generation, nicht in der Foveon-Verarbeitung)").into())
        } else {
            ("Schwarzwert".into(), format!("{:.1} / {:.1} / {:.1}", session.cal.black[0], session.cal.black[1], session.cal.black[2]).de_comma())
        },
        if session.cal.model == develop::calib::Model::SdQuattro {
            ("Farbkalibrierung".into(), crate::tr("Ersatzmatrix (keine Sigma-Farbkalibrierung für sd Quattro)").into())
        } else if session.cal.is_classic() {
            ("Farbkalibrierung".into(), "Kamera-Matrizen (Kalpanika), abgeglichen mit Kamera-JPEGs".into())
        } else {
            ("Defektpixel".into(), crate::trf!("{a} Einträge (Schwelle {b})", a = session.cal.bad_pixels.len(), b = session.cal.badness_threshold))
        },
        ("Sensorfläche".into(), {
            let ([w, h], raw) = (session.size(), session.raw());
            crate::trf!("{a} × {b} · Bild {w} × {h} × 3 Schichten", a = raw.columns, b = raw.rows, w = w, h = h)
        }),
        ("Firmware".into(), get("FIRMVERS")),
        ("Seriennummer".into(), get("CAMSERIAL")),
    ];
    let summary = format!("{} s · f/{} · ISO {} · {} mm", get("SH_DESC"), get("AP_DESC"), get("ISO"), get("FLEQ35MM"));
    // Initial values from SIGMA Photo Pro (stored in the X3F, no separate settings file)
    let start_values_from_spp = crate::sidecar::from_spp(path);
    if start_values_from_spp {
        info.push(("Startwerte".into(), "aus SIGMA Photo Pro".into()));
    }
    let full_size = session.size();
    if session.cal.binning > 1 {
        let [fw, fh] = session.cal.full_size();
        info.push(("Binning".into(), format!("×{} · {} × {} ({} × {})", session.cal.binning, full_size[0], full_size[1], fw, fh)));
    }
    let (camera_mode, saturation) = (session.cal.camera_mode.clone(), session.cal.saturation as f32);
    let damage = session.damage.as_ref().map(|(_, d)| d.clone());
    if let Some(d) = &damage {
        let note = damage_note(d);
        crate::log(&format!("{}: {note}", path.display()));
        info.push(("Zustand der Datei".into(), note));
    }
    Ok(Some(Loaded {
        cal: session.cal.clone(),
        session: Some(Arc::new(Mutex::new(session))),
        previews,
        histograms,
        info,
        full_size,
        camera_mode,
        saturation,
        summary,
        start_values_from_spp,
        lr_start: None,
        prerendered: None,
        path: path.clone(),
        kind: crate::image_kind::ImageKind::Foveon,
        damage,
    }))
}

pub enum JobResult {
    LutExported { result:Result<(),String> },
    /// `bounds: None`: not computed (crash) – the app takes nothing over and does not ask again for this key
    ScanBounds { key:u64,image:i64,path:PathBuf,bounds:Option<(f32,f32)> },
    ToneCurve { key: String, data: Result<Vec<develop::film::Series>,String> },
    FilmProbe { key: u64, image: Result<develop::Rgb16,String>, seconds: f32 },
    /// `prefetch`: prepared in the background, cache only
    Opened { index: usize, loaded: Loaded, prefetch: bool },
    RegionRendered { index: usize, settings: Settings, region: [usize; 4], factor: usize, width: usize, height: usize, rgb: Vec<u8> },
    /// Whole image at full resolution (`Job::RenderFull`), `seconds`: render time
    /// `half`: the same image at half the edge length (width, height, RGB), for views below ~75 %
    FullRendered { index: usize, settings: Settings, width: usize, height: usize, rgb: Vec<u8>, half: (usize, usize, Vec<u8>), seconds: f32 },
    /// `Job::RenderFull` failed (`outdated: false`: no new attempt for these settings) or was dropped because the
    /// settings or the image changed meanwhile (`outdated: true`: the app has already forgotten the job)
    /// `sent`: image change and edit generation the job was sent for (the app ends only the waiting state of exactly
    /// this job, see [`EditStamp::sent`])
    FullFailed { index: usize, settings: Settings, outdated: bool, sent: (usize, u64) },
    /// `Job::Render` could not be computed (crash): ends the waiting state, no new attempt for these settings
    RenderFailed { index: usize, settings: Settings, before: bool },
    /// `Job::Prepare` could not be computed (crash): ends the waiting state; the previews stay, marked as made for
    /// `denoise_key` so that the app does not ask again in a loop
    PrepareFailed { index: usize, denoise_key: Option<(u8, u8)> },
    OpenFailed { index: usize, error: String },
    /// Preloading an image failed (not an error for the display, just cleanup)
    PrefetchFailed { index: usize },
    Prepared { index: usize, previews: Previews },
    Rendered { index: usize, settings: Settings, before: bool, width: usize, height: usize, rgb: Vec<u8> },
    /// Response to `RenderWide`/`RenderWideSigma`: the preview (or crop) with preserved gamut as photo texture
    Wide { index: usize, settings: Settings, region: Option<[usize; 4]>, tex: Arc<crate::photo_display::PhotoTex> },
    /// Response to `RenderDetailWide`: the whole image (`full`) or a crop at full resolution with preserved gamut,
    /// as photo textures (tiles: texture and pixel rectangle in the rendered image); `half`: the whole image at half
    /// the edge length (width, height, tiles) for views below ~75 %
    DetailWide { index: usize, settings: Settings, region: [usize; 4], full: bool, size: [usize; 2], tiles: crate::photo_display::Tiles, half: Option<(usize, usize, crate::photo_display::Tiles)>, seconds: f32 },
    CameraJpeg { index: usize, width: usize, height: usize, rgb: Vec<u8> },
    SppReady { index: usize, mode: develop::SppKey, preview: Arc<Preview>, seconds: f32, prefetch: bool },
    SppCounts { index: usize, mode: develop::SppKey, counts: develop::Linear, seconds: f32 },
    SppFailed { index: usize, error: String, prefetch: bool },
    /// `failed`: this photo was not exported (`message` says why); an empty path: none was (destination folder)
    ExportProgress { done: usize, total: usize, message: String, failed: Option<PathBuf> },
    Stats { index: usize, rect: [usize; 4], stats: [develop::analysis::LayerStats; 3] },
    LayersExported { result: Result<PathBuf, String> },
    Camf { index: usize, camf: Result<Arc<x3f::Camf>, String> },
    HoverRendered { index: usize, settings: Settings, width: usize, height: usize, rgb: Vec<u8> },
    /// A hover preview was replaced by a newer one or could not be computed: whoever waits for exactly these settings
    /// forgets the request (the newest one is still on its way)
    HoverSuperseded { index: usize, settings: Settings },
    /// Tiles of [`Job::FilmThumbs`] (`None`: this tile failed); `end`: the job is done.
    FilmThumbs { index: usize, key: u64, thumbs: Vec<(String, Option<(usize, usize, Vec<u8>)>)>, end: bool, background: bool },
    /// Test strip: one small, upright image per variant (label)
    Variants { index: usize, key: u64, images: Vec<(String, usize, usize, Vec<u8>)> },
    /// Contact sheet: one finished row (number, per cell column and upright image); `end`: last row
    Contact { index: usize, key: u64, row: usize, fields: Vec<(usize, usize, usize, Vec<u8>)>, end: bool, target: ContactTarget },
    /// Number of pixels per half stop from −8 to +6 stops relative to middle grey (28 bins)
    SceneDistribution { index: usize, key: u64, bins: Vec<u32> },
    /// Crop at full resolution could not be computed (error or job crash) or was superseded at the gate by a newer
    /// crop of the same photo (`superseded`: no error, the app only releases the state and does not pause)
    RegionFailed { index: usize, superseded: bool },
    /// Contact print: the developed photo no. `nr` (`None` = not loadable); `end`: last or aborted
    Print { key: u64, nr: usize, image: Option<(usize, usize, Vec<u8>)>, end: bool },
    /// Contact print: quick stand-in (library preview, without film) for photo no. `nr` until the developed one arrives
    PrintPreview { key: u64, nr: usize, image: (usize, usize, Vec<u8>) },
    /// New preview of a photo after a multi-edit (image ID, file ID, width, height, RGB)
    Thumbnail { image: i64, file: i64, rgb: (usize, usize, Vec<u8>) },
}

/// Which photo a job works on, independent of its index in the list: the data of the loaded photo (address of its
/// calibration, RAW document or Sigma session, which every loaded photo has its own of) or the file. The app takes a
/// result only if it still belongs to the photo now at that index (see `App::origin_fits`) – an index alone once let
/// the result of one photo land on another.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Data(usize),
    Path(PathBuf),
}

fn data<T>(a: &Arc<T>) -> Origin {
    Origin::Data(Arc::as_ptr(a) as *const u8 as usize)
}

impl Origin {
    /// Origin of an index-bound job (`None`: none to check, e.g. opening – its result carries the path itself).
    fn of(job: &Job) -> Option<Origin> {
        match job {
            // the RAW document of another camera, else the raw data shared with the Sigma session (the
            // calibration may be shared between photos of one camera)
            Job::Render { previews, .. }
            | Job::HoverRender { previews, .. }
            | Job::FilmThumbs { previews, .. }
            | Job::Variants { previews, .. }
            | Job::Contact { previews, .. }
            | Job::SceneDistribution { previews, .. }
            | Job::ScanBounds { previews, .. }
            | Job::RenderWideSigma { previews, .. } => Some(previews.foreign.as_ref().map_or_else(|| data(&previews.raw), data)),
            Job::RenderWide { doc, .. } | Job::PrepareFull { doc, .. } => Some(data(doc)),
            Job::RenderRegion { session, foreign, .. } | Job::RenderFull { session, foreign, .. } | Job::RenderDetailWide { session, foreign, .. } => match (foreign, session) {
                (Some(d), _) => Some(data(d)),
                (None, Some(s)) => Some(data(s)),
                _ => None,
            },
            Job::Prepare { session, .. } | Job::SppInstall { session, .. } | Job::Stats { session, .. } => Some(data(session)),
            Job::Spp { session: Some(s), .. } => Some(data(s)),
            Job::Spp { path, session: None, .. } | Job::CameraJpeg { path, .. } | Job::Camf { path, .. } => Some(Origin::Path(path.clone())),
            _ => None,
        }
    }
}

/// Did a job with this origin work on these previews or this session (e.g. the second session of the binning
/// comparison, see `binning_ui::Old`)?
pub fn is_origin_of(o: &Origin, previews: &Previews, session: &Arc<Mutex<Session>>) -> bool {
    *o == data(&previews.raw) || *o == data(session)
}

impl Loaded {
    /// Is this the loaded photo a job with this origin worked on?
    pub fn is_origin(&self, o: &Origin) -> bool {
        match o {
            Origin::Data(_) => *o == data(&self.previews.raw) || self.previews.foreign.as_ref().is_some_and(|d| *o == data(d)) || self.session.as_ref().is_some_and(|s| *o == data(s)),
            Origin::Path(p) => *p == self.path,
        }
    }
}

thread_local! {
    /// Jobs sent from this thread so far (tests: a display-only change such as the colour proof starts none; per
    /// thread, so tests running in parallel do not count).
    pub static SENT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub struct Worker {
    tx: Sender<(u64, Option<Origin>, JobResult)>,
    rx: Receiver<(u64, Option<Origin>, JobResult)>,
    ctx: egui::Context,
    /// State of the image list when sent: results carry it along so that the app can map its indices
    /// to the list rebuilt in the meantime (otherwise e.g. a preloaded
    /// RAF lands on the index where an X3F now is)
    pub epoch: Arc<std::sync::atomic::AtomicU64>,
    /// Jobs sent and not finished yet (tests wait for 0: a result still on its way may start follow-up jobs).
    running: Arc<std::sync::atomic::AtomicUsize>,
}

impl JobResult {
    /// Index of the image in the list (for results bound to an image).
    pub fn index_mut(&mut self) -> Option<&mut usize> {
        match self {
            JobResult::FilmProbe { .. } | JobResult::ToneCurve { .. } | JobResult::ScanBounds { .. } | JobResult::LutExported { .. } => None,
            JobResult::Opened { index, .. }
            | JobResult::RegionRendered { index, .. }
            | JobResult::FullRendered { index, .. }
            | JobResult::FullFailed { index, .. }
            | JobResult::RenderFailed { index, .. }
            | JobResult::PrepareFailed { index, .. }
            | JobResult::OpenFailed { index, .. }
            | JobResult::PrefetchFailed { index }
            | JobResult::Prepared { index, .. }
            | JobResult::Rendered { index, .. }
            | JobResult::Wide { index, .. }
            | JobResult::DetailWide { index, .. }
            | JobResult::CameraJpeg { index, .. }
            | JobResult::SppReady { index, .. }
            | JobResult::SppCounts { index, .. }
            | JobResult::SppFailed { index, .. }
            | JobResult::Stats { index, .. }
            | JobResult::Camf { index, .. }
            | JobResult::HoverRendered { index, .. }
            | JobResult::HoverSuperseded { index, .. }
            | JobResult::FilmThumbs { index, .. }
            | JobResult::Variants { index, .. }
            | JobResult::RegionFailed { index, .. }
            | JobResult::SceneDistribution { index, .. } => Some(index),
            // Contact previews check their unique image/render keys themselves.
            // A list change must not discard their completion prematurely.
            JobResult::Contact { .. } | JobResult::Print { .. } | JobResult::PrintPreview { .. } | JobResult::Thumbnail { .. } | JobResult::ExportProgress { .. } | JobResult::LayersExported { .. } => None,
        }
    }
}

impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, ctx, epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)), running: Default::default() }
    }

    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// Next result with the list state its job was sent with and the photo it belongs to.
    pub fn try_recv(&self) -> Option<(u64, Option<Origin>, JobResult)> {
        self.rx.try_recv().ok()
    }

    /// Jobs of this worker sent and not finished yet.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    pub fn send(&self, job: Job) {
        SENT.with(|c| c.set(c.get() + 1));
        self.running.fetch_add(1, Ordering::SeqCst);
        let running = self.running.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let e = self.epoch.load(Ordering::SeqCst);
        crate::stress::count_job(matches!(job, Job::RenderRegion { .. } | Job::RenderDetailWide { region: Some(_), .. }), matches!(job, Job::RenderFull { .. } | Job::RenderDetailWide { region: None, .. }));
        let origin = Origin::of(&job);
        // If a job crashes, the app gets its terminal result (jobs with a waiting state)
        let on_crash = terminal_result(&job);
        crate::threads::spawn(lane(&job), move || {
            let send = |r: JobResult| {
                let _ = tx.send((e, origin.clone(), r));
                ctx.request_repaint();
            };
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job, &send))).is_err() {
                crate::log("background job crashed");
                if let Some(r) = on_crash {
                    send(r);
                }
            }
            running.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Terminal result a job with a waiting state in the app must send when it is aborted, superseded at a gate or
/// crashes – otherwise a marker (`detail_pending`, `full_job`, `rendering`, a busy flag of the darkroom, …) stays set
/// for ever. `None`: the job leaves no waiting state behind (reason at the arm). The match names every kind, so that a
/// new kind of job has to be sorted in here.
pub fn terminal_result(job: &Job) -> Option<JobResult> {
    let cancelled = || crate::tr("abgebrochen").to_string();
    match job {
        Job::RenderRegion { index, .. } => Some(JobResult::RegionFailed { index: *index, superseded: false }),
        Job::RenderFull { index, settings, generation, edit, .. } | Job::RenderDetailWide { index, settings, region: None, generation, edit, .. } => Some(JobResult::FullFailed { index: *index, settings: settings.clone(), outdated: false, sent: edit.sent(*generation) }),
        // the wide-gamut crop ends the same waiting state as the plain crop (`detail_pending`)
        Job::RenderDetailWide { index, .. } => Some(JobResult::RegionFailed { index: *index, superseded: false }),
        Job::HoverRender { index, settings, .. } => Some(JobResult::HoverSuperseded { index: *index, settings: settings.clone() }),
        Job::Spp { index, prefetch, .. } => Some(JobResult::SppFailed { index: *index, error: "abgebrochen".into(), prefetch: *prefetch }),
        // installing waits like the base itself (`spp_pending`, "Refining …")
        Job::SppInstall { index, .. } => Some(JobResult::SppFailed { index: *index, error: "abgebrochen".into(), prefetch: false }),
        Job::Prefetch { index, .. } => Some(JobResult::PrefetchFailed { index: *index }),
        Job::Open { index, .. } => Some(JobResult::OpenFailed { index: *index, error: "abgebrochen".into() }),
        Job::Render { index, settings, before, .. } => Some(JobResult::RenderFailed { index: *index, settings: settings.clone(), before: *before }),
        Job::Prepare { index, settings, .. } => Some(JobResult::PrepareFailed { index: *index, denoise_key: settings.denoise_key() }),
        Job::Print { key, .. } => Some(JobResult::Print { key: *key, nr: usize::MAX, image: None, end: true }),
        Job::Contact { index, key, target, .. } => Some(JobResult::Contact { index: *index, key: *key, row: 0, fields: Vec::new(), end: true, target: *target }),
        Job::Variants { index, key, .. } => Some(JobResult::Variants { index: *index, key: *key, images: Vec::new() }),
        Job::FilmThumbs { index, key, background, .. } => Some(JobResult::FilmThumbs { index: *index, key: *key, thumbs: Vec::new(), end: true, background: *background }),
        Job::SceneDistribution { index, key, .. } => Some(JobResult::SceneDistribution { index: *index, key: *key, bins: Vec::new() }),
        Job::ScanBounds { key, image, path, .. } => Some(JobResult::ScanBounds { key: *key, image: *image, path: path.clone(), bounds: None }),
        Job::ToneCurve { key, .. } => Some(JobResult::ToneCurve { key: key.clone(), data: Err(cancelled()) }),
        Job::FilmProbe { key, .. } => Some(JobResult::FilmProbe { key: *key, image: Err(cancelled()), seconds: 0.0 }),
        Job::ExportLut { .. } | Job::ExportLookLut { .. } => Some(JobResult::LutExported { result: Err(cancelled()) }),
        Job::ExportLayers { .. } => Some(JobResult::LayersExported { result: Err(cancelled()) }),
        Job::Camf { index, .. } => Some(JobResult::Camf { index: *index, camf: Err(cancelled()) }),
        Job::Export { items, .. } => Some(JobResult::ExportProgress { done: items.len(), total: items.len(), message: cancelled(), failed: Some(PathBuf::new()) }),
        Job::ExportOriginal { items, .. } => Some(JobResult::ExportProgress { done: items.len(), total: items.len(), message: cancelled(), failed: Some(PathBuf::new()) }),
        // measurement only: nothing waits for it (the panel shows the last values)
        Job::Stats { .. } => None,
        // the camera JPEG and the wide-gamut preview are optional: without an answer the normal view stays (both
        // answer nothing already when there is no camera JPEG / no wide version). The wide previews of other formats
        // and of Sigma (`RenderWide`, `RenderWideSigma`) wait at the gate `RENDER_WIDE`, where a newer one of the
        // same photo silently supersedes an older one; the app keeps no marker for them, the 8-bit preview stays
        // shown until a wide one arrives – so no waiting state.
        Job::CameraJpeg { .. } | Job::RenderWide { .. } | Job::RenderWideSigma { .. } => None,
        // works ahead without an answer
        Job::PrepareFull { .. } => None,
        // the photos still marked are sent again with the next multi-edit; nothing is shown as waiting
        Job::Thumbnails { .. } => None,
    }
}

/// Settings state of the shown photo a job was sent for (`App::edit_generation`, counted up on every change of its
/// settings): once it has passed, the job computes nothing more and sends only its terminal result – an outdated
/// whole image or crop is neither computed to the end nor shown. `generation`/`current` (image change), `cancel`
/// (preloading, Sigma base; checked inside the computation) and `epoch`/[`Origin`] (list mapping) stay as they are.
#[derive(Clone, Debug)]
pub struct EditStamp {
    at: u64,
    now: Option<Arc<AtomicU64>>,
}

impl EditStamp {
    /// The current state of `now` (the app's counter).
    pub fn current(now: &Arc<AtomicU64>) -> Self {
        Self { at: now.load(Ordering::SeqCst), now: Some(now.clone()) }
    }

    /// Not bound to the settings state of the shown photo (photos of the compare panes, tests): always live.
    pub fn none() -> Self {
        Self { at: 0, now: None }
    }

    /// Still the settings state the job was sent for?
    pub fn live(&self) -> bool {
        self.now.as_ref().is_none_or(|n| n.load(Ordering::SeqCst) == self.at)
    }

    /// Identity of the state a job was sent for: image change (`generation`) and this edit generation.
    pub fn sent(&self, generation: usize) -> (usize, u64) {
        (generation, self.at)
    }
}

/// A `sent` state no job has (results of another photo).
pub const NO_STAMP: (usize, u64) = (usize::MAX, u64::MAX);

/// Whole images computed once more for the same photo, settings and image change (`Job::RenderFull`): with the
/// zoom guard and the edit generation this stays 0 (stress test final line, measurement).
static FULL_DUPLICATES: AtomicUsize = AtomicUsize::new(0);
/// Jobs that computed nothing because their settings state had passed (`EditStamp`), and outdated results dropped
/// after computing.
static EDIT_DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Record a finished whole image (photo, image change, settings); counts a second one of the same state.
fn note_full_rendered(origin: &Origin, generation: usize, settings: &Settings) {
    use std::hash::{Hash, Hasher};
    static SEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    origin_hash(origin).hash(&mut h);
    generation.hash(&mut h);
    format!("{settings:?}").hash(&mut h);
    let k = h.finish();
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.contains(&k) {
        FULL_DUPLICATES.fetch_add(1, Ordering::SeqCst);
        crate::log(&format!("full resolution: whole image computed twice for the same state ({origin:?})"));
    } else {
        if seen.len() >= 256 {
            seen.remove(0);
        }
        seen.push(k);
    }
}

/// Final line of the stress test and the measurement: duplicate whole images, outdated jobs dropped, gates.
pub fn planning_report() -> String {
    format!(
        "stress: planning full_duplicates={} edit_dropped={} gates[{}]",
        FULL_DUPLICATES.load(Ordering::SeqCst),
        EDIT_DROPPED.load(Ordering::SeqCst),
        pools::gates_overview()
    )
}

/// Key of the hover previews of one photo at the gates (from the photo's identity, never from its index): a newer
/// hover preview supersedes an older waiting one.
pub fn hover_key(origin: &Origin) -> u64 {
    origin_hash(origin) ^ 0x486f_7665_72 // "Hover"
}

/// Key of the wide-gamut previews of one photo at the gates (a newer one supersedes an older waiting one).
pub fn wide_key(origin: &Origin) -> u64 {
    origin_hash(origin) ^ 0x5769_6465 // "Wide"
}

/// Key of the full-resolution crops of one photo at the gates (a newer crop supersedes an older waiting one).
pub fn region_key(origin: &Origin) -> u64 {
    origin_hash(origin) ^ 0x5265_6769_6f6e // "Region"
}

/// FNV-1a over the origin (address of the photo's data or its path).
fn origin_hash(origin: &Origin) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut byte = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    match origin {
        Origin::Data(a) => a.to_le_bytes().into_iter().for_each(&mut byte),
        Origin::Path(p) => p.as_os_str().as_encoded_bytes().iter().copied().for_each(&mut byte),
    }
    h
}

/// Which workers run a job: what the user waits for in front, work for the next moments ahead, the rest in the
/// background (see [`crate::threads`]). Preloading the neighbour stays in front: while paging it is what the user
/// waits for next (as "utility" the next image came later, measured).
fn lane(job: &Job) -> crate::threads::Lane {
    use crate::threads::Lane;
    match job {
        // the contact print is watched on screen, but its photos arrive one by one and nobody zooms into it meanwhile
        Job::PrepareFull { .. } | Job::Spp { prefetch: true, .. } | Job::Print { .. } => Lane::Ahead,
        // film tiles: the visible ones stay in front (as "Visible" at the film gate, taking turns with the test strip;
        // as "Soon" they waited behind it, measured 16.11.2026), the quiet fill of the others runs in the background
        Job::Thumbnails { .. } | Job::FilmThumbs { background: true, .. } => Lane::Back,
        _ => Lane::Front,
    }
}

/// [`Job::RenderDetailWide`]: the zoomed view with preserved gamut, gates and answers as the plain jobs; where
/// there is no wide output the plain job runs instead.
#[allow(clippy::too_many_arguments)]
fn render_detail_wide(index: usize, session: Option<Arc<Mutex<Session>>>, foreign: Option<Arc<rawdev::RawDoc>>, settings: Settings, region: Option<[usize; 4]>, generation: usize, current: Arc<AtomicUsize>, edit: EditStamp, send: &dyn Fn(JobResult)) {
    let plain = |session, foreign, settings| match region {
        Some(r) => Job::RenderRegion { index, session, foreign, settings, region: r, factor: 1, generation, current: current.clone(), edit: edit.clone() },
        None => Job::RenderFull { index, session, foreign, settings, generation, current: current.clone(), edit: edit.clone() },
    };
    // a crop with local adjustments needs the whole image; without a float surface there is nothing to show it on
    if region.is_some() && develop::local::active(&settings) || !crate::photo_display::float_surface() {
        return run_inner(plain(session, foreign, settings), send);
    }
    let t = std::time::Instant::now();
    // extended sRGB-encoded whole image or crop; `None`: no wide output, the plain job takes over
    let render = |session: &Option<Arc<Mutex<Session>>>, foreign: &Option<Arc<rawdev::RawDoc>>| -> Option<(usize, usize, Vec<f32>)> {
        match (foreign, session) {
            (Some(doc), _) => {
                let r = match region {
                    Some(r) => rawdev::color::render_region_encoded(doc, &settings, r),
                    None => rawdev::color::render_full_encoded(doc, &settings),
                };
                match r {
                    Ok(b) => b.map(|b| (b.width, b.height, b.rgb)),
                    Err(e) => {
                        eprintln!("wide-gamut display: {e}");
                        None
                    }
                }
            }
            (None, Some(session)) => {
                // from the shared Sigma base without the session lock, else the session as the plain jobs do
                let (cal, base) = {
                    let sess = timed_session_lock(session);
                    (sess.cal.clone(), sess.spp_snapshot())
                };
                if !develop::session::wide_display_possible(&cal, &settings.effective()) {
                    return None;
                }
                let img = match base.and_then(|b| develop::session::wide_from_base(&cal, &b, &settings, region)) {
                    Some(img) => Some(img),
                    None => {
                        let mut sess = timed_session_lock(session);
                        let r = match region {
                            Some(r) => sess.render_region_wide(&settings, r),
                            None => sess.render_full_wide(&settings, true),
                        };
                        r.unwrap_or_else(|e| {
                            eprintln!("wide-gamut display: {e}");
                            None
                        })
                    }
                };
                img.map(|i| (i.width, i.height, i.data))
            }
            _ => None,
        }
    };
    let max_side = crate::photo_display::max_texture_side();
    match region {
        Some(r) => {
            let key = match (&foreign, &session) {
                (Some(d), _) => Some(region_key(&data(d))),
                (None, Some(s)) => Some(region_key(&data(s))),
                _ => None,
            };
            let Some(gate) = pools::gate::RENDER_REGION.acquire(pools::intent(), key).taken() else {
                send(JobResult::RegionFailed { index, superseded: true });
                return;
            };
            if current.load(Ordering::SeqCst) != generation {
                return; // image changed – the app has already forgotten the job
            }
            if !edit.live() {
                // settings changed while it waited: the crop of the new settings is on its way
                EDIT_DROPPED.fetch_add(1, Ordering::SeqCst);
                send(JobResult::RegionFailed { index, superseded: true });
                return;
            }
            let Some((w, h, rgb)) = render(&session, &foreign) else {
                drop(gate);
                return run_inner(plain(session, foreign, settings), send);
            };
            if !edit.live() {
                // outdated while computing: neither converted nor shown
                EDIT_DROPPED.fetch_add(1, Ordering::SeqCst);
                send(JobResult::RegionFailed { index, superseded: true });
                return;
            }
            let rgba = crate::photo_display::rgba_from_encoded(&rgb);
            drop(rgb);
            let Some(tiles) = crate::photo_display::tiles(w, h, &rgba, max_side, true, false) else {
                send(JobResult::RegionFailed { index, superseded: false });
                return;
            };
            send(JobResult::DetailWide { index, settings, region: [r[0], r[1], w, h], full: false, size: [w, h], tiles, half: None, seconds: t.elapsed().as_secs_f32() });
        }
        None => {
            // as `RenderFull`: one at a time, dropped once outdated (`edit`, image change) before or after computing
            let live = || current.load(Ordering::SeqCst) == generation && edit.live();
            let outdated = |settings: Settings| {
                EDIT_DROPPED.fetch_add(1, Ordering::SeqCst);
                JobResult::FullFailed { index, settings, outdated: true, sent: edit.sent(generation) }
            };
            // already outdated: answer at once instead of queueing behind a whole image that is being computed
            if !live() {
                send(outdated(settings));
                return;
            }
            let _gate = pools::gate::RENDER_FULL.lock();
            if !live() {
                send(outdated(settings));
                return;
            }
            let origin = match (&foreign, &session) {
                (Some(d), _) => Some(data(d)),
                (None, Some(s)) => Some(data(s)),
                _ => None,
            };
            let Some((w, h, rgb)) = render(&session, &foreign) else {
                drop(_gate);
                return run_inner(plain(session, foreign, settings), send);
            };
            if !live() {
                send(outdated(settings));
                return;
            }
            if let Some(origin) = &origin {
                note_full_rendered(origin, generation, &settings);
            }
            let seconds = t.elapsed().as_secs_f32();
            let rgba = crate::photo_display::rgba_from_encoded(&rgb);
            drop(rgb);
            let (hw, hh, half) = crate::photo_display::half_rgba(w, h, &rgba);
            let (Some(tiles), Some(half_tiles)) = (crate::photo_display::tiles(w, h, &rgba, max_side, true, false), crate::photo_display::tiles(hw, hh, &half, max_side, true, false)) else {
                send(JobResult::FullFailed { index, settings, outdated: false, sent: edit.sent(generation) });
                return;
            };
            send(JobResult::DetailWide { index, settings, region: [0, 0, w, h], full: true, size: [w, h], tiles, half: Some((hw, hh, half_tiles)), seconds });
        }
    }
}

/// Image at half the edge length (2×2 box average of the 16-bit values) as 8-bit RGB.
fn half_size(img: &develop::Rgb16) -> (usize, usize, Vec<u8>) {
    use rayon::prelude::*;
    let (w, h) = ((img.width / 2).max(1), (img.height / 2).max(1));
    let mut out = vec![0u8; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let (y0, y1) = ((2 * y).min(img.height - 1), (2 * y + 1).min(img.height - 1));
        for x in 0..w {
            let (x0, x1) = ((2 * x).min(img.width - 1), (2 * x + 1).min(img.width - 1));
            for c in 0..3 {
                let v = |xx: usize, yy: usize| img.data[(yy * img.width + xx) * 3 + c] as u32;
                let sum = v(x0, y0) + v(x1, y0) + v(x0, y1) + v(x1, y1);
                row[x * 3 + c] = ((sum + 2 * 257) / (4 * 257)).min(255) as u8;
            }
        }
    });
    (w, h, out)
}

/// Camera orientation from the X3F header (degrees clockwise; version < 4: offset 36).
pub(crate) fn header_rotation(path: &PathBuf) -> u8 {
    // Header up to 3.x: offset 36, from 4.0 (Quattro): offset 48
    x3f::read_header_rotation(path).map_or(0, |deg| develop::geometry::quarters_from_degrees(deg as i32))
}

/// Rotates an RGB image by `q` quarter turns clockwise.
pub(crate) fn rotate_rgb(w: usize, h: usize, rgb: Vec<u8>, q: u8) -> (usize, usize, Vec<u8>) {
    if q % 4 == 0 {
        return (w, h, rgb);
    }
    let (ow, oh) = if q % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; rgb.len()];
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match q % 4 {
                1 => (y, h - 1 - x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (w - 1 - y, x),
            };
            let (si, di) = ((sy * w + sx) * 3, (y * ow + x) * 3);
            out[di..di + 3].copy_from_slice(&rgb[si..si + 3]);
        }
    }
    (ow, oh, out)
}

/// Thread pool for the SPP base (half the cores – the fast preview has priority by its larger pool). Priority as the
/// front pool: the user waits for the exact image (with "utility" it came up to twice as late while opening, measured).
fn spp_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = library::platform::foreground_threads();
        rayon::ThreadPoolBuilder::new()
            .num_threads((n / 2).max(2))
            .thread_name(|i| format!("spp-{i}"))
            .start_handler(|_| {
                pools::mark(pools::Pool::Spp);
                library::platform::set_priority(library::platform::Priority::UserInitiated)
            })
            .build()
            .unwrap()
    })
}

fn decode_jpeg(bytes: &[u8], max_edge: u32) -> Option<(usize, usize, Vec<u8>)> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let img = if img.width().max(img.height()) > max_edge { img.thumbnail(max_edge, max_edge) } else { img };
    let rgb = img.to_rgb8();
    Some((rgb.width() as usize, rgb.height() as usize, rgb.into_raw()))
}

/// Embedded camera image: JPEG, otherwise (SD9/SD10) the uncompressed preview.
fn embedded_rgb(path: &PathBuf, largest: bool, max_edge: u32) -> Option<(usize, usize, Vec<u8>)> {
    if let Some(r) = x3f::read_embedded_jpeg(path, largest).ok().and_then(|b| decode_jpeg(&b, max_edge)) {
        return Some(r);
    }
    let (w, h, rgb) = x3f::read_embedded_plain(path, largest)?;
    let img = image::RgbImage::from_raw(w, h, rgb)?;
    let img = image::DynamicImage::ImageRgb8(img);
    let img = if w.max(h) > max_edge { img.thumbnail(max_edge, max_edge) } else { img };
    let rgb = img.to_rgb8();
    Some((rgb.width() as usize, rgb.height() as usize, rgb.into_raw()))
}

/// The job for the wide-gamut version of the preview of `loaded` (display layer), `None` where there is none (film,
/// Sigma monochrome, Classic cameras, sensor and pipeline views: their output is defined in sRGB or has no colour beyond it).
pub fn wide_job(index: usize, loaded: &Loaded, settings: &Settings) -> Option<Job> {
    match &loaded.previews.foreign {
        Some(doc) => Some(Job::RenderWide { index, doc: doc.clone(), settings: settings.clone(), region: None }),
        None if loaded.session.is_some() && settings.view == View::Developed && !settings.film.is_active() && !settings.monochrome && !loaded.cal.is_classic() => {
            Some(Job::RenderWideSigma { index, previews: loaded.previews.clone(), cal: loaded.cal.clone(), settings: settings.clone() })
        }
        None => None,
    }
}

/// The Sigma preview a render of these settings starts from: sensor view, Sigma's exact base (once there and used),
/// else the own pipeline.
fn sigma_preview<'a>(previews: &'a Previews, settings: &Settings) -> &'a Arc<Preview> {
    match (&previews.spp, settings.view) {
        (_, v) if v != View::Developed => &previews.sensor,
        (Some(spp), _) if Session::uses_spp(settings) => spp,
        _ => &previews.developed,
    }
}

/// Is there a wide-gamut version of the zoomed view of `loaded` with these settings ([`Job::RenderDetailWide`])?
pub fn wide_detail_possible(loaded: &Loaded, settings: &Settings) -> bool {
    match &loaded.previews.foreign {
        Some(doc) => !settings.film.is_active() && doc.hdr.is_none(),
        None => loaded.session.is_some() && develop::session::wide_display_possible(&loaded.cal, &settings.effective()),
    }
}

pub(crate) fn previews(session: &Session, settings: &Settings) -> Previews {
    let (small, factor) = session.preview_linear(PREVIEW_EDGE);
    let mut sensor = Preview::plain(small.clone(), factor);
    sensor.rescue_rows = session.rescue_rows(factor, small.height).map(Arc::new);
    let sensor = Arc::new(sensor);
    let developed = Arc::new(Preview::developed(small, factor, &session.cal, settings));
    Previews { developed, sensor, denoise_key: settings.denoise_key(), spp: None, raw: session.raw_shared(), foreign: None }
}

/// Classic cameras: Sigma's whole chain runs while the photo opens, so its exact base is ready at once (from the
/// preparation; with changed noise sliders our noise reduction on top, as the base job would do). The first picture is
/// then the exact view, not the own quick pipeline (globally brighter there: +3 to +7 L*) followed by a jump.
/// `None` for other cameras, other views and when the base fails (then the base job runs as before).
fn classic_exact_preview(session: &Session, settings: &Settings) -> Option<Arc<Preview>> {
    if !session.cal.is_classic() || settings.view != View::Developed || !Session::uses_spp(settings) {
        return None;
    }
    let key = settings.spp_key(&session.path);
    let base = if key.neutral_noise() {
        // only a conversion of the finished chain (tens of milliseconds): computed, not read from or written to the
        // disk cache (≈ 50 MB per photo, slower to read than to compute)
        develop::spp::working_key(&session.path, &key, false, &AtomicBool::new(false)).map(|counts| {
            let base = Arc::new(develop::session::SppBase::new(key, counts));
            session.install_spp_base(base.clone());
            base
        })
    } else {
        session.ensure_spp(settings)
    };
    match base {
        Ok(base) => Some(Arc::new(Preview::from_spp(&base, base.matrix(&session.cal), PREVIEW_EDGE))),
        Err(e) => {
            crate::log(&format!("open {}: exact Classic base failed ({e}), the base job takes over", session.path.display()));
            None
        }
    }
}

fn run(job: Job, send: &dyn Fn(JobResult)) {
    // Log the run time per job kind (with GRAINEO_LOG) to detect congestion
    static RUNNING: AtomicUsize = AtomicUsize::new(0);
    let kind = job_kind(&job);
    let n = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
    let t = std::time::Instant::now();
    // for the tooltip of the spinner: what is being computed (work ahead and "other" stay out of it)
    let _step = (!matches!(job, Job::Prefetch { .. } | Job::PrepareFull { .. } | Job::Spp { prefetch: true, .. }) && kind != crate::tr("Sonstiges")).then(|| crate::work_step::enter(kind, job_stage(&job)));
    run_inner(job, send);
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    if crate::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("job {kind}: {:.0?} (concurrent {n})", t.elapsed());
    }
}

/// First stage of a job for the tooltip of the spinner (more stages are named by the job itself).
fn job_stage(job: &Job) -> Option<&'static str> {
    match job {
        Job::Render { settings, .. } | Job::RenderFull { settings, .. } if settings.film.is_active() => Some(crate::tr("Film berechnen")),
        Job::Render { .. } => Some(crate::tr("Bild entwickeln")),
        Job::RenderRegion { .. } | Job::RenderDetailWide { region: Some(_), .. } => Some(crate::tr("Ausschnitt in 100 % rechnen")),
        Job::Spp { .. } => Some(crate::tr("Foveon-Entwicklung wartet auf andere Berechnung")),
        _ => None,
    }
}

fn job_kind(job: &Job) -> &'static str {
    match job {
        Job::Open { .. } => crate::tr("Öffnen"),
        Job::Prefetch { .. } => crate::tr("Vorladen"),
        Job::Prepare { .. } => crate::tr("Vorbereiten"),
        Job::Render { .. } => crate::tr("Rendern"),
        Job::RenderWide { .. } | Job::RenderWideSigma { .. } => crate::tr("Rendern (breit)"),
        Job::RenderRegion { .. } => crate::tr("Ausschnitt"),
        Job::RenderFull { .. } | Job::RenderDetailWide { region: None, .. } => crate::tr("Volle Auflösung (ganzes Bild)"),
        Job::RenderDetailWide { .. } => crate::tr("Ausschnitt"),
        Job::PrepareFull { .. } => crate::tr("Volle Auflösung voraus"),
        Job::SppInstall { .. } => crate::tr("Foveon-Basis einsetzen"),
        Job::Spp { prefetch: true, .. } => crate::tr("Foveon-Basis voraus"),
        Job::Spp { .. } => crate::tr("Foveon-Basis rechnen"),
        _ => crate::tr("Sonstiges"),
    }
}

/// Opening goes one after another through `pools::gate::OPEN` instead of simultaneously: with fast browsing, dozens of
/// loads otherwise ran in parallel and slowed the currently selected image down to many seconds. Every open waits
/// with this key: a newer open supersedes an older one still waiting, which then leaves at once (it belongs to an
/// image the user has already left; if not, it waits on without key). Preloading has its own gate
/// (`pools::gate::PREFETCH`): the image the user asked for never waits behind a neighbour's preload (at most one open
/// and one preload run at the same time).
const OPEN_KEY: u64 = 0x4f70_656e; // "Open"

/// Key of the preloads of one file at `pools::gate::PREFETCH`: a newer preload of the same photo supersedes one still
/// waiting (that one answers `PrefetchFailed`, as when cancelled).
fn prefetch_key(path: &std::path::Path) -> u64 {
    origin_hash(&Origin::Path(path.to_path_buf())) ^ 0x5072_6566_6574_6368 // "Prefetch"
}

/// Opens and preloads in progress (the user is paging through images): background work such as
/// offline copies and the large preview waits meanwhile, see [`wait_for_foreground`].
static FOREGROUND: AtomicUsize = AtomicUsize::new(0);

struct Foreground;
impl Foreground {
    fn enter() -> Self {
        FOREGROUND.fetch_add(1, Ordering::SeqCst);
        Foreground
    }
}
impl Drop for Foreground {
    fn drop(&mut self) {
        FOREGROUND.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Last image change (ms since [`clock_start`]); 0 = none yet.
static LAST_BROWSE_MS: AtomicU64 = AtomicU64::new(0);
/// Background work also waits this long after the last image change (paging on usually follows within it).
const BROWSE_QUIET: std::time::Duration = std::time::Duration::from_millis(1500);

fn clock_start() -> std::time::Instant {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

/// The user changed the image (paging): background work pauses for a moment, see [`wait_for_foreground`].
pub fn note_browsing() {
    LAST_BROWSE_MS.store(clock_start().elapsed().as_millis() as u64 + 1, Ordering::SeqCst);
}

/// Time since the last image change (`None`: no change yet in this run).
pub fn since_browsing() -> Option<std::time::Duration> {
    let last = LAST_BROWSE_MS.load(Ordering::SeqCst);
    (last > 0).then(|| clock_start().elapsed().saturating_sub(std::time::Duration::from_millis(last - 1)))
}

/// Last input of the user (key, click, scroll; ms since [`clock_start`]); 0 = none yet.
static LAST_INPUT_MS: AtomicU64 = AtomicU64::new(0);
/// Without input for this long (and nothing being opened) the app counts as idle.
const IDLE_AFTER: std::time::Duration = std::time::Duration::from_secs(20);

/// The user typed, clicked or scrolled.
pub fn note_input() {
    LAST_INPUT_MS.store(clock_start().elapsed().as_millis() as u64 + 1, Ordering::SeqCst);
}

/// Nothing happens: no input and no image change for a while, nothing being opened (more background work may run).
pub fn idle() -> bool {
    let since = |ms: u64| clock_start().elapsed().saturating_sub(std::time::Duration::from_millis(ms.saturating_sub(1)));
    FOREGROUND.load(Ordering::SeqCst) == 0 && since(LAST_INPUT_MS.load(Ordering::SeqCst)) >= IDLE_AFTER && since_browsing().is_none_or(|d| d >= IDLE_AFTER)
}

/// Images are being opened or preloaded, or the user changed the image or worked (clicked, dragged a slider, typed)
/// a moment ago. Also the hold-back hook of the gates: background (`Idle`) waiters stay out meanwhile.
pub fn foreground_busy() -> bool {
    let input = LAST_INPUT_MS.load(Ordering::SeqCst);
    let recent_input = input > 0 && clock_start().elapsed().saturating_sub(std::time::Duration::from_millis(input - 1)) < BROWSE_QUIET;
    FOREGROUND.load(Ordering::SeqCst) > 0 || since_browsing().is_some_and(|d| d < BROWSE_QUIET) || recent_input
}

/// Preview of a freshly computed Sigma base, then the base installed in the session. The downscaling (hundreds of
/// milliseconds) runs without the session lock; the lock is taken only to read the colour matrix and to install the
/// base. Order matters: the preview exists before `has_spp_for` becomes true (the view asks that before it requests
/// crops from the base).
fn install_spp_with_preview(session: &Mutex<Session>, key: develop::SppKey, counts: develop::Linear) -> Preview {
    let matrix = {
        let s = session.lock().unwrap_or_else(|e| e.into_inner());
        s.cal.mode(key.mode.as_deref()).cam_to_srgb
    };
    let base = Arc::new(develop::session::SppBase::new(key, counts));
    let preview = Preview::from_spp(&base, matrix, PREVIEW_EDGE);
    session.lock().unwrap_or_else(|e| e.into_inner()).install_spp_base(base);
    preview
}

/// Lock the Sigma session and, with `GRAINEO_LOG`, report waits over 20 ms (`gate session: waited <ms> ms (lane
/// <thread name>)`, like the gates themselves).
fn timed_session_lock(m: &Mutex<Session>) -> std::sync::MutexGuard<'_, Session> {
    let t = std::time::Instant::now();
    let g = m.lock().unwrap_or_else(|e| e.into_inner());
    log_gate_wait("session", t);
    g
}

pub(crate) fn log_gate_wait(name: &str, since: std::time::Instant) {
    let waited = since.elapsed();
    if waited.as_millis() > 20 && crate::env::var_os("GRAINEO_LOG").is_some() {
        let thread = std::thread::current();
        crate::log(&format!("gate {name}: waited {} ms (lane {})", waited.as_millis(), thread.name().unwrap_or("?")));
    }
}

/// Background work (low priority): wait while images are being opened or preloaded or the user is paging – they share
/// the processor and, for photos on the NAS, the network. At most `max` long. Returns the time waited.
pub fn wait_for_foreground(max: std::time::Duration) -> std::time::Duration {
    let t = std::time::Instant::now();
    while foreground_busy() && t.elapsed() < max {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    t.elapsed()
}

fn run_inner(mut job: Job, send: &dyn Fn(JobResult)) {
    // the tone equalizer mask is a screen view: files, prints, thumbnails and comparison tiles never show it
    match &mut job {
        Job::Export { items, .. } => items.iter_mut().for_each(|(_, s, ..)| crate::tone_eq_ui::without_mask(s)),
        Job::Print { images, .. } => images.iter_mut().for_each(|(_, s)| crate::tone_eq_ui::without_mask(s)),
        Job::Thumbnails { images, .. } => images.iter_mut().for_each(|(.., s)| crate::tone_eq_ui::without_mask(s)),
        Job::Variants { list, .. } => list.iter_mut().for_each(|(_, s)| crate::tone_eq_ui::without_mask(s)),
        Job::Contact { rows, .. } => rows.iter_mut().flat_map(|(_, r)| r.iter_mut()).for_each(|(_, s)| crate::tone_eq_ui::without_mask(s)),
        Job::FilmThumbs { settings, .. } => crate::tone_eq_ui::without_mask(settings),
        _ => {}
    }
    match job {
        Job::Open { index, path, settings, generation, current } => {
            // the image the user asked for (lane in front: visible); an older open still waiting gives way to a newer one
            let still_wanted = || current.load(Ordering::SeqCst) == generation;
            let _gate = match pools::gate::OPEN.acquire(pools::intent(), Some(OPEN_KEY)) {
                pools::gate::Acquire::Taken(g) => g,
                pools::gate::Acquire::Superseded if !still_wanted() => return, // the app has already left this image
                pools::gate::Acquire::Superseded => pools::gate::OPEN.lock(),
            };
            if !still_wanted() {
                return; // superseded even before it started
            }
            let _busy = Foreground::enter();
            match load(&path, &settings, || current.load(Ordering::SeqCst) == generation) {
                Ok(Some(loaded)) => send(JobResult::Opened { index, loaded, prefetch: false }),
                Ok(None) => {}
                Err(error) => send(JobResult::OpenFailed { index, error }),
            }
        }
        Job::Prefetch { index, path, settings, cancel } => {
            let _busy = Foreground::enter();
            let load_result = {
                // for the next moments (soon): never before an open, which has its own gate and the visible intent
                let Some(_gate) = pools::gate::PREFETCH.acquire(pools::Intent::Soon, Some(prefetch_key(&path))).taken() else {
                    send(JobResult::PrefetchFailed { index });
                    return;
                };
                if cancel.load(Ordering::SeqCst) {
                    // report back: the app otherwise keeps the image as "being preloaded" (and a click on it would wait forever)
                    send(JobResult::PrefetchFailed { index });
                    return;
                }
                load(&path, &settings, || !cancel.load(Ordering::SeqCst))
            };
            if !matches!(load_result, Ok(Some(_))) {
                send(JobResult::PrefetchFailed { index });
                return;
            }
            if let Ok(Some(mut loaded)) = load_result {
                // Develop fully right away without the lock (as on first display: initial values from Lightroom if there are no own
                // settings), so that a click on this image no longer brings waiting or a change of appearance.
                if let Some(doc) = loaded.previews.foreign.clone().filter(|_| !cancel.load(Ordering::SeqCst)) {
                    let render_settings = match (&loaded.lr_start, settings == Settings::default()) {
                        (Some(l), true) => l.clone(),
                        _ => settings.clone(),
                    };
                    let img = doc.render_preview(&render_settings);
                    loaded.prerendered = Some((render_settings, img.width, img.height, img.to_rgb8()));
                }
                send(JobResult::Opened { index, loaded, prefetch: true });
            }
        }
        Job::Prepare { index, session, settings } => {
            let Ok(s) = session.lock() else { return };
            let previews = previews(&s, &settings);
            send(JobResult::Prepared { index, previews });
        }
        Job::Render { index, previews, cal, settings, before } => {
            if let Some(doc) = &previews.foreign {
                let img = doc.render_preview(&settings);
                send(JobResult::Rendered { index, settings, before, width: img.width, height: img.height, rgb: img.to_rgb8() });
                return;
            }
            if settings.view.is_stage() {
                // Pipeline inspection: only from the existing preview data, without session lock
                let src = develop::inspect::Sources { raw: &previews.raw, sensor: &previews.sensor, developed: &previews.developed, spp: previews.spp.as_deref() };
                let img = develop::inspect::render(&src, &cal, &settings);
                send(JobResult::Rendered { index, settings, before, width: img.width, height: img.height, rgb: img.to_rgb8() });
                return;
            }
            let img = sigma_preview(&previews, &settings).render(&cal, &settings);
            send(JobResult::Rendered { index, settings, before, width: img.width, height: img.height, rgb: img.to_rgb8() });
        }
        Job::RenderWideSigma { index, previews, cal, settings } => {
            // only the newest of a photo is computed; one waiting for the gate gives way to a newer one (no answer:
            // the app waits for nothing, the SDR view stays until the wide version arrives)
            let key = wide_key(&data(&previews.raw));
            let Some(_gate) = pools::gate::RENDER_WIDE.acquire(pools::intent(), Some(key)).taken() else { return };
            let t = std::time::Instant::now();
            match sigma_preview(&previews, &settings).render_wide(&cal, &settings) {
                Ok(Some(img)) => {
                    let rgba = crate::photo_display::rgba_from_encoded(&img.data);
                    let Some(tex) = crate::photo_display::PhotoTex::upload(img.width, img.height, &rgba, false, false) else { return };
                    crate::log(&format!("wide-gamut display {index}: Sigma preview {}×{} in {:.0} ms", img.width, img.height, t.elapsed().as_secs_f64() * 1000.0));
                    send(JobResult::Wide { index, settings, region: None, tex: Arc::new(tex) });
                }
                Ok(None) => {}
                Err(e) => eprintln!("wide-gamut display: {e}"),
            }
        }
        Job::RenderWide { index, doc, settings, region } => {
            let key = wide_key(&data(&doc));
            let Some(_gate) = pools::gate::RENDER_WIDE.acquire(pools::intent(), Some(key)).taken() else { return };
            let t = std::time::Instant::now();
            let outcome = match region {
                None => rawdev::color::render_preview_encoded(&doc, &settings),
                Some(r) => rawdev::color::render_region_encoded(&doc, &settings, r),
            };
            match outcome {
                Ok(Some(b)) => {
                    let rgba = crate::photo_display::rgba_from_encoded(&b.rgb);
                    let Some(tex) = crate::photo_display::PhotoTex::upload(b.width, b.height, &rgba, region.is_some(), false) else { return };
                    crate::log(&format!("wide-gamut display {index}: preview {}×{} in {:.0} ms", b.width, b.height, t.elapsed().as_secs_f64() * 1000.0));
                    send(JobResult::Wide { index, settings, region, tex: Arc::new(tex) })
                }
                Ok(None) => {}
                Err(e) => eprintln!("wide-gamut display: {e}"),
            }
        }
        Job::ExportLut { settings,name,path } => {let result=develop::film::cube_lut(&settings,&name).and_then(|s|std::fs::write(path,s).map_err(|e|e.to_string()));send(JobResult::LutExported{result});}
        Job::ExportLookLut { doc,settings,name,path } => {let result=doc.look_cube(&settings,&name).and_then(|s|std::fs::write(path,s).map_err(|e|e.to_string()));send(JobResult::LutExported{result});}
        Job::ScanBounds { key,image,path,previews,cal,settings } => {
            let img=if let Some(doc)=previews.foreign {doc.render_preview(&settings)}else{
                let p=if let Some(p)=previews.spp.as_ref().filter(|_|Session::uses_spp(&settings)){p}else{&previews.developed};p.render(&cal,&settings)
            };
            send(JobResult::ScanBounds{key,image,path,bounds:Some(develop::film::scan_bounds(&img))});
        }
        Job::ToneCurve { settings,key } => send(JobResult::ToneCurve{key,data:develop::film::tone_curves(&settings)}),
        Job::FilmProbe { settings,key } => {
            let now=std::time::Instant::now();
            let scene=develop::film::light_points(1024,512);
            let image=develop::film::develop(&scene,&settings,develop::look::Geo::full(1024,512));
            send(JobResult::FilmProbe{key,image,seconds:now.elapsed().as_secs_f32()});
        }
        Job::PrepareFull { index, doc, settings, generation, current } => {
            // low priority: never while images are being opened or preloaded
            wait_for_foreground(std::time::Duration::from_secs(20));
            let live = || current.load(Ordering::SeqCst) == generation;
            if !live() {
                return;
            }
            let t = std::time::Instant::now();
            let ok = doc.full_base().is_ok() && live() && doc.full_stage(&settings).is_ok();
            if !live() {
                // image changed meanwhile: do not keep half a gigabyte for an image no longer shown
                doc.release_memory();
                return;
            }
            crate::log(&format!("full resolution ahead {index}: {} in {:.0?}", if ok { "ready" } else { "failed" }, t.elapsed()));
        }
        Job::RenderRegion { index, session, foreign, settings, region, factor, generation, current, edit } => {
            // one crop at a time (shared with the hover previews), the visible photo first; a newer crop of the same
            // photo supersedes an older one still waiting (the view asks for the crop of the current position)
            let key = match (&foreign, &session) {
                (Some(d), _) => Some(region_key(&data(d))),
                (None, Some(s)) => Some(region_key(&data(s))),
                _ => None,
            };
            let Some(_gate) = pools::gate::RENDER_REGION.acquire(pools::intent(), key).taken() else {
                send(JobResult::RegionFailed { index, superseded: true });
                return;
            };
            if current.load(Ordering::SeqCst) != generation {
                return; // Image changed – the app has already forgotten the job
            }
            if !edit.live() {
                // settings changed while it waited: the crop of the new settings is on its way
                EDIT_DROPPED.fetch_add(1, Ordering::SeqCst);
                send(JobResult::RegionFailed { index, superseded: true });
                return;
            }
            let render_factor=if !settings.film.is_active() || develop::local::active(&settings){1}else{factor};
            let (img, actual_factor) = match (&foreign, &session) {
                (Some(doc), _) => match doc.render_grain_region(&settings, region, render_factor) {
                    Ok(img) => (img, render_factor),
                    Err(e) => {eprintln!("grain loupe: {e}; full pixel view");match doc.render_region(&settings,region){Ok(img)=>(img,1),Err(e)=>{eprintln!("crop: {e}");send(JobResult::RegionFailed { index, superseded: false });return;}}}
                },
                (None, Some(session)) => {
                    // the Sigma base is shared: snapshot under the lock, rendering without it; the session itself
                    // (own pipeline, under the lock) only when the settings do not use the base or it is not there
                    let (cal, base) = {
                        let sess = timed_session_lock(session);
                        (sess.cal.clone(), sess.spp_snapshot())
                    };
                    let shared = base.and_then(|b| match develop::session::grain_region_from_base(&cal, &b, &settings, region, render_factor) {
                        Ok(img) => Some((img, render_factor)),
                        Err(e) => develop::session::region_from_base(&cal, &b, &settings, region).map(|img| {
                            eprintln!("grain loupe: {e}; full pixel view");
                            (img, 1)
                        }),
                    });
                    match shared {
                        Some(r) => r,
                        None => {
                            let mut sess = timed_session_lock(session);
                            match sess.render_grain_region(&settings, region, render_factor) {
                                Ok(img) => (img, render_factor),
                                Err(e) => {
                                    eprintln!("grain loupe: {e}; full pixel view");
                                    (sess.render_region(&settings, region), 1)
                                }
                            }
                        }
                    }
                }
                _ => { send(JobResult::RegionFailed { index, superseded: false }); return }
            };
            if !edit.live() {
                // outdated while computing: neither converted nor shown
                EDIT_DROPPED.fetch_add(1, Ordering::SeqCst);
                send(JobResult::RegionFailed { index, superseded: true });
                return;
            }
            let region = [region[0], region[1], img.width/actual_factor, img.height/actual_factor];
            send(JobResult::RegionRendered { index, settings, region, factor, width: img.width, height: img.height, rgb: img.to_rgb8() });
        }
        Job::RenderFull { index, session, foreign, settings, generation, current, edit } => {
            // one at a time, the visible photo first, never superseded (zoom and pan send no new whole image); a
            // settings change makes it outdated (`edit`), it is then dropped before or after computing; crops
            // (RenderRegion) have their own gate and do not wait behind it
            let live = || current.load(Ordering::SeqCst) == generation && edit.live();
            let outdated = |settings: Settings| {
                EDIT_DROPPED.fetch_add(1, Ordering::SeqCst);
                JobResult::FullFailed { index, settings, outdated: true, sent: edit.sent(generation) }
            };
            // already outdated: answer at once instead of queueing behind a whole image that is being computed
            if !live() {
                send(outdated(settings));
                return;
            }
            let _gate = pools::gate::RENDER_FULL.lock();
            if !live() {
                send(outdated(settings));
                return;
            }
            let t = std::time::Instant::now();
            let origin = match (&foreign, &session) {
                (Some(d), _) => data(d),
                (None, Some(s)) => data(s),
                _ => {
                    send(JobResult::FullFailed { index, settings, outdated: false, sent: edit.sent(generation) });
                    return;
                }
            };
            let img = match (&foreign, &session) {
                (Some(doc), _) => match doc.render_full(&settings) {
                    Ok(img) => img,
                    Err(e) => {
                        eprintln!("full resolution: {e}");
                        send(JobResult::FullFailed { index, settings, outdated: false, sent: edit.sent(generation) });
                        return;
                    }
                },
                (None, Some(session)) => {
                    // from the shared Sigma base without the session lock; otherwise (own pipeline or base still
                    // missing: the export path computes it) under the lock as before
                    let (cal, base) = {
                        let sess = timed_session_lock(session);
                        (sess.cal.clone(), sess.spp_snapshot())
                    };
                    if !live() {
                        send(outdated(settings));
                        return;
                    }
                    match base.and_then(|b| develop::session::full_from_base(&cal, &b, &settings)) {
                        Some(img) => img,
                        None => {
                            let mut sess = timed_session_lock(session);
                            if !live() {
                                send(outdated(settings));
                                return;
                            }
                            sess.render_full(&settings)
                        }
                    }
                }
                _ => unreachable!("origin checked above"),
            };
            if !live() {
                send(outdated(settings));
                return;
            }
            note_full_rendered(&origin, generation, &settings);
            let seconds = t.elapsed().as_secs_f32();
            let half = half_size(&img);
            send(JobResult::FullRendered { index, settings, width: img.width, height: img.height, rgb: img.to_rgb8(), half, seconds });
        }
        Job::RenderDetailWide { index, session, foreign, settings, region, generation, current, edit } => {
            render_detail_wide(index, session, foreign, settings, region, generation, current, edit, send);
        }
        Job::SppInstall { index, session, mode, counts, seconds } => {
            let preview = install_spp_with_preview(&session, mode.clone(), counts);
            send(JobResult::SppReady { index, mode, preview: Arc::new(preview), seconds, prefetch: false });
        }
        Job::Spp { index, path, session, mode, cancel, prefetch } => {
            let t = std::time::Instant::now();
            // Own, smaller thread pool: the fast preview, opened in parallel,
            // keeps enough cores.
            // Only one base at a time: with fast browsing several full computations
            // (≈ 1 GB each) otherwise pile up – superseded ones abort immediately after waiting. The base of the
            // visible photo goes before one computed ahead (intent of the lane: front = visible, ahead = soon).
            let gate = pools::gate::SPP_BASE.lock();
            if cancel.load(Ordering::SeqCst) {
                drop(gate);
                if prefetch {
                    send(JobResult::SppFailed { index, error: "abgebrochen".into(), prefetch });
                }
                return;
            }
            crate::work_step::stage_here(if pools::gpu_allowed() { crate::tr("Rauschminderung (Grafikkarte)") } else { crate::tr("Rauschminderung (Prozessor)") });
            let result = spp_pool().install(|| develop::diskcache::working_key(&path, &mode, true, &cancel));
            drop(gate);
            crate::work_step::stage_here(crate::tr("Vorschau erzeugen"));
            match result {
                Ok(counts) => {
                    if cancel.load(Ordering::SeqCst) {
                        return;
                    }
                    let Some(session) = session else {
                        send(JobResult::SppCounts { index, mode, counts, seconds: t.elapsed().as_secs_f32() });
                        return;
                    };
                    let t_base = t.elapsed();
                    let preview = install_spp_with_preview(&session, mode.clone(), counts);
                    if crate::env::var_os("GRAINEO_LOG").is_some() {
                        eprintln!("spp {index}: base {:.0?}, preview {:.0?}", t_base, t.elapsed() - t_base);
                    }
                    send(JobResult::SppReady { index, mode, preview: Arc::new(preview), seconds: t.elapsed().as_secs_f32(), prefetch });
                }
                Err(_) if cancel.load(Ordering::SeqCst) && !prefetch => {}
                Err(error) => send(JobResult::SppFailed { index, error, prefetch }),
            }
        }
        Job::CameraJpeg { index, path } => {
            let kind = crate::image_kind::of(&path);
            if kind != crate::image_kind::ImageKind::Raw && kind != crate::image_kind::ImageKind::Foveon {
                return; // finished image: no embedded camera JPEG
            }
            if kind == crate::image_kind::ImageKind::Raw {
                // embedded camera JPEG, back in sensor orientation (the display rotates itself)
                let fmt = library::format_from_extension(&path.to_string_lossy()).unwrap_or("DNG");
                let q = raw::RawFile::open(&path).map(|f| crate::raw_app::layout(f.info.orientation)).unwrap_or(0);
                if let Ok((img, _)) = library::preview::load(&path, fmt, PREVIEW_EDGE as u32) {
                    let (w, h) = (img.width() as usize, img.height() as usize);
                    let (width, height, rgb) = rotate_rgb(w, h, img.into_raw(), (4 - q) % 4);
                    send(JobResult::CameraJpeg { index, width, height, rgb });
                }
                return;
            }
            if let Some((width, height, rgb)) = embedded_rgb(&path, true, PREVIEW_EDGE as u32) {
                send(JobResult::CameraJpeg { index, width, height, rgb });
            }
        }
        Job::Stats { index, session, rect } => {
            // the raw data and calibration are shared: the session is locked only to clone the handles
            let (raw, cal) = {
                let sess = session.lock().unwrap_or_else(|e| e.into_inner());
                (sess.raw_shared(), sess.cal.clone())
            };
            let stats = develop::analysis::region_stats(&raw, &cal, rect);
            send(JobResult::Stats { index, rect, stats });
        }
        Job::ExportLayers { session, dir, flat_field } => {
            let (raw, cal, path) = {
                let sess = session.lock().unwrap_or_else(|e| e.into_inner());
                (sess.raw_shared(), sess.cal.clone(), sess.path.clone())
            };
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let result = std::fs::create_dir_all(&dir)
                .and_then(|_| develop::analysis::export_layers(&raw, &cal, &dir, &stem, flat_field))
                .map(|_| dir)
                .map_err(|e| e.to_string());
            send(JobResult::LayersExported { result });
        }
        Job::HoverRender { index, previews, cal, settings, supersede } => {
            // A hover preview is a crop-class computation (2.8 MP): it takes the crops' gate, one at a time, so that a
            // storm of hovers slows no crop. With `supersede` only the newest hover of the photo computes: an older one
            // still waiting answers `HoverSuperseded` (whoever waited for it forgets the request).
            let key = supersede.then(|| hover_key(&previews.foreign.as_ref().map_or_else(|| data(&previews.raw), data)));
            let Some(_gate) = pools::gate::RENDER_REGION.acquire(pools::intent(), key).taken() else {
                send(JobResult::HoverSuperseded { index, settings });
                return;
            };
            if let Some(doc) = &previews.foreign {
                let img = doc.render_preview(&settings);
                send(JobResult::HoverRendered { index, settings, width: img.width, height: img.height, rgb: img.to_rgb8() });
                return;
            }
            let preview = match &previews.spp {
                Some(spp) if Session::uses_spp(&settings) => spp,
                _ => &previews.developed,
            };
            let img = preview.render(&cal, &settings);
            send(JobResult::HoverRendered { index, settings, width: img.width, height: img.height, rgb: img.to_rgb8() });
        }
        Job::FilmThumbs { index, previews, cal, settings, key, modes, valid, background } => {
            crate::film_previews::compute(index, &previews, &cal, &settings, key, &modes, &valid, background, send);
        }
        Job::Variants { index, previews, cal, list, key } => {
            let mut images = Vec::new();
            let mut cache = StageCache::default();
            for (name, s) in list {
                let r = render_cached(&mut cache, &previews, &cal, &s, 220, false);
                let (w, h, rgb) = shrink_staged(&r, 220);
                images.push(upright(name, w, h, rgb, r.orientation));
            }
            send(JobResult::Variants { index, key, images });
        }
        Job::Contact { index, previews, cal, rows, key, target } => {
            let last = rows.len().saturating_sub(1);
            // Grid cells with the same stage share the scaled image (once per job)
            let mut cache = StageCache::default();
            for (n, (row, fields)) in rows.into_iter().enumerate() {
                let mut finished = Vec::new();
                for (column, s) in fields {
                    let r = render_cached(&mut cache, &previews, &cal, &s, 380, true);
                    let q = r.orientation;
                    let (w, h, rgb) = if target != ContactTarget::Paper {
                        let img = develop::film::apply_border(develop::geometry::apply(r.img, &s, q), &s.film);
                        shrink_to_target(&img, r.ratio, 380)
                    } else {
                        let img = develop::geometry::apply(r.img, &s, q);
                        shrink_to_target(&img, r.ratio, 380)
                    };
                    finished.push((column, w, h, rgb));
                }
                send(JobResult::Contact { index, key, row, fields: finished, end: n == last, target });
            }
        }
        Job::Print { images, key, valid, previews } => {
            let is_current = || valid.load(Ordering::SeqCst) == key;
            // first the quick stand-ins from the library's preview cache (no film), then the developed photos
            if let Some((cache, keys)) = &previews {
                let level = library::preview::small();
                for (nr, k) in keys.iter().enumerate() {
                    if !is_current() {
                        break;
                    }
                    if let Some(img) = k.and_then(|k| cache.read(k, level)) {
                        send(JobResult::PrintPreview { key, nr, image: (img.width() as usize, img.height() as usize, img.into_raw()) });
                    }
                }
            }
            // one after another: measured, 2 or 3 photos at once bring nothing (the load already uses several cores)
            let last = images.len().saturating_sub(1);
            for (nr, (path, s)) in images.iter().enumerate() {
                if !is_current() {
                    send(JobResult::Print { key, nr, image: None, end: true });
                    return;
                }
                let t0 = std::time::Instant::now();
                let image = match load(path, s, is_current) {
                    Ok(Some(l)) => {
                        let t1 = std::time::Instant::now();
                        let r = render_cached(&mut StageCache::default(), &l.previews, &l.cal, s, PRINT_EDGE, true);
                        let img = develop::geometry::apply(r.img, s, r.orientation);
                        let out = shrink_to_target(&img, r.ratio, PRINT_EDGE);
                        if crate::env::var_os("GRAINEO_LOG").is_some() {
                            eprintln!("contact print {nr}: load {:.0?}, film+render {:.0?}", t1 - t0, t1.elapsed());
                        }
                        Some(out)
                    }
                    Ok(None) => None,
                    Err(e) => {
                        crate::log(&format!("contact print: {e}"));
                        None
                    }
                };
                send(JobResult::Print { key, nr, image, end: nr == last });
            }
        }
        Job::Thumbnails { images, key, valid, large } => {
            for (image, file, path, s) in images {
                let is_current = || valid.load(Ordering::SeqCst) == key;
                if !is_current() {
                    return;
                }
                match load(&path, &s, is_current) {
                    Ok(Some(l)) => {
                        let edge = library::preview::small() as usize;
                        let r = render_cached(&mut StageCache::default(), &l.previews, &l.cal, &s, edge, true);
                        let img = develop::geometry::apply(r.img, &s, r.orientation);
                        let rgb = shrink_to_target(&img, r.ratio, edge);
                        THUMBS_RENDERED.fetch_add(1, Ordering::Relaxed);
                        send(JobResult::Thumbnail { image, file, rgb });
                        if large {
                            crate::large_preview::schedule(file, &path, &s, crate::browse::settings_fp(&s), || false);
                        }
                    }
                    // no picture: still reported, so the next part of the previews follows
                    Ok(None) => send(JobResult::Thumbnail { image, file, rgb: (0, 0, Vec::new()) }),
                    Err(e) => {
                        crate::log(&format!("preview after multi-edit {}: {e}", path.display()));
                        send(JobResult::Thumbnail { image, file, rgb: (0, 0, Vec::new()) });
                    }
                }
            }
        }
        Job::SceneDistribution { index, previews, cal, settings, key } => {
            let scene = if let Some(doc) = &previews.foreign {
                let st = doc.small(&settings, 240);
                doc.film_scene(&st, &settings).0
            } else {
                let preview = match &previews.spp {
                    Some(spp) if Session::uses_spp(&settings) => spp,
                    _ => &previews.developed,
                };
                preview.film_scene(&cal, &settings, 240).0
            };
            let mut bins = vec![0u32; 28];
            for p in scene.data.chunks_exact(3) {
                let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                let ev = (y.max(1e-6) / 0.184).log2();
                let i = ((ev + 8.0) * 2.0).floor().clamp(0.0, 27.0) as usize;
                bins[i] += 1;
            }
            send(JobResult::SceneDistribution { index, key, bins });
        }
        Job::Camf { index, path } => {
            let camf = x3f::X3fFile::open(&path).and_then(|f| f.camf()).map(Arc::new).map_err(|e| e.to_string());
            send(JobResult::Camf { index, camf });
        }
        Job::ExportOriginal { items, dir, sidecars } => {
            let total = items.len();
            let db = library::db::Db::standard().ok();
            let mut errors = 0;
            for (i, (path, image, pair)) in items.into_iter().enumerate() {
                if EXPORT_CANCEL.load(Ordering::SeqCst) {
                    send(JobResult::ExportProgress { done: total, total, message: crate::trf!("abgebrochen nach {i} von {total}", i = i, total = total), failed: None });
                    return;
                }
                let file = db.as_ref().and_then(|d| d.file_of(image));
                // Apple Photos: the original (from iCloud if necessary), named as in Apple Photos (IMG_1234)
                let photo = if library::apple_photos::in_photos_library(&path) {
                    let name = file.and_then(crate::apple_photos_ui::original_name);
                    crate::apple_photos_ui::original(&path).map(|p| {
                        let mut s = library::original_export::Source::of(&p, file);
                        if let Some(n) = name {
                            let ext = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                            let stem = std::path::Path::new(&n).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n);
                            s.name = if ext.is_empty() { stem } else { format!("{stem}.{ext}") };
                        }
                        s
                    })
                } else {
                    Ok(library::original_export::Source::of(&path, file))
                };
                let pair = pair.and_then(|j| {
                    let d = db.as_ref()?;
                    let f = d.file_of(j)?;
                    Some(library::original_export::Source::of(&d.file_path(f)?, Some(f)))
                });
                let result = photo.and_then(|p| library::original_export::export(db.as_ref(), &p, pair.as_ref(), &dir, sidecars));
                let stem = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let (message, failed) = match result {
                    Ok(w) => (crate::trf!("{a} gespeichert", a = w.first().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy()), None),
                    Err(e) => {
                        errors += 1;
                        (crate::trf!("Fehler bei {stem}: {e}", stem = stem, e = crate::menu::i18n::tr_msg(&e)), Some(path.clone()))
                    }
                };
                let done = i + 1;
                let message = if done == total && errors == 0 { crate::trf!("fertig – {total} Bild(er) in {a}", total = total, a = dir.display()) } else { message };
                send(JobResult::ExportProgress { done, total, message, failed });
            }
            if total == 0 {
                send(JobResult::ExportProgress { done: 0, total: 0, message: String::new(), failed: None });
            }
        }
        Job::Export { items, format, engine, dir, gain_map, allow_missing_profiles, as_print, color_space, options } => {
            let total = items.len();
            // never into an Apple Photos library or another program's package
            if let Err(e) = library::write_target::check(&dir) {
                send(JobResult::ExportProgress { done: total, total, message: crate::menu::i18n::tr_msg(&e).into_owned(), failed: Some(PathBuf::new()) });
                return;
            }
            if let Err(e) = std::fs::create_dir_all(&dir) {
                send(JobResult::ExportProgress { done: total, total, message: crate::trf!("Zielordner: {e}", e = e), failed: Some(PathBuf::new()) });
                return;
            }
            let mut errors = 0;
            let db = library::db::Db::standard().ok();
            for (i, (path, settings, image, meta)) in items.into_iter().enumerate() {
                if EXPORT_CANCEL.load(Ordering::SeqCst) {
                    send(JobResult::ExportProgress { done: total, total, message: crate::trf!("abgebrochen nach {i} von {total}", i = i, total = total), failed: None });
                    return;
                }
                let source = path.clone();
                // Apple Photos: original (from iCloud if necessary) as a working copy, name as in Apple Photos (IMG_1234)
                let apple_name = library::apple_photos::in_photos_library(&path).then(|| db.as_ref().and_then(|d| d.file_of(image)).and_then(crate::apple_photos_ui::original_name)).flatten();
                let path = if library::apple_photos::in_photos_library(&path) {
                    match crate::apple_photos_ui::original(&path) {
                        Ok(p) => p,
                        Err(e) => {
                            errors += 1;
                            send(JobResult::ExportProgress { done: i + 1, total, message: crate::trf!("Fehler bei {stem}: {reason}", stem = apple_name.clone().unwrap_or_default(), reason = e), failed: Some(source.clone()) });
                            continue;
                        }
                    }
                } else {
                    path
                };
                let stem = apple_name.as_deref().map(std::path::Path::new).unwrap_or(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let suffix = if settings.monochrome { "_mono" } else { "" };
                let out = dir.join(format!("{stem}{suffix}{}.{}", format.name_suffix(), format.extension()));
                let mut s = Settings { view: View::Developed, ..settings };
                // Darkroom border only when exporting "as print"
                if !as_print {
                    s = crate::darkroom_border::without_border(s);
                }
                let s = match rawdev::film_profiles::for_export(&s, allow_missing_profiles) {
                    Ok(s) => s,
                    Err(e) => { errors += 1; send(JobResult::ExportProgress { done: i+1, total, message: e, failed: Some(source.clone()) }); continue; }
                };
                // Existing files do not belong to the export job: in particular, for JPEG/TIFF in the original folder
                // the destination can be the original itself. Instead of overwriting, the new file
                // gets a free name ("Name-2.jpg").
                let out = match free_target_name(&out) {
                    Ok(p) => p,
                    Err(reason) => {
                        errors += 1;
                        send(JobResult::ExportProgress { done: i + 1, total, message: crate::trf!("Fehler bei {stem}: {reason}", stem = stem, reason = reason), failed: Some(source.clone()) });
                        continue;
                    }
                };
                // HDR gain map: gets the same geometry as the image (rotation, crop, fine angle,
                // perspective); `hdr_geo` = image crop of the export if the original has a map
                // Writing goes to a side file next to the destination; only the finished file (with metadata and
                // gain map) gets the destination name, without overwriting anything. An abort leaves no half-written file.
                let target_name = out;
                let out = match library::files::side_path(&target_name) {
                    Ok(p) => p,
                    Err(e) => {
                        errors += 1;
                        send(JobResult::ExportProgress { done: i + 1, total, message: crate::trf!("Fehler bei {stem}: {e}", stem = stem, e = e), failed: Some(source.clone()) });
                        continue;
                    }
                };
                let mut hdr_geo: Option<develop::geometry::Frame> = None;
                // shooting data as the RAW decoder reads it (complements the original's metadata, see write_export_metadata)
                let mut decoder_shot: Option<library::export_meta::Shot> = None;
                // Output color space only applies to finished images (JPEG/TIFF); sRGB takes the existing, unchanged path
                let color_space = if format.is_finished_image() { color_space } else { graineo_color::Space::Srgb };
                let result = if crate::image_kind::of(&path) != crate::image_kind::ImageKind::Foveon {
                    rawdev::RawDoc::open(&path, &|| true)
                        .and_then(|d| d.ok_or_else(|| "abgebrochen".to_string()))
                        .and_then(|mut doc| {
                            // oriented as on opening (also rotated by hand in Lightroom): crop and rotation refer to it
                            rawdev::lightroom::apply_orientation(&mut doc);
                            // The HDR gain map belongs to the sRGB image; in other color spaces it is dropped
                            if doc.hdr.is_some() && develop::film::border_fractions(&s.film).is_none() && color_space == graineo_color::Space::Srgb {
                                hdr_geo = Some(rawdev::export::export_frame(&doc, &s));
                            }
                            decoder_shot = Some(rawdev::export::shot(&doc.info)).filter(|s| !s.is_empty());
                            rawdev::export::export_color_opts(&doc, &s, format, &out, color_space, &options)
                        })
                        .map(|_| true)
                } else {
                    Session::open_for(&path, &s)
                        .map_err(|e| e.to_string())
                        .and_then(|mut sess| develop::export::color::export(&mut sess, &s, format, engine, &out, color_space, &options).map_err(|e| e.to_string()))
                };
                // Metadata (shooting data of the original, copyright; personal fields as chosen) without recompressing
                let meta_report = match (&result, format) {
                    (Ok(_), Format::Jpeg { .. } | Format::Tiff16 | Format::Png16) => write_export_metadata(&out, &path, image, &meta, &options, decoder_shot.take()),
                    _ => String::new(),
                };
                if crate::env::var_os("GRAINEO_LOG").is_some() && !meta_report.is_empty() {
                    eprintln!("export {}: metadata {meta_report}", out.display());
                }
                let hdr_note = match (&result, format, hdr_geo) {
                    (Ok(_), Format::Jpeg { quality }, Some(f)) if gain_map => match rawdev::export::append_gain_map(&f, &path, &out, quality) {
                        Ok(true) => crate::tr(" · mit HDR-Gain-Map"),
                        Ok(false) => crate::tr(" · HDR-Gain-Map entfällt (Format der Karte)"),
                        Err(_) => crate::tr(" · HDR-Gain-Map nicht geschrieben"),
                    },
                    (Ok(_), Format::Jpeg { .. }, Some(_)) => crate::tr(" · ohne HDR-Gain-Map (Option aus)"),
                    (Ok(_), Format::Tiff16, Some(_)) => crate::tr(" · TIFF ohne HDR-Gain-Map"),
                    _ => "",
                };
                // Move the finished side file to the destination name. If the name has been taken in the meantime,
                // the next free one is used.
                let result = result.and_then(|spp| publish_side_file(&out, &target_name).map(|p| (spp, p)));
                // Quattro sensor layers: the full-resolution top layer is a second file next to the first
                let companion = |p: &std::path::Path| p.with_file_name(format!("{}_top.tif", p.file_stem().unwrap_or_default().to_string_lossy()));
                let side_top = companion(&out);
                if side_top.exists() {
                    match &result {
                        Ok((_, published)) => {
                            let _ = publish_side_file(&side_top, &companion(published));
                        }
                        Err(_) => {
                            let _ = std::fs::remove_file(&side_top);
                        }
                    }
                }
                if result.is_err() {
                    let _ = std::fs::remove_file(&out);
                }
                let (message, failed) = match result {
                    Ok((spp, target)) => (
                        format!(
                            "{}{}{hdr_note}",
                            crate::trf!("{a} gespeichert", a = target.file_name().unwrap_or_default().to_string_lossy()),
                            if engine == Engine::Spp && !spp { crate::tr(" (eigene Pipeline: Regler/Monochrom aktiv)") } else { "" }
                        ),
                        None,
                    ),
                    Err(e) => {
                        errors += 1;
                        (crate::trf!("Fehler bei {stem}: {e}", stem = stem, e = e), Some(source.clone()))
                    }
                };
                let done = i + 1;
                let message = if done == total && errors == 0 { crate::trf!("fertig – {total} Bild(er) in {a}", total = total, a = dir.display()) } else { message };
                send(JobResult::ExportProgress { done, total, message, failed });
            }
        }
    }
}

/// Renames the finished side file `finished` to the destination name without overwriting anything. If the name
/// is taken in the meantime, the next free one is used ("Name-2.jpg" …). Returns: the final path.
fn publish_side_file(finished: &std::path::Path, desired: &std::path::Path) -> Result<PathBuf, String> {
    for _ in 0..50 {
        let target = free_target_name(desired)?;
        match library::files::rename_no_replace(finished, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(crate::trf!("Zieldatei: {e}", e = e)),
        }
    }
    Err(crate::tr("Kein freier Dateiname").into())
}

/// Destination path that does not exist yet (not even a broken link): the requested one, otherwise "Name-2", "Name-3" …
/// Nothing is overwritten or deleted.
fn free_target_name(out: &std::path::Path) -> Result<PathBuf, String> {
    let is_free = |p: &std::path::Path| match std::fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(crate::trf!("Zieldatei nicht prüfbar: {e}", e = e)),
        Ok(_) => Ok(false),
    };
    if is_free(out)? {
        return Ok(out.to_path_buf());
    }
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = out.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 2..1000 {
        let p = out.with_file_name(format!("{stem}-{n}{extension}"));
        if is_free(&p)? {
            return Ok(p);
        }
    }
    Err(crate::tr("Kein freier Dateiname").into())
}

/// Camera capture time (seconds since 1970, without time zone – the camera stores its
/// configured local time) as "27.10.2026 14:03".
fn camera_time(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // Calendar date from days since 1970 (H. Hinnant, "civil_from_days")
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{d:02}.{m:02}.{y} {:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// Downscale 8-bit RGB to at most `edge` pixels edge length (area average).
fn shrink_rgb(rgb: &[u8], w: usize, h: usize, edge: usize) -> (usize, usize, Vec<u8>) {
    let k = w.max(h).div_ceil(edge.max(1)).max(1);
    let (tw, th) = ((w / k).max(1), (h / k).max(1));
    let mut out = vec![0u8; tw * th * 3];
    for y in 0..th {
        for x in 0..tw {
            let mut acc = [0u32; 3];
            for dy in 0..k {
                for dx in 0..k {
                    let i = ((y * k + dy) * w + x * k + dx) * 3;
                    for c in 0..3 {
                        acc[c] += rgb[i + c] as u32;
                    }
                }
            }
            for c in 0..3 {
                out[(y * tw + x) * 3 + c] = (acc[c] / (k * k) as u32) as u8;
            }
        }
    }
    (tw, th, out)
}

/// One 100 % crop of the film shelf. `None`: the full resolution is not there yet (other cameras: the full detail
/// stage; Sigma: the base or a free session) – nothing is computed ahead for the shelf, it waits for the whole image.
/// Rotate a thumbnail (key, width, height, RGB) into camera orientation.
fn upright(k: String, w: usize, h: usize, rgb: Vec<u8>, q: u8) -> (String, usize, usize, Vec<u8>) {
    let (w, h, rgb) = rotate_rgb(w, h, rgb, q);
    (k, w, h, rgb)
}

/// Metadata into the exported file as chosen in the export dialog ([`develop::export::Metadata`]):
/// shooting data of the original (for X3F from the X3F properties) and the copyright given at import;
/// with "All" also stars, label, keywords (without names of people), title and caption from the library (read only);
/// the location only with `options.gps`. Never serial numbers.
fn write_export_metadata(out: &std::path::Path, original: &std::path::Path, image: i64, meta: &crate::sidecar::Meta, options: &develop::export::Options, shot: Option<library::export_meta::Shot>) -> String {
    use develop::export::Metadata;
    if options.metadata == Metadata::None {
        return String::new();
    }
    let mut f = export_fields(meta, options, shot);
    if image > 0 {
        if let Ok(db) = library::db::Db::standard() {
            f.copyright = db.copyright_of(image);
            if f.personal {
                f.keywords = db.keywords_without_people(image).unwrap_or_default();
                f.description = db.image_text_of(image).filter(|t| !t.trim().is_empty());
                f.title = db.title_of(image);
            }
        }
    }
    library::export_meta::write(out, original, &f).unwrap_or_else(|e| format!("not written: {e}"))
}

/// The export fields of the chosen metadata level without the library (stars and label from `meta`).
fn export_fields(meta: &crate::sidecar::Meta, options: &develop::export::Options, shot: Option<library::export_meta::Shot>) -> library::imageio::ExportFields {
    let personal = options.metadata == develop::export::Metadata::All;
    let mut f = library::imageio::ExportFields { personal, gps: options.gps, shot, ..Default::default() };
    if personal {
        f.stars = meta.rating;
        if meta.label > 0 {
            f.label = library::label::NAMES[meta.label as usize % 6].to_string();
        }
    }
    f
}

/// The contact sheet shows the same crop, angle and orientation as the large photo.
#[cfg(test)]
fn contact_preview(img: develop::Rgb16, settings: &Settings, orientation: u8) -> (usize, usize, Vec<u8>) {
    let img = develop::geometry::apply(img, settings, orientation);
    shrink_rgb(&img.to_rgb8(), img.width, img.height, 380)
}

// ---- Small views compute small: Variants, contact sheet, thumbnails, print ----

/// Longest edge of the stage a small view is computed at: twice the target, divided by the crop share
/// (a strongly cropped photo needs more pixels to stay sharp), at most [`PREVIEW_EDGE`].
fn render_edge(target: usize, s: &Settings) -> usize {
    (((2 * target) as f32 / crop_share(s)).ceil() as usize).min(PREVIEW_EDGE)
}

/// The stage never falls below this edge: the target itself (with crop: target / share).
fn min_render_edge(target: usize, s: &Settings) -> usize {
    ((target as f32 / crop_share(s)).ceil() as usize).min(PREVIEW_EDGE)
}

fn crop_share(s: &Settings) -> f32 {
    s.crop[2].min(s.crop[3]).max(0.01)
}

/// Integer divisor that brings the stage closest to `want_edge` but never below `min_edge`.
fn stage_divisor(long_edge: usize, want_edge: usize, min_edge: usize) -> usize {
    let nearest = (long_edge as f64 / want_edge.max(1) as f64).round() as usize;
    nearest.clamp(1, (long_edge / min_edge.max(1)).max(1))
}

/// Value for `Preview::scaled(max_edge)` that yields exactly [`stage_divisor`].
fn stage_edge(long_edge: usize, want_edge: usize, min_edge: usize) -> usize {
    long_edge.div_ceil(stage_divisor(long_edge, want_edge, min_edge))
}

/// Does this view compute small? Not-Sigma always; Sigma only with film (without film unchanged).
fn computes_small(previews: &Previews, s: &Settings) -> bool {
    previews.foreign.is_some() || s.film.is_active()
}

/// Result of a small render: image at stage size, camera orientation, factor full preview / stage (1 = no stage).
struct Staged {
    img: develop::Rgb16,
    orientation: u8,
    ratio: f32,
}

/// Scaled stages already made in this job (once per job and stage).
#[derive(Default)]
struct StageCache {
    sigma: Vec<(bool, usize, Arc<Preview>)>,
    foreign: Vec<(usize, Arc<rawdev::Stage>, Arc<rawdev::Stage>)>,
}

fn render_cached(cache: &mut StageCache, previews: &Previews, cal: &develop::Calibration, s: &Settings, target: usize, cropped: bool) -> Staged {
    let own;
    let sc = if cropped { s } else { own = Settings { crop: [0.5, 0.5, 1.0, 1.0], ..s.clone() }; &own };
    if let Some(doc) = &previews.foreign {
        let base = doc.preview_stage(s);
        let long = base.w.max(base.h);
        let edge = render_edge(target, sc);
        if !computes_small(previews, s) || edge >= long {
            return Staged { img: doc.render_preview(s), orientation: doc.orientation, ratio: 1.0 };
        }
        let st = match cache.foreign.iter().find(|(e, b, _)| *e == edge && Arc::ptr_eq(b, &base)) {
            Some((_, _, st)) => st.clone(),
            None => {
                let st = Arc::new(doc.small(s, edge));
                cache.foreign.push((edge, base.clone(), st.clone()));
                st
            }
        };
        let ratio = long as f32 / st.w.max(st.h) as f32;
        return Staged { img: doc.render_stage(&st, s), orientation: doc.orientation, ratio };
    }
    let use_spp = previews.spp.is_some() && Session::uses_spp(s);
    let preview: &Arc<Preview> = match &previews.spp {
        Some(spp) if use_spp => spp,
        _ => &previews.developed,
    };
    if computes_small(previews, s) {
        let long = preview.lin.width.max(preview.lin.height);
        let max_edge = stage_edge(long, render_edge(target, sc), min_render_edge(target, sc));
        if long.div_ceil(max_edge) > 1 {
            let scaled = match cache.sigma.iter().find(|(u, e, _)| *u == use_spp && *e == max_edge) {
                Some((_, _, p)) => Some(p.clone()),
                None => preview.scaled(cal, max_edge).map(|p| {
                    let p = Arc::new(p);
                    cache.sigma.push((use_spp, max_edge, p.clone()));
                    p
                }),
            };
            if let Some(p) = scaled {
                let ratio = long as f32 / p.lin.width.max(p.lin.height) as f32;
                return Staged { img: p.render(cal, s), orientation: cal.orientation, ratio };
            }
        }
    }
    Staged { img: preview.render(cal, s), orientation: cal.orientation, ratio: 1.0 }
}

/// One render as today's jobs did it, but at the small stage where that is allowed (the jobs use `render_cached`).
#[cfg(test)]
fn render_at(previews: &Previews, cal: &develop::Calibration, s: &Settings, target: usize) -> (develop::Rgb16, u8) {
    let r = render_cached(&mut StageCache::default(), previews, cal, s, target, true);
    (r.img, r.orientation)
}

/// Area average to exactly `w × h` (fractions of source pixels weighted), 8-bit RGB.
fn area_to(rgb: &[u8], sw: usize, sh: usize, w: usize, h: usize) -> Vec<u8> {
    let (sx, sy) = (sw as f64 / w as f64, sh as f64 / h as f64);
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        let (y0, y1) = (y as f64 * sy, (y + 1) as f64 * sy);
        for x in 0..w {
            let (x0, x1) = (x as f64 * sx, (x + 1) as f64 * sx);
            let mut acc = [0f64; 3];
            let mut wsum = 0f64;
            for yy in (y0.floor() as usize)..(y1.ceil() as usize).min(sh) {
                let wy = (y1.min((yy + 1) as f64) - y0.max(yy as f64)).max(0.0);
                for xx in (x0.floor() as usize)..(x1.ceil() as usize).min(sw) {
                    let wx = (x1.min((xx + 1) as f64) - x0.max(xx as f64)).max(0.0);
                    let i = (yy * sw + xx) * 3;
                    for c in 0..3 {
                        acc[c] += rgb[i + c] as f64 * wx * wy;
                    }
                    wsum += wx * wy;
                }
            }
            for c in 0..3 {
                out[(y * w + x) * 3 + c] = (acc[c] / wsum.max(1e-9)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Final size like today's whole-number shrink of the full preview: its size is `ratio` times the image,
/// divided by `ceil(longest edge / target)`. Without a stage (`ratio` 1) exactly today's path.
fn shrink_to_target(img: &develop::Rgb16, ratio: f32, target: usize) -> (usize, usize, Vec<u8>) {
    let rgb = img.to_rgb8();
    if ratio <= 1.0 {
        return shrink_rgb(&rgb, img.width, img.height, target);
    }
    let (fw, fh) = (((img.width as f32 * ratio).round() as usize).max(1), ((img.height as f32 * ratio).round() as usize).max(1));
    let k = fw.max(fh).div_ceil(target.max(1)).max(1);
    let (w, h) = ((fw / k).max(1), (fh / k).max(1));
    let (w, h) = (w.min(img.width), h.min(img.height));
    (w, h, area_to(&rgb, img.width, img.height, w, h))
}

fn shrink_staged(r: &Staged, target: usize) -> (usize, usize, Vec<u8>) {
    shrink_to_target(&r.img, r.ratio, target)
}

#[cfg(test)]
mod contact_tests {
    use super::*;
    fn color_patches() -> develop::Rgb16 {
        let mut data = Vec::new();
        for _y in 0..60 { for x in 0..100 { data.extend(if x < 70 { [65535, 0, 0] } else { [0, 0, 65535] }); } }
        develop::Rgb16 { width: 100, height: 60, data }
    }
    #[test]
    fn render_edge_is_twice_target() {
        assert_eq!(render_edge(220, &Settings::default()), 440);
    }
    #[test]
    fn render_edge_grows_with_crop() {
        let s = Settings { crop: [0.5, 0.5, 0.25, 0.25], ..Default::default() };
        assert_eq!(render_edge(220, &s), 1760);
    }
    #[test]
    fn render_edge_is_capped() {
        let s = Settings { crop: [0.5, 0.5, 0.01, 0.01], ..Default::default() };
        assert_eq!(render_edge(220, &s), PREVIEW_EDGE);
    }
    #[test]
    fn very_small_crop_uses_preview_size() {
        // 1 % crop: the stage would exceed the preview, so there is no smaller stage (same pixel count as today)
        let s = Settings { crop: [0.5, 0.5, 0.01, 0.01], ..Default::default() };
        for long in [1176usize, 2352, 2400] {
            assert_eq!(stage_divisor(long, render_edge(220, &s), min_render_edge(220, &s)), 1, "long {long}");
        }
    }
    #[test]
    fn stage_is_never_smaller_than_target() {
        for long in [1176usize, 1568, 2352, 2400] {
            for target in [220usize, 380, 600, 800, 1200] {
                for share in [1.0f32, 0.5, 0.25, 0.1] {
                    let s = Settings { crop: [0.5, 0.5, share, share], ..Default::default() };
                    let max_edge = stage_edge(long, render_edge(target, &s), min_render_edge(target, &s));
                    let k = long.div_ceil(max_edge).max(1);
                    assert!(k <= stage_divisor(long, render_edge(target, &s), min_render_edge(target, &s)));
                    let need = min_render_edge(target, &s);
                    if need <= long {
                        assert!(long / k >= need, "long {long} target {target} share {share}: stage {}", long / k);
                    }
                }
            }
        }
        assert_eq!(stage_divisor(2352, 440, 220), 5);
        assert_eq!(stage_divisor(2352, 1200, 600), 2);
    }
    #[test]
    fn shrink_to_target_matches_todays_size() {
        // Stage 470 px of a 2352 px preview (ratio 5.004), target 220: today 2352 / ceil(2352/220)=11 -> 213 px
        let img = develop::Rgb16 { width: 470, height: 313, data: vec![30000; 470 * 313 * 3] };
        let (w, h, rgb) = shrink_to_target(&img, 2352.0 / 470.0, 220);
        assert_eq!((w, h), (213, 142));
        assert_eq!(rgb.len(), w * h * 3);
        assert!(rgb.iter().all(|&v| v == (30000u32 >> 8) as u8 || v == ((30000f32 / 257.0).round() as u8)));
        // without stage: exactly the old path
        let (w, h, _) = shrink_to_target(&img, 1.0, 220);
        assert_eq!((w, h), (470 / 3, 313 / 3));
    }
    #[test]
    fn stale_contact_completion_stays_deliverable_after_list_change() {
        let mut result = JobResult::Contact { index: 99, key: 42, row: 0, fields: Vec::new(), end: true, target: ContactTarget::Paper };
        assert!(result.index_mut().is_none(), "completion must not be discarded at an old list position");
    }
    #[test]
    fn preview_shows_chosen_crop_and_orientation() {
        let s = Settings { crop: [0.2, 0.5, 0.3, 1.0], ..Default::default() };
        let (w, h, rgb) = contact_preview(color_patches(), &s, 0);
        assert_eq!((w, h), (30, 60));
        assert!(rgb.chunks_exact(3).all(|p| p[0] > 250 && p[2] < 5));
        let (w, h, _) = contact_preview(color_patches(), &Settings::default(), 1);
        assert_eq!((w, h), (60, 100));
    }
}

#[cfg(test)]
mod export_safety_tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("graineo-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn free_target_name_overwrites_nothing() {
        let root = tmp("zielname");
        let target = root.join("Übung 日本.jpg");
        assert_eq!(free_target_name(&target).unwrap(), target);
        std::fs::write(&target, b"vorhanden").unwrap();
        assert_eq!(free_target_name(&target).unwrap(), root.join("Übung 日本-2.jpg"));
        std::fs::write(root.join("Übung 日本-2.jpg"), b"auch").unwrap();
        // a broken link counts as taken
        std::os::unix::fs::symlink(root.join("missing"), root.join("Übung 日本-3.jpg")).unwrap();
        assert_eq!(free_target_name(&target).unwrap(), root.join("Übung 日本-4.jpg"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Measurement (not a check): peak memory with simultaneous exports of the same photo.
    /// `GRAINEO_TEST_RAW=<File> GRAINEO_TEST_N=<Anzahl> cargo test --release -p graineo speicher_bei_gleichzeitigen -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn memory_with_concurrent_exports() {
        let Some(raw) = crate::env::var_os("GRAINEO_TEST_RAW").map(PathBuf::from) else { return };
        let n: usize = crate::env::var("GRAINEO_TEST_N").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
        let root = tmp("speicher");
        let rss_kb = || -> u64 {
            std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok()).unwrap_or(0)
        };
        let before = rss_kb();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let peak = Arc::new(AtomicUsize::new(0));
        let measurer = {
            let (stop, peak) = (stop.clone(), peak.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    peak.fetch_max(rss_kb() as usize, Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            })
        };
        let t = std::time::Instant::now();
        let hs: Vec<_> = (0..n).map(|i| {
            let (raw, dir) = (raw.clone(), root.join(i.to_string()));
            std::thread::spawn(move || run_inner(Job::Export {
                items: vec![(raw, Settings::default(), 0, crate::sidecar::Meta::default())],
                format: Format::Jpeg { quality: 90 }, engine: Engine::Own, dir,
                gain_map: false, allow_missing_profiles: false, as_print: false, color_space: graineo_color::Space::Srgb, options: Default::default(),
            }, &|_| {}))
        }).collect();
        for h in hs { h.join().unwrap(); }
        stop.store(true, Ordering::Relaxed);
        measurer.join().unwrap();
        eprintln!("MEASUREMENT {n} concurrent exports: {:.1} s, memory before {} MB, peak {} MB", t.elapsed().as_secs_f32(), before / 1024, peak.load(Ordering::Relaxed) / 1024);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publish_replaces_nothing_and_takes_next_free_name() {
        let root = tmp("veroeffentlichen");
        let desired = root.join("Bild.jpg");
        let finished = root.join(".nebendatei-Bild.jpg");
        std::fs::write(&finished, b"neu").unwrap();
        assert_eq!(publish_side_file(&finished, &desired).unwrap(), desired);
        assert_eq!(std::fs::read(&desired).unwrap(), b"neu");
        assert!(!finished.exists());
        // second export into the same folder: the existing file stays, the new one is named "-2"
        std::fs::write(&finished, b"noch neuer").unwrap();
        assert_eq!(publish_side_file(&finished, &desired).unwrap(), root.join("Bild-2.jpg"));
        assert_eq!(std::fs::read(&desired).unwrap(), b"neu");
        assert_eq!(std::fs::read(root.join("Bild-2.jpg")).unwrap(), b"noch neuer");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_overwrites_neither_original_nor_existing_target() {
        let root = tmp("export-audit");
        // The original is deliberately not a valid image: the export fails after choosing the name,
        // existing files must still not be touched.
        let original = root.join("Übung 日本.jpg");
        std::fs::write(&original, b"original-camera-file").unwrap();
        let separate = root.join("exports");
        std::fs::create_dir(&separate).unwrap();
        let target = separate.join("Übung 日本.jpg");
        std::fs::write(&target, b"existing-export").unwrap();
        for dir in [root.clone(), separate.clone()] {
            run_inner(Job::Export {
                items: vec![(original.clone(), Settings::default(), 0, crate::sidecar::Meta::default())],
                format: Format::Jpeg { quality: 92 }, engine: Engine::Own, dir,
                gain_map: false, allow_missing_profiles: false, as_print: false, color_space: graineo_color::Space::Srgb, options: Default::default(),
            }, &|_| {});
            assert_eq!(std::fs::read(&original).unwrap(), b"original-camera-file");
            assert_eq!(std::fs::read(&target).unwrap(), b"existing-export");
        }
        // A failed export leaves no half-written side file
        for d in [&root, &separate] {
            let hidden: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.')).collect();
            assert!(hidden.is_empty(), "{hidden:?}");
        }
        // A broken link also stays as it is.
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), &target).unwrap();
        run_inner(Job::Export { items: vec![(original.clone(), Settings::default(), 0, crate::sidecar::Meta::default())], format: Format::Jpeg { quality: 92 }, engine: Engine::Own, dir: separate, gain_map: false, allow_missing_profiles: false, as_print: false, color_space: graineo_color::Space::Srgb, options: Default::default() }, &|_| {});
        assert!(std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        assert!(!root.join("missing").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The colour proof (and its gamut warning) is display only: switching it sends no job, and an export made with an
    /// active proof is byte-identical to one without.
    #[test]
    fn colour_proof_changes_no_export_and_starts_no_job() {
        use crate::proof::{self, Proof};
        let _lock = proof::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tmp("export-proof");
        let source = root.join("bild.jpg");
        let data: Vec<u16> = (0..64 * 48).flat_map(|i| [(i * 13 % 65536) as u16, 65535, (i * 7 % 65536) as u16]).collect();
        develop::export::write_jpeg(&develop::Rgb16 { width: 64, height: 48, data }, &source, 95).unwrap();
        let export = |dir: &str| {
            let target = root.join(dir);
            for space in [graineo_color::Space::Srgb, graineo_color::Space::DisplayP3] {
                run_inner(Job::Export { items: vec![(source.clone(), Settings::default(), 0, crate::sidecar::Meta::default())], format: Format::Jpeg { quality: 92 }, engine: Engine::Own, dir: target.join(format!("{space:?}")), gain_map: false, allow_missing_profiles: false, as_print: false, color_space: space, options: Default::default() }, &|_| {});
            }
            ["Srgb", "DisplayP3"].map(|d| std::fs::read(target.join(d).join("bild.jpg")).unwrap())
        };
        let before = export("ohne");
        let sent = SENT.with(|c| c.get());
        proof::set(Proof::Srgb);
        proof::toggle_warning();
        proof::toggle();
        proof::toggle();
        proof::set(Proof::ProPhoto);
        assert_eq!(SENT.with(|c| c.get()), sent, "switching the proof sent a job");
        let with = export("mit");
        proof::toggle_warning();
        proof::set(Proof::Full);
        assert_eq!(before, with);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Each metadata level of the export dialog through the whole export path: exactly the expected fields.
    #[test]
    fn export_metadata_levels() {
        use develop::export::Metadata;
        let root = tmp("export-metadata");
        let source = root.join("camera.jpg");
        let data: Vec<u16> = (0..64 * 48).flat_map(|_| [30000u16, 20000, 10000]).collect();
        develop::export::write_jpeg(&develop::Rgb16 { width: 64, height: 48, data }, &source, 95).unwrap();
        // shooting data as a camera writes them
        let shot = library::export_meta::Shot {
            make: Some("FUJIFILM".into()),
            model: Some("X-T5".into()),
            lens: Some("XF23mmF1.4 R LM WR".into()),
            date: Some("2025:06:01 10:20:30".into()),
            exposure_time: Some(1.0 / 250.0),
            f_number: Some(1.4),
            iso: Some(200),
            focal_length: Some(23.0),
            focal_length_35mm: Some(35),
            ..Default::default()
        };
        let fields = library::imageio::ExportFields { shot: Some(shot), ..Default::default() };
        library::export_meta::write_own(&source, &library::export_meta::OwnFields { fields, width: 64, height: 48 }).unwrap();
        let meta = crate::sidecar::Meta { rating: 4, label: 1, ..Default::default() };
        for (level, name) in [(Metadata::None, "none"), (Metadata::Shooting, "shooting"), (Metadata::All, "all")] {
            let options = develop::export::Options { metadata: level, ..Default::default() };
            let dir = root.join(name);
            run_inner(Job::Export { items: vec![(source.clone(), Settings::default(), 0, meta.clone())], format: Format::Jpeg { quality: 92 }, engine: Engine::Own, dir: dir.clone(), gain_map: false, allow_missing_profiles: false, as_print: false, color_space: graineo_color::Space::Srgb, options }, &|_| {});
            let out = dir.join("camera.jpg");
            assert!(out.exists(), "{name}");
            let r = |p: &str| library::imageio::read_metadata(&out, p);
            match level {
                Metadata::None => {
                    for p in ["tiff:Make", "tiff:Model", "exifEX:LensModel", "xmp:Rating", "xmp:Label"] {
                        assert_eq!(r(p), None, "{name}: {p}");
                    }
                    assert_eq!(library::imageio::properties(&out).and_then(|e| e.capture), None, "{name}");
                }
                _ => {
                    assert_eq!(r("tiff:Make").as_deref(), Some("FUJIFILM"), "{name}");
                    assert_eq!(r("tiff:Model").as_deref(), Some("X-T5"), "{name}");
                    assert_eq!(r("exifEX:LensModel").as_deref(), Some("XF23mmF1.4 R LM WR"), "{name}");
                    let ratio = |v: String| v.split_once('/').map(|(n, d)| n.parse::<f64>().unwrap() / d.parse::<f64>().unwrap()).unwrap_or_else(|| v.parse().unwrap());
                    assert_eq!(r("exif:FNumber").map(ratio), Some(1.4), "{name}");
                    assert_eq!(r("exif:ExposureTime").map(ratio), Some(1.0 / 250.0), "{name}");
                    assert_eq!(library::imageio::properties(&out).and_then(|e| e.capture).as_deref(), Some("2025-06-01T10:20:30"), "{name}");
                    let personal = level == Metadata::All;
                    assert_eq!(r("xmp:Rating").as_deref(), personal.then_some("4"), "{name}");
                    assert_eq!(r("xmp:Label").is_some(), personal, "{name}");
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_from_apple_photos_leaves_library_untouched() {
        let root = tmp("export-apple");
        let source = root.join("Test.photoslibrary/originals/A/ABC-123.jpeg");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        let data: Vec<u16> = (0..32 * 24).flat_map(|_| [30000u16, 20000, 10000]).collect();
        develop::export::write_jpeg(&develop::Rgb16 { width: 32, height: 24, data }, &source, 95).unwrap();
        let before = std::fs::read(&source).unwrap();
        let target = root.join("ziel");
        run_inner(Job::Export { items: vec![(source.clone(), Settings::default(), 0, crate::sidecar::Meta::default())], format: Format::Jpeg { quality: 92 }, engine: Engine::Own, dir: target.clone(), gain_map: false, allow_missing_profiles: false, as_print: false, color_space: graineo_color::Space::Srgb, options: Default::default() }, &|_| {});
        assert!(target.join("ABC-123.jpg").is_file());
        assert_eq!(std::fs::read(&source).unwrap(), before);
        let in_library: Vec<_> = std::fs::read_dir(source.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(in_library.len(), 1, "{in_library:?}");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod color_space_tests {
    use super::*;
    use graineo_color::Space;

    /// ICC profile from the APP2 segments of a JPEG file.
    fn icc_from_jpeg(b: &[u8]) -> Option<Vec<u8>> {
        let mut i = 2;
        let mut parts: Vec<(u8, Vec<u8>)> = Vec::new();
        while i + 4 <= b.len() && b[i] == 0xff {
            let m = b[i + 1];
            if m == 0xda {
                break;
            }
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if m == 0xe2 && b[i + 4..].starts_with(b"ICC_PROFILE\0") {
                parts.push((b[i + 4 + 12], b[i + 4 + 14..i + 2 + len].to_vec()));
            }
            i += 2 + len;
        }
        parts.sort_by_key(|t| t.0);
        (!parts.is_empty()).then(|| parts.into_iter().flat_map(|t| t.1).collect())
    }

    fn export_test_image(color_space: Space, name: &str) -> Vec<u8> {
        let root = std::env::temp_dir().join(format!("graineo-farbraum-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("quelle.jpg");
        let data: Vec<u16> = (0..64 * 48).flat_map(|i| match (i / 8) % 3 { 0 => [60000, 12000, 9000], 1 => [9000, 52000, 14000], _ => [20000, 24000, 54000] }).collect();
        develop::export::write_jpeg(&develop::Rgb16 { width: 64, height: 48, data }, &source, 95).unwrap();
        let target = root.join("ziel");
        run_inner(
            Job::Export {
                items: vec![(source, Settings::default(), 0, crate::sidecar::Meta { rating: 3, ..Default::default() })],
                format: Format::Jpeg { quality: 92 },
                engine: Engine::Own,
                dir: target.clone(),
                gain_map: false,
                allow_missing_profiles: false,
                as_print: false,
                color_space,
                options: Default::default(),
            },
            &|_| {},
        );
        let file = std::fs::read_dir(&target).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "jpg")).expect("export file");
        let bytes = std::fs::read(file).unwrap();
        let _ = std::fs::remove_dir_all(root);
        bytes
    }

    /// After export (also after writing the metadata) the file carries the profile of its color space; sRGB stays without.
    #[test]
    fn exported_jpegs_carry_the_color_space_profile() {
        for (space, name) in [(Space::DisplayP3, "p3"), (Space::AdobeRgb, "adobe"), (Space::ProPhotoRgb, "prophoto")] {
            let b = export_test_image(space, name);
            let icc = icc_from_jpeg(&b).unwrap_or_else(|| panic!("{space:?}: no profile in the file"));
            assert_eq!(icc, space.icc().unwrap(), "{space:?}: embedded profile differs");
        }
        let srgb = export_test_image(Space::Srgb, "srgb");
        // sRGB takes the existing path: no profile or the sRGB profile (inserted by macOS when writing metadata)
        assert!(icc_from_jpeg(&srgb).is_none_or(|p| p.windows(4).any(|w| w == b"sRGB")), "sRGB: unexpected profile");
    }
}

#[cfg(test)]
#[path = "scheduling_tests.rs"]
mod scheduling_tests;

/// The gates of the jobs serve the visible photo first. No real files needed.
#[cfg(test)]
mod gate_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    #[test]
    fn hover_superseded_is_index_bound() {
        assert_eq!(JobResult::HoverSuperseded { index: 3, settings: Settings::default() }.index_mut().copied(), Some(3));
    }

    /// Every job the app waits for has a terminal result (sent when it crashes, aborts or is superseded), and it is
    /// the result that ends that waiting state. The match in `terminal_result` names every kind (a new one does not
    /// compile until it is sorted in); here the kinds that can be built without image data, the rest with a real
    /// file in `terminal_results_of_photo_jobs`.
    #[test]
    fn every_waiting_job_has_a_terminal_result() {
        let current = Arc::new(AtomicUsize::new(0));
        let flag = || Arc::new(AtomicBool::new(false));
        let item = || (PathBuf::from("/nirgends/a.jpg"), Settings::default(), 0i64, crate::sidecar::Meta::default());
        let cases: Vec<(Job, fn(&JobResult) -> bool)> = vec![
            (Job::Open { index: 0, path: PathBuf::new(), settings: Settings::default(), generation: 0, current: current.clone() }, |r| matches!(r, JobResult::OpenFailed { .. })),
            (Job::Prefetch { index: 0, path: PathBuf::new(), settings: Settings::default(), cancel: flag() }, |r| matches!(r, JobResult::PrefetchFailed { .. })),
            (Job::RenderRegion { index: 0, session: None, foreign: None, settings: Settings::default(), region: [0; 4], factor: 1, generation: 0, current: current.clone(), edit: EditStamp::none() }, |r| matches!(r, JobResult::RegionFailed { superseded: false, .. })),
            (Job::RenderFull { index: 0, session: None, foreign: None, settings: Settings::default(), generation: 3, current: current.clone(), edit: EditStamp::none() }, |r| matches!(r, JobResult::FullFailed { outdated: false, sent: (3, 0), .. })),
            (Job::RenderDetailWide { index: 0, session: None, foreign: None, settings: Settings::default(), region: None, generation: 3, current: current.clone(), edit: EditStamp::none() }, |r| matches!(r, JobResult::FullFailed { outdated: false, sent: (3, 0), .. })),
            (Job::RenderDetailWide { index: 0, session: None, foreign: None, settings: Settings::default(), region: Some([0; 4]), generation: 0, current: current.clone(), edit: EditStamp::none() }, |r| matches!(r, JobResult::RegionFailed { superseded: false, .. })),
            (spp_job(false), |r| matches!(r, JobResult::SppFailed { prefetch: false, .. })),
            (spp_job(true), |r| matches!(r, JobResult::SppFailed { prefetch: true, .. })),
            (Job::Print { images: vec![], key: 4, valid: Arc::new(AtomicU64::new(0)), previews: None }, |r| matches!(r, JobResult::Print { key: 4, image: None, end: true, .. })),
            (Job::ToneCurve { settings: Default::default(), key: "k".into() }, |r| matches!(r, JobResult::ToneCurve { data: Err(_), .. })),
            (Job::FilmProbe { settings: Default::default(), key: 5 }, |r| matches!(r, JobResult::FilmProbe { key: 5, image: Err(_), .. })),
            (Job::ExportLut { settings: Default::default(), name: String::new(), path: PathBuf::new() }, |r| matches!(r, JobResult::LutExported { result: Err(_) })),
            (Job::Camf { index: 0, path: PathBuf::new() }, |r| matches!(r, JobResult::Camf { camf: Err(_), .. })),
            (
                Job::Export { items: vec![item(), item()], format: Format::Jpeg { quality: 90 }, engine: Engine::Own, dir: PathBuf::new(), gain_map: false, allow_missing_profiles: false, as_print: false, color_space: graineo_color::Space::Srgb, options: Default::default() },
                // done == total ends the export in the app
                |r| matches!(r, JobResult::ExportProgress { done: 2, total: 2, failed: Some(_), .. }),
            ),
            (Job::ExportOriginal { items: vec![(PathBuf::new(), 0, None)], dir: PathBuf::new(), sidecars: false }, |r| matches!(r, JobResult::ExportProgress { done: 1, total: 1, failed: Some(_), .. })),
        ];
        for (j, fits) in &cases {
            let r = terminal_result(j).unwrap_or_else(|| panic!("{} has no terminal result", job_kind(j)));
            assert!(fits(&r), "{}: wrong terminal result", job_kind(j));
        }
        // documented as without a waiting state (nothing in the app freezes when no answer comes)
        let none: Vec<Job> = vec![
            Job::CameraJpeg { index: 0, path: PathBuf::new() },
            Job::Thumbnails { images: vec![], key: 0, valid: Arc::new(AtomicU64::new(0)), large: false },
        ];
        for j in &none {
            assert!(terminal_result(j).is_none(), "{}", job_kind(j));
        }
    }

    /// The jobs of a loaded photo (real file, else skipped): each has the terminal result that ends its waiting
    /// state; index-bound ones stay index-bound (list changes map them like the real result).
    #[test]
    fn terminal_results_of_photo_jobs() {
        let Some(path) = super::session_lock_tests::test_file() else { return };
        let loaded = load(&path, &Settings::default(), || true).expect("open").expect("loaded");
        let (previews, cal, session) = (loaded.previews.clone(), loaded.cal.clone(), loaded.session.clone().unwrap());
        let s = Settings::default();
        let cases: Vec<(Job, fn(&JobResult) -> bool)> = vec![
            (Job::Render { index: 2, previews: previews.clone(), cal: cal.clone(), settings: s.clone(), before: false }, |r| matches!(r, JobResult::RenderFailed { index: 2, before: false, .. })),
            (Job::Prepare { index: 2, session: session.clone(), settings: s.clone() }, |r| matches!(r, JobResult::PrepareFailed { index: 2, .. })),
            (Job::HoverRender { index: 2, previews: previews.clone(), cal: cal.clone(), settings: s.clone(), supersede: true }, |r| matches!(r, JobResult::HoverSuperseded { index: 2, .. })),
            (Job::SppInstall { index: 2, session: session.clone(), mode: s.spp_key_class(false), counts: previews.developed.lin.clone(), seconds: 0.0 }, |r| matches!(r, JobResult::SppFailed { index: 2, prefetch: false, .. })),
            (Job::FilmThumbs { index: 2, previews: previews.clone(), cal: cal.clone(), settings: s.clone(), key: 7, modes: vec![], valid: Arc::new(AtomicU64::new(7)), background: false }, |r| matches!(r, JobResult::FilmThumbs { key: 7, end: true, .. })),
            (Job::Variants { index: 2, previews: previews.clone(), cal: cal.clone(), list: vec![], key: 7 }, |r| matches!(r, JobResult::Variants { key: 7, .. })),
            (Job::Contact { index: 2, previews: previews.clone(), cal: cal.clone(), rows: vec![], key: 7, target: ContactTarget::Paper }, |r| matches!(r, JobResult::Contact { key: 7, end: true, .. })),
            (Job::SceneDistribution { index: 2, previews: previews.clone(), cal: cal.clone(), settings: s.clone(), key: 7 }, |r| matches!(r, JobResult::SceneDistribution { key: 7, .. })),
            (Job::ScanBounds { key: 7, image: 1, path: path.clone(), previews: previews.clone(), cal: cal.clone(), settings: s.clone() }, |r| matches!(r, JobResult::ScanBounds { key: 7, bounds: None, .. })),
            (Job::ExportLayers { session: session.clone(), dir: PathBuf::new(), flat_field: false }, |r| matches!(r, JobResult::LayersExported { result: Err(_) })),
        ];
        for (j, fits) in &cases {
            let mut r = terminal_result(j).unwrap_or_else(|| panic!("{} has no terminal result", job_kind(j)));
            assert!(fits(&r), "{}: wrong terminal result", job_kind(j));
            if let Some(i) = r.index_mut() {
                assert_eq!(*i, 2);
            }
        }
        // measurement only: nothing waits for it
        assert!(terminal_result(&Job::Stats { index: 0, session, rect: [0; 4] }).is_none());
    }

    /// The settings changed after the whole image was sent: it computes nothing (answers at once, outdated – the
    /// app neither pauses nor marks it failed) and is not counted as a duplicate.
    #[test]
    fn render_full_dropped_when_edit_changed() {
        let now = Arc::new(AtomicU64::new(10));
        let edit = EditStamp::current(&now);
        now.fetch_add(1, Ordering::SeqCst);
        let results = Arc::new(Mutex::new(Vec::<JobResult>::new()));
        let job = Job::RenderFull { index: 0, session: None, foreign: None, settings: Settings::default(), generation: 0, current: Arc::new(AtomicUsize::new(0)), edit };
        let t = Instant::now();
        let r2 = results.clone();
        run_inner(job, &|r| r2.lock().unwrap().push(r));
        assert!(t.elapsed() < Duration::from_millis(200), "took {:?}", t.elapsed());
        let results = results.lock().unwrap();
        assert!(matches!(results.as_slice(), [JobResult::FullFailed { outdated: true, sent: (0, 10), .. }]), "{} results", results.len());
    }

    /// The same for a crop: outdated, it answers `RegionFailed { superseded: true }` (no 3-s pause in the app).
    #[test]
    fn render_region_dropped_when_edit_changed() {
        let now = Arc::new(AtomicU64::new(0));
        let edit = EditStamp::current(&now);
        assert!(edit.live());
        now.fetch_add(1, Ordering::SeqCst);
        assert!(!edit.live());
        assert!(EditStamp::none().live());
        let results = Arc::new(Mutex::new(Vec::<JobResult>::new()));
        let job = Job::RenderRegion { index: 0, session: None, foreign: None, settings: Settings::default(), region: [0, 0, 10, 10], factor: 1, generation: 0, current: Arc::new(AtomicUsize::new(0)), edit };
        let r2 = results.clone();
        run_inner(job, &|r| r2.lock().unwrap().push(r));
        assert!(matches!(results.lock().unwrap().as_slice(), [JobResult::RegionFailed { superseded: true, .. }]));
    }

    /// A second whole image of the same photo, image change and settings counts as a duplicate; another image
    /// change or other settings do not.
    #[test]
    fn duplicate_whole_images_are_counted() {
        let o = Origin::Path(PathBuf::from("/nirgends/duplikat-test.X3F"));
        let before = FULL_DUPLICATES.load(Ordering::SeqCst);
        note_full_rendered(&o, 1, &Settings::default());
        note_full_rendered(&o, 2, &Settings::default());
        note_full_rendered(&o, 2, &Settings { exposure: 0.1, ..Settings::default() });
        assert_eq!(FULL_DUPLICATES.load(Ordering::SeqCst), before);
        note_full_rendered(&o, 2, &Settings::default());
        assert_eq!(FULL_DUPLICATES.load(Ordering::SeqCst), before + 1);
    }

    /// Opening: an older open still waiting at the gate gives way to a newer one. If the user has left its image it
    /// leaves at once without an answer (as before, after waiting); if not, it waits on and is opened as well.
    #[test]
    fn superseded_open_leaves_or_waits_on() {
        let _serial = global_gates_for_tests();
        let held = pools::gate::OPEN.lock();
        let results = Arc::new(Mutex::new(Vec::<usize>::new()));
        let current = Arc::new(AtomicUsize::new(1));
        let open = |index: usize, generation: usize| {
            let (results, current) = (results.clone(), current.clone());
            std::thread::spawn(move || {
                let job = Job::Open { index, path: PathBuf::from("/nirgends/oeffnen-test.X3F"), settings: Settings::default(), generation, current };
                run_inner(job, &|r| {
                    if let JobResult::OpenFailed { index, .. } = r {
                        results.lock().unwrap().push(index);
                    }
                })
            })
        };
        let wait_for = |n: usize| {
            let t = Instant::now();
            while pools::gate::OPEN.snapshot().waiting.iter().sum::<usize>() < n {
                assert!(t.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        // left: open 1 waits, the user moves on (generation 2), open 2 supersedes it – open 1 returns at once
        let first = open(1, 1);
        wait_for(1);
        current.store(2, Ordering::SeqCst);
        let second = open(2, 2);
        let t = Instant::now();
        first.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(1), "the left open waited for the gate");
        // still wanted (same generation, e.g. the list was rebuilt): it waits on without key and is opened too
        let third = open(3, 2);
        wait_for(2);
        drop(held);
        second.join().unwrap();
        third.join().unwrap();
        let mut got = results.lock().unwrap().clone();
        got.sort();
        assert_eq!(got, vec![2, 3], "the left open answered, or a wanted one did not");
    }

    /// Preloading: a newer preload of the same photo supersedes one still waiting, which answers `PrefetchFailed` at
    /// once (the app releases its marker); a preload waits with intent "soon".
    #[test]
    fn superseded_prefetch_answers_failed() {
        let _serial = global_gates_for_tests();
        let held = pools::gate::PREFETCH.lock();
        let results = Arc::new(Mutex::new(Vec::<JobResult>::new()));
        let prefetch = |index: usize| {
            let results = results.clone();
            std::thread::spawn(move || {
                let job = Job::Prefetch { index, path: PathBuf::from("/nirgends/vorladen-test.X3F"), settings: Settings::default(), cancel: Arc::new(AtomicBool::new(false)) };
                run_inner(job, &|r| results.lock().unwrap().push(r))
            })
        };
        let first = prefetch(1);
        let t = Instant::now();
        while pools::gate::PREFETCH.snapshot().waiting[pools::Intent::Soon as usize] < 1 {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        let second = prefetch(2);
        first.join().unwrap();
        assert!(matches!(results.lock().unwrap().as_slice(), [JobResult::PrefetchFailed { index: 1 }]), "the older preload did not answer at once");
        drop(held);
        second.join().unwrap();
        assert_eq!(results.lock().unwrap().len(), 2);
    }

    /// The whole image is never superseded: two whole images of the same photo behind a held gate both answer
    /// (here `FullFailed`, as there is no image data), none disappears silently.
    #[test]
    fn render_full_is_never_superseded() {
        let _serial = global_gates_for_tests();
        let held = pools::gate::RENDER_FULL.lock();
        let results = Arc::new(Mutex::new(Vec::<JobResult>::new()));
        let current = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let job = Job::RenderFull { index: 0, session: None, foreign: None, settings: Settings::default(), generation: 0, current: current.clone(), edit: EditStamp::none() };
                let results = results.clone();
                std::thread::spawn(move || run_inner(job, &|r| results.lock().unwrap().push(r)))
            })
            .collect();
        let t = Instant::now();
        while pools::gate::RENDER_FULL.snapshot().waiting[pools::Intent::Visible as usize] < 2 {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(held);
        for th in threads {
            th.join().unwrap();
        }
        let results = results.lock().unwrap();
        assert_eq!(results.iter().filter(|r| matches!(r, JobResult::FullFailed { .. })).count(), 2);
    }

    /// Two crops of the same photo wait behind the held gate: the older one is superseded (`RegionFailed` with
    /// `superseded`, no pause in the app), the newer one is computed.
    #[test]
    fn waiting_region_is_superseded_by_newer_region_of_same_photo() {
        let _serial = global_gates_for_tests();
        let Some(path) = super::session_lock_tests::test_file() else { return };
        let session = Arc::new(Mutex::new(Session::open(&path).unwrap()));
        let held = pools::gate::RENDER_REGION.lock();
        let results = Arc::new(Mutex::new(Vec::<JobResult>::new()));
        let current = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for k in 0..2u32 {
            let job = Job::RenderRegion { index: 0, session: Some(session.clone()), foreign: None, settings: Settings::default(), region: [100 * (k as usize + 1), 100, 200, 150], factor: 1, generation: 0, current: current.clone(), edit: EditStamp::none() };
            let results = results.clone();
            threads.push(std::thread::spawn(move || run_inner(job, &|r| results.lock().unwrap().push(r))));
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(held);
        for th in threads {
            th.join().unwrap();
        }
        let results = results.lock().unwrap();
        let superseded = results.iter().filter(|r| matches!(r, JobResult::RegionFailed { superseded: true, .. })).count();
        let rendered: Vec<[usize; 4]> = results.iter().filter_map(|r| match r { JobResult::RegionRendered { region, .. } => Some(*region), _ => None }).collect();
        assert_eq!((superseded, rendered.len()), (1, 1), "{} results", results.len());
        assert_eq!(rendered[0][0], 200, "the newer crop (x = 200) is the one computed");
    }

    /// Hover storm: the crops' gate is held; ten hover previews of the same photo arrive one after another; only the
    /// newest is computed, the nine older ones answer `HoverSuperseded` (sum ten, nothing is lost).
    #[test]
    fn hover_storm_renders_only_the_latest() {
        let _serial = global_gates_for_tests();
        let Some(path) = super::session_lock_tests::test_file() else { return };
        let loaded = load(&path, &Settings::default(), || true).expect("open").expect("loaded");
        let (previews, cal) = (loaded.previews.clone(), loaded.cal.clone());
        let held = pools::gate::RENDER_REGION.lock();
        let results = Arc::new(Mutex::new(Vec::<JobResult>::new()));
        let mut settings = Vec::new();
        let mut threads = Vec::new();
        for k in 0..10 {
            let mut s = Settings::default();
            s.exposure = 0.01 * (k + 1) as f64;
            settings.push(s.clone());
            let job = Job::HoverRender { index: 0, previews: previews.clone(), cal: cal.clone(), settings: s, supersede: true };
            assert!(terminal_result(&job).is_some());
            let results = results.clone();
            threads.push(std::thread::spawn(move || run_inner(job, &|r| results.lock().unwrap().push(r))));
            // in line one after another (the newer one supersedes the older one at once)
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
        for t in threads {
            t.join().unwrap();
        }
        let results = results.lock().unwrap();
        let rendered: Vec<&Settings> = results.iter().filter_map(|r| match r { JobResult::HoverRendered { settings, .. } => Some(settings), _ => None }).collect();
        let superseded = results.iter().filter(|r| matches!(r, JobResult::HoverSuperseded { .. })).count();
        assert_eq!((rendered.len(), superseded), (1, 9), "rendered {}, superseded {superseded}", rendered.len());
        assert_eq!(rendered[0], &settings[9], "the newest hover is the one computed");
    }

    fn spp_job(prefetch: bool) -> Job {
        Job::Spp { index: 0, path: PathBuf::from("/nirgends.X3F"), session: None, mode: Settings::default().spp_key_class(false), cancel: Arc::new(AtomicBool::new(true)), prefetch }
    }

    /// Gate held; a base computed ahead (soon) waits first, then the visible one arrives: after the release the
    /// visible one returns first (both return right after the gate because `cancel` is set).
    #[test]
    fn visible_spp_base_goes_before_prefetch() {
        let _serial = global_gates_for_tests();
        let gate = &pools::gate::SPP_BASE;
        let held = gate.lock();
        // the order is taken when the gate is granted: both jobs return right after it, so the order in which they
        // finish is a race (it once came out the other way round)
        gate.record_grants();
        let run = |name: &'static str, intent: pools::Intent, job: Job| {
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    pools::set_intent(intent);
                    run_inner(job, &|_| {});
                })
                .unwrap()
        };
        let ahead = run("ahead", pools::Intent::Soon, spp_job(true));
        let t = Instant::now();
        while gate.snapshot().waiting[pools::Intent::Soon as usize] < 1 {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        let visible = run("visible", pools::Intent::Visible, spp_job(false));
        while gate.snapshot().waiting[pools::Intent::Visible as usize] < 1 {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(held);
        visible.join().unwrap();
        ahead.join().unwrap();
        // only these two threads (another test may pass the global gate meanwhile)
        let grants: Vec<String> = gate.take_grants().into_iter().filter(|n| n == "visible" || n == "ahead").collect();
        assert_eq!(grants, vec!["visible", "ahead"]);
    }
}

/// The Sigma session is locked only to clone handles or to install a base; computing and writing run without it
/// With a real file (`GRAINEO_TEST_X3F` first entry or `DP2M5418.X3F` in `GRAINEO_TEST_MERRILL_DIR`),
/// skipped otherwise.
#[cfg(test)]
mod session_lock_tests {
    use super::*;
    use std::time::{Duration, Instant};

    pub(super) fn test_file() -> Option<PathBuf> {
        if let Ok(v) = crate::env::var("GRAINEO_TEST_X3F") {
            if let Some(p) = v.split(';').map(PathBuf::from).find(|p| p.is_file()) {
                return Some(p);
            }
        }
        let dir = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(PathBuf::from)?;
        Some(dir.join("DP2M5418.X3F")).filter(|p| p.is_file())
    }

    /// The test holds the session while `job` starts (it waits at the lock), releases it after 200 ms and takes it
    /// again at once: with the lock taken only to clone the handles the test gets it back within milliseconds while
    /// the job still computes; a job that computes under the lock keeps the test out until it is done. Returns (time
    /// until the session was free again, whether the job had not yet reported by then).
    fn regain_session_during(session: &Arc<Mutex<Session>>, job: Job, wanted: impl Fn(&JobResult) -> bool + Send + Sync + 'static) -> (Duration, bool) {
        let reported = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let held = session.lock().unwrap();
        let r2 = reported.clone();
        let worker = std::thread::spawn(move || {
            run_inner(job, &|r| {
                if wanted(&r) {
                    r2.store(true, Ordering::SeqCst);
                }
            });
        });
        std::thread::sleep(Duration::from_millis(200));
        drop(held);
        let t = Instant::now();
        let free_again = loop {
            if let Ok(g) = session.try_lock() {
                drop(g);
                break t.elapsed();
            }
            assert!(t.elapsed() < Duration::from_secs(30), "session never free again");
            std::thread::yield_now();
        };
        let early = !reported.load(Ordering::SeqCst);
        worker.join().unwrap();
        assert!(reported.load(Ordering::SeqCst), "job did not report");
        (free_again, early)
    }

    #[test]
    fn stats_do_not_hold_the_session() {
        let Some(path) = test_file() else { return };
        let session = Arc::new(Mutex::new(Session::open(&path).unwrap()));
        let [w, h] = session.lock().unwrap().size();
        let (free_again, _) = regain_session_during(&session, Job::Stats { index: 0, session: session.clone(), rect: [0, 0, w, h] }, |r| matches!(r, JobResult::Stats { .. }));
        assert!(free_again < Duration::from_millis(50), "stats held the session for {free_again:?}");
    }

    #[test]
    fn layers_export_does_not_hold_the_session() {
        let Some(path) = test_file() else { return };
        let session = Arc::new(Mutex::new(Session::open(&path).unwrap()));
        let dir = std::env::temp_dir().join(format!("graineo-layers-{}", std::process::id()));
        let d2 = dir.clone();
        let (free_again, early) = regain_session_during(&session, Job::ExportLayers { session: session.clone(), dir: dir.clone(), flat_field: false }, move |r| matches!(r, JobResult::LayersExported { result: Ok(d) } if *d == d2));
        let _ = std::fs::remove_dir_all(dir);
        assert!(free_again < Duration::from_millis(50), "layers export held the session for {free_again:?}");
        assert!(early, "the session was free only after the export had finished");
    }

    #[test]
    fn spp_install_preview_equals_old_path() {
        let Some(path) = test_file() else { return };
        let settings = Settings::default();
        let key = settings.spp_key(&path);
        let counts = develop::diskcache::working_key(&path, &key, false, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        // old path: install, then preview from the session's base
        let mut session = Session::open(&path).unwrap();
        session.install_spp(key.clone(), counts.clone());
        let old = Preview::from_spp(&session.spp_base().unwrap(), session.spp_matrix().unwrap(), PREVIEW_EDGE);
        // new path: preview from a local base, then install
        let session = Arc::new(Mutex::new(Session::open(&path).unwrap()));
        let new = install_spp_with_preview(&session, key.clone(), counts);
        assert_eq!(old.lin.data, new.lin.data);
        assert_eq!(old.fill_map, new.fill_map);
        assert!(session.lock().unwrap().has_spp_for(&settings));
    }
}

/// Tests that hold the app's global gates (`pools::gate`) or count their waiters run one after another: in one test
/// process a waiter of another test (e.g. the neighbour preload of the lock-up test) is counted as well – the check
/// "my waiter is in line" then passed too early, and the older preload could end up the newer one and wait forever.
#[cfg(test)]
fn global_gates_for_tests() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[path = "jobs_deadlock_tests.rs"]
mod deadlock_tests;

#[cfg(test)]
mod start_values_tests {
    use super::*;

    /// `Loaded::start_values_from_spp`: true for a Sigma file with SIGMA Photo Pro settings and no own edit, false after
    /// an own edit; the shooting-data summary never carries the note. Skipped without the test photo
    /// (`GRAINEO_TEST_SPP_X3F`, else `testbilder/dp2q/SDIM4388.X3F`).
    #[test]
    fn start_values_note_follows_own_edit() {
        let src = crate::env::var_os("GRAINEO_TEST_SPP_X3F").map(PathBuf::from).unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testbilder/dp2q/SDIM4388.X3F"));
        if !src.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("graineo-start-values-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let x3f = dir.join("SDIM4388.X3F");
        let _ = std::fs::remove_file(&x3f);
        std::os::unix::fs::symlink(&src, &x3f).unwrap();

        let (spp, meta) = crate::sidecar::load(&x3f);
        let loaded = load(&x3f, &spp, || true).expect("open").expect("loaded");
        assert!(loaded.start_values_from_spp);
        assert!(loaded.info.iter().any(|(k, _)| k == "Startwerte"));
        assert!(loaded.summary.contains(" mm") && !loaded.summary.contains("SIGMA") && !loaded.summary.contains("Sigma"), "{}", loaded.summary);

        let edited = Settings { exposure: 0.3, ..spp };
        crate::sidecar::save(&x3f, &edited, &meta).unwrap();
        let loaded = load(&x3f, &edited, || true).expect("open").expect("loaded");
        assert!(!loaded.start_values_from_spp);
        assert!(!loaded.info.iter().any(|(k, _)| k == "Startwerte"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Classic cameras open with the exact view as the first picture ([`classic_exact_preview`]). With Classic files in
/// `GRAINEO_TEST_CLASSIC_DIR` (folders separated by ":"; skipped otherwise). With `--nocapture` it prints the colour
/// difference of the first picture against the export (fitted to 1600 px) before (own quick pipeline) and now, and the
/// times to the first picture.
#[cfg(test)]
mod classic_first_picture_tests {
    use super::*;
    use std::time::Instant;

    fn classic_files() -> Vec<PathBuf> {
        let Ok(dirs) = std::env::var("GRAINEO_TEST_CLASSIC_DIR") else { return Vec::new() };
        let mut files: Vec<PathBuf> = dirs
            .split(':')
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("x3f")) && develop::quattro::is_classic(p))
            .collect();
        files.sort();
        files
    }

    /// sRGB (8 bit) → CIE L*a*b* (D65).
    fn lab(p: &[u8]) -> [f64; 3] {
        let l = |v: u8| {
            let c = f64::from(v) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let (r, g, b) = (l(p[0]), l(p[1]), l(p[2]));
        let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
        let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
        let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
        let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
        [116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z))]
    }

    /// CIEDE2000 (Sharma et al. 2005, kL = kC = kH = 1).
    fn de00(a: [f64; 3], b: [f64; 3]) -> f64 {
        let (c1, c2) = (a[1].hypot(a[2]), b[1].hypot(b[2]));
        let cb = (c1 + c2) / 2.0;
        let g = 0.5 * (1.0 - (cb.powi(7) / (cb.powi(7) + 25f64.powi(7))).sqrt());
        let (a1p, a2p) = ((1.0 + g) * a[1], (1.0 + g) * b[1]);
        let (c1p, c2p) = (a1p.hypot(a[2]), a2p.hypot(b[2]));
        let hue = |y: f64, x: f64| if x == 0.0 && y == 0.0 { 0.0 } else { y.atan2(x).to_degrees().rem_euclid(360.0) };
        let (h1, h2) = (hue(a[2], a1p), hue(b[2], a2p));
        let mut dh = h2 - h1;
        if dh > 180.0 {
            dh -= 360.0;
        } else if dh < -180.0 {
            dh += 360.0;
        }
        if c1p * c2p == 0.0 {
            dh = 0.0;
        }
        let (dl, dc) = (b[0] - a[0], c2p - c1p);
        let dh_big = 2.0 * (c1p * c2p).sqrt() * (dh.to_radians() / 2.0).sin();
        let lb = (a[0] + b[0]) / 2.0;
        let cbp = (c1p + c2p) / 2.0;
        let hs = h1 + h2;
        let hb = if c1p * c2p == 0.0 { hs } else if (h1 - h2).abs() > 180.0 { (hs + 360.0) / 2.0 } else { hs / 2.0 };
        let t = 1.0 - 0.17 * (hb - 30.0).to_radians().cos() + 0.24 * (2.0 * hb).to_radians().cos() + 0.32 * (3.0 * hb + 6.0).to_radians().cos() - 0.2 * (4.0 * hb - 63.0).to_radians().cos();
        let dth = 30.0 * (-((hb - 275.0) / 25.0).powi(2)).exp();
        let rc = 2.0 * (cbp.powi(7) / (cbp.powi(7) + 25f64.powi(7))).sqrt();
        let sl = 1.0 + 0.015 * (lb - 50.0).powi(2) / (20.0 + (lb - 50.0).powi(2)).sqrt();
        let (sc, sh) = (1.0 + 0.045 * cbp, 1.0 + 0.015 * cbp * t);
        let rt = -(2.0 * dth).to_radians().sin() * rc;
        ((dl / sl).powi(2) + (dc / sc).powi(2) + (dh_big / sh).powi(2) + rt * (dc / sc) * (dh_big / sh)).sqrt()
    }

    /// Fitted to `edge` px on the longer side (area average).
    fn fitted(img: &develop::Rgb16, edge: u32) -> image::RgbImage {
        let rgb = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.to_rgb8()).unwrap();
        let k = f64::from(edge) / f64::from(rgb.width().max(rgb.height()));
        image::imageops::thumbnail(&rgb, (f64::from(rgb.width()) * k).round() as u32, (f64::from(rgb.height()) * k).round() as u32)
    }

    /// ΔE00 mean, 95th percentile, mean ΔL* of `a` against the reference `r`.
    fn difference(a: &image::RgbImage, r: &image::RgbImage) -> (f64, f64, f64) {
        assert_eq!(a.dimensions(), r.dimensions());
        let (mut de, mut dl) = (Vec::new(), 0.0);
        for (p, q) in a.as_raw().chunks(3).zip(r.as_raw().chunks(3)) {
            let (x, y) = (lab(p), lab(q));
            de.push(de00(x, y));
            dl += x[0] - y[0];
        }
        let n = de.len() as f64;
        let mean = de.iter().sum::<f64>() / n;
        de.sort_by(f64::total_cmp);
        (mean, de[((n * 0.95) as usize).min(de.len() - 1)], dl / n)
    }

    #[test]
    fn classic_first_picture_is_the_exact_view() {
        let settings = Settings::default();
        for path in classic_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            // before: open + own quick previews (what the first picture was made of)
            develop::quattro::forget(&path);
            let t = Instant::now();
            let session = Session::open_for(&path, &settings).unwrap();
            let t_open = t.elapsed();
            let old = previews(&session, &settings);
            let t_before = t.elapsed();
            // the exact view alone (from the finished preparation)
            let t_exact = Instant::now();
            let exact = classic_exact_preview(&session, &settings);
            let t_exact = t_exact.elapsed();
            // now: opening, own previews and the exact view alongside, as in `load`
            develop::quattro::forget(&path);
            let t = Instant::now();
            let session = Session::open_for(&path, &settings).unwrap();
            let (mut now, exact_again) = rayon::join(|| previews(&session, &settings), || classic_exact_preview(&session, &settings));
            now.spp = exact_again;
            let t_now = t.elapsed();
            assert_eq!(exact.map(|p| p.lin.data.clone()), now.spp.as_ref().map(|p| p.lin.data.clone()));
            // the whole opening as the app does it
            develop::quattro::forget(&path);
            let t = Instant::now();
            let loaded = load(&path, &settings, || true).unwrap().unwrap();
            let t_load = t.elapsed();
            assert!(loaded.previews.spp.is_some(), "{name}: opened without the exact view");

            // the first picture renders from the exact view, the same as the view after the base job
            let cal = &session.cal;
            let first = sigma_preview(&now, &settings).render(cal, &settings);
            let exact = Preview::from_spp(&session.spp_base().unwrap(), session.spp_matrix().unwrap(), PREVIEW_EDGE).render(cal, &settings);
            assert_eq!(first.data, exact.data, "{name}: first picture is not the exact view");
            let before = sigma_preview(&old, &settings).render(cal, &settings);
            let export = {
                let mut s = Session::open_for(&path, &settings).unwrap();
                s.render_full(&settings)
            };
            let r = fitted(&export, 1600);
            let (b, n) = (difference(&fitted(&before, 1600), &r), difference(&fitted(&first, 1600), &r));
            eprintln!(
                "{name}: first picture against export (1600 px) before ΔE00 {:.2} / p95 {:.2} / ΔL {:+.2}, now ΔE00 {:.2} / p95 {:.2} / ΔL {:+.2}; \
                 open {:.0} ms, first picture before {:.0} ms, now {:.0} ms (exact view alone {:.0} ms; whole load {:.0} ms)",
                b.0,
                b.1,
                b.2,
                n.0,
                n.1,
                n.2,
                t_open.as_secs_f64() * 1e3,
                t_before.as_secs_f64() * 1e3,
                t_now.as_secs_f64() * 1e3,
                t_exact.as_secs_f64() * 1e3,
                t_load.as_secs_f64() * 1e3
            );
        }
    }
}
