//! Merging exposure brackets (2–9 RAWs of one camera) into one linear high-dynamic-range image.
//!
//! The result is white-balanced camera RGB of the reference frame in reference units (1.0 ≈ white of the reference,
//! values above 1 carry the extra highlight range) – the same convention as a demosaiced `rawdev` stage, so the whole
//! development (sliders, masks, film, export) applies unchanged.
//!
//! Steps (see the module docs): [`frames`] load the normalised mosaics and measure clip levels, [`align`] registers
//! every frame onto the reference (similarity model), [`exposure`] measures the exposure factors, [`merge`] streams the
//! frames through demosaic, warp and noise-optimal weighting, [`ghost`] decides which frame counts where when
//! something moved (reference chain, motion mask, pyramid blend, brush strokes).
//!
//! Own implementation after the literature (Granados et al. 2010, Hasinoff et al. 2010 for the weights; Baker &
//! Matthews 2004 for the alignment); the histogram-based clip level follows an idea of HDRMerge (idea only, no code).

pub mod align;
pub mod exposure;
pub mod frames;
pub mod fusion;
pub mod ghost;
pub mod img;
pub mod merge;
pub mod regions;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub use merge::Merged;

/// Version of the algorithm; stored with every result (a new version does not recompute by itself).
pub const ALGO_VERSION: u32 = 4;
/// Smallest and largest number of frames.
pub const MIN_FRAMES: usize = 2;
pub const MAX_FRAMES: usize = 9;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    TooFewFrames(usize),
    TooManyFrames(usize),
    /// format not supported (not a Bayer/X-Trans mosaic, Nikon HE, …)
    Unsupported { path: PathBuf, why: String },
    Read { path: PathBuf, why: String },
    /// frames from different cameras or sensor sizes
    Mismatch { path: PathBuf },
    /// the camera was turned between the frames (portrait and landscape in one bracket): the sensor sizes agree, the
    /// pictures do not lie on each other
    Turned { path: PathBuf },
    Cancelled,
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match self {
            Error::TooFewFrames(n) => write!(f, "{n} frames: at least {MIN_FRAMES} needed"),
            Error::TooManyFrames(n) => write!(f, "{n} frames: at most {MAX_FRAMES} supported"),
            Error::Unsupported { path, why } => write!(f, "{}: {why}", name(path)),
            Error::Read { path, why } => write!(f, "{}: {why}", name(path)),
            Error::Mismatch { path } => write!(f, "{}: other camera or sensor size", name(path)),
            Error::Turned { path } => write!(f, "{}: other orientation (portrait and landscape mixed)", name(path)),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Internal(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Timing of the steps (set `GRAINEO_HDR_PROFILE=1`; one line per step on stderr, summed per label at the end by the
/// caller). Used by `graineo-dev hdr` for the step table of the performance work.
pub fn profile<T>(label: &str, f: impl FnOnce() -> T) -> T {
    if std::env::var_os("GRAINEO_HDR_PROFILE").is_none() {
        return f();
    }
    let t = std::time::Instant::now();
    let r = f();
    eprintln!("hdr-profile {label} {:.3}", t.elapsed().as_secs_f64());
    r
}

/// Cancellation and progress of a running merge.
pub struct Control<'a> {
    pub cancel: &'a AtomicBool,
    /// progress 0 … 1 of the current phase
    pub progress: &'a (dyn Fn(f32) + Sync),
}

impl Control<'_> {
    pub fn check(&self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn report(&self, f: f32) {
        (self.progress)(f.clamp(0.0, 1.0))
    }
}

/// User settings of a merge (stored as JSON with the result, English keys).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// index of the reference frame in the source list; `None` = middle exposure
    pub reference: Option<usize>,
    /// align the frames (similarity model); off = frames are used as shot
    pub align: bool,
    /// ghost handling (moving subjects), see [`ghost`]
    pub ghost: ghost::Level,
    /// brush strokes of the first versions ("Quelle malen", forced source frame per area); read from older recipes
    /// only and turned into [`Self::regions`] by [`Self::migrate`]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paint: Vec<ghost::Stroke>,
    /// areas taken from one frame ("Geister entfernen"), applied after the merge ([`regions`]); later areas win
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<regions::Area>,
    /// delivery of the result: linear radiance or exposure fusion (see [`fusion`]); left out of the stored recipe when
    /// linear, so recipes and caches of earlier versions stay valid
    #[serde(skip_serializing_if = "fusion::Output::is_default")]
    pub output: fusion::Output,
    /// "Fusion" only: how strongly highlights and shadows are evened out, 0 = the reference exposure, 1 = the full
    /// fusion (see [`fusion::blend`]). Recipes without it are 1 (as computed before the setting existed); left out of
    /// the stored recipe at 1, so their keys and caches stay valid.
    #[serde(default = "full_strength", skip_serializing_if = "is_full_strength")]
    pub fusion_strength: f32,
    /// "Linear" only: automatic tone settings written to the develop sliders of the merged photo. Does not change the
    /// pixels (left out of the cache key, see `library::merge`). `None`: off – recipes made before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_tone: Option<AutoTone>,
}

/// Automatic tone of a linear merge: strength and the slider values it set last.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AutoTone {
    /// 0 … 1 (0 = off): scales the automatic values
    pub strength: f32,
    /// the values it set last (exposure, highlights, shadows, whites, blacks); sliders that differ from them were moved
    /// by hand and are not overwritten by an automatic re-tuning. `None`: not set yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<[f32; 5]>,
}

/// Default strength of the automatic tone of a new linear merge.
pub const AUTO_TONE_DEFAULT: f32 = 0.5;
/// Default fusion strength of a new merge.
pub const FUSION_STRENGTH_DEFAULT: f32 = 0.7;

fn full_strength() -> f32 {
    1.0
}

fn is_full_strength(v: &f32) -> bool {
    *v == 1.0
}

impl Default for Settings {
    /// The settings of a recipe that stores nothing: what earlier versions computed (fusion at full strength, no
    /// automatic tone). A new merge starts from [`Settings::new_merge`].
    fn default() -> Self {
        Self {
            reference: None,
            align: true,
            ghost: ghost::Level::default(),
            paint: Vec::new(),
            regions: Vec::new(),
            output: fusion::Output::default(),
            fusion_strength: 1.0,
            auto_tone: None,
        }
    }
}

impl Settings {
    /// Settings of a new merge: automatic tone at [`AUTO_TONE_DEFAULT`], fusion at [`FUSION_STRENGTH_DEFAULT`].
    pub fn new_merge() -> Settings {
        Settings { fusion_strength: FUSION_STRENGTH_DEFAULT, auto_tone: Some(AutoTone { strength: AUTO_TONE_DEFAULT, applied: None }), ..Default::default() }
    }

    /// Turn the brush strokes of older recipes into areas (see [`regions::from_strokes`]); nothing to do without them.
    pub fn migrate(&mut self) {
        if !self.paint.is_empty() {
            let strokes = std::mem::take(&mut self.paint);
            let mut areas = regions::from_strokes(&strokes);
            areas.append(&mut self.regions);
            self.regions = areas;
        }
    }

    /// The settings the merge itself depends on: without the areas (applied afterwards) and the delivery (linear or
    /// fusion, applied afterwards). Their JSON gives the key of the cached linear merge.
    pub fn base(&self) -> Settings {
        Settings { paint: Vec::new(), regions: Vec::new(), output: fusion::Output::Linear, fusion_strength: 1.0, auto_tone: None, ..self.clone() }
    }
}

/// Something the user should know about a merge (the UI words it).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Warning {
    /// the measured exposure step deviates from EXIF by more than 0.3 EV (light changed?); EXIF was used
    ExifDeviation { dark: usize, bright: usize, ev: f64 },
    /// the step could not be measured (too little overlap) and EXIF was used
    NotMeasured { dark: usize, bright: usize },
    /// the step could not be measured and the files carry no exposure time either: the two frames were taken as
    /// equally exposed (factor 1), which is most likely wrong
    ExposureUnknown { dark: usize, bright: usize },
    /// a frame had to be moved by this many pixels (corner) – hand-held?
    LargeShift { frame: usize, px: f64 },
    /// the alignment measured for this frame cannot be right (far beyond what a bracket moves, or the frame fits the
    /// reference worse with it than without): the frame is used as shot
    AlignmentRejected { frame: usize },
    /// the frames use different apertures (depth of field differs)
    ApertureDiffers,
    /// gap between neighbouring exposures larger than 3 EV
    LargeGap { dark: usize, bright: usize, ev: f64 },
}

/// Corner shift above which [`Warning::LargeShift`] is given (fraction of the long edge).
const LARGE_SHIFT: f64 = 0.02;

/// A measured alignment is dropped when the frame agrees with the reference worse with it than as shot by more than
/// this ([`align::agreement`], correlation).
const ALIGN_WORSE: f64 = 0.02;

/// Frames whose orientation flags differ (portrait/landscape) count as one bracket only when the pictures agree at
/// least this well ([`align::agreement`]): the flag alone is not trusted (a camera pointing up or down sets it at
/// random), the picture is.
const TURNED_AGREEMENT: f64 = 0.35;

/// Does the camera's orientation flag (EXIF 1 … 8) say "turned by a quarter" (portrait)?
fn quarter_turned(orientation: u16) -> bool {
    (5..=8).contains(&orientation)
}

/// Everything measured before the pixels are merged.
pub struct Plan {
    pub frames: Vec<frames::Frame>,
    /// frame indices dark → bright
    pub order: Vec<usize>,
    pub reference: usize,
    pub warps: Vec<align::Warp>,
    /// frames whose measured alignment was rejected (they are used as shot)
    pub rejected: Vec<usize>,
    /// exposure factor per frame relative to the reference
    pub k: Vec<f64>,
    pub steps: Vec<exposure::Step>,
    /// noise model of each frame (green, white-balanced units): (a, b)
    pub noise: Vec<(f32, f32)>,
    /// white balance of the reference (applied to all frames)
    pub wb: [f32; 3],
    /// crop of the reference (as the single-frame development crops)
    pub crop: raw::Region,
    pub warnings: Vec<Warning>,
    /// the settings the plan was made with (ghost level and strokes are used when merging)
    pub settings: Settings,
}

impl Plan {
    /// Highlight headroom of the result over the reference white: 1 / smallest exposure factor for the linear
    /// result, 1 for the fusion (its range is compressed to 0 … 1).
    pub fn headroom(&self) -> f32 {
        if self.settings.output == fusion::Output::Fusion {
            return 1.0;
        }
        let kmin = self.k.iter().copied().fold(f64::MAX, f64::min);
        (1.0 / kmin.max(1e-6)) as f32
    }
}

/// Summary of a plan for storing and display.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    pub reference: usize,
    /// EV of each frame relative to the reference (measured)
    pub ev: Vec<f64>,
    /// corner shift per frame in pixels
    pub shift: Vec<f64>,
    pub warnings: Vec<Warning>,
}

/// Load and analyse the frames: alignment, exposure factors, noise models, warnings.
pub fn analyze(paths: &[PathBuf], settings: &Settings, ctl: &Control) -> Result<Plan, Error> {
    if paths.len() < MIN_FRAMES {
        return Err(Error::TooFewFrames(paths.len()));
    }
    if paths.len() > MAX_FRAMES {
        return Err(Error::TooManyFrames(paths.len()));
    }
    // the files decode side by side (the threads of the merge pool; one at a time on a single thread)
    let done = std::sync::atomic::AtomicUsize::new(0);
    let loaded: Vec<Result<frames::Frame, Error>> = {
        use rayon::prelude::*;
        paths
            .par_iter()
            .map(|p| {
                ctl.check()?;
                let f = profile("load", || frames::load(p))?;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                ctl.report(n as f32 / paths.len() as f32 * 0.7);
                Ok(f)
            })
            .collect()
    };
    let mut fs = Vec::with_capacity(paths.len());
    for f in loaded {
        fs.push(f?);
    }
    plan_from_frames(fs, settings, ctl)
}

/// Rough exposure ratio frame/reference from the proxies when EXIF is missing (medians of unsaturated green).
fn rough_ratio(f: &frames::Frame, r: &frames::Frame) -> f64 {
    let med = |fr: &frames::Frame| {
        let mut v: Vec<f32> = fr.g.p.iter().zip(&fr.mx.p).filter(|(g, m)| **m < 0.85 && **g > 0.004).map(|(g, _)| *g).collect();
        if v.is_empty() {
            return f64::NAN;
        }
        let m = v.len() / 2;
        v.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
        v[m] as f64
    };
    let q = med(f) / med(r);
    if q.is_finite() && q > 0.0 {
        q
    } else {
        1.0
    }
}

/// [`analyze`] on frames that are already loaded (tests, synthetic brackets).
pub fn plan_from_frames(fs: Vec<frames::Frame>, settings: &Settings, ctl: &Control) -> Result<Plan, Error> {
    plan_with_warps(fs, settings, ctl, None)
}

/// [`plan_from_frames`] with the alignment of an earlier plan of the same frames, reference and alignment setting
/// (only the motion or the output changed: the alignment is not measured again).
pub fn plan_with_warps(fs: Vec<frames::Frame>, settings: &Settings, ctl: &Control, known: Option<&[align::Warp]>) -> Result<Plan, Error> {
    plan_with_alignment(fs, settings, ctl, known.map(|w| (w, &[][..])))
}

/// [`plan_with_warps`] with the frames whose measured alignment was rejected in that earlier plan
/// ([`Plan::rejected`]): the new plan says so again ([`Warning::AlignmentRejected`]).
pub fn plan_with_alignment(fs: Vec<frames::Frame>, settings: &Settings, ctl: &Control, known: Option<(&[align::Warp], &[usize])>) -> Result<Plan, Error> {
    let n = fs.len();
    if n < MIN_FRAMES {
        return Err(Error::TooFewFrames(n));
    }
    if n > MAX_FRAMES {
        return Err(Error::TooManyFrames(n));
    }
    frames::check_compatible(&fs)?;
    // an aperture missing in single frames must not distort the EXIF ratios (order, alignment range, plausibility
    // check of the measured factors)
    let mut fs = fs;
    frames::harmonize_exif(&mut fs);
    // dark → bright by EXIF; without EXIF by the brightness of the proxies
    let mut order: Vec<usize> = (0..n).collect();
    let brightness: Vec<f64> = if fs.iter().all(|f| f.exif.is_some()) {
        fs.iter().map(|f| f.exif.unwrap_or(1.0)).collect()
    } else {
        fs.iter().map(|f| rough_ratio(f, &fs[0])).collect()
    };
    order.sort_by(|&a, &b| brightness[a].total_cmp(&brightness[b]));
    let reference = settings.reference.filter(|r| *r < n).unwrap_or(order[n / 2]);
    let fr = &fs[reference];
    let wb = fr.info.wb.unwrap_or([1.0; 3]);
    let (w, h) = (fr.w, fr.h);
    let mut warnings = Vec::new();
    let t_align = std::time::Instant::now();
    // alignment on the log-clamped proxies, scaled by the expected exposure ratio
    let mut warps = vec![align::Warp::default(); n];
    let mut rejected: Vec<usize> = Vec::new();
    if let Some((k, was_rejected)) = known.filter(|k| k.0.len() == n && settings.align) {
        warps = k.to_vec();
        for i in was_rejected.iter().copied().filter(|i| *i < n && *i != reference) {
            rejected.push(i);
            warnings.push(Warning::AlignmentRejected { frame: i });
        }
        for (i, wp) in warps.iter().enumerate() {
            let shift = wp.corner_shift(w, h);
            if i != reference && shift > LARGE_SHIFT * w.max(h) as f64 {
                warnings.push(Warning::LargeShift { frame: i, px: shift });
            }
        }
    } else {
        for i in 0..n {
            if i == reference {
                continue;
            }
            // (frames of the other orientation are looked at also without alignment)
            let turned = quarter_turned(fs[i].info.orientation) != quarter_turned(fr.info.orientation);
            if !settings.align && !turned {
                continue;
            }
            ctl.check()?;
            let f = &fs[i];
            let ke = match (f.exif, fr.exif) {
                (Some(a), Some(b)) if b > 0.0 => a / b,
                _ => rough_ratio(f, fr),
            };
            let hi = (fr.clip[1].min(f.clip[1] / ke as f32)) * 0.9;
            let lo = 0.004f32.max(0.004 / ke as f32);
            if hi <= lo * 1.5 {
                continue; // no common valid range: keep as shot
            }
            let rr = fr.g.log_clamped(1.0, lo, hi);
            let mm = f.g.log_clamped(ke as f32, lo, hi);
            let as_shot = align::agreement(&rr, &mm, &align::Warp::default(), lo, hi);
            let mut aligned = None;
            if settings.align {
                let found = align::align(&rr, &mm, lo, hi, align::Model::Similarity).warp;
                // plausibility: within what a bracket can move, and the frame fits the reference at least as well as
                // it does as shot – a measurement that went astray would smear the frame over the picture
                aligned = align::agreement(&rr, &mm, &found, lo, hi);
                let worse = match (aligned, as_shot) {
                    (Some(a), Some(b)) => a < b - ALIGN_WORSE,
                    (None, Some(_)) => true,
                    _ => false,
                };
                if found.plausible(w, h) && !worse {
                    warps[i] = found;
                } else {
                    aligned = None;
                    rejected.push(i);
                    warnings.push(Warning::AlignmentRejected { frame: i });
                }
            }
            // portrait and landscape mixed: the sensor sizes agree, the pictures cannot be laid on each other
            if turned {
                let best = aligned.into_iter().chain(as_shot).fold(f64::MIN, f64::max);
                if best > f64::MIN && best < TURNED_AGREEMENT {
                    return Err(Error::Turned { path: f.path.clone() });
                }
            }
            let shift = warps[i].corner_shift(w, h);
            if shift > LARGE_SHIFT * w.max(h) as f64 {
                warnings.push(Warning::LargeShift { frame: i, px: shift });
            }
        }
    }
    ctl.report(0.85);
    let t_exposure = std::time::Instant::now();
    let (k, steps) = exposure::factors(&fs, &warps, &order, reference);
    for (j, s) in steps.iter().enumerate() {
        let (dark, bright) = (order[j], order[j + 1]);
        match (s.measured, s.exif) {
            (Some(r), Some(q)) if s.fell_back => warnings.push(Warning::ExifDeviation { dark, bright, ev: (r / q).log2() }),
            (None, Some(_)) => warnings.push(Warning::NotMeasured { dark, bright }),
            (None, None) => warnings.push(Warning::ExposureUnknown { dark, bright }),
            _ => {}
        }
        let gap = -s.used.log2();
        if gap > 3.0 {
            warnings.push(Warning::LargeGap { dark, bright, ev: gap });
        }
    }
    let apertures: Vec<f32> = fs.iter().filter_map(|f| f.info.exif.aperture).filter(|a| *a > 0.0).collect();
    if apertures.iter().any(|a| (a / apertures[0]).log2().abs() > 0.2) {
        warnings.push(Warning::ApertureDiffers);
    }
    // noise model per frame on the raw mosaic: physically one line for all colours; in white-balanced green units
    // a_g = wb_g · a, b_g = wb_g² · b
    let t_noise = std::time::Instant::now();
    let noise: Vec<(f32, f32)> = fs
        .iter()
        .map(|f| {
            let col = |x: usize, y: usize| f.color_at(x, y);
            let m = raw::noise::estimate(&f.mosaic, f.w, f.h, &col, [0.98; 3], [1.0; 3]);
            m.map(|m| (m.a[1] * wb[1], m.b[1] * wb[1] * wb[1])).unwrap_or((1e-4, 1e-7))
        })
        .collect();
    if std::env::var_os("GRAINEO_HDR_PROFILE").is_some() {
        eprintln!("hdr-profile align {:.3}\nhdr-profile exposure {:.3}\nhdr-profile noise {:.3}", (t_exposure - t_align).as_secs_f64(), (t_noise - t_exposure).as_secs_f64(), t_noise.elapsed().as_secs_f64());
    }
    ctl.report(1.0);
    // Where even the darkest frame is blown (the sun, a lamp) nothing else is known: its values are what the merge
    // shows there. They get the highlight handling of a single RAW (blown colours rebuilt from the others, fully blown
    // places neutral) – otherwise such places keep "clip level × white balance", red and blue above green.
    if let Some(dark) = regions::darkest(&k) {
        profile("highlights", || regions::reconstruct_highlights(&mut fs[dark], wb));
    }
    let fr = &fs[reference];
    let i = &fr.info;
    let c = i.crop.or(i.active).unwrap_or(raw::Region { x: 0, y: 0, w, h });
    let crop = raw::Region { x: c.x.min(w), y: c.y.min(h), w: c.w.min(w - c.x.min(w)), h: c.h.min(h - c.y.min(h)) };
    Ok(Plan { frames: fs, order, reference, warps, rejected, k, steps, noise, wb, crop, warnings, settings: settings.clone() })
}

impl Plan {
    pub fn summary(&self) -> Summary {
        let (w, h) = (self.frames[self.reference].w, self.frames[self.reference].h);
        Summary {
            reference: self.reference,
            ev: self.k.iter().map(|k| k.log2()).collect(),
            shift: self.warps.iter().map(|wp| wp.corner_shift(w, h)).collect(),
            warnings: self.warnings.clone(),
        }
    }

    pub fn reference_info(&self) -> &raw::Info {
        &self.frames[self.reference].info
    }

    pub fn reference_path(&self) -> &Path {
        &self.frames[self.reference].path
    }

    /// Analytic noise of the merge (green): `Var(X) = 1 / Σ k_i² / (a_i·k_i·X + b_i)` over the frames unsaturated at X,
    /// fitted by a line `a'·X + b'` (relative least squares over X ∈ [noise floor, 0.5]); the other channels scale
    /// with the white balance as in a single frame.
    pub fn result_noise(&self) -> raw::noise::NoiseModel {
        let wb = self.wb;
        let var = |x: f64| -> f64 {
            let mut s = 0.0;
            for (i, (a, b)) in self.noise.iter().enumerate() {
                let k = self.k[i];
                if k * x > 0.75 * (self.frames[i].clip[1] * wb[1]) as f64 {
                    continue;
                }
                s += k * k / (*a as f64 * k * x + *b as f64);
            }
            if s > 0.0 {
                1.0 / s
            } else {
                f64::NAN
            }
        };
        let (a_ref, b_ref) = self.noise[self.reference];
        let lo = (b_ref as f64).sqrt().max(1e-5);
        // weighted least squares on relative error: minimise Σ ((a·x + b − v)/v)²
        let (mut sxx, mut sx1, mut s11, mut sxv, mut s1v) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let steps = 48;
        for j in 0..=steps {
            let x = lo * (0.5 / lo).powf(j as f64 / steps as f64);
            let v = var(x);
            if !v.is_finite() || v <= 0.0 {
                continue;
            }
            let (p, q) = (x / v, 1.0 / v);
            sxx += p * p;
            sx1 += p * q;
            s11 += q * q;
            sxv += p;
            s1v += q;
        }
        let det = sxx * s11 - sx1 * sx1;
        let (mut a, mut b) = if det.abs() > 0.0 { ((sxv * s11 - s1v * sx1) / det, (s1v * sxx - sxv * sx1) / det) } else { (a_ref as f64, b_ref as f64) };
        if !(a.is_finite() && b.is_finite()) || a <= 0.0 {
            a = a_ref as f64;
        }
        if !b.is_finite() || b <= 0.0 {
            b = (var(lo) - a * lo).max(1e-12);
        }
        let g = wb[1].max(1e-6);
        raw::noise::NoiseModel { a: std::array::from_fn(|c| a as f32 * wb[c] / g), b: std::array::from_fn(|c| b as f32 * (wb[c] / g).powi(2)) }
    }
}

/// Noise profile (wavelet thresholds) of a merged full-resolution result: simulated with the demosaic of the
/// reference sensor, as for a single frame.
pub fn profile_full(model: raw::noise::NoiseModel, info: &raw::Info, pattern: [[u8; 6]; 6]) -> raw::noise::Profile {
    let col = move |x: usize, y: usize| pattern[y % 6][x % 6] as usize;
    let sensor = info.sensor;
    let dm = if sensor == raw::Sensor::XTrans { raw::development::Demosaic::Markesteijn3 } else { raw::development::Demosaic::Rcd };
    let d = move |m: &[f32], w: usize, h: usize| raw::development::demosaic_mosaic(m, w, h, pattern, dm, sensor);
    raw::noise::simulate_profile(model, &col, &d)
}

/// Noise profile of a result downscaled by `f` (preview).
pub fn profile_scaled(model: raw::noise::NoiseModel, info: &raw::Info, pattern: [[u8; 6]; 6], f: f32) -> raw::noise::Profile {
    let col = move |x: usize, y: usize| pattern[y % 6][x % 6] as usize;
    let sensor = info.sensor;
    let dm = if sensor == raw::Sensor::XTrans { raw::development::Demosaic::Markesteijn3 } else { raw::development::Demosaic::Rcd };
    let f = f.clamp(0.05, 1.0);
    let nq = (264.0 / f).ceil() as usize;
    let p = move |m: &[f32], w: usize, h: usize| {
        let rgb = raw::development::demosaic_mosaic(m, w, h, pattern, dm, sensor);
        let (nw, nh) = (((w as f32 * f).round() as usize).max(1), ((h as f32 * f).round() as usize).max(1));
        (raw::development::downscale(&rgb, w, h, nw, nh), nw, nh)
    };
    raw::noise::simulate_profile_with(model, &col, nq, &p)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod audit4_tests;
