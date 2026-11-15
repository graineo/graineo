//! Color according to the DNG specification 1.7, chapter 6 ("Mapping Camera Color Space to CIE XYZ Space"):
//! interpolation of the camera matrices of two illuminants via the color temperature of the
//! as-shot neutral, ForwardMatrix (if present) or inverted ColorMatrix with
//! Bradford adaptation to D50. Own code following the public specification; matrices come
//! from the file (DNG) or from rawler (CR2/CR3/RAF/ARW). No Adobe profiles (DCP).

pub type M3 = [[f32; 3]; 3];

pub const D50_XY: [f32; 2] = [0.345_67, 0.358_50];
pub const D65_XY: [f32; 2] = [0.312_70, 0.329_00];

/// Camera matrix of one illuminant.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    /// Color temperature of the illuminant in Kelvin
    pub kelvin: f32,
    /// XYZ → camera (ColorMatrix)
    pub color: M3,
    /// Camera (white balanced) → XYZ D50 (ForwardMatrix), if present
    pub forward: Option<M3>,
}

/// Result of the color determination for one image.
#[derive(Debug, Clone, Copy)]
pub struct Color {
    /// white-balanced camera RGB → XYZ (D50); maps (1,1,1) to D50 white with Y = 1
    pub camera_to_xyz_d50: M3,
    /// estimated color temperature of the as-shot white balance
    pub kelvin: f32,
    /// weight of the first (warmer) illuminant, 0…1
    pub weight: f32,
    pub forward: bool,
}

/// Color temperature for an EXIF/DNG illuminant (LightSource code).
pub fn kelvin_illuminant(code: u16) -> Option<f32> {
    Some(match code {
        17 | 3 | 24 => 2856.0, // A, tungsten, ISO studio tungsten
        18 => 4874.0,          // B
        19 => 6774.0,          // C
        20 => 5503.0,          // D55
        21 => 6504.0,          // D65
        22 => 7504.0,          // D75
        23 => 5003.0,          // D50
        1 | 4 | 9 => 5500.0,   // daylight, flash, fine weather
        10 => 6500.0,          // cloudy
        11 => 7500.0,          // shade
        2 | 14 => 4150.0,      // fluorescent, cool white
        12 => 6430.0,
        13 => 5000.0,
        15 => 3450.0,
        _ => return None,
    })
}

pub fn xy_to_xyz(xy: [f32; 2]) -> [f32; 3] {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

pub fn xyz_to_xy(v: [f32; 3]) -> [f32; 2] {
    let s = v[0] + v[1] + v[2];
    if s <= 0.0 {
        return D50_XY;
    }
    [v[0] / s, v[1] / s]
}

/// Correlated color temperature (McCamy approximation, sufficient for 2000–12000 K).
pub fn kelvin_from_xy(xy: [f32; 2]) -> f32 {
    let n = (xy[0] - 0.3320) / (0.1858 - xy[1]);
    (449.0 * n * n * n + 3525.0 * n * n + 6823.3 * n + 5520.33).clamp(1500.0, 50000.0)
}

pub fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

pub fn mulv(a: &M3, v: [f32; 3]) -> [f32; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

pub fn diag(v: [f32; 3]) -> M3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

pub fn inv(m: &M3) -> Option<M3> {
    let m: [[f64; 3]; 3] = m.map(|r| r.map(|v| v as f64));
    let cof = |i: usize, j: usize| {
        let (r0, r1) = ((i + 1) % 3, (i + 2) % 3);
        let (c0, c1) = ((j + 1) % 3, (j + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let det: f64 = (0..3).map(|j| m[0][j] * cof(0, j)).sum();
    if det.abs() < 1e-12 {
        return None;
    }
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[j][i] = (cof(i, j) / det) as f32;
        }
    }
    Some(r)
}

fn lerp(a: &M3, b: &M3, t: f32) -> M3 {
    let mut r = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][j] * t + b[i][j] * (1.0 - t);
        }
    }
    r
}

/// Bradford adaptation from white `src` to `dst` (XYZ).
pub fn bradford(src: [f32; 3], dst: [f32; 3]) -> M3 {
    const B: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let bi = inv(&B).expect("Bradford invertible");
    let s = mulv(&B, src);
    let d = mulv(&B, dst);
    mul(&bi, &mul(&diag([d[0] / s[0], d[1] / s[1], d[2] / s[2]]), &B))
}

/// RGB → XYZ from primaries and white point.
pub fn rgb_to_xyz(r: [f32; 2], g: [f32; 2], b: [f32; 2], w: [f32; 2]) -> M3 {
    let p = [xy_to_xyz(r), xy_to_xyz(g), xy_to_xyz(b)];
    let pm: M3 = [[p[0][0], p[1][0], p[2][0]], [p[0][1], p[1][1], p[2][1]], [p[0][2], p[1][2], p[2][2]]];
    let s = mulv(&inv(&pm).expect("primaries"), xy_to_xyz(w));
    mul(&pm, &diag(s))
}

pub fn srgb_to_xyz_d65() -> M3 {
    rgb_to_xyz([0.64, 0.33], [0.30, 0.60], [0.15, 0.06], D65_XY)
}

pub fn rec2020_to_xyz_d65() -> M3 {
    rgb_to_xyz([0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65_XY)
}

pub fn adobe_rgb_to_xyz_d65() -> M3 {
    rgb_to_xyz([0.64, 0.33], [0.21, 0.71], [0.15, 0.06], D65_XY)
}

/// XYZ (D50) → linear RGB of a D65 space (Bradford D50→D65).
pub fn xyz_d50_to(rgb_to_xyz_d65: &M3) -> M3 {
    let ad = bradford(xy_to_xyz(D50_XY), xy_to_xyz(D65_XY));
    mul(&inv(rgb_to_xyz_d65).expect("RGB matrix"), &ad)
}

/// Determine color according to the DNG specification. `neutral`: camera neutral of the as-shot WB
/// (AsShotNeutral or 1/WB multiplier), `cal`: one or two illuminants,
/// `ab_cc`: AnalogBalance·CameraCalibration (identity matrix if not present).
pub fn determine(neutral: [f32; 3], cal: &[Calibration], ab_cc: &M3) -> Option<Color> {
    if cal.is_empty() {
        return None;
    }
    let mut ks: Vec<Calibration> = cal.to_vec();
    ks.sort_by(|a, b| a.kelvin.total_cmp(&b.kelvin));
    let (k1, k2) = (ks[0], *ks.last().unwrap());
    let weight_for = |t: f32| -> f32 {
        if ks.len() < 2 || (k1.kelvin - k2.kelvin).abs() < 1.0 {
            return 1.0;
        }
        ((1.0 / t - 1.0 / k2.kelvin) / (1.0 / k1.kelvin - 1.0 / k2.kelvin)).clamp(0.0, 1.0)
    };
    // neutral → xy iteratively (specification: ColorMatrix depends on the white point)
    let mut xy = D50_XY;
    for _ in 0..30 {
        let g = weight_for(kelvin_from_xy(xy));
        let cm = mul(ab_cc, &lerp(&k1.color, &k2.color, g));
        let xyz = mulv(&inv(&cm)?, neutral);
        let nxy = xyz_to_xy(xyz);
        let d = (nxy[0] - xy[0]).abs() + (nxy[1] - xy[1]).abs();
        xy = nxy;
        if d < 1e-7 {
            break;
        }
    }
    let t = kelvin_from_xy(xy);
    let g = weight_for(t);
    let fm = match (k1.forward, k2.forward) {
        (Some(a), Some(b)) => Some(lerp(&a, &b, g)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        _ => None,
    };
    let m = if let Some(fm) = fm {
        // CameraToXYZ_D50 = FM · D · (AB·CC)^-1 with D = diag(1/ReferenceNeutral); our data are
        // already multiplied by 1/neutral → D·(AB·CC)^-1 on (AB·CC)^-1 · ref-neutral normalization
        let abcc_inv = inv(ab_cc)?;
        let refn = mulv(&abcc_inv, neutral);
        // data d = cam / neutral  ⇒ cam = diag(neutral)·d
        mul(&fm, &mul(&diag([1.0 / refn[0], 1.0 / refn[1], 1.0 / refn[2]]), &mul(&abcc_inv, &diag(neutral))))
    } else {
        let cm = mul(ab_cc, &lerp(&k1.color, &k2.color, g));
        let ca = bradford(xy_to_xyz(xy), xy_to_xyz(D50_XY));
        let m = mul(&ca, &mul(&inv(&cm)?, &diag(neutral)));
        // (1,1,1) → Y = 1
        let y = mulv(&m, [1.0, 1.0, 1.0])[1];
        if y <= 0.0 {
            return None;
        }
        m.map(|r| r.map(|v| v / y))
    };
    Some(Color { camera_to_xyz_d50: m, kelvin: t, weight: g, forward: fm.is_some() })
}

/// Weight of the warmer illuminant for color temperature `t` (DNG specification: linear in 1/T).
fn weight(ks: &[Calibration], t: f32) -> f32 {
    let (k1, k2) = (ks[0], *ks.last().unwrap());
    if ks.len() < 2 || (k1.kelvin - k2.kelvin).abs() < 1.0 {
        return 1.0;
    }
    ((1.0 / t - 1.0 / k2.kelvin) / (1.0 / k1.kelvin - 1.0 / k2.kelvin)).clamp(0.0, 1.0)
}

fn sorted(cal: &[Calibration]) -> Vec<Calibration> {
    let mut ks: Vec<Calibration> = cal.to_vec();
    ks.sort_by(|a, b| a.kelvin.total_cmp(&b.kelvin));
    ks
}

/// White point (xy) of a camera neutral (iterative as in [`determine`]).
pub fn neutral_to_xy(neutral: [f32; 3], cal: &[Calibration], ab_cc: &M3) -> Option<[f32; 2]> {
    if cal.is_empty() {
        return None;
    }
    let ks = sorted(cal);
    let mut xy = D50_XY;
    for _ in 0..30 {
        let g = weight(&ks, kelvin_from_xy(xy));
        let cm = mul(ab_cc, &lerp(&ks[0].color, &ks.last().unwrap().color, g));
        let nxy = xyz_to_xy(mulv(&inv(&cm)?, neutral));
        let d = (nxy[0] - xy[0]).abs() + (nxy[1] - xy[1]).abs();
        xy = nxy;
        if d < 1e-7 {
            break;
        }
    }
    Some(xy)
}

/// Camera neutral (green = 1) for a white point xy: ColorMatrix at its color temperature
/// (inverse of [`neutral_to_xy`]).
pub fn xy_to_neutral(xy: [f32; 2], cal: &[Calibration], ab_cc: &M3) -> Option<[f32; 3]> {
    if cal.is_empty() {
        return None;
    }
    let ks = sorted(cal);
    let g = weight(&ks, kelvin_from_xy(xy));
    let cm = mul(ab_cc, &lerp(&ks[0].color, &ks.last().unwrap().color, g));
    let n = mulv(&cm, xy_to_xyz(xy));
    (n[1] > 0.0 && n.iter().all(|v| v.is_finite() && *v > 0.0)).then(|| [n[0] / n[1], 1.0, n[2] / n[1]])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn srgb_white() {
        let m = xyz_d50_to(&srgb_to_xyz_d65());
        let w = mulv(&m, xy_to_xyz(D50_XY));
        for v in w {
            assert!((v - 1.0).abs() < 1e-3, "{w:?}");
        }
    }
}
