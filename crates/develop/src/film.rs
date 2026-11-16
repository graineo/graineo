//! Physical film simulation via spektrafilm-rs (GPL-3.0, `third_party/spektrafilm-rs`).
//!
//! Pipeline:
//! 1. Scene-linear image: [`crate::render_counts_scene`] or [`crate::render_scene`] return
//!    the values right before Sigma's tone curve (noise reduction, sharpening, Fill Light, presence,
//!    highlight neutralisation already applied) as linear sRGB (D65), normalised so that the
//!    count value that Sigma's tone curve (with the exposure slider, without contrast/shadows/highlights)
//!    maps to 18.4 % grey is 0.184 here ([`scene_gain`]).
//! 2. [`develop`]: expose the film (Hanatos spectral upsampling, CAT16 from D65 to the
//!    film's reference illuminant), develop it (density curves, DIR couplers, grain, halation),
//!    print onto paper (automatic print exposure to middle grey) and scan →
//!    sRGB-encoded 16 bit. This stage replaces Sigma's tone curve and output.
//! 3. After that the look stage ([`crate::look`]) runs as usual.
//!
//! Preview and detail crops are computed on the GPU (wgpu 30, f32), otherwise on the CPU
//! (same formulas). Calibrated pipelines (spectral LUT per film, print normalisation) and
//! profiles are cached. Grain is seeded in a fixed position from the location in the full image
//! ([`Geo`] as in the look stage); pixel size and pixel-based radii follow
//! the scale, so preview, detail and export match.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use spektrafilm_core::params::RuntimeParams;
use spektrafilm_core::pipeline::Pipeline;
use spektrafilm_core::profile::{self, Profile};
use spektrafilm_gpu::ComputeBackend;
use spektrafilm_math::image::ImageBuf;
use spektrafilm_math::precision::{from_f32, to_f32, Scalar};

mod card;
mod border;
mod contact_sheet;
mod tools;
mod light_points;
mod tonal;
mod scan;
mod lut;
mod color_tune;
pub use lut::cube_lut;
pub use scan::scan_bounds;
pub use tonal::{color_response, tone_curves};
/// One film computation at a time (GPU queue, Accelerate BLAS), the visible one first: the gate `pools::gate::FILM`
/// is taken on the calling thread, the computation runs in [`film_pool`] with the caller's intent. A worker of the
/// film pool passes the gate (nested, belongs to the computation in progress); a worker of another pool (e.g. an
/// offline copy in the background pool) waits for the film pool, which gets along without its threads – so no
/// worker ever waits for its own pool (rule of `pools`).
pub(super) fn with_film_gate<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    let intent = pools::intent();
    let _gate = pools::gate::FILM.acquire(intent, None);
    film_pool().install(move || pools::with_intent(intent, f))
}

/// Pool for the parallel parts of a film computation; created on first use, its threads take the priority of the
/// thread that starts them.
fn film_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(4, |n| n.get()).div_ceil(2).max(2);
        rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("film-{i}")).start_handler(|_| pools::mark(pools::Pool::Film)).build().expect("film thread pool")
    })
}
pub use light_points::light_points;
pub use contact_sheet::{contact_sheet_size, contact_sheet, downscaled, CONTACT_SHEET_COLUMNS};
pub use border::{band_extent, paper_texture, HOLE_PITCH, HOLE_W, HOLE_H, HOLE_CENTER, paper_white, border_fractions, apply_border, strip_text, texture_value, BorderFractions};
pub use card::{development_info, filter_transmission, filter_list, card, push_time, default_paper_grade, DevelopmentInfo, FilterInfo, InfoCard, Series};

use crate::look::Geo;
use crate::{Calibration, Linear, Rgb16, Settings};

/// Film simulation settings (part of [`Settings`], default: off).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct FilmSettings {
    /// Film simulation on (off = Sigma's tone curve and output, exactly as without this field)
    pub active: bool,
    /// Film stock (profile ID, e.g. `kodak_portra_400`, see [`films`])
    pub stock: String,
    /// Paper or print film (profile ID); `None` = the film's default paper
    /// ([`StockInfo::default_paper`]). No effect for slide film (scanned directly).
    pub paper: Option<String>,
    /// Negative output: scan the developed film directly (negative with orange mask)
    /// instead of printing it. Slide film is always scanned directly.
    pub negative: bool,
    /// Film exposure in stops (−3 … +3). For negatives the automatic print
    /// exposure compensates the brightness; what changes is the position on the
    /// film curve (density, colour, grain). For slide/negative scans the brightness changes.
    pub exposure: f32,
    /// Print brightness in stops (−2 … +2) relative to the automatic print exposure;
    /// for direct scans an additional exposure
    pub print_exposure: f32,
    /// Print colour via enlarger filtration: warmth −1 (cooler) … +1 (warmer)
    /// and tint −1 (greener) … +1 (magenta); each ±1 ≈ ±15 CC yellow or magenta filter.
    /// Only effective when printing onto paper.
    pub print_warmth: f32,
    pub print_tint: f32,
    /// Grain: strength 0 (off) … 2 (1 = profile) and size 0.5 … 3 (1 = default)
    pub grain: f32,
    pub grain_size: f32,
    /// Grain model: `""` = default for the film ([`grain_model`]), `"even"` = as spektrafilm computes
    /// it (colour films: fast/medium/slow sub-layers; B&W: one crystal size), `"film"` = graineo's
    /// grain whose size follows the density like on real film (coarser in thin parts of the negative).
    pub grain_model: String,
    /// Halation (red halos around bright areas): strength 0 … 3 and radius 0.25 … 3
    pub halation: f32,
    pub halation_radius: f32,
    /// DIR couplers (development inhibition, edge and colour separation) 0 (off) … 2, 1 = profile
    pub couplers: f32,
    /// Contrast of the film or paper as a gamma factor of the density curves 0.5 … 1.5
    pub film_gamma: f32,
    pub paper_gamma: f32,
    /// Paper matteness: surface flare in percent of the illuminating light, in addition
    /// to the viewing glare. 0 = high gloss (black ≈ density 2.0), ≈ 1.7 = matte
    /// baryta paper (Dmax ≈ 1.6), up to 10 = heavily faded; lifts the blacks.
    pub paper_matte: f32,
    /// Black and white: film development time in minutes (0 = profile default, otherwise the
    /// nearest time from [`StockInfo::family`]) and paper contrast grade (−1 = default;
    /// Multigrade: 0 = filter 00, 0.5 = filter 0, 1 … 5)
    pub film_dev: f32,
    pub paper_grade: f32,
    /// Black-and-white paper tone: −1 cool (bluish) … 0 neutral … +1 warm (brownish). Acts via the
    /// spectral density of the paper image, i.e. in highlights and shadows.
    pub paper_tone: f32,
    /// Taking filter in front of the lens (ID from [`filter_list`], empty = none). Exposure
    /// is compensated as with through-the-lens metering; what changes is the tonal rendering of colours.
    pub taking_filter: String,
    /// Taking filter strength 0 … 1 (1 = full filter, 0.5 = half the filter density)
    pub taking_filter_amount: f32,
    /// Push (+) / pull (−) in stops (−2 … +3): exposure that much shorter or longer,
    /// development time about 30 % longer or shorter per stop.
    pub push: f32,
    /// Print border (darkroom): paper border in % of the long edge (0 = off), extra
    /// at the bottom in %, and a black film border with rounded corners. Only written to the file
    /// on "export as print" and shown in the darkroom.
    pub print_border: f32,
    pub print_border_bottom: f32,
    pub film_border: bool,
    /// Film strip style: black strip with perforation, film name/maker and frame number (requires `film_border`)
    pub film_strip: bool,
    /// Paper white of the border and of "on paper": 0 warm (default), 1 neutral, 2 cream
    pub paper_white: u8,
    /// Capture format in mm (longer edge): 35 = 35 mm, 60 = medium format 6×6 (default,
    /// matches the resolution of the Foveon cameras), 120 = large format …
    /// Larger format = finer grain and smaller halos relative to the image.
    pub format_mm: f32,
    /// Neutral balance as in a lab: middle grey is made neutral via filtration when printing
    /// (important for tungsten film in daylight), slide film is brought to middle grey via exposure
    /// (Tri-X). Warmth/tint act relative to it. Off = filtration from the database.
    pub auto_balance: bool,
    /// Paper pre-flash 0 … 1; 1 corresponds to 0.02 in the physical core.
    /// Look switch "Datasheet": the film without its tuned default look (no default filter, no
    /// brightness/paper/grade presets); otherwise "Tuned" (default)
    pub look_datasheet: bool,
    /// "Green" slider (−1 … +1, 0 = the film's default in the selected look): rotates the hue of yellows and greens
    /// by up to ±8° (positive = towards green)
    pub green_tune: f32,
    /// "Blue" slider (−1 … +1, 0 = default): saturation of dark blues up to ±15 % and hue up to ±4°
    pub blue_tune: f32,
    pub preflash: f32,
    pub preflash_warmth: f32,
    pub preflash_tint: f32,
    /// Optics preset: empty/standard = as before, sharp/soft/vintage.
    pub optics: String,
    pub camera_blur_um: Option<f32>,
    pub enlarger_blur: Option<f32>,
    pub scanner_blur: Option<f32>,
    /// Neutral diffusion filter family: empty = off, soft/mist/bloom.
    pub diffusion: String,
    pub diffusion_amount: f32,
    pub diffusion_enlarger: bool,
    /// empty/film keeps the existing halo, none/subtle/strong/red change it.
    pub halation_type: String,
    /// Additional linear scan levels; 0/1 = off. Auto stores global image levels.
    pub scan_black: f32,
    pub scan_white: f32,
    pub scan_auto: bool,
    /// Sigma Merrill, black-and-white film: the film is exposed directly from the three sensor layers
    /// (see [`crate::analog`]) instead of from the colour image. Default on; no effect elsewhere.
    pub layer_exposure: bool,
    /// Grain balances the sensor noise: the film grain is reduced by the noise that is already in the image, so that
    /// both together are as grainy as the film; with colour film the colour noise is smoothed more strongly first
    /// (Sigma Merrill, any film). Default on.
    pub grain_balance: bool,
    /// Set by the caller for the grain balance (not stored): relative sensor noise of the film exposure at
    /// middle grey and the long edge of the full image in pixels ([`crate::analog::film_settings`]).
    #[serde(skip)]
    pub sensor_noise: Option<(f32, usize)>,
}

/// Stored settings of graineo 1.0 use old key names: every load goes through here and translates them once
/// ([`legacy_names::settings_keys`]); writing uses the current keys only.
impl<'de> serde::Deserialize<'de> for FilmSettings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut v = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
        legacy_names::settings_keys::upgrade_develop(&mut v);
        // the inherent function generated by `remote = "Self"` (no loop back into this impl)
        FilmSettings::deserialize(v).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for FilmSettings {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        FilmSettings::serialize(self, s)
    }
}

impl Default for FilmSettings {
    fn default() -> Self {
        Self {
            active: false,
            stock: "kodak_portra_400".into(),
            paper: None,
            negative: false,
            exposure: 0.0,
            print_exposure: 0.0,
            print_warmth: 0.0,
            print_tint: 0.0,
            grain: 1.0,
            grain_size: 1.0,
            grain_model: String::new(),
            halation: 1.0,
            halation_radius: 1.0,
            couplers: 1.0,
            film_gamma: 1.0,
            paper_gamma: 1.0,
            paper_matte: 0.0,
            film_dev: 0.0,
            paper_grade: -1.0,
            paper_tone: 0.0,
            taking_filter: String::new(),
            taking_filter_amount: 1.0,
            push: 0.0,
            print_border: 0.0,
            print_border_bottom: 0.0,
            film_border: false,
            film_strip: false,
            paper_white: 0,
            format_mm: 60.0,
            auto_balance: true,
            look_datasheet: false,
            green_tune: 0.0,
            blue_tune: 0.0,
            preflash: 0.0,
            preflash_warmth: 0.0,
            preflash_tint: 0.0,
            optics: String::new(),
            camera_blur_um: None,
            enlarger_blur: None,
            scanner_blur: None,
            diffusion: String::new(),
            diffusion_amount: 0.5,
            diffusion_enlarger: false,
            halation_type: String::new(),
            scan_black: 0.0,
            scan_white: 1.0,
            scan_auto: false,
            layer_exposure: true,
            grain_balance: true,
            sensor_noise: None,
        }
    }
}

impl FilmSettings {
    /// Is the film simulation active (switched on and a film selected)?
    pub fn is_active(&self) -> bool {
        self.active && !self.stock.is_empty()
    }
}

/// Group of a film or paper.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// Colour negative film (photography)
    ColorNegative,
    /// Slide film (reversal film, scanned directly)
    Slide,
    /// Cinema negative film
    Cine,
    /// Black-and-white film (negative and reversal)
    BlackWhite,
    /// Photo paper
    Paper,
    /// Cinema print film (positive film for projection)
    PrintFilm,
}

impl Category {
    /// Display name (German text, used as translation key).
    pub fn label(self) -> &'static str {
        match self {
            Category::ColorNegative => "Farbnegativ",
            Category::Slide => "Dia",
            Category::Cine => "Kino",
            Category::BlackWhite => "Schwarzweiß",
            Category::Paper => "Papier",
            Category::PrintFilm => "Kinokopie",
        }
    }
}

/// Default paper for black-and-white negatives without their own target paper (punchy print instead of cinema print film).
pub const DEFAULT_BW_PAPER: &str = "ilford_multigrade_iv_rc";
/// Default contrast grade of this paper (filter 3) while the paper grade is set to "Std".
const DEFAULT_BW_GRADE: f64 = 3.0;

/// A film or paper from the spektrafilm profiles.
#[derive(Clone, Debug)]
pub struct StockInfo {
    /// Profile ID (file name without `.json`), value for [`FilmSettings::stock`]/`paper`
    pub id: String,
    /// Display name from the profile (e.g. "Kodak Portra 400")
    pub name: String,
    pub category: Category,
    /// Default paper (`target_print` of the profile; black-and-white negative: Kodak 2302);
    /// `None` for slide film and for papers
    pub default_paper: Option<String>,
    /// Slide/reversal film: scanned directly, paper and print colour have no effect
    pub positive: bool,
    pub monochrome: bool,
    /// Black and white: development times (film) or contrast grades (paper) of the profile; otherwise empty
    pub family: Vec<f32>,
    /// Built from datasheets (profile carries an info card); otherwise a profile from spektrafilm
    pub custom: bool,
}

/// All films, sorted by group and name. Empty if the data is missing.
pub fn films() -> &'static [StockInfo] {
    &catalog().0
}

/// All papers and print films, sorted by group and name.
pub fn papers() -> &'static [StockInfo] {
    &catalog().1
}

/// Film or paper by ID.
pub fn stock(id: &str) -> Option<&'static StockInfo> {
    films().iter().chain(papers()).find(|s| s.id == id)
}

/// Directory with the spektrafilm data (profiles/, luts/, filters/), if found: in the app bundle
/// `Contents/Resources/spektrafilm` or next to the executable `spektrafilm/`, then `GRAINEO_FILM_DATA`,
/// otherwise (development) `third_party/spektrafilm-rs/data` in the source tree above the program,
/// `CARGO_MANIFEST_DIR` (at run time) or the working folder.
pub fn data_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        data_dir_candidates(
            std::env::var_os("GRAINEO_FILM_DATA").map(PathBuf::from),
            exe.as_deref(),
            std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from),
            std::env::current_dir().ok(),
        )
        .into_iter()
        .find(|p| p.join("profiles").is_dir() && p.join("luts").is_dir())
        .map(|p| p.canonicalize().unwrap_or(p))
    })
    .as_deref()
}

/// Places to look for the film data, in order: the data shipped with the app, then `GRAINEO_FILM_DATA`, then
/// the source tree, found upwards from the program's location (`target/release/graineo` or a test in
/// `target/release/deps`), from `CARGO_MANIFEST_DIR` at run time (`cargo test`) or from the working folder –
/// never from a path built into the program, which would carry the developer's home folder into every release.
fn data_dir_candidates(env: Option<PathBuf>, exe: Option<&Path>, manifest: Option<PathBuf>, cwd: Option<PathBuf>) -> Vec<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    let dir = exe.and_then(Path::parent);
    if let Some(dir) = dir {
        c.push(dir.join("../Resources/spektrafilm"));
        c.push(dir.join("spektrafilm"));
    }
    c.extend(env);
    let tree = |start: &Path| start.ancestors().take(8).map(|a| a.join("third_party/spektrafilm-rs/data")).collect::<Vec<_>>();
    if let Some(dir) = dir {
        c.extend(tree(dir));
    }
    if let Some(m) = manifest {
        c.extend(tree(&m));
    }
    if let Some(d) = cwd {
        c.extend(tree(&d));
    }
    c
}

fn catalog() -> &'static (Vec<StockInfo>, Vec<StockInfo>) {
    static CAT: OnceLock<(Vec<StockInfo>, Vec<StockInfo>)> = OnceLock::new();
    CAT.get_or_init(|| {
        let (mut films, mut papers) = (Vec::new(), Vec::new());
        let Some(dir) = data_dir() else { return (films, papers) };
        let Ok(entries) = std::fs::read_dir(dir.join("profiles")) else { return (films, papers) };
        let mut ids: Vec<String> =
            entries.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string)).collect();
        ids.sort();
        for id in ids {
            let Ok(p) = load_profile(&id) else { continue };
            let print_stage = p.info.stage == "printing" || p.is_paper();
            let category = match () {
                _ if print_stage && p.is_paper() => Category::Paper,
                _ if print_stage => Category::PrintFilm,
                _ if p.is_bw() => Category::BlackWhite,
                _ if p.is_positive() => Category::Slide,
                _ if p.info.usage == "cine" => Category::Cine,
                _ => Category::ColorNegative,
            };
            let default_paper = if print_stage || p.is_positive() {
                None
            } else {
                p.info.target_print.clone().or_else(|| p.info.default_look.as_ref().and_then(|l| l.paper.clone())).or_else(|| p.is_bw().then(|| DEFAULT_BW_PAPER.to_string()))
            };
            let info = StockInfo {
                name: p.info.name.clone().unwrap_or_else(|| id.clone()),
                id,
                category,
                default_paper,
                positive: p.is_positive() && !print_stage,
                monochrome: p.is_bw(),
                family: if p.is_bw() { p.data.development_time.iter().map(|&t| t as f32).collect() } else { Vec::new() },
                custom: card::is_custom_built(&p.info),
            };
            if print_stage {
                papers.push(info);
            } else {
                films.push(info);
            }
        }
        let key = |s: &StockInfo| (s.category, s.name.to_lowercase());
        films.sort_by_key(key);
        papers.sort_by_key(key);
        // Default paper only if the profile exists
        let known: Vec<String> = papers.iter().map(|p| p.id.clone()).collect();
        for f in &mut films {
            if f.default_paper.as_ref().is_some_and(|p| !known.contains(p)) {
                f.default_paper = known.first().cloned();
            }
        }
        (films, papers)
    })
}

/// Load a profile (cached; the JSON files are up to 400 kB).
pub(crate) fn load_profile(id: &str) -> Result<Profile, String> {
    static CACHE: Mutex<Vec<(String, Arc<Profile>)>> = Mutex::new(Vec::new());
    if let Some((_, p)) = CACHE.lock().unwrap().iter().find(|(k, _)| k == id) {
        return Ok((**p).clone());
    }
    let dir = data_dir().ok_or("Filmdaten (spektrafilm) nicht gefunden")?;
    let p = profile::load_profile_by_name(dir, id).map_err(|e| format!("Filmprofil {id}: {e}"))?;
    CACHE.lock().unwrap().push((id.to_string(), Arc::new(p.clone())));
    Ok(p)
}

/// Compute path of the film simulation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// GPU if available, otherwise CPU (default)
    Auto,
    /// always CPU (rayon + Accelerate)
    Cpu,
}

static FORCE_CPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set the compute path (for measurements; `GRAINEO_FILM_CPU=1` has the same effect).
pub fn set_backend(b: Backend) {
    FORCE_CPU.store(b == Backend::Cpu, std::sync::atomic::Ordering::Relaxed);
}

/// Name of the compute path that would be used right now ("wgpu" or "cpu").
pub fn backend_name() -> &'static str {
    if use_gpu() { "wgpu" } else { "cpu" }
}

/// Does the film simulation compute in f64 (feature `film-f64`)?
pub const F64: bool = cfg!(feature = "film-f64");

fn gpu() -> Option<&'static dyn ComputeBackend> {
    static GPU: OnceLock<Option<Box<dyn ComputeBackend>>> = OnceLock::new();
    GPU.get_or_init(|| {
        spektrafilm_gpu::wgpu_backend::WgpuBackend::new().map(|b| Box::new(b) as Box<dyn ComputeBackend>)
    })
    .as_deref()
}

/// The film computation on the GPU failed once (see [`develop_with`]): the CPU computes from then on.
static GPU_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn use_gpu() -> bool {
    !F64
        && !GPU_FAILED.load(std::sync::atomic::Ordering::Relaxed)
        && !FORCE_CPU.load(std::sync::atomic::Ordering::Relaxed)
        && std::env::var_os("GRAINEO_FILM_CPU").is_none()
        && pools::gpu_allowed()
        && gpu().is_some()
}

/// Film, paper and whether it is scanned directly.
fn resolve(fs: &FilmSettings) -> Result<(String, String, bool), String> {
    let film = stock(&fs.stock).ok_or_else(|| format!("Film unbekannt: {}", fs.stock))?;
    if film.positive || fs.negative {
        return Ok((film.id.clone(), film.id.clone(), true));
    }
    let paper = fs.paper.clone().or_else(|| film.default_paper.clone()).ok_or("kein Papier")?;
    Ok((film.id.clone(), paper, false))
}

/// spektrafilm parameters for an image `w`×`h` at position `geo`.
fn runtime_params(fs: &FilmSettings, geo: Geo, w: usize, h: usize, scan: bool, corr: Correction) -> RuntimeParams {
    let mut p = RuntimeParams::default();
    let scale = geo.scale.max(1e-3);
    // The film's default look: the sliders act relative to it (paper values only with the default paper)
    let look = look_for(fs);
    let paper = resolve(fs).ok().map(|(_, paper, _)| paper);
    let multigrade = paper.as_deref().is_some_and(|p| p.starts_with("ilford_multigrade"));
    let look_paper = paper.is_some() && (multigrade || paper.as_deref() == look.paper.as_deref());
    let pl = |v: Option<f64>, neutral: f32| if look_paper { v.map_or(neutral, |v| v as f32) } else { neutral };
    let print_ev = fs.print_exposure + if scan { look.print_ev.unwrap_or(0.0) as f32 } else { pl(look.print_ev, 0.0) };
    // Input and output: linear sRGB (D65) in, sRGB-encoded out
    p.io.input_color_space = "sRGB".into();
    p.io.input_cctf_decoding = false;
    p.io.output_color_space = "sRGB".into();
    p.io.output_cctf_encoding = true;
    p.io.scan_film = scan;
    // Exposure: fixed normalisation (middle grey 0.184) instead of metering per image,
    // otherwise the exposure would depend on the crop
    p.camera.auto_exposure = false;
    p.camera.exposure_compensation_ev = (fs.exposure - fs.push.clamp(-2.0, 3.0)).clamp(-6.0, 6.0) + if scan { print_ev + corr.ev } else { 0.0 };
    if !scan {
        p.enlarger.print_exposure = 2f32.powf(print_ev.clamp(-4.0, 4.0));
        // Enlarger filtration: more yellow → bluer print, more magenta → greener
        p.enlarger.y_filter_shift = corr.y - 15.0 * fs.print_warmth.clamp(-2.0, 2.0);
        p.enlarger.m_filter_shift = corr.m - 15.0 * fs.print_tint.clamp(-2.0, 2.0);
    }
    // Pixel size on the film: format relative to the full image, at this image's scale
    let full_edge = geo.full_w.max(geo.full_h).max(1) as f32;
    p.camera.film_format_mm = fs.format_mm.clamp(4.0, 250.0) * w.max(h) as f32 * scale / full_edge;
    // Grain: strength via the crystal area (noise ∝ √area), size via the blur of the
    // dye clouds (in full image pixels) with correspondingly fewer, larger crystals
    let g = &mut p.film_render.grain;
    if fs.grain > 0.0 {
        let size = fs.grain_size.clamp(0.25, 4.0) as f64;
        // calibrated per film to the datasheet graininess (profile field `grain_scale`, otherwise 1)
        let grain_calib = load_profile(&fs.stock).ok().and_then(|p| p.info.grain_scale).unwrap_or(1.0);
        g.agx_particle_area_um2 *= (fs.grain.clamp(0.0, 3.0) as f64 * grain_calib * look.grain.unwrap_or(1.0)).powi(2) * size * size;
        g.blur = 0.65 * size as f32 / scale;
        // grain model: even = spektrafilm's sub-layer grain (colour) / single size (B&W),
        // film = crystal size classes (see `grain_size_classes`)
        if grain_model(fs) == GRAIN_FILM {
            g.size_classes = grain_size_classes(&fs.stock);
        } else {
            g.use_sublayers = true;
        }
        g.origin = Some([
            (geo.off_x as f32 / scale).round() as u32,
            (geo.off_y as f32 / scale).round() as u32,
            (geo.full_w as f32 / scale).round().max(w as f32) as u32,
        ]);
    } else {
        g.active = false;
    }
    let hal = &mut p.film_render.halation;
    hal.halation_amount = fs.halation.clamp(0.0, 5.0) as f64;
    hal.halation_spatial_scale = fs.halation_radius.clamp(0.1, 5.0) as f64;
    let dir = &mut p.film_render.dir_couplers;
    dir.amount = fs.couplers.clamp(0.0, 3.0) as f64;
    dir.active = fs.couplers > 0.0;
    p.film_render.density_curve_gamma = (fs.film_gamma * look.film_gamma.unwrap_or(1.0) as f32).clamp(0.3, 3.0);
    let paper_gamma = (fs.paper_gamma * pl(look.paper_gamma, 1.0)).clamp(0.3, 3.0);
    // Papers without a fitted curve model (Multigrade) take the gamma on the stored curves;
    // papers with a model (all colour papers, 2383/2393) only take it through the print-curve
    // morph of spektrafilm (`density_curves_morph.gamma_factor`), otherwise it has no effect.
    p.print_render.density_curve_gamma = paper_gamma;
    let morph = &mut p.print_render.density_curves_morph;
    morph.active = paper_gamma != 1.0;
    morph.gamma_factor = paper_gamma as f64;
    p.print_render.glare.percent += (fs.paper_matte + pl(look.paper_matte, 0.0)).clamp(0.0, 10.0);
    if scan {
        // Slide film/direct scan: the same matteness as viewing flare (lifted blacks). The
        // original has no glare when scanning film, so only the matteness counts (no default
        // 0.03 % veil); with matteness 0 the glare is off.
        p.film_render.glare.percent = fs.paper_matte.clamp(0.0, 10.0);
        p.film_render.glare.active = p.film_render.glare.percent > 0.0;
    }
    p.film_render.development_time = if fs.film_dev > 0.0 {
        Some(fs.film_dev as f64)
    } else if fs.push != 0.0 {
        push_time(&fs.stock, look.film_dev, fs.push.clamp(-2.0, 3.0)).or(look.film_dev)
    } else {
        look.film_dev
    };
    p.print_render.development_time = if fs.paper_grade >= 0.0 {
        Some(fs.paper_grade as f64)
    } else if look_paper && !scan {
        look.paper_grade.or(multigrade.then_some(DEFAULT_BW_GRADE))
    } else {
        None
    };
    // Viewing glare as a uniform veil (no randomness, so crop and
    // full image stay identical); convert blurs in pixels to the scale
    for gl in [&mut p.film_render.glare, &mut p.print_render.glare] {
        gl.roughness = 0.0;
        gl.blur /= scale;
    }
    p.scanner.unsharp_mask[0] /= scale;
    tools::parameter(fs, &mut p, scale, scan);
    override_params(p)
}

/// Grain model "even" (shown as "Gleichmäßig"): as spektrafilm computes it, the grain size does not follow the density.
pub const GRAIN_EVEN: &str = "even";
/// Grain model "film" (shown as "Wie Film"): the grain size follows the density as on real film (crystal size classes).
pub const GRAIN_FILM: &str = "film";

/// Effective grain model of the settings: the stored choice, otherwise the default of the film –
/// "film" for negatives (B&W and colour), "even" for slide film (until checked against real
/// slide scans).
pub fn grain_model(fs: &FilmSettings) -> &'static str {
    match fs.grain_model.as_str() {
        GRAIN_EVEN => GRAIN_EVEN,
        GRAIN_FILM => GRAIN_FILM,
        _ if stock(&fs.stock).is_some_and(|f| f.positive) => GRAIN_EVEN,
        _ => GRAIN_FILM,
    }
}

/// Do the stored previews of photos with these settings show a grain that the app computes differently
/// now? (The grain model was introduced: until then one crystal size everywhere.) Only settings without
/// an own choice, with film and grain: colour films always (both models differ from before), B&W only
/// with the "film" model and size classes in the profile.
pub fn grain_changed_since_models(fs: &FilmSettings) -> bool {
    if !fs.is_active() || fs.grain <= 0.0 || !fs.grain_model.is_empty() {
        return false;
    }
    let Ok(p) = load_profile(&fs.stock) else { return false };
    if !p.is_bw() {
        return true;
    }
    grain_model(fs) == GRAIN_FILM && p.info.grain_size_classes.is_some()
}

/// Crystal size classes of the "film" grain: the profile field `grain_size_classes` (B&W: size
/// spread from the crystal type), colour films without it use their sub-layer curves. `None`: single
/// size (B&W profile without the field).
fn grain_size_classes(stock: &str) -> Option<spektrafilm_core::params::GrainSizeClasses> {
    let p = load_profile(stock).ok()?;
    p.info.grain_size_classes.or_else(|| {
        (!p.is_bw()).then_some(spektrafilm_core::params::GrainSizeClasses {
            classes: 3,
            area_spread: 1.0,
            speed_exponent: 1.0,
            film_layers: true,
        })
    })
}

/// Developer aid: `GRAINEO_FILM_PARAMS=file.json` overrides individual spektrafilm parameters
/// (same structure as `RuntimeParams`, only the fields present).
fn override_params(p: RuntimeParams) -> RuntimeParams {
    let Some(path) = std::env::var_os("GRAINEO_FILM_PARAMS") else { return p };
    fn merge(a: &mut serde_json::Value, b: serde_json::Value) {
        match (a, b) {
            (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
                for (k, v) in b {
                    merge(a.entry(k).or_insert(serde_json::Value::Null), v);
                }
            }
            (a, b) => *a = b,
        }
    }
    let Ok(text) = std::fs::read_to_string(&path) else { return p };
    let (Ok(mut base), Ok(patch)) = (serde_json::to_value(&p), serde_json::from_str::<serde_json::Value>(&text)) else { return p };
    merge(&mut base, patch);
    serde_json::from_value(base).unwrap_or(p)
}

/// Key of the calibrated pipeline: everything that `Pipeline::new_with_spectral` bakes in
/// (like the cache of the spektrafilm GUI).
fn pipeline_key(film: &str, paper: &str, p: &RuntimeParams) -> String {
    format!(
        "{film}|{paper}|{}|{:?}|{:?}|{}|{}|{}|{}|{}",
        p.print_render.glare.percent,
        p.film_render.development_time,
        p.print_render.development_time,
        p.io.scan_film,
        p.enlarger.y_filter_shift,
        p.enlarger.m_filter_shift,
        p.camera.exposure_compensation_ev,
        p.film_render.density_curve_gamma
    )
}

/// Calibrated pipeline from the cache (or new, ≈ 50–200 ms).
fn pipeline(film: &str, paper: &str, tone: f32, filter: &str, strength: f32, params: &RuntimeParams) -> Result<Arc<Pipeline>, String> {
    static CACHE: Mutex<Vec<(String, Arc<Pipeline>)>> = Mutex::new(Vec::new());
    let key = format!("{}|{:.3}|{filter}|{strength:.2}|{}", pipeline_key(film, paper, params), tone, tools::calibration_key(params));
    if let Some((_, p)) = CACHE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return Ok(p.clone());
    }
    let dir = data_dir().ok_or("Filmdaten (spektrafilm) nicht gefunden")?;
    let mut paper_profile = load_profile(paper)?;
    if tone != 0.0 && paper_profile.is_bw() {
        // Paper tone: more density in the blue (warm) or in the red (cool), neutral around 550 nm
        let a = 0.2 * tone.clamp(-1.0, 1.0) as f64;
        let wl = paper_profile.data.wavelengths.clone();
        for (row, w) in paper_profile.data.channel_density.iter_mut().zip(wl) {
            if let Some(d) = row.first_mut() {
                *d *= (1.0 + a * (550.0 - w) / 150.0).max(0.2);
            }
        }
    }
    let film_profile = profile_with_filter(film, filter, strength)?;
    let t = std::time::Instant::now();
    let pipe = Arc::new(Pipeline::new_with_spectral(film_profile, paper_profile, params.clone(), dir)?);
    if timing() {
        eprintln!("film: calibrated pipeline {film}/{paper} in {:.1?}", t.elapsed());
    }
    let mut cache = CACHE.lock().unwrap();
    // all films twice over (film tiles of "Film & Look" and the darkroom shelf, contact sheet, test strip) plus slider
    // positions: with 48 the tiles of the first photo pushed each other out and every further photo calibrated all
    // films again (≈ 50–500 ms each, measured 16.11.2026); a pipeline is small (no measurable RSS for 40 of them)
    if cache.len() >= 128 {
        cache.remove(0);
    }
    cache.push((key, pipe.clone()));
    Ok(pipe)
}

pub(crate) fn profile_with_filter(film: &str, filter: &str, strength: f32) -> Result<Profile, String> {
    let mut film_profile = load_profile(film)?;
    if !filter.is_empty() {
        // Taking filter: sensitivity × transmittance; total daylight (D55) exposure per
        // layer restored to the old value (through-the-lens metering), only colour rendering changes
        use spektrafilm_math::spectral::ILLUMINANT_D55_F64 as D55;
        // Strength = share of the filter density (T to the power of strength): 0.5 is a filter half as dense
        let filter_factor = |w: f64| filter_transmission(filter, w).max(1e-6).powf(strength as f64);
        let wl = film_profile.data.wavelengths.clone();
        let channels = film_profile.data.log_sensitivity.iter().map(|r| r.len()).max().unwrap_or(0);
        for c in 0..channels {
            let (mut without_filter, mut with_filter) = (0.0f64, 0.0f64);
            for (i, (row, &w)) in film_profile.data.log_sensitivity.iter().zip(&wl).enumerate() {
                if let Some(&v) = row.get(c).filter(|v| v.is_finite()) {
                    let e = D55.get(i).copied().unwrap_or(0.0);
                    without_filter += 10f64.powf(v) * e;
                    with_filter += 10f64.powf(v) * filter_factor(w) * e;
                }
            }
            let k = if with_filter > 0.0 { (without_filter / with_filter).log10() } else { 0.0 };
            for (row, &w) in film_profile.data.log_sensitivity.iter_mut().zip(&wl) {
                if let Some(v) = row.get_mut(c).filter(|v| v.is_finite()) {
                    *v += filter_factor(w).max(1e-6).log10() + k;
                }
            }
        }
    }
    Ok(film_profile)
}

/// Factor count value → scene-linear sRGB (middle grey 0.184): the count value that Sigma's
/// tone curve (colour mode, exposure slider; contrast, shadows and highlights neutral) maps to
/// 18.4 % becomes 0.184. This places the film exposure where Sigma's development
/// puts the midtones, and the exposure slider acts like a real exposure.
pub fn scene_gain(cal: &Calibration, s: &Settings) -> f32 {
    let mut t = s.clone();
    t.contrast = 0.0;
    t.shadow = 0.0;
    t.highlight = 0.0;
    t.monochrome = false;
    let mode = cal.mode(s.color_mode.as_deref());
    let tone = crate::cached_tone_lut(&mode.tone, &t, 0.0, cal.pixel_saturation, cal.is_prepared());
    let target = (0.184 * 65535.0) as u16;
    let (mut lo, mut hi) = (0usize, (cal.max_count() as usize).min(tone.len() - 1));
    if tone[hi] < target {
        return 0.184 / hi.max(1) as f32;
    }
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if tone[mid] >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.184 / hi.max(1) as f32
}

/// Margin in full image pixels that a crop additionally needs so that halation,
/// DIR diffusion and blurs at the edge behave as in the full image.
pub fn margin(fs: &FilmSettings, full_w: usize, full_h: usize) -> usize {
    let pix_um = fs.format_mm.clamp(4.0, 250.0) * 1000.0 / full_w.max(full_h).max(1) as f32;
    let mut um: f32 = 15.0; // scattering in the film
    if fs.couplers > 0.0 {
        um = um.max(200.0); // tail of the DIR diffusion
    }
    if fs.halation > 0.0 {
        um = um.max(65.0 * 2.0 * fs.halation_radius.clamp(0.1, 5.0));
    }
    // The new optical effects also need their context for detail crops.
    if fs.diffusion_amount>0.0 {um=um.max(match fs.diffusion.as_str(){"soft"=>1000.,"mist"=>2000.,"bloom"=>2500.,_=>0.});}
    if matches!(fs.halation_type.as_str(),"strong"|"red"){um=um.max(200.0*fs.halation_radius.clamp(0.1,5.0));}
    ((3.0 * um / pix_um).ceil() as usize + 8).min(600)
}

/// Develop the film: `scene` is scene-linear sRGB (see module docs), `geo` its position in the
/// full image. Result sRGB-encoded like [`crate::render_counts`].
pub fn develop(scene: &Linear, fs: &FilmSettings, geo: Geo) -> Result<Rgb16, String> {
    let corr = correction(fs)?;
    // colour film with grain balance: smooth the sensor's colour noise first (the film's dye clouds replace it)
    let smoothed = match fs.sensor_noise.filter(|_| fs.grain_balance && !stock(&fs.stock).is_some_and(|f| f.monochrome)) {
        Some((noise, _)) => Some(smooth_chroma(scene, chroma_sigma(noise) / geo.scale.max(1e-3))),
        None => None,
    };
    let fs = grain_balanced(fs, corr)?;
    develop_with(smoothed.as_ref().unwrap_or(scene), &fs, geo, corr)
}

/// Colour film: share of the sensor noise that counts for the grain balance. In the photos the colour film chain
/// (couplers, dye clouds of three layers) swallows more of the real noise than of the flat test noise; set so that
/// Portra 400 and Ektar 100 at ISO 1600/3200 on DP2 Merrill photos keep the graininess of ISO 100 (measured).
pub const COLOR_NOISE_SCALE: f32 = 0.6;

/// Strength of the colour noise smoothing for colour film (Gaussian σ in full-resolution pixels) from the relative
/// sensor noise: ≈ 1 px at ISO 100 … 4 px at ISO 3200.
pub fn chroma_sigma(noise: f32) -> f32 {
    (noise * 35.0).clamp(1.0, 4.0)
}

/// Colour noise smoothing: the chromaticity (RGB / luminance) is smoothed with a separable joint bilateral filter guided
/// by the log luminance (edges of brightness stop it, so colours do not bleed across them); the luminance stays.
pub fn smooth_chroma(scene: &Linear, sigma: f32) -> Linear {
    use rayon::prelude::*;
    let (w, h) = (scene.width, scene.height);
    if sigma < 0.3 || w < 3 || h < 3 {
        return scene.clone();
    }
    let r = (2.0 * sigma).ceil() as isize;
    let ks: Vec<f32> = (-r..=r).map(|d| (-((d * d) as f32) / (2.0 * sigma * sigma)).exp()).collect();
    let y: Vec<f32> = scene.data.chunks_exact(3).map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).max(1e-6)).collect();
    let ly: Vec<f32> = y.iter().map(|v| v.ln()).collect();
    let mut c: Vec<f32> = scene.data.chunks_exact(3).zip(&y).flat_map(|(p, &yy)| [p[0] / yy, p[1] / yy, p[2] / yy]).collect();
    let inv_range = 1.0 / (2.0 * 0.12f32 * 0.12);
    for horizontal in [true, false] {
        let src = c.clone();
        c.par_chunks_mut(w * 3).enumerate().for_each(|(yy, row)| {
            for x in 0..w {
                let i0 = yy * w + x;
                let (mut acc, mut wsum) = ([0.0f32; 3], 0.0f32);
                for (k, kv) in ks.iter().enumerate() {
                    let d = k as isize - r;
                    let (xx, yv) = if horizontal { ((x as isize + d).clamp(0, w as isize - 1) as usize, yy) } else { (x, (yy as isize + d).clamp(0, h as isize - 1) as usize) };
                    let j = yv * w + xx;
                    let dl = ly[j] - ly[i0];
                    let wt = kv * (-dl * dl * inv_range).exp();
                    for ch in 0..3 {
                        acc[ch] += wt * src[j * 3 + ch];
                    }
                    wsum += wt;
                }
                for ch in 0..3 {
                    row[x * 3 + ch] = acc[ch] / wsum;
                }
            }
        });
    }
    let data = c.chunks_exact(3).zip(&y).flat_map(|(q, &yy)| [q[0] * yy, q[1] * yy, q[2] * yy]).collect();
    Linear { data, ..scene.clone() }
}

/// Grain balance ([`FilmSettings::grain_balance`]): with sensor noise attached ([`FilmSettings::sensor_noise`]) the
/// grain strength is lowered so that sensor noise and grain together have the graininess of the film alone at middle
/// grey (variances add). Graininess = standard deviation of the output at full resolution; the sensor noise reaches
/// the output through the slope of the film/print curve at middle grey. Noise above the film's graininess: no grain.
fn grain_balanced(fs: &FilmSettings, corr: Correction) -> Result<std::borrow::Cow<'_, FilmSettings>, String> {
    let Some((noise, edge)) = fs.sensor_noise.filter(|_| fs.grain_balance && fs.grain > 0.0) else {
        return Ok(std::borrow::Cow::Borrowed(fs));
    };
    let base = FilmSettings { sensor_noise: None, ..fs.clone() };
    let noise = if stock(&fs.stock).is_some_and(|f| f.monochrome) { noise } else { noise * COLOR_NOISE_SCALE };
    let k = grain_factor(&base, noise, edge, corr)?;
    if timing() {
        let (g, n) = grain_and_noise(&base, noise, edge, corr)?;
        eprintln!("film: grain balance: sensor noise {:.2} %, graininess film {:.2} / noise {:.2} (8-bit levels), grain × {k:.3}", noise * 100.0, g * 255.0, n * 255.0);
    }
    Ok(std::borrow::Cow::Owned(FilmSettings { grain: base.grain * k, ..base }))
}

/// Factor on the grain strength for the grain balance (cached per settings and noise): the factor `k` for which a flat
/// middle grey with the sensor noise and grain `k · grain` is as grainy as the film alone without noise. Both are rendered
/// through the whole film chain, because the chain itself softens the noise (optical blur; the dye clouds of the grain
/// blur the exposure as well, so noise and grain do not simply add). Bisection on `k`, at least [`GRAIN_BALANCE_MIN`]
/// so that the grain's own softening stays.
pub fn grain_factor(fs: &FilmSettings, noise: f32, edge: usize, corr: Correction) -> Result<f32, String> {
    static CACHE: Mutex<Vec<(String, f32)>> = Mutex::new(Vec::new());
    let key = format!("{fs:?}|{noise:.5}|{edge}|{corr:?}");
    if let Some((_, k)) = CACHE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return Ok(*k);
    }
    let target = flat_sigma(fs, fs.grain, 0.0, edge, corr)?;
    let total = |k: f32| flat_sigma(fs, fs.grain * k, noise, edge, corr);
    let k = if total(1.0)? <= target {
        1.0
    } else if total(GRAIN_BALANCE_MIN)? >= target {
        GRAIN_BALANCE_MIN
    } else {
        let (mut lo, mut hi) = (GRAIN_BALANCE_MIN, 1.0f32);
        for _ in 0..7 {
            let mid = 0.5 * (lo + hi);
            if total(mid)? > target {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        0.5 * (lo + hi)
    };
    let mut cache = CACHE.lock().unwrap();
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((key, k));
    Ok(k)
}

/// Smallest grain factor of the grain balance (a trace of grain keeps the film's softening of the noise).
pub const GRAIN_BALANCE_MIN: f32 = 0.05;

/// Graininess of the film alone and of the sensor noise alone after the film chain (for measurements and the log).
pub fn grain_and_noise(fs: &FilmSettings, noise: f32, edge: usize, corr: Correction) -> Result<(f32, f32), String> {
    Ok((flat_sigma(fs, fs.grain, 0.0, edge, corr)?, flat_sigma(fs, 0.0, noise, edge, corr)?))
}

/// Fine structure ([`fine_sigma`]) of the output of a flat middle grey with grain strength `grain` and white sensor noise of
/// relative strength `noise`, at full resolution of an image with long edge `edge` (luminance, sRGB-encoded 0…1).
fn flat_sigma(fs: &FilmSettings, grain: f32, noise: f32, edge: usize, corr: Correction) -> Result<f32, String> {
    let n = 128;
    let field = noise_field(n);
    let data: Vec<f32> = field.iter().flat_map(|&u| {
        let v = 0.184 * (1.0 + noise * u).max(0.0);
        [v, v, v]
    }).collect();
    let scene = Linear { mono: false, haze: None, toneq: None, width: n, height: n, data, detail: None };
    let edge = edge.max(4 * n);
    let geo = Geo { full_w: edge, full_h: edge * 2 / 3, off_x: edge / 2 - n / 2, off_y: edge / 3 - n / 2, scale: 1.0, camera: 0 };
    let img = develop_with(&scene, &FilmSettings { active: true, grain, sensor_noise: None, ..fs.clone() }, geo, corr)?;
    // luminance (mean of the channels): the sensor noise left after the colour smoothing is luminance noise, and the
    // film's colour grain stays as it is (it replaces the colour noise)
    let g: Vec<f64> = (0..n * n).map(|i| (img.data[i * 3] as f64 + img.data[i * 3 + 1] as f64 + img.data[i * 3 + 2] as f64) / (3.0 * 65535.0)).collect();
    Ok(fine_sigma(&g, n, n) as f32)
}

/// Noise field like the sensor noise after Sigma's processing: deterministic Gaussian white noise, softened with a
/// Gaussian of σ = 0.6 px (measured on DP2 Merrill photos: the fine structure at σ = 4 px is 1.3–1.8 times the
/// pixel-to-pixel noise, white noise would give 1.0), scaled to a fine structure ([`fine_sigma`]) of 1.
fn noise_field(n: usize) -> Vec<f32> {
    static FIELD: OnceLock<Vec<f32>> = OnceLock::new();
    FIELD
        .get_or_init(|| {
            let hash = |i: u64| -> f64 {
                let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
                x ^= x >> 31;
                x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                x ^= x >> 29;
                (x >> 40) as f64 / (1u64 << 24) as f64
            };
            let white: Vec<f64> = (0..n * n).map(|p| ((0..4).map(|k| hash((p * 4 + k) as u64)).sum::<f64>() - 2.0) * 3f64.sqrt()).collect();
            let k: Vec<f64> = (-2i32..=2).map(|d| (-(d * d) as f64 / (2.0 * 0.36)).exp()).collect();
            let ks: f64 = k.iter().sum();
            let pass = |src: &[f64], horizontal: bool| -> Vec<f64> {
                (0..n * n)
                    .map(|i| {
                        let (x, y) = (i % n, i / n);
                        k.iter().enumerate().map(|(j, kv)| {
                            let d = j as isize - 2;
                            let (xx, yy) = if horizontal { ((x as isize + d).rem_euclid(n as isize) as usize, y) } else { (x, (y as isize + d).rem_euclid(n as isize) as usize) };
                            kv * src[yy * n + xx]
                        }).sum::<f64>() / ks
                    })
                    .collect()
            };
            let soft = pass(&pass(&white, true), false);
            let s = fine_sigma(&soft, n, n).max(1e-9);
            soft.iter().map(|v| (v / s) as f32).collect()
        })
        .clone()
}

/// Standard deviation of the fine structure of a flat image: image minus its Gaussian blur (σ = 4 px), inside a
/// margin of 3σ. The same measure is used for the sensor noise table (`analog::data`) and in
/// `tools/measure/grain`, so that grain and noise are compared at the same scale (coarse noise blotches look
/// different from grain and are not counted).
fn fine_sigma(v: &[f64], w: usize, h: usize) -> f64 {
    let sigma = 4.0f64;
    let r = (3.0 * sigma) as usize;
    let k: Vec<f64> = (0..=2 * r).map(|i| (-((i as f64 - r as f64).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let ks: f64 = k.iter().sum();
    let blur = |src: &[f64], horizontal: bool| -> Vec<f64> {
        let mut out = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let d = i as isize - r as isize;
                    let (xx, yy) = if horizontal { ((x as isize + d).clamp(0, w as isize - 1) as usize, y) } else { (x, (y as isize + d).clamp(0, h as isize - 1) as usize) };
                    s += kv * src[yy * w + xx];
                }
                out[y * w + x] = s / ks;
            }
        }
        out
    };
    let lo = blur(&blur(v, true), false);
    let d: Vec<f64> = (r..h - r).flat_map(|y| (r..w - r).map(move |x| (y, x))).map(|(y, x)| v[y * w + x] - lo[y * w + x]).collect();
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    (d.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (d.len() - 1) as f64).sqrt()
}

/// Automatic correction from the neutral balance ([`FilmSettings::auto_balance`]):
/// exposure (slide, EV) or yellow/magenta filter (print, CC) on top of the database.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Correction {
    pub ev: f32,
    pub y: f32,
    pub m: f32,
}

/// Neutral balance for these settings (cached; new ≈ 30–150 ms).
pub fn correction(fs: &FilmSettings) -> Result<Correction, String> {
    if !fs.auto_balance {
        return Ok(Correction::default());
    }
    let (_, paper, scan) = resolve(fs)?;
    let film = stock(&fs.stock).ok_or("Film unbekannt")?;
    if scan && !film.positive {
        return Ok(Correction::default()); // negative scan: orange mask stays
    }
    if !scan && film.monochrome && stock(&paper).is_some_and(|p| p.monochrome) {
        return Ok(Correction::default()); // black and white on black and white: nothing to filter
    }
    let key = format!("{}|{paper}|{scan}|{}|{}|{}|{}|{}|{}", fs.stock, fs.exposure, fs.film_gamma, fs.paper_gamma, fs.paper_matte, fs.film_dev, fs.paper_grade);
    let key = format!("{key}|{}|{}|{}|{}",fs.preflash,fs.preflash_warmth,fs.preflash_tint,fs.look_datasheet);
    static CACHE: Mutex<Vec<(String, Correction)>> = Mutex::new(Vec::new());
    if let Some((_, c)) = CACHE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return Ok(*c);
    }
    let t_total = std::time::Instant::now();
    let probe_fs = FilmSettings { print_exposure: 0.0, print_warmth: 0.0, print_tint: 0.0, ..fs.clone() };
    let probe = |c: Correction| -> Result<[f32; 3], String> {
        let g = gray_with(&probe_fs, c)?;
        Ok(g.map(|v| srgb_decode(v).max(1e-5)))
    };
    let mut c = Correction::default();
    if scan {
        // Slide: middle grey to 0.184 (secant method on the exposure)
        let err = |g: [f32; 3]| (0.2126 * g[0] + 0.7152 * g[1] + 0.0722 * g[2]).log2() - 0.184f32.log2();
        let (mut ev0, mut e0) = (0.0f32, err(probe(c)?));
        let mut ev1 = -e0;
        for _ in 0..6 {
            if e0.abs() < 0.02 {
                break;
            }
            c.ev = ev1;
            let e1 = err(probe(c)?);
            let slope = if (e1 - e0).abs() > 1e-4 { (e1 - e0) / (ev1 - ev0) } else { 1.0 };
            (ev0, e0) = (ev1, e1);
            ev1 -= e1 / slope.clamp(0.2, 5.0);
            c.ev = ev0;
        }
        // The film's default brightness (look) should be preserved: the probe ran with it, so the balance
        // factored it out; add it back here
        let look_ev = look_for(fs).print_ev.unwrap_or(0.0) as f32;
        c.ev = (ev0 + look_ev).clamp(-4.0, 4.0);
    } else {
        // Print: bring green via magenta filter and blue via yellow filter to the geometric mean
        let err = |g: [f32; 3]| {
            let gm = (g[0] * g[1] * g[2]).cbrt();
            [(g[1] / gm).ln(), (g[2] / gm).ln()]
        };
        let mut e = err(probe(c)?);
        let mut k = [0.06f32, 0.06]; // ln change per CC (start value, tracked)
        for _ in 0..8 {
            if e[0].abs() < 0.003 && e[1].abs() < 0.003 {
                break;
            }
            let step = [-e[0] / k[0], -e[1] / k[1]].map(|v| v.clamp(-40.0, 40.0));
            let next = Correction { m: c.m + step[0], y: c.y + step[1], ev: 0.0 };
            let e2 = err(probe(next)?);
            for i in 0..2 {
                let d = e2[i] - e[i];
                if step[i].abs() > 0.5 && d / step[i] > 0.005 {
                    k[i] = d / step[i];
                }
            }
            (c, e) = (next, e2);
        }
        c.m = c.m.clamp(-80.0, 80.0);
        c.y = c.y.clamp(-80.0, 80.0);
    }
    if timing() {
        eprintln!("film: neutral balance in {:.1?}", t_total.elapsed());
    }
    let mut cache = CACHE.lock().unwrap();
    // one entry per film and settings state; room for all film tiles and the open film's slider positions
    if cache.len() >= 128 {
        cache.remove(0);
    }
    cache.push((key, c));
    Ok(c)
}

/// The film's default look (`default_look` of the profile); empty in "Datasheet" mode (datasheet values only).
pub(crate) fn look_for(fs: &FilmSettings) -> profile::DefaultLook {
    if fs.look_datasheet {
        return profile::DefaultLook::default();
    }
    load_profile(&fs.stock).ok().and_then(|p| p.info.default_look).unwrap_or_default()
}

pub(crate) fn effective_filter(fs: &FilmSettings) -> (String, f32) {
    // Taking filter: explicit choice, otherwise the one belonging to the film (default look)
    if fs.taking_filter == "none" {
        return (String::new(), 1.0); // explicitly no filter, not even the look's
    }
    let look = look_for(fs);
    let (filter, strength) = if fs.taking_filter.is_empty() {
        (look.taking_filter.clone().unwrap_or_default(), look.taking_filter_amount.unwrap_or(1.0) as f32)
    } else {
        (fs.taking_filter.clone(), fs.taking_filter_amount.clamp(0.0, 1.0))
    };
    (filter, strength)
}

/// `GRAINEO_FILM_TIMING=1`: times of calibration, neutral balance and image computation on stderr (measurements).
fn timing() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var_os("GRAINEO_FILM_TIMING").is_some())
}

fn develop_with(scene: &Linear, fs: &FilmSettings, geo: Geo, corr: Correction) -> Result<Rgb16, String> {
    let (film, paper, scan) = resolve(fs)?;
    let (w, h) = (scene.width, scene.height);
    let params = runtime_params(fs, geo, w, h, scan, corr);
    let (filter, strength) = effective_filter(fs);
    let pipe = pipeline(&film, &paper, if scan { 0.0 } else { fs.paper_tone }, &filter, strength, &params)?;
    let pipe = (*pipe).clone().with_params(params);
    let image = || ImageBuf::from_data(w as u32, h as u32, scene.data.iter().map(|&v| from_f32(v)).collect::<Vec<Scalar>>());
    // One film computation at a time, the visible one first (see `with_film_gate`)
    let t_img = std::time::Instant::now();
    let out = with_film_gate(|| {
        // The GPU computes images up to its size limit in one piece (≈ 67 MP) and while the job fits a quarter
        // of the memory (`pools::gpu_fits_memory`); larger ones (e.g. 100-MP exports) on the CPU. A GPU failure (device lost, out of memory, driver error) costs this one
        // computation on the CPU instead of the export, and the film stays on the CPU from then on.
        if use_gpu() && w * h <= gpu().map_or(0, |g| g.max_pixels()) && pools::gpu_fits_memory(w * h) {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pipe.process(image(), gpu().unwrap()))) {
                Ok(out) => return out,
                Err(_) => {
                    eprintln!("film: GPU computation failed ({w}×{h}), computing on the CPU from now on");
                    GPU_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        pipe.process(image(), &spektrafilm_gpu::cpu_backend::CpuBackend)
    });
    if out.width as usize != w || out.height as usize != h {
        return Err(format!("Filmsimulation: unerwartete Größe {}×{}", out.width, out.height));
    }
    use rayon::prelude::*;
    let mut data = out.data.par_iter().map(|&v| (to_f32(v).clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect::<Vec<_>>();
    // Colour care (green, dark blue): default of the "Tuned" look plus sliders; "Datasheet" computes it plainly
    if timing() && w * h > 4096 {
        eprintln!("film: image {w}×{h} in {:.1?}", t_img.elapsed());
    }
    color_tune::apply(&mut data, color_tune::tune_for(&fs.stock, fs.look_datasheet, fs.green_tune, fs.blue_tune));
    scan::apply_scan_levels(&mut data, fs, scan);
    Ok(Rgb16 { width: w, height: h, data })
}

/// Copy rectangle `[x, y, w, h]` from an image.
pub(crate) fn crop_rgb16(img: &Rgb16, x: usize, y: usize, w: usize, h: usize) -> Rgb16 {
    let mut data = Vec::with_capacity(w * h * 3);
    for yy in y..y + h {
        data.extend_from_slice(&img.data[(yy * img.width + x) * 3..(yy * img.width + x + w) * 3]);
    }
    Rgb16 { width: w, height: h, data }
}

/// Small preview image per film for an image grid in the UI.
pub struct Thumb {
    pub stock: &'static StockInfo,
    pub image: Result<Rgb16, String>,
}

/// Render all films (or `ids`) on the scene-linear image `scene` (e.g. 256 px long edge,
/// see [`crate::session::Preview::film_scene`]) with their default paper and otherwise the
/// settings `base`. The look stage is not included.
/// First call ≈ calibration of all films, afterwards only the image computation.
pub fn thumbnails(scene: &Linear, base: &FilmSettings, geo: Geo, ids: Option<&[&str]>) -> Vec<Thumb> {
    films()
        .iter()
        .filter(|f| ids.is_none_or(|ids| ids.contains(&f.id.as_str())))
        .map(|f| {
            let fs = FilmSettings { active: true, stock: f.id.clone(), paper: None, ..base.clone() };
            Thumb { stock: f, image: develop(scene, &fs, geo) }
        })
        .collect()
}

/// Rendering of middle grey (scene 0.184): sRGB-encoded output 0 … 1 per channel (without grain).
/// For checking the exposure per film (`graineo-dev film --list`).
pub fn gray_response(fs: &FilmSettings) -> Result<[f32; 3], String> {
    gray_with(fs, correction(fs)?)
}

fn gray_with(fs: &FilmSettings, corr: Correction) -> Result<[f32; 3], String> {
    let (w, h) = (16, 16);
    let scene = Linear { mono: false, haze: None, toneq: None, width: w, height: h, data: vec![0.184; w * h * 3], detail: None };
    let fs = FilmSettings { active: true, grain: 0.0, ..fs.clone() };
    let img = develop_with(&scene, &fs, Geo::full(w * 256, h * 256), corr)?;
    let c = ((h / 2) * w + w / 2) * 3;
    Ok([0, 1, 2].map(|i| img.data[c + i] as f32 / 65535.0))
}

fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grain model: stored choice wins; otherwise "film" for negatives, "even" for slide film.
    #[test]
    fn grain_model_defaults() {
        let neg = FilmSettings { active: true, stock: "kodak_portra_400".into(), ..FilmSettings::default() };
        let slide = FilmSettings { stock: "fujifilm_velvia_100".into(), ..neg.clone() };
        let bw = FilmSettings { stock: "ilford_hp5_plus".into(), ..neg.clone() };
        assert_eq!(grain_model(&neg), GRAIN_FILM);
        assert_eq!(grain_model(&bw), GRAIN_FILM);
        assert_eq!(grain_model(&slide), GRAIN_EVEN);
        assert_eq!(grain_model(&FilmSettings { grain_model: GRAIN_EVEN.into(), ..neg.clone() }), GRAIN_EVEN);
        assert_eq!(grain_model(&FilmSettings { grain_model: GRAIN_FILM.into(), ..slide.clone() }), GRAIN_FILM);
        // older settings without the field read as "" (film default)
        let old: FilmSettings = serde_json::from_str(r#"{"active":true,"stock":"kodak_portra_400"}"#).unwrap();
        assert!(old.grain_model.is_empty() && grain_model(&old) == GRAIN_FILM);
    }

    /// The film gate is held on the calling thread while the work runs in the film pool, also when called from a
    /// worker of another pool (here rayon's global pool); the caller's intent arrives in the pool.
    #[test]
    fn film_gate_is_taken_outside_the_film_pool() {
        let inside = || {
            let held = pools::gate::FILM.snapshot().held;
            (pools::current(), held, pools::intent())
        };
        let plain = pools::with_intent(pools::Intent::Soon, || with_film_gate(inside));
        assert_eq!(plain, (pools::Thread::Worker(pools::Pool::Film), true, pools::Intent::Soon));
        let from_worker = rayon::scope(|_| pools::with_intent(pools::Intent::Idle, || with_film_gate(inside)));
        assert_eq!(from_worker, (pools::Thread::Worker(pools::Pool::Film), true, pools::Intent::Idle));
        assert!(!pools::gate::FILM.snapshot().held, "gate released");
    }

    #[test]
    fn default_is_off() {
        assert!(!Settings::default().film.is_active());
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.film.is_active());
        let f: FilmSettings = serde_json::from_str(r#"{"active":true,"stock":"kodak_gold_200"}"#).unwrap();
        assert!(f.is_active() && f.grain == 1.0 && f.auto_balance);
    }

    /// Look switch and "No filter": Tuned brings the default filter, Datasheet and "none" do not.
    #[test]
    fn look_mode_and_no_filter() {
        let mut f = FilmSettings { active: true, stock: "kodak_trix_400".into(), ..Default::default() };
        assert_eq!(effective_filter(&f), ("gelb15".to_string(), 0.2));
        f.taking_filter = "none".into();
        assert_eq!(effective_filter(&f).0, "");
        f.taking_filter.clear();
        f.look_datasheet = true;
        assert_eq!(effective_filter(&f).0, "");
        assert!(look_for(&f).print_ev.is_none() && look_for(&f).paper_grade.is_none());
        f.taking_filter = "gelb8".into();
        assert_eq!(effective_filter(&f).0, "gelb8");
        // old files without the field load as "Tuned"
        let alt: FilmSettings = serde_json::from_str(r#"{"active":true,"stock":"kodak_trix_400"}"#).unwrap();
        assert!(!alt.look_datasheet);
    }

    /// Crop with margin = the same area of the full image (grain fixed in place, halation/DIR at the edge).
    #[test]
    fn crop_matches_full_image() {
        if data_dir().is_none() {
            return;
        }
        let (w, h) = (360, 240);
        let data = (0..w * h * 3).map(|i| 0.02 + 0.6 * ((i / 3) % w) as f32 / w as f32 + 0.1 * (i % 3) as f32).collect();
        let full = Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
        let fs = FilmSettings { active: true, stock: "kodak_portra_400".into(), format_mm: 4.0, ..FilmSettings::default() };
        let whole = develop(&full, &fs, Geo::full(w, h)).unwrap();
        let (x, y, cw, ch) = (150, 90, 60, 50);
        let m = margin(&fs, w, h).min(60);
        let (ex, ey) = (x - m.min(x), y - m.min(y));
        let (ew, eh) = ((x + cw + m).min(w) - ex, (y + ch + m).min(h) - ey);
        let part = develop(&full.crop(ex, ey, ew, eh), &fs, Geo { full_w: w, full_h: h, off_x: ex, off_y: ey, scale: 1.0, camera: 0 }).unwrap();
        let part = crop_rgb16(&part, x - ex, y - ey, cw, ch);
        let mut diff = 0f64;
        for yy in 0..ch {
            for xx in 0..cw * 3 {
                diff += (part.data[yy * cw * 3 + xx] as f64 - whole.data[((y + yy) * w + x) * 3 + xx] as f64).abs();
            }
        }
        let mean = diff / (cw * ch * 3) as f64 / 257.0;
        assert!(mean < 0.5, "mean deviation {mean} (8-bit steps)");
    }
}

#[cfg(test)]
mod data_dir_tests {
    use super::*;

    /// The film data are found without a built-in source path: from the program in `target/release(/deps)` and
    /// from the bundle; the program contains no `CARGO_MANIFEST_DIR` literal for them.
    #[test]
    fn data_dir_without_built_in_path() {
        let root = Path::new("/projekt");
        let from_test = data_dir_candidates(None, Some(&root.join("target/release/deps/develop-123")), None, None);
        assert!(from_test.contains(&root.join("third_party/spektrafilm-rs/data")));
        let from_dev = data_dir_candidates(None, Some(&root.join("target/release/graineo-dev")), None, None);
        assert!(from_dev.contains(&root.join("third_party/spektrafilm-rs/data")));
        let from_app = data_dir_candidates(Some(PathBuf::from("/env")), Some(Path::new("/Applications/graineo.app/Contents/MacOS/graineo")), None, None);
        assert_eq!(from_app[0], Path::new("/Applications/graineo.app/Contents/MacOS/../Resources/spektrafilm"));
        assert_eq!(from_app[2], Path::new("/env"), "app resources first, then GRAINEO_FILM_DATA");
        let from_cwd = data_dir_candidates(None, None, None, Some(root.join("crates/develop")));
        assert!(from_cwd.contains(&root.join("third_party/spektrafilm-rs/data")));
        let src = include_str!("film.rs");
        let needle = ["env!(\"CARGO_MANIFEST", "_DIR\"), \"/../../third_party"].concat();
        assert!(!src.contains(&needle));
        assert!(data_dir().is_some(), "film data found in the source tree");
    }
}
