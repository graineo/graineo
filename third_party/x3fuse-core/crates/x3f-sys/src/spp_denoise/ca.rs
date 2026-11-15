//! graineo addition: correction of chromatic aberration as in SIGMA Photo Pro
//! (Merrill): preparation (reach, safety brake) and radial shift of red and
//! blue per tile, measured against SPP's output.
//!
//! The shift is applied to the smooth base image that has already passed the colour matrix
//! (after `apply_matrix_color_dq`, before the sharpening image and the residual); without
//! a correction a copy is made at that point. The coefficients (lens profile or manual) are chosen by the
//! caller (graineo `develop::lens`); this module only computes.
//!
//! Alternative form (test switch `GRAINEO_MERRILL_NEW=ca`, [`CaTransform::apply_own`]): the radial
//! lens model of Brown (1966) / PTLens, `r_src = r + s·(P(r) − r)` per colour with the profile
//! polynomial `P` and the profile amount `s`; manually a pure magnification difference
//! `r_src = r·(1 + MANUAL_GAIN·slider)` per colour (lateral colour = different magnification per
//! wavelength). Bilinear sampling; shifts beyond [`OWN_LIMIT`] of the radius are limited.

/// Selected correction: mode, sliders and the coefficients chosen by the caller
/// (`[R0, G0, B0, R1, G1, B1, R2, G2, B2, R3, G3, B3]`, polynomial
/// `P(r) = c0·r + c1·r² + c2·r³ + c3·r⁴`; green is not shifted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaCorrection {
    /// `true` = lens profile, `false` = manual
    pub profile: bool,
    /// Profile amount (SPP slider, 0 … 2, default 1)
    pub profile_scale: f32,
    /// Manual: red and blue (SPP sliders, −2 … 2)
    pub red: f32,
    pub blue: f32,
    pub coeffs: [f64; 12],
}

/// Fully prepared transformation for all tiles of an image.
#[derive(Clone, Debug)]
pub(crate) struct CaTransform {
    profile: bool,
    /// |red| or profile amount and |blue| (after the safety brake of the preparation)
    scale_r: f64,
    scale_b: f64,
    c: [f64; 12],
    /// Width and height of `ActiveImageArea` (half-open)
    width: i32,
    height: i32,
    /// graineo: alternative form (test switch), with the manual slider values (red, blue)
    own: Option<[f64; 2]>,
}

/// Alternative manual form: magnification difference per slider unit (measured: same corner shift
/// as the default manual correction at ±1 on DP2 Merrill, ≈ 2.6 px).
const MANUAL_GAIN: f64 = 0.0008;
/// Alternative form: largest relative shift of the radius (0.5 %, ≈ 14 px in the corner).
const OWN_LIMIT: f64 = 0.005;

const SQRT2: f64 = f64::from_bits(0x3ff6_a09e_667f_3bcd);
const SQRT2_3: f64 = f64::from_bits(0x4006_a09e_667f_3bce);
const FOUR: f64 = f64::from_bits(0x4010_0000_0000_0001);
const SIG_MID: f64 = f64::from_bits(0xbfe6_a09e_667f_3bcd);
const SIG_SLOPE: f64 = -10.0;
const MANUAL_STEP: f64 = f64::from_bits(0x3f46_f006_8db8_b801);
/// Index 0 = profile, 1 = manual
const REACH_STEP: [f64; 2] = [
    f64::from_bits(0x3f43_1d5a_beb1_0782),
    f64::from_bits(0x3f33_1d5a_beb1_0782),
];
const REACH_DIV: [f64; 2] = [
    f64::from_bits(0x4010_0393_bf60_ed7b),
    f64::from_bits(0x3ff0_0e4e_fd83_b5eb),
];

/// P(√2) in the same evaluation order as SPP (`ch` 0 = red, 2 = blue).
fn corner(c: &[f64; 12], ch: usize) -> f64 {
    c[9 + ch] * FOUR + c[6 + ch] * SQRT2_3 + c[3 + ch] * SQRT2 * SQRT2 + c[ch] * SQRT2
}

fn flag(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

impl CaTransform {
    /// Preparation: checks the coefficients and limits the sliders if the
    /// shift in the corner would exceed 15 pixels (then ±1). `None` if the correction
    /// is switched off because of missing coefficients (as in SPP).
    pub(crate) fn prepare(ca: &CaCorrection, active: [i32; 4]) -> Option<Self> {
        let c = ca.coeffs;
        if c[0] == 0.0 && c[1] == 0.0 && c[2] == 0.0 {
            return None;
        }
        let (width, height) = (active[2] - active[0], active[3] - active[1]);
        if width < 2 || height < 2 {
            return None;
        }
        if crate::merrill_new::active("ca") {
            return Some(Self {
                profile: ca.profile,
                scale_r: f64::from(ca.profile_scale.clamp(0.0, 2.0)),
                scale_b: f64::from(ca.profile_scale.clamp(0.0, 2.0)),
                c,
                width,
                height,
                own: Some([f64::from(ca.red.clamp(-2.0, 2.0)), f64::from(ca.blue.clamp(-2.0, 2.0))]),
            });
        }
        // In profile mode the profile amount is stored in the red field.
        let mut red = if ca.profile { ca.profile_scale } else { ca.red };
        let mut blue = ca.blue;
        let idx = usize::from(!ca.profile);
        let a_r = red.abs();
        let a_b = if ca.profile { a_r } else { blue.abs() };
        let widen = |a: f32| {
            if 1.0 < a {
                ((1.0 / REACH_DIV[idx] + 1.0) * f64::from(a)) as f32
            } else {
                a
            }
        };
        let step = REACH_STEP[idx];
        let reach = |p: f64, a: f32| -> f64 {
            flag(p == SQRT2) * SQRT2
                + flag(p < SQRT2) * (p - f64::from(a) * step)
                + flag(SQRT2 < p) * (p + f64::from(a) * step)
        };
        let reach_r = reach(corner(&c, 0), widen(a_r));
        let reach_b = if ca.profile {
            corner(&c, 2)
        } else {
            reach(corner(&c, 2), widen(a_b))
        };
        let (w, h) = (f64::from(width), f64::from(height));
        let pixels = |v: f64| -> f32 {
            let x = (w * 0.5 + 0.5 + (w * 0.5 + -0.5) * v).abs().floor();
            let y = (h * 0.5 + 0.5 + (h * 0.5 + -0.5) * v).abs().floor();
            x.max(y) as f32
        };
        let v_r = if red == 0.0 { -1.0 } else { reach_r / -SQRT2 };
        let v_b = if a_b == 0.0 { -1.0 } else { reach_b / -SQRT2 };
        if 15.0 < pixels(v_r) {
            red = if 0.0 < red { 1.0 } else { -1.0 };
        }
        if 15.0 < pixels(v_b) {
            blue = if 0.0 < blue { 1.0 } else { -1.0 };
        }
        let scale_r = f64::from(red.abs());
        let scale_b = if ca.profile {
            scale_r
        } else {
            f64::from(blue.abs())
        };
        Some(Self {
            profile: ca.profile,
            scale_r,
            scale_b,
            c,
            width,
            height,
            own: None,
        })
    }

    /// graineo addition: the prepared values for the GPU version of [`Self::apply`] (f32):
    /// `[profile, scale_r, scale_b, width, height, manual_step, sig_mid, sig_slope]`, the flags
    /// `[eq_r, gt_r, lt_r, eq_b, gt_b, lt_b]` (comparison of P(√2) with √2, computed here in f64)
    /// and the twelve coefficients.
    pub(crate) fn gpu_values(&self) -> ([f32; 8], [f32; 6], [f32; 12]) {
        let c = &self.c;
        let (pr_c, pb_c) = (corner(c, 0), corner(c, 2));
        let flags = [
            flag(pr_c == SQRT2),
            flag(SQRT2 < pr_c),
            flag(pr_c < SQRT2),
            flag(pb_c == SQRT2),
            flag(SQRT2 < pb_c),
            flag(pb_c < SQRT2),
        ]
        .map(|v| v as f32);
        (
            [
                f32::from(u8::from(self.profile)),
                self.scale_r as f32,
                self.scale_b as f32,
                self.width as f32,
                self.height as f32,
                MANUAL_STEP as f32,
                SIG_MID as f32,
                SIG_SLOPE as f32,
            ],
            flags,
            c.map(|v| v as f32),
        )
    }

    /// Shift: `src` → `dst` (same tile size `size`, RGB).
    /// `origin` = position of the tile in the raw image; negative values (border tiles with
    /// overlap) are clamped to 0 and the tile is computed as if it started at 0, matching SPP's output.
    pub(crate) fn apply(
        &self,
        src: &[f32],
        src_stride: usize,
        dst: &mut [f32],
        dst_stride: usize,
        size: [usize; 2],
        origin: [isize; 2],
    ) {
        if self.own.is_some() {
            self.apply_own(src, src_stride, dst, dst_stride, size, origin);
            return;
        }
        let c = &self.c;
        let (r0, r1, r2, r3) = (c[0], c[3], c[6], c[9]);
        let (b0, b1, b2, b3) = (c[2], c[5], c[8], c[11]);
        let ox = origin[0].max(0) as i32;
        let oy = origin[1].max(0) as i32;
        let (w, h) = (f64::from(self.width), f64::from(self.height));
        let sx = 2.0 / (w + -1.0);
        let sy = 2.0 / (h + -1.0);
        let (hw_m, hw_p) = (w * 0.5 + -0.5, w * 0.5 + 0.5);
        let (hh_m, hh_p) = (h * 0.5 + -0.5, h * 0.5 + 0.5);
        let (pr_c, pb_c) = (corner(c, 0), corner(c, 2));
        let (eq_r, gt_r, lt_r) = (flag(pr_c == SQRT2), flag(SQRT2 < pr_c), flag(pr_c < SQRT2));
        let (eq_b, gt_b, lt_b) = (flag(pb_c == SQRT2), flag(SQRT2 < pb_c), flag(pb_c < SQRT2));
        let (cols, rows) = (size[0], size[1]);
        let (tile_w, tile_h) = (cols as f64, rows as f64);
        let (fox, foy) = (f64::from(ox), f64::from(oy));
        let (s_r, s_b) = (self.scale_r, self.scale_b);
        // Bilinear as in SPP. Reads past the last pixel are clamped here; this only affects the
        // discarded overlap border.
        let sample = |ch: usize, sx_: f64, sy_: f64, fx: f64, fy: f64| -> f32 {
            let x0 = (fx as usize).min(cols - 1);
            let x1 = (fx as usize + 1).min(cols - 1);
            let y0 = (fy as usize).min(rows - 1);
            let y1 = (fy as usize + 1).min(rows - 1);
            let p = |x: usize, y: usize| f64::from(src[y * src_stride + x * 3 + ch]);
            let tx = sx_ - fx;
            let ty = sy_ - fy;
            ((p(x1, y1) * tx + (1.0 - tx) * p(x0, y1)) * ty
                + (1.0 - ty) * (p(x1, y0) * tx + p(x0, y0) * (1.0 - tx))) as f32
        };
        for row in 0..rows {
            let y = f64::from(oy + row as i32) * sy + -1.0;
            for col in 0..cols {
                let si = row * src_stride + col * 3;
                let di = row * dst_stride + col * 3;
                let x = f64::from(ox + col as i32) * sx + -1.0;
                let r = (x * x + y * y).sqrt();
                dst[di] = src[si];
                dst[di + 1] = src[si + 1];
                dst[di + 2] = src[si + 2];
                if r == 0.0 {
                    continue;
                }
                // P(r) in the same evaluation order as SPP (bit-identical):
                // ((c0·r + c1·r·r) + c2·r³) + c3·(r·r³)
                let rc = r * r * r;
                let r4 = r * rc;
                let pr = ((r1 * r * r + r0 * r) + r2 * rc) + r3 * r4;
                let pb = ((b1 * r * r + b0 * r) + b2 * rc) + b3 * r4;
                let (rr, rb) = if self.profile {
                    let rr = (r - pr).abs() * s_r + pr;
                    let rb = (r - pb).abs() * s_r + pb;
                    (rr, rb)
                } else {
                    let sig_r = 1.0 / (((pr + SIG_MID) * SIG_SLOPE).exp() + 1.0);
                    let sig_b = 1.0 / (((pb + SIG_MID) * SIG_SLOPE).exp() + 1.0);
                    let d_r = sig_r * s_r * MANUAL_STEP;
                    let d_b = sig_b * s_b * MANUAL_STEP;
                    let rr = r * eq_r + (pr - d_r) * lt_r + (pr + d_r) * gt_r;
                    let rb = (pb - d_b) * lt_b + (pb + d_b) * gt_b + r * eq_b;
                    (rr, rb)
                };
                let f_r = rr / r;
                let f_b = rb / r;
                // red
                let src_x = ((x * f_r * hw_m + hw_p) - fox) + -1.0;
                let fx = src_x.floor();
                if !(tile_w < fx || fx < 0.0) {
                    let src_y = ((f_r * y * hh_m + hh_p) - foy) + -1.0;
                    let fy = src_y.floor();
                    if !(tile_h < fy || fy < 0.0) {
                        dst[di] = sample(0, src_x, src_y, fx, fy);
                    }
                }
                // blue
                let src_x = ((x * f_b * hw_m + hw_p) - fox) + -1.0;
                let fx = src_x.floor();
                if !(tile_w < fx || fx < 0.0) {
                    let src_y = ((f_b * y * hh_m + hh_p) - foy) + -1.0;
                    let fy = src_y.floor();
                    if !(tile_h < fy || fy < 0.0) {
                        dst[di + 2] = sample(2, src_x, src_y, fx, fy);
                    }
                }
            }
        }
    }
}

impl CaTransform {
    /// Alternative form of [`Self::apply`] (see the module documentation): same tiles and
    /// coordinates, own radial model.
    fn apply_own(&self, src: &[f32], src_stride: usize, dst: &mut [f32], dst_stride: usize, size: [usize; 2], origin: [isize; 2]) {
        let Some([red, blue]) = self.own else { return };
        let c = &self.c;
        let (cols, rows) = (size[0], size[1]);
        let (ox, oy) = (origin[0].max(0) as f64, origin[1].max(0) as f64);
        // radius in the coordinates of the profile: x and y each −1 … 1 over the active area
        let (cx, cy) = ((f64::from(self.width) - 1.0) * 0.5, (f64::from(self.height) - 1.0) * 0.5);
        let poly = |ch: usize, r: f64| -> f64 { c[ch] * r + c[3 + ch] * r * r + c[6 + ch] * r * r * r + c[9 + ch] * r * r * r * r };
        let factor = |ch: usize, r: f64| -> f64 {
            let src = if self.profile {
                r + self.scale_r * (poly(ch, r) - r)
            } else {
                r * (1.0 + MANUAL_GAIN * if ch == 0 { red } else { blue })
            };
            (src / r).clamp(1.0 - OWN_LIMIT, 1.0 + OWN_LIMIT)
        };
        let sample = |ch: usize, x: f64, y: f64| -> Option<f32> {
            if !(x >= 0.0 && y >= 0.0) {
                return None;
            }
            let (x0, y0) = (x.floor() as usize, y.floor() as usize);
            if x0 >= cols || y0 >= rows {
                return None;
            }
            let (x1, y1) = ((x0 + 1).min(cols - 1), (y0 + 1).min(rows - 1));
            let (tx, ty) = (x - x0 as f64, y - y0 as f64);
            let p = |x: usize, y: usize| f64::from(src[y * src_stride + x * 3 + ch]);
            Some(((p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx) * (1.0 - ty) + (p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx) * ty) as f32)
        };
        for row in 0..rows {
            let dy = oy + row as f64 - cy;
            for col in 0..cols {
                let (si, di) = (row * src_stride + col * 3, row * dst_stride + col * 3);
                dst[di..di + 3].copy_from_slice(&src[si..si + 3]);
                let dx = ox + col as f64 - cx;
                let r = ((dx / cx).powi(2) + (dy / cy).powi(2)).sqrt();
                if r == 0.0 {
                    continue;
                }
                for ch in [0usize, 2] {
                    let f = factor(ch, r);
                    if let Some(v) = sample(ch, cx + dx * f - ox, cy + dy * f - oy) {
                        dst[di + ch] = v;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(cols: usize, rows: usize) -> Vec<f32> {
        (0..cols * rows * 3)
            .map(|i| ((i * 37) % 1000) as f32)
            .collect()
    }

    #[test]
    fn manual_zero_is_near_identity() {
        let ca = CaCorrection {
            profile: false,
            profile_scale: 1.0,
            red: 0.0,
            blue: 0.0,
            coeffs: {
                let mut c = [0.0; 12];
                c[0] = 1.0;
                c[2] = 1.0;
                c
            },
        };
        let t = CaTransform::prepare(&ca, [64, 36, 4768, 3172]).unwrap();
        let (cols, rows) = (96, 64);
        let src = ramp(cols, rows);
        let mut dst = vec![0.0; src.len()];
        t.apply(
            &src,
            cols * 3,
            &mut dst,
            cols * 3,
            [cols, rows],
            [1000, 500],
        );
        for (a, b) in src.iter().zip(&dst) {
            assert!((a - b).abs() < 1e-2, "{a} {b}");
        }
    }

    #[test]
    fn missing_coeffs_disable() {
        let ca = CaCorrection {
            profile: true,
            profile_scale: 1.0,
            red: 0.0,
            blue: 0.0,
            coeffs: [0.0; 12],
        };
        assert!(CaTransform::prepare(&ca, [0, 0, 100, 100]).is_none());
    }

    #[test]
    fn large_reach_clips_scale() {
        // c0 = 1.01 → corner ≈ 0.0141·2352 px ≫ 15 → profile amount limited to 1
        let mut c = [0.0; 12];
        c[0] = 1.01;
        c[2] = 1.0;
        let ca = CaCorrection {
            profile: true,
            profile_scale: 2.0,
            red: 0.0,
            blue: 0.0,
            coeffs: c,
        };
        let t = CaTransform::prepare(&ca, [64, 36, 4768, 3172]).unwrap();
        assert_eq!(t.scale_r, 1.0);
    }
}
