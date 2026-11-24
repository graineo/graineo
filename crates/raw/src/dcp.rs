//! Camera profiles in the DNG Camera Profile format (`.dcp`), read at run time from where they are installed, and the
//! profile tables of the DNG specification 1.7 (chapter 6, "Camera Profiles"): HueSatMap and LookTable. Own code
//! following the public specification; no profile is shipped or copied (rule: such files are only read when installed
//! locally). Idea of checking that the LookTable really acts: greycard ("A DCP's look table").
//!
//! Order (specification): camera → XYZ D50 (ForwardMatrix or ColorMatrix) → linear ProPhoto RGB → HueSatMap (interpolated
//! between the two illuminants like the matrices) → exposure → LookTable → tone curve. Both tables work in HSV of linear
//! ProPhoto: hue shift in degrees, saturation and value as factors, trilinear lookup (hue wraps), the value axis
//! optionally read through the sRGB curve (`…Encoding` = 1). The ProfileToneCurve is not applied (our own tone curve
//! stays), as are DefaultBlackRender and the embed policy.

use std::path::Path;

use crate::color::{self, Calibration, M3};

/// One HSV table (HueSatMap or LookTable).
#[derive(Debug, Clone, PartialEq)]
pub struct HsvTable {
    pub hues: usize,
    pub sats: usize,
    pub vals: usize,
    /// (hue shift in degrees, saturation factor, value factor) for v, h, s (s fastest)
    pub data: Vec<[f32; 3]>,
    /// value axis through the sRGB curve
    pub srgb_values: bool,
}

/// A camera profile (from a DCP file).
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub name: String,
    pub camera: String,
    /// matrices per illuminant (sorted by colour temperature, warmest first)
    pub calibration: Vec<Calibration>,
    /// HueSatMap per illuminant, in the order of `calibration` (one entry when both illuminants share it)
    pub hue_sat: Vec<HsvTable>,
    pub look: Option<HsvTable>,
    /// BaselineExposureOffset in EV (held within ±10)
    pub baseline_offset: f32,
}

const ILLUMINANT1: u16 = 50778;
const ILLUMINANT2: u16 = 50779;
const COLOR_MATRIX1: u16 = 50721;
const COLOR_MATRIX2: u16 = 50722;
const FORWARD_MATRIX1: u16 = 50964;
const FORWARD_MATRIX2: u16 = 50965;
const UNIQUE_CAMERA_MODEL: u16 = 50708;
const PROFILE_NAME: u16 = 50936;
const HSM_DIMS: u16 = 50937;
const HSM_DATA1: u16 = 50938;
const HSM_DATA2: u16 = 50939;
const LOOK_DIMS: u16 = 50981;
const LOOK_DATA: u16 = 50982;
const HSM_ENCODING: u16 = 51107;
const LOOK_ENCODING: u16 = 51108;
const BASELINE_OFFSET: u16 = 51109;

/// Raw IFD entry: type, count, bytes of the value.
struct Entry {
    typ: u16,
    count: usize,
    bytes: Vec<u8>,
}

fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

impl Entry {
    fn numbers(&self, le: bool) -> Vec<f64> {
        let u16_ = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
        let u32_ = |b: &[u8]| if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) };
        let sz = type_size(self.typ);
        (0..self.count)
            .filter_map(|i| {
                let b = self.bytes.get(i * sz..(i + 1) * sz)?;
                Some(match self.typ {
                    1 | 7 => b[0] as f64,
                    6 => b[0] as i8 as f64,
                    3 => u16_(b) as f64,
                    8 => u16_(b) as i16 as f64,
                    4 => u32_(b) as f64,
                    9 => u32_(b) as i32 as f64,
                    11 => f32::from_bits(u32_(b)) as f64,
                    5 | 10 => {
                        let (n, d) = (u32_(&b[0..4]), u32_(&b[4..8]));
                        let (n, d) = if self.typ == 10 { (n as i32 as f64, d as i32 as f64) } else { (n as f64, d as f64) };
                        if d == 0.0 {
                            return None;
                        }
                        n / d
                    }
                    12 => {
                        let a: [u8; 8] = b.try_into().ok()?;
                        if le { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }
                    }
                    _ => return None,
                })
            })
            .collect()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).trim_end_matches('\0').trim().to_string()
    }
}

/// Reads the first IFD of a DCP (or TIFF/DNG) file into tag → entry.
fn read_ifd(b: &[u8]) -> Option<(bool, std::collections::HashMap<u16, Entry>)> {
    let le = match b.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_ = |o: usize| b.get(o..o + 2).map(|x| if le { u16::from_le_bytes([x[0], x[1]]) } else { u16::from_be_bytes([x[0], x[1]]) });
    let u32_ = |o: usize| b.get(o..o + 4).map(|x| if le { u32::from_le_bytes([x[0], x[1], x[2], x[3]]) } else { u32::from_be_bytes([x[0], x[1], x[2], x[3]]) });
    // DCP: magic 0x4352 ("RC"), TIFF/DNG: 42
    let magic = u16_(2)?;
    if magic != 0x4352 && magic != 42 {
        return None;
    }
    let ifd = u32_(4)? as usize;
    let n = u16_(ifd)? as usize;
    let mut map = std::collections::HashMap::new();
    for i in 0..n.min(4096) {
        let e = ifd + 2 + 12 * i;
        let (tag, typ, count) = (u16_(e)?, u16_(e + 2)?, u32_(e + 4)? as usize);
        let len = type_size(typ).checked_mul(count)?;
        if len == 0 || len > 64 << 20 {
            continue;
        }
        let bytes = if len <= 4 { b.get(e + 8..e + 8 + len)?.to_vec() } else { b.get(u32_(e + 8)? as usize..)?.get(..len)?.to_vec() };
        map.insert(tag, Entry { typ, count, bytes });
    }
    Some((le, map))
}

fn matrix3(v: &[f64]) -> Option<M3> {
    if v.len() < 9 || v.iter().take(9).any(|x| !x.is_finite()) {
        return None;
    }
    Some([[v[0] as f32, v[1] as f32, v[2] as f32], [v[3] as f32, v[4] as f32, v[5] as f32], [v[6] as f32, v[7] as f32, v[8] as f32]])
}

fn table(dims: &[f64], data: &[f64], srgb: bool) -> Option<HsvTable> {
    let (h, s, v) = (*dims.first()? as usize, *dims.get(1)? as usize, *dims.get(2).unwrap_or(&1.0) as usize);
    // a saturation axis needs two nodes; zero nodes on any axis is no table
    if h == 0 || s < 2 || v == 0 || h * s * v * 3 != data.len() || h > 4096 || s > 4096 || v > 4096 {
        return None;
    }
    if data.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let data = data.chunks_exact(3).map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
    Some(HsvTable { hues: h, sats: s, vals: v, data, srgb_values: srgb })
}

impl Profile {
    /// Reads a DCP file. None: not a profile, or without colour matrix.
    pub fn load(path: &Path) -> Option<Profile> {
        Self::parse(&std::fs::read(path).ok()?)
    }

    pub fn parse(b: &[u8]) -> Option<Profile> {
        let (le, t) = read_ifd(b)?;
        let num = |tag| t.get(&tag).map(|e| e.numbers(le)).unwrap_or_default();
        let ill = |tag| num(tag).first().map(|v| *v as u16);
        let k1 = ill(ILLUMINANT1).and_then(color::kelvin_illuminant).unwrap_or(6504.0);
        let k2 = ill(ILLUMINANT2).and_then(color::kelvin_illuminant);
        let c1 = matrix3(&num(COLOR_MATRIX1))?;
        let mut cal = vec![(Calibration { kelvin: k1, color: c1, forward: matrix3(&num(FORWARD_MATRIX1)) }, HSM_DATA1)];
        if let (Some(k2), Some(c2)) = (k2, matrix3(&num(COLOR_MATRIX2))) {
            cal.push((Calibration { kelvin: k2, color: c2, forward: matrix3(&num(FORWARD_MATRIX2)) }, HSM_DATA2));
        }
        cal.sort_by(|a, b| a.0.kelvin.total_cmp(&b.0.kelvin));
        let hsm_srgb = num(HSM_ENCODING).first() == Some(&1.0);
        let dims = num(HSM_DIMS);
        let mut hue_sat: Vec<HsvTable> = cal.iter().filter_map(|(_, tag)| table(&dims, &num(*tag), hsm_srgb)).collect();
        // a map that does not read (or only one of two) is dropped whole: the matrices still render
        if hue_sat.len() != cal.len() {
            hue_sat.clear();
            if let Some(m) = table(&dims, &num(HSM_DATA1), hsm_srgb) {
                hue_sat.push(m);
            }
        }
        let look = table(&num(LOOK_DIMS), &num(LOOK_DATA), num(LOOK_ENCODING).first() == Some(&1.0));
        let offset = num(BASELINE_OFFSET).first().copied().filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-10.0, 10.0) as f32;
        Some(Profile {
            name: t.get(&PROFILE_NAME).map(|e| e.text()).unwrap_or_default(),
            camera: t.get(&UNIQUE_CAMERA_MODEL).map(|e| e.text()).unwrap_or_default(),
            calibration: cal.into_iter().map(|(c, _)| c).collect(),
            hue_sat,
            look,
            baseline_offset: offset,
        })
    }

    /// HueSatMap for the weight `w` of the warmer illuminant ([`color::Color::weight`]).
    pub fn hue_sat_for(&self, w: f32) -> Option<HsvTable> {
        match self.hue_sat.as_slice() {
            [] => None,
            [m] => Some(m.clone()),
            [a, b, ..] => {
                let w = w.clamp(0.0, 1.0);
                let mut m = a.clone();
                for (o, (x, y)) in m.data.iter_mut().zip(a.data.iter().zip(&b.data)) {
                    *o = std::array::from_fn(|c| w * x[c] + (1.0 - w) * y[c]);
                }
                Some(m)
            }
        }
    }
}

/// Folders where camera profiles are installed (read only).
pub fn profile_folders() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if cfg!(target_os = "macos") {
        v.push("/Library/Application Support/Adobe/CameraRaw/CameraProfiles".into());
        if let Some(h) = std::env::var_os("HOME") {
            v.push(std::path::Path::new(&h).join("Library/Application Support/Adobe/CameraRaw/CameraProfiles"));
        }
    } else if cfg!(windows) {
        for k in ["ProgramData", "APPDATA"] {
            if let Some(p) = std::env::var_os(k) {
                v.push(std::path::Path::new(&p).join("Adobe/CameraRaw/CameraProfiles"));
            }
        }
    }
    v
}

/// Installed profiles for a camera (`UniqueCameraModel` as in the profile, e.g. "Canon EOS R6"): (name, path), sorted.
/// Only reads the file headers it needs.
pub fn installed_for(camera: &str) -> Vec<(String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let mut stack = profile_folders();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("dcp")) {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if !stem.to_lowercase().starts_with(&camera.to_lowercase()) {
                    continue;
                }
                if let Some(pr) = Profile::load(&p).filter(|pr| pr.camera.eq_ignore_ascii_case(camera)) {
                    out.push((pr.name, p));
                }
            }
        }
    }
    out.sort();
    out
}

/// Linear ProPhoto RGB (D50) ↔ XYZ D50.
pub fn prophoto_to_xyz_d50() -> M3 {
    color::rgb_to_xyz([0.7347, 0.2653], [0.1596, 0.8404], [0.0366, 0.0001], color::D50_XY)
}

#[inline]
fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    let d = mx - mn;
    if mx <= 0.0 || d <= 0.0 {
        return [0.0, 0.0, mx];
    }
    let s = d / mx;
    let h = if mx == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    [h, s, mx]
}

#[inline]
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(6.0);
    let i = (h.floor() as i32).clamp(0, 5);
    let f = h - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

impl HsvTable {
    /// Trilinear lookup at hue `h` (0…6), saturation `s` (0…1) and value coordinate `v` (0…1, already encoded if the
    /// table asks for it); hue wraps, the other axes are held at their ends.
    #[inline]
    fn lookup(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        let hf = h * self.hues as f32 / 6.0;
        let h0 = (hf.floor() as isize).rem_euclid(self.hues as isize) as usize;
        let h1 = (h0 + 1) % self.hues;
        let fh = hf - hf.floor();
        let sf = (s.clamp(0.0, 1.0)) * (self.sats - 1) as f32;
        let s0 = (sf.floor() as usize).min(self.sats - 2);
        let fs = sf - s0 as f32;
        let (v0, v1, fv) = if self.vals > 1 {
            let vf = v.clamp(0.0, 1.0) * (self.vals - 1) as f32;
            let v0 = (vf.floor() as usize).min(self.vals - 2);
            (v0, v0 + 1, vf - v0 as f32)
        } else {
            (0, 0, 0.0)
        };
        let at = |vi: usize, hi: usize, si: usize| self.data[(vi * self.hues + hi) * self.sats + si];
        let mut o = [0f32; 3];
        for (vi, wv) in [(v0, 1.0 - fv), (v1, fv)] {
            if wv == 0.0 {
                continue;
            }
            for (hi, wh) in [(h0, 1.0 - fh), (h1, fh)] {
                for (si, ws) in [(s0, 1.0 - fs), (s0 + 1, fs)] {
                    let w = wv * wh * ws;
                    let e = at(vi, hi, si);
                    o[0] += w * e[0];
                    o[1] += w * e[1];
                    o[2] += w * e[2];
                }
            }
        }
        o
    }

    /// Applies the table to linear ProPhoto `c`. `read_scale`: factor on the value for the lookup only (the reading
    /// point of the LookTable lies at the profile's exposure, not the user's); the result keeps the pixel's own scale.
    /// Saturation is held at 1 where the table raises it (specification), lowering always lowers; values over 1 survive.
    #[inline]
    pub fn apply(&self, c: [f32; 3], read_scale: f32) -> [f32; 3] {
        let [h, s, v] = rgb_to_hsv(c);
        if v <= 0.0 {
            return c;
        }
        let vr = (v * read_scale).clamp(0.0, 1.0);
        let vc = if self.srgb_values { crate::srgb_oetf(vr) } else { vr };
        let [dh, ds, dv] = self.lookup(h, s, vc);
        let h2 = h + dh * (6.0 / 360.0);
        let s2 = if ds > 1.0 { (s * ds).min(s.max(1.0)) } else { s * ds.max(0.0) };
        hsv_to_rgb(h2, s2, v * dv.max(0.0))
    }
}

/// Stage of a profile in the working space of a development: converts working space ↔ linear ProPhoto and applies the
/// HueSatMap and the LookTable.
#[derive(Debug, Clone)]
pub struct Stage {
    ws_to_pp: M3,
    pp_to_ws: M3,
    hue_sat: Option<HsvTable>,
    look: Option<HsvTable>,
    /// value factor for reading the LookTable (2^(BaselineExposureOffset − user exposure))
    look_read: f32,
}

impl Stage {
    /// `ws_to_xyz_d65`: working space (linear, D65 white) → XYZ; `weight` of the warmer illuminant; `user_ev`: the
    /// user's exposure, left out of the LookTable's reading point; `with_look`: false = HueSatMap only.
    pub fn new(p: &Profile, ws_to_xyz_d65: &M3, weight: f32, user_ev: f32, with_look: bool) -> Option<Stage> {
        let ad = color::bradford(color::xy_to_xyz(color::D65_XY), color::xy_to_xyz(color::D50_XY));
        let ws_to_pp = color::mul(&color::inv(&prophoto_to_xyz_d50())?, &color::mul(&ad, ws_to_xyz_d65));
        let pp_to_ws = color::inv(&ws_to_pp)?;
        let hue_sat = p.hue_sat_for(weight);
        let look = if with_look { p.look.clone() } else { None };
        if hue_sat.is_none() && look.is_none() {
            return None;
        }
        Some(Stage { ws_to_pp, pp_to_ws, hue_sat, look, look_read: (p.baseline_offset - user_ev).clamp(-20.0, 20.0).exp2() })
    }

    #[inline]
    pub fn apply(&self, v: [f32; 3]) -> [f32; 3] {
        let mut c = color::mulv(&self.ws_to_pp, v);
        if !c.iter().all(|x| x.is_finite()) {
            return v;
        }
        if let Some(m) = &self.hue_sat {
            c = m.apply(c, 1.0);
        }
        if let Some(l) = &self.look {
            c = l.apply(c, self.look_read);
        }
        color::mulv(&self.pp_to_ws, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(h: usize, s: usize, v: usize) -> HsvTable {
        HsvTable { hues: h, sats: s, vals: v, data: vec![[0.0, 1.0, 1.0]; h * s * v], srgb_values: true }
    }

    #[test]
    fn identity_table_leaves_colours() {
        let t = identity(90, 16, 16);
        for c in [[0.2f32, 0.5, 0.1], [0.9, 0.1, 0.3], [0.05, 0.05, 0.4], [1.5, 0.7, 0.2]] {
            let o = t.apply(c, 1.0);
            for k in 0..3 {
                assert!((o[k] - c[k]).abs() < 1e-5, "{c:?} → {o:?}");
            }
        }
    }

    #[test]
    fn hue_shift_and_value_scale_act() {
        let mut t = identity(6, 2, 1);
        for e in t.data.iter_mut() {
            *e = [60.0, 1.0, 0.5];
        }
        // red + 60° = yellow, half the value
        let o = t.apply([0.8, 0.0, 0.0], 1.0);
        assert!((o[0] - 0.4).abs() < 1e-5 && (o[1] - 0.4).abs() < 1e-5 && o[2].abs() < 1e-5, "{o:?}");
    }

    #[test]
    fn look_table_reads_value_axis() {
        // value axis: dark half untouched, bright half desaturated
        let mut t = identity(6, 2, 2);
        for h in 0..6 {
            for s in 0..2 {
                t.data[(6 + h) * 2 + s] = [0.0, 0.5, 1.0];
            }
        }
        t.srgb_values = false;
        let dark = t.apply([0.1, 0.05, 0.05], 1.0);
        let bright = t.apply([0.1, 0.05, 0.05], 10.0);
        let sat = |c: [f32; 3]| (c[0] - c[1].min(c[2])) / c[0];
        assert!(sat(bright) < sat(dark), "{dark:?} {bright:?}");
        // the reading scale does not change the pixel's own brightness
        assert!((bright[0] - 0.1).abs() < 1e-5);
    }

    #[test]
    fn parses_a_minimal_dcp() {
        // little-endian "IIRC" file with ColorMatrix1, illuminant and a 2×2×1 HueSatMap
        let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = Vec::new();
        let srat = |v: &[i32]| v.iter().flat_map(|x| [x.to_le_bytes(), 10000i32.to_le_bytes()].concat()).collect::<Vec<u8>>();
        entries.push((ILLUMINANT1, 3, 1, 21u16.to_le_bytes().to_vec()));
        entries.push((COLOR_MATRIX1, 10, 9, srat(&[10000, 0, 0, 0, 10000, 0, 0, 0, 10000])));
        entries.push((HSM_DIMS, 4, 3, [2u32, 2, 1].iter().flat_map(|v| v.to_le_bytes()).collect()));
        entries.push((HSM_DATA1, 11, 12, [[5.0f32, 1.0, 1.0]; 4].iter().flatten().flat_map(|v| v.to_le_bytes()).collect()));
        entries.push((PROFILE_NAME, 2, 5, b"Test\0".to_vec()));
        let mut b = b"IIRC".to_vec();
        b.extend(8u32.to_le_bytes());
        let n = entries.len();
        b.extend((n as u16).to_le_bytes());
        let mut data_off = 8 + 2 + 12 * n + 4;
        let mut tail: Vec<u8> = Vec::new();
        for (tag, typ, count, bytes) in &entries {
            b.extend(tag.to_le_bytes());
            b.extend(typ.to_le_bytes());
            b.extend(count.to_le_bytes());
            if bytes.len() <= 4 {
                let mut v = bytes.clone();
                v.resize(4, 0);
                b.extend(v);
            } else {
                b.extend((data_off as u32).to_le_bytes());
                data_off += bytes.len();
                tail.extend(bytes);
            }
        }
        b.extend(0u32.to_le_bytes());
        b.extend(tail);
        let p = Profile::parse(&b).expect("parse");
        assert_eq!(p.name, "Test");
        assert_eq!(p.calibration.len(), 1);
        assert_eq!(p.hue_sat.len(), 1);
        assert_eq!(p.hue_sat[0].data[0], [5.0, 1.0, 1.0]);
        assert!(p.look.is_none());
    }
}
