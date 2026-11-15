//! Linear DNG for Lightroom and others.
//!
//! The linear camera image is stored (black level subtracted, spatial gain applied, without white
//! balance, without tone curve) as "LinearRaw" with three channels. Nothing is clipped: values above
//! sensor saturation are kept by scaling the data down and raising `BaselineExposure` by the same
//! amount, values below black (noise) are kept above a matching `BlackLevel`. As an option the samples
//! are written as 32-bit floating point (DNG 1.4), unscaled and compressed losslessly.
//! The colour description follows the DNG specification (freely usable): two calibration
//! illuminants with ColorMatrix/ForwardMatrix from the camera calibration, AsShotNeutral, EXIF data
//! of the shot and a small preview.

use std::path::Path;

use crate::calib::Calibration;
use crate::matrix::Mat3;
use crate::tiffw::{self, rat, srat, Ifd, Value::*};
use crate::{camera_to_xyz, Linear};

/// Options of the own linear DNG.
#[derive(Clone, Debug, Default)]
pub struct DngOptions {
    /// 32-bit floating point samples (DNG 1.4, Deflate with floating-point predictor)
    pub float: bool,
    /// Embed the original raw file (OriginalRawFileData)
    pub embed_original: Option<std::path::PathBuf>,
}

/// Shot data for IFD0 and the EXIF IFD.
#[derive(Clone, Debug, Default)]
pub struct ShotInfo {
    pub model: String,
    pub serial: Option<String>,
    /// "YYYY:MM:DD HH:MM:SS"
    pub date: Option<String>,
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<f64>,
    pub focal_length: Option<f64>,
    pub focal_length_35mm: Option<f64>,
    pub exposure_bias: Option<f64>,
    /// focal range (min, max) and aperture range (min, max) of the lens
    pub lens_range: Option<[f64; 4]>,
    pub lens_model: Option<String>,
}

impl ShotInfo {
    /// From the X3F properties (Merrill PROP or the synthesized list of Quattro/Classic).
    pub fn from_props(props: &x3f::PropList, camera: &str) -> Self {
        let num = |k: &str| props.get(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let text = |k: &str| props.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let date = props.get("TIME").map(str::trim).and_then(|t| match t.parse::<i64>() {
            Ok(secs) => Some(exif_date(secs)),
            Err(_) if t.len() >= 19 => Some(t[..19].to_string()),
            Err(_) => None,
        });
        let range = |k: &str| -> Option<[f64; 2]> {
            let v: Vec<f64> = props.get(k)?.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter_map(|s| s.parse().ok()).collect();
            match v.as_slice() {
                [a] => Some([*a, *a]),
                [a, b, ..] => Some([*a, *b]),
                _ => None,
            }
        };
        let lens_range = match (range("LENSFRANGE"), range("LENSARANGE")) {
            (Some(f), Some(a)) => Some([f[0], f[1], a[0], a[1]]),
            _ => None,
        };
        let lens_model = lens_range.map(|[f0, f1, a0, _]| {
            let f = if f0 == f1 { format!("{f0}mm") } else { format!("{f0}-{f1}mm") };
            format!("{f} F{a0}")
        });
        let model = text("CAMMODEL").unwrap_or_else(|| camera.to_string());
        ShotInfo {
            model,
            serial: text("CAMSERIAL"),
            date,
            exposure_time: num("SHUTTER"),
            f_number: num("APERTURE"),
            iso: num("ISO"),
            focal_length: num("FLENGTH"),
            focal_length_35mm: num("FLEQ35MM"),
            exposure_bias: num("EXPCOMP"),
            lens_range,
            lens_model,
        }
    }

    fn ifd0_tags(&self, tags: &mut Vec<(u16, tiffw::Value)>) {
        if let Some(d) = &self.date {
            tags.push((306, Ascii(d.clone())));
        }
        if let Some(s) = &self.serial {
            tags.push((50735, Ascii(s.clone()))); // CameraSerialNumber
        }
        if let Some([f0, f1, a0, a1]) = self.lens_range {
            tags.push((50736, Rational(vec![rat(f0), rat(f1), rat(a0), rat(a1)]))); // LensInfo
        }
    }

    fn exif_ifd(&self) -> Ifd {
        let mut t = vec![(36864, Undefined(b"0230".to_vec()))]; // ExifVersion
        if let Some(v) = self.exposure_time.filter(|v| *v > 0.0) {
            t.push((33434, Rational(vec![exposure_rational(v)])));
        }
        if let Some(v) = self.f_number {
            t.push((33437, Rational(vec![rat(v)])));
        }
        if let Some(v) = self.iso {
            t.push((34855, Short(vec![v.round().clamp(0.0, 65535.0) as u16])));
        }
        if let Some(d) = &self.date {
            t.push((36867, Ascii(d.clone())));
            t.push((36868, Ascii(d.clone())));
        }
        if let Some(v) = self.exposure_bias {
            t.push((37380, SRational(vec![srat(v)])));
        }
        if let Some(v) = self.focal_length {
            t.push((37386, Rational(vec![rat(v)])));
        }
        if let Some(v) = self.focal_length_35mm {
            t.push((41989, Short(vec![v.round().clamp(0.0, 65535.0) as u16])));
        }
        if let Some([f0, f1, a0, a1]) = self.lens_range {
            t.push((42034, Rational(vec![rat(f0), rat(f1), rat(a0), rat(a1)]))); // LensSpecification
        }
        t.push((42035, Ascii("SIGMA".into()))); // LensMake
        if let Some(l) = &self.lens_model {
            t.push((42036, Ascii(l.clone()))); // LensModel
        }
        if let Some(s) = &self.serial {
            t.push((42033, Ascii(s.clone()))); // BodySerialNumber
        }
        Ifd { tags: t, ..Default::default() }
    }
}

/// Exposure time as 1/n for short times, else in tenths.
fn exposure_rational(t: f64) -> (u32, u32) {
    if t < 0.3 {
        (1, (1.0 / t).round().max(1.0) as u32)
    } else {
        ((t * 10.0).round() as u32, 10)
    }
}

/// Seconds since 1970 (camera time, no time zone) → EXIF date.
pub fn exif_date(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}:{m:02}:{d:02} {:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

// ---- Colour --------------------------------------------------------------------------------

const D65: [f64; 3] = [0.95047, 1.0, 1.08883];
const D50: [f64; 3] = [0.96422, 1.0, 0.82521];
/// CIE illuminant A and D55 (XYZ, Y = 1)
const ILL_A: [f64; 3] = [1.09850, 1.0, 0.35585];
const ILL_D55: [f64; 3] = [0.95682, 1.0, 0.92149];

/// Bradford adaptation from white `from` to white `to`.
fn bradford(from: [f64; 3], to: [f64; 3]) -> Mat3 {
    let b = Mat3([0.8951, 0.2664, -0.1614, -0.7502, 1.7135, 0.0367, 0.0389, -0.0685, 1.0296]);
    let (s, d) = (b.apply(from), b.apply(to));
    b.inverse().unwrap_or(Mat3::IDENTITY).mul(&Mat3::diag([d[0] / s[0], d[1] / s[1], d[2] / s[2]])).mul(&b)
}

/// Colour description of one calibration illuminant: ColorMatrix (XYZ → camera, normalised so the
/// illuminant's white gives at most 1) and ForwardMatrix (white-balanced camera → XYZ D50).
struct Illuminant {
    code: u16,
    color: Mat3,
    forward: Mat3,
}

/// Camera space of the DNG = the normalised layers of [`Linear`]. `cam_to_srgb · diag(gain)` maps them
/// to linear sRGB with the preset's white going to D65 white (white-balanced); undoing that adaptation
/// towards the preset's illuminant `white` gives the plain camera → XYZ matrix.
fn illuminant(cal: &Calibration, mode: Option<&str>, preset: &str, white: [f64; 3], code: u16) -> Option<Illuminant> {
    let p = cal.wb_presets.iter().find(|p| p.name == preset)?;
    let to_srgb = cal.cam_to_srgb(mode, Some(preset));
    let adapted = Mat3::SRGB_TO_XYZ.mul(&to_srgb).mul(&Mat3::diag(p.gain));
    let to_xyz = bradford(D65, white).mul(&adapted);
    let mut color = to_xyz.inverse()?;
    let mx = color.apply(white).iter().cloned().fold(f64::MIN, f64::max);
    if !(mx > 0.0) {
        return None;
    }
    color = color.scale(1.0 / mx);
    // ForwardMatrix: (1,1,1) → D50 white exactly (scale per row of the XYZ result)
    let fwd = bradford(D65, D50).mul(&Mat3::SRGB_TO_XYZ).mul(&to_srgb);
    let n = fwd.apply([1.0; 3]);
    let forward = Mat3::diag([D50[0] / n[0], D50[1] / n[1], D50[2] / n[2]]).mul(&fwd);
    forward.0.iter().all(|v| v.is_finite()).then_some(Illuminant { code, color, forward })
}

/// Colour tags: two illuminants (A from "Incandescent", D55 from "Sunlight") when the calibration
/// has both presets, otherwise the shot's matrix as D65 (as before).
fn color_tags(cal: &Calibration, mode: Option<&str>) -> Vec<(u16, tiffw::Value)> {
    let m = |x: &Mat3| SRational(x.0.iter().map(|&v| srat(v)).collect());
    match (illuminant(cal, mode, "Incandescent", ILL_A, 17), illuminant(cal, mode, "Sunlight", ILL_D55, 20)) {
        (Some(a), Some(d)) => vec![
            (50721, m(&a.color)),
            (50722, m(&d.color)),
            (50778, Short(vec![a.code])),
            (50779, Short(vec![d.code])),
            (50964, m(&a.forward)),
            (50965, m(&d.forward)),
        ],
        _ => {
            let to_xyz = camera_to_xyz(cal, mode);
            let mut c = to_xyz.inverse().unwrap_or(Mat3::IDENTITY);
            let mx = c.apply(D65).iter().cloned().fold(f64::MIN, f64::max);
            if mx > 0.0 {
                c = c.scale(1.0 / mx);
            }
            let fwd = bradford(D65, D50).mul(&to_xyz).mul(&Mat3::diag(cal.wb_gain.map(|g| 1.0 / g)));
            let n = fwd.apply([1.0; 3]);
            let fwd = Mat3::diag([D50[0] / n[0], D50[1] / n[1], D50[2] / n[2]]).mul(&fwd);
            vec![(50721, m(&c)), (50778, Short(vec![21])), (50964, m(&fwd))]
        }
    }
}

// ---- Samples -------------------------------------------------------------------------------

/// Encoded samples with the tags that describe them.
struct Encoded {
    strips: Vec<Vec<u8>>,
    tags: Vec<(u16, tiffw::Value)>,
    /// additional exposure (EV) caused by scaling the data down
    extra_ev: f64,
}

/// 16 bit without clipping: scale by the peak (≥ 1) and lift by a black level that keeps the
/// most negative sample; 32-bit float: unchanged, Deflate with floating-point predictor.
fn encode(data: &[f32], width: usize, spp: usize, float: bool) -> Encoded {
    use rayon::prelude::*;
    let n = |v: u32| vec![v; spp];
    if float {
        let bytes: Vec<u8> = data.par_iter().flat_map_iter(|&v| (if v.is_finite() { v } else { 0.0 }).to_le_bytes()).collect();
        let (strips, mut tags) = tiffw::strips(&bytes, width, spp, tiffw::Samples::F32, tiffw::Compression::Deflate);
        tags.push((258, Short(vec![32; spp])));
        tags.push((339, Short(vec![3; spp]))); // SampleFormat IEEE float
        // BlackLevel 0; WhiteLevel 1.0 (the default for floating point, written explicitly because some
        // readers otherwise assume the integer default)
        tags.push((50717, Long(n(1))));
        return Encoded { strips, tags, extra_ev: 0.0 };
    }
    let (lo, hi) = data.par_iter().filter(|v| v.is_finite()).fold(|| (0f32, 0f32), |(a, b), &v| (a.min(v), b.max(v))).reduce(|| (0.0, 0.0), |(a, b), (c, d)| (a.min(c), b.max(d)));
    let scale = hi.max(1.0) as f64;
    let lo = (lo as f64 / scale).min(0.0);
    let black = ((-lo * 65535.0 / (1.0 - lo)).ceil() as u32).min(8192);
    let k = (65535.0 - black as f64) / scale;
    let bytes: Vec<u8> = data.par_iter().flat_map_iter(|&v| ((black as f64 + v as f64 * k).round().clamp(0.0, 65535.0) as u16).to_le_bytes()).collect();
    let (strips, mut tags) = tiffw::strips(&bytes, width, spp, tiffw::Samples::U16, tiffw::Compression::None);
    tags.push((258, Short(vec![16; spp])));
    tags.push((50714, Long(n(black)))); // BlackLevel
    tags.push((50717, Long(n(65535)))); // WhiteLevel
    Encoded { strips, tags, extra_ev: scale.log2() }
}

/// Small sRGB JPEG preview as a sub-IFD (NewSubFileType 1, YCbCr, PreviewColorSpace sRGB).
pub fn preview_ifd(img: &crate::Rgb16) -> Option<Ifd> {
    let (w, h) = (u16::try_from(img.width).ok()?, u16::try_from(img.height).ok()?);
    let mut jpeg = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut jpeg, 85);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    enc.encode(&img.to_rgb8(), w, h, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(Ifd {
        tags: vec![
            (254, Long(vec![1])),
            (256, Long(vec![w as u32])),
            (257, Long(vec![h as u32])),
            (258, Short(vec![8, 8, 8])),
            (259, Short(vec![7])),
            (262, Short(vec![6])),
            (277, Short(vec![3])),
            (278, Long(vec![h as u32])),
            (284, Short(vec![1])),
            (530, Short(vec![2, 2])),
            (50970, Long(vec![2])), // PreviewColorSpace sRGB
        ],
        strips: vec![jpeg],
        ..Default::default()
    })
}

/// OriginalRawFileData (DNG specification): data fork in 64 KiB zlib blocks with an offset index
/// relative to the start of the fork, then an empty resource fork and zero type/creator; big endian.
pub fn original_raw_data(file: &[u8]) -> Vec<u8> {
    use rayon::prelude::*;
    use std::io::Write;
    let blocks: Vec<Vec<u8>> = file
        .par_chunks(65536)
        .map(|b| {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
            let _ = e.write_all(b);
            e.finish().unwrap_or_default()
        })
        .collect();
    let mut out = Vec::with_capacity(file.len() + 64);
    out.extend_from_slice(&(file.len() as u32).to_be_bytes());
    if !file.is_empty() {
        // offsets measured from the start of this fork's data (= its ForkLength field)
        let mut off = 4 + 4 * (blocks.len() as u32 + 1);
        for b in &blocks {
            out.extend_from_slice(&off.to_be_bytes());
            off += b.len() as u32;
        }
        out.extend_from_slice(&off.to_be_bytes());
        for b in &blocks {
            out.extend_from_slice(b);
        }
    }
    out.extend_from_slice(&0u32.to_be_bytes()); // resource fork: empty
    out.extend_from_slice(&[0; 8]); // file type, creator
    out
}

// ---- Writers -------------------------------------------------------------------------------

/// Everything around the samples that both DNG kinds share.
pub struct DngMeta<'a> {
    pub shot: &'a ShotInfo,
    pub xmp: Option<&'a str>,
    pub preview: Option<Ifd>,
    pub options: &'a DngOptions,
}

fn base_tags(width: usize, height: usize, spp: usize, unique: String, meta: &DngMeta, float: bool) -> std::io::Result<Vec<(u16, tiffw::Value)>> {
    let mut tags = vec![
        (254, Long(vec![0])),
        (256, Long(vec![width as u32])),
        (257, Long(vec![height as u32])),
        (262, Short(vec![34892])), // LinearRaw
        (271, Ascii("SIGMA".into())),
        (272, Ascii(meta.shot.model.clone())),
        (274, Short(vec![1])),
        (277, Short(vec![spp as u16])),
        (284, Short(vec![1])),
        (305, Ascii(crate::export::SOFTWARE.into())),
        (50706, Byte(vec![1, 4, 0, 0])),                                 // DNGVersion
        (50707, Byte(if float { vec![1, 4, 0, 0] } else { vec![1, 1, 0, 0] })), // DNGBackwardVersion
        (50708, Ascii(unique)),                                           // UniqueCameraModel
    ];
    meta.shot.ifd0_tags(&mut tags);
    if let Some(x) = meta.xmp {
        tags.push((700, Byte(x.as_bytes().to_vec()))); // XMLPacket (Lightroom settings)
    }
    if let Some(p) = &meta.options.embed_original {
        let file = std::fs::read(p)?;
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        tags.push((50827, Ascii(name))); // OriginalRawFileName
        tags.push((50828, Undefined(original_raw_data(&file)))); // OriginalRawFileData
    }
    Ok(tags)
}

fn finish(mut tags: Vec<(u16, tiffw::Value)>, enc: Encoded, meta: DngMeta, path: &Path) -> std::io::Result<()> {
    tags.extend(enc.tags);
    let ifd = Ifd { tags, strips: enc.strips, sub_ifds: meta.preview.into_iter().collect(), exif: Some(Box::new(meta.shot.exif_ifd())) };
    // write next to the target and rename only when complete
    tiffw::write_ifd(std::io::BufWriter::new(std::fs::File::create(path)?), ifd)
}

/// Colour linear DNG: three channels, camera space.
pub fn write_linear_dng(lin: &Linear, cal: &Calibration, mode: Option<&str>, meta: DngMeta, path: &Path) -> std::io::Result<()> {
    let neutral = cal.wb_gain.map(|g| 1.0 / g);
    let nmax = neutral.iter().cloned().fold(f64::MIN, f64::max);
    let neutral = neutral.map(|n| n / nmax);
    // Lightroom scales to the strongest channel; the baseline exposure matches that
    // to the brightness of our own development.
    let min_gain = cal.wb_gain.iter().cloned().fold(f64::MAX, f64::min);
    let float = meta.options.float;
    let enc = encode(&lin.data, lin.width, 3, float);
    let mut tags = base_tags(lin.width, lin.height, 3, cal.camera.clone(), &meta, float)?;
    tags.extend(color_tags(cal, mode));
    tags.push((50728, Rational(neutral.iter().map(|&v| rat(v)).collect()))); // AsShotNeutral
    tags.push((50730, SRational(vec![srat(min_gain.log2() + enc.extra_ev)]))); // BaselineExposure
    finish(tags, enc, meta, path)
}

/// Foveon monochrome DNG: the three sensor layers are combined linearly into one channel
/// according to the layer mix `mix` (bottom, middle, top), as in our monochrome development,
/// but still as raw data (linear, without tone curve). Lightroom treats the file like one from a
/// monochrome camera; exposure and tones stay freely adjustable there.
pub fn write_mono_dng(lin: &Linear, cal: &Calibration, mix: [f32; 3], meta: DngMeta, path: &Path) -> std::io::Result<()> {
    use rayon::prelude::*;
    // like our monochrome output: layers weighted with the white balance, then mixed (unclipped)
    let gmax = cal.wb_gain.iter().cloned().fold(f64::MIN, f64::max);
    let g: [f32; 3] = cal.wb_gain.map(|v| (v / gmax) as f32);
    let px: Vec<f32> = lin.data.par_chunks_exact(3).map(|p| mix[0] * g[0] * p[0] + mix[1] * g[1] * p[1] + mix[2] * g[2] * p[2]).collect();
    // Baseline exposure: same brightness as the colour DNG (where the white balance raises the
    // grey by the strongest channel gain); the scaling to 16 bits is compensated in `encode`.
    let float = meta.options.float;
    let enc = encode(&px, lin.width, 1, float);
    let mut tags = base_tags(lin.width, lin.height, 1, format!("{} Foveon-Monochrom", cal.camera), &meta, float)?;
    tags.push((50730, SRational(vec![srat(gmax.log2() + enc.extra_ev)])));
    finish(tags, enc, meta, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_date_from_seconds() {
        assert_eq!(exif_date(0), "1970:01:01 00:00:00");
        assert_eq!(exif_date(1_700_000_000), "2023:11:14 22:13:20");
    }

    #[test]
    fn integer_encoding_keeps_highlights_and_negatives() {
        let data = [-0.01f32, 0.0, 0.5, 1.0, 2.0, 0.25];
        let e = encode(&data, 2, 3, false);
        assert!((e.extra_ev - 1.0).abs() < 1e-9, "peak 2 → +1 EV");
        let black = match e.tags.iter().find(|t| t.0 == 50714) { Some((_, Long(v))) => v[0], _ => panic!() };
        assert!(black > 0);
        let px: Vec<u16> = e.strips[0].chunks(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        assert!(px[0] < black as u16 && px[0] > 0, "below black kept: {px:?}");
        assert_eq!(px[4], 65535);
        let decode = |v: u16| (v as f64 - black as f64) / (65535.0 - black as f64) * 2.0;
        assert!((decode(px[2]) - 0.5).abs() < 1e-4 && (decode(px[3]) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn float_encoding_is_lossless() {
        let data: Vec<f32> = (0..300).map(|i| i as f32 * 0.37 - 20.0).collect();
        let e = encode(&data, 10, 3, true);
        assert_eq!(e.extra_ev, 0.0);
        assert!(e.tags.iter().any(|t| t.0 == 339));
    }

    #[test]
    fn original_raw_layout() {
        let file: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
        let d = original_raw_data(&file);
        assert_eq!(u32::from_be_bytes(d[0..4].try_into().unwrap()), 150_000);
        let blocks = 3usize;
        let off: Vec<usize> = (0..=blocks).map(|i| u32::from_be_bytes(d[4 + 4 * i..8 + 4 * i].try_into().unwrap()) as usize).collect();
        assert_eq!(off[0], 4 + 4 * (blocks + 1));
        let mut back = Vec::new();
        for i in 0..blocks {
            use std::io::Read;
            flate2::read::ZlibDecoder::new(&d[off[i]..off[i + 1]]).read_to_end(&mut back).unwrap();
        }
        assert_eq!(back, file);
        assert_eq!(&d[off[blocks]..], &[0u8; 12]);
    }
}
