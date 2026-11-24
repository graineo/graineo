//! Development settings of an image (storable as JSON).

use serde::{Deserialize, Serialize};

/// What is displayed or output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum View {
    /// Normally developed image (colour or monochrome)
    Developed,
    /// A sensor layer: 0 = bottom ("R"), 1 = middle ("G"), 2 = top ("B")
    Layer(usize),
    /// All three layers directly as R/G/B, without colour matrix
    RawComposite,
    /// Saturated sensor sites highlighted per layer
    Clipping,
    /// Ratio of the layers to each other (brightness factored out)
    Chroma,
    /// Pipeline inspection: intermediate stage of the development or, with `diff`, its difference image
    /// to the previous stage, amplified by 2^`gain` ([`crate::inspect`]). Like all views, display
    /// only; not saved.
    Stage { stage: crate::inspect::Stage, diff: bool, gain: u8 },
}

impl View {
    /// A pipeline inspection stage?
    pub fn is_stage(self) -> bool {
        matches!(self, View::Stage { .. })
    }
}

/// Binning at high ISO (Quattro): `Auto` = as in the camera maker's software (sd Quattro / sd Quattro H by capture
/// ISO, dp Quattro never); `Off` = never; `On` = the same ISO steps also for the dp Quattro (own addition, the maker's
/// software never bins the dp Quattro).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Binning {
    #[default]
    Auto,
    Off,
    On,
}

impl Binning {
    pub fn is_auto(&self) -> bool {
        *self == Binning::Auto
    }
}

/// Steps of the Quattro detail slider on each side of the middle ([`Settings::quattro_detail`]).
pub const QUATTRO_DETAIL_RANGE: i8 = 3;

fn half() -> f32 {
    0.5
}

fn is_half(v: &f32) -> bool {
    *v == 0.5
}

/// Highlight sliders of the rebuild ([`crate::highlight_rebuild`]) in thousandths, part of the base key: 500 = as
/// rebuilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HighlightSliders {
    pub colour_milli: u16,
    pub structure_milli: u16,
    /// the rebuild is on (a Merrill base developed with it differs although the sliders do not act there)
    pub on: bool,
}

impl Default for HighlightSliders {
    fn default() -> Self {
        Self { colour_milli: 500, structure_milli: 500, on: false }
    }
}

impl HighlightSliders {
    pub fn new(colour: f32, structure: f32) -> Self {
        let m = |v: f32| (v.clamp(0.0, 1.0) * 1000.0).round() as u16;
        Self { colour_milli: m(colour), structure_milli: m(structure), on: true }
    }
    pub fn colour(&self) -> f32 {
        self.colour_milli as f32 / 1000.0
    }
    pub fn structure(&self) -> f32 {
        self.structure_milli as f32 / 1000.0
    }
    pub fn is_default(&self) -> bool {
        self.colour_milli == 500 && self.structure_milli == 500
    }
}

fn is_zero_step(v: &i8) -> bool {
    *v == 0
}

/// Slider position "Off" of the luminance/colour noise sliders, a quarter step below SPP's
/// minimum 0 (x3fuse-core `SppNoiseSliders::OFF`).
pub const NR_OFF: f64 = -0.25;

/// Colour mode value of the linear scene rendering ("Linear (scene)"): the camera matrix of the neutral reference
/// mode and the white balance, then a straight line instead of the tone curve (no contrast curve, no colour mode
/// tables). Stored like any colour mode in [`Settings::color_mode`].
pub const LINEAR_MODE: &str = "Linear";

/// Colour mode whose matrix and base a stored mode uses: the linear rendering takes the neutral reference mode.
pub fn matrix_mode(name: Option<&str>) -> Option<&str> {
    match name {
        Some(LINEAR_MODE) => Some(crate::profile::NEUTRAL_MODE),
        n => n,
    }
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

/// What an SPP base was computed for: colour mode and steps (luminance, colour) 0…4 of the
/// noise sliders (2 = middle position, −1 = "Off" below SPP's minimum: Sigma's
/// detail processing without smoothing). See [`Settings::spp_key`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppKey {
    pub mode: Option<String>,
    pub nr: (i8, i8),
    /// Detail step of the Quattro chain from capture ISO 400 on, −3 (soft) … 3 (strong), 0 = the camera's setting
    /// ([`Settings::quattro_detail`]); 0 for every other class
    pub detail: i8,
    /// Chromatic aberration correction as in SPP (computed in the base, see [`crate::lens`])
    pub ca: crate::lens::CaKey,
    /// Black and white the Sigma way: grey base from the preprocessed layers ([`crate::mono`])
    /// instead of the colour base (Merrill; other models ignore it)
    pub mono: bool,
    /// Black and white the Sigma way: normalised layer mix in thousandths (part of the grey
    /// base, see [`crate::mono`]); zero otherwise
    pub mono_mix: [u16; 3],
    /// Camera class the key was made for (Quattro or Classic; otherwise Merrill), see [`SppKey::for_class`]
    pub prepared: bool,
    /// Binning setting (Quattro, see [`crate::quattro::binning_factor`]); always `Auto` for Merrill
    pub binning: Binning,
    /// Neural denoising after Sigma's noise reduction in percent ([`crate::ai_denoise`]); 0 = off (colour bases only)
    pub ai: u8,
    /// Highlight sliders (Quattro with the highlight rebuild switched on; default otherwise)
    pub highlight: HighlightSliders,
    /// Foveon detail of the Merrill base in percent: gain of the fine and the structure band of the luminance
    /// residual ([`Settings::detail_fine`], [`Settings::detail_structure`]); `[100, 100]` = Sigma's detail
    /// unchanged. Quattro and Classic: the same bands on the noise-reduced base ([`crate::foveon_texture`]).
    /// Neutral in monochrome.
    pub texture: [u8; 2],
}

impl Default for SppKey {
    fn default() -> Self {
        Self { mode: None, nr: (0, 0), detail: 0, ca: Default::default(), mono: false, mono_mix: [0; 3], prepared: false, binning: Default::default(), ai: 0, highlight: Default::default(), texture: TEXTURE_NEUTRAL }
    }
}

/// [`SppKey::texture`] with Sigma's detail unchanged.
pub const TEXTURE_NEUTRAL: [u8; 2] = [100, 100];

impl SppKey {
    /// Band gains for the SPP noise reduction (`[1, 1]` = unchanged).
    pub fn texture_gains(&self) -> [f32; 2] {
        self.texture.map(|v| f32::from(v) / 100.0)
    }

    /// Slider positions 0…1 for Sigma's noise reduction (−0.25 = "Off").
    pub fn sliders(&self) -> crate::denoise::DenoiseSliders {
        crate::denoise::DenoiseSliders::new(self.nr.0 as f64 / 4.0, self.nr.1 as f64 / 4.0)
    }

    /// Both noise sliders and the detail slider in the middle position?
    pub fn neutral_noise(&self) -> bool {
        self.nr == (2, 2) && self.detail == 0
    }

    /// Only what changes the base of this camera class (`prepared`: Quattro or Classic, otherwise Merrill);
    /// everything else gets a fixed value, so that a change without effect never recomputes the base or misses
    /// the disk cache. The base computed from the key is identical (the dropped parts are not read for that class):
    /// - Merrill: the binning setting, the Quattro detail
    /// - Quattro/Classic: black and white base and mix, CA correction
    pub fn for_class(mut self, prepared: bool) -> Self {
        let mono = self.mono;
        self.prepared = prepared;
        if prepared {
            self.mono = false;
            self.mono_mix = [0; 3];
            self.ca = crate::lens::CaKey::default();
        } else {
            self.binning = Binning::Auto;
            self.detail = 0;
        }
        if mono {
            self.texture = TEXTURE_NEUTRAL;
        }
        self
    }
}

/// Sections whose effect is temporarily switched off (an eye or checkbox toggle per
/// section); the slider values are kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bypass {
    /// Basic settings: white balance fine adjustment, tone, Fill Light, presence
    pub basic: bool,
    pub mixer: bool,
    pub grading: bool,
    pub effects: bool,
    /// Tone curve
    pub curve: bool,
    /// additional sharpening
    pub sharpen: bool,
    /// Film simulation ([`crate::film`])
    pub film: bool,
    /// Layer mixer in colour ([`crate::layers`])
    pub layers: bool,
    /// Colour casts: neutralise shadows, neutral highlights
    pub casts: bool,
}

impl Bypass {
    pub fn any(&self) -> bool {
        self.basic || self.mixer || self.grading || self.effects || self.sharpen || self.curve || self.film || self.layers || self.casts
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
pub struct Settings {
    /// Temporarily disabled sections
    pub bypass: Bypass,
    pub monochrome: bool,
    /// Sigma colour mode; `None` = as set in the camera
    pub color_mode: Option<String>,
    /// Profile amount (colour mode or film) 0 … 1, default 1; blends with the neutral
    /// reference, see [`crate::profile`]
    pub profile_amount: f32,
    /// Exposure in stops (−2 … +2 as in SPP, unrestricted here)
    pub exposure: f64,
    pub contrast: f64,
    pub shadow: f64,
    pub highlight: f64,
    /// Fill Light −2 … +2
    pub fill_light: f32,
    /// Slider "Lichter-Farbe" 0 … 1 (0.5 = as rebuilt, 0 = neutral): colour of the rebuilt highlights of Foveon photos
    /// ([`crate::highlight_rebuild`], only with the rebuild switched on). Left out of the stored JSON at its default.
    #[serde(default = "half", skip_serializing_if = "is_half")]
    pub highlight_colour: f32,
    /// Slider "Lichter-Struktur" 0 … 1 (0.5 = as rebuilt, 0 = smooth): detail of the rebuilt highlights.
    #[serde(default = "half", skip_serializing_if = "is_half")]
    pub highlight_structure: f32,
    /// Noise brake when brightening (see [`crate::lift::Brake`]); off by default (switched on by hand, costs some
    /// export time).
    #[serde(default = "NoiseBrake::off", skip_serializing_if = "NoiseBrake::is_off")]
    pub noise_brake: NoiseBrake,
    /// Dehaze −1 … +1 (0 = off), see [`crate::dehaze`]
    #[serde(skip_serializing_if = "is_zero")]
    pub dehaze: f32,
    /// Tone equalizer: exposure per brightness zone with an edge-preserving mask, see [`crate::tone_eq`]. Left out of
    /// the stored JSON while neutral, so existing photos stay byte-identical.
    #[serde(rename = "toneEqualizer", default, skip_serializing_if = "crate::tone_eq::ToneEq::is_default")]
    pub tone_eq: crate::tone_eq::ToneEq,
    /// Tone equalizer: show its mask instead of the image (display only, never stored; switches film and look stage off)
    #[serde(skip)]
    pub tone_eq_mask: bool,
    /// Monochrome mix bottom / middle / top layer (SPP default 0.33/0.33/0.34)
    pub mix: [f32; 3],
    /// Layer mixer in colour: `m[R/G/B][bottom/middle/top layer]` instead of Sigma's
    /// colour matrix/ColorDQ; `None` = off (Sigma). Monochrome takes precedence. See [`crate::layers`].
    pub layer_matrix: Option<[[f32; 3]; 3]>,
    /// Highlight neutralisation 0 … 1 (SPP default: off)
    pub highlight_neutralize: f32,
    /// Presence: vibrance (spares already strong colours) and saturation, −1 … +1
    pub vibrance: f32,
    pub saturation: f32,
    /// Color grading: [hue in degrees, saturation 0 … 1] for shadows, midtones, highlights
    pub grade_shadows: [f32; 2],
    pub grade_midtones: [f32; 2],
    pub grade_highlights: [f32; 2],
    /// −1 … +1: shifts the boundary between shadow and highlight toning
    pub grade_balance: f32,
    /// Matte blacks (raised black point), 0 … 0.3
    pub fade: f32,
    /// Vignette −1 (dark) … +1 (bright) and midpoint 0 … 1
    pub vignette: f32,
    pub vignette_mid: f32,
    /// Colour mixer (HSL): per colour range (red, orange, yellow, green, aqua,
    /// blue, purple, magenta) [hue, saturation, luminance], each −1 … +1
    pub hsl: [[f32; 3]; 8],
    /// Clarity (local contrast, −1 … +1)
    pub clarity: f32,
    /// Tone curve (point curve RGB/red/green/blue) on the output values, see [`crate::curve`]
    pub tone_curve: crate::curve::ToneCurve,
    /// Sigma's sharpness slider as in SPP, −2 … +2 in tenths (0 = SPP default, already includes
    /// Sigma's base sharpening), see [`crate::sharpness`]
    pub sigma_sharpness: f32,
    /// Additional sharpening: amount 0 … 2, radius 0.5 … 3 px, masking 0 … 1
    pub sharpen: f32,
    pub sharpen_radius: f32,
    pub sharpen_mask: f32,
    /// Crop: centre x/y and width/height relative to the image size (rotated frame)
    pub crop: [f32; 4],
    /// Straighten: rotation in degrees (−45 … 45)
    pub crop_angle: f32,
    /// Perspective (transform) vertical/horizontal, −1 … +1, 0 = off;
    /// negative widens the top or left, see [`crate::geometry::Frame::persp_to_ori`]
    pub perspective_v: f32,
    pub perspective_h: f32,
    /// Additional rotation in 90° steps clockwise (on top of the camera orientation), 0 … 3
    pub rotate: u8,
    /// Film grain: amount 0 … 1, size 0.5 … 3, roughness 0 … 1
    pub grain_amount: f32,
    pub grain_size: f32,
    pub grain_rough: f32,
    /// Remove colour fringes (manual defringe): amount 0 … 1
    /// (Lightroom XMP 0 … 20) and hue range [from, to] in Lightroom XMP units 0 … 100,
    /// see [`crate::defringe`]
    pub defringe_purple_amount: f32,
    pub defringe_purple_hue: [f32; 2],
    pub defringe_green_amount: f32,
    pub defringe_green_hue: [f32; 2],
    /// SPP's own fringe correction (green/magenta hue band), taken over from files saved in
    /// SPP; off by default, see [`crate::spp_defringe`]
    #[serde(default, skip_serializing_if = "crate::spp_defringe::SppDefringe::is_off")]
    pub spp_defringe: crate::spp_defringe::SppDefringe,
    /// Lens corrections as in SPP: chromatic aberration off / lens profile / manual,
    /// profile amount 0 … 2 (default 1), red and blue −2 … 2; step 0.1 as in SPP.
    /// Acts in the SPP base (Merrill only, not in monochrome), see [`crate::lens`].
    pub ca_mode: crate::lens::CaMode,
    pub ca_profile: f32,
    pub ca_red: f32,
    pub ca_blue: f32,
    /// Legacy switch "Foveon-Detail" (until 1.5.0): `false` = own pipeline without Sigma's base/detail
    /// processing. Kept so that photos stored with it look the same; the panel now offers
    /// [`Self::detail_fine`] and [`Self::detail_structure`] instead, moving one of them switches it on again.
    pub denoise: bool,
    /// Merrill: gain of the fine band of Sigma's detail (pores, grain, residual noise), 0 … 2, 1 = Sigma's
    /// detail unchanged. Acts in the SPP base, independent of the noise sliders.
    #[serde(skip_serializing_if = "is_one")]
    pub detail_fine: f32,
    /// Merrill: gain of the structure band (eyelashes, hair, edges, skin relief), 0 … 2, 1 = unchanged.
    #[serde(skip_serializing_if = "is_one")]
    pub detail_structure: f32,
    /// Noise sliders 0 … 1 (0.5 = neutral); [`NR_OFF`] = "Off" (SPP base only: Sigma's
    /// detail without smoothing; our own noise reduction treats it as 0)
    pub nr_luma: f64,
    pub nr_chroma: f64,
    /// Detail of the Quattro from capture ISO 400 on: seven steps −3 (soft) … 3 (strong), 0 = the camera's setting.
    /// Sets how much of the fine detail of the top layer comes back after the noise reduction, the local contrast and
    /// the sharpening of the chain ([`crate::quattro_hi`]). Missing in stored settings → 0.
    #[serde(default, skip_serializing_if = "is_zero_step")]
    pub quattro_detail: i8,
    /// Binning of the sd Quattro at high ISO (neighbouring pixels averaged, smaller output, see
    /// [`crate::quattro::binning_factor`]). Missing in stored settings → automatic.
    #[serde(default, skip_serializing_if = "Binning::is_auto")]
    pub binning: Binning,
    /// Sigma white balance preset; `None` = as shot
    pub white_balance: Option<String>,
    /// Eyedropper: additional channel gain (camera space) relative to the preset
    pub wb_mult: [f32; 3],
    /// Fine adjustment: temperature (− cooler / + warmer) and tint (− green / + magenta), −1 … +1
    pub wb_temp: f32,
    pub wb_tint: f32,
    pub view: View,
    /// Brightness of the sensor views in stops
    pub view_gain_ev: f32,
    /// Sensor views of a damaged Sigma file: mark estimated rows (yellow) and grey-filled rows (hatched); display only, never stored
    #[serde(skip)]
    pub view_rescue: bool,
    /// Film simulation (spektrafilm): replaces Sigma's tone curve/output, default off
    pub film: crate::film::FilmSettings,
    /// Non-Sigma RAWs only (CR2/CR3/RAF/DNG/ARW …): own sliders, see [`RawAdjust`]
    #[serde(skip_serializing_if = "RawAdjust::is_default")]
    pub raw: RawAdjust,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<masks::Document>,
    /// Spot retouching (heal/clone), see [`crate::spots`]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spots: Vec<crate::spots::Spot>,
    /// Red-eye correction, see [`crate::spots`]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub red_eyes: Vec<crate::spots::RedEye>,
    /// Removed objects (filled by an inpainting model, patch stored in the library), see [`crate::spots::Removal`]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removals: Vec<crate::spots::Removal>,
    /// Black-and-white mixer: brightness per colour range (red, orange, yellow, green, aqua, blue, violet, magenta), −1 … +1; only acts with `monochrome`
    #[serde(default, skip_serializing_if = "gray_mix_empty")]
    pub gray_mix: [f32; 8],
    /// Export only, never saved: maximum output size for JPEG/TIFF (downscales, never upscales)
    #[serde(skip)]
    pub output_size: Option<crate::export::OutputSize>,
    /// Export only, never saved: resampling filter for `output_size`
    #[serde(skip)]
    pub output_filter: crate::resize::ResizeFilter,
    /// Export only, never saved: output sharpening after scaling ([`crate::output_sharpen`])
    #[serde(skip)]
    pub output_sharpen: crate::output_sharpen::OutputSharpen,
}

/// Noise brake when brightening with Fill Light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseBrake {
    /// Signal and noise are brightened alike (previous behaviour)
    #[default]
    Off,
    /// Colour detail (colour noise) is not brightened
    Color,
    /// In addition luminance grain is brightened less
    ColorGrain,
}

impl NoiseBrake {
    pub fn off() -> Self {
        Self::Off
    }
    pub fn is_off(&self) -> bool {
        *self == Self::Off
    }
    /// Exponents of the gain for luminance and colour detail.
    pub fn exponents(&self) -> Option<(f32, f32)> {
        match self {
            Self::Off => None,
            Self::Color => Some((1.0, 0.0)),
            Self::ColorGrain => Some((0.7, 0.0)),
        }
    }
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn gray_mix_empty(m: &[f32; 8]) -> bool {
    m.iter().all(|v| *v == 0.0)
}

/// Persistent result of the Lightroom mask import, independent of the image computation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MaskImportReport {
    pub version: u32,
    pub total: usize,
    pub imported: usize,
    pub skipped: usize,
    /// Source data that could not be mapped (key or mask type → count).
    pub unsupported: std::collections::BTreeMap<String, usize>,
}

/// Sliders that exist only for non-Sigma RAWs (crate `rawdev`). Shared with the other
/// fields of [`Settings`]: `exposure` (EV), `contrast`/`highlight`/`shadow`/`vibrance`/
/// `saturation`/`clarity` (−1 … +1 = Lightroom XMP value ÷ 100), HSL, tone curve, sharpening, crop,
/// effects, film and amount.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RawAdjust {
    /// White balance: "as_shot", "auto", "custom" (temperature/tint) or a preset
    /// ("daylight", "cloudy", "shade", "tungsten", "fluorescent", "flash")
    pub wb_mode: String,
    /// Kelvin and tint (Lightroom XMP scale, + magenta) for "custom"
    pub temperature: f32,
    pub tint: f32,
    /// Whites and blacks −1 … +1
    pub whites: f32,
    pub blacks: f32,
    /// Texture (fine detail) −1 … +1
    pub texture: f32,
    /// Noise reduction, 0 … 100
    pub nr_luma: f32,
    pub nr_luma_detail: f32,
    pub nr_color: f32,
    pub nr_color_detail: f32,
    /// False colour suppression (X-Trans only), 0 … 1
    pub false_color: f32,
    /// Neural denoising (`rawdev::ai_denoise`, RawNIND): share of the denoised image, 0 = off … 1 = full; for Sigma
    /// photos after Sigma's noise reduction ([`crate::ai_denoise`], key [`SppKey::ai`]). Not written
    /// while off, so the JSON of existing photos stays byte-identical; older versions ignore the field.
    #[serde(rename = "aiDenoise", default, skip_serializing_if = "is_zero")]
    pub ai_denoise: f32,
    /// Camera profile (DCP) installed locally, as its path: replaces the camera matrices and applies its HueSatMap and
    /// LookTable (`raw::dcp`). Not written while unset, so existing documents stay byte-identical.
    #[serde(rename = "cameraProfile", default, skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<String>,
    /// Calibration: hue and saturation of red, green, blue, then shadow tint (+ magenta), each −1 … +1 (Lightroom XMP value ÷ 100)
    pub calibration: [f32; 7],
    /// Upright mode from Lightroom XMP (1 Auto, 2 Level, 3 Vertical, 4 Full); applied with our own detection on first open
    /// and not saved
    #[serde(skip)]
    pub upright_from_lr: Option<u8>,
    /// Lens correction (profile): on/off, distortion and vignette 0 … 2 (1 = 100 %), lateral chromatic aberration
    pub lens: bool,
    pub lens_distortion: f32,
    pub lens_vignette: f32,
    pub lens_ca: bool,
    /// Finished images (JPEG/HEIC/TIFF): lensfun correction on/off; None = automatic (off as long as
    /// it is not clear that the camera or phone has not already corrected)
    pub lens_image: Option<bool>,
    /// Assigned own lens profile `"id@version"`; `""` = explicitly none; `None` = automatic (rules, lens name).
    /// Not written while unset, so the JSON of existing photos stays byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens_profile: Option<String>,
    /// Engine of the raw stage: `None` = automatic (graineo; on macOS the system RAW engine only for files graineo cannot
    /// decode), `"graineo"`, `"apple:<decoder version>"` (`"apple:8"`, `"apple:9.dng"`: the system RAW engine of macOS
    /// with that pinned version, see `rawdev::apple`). Not written while unset, so the JSON of existing photos stays
    /// byte-identical; older versions ignore the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Profile: "" or "neutral" = neutral development, otherwise look name ("x100v/provia"). The camera look of the
    /// shot's picture style is only offered, never applied automatically (before 13.11.2026 "" meant it).
    /// Old look names of graineo 1.0 are translated on loading ([`legacy_names::looks`]).
    #[serde(deserialize_with = "legacy_names::looks::deserialize_look")]
    pub look: String,
    /// Display name and package are kept even if the film profile is missing.
    #[serde(rename = "lookName")]
    pub look_name: String,
    #[serde(rename = "lookPackage")]
    pub look_package: String,
    /// Initial values came from Lightroom (notice in Develop)
    pub from_lightroom: bool,
    /// Kept in the sidecar together with the editable masks.
    #[serde(rename = "lightroomMasks", skip_serializing_if = "Option::is_none")]
    pub lightroom_masks: Option<MaskImportReport>,
}

impl Default for RawAdjust {
    fn default() -> Self {
        Self {
            wb_mode: "as_shot".into(),
            temperature: 5500.0,
            tint: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            nr_luma: 0.0,
            nr_luma_detail: 50.0,
            nr_color: 25.0,
            nr_color_detail: 50.0,
            false_color: 0.6,
            ai_denoise: 0.0,
            camera_profile: None,
            calibration: [0.0; 7],
            upright_from_lr: None,
            lens: true,
            lens_distortion: 1.0,
            lens_vignette: 1.0,
            lens_ca: true,
            lens_image: None,
            lens_profile: None,
            engine: None,
            look: String::new(),
            look_name: String::new(),
            look_package: String::new(),
            from_lightroom: false,
            lightroom_masks: None,
        }
    }
}

impl RawAdjust {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Stored settings of graineo 1.0 use old key names: every load goes through here and translates them once
/// ([`legacy_names::settings_keys`]); writing uses the current keys only.
impl<'de> serde::Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut v = <serde_json::Value as serde::Deserialize>::deserialize(d)?;
        legacy_names::settings_keys::upgrade_develop(&mut v);
        // the inherent function generated by `remote = "Self"` (no loop back into this impl)
        Settings::deserialize(v).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for Settings {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Settings::serialize(self, s)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bypass: Bypass::default(),
            monochrome: false,
            color_mode: None,
            profile_amount: 1.0,
            exposure: 0.0,
            contrast: 0.0,
            shadow: 0.0,
            highlight: 0.0,
            fill_light: 0.0,
            highlight_colour: 0.5,
            highlight_structure: 0.5,
            dehaze: 0.0,
            tone_eq: crate::tone_eq::ToneEq::default(),
            tone_eq_mask: false,
            noise_brake: NoiseBrake::Off,
            mix: [0.33, 0.33, 0.34],
            layer_matrix: None,
            highlight_neutralize: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            grade_shadows: [220.0, 0.0],
            grade_midtones: [40.0, 0.0],
            grade_highlights: [45.0, 0.0],
            grade_balance: 0.0,
            fade: 0.0,
            vignette: 0.0,
            vignette_mid: 0.5,
            hsl: [[0.0; 3]; 8],
            clarity: 0.0,
            tone_curve: crate::curve::ToneCurve::default(),
            sigma_sharpness: 0.0,
            sharpen: 0.0,
            sharpen_radius: 1.0,
            sharpen_mask: 0.0,
            crop: [0.5, 0.5, 1.0, 1.0],
            crop_angle: 0.0,
            perspective_v: 0.0,
            perspective_h: 0.0,
            rotate: 0,
            grain_amount: 0.0,
            grain_size: 1.0,
            grain_rough: 0.5,
            defringe_purple_amount: 0.0,
            defringe_purple_hue: [30.0, 70.0],
            defringe_green_amount: 0.0,
            defringe_green_hue: [40.0, 60.0],
            spp_defringe: crate::spp_defringe::SppDefringe::default(),
            ca_mode: crate::lens::CaMode::Off,
            ca_profile: 1.0,
            ca_red: 0.0,
            ca_blue: 0.0,
            denoise: true,
            detail_fine: 1.0,
            detail_structure: 1.0,
            nr_luma: 0.5,
            nr_chroma: 0.5,
            quattro_detail: 0,
            binning: Binning::Auto,
            white_balance: None,
            wb_mult: [1.0; 3],
            wb_temp: 0.0,
            wb_tint: 0.0,
            view: View::Developed,
            view_gain_ev: 1.0,
            view_rescue: false,
            film: crate::film::FilmSettings::default(),
            raw: RawAdjust::default(),
            local: None,
            spots: Vec::new(),
            red_eyes: Vec::new(),
            removals: Vec::new(),
            gray_mix: [0.0; 8],
            output_size: None,
            output_filter: Default::default(),
            output_sharpen: Default::default(),
        }
    }
}

impl Settings {
    /// Linear scene rendering selected ([`LINEAR_MODE`]; black and white keeps its own curve).
    pub fn is_linear(&self) -> bool {
        !self.monochrome && self.color_mode.as_deref() == Some(LINEAR_MODE)
    }
}

impl Settings {
    /// The settings actually in effect: disabled sections reset to default.
    pub fn effective(&self) -> std::borrow::Cow<'_, Settings> {
        if !self.bypass.any() && !self.tone_eq_mask {
            return std::borrow::Cow::Borrowed(self);
        }
        let d = Settings::default();
        let b = self.bypass;
        let mut s = self.clone();
        // the tone equalizer mask is shown as it is: no film over it (the look stage skips itself)
        if self.tone_eq_mask {
            s.film.active = false;
        }
        if b.basic {
            s.wb_temp = d.wb_temp;
            s.wb_tint = d.wb_tint;
            s.exposure = d.exposure;
            s.contrast = d.contrast;
            s.highlight = d.highlight;
            s.shadow = d.shadow;
            s.fill_light = d.fill_light;
            s.highlight_colour = d.highlight_colour;
            s.highlight_structure = d.highlight_structure;
            s.vibrance = d.vibrance;
            s.saturation = d.saturation;
            s.clarity = d.clarity;
        }
        if b.casts {
            s.highlight_neutralize = d.highlight_neutralize;
        }
        if b.mixer {
            s.hsl = d.hsl;
        }
        if b.curve {
            s.tone_curve = d.tone_curve.clone();
        }
        if b.grading {
            s.grade_shadows = d.grade_shadows;
            s.grade_midtones = d.grade_midtones;
            s.grade_highlights = d.grade_highlights;
            s.grade_balance = d.grade_balance;
        }
        if b.effects {
            s.vignette = d.vignette;
            s.grain_amount = d.grain_amount;
            s.fade = d.fade;
        }
        if b.sharpen {
            s.sharpen = d.sharpen;
            s.sigma_sharpness = d.sigma_sharpness;
        }
        if b.film {
            s.film.active = false;
        }
        if b.layers {
            s.layer_matrix = None;
        }
        s.bypass = Bypass::default();
        std::borrow::Cow::Owned(s)
    }

    /// White balance unchanged (as shot)?
    pub fn wb_is_default(&self) -> bool {
        self.white_balance.is_none() && self.wb_mult == [1.0; 3] && self.wb_temp == 0.0 && self.wb_tint == 0.0
    }

    pub fn normalized_mix(&self) -> [f32; 3] {
        let s: f32 = self.mix.iter().sum();
        // A sum near 0 (e.g. old, contradictory slider values) would tip the image into black or white
        if s < 0.1 {
            [1.0 / 3.0; 3]
        } else {
            self.mix.map(|m| m / s)
        }
    }

    /// Key of the SPP base: colour mode and steps −1 ("Off") and 0…4 of the noise sliders. If it changes,
    /// the base must be recomputed (Sigma's noise reduction depends on both).
    pub fn spp_key(&self, path: &std::path::Path) -> SppKey {
        self.spp_key_class(crate::quattro::is_prepared_cached(path))
    }

    /// [`Self::spp_key`] for a known camera class (`prepared`: Quattro or Classic, otherwise Merrill).
    pub fn spp_key_class(&self, prepared: bool) -> SppKey {
        let mono = self.monochrome;
        let step = |v: f64| (v.clamp(NR_OFF, 1.0) * 4.0).round() as i8;
        // neural denoising after Sigma's noise reduction (every Foveon generation), colour only
        let ai = if mono || self.raw.ai_denoise <= 0.0 || !crate::ai_denoise::installed() { 0 } else { (self.raw.ai_denoise.clamp(0.0, 1.0) * 100.0).round() as u8 };
        let pct = |v: f32| (v.clamp(0.0, 2.0) * 100.0).round() as u8;
        SppKey { texture: [pct(self.detail_fine), pct(self.detail_structure)], mode: matrix_mode(self.color_mode.as_deref()).map(str::to_string), nr: (step(self.nr_luma), step(self.nr_chroma)), detail: self.quattro_detail.clamp(-QUATTRO_DETAIL_RANGE, QUATTRO_DETAIL_RANGE), ca: self.ca_key(), mono, mono_mix: if mono { crate::mono::key_mix(self.normalized_mix()) } else { [0; 3] }, prepared, binning: self.binning, ai, highlight: if crate::highlight_rebuild::enabled_for(prepared) && !mono { HighlightSliders::new(self.highlight_colour, self.highlight_structure) } else { HighlightSliders::default() } }
            .for_class(prepared)
    }

    /// CA correction for the base; none in monochrome (same as SPP).
    pub fn ca_key(&self) -> crate::lens::CaKey {
        use crate::lens::{CaKey, CaMode};
        let tenth = |v: f32, lo: f32, hi: f32| (v.clamp(lo, hi) * 10.0).round() as i8;
        if self.monochrome || self.ca_mode == CaMode::Off {
            return CaKey::default();
        }
        let manual = self.ca_mode == CaMode::Manual;
        CaKey {
            mode: self.ca_mode,
            profile: if manual { 0 } else { tenth(self.ca_profile, 0.0, 2.0) },
            red: if manual { tenth(self.ca_red, -2.0, 2.0) } else { 0 },
            blue: if manual { tenth(self.ca_blue, -2.0, 2.0) } else { 0 },
        }
    }

    /// Key of the noise reduction; if it changes, denoising must be redone.
    pub fn denoise_key(&self) -> Option<(u8, u8)> {
        self.denoise.then(|| ((self.nr_luma * 4.0).round() as u8, (self.nr_chroma * 4.0).round() as u8))
    }
}

#[cfg(test)]
mod ai_denoise_settings_tests {
    use super::*;

    /// Off is not written (existing photos stay byte-identical); a missing field loads as off; a value survives.
    #[test]
    fn ai_denoise_json() {
        let mut s = Settings::default();
        s.raw.nr_luma = 10.0;
        let t = serde_json::to_string(&s).unwrap();
        assert!(!t.contains("aiDenoise"), "{t}");
        assert_eq!(serde_json::from_str::<Settings>(&t).unwrap().raw.ai_denoise, 0.0);
        assert_eq!(serde_json::from_str::<Settings>(r#"{"raw":{"nr_luma":10.0}}"#).unwrap().raw.ai_denoise, 0.0);
        s.raw.ai_denoise = 0.8;
        let t = serde_json::to_string(&s).unwrap();
        assert!(t.contains("\"aiDenoise\":0.8"), "{t}");
        assert_eq!(serde_json::from_str::<Settings>(&t).unwrap().raw.ai_denoise, 0.8);
        assert!(!RawAdjust { ai_denoise: 0.5, ..Default::default() }.is_default());
    }
}

#[cfg(test)]
mod tone_eq_settings_tests {
    use super::*;

    /// Stored as `toneEqualizer` only when set; old JSON loads neutral; the mask switch is never stored.
    #[test]
    fn tone_eq_json() {
        let d = serde_json::to_string(&Settings::default()).unwrap();
        assert!(!d.contains("toneEqualizer"));
        let mut s = Settings::default();
        s.tone_eq.zones[2] = 0.5;
        s.tone_eq_mask = true;
        let t = serde_json::to_string(&s).unwrap();
        assert!(t.contains("\"toneEqualizer\":{\"amount\":0.0,\"zones\":[0.0,0.0,0.5,"), "{t}");
        let back: Settings = serde_json::from_str(&t).unwrap();
        assert_eq!(back.tone_eq, s.tone_eq);
        assert!(!back.tone_eq_mask);
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap().tone_eq, crate::tone_eq::ToneEq::default());
        // showing the mask switches the film off for the display
        let mut f = Settings { tone_eq_mask: true, ..Settings::default() };
        f.film.active = true;
        assert!(!f.effective().film.active);
    }
}

#[cfg(test)]
mod bypass_tests {
    use super::*;

    /// The "Colour casts" switch resets exactly its slider; old sidecars without the field load unchanged.
    #[test]
    fn casts_bypass() {
        let s = Settings { highlight_neutralize: 0.4, dehaze: 0.3, fill_light: 0.5, ..Settings::default() };
        assert!(matches!(s.effective(), std::borrow::Cow::Borrowed(_)));
        let off = Settings { bypass: Bypass { casts: true, ..Bypass::default() }, ..s.clone() };
        let e = off.effective();
        assert_eq!(e.highlight_neutralize, 0.0);
        assert_eq!((e.dehaze, e.fill_light), (0.3, 0.5));
        let basic = Settings { bypass: Bypass { basic: true, ..Bypass::default() }, ..s.clone() };
        assert_eq!(basic.effective().highlight_neutralize, 0.4);
        let old: Bypass = serde_json::from_str(r#"{"basic":true,"curve":false}"#).unwrap();
        assert_eq!(old, Bypass { basic: true, ..Bypass::default() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit round C: the base key follows monochrome (grey base), and every switch that changes the base
    /// changes the key.
    #[test]
    fn spp_key_follows_monochrome() {
        let colour = Settings::default();
        let mono = Settings { monochrome: true, ..Settings::default() };
        assert!(!colour.spp_key_class(false).mono);
        assert!(mono.spp_key_class(false).mono);
        assert_ne!(mono.spp_key_class(false), colour.spp_key_class(false));
        // settings of earlier versions with the field `methods` still load
        let old: Settings = serde_json::from_str(r#"{"methods":{"defects":"own"}}"#).unwrap();
        assert_eq!(old.spp_key_class(false), colour.spp_key_class(false));
        // Quattro/Classic: black and white and CA do not touch the base
        let q = Settings { ca_mode: crate::lens::CaMode::Manual, ca_red: 1.0, ..mono.clone() };
        assert_eq!(q.spp_key_class(true), colour.spp_key_class(true));
        assert_ne!(colour.spp_key_class(true), colour.spp_key_class(false), "keys of different classes never match");
        // monochrome drops CA from the key (as in SPP)
        let m2 = Settings { ca_mode: crate::lens::CaMode::Manual, ca_red: 1.0, ..mono.clone() };
        assert_eq!(m2.spp_key_class(false), mono.spp_key_class(false));
    }
}

#[cfg(test)]
mod binning_settings_tests {
    use super::*;

    #[test]
    fn binning_missing_means_auto_and_off_round_trips() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.binning, Binning::Auto);
        let off = Settings { binning: Binning::Off, ..Settings::default() };
        let text = serde_json::to_string(&off).unwrap();
        assert!(text.contains("\"binning\":\"off\""));
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap().binning, Binning::Off);
        assert!(!serde_json::to_string(&Settings::default()).unwrap().contains("binning"));
        // key: only the Quattro/Classic class carries it
        assert_eq!(off.spp_key_class(true).binning, Binning::Off);
        assert_eq!(off.spp_key_class(false).binning, Binning::Auto);
        assert_eq!(serde_json::from_str::<Settings>("{\"binning\":\"on\"}").unwrap().binning, Binning::On);
    }

    #[test]
    fn detail_step_is_stored_only_away_from_the_middle_and_keys_the_quattro_base() {
        assert!(!serde_json::to_string(&Settings::default()).unwrap().contains("quattro_detail"));
        let strong = Settings { quattro_detail: 2, ..Settings::default() };
        let text = serde_json::to_string(&strong).unwrap();
        assert!(text.contains("\"quattro_detail\":2"));
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap().quattro_detail, 2);
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap().quattro_detail, 0);
        assert_eq!(strong.spp_key_class(true).detail, 2);
        assert!(!strong.spp_key_class(true).neutral_noise());
        assert_eq!(strong.spp_key_class(false).detail, 0, "Merrill has no such step");
        assert_eq!(Settings { quattro_detail: 9, ..Settings::default() }.spp_key_class(true).detail, QUATTRO_DETAIL_RANGE);
    }
}

#[cfg(test)]
mod lens_profile_tests {
    use super::*;

    #[test]
    fn engine_is_written_only_when_set() {
        // existing photos stay byte-identical: no field while automatic
        let plain = serde_json::to_string(&Settings::default()).unwrap();
        assert!(!plain.contains("\"engine\""), "{plain}");
        let s = Settings { raw: RawAdjust { engine: Some("apple:8".into()), ..RawAdjust::default() }, ..Settings::default() };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"engine\":\"apple:8\""), "{j}");
        let back: Settings = serde_json::from_str(&j).unwrap();
        assert_eq!(back.raw.engine.as_deref(), Some("apple:8"));
        // a document without the field (older versions) reads as automatic
        let old: Settings = serde_json::from_str(&plain).unwrap();
        assert_eq!(old.raw.engine, None);
        assert_eq!(serde_json::to_string(&old).unwrap(), plain);
    }

    #[test]
    fn settings_json_unchanged_without_lens_profile() {
        assert!(!serde_json::to_string(&Settings::default()).unwrap().contains("lens_profile"));
        let s = Settings { raw: RawAdjust { lens_profile: Some("3fa9c0d1e2b4@2".into()), ..RawAdjust::default() }, ..Settings::default() };
        let t = serde_json::to_string(&s).unwrap();
        assert!(t.contains("\"lens_profile\":\"3fa9c0d1e2b4@2\""));
        assert_eq!(serde_json::from_str::<Settings>(&t).unwrap().raw.lens_profile.as_deref(), Some("3fa9c0d1e2b4@2"));
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap().raw.lens_profile, None);
    }
}

#[cfg(test)]
mod texture_key_tests {
    use super::*;

    #[test]
    fn detail_bands_reach_every_colour_base() {
        let d = Settings::default();
        assert_eq!(d.spp_key_class(false).texture, TEXTURE_NEUTRAL);
        let p = Settings { detail_fine: 0.3, detail_structure: 1.2, ..Settings::default() };
        assert_eq!(p.spp_key_class(false).texture, [30, 120]);
        assert_eq!(p.spp_key_class(true).texture, [30, 120]);
        assert_eq!(Settings { monochrome: true, ..p.clone() }.spp_key_class(true).texture, TEXTURE_NEUTRAL);
        assert_eq!(Settings { monochrome: true, ..p.clone() }.spp_key_class(false).texture, TEXTURE_NEUTRAL);
        // stored settings without the fields load as Sigma's detail; defaults are not written
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!((old.detail_fine, old.detail_structure), (1.0, 1.0));
        assert!(!serde_json::to_string(&d).unwrap().contains("detail_fine"));
    }
}
