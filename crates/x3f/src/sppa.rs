//! Development settings saved by SIGMA Photo Pro (SPP), **read only**.
//!
//! SPP stores the slider positions in the X3F file itself (format):
//!
//! - **Quattro and newer (X3F ≥ 4.0):** separate section `SPPA` (`SECs`) with three groups
//!   `APD1`…`APD3` in IFD format (12-byte entries like TIFF). The section header records which
//!   group was last used for colour or monochrome. Without `SPPA` the file header
//!   (IFD from byte 52) contains only the camera settings.
//! - **Merrill and older (X3F 2.x/3.x):** in the file header: white balance and colour mode name
//!   (32 bytes ASCII each) and the "extended data": 32 or 64 type bytes, followed by as many
//!   4-byte values (mostly `f32`). The camera writes its own settings there;
//!   SPP overwrites the whole block when saving.
//! - Monochrome mode: bit 1 (value 2) of the flag bits in the file header.
//!
//! This module never writes anything.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{PropList, X3fFile};

/// Where the values come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `SPPA` section; group 1…3 that was last active for colour or monochrome
    Sppa { color_group: u8, mono_group: u8 },
    /// File header (X3F 2.x/3.x: extended data, X3F 4.x: header IFD)
    Header,
}

/// Tone sliders of one mode (colour or monochrome), SPP scale mostly −2 … +2.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tone {
    pub exposure: Option<f32>,
    pub contrast: Option<f32>,
    pub shadow: Option<f32>,
    pub highlight: Option<f32>,
    pub sharpness: Option<f32>,
    pub fill_light: Option<f32>,
}

/// Settings read, in SPP's own units (`None` = not stored).
#[derive(Clone, Debug, PartialEq)]
pub struct SppAdjustments {
    pub source: Source,
    pub version: u32,
    /// Do the values come from SPP (`true`) or only from the camera (`false`)?
    /// `SPPA` → always SPP; Merrill header → SPP if types typical for SPP (≥ 38 or
    /// colour adjustment ≠ 0) occur; Quattro header → camera.
    pub saved_by_spp: bool,
    /// Flag bits of the file header (bit 1 = monochrome, bits 4–7 stars, 9–12 colour)
    pub mark_bits: u32,
    pub monochrome: bool,
    /// SPP white balance: Auto, Sunlight, Shade, Overcast, Incandescent, Fluorescent, Flash,
    /// Custom, Monochrome, AutoLSP ("Auto (Natural)"), ColorTemp
    pub white_balance: Option<String>,
    /// Colour temperature in kelvin (only relevant for white balance "ColorTemp")
    pub wb_temperature: Option<f32>,
    /// Colour mode with our names (Standard, Vivid, Neutral, Portrait, Landscape, FCBlue,
    /// SunsetRed, ForestGreen, FCYellow, Cinema, TealAndOrange, Monochrome, Sepia, …)
    pub color_mode: Option<String>,
    /// Effect strength of the colour mode (Quattro, 0x53)
    pub color_mode_effect: Option<f32>,
    pub color: Tone,
    pub saturation: Option<f32>,
    /// Highlight correction
    pub highlight_correction: Option<f32>,
    /// Colour adjustment R/G/B as gain (1 = neutral)
    pub color_adjust: Option<[f32; 3]>,
    /// Noise sliders 0 … 1 (0.5 = middle)
    pub chroma_noise: Option<f32>,
    pub luma_noise: Option<f32>,
    pub banding_noise: Option<f32>,
    pub residual_noise: Option<f32>,
    pub mono: Tone,
    pub mono_luma_noise: Option<f32>,
    pub mono_banding_noise: Option<f32>,
    pub mono_residual_noise: Option<f32>,
    /// Monochrome mix bottom/middle/top layer (sum 1)
    pub mix: Option<[f32; 3]>,
    /// Monochrome toning as gain R/G/B (1/1/1 = neutral)
    pub toning: Option<[f32; 3]>,
    pub grain: Option<bool>,
    pub grain_size: Option<f32>,
    pub grain_roughness: Option<f32>,
    pub vignette: Option<bool>,
    pub vignette_amount: Option<f32>,
    pub vignette_midpoint: Option<f32>,
    /// Crop left, top, right, bottom (SPP image coordinates)
    pub crop: Option<[u16; 4]>,
    /// White balance or colour mode set in the camera (Merrill: PROP `WB_DESC`/
    /// `CM_DESC`, Quattro: header IFD), same names as above
    pub camera_white_balance: Option<String>,
    pub camera_color_mode: Option<String>,
    /// ISO of the shot (PROP `ISO`, Merrill and older only)
    pub iso: Option<f32>,
    /// CA correction as SPP takes it from the Merrill header: 0 = off, 1 = manual,
    /// 2 = lens profile (on/off switch type 17, lens switch type 20)
    pub ca_switch: Option<u8>,
    /// CA profile amount (type 16, 1 = 100 %), manual red/blue (types 18/19, −2 … +2)
    pub ca_profile: Option<f32>,
    pub ca_red: Option<f32>,
    pub ca_blue: Option<f32>,
    /// SPP fringe correction (Merrill header types 21/22): green and magenta band, each
    /// `Some((hue 0…255, range 0…1, amount 0…1))` if switched on
    pub defringe_green: Option<(f32, f32, f32)>,
    pub defringe_magenta: Option<(f32, f32, f32)>,
}

impl SppAdjustments {
    fn empty(source: Source, version: u32, mark_bits: u32) -> Self {
        Self {
            source,
            version,
            saved_by_spp: false,
            mark_bits,
            monochrome: mark_bits & 2 != 0,
            white_balance: None,
            wb_temperature: None,
            color_mode: None,
            color_mode_effect: None,
            color: Tone::default(),
            saturation: None,
            highlight_correction: None,
            color_adjust: None,
            chroma_noise: None,
            luma_noise: None,
            banding_noise: None,
            residual_noise: None,
            mono: Tone::default(),
            mono_luma_noise: None,
            mono_banding_noise: None,
            mono_residual_noise: None,
            mix: None,
            toning: None,
            grain: None,
            iso: None,
            grain_size: None,
            grain_roughness: None,
            vignette: None,
            vignette_amount: None,
            vignette_midpoint: None,
            crop: None,
            camera_white_balance: None,
            camera_color_mode: None,
            ca_switch: None,
            ca_profile: None,
            ca_red: None,
            ca_blue: None,
            defringe_green: None,
            defringe_magenta: None,
        }
    }
}

/// Read settings from a loaded file.
pub fn read(x3f: &X3fFile) -> Option<SppAdjustments> {
    let find = |kind: &[u8; 4]| x3f.entries.iter().find(|e| &e.kind == kind).map(|e| x3f.section(e));
    let prop = find(b"PROP").and_then(|s| PropList::parse(s).ok());
    let iso = prop_iso(prop.as_ref());
    parse(x3f.data(), find(b"SPPA"), || prop).map(|a| SppAdjustments { iso, ..a })
}

/// Like [`read`], but reads only file header, directory, `SPPA` and possibly `PROP` (a few KB)
/// instead of the whole file; fast enough for every folder change.
pub fn read_path(path: impl AsRef<Path>) -> Option<SppAdjustments> {
    let mut f = std::fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size {
            return None;
        }
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    let head = read_at(0, size.min(HEAD_BYTES) as usize)?;
    if head.get(0..4)? != b"FOVb" {
        return None;
    }
    let dir = u64::from(le32(&read_at(size.checked_sub(4)?, 4)?, 0)?);
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return None;
    }
    let count = (le32(&dir_head, 8)? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    let entry = |kind: &[u8]| {
        (0..count).find(|i| &table[i * 12 + 8..i * 12 + 12] == kind).and_then(|i| Some((le32(&table, i * 12)?, le32(&table, i * 12 + 4)?)))
    };
    let (sppa_at, prop_at) = (entry(b"SPPA"), entry(b"PROP"));
    let sppa = sppa_at.filter(|&(_, len)| len <= MAX_SECTION).and_then(|(off, len)| read_at(off.into(), len as usize));
    let prop = prop_at.filter(|&(_, len)| len <= MAX_SECTION).and_then(|(off, len)| PropList::parse(&read_at(off.into(), len as usize)?).ok());
    let iso = prop_iso(prop.as_ref());
    parse(&head, sppa.as_deref(), || prop).map(|a| SppAdjustments { iso, ..a })
}

fn prop_iso(prop: Option<&PropList>) -> Option<f32> {
    prop?.get("ISO")?.trim().parse().ok()
}

/// This much of the start of the file suffices for header and header IFD (Quattro: values end at ≈ 1.5 KB).
const HEAD_BYTES: u64 = 4096;
const MAX_SECTION: u32 = 4 << 20;

fn le16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn le32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn f32_at(d: &[u8], o: usize) -> Option<f32> {
    le32(d, o).map(f32::from_bits)
}

/// Core: `head` = start of the file (at least header and header IFD), `sppa` = section content.
fn parse(head: &[u8], sppa: Option<&[u8]>, prop: impl FnOnce() -> Option<PropList>) -> Option<SppAdjustments> {
    if head.get(0..4)? != b"FOVb" {
        return None;
    }
    let version = le32(head, 4)?;
    if version >= 0x4_0000 {
        let mark = le32(head, 36)?;
        // Camera settings from the header IFD (tags 0x0800_0000 + n)
        let count = (le32(head, 52)? as usize).min(1000);
        let camera = ifd(head, 56, count, 0);
        let camera_wb = camera.get(&1).and_then(|v| wb_name(v[0] as u32));
        let camera_mode = camera.get(&2).and_then(|v| mode_name(v[0] as u32));
        let groups = sppa.and_then(sppa_groups);
        let mut a = match groups {
            Some((color_group, mono_group, g)) => {
                let color = g.get(&color_group).cloned().unwrap_or_default();
                let mono = g.get(&mono_group).cloned().unwrap_or_default();
                let mut a = SppAdjustments::empty(Source::Sppa { color_group, mono_group }, version, mark);
                apply_sppa(&mut a, &color, &mono);
                a.saved_by_spp = true;
                a
            }
            None => {
                let mut a = SppAdjustments::empty(Source::Header, version, mark);
                apply_v4_header(&mut a, &camera);
                a
            }
        };
        a.camera_white_balance = camera_wb;
        a.camera_color_mode = camera_mode;
        Some(a)
    } else {
        let mut a = SppAdjustments::empty(Source::Header, version, le32(head, 0x18)?);
        // White balance from 0x28, colour mode from 0x48 since version 2.3; then types and values
        let text = |o: usize| -> Option<String> {
            let b = head.get(o..o + 32)?;
            let s = String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(32)]).trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        a.white_balance = text(0x28).and_then(|s| wb_from_text(&s));
        let (types_at, n) = match version {
            0x2_0003 => (0x68, 32),
            0x3_0000.. => (0x68, 64),
            _ => (0x48, 32),
        };
        if version >= 0x2_0003 {
            a.color_mode = text(0x48).map(|s| mode_from_text(&s));
        }
        let types = head.get(types_at..types_at + n)?;
        let mut ext = BTreeMap::new();
        for (i, &t) in types.iter().enumerate() {
            // Types 21/22 (defringe) are packed integers, everything else f32
            if t != 0 && t != 21 && t != 22 {
                if let Some(v) = f32_at(head, types_at + n + 4 * i) {
                    ext.insert(t, v);
                }
            }
        }
        a.saved_by_spp = ext.keys().any(|&t| (38..=45).contains(&t)) || [7, 8, 9].iter().any(|t| ext.get(t).is_some_and(|&v| v != 0.0));
        apply_v3_ext(&mut a, &ext);
        // Fringe correction: packed integer stored as f32 (as SPP reads it); only if the green
        // entry is non-zero, then switch bits 24–27 of each entry, hue 16–23, range 8–11, amount 1–4
        let packed = |want: u8| {
            let i = types.iter().position(|&t| t == want)?;
            let v = f32_at(head, types_at + n + 4 * i)?;
            (v.is_finite() && v >= 0.0).then_some(v as u32)
        };
        if let Some(g) = packed(21).filter(|&g| g != 0) {
            let m = packed(22).unwrap_or(0);
            let band = |v: u32| ((v >> 24) & 0xF != 0).then(|| (((v >> 16) & 0xFF) as f32, fringe_step(v >> 8), fringe_step(v >> 1)));
            a.defringe_green = band(g);
            a.defringe_magenta = band(m);
        }
        if a.saved_by_spp {
            if let Some(p) = prop() {
                a.camera_white_balance = p.get("WB_DESC").and_then(wb_from_text);
                a.camera_color_mode = p.get("CM_DESC").map(mode_from_text);
            }
        }
        Some(a)
    }
}

/// Values of an IFD list: tag (16 bit) → numbers. `base` = reference for offsets (SPPA: section start,
/// file header: 0). Values up to 4 bytes are stored in the entry itself.
fn ifd(d: &[u8], at: usize, count: usize, base: usize) -> BTreeMap<u16, Vec<f64>> {
    let mut out = BTreeMap::new();
    for i in 0..count {
        let e = at + 12 * i;
        let (Some(tag), Some(type_name), Some(n)) = (le16(d, e), le16(d, e + 2), le32(d, e + 4)) else { break };
        let n = n as usize;
        let size = match type_name {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => continue,
        };
        if n == 0 || n > 4096 {
            continue;
        }
        let bytes = if size * n <= 4 {
            d.get(e + 8..e + 8 + size * n)
        } else {
            le32(d, e + 8).and_then(|o| {
                let o = base + o as usize;
                d.get(o..o + size * n)
            })
        };
        let Some(b) = bytes else { continue };
        let vals: Vec<f64> = (0..n)
            .filter_map(|k| {
                let o = k * size;
                Some(match type_name {
                    1 | 7 => b[o] as f64,
                    6 => b[o] as i8 as f64,
                    3 => le16(b, o)? as f64,
                    8 => le16(b, o)? as i16 as f64,
                    4 => le32(b, o)? as f64,
                    9 => le32(b, o)? as i32 as f64,
                    11 => f32_at(b, o)? as f64,
                    5 => {
                        let den = le32(b, o + 4)?;
                        if den == 0 {
                            return None;
                        }
                        le32(b, o)? as f64 / den as f64
                    }
                    10 => {
                        let den = le32(b, o + 4)? as i32;
                        if den == 0 {
                            return None;
                        }
                        le32(b, o)? as i32 as f64 / den as f64
                    }
                    _ => return None,
                })
            })
            .collect();
        if vals.len() == n {
            out.insert(tag, vals);
        }
    }
    out
}

type Group = BTreeMap<u16, Vec<f64>>;

/// `SPPA`: header "SECs", version, type, last used group for colour (+12) and monochrome (+16),
/// from +20 the groups "APDn", count, entries, offset of the next group.
fn sppa_groups(s: &[u8]) -> Option<(u8, u8, BTreeMap<u8, Group>)> {
    if s.get(0..4)? != b"SECs" {
        return None;
    }
    let apd = |o: usize| -> Option<u8> {
        let b = s.get(o..o + 4)?;
        (&b[..3] == b"APD" && (b'1'..=b'3').contains(&b[3])).then(|| b[3] - b'0')
    };
    let color_group = apd(12).unwrap_or(1);
    let mono_group = apd(16).unwrap_or(1);
    let mut groups = BTreeMap::new();
    let mut pos = 20;
    for _ in 0..3 {
        let Some(g) = apd(pos) else { break };
        let count = (le32(s, pos + 4)? as usize).min(1000);
        groups.insert(g, ifd(s, pos + 8, count, 0));
        let next = le32(s, pos + 8 + 12 * count).unwrap_or(0) as usize;
        if next <= pos || next >= s.len() {
            break;
        }
        pos = next;
    }
    (!groups.is_empty()).then_some((color_group, mono_group, groups))
}

fn one(g: &Group, tag: u16) -> Option<f32> {
    g.get(&tag).and_then(|v| v.first()).map(|&v| v as f32)
}

fn rgb(g: &Group, tags: [u16; 3], scale: f32) -> Option<[f32; 3]> {
    let v = [one(g, tags[0])?, one(g, tags[1])?, one(g, tags[2])?];
    Some(v.map(|x| x / scale))
}

/// Group of the `SPPA` section (tag numbers as stored in the file).
fn apply_sppa(a: &mut SppAdjustments, c: &Group, m: &Group) {
    a.white_balance = one(c, 1).and_then(|v| wb_name(v as u32));
    a.color_mode = one(c, 2).and_then(|v| mode_name(v as u32));
    a.color = Tone {
        exposure: one(c, 3),
        contrast: one(c, 4),
        shadow: one(c, 5),
        highlight: one(c, 6),
        sharpness: one(c, 8),
        fill_light: one(c, 9),
    };
    a.saturation = one(c, 7);
    a.highlight_correction = one(c, 10);
    a.color_adjust = rgb(c, [11, 12, 13], 32768.0).map(neutral_if_zero);
    a.chroma_noise = one(c, 14);
    a.luma_noise = one(c, 15);
    a.banding_noise = one(c, 16);
    a.mono = Tone {
        exposure: one(m, 17),
        contrast: one(m, 18),
        shadow: one(m, 19),
        highlight: one(m, 20),
        sharpness: one(m, 21),
        fill_light: one(m, 22),
    };
    a.mono_luma_noise = one(m, 23);
    a.mono_banding_noise = one(m, 24);
    a.mix = rgb(m, [25, 26, 27], 32768.0);
    a.toning = rgb(m, [28, 29, 30], 32768.0).map(neutral_if_zero);
    // Crop: in monochrome mode from the mono group (as SPP does)
    let crop_group = if a.monochrome { m } else { c };
    a.crop = crop_group.get(&31).filter(|v| v.len() == 4).map(|v| [v[0] as u16, v[1] as u16, v[2] as u16, v[3] as u16]);
    a.grain = one(m, 43).map(|v| v != 0.0);
    a.grain_size = one(m, 44);
    a.grain_roughness = one(m, 45);
    a.vignette = one(m, 46).map(|v| v != 0.0);
    a.vignette_amount = one(m, 47);
    a.vignette_midpoint = one(m, 48);
    a.wb_temperature = one(c, 50);
    a.residual_noise = one(c, 60);
    a.mono_residual_noise = one(m, 61);
    a.color_mode_effect = one(c, 83);
}

/// Quattro header IFD (tag numbers relative to 0x0800_0000; up to 30 the same as in `SPPA`).
fn apply_v4_header(a: &mut SppAdjustments, h: &Group) {
    apply_sppa(a, h, h);
    a.crop = match (one(h, 31), one(h, 32), one(h, 33), one(h, 34)) {
        (Some(l), Some(t), Some(r), Some(b)) => Some([l as u16, t as u16, r as u16, b as u16]),
        _ => None,
    };
    a.grain = one(h, 55).map(|v| v != 0.0);
    a.grain_size = one(h, 56);
    a.grain_roughness = one(h, 57);
    a.vignette = one(h, 58).map(|v| v != 0.0);
    a.vignette_amount = one(h, 59);
    a.vignette_midpoint = one(h, 60);
    a.wb_temperature = one(h, 36);
    a.residual_noise = one(h, 63);
    a.mono_residual_noise = one(h, 64);
    a.color_mode_effect = None;
}

/// Merrill header: extended data (type = stored type ID − 0x0200_0000), all `f32`.
fn apply_v3_ext(a: &mut SppAdjustments, e: &BTreeMap<u8, f32>) {
    let g = |t: u8| e.get(&t).copied();
    let rgb = |t: [u8; 3]| Some([g(t[0])?, g(t[1])?, g(t[2])?]);
    a.color = Tone { exposure: g(1), contrast: g(2), shadow: g(3), highlight: g(4), sharpness: g(6), fill_light: g(10) };
    a.saturation = g(5);
    a.color_adjust = rgb([7, 8, 9]).map(neutral_if_zero);
    a.chroma_noise = g(12);
    a.luma_noise = g(13);
    a.highlight_correction = g(15);
    a.banding_noise = g(23);
    a.mix = rgb([24, 25, 26]);
    a.grain_size = g(27);
    a.grain_roughness = g(28);
    a.grain = g(29).map(|v| v != 0.0);
    a.mono = Tone { exposure: g(30), contrast: g(31), shadow: g(32), highlight: g(33), sharpness: g(34), fill_light: g(35) };
    a.mono_luma_noise = g(36);
    a.mono_banding_noise = g(37);
    a.toning = rgb([38, 39, 40]).map(neutral_if_zero);
    a.vignette_amount = g(41);
    a.vignette_midpoint = g(42);
    a.vignette = g(43).map(|v| v != 0.0);
    // One shared value for colour and monochrome
    a.residual_noise = g(44);
    a.mono_residual_noise = g(44);
    // CA switch as SPP reads it: on/off 0 → off, else lens switch 0 → manual, else profile
    a.ca_switch = g(17).map(|on| if on == 0.0 { 0 } else if g(20).unwrap_or(0.0) == 0.0 { 1 } else { 2 });
    a.ca_profile = g(16);
    a.ca_red = g(18);
    a.ca_blue = g(19);
}

/// SPP's step table for the fringe sliders: 4-bit index → 0, 0.1 … 1 (larger indices → 1).
fn fringe_step(v: u32) -> f32 {
    ((v & 0xF).min(10) as f32) / 10.0
}

/// SPP replaces a 0 in colour adjustment and toning with 1 (neutral).
fn neutral_if_zero(v: [f32; 3]) -> [f32; 3] {
    v.map(|x| if x == 0.0 { 1.0 } else { x })
}

/// Stored white balance value → name.
fn wb_name(code: u32) -> Option<String> {
    let n = match code {
        1 => "Auto",
        2 => "Sunlight",
        3 => "Shade",
        4 => "Overcast",
        5 => "Incandescent",
        6 => "Fluorescent",
        7 => "Flash",
        8 => "Custom",
        9 => "Monochrome",
        10 => "AutoLSP",
        11 => "ColorTemp",
        _ => return None,
    };
    Some(n.to_string())
}

/// White balance text from the Merrill header or PROP → name as in [`wb_name`].
fn wb_from_text(s: &str) -> Option<String> {
    let s = s.trim();
    Some(match s {
        "" => return None,
        "Daylight" => "Sunlight".into(),
        "Auto(Natural)" => "AutoLSP".into(),
        other => other.to_string(),
    })
}

/// Stored colour mode value → name (our names, see `develop::calib`).
fn mode_name(code: u32) -> Option<String> {
    let n = match code {
        1 => "Sepia",
        2 => "Monochrome",
        3 => "Standard",
        4 => "Vivid",
        5 => "Neutral",
        6 => "Portrait",
        7 => "Landscape",
        8 => "FCBlue",
        9 => "SunsetRed",
        10 => "ForestGreen",
        11 => "FCYellow",
        12 => "Cinema",
        13 => "TealAndOrange",
        14 => "Off",
        15 => "PowderBlue",
        16 => "Duotone",
        _ => return None,
    };
    Some(n.to_string())
}

/// Colour mode text from the Merrill header or PROP → name as in [`mode_name`].
fn mode_from_text(s: &str) -> String {
    match s.trim() {
        "FCGreen" => "ForestGreen".into(),
        "Cinematic" => "Cinema".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ifd_reads_inline_and_offset_values() {
        // Entry 1: BYTE 6 inline; entry 2: SRATIONAL 3/10 at offset 40
        let mut d = vec![0u8; 48];
        d[0..2].copy_from_slice(&1u16.to_le_bytes());
        d[2..4].copy_from_slice(&1u16.to_le_bytes());
        d[4..8].copy_from_slice(&1u32.to_le_bytes());
        d[8] = 6;
        d[12..14].copy_from_slice(&3u16.to_le_bytes());
        d[14..16].copy_from_slice(&10u16.to_le_bytes());
        d[16..20].copy_from_slice(&1u32.to_le_bytes());
        d[20..24].copy_from_slice(&40u32.to_le_bytes());
        d[40..44].copy_from_slice(&(-3i32).to_le_bytes());
        d[44..48].copy_from_slice(&10i32.to_le_bytes());
        let m = ifd(&d, 0, 2, 0);
        assert_eq!(m[&1], vec![6.0]);
        assert!((m[&3][0] + 0.3).abs() < 1e-9);
    }

    /// Merrill header with CA switch (types 16–20) and fringe correction (types 21/22) as SPP
    /// saves them (values from an SD1 Merrill file edited in SPP).
    #[test]
    fn merrill_header_ca_and_fringe() {
        let mut h = vec![0u8; 0x68 + 64 + 4 * 64];
        h[0..4].copy_from_slice(b"FOVb");
        h[4..8].copy_from_slice(&0x3_0000u32.to_le_bytes());
        let entries: [(u8, f32); 8] = [(16, 1.0), (17, 1.0), (18, 0.0), (19, 0.0), (20, 1.0), (21, 18941194.0), (22, 18941194.0), (45, 0.5)];
        for (i, (t, v)) in entries.iter().enumerate() {
            h[0x68 + i] = *t;
            h[0x68 + 64 + 4 * i..0x68 + 64 + 4 * i + 4].copy_from_slice(&v.to_le_bytes());
        }
        let a = parse(&h, None, || None).unwrap();
        assert_eq!(a.ca_switch, Some(2));
        assert_eq!(a.ca_profile, Some(1.0));
        assert_eq!(a.defringe_green, Some((33.0, 0.5, 0.5)));
        assert_eq!(a.defringe_magenta, Some((33.0, 0.5, 0.5)));
        // As written by the camera: switch bits 0 → off; CA off
        h[0x68 + 64 + 4..0x68 + 64 + 8].copy_from_slice(&0.0f32.to_le_bytes());
        for i in [5, 6] {
            h[0x68 + 64 + 4 * i..0x68 + 64 + 4 * i + 4].copy_from_slice(&2163978.0f32.to_le_bytes());
        }
        let a = parse(&h, None, || None).unwrap();
        assert_eq!(a.ca_switch, Some(0));
        assert_eq!((a.defringe_green, a.defringe_magenta), (None, None));
    }

    #[test]
    fn names() {
        assert_eq!(wb_from_text("Daylight").as_deref(), Some("Sunlight"));
        assert_eq!(mode_from_text("FCGreen"), "ForestGreen");
        assert_eq!(mode_name(6).as_deref(), Some("Portrait"));
    }
}
