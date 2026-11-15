//! Classic chain, step "colour": parameters and the black level estimate. The step itself (coring
//! of each layer and of its deviation from the grey of the three layers, then the colour matrix)
//! runs after dcraw in [`super::dcraw::color`].
//!
//! The black level is estimated from the image itself: a histogram of the (weighted) sum of the
//! layers on every fourth pixel of the central part; the pixels below a quantile of the histogram
//! give the mean level of each layer (shrunk towards zero by a fixed count).
//!
//! This is the low-ISO variant. When the log-ratio reconstruction follows, it hands the matrix
//! result and the cored layers on, and the matrix is scaled so that the green row sums to one (the
//! scale is handed on as well). When that reconstruction is switched off (camera data, flag) or the
//! matrix is monochrome, the matrix takes the digital ISO factor instead and is not rescaled; the
//! row computation is the same. The variant for the range-based reconstruction (high ISO) is not
//! covered here.

use super::calibration::{mm, Data, Calibration, Mat};

/// Result of the step.
pub struct ColorResult {
    /// matrix result (codes of the working colour space), `w × h × 3`
    pub codes: Vec<i32>,
    /// the cored layers
    pub layers: Vec<i32>,
    /// the scale that was applied so that the green row sums to one (as handed on)
    pub green_sum: f32,
}

/// Parameters derived from the calibration and the camera data.
pub struct Parameter {
    pub matrix: Mat,
    pub shift: u32,
    pub amp: [f64; 3],
    pub green_sum: f32,
    /// the log-ratio reconstruction follows (otherwise the colour result is final)
    pub edge: bool,
}

/// The log-ratio reconstruction is switched off: by the camera data (`FIR_DisableStep`), by flag
/// 0x400000 of `SkipStepFlags`, or because the matrix is monochrome.
pub fn without_edge(d: &Data, k: &Calibration) -> bool {
    d.integer("FIR_DisableStep") & 0xffff != 0 || k.skip & 0x400000 != 0 || k.color.degenerate
}

/// Matrix of the colour mode (a 3 × 3 matrix named in a list of the camera data), or `None`.
pub(crate) fn mode_matrix(d: &Data, mode: &str) -> Option<Mat> {
    let list = d.camf.properties("ColorModeCompensations").ok()?;
    let name = list.iter().find(|(k, _)| k.trim().eq_ignore_ascii_case(mode.trim())).map(|(_, v)| v.trim().to_string())?;
    let c = d.matrix(&name);
    (c[0] != 0.0).then_some(c)
}

pub fn parameter(d: &Data, k: &Calibration, mode: &str) -> Result<Parameter, String> {
    let mut m = k.color.m;
    if let Some(c) = mode_matrix(d, mode) {
        m = mm(&m, &c);
    }
    let digital = k.iso.digital;
    let edge = !without_edge(d, k);
    let (m, sum) = if edge {
        let mut m = super::calibration::cam2rimm(&m, &[1.0; 3], 1.0, 0.0);
        let sum = ((m[3] + m[4]) + m[5]) as f32;
        let scale = f64::from(1.0f32 / sum);
        for v in &mut m {
            *v *= scale;
        }
        (m, sum)
    } else {
        (super::calibration::cam2rimm(&m, &[1.0; 3], f64::from(digital), 0.0), 1.0)
    };
    let shift = if digital < 2.0 {
        14
    } else if digital > 4.0 {
        12
    } else {
        13
    };
    // amplitude of the coring
    let mut dq = d.triple("ColorDQCamRGB", [0.0; 3]);
    if dq == [0.0; 3] {
        let divisor = f64::from(k.iso.factor).min(8.0).sqrt().sqrt();
        dq = [4.0 / divisor; 3];
    }
    let bulb = k.iso.bulb_factor;
    let f1 = f64::from(k.iso.factor1);
    let g = k.amplification.gain;
    let mut amp = [0.0; 3];
    for c in 0..3 {
        amp[c] = dq[c] * bulb * ((g[c] * f1) * k.statistics);
    }
    // flag 0x20000: no coring (amplitude zero, the table is all zeros)
    if k.skip & 0x20000 != 0 {
        amp = [0.0; 3];
    }
    Ok(Parameter { matrix: m, shift, amp, green_sum: sum, edge })
}

#[inline]
fn short(v: i32) -> i32 {
    i32::from(v as i16)
}

/// Black level estimate (three layers) from the central part of the image.
pub fn black_level(img: &[i32], w: usize, h: usize, amp: [f64; 3]) -> [i32; 3] {
    black_level_with(img, w, h, amp, [0; 3])
}

/// Like [`black_level`], relative to a given level `off` per layer (subtracted from the codes).
pub fn black_level_with(img: &[i32], w: usize, h: usize, amp: [f64; 3], off: [i32; 3]) -> [i32; 3] {
    let (cols, rows) = (w as u32, h as u32);
    let threshold = (f64::from(cols - cols / 10) * f64::from(rows - rows / 10) * 0.005 * 0.0625) as i64 as u32;
    if threshold < 10 {
        return [0; 3];
    }
    let sum = amp[2] * 0.5 + amp[0] + amp[1];
    let i1 = (5.0 * sum) as i32;
    let i5 = i1.min(200);
    let bins = i5 * 2 - 1;
    let (y0, y1) = ((rows / 20) as usize, (rows - rows / 20) as usize);
    let (x0, x1) = ((cols / 20) as usize, (cols - cols / 20) as usize);
    let mut hist = [0u32; 400];
    let pix = |x: usize, y: usize| -> (i32, i32, i32) {
        let p = (y * w + x) * 3;
        (short(img[p]) - off[0], short(img[p + 1]) - off[1], short(img[p + 2]) - off[2])
    };
    let visited = y0 < y1 && x0 < x1;
    if visited {
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let (r, g, b) = pix(x, y);
                let i2 = (b >> 1) + g + r + i5;
                if i2 < i5 * 2 && i2 > 0 {
                    hist[(i2 - 1) as usize] += 1;
                }
                x += 4;
            }
            y += 4;
        }
    }
    let mut last = 0i32;
    if bins > 1 {
        let mut i = 1usize;
        while (i as i32) < bins {
            hist[0] += hist[i];
            hist[i] = hist[0];
            if hist[0] < threshold {
                last = i as i32;
            }
            hist[0] += hist[i + 1];
            hist[i + 1] = hist[0];
            if hist[0] < threshold {
                last = i as i32 + 1;
            }
            i += 2;
        }
    }
    if !visited {
        return [0; 3];
    }
    let limit = ((sum as i32) + 1 - i5) + last;
    let (mut sr, mut sg, mut sb, mut n) = (0i32, 0i32, 0i32, 0u32);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let (r, g, b) = pix(x, y);
            let s = (b >> 1) + g + r;
            if s <= limit && -i5 < s {
                sr += r;
                sg += g;
                sb += b;
                n += 1;
            }
            x += 4;
        }
        y += 4;
    }
    if n > 200 {
        let denominator = f64::from(n + 200);
        [(f64::from(sr) / denominator) as i32, (f64::from(sg) / denominator) as i32, (f64::from(sb) / denominator) as i32]
    } else {
        [0; 3]
    }
}
