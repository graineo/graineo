//! Crop and straighten: a rotated rectangle in the source image.
//! Development always runs on the whole image; crop and rotation are only applied for
//! display (as a texture mapping) or on export (resampling).
//!
//! Coordinate spaces: sensor image → (90° steps) upright image → (perspective,
//! projective keystone correction around the image centre) rectified image → (fine angle, crop)
//! crop 0 … 1. All paths (preview, 100 %, loupe, export, vignette) go through
//! [`Frame::to_src`]/[`Frame::from_src`] and therefore see the same position.

use rayon::prelude::*;

use crate::{Rgb16, Settings};

/// Crop of an image (made upright in 90° steps): centre, width/height in the
/// rotated frame and fine angle, all in the *upright* image. `q` quarter turns
/// clockwise turn the sensor image (`sw` × `sh`) into the upright image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    /// Fine angle in degrees (positive = straightened clockwise)
    pub angle: f32,
    pub q: u8,
    pub sw: f32,
    pub sh: f32,
    /// Perspective vertical/horizontal, −1 … +1 (0 = none). Negative widens the top or
    /// left (converging verticals when looking up), see [`Frame::persp_to_ori`].
    pub pv: f32,
    pub ph: f32,
}

/// Perspective strength at ±1: projective part relative to half the long image side.
/// At ±1 one image edge (horizontal, vertical) is about 2.2 times as wide as the other.
pub const PERSPECTIVE_STRENGTH: f32 = 0.5;

/// Effective quarter turns: camera orientation + rotation by the user.
pub fn quarters(s: &Settings, camera: u8) -> u8 {
    (camera + s.rotate) % 4
}

/// Quarter turns from the camera value `ROTATION` (degrees clockwise).
pub fn quarters_from_degrees(deg: i32) -> u8 {
    (deg.rem_euclid(360) / 90) as u8
}

impl Frame {
    /// Dimensions of the upright image.
    pub fn oriented(&self) -> (f32, f32) {
        if self.q % 2 == 1 {
            (self.sh, self.sw)
        } else {
            (self.sw, self.sh)
        }
    }

    /// Whole (upright) image without fine angle.
    pub fn full(full_w: usize, full_h: usize, q: u8) -> Self {
        let (sw, sh) = (full_w as f32, full_h as f32);
        let (ow, oh) = if q % 2 == 1 { (sh, sw) } else { (sw, sh) };
        Self { cx: ow / 2.0, cy: oh / 2.0, w: ow, h: oh, angle: 0.0, q: q % 4, sw, sh, pv: 0.0, ph: 0.0 }
    }

    /// Whole rectified image without fine angle and crop, but with the perspective from the
    /// settings (view in crop mode).
    pub fn full_settings(s: &Settings, full_w: usize, full_h: usize, camera: u8) -> Self {
        let (pv, ph) = perspective(s);
        Self { pv, ph, ..Self::full(full_w, full_h, quarters(s, camera)) }
    }

    /// From the settings (`crop` relative to the upright image); `camera` = camera orientation.
    pub fn from_settings(s: &Settings, full_w: usize, full_h: usize, camera: u8) -> Self {
        let base = Self::full(full_w, full_h, quarters(s, camera));
        let (ow, oh) = base.oriented();
        let [cx, cy, w, h] = s.crop;
        // A crop without area (from old settings or foreign values) is not a crop: show the whole image
        let [cx, cy, w, h] = if w < 0.02 || h < 0.02 || !(w.is_finite() && h.is_finite()) { [0.5, 0.5, 1.0, 1.0] } else { [cx, cy, w, h] };
        let (pv, ph) = perspective(s);
        Self { cx: cx * ow, cy: cy * oh, w: w * ow, h: h * oh, angle: s.crop_angle, pv, ph, ..base }
    }

    /// Back to the relative representation of the settings.
    pub fn to_settings(&self) -> ([f32; 4], f32) {
        let (ow, oh) = self.oriented();
        ([self.cx / ow, self.cy / oh, self.w / ow, self.h / oh], self.angle)
    }

    pub fn is_identity(&self) -> bool {
        let f = Self::full(self.sw as usize, self.sh as usize, 0);
        self.q == 0 && (self.cx - f.cx).abs() < 0.5 && (self.cy - f.cy).abs() < 0.5 && (self.w - f.w).abs() < 0.5 && (self.h - f.h).abs() < 0.5 && self.angle.abs() < 1e-4 && !self.has_perspective()
    }

    pub fn has_perspective(&self) -> bool {
        self.pv != 0.0 || self.ph != 0.0
    }

    /// Projective coefficients (a, b) per pixel around the image centre.
    fn keystone(&self) -> (f32, f32) {
        let (ow, oh) = self.oriented();
        let r = 0.5 * ow.max(oh);
        (self.ph * PERSPECTIVE_STRENGTH / r, self.pv * PERSPECTIVE_STRENGTH / r)
    }

    /// Rectified image → upright image (homography with fixed image centre and scale 1
    /// at the centre): x' = x / (1 + a·x + b·y), y' = y / (1 + a·x + b·y), around the centre.
    pub fn persp_to_ori(&self, p: [f32; 2]) -> [f32; 2] {
        if !self.has_perspective() {
            return p;
        }
        let (ow, oh) = self.oriented();
        let (a, b) = self.keystone();
        let (x, y) = (p[0] - ow / 2.0, p[1] - oh / 2.0);
        let d = (1.0 + a * x + b * y).max(1e-3);
        [ow / 2.0 + x / d, oh / 2.0 + y / d]
    }

    /// Upright image → rectified image (inverse of [`Self::persp_to_ori`]).
    pub fn ori_to_persp(&self, p: [f32; 2]) -> [f32; 2] {
        if !self.has_perspective() {
            return p;
        }
        let (ow, oh) = self.oriented();
        let (a, b) = self.keystone();
        let (x, y) = (p[0] - ow / 2.0, p[1] - oh / 2.0);
        let d = (1.0 - a * x - b * y).max(1e-3);
        [ow / 2.0 + x / d, oh / 2.0 + y / d]
    }

    fn rot(&self) -> (f32, f32) {
        let a = self.angle.to_radians();
        (a.sin(), a.cos())
    }

    /// Upright image → sensor image.
    pub fn ori_to_src(&self, p: [f32; 2]) -> [f32; 2] {
        let (sw, sh) = (self.sw, self.sh);
        match self.q {
            1 => [p[1], sh - p[0]],
            2 => [sw - p[0], sh - p[1]],
            3 => [sw - p[1], p[0]],
            _ => p,
        }
    }

    /// Sensor image → upright image.
    pub fn src_to_ori(&self, p: [f32; 2]) -> [f32; 2] {
        let (sw, sh) = (self.sw, self.sh);
        match self.q {
            1 => [sh - p[1], p[0]],
            2 => [sw - p[0], sh - p[1]],
            3 => [p[1], sw - p[0]],
            _ => p,
        }
    }

    /// Point in the crop (0 … 1 per axis) → rectified (upright) image.
    pub fn to_ori(&self, u: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.rot();
        let (lx, ly) = ((u[0] - 0.5) * self.w, (u[1] - 0.5) * self.h);
        [self.cx + c * lx - s * ly, self.cy + s * lx + c * ly]
    }

    /// Rectified (upright) image → point in the crop.
    pub fn from_ori(&self, p: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.rot();
        let (dx, dy) = (p[0] - self.cx, p[1] - self.cy);
        let (lx, ly) = (c * dx + s * dy, -s * dx + c * dy);
        [lx / self.w + 0.5, ly / self.h + 0.5]
    }

    /// Point in the crop (0 … 1 per axis) → sensor pixel.
    pub fn to_src(&self, u: [f32; 2]) -> [f32; 2] {
        self.ori_to_src(self.persp_to_ori(self.to_ori(u)))
    }

    /// Sensor pixel → point in the crop (0 … 1 per axis, may be outside).
    pub fn from_src(&self, p: [f32; 2]) -> [f32; 2] {
        self.from_ori(self.ori_to_persp(self.src_to_ori(p)))
    }

    /// The four corners (top left, top right, bottom right, bottom left) in the rectified image.
    pub fn corners_ori(&self) -> [[f32; 2]; 4] {
        [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]].map(|u| self.to_ori(u))
    }

    /// Axis-aligned rectangle in the sensor image enclosing the crop: x, y, w, h.
    pub fn bounds(&self) -> [usize; 4] {
        // Lines stay lines under the homography: the corners suffice for the hull
        let c = self.corners_ori().map(|p| self.ori_to_src(self.persp_to_ori(p)));
        let (x0, x1) = c.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        let (y0, y1) = c.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        let (x0, y0) = (x0.floor().max(0.0) as usize, y0.floor().max(0.0) as usize);
        let (x1, y1) = ((x1.ceil() as usize).min(self.sw as usize), (y1.ceil() as usize).min(self.sh as usize));
        [x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1)]
    }

    /// Does the crop lie entirely within the image?
    pub fn inside(&self) -> bool {
        let (ow, oh) = self.oriented();
        // The image is convex and lines stay lines: if the corners are inside, the
        // whole crop is inside, even with perspective.
        self.corners_ori().iter().map(|p| self.persp_to_ori(*p)).all(|p| p[0] >= -0.5 && p[1] >= -0.5 && p[0] <= ow + 0.5 && p[1] <= oh + 0.5)
    }

    /// Shrinks the crop (aspect ratio stays) and moves it back if necessary
    /// until it lies entirely within the image, as common photo apps do when rotating.
    pub fn constrain(mut self) -> Self {
        let (fw, fh) = self.oriented();
        self.w = self.w.clamp(16.0, fw * 2.0);
        self.h = self.h.clamp(16.0, fh * 2.0);
        self.cx = self.cx.clamp(0.0, fw);
        self.cy = self.cy.clamp(0.0, fh);
        if self.inside() {
            return self;
        }
        // move first (if the size would fit), then shrink
        for _ in 0..2 {
            let c = self.corners_ori();
            let (x0, x1) = c.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (y0, y1) = c.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            if x1 - x0 <= fw {
                self.cx += (-x0).max(0.0) - (x1 - fw).max(0.0);
            }
            if y1 - y0 <= fh {
                self.cy += (-y0).max(0.0) - (y1 - fh).max(0.0);
            }
        }
        if self.inside() {
            return self;
        }
        // With perspective the valid area is a quadrilateral: if even the centre lies outside,
        // shrinking does not help; then go back to the image centre (which the rectification keeps fixed).
        let tiny = Self { w: 1.0, h: 1.0, ..self };
        if !tiny.inside() {
            self.cx = fw / 2.0;
            self.cy = fh / 2.0;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let base = self;
        for _ in 0..30 {
            let k = (lo + hi) / 2.0;
            let t = Self { w: base.w * k, h: base.h * k, ..base };
            if t.inside() {
                lo = k;
            } else {
                hi = k;
            }
        }
        Self { w: base.w * lo, h: base.h * lo, ..base }
    }
}

/// Sets angle and perspective and adjusts the crop: centre and aspect ratio
/// stay, the size becomes as large as possible without empty corners (like "constrain
/// crop" in common photo apps). This way the crop grows again when the correction is reduced.
pub fn set_transform(s: &mut Settings, angle: f32, pv: f32, ph: f32, full_w: usize, full_h: usize, camera: u8) {
    s.crop_angle = angle.clamp(-45.0, 45.0);
    s.perspective_v = pv.clamp(-1.0, 1.0);
    s.perspective_h = ph.clamp(-1.0, 1.0);
    let f = Frame::from_settings(s, full_w, full_h, camera);
    let (fw, fh) = f.oriented();
    let r = f.w / f.h.max(1.0);
    let (w, h) = if fw / fh > r { (fh * r, fh) } else { (fw, fw / r) };
    let f = Frame { w, h, ..f }.constrain();
    s.crop = f.to_settings().0;
}

/// Perspective from the settings (clamped to −1 … +1).
pub fn perspective(s: &Settings) -> (f32, f32) {
    (s.perspective_v.clamp(-1.0, 1.0), s.perspective_h.clamp(-1.0, 1.0))
}

/// Phases per pixel for the Lanczos table (sampling position accurate to 1/64 pixel).
const PHASES: usize = 64;

/// Lanczos-3 weights for all phases, each phase normalised to sum 1.
fn lanczos_table() -> Vec<[f32; 6]> {
    let l = |x: f32| -> f32 {
        if x.abs() < 1e-6 {
            1.0
        } else if x.abs() >= 3.0 {
            0.0
        } else {
            let px = std::f32::consts::PI * x;
            3.0 * px.sin() * (px / 3.0).sin() / (px * px)
        }
    };
    (0..PHASES)
        .map(|k| {
            let t = k as f32 / PHASES as f32;
            // Taps at −2 … +3 relative to the pixel left/above the sampling position
            let mut w = [0.0f32; 6];
            for (i, v) in w.iter_mut().enumerate() {
                *v = l(i as f32 - 2.0 - t);
            }
            let sum: f32 = w.iter().sum();
            w.map(|v| v / sum)
        })
        .collect()
}

/// Applies perspective, crop and rotation to a fully rendered image
/// (Lanczos-3, so that the Foveon sharpness is preserved). Without a crop the image is
/// returned unchanged.
pub fn apply(img: Rgb16, s: &Settings, camera: u8) -> Rgb16 {
    let f = Frame::from_settings(s, img.width, img.height, camera);
    if f.is_identity() {
        return img;
    }
    let (ow, oh) = (f.w.round().max(1.0) as usize, f.h.round().max(1.0) as usize);
    let (w, h) = (img.width, img.height);
    let src = &img.data;
    let table = lanczos_table();
    let mut data = vec![0u16; ow * oh * 3];
    data.par_chunks_mut(ow * 3).enumerate().for_each(|(y, line)| {
        for x in 0..ow {
            let p = f.to_src([(x as f32 + 0.5) / ow as f32, (y as f32 + 0.5) / oh as f32]);
            let (px, py) = ((p[0] - 0.5).clamp(0.0, (w - 1) as f32), (p[1] - 0.5).clamp(0.0, (h - 1) as f32));
            let (x0, y0) = (px.floor() as isize, py.floor() as isize);
            let wx = &table[(((px - x0 as f32) * PHASES as f32) as usize).min(PHASES - 1)];
            let wy = &table[(((py - y0 as f32) * PHASES as f32) as usize).min(PHASES - 1)];
            let mut acc = [0.0f32; 3];
            for (j, wyj) in wy.iter().enumerate() {
                let yy = (y0 + j as isize - 2).clamp(0, h as isize - 1) as usize;
                let row = &src[yy * w * 3..(yy + 1) * w * 3];
                let mut r = [0.0f32; 3];
                for (i, wxi) in wx.iter().enumerate() {
                    let xx = (x0 + i as isize - 2).clamp(0, w as isize - 1) as usize;
                    let q = &row[xx * 3..xx * 3 + 3];
                    r[0] += wxi * q[0] as f32;
                    r[1] += wxi * q[1] as f32;
                    r[2] += wxi * q[2] as f32;
                }
                for c in 0..3 {
                    acc[c] += wyj * r[c];
                }
            }
            for c in 0..3 {
                line[x * 3 + c] = (acc[c] + 0.5).clamp(0.0, 65535.0) as u16;
            }
        }
    });
    Rgb16 { width: ow, height: oh, data }
}

/// [`apply`] for unclipped floating-point RGB (scene-linear or extended values); nothing is clamped.
pub fn apply_f32(img: crate::Linear, s: &Settings, camera: u8) -> crate::Linear {
    let f = Frame::from_settings(s, img.width, img.height, camera);
    if f.is_identity() {
        return img;
    }
    let (ow, oh) = (f.w.round().max(1.0) as usize, f.h.round().max(1.0) as usize);
    let (w, h) = (img.width, img.height);
    let src = &img.data;
    let table = lanczos_table();
    let mut data = vec![0f32; ow * oh * 3];
    data.par_chunks_mut(ow * 3).enumerate().for_each(|(y, line)| {
        for x in 0..ow {
            let p = f.to_src([(x as f32 + 0.5) / ow as f32, (y as f32 + 0.5) / oh as f32]);
            let (px, py) = ((p[0] - 0.5).clamp(0.0, (w - 1) as f32), (p[1] - 0.5).clamp(0.0, (h - 1) as f32));
            let (x0, y0) = (px.floor() as isize, py.floor() as isize);
            let wx = &table[(((px - x0 as f32) * PHASES as f32) as usize).min(PHASES - 1)];
            let wy = &table[(((py - y0 as f32) * PHASES as f32) as usize).min(PHASES - 1)];
            let mut acc = [0.0f32; 3];
            for (j, wyj) in wy.iter().enumerate() {
                let yy = (y0 + j as isize - 2).clamp(0, h as isize - 1) as usize;
                let row = &src[yy * w * 3..(yy + 1) * w * 3];
                let mut r = [0.0f32; 3];
                for (i, wxi) in wx.iter().enumerate() {
                    let xx = (x0 + i as isize - 2).clamp(0, w as isize - 1) as usize;
                    let q = &row[xx * 3..xx * 3 + 3];
                    r[0] += wxi * q[0];
                    r[1] += wxi * q[1];
                    r[2] += wxi * q[2];
                }
                for c in 0..3 {
                    acc[c] += wyj * r[c];
                }
            }
            for c in 0..3 {
                line[x * 3 + c] = acc[c];
            }
        }
    });
    crate::Linear { mono: false, haze: None, toneq: None, width: ow, height: oh, data, detail: None }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pv: f32, ph: f32) -> Frame {
        Frame { pv, ph, ..Frame::full(4704, 3136, 0) }
    }

    #[test]
    fn perspective_invertible() {
        for (pv, ph) in [(0.0, 0.0), (-0.7, 0.0), (0.4, -0.6), (1.0, 1.0)] {
            let f = frame(pv, ph);
            for p in [[0.0, 0.0], [4704.0, 0.0], [1000.0, 2500.0], [2352.0, 1568.0]] {
                let q = f.ori_to_persp(f.persp_to_ori(p));
                assert!((q[0] - p[0]).abs() < 0.05 && (q[1] - p[1]).abs() < 0.05, "{pv} {ph} {p:?} → {q:?}");
            }
            // centre stays fixed
            let c = f.persp_to_ori([2352.0, 1568.0]);
            assert!((c[0] - 2352.0).abs() < 1e-3 && (c[1] - 1568.0).abs() < 1e-3);
        }
    }

    #[test]
    fn no_perspective_unchanged() {
        let f = frame(0.0, 0.0);
        assert!(f.is_identity());
        assert_eq!(f.to_src([0.25, 0.75]), [1176.0, 2352.0]);
        assert!(!frame(0.1, 0.0).is_identity());
    }

    #[test]
    fn negative_widens_top() {
        // Vertical negative: the top is sampled from a narrower strip (buildings
        // photographed from below become wider at the top, the edges parallel)
        let f = frame(-0.5, 0.0);
        let top = f.persp_to_ori([0.0, 0.0])[0];
        let bottom = f.persp_to_ori([0.0, 3136.0])[0];
        assert!(top > 0.0 && bottom < 0.0, "{top} {bottom}");
    }

    #[test]
    fn limit_without_empty_corners() {
        for (pv, ph, angle) in [(-0.8, 0.0, 0.0), (0.5, 0.6, 3.0), (0.0, -1.0, -10.0)] {
            let f = Frame { angle, ..frame(pv, ph) }.constrain();
            assert!(f.inside(), "{pv} {ph}");
            assert!(f.w > 1000.0 && f.h > 600.0, "{f:?}");
            let a = f.w / f.h;
            assert!((a - 1.5).abs() < 0.01, "aspect ratio {a}");
        }
    }
}
