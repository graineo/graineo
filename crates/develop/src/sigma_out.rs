//! Sigma's Merrill output stage: ColorDQ, tone curve over sensor counts, gamma.
//!
//! Taken over and adapted from x3fuse-core `crates/x3f-sys/src/merrill.rs`
//! (Apache-2.0, © 2026 Sang Lee; see third_party/x3fuse-core/LICENSE and NOTICE).
//! Matched there against output of SIGMA Photo Pro (ColorDQ, tone and gamma curves).
//! Changes: no cancellation control, rewritten comments; Quattro tone curve (`tone_lut_quattro`)
//! extended with shadow/highlight sliders.

const LUT_SIZE: usize = 65536;

/// ColorDQ (as in SPP): gentle non-linear correction
/// of the channel deviations from the weighted grey component, right after the colour matrix.
#[derive(Clone, Debug)]
pub struct ColorDq {
    tables: [Vec<i16>; 3],
    mask: usize,
}

impl ColorDq {
    #[allow(clippy::approx_constant)] // Sigma's literals are not exactly π or 2π.
    pub fn new(strengths: [f64; 3]) -> Option<Self> {
        let ratios = strengths.map(|v| v / 0.8);
        if !ratios.iter().all(|v| v.is_finite() && *v > 0.0) {
            return None;
        }
        let step = (0.5 / ratios.into_iter().fold(0.0_f64, f64::max)) * 0.5;
        if !step.is_finite() || step <= 0.0 {
            return None;
        }
        let requested = (6.2853 / step) as i32;
        let size = match requested {
            ..=512 => 512,
            ..=1024 => 1024,
            ..=2048 => 2048,
            ..=4096 => 4096,
            _ => 8192,
        };
        let mut tables = std::array::from_fn(|_| vec![0_i16; size]);
        for i in 0..size / 2 {
            let phase = step * i as f64;
            let window = if phase < 3.1416 { (phase.cos() + 1.0) * 0.5 } else { 0.0 };
            for (table, strength) in tables.iter_mut().zip(strengths) {
                table[i] = ((strength * window).mul_add((0.8 * i as f64 / strength).tanh(), 0.5) as i32) as i16;
            }
        }
        for table in &mut tables {
            for i in 1..size / 2 {
                table[size - i] = table[i].wrapping_neg();
            }
            table[size / 2] = table[size / 2 + 1];
        }
        Some(Self { tables, mask: size - 1 })
    }

    #[inline]
    pub fn apply(&self, q: [f32; 3]) -> [f32; 3] {
        let residual: [i32; 3] = std::array::from_fn(|c| {
            // rint via the 1.5·2⁵² trick as in the original
            let i = ((q[c] as f64 + 1.5e-8) + 6755399441055744.0).to_bits() as i32;
            i.wrapping_sub(self.tables[c][i as usize & self.mask] as i32)
        });
        let mean = residual[0].wrapping_add(residual[1].wrapping_shl(1)).wrapping_add(residual[2]) >> 2;
        std::array::from_fn(|c| {
            (q[c] as f64 - self.tables[c][residual[c].wrapping_sub(mean) as usize & self.mask] as f64) as f32
        })
    }
}

/// Parameters of a `TCColorModeSettings` row.
#[derive(Clone, Copy, Debug)]
pub struct ToneShape {
    pub start: f64,
    pub end: f64,
    pub steepness: f64,
    pub steepness2: f64,
    pub breakpoint: f64,
    pub scale: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct ToneSettings {
    /// psat = 2^ImageDepth − 1 (Merrill: 4095), in gained counts
    pub pixel_saturation: f64,
    pub exposure: f64,
    pub shadows: f64,
    /// incl. contrast offset of the colour mode (colour only)
    pub contrast: f64,
    pub highlights: f64,
    /// Monochrome curve
    pub mono: bool,
    /// Quattro: own curve shape (gamma 1/2.2222, combination on a quarter grid) and
    /// counts up to 65535
    pub quattro: bool,
}

/// F20 tone curve: 65536 LUT from count → linear light 0…65535.
pub fn tone_lut(p: ToneShape, s: ToneSettings) -> Option<Vec<u16>> {
    if s.quattro && !s.mono {
        return tone_lut_quattro(p, s);
    }
    let max = if s.quattro { 65535.0 } else { 16384.0 };
    let saturation = (s.pixel_saturation * (-s.exposure).exp2()).clamp(1.0, max);
    let extent = saturation.max(4095.0);
    let n = extent as usize;
    let (mut start, mut end, mut k1, mut k2) =
        if s.mono { (-1.17, 1.55, 3.25, 2.46) } else { (p.start, p.end, p.steepness, p.steepness2) };
    if start >= end || k1 <= 0.0 || k2 <= 0.0 {
        return None;
    }
    let contrast_scale = (0.5 * s.contrast.clamp(-2.0, 2.0)).exp2();
    k1 *= contrast_scale;
    k2 *= contrast_scale;
    let shadow = (-s.shadows).clamp(-2.0, 2.0);
    let mut highlight = s.highlights.clamp(-2.0, 2.0);
    if !s.mono {
        start *= (shadow / 8.0).exp2();
        end *= (highlight / 8.0).exp2();
    }
    let inv_gamma = 1.0 / if s.mono { 2.4 } else { 2.2 };
    let domain = |x: f64| x.powf(inv_gamma).mul_add(end - start, start);
    let sigmoid = |x: f64, k: f64| 1.0 / (1.0 + (-k * x).exp());
    let mut curve = vec![0.0_f32; n + 1];
    if p.breakpoint < 1.0 {
        let mut i = 0;
        let mut x = 0.0;
        let step = 1.0 / saturation;
        loop {
            curve[i] = sigmoid(domain(x), k1) as f32;
            x += step;
            if x >= p.breakpoint || i >= n {
                break;
            }
            i += 1;
        }
        let at_break = domain(x);
        let correction = sigmoid(at_break, k1) - sigmoid(at_break, k2);
        for sample in &mut curve[i + 1..] {
            *sample = (sigmoid(domain(x), k2) + correction) as f32;
            x += step;
        }
    } else {
        let norm = sigmoid(domain(extent / saturation), k1);
        for (i, sample) in curve.iter_mut().enumerate() {
            *sample = (sigmoid(domain(i as f64 / saturation), k1) / norm) as f32;
        }
    }
    if s.mono && shadow != 0.0 {
        let threshold = curve[n] * 0.25;
        let stop = curve.iter().position(|v| *v >= threshold)?;
        let base = curve[0];
        let range = curve[stop] - base;
        if range > 0.0 {
            let exponent = (2.0 * shadow).exp2() as f32;
            for sample in &mut curve[..=stop] {
                *sample = ((*sample - base) / range).powf(exponent).mul_add(range, base);
            }
        }
    }
    if s.mono && highlight != 0.0 {
        let threshold = curve[n] * 0.75;
        let begin = curve.iter().rposition(|v| *v <= threshold)?;
        let base = curve[begin];
        let range = curve[n] - base;
        if range > 0.0 {
            if highlight < 0.0 {
                highlight *= 0.25;
            }
            let exponent = (-2.0 * highlight).exp2() as f32;
            for sample in &mut curve[begin..] {
                *sample = ((*sample - base) / range).powf(exponent).mul_add(range, base);
            }
        }
    }

    // De Casteljau smoothing in f32 over every 50th sample plus end value (as in the original).
    let mut controls: Vec<f32> = (0..=(extent / 50.0) as usize).map(|i| curve[i * 50]).collect();
    controls.push(curve[n]);
    let reciprocal = 1.0_f32 / extent as f32;
    let smoothed: Vec<f32> = {
        use rayon::prelude::*;
        (0..=n)
            .into_par_iter()
            .map_init(
                || controls.clone(),
                |work, i| {
                    work.copy_from_slice(&controls);
                    let t = i as f32 * reciprocal;
                    let omt = 1.0 - t;
                    for level in 1..work.len() {
                        for j in (level..work.len()).rev() {
                            work[j] = omt.mul_add(work[j - 1], t * work[j]);
                        }
                    }
                    work[work.len() - 1]
                },
            )
            .collect()
    };
    let mut curve = smoothed;

    let lo = curve[0] as f64;
    let range = curve[n] as f64 - lo;
    let at_sat = (curve[saturation as usize] as f64 - lo) / range;
    let normalization = saturation / at_sat;
    if !normalization.is_finite() || range <= 0.0 || at_sat <= 0.0 {
        return None;
    }
    for sample in &mut curve {
        let value = (*sample as f64 - lo) / range;
        let linear = if value < 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) };
        *sample = (normalization * linear) as f32;
    }
    let scale = p.scale * 65535.0 / curve[n] as f64;
    if !scale.is_finite() {
        return None;
    }
    let mut lut = vec![0u16; LUT_SIZE];
    for i in 0..n {
        lut[i] = (curve[i] as f64).mul_add(scale, 0.5).clamp(0.0, 65535.0) as u16;
    }
    // The original normalises with sample n but repeats n−1.
    let tail = lut[n - 1];
    lut[n..].fill(tail);
    Some(lut)
}

/// Quattro tone curve (as in SPP for Quattro; after x3fuse-core
/// `generate_quattro_tone_curve`, there with alpha 1).
/// Differences from Merrill: domain gamma 1/2.2222, no rows from `TCColorModeSettings`,
/// smoothing on a quarter grid. Our extension: shadows/highlights shift the start/end of the
/// sigmoid as for Merrill (SPP's Quattro sliders are not reproduced).
pub fn tone_lut_quattro(p: ToneShape, s: ToneSettings) -> Option<Vec<u16>> {
    let saturation = (s.pixel_saturation * (-s.exposure).exp2()).clamp(16.0, 65535.0);
    if p.start >= p.end || p.steepness <= 0.0 || p.steepness2 <= 0.0 || p.scale <= 0.0 {
        return None;
    }
    let n = saturation as usize;
    let contrast = (0.5 * s.contrast.clamp(-2.0, 2.0)).exp2();
    let (k1, k2) = (p.steepness * contrast, p.steepness2 * contrast);
    let shadow = (-s.shadows).clamp(-2.0, 2.0);
    let highlight = s.highlights.clamp(-2.0, 2.0);
    let (start, end) = (p.start * (shadow / 8.0).exp2(), p.end * (highlight / 8.0).exp2());
    let domain = |x: f64| x.powf(1.0 / 2.2222222222222223).mul_add(end - start, start);
    let sigmoid = |x: f64, k: f64| 1.0 / (1.0 + (-k * x).exp());
    let mut curve = vec![0.0_f32; n + 1];
    let mut x = 0.0;
    let step = 1.0 / saturation;
    let mut i = 0;
    loop {
        curve[i] = sigmoid(domain(x), k1) as f32;
        x += step;
        if x >= p.breakpoint || i >= n {
            break;
        }
        i += 1;
    }
    let at_break = domain(x);
    let correction = sigmoid(at_break, k1) - sigmoid(at_break, k2);
    for sample in &mut curve[i + 1..] {
        *sample = (sigmoid(domain(x), k2) + correction) as f32;
        x += step;
    }
    // Combination: one sample per four counts, De Casteljau smoothing, linear in between.
    let quarter_extent = saturation * 0.25;
    let quarter_n = n / 4;
    let mut controls: Vec<f32> = (0..=(saturation / 50.0 * 0.25) as usize).map(|i| curve[(i * 200).min(n)]).collect();
    controls.push(curve[quarter_n * 4]);
    let reciprocal = 1.0_f32 / quarter_extent as f32;
    let quarter: Vec<f32> = {
        use rayon::prelude::*;
        (0..=quarter_n)
            .into_par_iter()
            .map_init(
                || controls.clone(),
                |work, i| {
                    work.copy_from_slice(&controls);
                    let t = i as f32 * reciprocal;
                    let omt = 1.0 - t;
                    for level in 1..work.len() {
                        for j in (level..work.len()).rev() {
                            work[j] = omt.mul_add(work[j - 1], t * work[j]);
                        }
                    }
                    work[work.len() - 1]
                },
            )
            .collect()
    };
    curve[0] = quarter[0];
    for (i, pair) in quarter.windows(2).enumerate() {
        let base = curve[i * 4];
        let delta = (pair[1] - pair[0]) * 0.25;
        curve[i * 4 + 1] = base + delta;
        curve[i * 4 + 2] = delta.mul_add(2.0, base);
        curve[i * 4 + 3] = delta.mul_add(3.0, base);
        curve[i * 4 + 4] = pair[1];
    }
    curve[quarter_n * 4 + 1..].fill(quarter[quarter_n]);
    let lo = f64::from(curve[0]);
    let range = f64::from(curve[n]) - lo;
    let at_sat = (f64::from(curve[saturation as usize]) - lo) / range;
    let normalization = saturation / at_sat;
    if !normalization.is_finite() || range <= 0.0 || at_sat <= 0.0 {
        return None;
    }
    for sample in &mut curve {
        let v = (f64::from(*sample) - lo) / range;
        let linear = if v < 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        *sample = (normalization * linear) as f32;
    }
    let scale = p.scale * 65535.0 / f64::from(curve[n]);
    if !scale.is_finite() {
        return None;
    }
    let mut lut = vec![0u16; LUT_SIZE];
    for i in 0..n.min(LUT_SIZE) {
        lut[i] = (curve[i] as f64).mul_add(scale, 0.5).clamp(0.0, 65535.0) as u16;
    }
    if n < LUT_SIZE {
        let tail = lut[n - 1];
        lut[n..].fill(tail);
    }
    Some(lut)
}

/// sRGB gamma as a 65536 table (Sigma's rounded integer LUT).
pub fn srgb_gamma_lut() -> Vec<u16> {
    (0..LUT_SIZE)
        .map(|i| {
            let value = i as f64 / LUT_SIZE as f64;
            let encoded = if value < 0.0031308 { 12.92 * value } else { 1.055f64.mul_add(value.powf(1.0 / 2.4), -0.055) };
            encoded.mul_add(65535.0, 0.5) as u16
        })
        .collect()
}

/// Table index as in SPP: +0.5, truncate, lower 16 bits.
#[inline]
pub fn tone_index(v: f32) -> usize {
    ((v as f64 + 0.5) as i64 as u16) as usize
}
