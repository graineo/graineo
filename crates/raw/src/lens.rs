//! Lens correction (distortion, lateral chromatic aberration, vignetting) for non-Sigma RAWs.
//!
//! Sources, in this order (per part: geometry or vignetting):
//! 1. **Embedded in the file**
//!    - Fujifilm RAF: FujiIFD tags 0xF00B GeometricDistortionParams, 0xF00F ChromaticAberrationParams,
//!      0xF010 VignettingParams (tag numbers according to Exiv2 `fujimn_int.cpp`/exiftool).
//!    - Sony ARW: SubIFD tags 0x7032 VignettingCorrParams, 0x7035 ChromaticAberrationCorrParams,
//!      0x7037 DistortionCorrParams.
//!    - DNG: opcodes WarpRectilinear (opcode 1) and FixVignetteRadial (opcode 3) from OpcodeList1–3
//!      (DNG specification 1.7, chapter 7).
//!    Interpretation of the Fuji/Sony values following darktable `src/iop/lens.cc` ("embedded metadata", version 2;
//!    method by Freddie Witherden, darktable PR 7092) and `src/common/exif.cc`
//!    (GPL-3.0-or-later, © darktable developers) – rewritten in Rust, formulas adopted:
//!    Fuji: nodes in the source radius, factor 1 + distortion/100, lateral CA 1 + ca,
//!    vignetting value/100; Sony: nodes (i + ½)/(n − 1) in the target radius, factor 1 + d·2⁻¹⁴,
//!    lateral CA 1 + ca·2⁻²¹, vignetting 2^(½ − 2^(v·2⁻¹³ − 1)). Radius 1 = half image diagonal.
//!    Olympus/OM System ORF: makernote ImageProcessing IFD (0x2040) tags 0x150A (distortion
//!    k2, k4, k6, scale) and 0x150C (lateral CA: red k0, k2, k4, blue k0, k2, k4), following
//!    darktable (`src/common/exif.cc` "Olympus distortion/CA correction", `src/iop/lens.cc`
//!    CORRECTION_TYPE_OLYMPUS): Rin/Rout = s·(1 + k2 (r s)² + k4 (r s)⁴ + k6 (r s)⁶) with 1 = corner;
//!    CA on the distorted radius rd: Rin_c = rd·((1 + c0) + c2 rd² + c4 rd⁴). No vignetting data; if the
//!    camera applied ShadingCompensation (CameraSettings 0x050C = 1) the raw is already corrected for
//!    vignetting and lensfun vignetting is skipped (as darktable does).
//!    Panasonic RW2: see [`panasonic`] (distortion only, after darktable). Nikon Z NEF: darktable documents no
//!    embedded correction data for it (as of 11/2026), so none is read; lensfun covers Nikon Z lenses.
//! 2. **lensfun database** ([`crate::lensfun`]) for cameras without embedded data.
//!
//! Application ([`Correction::apply`], in linear light after noise reduction and capture
//! sharpening): first vignetting (function of the source radius), then distortion and lateral CA in
//! *one* resampling step with **Lanczos-3** per channel (overshoot limited to the 2×2 neighborhood).
//! Automatic scaling so that no empty borders appear (largest possible angle of view).
//! Perspective/crop come afterwards (in `develop`).

use rayon::prelude::*;

use crate::lcp;
use crate::lensfun::{self, Database};
use crate::{RawFile, Sensor};

/// Table size for radial functions (radius 0 … `R_MAX`, 1 = half image diagonal)
const N: usize = 1024;
const R_MAX: f32 = 1.6;

/// Radial function in a uniform table, per channel.
#[derive(Debug, Clone)]
pub struct Radial {
    t: Vec<[f32; 3]>,
}

impl Radial {
    fn from_func(f: impl Fn(f32) -> [f32; 3]) -> Self {
        Self { t: (0..N).map(|i| f(i as f32 / (N - 1) as f32 * R_MAX)).collect() }
    }

    #[inline]
    pub fn value(&self, r: f32) -> [f32; 3] {
        let p = (r / R_MAX * (N - 1) as f32).clamp(0.0, (N - 1) as f32 - 1e-3);
        let i = p as usize;
        let f = p - i as f32;
        let (a, b) = (self.t[i], self.t[i + 1]);
        [a[0] + f * (b[0] - a[0]), a[1] + f * (b[1] - a[1]), a[2] + f * (b[2] - a[2])]
    }
}

/// Position of the radial function in the image: center (pixels) and pixels per radius unit.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub center: [f32; 2],
    pub unit: f32,
}

impl Placement {
    fn image_center(w: usize, h: usize) -> Self {
        Self { center: [(w as f32 - 1.0) * 0.5, (h as f32 - 1.0) * 0.5], unit: (w as f32).hypot(h as f32) * 0.5 }
    }
}

#[derive(Debug, Clone)]
pub struct Part {
    pub source: String,
    pub placement: Placement,
    pub f: Radial,
    /// origin for display (short label)
    pub kind: Source,
    /// Adobe LCP model (not radially symmetric: center, tangential terms) – replaces `placement`/`f`
    pub lcp: Option<LcpParam>,
    /// DNG GainMap (vignetting as a gain map) – replaces `placement`/`f`
    pub map: Option<GainMap>,
}

/// Origin of a correction (label in the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// self-measured profile (`lens-profiles/own/`)
    Custom,
    AdobeLcp,
    Backup,
    Camera,
    Lensfun,
    /// only the manual sliders (no profile)
    Manual,
}

impl Source {
    /// Short label, e.g. "Profil: Adobe LCP".
    pub fn short(&self) -> &'static str {
        match self {
            Source::Custom => "Eigenes Profil",
            Source::AdobeLcp => "Adobe LCP",
            Source::Backup => "Sicherung",
            Source::Camera => "Kamera",
            Source::Lensfun => "lensfun",
            Source::Manual => "von Hand",
        }
    }
}

/// DNG opcode GainMap (opcode 9): gain map over relative image coordinates. Several maps
/// (per color filter position) are averaged and applied to the developed image (approximation).
#[derive(Debug, Clone, Default)]
pub struct GainMap {
    pub points: [usize; 2],
    pub spacing: [f64; 2],
    pub map_origin: [f64; 2],
    pub values: Vec<f32>,
    pub full: [f64; 2],
    pub offset: [f64; 2],
    pub strength: f64,
}

impl GainMap {
    /// Gain at (x, y) in the cropped image (row v, column h).
    fn gain(&self, x: f64, y: f64) -> f64 {
        let [nv, nh] = self.points;
        let v = ((y + self.offset[1]) / self.full[1].max(1.0) - self.map_origin[0]) / self.spacing[0].max(1e-9);
        let hh = ((x + self.offset[0]) / self.full[0].max(1.0) - self.map_origin[1]) / self.spacing[1].max(1e-9);
        let v = v.clamp(0.0, (nv - 1) as f64);
        let hh = hh.clamp(0.0, (nh - 1) as f64);
        let (v0, h0) = ((v as usize).min(nv.saturating_sub(2)), (hh as usize).min(nh.saturating_sub(2)));
        let (fv, fh) = (v - v0 as f64, hh - h0 as f64);
        let at = |a: usize, b: usize| self.values[(a.min(nv - 1)) * nh + b.min(nh - 1)] as f64;
        let g = at(v0, h0) * (1.0 - fv) * (1.0 - fh) + at(v0, h0 + 1) * (1.0 - fv) * fh + at(v0 + 1, h0) * fv * (1.0 - fh) + at(v0 + 1, h0 + 1) * fv * fh;
        1.0 + self.strength * (g - 1.0)
    }
}

/// LCP models with their image reference (whole image before cropping).
#[derive(Debug, Clone)]
pub struct LcpParam {
    pub geo: Option<lcp::Model>,
    pub ca: Option<(lcp::Model, lcp::Model)>,
    pub vig: Option<lcp::Vignette>,
    /// width/height of the whole image, offset of the crop within it
    pub full: [f64; 2],
    pub offset: [f64; 2],
    /// pixels per normalized unit / focal length value (max(W, H), with sensor size compensation if needed)
    pub d: f64,
    /// strength (Lightroom setting 0…200 % → 0…2)
    pub strength: f64,
    pub lateral_ca: bool,
}

impl LcpParam {
    /// Source coordinates (R, G, B) in the cropped image for the target coordinate (x, y).
    #[inline]
    fn source(&self, x: f64, y: f64) -> [[f32; 2]; 3] {
        let (xv, yv) = (x + self.offset[0], y + self.offset[1]);
        let [w, h] = self.full;
        let g = self.geo.or(self.ca.map(|c| lcp::Model { k: [0.0; 3], p: [0.0; 2], scale: 1.0, ..c.0 })).expect("LCP without model");
        let (sx, sy) = (g.fx * self.d, g.fy * self.d);
        let (mx, my) = (g.cx * w, g.cy * h);
        let (xn, yn) = ((xv - mx) / sx, (yv - my) / sy);
        let (xg, yg) = if self.geo.is_some() { g.distort(xn, yn) } else { (xn, yn) };
        // strength: scale the displacement
        let (gx, gy) = (xn + self.strength * (xg - xn), yn + self.strength * (yg - yn));
        let px = |a: f64, b: f64| [(mx + a * sx - self.offset[0]) as f32, (my + b * sy - self.offset[1]) as f32];
        let green = px(gx, gy);
        match (&self.ca, self.lateral_ca) {
            (Some((rg, bg)), true) => {
                let red = rg.distort(xg, yg);
                let blue = bg.distort(xg, yg);
                let r = px(gx + rg.scale * red.0 - xg, gy + rg.scale * red.1 - yg);
                let b = px(gx + bg.scale * blue.0 - xg, gy + bg.scale * blue.1 - yg);
                [r, green, b]
            }
            _ => [green; 3],
        }
    }

    /// Light falloff (≤ 1) at the source position (x, y) in the cropped image.
    #[inline]
    fn falloff(&self, x: f64, y: f64, strength: f64) -> f64 {
        let Some(v) = &self.vig else { return 1.0 };
        let (xv, yv) = (x + self.offset[0], y + self.offset[1]);
        let xn = (xv - v.cx * self.full[0]) / (v.fx * self.d);
        let yn = (yv - v.cy * self.full[1]) / (v.fy * self.d);
        // strength in the exponent (0 = off, 2 = double), like an exposure slider in the corners
        v.falloff(xn, yn).max(1e-3).powf(strength)
    }
}

/// Finished correction for an image of a given size.
#[derive(Debug, Clone)]
pub struct Correction {
    pub width: usize,
    pub height: usize,
    /// ratio source radius/target radius per channel over the target radius
    pub geometry: Option<Part>,
    /// light falloff (≤ 1) over the source radius; corrected by dividing
    pub vignette: Option<Part>,
    /// zoom of the automatic scaling (target coordinates are pulled towards the center by it)
    pub zoom: f32,
    /// manual distortion and lateral color ([`LensOptions::manual`]); `None` = none (the profile path unchanged)
    pub manual: Option<[f32; 3]>,
}

/// Which parts are applied (imported Lightroom settings: "Enable Profile Corrections", distortion/vignetting 0…200 %).
#[derive(Debug, Clone, PartialEq)]
pub struct LensOptions {
    pub distortion: f32,
    pub vignette: f32,
    pub lateral_ca: bool,
    /// use lensfun if the file brings nothing
    pub lensfun: bool,
    /// use Adobe lens profiles (LCP) from the local Adobe folders or the private backup
    pub adobe: bool,
    /// additional folder with LCP profiles (settings)
    pub extra_folders: Option<std::path::PathBuf>,
    /// back up used LCP profiles privately automatically (on first use)
    pub backup_enabled: bool,
    /// assigned profile: own `"id@version"`, a lensfun lens `"lensfun:<name>"` ([`PIN_LENSFUN`]) or an Adobe
    /// profile `"lcp:<file name>"` ([`PIN_LCP`]); `Some("")` = explicitly none; `None` = automatic (by lens name)
    pub pinned: Option<String>,
    /// manual corrections with or without a profile: distortion, red and blue lateral color (−100 … +100, 0 = off)
    pub manual: [f32; 3],
}

impl Default for LensOptions {
    fn default() -> Self {
        Self { distortion: 1.0, vignette: 1.0, lateral_ca: true, lensfun: true, adobe: true, extra_folders: None, backup_enabled: true, pinned: None, manual: [0.0; 3] }
    }
}

impl LensOptions {
    /// Do the options ask for anything at all (profile parts or a manual correction)?
    fn anything(&self) -> bool {
        self.distortion != 0.0 || self.vignette != 0.0 || self.lateral_ca || self.has_manual()
    }

    fn has_manual(&self) -> bool {
        self.manual != [0.0; 3]
    }
}

/// Manual distortion: ratio source/target radius at the image corner for slider +100 is 1 − this.
const MANUAL_DISTORTION: f32 = 0.12;
/// Manual lateral color: radius scale of a channel per slider step (±100 → ±0.15 %, ≈ ±5 px in the corner of 24 MP).
const MANUAL_CA: f32 = 1.5e-5;

/// Version of the automatic profile choice (lensfun matching in [`crate::lensfun`], order of the sources). Raise it
/// when the matching changes so that photos may get another profile; together with the database state it forms
/// [`matching_stamp`], and stored previews of photos whose automatic choice changed are developed again.
/// 1: full lensfun database, mount logic, fuzzy names, teleconverters (1.5.5).
pub const MATCHING: u32 = 1;

/// Stamp of the automatic choice: [`MATCHING`] and the commit of the lensfun data in use.
///
/// `None` while the database is still loading in the background (never blocks; for the UI thread).
pub fn matching_stamp() -> Option<String> {
    Some(format!("{MATCHING}/{}", Database::try_active()?.commit))
}

/// Name of the automatically chosen lensfun lens for a library photo (camera model as the library stores it, without
/// the maker; lens name), `""` if none. Used to see which photos change when [`matching_stamp`] changes.
pub fn auto_lensfun_name(camera: &str, lens: Option<&str>) -> String {
    let db = Database::active();
    let Some(cam) = db.camera("", camera) else { return String::new() };
    db.lenses(cam, lens.as_slice()).first().and_then(|o| o.models.first().cloned()).unwrap_or_default()
}

/// Why no profile was found for a lens (UI text, German key): teleconverter, no lens name, camera or lens unknown to
/// lensfun, lensfun knows the lens only for another mount.
pub fn no_profile_reason(maker: &str, model: &str, lens: Option<&str>) -> &'static str {
    let lens = lens.map(str::trim).filter(|l| !l.is_empty());
    if lens.and_then(lensfun::teleconverter).is_some() {
        return "Telekonverter erkannt";
    }
    let db = Database::active();
    let Some(cam) = db.camera(maker, model) else { return "Kamera nicht in der Datenbank" };
    let fixed = !db.mounts.iter().any(|(n, _)| *n == cam.mount);
    let Some(l) = lens else {
        return if fixed { "Objektiv nicht in der Datenbank" } else { "kein Objektivname in der Datei" };
    };
    // lensfun has the lens, but only for a mount that does not fit this camera (or with a larger crop factor)
    if !fixed && db.lenses_any_mount(l).iter().any(|o| !db.mount_fits(Some(cam), &o.mounts)) {
        return "anderes Bajonett";
    }
    "Objektiv nicht in der Datenbank"
}

/// Prefix of a manually chosen lensfun lens in [`LensOptions::pinned`] (followed by its first model name).
pub const PIN_LENSFUN: &str = "lensfun:";
/// Prefix of a manually chosen Adobe profile in [`LensOptions::pinned`] (followed by the LCP file name).
pub const PIN_LCP: &str = "lcp:";

/// lensfun profile for a manually chosen lens (by its first model name), also for an adapted lens of another mount.
/// The camera comes from lensfun if known, otherwise the lens calibration's crop factor stands in.
pub fn lensfun_pinned_profile(maker: &str, model: &str, name: &str, focal_length: Option<f32>, aperture: Option<f32>) -> Option<(lensfun::Profile, String)> {
    let db = Database::active();
    let lenses = db.lens_by_name(name);
    let first = lenses.first()?;
    let cam = db.camera(maker, model).cloned().unwrap_or_else(|| lensfun::Camera { maker: maker.into(), makers: vec![maker.into()], models: vec![model.into()], mount: String::new(), crop: first.crop });
    let fitting: Vec<&lensfun::Lens> = lenses.iter().copied().filter(|o| o.crop <= cam.crop * 1.01).collect();
    let p = lensfun::Profile::determine(&cam, if fitting.is_empty() { &lenses } else { &fitting }, focal_length, aperture)?;
    let name = format!("lensfun: {}", p.name);
    Some((p, name))
}

// ---------------------------------------------------------------------------------------------
// Read TIFF (only the required tags)

struct Tiff<'a> {
    d: &'a [u8],
    base: usize,
    le: bool,
}

#[derive(Clone, Copy)]
struct Entry<'a> {
    tag: u16,
    type_name: u16,
    n: usize,
    data: &'a [u8],
}

impl<'a> Tiff<'a> {
    fn new_at(d: &'a [u8], base: usize) -> Option<Self> {
        let le = match d.get(base..base + 2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Self { d, base, le })
    }

    fn u16(&self, o: usize) -> Option<u16> {
        let b: [u8; 2] = self.d.get(o..o + 2)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }

    fn u32(&self, o: usize) -> Option<u32> {
        let b: [u8; 4] = self.d.get(o..o + 4)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    fn first_ifd(&self) -> Option<usize> {
        self.u32(self.base + 4).map(|v| v as usize)
    }

    /// Entries of an IFD (offset relative to `base`) and offset of the next one.
    fn ifd(&self, off: usize) -> Option<(Vec<Entry<'a>>, usize)> {
        let p = self.base + off;
        let n = self.u16(p)? as usize;
        if n > 4000 {
            return None;
        }
        let mut v = Vec::with_capacity(n);
        for k in 0..n {
            let e = p + 2 + 12 * k;
            let (tag, type_name, cnt) = (self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)? as usize);
            let size = match type_name {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => 1,
            } * cnt;
            let vo = if size <= 4 { e + 8 } else { self.base + self.u32(e + 8)? as usize };
            let Some(data) = self.d.get(vo..vo + size) else { continue };
            v.push(Entry { tag, type_name, n: cnt, data });
        }
        let next = self.u32(p + 2 + 12 * n).unwrap_or(0) as usize;
        Some((v, next))
    }

    fn numbers(&self, e: &Entry) -> Vec<f64> {
        let d = e.data;
        let r16 = |o: usize| if self.le { u16::from_le_bytes([d[o], d[o + 1]]) } else { u16::from_be_bytes([d[o], d[o + 1]]) };
        let r32 = |o: usize| {
            let b = [d[o], d[o + 1], d[o + 2], d[o + 3]];
            if self.le {
                u32::from_le_bytes(b)
            } else {
                u32::from_be_bytes(b)
            }
        };
        (0..e.n)
            .map(|i| match e.type_name {
                3 => r16(2 * i) as f64,
                8 => r16(2 * i) as i16 as f64,
                4 | 13 => r32(4 * i) as f64,
                9 => r32(4 * i) as i32 as f64,
                5 => {
                    let (a, b) = (r32(8 * i), r32(8 * i + 4));
                    if b == 0 { 0.0 } else { a as f64 / b as f64 }
                }
                10 => {
                    let (a, b) = (r32(8 * i) as i32, r32(8 * i + 4) as i32);
                    if b == 0 { 0.0 } else { a as f64 / b as f64 }
                }
                11 => f32::from_bits(r32(4 * i)) as f64,
                _ => d[i] as f64,
            })
            .collect()
    }

    /// Walk all IFDs (chain, SubIFDs, Exif, FujiIFD) and collect the requested tags (first hit).
    fn collect(&self, wanted: &[u16]) -> Vec<(u16, Entry<'a>)> {
        let mut out: Vec<(u16, Entry<'a>)> = Vec::new();
        let mut stack = vec![(self.first_ifd().unwrap_or(0), 0usize)];
        let mut seen = std::collections::HashSet::new();
        while let Some((off, depth)) = stack.pop() {
            if off == 0 || depth > 6 || !seen.insert(off) || seen.len() > 64 {
                continue;
            }
            let Some((entries, next)) = self.ifd(off) else { continue };
            for e in &entries {
                if wanted.contains(&e.tag) && !out.iter().any(|(t, _)| *t == e.tag) {
                    out.push((e.tag, *e));
                }
                if matches!(e.tag, 0x14a | 0x8769 | 0xf000) && matches!(e.type_name, 4 | 13) {
                    for v in self.numbers(e) {
                        stack.push((v as usize, depth + 1));
                    }
                }
            }
            stack.push((next, depth));
        }
        out
    }
}

/// Correction data embedded in the file (raw form).
#[derive(Debug, Clone, Default)]
pub struct Embedded {
    pub source: &'static str,
    /// nodes and factors (R, G, B) for the geometry; `source_radius` = nodes in the source image
    pub geo: Option<(Vec<f32>, Vec<[f32; 3]>, bool)>,
    /// nodes (source radius) and falloff (≤ 1)
    pub vig: Option<(Vec<f32>, Vec<f32>)>,
    /// DNG WarpRectilinear: per plane [kr0..kr3, kt0, kt1], center (relative)
    pub dng_warp: Option<(Vec<[f64; 6]>, [f64; 2])>,
    /// DNG FixVignetteRadial: k0..k4, center (relative)
    pub dng_vig: Option<([f64; 5], [f64; 2])>,
    /// DNG GainMap (averaged over all maps)
    pub dng_gainmap: Option<GainMap>,
    /// the camera already corrected vignetting in the raw data (Olympus ShadingCompensation)
    pub shading_done: bool,
}

impl Embedded {
    pub fn is_empty(&self) -> bool {
        self.geo.is_none() && self.vig.is_none() && self.dng_warp.is_none() && self.dng_vig.is_none() && self.dng_gainmap.is_none()
    }
}

fn fuji(d: &[u8]) -> Option<Embedded> {
    if d.get(..8)? != b"FUJIFILM" {
        return None;
    }
    let off = u32::from_be_bytes(d.get(100..104)?.try_into().ok()?) as usize;
    let t = Tiff::new_at(d, off)?;
    let tags = t.collect(&[0xf00b, 0xf00f, 0xf010]);
    let get = |k: u16| tags.iter().find(|(t2, _)| *t2 == k).map(|(_, e)| t.numbers(e));
    let (dist, ca, vig) = (get(0xf00b)?, get(0xf00f)?, get(0xf010)?);
    // layout as in darktable exif.cc: X-Trans IV/V 19/29/19 values (9 nodes), older 23/31/23 (11)
    let (nodes, dv, car, cab, vv): (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) = if dist.len() == 19 && ca.len() == 29 && vig.len() == 19 {
        let nc = 9;
        let k: Vec<f32> = (0..nc).map(|i| dist[i + 1] as f32).collect();
        if (0..nc).any(|i| ca[i + 1] as f32 != k[i] || vig[i + 1] as f32 != k[i]) {
            return None;
        }
        (k, (0..nc).map(|i| dist[i + 10] as f32).collect(), (0..nc).map(|i| ca[i + 10] as f32).collect(), (0..nc).map(|i| ca[i + 19] as f32).collect(), (0..nc).map(|i| vig[i + 10] as f32).collect())
    } else if dist.len() == 23 && ca.len() == 31 && vig.len() == 23 {
        let nc = 11;
        let k: Vec<f32> = (0..nc).map(|i| dist[i + 1] as f32).collect();
        // lateral CA without the first node (0)
        if (1..nc).any(|i| ca[i] as f32 != k[i] || vig[i + 1] as f32 != k[i]) {
            return None;
        }
        (
            k,
            (0..nc).map(|i| dist[i + 12] as f32).collect(),
            (0..nc).map(|i| if i == 0 { 0.0 } else { ca[i + 10] as f32 }).collect(),
            (0..nc).map(|i| if i == 0 { 0.0 } else { ca[i + 20] as f32 }).collect(),
            (0..nc).map(|i| vig[i + 12] as f32).collect(),
        )
    } else {
        return None;
    };
    // add node 0 without correction (darktable v2)
    let mut k = Vec::new();
    let mut g = Vec::new();
    let mut v = Vec::new();
    if nodes[0] > 0.0 {
        k.push(0.0);
        g.push([1.0; 3]);
        v.push(1.0);
    }
    for i in 0..nodes.len() {
        k.push(nodes[i]);
        let m = 1.0 + dv[i] / 100.0;
        g.push([m * (1.0 + car[i]), m, m * (1.0 + cab[i])]);
        v.push(vv[i] / 100.0);
    }
    Some(Embedded { source: "Fujifilm (eingebettet)", geo: Some((k.clone(), g, true)), vig: Some((k, v)), ..Default::default() })
}

fn sony(d: &[u8]) -> Option<Embedded> {
    let t = Tiff::new_at(d, 0)?;
    let tags = t.collect(&[0x7032, 0x7035, 0x7037]);
    let get = |k: u16| tags.iter().find(|(t2, _)| *t2 == k).map(|(_, e)| t.numbers(e));
    let dist = get(0x7037)?;
    let nc = *dist.first()? as usize;
    if nc < 2 || nc > 16 || dist.len() < nc + 1 {
        return None;
    }
    let ca = get(0x7035);
    let nodes: Vec<f32> = (0..nc).map(|i| (i as f32 + 0.5) / (nc - 1) as f32).collect();
    let g: Vec<[f32; 3]> = (0..nc)
        .map(|i| {
            let m = 1.0 + dist[i + 1] as f32 / 16384.0;
            let (r, b) = match &ca {
                Some(c) if c.first().copied() == Some(2.0 * nc as f64) && c.len() > 2 * nc => {
                    (1.0 + c[i + 1] as f32 / 2_097_152.0, 1.0 + c[nc + i + 1] as f32 / 2_097_152.0)
                }
                _ => (1.0, 1.0),
            };
            [m * r, m, m * b]
        })
        .collect();
    // vignetting: own node count (RX100 III: 16 instead of 11) – same spacing as distortion
    let vig = get(0x7032).and_then(|v| {
        let nv = *v.first()? as usize;
        if nv < 2 || nv > 16 || v.len() < nv + 1 {
            return None;
        }
        let k: Vec<f32> = (0..nv).map(|i| (i as f32 + 0.5) / (nv - 1) as f32).collect();
        let a: Vec<f32> = (0..nv).map(|i| 2f32.powf(0.5 - 2f32.powf(v[i + 1] as f32 / 8192.0 - 1.0))).collect();
        Some((k, a))
    });
    Some(Embedded { source: "Sony (eingebettet)", geo: Some((nodes, g, false)), vig, ..Default::default() })
}

/// Olympus/OM System makernote: `(Tiff with base = makernote start, offset of the main IFD)`.
/// Header "OLYMPUS\0II\x03\0" (IFD at 12) or "OM SYSTEM\0\0\0II\x03\0" (IFD at 16); offsets are
/// relative to the makernote start (exiftool "Olympus" makernote types 2/3).
fn olympus_makernote(d: &[u8]) -> Option<(Tiff<'_>, usize)> {
    let t = Tiff::new_at(d, 0)?;
    let mn = t.collect(&[0x927c]);
    let (_, e) = mn.first()?;
    let start = e.data.as_ptr() as usize - d.as_ptr() as usize;
    let h = e.data;
    let (bo, ifd) = if h.starts_with(b"OLYMPUS\0") {
        (8, 12)
    } else if h.starts_with(b"OM SYSTEM\0") {
        (12, 16)
    } else {
        return None;
    };
    let le = match h.get(bo..bo + 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    Some((Tiff { d, base: start, le }, ifd))
}

/// Sub-IFD of the Olympus makernote (tag `tag`): pointer (LONG/IFD) or embedded block (UNDEFINED).
fn olympus_sub<'a>(t: &Tiff<'a>, entries: &[Entry<'a>], tag: u16) -> Option<Vec<Entry<'a>>> {
    let e = entries.iter().find(|e| e.tag == tag)?;
    let off = match e.type_name {
        4 | 13 => *t.numbers(e).first()? as usize,
        _ => (e.data.as_ptr() as usize).checked_sub(t.d.as_ptr() as usize + t.base)?,
    };
    t.ifd(off).map(|(v, _)| v)
}

fn olympus(d: &[u8]) -> Option<Embedded> {
    let (t, ifd) = olympus_makernote(d)?;
    let (main, _) = t.ifd(ifd)?;
    let mut e = Embedded { source: "Olympus (eingebettet)", ..Default::default() };
    if let Some(cs) = olympus_sub(&t, &main, 0x2020) {
        e.shading_done = cs.iter().find(|x| x.tag == 0x050c).map(|x| t.numbers(x)).is_some_and(|v| v.len() == 1 && v[0] == 1.0);
    }
    let ip = olympus_sub(&t, &main, 0x2040);
    let get = |k: u16| ip.as_ref().and_then(|ip| ip.iter().find(|x| x.tag == k)).map(|x| t.numbers(x));
    // valid if any of the first three is non-zero (no correction = "0 0 0 1"), as in darktable
    let dist = get(0x150a).filter(|v| v.len() == 4 && v[..3].iter().any(|x| *x != 0.0));
    let ca = get(0x150c).filter(|v| v.len() == 6 && v.iter().any(|x| *x != 0.0));
    if dist.is_some() || ca.is_some() {
        let (s, k2, k4, k6) = dist.as_ref().map_or((1.0, 0.0, 0.0, 0.0), |v| (v[3] as f32, v[0] as f32, v[1] as f32, v[2] as f32));
        let c: [f32; 6] = ca.as_ref().map_or([0.0; 6], |v| std::array::from_fn(|i| v[i] as f32));
        let n = 129;
        let nodes: Vec<f32> = (0..n).map(|i| i as f32 / (n - 1) as f32 * R_MAX).collect();
        let g = nodes
            .iter()
            .map(|&r| {
                let rs2 = (r * s).powi(2);
                let m = s * (1.0 + rs2 * (k2 + rs2 * (k4 + rs2 * k6)));
                let rd = m * r;
                let rd2 = rd * rd;
                let (fr, fb) = if r > 0.0 { (rd * (c[0] + rd2 * (c[1] + rd2 * c[2])) / r, rd * (c[3] + rd2 * (c[4] + rd2 * c[5])) / r) } else { (c[0] * m, c[3] * m) };
                [m + fr, m, m + fb]
            })
            .collect();
        e.geo = Some((nodes, g, false));
    }
    (!e.is_empty() || e.shading_done).then_some(e)
}

/// Panasonic/Leica RW2/RWL: IFD0 tag 0x0119 (DistortionInfo, 16 signed 16-bit values, little-endian) and the
/// makernote tag 0x008a (1 = the camera already compensated vignetting in the raw data), following darktable
/// (`src/common/exif.cc` "Panasonic distortion correction", `src/iop/lens.cc` CORRECTION_TYPE_PANASONIC;
/// GPL-3.0-or-later, © darktable developers; layout after ExifTool PanasonicRaw and the notes of
/// trou/panasonic-rw2) – rewritten in Rust: enabled if the low nibble of value 7 is 1; scale = 1/(1 + v5/32768),
/// a = v8/32768, b = v4/32768, c = v11/32768; Ru = Rd·(1 + scale·(a Rd² + b Rd⁴ + c Rd⁶)) with 1 = half image
/// diagonal (Rd source, Ru target), inverted by fixed-point iteration. No lateral CA or vignetting data.
/// Scale of the Panasonic distortion model, `None` if the correction is off (low nibble of value 7 not 1) or the
/// denominator 1 + v5/32768 is not positive (v5 = −32768 would divide by zero, below that the sign flips).
fn panasonic_scale(v7: f32, v5: f32) -> Option<f32> {
    let denom = 1.0 + v5 / 32768.0;
    ((v7 as i32) & 0x0f == 1 && denom > 0.0).then(|| 1.0 / denom)
}

fn panasonic(d: &[u8]) -> Option<Embedded> {
    let t = Tiff::new_at(d, 0)?;
    let tags = t.collect(&[0x0119, 0x927c]);
    let mut e = Embedded { source: "Panasonic (eingebettet)", ..Default::default() };
    if let Some((_, mn)) = tags.iter().find(|(k, _)| *k == 0x927c) {
        // "Panasonic\0\0\0", IFD at 12, offsets relative to the TIFF start
        if mn.data.starts_with(b"Panasonic\0") {
            let start = mn.data.as_ptr() as usize - d.as_ptr() as usize;
            if let Some((entries, _)) = t.ifd(start + 12) {
                e.shading_done = entries.iter().find(|x| x.tag == 0x008a).map(|x| t.numbers(x)).is_some_and(|v| v.len() == 1 && v[0] == 1.0);
            }
        }
    }
    if let Some((_, di)) = tags.iter().find(|(k, _)| *k == 0x0119).filter(|(_, x)| x.data.len() == 32) {
        let v: Vec<f32> = di.data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32).collect();
        if let Some(sc) = panasonic_scale(v[7], v[5]) {
            let (a, b, c) = (v[8] / 32768.0, v[4] / 32768.0, v[11] / 32768.0);
            let n = 129;
            let nodes: Vec<f32> = (0..n).map(|i| i as f32 / (n - 1) as f32 * R_MAX).collect();
            let g = nodes
                .iter()
                .map(|&r| {
                    let mut rd = r;
                    for _ in 0..4 {
                        let rd2 = rd * rd;
                        let f = 1.0 + sc * rd2 * (a + rd2 * (b + rd2 * c));
                        rd = if f > 1e-6 { r / f } else { r };
                    }
                    let m = if r > 0.0 { rd / r } else { 1.0 };
                    [m; 3]
                })
                .collect();
            e.geo = Some((nodes, g, false));
        }
    }
    (!e.is_empty() || e.shading_done).then_some(e)
}

fn dng(d: &[u8]) -> Option<Embedded> {
    let t = Tiff::new_at(d, 0)?;
    let tags = t.collect(&[51008, 51009, 51022]);
    let mut e = Embedded { source: "DNG-Opcodes", ..Default::default() };
    for (_, entry) in tags {
        let b = entry.data;
        let be32 = |o: usize| -> Option<u32> { Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
        let read_be_f64 = |o: usize| -> Option<f64> { Some(f64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?)) };
        let Some(n) = be32(0) else { continue };
        let mut p = 4;
        for _ in 0..n.min(64) {
            let (Some(id), Some(size)) = (be32(p), be32(p + 12)) else { break };
            let dd = p + 16;
            match id {
                1 => {
                    if let Some(planes) = be32(dd) {
                        let planes = planes as usize;
                        let mut ks = Vec::new();
                        for pl in 0..planes.min(4) {
                            let k: Option<Vec<f64>> = (0..6).map(|j| read_be_f64(dd + 4 + (pl * 6 + j) * 8)).collect();
                            if let Some(k) = k {
                                ks.push([k[0], k[1], k[2], k[3], k[4], k[5]]);
                            }
                        }
                        let c = [read_be_f64(dd + 4 + planes * 48), read_be_f64(dd + 12 + planes * 48)];
                        if let (false, [Some(cx), Some(cy)]) = (ks.is_empty(), c) {
                            e.dng_warp = Some((ks, [cx, cy]));
                        }
                    }
                }
                9 => {
                    // Top, Left, Bottom, Right, Plane, Planes, RowPitch, ColPitch, PointsV, PointsH (u32),
                    // SpacingV, SpacingH, OriginV, OriginH (f64), MapPlanes (u32), values (f32)
                    let u = |k: usize| be32(dd + 4 * k).map(|v| v as usize);
                    let f = |k: usize| read_be_f64(dd + 40 + 8 * k);
                    if let (Some(nv), Some(nh), Some(sv), Some(sh), Some(ov), Some(oh), Some(mp)) = (u(8), u(9), f(0), f(1), f(2), f(3), be32(dd + 72)) {
                        let mp = mp.max(1) as usize;
                        let n = nv * nh;
                        if n > 0 && n < 1 << 20 {
                            let values: Option<Vec<f32>> = (0..n)
                                .map(|j| {
                                    let s: Option<f32> = (0..mp).map(|q| b.get(dd + 76 + 4 * (j * mp + q)..dd + 80 + 4 * (j * mp + q)).map(|x| f32::from_be_bytes(x.try_into().unwrap()))).sum();
                                    s.map(|v| v / mp as f32)
                                })
                                .collect();
                            if let Some(values) = values {
                                let new_at = GainMap { points: [nv, nh], spacing: [sv, sh], map_origin: [ov, oh], values, ..Default::default() };
                                // several maps (per color filter position): average
                                e.dng_gainmap = Some(match e.dng_gainmap.take() {
                                    Some(mut old) if old.values.len() == new_at.values.len() => {
                                        old.values.iter_mut().zip(&new_at.values).for_each(|(a, b)| *a = 0.5 * (*a + b));
                                        old
                                    }
                                    _ => new_at,
                                });
                            }
                        }
                    }
                }
                3 => {
                    let v: Option<Vec<f64>> = (0..7).map(|j| read_be_f64(dd + j * 8)).collect();
                    if let Some(v) = v {
                        e.dng_vig = Some(([v[0], v[1], v[2], v[3], v[4]], [v[5], v[6]]));
                    }
                }
                _ => {}
            }
            p = dd + size as usize;
        }
    }
    (!e.is_empty()).then_some(e)
}

/// Canon: focus distance (meters, mean of FocusDistanceUpper/-Lower) from the makernote
/// (tag 0x0093 FileInfo, values 20/21 in cm, 0xFFFF = infinity; tag layout according to exiftool
/// "Canon FileInfo"). CR3: box CMT3; CR2: EXIF → MakerNote. For choosing the LCP entry.
fn canon_focus(d: &[u8], format: &str) -> Option<f32> {
    let (t, ifd) = if format == "CR3" {
        let i = d.get(..4_000_000.min(d.len()))?.windows(4).position(|w| w == b"CMT3")? + 4;
        let t = Tiff::new_at(d, i)?;
        let o = t.first_ifd()?;
        (t, o)
    } else {
        let t = Tiff::new_at(d, 0)?;
        let mn = t.collect(&[0x927c]);
        let (_, e) = mn.first()?;
        let off = e.data.as_ptr() as usize - d.as_ptr() as usize;
        (t, off)
    };
    let (entries, _) = t.ifd(ifd)?;
    let fi = entries.iter().find(|e| e.tag == 0x93)?;
    let v = t.numbers(fi);
    let (o, u) = (*v.get(20)? as u16, *v.get(21)? as u16);
    if o == 0 || o == 0xffff {
        return if o == 0xffff { Some(1000.0) } else { None };
    }
    Some(0.5 * (o as f32 + u.max(1) as f32) / 100.0)
}

fn spline(k: &[f32], y: &[f32], x: f32) -> f32 {
    if x <= k[0] {
        return y[0];
    }
    for i in 1..k.len() {
        if x <= k[i] {
            let t = (x - k[i - 1]) / (k[i] - k[i - 1]).max(1e-9);
            return y[i - 1] + t * (y[i] - y[i - 1]);
        }
    }
    *y.last().unwrap_or(&1.0)
}

impl RawFile {
    /// Embedded correction data (Fuji/Sony/Olympus/Panasonic/DNG) – None if there is none.
    pub fn lens_embedded(&self) -> Option<Embedded> {
        let d = self.source.buf();
        match self.info.format.as_str() {
            "RAF" => fuji(d),
            "ARW" => sony(d),
            "ORF" => olympus(d),
            "RW2" | "RWL" => panasonic(d),
            "DNG" => dng(d),
            _ => None,
        }
    }

    /// Lens correction for the developed (cropped) image of size `w`×`h`.
    ///
    /// Sources in this order; per part (geometry, vignetting) the first hit wins:
    /// 0. own, self-measured profiles (`lens-profiles/own/`),
    /// 1. local Adobe LCP profiles (read at runtime), automatically backed up privately along the way
    ///    (exception: Fujifilm/Sony/Olympus with a built-in profile → 3.),
    /// 2. private backup (`~/Library/Application Support/graineo/lens-profiles/`),
    /// 3. correction data from the camera (DNG opcodes, Fuji/Sony/Olympus),
    /// 4. lensfun database.
    /// If nothing is found, the correction stays off (None).
    pub fn lens_correction(&self, w: usize, h: usize, o: &LensOptions) -> Option<Correction> {
        // linear DNG (e.g. Apple ProRAW) is already corrected; Canon sRAW (linear, CR2) however is not
        // linear DNG (already corrected): only the manual sliders
        if self.info.sensor == Sensor::Linear && self.info.format == "DNG" {
            return o.has_manual().then(|| Correction::manual_only(w, h, o.manual));
        }
        if !o.anything() {
            return None;
        }
        let i = &self.info;
        let img = Placement::image_center(w, h);
        let full = i.active.unwrap_or(crate::Region { x: 0, y: 0, w: i.width, h: i.height });
        let z = i.crop.unwrap_or(full);
        let offset = [z.x.saturating_sub(full.x) as f64, z.y.saturating_sub(full.y) as f64];
        // DNG opcodes refer to the whole image before the DefaultCrop (active area)
        let dng_placement = |c: [f64; 2]| -> Placement {
            let mx = c[0] as f32 * (full.w as f32 - 1.0);
            let my = c[1] as f32 * (full.h as f32 - 1.0);
            let dx = mx.max(full.w as f32 - 1.0 - mx);
            let dy = my.max(full.h as f32 - 1.0 - my);
            Placement { center: [mx - offset[0] as f32, my - offset[1] as f32], unit: dx.hypot(dy).max(1.0) }
        };
        let mut geometry: Option<Part> = None;
        let mut vignette: Option<Part> = None;
        let dv = o.distortion;
        let qf = o.lateral_ca;
        // fine adjustment as in darktable: factor 1 + s·(f − 1)
        let mix = |f: [f32; 3]| -> [f32; 3] {
            let g = 1.0 + dv * (f[1] - 1.0);
            if qf {
                [g * f[0] / f[1].max(1e-6), g, g * f[2] / f[1].max(1e-6)]
            } else {
                [g; 3]
            }
        };
        let part = |source: String, kind: Source, placement: Placement, f: Radial| Part { source, kind, placement, f, lcp: None, map: None };
        let embedded = self.lens_embedded().unwrap_or_default();
        // exception: Fujifilm/Sony bring a built-in profile; the reference exports then use
        // that one (measured: X100 previews match the embedded data, not the LCP).
        let camera_first = matches!(i.format.as_str(), "RAF" | "ARW" | "ORF" | "RW2" | "RWL") && embedded.geo.is_some();
        // 0. own profiles, 1./2. Adobe LCP or private backup
        let obj = i.exif.lens.as_deref();
        // assigned profile: found by its reference; missing or switched off → no own profile (camera/lensfun/none follow)
        let pin_lensfun = o.pinned.as_deref().and_then(|p| p.strip_prefix(PIN_LENSFUN));
        let pin_lcp = o.pinned.as_deref().and_then(|p| p.strip_prefix(PIN_LCP));
        let own = match o.pinned.as_deref() {
            _ if pin_lensfun.is_some() || pin_lcp.is_some() => None,
            Some(r) => lensprof::ProfileRef::parse(r).and_then(|r| lensprof::store::load(&r)),
            None => lensprof::store::find_auto(&i.maker, &i.model, obj),
        };
        let custom_profile = own
            .filter(|s| !s.profile.entries.is_empty() && s.profile.covers(i.exif.focal_length))
            .map(|s| (s.profile.clone(), format!("{}.json", s.id), Source::Custom));
        if custom_profile.is_some() || pin_lcp.is_some() || (o.adobe && !camera_first && pin_lensfun.is_none()) {
            let found: Option<(lcp::Profile, String, Source)> = if custom_profile.is_some() { custom_profile } else if let Some(f) = pin_lcp {
                // manual choice: exactly this file (whatever lens or camera it was made for)
                lcp::by_file(f, o.extra_folders.as_deref()).and_then(|h| lcp::load(&h.path)).map(|p| (p, f.to_string(), Source::AdobeLcp))
            } else { match lcp::search(&i.maker, &i.model, obj, o.extra_folders.as_deref()) {
                Some(header) => lcp::load(&header.path).filter(|p| p.covers(i.exif.focal_length)).map(|p| {
                    if o.backup_enabled {
                        // silent: errors while backing up do not prevent the correction
                        let _ = lcp::write_backup(&i.maker, &i.model, obj, &header, &p, false);
                    }
                    let file = header.path.file_name().and_then(|f| f.to_str()).unwrap_or("").to_string();
                    (p, file, Source::AdobeLcp)
                }),
                None => None,
            }
            .or_else(|| lcp::load_backup(&i.maker, &i.model, obj).filter(|s| s.profile.covers(i.exif.focal_length)).map(|s| (s.profile, s.origin, Source::Backup))) };
            if let Some((prof, file, kind)) = found {
                let distance = i.exif.distance.or_else(|| if i.format.starts_with("CR") { canon_focus(self.source.buf(), &i.format) } else { None });
                let a = prof.select(i.exif.focal_length, i.exif.aperture, distance);
                // sensor size: only compensate for a clearly different crop factor (profile of another camera)
                let crop_img = Database::active().camera(&i.maker, &i.model).map(|k| k.crop as f64).unwrap_or(a.crop);
                let compensation = if (crop_img / a.crop - 1.0).abs() > 0.1 { crop_img / a.crop } else { 1.0 };
                let basis = LcpParam {
                    geo: a.geo,
                    ca: a.ca,
                    vig: a.vig,
                    full: [full.w as f64, full.h as f64],
                    offset,
                    d: full.w.max(full.h) as f64 * compensation,
                    strength: o.distortion as f64,
                    lateral_ca: o.lateral_ca,
                };
                let name = format!("{}: {} ({file})", kind.short(), a.name);
                if a.geo.is_some() || a.ca.is_some() {
                    let mut t = part(name.clone(), kind, img, Radial::from_func(|_| [1.0; 3]));
                    t.lcp = Some(LcpParam { vig: None, ..basis.clone() });
                    geometry = Some(t);
                }
                if a.vig.is_some() && !embedded.shading_done {
                    let mut t = part(name, kind, img, Radial::from_func(|_| [1.0; 3]));
                    t.lcp = Some(LcpParam { strength: o.vignette as f64, ..basis });
                    vignette = Some(t);
                }
            }
        }
        // manually chosen lensfun lens: before the camera data
        if let Some(name) = pin_lensfun {
            if let Some((p, label)) = lensfun_pinned_profile(&i.maker, &i.model, name, i.exif.focal_length, i.exif.aperture) {
                if geometry.is_none() && (p.distortion.is_some() || p.lateral_ca.is_some()) {
                    let pp = p.clone();
                    geometry = Some(part(label.clone(), Source::Lensfun, img, Radial::from_func(|r| mix(pp.geometry(r)))));
                }
                if vignette.is_none() && p.vignette.is_some() && !embedded.shading_done {
                    let s = o.vignette;
                    vignette = Some(part(label, Source::Lensfun, img, Radial::from_func(|r| [1.0 - s * (1.0 - p.falloff(r)); 3])));
                }
            }
        }
        // 3. camera
        if geometry.is_none() || vignette.is_none() {
            if geometry.is_none() {
                if let Some((k, g, source_radius)) = &embedded.geo {
                    let f = if *source_radius {
                        // nodes in the source radius → convert to the target radius (darktable v2)
                        let fine_table: Vec<(f32, [f32; 3])> = (0..2048)
                            .map(|j| {
                                let rin = j as f32 / 2047.0 * R_MAX;
                                let gg: [f32; 3] = std::array::from_fn(|c| spline(k, &g.iter().map(|v| v[c]).collect::<Vec<_>>(), rin));
                                (rin / gg[1].max(1e-6), gg)
                            })
                            .collect();
                        Radial::from_func(|r| {
                            let idx = fine_table.partition_point(|p| p.0 < r).min(fine_table.len() - 1);
                            mix(fine_table[idx].1)
                        })
                    } else {
                        Radial::from_func(|r| mix(std::array::from_fn(|c| spline(k, &g.iter().map(|v| v[c]).collect::<Vec<_>>(), r))))
                    };
                    geometry = Some(part(embedded.source.to_string(), Source::Camera, img, f));
                } else if let Some((ks, c)) = &embedded.dng_warp {
                    // radial part only (tangential terms kt0/kt1 are 0 in our own files)
                    let ks = ks.clone();
                    let f = Radial::from_func(|r| {
                        let r2 = (r * r) as f64;
                        let ev = |k: &[f64; 6]| (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * k[3]))) as f32;
                        // one plane applies to all channels, otherwise per channel (R, G, B)
                        mix(if ks.len() >= 3 { [ev(&ks[0]), ev(&ks[1]), ev(&ks[2])] } else { [ev(&ks[0]); 3] })
                    });
                    geometry = Some(part("DNG WarpRectilinear".into(), Source::Camera, dng_placement(*c), f));
                }
            }
            if vignette.is_none() {
                let s = o.vignette;
                if let Some((k, v)) = &embedded.vig {
                    vignette = Some(part(embedded.source.to_string(), Source::Camera, img, Radial::from_func(|r| [1.0 - s * (1.0 - spline(k, v, r)); 3])));
                } else if let Some((k, c)) = &embedded.dng_vig {
                    let k = *k;
                    vignette = Some(part(
                        "DNG FixVignetteRadial".into(),
                        Source::Camera,
                        dng_placement(*c),
                        Radial::from_func(|r| {
                            let r2 = (r * r) as f64;
                            let g = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
                            [1.0 - s * (1.0 - 1.0 / g as f32); 3]
                        }),
                    ));
                } else if let Some(mut gm) = embedded.dng_gainmap.clone() {
                    gm.full = [full.w as f64, full.h as f64];
                    gm.offset = offset;
                    gm.strength = s as f64;
                    let mut t = part("DNG GainMap".into(), Source::Camera, img, Radial::from_func(|_| [1.0; 3]));
                    t.map = Some(gm);
                    vignette = Some(t);
                }
            }
        }
        // 4. lensfun
        if o.lensfun && pin_lensfun.is_none() && (geometry.is_none() || vignette.is_none()) {
            let db = Database::active();
            if let Some(cam_entry) = db.camera(&i.maker, &i.model) {
                let names: Vec<&str> = [i.exif.lens.as_deref(), i.exif.lens_db.as_deref()].into_iter().flatten().collect();
                let obj = db.lenses(cam_entry, &names);
                if let Some(p) = lensfun::Profile::determine(cam_entry, &obj, i.exif.focal_length, i.exif.aperture) {
                    let name = format!("lensfun: {}", p.name);
                    if geometry.is_none() && (p.distortion.is_some() || p.lateral_ca.is_some()) {
                        let pp = p.clone();
                        geometry = Some(part(name.clone(), Source::Lensfun, img, Radial::from_func(|r| mix(pp.geometry(r)))));
                    }
                    if vignette.is_none() && p.vignette.is_some() && !embedded.shading_done {
                        let s = o.vignette;
                        vignette = Some(part(name, Source::Lensfun, img, Radial::from_func(|r| [1.0 - s * (1.0 - p.falloff(r)); 3])));
                    }
                }
            }
        }
        if o.distortion == 0.0 && !o.lateral_ca {
            geometry = None;
        }
        if o.vignette == 0.0 {
            vignette = None;
        }
        Correction::finish(w, h, geometry, vignette, o.manual)
    }
}

// ---------------------------------------------------------------------------------------------
// Apply

// Lanczos-3 table and sampling: `lensprof::resample` (moved 10.11.2026)
use lensprof::resample::{lanczos_table, sample};

impl Correction {
    /// Finished correction from the profile parts and the manual sliders; `None` if nothing is left.
    fn finish(w: usize, h: usize, mut geometry: Option<Part>, vignette: Option<Part>, manual: [f32; 3]) -> Option<Correction> {
        let manual = (manual != [0.0; 3]).then_some(manual);
        if manual.is_some() && geometry.is_none() {
            geometry = Some(Part { source: Source::Manual.short().into(), kind: Source::Manual, placement: Placement::image_center(w, h), f: Radial::from_func(|_| [1.0; 3]), lcp: None, map: None });
        }
        if geometry.is_none() && vignette.is_none() {
            return None;
        }
        let mut k = Correction { width: w, height: h, geometry, vignette, zoom: 1.0, manual };
        k.zoom = k.auto_zoom();
        Some(k)
    }

    /// Only the manual sliders (no profile).
    fn manual_only(w: usize, h: usize, manual: [f32; 3]) -> Correction {
        Self::finish(w, h, None, None, manual).expect("manual correction")
    }

    /// Source coordinate per channel for the target pixel (x, y), with the manual sliders around the profile.
    #[inline]
    fn source(&self, g: &Part, x: f32, y: f32) -> [[f32; 2]; 3] {
        let Some(m) = self.manual else { return self.source_profile(g, x, y) };
        let (cx, cy) = ((self.width as f32 - 1.0) * 0.5, (self.height as f32 - 1.0) * 0.5);
        let unit = (self.width as f32).hypot(self.height as f32) * 0.5;
        // distortion: target → intermediate radius, before the zoom and the profile
        let (dx, dy) = (x - cx, y - cy);
        let r2 = (dx * dx + dy * dy) / (unit * unit);
        let f = 1.0 - m[0] / 100.0 * MANUAL_DISTORTION * r2;
        let mut q = self.source_profile(g, cx + dx * f, cy + dy * f);
        // lateral color: scale the red and blue source points around the centre
        for (c, v) in [(0, m[1]), (2, m[2])] {
            if v != 0.0 {
                let s = 1.0 + v * MANUAL_CA;
                q[c] = [cx + (q[c][0] - cx) * s, cy + (q[c][1] - cy) * s];
            }
        }
        q
    }

    /// Source coordinate per channel of the profile alone.
    #[inline]
    fn source_profile(&self, g: &Part, x: f32, y: f32) -> [[f32; 2]; 3] {
        if let Some(l) = &g.lcp {
            let (mx, my) = ((self.width as f32 - 1.0) * 0.5, (self.height as f32 - 1.0) * 0.5);
            return l.source((mx + (x - mx) * self.zoom) as f64, (my + (y - my) * self.zoom) as f64);
        }
        let m = g.placement.center;
        let dx = (x - m[0]) * self.zoom;
        let dy = (y - m[1]) * self.zoom;
        let r = dx.hypot(dy) / g.placement.unit;
        let f = g.f.value(r);
        std::array::from_fn(|c| [m[0] + dx * f[c], m[1] + dy * f[c]])
    }

    /// Largest zoom (≤ 1.5) at which all border points of the target image land in the source image.
    fn auto_zoom(&self) -> f32 {
        let Some(g) = &self.geometry else { return 1.0 };
        let (w, h) = (self.width as f32, self.height as f32);
        let mut border = Vec::new();
        for k in 0..=200 {
            let t = k as f32 / 200.0;
            border.extend_from_slice(&[[t * (w - 1.0), 0.0], [t * (w - 1.0), h - 1.0], [0.0, t * (h - 1.0)], [w - 1.0, t * (h - 1.0)]]);
        }
        let inner = |z: f32| -> bool {
            let k = Correction { zoom: z, ..self.clone_without_tables() };
            border.iter().all(|p| k.source(g, p[0], p[1]).iter().all(|q| q[0] >= -0.5 && q[1] >= -0.5 && q[0] <= w - 0.5 && q[1] <= h - 0.5))
        };
        let (mut lo, mut hi) = (0.5f32, 1.5f32);
        if !inner(lo) {
            return 1.0;
        }
        for _ in 0..30 {
            let m = 0.5 * (lo + hi);
            if inner(m) {
                lo = m;
            } else {
                hi = m;
            }
        }
        lo
    }

    fn clone_without_tables(&self) -> Correction {
        Correction { width: self.width, height: self.height, geometry: None, vignette: None, zoom: self.zoom, manual: self.manual }
    }

    /// Apply the correction to a linear RGB image (size must match).
    pub fn apply(&self, px: &mut Vec<[f32; 3]>, w: usize, h: usize) {
        assert_eq!((w, h), (self.width, self.height), "image size does not match the correction");
        self.apply_scaled(px, w, h);
    }

    /// Apply to a downscaled image of the same region (preview): `w`×`h` covers the
    /// whole image of the correction (`width`×`height`); coordinates are converted.
    pub fn apply_scaled(&self, px: &mut Vec<[f32; 3]>, w: usize, h: usize) {
        let (sx, sy) = (self.width as f32 / w.max(1) as f32, self.height as f32 / h.max(1) as f32);
        // center of the preview pixel in full pixels and back
        let is_unit = sx == 1.0 && sy == 1.0;
        let full = |x: usize, y: usize| if is_unit { (x as f32, y as f32) } else { ((x as f32 + 0.5) * sx - 0.5, (y as f32 + 0.5) * sy - 0.5) };
        if let Some(gm) = self.vignette.as_ref().and_then(|v| v.map.as_ref()) {
            px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, p) in row.iter_mut().enumerate() {
                    let (fx, fy) = full(x, y);
                    let k = gm.gain(fx as f64, fy as f64) as f32;
                    p[0] *= k;
                    p[1] *= k;
                    p[2] *= k;
                }
            });
        } else if let Some(l) = self.vignette.as_ref().and_then(|v| v.lcp.as_ref()) {
            px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, p) in row.iter_mut().enumerate() {
                    let (fx, fy) = full(x, y);
                    let k = (1.0 / l.falloff(fx as f64, fy as f64, l.strength)) as f32;
                    p[0] *= k;
                    p[1] *= k;
                    p[2] *= k;
                }
            });
        } else if let Some(v) = &self.vignette {
            let m = v.placement.center;
            let e = v.placement.unit;
            px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                for (x, p) in row.iter_mut().enumerate() {
                    let (fx, fy) = full(x, y);
                    let r = (fx - m[0]).hypot(fy - m[1]) / e;
                    let a = v.f.value(r)[1].max(1e-3);
                    let k = 1.0 / a;
                    p[0] *= k;
                    p[1] *= k;
                    p[2] *= k;
                }
            });
        }
        let Some(g) = &self.geometry else { return };
        let tab = lanczos_table();
        // lay out planar: contiguous rows per channel (less memory traffic when sampling)
        let layers: [Vec<f32>; 3] = std::array::from_fn(|c| px.par_iter().map(|p| p[c]).collect());
        px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let (fx, fy) = full(x, y);
                let q = self.source(g, fx, fy);
                for c in 0..3 {
                    let qp = if is_unit { q[c] } else { [(q[c][0] + 0.5) / sx - 0.5, (q[c][1] + 0.5) / sy - 0.5] };
                    o[c] = sample(&layers[c], w, h, qp, tab);
                }
            }
        });
    }
}

/// lensfun correction for a finished image (JPEG/HEIC/TIFF), `w`×`h` = whole camera image
/// (not cropped). Returns: correction and profile name.
pub fn lensfun_img(maker: &str, model: &str, lens: Option<&str>, focal_length: Option<f32>, aperture: Option<f32>, w: usize, h: usize, o: &LensOptions) -> Option<(Correction, String)> {
    if !o.anything() {
        return None;
    }
    let found = match o.pinned.as_deref().and_then(|p| p.strip_prefix(PIN_LENSFUN)) {
        Some(n) => lensfun_pinned_profile(maker, model, n, focal_length, aperture),
        None => lensfun_profile(maker, model, lens, focal_length, aperture),
    };
    let Some((p, name)) = found else {
        return o.has_manual().then(|| (Correction::manual_only(w, h, o.manual), Source::Manual.short().to_string()));
    };
    let img = Placement::image_center(w, h);
    let dv = o.distortion;
    let qf = o.lateral_ca;
    let mix = move |f: [f32; 3]| -> [f32; 3] {
        let g = 1.0 + dv * (f[1] - 1.0);
        if qf {
            [g * f[0] / f[1].max(1e-6), g, g * f[2] / f[1].max(1e-6)]
        } else {
            [g; 3]
        }
    };
    let part = |f: Radial| Part { source: name.clone(), kind: Source::Lensfun, placement: img, f, lcp: None, map: None };
    let geometry = (p.distortion.is_some() || p.lateral_ca.is_some()).then(|| {
        let pp = p.clone();
        part(Radial::from_func(move |r| mix(pp.geometry(r))))
    }).filter(|_| o.distortion != 0.0 || o.lateral_ca);
    let s = o.vignette;
    let vignette = p.vignette.is_some().then(|| {
        let pp = p.clone();
        part(Radial::from_func(move |r| [1.0 - s * (1.0 - pp.falloff(r)); 3]))
    }).filter(|_| s != 0.0);
    Correction::finish(w, h, geometry, vignette, o.manual).map(|k| (k, name))
}

/// Matching lensfun profile (camera from make/model, lens from EXIF or built-in).
pub fn lensfun_profile(maker: &str, model: &str, lens: Option<&str>, focal_length: Option<f32>, aperture: Option<f32>) -> Option<(lensfun::Profile, String)> {
    let db = Database::active();
    let cam_entry = db.camera(maker, model)?;
    let obj = db.lenses(cam_entry, lens.as_slice());
    let p = lensfun::Profile::determine(cam_entry, &obj, focal_length, aperture)?;
    let name = format!("lensfun: {}", p.name);
    Some((p, name))
}

/// Grid of the lens correction for the GPU (see `rawdev::gpu`): at every `advance`-th pixel
/// (full resolution, `width`×`height` of the correction) vignetting factor and source points per channel.
/// Both are smooth, the GPU interpolates bilinearly in between.
#[derive(Debug, Clone)]
pub struct Grid {
    pub gw: usize,
    pub gh: usize,
    pub advance: usize,
    /// factor per grid point (None: no vignetting correction)
    pub factor: Option<Vec<f32>>,
    /// source points (x, y) for R, G, B per grid point (None: no geometry)
    pub source: Option<Vec<[f32; 6]>>,
    /// The grid is split at the centre of the correction: per axis the last pixel left of (above) the centre and the
    /// number of grid lines up to it. Radial models with an offset at radius 0 (e.g. embedded lateral CA tables) jump by
    /// up to a pixel across the centre lines; nodes on both sides of the jump keep the bilinear interpolation from
    /// smearing it over a whole cell (visible as a seam along the centre row and column against the CPU).
    pub split: [(i64, usize); 2],
}

/// Grid lines of one axis of length `n`: up to and including `last_left` every `advance` pixels counted from it,
/// from `last_left + 1` on likewise; returns the positions and the number of lines on the left side.
fn split_axis(n: usize, last_left: i64, advance: usize) -> (Vec<f32>, usize) {
    let a = advance as i64;
    let last_left = last_left.clamp(0, n as i64 - 1);
    let nl = (last_left as usize).div_ceil(advance) + 1;
    let first_right = last_left + 1;
    let nr = if first_right as usize >= n { 0 } else { ((n as i64 - 1 - first_right) as usize).div_ceil(advance) + 1 };
    let mut v: Vec<f32> = (0..nl).map(|i| (last_left - a * (nl - 1 - i) as i64) as f32).collect();
    v.extend((0..nr).map(|j| (first_right + a * j as i64) as f32));
    (v, nl)
}

/// Lanczos-3 weights (257 steps × 6), as used by the CPU.
pub fn lanczos_weights() -> Vec<[f32; 6]> {
    lanczos_table().to_vec()
}

impl Correction {
    /// Vignetting correction factor at a full-image pixel (like [`Correction::apply_scaled`]).
    fn vignette_factor(&self, fx: f32, fy: f32) -> Option<f32> {
        if let Some(gm) = self.vignette.as_ref().and_then(|v| v.map.as_ref()) {
            Some(gm.gain(fx as f64, fy as f64) as f32)
        } else if let Some(l) = self.vignette.as_ref().and_then(|v| v.lcp.as_ref()) {
            Some((1.0 / l.falloff(fx as f64, fy as f64, l.strength)) as f32)
        } else if let Some(v) = &self.vignette {
            let m = v.placement.center;
            let r = (fx - m[0]).hypot(fy - m[1]) / v.placement.unit;
            Some(1.0 / v.f.value(r)[1].max(1e-3))
        } else {
            None
        }
    }

    /// Grid for full resolution (see [`Grid`]).
    pub fn grid(&self, advance: usize) -> Grid {
        let (w, h) = (self.width, self.height);
        let advance = advance.max(1);
        // centre of the correction (where a radial model may jump): last pixel left of / above it
        let centre = match self.geometry.as_ref() {
            Some(g) if g.lcp.is_none() => g.placement.center,
            _ => [(w as f32 - 1.0) * 0.5, (h as f32 - 1.0) * 0.5],
        };
        let (xs, nlx) = split_axis(w.max(1), centre[0].ceil() as i64 - 1, advance);
        let (ys, nly) = split_axis(h.max(1), centre[1].ceil() as i64 - 1, advance);
        let (gw, gh) = (xs.len(), ys.len());
        let split = [((centre[0].ceil() as i64 - 1).clamp(0, w as i64 - 1), nlx), ((centre[1].ceil() as i64 - 1).clamp(0, h as i64 - 1), nly)];
        let pos: Vec<(f32, f32)> = ys.iter().flat_map(|&y| xs.iter().map(move |&x| (x, y))).collect();
        let factor = self.vignette.as_ref().map(|_| pos.par_iter().map(|&(x, y)| self.vignette_factor(x, y).unwrap_or(1.0)).collect());
        let source = self.geometry.as_ref().map(|g| {
            pos.par_iter()
                .map(|&(x, y)| {
                    let q = self.source(g, x, y);
                    [q[0][0], q[0][1], q[1][0], q[1][1], q[2][0], q[2][1]]
                })
                .collect()
        });
        Grid { gw, gh, advance, factor, source, split }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panasonic_scale_needs_a_positive_denominator() {
        assert_eq!(panasonic_scale(1.0, 0.0), Some(1.0));
        assert_eq!(panasonic_scale(0.0, 0.0), None);
        assert_eq!(panasonic_scale(1.0, -32768.0), None);
        assert!(panasonic_scale(1.0, -32767.0).is_some_and(f32::is_finite));
    }

    #[test]
    fn pinned_lensfun_lens_also_adapted() {
        let db = Database::active();
        let sony = db.camera("SONY", "ILCE-7M4").expect("A7 IV");
        let ef = db.lens_names().into_iter().find(|(n, _)| n == "Canon EF 50mm f/1.4 USM").expect("EF 50");
        assert!(!db.mount_fits(Some(sony), &ef.1), "EF on E is adapted");
        let (p, name) = lensfun_pinned_profile("SONY", "ILCE-7M4", "Canon EF 50mm f/1.4 USM", Some(50.0), Some(2.0)).expect("profile");
        assert!(name.contains("EF 50mm") && p.distortion.is_some());
        // a camera unknown to lensfun still gets the chosen lens
        assert!(lensfun_pinned_profile("Nobody", "Cam 1", "Canon EF 50mm f/1.4 USM", Some(50.0), None).is_some());
        assert!(lensfun_pinned_profile("SONY", "ILCE-7M4", "no such lens", None, None).is_none());
    }

    /// Embedded data of real files (free samples, raw.pixls.us); `GRAINEO_TEST_LENS_FILES` = files separated by ':'.
    #[test]
    fn embedded_of_sample_files() {
        let Ok(list) = std::env::var("GRAINEO_TEST_LENS_FILES") else { return };
        for f in list.split(':').filter(|f| !f.is_empty()) {
            let Ok(r) = crate::RawFile::open(std::path::Path::new(f)) else { eprintln!("{f}: cannot open"); continue };
            let e = r.lens_embedded();
            let centre = e.as_ref().and_then(|e| e.geo.as_ref()).map(|(_, g, _)| g[0][1]);
            eprintln!("{f}: embedded centre factor {centre:?}");
            let corner = e.as_ref().and_then(|e| e.geo.as_ref()).map(|(k, g, _)| spline(k, &g.iter().map(|v| v[1]).collect::<Vec<_>>(), 1.0));
            let lf = lensfun_profile(&r.info.maker, &r.info.model, r.info.exif.lens.as_deref(), r.info.exif.focal_length, r.info.exif.aperture).map(|p| p.0.geometry(1.0)[1]);
            eprintln!("{f}: lensfun corner factor {lf:?}");
            let (w, h) = (r.info.width, r.info.height);
            let c = r.lens_correction(w, h, &LensOptions { adobe: false, ..Default::default() });
            eprintln!(
                "{f}: {} {} · {:?} {:?} mm · embedded {:?} corner factor {corner:?} shading {:?} · correction geo {:?} vig {:?} zoom {:?}",
                r.info.maker,
                r.info.model,
                r.info.exif.lens,
                r.info.exif.focal_length,
                e.as_ref().map(|e| e.source),
                e.as_ref().map(|e| e.shading_done),
                c.as_ref().and_then(|c| c.geometry.as_ref().map(|g| g.source.clone())),
                c.as_ref().and_then(|c| c.vignette.as_ref().map(|g| g.source.clone())),
                c.as_ref().map(|c| c.zoom)
            );
        }
    }

    #[test]
    fn manual_sliders_without_profile() {
        let o = LensOptions { distortion: 0.0, vignette: 0.0, lateral_ca: false, manual: [0.0; 3], ..Default::default() };
        assert!(!o.anything());
        assert!(Correction::finish(100, 80, None, None, [0.0; 3]).is_none());
        let k = Correction::finish(1000, 800, None, None, [50.0, 0.0, 0.0]).expect("manual");
        assert_eq!(k.geometry.as_ref().map(|g| g.kind), Some(Source::Manual));
        // + removes barrel distortion: the corner takes its pixel from further in, the zoom keeps the borders filled
        let g = k.geometry.as_ref().unwrap();
        let q = k.source_profile(g, 0.0, 0.0);
        let corner = k.source(g, 0.0, 0.0);
        assert!(corner[1][0] > q[1][0] && corner[1][1] > q[1][1], "{corner:?} vs {q:?}");
        // red + pulls the red channel inwards: its source point lies further out than green
        let k = Correction::finish(1000, 800, None, None, [0.0, 100.0, -100.0]).unwrap();
        let g = k.geometry.as_ref().unwrap();
        let s = k.source(g, 10.0, 10.0);
        assert!(s[0][0] < s[1][0] && s[2][0] > s[1][0], "{s:?}");
        // zero sliders keep the profile path identical
        let k = Correction::finish(1000, 800, Some(g.clone()), None, [0.0; 3]).unwrap();
        assert!(k.manual.is_none());
    }

    #[test]
    fn reasons_without_profile() {
        assert_eq!(no_profile_reason("Canon", "EOS R5", Some("EF70-200mm f/2.8L IS III USM +1.4x III")), "Telekonverter erkannt");
        assert_eq!(no_profile_reason("Nobody", "Cam 1", Some("X 50mm")), "Kamera nicht in der Datenbank");
        assert_eq!(no_profile_reason("SONY", "ILCE-7M4", None), "kein Objektivname in der Datei");
        assert_eq!(no_profile_reason("SONY", "ILCE-7M4", Some("Canon EF 50mm f/1.4 USM")), "anderes Bajonett");
        assert_eq!(no_profile_reason("SONY", "ILCE-7M4", Some("Foo 13mm F9 Wonder")), "Objektiv nicht in der Datenbank");
    }

    #[test]
    fn spline_bounds() {
        let k = [0.0, 0.5, 1.0];
        let y = [1.0, 2.0, 4.0];
        assert_eq!(spline(&k, &y, -1.0), 1.0);
        assert!((spline(&k, &y, 0.75) - 3.0).abs() < 1e-6);
        assert_eq!(spline(&k, &y, 2.0), 4.0);
    }
}
