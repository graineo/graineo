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
    /// Guided perspective (general homography from guide lines, see [`crate::guides`]); replaces `pv`/`ph` when set
    pub hom: Option<crate::guides::Hom>,
}

/// Largest crop side relative to the upright image: "Whole" after a guided perspective reaches a few times the image
/// (the correction is limited, see [`crate::guides`]); anything larger comes from broken or manipulated settings.
pub const MAX_CROP_REL: f32 = 8.0;
/// Largest crop area relative to the upright image (an extreme correction shown "Whole" is scaled down to it, centre
/// and aspect ratio stay): keeps the output of a 200-MP photo within a few times its own size.
pub const MAX_CROP_AREA: f32 = 4.0;

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
        Self { cx: ow / 2.0, cy: oh / 2.0, w: ow, h: oh, angle: 0.0, q: q % 4, sw, sh, pv: 0.0, ph: 0.0, hom: None }
    }

    /// Whole rectified image without fine angle and crop, but with the perspective from the
    /// settings (view in crop mode).
    pub fn full_settings(s: &Settings, full_w: usize, full_h: usize, camera: u8) -> Self {
        let base = Self::full(full_w, full_h, quarters(s, camera));
        let (pv, ph, hom) = perspective_of(s, &base);
        Self { pv, ph, hom, ..base }
    }

    /// From the settings (`crop` relative to the upright image); `camera` = camera orientation.
    pub fn from_settings(s: &Settings, full_w: usize, full_h: usize, camera: u8) -> Self {
        let base = Self::full(full_w, full_h, quarters(s, camera));
        let (ow, oh) = base.oriented();
        let [cx, cy, w, h] = s.crop;
        // A crop without area (from old settings or foreign values) is not a crop: show the whole image
        let [cx, cy, w, h] = if w < 0.02 || h < 0.02 || !(w.is_finite() && h.is_finite()) { [0.5, 0.5, 1.0, 1.0] } else { [cx, cy, w, h] };
        // a crop far larger than the image (manipulated values) would allocate an output of that size: bounded
        let (w, h) = (w.min(MAX_CROP_REL), h.min(MAX_CROP_REL));
        let shrink = (MAX_CROP_AREA / (w * h)).sqrt().min(1.0);
        let (w, h) = (w * shrink, h * shrink);
        let (cx, cy) = (if cx.is_finite() { cx.clamp(-MAX_CROP_REL, MAX_CROP_REL) } else { 0.5 }, if cy.is_finite() { cy.clamp(-MAX_CROP_REL, MAX_CROP_REL) } else { 0.5 });
        let (pv, ph, hom) = perspective_of(s, &base);
        Self { cx: cx * ow, cy: cy * oh, w: w * ow, h: h * oh, angle: s.crop_angle, pv, ph, hom, ..base }
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
        self.pv != 0.0 || self.ph != 0.0 || self.hom.is_some()
    }

    /// Centred, normalised coordinates of the guided homography: scale r and centre.
    fn hom_frame(&self) -> (f32, f32, f32) {
        let (ow, oh) = self.oriented();
        (0.5 * ow.max(oh), ow / 2.0, oh / 2.0)
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
        if let Some(h) = &self.hom {
            let (r, cx, cy) = self.hom_frame();
            let q = crate::guides::apply(&h.fwd, [(p[0] - cx) / r, (p[1] - cy) / r]);
            return [q[0] * r + cx, q[1] * r + cy];
        }
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
        if let Some(h) = &self.hom {
            let (r, cx, cy) = self.hom_frame();
            let q = crate::guides::apply(&h.inv, [(p[0] - cx) / r, (p[1] - cy) / r]);
            return [q[0] * r + cx, q[1] * r + cy];
        }
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

    /// The valid image area in the rectified frame (where the crop lives): the upright image under the perspective
    /// correction, a convex quadrilateral (lines stay lines), corners in order.
    pub fn valid_quad(&self) -> [[f32; 2]; 4] {
        let (ow, oh) = self.oriented();
        [[0.0, 0.0], [ow, 0.0], [ow, oh], [0.0, oh]].map(|p| self.ori_to_persp(p))
    }

    /// Moves the crop towards the centre `(tx, ty)` without ever changing its size, angle or aspect ratio: as far as
    /// the image allows, right up to the real edge of the valid area (also after straightening and perspective); at an
    /// edge it slides along it. The centres that keep the crop inside form a convex polygon (the valid area shrunk by
    /// the crop); the result is the point of it closest to the target. A crop larger than the valid area stays where
    /// it is.
    pub fn slide_to(self, tx: f32, ty: f32) -> Self {
        let quad = self.valid_quad();
        let offsets = self.corners_ori().map(|c| [c[0] - self.cx, c[1] - self.cy]);
        let orient = {
            let a: f32 = (0..4).map(|i| {
                let (p, q) = (quad[i], quad[(i + 1) % 4]);
                p[0] * q[1] - q[0] * p[1]
            }).sum();
            if a >= 0.0 { 1.0 } else { -1.0 }
        };
        // centres: inside quad - offset for every corner of the crop
        let mut poly: Vec<[f32; 2]> = quad.iter().map(|p| [p[0] - offsets[0][0], p[1] - offsets[0][1]]).collect();
        for o in &offsets[1..] {
            for i in 0..4 {
                let a = [quad[i][0] - o[0], quad[i][1] - o[1]];
                let b = [quad[(i + 1) % 4][0] - o[0], quad[(i + 1) % 4][1] - o[1]];
                poly = clip_half_plane(&poly, a, b, orient);
                if poly.is_empty() {
                    return self;
                }
            }
        }
        if poly.len() < 3 {
            return self;
        }
        let t = [tx, ty];
        let inside = (0..poly.len()).all(|i| orient * cross(poly[i], poly[(i + 1) % poly.len()], t) >= 0.0);
        let p = if inside {
            t
        } else {
            (0..poly.len())
                .map(|i| nearest_on_segment(t, poly[i], poly[(i + 1) % poly.len()]))
                .min_by(|a, b| dist2(*a, t).total_cmp(&dist2(*b, t)))
                .unwrap_or([self.cx, self.cy])
        };
        Self { cx: p[0], cy: p[1], ..self }
    }

    /// Shrinks the crop (aspect ratio stays) and moves it back if necessary
    /// until it lies entirely within the image, as common photo apps do when rotating.
    pub fn constrain(mut self) -> Self {
        let (fw, fh) = self.oriented();
        // an image smaller than 8 pixels must not panic here (lower bound above the upper one)
        self.w = self.w.clamp(16.0f32.min(fw * 2.0), fw * 2.0);
        self.h = self.h.clamp(16.0f32.min(fh * 2.0), fh * 2.0);
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

/// z of (b - a) × (p - a): > 0 left of a→b (in a y-down frame: clockwise side).
fn cross(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn dist2(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

fn nearest_on_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l = dx * dx + dy * dy;
    let t = if l > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l).clamp(0.0, 1.0) } else { 0.0 };
    [a[0] + t * dx, a[1] + t * dy]
}

/// Clips a convex polygon to the side of the line a→b where `orient · cross ≥ 0` (Sutherland–Hodgman, one edge).
fn clip_half_plane(poly: &[[f32; 2]], a: [f32; 2], b: [f32; 2], orient: f32) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let (dp, dq) = (orient * cross(a, b, p), orient * cross(a, b, q));
        if dp >= 0.0 {
            out.push(p);
        }
        if (dp >= 0.0) != (dq >= 0.0) {
            let t = dp / (dp - dq);
            out.push([p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1])]);
        }
    }
    out
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

/// Perspective of the frame: guide lines (general homography) if set and solvable, else the slider model.
fn perspective_of(s: &Settings, base: &Frame) -> (f32, f32, Option<crate::guides::Hom>) {
    if !s.perspective_guides.is_off() {
        let (ow, oh) = base.oriented();
        if let Some(sol) = crate::guides::solve(&s.perspective_guides, ow, oh) {
            return (0.0, 0.0, Some(sol.hom));
        }
    }
    let (pv, ph) = perspective(s);
    (pv, ph, None)
}

/// Fits the crop after the guides changed: centre and aspect ratio stay, as large as possible inside the image
/// ("Largest rectangle"); with `whole` the crop covers the whole rectified image instead (the margins stay visible).
pub fn fit_crop(s: &mut Settings, full_w: usize, full_h: usize, camera: u8, whole: bool) {
    let f = Frame::from_settings(s, full_w, full_h, camera);
    if whole {
        let q = f.valid_quad();
        let (x0, x1) = q.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
        let (y0, y1) = q.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        let g = Frame { cx: (x0 + x1) / 2.0, cy: (y0 + y1) / 2.0, w: x1 - x0, h: y1 - y0, angle: 0.0, ..f };
        s.crop = g.to_settings().0;
        s.crop_angle = 0.0;
        return;
    }
    let (fw, fh) = f.oriented();
    let r = f.w / f.h.max(1.0);
    let (w, h) = if fw / fh > r { (fh * r, fh) } else { (fw, fw / r) };
    let f = Frame { w, h, ..f }.constrain();
    s.crop = f.to_settings().0;
}

/// Fill of the area outside the photo when a guided crop shows the whole rectified image ("Whole"): a dark neutral
/// grey (display-encoded 16 bit, and scene-linear for the floating-point path).
const OUTSIDE_U16: u16 = 0x2020;
const OUTSIDE_F32: f32 = 0.015;

/// Sensor position more than one pixel outside the image?
fn outside(p: [f32; 2], w: usize, h: usize) -> bool {
    p[0] < -1.0 || p[1] < -1.0 || p[0] > w as f32 + 1.0 || p[1] > h as f32 + 1.0
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
            if f.hom.is_some() && outside(p, w, h) {
                line[x * 3..x * 3 + 3].fill(OUTSIDE_U16);
                continue;
            }
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
            if f.hom.is_some() && outside(p, w, h) {
                line[x * 3..x * 3 + 3].fill(OUTSIDE_F32);
                continue;
            }
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

    /// Guided homography: forward and inverse agree, masks/retouching (points in the upright image) map through the
    /// crop and back to the same spot, and the crop limit still finds a valid crop.
    #[test]
    fn guided_round_trip_and_constrain() {
        use crate::guides::{Guides, Line, Role};
        let mut s = Settings::default();
        s.perspective_guides = Guides {
            lines: vec![
                Line { p0: [0.25, 0.95], p1: [0.3, 0.08], role: Role::Vertical },
                Line { p0: [0.77, 0.93], p1: [0.71, 0.06], role: Role::Vertical },
                Line { p0: [0.2, 0.2], p1: [0.8, 0.16], role: Role::Horizontal },
                Line { p0: [0.15, 0.8], p1: [0.85, 0.85], role: Role::Horizontal },
            ],
            ..Default::default()
        };
        let f = Frame::from_settings(&s, 6000, 4000, 0);
        assert!(f.hom.is_some() && f.has_perspective() && !f.is_identity());
        for p in [[10.0, 10.0], [5990.0, 20.0], [3000.0, 2000.0], [1234.0, 3456.0]] {
            let q = f.persp_to_ori(f.ori_to_persp(p));
            assert!((q[0] - p[0]).abs() < 0.05 && (q[1] - p[1]).abs() < 0.05, "{p:?} → {q:?}");
            // through the crop and back (what masks, retouching and export do)
            let u = f.from_src(p);
            let b = f.to_src(u);
            assert!((b[0] - p[0]).abs() < 0.05 && (b[1] - p[1]).abs() < 0.05);
        }
        let mut t = s.clone();
        fit_crop(&mut t, 6000, 4000, 0, false);
        let g = Frame::from_settings(&t, 6000, 4000, 0);
        assert!(g.inside() && g.w > 1500.0, "{g:?}");
        assert!(((g.w / g.h) - 1.5).abs() < 0.01);
        fit_crop(&mut t, 6000, 4000, 0, true);
        let g = Frame::from_settings(&t, 6000, 4000, 0);
        assert!(!g.inside());
        // guides off: exactly the old slider model
        s.perspective_guides = Guides::default();
        s.perspective_v = -0.4;
        let f = Frame::from_settings(&s, 6000, 4000, 0);
        assert!(f.hom.is_none() && f.pv == -0.4);
    }

    /// Fuzz: guide lines and quadrilaterals as a manipulated file could carry them (NaN, infinite, huge, length 0,
    /// parallel, collinear, crossing) never panic, never give non-finite crops and never a crop (output size) beyond
    /// [`MAX_CROP_REL`] of the image.
    #[test]
    fn guided_fuzz_never_panics_or_explodes() {
        use crate::guides::{Guides, Line, Role};
        let seed = std::cell::Cell::new(0x9e37_79b9_7f4a_7c15u64);
        let rnd = || {
            let mut x = seed.get();
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            seed.set(x);
            x
        };
        let specials = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, 1.0, 0.5, -0.5, 1.5, 1e-30, 1e30, -1e30, 1e-7, 0.5 + 1e-7, f32::MAX, f32::MIN_POSITIVE];
        let num = || -> f32 {
            let k = rnd();
            if k % 5 == 0 { specials[(k >> 8) as usize % specials.len()] } else { ((k >> 11) as f64 / (1u64 << 53) as f64 * 1.6 - 0.3) as f32 }
        };
        let mut max_rel = 0.0f32;
        for case in 0..6000 {
            let mut s = Settings::default();
            let pt = || [num(), num()];
            let n = (rnd() % 6) as usize;
            let mut lines: Vec<Line> = (0..n).map(|_| Line { p0: pt(), p1: pt(), role: if rnd() % 2 == 0 { Role::Vertical } else { Role::Horizontal } }).collect();
            // degenerate on purpose: length 0, parallel copies, collinear continuation
            match case % 4 {
                1 if !lines.is_empty() => lines[0].p1 = lines[0].p0,
                2 if !lines.is_empty() => {
                    let l = lines[0];
                    lines.push(Line { p0: [l.p0[0] + 0.1, l.p0[1]], p1: [l.p1[0] + 0.1, l.p1[1]], ..l });
                }
                3 if !lines.is_empty() => {
                    let l = lines[0];
                    lines.push(Line { p0: l.p1, p1: [2.0 * l.p1[0] - l.p0[0], 2.0 * l.p1[1] - l.p0[1]], ..l });
                }
                _ => {}
            }
            let quad = (rnd() % 3 == 0).then(|| [pt(), pt(), pt(), pt()]);
            s.perspective_guides = Guides { lines, quad, aspect: num(), amount: num() };
            s.crop_angle = num().clamp(-45.0, 45.0);
            let (w, h) = [(6000, 4000), (4000, 6000), (1, 1), (17, 9000)][case % 4];
            let f = Frame::from_settings(&s, w, h, (case % 4) as u8);
            let _ = (f.bounds(), f.inside(), f.to_src([0.5, 0.5]), f.from_src([1.0, 1.0]));
            let _ = f.slide_to(f.cx + 100.0, f.cy - 1e9);
            for whole in [false, true] {
                let mut t = s.clone();
                fit_crop(&mut t, w, h, (case % 4) as u8, whole);
                assert!(t.crop.iter().all(|v| v.is_finite()), "case {case}: {:?} from {:?}", t.crop, s.perspective_guides);
                let g = Frame::from_settings(&t, w, h, (case % 4) as u8);
                let (ow, oh) = g.oriented();
                assert!(g.w.is_finite() && g.h.is_finite() && g.w <= MAX_CROP_REL * ow + 1.0 && g.h <= MAX_CROP_REL * oh + 1.0, "case {case}: {g:?}");
                assert!(g.w * g.h <= MAX_CROP_AREA * ow * oh * 1.001 + 1.0, "case {case}: area {g:?}");
                // normal image shapes: "Whole" stays well below the bound (a 17 × 9000 strip turned by 90° does not)
                if whole && g.hom.is_some() && case % 4 < 2 {
                    max_rel = max_rel.max(t.crop[2]).max(t.crop[3]);
                }
            }
        }
        eprintln!("largest relative whole crop side (before the bound): {max_rel}");
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

    /// Moving never changes size, angle or aspect ratio; after straightening and perspective ("Auto") the crop reaches
    /// the real edge of the valid area (no gap left above it) and slides along an edge instead of stopping.
    #[test]
    fn slide_keeps_size_and_reaches_the_real_edge() {
        for (angle, pv, ph) in [(0.0, 0.0, 0.0), (3.5, 0.0, 0.0), (-2.0, -0.3, 0.0), (1.2, 0.25, -0.15), (0.0, -0.4, 0.0)] {
            let base = Frame { angle, pv, ph, ..Frame::full(6000, 4000, 0) };
            let start = Frame { w: 3000.0, h: 2000.0, ..base }.constrain();
            assert!(start.inside());
            // far above the image: the crop stops at the top edge, as high as the image allows
            let top = start.slide_to(start.cx, -5000.0);
            assert_eq!((top.w, top.h, top.angle), (start.w, start.h, start.angle), "size stays ({angle}, {pv}, {ph})");
            assert!(top.inside(), "inside ({angle}, {pv}, {ph}): {top:?}");
            // a pixel further up would leave the image: it really touches the edge
            let higher = Frame { cy: top.cy - 1.0, ..top };
            assert!(!higher.inside(), "touches the top edge ({angle}, {pv}, {ph}): {top:?}");
            // sliding along: dragging up and to the right still moves right
            let right = top.slide_to(top.cx + 300.0, top.cy - 20.0);
            assert!(right.cx > top.cx + 50.0 && right.inside(), "slides along the edge ({angle}, {pv}, {ph}): {right:?}");
            assert_eq!((right.w, right.h), (start.w, start.h));
            // inside: exactly where it was dragged
            let inner = start.slide_to(start.cx + 10.0, start.cy - 10.0);
            assert!((inner.cx - start.cx - 10.0).abs() < 1e-3 && (inner.cy - start.cy + 10.0).abs() < 1e-3);
        }
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
