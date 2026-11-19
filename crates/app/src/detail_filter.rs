//! How the full-resolution detail image is filtered on screen, depending on the scale.
//!
//! The detail textures have no mipmaps. Without a prefilter a reduced view (below 100 %) aliases: plain bilinear
//! minification skips texels (moiré on fine diagonals), and the half level magnified with nearest sampling repeats
//! every n-th row and column (stairs on diagonals). Magnification with nearest sampling at a non-integer factor
//! (e.g. 150 %) repeats every other or third row and column unevenly. So:
//!
//! - below 100 %: the full level and the half level (2×2 mean, bilinear) are blended like trilinear mipmapping,
//!   weight of the half level `log2(1 / scale)` (0 at 100 %, 1 at 50 % and below);
//! - exactly 100 % (one texel per physical pixel): nearest – every pixel exactly as rendered, whatever the
//!   sub-pixel position of the image on screen (bilinear would blur it by the sub-pixel offset);
//! - between 100 % and 200 %, and whenever the view is rotated (fine angle) or has perspective: bilinear;
//! - from 200 % on: nearest – pixels stay visible as pixels (pixel peeping).

/// Tolerance for "exactly 100 %" (scale computed from float sizes).
const ONE_TO_ONE: f32 = 1e-3;
/// From this magnification on, texels are drawn as pixels (nearest).
const PIXELS_FROM: f32 = 2.0 - 1e-3;

/// What to draw for one detail level at `scale` screen pixels per texel of the full level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// draw the full level
    pub full: bool,
    /// sample the full level bilinearly (else nearest) when it is magnified
    pub smooth: bool,
    /// opacity of the half level drawn over the full one (0: not drawn)
    pub half: f32,
}

/// `has_half`: the level has a half level; `transformed`: the view is rotated or has perspective.
pub fn plan(scale: f32, has_half: bool, transformed: bool) -> Plan {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let half = if has_half && scale < 1.0 - ONE_TO_ONE { (1.0 / scale).log2().clamp(0.0, 1.0) } else { 0.0 };
    let exact = (scale - 1.0).abs() <= ONE_TO_ONE && !transformed;
    let smooth = !exact && scale < PIXELS_FROM;
    Plan { full: half < 1.0, smooth, half }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_to_one_is_exact_pixels() {
        let p = plan(1.0, true, false);
        assert_eq!(p, Plan { full: true, smooth: false, half: 0.0 });
        // float noise of the scale
        assert!(!plan(1.0004, true, false).smooth);
        assert!(!plan(0.9996, true, false).smooth);
        assert_eq!(plan(0.9996, true, false).half, 0.0);
    }

    #[test]
    fn rotated_one_to_one_is_bilinear() {
        assert!(plan(1.0, true, true).smooth);
    }

    #[test]
    fn reduced_views_blend_the_half_level() {
        let p = plan(0.8, true, false);
        assert!(p.full);
        assert!((p.half - (1.25f32).log2()).abs() < 1e-6);
        let p = plan(0.5, true, false);
        assert_eq!((p.full, p.half), (false, 1.0));
        let p = plan(0.3, true, false);
        assert_eq!((p.full, p.half), (false, 1.0));
        // without a half level only the full one (bilinear minification)
        assert_eq!(plan(0.6, false, false), Plan { full: true, smooth: true, half: 0.0 });
    }

    #[test]
    fn magnification() {
        assert!(plan(1.5, true, false).smooth);
        assert!(plan(1.25, false, false).smooth);
        assert!(!plan(2.0, true, false).smooth);
        assert!(!plan(4.0, true, true).smooth);
        assert_eq!(plan(1.5, true, false).half, 0.0);
    }

    #[test]
    fn nonsense_scale_is_treated_as_one_to_one() {
        assert_eq!(plan(f32::NAN, true, false), plan(1.0, true, false));
        assert_eq!(plan(0.0, true, false), plan(1.0, true, false));
    }
}
