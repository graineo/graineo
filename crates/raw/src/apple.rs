//! The system RAW engine of macOS (Core Image, `CIRAWFilter`) for side-by-side comparisons with our own pipeline.
//!
//! A RAW file is decoded with fixed parameters into a linear float buffer (RGBA, linear Rec. 2020). Nothing here
//! changes a development; the app shows the result next to its own rendering (sensor tray, "Pipeline: Apple |
//! Compare"). Only the operating system's decoder is used, nothing is downloaded or sent.
//!
//! - Parameters ([`Params`]): decoder version (default: the newest the file offers, see [`choose_version`]), steps
//!   ("neutral" = noise reduction, sharpening, detail, moiré, local contrast and lens correction off; "standard" = the
//!   engine's own defaults), white balance (as shot or Kelvin/tint), exposure in EV, a scaled preview or a crop at
//!   full resolution.
//! - Orientation is always "up" (sensor orientation, like our own full-resolution image); the caller rotates.
//! - Errors are named ([`Error`]): not on this system, file missing, format/camera not supported, engine failed.
//!
//! macOS only; elsewhere every call answers [`Error::NotAvailable`]. Objective-C calls go through `objc2` directly
//! (no Core Image bindings needed). Not thread-safe per [`Engine`]: keep one engine per worker thread.

use std::path::{Path, PathBuf};

/// Which optional processing steps of the engine run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Steps {
    /// Noise reduction, sharpening, detail, moiré reduction, local contrast/tone map, lens correction, gamut mapping
    /// and extended dynamic range off; the engine's base tone curve stays.
    #[default]
    Neutral,
    /// The engine's defaults for this camera (as macOS Preview/Photos render), except extended dynamic range (off:
    /// the comparison is shown in SDR).
    Standard,
    /// [`Steps::Neutral`] without the engine's tone curve (boost 0): scene-linear data for graineo's own chain (the
    /// engine as decoding stage of a development, see `rawdev::apple`).
    Linear,
}

/// Parameters of one rendering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Params {
    /// Decoder version as offered by the file (e.g. "8", "8.dng"); `None` = the newest ([`newest`]).
    pub decoder: Option<String>,
    pub steps: Steps,
    /// Exposure in EV on top of the engine's own baseline exposure.
    pub exposure_ev: f32,
    /// White balance as (Kelvin, tint); `None` = as shot.
    pub white: Option<(f32, f32)>,
    /// Scaled rendering with this longest edge (the decoder scales itself, fast); `None` = full resolution.
    pub max_edge: Option<usize>,
    /// Only this crop (x, y, w, h) in pixels of the full-resolution image, origin top left; `None` = all.
    pub region: Option<[usize; 4]>,
}

impl Params {
    /// The part of the parameters that defines the decoded image (without size and crop): renderings with equal keys
    /// can share one filter object.
    pub fn filter_key(&self) -> (Option<String>, Steps, u32, Option<(u32, u32)>) {
        (self.decoder.clone(), self.steps, self.exposure_ev.to_bits(), self.white.map(|(k, t)| (k.to_bits(), t.to_bits())))
    }
}

/// What the engine says about a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// decoder versions offered for this file, in the engine's order
    pub versions: Vec<String>,
    /// the version the engine would use by itself
    pub engine_default: String,
    /// size at full resolution (orientation "up")
    pub native: [usize; 2],
    /// baseline exposure the engine applies (EV)
    pub baseline_exposure: f32,
    /// white balance as shot in the engine's terms (Kelvin, tint); (0, 0) if the engine does not say
    pub as_shot: (f32, f32),
}

/// A rendered image: RGBA, linear Rec. 2020, rows top to bottom.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<f32>,
    /// decoder version actually used
    pub decoder: String,
    /// size of the whole image at this scale (for a crop: of the full resolution)
    pub extent: [usize; 2],
    /// time for decoding and rendering in milliseconds
    pub millis: f64,
}

/// Why the engine gave no image.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// not macOS (or Core Image without `CIRAWFilter`)
    NotAvailable,
    /// the file does not exist (or cannot be reached)
    Missing(PathBuf),
    /// the engine does not know this format or camera (reason)
    Unsupported(String),
    /// the engine failed (reason)
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAvailable => write!(f, "system RAW engine not available on this system"),
            Error::Missing(p) => write!(f, "file not found: {}", p.display()),
            Error::Unsupported(r) => write!(f, "not supported: {r}"),
            Error::Failed(r) => write!(f, "system RAW engine failed: {r}"),
        }
    }
}

impl std::error::Error for Error {}

/// Number of a decoder version ("8.dng" → 8.0); `None` for anything else ("none").
pub fn version_number(v: &str) -> Option<f32> {
    v.split('.').next()?.trim().parse::<f32>().ok()
}

/// Does the version use the DNG's own metadata (colour, noise profile) instead of the engine's calibration?
pub fn version_is_dng(v: &str) -> bool {
    v.to_ascii_lowercase().ends_with(".dng")
}

/// Short name of a version for the UI: "8", "8 DNG".
pub fn version_label(v: &str) -> String {
    match v.strip_suffix(".dng").or_else(|| v.strip_suffix(".DNG")) {
        Some(n) => format!("{n} DNG"),
        None => v.to_string(),
    }
}

/// The newest version: highest number; with the same number the one the engine uses by itself, otherwise the one with
/// the engine's own calibration (not ".dng").
pub fn newest(versions: &[String], engine_default: &str) -> Option<String> {
    versions
        .iter()
        .filter(|v| version_number(v).is_some())
        .max_by(|a, b| {
            let key = |v: &String| (version_number(v).unwrap_or(0.0), v.as_str() == engine_default, !version_is_dng(v));
            let (ka, kb) = (key(a), key(b));
            ka.0.total_cmp(&kb.0).then(ka.1.cmp(&kb.1)).then(ka.2.cmp(&kb.2))
        })
        .cloned()
}

/// Choice of the decoder version for a rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionChoice {
    /// the version to use
    pub used: String,
    /// the pinned version the file does not offer (after a system update): the newest is used instead
    pub pinned_missing: Option<String>,
}

/// Version to use: the pinned one if the file offers it, otherwise the newest (and the missing pin is reported).
/// `None` if the file offers no version at all (not supported).
pub fn choose_version(versions: &[String], engine_default: &str, pinned: Option<&str>) -> Option<VersionChoice> {
    let newest = newest(versions, engine_default)?;
    match pinned {
        Some(p) if versions.iter().any(|v| v == p) => Some(VersionChoice { used: p.to_string(), pinned_missing: None }),
        Some(p) => Some(VersionChoice { used: newest, pinned_missing: Some(p.to_string()) }),
        None => Some(VersionChoice { used: newest, pinned_missing: None }),
    }
}

/// Numeric properties set on the filter for these parameters (Objective-C setter, value). Booleans are 0/1 and go to
/// the setters in [`BOOL_SETTERS`]. Size and crop are not part of it (see [`Params::max_edge`]).
pub fn filter_values(p: &Params) -> Vec<(&'static str, f32)> {
    let mut v = vec![("setExposure:", p.exposure_ev), ("setExtendedDynamicRangeAmount:", 0.0)];
    if let Some((k, t)) = p.white {
        v.push(("setNeutralTemperature:", k));
        v.push(("setNeutralTint:", t));
    }
    if p.steps != Steps::Standard {
        for s in [
            "setLuminanceNoiseReductionAmount:",
            "setColorNoiseReductionAmount:",
            "setSharpnessAmount:",
            "setContrastAmount:",
            "setDetailAmount:",
            "setMoireReductionAmount:",
            "setLocalToneMapAmount:",
        ] {
            v.push((s, 0.0));
        }
        v.push(("setLensCorrectionEnabled:", 0.0));
        v.push(("setGamutMappingEnabled:", 0.0));
    }
    if p.steps == Steps::Linear {
        v.push(("setBoostAmount:", 0.0));
        v.push(("setBoostShadowAmount:", 0.0));
        // graineo applies its own base exposure
        v.push(("setBaselineExposure:", 0.0));
    }
    v
}

/// Version for the automatic fallback (a file graineo cannot decode, e.g. Nikon HE): the newest version below 9 when
/// the file offers one (`prefer_newest == false`), otherwise the newest. Version 9 is 3–7× slower than 8, denoises even
/// with its noise reduction at 0 (graineo's noise reduction would come on top) and was not closer in colour in the
/// measurement (docs, section 12); 8 is the conservative default, 9 stays selectable per photo.
pub fn fallback_version(versions: &[String], engine_default: &str, prefer_newest: bool) -> Option<String> {
    if prefer_newest {
        return newest(versions, engine_default);
    }
    let below: Vec<String> = versions.iter().filter(|v| version_number(v).is_some_and(|n| n < 9.0)).cloned().collect();
    newest(&below, engine_default).or_else(|| newest(versions, engine_default))
}

/// Setters among [`filter_values`] that take a `BOOL`.
pub const BOOL_SETTERS: [&str; 2] = ["setLensCorrectionEnabled:", "setGamutMappingEnabled:"];

/// Scale factor for a longest edge of `max_edge` pixels (never enlarges).
pub fn scale_for(native: [usize; 2], max_edge: usize) -> f32 {
    let long = native[0].max(native[1]).max(1) as f32;
    (max_edge as f32 / long).min(1.0)
}

/// Linear Rec. 2020 → linear sRGB (D65).
const REC2020_TO_SRGB: [[f32; 3]; 3] = [[1.660_491, -0.587_641, -0.072_850], [-0.124_551, 1.132_9, -0.008_349], [-0.018_151, -0.100_579, 1.118_73]];

/// RGBA linear Rec. 2020 (float) → RGBA 8 bit sRGB for display (clipped to the sRGB gamut, like our own preview).
pub fn to_srgb8(rgba: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        for row in REC2020_TO_SRGB {
            let l = row[0] * p[0] + row[1] * p[1] + row[2] * p[2];
            let l = if l.is_finite() { l.clamp(0.0, 1.0) } else { 0.0 };
            let e = if l <= 0.003_130_8 { 12.92 * l } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
            out.push((e * 255.0 + 0.5) as u8);
        }
        out.push(255);
    }
    out
}

/// Engines shared by the developments of the app: at most [`SHARED_ENGINES`] renderings at the same time (the engine is
/// GPU/Neural-Engine bound; more in parallel only costs memory). A caller waits for a free engine; one that last
/// decoded the same file is preferred (the engine keeps the decoded data while only size, crop or nothing changes).
pub const SHARED_ENGINES: usize = 2;

struct Pool {
    free: std::sync::Mutex<(Vec<Engine>, usize)>,
    ready: std::sync::Condvar,
}

fn pool() -> &'static Pool {
    static P: std::sync::OnceLock<Pool> = std::sync::OnceLock::new();
    P.get_or_init(|| Pool { free: std::sync::Mutex::new((Vec::new(), 0)), ready: std::sync::Condvar::new() })
}

/// An engine taken from the pool: goes back when the work is done. If the work panicked, the engine is dropped (its
/// state is unknown) and its place is freed, so the pool never runs out of engines and nobody waits for ever.
struct Lease {
    engine: Option<Engine>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let p = pool();
        let mut g = p.free.lock().unwrap_or_else(|e| e.into_inner());
        match self.engine.take() {
            Some(e) if !std::thread::panicking() => g.0.push(e),
            _ => g.1 = g.1.saturating_sub(1),
        }
        drop(g);
        p.ready.notify_one();
    }
}

/// Run `f` with a shared engine (see [`SHARED_ENGINES`]); `path`: the file it will work on (for reuse).
pub fn with_shared<R>(path: &Path, f: impl FnOnce(&mut Engine) -> R) -> Result<R, Error> {
    let p = pool();
    let engine = {
        let mut g = p.free.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(i) = g.0.iter().position(|e| e.holds(path)).or(if g.0.is_empty() { None } else { Some(g.0.len() - 1) }) {
                break g.0.remove(i);
            }
            if g.1 < SHARED_ENGINES {
                g.1 += 1;
                drop(g);
                match Engine::new() {
                    Ok(e) => break e,
                    Err(e) => {
                        p.free.lock().unwrap_or_else(|e| e.into_inner()).1 -= 1;
                        p.ready.notify_one();
                        return Err(e);
                    }
                }
            }
            g = p.ready.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    };
    let mut lease = Lease { engine: Some(engine) };
    Ok(f(lease.engine.as_mut().expect("engine leased")))
}

/// Largest rendering in pixels (RGBA float: 16 bytes each, 3.2 GB at the limit). Above it a file's size is not
/// believed (a damaged header) and nothing is allocated: the engine answers with an error instead of the app aborting.
pub const MAX_RENDER_PIXELS: usize = 200_000_000;

/// Number of floats of an RGBA rendering of `w` × `h`, or why it is not rendered (empty, too large, overflow).
pub fn render_buffer_len(w: usize, h: usize) -> Result<usize, Error> {
    if w == 0 || h == 0 {
        return Err(Error::Failed("empty output image".into()));
    }
    match w.checked_mul(h) {
        Some(n) if n <= MAX_RENDER_PIXELS => Ok(n * 4),
        _ => Err(Error::Failed(format!("output image of {w}×{h} pixels is too large"))),
    }
}

/// Version of the operating system ("27.2.1 (Build 26B131)"); empty off macOS. Answers of the engine (which files,
/// which decoder versions) hold only for this version.
pub fn system_version() -> String {
    #[cfg(target_os = "macos")]
    {
        mac::system_version()
    }
    #[cfg(not(target_os = "macos"))]
    {
        String::new()
    }
}

/// Key of the question "does the engine support this file, with which versions?": maker, model, image size and format
/// (the answer differs between full RAW and RAW-S/sRAW of the same camera, see docs section 12.10).
pub fn support_key(maker: &str, model: &str, size: [usize; 2], format: &str) -> String {
    format!("{}|{}|{}x{}|{}", maker.trim().to_ascii_lowercase(), model.trim().to_ascii_lowercase(), size[0].max(size[1]), size[0].min(size[1]), format.to_ascii_lowercase())
}

/// Remembered answers per [`support_key`] for the running system version: offered versions (empty = not supported).
/// Filled by every probe in the background; never asked in the UI thread, never per photo on import. A failure is
/// not kept as "not supported" for a whole key when the file was missing or the engine failed.
pub struct SupportCache {
    system: String,
    map: std::collections::HashMap<String, Vec<String>>,
}

impl SupportCache {
    pub fn new(system: String) -> Self {
        SupportCache { system, map: Default::default() }
    }
    /// From stored entries; empty when they belong to another system version (macOS update: everything is asked again).
    pub fn restore(system: String, stored_system: &str, entries: impl IntoIterator<Item = (String, Vec<String>)>) -> Self {
        let mut c = SupportCache::new(system);
        if c.system == stored_system && !c.system.is_empty() {
            c.map.extend(entries);
        }
        c
    }
    pub fn system(&self) -> &str {
        &self.system
    }
    pub fn get(&self, key: &str) -> Option<&Vec<String>> {
        self.map.get(key)
    }
    /// Remember the result of a probe; returns whether something changed.
    pub fn note(&mut self, key: String, result: &Result<Probe, Error>) -> bool {
        let versions = match result {
            Ok(p) => p.versions.clone(),
            Err(Error::Unsupported(_)) => Vec::new(),
            Err(_) => return false,
        };
        self.map.insert(key, versions.clone()) != Some(versions)
    }
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Vec<String>)> {
        self.map.iter()
    }
}

/// Checks that need no engine: file present, not a Sigma X3F (never supported).
fn precheck(path: &Path) -> Result<(), Error> {
    if !path.is_file() {
        return Err(Error::Missing(path.to_path_buf()));
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "x3f" {
        return Err(Error::Unsupported("Sigma X3F".into()));
    }
    Ok(())
}

/// Probe a file once (own engine; in a worker keep an [`Engine`]).
pub fn probe(path: &Path) -> Result<Probe, Error> {
    Engine::new()?.probe(path)
}

/// Render a file once (own engine; in a worker keep an [`Engine`]).
pub fn render(path: &Path, p: &Params) -> Result<Rendered, Error> {
    Engine::new()?.render(path, p)
}

#[cfg(target_os = "macos")]
pub use mac::Engine;

#[cfg(not(target_os = "macos"))]
/// Stand-in without function (the engine exists only on macOS).
pub struct Engine;

#[cfg(not(target_os = "macos"))]
impl Engine {
    pub fn new() -> Result<Engine, Error> {
        Err(Error::NotAvailable)
    }
    pub fn probe(&mut self, path: &Path) -> Result<Probe, Error> {
        precheck(path)?;
        Err(Error::NotAvailable)
    }
    pub fn render(&mut self, path: &Path, _p: &Params) -> Result<Rendered, Error> {
        precheck(path)?;
        Err(Error::NotAvailable)
    }
    pub fn forget(&mut self) {}
    pub fn holds(&self, _path: &Path) -> bool {
        false
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_void, CString};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use objc2::encode::{Encode, Encoding};
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool, MessageReceiver, Sel};
    use objc2::msg_send;
    use objc2_foundation::{NSArray, NSString, NSURL};

    use super::{choose_version, filter_values, precheck, scale_for, Error, Params, Probe, Rendered, BOOL_SETTERS};

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    // SAFETY: layouts of CGPoint, CGSize, CGRect (two/four doubles on 64-bit macOS)
    unsafe impl Encode for Point {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for Size {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
    }

    /// Core Graphics colour space as Objective-C argument (`^{CGColorSpace=}`).
    #[repr(transparent)]
    #[derive(Clone, Copy)]
    struct ColorSpaceRef(*mut c_void);
    // SAFETY: a plain pointer to a CGColorSpace
    unsafe impl Encode for ColorSpaceRef {
        const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
    }

    #[link(name = "CoreImage", kind = "framework")]
    extern "C" {
        static kCIFormatRGBAf: i32;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        static kCGColorSpaceLinearITUR_2020: *const c_void;
    }

    /// Core Image context, output colour space and the last filter object (reused while only size or crop change, so
    /// the engine keeps its decoded data).
    pub struct Engine {
        ctx: Retained<AnyObject>,
        space: ColorSpaceRef,
        filter: Option<(PathBuf, (Option<String>, super::Steps, u32, Option<(u32, u32)>), Retained<AnyObject>, String)>,
    }

    // SAFETY: an engine is used by one thread at a time (moved in and out of the shared pool under a mutex, or owned by
    // one worker thread). Core Image contexts and colour spaces are thread-safe; the filter object is not shared
    // between threads concurrently, and every call runs inside its own autorelease pool.
    unsafe impl Send for Engine {}

    /// Operating system version string of NSProcessInfo.
    pub fn system_version() -> String {
        let Some(class) = AnyClass::get(c"NSProcessInfo") else { return String::new() };
        autoreleasepool(|_| {
            // SAFETY: documented class method and property of NSProcessInfo
            let info: Option<Retained<AnyObject>> = unsafe { msg_send![class, processInfo] };
            let Some(info) = info else { return String::new() };
            let s: Option<Retained<NSString>> = unsafe { msg_send![&*info, operatingSystemVersionString] };
            s.map(|s| s.to_string()).unwrap_or_default()
        })
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            // SAFETY: created by CGColorSpaceCreateWithName in `new`, released once
            unsafe { CGColorSpaceRelease(self.space.0) };
        }
    }

    fn raw_filter_class() -> Option<&'static AnyClass> {
        AnyClass::get(c"CIRAWFilter")
    }

    fn strings(a: &NSArray<NSString>) -> Vec<String> {
        a.to_vec().iter().map(|s| s.to_string()).collect()
    }

    impl Engine {
        pub fn new() -> Result<Engine, Error> {
            if raw_filter_class().is_none() {
                return Err(Error::NotAvailable);
            }
            let class = AnyClass::get(c"CIContext").ok_or(Error::NotAvailable)?;
            // SAFETY: class method of CIContext
            let ctx: Option<Retained<AnyObject>> = autoreleasepool(|_| unsafe { msg_send![class, context] });
            let ctx = ctx.ok_or_else(|| Error::Failed("no Core Image context".into()))?;
            // SAFETY: system constant; the returned colour space is released in Drop
            let space = ColorSpaceRef(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceLinearITUR_2020) });
            if space.0.is_null() {
                return Err(Error::Failed("no linear Rec. 2020 colour space".into()));
            }
            Ok(Engine { ctx, space, filter: None })
        }

        /// Release the filter (other photo; the engine's decoded data goes with it).
        pub fn forget(&mut self) {
            self.filter = None;
        }

        /// Does the engine hold a filter for this file (from the last rendering)?
        pub fn holds(&self, path: &Path) -> bool {
            self.filter.as_ref().is_some_and(|(p, ..)| p == path)
        }

        /// A fresh filter object for the file, or why there is none.
        fn open(path: &Path) -> Result<Retained<AnyObject>, Error> {
            precheck(path)?;
            let class = raw_filter_class().ok_or(Error::NotAvailable)?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            // SAFETY: class method of CIRAWFilter (macOS 12+), nullable result
            let f: Option<Retained<AnyObject>> = unsafe { msg_send![class, filterWithImageURL: &*url] };
            let f = f.ok_or_else(|| Error::Unsupported(format!("format of {}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())))?;
            // for some formats (e.g. Sigma X3F) a filter comes back without size and without decoder versions
            let size: Size = unsafe { msg_send![&*f, nativeSize] };
            let versions: Option<Retained<NSArray<NSString>>> = unsafe { msg_send![&*f, supportedDecoderVersions] };
            if size.width < 1.0 || versions.is_none_or(|v| v.is_empty()) {
                return Err(Error::Unsupported("camera or format unknown to the system decoder".into()));
            }
            Ok(f)
        }

        fn probe_filter(f: &AnyObject) -> Probe {
            // SAFETY: documented properties of CIRAWFilter
            unsafe {
                let size: Size = msg_send![f, nativeSize];
                let versions: Option<Retained<NSArray<NSString>>> = msg_send![f, supportedDecoderVersions];
                let current: Option<Retained<NSString>> = msg_send![f, decoderVersion];
                let baseline: f32 = msg_send![f, baselineExposure];
                let kelvin: f32 = msg_send![f, neutralTemperature];
                let tint: f32 = msg_send![f, neutralTint];
                Probe {
                    versions: versions.map(|v| strings(&v)).unwrap_or_default(),
                    engine_default: current.map(|s| s.to_string()).unwrap_or_default(),
                    native: [size.width.round() as usize, size.height.round() as usize],
                    baseline_exposure: baseline,
                    as_shot: (if kelvin.is_finite() { kelvin } else { 0.0 }, if tint.is_finite() { tint } else { 0.0 }),
                }
            }
        }

        pub fn probe(&mut self, path: &Path) -> Result<Probe, Error> {
            autoreleasepool(|_| Self::open(path).map(|f| Self::probe_filter(&f)))
        }

        /// Set a property if the filter knows its setter (older systems lack some).
        fn set(f: &AnyObject, setter: &str, value: f32) {
            let Ok(name) = CString::new(setter) else { return };
            let sel = Sel::register(&name);
            // SAFETY: respondsToSelector: guards the call; the setters take a float or a BOOL
            unsafe {
                let ok: bool = msg_send![f, respondsToSelector: sel];
                if !ok {
                    return;
                }
                if BOOL_SETTERS.contains(&setter) {
                    let _: () = f.send_message(sel, (Bool::new(value != 0.0),));
                } else {
                    let _: () = f.send_message(sel, (value,));
                }
            }
        }

        pub fn render(&mut self, path: &Path, p: &Params) -> Result<Rendered, Error> {
            autoreleasepool(|_| self.render_inner(path, p))
        }

        fn render_inner(&mut self, path: &Path, p: &Params) -> Result<Rendered, Error> {
            let t0 = Instant::now();
            let key = p.filter_key();
            let reuse = self.filter.as_ref().is_some_and(|(fp, k, _, _)| fp == path && *k == key);
            if !reuse {
                self.filter = None;
                let f = Self::open(path)?;
                let probe = Self::probe_filter(&f);
                let choice = choose_version(&probe.versions, &probe.engine_default, p.decoder.as_deref()).ok_or_else(|| Error::Unsupported("no decoder version".into()))?;
                // a pin the file does not offer falls back to the newest; the caller sees it in `Rendered::decoder`
                // SAFETY: documented setters of CIRAWFilter
                unsafe {
                    let _: () = msg_send![&*f, setDecoderVersion: &*NSString::from_str(&choice.used)];
                    // sensor orientation (kCGImagePropertyOrientationUp), like our own full-resolution image
                    let _: () = msg_send![&*f, setOrientation: 1u32];
                }
                for (setter, value) in filter_values(p) {
                    Self::set(&f, setter, value);
                }
                self.filter = Some((path.to_path_buf(), key, f, choice.used));
            }
            let (_, _, f, used) = self.filter.as_ref().expect("filter set above");
            let native: Size = unsafe { msg_send![&**f, nativeSize] };
            let scale = match (p.max_edge, p.region) {
                (Some(m), None) => scale_for([native.width as usize, native.height as usize], m),
                _ => 1.0,
            };
            Self::set(f, "setScaleFactor:", scale);
            // SAFETY: outputImage is nullable; extent is a CGRect
            let img: Option<Retained<AnyObject>> = unsafe { msg_send![&**f, outputImage] };
            let img = img.ok_or_else(|| Error::Failed("no output image".into()))?;
            let ext: Rect = unsafe { msg_send![&*img, extent] };
            // a non-finite extent (an infinite image) becomes 0 and is refused below
            let side = |v: f64| if v.is_finite() { v.round().max(0.0) as usize } else { 0 };
            let (ew, eh) = (side(ext.size.width), side(ext.size.height));
            if ew == 0 || eh == 0 {
                return Err(Error::Failed("empty output image".into()));
            }
            // crop: our origin is top left, Core Image's bottom left
            let (bounds, w, h) = match p.region {
                Some([x, y, w, h]) => {
                    let x = x.min(ew.saturating_sub(1));
                    let y = y.min(eh.saturating_sub(1));
                    let (w, h) = (w.min(ew - x).max(1), h.min(eh - y).max(1));
                    let r = Rect { origin: Point { x: ext.origin.x + x as f64, y: ext.origin.y + (eh - y - h) as f64 }, size: Size { width: w as f64, height: h as f64 } };
                    (r, w, h)
                }
                None => (Rect { origin: ext.origin, size: Size { width: ew as f64, height: eh as f64 } }, ew, eh),
            };
            let mut buf = vec![0f32; super::render_buffer_len(w, h)?];
            // SAFETY: the buffer holds h rows of w RGBAf pixels; colour space valid for the engine's lifetime
            unsafe {
                let _: () = msg_send![&*self.ctx, render: &*img, toBitmap: buf.as_mut_ptr() as *mut c_void, rowBytes: (w * 16) as isize, bounds: bounds, format: kCIFormatRGBAf, colorSpace: self.space];
            }
            Ok(Rendered { width: w, height: h, rgba: buf, decoder: used.clone(), extent: [ew, eh], millis: t0.elapsed().as_secs_f64() * 1000.0 })
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn newest_prefers_highest_number_then_engine_default_then_own_calibration() {
        // Ricoh GR III on macOS 26: 6.dng, 7.dng, 8.dng, 8 (engine default 8)
        assert_eq!(newest(&v(&["6.dng", "7.dng", "8.dng", "8"]), "8").as_deref(), Some("8"));
        // Leica Q: engine default 7, but 8 exists only as DNG
        assert_eq!(newest(&v(&["6.dng", "6", "7.dng", "8.dng", "7"]), "7").as_deref(), Some("8.dng"));
        // same number, engine default is the DNG variant
        assert_eq!(newest(&v(&["8", "8.dng"]), "8.dng").as_deref(), Some("8.dng"));
        assert_eq!(newest(&v(&["7", "6"]), "7").as_deref(), Some("7"));
        assert_eq!(newest(&v(&["none"]), "none"), None);
        assert_eq!(newest(&[], ""), None);
    }

    #[test]
    fn pinned_version_is_used_and_a_missing_pin_falls_back_to_the_newest() {
        let offered = v(&["7", "8"]);
        assert_eq!(choose_version(&offered, "8", None), Some(VersionChoice { used: "8".into(), pinned_missing: None }));
        assert_eq!(choose_version(&offered, "8", Some("7")), Some(VersionChoice { used: "7".into(), pinned_missing: None }));
        // after a system update the pinned version may be gone: newest, and say so
        assert_eq!(choose_version(&offered, "8", Some("6")), Some(VersionChoice { used: "8".into(), pinned_missing: Some("6".into()) }));
        assert_eq!(choose_version(&[], "", Some("8")), None);
    }

    /// Sizes from a damaged header or an infinite image never allocate: an error instead of an abort.
    #[test]
    fn render_buffers_are_bounded() {
        assert_eq!(render_buffer_len(8256, 5504).unwrap(), 8256 * 5504 * 4);
        assert!(render_buffer_len(0, 100).is_err() && render_buffer_len(100, 0).is_err());
        assert!(render_buffer_len(usize::MAX, 3).is_err(), "overflow");
        assert!(render_buffer_len(20_000, 20_000).is_err(), "400 MP");
    }

    /// A panic in the work of a shared engine frees its place: later callers still get an engine (or the error of
    /// the system) instead of waiting for ever.
    #[test]
    fn a_panic_does_not_use_up_the_shared_engines() {
        let path = Path::new("/nonexistent/graineo-test/a.NEF");
        for _ in 0..SHARED_ENGINES + 1 {
            let r = std::panic::catch_unwind(|| with_shared(path, |_| -> () { panic!("test panic") }));
            assert!(r.is_err() || cfg!(not(target_os = "macos")), "the panic comes through (or there is no engine)");
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(with_shared(Path::new("/nonexistent/graineo-test/a.NEF"), |_| 7).ok());
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("an engine is free again (no deadlock)");
        assert_eq!(got, cfg!(target_os = "macos").then_some(7));
        let g = pool().free.lock().unwrap_or_else(|e| e.into_inner());
        assert!(g.1 <= SHARED_ENGINES && g.0.len() <= g.1, "{} engines, {} free", g.1, g.0.len());
    }

    #[test]
    fn fallback_prefers_the_newest_below_9_unless_told_otherwise() {
        // Nikon Z 8 on macOS 27: 7, 8, 9 → 8 (9 is slower and denoises on its own), or 9 when asked for the newest
        assert_eq!(fallback_version(&v(&["7", "8", "9"]), "9", false).as_deref(), Some("8"));
        assert_eq!(fallback_version(&v(&["7", "8", "9"]), "9", true).as_deref(), Some("9"));
        // Z 9: only 8
        assert_eq!(fallback_version(&v(&["8"]), "8", false).as_deref(), Some("8"));
        // only 9 offered: 9
        assert_eq!(fallback_version(&v(&["9"]), "9", false).as_deref(), Some("9"));
        // DNG lines: the newest below 9 with the engine's own calibration first
        assert_eq!(fallback_version(&v(&["6.dng", "7.dng", "8", "8.dng", "9", "9.dng"]), "9", false).as_deref(), Some("8"));
        assert_eq!(fallback_version(&[], "", false), None);
    }

    #[test]
    fn support_answers_are_kept_per_kind_of_file_and_dropped_after_a_system_update() {
        let key = support_key("NIKON CORPORATION", "NIKON Z 8", [8256, 5504], "NEF");
        assert_eq!(key, support_key("nikon corporation", "nikon z 8", [5504, 8256], "nef"), "orientation and case do not matter");
        assert_ne!(key, support_key("NIKON CORPORATION", "NIKON D850", [6192, 4128], "NEF"));
        let mut c = SupportCache::new("27.2.1".into());
        let probe = Probe { versions: v(&["7", "8", "9"]), engine_default: "9".into(), native: [8256, 5504], baseline_exposure: 0.0, as_shot: (5000.0, 0.0) };
        assert!(c.note(key.clone(), &Ok(probe.clone())));
        assert!(!c.note(key.clone(), &Ok(probe)), "same answer: nothing to write");
        assert_eq!(c.get(&key).map(|v| v.len()), Some(3));
        let x3f = support_key("SIGMA", "DP3 Merrill", [4704, 3136], "X3F");
        c.note(x3f.clone(), &Err(Error::Unsupported("camera".into())));
        assert_eq!(c.get(&x3f), Some(&Vec::new()), "not supported is remembered");
        // a missing file or a failing engine says nothing about the kind of file
        assert!(!c.note("other".into(), &Err(Error::Missing(PathBuf::from("/x")))));
        assert!(c.get("other").is_none());
        let stored: Vec<(String, Vec<String>)> = c.entries().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(SupportCache::restore("27.2.1".into(), "27.2.1", stored.clone()).get(&key).map(|v| v.len()), Some(3));
        assert!(SupportCache::restore("27.3".into(), "27.2.1", stored).get(&key).is_none(), "another system version asks again");
    }

    #[test]
    fn linear_steps_switch_the_tone_curve_and_baseline_off() {
        let p = Params { steps: Steps::Linear, ..Default::default() };
        let vals = filter_values(&p);
        for s in ["setBoostAmount:", "setBoostShadowAmount:", "setBaselineExposure:", "setLuminanceNoiseReductionAmount:", "setLensCorrectionEnabled:"] {
            assert!(vals.contains(&(s, 0.0)), "{s}");
        }
        assert!(!filter_values(&Params { steps: Steps::Neutral, ..Default::default() }).iter().any(|(s, _)| s.starts_with("setBoost")), "neutral keeps the tone curve (comparison view)");
    }

    #[test]
    fn labels_and_numbers() {
        assert_eq!(version_label("8"), "8");
        assert_eq!(version_label("8.dng"), "8 DNG");
        assert_eq!(version_number("8.dng"), Some(8.0));
        assert_eq!(version_number("none"), None);
        assert!(version_is_dng("7.dng") && !version_is_dng("7"));
    }

    #[test]
    fn neutral_switches_the_optional_steps_off_and_standard_keeps_them() {
        let n = Params { steps: Steps::Neutral, exposure_ev: 0.5, ..Default::default() };
        let vals = filter_values(&n);
        for s in ["setLuminanceNoiseReductionAmount:", "setColorNoiseReductionAmount:", "setSharpnessAmount:", "setDetailAmount:", "setMoireReductionAmount:", "setLocalToneMapAmount:", "setContrastAmount:"] {
            assert!(vals.contains(&(s, 0.0)), "{s}");
        }
        assert!(vals.contains(&("setLensCorrectionEnabled:", 0.0)));
        assert!(vals.contains(&("setExposure:", 0.5)));
        assert!(vals.contains(&("setExtendedDynamicRangeAmount:", 0.0)));
        assert!(!vals.iter().any(|(s, _)| s.starts_with("setNeutral")), "as shot sets no white balance");
        let s = Params { steps: Steps::Standard, white: Some((4800.0, 6.0)), ..Default::default() };
        let vals = filter_values(&s);
        assert!(!vals.iter().any(|(s, _)| s.contains("Noise") || s.contains("Sharpness") || s.contains("Lens")), "standard keeps the engine's defaults");
        assert!(vals.contains(&("setNeutralTemperature:", 4800.0)) && vals.contains(&("setNeutralTint:", 6.0)));
        for (s, _) in filter_values(&n) {
            assert!(s.starts_with("set") && s.ends_with(':'), "{s}");
        }
    }

    #[test]
    fn filter_key_ignores_size_and_crop() {
        let a = Params { max_edge: Some(2048), ..Default::default() };
        let b = Params { region: Some([0, 0, 10, 10]), ..Default::default() };
        assert_eq!(a.filter_key(), b.filter_key());
        let c = Params { exposure_ev: 1.0, ..Default::default() };
        assert_ne!(a.filter_key(), c.filter_key());
    }

    #[test]
    fn scale_never_enlarges() {
        assert!((scale_for([6000, 4000], 2048) - 2048.0 / 6000.0).abs() < 1e-6);
        assert_eq!(scale_for([1000, 800], 2048), 1.0);
    }

    #[test]
    fn display_conversion() {
        let px = to_srgb8(&[1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.18, 0.18, 0.18, 1.0, f32::NAN, 2.0, -1.0, 1.0]);
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 255]);
        // 18 % grey → sRGB 118
        assert!((px[8] as i32 - 118).abs() <= 1 && px[8] == px[9] && px[9] == px[10], "{:?}", &px[8..12]);
        assert_eq!(px[12], 0, "NaN → 0");
    }

    #[test]
    fn missing_file_and_sigma_are_named() {
        let missing = Path::new("/nonexistent/graineo-test/IMG_0001.CR3");
        match probe(missing) {
            Err(Error::Missing(p)) => assert_eq!(p, missing),
            Err(Error::NotAvailable) => assert!(!cfg!(target_os = "macos")),
            other => panic!("{other:?}"),
        }
        let dir = std::env::temp_dir().join(format!("graineo-apple-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let x3f = dir.join("SDIM0001.X3F");
        std::fs::write(&x3f, b"FOVb").unwrap();
        assert!(matches!(probe(&x3f), Err(Error::Unsupported(_) | Error::NotAvailable)));
        // not an image at all
        let txt = dir.join("notes.dng");
        std::fs::write(&txt, b"not a raw file").unwrap();
        assert!(matches!(render(&txt, &Params::default()), Err(Error::Unsupported(_) | Error::NotAvailable)), "{:?}", render(&txt, &Params::default()).err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The public test RAWs (Fuji RAF Bayer and X-Trans, Canon CR3, DNG): probe and render a small preview and a crop.
    /// Skipped without the folder (`GRAINEO_TESTDATA_RAW`, default `../graineo-testdata/raw` next to the project) or off
    /// macOS.
    #[test]
    fn public_test_raws() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let dir = std::env::var_os("GRAINEO_TESTDATA_RAW").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../graineo-testdata/raw"));
        if !dir.is_dir() {
            eprintln!("skipped: {} missing", dir.display());
            return;
        }
        let mut seen = 0;
        for name in ["DSCF6584.RAF", "DSCF0804.RAF", "IMG_0361.CR3", "R3013014.DNG"] {
            let p = dir.join(name);
            if !p.is_file() {
                continue;
            }
            let mut e = Engine::new().expect("engine");
            let pr = match e.probe(&p) {
                Ok(pr) => pr,
                Err(Error::Unsupported(r)) => {
                    eprintln!("{name}: not supported on this system ({r})");
                    continue;
                }
                Err(err) => panic!("{name}: {err}"),
            };
            assert!(!pr.versions.is_empty() && pr.native[0] > 1000, "{name}: {pr:?}");
            let prev = e.render(&p, &Params { max_edge: Some(512), ..Default::default() }).expect(name);
            assert_eq!(prev.width.max(prev.height), 512.min(pr.native[0].max(pr.native[1])), "{name}");
            assert_eq!(prev.rgba.len(), prev.width * prev.height * 4);
            let mean: f32 = prev.rgba.chunks_exact(4).map(|p| p[1]).sum::<f32>() / (prev.width * prev.height) as f32;
            assert!(mean > 0.005 && mean < 2.0, "{name}: mean green {mean}");
            // a crop at full resolution from the same filter
            let crop = e.render(&p, &Params { region: Some([100, 200, 64, 32]), ..Default::default() }).expect(name);
            assert_eq!((crop.width, crop.height), (64, 32), "{name}");
            assert_eq!(crop.extent, pr.native, "{name}");
            // reproducible
            let again = render(&p, &Params { region: Some([100, 200, 64, 32]), ..Default::default() }).expect(name);
            assert_eq!(crop.rgba, again.rgba, "{name}");
            // a pin the file does not offer: the newest is used
            let r = e.render(&p, &Params { decoder: Some("1".into()), max_edge: Some(256), ..Default::default() }).expect(name);
            assert_eq!(Some(r.decoder.clone()), newest(&pr.versions, &pr.engine_default), "{name}");
            eprintln!("{name}: versions {:?} (default {}), native {:?}, baseline {:+.2} EV, preview {:.0} ms, crop {:.0} ms", pr.versions, pr.engine_default, pr.native, pr.baseline_exposure, prev.millis, crop.millis);
            seen += 1;
        }
        eprintln!("{seen} test RAWs rendered");
    }
}
