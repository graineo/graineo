//! Metadata of exported images (JPEG, JPEG XL, TIFF, PNG): what goes into the file and a small writer that
//! works on every platform.
//!
//! - [`Shot`]: shooting data in neutral form (camera, lens, exposure, date). Sources: the X3F
//!   properties (ImageIO cannot read X3F), the EXIF of a JPEG, or the RAW decoder (filled by the caller).
//! - On macOS ImageIO writes the metadata ([`crate::imageio::write_metadata`]): it reads the
//!   original's EXIF (RAW, HEIC, JPEG …) and takes over only the fields allowed by [`allowed_tag`];
//!   [`Shot`] fills what ImageIO does not know.
//! - Without macOS [`write_own`] writes EXIF (camera block) and XMP (copyright, personal fields)
//!   itself: JPEG APP1, PNG `eXIf`/`iTXt`, TIFF IFD0 + EXIF IFD appended to the file.
//! - JPEG XL (all platforms): `Exif` and `xml ` boxes of the container ([`jxl_with`]). On macOS their content is
//!   what ImageIO writes for a JPEG (so exactly the fields of the JPEG export), elsewhere the own writer's.
//!
//! Never written: serial numbers of camera and lens, owner names, names of people.

use std::path::Path;

pub use crate::imageio_common::ExportFields;

/// Shooting data of a photo (all optional).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shot {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens_make: Option<String>,
    pub lens: Option<String>,
    /// "YYYY:MM:DD HH:MM:SS" (camera time, without time zone)
    pub date: Option<String>,
    /// seconds
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<u32>,
    /// mm
    pub focal_length: Option<f64>,
    pub focal_length_35mm: Option<u32>,
    /// EV
    pub exposure_bias: Option<f64>,
    /// EXIF ExposureProgram: 1 manual, 2 program, 3 aperture priority, 4 shutter priority
    pub exposure_program: Option<u16>,
}

fn text(v: Option<&str>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn number(v: Option<&str>) -> Option<f64> {
    v?.trim().trim_start_matches('+').parse::<f64>().ok().filter(|v| v.is_finite())
}

/// "1/320" or "2.5" → seconds.
fn shutter_text(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.split_once('/') {
        Some((n, d)) => Some(n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok().filter(|d| *d > 0.0)?),
        None => s.parse().ok(),
    }
}

/// Seconds since 1970 (camera time) → "YYYY:MM:DD HH:MM:SS" (H. Hinnant, "civil_from_days").
pub fn exif_date_from_secs(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
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

/// A date as the EXIF form "YYYY:MM:DD HH:MM:SS" (also accepts "YYYY-MM-DDTHH:MM:SS"). None: not a date.
pub fn normalize_date(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes().get(..19)?;
    let ok = b.iter().enumerate().all(|(i, &c)| match i {
        4 | 7 => c == b':' || c == b'-',
        10 => c == b' ' || c == b'T',
        13 | 16 => c == b':',
        _ => c.is_ascii_digit(),
    });
    ok.then(|| format!("{}:{}:{} {}", &s[0..4], &s[5..7], &s[8..10], &s[11..19]))
}

impl Shot {
    pub fn is_empty(&self) -> bool {
        *self == Shot::default()
    }

    /// From the X3F properties (Merrill PROP; Quattro and older models: synthesized from the EXIF of
    /// the embedded JPEG and the CAMF). Lens: name of the fixed lens, otherwise from the focal and aperture range.
    pub fn from_x3f(path: &Path) -> Option<Shot> {
        let x = x3f::X3fFile::open(path).ok()?;
        let p = x.properties().ok()?;
        let mut s = Self::from_x3f_props(&p);
        // interchangeable lenses (sd Quattro): the name stands in the EXIF of the embedded JPEG (the dp models
        // with a fixed lens only write a lens number there)
        let fixed = s.model.as_deref().and_then(crate::exif::fixed_lens).is_some();
        let name = x.exif().and_then(|e| text(e.lens_model.as_deref())).filter(|l| l.chars().any(|c| c.is_alphabetic()));
        if let Some(lens) = name.filter(|_| !fixed) {
            s.lens = Some(lens);
            s.lens_make.get_or_insert_with(|| "SIGMA".into());
        }
        Some(s)
    }

    pub fn from_x3f_props(p: &x3f::PropList) -> Shot {
        let model = text(p.get("CAMMODEL"));
        let date = p.get("TIME").map(str::trim).and_then(|t| match t.parse::<i64>() {
            Ok(secs) if secs > 0 => Some(exif_date_from_secs(secs)),
            Ok(_) => None,
            Err(_) => normalize_date(t),
        });
        let range = |k: &str| -> Option<(f64, f64)> {
            let v: Vec<f64> = p.get(k)?.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter_map(|s| s.parse().ok()).collect();
            let lo = v.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = v.iter().cloned().fold(0.0, f64::max);
            (!v.is_empty()).then_some((lo, hi))
        };
        let fmt = |v: f64| if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v}") };
        let lens = model.as_deref().and_then(crate::exif::fixed_lens).map(|l| l.trim_end_matches(" (fest)").to_string()).or_else(|| {
            let (f0, f1) = range("LENSFRANGE")?;
            let focal = if f0 == f1 { format!("{}mm", fmt(f0)) } else { format!("{}-{}mm", fmt(f0), fmt(f1)) };
            Some(match range("LENSARANGE") {
                Some((a, _)) => format!("{focal} f/{}", fmt(a)),
                None => focal,
            })
        });
        let program = match p.get("PMODE").map(str::trim) {
            Some("M") => Some(1),
            Some("P") => Some(2),
            Some("A") => Some(3),
            Some("S") => Some(4),
            _ => None,
        };
        Shot {
            make: text(p.get("CAMMANUF")).or(Some("SIGMA".into())),
            lens_make: lens.as_ref().map(|_| "SIGMA".to_string()),
            lens,
            model,
            date,
            // the camera's display values ("1/320", "7.1") first: the measured ones are slightly off (0.00301 s, f/7.025)
            exposure_time: p.get("SH_DESC").and_then(shutter_text).or_else(|| number(p.get("SHUTTER"))).filter(|v| *v > 0.0),
            f_number: number(p.get("AP_DESC")).or_else(|| number(p.get("APERTURE")).map(|f| (f * 10.0).round() / 10.0)).filter(|v| *v > 0.0),
            iso: number(p.get("ISO")).filter(|v| *v > 0.0).map(|v| v.round() as u32),
            focal_length: number(p.get("FLENGTH")).filter(|v| *v > 0.0),
            focal_length_35mm: number(p.get("FLEQ35MM")).filter(|v| *v > 0.0).map(|v| v.round() as u32),
            exposure_bias: number(p.get("EXPCOMP")),
            exposure_program: program,
        }
    }

    /// From the file header of any format the own reader knows (for platforms without ImageIO; see
    /// [`crate::exif_header`]).
    pub fn from_header(path: &Path) -> Option<Shot> {
        let h = crate::exif_header::read(path)?;
        let p = h.props;
        let s = Shot {
            make: text(p.maker.as_deref()),
            model: text(p.camera.as_deref()),
            lens_make: text(h.lens_make.as_deref()),
            lens: text(p.lens.as_deref()),
            date: p.capture.as_deref().and_then(normalize_date),
            exposure_time: p.time,
            f_number: p.aperture,
            iso: p.iso.and_then(|v| u32::try_from(v).ok()),
            focal_length: p.focal,
            focal_length_35mm: h.focal_length_35mm,
            exposure_bias: h.exposure_bias,
            ..Default::default()
        };
        (!s.is_empty()).then_some(s)
    }

    /// Fields of `self`, missing ones from `other`.
    pub fn or(self, other: Option<Shot>) -> Shot {
        let Some(o) = other else { return self };
        Shot {
            make: self.make.or(o.make),
            model: self.model.or(o.model),
            lens_make: self.lens_make.or(o.lens_make),
            lens: self.lens.or(o.lens),
            date: self.date.or(o.date),
            exposure_time: self.exposure_time.or(o.exposure_time),
            f_number: self.f_number.or(o.f_number),
            iso: self.iso.or(o.iso),
            focal_length: self.focal_length.or(o.focal_length),
            focal_length_35mm: self.focal_length_35mm.or(o.focal_length_35mm),
            exposure_bias: self.exposure_bias.or(o.exposure_bias),
            exposure_program: self.exposure_program.or(o.exposure_program),
        }
    }

    /// The fields as XMP paths (rationals as "n/d", dates ISO 8601).
    pub fn xmp_pairs(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        let mut put = |k: &'static str, val: Option<String>| {
            if let Some(val) = val {
                v.push((k, val));
            }
        };
        put("tiff:Make", self.make.clone());
        put("tiff:Model", self.model.clone());
        // also given as image properties (see `exif_properties`); TIFF takes them from here, JPEG from there
        put("exifEX:LensMake", self.lens_make.clone());
        put("exifEX:LensModel", self.lens.clone());
        let iso_date = self.date.as_deref().and_then(crate::imageio_common::exif_date);
        put("exif:DateTimeOriginal", iso_date.clone());
        put("exif:DateTimeDigitized", iso_date);
        put("exif:FocalLengthIn35mmFilm", self.focal_length_35mm.map(|f| f.to_string()));
        put("exif:ExposureProgram", self.exposure_program.map(|p| p.to_string()));
        put("exif:ExposureTime", self.exposure_time.map(|t| fraction(exposure_rational(t))));
        put("exif:FNumber", self.f_number.map(|f| fraction(rat(f))));
        put("exifEX:PhotographicSensitivity", self.iso.map(|i| i.to_string()));
        put("exif:FocalLength", self.focal_length.map(|f| fraction(rat(f))));
        put("exif:ExposureBiasValue", self.exposure_bias.map(|b| {
            let (n, d) = srat(b);
            format!("{n}/{d}")
        }));
        v
    }

    /// Fields ImageIO only maps into the EXIF block when set as image properties of the `{Exif}` dictionary
    /// (set through XMP paths they stay in the XMP).
    pub fn exif_properties(&self) -> Vec<(&'static str, Prop)> {
        let mut v = Vec::new();
        if let Some(d) = &self.date {
            v.push(("DateTimeOriginal", Prop::Text(d.clone())));
            v.push(("DateTimeDigitized", Prop::Text(d.clone())));
        }
        if let Some(f) = self.focal_length_35mm {
            v.push(("FocalLenIn35mmFilm", Prop::Number(f as f64)));
        }
        if let Some(l) = &self.lens_make {
            v.push(("LensMake", Prop::Text(l.clone())));
        }
        if let Some(l) = &self.lens {
            v.push(("LensModel", Prop::Text(l.clone())));
        }
        if let Some(p) = self.exposure_program {
            v.push(("ExposureProgram", Prop::Number(p as f64)));
        }
        v
    }
}

/// Value of an image property.
#[derive(Clone, Debug, PartialEq)]
pub enum Prop {
    Text(String),
    Number(f64),
}

fn has_extension(p: &Path, exts: &[&str]) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Writes the metadata of an exported JPEG/JPEG XL/TIFF/PNG (the one entry point for the app and graineo-dev):
/// shooting data of `original` and copyright always; personal fields and location as `fields` says.
/// X3F: shooting data from the X3F properties. Returns: short report.
pub fn write(target: &Path, original: &Path, fields: &ExportFields) -> Result<String, String> {
    let mut f = fields.clone();
    if has_extension(original, &["x3f"]) {
        f.shot = Shot::from_x3f(original).or(f.shot);
    } else if f.shot.is_none() && cfg!(not(target_os = "macos")) {
        f.shot = Shot::from_header(original);
    }
    if has_extension(target, &["jxl"]) {
        return write_jxl(target, original, &f);
    }
    let (w, h) = crate::imageio::properties(target).map(|e| (e.width.unwrap_or(0), e.height.unwrap_or(0))).unwrap_or((0, 0));
    let report = crate::imageio::write_metadata(target, original, w, h, &f)?;
    // TIFF: ImageIO puts the capture date only into XMP/Photoshop data, not into the EXIF block: add it there
    #[cfg(target_os = "macos")]
    if has_extension(target, &["tif", "tiff"]) {
        let shot = crate::imageio::original_shot(original).unwrap_or_default().or(f.shot.clone());
        if let Some(date) = shot.date {
            let data = std::fs::read(target).map_err(|e| e.to_string())?;
            if let Some(out) = tiff_exif_dates(&data, &date)? {
                replace_file(target, &out)?;
            }
        }
    }
    // PNG: depending on the metadata, ImageIO writes only XMP or only the EXIF chunk; add what is missing
    #[cfg(target_os = "macos")]
    if has_extension(target, &["png"]) {
        let shot = crate::imageio::original_shot(original).unwrap_or_default().or(f.shot.clone());
        let own = OwnFields { fields: ExportFields { shot: Some(shot), ..f.clone() }, width: w, height: h };
        let data = std::fs::read(target).map_err(|e| e.to_string())?;
        let chunks = png_chunks(&data)?;
        let has_exif = chunks.iter().any(|(k, ..)| *k == b"eXIf");
        let has_xmp = chunks.iter().any(|(k, b, _)| *k == b"iTXt" && b.starts_with(PNG_XMP));
        let exif = (!has_exif).then(|| exif_block(&own));
        let xmp = if has_xmp { None } else { xmp_packet(&own) };
        if exif.is_some() || xmp.is_some() {
            let out = png_with(&data, exif.as_deref(), xmp.as_deref())?;
            replace_file(target, &out)?;
        }
    }
    Ok(report)
}

/// Replaces `target` by `data` via a side file.
fn replace_file(target: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = target.with_file_name(format!(".{}.meta", target.file_name().unwrap_or_default().to_string_lossy()));
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Is an XMP tag (prefix:name) of the original allowed into the export? Shooting data only;
/// GPS only with `gps`. Serial numbers, owner names and everything personal stay out.
pub fn allowed_tag(path: &str, gps: bool) -> bool {
    const SHOOTING: &[&str] = &[
        "tiff:Make",
        "tiff:Model",
        "exif:ExposureTime",
        "exif:FNumber",
        "exif:ExposureProgram",
        "exif:ISOSpeedRatings",
        "exif:DateTimeOriginal",
        "exif:DateTimeDigitized",
        "exif:ShutterSpeedValue",
        "exif:ApertureValue",
        "exif:ExposureBiasValue",
        "exif:MaxApertureValue",
        "exif:MeteringMode",
        "exif:Flash",
        "exif:FocalLength",
        "exif:FocalLengthIn35mmFilm",
        // ImageIO's name for EXIF FocalLengthIn35mmFilm
        "exif:FocalLenIn35mmFilm",
        "exif:ExposureMode",
        "exif:WhiteBalance",
        "exif:SceneCaptureType",
        "exifEX:LensMake",
        "exifEX:LensModel",
        "exifEX:LensSpecification",
        "exifEX:PhotographicSensitivity",
        "exifEX:SensitivityType",
        "exifEX:RecommendedExposureIndex",
        "exifEX:OffsetTime",
        "exifEX:OffsetTimeOriginal",
        "exifEX:OffsetTimeDigitized",
        "aux:Lens",
        "aux:LensInfo",
    ];
    if SHOOTING.contains(&path) {
        return true;
    }
    gps && path.strip_prefix("exif:GPS").is_some()
}

/// Program name for EXIF Software / XMP CreatorTool.
pub fn software() -> String {
    format!("graineo {}", env!("CARGO_PKG_VERSION"))
}

// ─────────────────────────────────────────────────────────── own writer (all platforms)

/// Rational for EXIF: up to 4 decimals.
fn rat(x: f64) -> (u32, u32) {
    let x = x.max(0.0);
    for d in [1u32, 10, 100, 1000, 10_000] {
        let n = x * d as f64;
        if (n - n.round()).abs() < 1e-6 || d == 10_000 {
            return (n.round().min(u32::MAX as f64) as u32, d);
        }
    }
    (0, 1)
}

fn srat(x: f64) -> (i32, i32) {
    let n = (x * 100.0).round();
    let (mut n, mut d) = (n as i32, 100);
    while d > 1 && n % 10 == 0 {
        n /= 10;
        d /= 10;
    }
    (n, d)
}

/// Exposure time as 1/n for short times, else in tenths.
fn exposure_rational(t: f64) -> (u32, u32) {
    if t > 0.0 && t < 0.3 {
        (1, (1.0 / t).round().max(1.0) as u32)
    } else {
        ((t * 10.0).round() as u32, 10)
    }
}

fn fraction((n, d): (u32, u32)) -> String {
    format!("{n}/{d}")
}

enum Val {
    Ascii(String),
    Short(u16),
    Long(u32),
    Rational((u32, u32)),
    SRational((i32, i32)),
    Undefined(Vec<u8>),
    /// an entry taken over unchanged from an existing TIFF (type, count, 4 value bytes)
    Keep(u16, u32, [u8; 4]),
}

struct Enc {
    le: bool,
}

impl Enc {
    fn u16(&self, v: u16) -> [u8; 2] {
        if self.le { v.to_le_bytes() } else { v.to_be_bytes() }
    }
    fn u32(&self, v: u32) -> [u8; 4] {
        if self.le { v.to_le_bytes() } else { v.to_be_bytes() }
    }

    /// (type, count, payload) of a value.
    fn payload(&self, v: &Val) -> (u16, u32, Vec<u8>) {
        match v {
            Val::Ascii(s) => {
                let mut b: Vec<u8> = s.bytes().filter(|&c| c != 0).collect();
                b.push(0);
                (2, b.len() as u32, b)
            }
            Val::Short(x) => (3, 1, self.u16(*x).to_vec()),
            Val::Long(x) => (4, 1, self.u32(*x).to_vec()),
            Val::Rational((n, d)) => (5, 1, [self.u32(*n), self.u32(*d)].concat()),
            Val::SRational((n, d)) => (10, 1, [self.u32(*n as u32), self.u32(*d as u32)].concat()),
            Val::Undefined(b) => (7, b.len() as u32, b.clone()),
            Val::Keep(t, c, b) => (*t, *c, b.to_vec()),
        }
    }

    /// One IFD at absolute offset `at`: entries (sorted by tag), next-IFD offset, then out-of-line values.
    fn ifd(&self, mut entries: Vec<(u16, Val)>, at: u32, next: u32) -> Vec<u8> {
        entries.sort_by_key(|(t, _)| *t);
        entries.dedup_by_key(|(t, _)| *t);
        let n = entries.len();
        let mut data_at = at as usize + 2 + 12 * n + 4;
        let mut head = self.u16(n as u16).to_vec();
        let mut data = Vec::new();
        for (tag, v) in &entries {
            let (typ, count, bytes) = self.payload(v);
            head.extend_from_slice(&self.u16(*tag));
            head.extend_from_slice(&self.u16(typ));
            head.extend_from_slice(&self.u32(count));
            if matches!(v, Val::Keep(..)) || bytes.len() <= 4 {
                let mut b = bytes.clone();
                b.resize(4, 0);
                head.extend_from_slice(&b);
            } else {
                head.extend_from_slice(&self.u32(data_at as u32));
                data.extend_from_slice(&bytes);
                if bytes.len() % 2 == 1 {
                    data.push(0);
                }
                data_at += bytes.len().div_ceil(2) * 2;
            }
        }
        head.extend_from_slice(&self.u32(next));
        head.extend_from_slice(&data);
        head
    }
}

/// What [`write_own`] writes: the export fields plus the pixel size of the exported image.
#[derive(Clone, Debug, Default)]
pub struct OwnFields {
    pub fields: ExportFields,
    pub width: u32,
    pub height: u32,
}

fn ifd0_and_exif(f: &OwnFields) -> (Vec<(u16, Val)>, Vec<(u16, Val)>) {
    let mut ifd0 = vec![(0x0112, Val::Short(1)), (0x0131, Val::Ascii(software()))];
    let mut exif = vec![(0x9000, Val::Undefined(b"0232".to_vec()))];
    if f.width > 0 && f.height > 0 {
        exif.push((0xA002, Val::Long(f.width)));
        exif.push((0xA003, Val::Long(f.height)));
    }
    if let Some(c) = f.fields.copyright.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        ifd0.push((0x8298, Val::Ascii(c.to_string())));
    }
    if let Some(s) = &f.fields.shot {
        if let Some(v) = &s.make {
            ifd0.push((0x010F, Val::Ascii(v.clone())));
        }
        if let Some(v) = &s.model {
            ifd0.push((0x0110, Val::Ascii(v.clone())));
        }
        if let Some(v) = s.exposure_time.filter(|v| *v > 0.0) {
            exif.push((0x829A, Val::Rational(exposure_rational(v))));
        }
        if let Some(v) = s.f_number {
            exif.push((0x829D, Val::Rational(rat(v))));
        }
        if let Some(v) = s.exposure_program {
            exif.push((0x8822, Val::Short(v)));
        }
        if let Some(v) = s.iso {
            exif.push((0x8827, Val::Short(v.min(65535) as u16)));
        }
        if let Some(d) = &s.date {
            exif.push((0x9003, Val::Ascii(d.clone())));
            exif.push((0x9004, Val::Ascii(d.clone())));
        }
        if let Some(v) = s.exposure_bias {
            exif.push((0x9204, Val::SRational(srat(v))));
        }
        if let Some(v) = s.focal_length {
            exif.push((0x920A, Val::Rational(rat(v))));
        }
        if let Some(v) = s.focal_length_35mm {
            exif.push((0xA405, Val::Short(v.min(65535) as u16)));
        }
        if let Some(v) = &s.lens_make {
            exif.push((0xA433, Val::Ascii(v.clone())));
        }
        if let Some(v) = &s.lens {
            exif.push((0xA434, Val::Ascii(v.clone())));
        }
    }
    (ifd0, exif)
}

/// TIFF block (little endian) with IFD0 and EXIF IFD, as in JPEG APP1 / PNG eXIf.
fn exif_block(f: &OwnFields) -> Vec<u8> {
    let e = Enc { le: true };
    let (mut ifd0, exif) = ifd0_and_exif(f);
    ifd0.push((0x8769, Val::Long(0)));
    let len0 = e.ifd(ifd0_clone(&ifd0), 8, 0).len() as u32;
    let exif_at = 8 + len0;
    ifd0.retain(|(t, _)| *t != 0x8769);
    ifd0.push((0x8769, Val::Long(exif_at)));
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&e.u32(8));
    out.extend_from_slice(&e.ifd(ifd0, 8, 0));
    out.extend_from_slice(&e.ifd(exif, exif_at, 0));
    out
}

fn ifd0_clone(v: &[(u16, Val)]) -> Vec<(u16, Val)> {
    v.iter()
        .map(|(t, v)| {
            (
                *t,
                match v {
                    Val::Ascii(s) => Val::Ascii(s.clone()),
                    Val::Short(x) => Val::Short(*x),
                    Val::Long(x) => Val::Long(*x),
                    Val::Rational(x) => Val::Rational(*x),
                    Val::SRational(x) => Val::SRational(*x),
                    Val::Undefined(b) => Val::Undefined(b.clone()),
                    Val::Keep(a, b, c) => Val::Keep(*a, *b, *c),
                },
            )
        })
        .collect()
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Longest title/description in the export (characters): longer texts (Lightroom allows megabytes) would burst the
/// 64 KB segment of a JPEG, and the whole metadata would be lost.
const MAX_TEXT_CHARS: usize = 2000;
/// Longest keyword (characters) and largest number of keywords written to the export.
const MAX_KEYWORD_CHARS: usize = 200;
const MAX_KEYWORDS: usize = 500;

fn shortened(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// XMP packet with copyright and (optionally) the personal fields. None: nothing to write.
fn xmp_packet(f: &OwnFields) -> Option<String> {
    let copyright = f.fields.copyright.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let personal = f.fields.personal.then_some(&f.fields);
    if copyright.is_none() && personal.is_none() {
        return None;
    }
    let mut attrs = format!(" xmp:CreatorTool=\"{}\"", xml(&software()));
    let mut body = String::new();
    let alt = |name: &str, v: &str| format!("<{name}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></{name}>", xml(v));
    if let Some(p) = personal {
        attrs.push_str(&format!(" xmp:Rating=\"{}\"", p.stars.min(5)));
        if !p.label.is_empty() {
            attrs.push_str(&format!(" xmp:Label=\"{}\"", xml(&p.label)));
        }
        if !p.keywords.is_empty() {
            body.push_str("<dc:subject><rdf:Bag>");
            for k in p.keywords.iter().take(MAX_KEYWORDS) {
                body.push_str(&format!("<rdf:li>{}</rdf:li>", xml(&shortened(k, MAX_KEYWORD_CHARS))));
            }
            body.push_str("</rdf:Bag></dc:subject>");
        }
        if !p.hierarchy.is_empty() {
            body.push_str("<lr:hierarchicalSubject><rdf:Bag>");
            for k in p.hierarchy.iter().take(MAX_KEYWORDS) {
                body.push_str(&format!("<rdf:li>{}</rdf:li>", xml(&shortened(k, MAX_KEYWORD_CHARS * 4))));
            }
            body.push_str("</rdf:Bag></lr:hierarchicalSubject>");
        }
        if let Some(t) = &p.title {
            body.push_str(&alt("dc:title", &shortened(t, MAX_TEXT_CHARS)));
        }
        if let Some(t) = &p.description {
            body.push_str(&alt("dc:description", &shortened(t, MAX_TEXT_CHARS)));
        }
    }
    if let Some(c) = copyright {
        body.push_str(&alt("dc:rights", c));
    }
    Some(format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:lr=\"http://ns.adobe.com/lightroom/1.0/\"{attrs}>{body}</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>"
    ))
}

const XMP_ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// JPEG: removes existing EXIF/XMP APP1 segments and inserts new ones after SOI (and after JFIF APP0).
fn jpeg_with(data: &[u8], exif: &[u8], xmp: Option<&str>) -> Result<Vec<u8>, String> {
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err("not a JPEG".into());
    }
    let seg = |id: &[u8], payload: &[u8]| -> Result<Vec<u8>, String> {
        let len = id.len() + payload.len() + 2;
        if len > 0xFFFF {
            return Err("metadata too large".into());
        }
        Ok([&[0xFF, 0xE1][..], &(len as u16).to_be_bytes(), id, payload].concat())
    };
    let mut inserted = seg(b"Exif\0\0", exif)?;
    if let Some(x) = xmp {
        // an XMP packet that still does not fit only loses itself, never the EXIF block (with the copyright)
        if let Ok(x) = seg(XMP_ID, x.as_bytes()) {
            inserted.extend(x);
        }
    }
    let mut out = data[..2].to_vec();
    let mut i = 2;
    let mut placed = false;
    while i + 4 <= data.len() && data[i] == 0xFF {
        let m = data[i + 1];
        if m == 0xDA || m == 0xD9 || !(0xC0..=0xFE).contains(&m) {
            break;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let end = (i + 2 + len).min(data.len());
        let body = &data[(i + 4).min(end)..end];
        if !placed && m != 0xE0 {
            out.extend_from_slice(&inserted);
            placed = true;
        }
        let drop = m == 0xE1 && (body.starts_with(b"Exif\0\0") || body.starts_with(XMP_ID));
        if !drop {
            out.extend_from_slice(&data[i..end]);
        }
        i = end;
    }
    if !placed {
        out.extend_from_slice(&inserted);
    }
    out.extend_from_slice(&data[i..]);
    Ok(out)
}

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for p in parts {
        for &b in *p {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
    }
    !c
}

fn png_chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [&(body.len() as u32).to_be_bytes()[..], kind, body, &crc32(&[kind, body]).to_be_bytes()].concat()
}

/// PNG: `eXIf` and/or XMP (`iTXt` "XML:com.adobe.xmp") right after IHDR; an old chunk of the same kind is removed.
fn png_with(data: &[u8], exif: Option<&[u8]>, xmp: Option<&str>) -> Result<Vec<u8>, String> {
    let mut out = PNG_SIG.to_vec();
    for (kind, body, whole) in png_chunks(data)? {
        let old_xmp = kind == b"iTXt" && body.starts_with(PNG_XMP);
        if !((kind == b"eXIf" && exif.is_some()) || (old_xmp && xmp.is_some())) {
            out.extend_from_slice(whole);
        }
        if kind == b"IHDR" {
            if let Some(e) = exif {
                out.extend(png_chunk(b"eXIf", e));
            }
            if let Some(x) = xmp {
                let body = [PNG_XMP, &b"\0\0\0\0"[..], x.as_bytes()].concat();
                out.extend(png_chunk(b"iTXt", &body));
            }
        }
    }
    Ok(out)
}

const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
const PNG_XMP: &[u8] = b"XML:com.adobe.xmp\0";

/// Chunks of a PNG: (kind, body, whole chunk).
fn png_chunks(data: &[u8]) -> Result<Vec<(&[u8], &[u8], &[u8])>, String> {
    if !data.starts_with(PNG_SIG) {
        return Err("not a PNG".into());
    }
    let mut v = Vec::new();
    let mut i = PNG_SIG.len();
    while i + 12 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let end = i.checked_add(12 + len).filter(|e| *e <= data.len()).ok_or("damaged PNG")?;
        v.push((&data[i + 4..i + 8], &data[i + 8..i + 8 + len], &data[i..end]));
        i = end;
    }
    Ok(v)
}

// ─────────────────────────────────────────────────────────── JPEG XL container

/// Signature box of the JPEG XL container (ISO/IEC 18181-2).
const JXL_SIGNATURE: &[u8] = &[0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A];
/// Start of a bare JPEG XL codestream.
const JXL_CODESTREAM: &[u8] = &[0xFF, 0x0A];

/// Is `data` a JPEG XL file (container or bare codestream)?
pub fn is_jxl(data: &[u8]) -> bool {
    data.starts_with(JXL_SIGNATURE) || data.starts_with(JXL_CODESTREAM)
}

/// Boxes of a JPEG XL container: (type, body, whole box).
fn jxl_boxes(data: &[u8]) -> Result<Vec<([u8; 4], &[u8], &[u8])>, String> {
    if !data.starts_with(JXL_SIGNATURE) {
        return Err("not a JPEG XL container".into());
    }
    let mut v = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let head = data.get(i..i + 8).ok_or("damaged JPEG XL")?;
        let kind = [head[4], head[5], head[6], head[7]];
        let (start, end) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            // size 0: the box runs to the end of the file
            0 => (i + 8, data.len()),
            // size 1: 64-bit size follows the type
            1 => {
                let b = data.get(i + 8..i + 16).ok_or("damaged JPEG XL")?;
                let n = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                (i + 16, usize::try_from(n).ok().and_then(|n| i.checked_add(n)).ok_or("damaged JPEG XL")?)
            }
            n => (i + 8, i.checked_add(n as usize).ok_or("damaged JPEG XL")?),
        };
        if end < start || end > data.len() {
            return Err("damaged JPEG XL".into());
        }
        v.push((kind, &data[start..end], &data[i..end]));
        i = end;
    }
    Ok(v)
}

fn jxl_box(kind: &[u8; 4], body: &[u8]) -> Result<Vec<u8>, String> {
    let n = u32::try_from(body.len() + 8).map_err(|_| "metadata too large")?;
    Ok([&n.to_be_bytes()[..], kind, body].concat())
}

/// Start of the codestream of a JPEG XL file (bare, `jxlc` or the first `jxlp` box).
fn jxl_codestream(data: &[u8]) -> Option<&[u8]> {
    if data.starts_with(JXL_CODESTREAM) {
        return Some(data);
    }
    jxl_boxes(data).ok()?.into_iter().find_map(|(k, b, _)| match &k {
        b"jxlc" => Some(b),
        b"jxlp" => b.get(4..),
        _ => None,
    })
}

/// Pixel size of a JPEG XL file (SizeHeader of the codestream, ISO/IEC 18181-1 D.2).
pub fn jxl_size(data: &[u8]) -> Option<(u32, u32)> {
    let cs = jxl_codestream(data)?;
    if !cs.starts_with(JXL_CODESTREAM) {
        return None;
    }
    // bits are read least significant first
    let mut pos = 16usize;
    let mut read = |n: usize| -> Option<u64> {
        let mut v = 0u64;
        for k in 0..n {
            let byte = *cs.get(pos / 8)?;
            v |= (((byte >> (pos % 8)) & 1) as u64) << k;
            pos += 1;
        }
        Some(v)
    };
    let small = read(1)? == 1;
    let dimension = |read: &mut dyn FnMut(usize) -> Option<u64>| -> Option<u64> {
        if small {
            return Some((read(5)? + 1) * 8);
        }
        let bits = [9, 13, 18, 30][read(2)? as usize];
        Some(read(bits)? + 1)
    };
    let height = dimension(&mut read)?;
    let ratio = read(3)?;
    let width = match ratio {
        0 => dimension(&mut read)?,
        r => {
            let (n, d) = [(1, 1), (12, 10), (4, 3), (3, 2), (16, 9), (5, 4), (2, 1)][r as usize - 1];
            height * n / d
        }
    };
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

/// JPEG XL with new `Exif` and `xml ` boxes (old ones of the same kind removed), right after `ftyp` (and the level
/// box). A bare codestream is wrapped into a container first. The codestream itself stays byte for byte.
pub fn jxl_with(data: &[u8], exif: Option<&[u8]>, xmp: Option<&str>) -> Result<Vec<u8>, String> {
    let mut inserted = Vec::new();
    if let Some(e) = exif {
        // Exif box: 4-byte offset to the TIFF header, then the TIFF block
        inserted.extend(jxl_box(b"Exif", &[&[0u8; 4][..], e].concat())?);
    }
    if let Some(x) = xmp {
        inserted.extend(jxl_box(b"xml ", x.as_bytes())?);
    }
    if data.starts_with(JXL_CODESTREAM) {
        let mut out = JXL_SIGNATURE.to_vec();
        out.extend(jxl_box(b"ftyp", b"jxl \0\0\0\0jxl ")?);
        out.extend(inserted);
        out.extend(jxl_box(b"jxlc", data)?);
        return Ok(out);
    }
    let boxes = jxl_boxes(data)?;
    let replaced = |k: &[u8; 4], body: &[u8]| -> bool {
        // also the Brotli-compressed form ("brob" + original type) of a box we replace
        let k = if k == b"brob" { body.get(..4).unwrap_or_default() } else { &k[..] };
        (k == b"Exif" && exif.is_some()) || (k == b"xml " && xmp.is_some())
    };
    let mut out = Vec::with_capacity(data.len() + inserted.len());
    let mut placed = false;
    for (n, (kind, body, whole)) in boxes.iter().enumerate() {
        if !placed && !matches!(kind, b"JXL " | b"ftyp" | b"jxll") {
            out.extend_from_slice(&inserted);
            placed = true;
        }
        if !replaced(kind, body) {
            out.extend_from_slice(whole);
        }
        if n + 1 == boxes.len() && !placed {
            out.extend_from_slice(&inserted);
            placed = true;
        }
    }
    Ok(out)
}

/// Metadata of an exported JPEG XL: EXIF and XMP as boxes of the container, without touching the codestream.
fn write_jxl(target: &Path, original: &Path, f: &ExportFields) -> Result<String, String> {
    let data = std::fs::read(target).map_err(|e| e.to_string())?;
    let (width, height) = jxl_size(&data).ok_or("damaged JPEG XL")?;
    let (exif, xmp, report) = jxl_metadata(target, original, f, width, height)?;
    let out = jxl_with(&data, Some(&exif), xmp.as_deref())?;
    replace_file(target, &out)?;
    Ok(report)
}

/// EXIF block and XMP packet for a JPEG XL: on macOS ImageIO writes them into a small JPEG next to the
/// target (the same function and fields as for the JPEG export), they are taken from there.
#[cfg(target_os = "macos")]
fn jxl_metadata(target: &Path, original: &Path, f: &ExportFields, width: u32, height: u32) -> Result<(Vec<u8>, Option<String>, String), String> {
    let carrier = target.with_file_name(format!(".{}.metadata.jpg", target.file_name().unwrap_or_default().to_string_lossy()));
    let result = (|| {
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8)).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).map_err(|e| e.to_string())?;
        std::fs::write(&carrier, &jpeg).map_err(|e| e.to_string())?;
        let report = crate::imageio::write_metadata(&carrier, original, width, height, f)?;
        let (exif, xmp) = jpeg_app1(&std::fs::read(&carrier).map_err(|e| e.to_string())?);
        Ok((exif.ok_or("no EXIF written")?, xmp, report))
    })();
    let _ = std::fs::remove_file(&carrier);
    result
}

#[cfg(not(target_os = "macos"))]
fn jxl_metadata(_target: &Path, _original: &Path, f: &ExportFields, width: u32, height: u32) -> Result<(Vec<u8>, Option<String>, String), String> {
    let own = OwnFields { fields: f.clone(), width, height };
    Ok((exif_block(&own), xmp_packet(&own), own_report(&own)))
}

/// EXIF (TIFF block) and XMP packet from the APP1 segments of a JPEG.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn jpeg_app1(data: &[u8]) -> (Option<Vec<u8>>, Option<String>) {
    let (mut exif, mut xmp) = (None, None);
    let mut i = 2;
    while i + 4 <= data.len() && data[i] == 0xFF {
        let m = data[i + 1];
        if m == 0xDA || m == 0xD9 {
            break;
        }
        let end = (i + 2 + u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize).min(data.len());
        let body = &data[(i + 4).min(end)..end];
        if m == 0xE1 {
            if let Some(b) = body.strip_prefix(b"Exif\0\0") {
                exif.get_or_insert_with(|| b.to_vec());
            } else if let Some(b) = body.strip_prefix(XMP_ID) {
                xmp.get_or_insert_with(|| String::from_utf8_lossy(b).into_owned());
            }
        }
        i = end;
    }
    (exif, xmp)
}

/// TIFF with an EXIF IFD: DateTimeOriginal and DateTimeDigitized added (a copy of the EXIF IFD is appended,
/// the pointer in IFD0 changed in place; everything else stays where it is). None: nothing to do (no EXIF IFD,
/// date already there).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn tiff_exif_dates(data: &[u8], date: &str) -> Result<Option<Vec<u8>>, String> {
    let le = match data.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err("not a TIFF".into()),
    };
    let e = Enc { le };
    let r16 = |o: usize| data.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let r32 = |o: usize| data.get(o..o + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let entries = |at: usize| -> Option<Vec<(usize, u16, u16, u32)>> {
        let n = r16(at)? as usize;
        (0..n).map(|k| { let p = at + 2 + 12 * k; Some((p, r16(p)?, r16(p + 2)?, r32(p + 4)?)) }).collect()
    };
    let at0 = r32(4).ok_or("damaged TIFF")? as usize;
    let ifd0 = entries(at0).ok_or("damaged TIFF")?;
    let Some(&(pointer_at, ..)) = ifd0.iter().find(|(_, t, ..)| *t == 0x8769) else { return Ok(None) };
    let exif_at = r32(pointer_at + 8).ok_or("damaged TIFF")? as usize;
    let exif = entries(exif_at).ok_or("damaged TIFF")?;
    if exif.iter().any(|(_, t, ..)| *t == 0x9003) {
        return Ok(None);
    }
    let mut new: Vec<(u16, Val)> = exif
        .iter()
        .map(|&(p, tag, typ, count)| {
            let mut b = [0u8; 4];
            b.copy_from_slice(&data[p + 8..p + 12]);
            (tag, Val::Keep(typ, count, b))
        })
        .collect();
    new.push((0x9003, Val::Ascii(date.to_string())));
    new.push((0x9004, Val::Ascii(date.to_string())));
    let mut out = data.to_vec();
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let new_at = out.len() as u32;
    let next = r32(exif_at + 2 + 12 * exif.len()).unwrap_or(0);
    out.extend_from_slice(&e.ifd(new, new_at, next));
    out[pointer_at + 8..pointer_at + 12].copy_from_slice(&e.u32(new_at));
    Ok(Some(out))
}

/// TIFF: new IFD0 (old entries + metadata) and EXIF IFD appended to the end; the header points to the new
/// IFD0. Image data and old values stay where they are.
fn tiff_with(data: &[u8], f: &OwnFields, xmp: Option<&str>) -> Result<Vec<u8>, String> {
    let le = match data.get(..4) {
        Some(b"II*\0") => true,
        Some(b"MM\0*") => false,
        _ => return Err("not a TIFF".into()),
    };
    let e = Enc { le };
    let r16 = |o: usize| data.get(o..o + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let r32 = |o: usize| data.get(o..o + 4).map(|b| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) });
    let at0 = r32(4).ok_or("damaged TIFF")? as usize;
    let n = r16(at0).ok_or("damaged TIFF")? as usize;
    let next = r32(at0 + 2 + 12 * n).ok_or("damaged TIFF")?;
    let (mine0, exif) = ifd0_and_exif(f);
    let mut ifd0: Vec<(u16, Val)> = Vec::new();
    for k in 0..n {
        let p = at0 + 2 + 12 * k;
        let (tag, typ, count) = (r16(p).unwrap_or(0), r16(p + 2).unwrap_or(0), r32(p + 4).unwrap_or(0));
        // replaced: our tags, an old EXIF/GPS pointer and old XMP
        if mine0.iter().any(|(t, _)| *t == tag) || matches!(tag, 0x8769 | 0x8825 | 0x02BC) {
            continue;
        }
        let mut b = [0u8; 4];
        b.copy_from_slice(&data[p + 8..p + 12]);
        ifd0.push((tag, Val::Keep(typ, count, b)));
    }
    ifd0.extend(mine0);
    if let Some(x) = xmp {
        ifd0.push((0x02BC, Val::Undefined(x.as_bytes().to_vec())));
    }
    let mut out = data.to_vec();
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let exif_at = out.len() as u32;
    let exif_bytes = e.ifd(exif, exif_at, 0);
    let ifd0_at = exif_at + exif_bytes.len() as u32 + (exif_bytes.len() as u32 % 2);
    out.extend_from_slice(&exif_bytes);
    if out.len() % 2 == 1 {
        out.push(0);
    }
    ifd0.push((0x8769, Val::Long(exif_at)));
    out.extend_from_slice(&e.ifd(ifd0, ifd0_at, next));
    out[4..8].copy_from_slice(&e.u32(ifd0_at));
    Ok(out)
}

/// Writes metadata into a finished JPEG/JPEG XL/PNG/TIFF without touching the image data (no recompression).
/// Writes to a side file first, then replaces the target. Returns: short report.
pub fn write_own(target: &Path, f: &OwnFields) -> Result<String, String> {
    let data = std::fs::read(target).map_err(|e| e.to_string())?;
    let exif = exif_block(f);
    let xmp = xmp_packet(f);
    let out = if data.starts_with(&[0xFF, 0xD8]) {
        jpeg_with(&data, &exif, xmp.as_deref())?
    } else if data.starts_with(b"\x89PNG") {
        png_with(&data, Some(&exif), xmp.as_deref())?
    } else if is_jxl(&data) {
        jxl_with(&data, Some(&exif), xmp.as_deref())?
    } else {
        tiff_with(&data, f, xmp.as_deref())?
    };
    replace_file(target, &out)?;
    Ok(own_report(f))
}

/// Short report of the own writer.
fn own_report(f: &OwnFields) -> String {
    let mut parts = vec![if f.fields.shot.as_ref().is_some_and(|s| !s.is_empty()) { "shooting data" } else { "software" }.to_string()];
    if f.fields.copyright.is_some() {
        parts.push("copyright".into());
    }
    if f.fields.personal {
        parts.push("stars/keywords".into());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot() -> Shot {
        Shot {
            make: Some("SIGMA".into()),
            model: Some("SIGMA DP2 Merrill".into()),
            lens_make: Some("SIGMA".into()),
            lens: Some("SIGMA 30mm f/2.8".into()),
            date: Some("2014:07:16 14:02:02".into()),
            exposure_time: Some(1.0 / 320.0),
            f_number: Some(7.1),
            iso: Some(100),
            focal_length: Some(30.0),
            focal_length_35mm: Some(45),
            exposure_bias: Some(-0.3),
            exposure_program: Some(2),
        }
    }

    /// Minimal reader for the tests: IFD0 + EXIF IFD of a TIFF block → (tag, text of the value).
    pub(crate) fn read_tags(t: &[u8]) -> Vec<(u16, String)> {
        let le = &t[..2] == b"II";
        let r16 = |o: usize| if le { u16::from_le_bytes([t[o], t[o + 1]]) } else { u16::from_be_bytes([t[o], t[o + 1]]) };
        let r32 = |o: usize| if le { u32::from_le_bytes([t[o], t[o + 1], t[o + 2], t[o + 3]]) } else { u32::from_be_bytes([t[o], t[o + 1], t[o + 2], t[o + 3]]) };
        let mut out = Vec::new();
        let mut todo = vec![r32(4) as usize];
        while let Some(at) = todo.pop() {
            for k in 0..r16(at) as usize {
                let p = at + 2 + 12 * k;
                let (tag, typ, count) = (r16(p), r16(p + 2), r32(p + 4) as usize);
                let size = count * match typ { 3 => 2, 4 => 4, 5 | 10 => 8, _ => 1 };
                let vo = if size <= 4 { p + 8 } else { r32(p + 8) as usize };
                let s = match typ {
                    2 => String::from_utf8_lossy(&t[vo..vo + count]).trim_end_matches('\0').to_string(),
                    3 => r16(vo).to_string(),
                    4 => r32(vo).to_string(),
                    5 => format!("{}/{}", r32(vo), r32(vo + 4)),
                    10 => format!("{}/{}", r32(vo) as i32, r32(vo + 4) as i32),
                    _ => String::from_utf8_lossy(&t[vo..vo + size.min(8)]).to_string(),
                };
                if tag == 0x8769 {
                    todo.push(r32(p + 8) as usize);
                }
                out.push((tag, s));
            }
        }
        out
    }

    pub(crate) fn tag(tags: &[(u16, String)], t: u16) -> Option<String> {
        tags.iter().find(|(k, _)| *k == t).map(|(_, v)| v.clone())
    }

    /// The camera block reads back field by field (round trip), date in EXIF form.
    #[test]
    fn exif_block_round_trip() {
        let f = OwnFields { fields: ExportFields { shot: Some(shot()), copyright: Some("© 2026 Test".into()), ..Default::default() }, width: 640, height: 480 };
        let t = read_tags(&exif_block(&f));
        assert_eq!(tag(&t, 0x010F).as_deref(), Some("SIGMA"));
        assert_eq!(tag(&t, 0x0110).as_deref(), Some("SIGMA DP2 Merrill"));
        assert_eq!(tag(&t, 0x0112).as_deref(), Some("1"));
        assert_eq!(tag(&t, 0x8298).as_deref(), Some("© 2026 Test"));
        assert_eq!(tag(&t, 0x829A).as_deref(), Some("1/320"));
        assert_eq!(tag(&t, 0x829D).as_deref(), Some("71/10"));
        assert_eq!(tag(&t, 0x8827).as_deref(), Some("100"));
        assert_eq!(tag(&t, 0x9003).as_deref(), Some("2014:07:16 14:02:02"));
        assert_eq!(tag(&t, 0x9204).as_deref(), Some("-3/10"));
        assert_eq!(tag(&t, 0x920A).as_deref(), Some("30/1"));
        assert_eq!(tag(&t, 0xA405).as_deref(), Some("45"));
        assert_eq!(tag(&t, 0xA434).as_deref(), Some("SIGMA 30mm f/2.8"));
        assert_eq!(tag(&t, 0xA002).as_deref(), Some("640"));
        assert!(tag(&t, 0x0131).unwrap().starts_with("graineo "));
        // never: serial numbers
        assert!(tag(&t, 0xA431).is_none() && tag(&t, 0xC62F).is_none() && tag(&t, 0xA435).is_none());
    }

    #[test]
    fn x3f_props_to_shot() {
        let p = x3f::PropList {
            pairs: [
                ("CAMMANUF", "SIGMA"),
                ("CAMMODEL", "SIGMA DP2 Merrill"),
                ("CAMSERIAL", "90406517"),
                ("TIME", "1405519322"),
                ("SHUTTER", "0.00301"),
                ("SH_DESC", "1/320"),
                ("APERTURE", "7.02501"),
                ("AP_DESC", "7.1"),
                ("ISO", "100"),
                ("FLENGTH", "30"),
                ("FLEQ35MM", "45"),
                ("EXPCOMP", "+0.30"),
                ("PMODE", "P"),
            ]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        };
        let s = Shot::from_x3f_props(&p);
        assert_eq!(s.date.as_deref(), Some("2014:07:16 14:02:02"));
        assert_eq!(s.lens.as_deref(), Some("SIGMA 30mm f/2.8"));
        assert_eq!(s.f_number, Some(7.1));
        assert_eq!(s.iso, Some(100));
        assert_eq!(s.focal_length_35mm, Some(45));
        assert_eq!(s.exposure_bias, Some(0.3));
        assert_eq!(s.exposure_program, Some(2));
        assert_eq!(exposure_rational(s.exposure_time.unwrap()), (1, 320));
        // interchangeable lens (SD1): from the ranges; aperture range reversed in older models
        let p = x3f::PropList { pairs: vec![("CAMMODEL".into(), "SIGMA SD10".into()), ("LENSFRANGE".into(), "18 to 50".into()), ("LENSARANGE".into(), "22 to 2.8".into()), ("TIME".into(), "2017:02:06 11:55:24".into())] };
        let s = Shot::from_x3f_props(&p);
        assert_eq!(s.lens.as_deref(), Some("18-50mm f/2.8"));
        assert_eq!(s.date.as_deref(), Some("2017:02:06 11:55:24"));
        let props = s.exif_properties();
        assert!(props.contains(&("DateTimeOriginal", Prop::Text("2017:02:06 11:55:24".to_string()))));
    }

    #[test]
    fn long_exposure_is_rational() {
        assert_eq!(exposure_rational(0.32421), (3, 10));
        assert_eq!(exposure_rational(2.5), (25, 10));
        assert_eq!(rat(2.8), (28, 10));
        assert_eq!(srat(-1.0 / 3.0), (-33, 100));
    }

    #[test]
    fn allowed_tags() {
        for p in ["tiff:Make", "exifEX:LensModel", "exif:DateTimeOriginal", "exif:FocalLengthIn35mmFilm"] {
            assert!(allowed_tag(p, false), "{p}");
        }
        for p in ["exifEX:BodySerialNumber", "aux:SerialNumber", "exifEX:LensSerialNumber", "aux:OwnerName", "exifEX:CameraOwnerName", "exif:GPSLatitude", "dc:subject", "xmp:Rating", "tiff:Artist", "photoshop:Headline", "Iptc4xmpExt:PersonInImage", "mwg-rs:Regions"] {
            assert!(!allowed_tag(p, false), "{p}");
        }
        assert!(allowed_tag("exif:GPSLatitude", true));
        assert!(!allowed_tag("exifEX:BodySerialNumber", true));
    }

    /// PNG by hand (signature, IHDR, IEND): enough for the chunk logic.
    fn png_bytes() -> Vec<u8> {
        let ihdr = [0, 0, 0, 4, 0, 0, 0, 3, 8, 2, 0, 0, 0];
        [&b"\x89PNG\r\n\x1a\n"[..], &png_chunk(b"IHDR", &ihdr), &png_chunk(b"IEND", &[])].concat()
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut v = Vec::new();
        let img = image::RgbImage::from_pixel(16, 8, image::Rgb([120, 90, 60]));
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut v), image::ImageFormat::Jpeg).unwrap();
        v
    }

    fn app1_exif(j: &[u8]) -> Option<Vec<u8>> {
        let mut i = 2;
        while i + 4 <= j.len() && j[i] == 0xFF && j[i + 1] != 0xDA {
            let len = u16::from_be_bytes([j[i + 2], j[i + 3]]) as usize;
            let body = &j[i + 4..i + 2 + len];
            if j[i + 1] == 0xE1 && body.starts_with(b"Exif\0\0") {
                return Some(body[6..].to_vec());
            }
            i += 2 + len;
        }
        None
    }

    #[test]
    fn jpeg_keeps_image_and_gets_exif_and_xmp() {
        let j = jpeg_bytes();
        let fields = ExportFields { personal: true, stars: 4, keywords: vec!["Meer & Sand".into()], shot: Some(shot()), ..Default::default() };
        let f = OwnFields { fields, width: 16, height: 8 };
        let out = jpeg_with(&j, &exif_block(&f), xmp_packet(&f).as_deref()).unwrap();
        let t = read_tags(&app1_exif(&out).unwrap());
        assert_eq!(tag(&t, 0xA434).as_deref(), Some("SIGMA 30mm f/2.8"));
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("xmp:Rating=\"4\"") && s.contains("Meer &amp; Sand"));
        // image unchanged: decodes to the same pixels
        let a = image::load_from_memory(&j).unwrap().to_rgb8();
        let b = image::load_from_memory(&out).unwrap().to_rgb8();
        assert_eq!(a, b);
        // written twice: still one EXIF segment
        let again = jpeg_with(&out, &exif_block(&f), None).unwrap();
        assert_eq!(String::from_utf8_lossy(&again).matches("Exif\0\0").count(), 1);
        assert!(!String::from_utf8_lossy(&again).contains("xmp:Rating"));
    }

    /// Keywords with parents in dc:subject and the paths in lr:hierarchicalSubject (only when given).
    #[test]
    fn keyword_hierarchy_in_the_packet() {
        let fields = ExportFields { personal: true, keywords: vec!["Rom".into(), "Italien".into(), "Orte".into()], hierarchy: vec!["Orte|Italien|Rom".into()], ..Default::default() };
        let xmp = xmp_packet(&OwnFields { fields: fields.clone(), width: 1, height: 1 }).unwrap();
        assert!(xmp.contains("<rdf:li>Orte|Italien|Rom</rdf:li>") && xmp.contains("<lr:hierarchicalSubject>") && xmp.contains("<rdf:li>Italien</rdf:li>"));
        let back = crate::xmp::read_text(&xmp).unwrap();
        assert_eq!(back.keywords, vec!["Orte|Italien|Rom".to_string()], "read back: parents are no extra keywords");
        let plain = xmp_packet(&OwnFields { fields: ExportFields { hierarchy: Vec::new(), ..fields }, width: 1, height: 1 }).unwrap();
        assert!(!plain.contains("hierarchicalSubject"));
    }

    /// Review N4: a huge title (Lightroom allows megabytes) neither removes the other metadata nor the copyright.
    #[test]
    fn huge_title_keeps_the_other_metadata() {
        let j = jpeg_bytes();
        let fields = ExportFields {
            personal: true,
            stars: 3,
            copyright: Some("© Test Person".into()),
            title: Some("T".repeat(200_000)),
            description: Some("D".repeat(100_000)),
            keywords: vec!["Meer".into(), "k".repeat(100_000)],
            shot: Some(shot()),
            ..Default::default()
        };
        let f = OwnFields { fields, width: 16, height: 8 };
        let xmp = xmp_packet(&f).unwrap();
        assert!(xmp.len() < 60_000, "{}", xmp.len());
        let out = jpeg_with(&j, &exif_block(&f), Some(&xmp)).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("xmp:Rating=\"3\"") && s.contains("Test Person") && s.contains("Meer"));
        assert_eq!(tag(&read_tags(&app1_exif(&out).unwrap()), 0xA434).as_deref(), Some("SIGMA 30mm f/2.8"));
        // an oversized packet loses only itself
        let huge = "x".repeat(70_000);
        let out = jpeg_with(&j, &exif_block(&f), Some(&huge)).unwrap();
        assert_eq!(tag(&read_tags(&app1_exif(&out).unwrap()), 0xA434).as_deref(), Some("SIGMA 30mm f/2.8"));
    }

    #[test]
    fn png_gets_exif_chunk() {
        let p = png_bytes();
        let f = OwnFields { fields: ExportFields { shot: Some(shot()), ..Default::default() }, width: 4, height: 3 };
        let out = png_with(&p, Some(&exif_block(&f)), None).unwrap();
        let at = out.windows(4).position(|w| w == b"eXIf").unwrap();
        let len = u32::from_be_bytes([out[at - 4], out[at - 3], out[at - 2], out[at - 1]]) as usize;
        let t = read_tags(&out[at + 4..at + 4 + len]);
        assert_eq!(tag(&t, 0x0110).as_deref(), Some("SIGMA DP2 Merrill"));
        assert_eq!(crc32(&[b"IEND"]), 0xAE42_6082);
    }

    #[test]
    fn tiff_gets_new_ifd0_and_keeps_old_entries() {
        // little TIFF by hand: 2×1 RGB 8 bit, one strip
        let e = Enc { le: false };
        let pixels = [1u8, 2, 3, 4, 5, 6];
        let entries = vec![
            (256, Val::Short(2)),
            (257, Val::Short(1)),
            (262, Val::Short(2)),
            (273, Val::Long(8)),
            (277, Val::Short(3)),
            (278, Val::Short(1)),
            (279, Val::Long(6)),
            (0x0131, Val::Ascii("alt".into())),
        ];
        let mut t = b"MM\0*".to_vec();
        t.extend_from_slice(&e.u32(14));
        t.extend_from_slice(&pixels);
        t.extend_from_slice(&e.ifd(entries, 14, 0));
        let f = OwnFields { fields: ExportFields { shot: Some(shot()), copyright: Some("C".into()), ..Default::default() }, width: 2, height: 1 };
        let out = tiff_with(&t, &f, xmp_packet(&f).as_deref()).unwrap();
        let tags = read_tags(&out);
        assert_eq!(tag(&tags, 256).as_deref(), Some("2"));
        assert_eq!(tag(&tags, 273).as_deref(), Some("8"));
        assert!(tag(&tags, 0x0131).unwrap().starts_with("graineo"));
        assert_eq!(tag(&tags, 0x9003).as_deref(), Some("2014:07:16 14:02:02"));
        assert_eq!(&out[8..14], &pixels);
        assert!(tag(&tags, 0x02BC).is_some());
        // a TIFF whose EXIF block lacks the date (as ImageIO writes it) gets it, the rest stays
        let no_date = OwnFields { fields: ExportFields { shot: Some(Shot { date: None, ..shot() }), ..Default::default() }, width: 2, height: 1 };
        let t2 = tiff_with(&t, &no_date, None).unwrap();
        assert_eq!(tag(&read_tags(&t2), 0x9003), None);
        let t3 = tiff_exif_dates(&t2, "2014:07:16 14:02:02").unwrap().unwrap();
        let tags = read_tags(&t3);
        assert_eq!(tag(&tags, 0x9003).as_deref(), Some("2014:07:16 14:02:02"));
        assert_eq!(tag(&tags, 0xA434).as_deref(), Some("SIGMA 30mm f/2.8"));
        assert_eq!(&t3[8..14], &pixels);
        assert!(tiff_exif_dates(&t3, "2014:07:16 14:02:02").unwrap().is_none());
    }

    /// A JPEG XL file of `w` × `h` (lossless, 8 bit) from the encoder of the export.
    fn jxl_bytes(w: usize, h: usize) -> Vec<u8> {
        let px: Vec<u8> = (0..w * h * 3).map(|i| (i * 37 % 251) as u8).collect();
        jixel::encode_image(&px, w, h, &jixel::EncodeConfig::default().with_lossless(true).with_speed(jixel::Speed::Fastest)).unwrap()
    }

    fn jxl_pixels(b: &[u8]) -> Vec<f32> {
        let img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(b)).unwrap();
        img.render_frame(0).unwrap().image_all_channels().buf().to_vec()
    }

    /// Bare codestream of a JPEG XL file (as some encoders write it).
    fn bare(b: &[u8]) -> Vec<u8> {
        if b.starts_with(JXL_CODESTREAM) {
            return b.to_vec();
        }
        jxl_boxes(b).unwrap().iter().find(|(k, ..)| k == b"jxlc").map(|(_, body, _)| body.to_vec()).expect("jxlc box")
    }

    #[test]
    fn jxl_size_from_the_codestream() {
        // small (multiples of 8), the fixed ratios and free sizes in every coding range
        for (w, h) in [(1, 1), (8, 8), (16, 24), (40, 30), (64, 36), (300, 200), (513, 7), (9000, 2), (2, 300_000)] {
            let b = jxl_bytes(w, h);
            assert_eq!(jxl_size(&b), Some((w as u32, h as u32)), "{w}×{h}");
            assert_eq!(jxl_size(&bare(&b)), Some((w as u32, h as u32)), "{w}×{h} bare");
        }
        assert_eq!(jxl_size(b"\xFF\x0A"), None);
        assert_eq!(jxl_size(&jpeg_bytes()), None);
    }

    /// EXIF and XMP as boxes, the codestream unchanged, from a bare codestream and from a container; written
    /// twice: still one box of each kind.
    #[test]
    fn jxl_gets_exif_and_xmp_boxes() {
        let fields = ExportFields { personal: true, stars: 4, keywords: vec!["Meer & Sand".into()], shot: Some(shot()), copyright: Some("© 2026 Test".into()), ..Default::default() };
        let f = OwnFields { fields, width: 40, height: 30 };
        let container = jxl_bytes(40, 30);
        for data in [bare(&container), container.clone()] {
            let pixels = jxl_pixels(&data);
            let out = jxl_with(&data, Some(&exif_block(&f)), xmp_packet(&f).as_deref()).unwrap();
            let out = jxl_with(&out, Some(&exif_block(&f)), xmp_packet(&f).as_deref()).unwrap();
            let boxes = jxl_boxes(&out).unwrap();
            let kinds: Vec<[u8; 4]> = boxes.iter().map(|(k, ..)| *k).collect();
            assert_eq!(kinds.iter().filter(|k| *k == b"Exif").count(), 1, "{kinds:?}");
            assert_eq!(kinds.iter().filter(|k| *k == b"xml ").count(), 1);
            assert_eq!(&kinds[..2], &[*b"JXL ", *b"ftyp"]);
            let exif = boxes.iter().find(|(k, ..)| k == b"Exif").unwrap().1;
            let t = read_tags(&exif[4..]);
            assert_eq!(tag(&t, 0x0110).as_deref(), Some("SIGMA DP2 Merrill"));
            assert_eq!(tag(&t, 0x9003).as_deref(), Some("2014:07:16 14:02:02"));
            assert_eq!(tag(&t, 0x8298).as_deref(), Some("© 2026 Test"));
            assert_eq!(tag(&t, 0xA002).as_deref(), Some("40"));
            let xmp = String::from_utf8_lossy(boxes.iter().find(|(k, ..)| k == b"xml ").unwrap().1).into_owned();
            assert!(xmp.contains("xmp:Rating=\"4\"") && xmp.contains("Meer &amp; Sand"));
            // the reference decoder sees the same image and the metadata boxes
            assert_eq!(jxl_pixels(&out), pixels);
            assert_eq!(jxl_size(&out), Some((40, 30)));
            // without personal fields: the XMP box goes, the EXIF box is replaced
            let shooting = OwnFields { fields: ExportFields { shot: Some(shot()), ..Default::default() }, width: 40, height: 30 };
            let out = jxl_with(&out, Some(&exif_block(&shooting)), None).unwrap();
            assert_eq!(jxl_boxes(&out).unwrap().iter().filter(|(k, ..)| k == b"Exif").count(), 1);
            assert_eq!(jxl_pixels(&out), pixels);
        }
        assert!(jxl_with(&jpeg_bytes(), None, None).is_err());
    }

    /// The export entry point on a .jxl: metadata of the original in the boxes, on macOS through ImageIO (as for the
    /// JPEG export), elsewhere through the own writer.
    #[test]
    fn write_puts_the_shooting_data_into_a_jxl() {
        let dir = std::env::temp_dir().join(format!("graineo-jxl-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("camera.jpg");
        let shot = Shot { make: Some("FUJIFILM".into()), model: Some("X-T5".into()), date: Some("2025:06:01 10:20:30".into()), f_number: Some(1.4), ..Default::default() };
        std::fs::write(&original, jpeg_bytes()).unwrap();
        write_own(&original, &OwnFields { fields: ExportFields { shot: Some(shot.clone()), ..Default::default() }, width: 16, height: 8 }).unwrap();
        let target = dir.join("export.jxl");
        let data = jxl_bytes(64, 48);
        std::fs::write(&target, &data).unwrap();
        let fields = ExportFields { personal: true, stars: 3, shot: Some(shot), copyright: Some("C".into()), ..Default::default() };
        write(&target, &original, &fields).unwrap();
        let out = std::fs::read(&target).unwrap();
        let boxes = jxl_boxes(&out).unwrap();
        let exif = boxes.iter().find(|(k, ..)| k == b"Exif").expect("Exif box").1;
        let t = read_tags(&exif[4..]);
        assert_eq!(tag(&t, 0x010F).as_deref(), Some("FUJIFILM"));
        assert_eq!(tag(&t, 0x0110).as_deref(), Some("X-T5"));
        assert_eq!(tag(&t, 0x9003).as_deref(), Some("2025:06:01 10:20:30"));
        assert_eq!(tag(&t, 0xA002).as_deref(), Some("64"));
        assert_eq!(tag(&t, 0xA003).as_deref(), Some("48"));
        assert_eq!(tag(&t, 0x0112).as_deref(), Some("1"));
        let xmp = String::from_utf8_lossy(boxes.iter().find(|(k, ..)| k == b"xml ").expect("xml box").1).into_owned();
        assert!(xmp.contains("Rating") && xmp.contains('3'), "{xmp}");
        assert_eq!(jxl_pixels(&out), jxl_pixels(&data));
        // nothing left next to the file (carrier JPEG, side file)
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
