//! The system RAW engine of macOS (Core Image, [`raw::apple`]) as the decoding stage of a development ("Apple path",
//! stage 1).
//!
//! The engine decodes and demosaics with every optional step off (noise reduction, sharpening, contrast, detail,
//! moiré, local tone, boost, lens correction, gamut mapping), with the photo's white balance, into linear Rec. 2020. The
//! result takes the place of our raw stage (demosaic + colour): everything after it is graineo's chain (noise
//! reduction, lens correction, exposure, tone, looks, film, masks, export), as for a finished image, but with the RAW
//! tone curve.
//!
//! - The engine renders again only when the white balance or the decoder version changes (the base exposure is ours):
//!   the preview once per white balance (≈ 2048 px via the engine's own scaling), the full resolution once per white
//!   balance (zoom, export). Zoom and pan never render again.
//! - At most [`raw::apple::SHARED_ENGINES`] renderings at a time (shared engines).
//! - Which engine develops a photo is the document field `RawAdjust::engine` ([`Choice`]): `None` = automatic
//!   (graineo; Apple only when graineo cannot decode the file, e.g. Nikon High Efficiency), `"graineo"`, `"apple:8"` …
//!   (pinned decoder version). A pinned version the system no longer offers is replaced by the newest one (and said
//!   so, [`AppleDev::pinned_missing`]); the entry itself is never changed here.
//!
//! macOS only; elsewhere [`open`] answers that the engine is not available and the photo opens with graineo.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use develop::Settings;
use raw::apple::{self as engine, Params, Probe, Steps};

use super::{RawDoc, Stage, PREVIEW_EDGE};

/// Value of `RawAdjust::engine` for graineo's own pipeline.
pub const GRAINEO: &str = "graineo";
/// Prefix of the Apple values (`"apple:8"`, `"apple:9.dng"`).
pub const APPLE_PREFIX: &str = "apple:";

/// The document field `RawAdjust::engine`, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// `None`: graineo; Apple only as fallback for files graineo cannot decode.
    Auto,
    /// `"graineo"`.
    Graineo,
    /// `"apple:<version>"`; `None` = without version (`"apple"`): the default version for the file.
    Apple(Option<String>),
    /// a value this version does not know (written by a newer graineo): developed with graineo, entry kept.
    Unknown(String),
}

impl Choice {
    pub fn parse(engine: Option<&str>) -> Choice {
        match engine.map(str::trim) {
            None | Some("") => Choice::Auto,
            Some(GRAINEO) => Choice::Graineo,
            Some("apple") => Choice::Apple(None),
            Some(s) => match s.strip_prefix(APPLE_PREFIX) {
                Some(v) if !v.is_empty() => Choice::Apple(Some(v.to_string())),
                Some(_) => Choice::Apple(None),
                None => Choice::Unknown(s.to_string()),
            },
        }
    }
    pub fn is_apple(&self) -> bool {
        matches!(self, Choice::Apple(_))
    }
}

/// The document value for a pinned decoder version: `"apple:8"`.
pub fn apple_value(version: &str) -> String {
    format!("{APPLE_PREFIX}{version}")
}

/// Why the engine develops this photo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// chosen in the document (`"apple:…"`)
    Chosen,
    /// automatic: graineo cannot decode the file
    Fallback,
}

/// Settings key `appleRawFallback`: `"newest"` = the automatic fallback uses the newest version the file offers;
/// anything else (default) = the newest below 9 when there is one (see [`raw::apple::fallback_version`]).
pub const FALLBACK_SETTING: &str = "appleRawFallback";

fn fallback_prefers_newest() -> bool {
    library::settings::value(FALLBACK_SETTING).as_str() == Some("newest")
}

/// White balance handed to the engine: (Kelvin, tint), the engine uses the same temperature scale and tint sign as
/// [`crate::wb`] (+ = magenta); `None` = as shot. "Automatic" is as shot
/// on this path (the engine has no automatic white balance of its own; open point).
pub fn white_for(s: &Settings) -> Option<(f32, f32)> {
    let r = &s.raw;
    match r.wb_mode.as_str() {
        "as_shot" | "" | "auto" => None,
        "custom" => Some((r.temperature, r.tint)),
        v => crate::wb::preset(v),
    }
}

/// White balance as cache key (10 K, 0.5 tint): slider moves below that do not render again.
fn white_key(w: Option<(f32, f32)>) -> Option<(i32, i32)> {
    w.map(|(k, t)| ((k / 10.0).round() as i32, (t * 2.0).round() as i32))
}

/// Exposure of the engine's linear output relative to graineo's linear stage, in EV (added to graineo's base
/// exposure). The engine's own baseline exposure is switched off ([`params`]). Measured with `graineo-dev apple-stage`
/// (same file through both paths, median of the luminance ratio of the linear previews): Nikon Z 8 lossless +0.06 EV
/// (decoder 8 and 9), Canon EOS RP −0.04, Nikon D850 +0.26; the value is the one of the Z 8, the camera of the
/// High Efficiency fallback.
pub const LINEAR_OFFSET_EV: f32 = 0.06;

/// Times of the engine for the UI (milliseconds).
#[derive(Clone, Copy, Debug, Default)]
pub struct Times {
    pub preview: Option<f64>,
    pub full: Option<f64>,
    /// the first rendering of this photo (may include the engine's shader/model compilation)
    pub first: Option<f64>,
}

/// The Apple development of one photo: what the engine said, the version used and the rendered stages.
pub struct AppleDev {
    pub path: PathBuf,
    pub probe: Probe,
    /// decoder version used
    pub used: String,
    /// the version requested in the document when the system does not offer it (then [`Self::used`] is the newest)
    pub pinned_missing: Option<String>,
    pub reason: Reason,
    /// why graineo cannot decode the file (e.g. Nikon HE), if known
    pub graineo_blocked: Option<String>,
    preview: Mutex<Vec<(Option<(i32, i32)>, Arc<Stage>)>>,
    full: Mutex<Option<(Option<(i32, i32)>, Arc<Stage>)>>,
    times: Mutex<Times>,
    /// renderings that failed, per white balance (key) and size (`true` = full resolution), with the error: asked
    /// again only for other parameters (another white balance) or after opening the photo again. Without it a
    /// failing engine (file gone from the NAS, Core Image without an image) would render again on every request:
    /// each slider move of the preview, a full-resolution crop every few seconds.
    failed: Mutex<Vec<(Option<(i32, i32)>, bool, String)>>,
    /// renderings asked of the engine (for tests and the log)
    renders: AtomicUsize,
    /// white balance (Kelvin, tint) of the preview rendered on opening: the document's preview (the app's sample
    /// image for the white balance picker, [`crate::wb_pick`]) is balanced for it
    opened_white: (f32, f32),
}

impl AppleDev {
    /// Document value of the version used (`"apple:8"`): what is pinned on first development.
    pub fn pin_value(&self) -> String {
        apple_value(&self.used)
    }
    /// White balance the document's preview was rendered with (see the field).
    pub fn opened_white(&self) -> (f32, f32) {
        self.opened_white
    }
    pub fn times(&self) -> Times {
        *self.times.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn note_time(&self, ms: f64, full: bool) {
        let mut t = self.times.lock().unwrap_or_else(|e| e.into_inner());
        if t.first.is_none() {
            t.first = Some(ms);
        }
        if full {
            t.full = Some(ms);
        } else {
            t.preview = Some(ms);
        }
    }

    fn params(&self, s: &Settings, max_edge: Option<usize>) -> Params {
        params(&self.used, white_for(s), max_edge)
    }

    /// The development of this photo (`path`, version, everything else empty); see [`open`].
    fn new(path: &Path, probe: Probe, used: String, pinned_missing: Option<String>, reason: Reason, graineo_blocked: Option<String>) -> AppleDev {
        AppleDev {
            path: path.to_path_buf(),
            probe,
            used,
            pinned_missing,
            reason,
            graineo_blocked,
            preview: Mutex::new(Vec::new()),
            full: Mutex::new(None),
            times: Mutex::new(Times::default()),
            failed: Mutex::new(Vec::new()),
            renders: AtomicUsize::new(0),
            opened_white: (5500.0, 0.0),
        }
    }

    /// Renderings asked of the engine so far.
    pub fn renders(&self) -> usize {
        self.renders.load(Ordering::Relaxed)
    }

    /// Render once for this key and size; a failure is remembered and answered without asking the engine again.
    fn render_once(&self, key: Option<(i32, i32)>, full: bool, p: &Params) -> Result<(Stage, f64), String> {
        if let Some((.., e)) = self.failed.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, f, _)| *k == key && *f == full) {
            return Err(e.clone());
        }
        self.renders.fetch_add(1, Ordering::Relaxed);
        render_stage(&self.path, p, full).inspect_err(|e| {
            eprintln!("apple raw {} {}: {e} (not asked again for this white balance)", if full { "full" } else { "preview" }, self.path.display());
            self.failed.lock().unwrap_or_else(|e| e.into_inner()).push((key, full, e.clone()));
        })
    }

    /// Preview stage (≈ 2048 px) for the white balance of `s`, cached (the last two).
    pub(crate) fn preview_for(&self, s: &Settings) -> Result<Arc<Stage>, String> {
        let key = white_key(white_for(s));
        if let Some(st) = self.preview.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == key).map(|(_, s)| s.clone()) {
            return Ok(st);
        }
        let st = Arc::new(self.render_once(key, false, &self.params(s, Some(PREVIEW_EDGE))).map(|(st, ms)| {
            self.note_time(ms, false);
            st
        })?);
        let mut c = self.preview.lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|(k, _)| *k != key);
        c.insert(0, (key, st.clone()));
        c.truncate(2);
        Ok(st)
    }

    /// Full-resolution stage for the white balance of `s`, cached (one).
    pub(crate) fn full_for(&self, s: &Settings) -> Result<Arc<Stage>, String> {
        let key = white_key(white_for(s));
        if let Some((k, st)) = self.full.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *k == key {
                return Ok(st.clone());
            }
        }
        let (st, ms) = self.render_once(key, true, &self.params(s, None))?;
        self.note_time(ms, true);
        let st = Arc::new(st);
        *self.full.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, st.clone()));
        Ok(st)
    }

    /// Part of the detail key: white balance and version (another rendering → other detail stages).
    pub(crate) fn key(&self, s: &Settings) -> u64 {
        let w = white_key(white_for(s)).map_or(0u64, |(k, t)| ((k as u32 as u64) << 32) | t as u32 as u64);
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.used.bytes().chain(w.to_le_bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h | 1
    }

    pub(crate) fn release(&self) {
        *self.full.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.preview.lock().unwrap_or_else(|e| e.into_inner()).truncate(1);
    }
}

/// Parameters of a rendering for the development: all optional steps off, no tone curve, linear Rec. 2020.
pub fn params(version: &str, white: Option<(f32, f32)>, max_edge: Option<usize>) -> Params {
    Params { decoder: Some(version.to_string()), steps: Steps::Linear, exposure_ev: 0.0, white, max_edge, region: None }
}

/// Render through a shared engine into a stage (RGB, linear Rec. 2020; negative and non-finite values to 0), with a
/// noise profile estimated from the image (as for finished images: there is no mosaic to measure).
fn render_stage(path: &Path, p: &Params, full: bool) -> Result<(Stage, f64), String> {
    let t = std::time::Instant::now();
    let r = engine::with_shared(path, |e| e.render(path, p)).and_then(|r| r).map_err(|e| e.to_string())?;
    let px: Vec<[f32; 3]> = r.rgba.chunks_exact(4).map(|q| [q[0], q[1], q[2]].map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 })).collect();
    let profile = raw::noise::image_noise_model(&px, r.width, r.height).map(|m| raw::noise::image_profile(m, 1.0));
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    if std::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("apple {} {}×{} decoder {} in {:.0} ms (engine {:.0} ms)", if full { "full" } else { "preview" }, r.width, r.height, r.decoder, ms, r.millis);
    }
    Ok((Stage::new(px, r.width, r.height, profile), ms))
}

/// Is the engine there at all (macOS with Core Image)? Test switch `GRAINEO_NO_APPLE_RAW=1`: as if it were not (shows
/// what Windows or an engine without the camera would show).
pub fn available() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("GRAINEO_NO_APPLE_RAW").is_none()
}

/// Open `path` with the engine. `requested`: pinned version (`None` = default version for `reason`). `blocked`: why
/// graineo cannot decode it (if known).
pub(crate) fn open(path: &Path, s: &Settings, requested: Option<&str>, reason: Reason, blocked: Option<String>) -> Result<RawDoc, String> {
    if !available() {
        return Err(engine::Error::NotAvailable.to_string());
    }
    let t = std::time::Instant::now();
    let probe = engine::with_shared(path, |e| e.probe(path)).and_then(|r| r).map_err(|e| e.to_string())?;
    remember_support(path, &probe);
    let (used, pinned_missing) = match requested {
        Some(v) => {
            let c = engine::choose_version(&probe.versions, &probe.engine_default, Some(v)).ok_or("no decoder version")?;
            (c.used, c.pinned_missing)
        }
        None => (engine::fallback_version(&probe.versions, &probe.engine_default, fallback_prefers_newest()).ok_or("no decoder version")?, None),
    };
    let mut dev = AppleDev::new(path, probe, used, pinned_missing, reason, blocked);
    dev.opened_white = white_for(s).unwrap_or(if dev.probe.as_shot.0 > 1000.0 { dev.probe.as_shot } else { (5500.0, 0.0) });
    let preview = dev.preview_for(s)?;
    let doc = assemble(path, Arc::new(dev), preview);
    if std::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("apple open {}: {:.0?}", path.display(), t.elapsed());
    }
    Ok(doc)
}

/// Remember the engine's answer for this kind of file (see [`support`]).
fn remember_support(path: &Path, probe: &Probe) {
    if let Some(key) = support_key_of(path, probe.native) {
        support_note(key, &Ok(probe.clone()));
    }
}

#[cfg(target_os = "macos")]
fn file_properties(path: &Path) -> library::imageio::Properties {
    library::imageio::properties(path).unwrap_or_default()
}

/// Key of the support question for a file: maker, model, size, format (from the file header via the system's image
/// reader; `None` when the header says nothing).
pub fn support_key_of(path: &Path, size: [usize; 2]) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let e = file_properties(path);
        let model = e.camera.clone()?;
        let size = if size[0] > 0 { size } else { [e.width? as usize, e.height? as usize] };
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        Some(engine::support_key(e.maker.as_deref().unwrap_or(""), &model, size, ext))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, size);
        None
    }
}

fn support_cache() -> &'static Mutex<Option<engine::SupportCache>> {
    static C: Mutex<Option<engine::SupportCache>> = Mutex::new(None);
    &C
}

/// Settings key of the remembered answers: `{ "system": "<macOS version>", "files": { "<key>": ["7","8"] } }`.
const SUPPORT_SETTING: &str = "appleRawSupport";

fn with_support<R>(f: impl FnOnce(&mut engine::SupportCache) -> R) -> R {
    let mut g = support_cache().lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(|| {
        let v = library::settings::value(SUPPORT_SETTING);
        let stored = v.get("system").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let entries = v.get("files").and_then(|x| x.as_object()).map(|m| {
            m.iter().map(|(k, a)| (k.clone(), a.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default())).collect::<Vec<_>>()
        });
        engine::SupportCache::restore(engine::system_version(), &stored, entries.unwrap_or_default())
    });
    f(c)
}

/// Remember an answer of the engine (in the background: written to `settings.json` on a helper thread).
pub fn support_note(key: String, result: &Result<Probe, engine::Error>) {
    let changed = with_support(|c| c.note(key, result));
    if changed {
        let v = with_support(|c| {
            let files: serde_json::Map<String, serde_json::Value> = c.entries().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect();
            serde_json::json!({ "system": c.system(), "files": files })
        });
        std::thread::spawn(move || library::settings::set(SUPPORT_SETTING, v));
    }
}

/// Remembered answer for this key: offered versions (empty = not supported); `None` = not asked yet on this system.
pub fn support(key: &str) -> Option<Vec<String>> {
    with_support(|c| c.get(key).cloned())
}

/// The document of an Apple development: like a finished image in linear Rec. 2020 ("camera RGB" = Rec. 2020, white
/// balance done by the engine), but with the RAW tone curve and graineo's base exposure.
fn assemble(path: &Path, dev: Arc<AppleDev>, preview: Arc<Stage>) -> RawDoc {
    use raw::color;
    #[cfg(target_os = "macos")]
    let e = file_properties(path);
    #[cfg(not(target_os = "macos"))]
    let e = library::imageio::Properties::default();
    let xyz_rec2020 = color::inv(&color::rec2020_to_xyz_d65()).expect("Rec. 2020");
    let [w, h] = dev.probe.native;
    let maker = e.maker.clone().unwrap_or_default();
    let model = e.camera.clone().unwrap_or_default();
    let info = raw::Info {
        format: path.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_uppercase(),
        maker: maker.clone(),
        model: model.clone(),
        mode: format!("Apple RAW {}", engine::version_label(&dev.used)),
        width: w,
        height: h,
        channels: 3,
        bits: 16,
        sensor: raw::Sensor::Linear,
        cfa: String::new(),
        cfa_size: (0, 0),
        black: Vec::new(),
        black_pattern: (1, 1, 1),
        white: vec![1.0],
        wb: Some([1.0; 3]),
        matrix: Some(("D65".into(), xyz_rec2020)),
        matrix_illuminants: vec!["D65".into()],
        // the engine renders in sensor orientation ("up"); the photo's orientation comes from the header
        orientation: e.orientation.unwrap_or(1) as u16,
        active: None,
        crop: None,
        fuji_rotated: false,
        baseline_exposure: 0.0,
        dng_profile: Vec::new(),
        exif: raw::Exif {
            iso: e.iso.map(|v| v as u32),
            exposure_time: e.time.map(|v| v as f32),
            aperture: e.aperture.map(|v| v as f32),
            focal_length: e.focal.map(|v| v as f32),
            distance: None,
            lens_maker: None,
            lens: e.lens.clone(),
            lens_db: None,
            capture_date: e.capture.clone(),
            serial_number: None,
        },
        corrections: vec!["Apple RAW"],
        calibration: vec![color::Calibration { kelvin: 6504.0, color: xyz_rec2020, forward: None }],
        analog_balance: None,
        fuji_dr: None,
    };
    let cam_xyz_shot = color::determine([1.0; 3], &info.calibration, &color::diag([1.0; 3])).map(|c| c.camera_to_xyz_d50).unwrap_or(color::diag([1.0; 3]));
    let as_shot = if dev.probe.as_shot.0 > 1000.0 { dev.probe.as_shot } else { (5500.0, 0.0) };
    let picture_style = raw::picture_style::read(path);
    let short = raw::picture_style::camera_short(&maker, &model);
    let looks: Vec<String> = crate::looks::names_for_camera(&short);
    let auto_look = picture_style.as_ref().map(|b| format!("{}/{}", b.camera_short(), b.style)).filter(|n| looks.contains(n));
    let (info_rows, summary) = super::info_texts(&info, picture_style.as_ref(), as_shot);
    let base_ev = raw::development::BASE_EV + raw::development::camera_ev(&maker, &model) + LINEAR_OFFSET_EV;
    RawDoc {
        path: path.to_path_buf(),
        logical_path: path.to_path_buf(),
        orientation: super::quarters(info.orientation),
        full_size: [w, h],
        info,
        preview,
        picture_style,
        auto_look,
        // learned looks and the per-camera hue table were fitted on graineo's colour: not on this path
        learn_key: None,
        plain: Default::default(),
        looks,
        as_shot,
        auto_neutral: [1.0; 3],
        base_ev,
        lr_offset: 0.0,
        cam_xyz_shot,
        std_look: None,
        info_rows,
        summary,
        finished: false,
        hdr: None,
        file: Mutex::new(None),
        full: Mutex::new(None),
        full_detail: Mutex::new(None),
        full_busy: Mutex::new(()),
        preview_detail: Mutex::new(Vec::new()),
        correction: Mutex::new(None),
        looks_cache: Mutex::new(Vec::new()),
        matched: Mutex::new(None),
        apple: Some(dev),
        engine_request: None,
        ai: Default::default(),
        merged: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_values_are_read() {
        assert_eq!(Choice::parse(None), Choice::Auto);
        assert_eq!(Choice::parse(Some("")), Choice::Auto);
        assert_eq!(Choice::parse(Some("graineo")), Choice::Graineo);
        assert_eq!(Choice::parse(Some("apple:8")), Choice::Apple(Some("8".into())));
        assert_eq!(Choice::parse(Some("apple:9.dng")), Choice::Apple(Some("9.dng".into())));
        assert_eq!(Choice::parse(Some("apple")), Choice::Apple(None));
        assert_eq!(Choice::parse(Some("other:1")), Choice::Unknown("other:1".into()));
        assert_eq!(apple_value("8.dng"), "apple:8.dng");
        assert!(Choice::parse(Some(&apple_value("8"))).is_apple());
    }

    #[test]
    fn white_balance_goes_to_the_engine_and_as_shot_stays_as_shot() {
        let mut s = Settings::default();
        assert_eq!(white_for(&s), None);
        s.raw.wb_mode = "auto".into();
        assert_eq!(white_for(&s), None, "automatic is as shot on this path");
        s.raw.wb_mode = "custom".into();
        s.raw.temperature = 4321.0;
        s.raw.tint = 7.0;
        assert_eq!(white_for(&s), Some((4321.0, 7.0)));
        s.raw.wb_mode = "daylight".into();
        assert_eq!(white_for(&s), crate::wb::preset("daylight"));
        // small slider moves share one rendering
        assert_eq!(white_key(Some((4321.0, 7.1))), white_key(Some((4322.0, 7.0))));
        assert_ne!(white_key(Some((4321.0, 7.0))), white_key(Some((4400.0, 7.0))));
    }

    /// A rendering that failed is not asked of the engine again for the same white balance and size (it would
    /// otherwise run again on every slider move or crop request); another white balance asks again.
    #[test]
    fn a_failed_rendering_is_not_asked_again_for_the_same_parameters() {
        let probe = Probe { versions: vec!["8".into()], engine_default: "8".into(), native: [6000, 4000], baseline_exposure: 0.0, as_shot: (5000.0, 0.0) };
        // a file that is gone (NAS disconnected after opening): every rendering fails, on every system
        let dev = AppleDev::new(Path::new("/nonexistent/graineo-test/DSC_0001.NEF"), probe, "8".into(), None, Reason::Chosen, None);
        let s = Settings::default();
        let first = dev.preview_for(&s).err().expect("fails");
        assert_eq!(dev.renders(), 1);
        for _ in 0..5 {
            assert_eq!(dev.preview_for(&s).err().as_deref(), Some(first.as_str()));
        }
        assert_eq!(dev.renders(), 1, "the same parameters are not rendered again");
        assert!(dev.full_for(&s).is_err());
        assert!(dev.full_for(&s).is_err());
        assert_eq!(dev.renders(), 2, "full resolution: one attempt of its own");
        let mut warm = s.clone();
        warm.raw.wb_mode = "custom".into();
        warm.raw.temperature = 7500.0;
        assert!(dev.preview_for(&warm).is_err());
        assert_eq!(dev.renders(), 3, "other parameters are asked again");
    }

    #[test]
    fn the_rendering_is_linear_without_optional_steps() {
        let p = params("8", None, Some(2048));
        assert_eq!((p.steps, p.exposure_ev, p.decoder.as_deref()), (Steps::Linear, 0.0, Some("8")));
        let v = engine::filter_values(&p);
        for s in ["setBoostAmount:", "setLuminanceNoiseReductionAmount:", "setColorNoiseReductionAmount:", "setSharpnessAmount:", "setLensCorrectionEnabled:", "setGamutMappingEnabled:", "setLocalToneMapAmount:", "setMoireReductionAmount:", "setDetailAmount:", "setContrastAmount:"] {
            assert!(v.contains(&(s, 0.0)), "{s}");
        }
    }
}
