//! White balance picker for RAWs and finished images: which light (Kelvin, tint) makes a sampled spot neutral.
//!
//! The sample is a mean of the app's sensor stage around the clicked point (see the app's `wb_picker`), so it is
//! linear and differs per path:
//! - graineo RAW: camera RGB, white-balanced as shot (`info.wb`) → divided by the as-shot gains it is the camera
//!   neutral of the light, converted with the file's matrices ([`wb::temp_from_neutral`]).
//! - finished image (JPEG, HEIC …): linear sRGB; the light is the white whose sRGB value the sample is.
//! - Apple path: linear Rec. 2020 already balanced by the engine for the white balance it was opened with
//!   ([`apple::AppleDev::opened_white`]); the sample's colour is adapted back from D65 to that white (Bradford), so a
//!   neutral sample gives the opened white balance exactly and a coloured one the light it implies.
//!
//! [`Light::exact`] tells whether the result can be reproduced: the white balance is stored as Kelvin and tint, which
//! cover 2000–50 000 K; a light outside that range (or one the matrices cannot reach) would be clamped and the spot
//! would not come out neutral.

use raw::color;

use crate::{wb, RawDoc};

/// Largest deviation per channel of the round trip light → camera neutral for an exact result.
pub const ROUND_TRIP_MAX: f32 = 0.01;

/// The light a neutral sample implies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub kelvin: f32,
    pub tint: f32,
    /// the white balance (Kelvin, tint) makes the spot neutral (within [`ROUND_TRIP_MAX`]); `false`: outside the
    /// range of the scale, the spot keeps a cast
    pub exact: bool,
}

/// Light of a sample `p` of the document's preview (linear, see the module text). `None` for an unusable sample (zero
/// or negative channel).
pub fn light_of(doc: &RawDoc, p: [f64; 3]) -> Option<Light> {
    if !p.iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    if let Some(a) = &doc.apple {
        return apple_light(a.opened_white(), p.map(|v| v as f32));
    }
    let gains = doc.info.wb.unwrap_or([1.0; 3]);
    let cam = [p[0] as f32 / gains[0], p[1] as f32 / gains[1], p[2] as f32 / gains[2]];
    let n = [cam[0] / cam[1], 1.0, cam[2] / cam[1]];
    let (kelvin, tint) = wb::temp_from_neutral(&doc.info, n)?;
    let exact = wb::neutral_for(&doc.info, kelvin, tint).is_some_and(|b| (0..3).all(|i| (b[i] / n[i] - 1.0).abs() <= ROUND_TRIP_MAX));
    Some(Light { kelvin, tint, exact })
}

/// Apple path: `p` is linear Rec. 2020 rendered for the white balance `opened`; the light that makes it neutral.
pub fn apple_light(opened: (f32, f32), p: [f32; 3]) -> Option<Light> {
    let xyz = color::mulv(&color::rec2020_to_xyz_d65(), p);
    if !(xyz[1] > 0.0) {
        return None;
    }
    // the engine adapted the opened white to D65: adapt back
    let w = color::xy_to_xyz(wb::temp_to_xy(opened.0, opened.1));
    let back = color::mulv(&color::bradford(color::xy_to_xyz(color::D65_XY), w), xyz);
    let xy = color::xyz_to_xy(back);
    if !xy.iter().all(|v| v.is_finite() && *v > 0.0) {
        return None;
    }
    let (kelvin, tint) = wb::xy_to_temp(xy);
    let again = wb::temp_to_xy(kelvin, tint);
    let exact = (again[0] - xy[0]).abs() < 2e-3 && (again[1] - xy[1]).abs() < 2e-3;
    Some(Light { kelvin, tint, exact })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A neutral sample on the Apple path keeps the white balance the preview was rendered with.
    #[test]
    fn apple_neutral_sample_is_the_opened_white() {
        for (k, t) in [(5500.0, 10.0), (3200.0, -5.0), (7500.0, 20.0)] {
            let l = apple_light((k, t), [0.4, 0.4, 0.4]).unwrap();
            assert!((l.kelvin - k).abs() < 15.0 && (l.tint - t).abs() < 0.3 && l.exact, "{k}/{t} → {l:?}");
        }
    }

    /// A warm (orange) sample on the Apple path means warmer light than the opened one.
    #[test]
    fn apple_warm_sample_means_warmer_light() {
        let l = apple_light((5500.0, 10.0), [0.5, 0.4, 0.3]).unwrap();
        assert!(l.kelvin < 4500.0, "{l:?}");
        let l = apple_light((5500.0, 10.0), [0.3, 0.4, 0.5]).unwrap();
        assert!(l.kelvin > 7000.0, "{l:?}");
    }

    #[test]
    fn unusable_samples() {
        assert_eq!(apple_light((5500.0, 10.0), [0.0, 0.0, 0.0]), None);
    }
}
