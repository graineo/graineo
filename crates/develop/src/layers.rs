//! Layer mixer in colour: distribute the three Foveon layers freely to output R/G/B.
//!
//! Idea as in "Raw Channels" in x3fuse (only the idea, own code): instead of Sigma's colour matrix
//! (and ColorDQ for Merrill) our own 3×3 matrix maps the layers directly to linear
//! RGB, for infrared/full-spectrum shots and creative looks.
//!
//! Convention ([`crate::Settings::layer_matrix`]): `m[output][layer]`, output 0/1/2 =
//! R/G/B, layer 0 = bottom (≈ red/NIR), 1 = middle (≈ green), 2 = top (≈ blue);
//! the same order as the planes of [`crate::Linear`] and the B/W mix `mix`.
//!
//! Normalisation: the layers are first amplified with the white balance (preset, eyedropper,
//! as on the normal path) and then divided so that a spot that Sigma's matrix would output as
//! neutral grey has the same value in all three layers (`w = M⁻¹·[1,1,1]`). A
//! matrix with row sum 1 thus keeps "As shot" neutral and the brightness constant; the
//! eyedropper works unchanged (it makes exactly `M·v` grey). Temperature/tint then act in the
//! output space as usual.
//!
//! Where Sigma's colour is skipped:
//! - own pipeline ([`crate::render`]): the camera→sRGB matrix is replaced by the matrix
//!   computed here, ColorDQ is dropped;
//! - SPP base ([`crate::render_counts`]): the base already contains Sigma's matrix and ColorDQ;
//!   the counts are converted back to camera space with the inverse of the colour mode matrix,
//!   as for monochrome (ColorDQ is non-linear and remains as a small residual).
//!
//! Everything linear in a single 3×3 matrix per image; costs nothing extra per pixel.

use crate::matrix::Mat3;
use crate::{Calibration, Settings};

/// Layer matrix `m[output][layer]`.
pub type LayerMatrix = [[f32; 3]; 3];

/// Identity: bottom → R, middle → G, top → B (raw, weakly saturated).
pub const IDENTITY: LayerMatrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// False-colour infrared (NRG, in the style of Kodak Aerochrome/CIR): output R ← NIR, G ← red, B ← green.
/// - R = bottom layer raw: it collects the deeply penetrating long wavelengths, so without an IR cut filter
///   (full-spectrum conversion) mainly NIR; vegetation turns strongly red.
/// - G = "red", B = "green" unmixed from the layers: the layers overlap strongly in their spectra,
///   Sigma's matrices separate them with off-diagonals of −3.4 … +4.4 (DP2M, row sum 1). Here
///   about half of that strength (red ≈ 1.8·bottom − 1.2·middle + 0.4·top, green ≈ −0.8·bottom
///   + 2·middle − 0.2·top): clearly coloured, but without Sigma's noise amplification.
/// Row sum 1: neutral grey stays neutral. On a normal camera this yields the familiar
/// CIR emulation (red → green, blue → violet).
pub const INFRARED_NRG: LayerMatrix = [[1.0, 0.0, 0.0], [1.8, -1.2, 0.4], [-0.8, 2.0, -0.2]];

/// Channel swap R ↔ B (classic IR swap for full spectrum: blue sky, bright foliage):
/// top → R, middle → G, bottom → B.
pub const SWAP_RB: LayerMatrix = [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];

/// Presets for the UI: name, matrix (`None` = Sigma, mixer off).
pub const PRESETS: [(&str, Option<LayerMatrix>); 4] = [
    ("Kamera (aus)", None),
    ("Identität", Some(IDENTITY)),
    ("Infrarot NRG", Some(INFRARED_NRG)),
    ("Kanaltausch R↔B", Some(SWAP_RB)),
];

/// Layer mixer active? (Colour; monochrome takes precedence.)
pub fn active(s: &Settings) -> Option<&LayerMatrix> {
    s.layer_matrix.as_ref().filter(|_| !s.monochrome)
}

/// Matrix from white-balanced camera values (count scale, as they enter Sigma's matrix in
/// [`crate::render`]) to linear sRGB, including normalisation and temperature/tint.
pub fn cam_to_rgb(cal: &Calibration, s: &Settings, l: &LayerMatrix) -> Mat3 {
    let m = cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref());
    // Camera values that Sigma outputs as neutral grey; unusable (singular, ≤ 0) → no normalisation
    let w = m.inverse().map(|inv| inv.apply([1.0; 3])).filter(|w| w.iter().all(|v| v.is_finite() && *v > 1e-9)).unwrap_or([1.0; 3]);
    let lm = Mat3(std::array::from_fn(|i| l[i / 3][i % 3] as f64));
    Mat3::diag(crate::calib::temp_tint_gains(s.wb_temp, s.wb_tint)).mul(&lm).mul(&Mat3::diag(w.map(|v| 1.0 / v)))
}
