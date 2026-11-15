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
//! (fast). A 17³ table thus needs ≈ 29 KB.

use rayon::prelude::*;

use crate::picture_style::PictureStyle;
use crate::RgbImage;

/// A loaded camera look.
#[derive(Debug, Clone)]
pub struct Look {
    /// `camera/style`, e.g. `x100v/classic-neg`
    pub name: String,
    /// Metadata (JSON) from the measurement
    pub meta: String,
    n: usize,
    d: Vec<[f32; 3]>,
}

/// Bundled looks (name, data): every file in `looks/<camera>/<style>.x3look`, sorted by name (see `build.rs`).
/// Only styles whose cross-validation showed a clear gain over the neutral development are kept there (at least
/// 75 % of the hold-out images better and mean ΔE00 at least 15 % smaller).
const BUILT_IN: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/looks.rs"));

impl Look {
    /// Read a look from `X3LOOK1` data.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.get(..8)? != b"X3LOOK1\0" {
            return None;
        }
        let u16_at = |o: usize| -> Option<usize> { Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize) };
        let n = u16_at(8)?;
        let ln = u16_at(10)?;
        let name = String::from_utf8(b.get(12..12 + ln)?.to_vec()).ok()?;
        let lm = u16_at(12 + ln)?;
        let meta = String::from_utf8(b.get(14 + ln..14 + ln + lm)?.to_vec()).ok()?;
        let start = 14 + ln + lm;
        if !(2..=65).contains(&n) {
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
        Some(Self { name, meta, n, d })
    }

    /// Look from a finished table (`n`³ nodes, r slow, b fast; Δ = output − input,
    /// encoded sRGB 0…1). `None` if `n` or the number of values does not fit.
    pub fn from_table(name: &str, meta: &str, n: usize, d: Vec<[f32; 3]>) -> Option<Self> {
        if !(2..=65).contains(&n) || d.len() != n * n * n {
            return None;
        }
        Some(Self { name: name.to_string(), meta: meta.to_string(), n, d })
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

#[cfg(test)]
mod tests {
    use super::*;

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
