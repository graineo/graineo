//! Camera looks: color/tone of the camera JPEG per camera and picture style (film simulation, Picture
//! Style, image mode), measured on own RAW+JPEG pairs.
//!
//! A look is a smooth 3D table (n³ nodes, trilinear) from the **encoded sRGB** of the
//! neutral standard development ([`crate::RawFile::develop`] without look) → encoded sRGB of the
//! camera JPEG. Color and tone only – the camera's sharpening, noise reduction, grain and lens correction
//! are not reproduced. Black & white styles are fitted as channel weights + curve +
//! toning and baked into the same table form.
//!
//! File format `X3LOOK1` (little endian): 8 bytes magic `X3LOOK1\0`, u16 n, u16 name length,
//! name (UTF-8, `camera/style`), u16 meta length, meta (JSON: image count, ΔE00 of the cross-validation …),
//! then n³ × 3 i16: Δ = output − input in units of 1/8192, order r (slow), g, b
//! (fast). A 17³ table thus needs ≈ 29 KB. Meta `"interpolation": "tetrahedral"` selects tetrahedral instead of
//! trilinear interpolation (learned looks, [`crate::learn_look`]).

use rayon::prelude::*;

use crate::picture_style::PictureStyle;
use crate::RgbImage;

/// Longest look name (`camera/style`, bytes).
pub const MAX_NAME: usize = 256;
/// Largest metadata JSON of a look (bytes); real looks have well under 4 KB.
pub const MAX_META: usize = 16 * 1024;
/// Largest table edge length.
pub const MAX_NODES: usize = 65;

/// A loaded camera look.
#[derive(Debug, Clone)]
pub struct Look {
    /// `camera/style`, e.g. `x100v/classic-neg`
    pub name: String,
    /// Metadata (JSON) from the measurement
    pub meta: String,
    n: usize,
    d: Vec<[f32; 3]>,
    /// tetrahedral instead of trilinear interpolation (meta `"interpolation": "tetrahedral"`)
    tetrahedral: bool,
}

fn check(name: &str, meta: &str, n: usize, d: &[[f32; 3]]) -> Result<(), String> {
    if !(2..=MAX_NODES).contains(&n) || d.len() != n * n * n {
        return Err(format!("look table: {n} nodes per edge, {} values", d.len()));
    }
    if name.len() > MAX_NAME {
        return Err(format!("look name too long ({} bytes)", name.len()));
    }
    if meta.len() > MAX_META {
        return Err(format!("look metadata too large ({} bytes, at most {MAX_META})", meta.len()));
    }
    if d.iter().flatten().any(|v| !v.is_finite()) {
        return Err("look table with invalid values (NaN or infinite)".into());
    }
    Ok(())
}

/// Bundled looks (name, data): every file in `looks/<camera>/<style>.x3look`, sorted by name (see `build.rs`).
/// Only styles whose cross-validation showed a clear gain over the neutral development are kept there (at least
/// 75 % of the hold-out images better and mean ΔE00 at least 15 % smaller).
const BUILT_IN: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/looks.rs"));

impl Look {
    /// Read a look from `X3LOOK1` data.
    /// The data must have exactly the announced length (a cut or padded file is no look).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.get(..8)? != b"X3LOOK1\0" {
            return None;
        }
        let u16_at = |o: usize| -> Option<usize> { Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize) };
        let n = u16_at(8)?;
        let ln = u16_at(10)?;
        if !(2..=MAX_NODES).contains(&n) || ln > MAX_NAME {
            return None;
        }
        let name = String::from_utf8(b.get(12..12 + ln)?.to_vec()).ok()?;
        let lm = u16_at(12 + ln)?;
        if lm > MAX_META {
            return None;
        }
        let meta = String::from_utf8(b.get(14 + ln..14 + ln + lm)?.to_vec()).ok()?;
        let start = 14 + ln + lm;
        if b.len() != start + n * n * n * 6 {
            return None;
        }
        let data = b.get(start..start + n * n * n * 6)?;
        let d = data
            .chunks_exact(6)
            .map(|c| {
                let v = |k: usize| i16::from_le_bytes([c[2 * k], c[2 * k + 1]]) as f32 / 8192.0;
                [v(0), v(1), v(2)]
            })
            .collect();
        let tetrahedral = meta_says_tetrahedral(&meta);
        Some(Self { name, meta, n, d, tetrahedral })
    }

    /// `X3LOOK1` data of this look (the format [`Self::from_bytes`] reads). Differences beyond ±4 are clipped.
    /// Errors (instead of a file that reads back as a different table): name longer than [`MAX_NAME`], metadata
    /// longer than [`MAX_META`], values that are not finite.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        check(&self.name, &self.meta, self.n, &self.d)?;
        let mut b = Vec::with_capacity(16 + self.name.len() + self.meta.len() + self.d.len() * 6);
        b.extend_from_slice(b"X3LOOK1\0");
        b.extend_from_slice(&(self.n as u16).to_le_bytes());
        b.extend_from_slice(&(self.name.len() as u16).to_le_bytes());
        b.extend_from_slice(self.name.as_bytes());
        b.extend_from_slice(&(self.meta.len() as u16).to_le_bytes());
        b.extend_from_slice(self.meta.as_bytes());
        for v in &self.d {
            for c in v {
                b.extend_from_slice(&((c * 8192.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
            }
        }
        Ok(b)
    }

    /// Why [`Self::from_table`] would refuse this table (`Ok` if it is fine).
    pub fn check_table(name: &str, meta: &str, n: usize, d: &[[f32; 3]]) -> Result<(), String> {
        check(name, meta, n, d)
    }

    /// Tetrahedral interpolation (instead of trilinear)?
    pub fn tetrahedral(&self) -> bool {
        self.tetrahedral
    }

    /// Look from a finished table (`n`³ nodes, r slow, b fast; Δ = output − input,
    /// encoded sRGB 0…1). `None` if `n` or the number of values does not fit, a value is not finite, or name or
    /// metadata are too long for the file format (see [`Self::check_table`] for the reason).
    pub fn from_table(name: &str, meta: &str, n: usize, d: Vec<[f32; 3]>) -> Option<Self> {
        if check(name, meta, n, &d).is_err() {
            return None;
        }
        Some(Self { name: name.to_string(), meta: meta.to_string(), n, d, tetrahedral: meta_says_tetrahedral(meta) })
    }

    /// Read a look from a file.
    pub fn file(path: &std::path::Path) -> Option<Self> {
        Self::from_bytes(&std::fs::read(path).ok()?)
    }

    /// Load a bundled look by name (`camera/style`).
    pub fn built_in(name: &str) -> Option<Self> {
        BUILT_IN.iter().find(|(n, _)| *n == name).and_then(|(_, b)| Self::from_bytes(b))
    }

    /// Names of all bundled looks.
    pub fn names() -> Vec<&'static str> {
        BUILT_IN.iter().map(|(n, _)| *n).collect()
    }

    /// Matching bundled look for the picture style stored in the file ("as shot").
    pub fn for_picture_style(b: &PictureStyle) -> Option<Self> {
        Self::built_in(&format!("{}/{}", b.camera_short(), b.style))
    }

    /// Table (edge length, Δ per node; r slow, b fast) for the GPU.
    pub fn table(&self) -> (usize, &[[f32; 3]]) {
        (self.n, &self.d)
    }

    /// Edge length of the table.
    pub fn support_points(&self) -> usize {
        self.n
    }

    /// Convert one pixel (encoded sRGB, 0…1).
    ///
    /// Values outside 0…1 (extended sRGB of the wide-gamut display path) are clipped first: the table
    /// is only defined on the sRGB cube, and adding its difference to the unclipped value kept the part
    /// outside the cube as color – a black & white look then left bright or saturated colors visibly
    /// colored instead of gray.
    #[inline]
    pub fn pixel(&self, c: [f32; 3]) -> [f32; 3] {
        let n = self.n;
        let m = (n - 1) as f32;
        let c = c.map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) });
        if self.tetrahedral {
            let mut o = c;
            for (k, w) in tetrahedral_weights(n, c) {
                let v = self.d[k];
                o[0] += w * v[0];
                o[1] += w * v[1];
                o[2] += w * v[2];
            }
            return o.map(|v| v.clamp(0.0, 1.0));
        }
        let p = [c[0] * m, c[1] * m, c[2] * m];
        let i = p.map(|v| (v as usize).min(n - 2));
        let f = [p[0] - i[0] as f32, p[1] - i[1] as f32, p[2] - i[2] as f32];
        let mut o = [c[0], c[1], c[2]];
        for (a, wa) in [(0, 1.0 - f[0]), (1, f[0])] {
            for (b, wb) in [(0, 1.0 - f[1]), (1, f[1])] {
                for (d, wd) in [(0, 1.0 - f[2]), (1, f[2])] {
                    let w = wa * wb * wd;
                    let v = self.d[((i[0] + a) * n + i[1] + b) * n + i[2] + d];
                    o[0] += w * v[0];
                    o[1] += w * v[1];
                    o[2] += w * v[2];
                }
            }
        }
        o.map(|v| v.clamp(0.0, 1.0))
    }

    /// `.cube` text of this look (33³, sRGB → sRGB, red fastest) with the strength `amount` (0…1, as the
    /// "Intensity" slider mixes the look). Samples exactly the table the development uses.
    pub fn cube_text(&self, title: &str, amount: f32) -> Result<String, String> {
        let a = amount.clamp(0.0, 1.0);
        let values: Vec<[f32; 3]> = graineo_color::cube::grid(33)
            .into_iter()
            .map(|c| {
                let o = self.pixel(c);
                std::array::from_fn(|k| c[k] + a * (o[k] - c[k]))
            })
            .collect();
        graineo_color::cube::write(
            title,
            &["sRGB encoded input and output", "Look only: no exposure, white balance, tone curve, local adjustments, sharpening, noise reduction or grain"],
            33,
            &values,
        )
    }

    /// Apply to a finished 8-bit sRGB image of the neutral development.
    pub fn apply(&self, img: &mut RgbImage) {
        let w = img.width() as usize * 3;
        img.as_mut().par_chunks_mut(w.max(3)).for_each(|row| {
            for p in row.chunks_exact_mut(3) {
                let o = self.pixel([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]);
                for c in 0..3 {
                    p[c] = (o[c] * 255.0 + 0.5) as u8;
                }
            }
        });
    }
}

fn meta_says_tetrahedral(meta: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(meta).ok().and_then(|v| v.get("interpolation").and_then(|i| i.as_str()).map(|i| i == "tetrahedral")).unwrap_or(false)
}

/// Tetrahedral interpolation on an `n`³ grid over the unit cube (r slow, b fast): the four nodes of the tetrahedron
/// holding `c` (clamped to 0…1) and their weights (sum 1). Exact at the nodes; along the gray axis only gray nodes count.
pub fn tetrahedral_weights(n: usize, c: [f32; 3]) -> [(usize, f32); 4] {
    let m = (n - 1) as f32;
    let p = c.map(|v| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) * m });
    let i = p.map(|v| (v as usize).min(n - 2));
    let f = [p[0] - i[0] as f32, p[1] - i[1] as f32, p[2] - i[2] as f32];
    let at = |a: usize, b: usize, d: usize| ((i[0] + a) * n + i[1] + b) * n + i[2] + d;
    let (r, g, b) = (f[0], f[1], f[2]);
    let c000 = at(0, 0, 0);
    let c111 = at(1, 1, 1);
    if r >= g && g >= b {
        [(c000, 1.0 - r), (at(1, 0, 0), r - g), (at(1, 1, 0), g - b), (c111, b)]
    } else if r >= b && b > g {
        [(c000, 1.0 - r), (at(1, 0, 0), r - b), (at(1, 0, 1), b - g), (c111, g)]
    } else if b > r && r >= g {
        [(c000, 1.0 - b), (at(0, 0, 1), b - r), (at(1, 0, 1), r - g), (c111, g)]
    } else if g > r && r >= b {
        [(c000, 1.0 - g), (at(0, 1, 0), g - r), (at(1, 1, 0), r - b), (c111, b)]
    } else if g >= b && b > r {
        [(c000, 1.0 - g), (at(0, 1, 0), g - b), (at(0, 1, 1), b - r), (c111, r)]
    } else {
        [(c000, 1.0 - b), (at(0, 0, 1), b - g), (at(0, 1, 1), g - r), (c111, r)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip_and_tetrahedral_is_exact_at_nodes() {
        let n = 5;
        let d: Vec<[f32; 3]> = (0..n * n * n).map(|k| [0.01 * (k % 7) as f32, -0.02 * (k % 3) as f32, 0.005 * (k % 11) as f32]).collect();
        let l = Look::from_table("cam/style", r#"{"interpolation":"tetrahedral"}"#, n, d.clone()).unwrap();
        assert!(l.tetrahedral());
        let back = Look::from_bytes(&l.to_bytes().unwrap()).unwrap();
        assert!(back.tetrahedral() && back.name == "cam/style" && back.support_points() == n);
        for (a, b) in back.table().1.iter().zip(&d) {
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1.0 / 8192.0);
            }
        }
        // at every node the tetrahedral and the trilinear look agree (both give the node value)
        let tri = Look::from_table("cam/style", "{}", n, d.clone()).unwrap();
        assert!(!tri.tetrahedral());
        for k in 0..n * n * n {
            let x = [k / (n * n), (k / n) % n, k % n].map(|v| v as f32 / (n - 1) as f32);
            let (a, b) = (back.pixel(x), tri.pixel(x));
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 2e-4, "{x:?}: {a:?} {b:?}");
            }
        }
        // weights sum to one, also between nodes
        for x in [[0.13, 0.77, 0.4], [0.9, 0.1, 0.5], [0.5, 0.5, 0.5], [1.0, 0.0, 0.3]] {
            let w = tetrahedral_weights(n, x);
            assert!((w.iter().map(|v| v.1).sum::<f32>() - 1.0).abs() < 1e-5);
            assert!(w.iter().all(|v| v.1 >= -1e-6));
        }
    }

    #[test]
    fn oversized_or_broken_looks_are_refused() {
        let n = 3;
        let d = vec![[0.01f32, 0.0, -0.01]; n * n * n];
        // metadata over the 16-bit length field: once silently cut to (len mod 65536), now refused everywhere
        let huge = format!(r#"{{"source":{{"name":"x","pad":"{}"}}}}"#, "A".repeat(70_000));
        assert!(Look::from_table("cam/std", &huge, n, d.clone()).is_none());
        assert!(Look::check_table("cam/std", &huge, n, &d).unwrap_err().contains("metadata"));
        let mut l = Look::from_table("cam/std", "{}", n, d.clone()).unwrap();
        l.meta = huge.clone();
        assert!(l.to_bytes().is_err(), "no file with a wrapped length");
        l.meta = "{}".into();
        l.name = "c/".to_string() + &"s".repeat(MAX_NAME);
        assert!(l.to_bytes().is_err());
        // a file written the old way (length field wrapped): the rest of the JSON would be read as table
        let mut wrapped = b"X3LOOK1\0".to_vec();
        wrapped.extend_from_slice(&(n as u16).to_le_bytes());
        wrapped.extend_from_slice(&7u16.to_le_bytes());
        wrapped.extend_from_slice(b"cam/std");
        wrapped.extend_from_slice(&((huge.len() % 65536) as u16).to_le_bytes());
        wrapped.extend_from_slice(huge.as_bytes());
        wrapped.extend(std::iter::repeat_n(0u8, n * n * n * 6));
        assert!(Look::from_bytes(&wrapped).is_none());
        // NaN or infinite values
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut d2 = d.clone();
            d2[5][1] = bad;
            assert!(Look::from_table("cam/std", "{}", n, d2).is_none());
        }
        // cut, padded, wrong sizes
        let good = Look::from_table("cam/std", "{}", n, d.clone()).unwrap().to_bytes().unwrap();
        assert!(Look::from_bytes(&good).is_some());
        assert!(Look::from_bytes(&good[..good.len() - 1]).is_none());
        let mut padded = good.clone();
        padded.push(0);
        assert!(Look::from_bytes(&padded).is_none());
        let mut big_n = good.clone();
        big_n[8..10].copy_from_slice(&66u16.to_le_bytes());
        assert!(Look::from_bytes(&big_n).is_none());
        assert!(Look::from_table("cam/std", "{}", 66, vec![[0.0; 3]; 66 * 66 * 66]).is_none());
        assert!(Look::from_bytes(&[]).is_none() && Look::from_bytes(b"X3LOOK1\0").is_none());
    }

    #[test]
    fn built_in_looks_readable() {
        for (n, b) in BUILT_IN {
            let l = Look::from_bytes(b).expect("look readable");
            assert_eq!(&l.name, n);
        }
    }

    /// Black & white looks (all Fujifilm Monochrome/Acros, Ricoh Monotone/B&W, gray at every table node)
    /// give R = G = B for every input – also for saturated, overexposed or out-of-gamut values of the
    /// extended display path (below 0, above 1). Color looks stay in 0…1 there as well.
    #[test]
    fn monochrome_looks_stay_gray_for_all_inputs() {
        let mut values = vec![-0.6f32, -0.2, 0.0, 0.03, 0.2, 0.5, 0.77, 0.99, 1.0, 1.3, 2.5, f32::NAN];
        values.extend((0..9).map(|k| k as f32 / 8.0));
        let mut mono = 0;
        for name in Look::names() {
            let l = Look::built_in(name).unwrap();
            let (n, d) = l.table();
            // gray table = every node output R = G = B
            let gray = (0..d.len()).all(|k| {
                let i = [k / (n * n), (k / n) % n, k % n].map(|v| v as f32 / (n - 1) as f32);
                let o = [i[0] + d[k][0], i[1] + d[k][1], i[2] + d[k][2]].map(|v| v.clamp(0.0, 1.0));
                (o[0] - o[1]).abs() < 1e-3 && (o[1] - o[2]).abs() < 1e-3
            });
            let style = name.split('/').nth(1).unwrap();
            let is_bw = ["monochrome", "acros", "monotone", "bw"].iter().any(|k| style.starts_with(k));
            assert_eq!(gray, is_bw, "{name}: table gray = {gray}, name says black & white = {is_bw}");
            if gray {
                mono += 1;
            }
            for &r in &values {
                for &g in &values {
                    for &b in &values {
                        let o = l.pixel([r, g, b]);
                        assert!(o.iter().all(|v| (0.0..=1.0).contains(v)), "{name} {:?} → {o:?}", [r, g, b]);
                        if gray {
                            assert!((o[0] - o[1]).abs() < 1e-3 && (o[1] - o[2]).abs() < 1e-3, "{name} {:?} → {o:?}", [r, g, b]);
                        }
                    }
                }
            }
        }
        assert!(mono >= 10, "black & white looks found: {mono}");
    }
}
