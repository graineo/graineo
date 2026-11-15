//! Lens model in the form of the open "Adobe Camera Model" (also used by own profiles):
//! perspective model with radial (k1–k3) and tangential (p1, p2) terms, lateral CA as two extra
//! models (red/green, blue/green) and radial vignetting `1 + a1 r² + a2 r⁴ + a3 r⁶`.
//! Entries per focal length, distance and aperture; [`Profile::select`] interpolates between them
//! (focal length linear, distance linear in 1/d, aperture linear in APEX).
//!
//! Moved unchanged from `raw::lcp` (10.11.2026); `raw::lcp` re-exports everything. Files written by graineo 1.0
//! (German keys) are translated when read (`legacy_names::settings_keys`).

use serde::{Deserialize, Serialize};

/// Imaging model (perspective or lateral CA).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub k: [f64; 3],
    pub p: [f64; 2],
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vignette {
    pub fx: f64,
    pub fy: f64,
    pub cx: f64,
    pub cy: f64,
    pub a: [f64; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub focal_length: f64,
    pub distance: f64,
    pub av: f64,
    pub geo: Option<Model>,
    pub ca: Option<(Model, Model)>,
    pub vig: Option<Vignette>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub crop: f64,
    pub entries: Vec<Entry>,
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

trait Blend: Copy {
    fn blend(a: Self, b: Self, t: f64) -> Self;
}

impl Blend for Model {
    fn blend(a: Self, b: Self, t: f64) -> Self {
        Model {
            fx: lerp(a.fx, b.fx, t),
            fy: lerp(a.fy, b.fy, t),
            cx: lerp(a.cx, b.cx, t),
            cy: lerp(a.cy, b.cy, t),
            k: std::array::from_fn(|i| lerp(a.k[i], b.k[i], t)),
            p: std::array::from_fn(|i| lerp(a.p[i], b.p[i], t)),
            scale: lerp(a.scale, b.scale, t),
        }
    }
}

impl Blend for Vignette {
    fn blend(a: Self, b: Self, t: f64) -> Self {
        Vignette { fx: lerp(a.fx, b.fx, t), fy: lerp(a.fy, b.fy, t), cx: lerp(a.cx, b.cx, t), cy: lerp(a.cy, b.cy, t), a: std::array::from_fn(|i| lerp(a.a[i], b.a[i], t)) }
    }
}

impl Blend for (Model, Model) {
    fn blend(a: Self, b: Self, t: f64) -> Self {
        (Model::blend(a.0, b.0, t), Model::blend(a.1, b.1, t))
    }
}

/// Interpolate over a key: nodes (key, value), clamped outside.
fn interp_over<T: Blend>(mut v: Vec<(f64, T)>, x: f64) -> Option<T> {
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v.dedup_by(|b, a| (a.0 - b.0).abs() < 1e-9);
    let first = v.first()?;
    if v.len() == 1 || x <= first.0 {
        return Some(first.1);
    }
    let last = v.last()?;
    if x >= last.0 {
        return Some(last.1);
    }
    let i = v.iter().position(|p| p.0 >= x)?;
    let (a, b) = (v[i - 1], v[i]);
    Some(T::blend(a.1, b.1, (x - a.0) / (b.0 - a.0)))
}

/// Chosen (interpolated) models for a shot.
#[derive(Debug, Clone)]
pub struct Selection {
    pub name: String,
    pub crop: f64,
    pub geo: Option<Model>,
    pub ca: Option<(Model, Model)>,
    pub vig: Option<Vignette>,
}

impl Profile {
    /// Does the profile cover this focal length (±10 %)? Otherwise it does not fit the shot
    /// (e.g. X100 profile for 19 mm with wide-angle converter, shot at 23 mm).
    pub fn covers(&self, focal_length: Option<f32>) -> bool {
        let Some(f) = focal_length.map(|v| v as f64).filter(|v| *v > 0.0) else { return true };
        let lo = self.entries.iter().map(|e| e.focal_length).fold(f64::MAX, f64::min);
        let hi = self.entries.iter().map(|e| e.focal_length).fold(0.0, f64::max);
        f >= lo * 0.9 && f <= hi * 1.1
    }

    /// Focal length (mm), aperture (f-number), distance (m; None = unknown → largest distance).
    pub fn select(&self, focal_length: Option<f32>, aperture: Option<f32>, distance: Option<f32>) -> Selection {
        let f = focal_length.map(|v| v as f64).unwrap_or_else(|| self.entries[0].focal_length);
        let av = aperture.map(|n| 2.0 * (n.max(0.5) as f64).log2()).unwrap_or(6.0);
        let inv_d = distance.map(|d| 1.0 / d.max(0.01) as f64).unwrap_or(0.0);
        // per property: per focal length over 1/distance (and for vignetting over the aperture), then over the focal length
        fn choose<T: Blend>(e: &[Entry], get: impl Fn(&Entry) -> Option<T>, f: f64, inv_d: f64, av: Option<f64>) -> Option<T> {
            let mut focal_lengths: Vec<f64> = e.iter().filter(|x| get(x).is_some()).map(|x| x.focal_length).collect();
            focal_lengths.sort_by(f64::total_cmp);
            focal_lengths.dedup_by(|b, a| (*a - *b).abs() < 1e-6);
            let per_f: Vec<(f64, T)> = focal_lengths
                .iter()
                .filter_map(|&bf| {
                    let group: Vec<&Entry> = e.iter().filter(|x| (x.focal_length - bf).abs() < 1e-6 && get(x).is_some()).collect();
                    let value = match av {
                        None => interp_over(group.iter().map(|x| (1.0 / x.distance, get(x).unwrap())).collect(), inv_d),
                        Some(av) => {
                            // nearest distance, then over the aperture
                            let best = group.iter().map(|x| ((1.0 / x.distance) - inv_d).abs()).fold(f64::MAX, f64::min);
                            interp_over(
                                group.iter().filter(|x| ((1.0 / x.distance) - inv_d).abs() <= best + 1e-9).map(|x| (x.av, get(x).unwrap())).collect(),
                                av,
                            )
                        }
                    };
                    value.map(|w| (bf, w))
                })
                .collect();
            interp_over(per_f, f)
        }
        Selection {
            name: self.name.clone(),
            crop: self.crop,
            geo: choose(&self.entries, |x| x.geo, f, inv_d, None),
            ca: choose(&self.entries, |x| x.ca, f, inv_d, None),
            vig: choose(&self.entries, |x| x.vig, f, inv_d, Some(av)),
        }
    }
}

impl Model {
    /// Normalized coordinates (ideal) → captured.
    #[inline]
    pub fn distort(&self, x: f64, y: f64) -> (f64, f64) {
        let r2 = x * x + y * y;
        let c = 1.0 + r2 * (self.k[0] + r2 * (self.k[1] + r2 * self.k[2])) + 2.0 * (self.p[0] * y + self.p[1] * x);
        (x * c + self.p[1] * r2, y * c + self.p[0] * r2)
    }
}

impl Vignette {
    /// Falloff (≤ 1) at normalized coordinates.
    #[inline]
    pub fn falloff(&self, x: f64, y: f64) -> f64 {
        let r2 = x * x + y * y;
        1.0 + r2 * (self.a[0] + r2 * (self.a[1] + r2 * self.a[2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distort_center_fixed() {
        let m = Model { fx: 1.0, fy: 1.0, cx: 0.5, cy: 0.5, k: [-0.1, 0.0, 0.0], p: [0.0; 2], scale: 1.0 };
        assert_eq!(m.distort(0.0, 0.0), (0.0, 0.0));
        let (x, _) = m.distort(0.5, 0.0);
        assert!((x - 0.5 * (1.0 - 0.025)).abs() < 1e-12);
    }
}
