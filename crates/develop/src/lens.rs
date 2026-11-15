//! Lens corrections as in SIGMA Photo Pro: correction of chromatic aberration (CA)
//! with a lens profile or manually; the only lens correction SPP offers for Foveon
//! cameras.
//!
//! Here: read lens profiles (`.lpi` from SPP's lens database, Huffman-coded text)
//! and choose the coefficients so that the result matches SPP.
//! The shift itself is computed by x3fuse-core in the tile pipeline of the SPP base
//! (`spp_denoise::ca`), at the same point in the pipeline as in SPP.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Mode of the CA correction (in SPP two mutually exclusive checkboxes "lens profile" and
/// "manual"; no "both").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaMode {
    /// SPP default for Merrill files (the camera writes "off" into the file header)
    #[default]
    Off,
    Profile,
    Manual,
}

/// For the base key: mode and sliders in tenths (SPP step 0.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CaKey {
    pub mode: CaMode,
    /// Profile amount ×10 (0 … 20, default 10)
    pub profile: i8,
    /// Red/blue ×10 (−20 … 20)
    pub red: i8,
    pub blue: i8,
}

impl CaKey {
    pub fn is_off(&self) -> bool {
        self.mode == CaMode::Off
    }
}

/// Lens data of the shot, as used for profile selection.
#[derive(Clone, Debug, PartialEq)]
pub struct LensShot {
    /// PROP `LENSMODEL` (lens ID, e.g. "1004" = DP2 Merrill)
    pub id: String,
    /// PROP `FLENGTH` in mm
    pub focal: f32,
    /// PROP `APERTURE` (actual f-number, e.g. 2.82843)
    pub fnumber: f32,
    /// EXIF subject distance; Merrill X3F has no EXIF → 0 (same in SPP)
    pub distance: f32,
}

/// A profile: rows `FL FN FD R0 R1 R2 R3 B0 B1 B2 B3`.
#[derive(Clone, Debug, PartialEq)]
pub struct LensProfile {
    pub name: String,
    pub rows: Vec<ProfileRow>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileRow {
    pub fl: f32,
    pub fn_: f32,
    pub fd: f32,
    /// R0 R1 R2 R3 B0 B1 B2 B3
    pub k: [f64; 8],
}

/// Where SPP stores its lens database (first the user copy, then the program).
fn database_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("GRAINEO_LENS_DB") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(d) = sysdirs::app_data_dir() {
        dirs.push(d.join("SIGMA_PhotoPro6/.LensData/LensDatabase/Chromatic"));
    }
    dirs.push(PathBuf::from("/Applications/SIGMA_PhotoPro6.app/Contents/Resources/LensDatabase/Chromatic"));
    dirs
}

/// Profile for the lens ID from SPP's database (if SPP is installed).
pub fn find_profile(id: &str) -> Option<LensProfile> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    for dir in database_dirs() {
        if let Ok(data) = std::fs::read(dir.join(format!("{id}.lpi"))) {
            if let Some(p) = decode_lpi(&data).and_then(|t| parse_profile(&t)) {
                return Some(p);
            }
        }
    }
    None
}

/// Decode a lens file: 256 codes from 0x70 (length = upper 5 bits, code = lower 27 bits),
/// data from 0x470 up to 32 bytes before the end, where the text length (u32) is stored.
pub fn decode_lpi(d: &[u8]) -> Option<String> {
    let n = d.len();
    if n < 0x490 + 4 {
        return None;
    }
    let u32_at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let mut codes = std::collections::HashMap::new();
    for s in 0..256 {
        let e = u32_at(0x70 + 4 * s);
        let len = e >> 27;
        if len > 0 {
            codes.insert((len, e & 0x7ff_ffff), s as u8);
        }
    }
    let out_len = u32_at(n - 0x20) as usize;
    if out_len == 0 || out_len > 1 << 20 {
        return None;
    }
    let mut out = Vec::with_capacity(out_len);
    let (mut acc, mut bits) = (0u32, 0u32);
    'outer: for &b in &d[0x470..n - 0x20] {
        for i in (0..8).rev() {
            acc = (acc << 1) | u32::from((b >> i) & 1);
            bits += 1;
            if let Some(&s) = codes.get(&(bits, acc)) {
                out.push(s);
                acc = 0;
                bits = 0;
                if out.len() == out_len {
                    break 'outer;
                }
            } else if bits > 27 {
                return None;
            }
        }
    }
    (out.len() == out_len).then(|| String::from_utf8_lossy(&out).into_owned())
}

/// Text of the lens file: 17 header lines (2 = name, 14 = count), then the rows.
pub fn parse_profile(text: &str) -> Option<LensProfile> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let name = lines.get(1)?.trim().to_string();
    let count: usize = lines.get(13)?.trim().parse().ok()?;
    let mut rows = Vec::with_capacity(count);
    for line in lines.iter().skip(17).take(count) {
        let v: Vec<f64> = line.split_whitespace().take(11).map(atof).collect();
        if v.len() < 11 {
            // Incomplete rows are accepted too (as in SPP); the missing fields stay 0
            let mut w = v.clone();
            w.resize(11, 0.0);
            rows.push(row(&w));
        } else {
            rows.push(row(&v));
        }
    }
    Some(LensProfile { name, rows })
}

fn row(v: &[f64]) -> ProfileRow {
    ProfileRow { fl: v[0] as f32, fn_: v[1] as f32, fd: v[2] as f32, k: [v[3], v[4], v[5], v[6], v[7], v[8], v[9], v[10]] }
}

/// Leading number as with C `atof`, rest ignored, otherwise 0.
fn atof(s: &str) -> f64 {
    let end = s
        .char_indices()
        .take_while(|&(i, c)| c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0) || c == 'e' || c == 'E')
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    let mut t = &s[..end];
    while !t.is_empty() {
        if let Ok(v) = t.parse() {
            return v;
        }
        t = &t[..t.len() - 1];
    }
    0.0
}

/// Profile selection with a profile: coefficients in SPP format `[R0,G0,B0, R1,G1,B1, …]`.
///
/// - Same focal length: first row with the same aperture and distance ≥ shot, otherwise from
///   the largest aperture ≤ shot the first row with distance ≥ shot (no
///   interpolation over aperture and distance).
/// - Otherwise linear over focal length between the next smaller and next larger one
///   (each with the same aperture/distance selection; above: aperture < shot + 0.5). If that
///   gives implausible values (> 1.1) or one side is missing: the nearer end.
pub fn select_profile(p: &LensProfile, shot: &LensShot) -> [f64; 12] {
    let (lfl, lfn, lfd) = (shot.focal, shot.fnumber, shot.distance);
    let mut out = [0.0f64; 8];
    let mut exact = false;
    let (mut cand, mut cand_fn) = (false, 0.0f32);
    let (mut up, mut up_flag, mut up_fn, mut up_fl) = ([0.0f64; 8], false, 0.0f32, 0.0f32);
    let (mut lo, mut lo_flag, mut lo_fn, mut lo_fl) = ([0.0f64; 8], false, 0.0f32, 0.0f32);
    let mut last = [0.0f64; 8];
    for r in &p.rows {
        last = r.k;
        if r.fl == lfl {
            if lfn == r.fn_ && !(r.fd < lfd) {
                out = r.k;
                exact = true;
                break;
            }
            if r.fn_ <= lfn {
                cand = if cand_fn == r.fn_ { cand } else { false };
                if !cand && lfd <= r.fd {
                    out = r.k;
                    cand = true;
                    cand_fn = r.fn_;
                }
            }
        } else if lfl < r.fl {
            if r.fn_ == lfn || f64::from(r.fn_) < f64::from(lfn) + 0.5 {
                let keep = if up_fn == r.fn_ { up_flag } else { false };
                if up_fl == r.fl {
                    up_flag = keep;
                }
                if !up_flag && lfd <= r.fd {
                    up = r.k;
                    up_flag = true;
                    up_fn = r.fn_;
                    up_fl = r.fl;
                }
            }
        } else if !(lfn < r.fn_) {
            let keep = if lo_fn == r.fn_ { lo_flag } else { false };
            lo_flag = if lo_fl == r.fl { keep } else { false };
            if !lo_flag && lfd <= r.fd {
                lo = r.k;
                lo_flag = true;
                lo_fn = r.fn_;
                lo_fl = r.fl;
            }
        }
    }
    if !(cand || exact) {
        let d_up = up_fl - lfl;
        let d_lo = lfl - lo_fl;
        let (a_lo, a_up) = (d_lo.abs(), d_up.abs());
        let mut i = last; // without interpolation the last row read stays in effect (same result as SPP)
        if d_lo != 0.0 || d_up != 0.0 {
            let w_lo = f64::from(a_up / (a_lo + a_up));
            let w_up = f64::from(a_lo / (a_lo + a_up));
            for k in 0..8 {
                i[k] = up[k] * w_up + lo[k] * w_lo;
            }
        }
        let bad = i.iter().any(|&v| 1.1 < v) || a_lo + a_up == 0.0 || up_fl == 0.0 || lo_fl == 0.0;
        if bad {
            i = if a_up <= a_lo { up } else { lo };
        }
        out = i;
    }
    let k = out;
    [k[0], 0.0, k[4], k[1], 0.0, k[5], k[2], 0.0, k[6], k[3], 0.0, k[7]]
}

/// Alternative profile selection (test switch `GRAINEO_MERRILL_NEW=ca`): per focal length of the
/// profile the row with the f-number nearest to the shot (equal distance: the smaller one) and then the
/// smallest focus distance ≥ the shot's (else the largest); linear interpolation over the focal length
/// between the nearest focal lengths below and above (outside: the nearest one).
pub fn select_profile_own(p: &LensProfile, shot: &LensShot) -> [f64; 12] {
    let mut focals: Vec<f32> = p.rows.iter().map(|r| r.fl).collect();
    focals.sort_by(f32::total_cmp);
    focals.dedup();
    let pick = |fl: f32| -> Option<[f64; 8]> {
        let rows: Vec<&ProfileRow> = p.rows.iter().filter(|r| r.fl == fl).collect();
        let key = |r: &&ProfileRow| ((r.fn_ - shot.fnumber).abs(), r.fn_);
        let best = rows.iter().min_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal))?.fn_;
        let same: Vec<&&ProfileRow> = rows.iter().filter(|r| r.fn_ == best).collect();
        let row = same.iter().filter(|r| r.fd >= shot.distance).min_by(|a, b| a.fd.total_cmp(&b.fd)).or_else(|| same.iter().max_by(|a, b| a.fd.total_cmp(&b.fd)))?;
        Some(row.k)
    };
    let below = focals.iter().copied().filter(|&f| f <= shot.focal).last();
    let above = focals.iter().copied().find(|&f| f >= shot.focal);
    let k = match (below, above) {
        (Some(a), Some(b)) if a != b => match (pick(a), pick(b)) {
            (Some(ka), Some(kb)) => {
                let t = f64::from((shot.focal - a) / (b - a));
                std::array::from_fn(|i| ka[i] * (1.0 - t) + kb[i] * t)
            }
            (ka, kb) => ka.or(kb).unwrap_or_default(),
        },
        (Some(f), _) | (None, Some(f)) => pick(f).unwrap_or_default(),
        (None, None) => [0.0; 8],
    };
    [k[0], 0.0, k[4], k[1], 0.0, k[5], k[2], 0.0, k[6], k[3], 0.0, k[7]]
}

/// Profile selection without a profile (manual): only c0 = 1.0003 / 1 / 0.9997 according to the sign
/// of red or blue.
pub fn manual_coeffs(red: f32, blue: f32) -> [f64; 12] {
    let c0 = |v: f32| if 0.0 < v { 1.0003 } else if v < 0.0 { 0.9997 } else { 1.0 };
    let mut c = [0.0; 12];
    c[0] = c0(red);
    c[2] = c0(blue);
    c
}

/// Lens data of the shot from the PROP list (Merrill and older only).
pub fn shot_from_props(props: &x3f::PropList) -> Option<LensShot> {
    let id = props.get("LENSMODEL")?.trim().to_string();
    let num = |k: &str| props.get(k).map(|v| atof(v.trim()) as f32).unwrap_or(0.0);
    Some(LensShot { id, focal: num("FLENGTH"), fnumber: num("APERTURE"), distance: 0.0 })
}

/// Read the PROP list without loading the whole file (header, directory, PROP: a few KB).
pub fn read_props(path: &Path) -> Option<x3f::PropList> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size || len > 4 << 20 {
            return None;
        }
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    let le32 = |d: &[u8], o: usize| -> Option<u32> { Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    if read_at(0, 4)? != b"FOVb" {
        return None;
    }
    let dir = u64::from(le32(&read_at(size.checked_sub(4)?, 4)?, 0)?);
    let head = read_at(dir, 12)?;
    if &head[0..4] != b"SECd" {
        return None;
    }
    let count = (le32(&head, 8)? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    let i = (0..count).find(|i| &table[i * 12 + 8..i * 12 + 12] == b"PROP")?;
    let (off, len) = (le32(&table, i * 12)?, le32(&table, i * 12 + 4)?);
    x3f::PropList::parse(&read_at(off.into(), len as usize)?).ok()
}

/// Finished correction for x3fuse-core (`None` = off or no profile available).
pub fn correction(path: &Path, key: &CaKey) -> Option<x3f_core::SppCaCorrection> {
    let profile_scale = f32::from(key.profile) / 10.0;
    let red = f32::from(key.red) / 10.0;
    let blue = f32::from(key.blue) / 10.0;
    match key.mode {
        CaMode::Off => None,
        CaMode::Manual => Some(x3f_core::SppCaCorrection { profile: false, profile_scale, red, blue, coeffs: manual_coeffs(red, blue) }),
        CaMode::Profile => {
            let shot = shot_from_props(&read_props(path)?)?;
            let prof = find_profile(&shot.id)?;
            let coeffs = if x3f_core::merrill_new::active("ca") { select_profile_own(&prof, &shot) } else { select_profile(&prof, &shot) };
            Some(x3f_core::SppCaCorrection { profile: true, profile_scale, red, blue, coeffs })
        }
    }
}

/// Is there an SPP lens profile for this file? Returns the profile name (for the
/// UI; reads only a few KB).
pub fn profile_name(path: &Path) -> Option<String> {
    let shot = shot_from_props(&read_props(path)?)?;
    find_profile(&shot.id).map(|p| p.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dp2m() -> LensProfile {
        let mut rows = Vec::new();
        for (fn_, fds) in [(2.8f32, &[0.28f32, 0.35, 1.0][..]), (4.0, &[0.28, 1.0][..]), (8.0, &[0.35, 1.0][..])] {
            for &fd in fds {
                let k = [1.0 + f64::from(fn_) / 1000.0 + f64::from(fd) / 1e5, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
                rows.push(ProfileRow { fl: 30.0, fn_, fd, k });
            }
        }
        LensProfile { name: "Test".into(), rows }
    }

    #[test]
    fn selects_largest_fnumber_below_and_nearest_distance() {
        let p = dp2m();
        let c = select_profile(&p, &LensShot { id: "1004".into(), focal: 30.0, fnumber: 4.75683, distance: 0.0 });
        assert!((c[0] - (1.004 + 0.28 / 1e5)).abs() < 1e-9, "{}", c[0]);
        // f/8: smallest distance there 0.35
        let c = select_profile(&p, &LensShot { id: "1004".into(), focal: 30.0, fnumber: 11.0, distance: 0.0 });
        assert!((c[0] - (1.008 + 0.35 / 1e5)).abs() < 1e-9);
        // exact aperture
        let c = select_profile(&p, &LensShot { id: "1004".into(), focal: 30.0, fnumber: 2.8, distance: 0.5 });
        assert!((c[0] - (1.0028 + 1.0 / 1e5)).abs() < 1e-9);
    }

    #[test]
    fn interpolates_focal_length() {
        let mk = |fl: f32, v: f64| ProfileRow { fl, fn_: 4.0, fd: 1e12, k: [v, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0] };
        let p = LensProfile { name: "Zoom".into(), rows: vec![mk(10.0, 1.0), mk(20.0, 1.002)] };
        let c = select_profile(&p, &LensShot { id: "x".into(), focal: 12.5, fnumber: 4.0, distance: 0.0 });
        assert!((c[0] - 1.0005).abs() < 1e-7, "{}", c[0]);
    }

    #[test]
    fn manual_signs() {
        let c = manual_coeffs(0.5, -0.1);
        assert_eq!((c[0], c[2]), (1.0003, 0.9997));
        assert_eq!(manual_coeffs(0.0, 0.0)[0], 1.0);
    }

    #[test]
    fn decodes_installed_profile() {
        // Only if SPP is installed (otherwise silently skipped)
        if let Some(p) = find_profile("1004") {
            assert_eq!(p.name, "SIGMA DP2 Merrill");
            assert_eq!(p.rows.len(), 27);
            assert_eq!(p.rows[0].fl, 30.0);
        }
    }
}
