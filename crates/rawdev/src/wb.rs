//! White balance as color temperature and tint, converted via the camera matrices
//! according to the DNG specification ([`raw::color::neutral_to_xy`], [`raw::color::xy_to_neutral`]).
//!
//! Temperature: correlated color temperature after Robertson (1968) with the published table of
//! isotherms (Wyszecki & Stiles, "Color Science", Table 1(3.11)). Tint: distance from the
//! Planckian locus along the isotherm in CIE 1960 uv, 1 unit = 1/3000 (+ = magenta,
//! − = green) – the same order of magnitude as the tint slider of common photo apps. Own code.

use raw::color;

/// Robertson table: mired, u, v, slope of the isotherm.
const TAB: [[f64; 4]; 31] = [
    [0.0, 0.18006, 0.26352, -0.24341],
    [10.0, 0.18066, 0.26589, -0.25479],
    [20.0, 0.18133, 0.26846, -0.26876],
    [30.0, 0.18208, 0.27119, -0.28539],
    [40.0, 0.18293, 0.27407, -0.30470],
    [50.0, 0.18388, 0.27709, -0.32675],
    [60.0, 0.18494, 0.28021, -0.35156],
    [70.0, 0.18611, 0.28342, -0.37915],
    [80.0, 0.18740, 0.28668, -0.40955],
    [90.0, 0.18880, 0.28997, -0.44278],
    [100.0, 0.19032, 0.29326, -0.47888],
    [125.0, 0.19462, 0.30141, -0.58204],
    [150.0, 0.19962, 0.30921, -0.70471],
    [175.0, 0.20525, 0.31647, -0.84901],
    [200.0, 0.21142, 0.32312, -1.0182],
    [225.0, 0.21807, 0.32909, -1.2168],
    [250.0, 0.22511, 0.33439, -1.4512],
    [275.0, 0.23247, 0.33904, -1.7298],
    [300.0, 0.24010, 0.34308, -2.0637],
    [325.0, 0.24792, 0.34655, -2.4681],
    [350.0, 0.25591, 0.34951, -2.9641],
    [375.0, 0.26400, 0.35200, -3.5814],
    [400.0, 0.27218, 0.35407, -4.3633],
    [425.0, 0.28039, 0.35577, -5.3762],
    [450.0, 0.28863, 0.35714, -6.7262],
    [475.0, 0.29685, 0.35823, -8.5955],
    [500.0, 0.30505, 0.35907, -11.324],
    [525.0, 0.31320, 0.35968, -15.628],
    [550.0, 0.32129, 0.36011, -23.325],
    [575.0, 0.32931, 0.36038, -40.770],
    [600.0, 0.33724, 0.36051, -116.45],
];

/// Unit of the tint in uv.
const TINT_SCALE: f64 = 3000.0;

pub const KELVIN_MIN: f32 = 2000.0;
pub const KELVIN_MAX: f32 = 50000.0;

fn xy_to_uv(xy: [f64; 2]) -> [f64; 2] {
    let d = -2.0 * xy[0] + 12.0 * xy[1] + 3.0;
    [4.0 * xy[0] / d, 6.0 * xy[1] / d]
}

fn uv_to_xy(uv: [f64; 2]) -> [f64; 2] {
    let d = 2.0 * uv[0] - 8.0 * uv[1] + 4.0;
    [3.0 * uv[0] / d, 2.0 * uv[1] / d]
}

/// Unit vector along the isotherm (points towards magenta, i.e. towards smaller v).
fn direction(t: f64) -> [f64; 2] {
    let n = (1.0 + t * t).sqrt();
    [1.0 / n, t / n]
}

/// Point on the locus and isotherm direction at `mired`.
fn locus_at(mired: f64) -> ([f64; 2], [f64; 2]) {
    let m = mired.clamp(0.0, 600.0);
    let i = TAB.iter().rposition(|r| r[0] <= m).unwrap_or(0).min(TAB.len() - 2);
    let (a, b) = (TAB[i], TAB[i + 1]);
    let f = (m - a[0]) / (b[0] - a[0]);
    let uv = [a[1] + f * (b[1] - a[1]), a[2] + f * (b[2] - a[2])];
    let (da, db) = (direction(a[3]), direction(b[3]));
    let d = [da[0] + f * (db[0] - da[0]), da[1] + f * (db[1] - da[1])];
    let n = d[0].hypot(d[1]);
    (uv, [d[0] / n, d[1] / n])
}

/// White point (xy) → (Kelvin, tint).
pub fn xy_to_temp(xy: [f32; 2]) -> (f32, f32) {
    let uv = xy_to_uv([xy[0] as f64, xy[1] as f64]);
    // signed distance to each isotherm; looking for the zero crossing
    let dist = |r: &[f64; 4]| ((uv[1] - r[2]) - r[3] * (uv[0] - r[1])) / (1.0 + r[3] * r[3]).sqrt();
    let mut mired = 600.0;
    let mut prev = dist(&TAB[0]);
    for i in 1..TAB.len() {
        let d = dist(&TAB[i]);
        if (d < 0.0) != (prev < 0.0) || d == 0.0 {
            let f = prev / (prev - d);
            mired = TAB[i - 1][0] + f * (TAB[i][0] - TAB[i - 1][0]);
            break;
        }
        prev = d;
    }
    let (p, d) = locus_at(mired);
    let tint = ((uv[0] - p[0]) * d[0] + (uv[1] - p[1]) * d[1]) * TINT_SCALE;
    let k = if mired > 1e-6 { 1e6 / mired } else { KELVIN_MAX as f64 };
    ((k as f32).clamp(KELVIN_MIN, KELVIN_MAX), tint as f32)
}

/// (Kelvin, tint) → white point xy.
pub fn temp_to_xy(kelvin: f32, tint: f32) -> [f32; 2] {
    let mired = 1e6 / kelvin.clamp(KELVIN_MIN, KELVIN_MAX) as f64;
    let (p, d) = locus_at(mired);
    let o = tint as f64 / TINT_SCALE;
    let xy = uv_to_xy([p[0] + o * d[0], p[1] + o * d[1]]);
    [xy[0] as f32, xy[1] as f32]
}

/// White point of finished images (sRGB, D65).
pub const IMAGE_KELVIN: f32 = 6504.0;

/// Relative sliders for finished images (−100 … +100) → (Kelvin, tint): ±100 ≙ ±60 mired,
/// tint halved. + warmer or magenta.
pub fn relative_to(t: f32, g: f32) -> (f32, f32) {
    let mired = 1e6 / IMAGE_KELVIN - 0.6 * t.clamp(-100.0, 100.0);
    (1e6 / mired.max(20.0), 0.5 * g)
}

/// Inverse of [`relative_to`].
pub fn relative_from(k: f32, tint: f32) -> (f32, f32) {
    ((1e6 / IMAGE_KELVIN - 1e6 / k.max(1.0)) / 0.6, tint / 0.5)
}

/// Presets as in common photo apps (Kelvin, tint).
pub fn preset(name: &str) -> Option<(f32, f32)> {
    Some(match name {
        "daylight" => (5500.0, 10.0),
        "cloudy" => (6500.0, 10.0),
        "shade" => (7500.0, 10.0),
        "tungsten" => (2850.0, 0.0),
        "fluorescent" => (3800.0, 21.0),
        "flash" => (5500.0, 0.0),
        _ => return None,
    })
}

/// Camera neutral value for (Kelvin, tint) with the file's matrices.
pub fn neutral_for(info: &raw::Info, kelvin: f32, tint: f32) -> Option<[f32; 3]> {
    let ab = info.analog_balance.map(color::diag).unwrap_or(color::diag([1.0; 3]));
    color::xy_to_neutral(temp_to_xy(kelvin, tint), &info.calibration, &ab)
}

/// (Kelvin, tint) of a camera neutral value.
pub fn temp_from_neutral(info: &raw::Info, neutral: [f32; 3]) -> Option<(f32, f32)> {
    let ab = info.analog_balance.map(color::diag).unwrap_or(color::diag([1.0; 3]));
    color::neutral_to_xy(neutral, &info.calibration, &ab).map(xy_to_temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for k in [2500.0f32, 3200.0, 5000.0, 6500.0, 9000.0] {
            for t in [-40.0f32, 0.0, 25.0] {
                let (k2, t2) = xy_to_temp(temp_to_xy(k, t));
                assert!((k2 - k).abs() / k < 0.005, "{k} → {k2}");
                assert!((t2 - t).abs() < 0.5, "{t} → {t2}");
            }
        }
        // D65 ≈ 6504 K, slightly above the locus (greenish → tint negative? D65 lies ~+0.003 uv above it)
        let (k, _) = xy_to_temp([0.3127, 0.3290]);
        assert!((k - 6504.0).abs() < 40.0, "{k}");
    }
}
