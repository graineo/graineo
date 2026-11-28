//! Crop & straighten: tool selection in the right panel, overlay grid over the crop and the
//! angle given by a line drawn in the image (horizon, edge).

/// Tool in crop mode; the icon row on the right selects it, its settings appear below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GeoTool {
    #[default]
    Crop,
    Align,
    Perspective,
    /// Guide lines / quadrilateral (guided perspective, guides_ui.rs)
    Guides,
}

/// Guide lines inside the crop (display only, nothing of it is saved).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Raster {
    #[default]
    Thirds,
    Gold,
    Diagonal,
    NoGuide,
}

impl Raster {
    pub const ALL: [(Raster, &'static str); 4] = [(Raster::Thirds, "Drittel"), (Raster::Gold, "Gold"), (Raster::Diagonal, "Diagonal"), (Raster::NoGuide, "Keins")];

    /// Lines as segments in crop coordinates (0 … 1 per axis).
    pub fn lines(self) -> Vec<([f32; 2], [f32; 2])> {
        let vertical_horizontal = |ks: &[f32]| ks.iter().flat_map(|&k| [([k, 0.0], [k, 1.0]), ([0.0, k], [1.0, k])]).collect();
        match self {
            Raster::Thirds => vertical_horizontal(&[1.0 / 3.0, 2.0 / 3.0]),
            // Golden ratio: division 0.382 : 0.618
            Raster::Gold => vertical_horizontal(&[0.381_966, 0.618_034]),
            Raster::Diagonal => vec![([0.0, 0.0], [1.0, 1.0]), ([1.0, 0.0], [0.0, 1.0])],
            Raster::NoGuide => Vec::new(),
        }
    }
}

/// Tilt of the line `a` → `b` (points in the upright image) in degrees: the angle by which the image
/// must be rotated so that the line runs horizontally (or, if steeper than 45°, vertically).
/// Always in the range −45° … +45°.
pub fn line_angles(a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = (b[1] - a[1]).atan2(b[0] - a[0]).to_degrees();
    let d = (d + 90.0).rem_euclid(180.0) - 90.0;
    let angle = if d.abs() <= 45.0 { d } else { (d + 180.0).rem_euclid(180.0) - 90.0 };
    angle.clamp(-45.0, 45.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_line_with_slight_tilt() {
        let w = line_angles([0.0, 0.0], [100.0, 10.0]);
        assert!((w - 5.7106).abs() < 1e-3, "{w}");
        // Direction of the line (left→right or the other way) makes no difference
        assert!((line_angles([100.0, 10.0], [0.0, 0.0]) - w).abs() < 1e-3);
    }

    #[test]
    fn vertical_line_is_aligned_vertically() {
        let w = line_angles([0.0, 0.0], [10.0, 100.0]);
        assert!((w + 5.7106).abs() < 1e-3, "{w}");
        assert!(line_angles([0.0, 0.0], [0.0, 100.0]).abs() < 1e-4);
        assert!(line_angles([0.0, 0.0], [100.0, 0.0]).abs() < 1e-4);
    }

    #[test]
    fn angle_stays_in_range() {
        for k in 0..360 {
            let r = (k as f32).to_radians();
            let w = line_angles([0.0, 0.0], [r.cos() * 50.0, r.sin() * 50.0]);
            assert!((-45.0..=45.0).contains(&w), "{k}° → {w}");
        }
    }

    #[test]
    fn grids_have_the_expected_number_of_lines() {
        assert_eq!(Raster::Thirds.lines().len(), 4);
        assert_eq!(Raster::Gold.lines().len(), 4);
        assert_eq!(Raster::Diagonal.lines().len(), 2);
        assert!(Raster::NoGuide.lines().is_empty());
        for (r, _) in Raster::ALL {
            for (a, b) in r.lines() {
                assert!(a.iter().chain(b.iter()).all(|v| (0.0..=1.0).contains(v)));
            }
        }
    }
}
