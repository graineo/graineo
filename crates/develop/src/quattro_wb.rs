//! Quattro white balance "ColorTemp" (CAMF white balance code 11): gains and colour correction matrix for a colour
//! temperature in kelvin, computed from the CAMF tables like the camera maker's software does.
//!
//! The camera stores no `WhiteBalanceGains` entry for this setting. Instead:
//! - `ColorTempTableInfo` (rows of 12 floats: kelvin, two chromaticity ratios, 3 × 3 matrix) is interpolated
//!   linearly in kelvin (clamped at both ends);
//! - the ratios go through `AdjustWhitebalanceMatrix02` and `AdjustWhitebalanceMatrix01` (second 3 × 3 block each)
//!   to the gains, multiplied by `AGain`;
//! - the colour correction matrix is the interpolated table matrix times a matrix blended from the three blocks of
//!   `AdjustColorMatrix` over the mired positions `Index_AdjustColorMatrix`.
//!
//! The arithmetic keeps the maker's mix of single and double precision. Every table must be present with the
//! expected size; otherwise `None` (the caller falls back to the automatic white balance and says so).

use crate::matrix::Mat3;

/// CAMF tables used for "ColorTemp".
#[derive(Clone, Debug)]
pub struct ColorTempTables {
    /// `ColorTempTableInfo`: n rows of 12 values
    pub table: Vec<f64>,
    /// `AGain`
    pub a_gain: [f64; 3],
    /// `AdjustWhitebalanceMatrix01`, 18 values (two 3 × 3 blocks, the second is used)
    pub adjust_wb_01: Vec<f64>,
    /// `AdjustWhitebalanceMatrix02`, 18 values
    pub adjust_wb_02: Vec<f64>,
    /// `Index_AdjustColorMatrix`: mired positions of the three blocks of `AdjustColorMatrix`
    pub index: [f64; 3],
    /// `AdjustColorMatrix`, 27 values (three 3 × 3 blocks)
    pub adjust_color: Vec<f64>,
}

impl ColorTempTables {
    pub fn read(camf: &x3f::Camf) -> Option<Self> {
        let get = |n: &str| camf.matrix(n).ok().map(|m| m.as_f64());
        let v3 = |n: &str| get(n).filter(|v| v.len() == 3).map(|v| [v[0], v[1], v[2]]);
        let t = Self {
            table: get("ColorTempTableInfo")?,
            a_gain: v3("AGain")?,
            adjust_wb_01: get("AdjustWhitebalanceMatrix01")?,
            adjust_wb_02: get("AdjustWhitebalanceMatrix02")?,
            index: v3("Index_AdjustColorMatrix")?,
            adjust_color: get("AdjustColorMatrix")?,
        };
        t.valid().then_some(t)
    }

    fn valid(&self) -> bool {
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        !self.table.is_empty()
            && self.table.len() % 12 == 0
            && self.adjust_wb_01.len() == 18
            && self.adjust_wb_02.len() == 18
            && self.adjust_color.len() == 27
            && finite(&self.table)
            && finite(&self.a_gain)
            && finite(&self.adjust_wb_01)
            && finite(&self.adjust_wb_02)
            && finite(&self.index)
            && finite(&self.adjust_color)
    }
}

/// Interpolated table row: kelvin, ratios (a, b = 1, c) in f32, matrix in f64.
struct Row {
    ratio: [f32; 3],
    matrix: [f64; 9],
}

fn row(t: &[f64], i: usize) -> Row {
    let r = &t[i * 12..i * 12 + 12];
    // the second ratio is not stored; the table entry keeps its default 1.0
    Row { ratio: [r[1] as f32, 1.0, r[2] as f32], matrix: std::array::from_fn(|k| r[3 + k] as f32 as f64) }
}

fn interpolate(t: &[f64], kelvin: f32) -> Row {
    let n = t.len() / 12;
    let k = |i: usize| t[i * 12] as f32;
    if !(k(0) < kelvin) {
        return row(t, 0);
    }
    if !(kelvin < k(n - 1)) {
        return row(t, n - 1);
    }
    let hi = (1..n).find(|&i| kelvin < k(i)).unwrap_or(n - 1);
    let lo = hi - 1;
    let w = (k(hi) - kelvin) / (k(hi) - k(lo)); // weight of the lower row
    let v = 1.0 - w;
    let (a, b) = (row(t, lo), row(t, hi));
    Row {
        ratio: std::array::from_fn(|c| b.ratio[c] * v + a.ratio[c] * w),
        matrix: std::array::from_fn(|c| b.matrix[c] * v as f64 + a.matrix[c] * w as f64),
    }
}

/// Gains (before normalisation) and colour correction matrix for `kelvin`.
pub fn gains_and_matrix(t: &ColorTempTables, kelvin: f32) -> Option<([f64; 3], Mat3)> {
    if !t.valid() || !kelvin.is_finite() || kelvin <= 0.0 {
        return None;
    }
    let mired = (1.0e6 / kelvin as f64) as f32;
    let r = interpolate(&t.table, kelvin);
    let [a, b, c] = r.ratio;
    let sum = a + b + c;
    let v = [((a - b) / sum) as f64, (b / sum) as f64, 1.0f64];
    let block = |m: &[f64], i: usize, v: [f64; 3]| (v[0] * m[9 + 3 * i] + v[1] * m[10 + 3 * i] + v[2] * m[11 + 3 * i]) as f32;
    let w: [f64; 3] = std::array::from_fn(|i| block(&t.adjust_wb_02, i, v) as f64);
    let u: [f32; 3] = std::array::from_fn(|i| block(&t.adjust_wb_01, i, w));
    let (p, q) = (u[0] / u[2], u[1] / u[2]);
    let inv_q = 1.0f32 / q;
    let s0 = (p + q) * inv_q;
    let s2 = ((1.0f32 - (p + q)) - q) * inv_q;
    let gain = [(1.0f32 / s0) as f64 * t.a_gain[0], 1.0 * t.a_gain[1], (1.0f32 / s2) as f64 * t.a_gain[2]];

    let m = mired as f64;
    let [i0, i1, i2] = t.index;
    let blk = |j: usize| -> [f64; 9] { std::array::from_fn(|k| t.adjust_color[j * 9 + k] as f32 as f64) };
    let mix = |x: [f64; 9], y: [f64; 9], w: f64| -> [f64; 9] { std::array::from_fn(|k| x[k] * w + y[k] * (1.0 - w)) };
    // Outside the last position the maker's software leaves its matrix at its initial value (all elements 1.0).
    let adjust = if m < i1 {
        mix(blk(0), blk(1), (i1 - m) / (i1 - i0))
    } else if m < i2 {
        mix(blk(1), blk(2), (i2 - m) / (i2 - i1))
    } else {
        [1.0; 9]
    };
    let ccm = Mat3(r.matrix).mul(&Mat3(adjust));
    (gain.iter().all(|g| g.is_finite() && *g > 0.0) && ccm.0.iter().all(|x| x.is_finite())).then_some((gain, ccm))
}

/// Kelvin of the shot (`ColorTempValue`, default 5400 K as in the maker's software).
pub fn shot_kelvin(camf: &x3f::Camf) -> f32 {
    camf.matrix("ColorTempValue").ok().and_then(|m| m.as_f64().first().copied()).filter(|k| k.is_finite() && *k > 0.0).map_or(5400.0, |k| k as f32)
}

/// Name of the white balance preset for a colour temperature other than the shot's.
pub fn preset_name(kelvin: f32) -> String {
    format!("ColorTemp {:.0}K", kelvin)
}

/// Kelvin of a preset name made by [`preset_name`] (or plain "ColorTemp" → `None`, meaning the shot's value).
pub fn kelvin_of(name: &str) -> Option<f32> {
    name.strip_prefix("ColorTemp ")?.strip_suffix('K')?.parse().ok().filter(|k: &f32| k.is_finite() && *k > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_tables() -> ColorTempTables {
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let mut table = Vec::new();
        for (k, a, c) in [(2000.0, 0.6, 1.6), (5000.0, 1.0, 1.0), (8000.0, 1.4, 0.7)] {
            table.extend([k, a, c]);
            table.extend(id);
        }
        let wb: Vec<f64> = id.iter().chain(id.iter()).copied().collect();
        ColorTempTables {
            table,
            a_gain: [1.0; 3],
            adjust_wb_01: wb.clone(),
            adjust_wb_02: wb,
            index: [150.0, 300.0, 480.0],
            adjust_color: id.iter().chain(id.iter()).chain(id.iter()).copied().collect(),
        }
    }

    #[test]
    fn neutral_ratios_give_neutral_gains() {
        // a = b = c = 1 → (x, y) = (0, 1/3) → s = (1, 1, 2) … the formula is the maker's; check it is finite and
        // reproduces the expected values for the identity tables.
        let t = identity_tables();
        let (g, m) = gains_and_matrix(&t, 5000.0).unwrap();
        // x = 0, y = 1/3, z = 1 → p = 0, q = 1/3 → s0 = 1, s2 = (1 − 1/3 − 1/3)·3 = 1
        assert!((g[0] - 1.0).abs() < 1e-6 && (g[2] - 1.0).abs() < 1e-6, "{g:?}");
        assert!((m.0[0] - 1.0).abs() < 1e-9 && m.0[1].abs() < 1e-9);
    }

    #[test]
    fn clamps_and_interpolates() {
        let t = identity_tables();
        let lo = gains_and_matrix(&t, 1000.0).unwrap().0;
        let first = gains_and_matrix(&t, 2000.0).unwrap().0;
        assert_eq!(lo, first);
        let mid = gains_and_matrix(&t, 3500.0).unwrap().0;
        assert!(mid[0] > first[0].min(1.0) - 1e-9);
        // beyond the last mired position: matrix of ones (maker's initial value)
        let (_, m) = gains_and_matrix(&t, 2000.0).unwrap();
        assert!(m.0.iter().all(|v| (v - 1.0).abs() < 1e-9), "{:?}", m.0);
    }

    #[test]
    fn rejects_broken_tables() {
        let mut t = identity_tables();
        t.table.pop();
        assert!(gains_and_matrix(&t, 5000.0).is_none());
        let mut t = identity_tables();
        t.adjust_color.truncate(9);
        assert!(gains_and_matrix(&t, 5000.0).is_none());
        let t = identity_tables();
        assert!(gains_and_matrix(&t, f32::NAN).is_none());
        assert!(gains_and_matrix(&t, 0.0).is_none());
        // degenerate ratios (division by zero) → None instead of infinite gains
        let mut t = identity_tables();
        for v in t.adjust_wb_01.iter_mut() {
            *v = 0.0;
        }
        assert!(gains_and_matrix(&t, 5000.0).is_none());
    }

    #[test]
    fn preset_names_round_trip() {
        assert_eq!(kelvin_of(&preset_name(6500.0)), Some(6500.0));
        assert_eq!(kelvin_of("ColorTemp"), None);
        assert_eq!(kelvin_of("Auto"), None);
    }

    /// Real CAMF tables (dp Quattro sample) when available: 5400 K must be close to the daylight preset.
    #[test]
    fn real_tables_near_daylight() {
        // GRAINEO_TEST_QUATTRO = path of a dp Quattro X3F (e.g. testbilder/dp1q/…); skipped without it
        let Some(p) = std::env::var_os("GRAINEO_TEST_QUATTRO") else { return };
        let Ok(f) = x3f::X3fFile::open(std::path::Path::new(&p)) else { return };
        let Ok(camf) = f.camf() else { return };
        let t = ColorTempTables::read(&camf).expect("tables");
        let (g, _) = gains_and_matrix(&t, 5400.0).unwrap();
        let sun = camf.matrix("SunlightWBGain").unwrap().as_f64();
        eprintln!("ColorTemp 5400 K gains {g:?}, Sunlight {sun:?}");
        for c in [0, 2] {
            assert!((g[c] / g[1] - sun[c] / sun[1]).abs() < 0.15, "{g:?} vs {sun:?}");
        }
    }
}
