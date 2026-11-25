//! Thin layer over **rawler** (dnglab, LGPL-2.1) for non-Sigma RAWs
//! (CR2, CR3, RAF, DNG, ARW …).
//!
//! Purpose of the layer: the rest of graineo only sees the types here ([`RawFile`], [`Info`],
//! [`Linear`]), never rawler directly. That way the decoder can be swapped later (rawler ↔ LightCraft
//! ↔ LibRaw) without touching the pipeline or the app. rawler is pinned exactly (`=0.8.0`) because
//! the author says the API is not stable yet.
//!
//! Simple path (`linear()`, `camera_to_srgb()`, `tone_curve_srgb8()`): deliberately simple test development
//! with averaging demosaic. Full path ([`RawFile::develop`], module [`development`]):
//! RCD/Markesteijn, highlight reconstruction, DNG color, sigmoid,
//! capture sharpening. Ported GPL-3 parts (RawTherapee, darktable) are credited in the
//! header of the respective module.

/// Neural denoising (RawNIND UtNet2, linear variant) with an own executor
pub mod ai_denoise;
pub mod picture_style;
pub mod focus;
pub mod development;
pub mod dng_map;
pub mod color;
pub mod film_profile;
pub mod fuji;
pub mod looks;
pub mod learn_look;
pub mod highlights;
pub mod markesteijn;
pub mod lcp;
pub mod dcp;
pub mod lensfun;
pub mod lens;
pub mod noise;
mod rcd;
pub mod sharpness;
pub mod sharpen_study;
pub mod tone;
/// System RAW engine of macOS (Core Image) for comparisons; stand-in without function elsewhere
pub mod apple;
pub mod measure;
pub mod measure_run;

use std::path::Path;

use rawler::decoders::{Decoder, RawDecodeParams, WellKnownIFD};
use rawler::imgop::develop::{ProcessingStep, RawDevelop};
use rawler::imgop::sensor::SensorType;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::{RawImage, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use rawler::tags::{DngTag, TiffCommonTag};
use rayon::prelude::*;

pub use image::RgbImage;

/// Errors of this layer: always with plain text; rawler errors are passed through and
/// marked as such, so it is clear whether the decoder or our layer fails.
#[derive(Debug)]
pub enum Error {
    /// rawler could not read/decode the file.
    Rawler(String),
    /// our layer does not support this case (yet).
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Rawler(s) => write!(f, "rawler: {s}"),
            Error::Unsupported(s) => write!(f, "nicht unterstützt: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn rl<E: std::fmt::Display>(e: E) -> Error {
    classify(e.to_string())
}

/// Text of the error for Nikon's "High Efficiency" NEF compression (Z 8, Z 9, Z f, Z 6III …), which rawler does
/// not decode yet. A known gap, not a damaged file: says so instead of rawler's "possibly corrupt image". English
/// (developer text); the app shows its own translated message for it.
pub const NIKON_HE: &str = "Nikon High Efficiency (HE/HE*): this NEF compression is not decoded yet";

/// Text of the error for sensor data the decoder returned but that cannot be right: most values far above the white
/// level (seen with the 3-channel lossless-JPEG DNG of the Galaxy S23 Ultra, which came out as stripes). The app opens
/// such files with the system engine instead, as for [`NIKON_HE`].
pub const DECODE_IMPLAUSIBLE: &str = "decoded sensor data is implausible (values far above the white level)";

/// Start of the error text for a camera the decoder does not know (yet): followed by ": <make> <model>". English
/// (developer text); the app finds it with [`unknown_camera`] and shows its own translated message.
pub const UNKNOWN_CAMERA: &str = "camera not supported by the decoder yet";

/// rawler error → our error; known unsupported cases become [`Error::Unsupported`] with a plain text.
fn classify(s: String) -> Error {
    if is_nikon_he_refusal(&s) {
        return Error::Unsupported(NIKON_HE.into());
    }
    if let Some(camera) = unknown_camera_refusal(&s) {
        return Error::Unsupported(format!("{UNKNOWN_CAMERA}: {camera}"));
    }
    if looks_damaged(&s) {
        return Error::Rawler(format!("{DAMAGED}: {s}"));
    }
    Error::Rawler(s)
}

/// Start of the error text for a file whose structure the decoder cannot follow although its format is one it reads:
/// cut short (aborted copy, card error) or otherwise damaged. Followed by ": <rawler's text>".
pub const DAMAGED: &str = "file damaged or incomplete";

/// rawler's texts for a file it cannot follow: no decoder recognises the (cut) structure, "possibly corrupt image",
/// or reads past the end of the data.
fn looks_damaged(s: &str) -> bool {
    ["No decoder found", "possibly corrupt image", "failed to fill whole buffer", "unexpected end of file", "out of bounds"].iter().any(|t| s.contains(t))
}

/// Camera name from rawler's refusal of a camera missing in its list. rawler 0.8 says
/// "Error: Unknown camera, model 'X', make: 'Y', mode: 'Z'"; the name is "Y X" (model alone when it already starts
/// with the make). Checked again on every rawler update (test `rawler_version_is_the_one_checked`).
fn unknown_camera_refusal(s: &str) -> Option<String> {
    if !s.contains("Unknown camera") {
        return None;
    }
    let quoted = |key: &str| -> Option<String> {
        let rest = &s[s.find(key)? + key.len()..];
        let rest = rest.trim_start_matches([':', ' ']).strip_prefix('\'')?;
        Some(rest[..rest.find('\'')?].trim().to_string())
    };
    let model = quoted("model").unwrap_or_default();
    let make = quoted("make").unwrap_or_default();
    let name = if model.is_empty() || make.is_empty() || !model.to_lowercase().starts_with(&make.to_lowercase()) { format!("{make} {model}") } else { model.clone() };
    let name = name.trim().to_string();
    Some(if name.is_empty() { "?".into() } else { name })
}

/// The camera of an error text of this layer when it says the decoder does not know the camera ([`UNKNOWN_CAMERA`]),
/// e.g. "Sony ILCE-7Z9"; `None` for every other error (damaged file, unsupported compression …).
pub fn unknown_camera(error: &str) -> Option<String> {
    let rest = &error[error.find(UNKNOWN_CAMERA)? + UNKNOWN_CAMERA.len()..];
    Some(rest.strip_prefix(": ").unwrap_or(rest).trim().to_string()).filter(|s| !s.is_empty())
}

/// Does a rawler error refuse Nikon's High Efficiency compression? rawler 0.8 says "NEF compression
/// Some(HighEfficency) is not supported" (its enum name has a typo; "HighEfficencyStar" for HE*). Matched on letters
/// and digits only, case-insensitively, and on the common prefix "higheffic", so the correct spelling, spaces or
/// another wrapping of the enum name are recognised as well. The test `rawler_version_is_the_one_checked` breaks on
/// every rawler update, so the text is checked again then.
fn is_nikon_he_refusal(s: &str) -> bool {
    let flat: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect();
    flat.contains("nefcompression") && flat.contains("higheffic")
}

/// More than a quarter of the values (sampled) above twice the white level: the decoder misread the data. Real
/// files clip at the white level; a few hot values or a linear DNG with headroom stay far below this share.
fn implausible(data: &[f32], white: &[f32]) -> bool {
    let w = white.iter().copied().fold(0.0f32, f32::max);
    if data.is_empty() || w <= 0.0 {
        return false;
    }
    let step = (data.len() / 100_000).max(1);
    let (n, over) = data.iter().step_by(step).fold((0usize, 0usize), |(n, o), &v| (n + 1, o + usize::from(v > 2.0 * w)));
    over * 4 > n
}

/// DNG OpcodeList2 value maps (MapTable, MapPolynomial; e.g. the curve of a lossy DNG), which the decoder does not
/// run: applied to the raw values with the black and white levels of [`RawFile::linear`], see [`dng_map`].
fn apply_value_maps(dec: &dyn Decoder, image: &mut RawImage, info: &Info) {
    let Some(ifd) = dec.ifd(WellKnownIFD::VirtualDngRawTags).ok().flatten() else { return };
    let Some(entry) = ifd.get_entry(DngTag::OpcodeList2) else { return };
    let bytes = match &entry.value {
        rawler::formats::tiff::Value::Undefined(b) | rawler::formats::tiff::Value::Byte(b) => b,
        _ => return,
    };
    let maps = dng_map::parse(bytes);
    if maps.is_empty() {
        return;
    }
    let (w, h, cpp) = (info.width, info.height, info.channels.max(1));
    let mut data = image.data.as_f32().into_owned();
    if data.len() != w * h * cpp {
        return;
    }
    let (bw, bh, bc) = info.black_pattern;
    let black = |x: usize, y: usize, c: usize| -> f32 {
        if info.black.is_empty() {
            return 0.0;
        }
        let idx = ((y % bh.max(1)) * bw.max(1) + x % bw.max(1)) * bc.max(1) + c.min(bc.max(1) - 1);
        info.black.get(idx).copied().unwrap_or(info.black[0])
    };
    let white = |c: usize| -> f32 { info.white.get(c).or(info.white.first()).copied().unwrap_or(65535.0) };
    dng_map::apply(&maps, &mut data, w, h, cpp, &black, &white);
    image.data = rawler::rawimage::RawImageData::Float(data);
}

/// Can our decoder read this file? Reads the header only (no pixel data); the error says why not (e.g. [`NIKON_HE`]).
pub fn check_decodable(path: &Path) -> Result<()> {
    let source = RawSource::new(path).map_err(|e| Error::Rawler(format!("{}: {e}", path.display())))?;
    let decoder = rawler::get_decoder(&source).map_err(rl)?;
    let params = RawDecodeParams::default();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decoder.raw_image(&source, &params, true))).map_err(|_| Error::Rawler("panic in the decoder".into()))?.map_err(rl)?;
    Ok(())
}

/// Kind of sensor data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensor {
    /// 2×2 color filter (RGGB etc.)
    Bayer,
    /// Fujifilm 6×6
    XTrans,
    /// other mosaic (e.g. CYGM) – not developed here
    OtherMosaic,
    /// already demosaiced / linear (DNG LinearRaw, Apple ProRAW, Canon sRAW)
    Linear,
    /// single channel without color filter
    Mono,
}

/// Rectangle in pixels of the full sensor image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Everything we need from a RAW file for development (without pixels).
#[derive(Debug, Clone)]
pub struct Info {
    /// container format according to rawler (CR2, CR3, RAF, DNG, ARW …)
    pub format: String,
    pub maker: String,
    pub model: String,
    /// rawler camera mode, e.g. "craw"/"raw" (CR3), "sRAW1" (CR2), "compressed" (RAF)
    pub mode: String,
    pub width: usize,
    pub height: usize,
    /// channels per pixel (1 = mosaic/mono, 3 = linear RGB)
    pub channels: usize,
    pub bits: usize,
    pub sensor: Sensor,
    /// color filter pattern as text (e.g. "RGGB" or 36 characters for X-Trans)
    pub cfa: String,
    pub cfa_size: (usize, usize),
    /// black level per pattern position/channel (raw values) and pattern size (w, h, channels)
    pub black: Vec<f32>,
    pub black_pattern: (usize, usize, usize),
    /// white level per channel (raw values)
    pub white: Vec<f32>,
    /// as-shot white balance (multipliers, normalized to green = 1); None = not in the file
    pub wb: Option<[f32; 3]>,
    /// camera matrix XYZ → camera (row by row, 3×3) and its illuminant
    pub matrix: Option<(String, [[f32; 3]; 3])>,
    /// all available illuminants of the matrices
    pub matrix_illuminants: Vec<String>,
    /// EXIF orientation 1…8 (0 = unknown)
    pub orientation: u16,
    pub active: Option<Region>,
    pub crop: Option<Region>,
    /// Fuji SuperCCD (sensor rotated by 45°) – not supported here
    pub fuji_rotated: bool,
    /// DNG: BaselineExposure in EV (otherwise 0)
    pub baseline_exposure: f32,
    /// DNG: profile tags present (ForwardMatrix, HueSatMap, LookTable, ProfileToneCurve …)
    pub dng_profile: Vec<&'static str>,
    pub exif: Exif,
    /// corrections of our layer to rawler data (see `make_info`)
    pub corrections: Vec<&'static str>,
    /// camera matrices per illuminant (+ ForwardMatrix for DNG) for color according to the DNG specification
    pub calibration: Vec<color::Calibration>,
    /// DNG AnalogBalance
    pub analog_balance: Option<[f32; 3]>,
    /// Fujifilm: effective dynamic range (100/200/400) from the RAF makernote
    pub fuji_dr: Option<u16>,
}

/// Shooting data (subset).
#[derive(Debug, Clone, Default)]
pub struct Exif {
    pub iso: Option<u32>,
    /// exposure time in seconds
    pub exposure_time: Option<f32>,
    pub aperture: Option<f32>,
    pub focal_length: Option<f32>,
    /// distance (EXIF SubjectDistance) in meters, if given
    pub distance: Option<f32>,
    pub lens_maker: Option<String>,
    pub lens: Option<String>,
    /// lens according to the rawler lens database (if recognized)
    pub lens_db: Option<String>,
    pub capture_date: Option<String>,
    pub serial_number: Option<String>,
}

/// An opened RAW file.
pub struct RawFile {
    pub info: Info,
    source: RawSource,
    decoder: Box<dyn Decoder>,
    image: RawImage,
}

/// Linear camera RGB after black/white level and demosaicing, cropped,
/// 0…1 relative to the white level (values > 1 possible with linear DNG), still without white balance.
pub struct Linear {
    pub width: usize,
    pub height: usize,
    pub px: Vec<[f32; 3]>,
}

fn illu_name(i: &Illuminant) -> String {
    format!("{i:?}")
}

impl RawFile {
    /// Open the file and decode the sensor data (the file is only read, via mmap).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let source = RawSource::new(path).map_err(|e| Error::Rawler(format!("{}: {e}", path.display())))?;
        let decoder = rawler::get_decoder(&source).map_err(rl)?;
        let params = RawDecodeParams::default();
        // rawler catches panics in `decode`, but not in the decoder directly → catch them ourselves
        let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decoder.raw_image(&source, &params, false)))
            .map_err(|_| Error::Rawler("panic in the decoder".into()))?
            .map_err(rl)?;
        let meta = decoder.raw_metadata(&source, &params).ok();
        let mut info = make_info(&*decoder, &image, meta.as_ref());
        // integer data only: floating-point DNGs (HDR merges) may hold values above the white level on purpose
        if matches!(image.data, rawler::rawimage::RawImageData::Integer(_)) && implausible(&image.data.as_f32(), &info.white) {
            return Err(Error::Unsupported(DECODE_IMPLAUSIBLE.into()));
        }
        let mut image = image;
        apply_value_maps(&*decoder, &mut image, &info);
        if info.format == "RAF" {
            info.fuji_dr = fuji::dynamic_range(path);
        }
        Ok(Self { info, source, decoder, image })
    }

    /// Embedded camera preview (largest available: full JPEG → preview → thumbnail),
    /// in sensor orientation (EXIF rotation not applied).
    pub fn preview(&self) -> Option<RgbImage> {
        let p = RawDecodeParams::default();
        let tries: [&dyn Fn() -> rawler::Result<Option<image::DynamicImage>>; 3] = [
            &|| self.decoder.full_image(&self.source, &p),
            &|| self.decoder.preview_image(&self.source, &p),
            &|| self.decoder.thumbnail_image(&self.source, &p),
        ];
        for t in tries {
            if let Ok(Ok(Some(img))) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(t)) {
                return Some(img.to_rgb8());
            }
        }
        None
    }

    /// Optically black (masked) border areas of the sensor according to rawler, in coordinates of the
    /// full sensor image; empty if rawler knows none.
    pub fn black_margins(&self) -> Vec<Region> {
        self.image.blackareas.iter().map(|r| Region { x: r.p.x, y: r.p.y, w: r.d.w, h: r.d.h }).collect()
    }

    /// Raw data as f32 (unchanged, raw values), width × height × channels.
    pub fn raw_data(&self) -> std::borrow::Cow<'_, Vec<f32>> {
        self.image.data.as_f32()
    }

    /// Our layer: black/white level, simple demosaic, crop → linear camera RGB.
    pub fn linear(&self) -> Result<Linear> {
        let i = &self.info;
        if i.fuji_rotated {
            return Err(Error::Unsupported("Fuji SuperCCD (gedrehter Sensor)".into()));
        }
        let (w, h) = (i.width, i.height);
        let data = self.image.data.as_f32();
        let (bw, bh, bc) = i.black_pattern;
        let black = |x: usize, y: usize, c: usize| -> f32 {
            if i.black.is_empty() {
                return 0.0;
            }
            let idx = ((y % bh.max(1)) * bw.max(1) + x % bw.max(1)) * bc.max(1) + c.min(bc.max(1) - 1);
            i.black.get(idx).copied().unwrap_or(i.black[0])
        };
        let white = |c: usize| -> f32 { i.white.get(c).or(i.white.first()).copied().unwrap_or(65535.0) };
        let full: Vec<[f32; 3]> = match i.sensor {
            Sensor::Linear => {
                let mut out = vec![[0f32; 3]; w * h];
                out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    for (x, o) in row.iter_mut().enumerate() {
                        let s = &data[(y * w + x) * 3..][..3];
                        for c in 0..3 {
                            let b = black(x, y, c);
                            o[c] = (s[c] - b) / (white(c) - b);
                        }
                    }
                });
                out
            }
            Sensor::Mono => {
                let mut out = vec![[0f32; 3]; w * h];
                out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    for (x, o) in row.iter_mut().enumerate() {
                        let b = black(x, y, 0);
                        let v = (data[y * w + x] - b) / (white(0) - b);
                        *o = [v; 3];
                    }
                });
                out
            }
            Sensor::Bayer | Sensor::XTrans => {
                let RawPhotometricInterpretation::Cfa(cfg) = &self.image.photometric else {
                    return Err(Error::Unsupported("Mosaik ohne CFA-Angabe".into()));
                };
                let cfa = &cfg.cfa;
                // normalized mosaic values (black level per pattern position)
                let mut norm = vec![0f32; w * h];
                norm.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    for (x, o) in row.iter_mut().enumerate() {
                        let b = black(x, y, 0);
                        *o = (data[y * w + x] - b) / (white(0) - b);
                    }
                });
                demosaic_mean(&norm, w, h, |x, y| cfa.color_at(y, x))?
            }
            Sensor::OtherMosaic => return Err(Error::Unsupported(format!("Farbfilter {}", i.cfa))),
        };
        // crop: DefaultCrop or active area (coordinates in the full image)
        let r = i.crop.or(i.active).unwrap_or(Region { x: 0, y: 0, w, h });
        let r = Region { x: r.x.min(w), y: r.y.min(h), w: r.w.min(w - r.x.min(w)), h: r.h.min(h - r.y.min(h)) };
        let mut px = Vec::with_capacity(r.w * r.h);
        for y in r.y..r.y + r.h {
            px.extend_from_slice(&full[y * w + r.x..y * w + r.x + r.w]);
        }
        Ok(Linear { width: r.w, height: r.h, px })
    }

    /// Comparison path entirely via rawler (its PPG/X-Trans bilinear, white balance, calibration,
    /// crop), without gamma: linear sRGB. Only for debugging "rawler or us?".
    pub fn rawler_linear_srgb(&self) -> Result<Linear> {
        let dev = RawDevelop::new_with(&[
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::FujiRotate,
            ProcessingStep::CropActiveArea,
            ProcessingStep::WhiteBalance,
            ProcessingStep::Calibrate,
            ProcessingStep::CropDefault,
        ]);
        let im = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.develop_intermediate(&self.image)))
            .map_err(|_| Error::Rawler("Panic in RawDevelop".into()))?
            .map_err(rl)?;
        match im {
            rawler::imgop::develop::Intermediate::ThreeColor(p) => {
                let d = p.dim();
                Ok(Linear { width: d.w, height: d.h, px: p.data })
            }
            rawler::imgop::develop::Intermediate::Monochrome(p) => {
                let d = p.dim();
                Ok(Linear { width: d.w, height: d.h, px: p.data.iter().map(|&v| [v; 3]).collect() })
            }
            _ => Err(Error::Unsupported("rawler: Vierfarb-Ergebnis".into())),
        }
    }

    /// Matrix camera → linear sRGB (D65) the dcraw way: rows of (XYZ→camera · sRGB→XYZ)
    /// normalized to sum 1, inverted. With white balance normalized to green = 1 it maps
    /// the as-shot neutral to (1,1,1).
    pub fn camera_to_srgb(&self) -> Result<[[f32; 3]; 3]> {
        let (_, xyz2cam) = self.info.matrix.clone().ok_or_else(|| Error::Unsupported("keine Kameramatrix".into()))?;
        let mut rgb2cam = mul3(&xyz2cam, &SRGB_TO_XYZ);
        for row in rgb2cam.iter_mut() {
            let s: f32 = row.iter().sum();
            if s.abs() > 1e-6 {
                row.iter_mut().for_each(|v| *v /= s);
            }
        }
        inv3(&rgb2cam).ok_or_else(|| Error::Unsupported("Kameramatrix nicht invertierbar".into()))
    }
}

/// Linear camera RGB → linear sRGB with as-shot white balance. Camera values are clipped at 1
/// after white balance (highlights stay neutral, like dcraw `-H 0`).
pub fn camera_to_srgb(lin: &Linear, wb: [f32; 3], m: &[[f32; 3]; 3], limit: bool) -> Linear {
    let px = lin
        .px
        .par_iter()
        .map(|p| {
            let mut c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
            if limit {
                c.iter_mut().for_each(|v| *v = v.min(1.0));
            }
            [
                m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2],
                m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2],
                m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2],
            ]
        })
        .collect();
    Linear { width: lin.width, height: lin.height, px }
}

/// Simple tone curve: exposure `ev` (base value +1 EV, plus DNG BaselineExposure), then
/// "extended Reinhard" per channel with white = gain (clip stays at 1), then sRGB gamma.
pub fn tone_curve_srgb8(lin: &Linear, ev: f32) -> RgbImage {
    let g = 2f32.powf(ev);
    let wsq = g * g;
    let mut lut = vec![0u8; 4097];
    for (i, o) in lut.iter_mut().enumerate() {
        let x = i as f32 / 4096.0 * g;
        let y = (x * (1.0 + x / wsq) / (1.0 + x)).clamp(0.0, 1.0);
        *o = (srgb_oetf(y) * 255.0 + 0.5) as u8;
    }
    let mut out = vec![0u8; lin.px.len() * 3];
    out.par_chunks_mut(3).zip(lin.px.par_iter()).for_each(|(o, p)| {
        for c in 0..3 {
            o[c] = lut[(p[c].clamp(0.0, 1.0) * 4096.0) as usize];
        }
    });
    RgbImage::from_raw(lin.width as u32, lin.height as u32, out).expect("buffer size")
}

/// Base exposure of the tone curve (EV), so that 18 % gray lands roughly as in the camera JPEG.
pub const BASE_EV: f32 = 1.0;

pub fn srgb_oetf(v: f32) -> f32 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Camera name for display: manufacturer once, even if the model already starts with it
/// ("SIGMA" + "SIGMA fp" → "Sigma fp", "Sigma" + "Sigma BF" → "Sigma BF", "Canon" + "Canon EOS RP" →
/// "Canon EOS RP", "SONY" + "ILCE-7M3" → "SONY ILCE-7M3").
pub fn camera_display(maker: &str, model: &str) -> String {
    let (maker, model) = (maker.trim(), model.trim());
    let first = maker.split_whitespace().next().unwrap_or("");
    let rest = |m: &str| -> Option<String> {
        let w = m.split_whitespace().next()?;
        (!first.is_empty() && w.eq_ignore_ascii_case(first)).then(|| m[w.len()..].trim_start().to_string())
    };
    let name = match rest(model) {
        Some(r) if !r.is_empty() => {
            // the model carries the manufacturer: keep its spelling, only "SIGMA" is shown as "Sigma"
            let w = &model[..model.len() - r.len()].trim_end().to_string();
            let w = if w == "SIGMA" { "Sigma" } else { w.as_str() };
            format!("{w} {r}")
        }
        Some(_) => model.to_string(),
        _ if maker.is_empty() => model.to_string(),
        _ if model.is_empty() => maker.to_string(),
        _ => format!("{maker} {model}"),
    };
    name
}

/// Apply EXIF orientation (1…8) to a finished image.
pub fn orient(img: RgbImage, o: u16) -> RgbImage {
    use image::imageops::*;
    match o {
        2 => flip_horizontal(&img),
        3 => rotate180(&img),
        4 => flip_vertical(&img),
        5 => flip_horizontal(&rotate90(&img)),
        6 => rotate90(&img),
        7 => flip_horizontal(&rotate270(&img)),
        8 => rotate270(&img),
        _ => img,
    }
}

const SRGB_TO_XYZ: [[f32; 3]; 3] = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192, 0.950_304_1],
];

fn mul3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

fn inv3(m: &[[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let m: [[f64; 3]; 3] = m.map(|r| r.map(|v| v as f64));
    // adjugate: inv[j][i] = cofactor(i, j) / det
    let cof = |i: usize, j: usize| {
        let (r0, r1) = ((i + 1) % 3, (i + 2) % 3);
        let (c0, c1) = ((j + 1) % 3, (j + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let det: f64 = (0..3).map(|j| m[0][j] * cof(0, j)).sum();
    if det.abs() < 1e-12 {
        return None;
    }
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[j][i] = (cof(i, j) / det) as f32;
        }
    }
    Some(r)
}

/// Generic demosaic: missing colors = mean of all pixels of that color in the
/// 3×3 neighborhood (if a color is missing there, 5×5). For Bayer this is exactly bilinear.
/// The neighborhood lists are precomputed per pattern position (pattern ≤ 6×6).
fn demosaic_mean(norm: &[f32], w: usize, h: usize, color_at: impl Fn(usize, usize) -> usize + Sync) -> Result<Vec<[f32; 3]>> {
    // determine the pattern period (2 or 6), sufficient for Bayer and X-Trans
    let per = if (0..12).all(|y| (0..12).all(|x| color_at(x, y) == color_at(x % 2, y % 2))) { 2 } else { 6 };
    let mut out = vec![[0f32; 3]; w * h];
    demosaic_mean_in(norm, w, h, per, &color_at, &mut out, None)?;
    Ok(out)
}

/// Neighbour offsets of the mean demosaic per pattern position and colour: the same colour in 3×3, or in 5×5 if 3×3
/// has none (index `(y % per) * per + x % per`).
fn mean_offsets(per: usize, color_at: &impl Fn(usize, usize) -> usize) -> Result<Vec<[Vec<(isize, isize)>; 3]>> {
    let mut offs: Vec<[Vec<(isize, isize)>; 3]> = Vec::with_capacity(per * per);
    for py in 0..per {
        for px in 0..per {
            let mut o: [Vec<(isize, isize)>; 3] = Default::default();
            for c in 0..3 {
                for r in [1isize, 2] {
                    o[c].clear();
                    for dy in -r..=r {
                        for dx in -r..=r {
                            // pattern position relative (keep positive: +per*4)
                            let xx = (px as isize + dx + per as isize * 4) as usize;
                            let yy = (py as isize + dy + per as isize * 4) as usize;
                            if color_at(xx % per, yy % per) == c {
                                o[c].push((dx, dy));
                            }
                        }
                    }
                    if !o[c].is_empty() {
                        break;
                    }
                }
                if o[c].is_empty() {
                    return Err(Error::Unsupported("Farbe fehlt im Muster".into()));
                }
            }
            offs.push(o);
        }
    }
    Ok(offs)
}

/// Like [`demosaic_mean`], writes into `out`; with `border = Some(b)` only pixels at distance < b
/// from the image edge (border fill for RCD).
pub(crate) fn demosaic_mean_in(
    norm: &[f32],
    w: usize,
    h: usize,
    per: usize,
    color_at: &(impl Fn(usize, usize) -> usize + Sync),
    out: &mut [[f32; 3]],
    border: Option<usize>,
) -> Result<()> {
    let offs = mean_offsets(per, color_at)?;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            if let Some(b) = border {
                if y >= b && y + b < h && x >= b && x + b < w {
                    continue;
                }
            }
            let own = color_at(x % per, y % per);
            let lst = &offs[(y % per) * per + x % per];
            let inner = x >= 2 && y >= 2 && x + 2 < w && y + 2 < h;
            for c in 0..3 {
                if c == own {
                    o[c] = norm[y * w + x];
                    continue;
                }
                let (mut s, mut n) = (0f32, 0f32);
                for &(dx, dy) in &lst[c] {
                    let (xx, yy) = if inner {
                        ((x as isize + dx) as usize, (y as isize + dy) as usize)
                    } else {
                        // border: mirroring by two pattern periods would change the color → only take valid ones
                        let xx = x as isize + dx;
                        let yy = y as isize + dy;
                        if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                            continue;
                        }
                        (xx as usize, yy as usize)
                    };
                    s += norm[yy * w + xx];
                    n += 1.0;
                }
                o[c] = if n > 0.0 { s / n } else { 0.0 };
            }
        }
    });
    Ok(())
}

/// Adds the part of the mosaic below 0 to a demosaic result, interpolated with the mean demosaic (bilinear for
/// Bayer).
///
/// RCD and Markesteijn work on the mosaic clipped at 0 (their ratios and colour-space steps need non-negative input).
/// Around black, half of the read noise is negative: clipping it lifts the mean of the deepest shadows by about
/// 0.3 σ, more in red and blue (fewer photosites, larger white-balance gain), and an HDR merge multiplies that lift
/// by the exposure factor of the dark frames. Adding the clipped-off negative part back as a linear interpolation keeps
/// the local mean of the mosaic: the own colour gets its exact value again, and wherever the mosaic is ≥ 0 nothing
/// changes. The result may be negative; the development clips only at the very end.
pub(crate) fn add_negative_part(m: &[f32], w: usize, h: usize, per: usize, color_at: &(impl Fn(usize, usize) -> usize + Sync), out: &mut [[f32; 3]]) {
    if w == 0 || h == 0 || m.len() < w * h || out.len() < w * h {
        return;
    }
    // columns with a negative value per row (sparse: only a few percent of the photosites, all in the shadows)
    let neg_cols: Vec<Vec<u32>> = m[..w * h].par_chunks(w).map(|r| r.iter().enumerate().filter(|(_, v)| **v < 0.0).map(|(x, _)| x as u32).collect()).collect();
    if neg_cols.iter().all(|c| c.is_empty()) {
        return;
    }
    let Ok(offs) = mean_offsets(per, color_at) else { return };
    out[..w * h].par_chunks_mut(w).enumerate().for_each_init(Vec::new, |touched: &mut Vec<bool>, (y, row)| {
        // pixels within 2 of a negative photosite (the reach of the mean demosaic)
        touched.clear();
        touched.resize(w, false);
        let mut any = false;
        for cols in &neg_cols[y.saturating_sub(2)..(y + 3).min(h)] {
            for &c in cols {
                let c = c as usize;
                touched[c.saturating_sub(2)..(c + 3).min(w)].iter_mut().for_each(|t| *t = true);
                any = true;
            }
        }
        if !any {
            return;
        }
        let neg = |i: usize| m[i].min(0.0);
        for (x, o) in row.iter_mut().enumerate() {
            if !touched[x] {
                continue;
            }
            let own = color_at(x % per, y % per);
            let lst = &offs[(y % per) * per + x % per];
            let inner = x >= 2 && y >= 2 && x + 2 < w && y + 2 < h;
            for c in 0..3 {
                if c == own {
                    o[c] += neg(y * w + x);
                    continue;
                }
                let (mut s, mut n) = (0f32, 0f32);
                for &(dx, dy) in &lst[c] {
                    let (xx, yy) = (x as isize + dx, y as isize + dy);
                    if !inner && (xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize) {
                        continue;
                    }
                    s += neg(yy as usize * w + xx as usize);
                    n += 1.0;
                }
                if s < 0.0 {
                    o[c] += s / n;
                }
            }
        }
    });
}

fn make_info(dec: &dyn Decoder, img: &RawImage, meta: Option<&rawler::decoders::RawMetadata>) -> Info {
    let (sensor, cfa, cfa_size) = match &img.photometric {
        RawPhotometricInterpretation::Cfa(cfg) => {
            let s = if !cfg.cfa.is_rgb() {
                Sensor::OtherMosaic
            } else if cfg.sensor == SensorType::Xtrans {
                Sensor::XTrans
            } else {
                Sensor::Bayer
            };
            (s, cfg.cfa.name.clone(), (cfg.cfa.width, cfg.cfa.height))
        }
        RawPhotometricInterpretation::LinearRaw => (if img.cpp == 1 { Sensor::Mono } else { Sensor::Linear }, String::new(), (0, 0)),
        RawPhotometricInterpretation::BlackIsZero => (Sensor::Mono, String::new(), (0, 0)),
    };
    let (sensor, cfa) = if img.cpp == 3 && sensor != Sensor::Linear { (Sensor::Linear, String::new()) } else { (sensor, cfa) };
    let wb = (!img.wb_coeffs[0].is_nan() && img.wb_coeffs[1] > 0.0)
        .then(|| [img.wb_coeffs[0] / img.wb_coeffs[1], 1.0, img.wb_coeffs[2] / img.wb_coeffs[1]]);
    let order = [
        Illuminant::D65,
        Illuminant::D55,
        Illuminant::D50,
        Illuminant::D75,
        Illuminant::Daylight,
        Illuminant::FineWeather,
        Illuminant::Flash,
        Illuminant::C,
        Illuminant::B,
        Illuminant::A,
        Illuminant::Tungsten,
    ];
    let matrix = img
        .color_matrix_find_first(order)
        .or_else(|| img.color_matrix.iter().next().map(|(k, v)| (*k, v.clone())))
        .filter(|(_, m)| m.len() >= 9)
        .map(|(il, m)| (illu_name(&il), [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]]));
    let mut matrix_illuminants: Vec<String> = img.color_matrix.keys().map(illu_name).collect();
    matrix_illuminants.sort();
    let rect = |r: Option<rawler::imgop::Rect>| r.map(|r| Region { x: r.p.x, y: r.p.y, w: r.d.w, h: r.d.h });
    let root = dec.ifd(WellKnownIFD::VirtualDngRootTags).ok().flatten();
    let raw_ifd = dec.ifd(WellKnownIFD::VirtualDngRawTags).ok().flatten();
    let baseline_exposure = root
        .as_ref()
        .and_then(|i| i.get_entry(DngTag::BaselineExposure))
        .map(|e| e.value.force_f32(0))
        .unwrap_or(0.0);
    let mut dng_profile = Vec::new();
    for (tag, name) in [
        (DngTag::ProfileHueSatMapData1, "HueSatMap"),
        (DngTag::ProfileLookTableData, "LookTable"),
        (DngTag::ProfileToneCurve, "ProfileToneCurve"),
        (DngTag::ProfileName, "ProfileName"),
    ] {
        if root.as_ref().is_some_and(|i| i.get_entry(tag).is_some()) {
            dng_profile.push(name);
        }
    }
    for (tag, name) in [(DngTag::ForwardMatrix1, "ForwardMatrix1"), (DngTag::ForwardMatrix2, "ForwardMatrix2")] {
        if raw_ifd.as_ref().is_some_and(|i| i.get_entry(tag).is_some()) {
            dng_profile.push(name);
        }
    }
    let r2f = |r: &rawler::formats::tiff::Rational| if r.d == 0 { None } else { Some(r.n as f32 / r.d as f32) };
    let exif = meta
        .map(|m| {
            let e = &m.exif;
            Exif {
                iso: e.iso_speed_ratings.map(u32::from).or(e.iso_speed).or(e.recommended_exposure_index),
                exposure_time: e.exposure_time.as_ref().and_then(r2f),
                aperture: e.fnumber.as_ref().and_then(r2f),
                focal_length: e.focal_length.as_ref().and_then(r2f),
                distance: e.subject_distance.as_ref().and_then(r2f).filter(|d| *d > 0.0),
                lens_maker: e.lens_make.clone(),
                lens: e.lens_model.clone(),
                lens_db: m.lens.as_ref().map(|l| format!("{} {}", l.lens_make, l.lens_model)),
                capture_date: e.date_time_original.clone(),
                serial_number: e.serial_number.clone(),
            }
        })
        .unwrap_or_default();
    // rawler sets `orientation` only for DNG; otherwise it is in the EXIF (IFD0 or CR3 CMT1)
    let orientation = meta
        .and_then(|m| m.exif.orientation)
        .filter(|o| (1..=8).contains(o))
        .unwrap_or_else(|| img.orientation.to_u16());
    let format = format!("{:?}", dec.format_hint());
    let mut black = img.blacklevel.as_vec();
    let mut corrections = Vec::new();
    // rawler 0.8.0: Canon sRAW/mRAW (CR2, 3 channels) gets the black level of the mosaic data
    // (e.g. 1023), but the YCbCr→RGB conversion already delivers black-subtracted values
    // (median of the raw values ~500 < 1023). With 0 the result matches the camera preview
    // (5D Mark II: ΔE76 18 → 6).
    if format == "CR2" && img.cpp == 3 {
        black.iter_mut().for_each(|v| *v = 0.0);
        corrections.push("Canon sRAW: Schwarzpunkt 0 statt rawler-Wert");
    }
    // calibration: ColorMatrix per illuminant, for DNG the matching ForwardMatrix (root IFD, otherwise raw IFD)
    let root_ifd = dec.ifd(WellKnownIFD::Root).ok().flatten();
    let dng_tag = |t: DngTag| -> Option<Vec<f32>> {
        for ifd in [root_ifd.as_ref(), raw_ifd.as_ref(), root.as_ref()].into_iter().flatten() {
            if let Some(e) = ifd.get_entry(t) {
                let n = e.value.count();
                return Some((0..n).map(|k| e.value.force_f32(k)).collect());
            }
        }
        None
    };
    let mut calibration = Vec::new();
    let ill_codes = [dng_tag(DngTag::CalibrationIlluminant1), dng_tag(DngTag::CalibrationIlluminant2)];
    let fms = [dng_tag(DngTag::ForwardMatrix1), dng_tag(DngTag::ForwardMatrix2)];
    for (il, m) in img.color_matrix.iter() {
        if m.len() < 9 {
            continue;
        }
        let code = *il as u16;
        let Some(kelvin) = color::kelvin_illuminant(code) else { continue };
        let to3 = |v: &[f32]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
        let forward = (0..2)
            .find(|&k| ill_codes[k].as_ref().and_then(|c| c.first()).map(|&c| c as u16) == Some(code))
            .and_then(|k| fms[k].as_ref())
            .filter(|v| v.len() >= 9)
            .map(|v| to3(v));
        calibration.push(color::Calibration { kelvin, color: to3(m), forward });
    }
    // DNG with linearization table (e.g. 8-bit Cine-DNG of the fp): rawler applies the table and delivers 16-bit
    // values on the table's output scale (white level 4095). `bits` is the recorded depth of the file.
    let bits = [raw_ifd.as_ref(), root_ifd.as_ref(), root.as_ref()]
        .into_iter()
        .flatten()
        .find(|i| i.has_entry(TiffCommonTag::Linearization))
        .and_then(|i| i.get_entry(TiffCommonTag::BitsPerSample))
        .map(|e| e.force_usize(0))
        .unwrap_or(img.bps);
    let analog_balance = dng_tag(DngTag::AnalogBalance).filter(|v| v.len() >= 3).map(|v| [v[0], v[1], v[2]]);
    // monochrome sensor (Leica Monochrom, Pentax K-3 III Monochrome) without a color matrix: the gray values count as
    // linear sRGB with a neutral white balance, as finished images do. Without this the development assumed an
    // as-shot white of 5500 K, adapted it to the output white and gave the gray image a blue cast.
    let (wb, matrix, calibration) = if sensor == Sensor::Mono && matrix.is_none() {
        let xyz_srgb = color::inv(&color::srgb_to_xyz_d65()).unwrap_or(color::diag([1.0; 3]));
        (Some([1.0; 3]), Some(("D65".to_string(), xyz_srgb)), vec![color::Calibration { kelvin: 6504.0, color: xyz_srgb, forward: None }])
    } else {
        // no white balance in the file (Kodak DCR, CHDK files of Canon compacts …): the camera's response to D65
        // from its color matrix, as daylight white balance. Without it the raw neutral 1:1:1 counted as the shot's
        // white and the image came out strongly green or yellow (white balance far beyond 20000 K).
        let wb = wb.or_else(|| {
            let (_, m) = matrix.as_ref()?;
            let d65 = [0.950_47f32, 1.0, 1.088_83];
            let cam: [f32; 3] = std::array::from_fn(|r| m[r][0] * d65[0] + m[r][1] * d65[1] + m[r][2] * d65[2]);
            (cam.iter().all(|&v| v > 1e-4)).then(|| [cam[1] / cam[0], 1.0, cam[1] / cam[2]])
        });
        (wb, matrix, calibration)
    };
    Info {
        format,
        maker: img.clean_make.clone(),
        model: img.clean_model.clone(),
        mode: img.camera.mode.clone(),
        width: img.width,
        height: img.height,
        channels: img.cpp,
        bits,
        sensor,
        cfa,
        cfa_size,
        black,
        black_pattern: (img.blacklevel.width, img.blacklevel.height, img.blacklevel.cpp),
        white: img.whitelevel.as_vec(),
        wb,
        matrix,
        matrix_illuminants,
        orientation: if (1..=8).contains(&orientation) { orientation } else { 0 },
        active: rect(img.active_area),
        crop: rect(img.crop_area),
        fuji_rotated: img.fuji_rotation_width.is_some(),
        baseline_exposure,
        dng_profile,
        exif,
        corrections,
        calibration,
        analog_balance,
        fuji_dr: None,
    }
}

#[cfg(test)]
mod name_tests {
    use super::camera_display;

    #[test]
    fn manufacturer_once() {
        assert_eq!(camera_display("SIGMA", "SIGMA fp"), "Sigma fp");
        assert_eq!(camera_display("Sigma", "Sigma BF"), "Sigma BF");
        assert_eq!(camera_display("SIGMA", "SIGMA fp L"), "Sigma fp L");
        assert_eq!(camera_display("Canon", "Canon EOS RP"), "Canon EOS RP");
        assert_eq!(camera_display("Canon", "EOS RP"), "Canon EOS RP");
        assert_eq!(camera_display("Fujifilm", "X100V"), "Fujifilm X100V");
        assert_eq!(camera_display("RICOH", "RICOH GR III"), "RICOH GR III");
        assert_eq!(camera_display("Sigma", "DP2 Merrill"), "Sigma DP2 Merrill");
        assert_eq!(camera_display("Sigma", "Sigma"), "Sigma");
        assert_eq!(camera_display("SIGMA", ""), "SIGMA");
    }
}

#[cfg(test)]
mod cine_dng_tests {
    use super::*;

    /// 8-bit Cine-DNG of the Sigma fp (internal sample file, test is skipped without it): the linearization table
    /// maps the 8-bit codes onto the 12-bit scale (white level 4095, black level 256). rawler dithers the table
    /// values; saturated pixels (code 255 → 4095) land in 4077…4113 with mean 4095 - no scaling error.
    #[test]
    fn fp_8bit_cine_dng_scale() {
        let p = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testbilder/sigma-neue-kameras/fp/A002_653_20240229_000001_2k_8bit.DNG"));
        if !p.exists() {
            return;
        }
        let f = RawFile::open(p).unwrap();
        assert_eq!(f.info.bits, 8, "recorded depth");
        assert_eq!(f.info.white, vec![4095.0]);
        assert!(f.info.black.iter().all(|b| (b - 256.0).abs() < 1.0), "{:?}", f.info.black);
        let d = f.raw_data();
        let max = d.iter().cloned().fold(0.0f32, f32::max);
        assert!(max <= 4113.0, "max {max}");
        // saturated pixels: mean at the white level
        let sat: Vec<f32> = d.iter().cloned().filter(|&v| v >= 4077.0).collect();
        assert!(sat.len() > 2000 && sat.len() < 2400, "{}", sat.len());
        let mean = sat.iter().sum::<f32>() / sat.len() as f32;
        assert!((mean - 4095.0).abs() < 8.0, "mean {mean}");
    }
}

#[cfg(test)]
mod nikon_he_tests {
    use super::*;

    /// rawler's refusal of Nikon "High Efficiency" becomes a plain "not supported", never "possibly corrupt".
    #[test]
    fn high_efficiency_is_unsupported_not_damaged() {
        // rawler 0.8 (typo in its enum name), the correct spelling and other wrappings of the same refusal
        for c in ["Some(HighEfficency)", "Some(HighEfficencyStar)", "Some(HighEfficiency)", "HighEfficiencyStar", "High Efficiency", "high_efficiency_star"] {
            let e = classify(format!("Failed to decode image, possibly corrupt image: NEF compression {c} is not supported"));
            assert!(matches!(&e, Error::Unsupported(s) if s == NIKON_HE), "{c}: {e}");
        }
        assert!(matches!(classify("anything else".into()), Error::Rawler(_)));
        // other NEF compressions and an efficiency word elsewhere stay rawler errors
        assert!(matches!(classify("NEF compression Some(Packed14Bits) is not supported".into()), Error::Rawler(_)));
        assert!(matches!(classify("High efficiency mode of something else".into()), Error::Rawler(_)));
        // the developer text is English (public crate); the app translates by its own key
        assert!(NIKON_HE.is_ascii() && NIKON_HE.contains("not decoded"));
    }

    /// The HE detection depends on rawler's wording. rawler is pinned exactly (`=0.8.0` in Cargo.toml); when it is
    /// updated, this test fails on purpose: check the refusal text of a High Efficiency NEF again (e.g.
    /// `GRAINEO_TEST_NEF_HE_DIR=… cargo test -p raw --test nikon_samples`, `is_nikon_he_refusal`), then update the
    /// version here.
    #[test]
    fn unknown_camera_is_named() {
        let e = classify("Error: Unknown camera, model 'ILCE-7Z9', make: 'SONY', mode: ''".into());
        assert!(matches!(&e, Error::Unsupported(_)), "{e}");
        assert_eq!(unknown_camera(&e.to_string()).as_deref(), Some("SONY ILCE-7Z9"));
        let e = classify("Error: Unknown camera, model 'Canon EOS R1', make: 'Canon', mode: 'raw'".into());
        assert_eq!(unknown_camera(&e.to_string()).as_deref(), Some("Canon EOS R1"));
        let damaged = classify("Failed to decode image, possibly corrupt image: x".into()).to_string();
        assert_eq!(unknown_camera(&damaged), None);
        assert!(damaged.contains(DAMAGED), "{damaged}");
        assert!(classify("Error: No decoder found, model '', make: '', mode: ''".into()).to_string().contains(DAMAGED));
    }

    #[test]
    fn rawler_version_is_the_one_checked() {
        let lock = include_str!("../../../Cargo.lock");
        let version = lock.split("[[package]]").find(|p| p.contains("\nname = \"rawler\"\n")).and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \""))).map(|v| v.trim_end_matches('"'));
        assert_eq!(version, Some("0.8.0"), "rawler was updated: check the Nikon HE detection (is_nikon_he_refusal) against the new error text");
    }
}
