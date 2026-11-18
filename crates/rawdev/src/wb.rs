//! White balance as color temperature and tint, converted via the camera matrices
//! according to the DNG specification ([`raw::color::neutral_to_xy`], [`raw::color::xy_to_neutral`]).
//!
//! Temperature: correlated color temperature after Robertson (1968) with the published table of
//! isotherms (Wyszecki & Stiles, "Color Science", Table 1(3.11)). Tint: distance from the
//! Planckian locus along the isotherm in CIE 1960 uv, 1 unit = 1/3000, with the sign of the DNG
//! reference implementation and common photo apps: a white point **above** the locus (greenish light)
//! has a positive tint, so raising the tint slider assumes greener light and turns the image magenta
//! (+ = magenta, − = green). Reference values: D55 = 5502 K / +10.0, D65 = 6504 K / +9.8,
//! D50 = 5001 K / +9.6, so the "Daylight" preset (5500 K / +10) is D55. Own code.

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

/// Tint units per uv distance along the isotherm. Negative: the isotherm direction ([`direction`]) points to
/// smaller v (below the locus, magenta light), while a positive tint means a white point above the locus.
const TINT_SCALE: f64 = -3000.0;

pub const KELVIN_MIN: f32 = 2000.0;
pub const KELVIN_MAX: f32 = 50000.0;
/// Largest tint magnitude of the white balance (the range of the tint slider).
const TINT_LIMIT: f32 = 150.0;

fn xy_to_uv(xy: [f64; 2]) -> [f64; 2] {
    let d = -2.0 * xy[0] + 12.0 * xy[1] + 3.0;
    [4.0 * xy[0] / d, 6.0 * xy[1] / d]
}

fn uv_to_xy(uv: [f64; 2]) -> [f64; 2] {
    let d = 2.0 * uv[0] - 8.0 * uv[1] + 4.0;
    [3.0 * uv[0] / d, 2.0 * uv[1] / d]
}

/// Unit vector along the isotherm (points below the locus, towards magenta light, i.e. towards smaller v).
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
///
/// Robertson's method as in the DNG reference implementation: walk the isotherms from the 10 mired row on and stop
/// at the first one the point does not lie on the warm side of (signed distance ≤ 0), then interpolate between it and
/// the row before. Outside the table the result sticks to its end: bluer than the 10 mired isotherm gives 100 000 K
/// (clamped to [`KELVIN_MAX`]), redder than 600 mired gives 1667 K (clamped to [`KELVIN_MIN`]).
pub fn xy_to_temp(xy: [f32; 2]) -> (f32, f32) {
    let uv = xy_to_uv([xy[0] as f64, xy[1] as f64]);
    // signed distance to an isotherm: > 0 on its warm side (towards higher mired)
    let dist = |r: &[f64; 4]| {
        let d = direction(r[3]);
        (uv[1] - r[2]) * d[0] - (uv[0] - r[1]) * d[1]
    };
    let last = TAB.len() - 1;
    let mut prev = 0.0;
    let mut mired = TAB[last][0];
    for i in 1..=last {
        let d = dist(&TAB[i]);
        if d <= 0.0 || i == last {
            // past the end of the table on the warm side: stay on the last row
            let d = -d.min(0.0);
            let f = if i == 1 { 0.0 } else { d / (prev + d) };
            mired = TAB[i][0] - f * (TAB[i][0] - TAB[i - 1][0]);
            break;
        }
        prev = d;
    }
    let (p, d) = locus_at(mired);
    let tint = ((uv[0] - p[0]) * d[0] + (uv[1] - p[1]) * d[1]) * TINT_SCALE;
    (((1e6 / mired) as f32).clamp(KELVIN_MIN, KELVIN_MAX), tint as f32)
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

/// (Kelvin, tint) of the white point of finished images (D65): "no change" for a JPEG. D65 lies above the Planckian
/// locus, so its tint is not 0 but ≈ +9.8 ([`xy_to_temp`] of [`color::D65_XY`]); the relative sliders count from here.
pub fn image_white() -> (f32, f32) {
    xy_to_temp(color::D65_XY)
}

/// Relative sliders for finished images (−100 … +100) → (Kelvin, tint): ±100 ≙ ±60 mired, tint halved, both counted
/// from the image's own white ([`image_white`]: 0/0 = no change). + warmer or magenta.
pub fn relative_to(t: f32, g: f32) -> (f32, f32) {
    let (k0, g0) = image_white();
    let mired = 1e6 / k0 - 0.6 * t.clamp(-100.0, 100.0);
    (1e6 / mired.max(20.0), g0 + 0.5 * g)
}

/// Inverse of [`relative_to`].
pub fn relative_from(k: f32, tint: f32) -> (f32, f32) {
    let (k0, g0) = image_white();
    ((1e6 / k0 - 1e6 / k.max(1.0)) / 0.6, (tint - g0) / 0.5)
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

/// The file's own as-shot white balance in (Kelvin, tint), with whether it is faithful: converting it back gives the
/// as-shot neutral again (within 0.5 %) and it does not sit at an end of the scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OwnAsShot {
    pub kelvin: f32,
    pub tint: f32,
    pub exact: bool,
}

/// Largest round-trip deviation (as-shot neutral → Kelvin/tint → neutral, per channel) for a faithful as-shot value.
pub const ROUND_TRIP_MAX: f32 = 0.005;

/// [`OwnAsShot`] of a file with an as-shot neutral and colour matrices; `None` without them.
pub fn own_as_shot(info: &raw::Info) -> Option<OwnAsShot> {
    let wb = info.wb?;
    let n = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
    if !n.iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    let (kelvin, tint) = temp_from_neutral(info, n)?;
    let back = neutral_for(info, kelvin, tint);
    let error = back.map(|b| (0..3).map(|i| (b[i] / n[i] - 1.0).abs()).fold(0.0f32, f32::max)).unwrap_or(f32::INFINITY);
    Some(OwnAsShot { kelvin, tint, exact: error <= ROUND_TRIP_MAX && kelvin > KELVIN_MIN && kelvin < KELVIN_MAX })
}

/// White balance taken over from Lightroom numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FromLightroom {
    /// "As shot" (the file's own white balance)
    AsShot,
    /// our Kelvin and tint
    Custom(f32, f32),
}

/// Lightroom's temperature/tint `lr`, read as a distance from Lightroom's own as-shot value `lr_as_shot` and laid onto
/// the file's own as-shot value `own`: temperature in mired (1e6/K, equal steps are equal colour steps), tint as a
/// difference. Lightroom's numbers are computed with its own camera profile; where its matrices differ from the
/// file's (measured: Leica Q/Q2/Q3, Sigma fp, ΔE 14–17 with the absolute numbers), the absolute numbers mean another
/// colour, the distance carries over (ΔE 1–2); where they agree the result is the same as before.
///
/// "As shot" instead when Lightroom's numbers are its as-shot value, when our own as-shot value is not faithful (see
/// [`OwnAsShot::exact`]), or when the result would leave 2000–50 000 K or a tint of ±150.
pub fn from_lightroom_relative(lr: (f32, f32), lr_as_shot: (f32, f32), own: OwnAsShot) -> FromLightroom {
    if (lr.0 - lr_as_shot.0).abs() < 0.5 && (lr.1 - lr_as_shot.1).abs() < 0.05 {
        return FromLightroom::AsShot;
    }
    if !own.exact || lr.0 <= 0.0 || lr_as_shot.0 <= 0.0 {
        return FromLightroom::AsShot;
    }
    let mired = 1e6 / own.kelvin as f64 + (1e6 / lr.0 as f64 - 1e6 / lr_as_shot.0 as f64);
    let (lo, hi) = (1e6 / KELVIN_MAX as f64, 1e6 / KELVIN_MIN as f64);
    if !(lo..=hi).contains(&mired) {
        return FromLightroom::AsShot;
    }
    let kelvin = ((1e6 / mired).round() as f32).clamp(KELVIN_MIN, KELVIN_MAX);
    let tint = ((own.tint + (lr.1 - lr_as_shot.1)) * 10.0).round() / 10.0;
    if !(-TINT_LIMIT..=TINT_LIMIT).contains(&tint) {
        return FromLightroom::AsShot;
    }
    FromLightroom::Custom(kelvin, tint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own(kelvin: f32, tint: f32) -> OwnAsShot {
        OwnAsShot { kelvin, tint, exact: true }
    }

    /// Leica Q (Typ 116), measured: Lightroom's as-shot 8000 K/+1, ours 17173 K/+8.4 (other matrices).
    #[test]
    fn lightroom_distance_on_our_as_shot() {
        let q = own(17173.0, 8.4);
        // 9000 K is 13.9 mired warmer than Lightroom's 8000 K: 58.2 + 111.1 − 125 = 44.3 mired
        assert_eq!(from_lightroom_relative((9000.0, 1.0), (8000.0, 1.0), q), FromLightroom::Custom(22552.0, 8.4));
        // tint as a difference
        assert_eq!(from_lightroom_relative((8000.0, 21.0), (8000.0, 1.0), q), FromLightroom::Custom(17173.0, 28.4));
        // daylight preset numbers (5500/+10): 58.2 + 181.8 − 125 = 115.0 mired
        assert_eq!(from_lightroom_relative((5500.0, 10.0), (8000.0, 1.0), q), FromLightroom::Custom(8692.0, 17.4));
    }

    /// Matching matrices (our as-shot = Lightroom's): the result is Lightroom's number (up to rounding).
    #[test]
    fn same_matrices_keep_the_numbers() {
        for (k, t) in [(5000.0, 3.0), (3200.0, -10.0), (7500.0, 10.0), (12000.0, 25.0)] {
            match from_lightroom_relative((k, t), (5600.0, 14.0), own(5600.0, 14.0)) {
                FromLightroom::Custom(k2, t2) => assert!((k2 - k).abs() <= 1.0 && (t2 - t).abs() < 0.06, "{k}/{t} → {k2}/{t2}"),
                x => panic!("{x:?}"),
            }
        }
    }

    #[test]
    fn falls_back_to_as_shot() {
        let q = own(17173.0, 8.4);
        // (a) Lightroom's numbers are its as-shot value
        assert_eq!(from_lightroom_relative((8000.0, 1.0), (8000.0, 1.0), q), FromLightroom::AsShot);
        // (b) our own as-shot not faithful: Leica Q3 file with an as-shot neutral outside the matrices' range
        //     (measured on a sample file: round trip 7.9 %, clamped at 50 000 K)
        assert_eq!(from_lightroom_relative((5600.0, 3.0), (5450.0, 2.0), OwnAsShot { kelvin: KELVIN_MAX, tint: 30.0, exact: false }), FromLightroom::AsShot);
        // (c) result beyond 50 000 K (below 20 mired) or below 2000 K (above 500 mired)
        assert_eq!(from_lightroom_relative((20000.0, 1.0), (8000.0, 1.0), q), FromLightroom::AsShot);
        assert_eq!(from_lightroom_relative((2000.0, 0.0), (6000.0, 0.0), own(3000.0, 0.0)), FromLightroom::AsShot);
        // (d) tint beyond ±150: Q3 as-shot +41, Lightroom tint +140 against its as-shot +1 gives +180
        assert_eq!(from_lightroom_relative((8000.0, 141.0), (8000.0, 1.0), own(17000.0, 41.0)), FromLightroom::AsShot);
        // −199 against +1 is −200; +41 − 200 = −159
        assert_eq!(from_lightroom_relative((8000.0, -199.0), (8000.0, 1.0), own(17000.0, 41.0)), FromLightroom::AsShot);
        // inside the limit: −149 against +1 is −150; +41 − 150 = −109 stays custom
        assert_eq!(from_lightroom_relative((8000.0, -149.0), (8000.0, 1.0), own(17000.0, 41.0)), FromLightroom::Custom(17000.0, -109.0));
    }

    fn near(a: (f32, f32), k: f32, t: f32, dk: f32, dt: f32) {
        assert!((a.0 - k).abs() <= dk && (a.1 - t).abs() <= dt, "{a:?}, expected {k} K / {t:+}");
    }

    #[test]
    fn round_trip() {
        for k in [2000.0f32, 2500.0, 3200.0, 5000.0, 6500.0, 9000.0, 15000.0, 30000.0] {
            for t in [-60.0f32, -10.0, 0.0, 25.0, 100.0] {
                let (k2, t2) = xy_to_temp(temp_to_xy(k, t));
                assert!((k2 - k).abs() / k < 0.002, "{k}/{t} → {k2}");
                assert!((t2 - t).abs() < 0.05, "{k}/{t} → {t2}");
            }
        }
    }

    /// Standard illuminants: the values of the DNG reference implementation (the "Daylight" preset 5500 K / +10 is D55).
    #[test]
    fn standard_illuminants() {
        near(xy_to_temp([0.33242, 0.34743]), 5502.0, 10.0, 3.0, 0.15); // D55
        near(xy_to_temp([0.31271, 0.32902]), 6504.0, 9.8, 3.0, 0.15); // D65
        near(xy_to_temp([0.34567, 0.35850]), 5001.0, 9.6, 3.0, 0.15); // D50
        // a point on the locus has no tint
        let xy = temp_to_xy(4000.0, 0.0);
        near(xy_to_temp(xy), 4000.0, 0.0, 2.0, 0.01);
    }

    /// White points beyond the end of the table: very blue → highest temperature (not the lowest, as before 18.11.2026),
    /// very red → lowest.
    #[test]
    fn outside_the_table() {
        let (k, t) = xy_to_temp([0.22, 0.22]);
        assert_eq!(k, KELVIN_MAX);
        assert!((t - 24.0).abs() < 1.5, "{t}");
        let (k, _) = xy_to_temp([0.25, 0.25]);
        assert!(k > 25000.0, "{k}");
        let (k, _) = xy_to_temp([0.60, 0.39]);
        assert_eq!(k, KELVIN_MIN);
    }

    /// Positive tint = greener light assumed = the image turns magenta (white balance gains of a simple sRGB "camera").
    #[test]
    fn positive_tint_turns_the_image_magenta() {
        const XYZ_TO_RGB: [[f64; 3]; 3] = [[3.2406, -1.5372, -0.4986], [-0.9689, 1.8758, 0.0415], [0.0557, -0.2040, 1.0570]];
        let gains = |k: f32, t: f32| {
            let [x, y] = temp_to_xy(k, t).map(|v| v as f64);
            let xyz = [x / y, 1.0, (1.0 - x - y) / y];
            let n: Vec<f64> = XYZ_TO_RGB.iter().map(|r| r[0] * xyz[0] + r[1] * xyz[1] + r[2] * xyz[2]).collect();
            [n[1] / n[0], 1.0, n[1] / n[2]]
        };
        // green of a grey patch relative to red and blue after the white balance
        let green = |g: [f64; 3]| g[1] / (0.5 * (g[0] + g[2]));
        for k in [3000.0, 5500.0, 8000.0] {
            assert!(green(gains(k, 30.0)) < green(gains(k, 0.0)) && green(gains(k, 0.0)) < green(gains(k, -30.0)), "{k} K");
            // the assumed light itself lies above the locus (greener) for a positive tint
            let v = |t: f32| xy_to_uv(temp_to_xy(k, t).map(|v| v as f64))[1];
            assert!(v(30.0) > v(0.0), "{k} K");
        }
    }

    /// Finished images: relative 0/0 is the image's own white (D65) and renders without a change; the relative
    /// sliders and the picker meet there (before 18.11.2026 tint 0 was taken for "no change", a cast of ≈ 10 units).
    #[test]
    fn relative_zero_is_the_image_white() {
        let (k, g) = relative_to(0.0, 0.0);
        let xy = temp_to_xy(k, g);
        assert!((xy[0] - color::D65_XY[0]).abs() < 2e-4 && (xy[1] - color::D65_XY[1]).abs() < 2e-4, "{k}/{g} → {xy:?}");
        assert!((g - 9.8).abs() < 0.2, "{g}");
        let (rt, rg) = relative_from(k, g);
        assert!(rt.abs() < 0.01 && rg.abs() < 0.01, "{rt}/{rg}");
        for (t, g) in [(20.0, -10.0), (-60.0, 30.0)] {
            let (k, tint) = relative_to(t, g);
            let (t2, g2) = relative_from(k, tint);
            assert!((t2 - t).abs() < 0.01 && (g2 - g).abs() < 0.01, "{t}/{g} → {t2}/{g2}");
        }
    }

    #[test]
    fn presets_are_the_standard_illuminants() {
        let (k, t) = preset("daylight").unwrap();
        let xy = temp_to_xy(k, t);
        assert!((xy[0] - 0.33242).abs() < 3e-4 && (xy[1] - 0.34743).abs() < 3e-4, "{xy:?}");
    }
}
