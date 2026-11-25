//! "Look" stage after the Sigma tone curve:
//! tone curve ([`crate::curve`]), colour toning of shadows/midtones/highlights, matte blacks (fade), vignette and
//! film grain. Works on the finished sRGB image (0 … 1) and knows the position of the crop
//! in the full image, so that vignette and grain sit the same in preview, detail view and export.

use rayon::prelude::*;

use crate::{Rgb16, Settings, View};

/// Position of a rendered image in the full image (full resolution).
#[derive(Clone, Copy, Debug)]
pub struct Geo {
    pub full_w: usize,
    pub full_h: usize,
    /// top left corner of the crop in full image pixels
    pub off_x: usize,
    pub off_y: usize,
    /// full image pixels per output pixel (preview > 1)
    pub scale: f32,
    /// Camera orientation in quarter turns (for the vignette after cropping)
    pub camera: u8,
}

impl Geo {
    pub fn full(w: usize, h: usize) -> Self {
        Self { full_w: w, full_h: h, off_x: 0, off_y: 0, scale: 1.0, camera: 0 }
    }
}

/// Is any look slider active?
/// Centres of the colour ranges of the colour mixer (degrees).
pub const HSL_CENTERS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0];

/// Share of each colour range in a hue (linear crossfade between neighbours).
pub fn hsl_weights(hue: f32) -> [f32; 8] {
    let mut w = [0.0f32; 8];
    let h = hue.rem_euclid(360.0);
    for i in 0..8 {
        let (a, b) = (HSL_CENTERS[i], if i == 7 { 360.0 } else { HSL_CENTERS[i + 1] });
        if h >= a && h < b {
            let t = (h - a) / (b - a);
            w[i] = 1.0 - t;
            w[(i + 1) % 8] += t;
            break;
        }
    }
    w
}

pub fn rgb_to_hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 1e-6 {
        0.0
    } else if mx == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    (h, if mx > 1e-6 { d / mx } else { 0.0 }, mx)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// Colour mixer: shifts hue (± 30°), saturation and luminance per colour range.
/// Neutral tones stay untouched (the effect grows with saturation).
#[inline]
fn apply_hsl(c: [f32; 3], hsl: &[[f32; 3]; 8]) -> [f32; 3] {
    let (h, s, v) = rgb_to_hsv(c);
    if s < 0.01 {
        return c;
    }
    let w = hsl_weights(h);
    let (mut dh, mut ds, mut dl) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..8 {
        dh += w[i] * hsl[i][0];
        ds += w[i] * hsl[i][1];
        dl += w[i] * hsl[i][2];
    }
    let strength = (s * 2.5).min(1.0);
    let h2 = h + dh * 30.0 * strength;
    let s2 = (s * (1.0 + ds)).clamp(0.0, 1.0);
    let v2 = v * (1.0 + dl * 0.6 * strength);
    hsv_to_rgb(h2, s2, v2.max(0.0))
}

pub fn active(s: &Settings) -> bool {
    s.view == View::Developed
        && (s.fade > 0.0
            || !s.tone_curve.is_identity()
            || (!s.monochrome && s.hsl.iter().flatten().any(|&v| v != 0.0))
            || crate::skin::active(s)
            || s.clarity != 0.0
            || s.sharpen > 0.0
            || s.vignette != 0.0
            || s.grain_amount > 0.0
            || s.grade_shadows[1] > 0.0
            || s.grade_midtones[1] > 0.0
            || s.grade_highlights[1] > 0.0)
}

/// Colour of a hue (degrees) as a luminance-neutral offset.
pub(crate) fn hue_offset(hue: f32) -> [f32; 3] {
    let h = hue.rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let rgb = match h as u32 {
        0 => [1.0, x, 0.0],
        1 => [x, 1.0, 0.0],
        2 => [0.0, 1.0, x],
        3 => [0.0, x, 1.0],
        4 => [x, 0.0, 1.0],
        _ => [1.0, 0.0, x],
    };
    let y = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    rgb.map(|c| c - y)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Lattice noise: deterministic value per lattice point, approximately normally distributed (−1 … 1).
#[inline]
fn lattice(ix: i64, iy: i64, seed: u64) -> f32 {
    let mut h = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed;
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    // Sum of two uniform distributions ≈ triangular distribution, softer than pure white noise
    let a = (h & 0xFFFF) as f32 / 65535.0;
    let b = ((h >> 16) & 0xFFFF) as f32 / 65535.0;
    a + b - 1.0
}

/// Value noise with bilinear interpolation, cell size `cell` (pixels).
#[inline]
fn value_noise(x: f32, y: f32, cell: f32, seed: u64) -> f32 {
    let (fx, fy) = (x / cell, y / cell);
    let (ix, iy) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - ix, fy - iy);
    let (ix, iy) = (ix as i64, iy as i64);
    let a = lattice(ix, iy, seed);
    let b = lattice(ix + 1, iy, seed);
    let c = lattice(ix, iy + 1, seed);
    let d = lattice(ix + 1, iy + 1, seed);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

/// Box blur (three times ≈ Gaussian) on one channel, radius in pixels.
fn blur(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut a = src.to_vec();
    let mut b = vec![0f32; w * h];
    let r = radius.max(1);
    for _ in 0..3 {
        // horizontal
        b.par_chunks_mut(w).zip(a.par_chunks(w)).for_each(|(o, line)| {
            let mut acc = 0f32;
            let get = |x: isize| line[x.clamp(0, w as isize - 1) as usize];
            for x in -(r as isize)..=(r as isize) {
                acc += get(x);
            }
            let n = (2 * r + 1) as f32;
            for x in 0..w {
                o[x] = acc / n;
                acc += get(x as isize + r as isize + 1) - get(x as isize - r as isize);
            }
        });
        // vertical (column by column in strips)
        let cols: Vec<usize> = (0..w).collect();
        let out: Vec<Vec<f32>> = cols
            .par_chunks(64)
            .map(|cs| {
                let mut res = vec![0f32; cs.len() * h];
                for (k, &x) in cs.iter().enumerate() {
                    let get = |y: isize| b[y.clamp(0, h as isize - 1) as usize * w + x];
                    let mut acc = 0f32;
                    for y in -(r as isize)..=(r as isize) {
                        acc += get(y);
                    }
                    let n = (2 * r + 1) as f32;
                    for y in 0..h {
                        res[k * h + y] = acc / n;
                        acc += get(y as isize + r as isize + 1) - get(y as isize - r as isize);
                    }
                }
                res
            })
            .collect();
        for (chunk, res) in cols.chunks(64).zip(out) {
            for (k, &x) in chunk.iter().enumerate() {
                for y in 0..h {
                    a[y * w + x] = res[k * h + y];
                }
            }
        }
    }
    a
}

/// Clarity (large radius, midtones) and sharpening (small radius, edge mask) on the
/// luminance; colour stays, only the brightness per pixel is scaled.
fn local_contrast(img: &mut Rgb16, s: &Settings, geo: Geo) {
    let (w, h) = (img.width, img.height);
    if w < 4 || h < 4 {
        return;
    }
    let luma: Vec<f32> = img
        .data
        .par_chunks(3)
        .map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 65535.0)
        .collect();
    let mut target = luma.clone();
    if s.clarity != 0.0 {
        // Radius ≈ 0.8 % of the image width (≈ 38 px at 4704 px), in output pixels
        let r = ((geo.full_w as f32 * 0.008) / geo.scale.max(1.0) / 1.7).round().max(1.0) as usize;
        let base = blur(&luma, w, h, r);
        let k = s.clarity.clamp(-1.0, 1.0) * 0.8;
        target.par_iter_mut().zip(luma.par_iter().zip(base.par_iter())).for_each(|(t, (&l, &b))| {
            // Emphasise midtones, spare highlights and shadows
            let m = (4.0 * l * (1.0 - l)).clamp(0.0, 1.0);
            *t = l + k * m * (l - b);
        });
    }
    // Sharpening only visible from a radius ≥ 0.5 output pixels (hardly in the downscaled preview)
    let sr = s.sharpen_radius.clamp(0.5, 3.0) / geo.scale.max(1.0);
    if s.sharpen > 0.0 && sr >= 0.45 {
        let base = blur(&target, w, h, sr.round().max(1.0) as usize);
        let amount = s.sharpen.clamp(0.0, 2.0) * 1.2;
        let mask = s.sharpen_mask.clamp(0.0, 1.0);
        // Edge mask: gradient magnitude of the luminance
        let edge: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let at = |xx: usize, yy: usize| target[yy * w + xx];
                let gx = at((x + 1).min(w - 1), y) - at(x.saturating_sub(1), y);
                let gy = at(x, (y + 1).min(h - 1)) - at(x, y.saturating_sub(1));
                (gx * gx + gy * gy).sqrt()
            })
            .collect();
        let thr = mask * 0.08;
        target.par_iter_mut().enumerate().for_each(|(i, t)| {
            let m = if mask > 0.0 { ((edge[i] - thr * 0.5) / thr.max(1e-4)).clamp(0.0, 1.0) } else { 1.0 };
            *t += amount * m * (*t - base[i]);
        });
    }
    img.data.par_chunks_mut(3).zip(luma.par_iter().zip(target.par_iter())).for_each(|(p, (&l, &t))| {
        if l <= 1e-5 {
            return;
        }
        let f = (t.max(0.0) / l).clamp(0.0, 4.0);
        for v in p.iter_mut() {
            *v = (*v as f32 * f).clamp(0.0, 65535.0) as u16;
        }
    });
}

pub fn apply(img: &mut Rgb16, s: &Settings, geo: Geo) {
    // tone equalizer "show mask": the mask is shown as it is
    if s.tone_eq_mask {
        return;
    }
    // a crop at full resolution: the local adjustments follow the whole image (see `local::apply_view`)
    let view = (geo.scale == 1.0 && [img.width, img.height] != [geo.full_w, geo.full_h])
        .then_some(masks::View { full: [geo.full_w, geo.full_h], offset: [geo.off_x, geo.off_y] });
    crate::local::apply_view(img, s, view);
    // Remove colour fringes before all look steps (own detection on the output of the
    // Sigma tone curve, see there); no effect without an amount
    crate::defringe::apply(img, s, geo);
    if !active(s) {
        return;
    }
    if s.clarity != 0.0 || s.sharpen > 0.0 {
        local_contrast(img, s, geo);
    }
    let grades = [s.grade_shadows, s.grade_midtones, s.grade_highlights].map(|[h, sat]| {
        let o = hue_offset(h);
        o.map(|c| c * sat.clamp(0.0, 1.0) * 0.35)
    });
    // Tone curve as a table (None = diagonal, step skipped). It acts as the first
    // colour step after clarity/sharpening: as in common photo apps it belongs to the base tonality on
    // which colour mixer and color grading build (so the grading zones follow the
    // brightness you see). Vignette, grain and matte shadows are effects on top; a
    // steep curve should not push the raised black point back down.
    let curve = crate::curve::CurveLut::new(&s.tone_curve);
    let hsl_on = !s.monochrome && s.hsl.iter().flatten().any(|&v| v != 0.0);
    let skin_on = crate::skin::active(s);
    let grading = s.grade_shadows[1] > 0.0 || s.grade_midtones[1] > 0.0 || s.grade_highlights[1] > 0.0;
    let pivot = (0.5 - 0.3 * s.grade_balance).clamp(0.15, 0.85);
    let fade = s.fade.clamp(0.0, 0.4);
    let frame = crate::geometry::Frame::from_settings(s, geo.full_w, geo.full_h, geo.camera);
    let vig = s.vignette.clamp(-1.0, 1.0);
    let vmid = s.vignette_mid.clamp(0.0, 1.0);
    // Grain size relative to the image width (at 4704 px: size 1 ≈ 1.6 px)
    let cell = (s.grain_size.clamp(0.3, 4.0) * 1.6 * geo.full_w as f32 / 4704.0).max(0.6);
    // In the preview fine grains average out; dampen the strength accordingly
    let grain_amp = s.grain_amount.clamp(0.0, 1.0) * 0.16 * (cell / geo.scale.max(1.0)).min(1.0).sqrt();
    let rough = s.grain_rough.clamp(0.0, 1.0);
    let width = img.width;
    // skin-tone weights after tone curve and colour mixer, smoothed edge-preserving (empty when the stage is off)
    let skin_w: Vec<f32> = if skin_on {
        let pre = |i: usize| {
            let mut c = [img.data[i * 3] as f32 / 65535.0, img.data[i * 3 + 1] as f32 / 65535.0, img.data[i * 3 + 2] as f32 / 65535.0];
            if let Some(lut) = &curve {
                c = lut.apply(c);
            }
            if hsl_on {
                c = apply_hsl(c, &s.hsl);
            }
            c
        };
        let height = img.data.len() / 3 / width.max(1);
        crate::skin::weight_map(width, height, crate::skin::smooth_radius(geo.full_w, geo.full_h, geo.scale), pre)
    } else {
        Vec::new()
    };
    img.data.par_chunks_mut(width * 3).enumerate().for_each(|(y, line)| {
        let fy = geo.off_y as f32 + (y as f32 + 0.5) * geo.scale;
        for x in 0..width {
            let fx = geo.off_x as f32 + (x as f32 + 0.5) * geo.scale;
            let px = &mut line[x * 3..x * 3 + 3];
            let mut c = [px[0] as f32 / 65535.0, px[1] as f32 / 65535.0, px[2] as f32 / 65535.0];
            if let Some(lut) = &curve {
                c = lut.apply(c);
            }
            if hsl_on {
                c = apply_hsl(c, &s.hsl);
            }
            if skin_on {
                c = crate::skin::apply_weighted(c, s.skin_hue, s.skin_sat, skin_w[y * width + x]);
            }
            let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            if grading {
                let ws = 1.0 - smoothstep(0.0, pivot, l);
                let wh = smoothstep(pivot, 1.0, l);
                let wm = (1.0 - ws - wh).max(0.0);
                for k in 0..3 {
                    c[k] += ws * grades[0][k] + wm * grades[1][k] + wh * grades[2][k];
                }
            }
            if vig != 0.0 {
                // "After crop": relative to the (rotated) crop
                let u = frame.from_src([fx, fy]);
                let (dx, dy) = ((u[0] - 0.5) * 2.0, (u[1] - 0.5) * 2.0);
                let d = ((dx * dx + dy * dy) * 0.5).sqrt();
                let f = smoothstep(vmid * 0.8, 1.05, d);
                for v in &mut c {
                    *v = if vig < 0.0 { *v * (1.0 + vig * f) } else { *v + vig * f * (1.0 - *v) };
                }
            }
            if grain_amp > 0.0 {
                let mut n = value_noise(fx, fy, cell, 0x5157);
                if rough > 0.0 {
                    n = n * (1.0 - 0.5 * rough) + rough * value_noise(fx, fy, cell * 0.5, 0xA11CE);
                }
                // Grain is most visible in the midtones, hardly in deep black and white
                let m = (4.0 * l * (1.0 - l)).clamp(0.0, 1.0).sqrt() * 0.85 + 0.15;
                let g = n * grain_amp * m;
                for v in &mut c {
                    *v += g;
                }
            }
            if fade > 0.0 {
                for v in &mut c {
                    *v = fade + *v * (1.0 - fade);
                }
            }
            for k in 0..3 {
                px[k] = (c[k].clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            }
        }
    });
}

/// Extended-range export path; the legacy RGB16 path stays unchanged.
#[path = "look_wide.rs"]
pub mod wide;
