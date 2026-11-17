//! Film profiles: Camera Raw "Looks" with an RGB table (e.g. RNI Films) that the user has installed locally
//! (`~/Library/Application Support/Adobe/CameraRaw/Settings/<package>/*.xmp`, `crs:PresetType="Look"`).
//!
//! Read at runtime from referenced folders; profile files are never copied. No profiles are shipped.
//!
//! Format of the table (`crs:Table_<key>`):
//! - Text in a Base85 variant: Z85 character set, but `` ` `` `'` `|` instead of `&` `<` `>` (XML-safe),
//!   5 characters = one 32-bit word, **least significant digit first**, word little-endian.
//! - The bytes are a 4-byte length (little-endian) + zlib stream. The table reference is an opaque
//!   identifier (`Look.Parameters.RGBTable`), not a checksum of the complete unpacked bytes.
//! - Unpacked: header of four 32-bit words (1, 1, dimensions = 3, divisions = 32), then
//!   divisions³ × 3 × u16 (r slow, b fast) as **deviation from the identity modulo 65536**:
//!   `output = (stored + 65535·i/(divisions−1)) mod 65536`; at the end a small tail (28 bytes).
//!
//! The table is converted into our encoded sRGB (see [`Space`]) and offered as a [`Look`],
//! so it runs through the same path (intensity, GPU) as the camera looks.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
mod manager;
mod cube;
pub use cube::table as cube_table;
pub use manager::{
    generation, installed, load_failed, reread, visible_profiles, Storage, Report, Options, Package, Source,
    NOTICE,
};

use crate::looks::Look;

/// An installed film profile (header data only; the table is read only when needed).
#[derive(Clone, Debug)]
pub struct FilmProfile {
    /// Identifier of the profile (`crs:UUID`); the same appears in the Lightroom settings as `Look.UUID`
    pub uuid: String,
    pub name: String,
    /// Package, e.g. "RNI FILMS 5 BW - Pro"
    pub group: String,
    pub path: PathBuf,
    pub source: Source,
    pub available: bool,
    /// The profile provides an intensity slider
    pub strength: bool,
    /// Further non-zero sliders of the profile (contrast, curve …); not applied yet
    pub additional: Vec<(String, f64)>,
}

/// In which color space the table applies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Space {
    /// Apply the table directly to encoded sRGB
    Direct,
    /// ProPhoto primaries, sRGB transfer curve
    ProPhotoSrgb,
    /// ProPhoto primaries, gamma 1.8
    ProPhoto18,
}

/// Default: directly on encoded sRGB. Measured on 54 reference exports (47 RNI profiles) this is on average
/// closer to the reference than ProPhoto with sRGB transfer curve (ΔE00 7.40 vs. 7.78; better for 31 vs. 15 images).
/// For comparison: `GRAINEO_FILM_PROFILE_SPACE=prophoto|prophoto18`.
fn space_from_env() -> Space {
    match std::env::var("GRAINEO_FILM_PROFILE_SPACE").as_deref() {
        Ok("prophoto") | Ok("prophoto-srgb") => Space::ProPhotoSrgb,
        Ok("prophoto18") => Space::ProPhoto18,
        _ => Space::Direct,
    }
}

const ALPHABET: &[u8; 85] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?`'|()[]{}@%$#";

/// Base85 text (variant, see module docs) → bytes (4 per group). `None` on an invalid character or overflow.
pub fn base85_decode(text: &str) -> Option<Vec<u8>> {
    let mut inv = [255u8; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        inv[c as usize] = i as u8;
    }
    let b = text.as_bytes();
    // The last group can be shorter than 5 characters (missing most significant digits = 0); it then
    // yields padding bytes after the zlib stream, which [`table_from_text`] ignores.
    let mut out = Vec::with_capacity(b.len() / 5 * 4 + 4);
    for g in b.chunks(5) {
        let mut v: u64 = 0;
        for &c in g.iter().rev() {
            let d = inv[c as usize];
            if d == 255 {
                return None;
            }
            v = v * 85 + d as u64;
        }
        if v > u32::MAX as u64 {
            return None;
        }
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
    Some(out)
}

/// Inverse of [`base85_decode`] (length must be divisible by 4), for tests.
pub fn base85_encode(data: &[u8]) -> String {
    let mut s = String::new();
    for w in data.chunks_exact(4) {
        let mut v = u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as u64;
        for _ in 0..5 {
            s.push(ALPHABET[(v % 85) as usize] as char);
            v /= 85;
        }
    }
    s
}

/// Unpacked RGB table.
#[derive(Clone, Debug)]
pub struct RgbTable {
    /// divisions per axis
    pub n: usize,
    /// output values 0…1, r slow, b fast
    pub output: Vec<[f32; 3]>,
}

/// `crs:Table_…` text → table; checks header and length.
pub fn table_from_text(text: &str) -> Option<(RgbTable, [u8; 16])> {
    let raw_bytes = base85_decode(text)?;
    let length = u32::from_le_bytes(raw_bytes.get(..4)?.try_into().ok()?) as usize;
    const MAX_UNPACKED: usize = 2 * 1024 * 1024;
    if length > MAX_UNPACKED {
        return None;
    }
    let mut data = Vec::with_capacity(length);
    flate2::read::ZlibDecoder::new(raw_bytes.get(4..)?)
        .take((MAX_UNPACKED + 1) as u64)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() != length || data.len() < 16 {
        return None;
    }
    let w = |i: usize| {
        u32::from_le_bytes([
            data[4 * i],
            data[4 * i + 1],
            data[4 * i + 2],
            data[4 * i + 3],
        ])
    };
    let (dim, n) = (w(2) as usize, w(3) as usize);
    if dim != 3 || !(2..=65).contains(&n) || data.len() < 16 + n * n * n * 6 {
        return None;
    }
    let md5 = md5(&data);
    let ramp = |i: usize| (65535.0 * i as f64 / (n - 1) as f64).round() as u32;
    let mut output = Vec::with_capacity(n * n * n);
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                let k = 16 + ((r * n + g) * n + b) * 6;
                let v = |c: usize, i: usize| {
                    let stored =
                        u16::from_le_bytes([data[k + 2 * c], data[k + 2 * c + 1]]) as u32;
                    ((stored + ramp(i)) & 0xFFFF) as f32 / 65535.0
                };
                output.push([v(0, r), v(1, g), v(2, b)]);
            }
        }
    }
    Some((RgbTable { n, output }, md5))
}

// ------------------------------------------------------------------------------------ Color space

type M3 = [[f64; 3]; 3];

fn mul3(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

fn mul3_v(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// Linear sRGB (D65) → linear ProPhoto (D50, Bradford) and back.
fn prophoto_matrices() -> (M3, M3) {
    let srgb_xyz: M3 = [
        [0.4124564, 0.3575761, 0.1804375],
        [0.2126729, 0.7151522, 0.0721750],
        [0.0193339, 0.1191920, 0.9503041],
    ];
    let bradford: M3 = [
        [1.0478112, 0.0228866, -0.0501270],
        [0.0295424, 0.9904844, -0.0170491],
        [-0.0092345, 0.0150436, 0.7521316],
    ];
    let xyz_pro: M3 = [
        [1.3459433, -0.2556075, -0.0511118],
        [-0.5445989, 1.5081673, 0.0205351],
        [0.0, 0.0, 1.2118128],
    ];
    let forward_m = mul3(&xyz_pro, &mul3(&bradford, &srgb_xyz));
    // way back: inverse via the adjugate
    let d = |m: &M3| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let det = d(&forward_m);
    let mut inv = [[0.0; 3]; 3];
    let m = &forward_m;
    inv[0][0] = (m[1][1] * m[2][2] - m[1][2] * m[2][1]) / det;
    inv[0][1] = (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det;
    inv[0][2] = (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det;
    inv[1][0] = (m[1][2] * m[2][0] - m[1][0] * m[2][2]) / det;
    inv[1][1] = (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det;
    inv[1][2] = (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det;
    inv[2][0] = (m[1][0] * m[2][1] - m[1][1] * m[2][0]) / det;
    inv[2][1] = (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det;
    inv[2][2] = (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det;
    (forward_m, inv)
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> f64 {
    let v = v.max(0.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

impl RgbTable {
    /// Trilinear lookup (input 0…1 per axis).
    pub fn pixel(&self, c: [f64; 3]) -> [f64; 3] {
        let n = self.n;
        let m = (n - 1) as f64;
        let p = c.map(|v| v.clamp(0.0, 1.0) * m);
        let i = p.map(|v| (v as usize).min(n - 2));
        let f = [p[0] - i[0] as f64, p[1] - i[1] as f64, p[2] - i[2] as f64];
        let mut o = [0.0; 3];
        for (a, wa) in [(0, 1.0 - f[0]), (1, f[0])] {
            for (b, wb) in [(0, 1.0 - f[1]), (1, f[1])] {
                for (d, wd) in [(0, 1.0 - f[2]), (1, f[2])] {
                    let w = wa * wb * wd;
                    let v = self.output[((i[0] + a) * n + i[1] + b) * n + i[2] + d];
                    for k in 0..3 {
                        o[k] += w * v[k] as f64;
                    }
                }
            }
        }
        o
    }

    /// Convert into our encoded sRGB: a [`Look`] with `nodes_n`³ nodes.
    pub fn to_look(&self, name: &str, space: Space, nodes_n: usize) -> Option<Look> {
        if !(2..=65).contains(&nodes_n) || self.n < 2 || self.output.len() != self.n.checked_pow(3)? {
            return None;
        }
        let (forward_m, her) = prophoto_matrices();
        let mut d = Vec::with_capacity(nodes_n * nodes_n * nodes_n);
        let k = |i: usize| i as f64 / (nodes_n - 1) as f64;
        for r in 0..nodes_n {
            for g in 0..nodes_n {
                for b in 0..nodes_n {
                    let e = [k(r), k(g), k(b)];
                    let o = match space {
                        Space::Direct => self.pixel(e),
                        Space::ProPhotoSrgb | Space::ProPhoto18 => {
                            let lin = e.map(srgb_to_linear);
                            let pro = mul3_v(&forward_m, lin);
                            let encoded = |v: f64| match space {
                                Space::ProPhoto18 => v.max(0.0).powf(1.0 / 1.8),
                                _ => linear_to_srgb(v),
                            };
                            let decoded = |v: f64| match space {
                                Space::ProPhoto18 => v.max(0.0).powf(1.8),
                                _ => srgb_to_linear(v),
                            };
                            let t = self.pixel(pro.map(encoded));
                            let back_lin = mul3_v(&her, t.map(decoded));
                            back_lin.map(linear_to_srgb)
                        }
                    };
                    d.push([
                        (o[0] - e[0]) as f32,
                        (o[1] - e[1]) as f32,
                        (o[2] - e[2]) as f32,
                    ]);
                }
            }
        }
        Look::from_table(
            name,
            &format!("{{\"source\":\"filmProfile\",\"colorSpace\":\"{space:?}\"}}"),
            nodes_n,
            d,
        )
    }
}

// ------------------------------------------------------------------------------------ MD5 (for checking only)

fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut m = data.to_vec();
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for blk in m.chunks_exact(64) {
        let w: Vec<u32> = blk
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(w[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut o = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        o[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    o
}

pub fn md5_hex(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

// ------------------------------------------------------------------------------------ Installed profiles

/// Folders of the Camera Raw settings (user, then system).
pub fn folders() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = sysdirs::app_data_dir() {
        v.push(d.join("Adobe/CameraRaw/Settings"));
    }
    if let Some(s) = sysdirs::system_data_dir() {
        v.push(s.join("Adobe/CameraRaw/Settings"));
    }
    v
}

fn attr(header: &str, name: &str) -> Option<String> {
    let k = format!("crs:{name}=\"");
    let i = header.find(&k)? + k.len();
    Some(header[i..i + header[i..].find('"')?].to_string())
}

fn alt_text(tail: &str, element: &str) -> Option<String> {
    let k = format!("<crs:{element}>");
    let i = tail.find(&k)?;
    let rest = &tail[i..];
    let j = rest.find("<rdf:li")?;
    let rest = &rest[j..];
    let a = rest.find('>')? + 1;
    let b = rest[a..].find("</rdf:li>")?;
    let text = rest[a..a + b].trim();
    let xml = format!("<value>{text}</value>");
    let doc = roxmltree::Document::parse(&xml).ok()?;
    Some(
        doc.root_element()
            .text()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control())
            .take(512)
            .collect(),
    )
}

// Adobe's own shipped profile tree is outside the supported third-party sources.
fn adobe_origin(path: &Path) -> bool {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase()).collect();
    parts.windows(2).any(|p| p[0] == "settings" && p[1] == "adobe")
}

/// Read the header data of a profile file (only the start and end of the file, the large table stays unread).
pub fn read_profile(path: &Path) -> Option<FilmProfile> {
    if adobe_origin(path) { return None; }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > manager::MAX_FILE {
        return None;
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube")) { return cube::profile(path); }
    let mut f = std::fs::File::open(path).ok()?;
    let length = f.metadata().ok()?.len();
    let mut header = vec![0u8; 6000.min(length as usize)];
    f.read_exact(&mut header).ok()?;
    let header = String::from_utf8_lossy(&header).to_string();
    if attr(&header, "Copyright").is_some_and(|c| c.to_ascii_lowercase().contains("adobe")) { return None; }
    if header.contains("<!DOCTYPE") || header.contains("<!ENTITY") {
        return None;
    }
    if attr(&header, "PresetType").as_deref() != Some("Look") || attr(&header, "RGBTable").is_none() {
        return None;
    }
    let start_off = length.saturating_sub(4000);
    f.seek(SeekFrom::Start(start_off)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail).to_string();
    let group = alt_text(&tail, "Group").unwrap_or_else(|| {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|n| {
                n.to_string_lossy()
                    .trim_end_matches(" - profiles")
                    .to_string()
            })
            .unwrap_or_default()
    });
    let name = alt_text(&tail, "Name").unwrap_or_else(|| {
        path.file_stem()
            .map(|n| n.to_string_lossy().trim_end_matches(" profile").to_string())
            .unwrap_or_default()
    });
    if name.starts_with("Adobe ") || group.starts_with("Adobe ") || group == "Adobe" {
        return None;
    }
    let uuid = attr(&header, "UUID")?.to_uppercase();
    if uuid.is_empty()
        || uuid.len() > 128
        || !uuid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    // further sliders ≠ default
    let mut additional = Vec::new();
    let standard: BTreeMap<&str, f64> = [
        ("ParametricShadowSplit", 25.0),
        ("ParametricMidtoneSplit", 50.0),
        ("ParametricHighlightSplit", 75.0),
    ]
    .into_iter()
    .collect();
    let mut rest = header.as_str();
    while let Some(i) = rest.find(" crs:") {
        rest = &rest[i + 5..];
        let Some(e) = rest.find("=\"") else { break };
        let n = &rest[..e];
        let w = &rest[e + 2..];
        let Some(q) = w.find('"') else { break };
        if let Ok(v) = w[..q].parse::<f64>() {
            let s = standard.get(n).copied().unwrap_or(0.0);
            if (v - s).abs() > 1e-9 && !matches!(n, "Version" | "ProcessVersion") {
                additional.push((n.to_string(), v));
            }
        }
        rest = &w[q..];
    }
    Some(FilmProfile {
        uuid,
        name,
        group,
        path: path.to_path_buf(),
        source: Source::Automatic,
        available: true,
        strength: attr(&header, "SupportsAmount").is_some_and(|v| v.eq_ignore_ascii_case("true")),
        additional,
    })
}

/// Profile by identifier (`crs:UUID`, case-insensitive).
pub fn by_uuid(uuid: &str) -> Option<FilmProfile> {
    let u = uuid.to_uppercase();
    installed().iter().find(|p| p.uuid == u).cloned()
}

impl FilmProfile {
    /// Read the table and return it as a [`Look`] (name `film:<UUID>`).
    pub fn look(&self) -> Option<Look> {
        manager::look(self)
    }
}

/// Validate the complete XML and referenced table without resolving external entities.
fn look_from_text(text: &str, f: &FilmProfile) -> Result<Look, String> {
    if f.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube")) { return cube::look(text, f); }
    if text.len() as u64 > manager::MAX_FILE
        || text.contains("<!DOCTYPE")
        || text.contains("<!ENTITY")
    {
        return Err("Ungültige oder zu große Profildatei".into());
    }
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100_000,
            entity_resolver: None,
        },
    )
    .map_err(|e| e.to_string())?;
    let key = attr(text, "RGBTable").ok_or("RGB-Tabelle fehlt")?;
    let name = format!("Table_{key}");
    let value = doc
        .descendants()
        .flat_map(|n| n.attributes())
        .find(|a| a.name() == name)
        .map(|a| a.value())
        .ok_or("Verwiesene RGB-Tabelle fehlt")?;
    let (tab, _) = table_from_text(value).ok_or("RGB-Tabelle ist beschädigt")?;
    // The table reference is an opaque identifier, not a checksum of the complete payload.
    tab.to_look(&format!("film:{}", f.uuid), space_from_env(), 33)
        .ok_or("Tabelle lässt sich nicht umrechnen".into())
}

/// Look for a name `film:<UUID>`.
pub fn look_for_names(name: &str) -> Option<Look> {
    by_uuid(name.strip_prefix("film:")?)?.look()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base85_round_trip() {
        let b: Vec<u8> = (0..64u32).map(|i| (i * 37 + 11) as u8).collect();
        let s = base85_encode(&b);
        assert_eq!(s.len(), 80);
        assert_eq!(base85_decode(&s).unwrap(), b);
        // characters that must not appear in XML attributes
        assert!(!s.contains(['&', '<', '>', '"']));
    }

    #[test]
    fn base85_incomplete_last_group() {
        // 7 characters = one full group + one with 2 characters: 8 bytes, the first 4 match
        let b = [1u8, 2, 3, 4];
        let s = base85_encode(&b) + "01";
        let d = base85_decode(&s).unwrap();
        assert_eq!(d.len(), 8);
        assert_eq!(&d[..4], &b);
    }

    #[test]
    fn md5_known() {
        assert_eq!(md5_hex(&md5(b"")), "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(md5_hex(&md5(b"abc")), "900150983CD24FB0D6963F7D28E17F72");
    }

    #[test]
    fn table_round_trip() {
        // 2³ table: identity (stored 0), except output red at (1,1,1) by −0.1 (overflow modulo 65536)
        use std::io::Write;
        let n = 2usize;
        let mut d = Vec::new();
        for w in [1u32, 1, 3, n as u32] {
            d.extend_from_slice(&w.to_le_bytes());
        }
        for r in 0..n {
            for g in 0..n {
                for b in 0..n {
                    let mut v = [0u16; 3];
                    if (r, g, b) == (1, 1, 1) {
                        v[0] = (-6554i32).rem_euclid(65536) as u16;
                    }
                    for x in v {
                        d.extend_from_slice(&x.to_le_bytes());
                    }
                }
            }
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&d).unwrap();
        let mut raw_bytes = (d.len() as u32).to_le_bytes().to_vec();
        raw_bytes.extend(z.finish().unwrap());
        while raw_bytes.len() % 4 != 0 {
            raw_bytes.push(0);
        }
        let (t, _) = table_from_text(&base85_encode(&raw_bytes)).unwrap();
        assert_eq!(t.n, 2);
        assert!((t.output[0][0] - 0.0).abs() < 1e-6 && (t.output[7][1] - 1.0).abs() < 1e-6);
        assert!(
            (t.output[7][0] - 0.9).abs() < 1e-3,
            "white red = 1 − 0.1, was {}",
            t.output[7][0]
        );
    }

    /// All installed film profiles of the user must load and have an effect (no identity).
    #[test]
    #[ignore = "Manual cross-check, no third-party profiles in automated tests"]
    fn load_all_installed() {
        let all = installed();
        if all.is_empty() {
            return;
        }
        let mut error = Vec::new();
        for p in all.iter() {
            match p.look() {
                None => error.push(format!("{}: cannot be loaded", p.name)),
                Some(l) => {
                    let (n, d) = l.table();
                    let strong = d
                        .iter()
                        .step_by(37)
                        .any(|v| v.iter().any(|x| x.abs() > 0.01));
                    if n < 2 || !strong {
                        error.push(format!("{}: has no effect (n={n})", p.name));
                    }
                }
            }
        }
        assert!(error.is_empty(), "{error:?}");
    }

    /// With the user's real file (if installed): checksum of the unpacked table = key of the catalog.
    #[test]
    #[ignore = "Manual cross-check, no third-party profiles in automated tests"]
    fn real_profile_if_present() {
        let Some(p) = installed()
            .iter()
            .find(|p| p.name == "Ilford Delta 100 faded")
            .cloned()
        else {
            return;
        };
        let t = std::fs::read_to_string(&p.path).unwrap();
        let i = t.find(" crs:Table_").unwrap();
        let rest = &t[i + 1..];
        let a = rest.find("=\"").unwrap() + 2;
        let b = rest[a..].find('"').unwrap();
        let (tab, md) = table_from_text(&rest[a..a + b]).unwrap();
        assert_eq!(tab.n, 32);
        assert_eq!(md5_hex(&md), "B8A6292EB33424F01383EB5DFBA80DA2");
        // black & white: output always neutral
        for v in tab.output.iter().step_by(97) {
            assert!(
                (v[0] - v[1]).abs() < 0.002 && (v[1] - v[2]).abs() < 0.002,
                "{v:?}"
            );
        }
    }
}
