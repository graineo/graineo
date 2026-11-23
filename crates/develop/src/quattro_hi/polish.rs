//! High-ISO chain, step 4: local contrast over box means and sharpening with the detail signal of step 3.
//!
//! Both act on the brightness only: every pixel's three channels are multiplied by the same factor. The local
//! contrast compares the luma with its mean over a square window (unsharp masking with a large radius), ignores
//! small differences (coring) and limits the change. The sharpening adds the detail signal, scaled by a curve over
//! the brightness, within an absolute and a relative limit.
//!
//! Values: interleaved RGB in the 16-bit range of the colour stage.

use rayon::prelude::*;

use super::curve;

/// The two passes of the local contrast at the camera's detail step: window radius and strength.
fn contrast_passes(step: usize, pooled: bool) -> [(usize, f32); 2] {
    let level = step.min(6) as f32 / 3.0;
    [(7, 0.2 * level), (3, level * if pooled { 0.25 } else { 0.1 })]
}

/// Local contrast for ISO factors from 8 on (`step`: detail step, 3 = camera setting).
pub fn local_contrast(rgb: &mut [f32], w: usize, h: usize, step: usize, iso_factor: f32, pooled: bool) {
    for (radius, strength) in contrast_passes(step, pooled) {
        contrast_pass(rgb, w, h, radius, strength, DEAD_BAND_PER_ISO * iso_factor);
    }
}

/// Dead band of the local contrast per unit of the ISO factor.
const DEAD_BAND_PER_ISO: f32 = 3.333_333_3;
/// Largest luma change of one pass and largest relative change of a pixel.
const CONTRAST_STEP: f32 = 20.0;
const CONTRAST_SHARE: f32 = 0.1;

/// Luma weights of the stage (bottom, middle, top output channel = R, G, B).
#[inline]
fn luma(p: &[f32]) -> f32 {
    0.114 * p[2] + 0.587 * p[1] + 0.299 * p[0]
}

/// Factor on the three channels of a pixel with luma `y` whose box mean is `mean`.
#[inline]
fn contrast_factor(y: f32, mean: f32, strength: f32, dead_band: f32) -> f32 {
    let d = y - mean;
    let pushed = if d == 0.0 { 0.0 } else { d.signum() * (d.abs() - dead_band).max(0.0) };
    let target = (pushed * strength + y).clamp(y - CONTRAST_STEP, y + CONTRAST_STEP);
    ((target + 5.0) / (y + 5.0).max(1e-4)).clamp(1.0 - CONTRAST_SHARE, 1.0 + CONTRAST_SHARE)
}

/// One pass: whole-number luma, its mean along the rows (rounded) and then along the columns; pixels closer than
/// `r` to the border stay, and the column mean reads unfiltered lumas in the first and last `r` rows.
fn contrast_pass(rgb: &mut [f32], w: usize, h: usize, r: usize, strength: f32, dead_band: f32) {
    if w <= 2 * r + 1 || h <= 2 * r + 1 {
        return;
    }
    let n = (2 * r + 1) as f32;
    let mut rows: Vec<f32> = rgb.par_chunks(3).map(|p| luma(p).trunc()).collect();
    rows.par_chunks_mut(w).enumerate().for_each_init(
        || vec![0f32; w],
        |line, (y, row)| {
            if y < r || y + r >= h {
                return;
            }
            line.copy_from_slice(row);
            let mut sum: f32 = line[..2 * r].iter().sum();
            for x in r..w - r {
                sum += line[x + r];
                row[x] = (sum / n + 0.5).trunc();
                sum -= line[x - r];
            }
        },
    );
    rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, out)| {
        if y < r || y + r >= h {
            return;
        }
        for x in r..w - r {
            let mean = (y - r..=y + r).map(|yy| rows[yy * w + x]).sum::<f32>() / n;
            let p = &mut out[x * 3..x * 3 + 3];
            let f = contrast_factor(luma(p), mean, strength, dead_band);
            for v in p {
                *v = (*v * f).min(65534.0).max(0.0).trunc();
            }
        }
    });
}

/// Sharpening amount per detail step at the fifteen brightness nodes of [`curve::NODES`]: no sharpening up to
/// brightness 64, rising to a plateau from 512 to 8192, lower again at the top.
const AMOUNTS: [[f64; 15]; 7] = [
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.04, 1.05, 1.11, 1.11, 1.11, 1.11, 1.11, 1.04],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.06, 1.11, 1.25, 1.25, 1.25, 1.25, 1.25, 1.10],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.08, 1.16, 1.40, 1.40, 1.40, 1.40, 1.40, 1.16],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.10, 1.21, 1.56, 1.56, 1.56, 1.56, 1.56, 1.23],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.11, 1.27, 1.73, 1.73, 1.73, 1.73, 1.73, 1.31],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.13, 1.32, 1.91, 1.91, 1.91, 1.91, 1.91, 1.39],
];
/// The same for block means: one amount for all steps, halved at the top node.
const POOLED_AMOUNTS: [f64; 15] = [1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 1.15, 0.5];

/// Limits and the amount table of one sharpening run.
struct Sharpening {
    /// amount by brightness ([`curve::table`])
    pub amounts: Vec<f32>,
    /// largest change (absolute) and its largest share of the luma
    pub absolute: f32,
    pub relative: f32,
    /// factor on the brightness for the table lookup and on the luma
    pub lookup: f32,
    pub luma: f32,
    /// factor on the table value
    pub gain: f32,
}

impl Sharpening {
    /// `step`: detail step, `sharpness`: the sharpness setting (0 = camera setting).
    pub fn new(step: usize, iso_factor: f32, sharpness: f32, pooled: bool) -> Self {
        let s = f64::from(sharpness);
        if pooled {
            return Self { amounts: curve::table(&POOLED_AMOUNTS), absolute: 6000.0, relative: 0.35, lookup: 1.0, luma: 3.0, gain: (s * 0.5 + 1.0) as f32 };
        }
        let mut values = AMOUNTS[step.min(AMOUNTS.len() - 1)];
        let stops = f64::from(iso_factor.log2());
        let per_setting = stops * stops / 36.0 + 1.0;
        for (i, v) in values.iter_mut().enumerate() {
            let own = *v * if i == 14 { 0.5 } else { 1.15 };
            *v = if s >= 0.0 { own + s * per_setting } else { own * (s * 0.5 + 1.0) };
        }
        let absolute = (8000.0 * stops * stops / 36.0 + 2000.0) as f32 / iso_factor;
        Self { amounts: curve::table(&values), absolute, relative: 0.35, lookup: 1.0 / iso_factor, luma: 1.0, gain: 1.0 }
    }

    /// Factor on the three channels of pixel `p` with detail value `detail` (quarter steps, see step 3).
    #[inline]
    pub fn factor(&self, p: &[f32], detail: f32) -> f32 {
        let d = detail * 0.25;
        let y = (0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])) as f32 * self.luma;
        if y <= DARK {
            return 1.0;
        }
        let brightness = f64::from(((p[1] * self.lookup + p[0] * self.lookup) + p[2] * self.lookup) * 0.33);
        let amount = self.amounts[curve::index(brightness)] * self.gain;
        let change = (d.abs() * amount).min(self.absolute).min(self.relative * y);
        (if d < 0.0 { -change } else { change }) / y.max(1.0) + 1.0
    }
}

/// Up to this luma a pixel is not sharpened.
const DARK: f32 = 48.0;

/// Sharpening with the detail signal of step 3 (`detail` per pixel).
pub fn sharpen(rgb: &mut [f32], detail: &[f32], step: usize, iso_factor: f32, sharpness: f32, pooled: bool) {
    let s = Sharpening::new(step, iso_factor, sharpness, pooled);
    rgb.par_chunks_mut(3).zip(detail.par_iter()).for_each(|(p, &d)| {
        let f = s.factor(p, d);
        for v in p {
            *v = sharpened(*v, f);
        }
    });
}

/// One channel times the sharpening factor: a result above the 16-bit range keeps the input.
#[inline]
fn sharpened(v: f32, f: f32) -> f32 {
    let out = v * f;
    let out = if out > 65534.0 { v.min(65534.0) } else { out };
    (out.max(0.0) + 0.5).min(65535.0).trunc()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_image_stays() {
        let (w, h) = (48, 40);
        let mut rgb = vec![5000f32; w * h * 3];
        local_contrast(&mut rgb, w, h, 3, 16.0, false);
        sharpen(&mut rgb, &vec![0.0; w * h], 3, 16.0, 0.0, false);
        assert!(rgb.iter().all(|&v| v == 5000.0));
    }

    #[test]
    fn contrast_change_is_limited() {
        // luma far above the mean: at most 20 units, at most 10 %
        assert!((contrast_factor(1000.0, 0.0, 1.0, 10.0) - 1025.0 / 1005.0).abs() < 1e-6);
        assert_eq!(contrast_factor(50.0, 5000.0, 1.0, 10.0), 0.9);
        // inside the dead band nothing happens
        assert_eq!(contrast_factor(1000.0, 995.0, 1.0, 10.0), 1.0);
    }

    #[test]
    fn sharpening_follows_the_detail_sign_and_spares_the_dark() {
        let s = Sharpening::new(3, 16.0, 0.0, false);
        let bright = [4000f32, 4000.0, 4000.0];
        assert!(s.factor(&bright, 400.0) > 1.0);
        assert!(s.factor(&bright, -400.0) < 1.0);
        assert_eq!(s.factor(&[40.0, 40.0, 40.0], 400.0), 1.0);
        // the change never exceeds 35 % of the luma
        assert!(s.factor(&bright, 1e6) <= 1.35 + 1e-6);
    }
}
