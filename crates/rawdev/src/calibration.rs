//! Calibration: hue and saturation of the three primaries (red, green, blue) of the camera profile and the shadow tint.
//! Own implementation: each primary of the working space is rotated in the chroma plane (perpendicular to the gray axis) towards the next
//! primary (red → yellow, green → cyan, blue → violet) and scaled in chroma; white stays white afterwards. The matrix acts on
//! all colors mixed, like a modified profile. The strengths were measured against own reference exports (see tests/comments).
use raw::color::{self, M3};

/// Rotation at slider ±1 (= ±100 in imported Lightroom settings), in degrees.
const ROTATION_DEG: f32 = 20.0;
/// Chroma at slider ±1: factor 1 ± this value. Measured on 90 reference exports with strong calibration (ΔE00 median without 5.55, with 20°/0.5 5.48, 20°/0.9 5.47; 35° worse).
const SATURATION: f32 = 0.7;
/// Shadow tint at ±1: green in the shadows lowered (+, magenta) or raised (−, green) by this many EV.
pub const SHADOW_EV: f32 = 0.25;

fn strengths() -> (f32, f32) {
    // test hook for tuning: GRAINEO_CALIB="rotation,saturation"
    if let Ok(v) = std::env::var("GRAINEO_CALIB") {
        let n: Vec<f32> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
        if n.len() >= 2 {
            return (n[0], n[1]);
        }
    }
    (ROTATION_DEG, SATURATION)
}

/// Shadow tint in EV at ±1 (test hook: third value of GRAINEO_CALIB).
pub fn shadow_ev() -> f32 {
    std::env::var("GRAINEO_CALIB").ok().and_then(|v| v.split(',').nth(2).and_then(|t| t.trim().parse().ok())).unwrap_or(SHADOW_EV)
}

/// Matrix in the working space (linear) for `k` = [red hue, red saturation, green hue, green saturation, blue hue, blue saturation, …],
/// each −1 … +1. None if all values are 0.
pub fn matrix(k: &[f32; 7]) -> Option<M3> {
    if k[..6].iter().all(|v| *v == 0.0) {
        return None;
    }
    let (grad, sat) = strengths();
    let s2 = std::f32::consts::FRAC_1_SQRT_2;
    let s6 = 1.0 / 6f32.sqrt();
    let u1 = [s2, -s2, 0.0];
    let u2 = [s6, s6, -2.0 * s6];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let e = |i: usize| {
        let mut v = [0.0; 3];
        v[i] = 1.0;
        v
    };
    let angle_deg = |i: usize| dot(e(i), u2).atan2(dot(e(i), u1));
    let mut c = [[0.0f32; 3]; 3];
    for i in 0..3 {
        let (x, y) = (dot(e(i), u1), dot(e(i), u2));
        // direction to the next primary (red → green → blue → red)
        let mut d = angle_deg((i + 1) % 3) - angle_deg(i);
        while d > std::f32::consts::PI {
            d -= 2.0 * std::f32::consts::PI;
        }
        while d < -std::f32::consts::PI {
            d += 2.0 * std::f32::consts::PI;
        }
        let t = d.signum() * (k[2 * i] * grad).to_radians();
        let f = (1.0 + k[2 * i + 1] * sat).max(0.0);
        let (sn, cs) = t.sin_cos();
        let (x2, y2) = ((cs * x - sn * y) * f, (sn * x + cs * y) * f);
        for r in 0..3 {
            c[r][i] = 1.0 / 3.0 + x2 * u1[r] + y2 * u2[r];
        }
    }
    // white stays white
    let d = color::mulv(&color::inv(&c)?, [1.0; 3]);
    Some(color::mul(&c, &color::diag(d)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_stays_and_red_turns_yellow() {
        let m = matrix(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        let w = color::mulv(&m, [1.0; 3]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-4), "{w:?}");
        let r = color::mulv(&m, [1.0, 0.0, 0.0]);
        assert!(r[1] > r[2], "red + hue → more green than blue (yellowish): {r:?}");
        assert!(matrix(&[0.0; 7]).is_none());
    }

    #[test]
    fn saturation_makes_red_more_vivid() {
        let m = matrix(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        let r = color::mulv(&m, [1.0, 0.2, 0.2]);
        assert!(r[0] - r[1] > 0.85, "{r:?}");
    }
}
