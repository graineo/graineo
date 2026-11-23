//! Classic chain, range-based reconstruction (from ISO 400 on): instead of the log-ratio
//! reconstruction of lower ISO, the layer image is reduced (by 2 or 4), filtered there with a range
//! filter guided by the colour noise model, two edge maps, a neutralisation of dark pixels and an
//! edge-guided local mean, and enlarged again. The fine structure comes back through a brightness
//! ratio: the brightness of the original layers, smoothed by two diffusion steps (one with the
//! intensity noise model, one with a fixed conduction) and freed from single bright peaks, divided
//! by the brightness of the enlarged image. The enlarged image minus the black level estimated from
//! the reduced image goes through the colour matrix (RIMM mapping of the camera matrix), the coring
//! of the colour step (tanh table on the matrix result and on its deviation from grey), and is
//! multiplied by a factor from that ratio.
//!
//! Parameters come from the camera data with fixed defaults. The two noise sliders (luminance and
//! colour, 0 … 1, 0.5 = neutral, set per run with [`with_noise_sliders`]) scale three of them, the
//! same way as the maker's software ([`slider_factor`]): colour scales the range filter's exponent
//! (and from 0.751 on the reduction is always 4), luminance scales the intensity noise model of the
//! diffusion and the strength of the brightness ratio. At the neutral position all factors are
//! exactly 1. Monochrome runs through the same path with its matrix. With flag 0x800000 of
//! `SkipStepFlags` (or both sliders at "Off") the noise reduction is skipped:
//! [`without_noise_reduction`] only subtracts the black level and applies the matrix.

use rayon::prelude::*;
use super::region_filter as filter;
use super::region_scaling::{self as scaling, Rgb};
use super::calibration::{self, cam2rimm, diag, inv, mm, mv, Data, Calibration, Mat, D65};
use super::tanh_table;

thread_local! {
    /// Noise slider positions (luminance, colour) of the chain run on this thread, 0 … 1
    /// (0.5 = neutral; negative = "Off").
    static SLIDERS: std::cell::Cell<(f64, f64)> = const { std::cell::Cell::new((0.5, 0.5)) };
}

/// Runs `f` (a chain run on this thread) with the noise sliders `luma` and `chroma` (0 … 1,
/// 0.5 = neutral, quarter steps; negative = "Off"). Outside such a call the sliders are neutral.
pub fn with_noise_sliders<R>(luma: f64, chroma: f64, f: impl FnOnce() -> R) -> R {
    let before = SLIDERS.replace((luma, chroma));
    let r = f();
    SLIDERS.set(before);
    r
}

/// Noise slider positions of the current run (luminance, colour).
pub fn noise_sliders() -> (f64, f64) {
    SLIDERS.get()
}

/// Both sliders at "Off": the step runs without noise reduction ([`without_noise_reduction`]).
pub fn sliders_off() -> bool {
    let (l, c) = SLIDERS.get();
    l < 0.0 && c < 0.0
}

/// Factor of a parameter for slider position `s` (0 … 1): `low` at 0, 1 at 0.5, `high` at 1,
/// linear in between (each half on its own), computed in the same mixed precision as the maker's
/// software; below 0 `low`, above 1 `high`. Exactly 1 at 0.5.
pub fn slider_factor(s: f64, low: f32, high: f32) -> f32 {
    if s < 0.0 {
        low
    } else if s < 0.5 {
        ((1.0 - 2.0 * s) as f32) * low + (2.0 * s) as f32
    } else if s <= 1.0 {
        let t = 2.0 * (s - 0.5);
        (1.0 - t) as f32 + (t as f32) * high
    } else {
        high
    }
}

/// Everything the step needs, derived from the calibration and the camera data.
#[derive(Clone, Debug)]
pub struct Parameter {
    /// reduction factor (1, 2 or 4)
    pub factor: usize,
    /// window size of the range filter (odd) and whether only every other position is used
    pub window: u32,
    pub subsample: bool,
    /// scale of the range filter's exponent
    pub noise_scale: f32,
    /// scale of the first edge map
    pub edges_scale: f32,
    /// decay and passes of the local mean
    pub mean_falloff: f32,
    pub mean_passes: u32,
    /// dark neutralisation limits
    pub dark_low: u32,
    pub dark_high: u32,
    /// fixed conduction coefficient and passes of the second diffusion
    pub conductance: i32,
    pub diffusion_passes: u32,
    /// passes of the diffusion with the intensity noise model
    pub noise_passes: u32,
    pub lambda: f32,
    /// offset of the brightness ratio and the strength of its effect
    pub epsilon: f32,
    pub rest_scale: f32,
    /// colour noise model: offsets, slopes, curvatures of the three layers
    pub color_model: [f64; 9],
    /// intensity noise model (offset, slope)
    pub bright_model: [f32; 2],
    /// layer gain before shading and temperature factors, limited ISO factor, digital ISO factor
    pub gain_pre: [f32; 3],
    pub iso_factor1: f32,
    pub digital: f32,
    /// coring amplitudes (before the digital factor)
    pub amp: [f64; 3],
    /// RIMM matrix applied to the enlarged image
    pub matrix: Mat,
}

/// The value as a 16-bit integer, `standard` when missing.
fn int16(d: &Data, name: &str, standard: i32) -> i32 {
    if d.present(name) { i32::from(d.number(name) as i32 as i16) } else { standard }
}

fn number(d: &Data, name: &str, standard: f32) -> f32 {
    if d.present(name) { d.number(name) } else { standard }
}

/// Daylight matrix of the plain path (cameras without stored calibration), used for the mixing at
/// high ISO with automatic or incandescent white balance.
pub(crate) fn daylight_simple(d: &Data, wb_name: &str) -> Result<Mat, String> {
    let light = d.list("WhiteBalanceIlluminants").ok_or("Bereich: Lichtarten fehlen")?;
    let corr = d.list("WhiteBalanceCorrections").ok_or("Bereich: Korrekturen fehlen")?;
    let m_light = d.matrix(&calibration::entry(light, wb_name));
    let mut n = calibration::entry(corr, "Daylight");
    if n.is_empty() {
        n = calibration::entry(corr, "Sunlight");
    }
    let m = mm(&d.matrix(&n), &m_light);
    let x = mv(&inv(&m), &D65);
    let mx = calibration::max3(&x);
    Ok(mm(&m, &diag(&[x[0] / mx, x[1] / mx, x[2] / mx])))
}

/// Parameters of the step for the given calibration; `Err` when the step does not cover the file.
pub fn parameter(d: &Data, k: &Calibration, mode: &str) -> Result<Parameter, String> {
    let iso = k.iso.iso;
    let tungsten = k.wb.code == 5;
    let high = tungsten || iso > 400.0;
    let (luma, chroma) = noise_sliders();
    // noise parameters (camera data, else fixed defaults); the colour slider far up always reduces by 4
    let mut factor = int16(d, "DownSampleFactor", if high { 4 } else { 2 });
    if chroma > 0.751 {
        factor = 4;
    }
    if !matches!(factor, 1 | 2 | 4) {
        return Err(format!("Bereich: Verkleinerung {factor} nicht vorgesehen"));
    }
    let window = (int16(d, "BilateralFilterRadius", 5) * 2 + 1) as u32;
    let subsample = int16(d, "SubSampleKernel", 1) != 0;
    let noise_scale = number(d, "NoiseScaling", 0.5) * slider_factor(chroma, number(d, "MinNoiseScalingFactor", 4.0), number(d, "MaxNoiseScalingFactor", 0.02));
    let edges_scale = number(d, "EdgeMapScale", 0.5);
    let mean_falloff = number(d, "LocalMeanDecay", 0.9);
    let mean_passes = int16(d, "LocalMeanIters", 1) as u32;
    let dark_low = int16(d, "DarkNeutralizeLow", 2) as u32;
    let dark_high = int16(d, "DarkNeutralizeHigh", 50) as u32;
    let conductance = int16(d, "ResidualAnisoDiffK2", 2);
    let diffusion_passes = int16(d, "ResidualAnisoDiffIters2", i32::from(high)) as u32;
    let scale_nm_standard = if iso == 400.0 {
        if tungsten { 0.75 } else { 0.25 }
    } else if iso == 800.0 && !tungsten {
        0.75
    } else {
        1.0
    };
    // (ISO 1600 and above keep the general default 1, as in the maker's software)
    let scale_nm = number(d, "ResidualAnisoDiffScaleNM", scale_nm_standard) * slider_factor(luma, number(d, "MinAnisoDiffScaleNMFactor", 0.5), number(d, "MaxAnisoDiffScaleNMFactor", 2.0));
    let lambda = number(d, "AnisoLambda", 0.25);
    let epsilon = number(d, "ResidualEpsilon", 5.0);
    let rest_scale = number(d, "ResidualScaling", 1.0) * slider_factor(luma, number(d, "MinResidualScalingFactor", 1.0), number(d, "MaxResidualScalingFactor", 0.75));
    // colour noise model, scaled to the layer codes of this exposure
    let mut fm: [f64; 9] = match d.values("ColorNoiseModel") {
        Some(v) if v.len() >= 9 => std::array::from_fn(|i| f64::from(v[i] as f32)),
        _ => [6.3861, 5.1706, 5.3139, 0.0046, 0.0044, 0.004, 0.0, 0.0, 0.0],
    };
    let f1 = k.iso.factor1;
    let sr = k.iso.sensor_rel;
    for c in 0..3 {
        fm[c] *= f64::from(f1 * f1);
        fm[3 + c] *= f64::from(f1);
    }
    for c in 0..3 {
        fm[c] *= f64::from(sr * sr);
        fm[3 + c] *= f64::from(sr);
    }
    if !tungsten {
        let g = k.amplification.gain;
        for c in 0..3 {
            fm[c] *= f64::from((g[c] * g[c]) as f32);
            fm[3 + c] *= f64::from(g[c] as f32);
        }
    }
    let hm = d.triple("IntensityNoiseModel", [3.0698 * 3.0, 0.0025 * 3.0, 0.0]);
    let bright_model = [(hm[0] * f64::from(scale_nm)) as f32, (hm[1] * f64::from(scale_nm)) as f32];
    // gain before shading and temperature factors (relative gain times aperture factor)
    let fnz = d.triple("FnumberGainFact", [1.0; 3]);
    let gain_pre: [f32; 3] = std::array::from_fn(|c| (k.amplification.relative[c] * fnz[c]) as f32);
    // coring amplitudes
    if !d.present("ColorDQCamRGB") {
        return Err("Bereich: ColorDQCamRGB fehlt".into());
    }
    let dq = d.triple("ColorDQCamRGB", [0.0; 3]);
    let g = k.amplification.gain;
    let mut amp: [f64; 3] = std::array::from_fn(|c| dq[c] * ((g[c] * f64::from(f1)) * k.statistics));
    if k.model.class == calibration::Camera::Dp2 {
        let iso100 = f64::from(sr * k.iso.factor);
        if iso100 > 20.0 {
            amp = amp.map(|a| a / 4.0);
        } else if iso100 > 10.0 {
            amp = amp.map(|a| a / 2.0);
        }
    }
    // matrix: camera matrix with the colour-mode matrix, mixed with the daylight matrix at high ISO
    // for automatic or incandescent white balance, then mapped to RIMM with the digital factor
    let mut m = k.color.m_single;
    if let Some(c) = super::color::mode_matrix(d, mode) {
        m = mm(&m, &c);
    }
    if k.iso.high && (k.wb.code == 1 || k.wb.code == 5) {
        let m2 = if k.color.cached {
            k.color.m2.ok_or("Bereich: Tageslichtmatrix fehlt")?
        } else {
            let mut name = k.wb.name.to_string();
            if matches!(k.model.class, calibration::Camera::Sd9 | calibration::Camera::Sd10 | calibration::Camera::Sd14) && name == "Daylight" {
                name = "Sunlight".into();
            }
            daylight_simple(d, &name)?
        };
        let w = f64::from(number(d, "MatrixMixing", 0.5));
        for i in 0..9 {
            m[i] = w * m[i] + (1.0 - w) * m2[i];
        }
    }
    if k.skip & 0x40000 != 0 {
        m = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    }
    let matrix = cam2rimm(&m, &[1.0; 3], f64::from(k.iso.digital), 0.0);
    Ok(Parameter {
        factor: factor as usize,
        window,
        subsample,
        noise_scale,
        edges_scale,
        mean_falloff,
        mean_passes,
        dark_low,
        dark_high,
        conductance,
        diffusion_passes,
        noise_passes: 1,
        lambda,
        epsilon,
        rest_scale,
        color_model: fm,
        bright_model,
        gain_pre,
        iso_factor1: f1,
        digital: k.iso.digital,
        amp,
        matrix,
    })
}

/// Black level of the reduced image: histogram of `B/2 + G + R` on every fourth pixel of the
/// central part, mean of the pixels below a low quantile, shrunk towards zero by a fixed count.
pub fn black_level(b: &Rgb, amp: [f64; 3]) -> [f64; 3] {
    let (cols, rows) = (b.w as u32, b.h as u32);
    let threshold = ((f64::from(cols - cols / 10) * f64::from(rows - rows / 10)) * 0.01 * 0.0625) as i64 as u32;
    if threshold < 10 {
        return [0.0; 3];
    }
    let sum = (amp[2] * 0.5 + amp[0]) + amp[1];
    let i1 = (5.0 * sum) as i32;
    let i5 = if i1 < 0xc9 { i1 } else { 200 };
    let bins = i5 * 2 - 1;
    let (y0, y1) = (rows / 20, rows - rows / 20);
    let (x0, x1) = (cols / 20, cols - cols / 20);
    let mut hist = [0u32; 400];
    let px = |x: u32, y: u32| b.px(x as usize, y as usize).map(f64::from);
    if y0 < y1 && x0 < x1 {
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let p = px(x, y);
                let s = (((p[2] * 0.5 + p[1]) + p[0]) + f64::from(i5)) + -1.0;
                let sf = s as f32;
                if sf < bins as f32 && 0.0 <= sf {
                    hist[s as i32 as usize] += 1;
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
    if !(y0 < y1 && x0 < x1) {
        return [0.0; 3];
    }
    let limit = ((((sum as i32) + 1) - i5) + last) as f32;
    let (mut sr, mut sg, mut sb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u32);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let p = px(x, y);
            let s = ((p[2] * 0.5 + p[0]) + p[1]) as f32;
            if s <= limit && ((-i5) as f32) < s {
                sr += p[0];
                sg += p[1];
                sb += p[2];
                n += 1;
            }
            x += 4;
        }
        y += 4;
    }
    if n > 200 {
        let denominator = f64::from(n + 400);
        [sr / denominator, sg / denominator, sb / denominator]
    } else {
        [0.0; 3]
    }
}

/// Runs the step on the layer image (`w × h × 3`, 16-bit values) and returns the working codes.
pub fn run(img: &[i32], w: usize, h: usize, p: &Parameter) -> Vec<i32> {
    let f = p.factor;
    let small = scaling::shrink(img, w, h, f);
    let (wk, hk) = (small.w, small.h);
    // black level of the reduced image, as the deviation from its grey
    let mut black = black_level(&small, p.amp);
    let gray = (((black[1] + black[1]) + black[0]) + black[2]) * 0.25;
    for v in &mut black {
        *v -= gray;
    }
    let tab = filter::exp_table(p.noise_scale);
    let mut bf = filter::region_filter(&small, p.window, &p.color_model, p.subsample, &tab);
    drop(small);
    let mut edge = filter::edge_map(&bf, p.edges_scale, &p.color_model);
    // brightness weights: reciprocal of the layer gains (with the limited ISO factor), / 3
    let gw: [f32; 3] = std::array::from_fn(|c| 1.0f32 / ((p.gain_pre[c] * p.iso_factor1) * 3.0f32));
    let edge2 = filter::edge_map2(&bf, gw);
    if f <= hk && f <= wk {
        for (k1, k2) in edge.iter_mut().zip(&edge2) {
            let e2 = f64::from(*k2);
            *k1 = (f64::from(*k2 * *k1) + (1.0 - e2) * e2) as f32;
        }
    }
    drop(edge2);
    filter::dark_neutral(&mut bf, p.dark_low, p.dark_high);
    filter::local_mean(&mut bf, &edge, p.mean_falloff, p.mean_passes);
    drop(edge);
    let large = scaling::enlarge(&bf, w, h, f);
    drop(bf);
    // brightness of the original layers
    let short = |v: i32| f32::from(v as i16);
    let bright: Vec<f32> = img.par_chunks_exact(3).map(|q| (short(q[0]) * gw[0] + short(q[1]) * gw[1]) + short(q[2]) * gw[2]).collect();
    // two diffusion steps and the removal of single peaks; the buffers keep earlier contents where
    // the steps do not write (needed for identical results)
    let smoothed = if p.diffusion_passes != 0 {
        let mut b = vec![0.0f32; w * h];
        filter::diffusion_noise_model(&mut b, &bright, w, h, p.noise_passes, p.bright_model, p.lambda);
        let mut a = bright;
        let b_in = b.clone();
        filter::diffusion_fixed(&mut a, &b_in, w, h, p.diffusion_passes, p.conductance, p.lambda);
        filter::peaks(&mut b, &a, w, h);
        b
    } else {
        let mut a = vec![0.0f32; w * h];
        filter::diffusion_noise_model(&mut a, &bright, w, h, p.noise_passes, p.bright_model, p.lambda);
        let mut b = bright;
        filter::peaks(&mut b, &a, w, h);
        b
    };
    // matrix, coring and the brightness ratio
    let amp_t: [f64; 3] = std::array::from_fn(|c| f64::from(p.digital.min(8.0)) * p.amp[c]);
    let lut = tanh_table::build(amp_t, [0.8; 3]);
    let lim = (((lut.n as i32) & !1).wrapping_mul(0x8000).wrapping_sub(0x10000)) >> 16;
    let kl = |v: i32| if v < -lim { -lim } else if v > lim { lim } else { v };
    let m = &p.matrix;
    let sw = [black[0] as f32, black[1] as f32, black[2] as f32];
    let mut codes = vec![0i32; w * h * 3];
    // pixel by pixel, independent: in parallel
    codes.par_chunks_mut(3).enumerate().for_each(|(i, code)| {
        let q = i * 3;
        let u = [large.d[q], large.d[q + 1], large.d[q + 2]];
        let mono = (u[0] * gw[0] + u[1] * gw[1]) + u[2] * gw[2];
        let mono = if mono > 0.0 { mono } else { 0.0 };
        let r = (smoothed[i] + p.epsilon) / (mono + p.epsilon);
        let v = [f64::from(u[0] - sw[0]), f64::from(u[1] - sw[1]), f64::from(u[2] - sw[2])];
        let s: [f64; 3] = std::array::from_fn(|c| (m[c * 3 + 1] * v[1] + m[c * 3] * v[0]) + m[c * 3 + 2] * v[2]);
        let o: [i32; 3] = std::array::from_fn(|c| s[c] as i32);
        let t1: [i32; 3] = std::array::from_fn(|c| lut.get(c, kl(o[c])));
        let a: [i32; 3] = std::array::from_fn(|c| o[c] - t1[c]);
        let gray = ((a[0] + a[1] * 2) + a[2]) >> 2;
        let t2: [i32; 3] = std::array::from_fn(|c| lut.get(c, kl(a[c] - gray)));
        // strength of the ratio: `r^s`, detail scaled in the log domain (Durand & Dorsey 2002);
        // s = 1 (neutral) is the ratio itself
        let rf = f64::from(r);
        let factor = if p.rest_scale == 1.0 { rf } else { rf.max(1e-6).powf(f64::from(p.rest_scale)) };
        for c in 0..3 {
            let x = (s[c] - f64::from(t1[c] + t2[c])) * factor;
            let x = if 24000.0 < x { 24000.0 } else { x };
            code[c] = i32::from((x + 0.5) as i32 as i16);
        }
    });
    codes
}

/// Black level of the 16-bit layer image (same estimate as the colour step, result not cut to
/// integers): histogram of `B/2 + G + R` on every fourth pixel of the central part, mean of the
/// pixels below a low quantile, shrunk towards zero by a fixed count.
pub fn black_level16(img: &[i32], w: usize, h: usize, amp: [f64; 3]) -> [f64; 3] {
    let (cols, rows) = (w as u32, h as u32);
    let threshold = ((f64::from(cols - cols / 10) * f64::from(rows - rows / 10)) * 0.005 * 0.0625) as i64 as u32;
    if threshold < 10 {
        return [0.0; 3];
    }
    let sum = (amp[2] * 0.5 + amp[0]) + amp[1];
    let i1 = (5.0 * sum) as i32;
    let i5 = if i1 < 0xc9 { i1 } else { 200 };
    let bins = i5 * 2 - 1;
    let (y0, y1) = (rows / 20, rows - rows / 20);
    let (x0, x1) = (cols / 20, cols - cols / 20);
    let short = |v: i32| i32::from(v as i16);
    let px = |x: u32, y: u32| {
        let i = (y as usize * w + x as usize) * 3;
        (short(img[i]), short(img[i + 1]), short(img[i + 2]))
    };
    let mut hist = [0u32; 400];
    let visited = y0 < y1 && x0 < x1;
    if visited {
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let (r, g, b) = px(x, y);
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
        return [0.0; 3];
    }
    let limit = (((sum as i32) + 1) - i5) + last;
    let (mut sr, mut sg, mut sb, mut n) = (0i32, 0i32, 0i32, 0u32);
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let (r, g, b) = px(x, y);
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
        [f64::from(sr) / denominator, f64::from(sg) / denominator, f64::from(sb) / denominator]
    } else {
        [0.0; 3]
    }
}

/// Variant without noise reduction (flag 0x800000, from ISO 400 on): black level of the image
/// (relative to its grey) subtracted, camera matrix (with colour mode, mapped to RIMM with the
/// digital factor) applied, `+0.5`, cut to an integer, kept as 16 bits.
pub fn without_noise_reduction(img: &[i32], d: &Data, k: &Calibration, mode: &str, w: usize, h: usize) -> Result<Vec<i32>, String> {
    if !d.present("ColorDQCamRGB") {
        return Err("Bereich: ColorDQCamRGB fehlt".into());
    }
    let dq = d.triple("ColorDQCamRGB", [0.0; 3]);
    let g = k.amplification.gain;
    let f1 = f64::from(k.iso.factor1);
    let mut amp: [f64; 3] = std::array::from_fn(|c| dq[c] * ((g[c] * f1) * k.statistics));
    if k.model.class == calibration::Camera::Dp2 {
        let iso100 = f64::from(k.iso.sensor_rel * k.iso.factor);
        if iso100 > 20.0 {
            amp = amp.map(|a| a / 4.0);
        } else if iso100 > 10.0 {
            amp = amp.map(|a| a / 2.0);
        }
    }
    let mut b = black_level16(img, w, h, amp);
    let gray = ((b[0] + (b[1] + b[1])) + b[2]) / 4.0;
    for v in &mut b {
        *v -= gray;
    }
    let mut m = k.color.m_single;
    if let Some(c) = super::color::mode_matrix(d, mode) {
        m = mm(&m, &c);
    }
    let m = cam2rimm(&m, &[1.0; 3], f64::from(k.iso.digital), 0.0);
    let mut outp = vec![0i32; img.len()];
    for (q, o) in img.chunks_exact(3).zip(outp.chunks_exact_mut(3)) {
        let v: [f64; 3] = std::array::from_fn(|c| f64::from(f32::from(q[c] as i16)) - b[c]);
        for c in 0..3 {
            let s = (m[c * 3] * v[0] + m[c * 3 + 1] * v[1]) + m[c * 3 + 2] * v[2];
            o[c] = i32::from((s + 0.5) as i32 as i16);
        }
    }
    Ok(outp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slider factor: exactly 1 in the middle, the limits at 0 and 1, linear in each half, held
    /// outside 0 … 1.
    #[test]
    fn slider_factor_neutral_and_limits() {
        for (lo, hi) in [(4.0f32, 0.02f32), (0.5, 2.0), (1.0, 0.75)] {
            assert_eq!(slider_factor(0.5, lo, hi), 1.0);
            assert_eq!(slider_factor(0.0, lo, hi), lo);
            assert_eq!(slider_factor(1.0, lo, hi), hi);
            assert_eq!(slider_factor(-0.25, lo, hi), lo);
            assert!((slider_factor(0.25, lo, hi) - (lo + 1.0) / 2.0).abs() < 1e-6);
            assert!((slider_factor(0.75, lo, hi) - (hi + 1.0) / 2.0).abs() < 1e-6);
        }
        // the sliders are neutral outside a run with them, and restored afterwards
        assert_eq!(noise_sliders(), (0.5, 0.5));
        assert_eq!(with_noise_sliders(1.0, 0.0, noise_sliders), (1.0, 0.0));
        assert_eq!(noise_sliders(), (0.5, 0.5));
        assert!(with_noise_sliders(-0.25, -0.25, sliders_off) && !with_noise_sliders(-0.25, 0.5, sliders_off));
    }

    /// Prints the parameters of the step for one file (`GRAINEO_CLASSIC_REGION_FILE=<x3f>`).
    #[test]
    #[ignore]
    fn parameter_output() {
        let Ok(f) = std::env::var("GRAINEO_CLASSIC_REGION_FILE") else { return };
        let x = x3f::X3fFile::open(&f).unwrap();
        let (camf, props) = (x.camf().unwrap(), x.properties().unwrap());
        let wb = props.get("WB_DESC").unwrap_or("Auto").trim().to_string();
        let adobe = x.exif().and_then(|e| e.color_space) == Some(0xFFFF);
        let k = calibration::calc(&calibration::Input { camf: &camf, props: &props, wb: &wb, adobe, auto_neutral: None, gray_mean: None }).unwrap();
        println!("{:#?}", parameter(&Data { camf: &camf }, &k, "Standard"));
    }
}
