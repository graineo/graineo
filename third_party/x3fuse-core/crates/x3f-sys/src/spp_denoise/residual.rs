//! Bounded residual and diffusion kernels from the Merrill SPP pipeline.

use crate::{Control, Error, Result};

const PIXEL_MAX: f32 = f32::from_bits(0x46ff_fc00);
const SHARP_L_MAX: f32 = f32::from_bits(0x46ff_fe00);

fn lower_bound_zero(value: f64) -> f64 {
    if value < 0.0 {
        0.0
    } else {
        value
    }
}

fn upper_bound_pixel(value: f64) -> f32 {
    (if value > f64::from(PIXEL_MAX) {
        f64::from(PIXEL_MAX)
    } else {
        value
    }) as f32
}

fn required_len(size: [usize; 2], stride: usize, channels: usize) -> Option<usize> {
    let [cols, rows] = size;
    if cols == 0 || rows == 0 || stride < cols.checked_mul(channels)? {
        return None;
    }
    (rows - 1)
        .checked_mul(stride)?
        .checked_add(cols.checked_mul(channels)?)
}

/// graineo: log-domain scaling of a ratio: `r^amount` for `r > 0` (continuous, `1` stays `1`). Base and detail are
/// separated in the logarithm as in Durand & Dorsey, "Fast bilateral filtering for the display of
/// high-dynamic-range images" (2002); the detail layer is scaled there. The amounts used are never negative.
/// Non-positive or non-finite ratios are passed through unchanged.
#[inline]
pub(crate) fn power_tone(value: f32, amount: f64) -> f32 {
    let v = f64::from(value);
    if !(v > 0.0 && v.is_finite()) {
        return value;
    }
    (amount * v.ln()).exp() as f32
}

fn check_layout(data_len: usize, size: [usize; 2], stride: usize, channels: usize) -> Result<()> {
    if required_len(size, stride, channels).is_none_or(|needed| data_len < needed) {
        return Err(Error::InvalidData("invalid SPP residual image layout"));
    }
    Ok(())
}

/// Mix a color residual into a mono base and write RGB scale factors (graineo: the detail ratio is scaled in the
/// log domain, [`power_tone`]).
#[allow(clippy::too_many_arguments)]
pub fn color_residual_mixing(
    output: &mut [f32],
    output_stride: usize,
    mono: &[f32],
    mono_stride: usize,
    reference: &[f32],
    reference_stride: usize,
    filtered: &[f32],
    filtered_stride: usize,
    size: [usize; 2],
    residual_mixing: f64,
    color_mixing: f64,
    tone_amount: f64,
    channel_weights: [f32; 3],
    epsilon: f32,
) -> Result<()> {
    let tone = power_tone;
    check_layout(output.len(), size, output_stride, 3)?;
    check_layout(mono.len(), size, mono_stride, 1)?;
    check_layout(reference.len(), size, reference_stride, 3)?;
    check_layout(filtered.len(), size, filtered_stride, 3)?;
    if !residual_mixing.is_finite()
        || !color_mixing.is_finite()
        || !tone_amount.is_finite()
        || !epsilon.is_finite()
        || !channel_weights.iter().all(|value| value.is_finite())
    {
        return Err(Error::InvalidData("invalid SPP color residual parameters"));
    }

    for y in 0..size[1] {
        for x in 0..size[0] {
            let oi = y * output_stride + x * 3;
            let mi = y * mono_stride + x;
            let ri = y * reference_stride + x * 3;
            let fi = y * filtered_stride + x * 3;
            let mut source = [0.0_f32; 3];
            let mut smooth = [0.0_f32; 3];
            for c in 0..3 {
                source[c] = reference[ri + c] * channel_weights[c];
                smooth[c] = (filtered[fi + c] * channel_weights[c]).max(0.0);
            }
            let base = if residual_mixing < 1.0 {
                let numerator = smooth[0] + smooth[1] + smooth[2] + epsilon;
                let denominator = (source[0] + source[1] + source[2]).max(0.0) + epsilon;
                residual_mixing.mul_add(
                    f64::from(mono[mi]),
                    (1.0 - residual_mixing) * f64::from(numerator / denominator),
                ) as f32
            } else {
                mono[mi]
            };
            for c in 0..3 {
                let ratio = (smooth[c] * 3.0 + epsilon) / (source[c] * 3.0 + epsilon);
                let base = ((1.0 - color_mixing) * f64::from(base)) as f32;
                let residual = (color_mixing * f64::from(ratio)) as f32;
                let value = base + residual;
                output[oi + c] = tone(value, tone_amount);
            }
        }
    }
    Ok(())
}

/// Mix an RGB residual into a mono scale map (graineo: the detail ratio is scaled in the log domain,
/// [`power_tone`]).
#[allow(clippy::too_many_arguments)]
pub fn mono_residual_mixing(
    output: &mut [f32],
    output_stride: usize,
    reference: &[f32],
    reference_stride: usize,
    filtered: &[f32],
    filtered_stride: usize,
    size: [usize; 2],
    residual_mixing: f64,
    tone_amount: f64,
    channel_weights: [f32; 3],
    epsilon: f32,
) -> Result<()> {
    let tone = power_tone;
    check_layout(output.len(), size, output_stride, 1)?;
    check_layout(reference.len(), size, reference_stride, 3)?;
    check_layout(filtered.len(), size, filtered_stride, 3)?;
    if !residual_mixing.is_finite()
        || !tone_amount.is_finite()
        || !epsilon.is_finite()
        || !channel_weights.iter().all(|value| value.is_finite())
    {
        return Err(Error::InvalidData("invalid SPP mono residual parameters"));
    }

    let rows = size[1] & !1;
    if residual_mixing < 1.0 {
        for y in 0..rows {
            for x in 0..size[0] {
                let oi = y * output_stride + x;
                let ri = y * reference_stride + x * 3;
                let fi = y * filtered_stride + x * 3;
                let mut denominator = 0.0_f64;
                let mut numerator = 0.0_f32;
                for c in 0..3 {
                    denominator += f64::from(reference[ri + c] * channel_weights[c]);
                    numerator += (filtered[fi + c] * channel_weights[c]).max(0.0);
                }
                let ratio = (f64::from(numerator + epsilon)
                    / (denominator.max(0.0) + f64::from(epsilon)))
                    as f32;
                let mixed = residual_mixing.mul_add(
                    f64::from(output[oi]),
                    (1.0 - residual_mixing) * f64::from(ratio),
                ) as f32;
                output[oi] = tone(mixed, tone_amount);
            }
        }
    } else {
        for y in 0..rows {
            for x in 0..size[0] {
                let i = y * output_stride + x;
                output[i] = tone(output[i], tone_amount);
            }
        }
    }
    Ok(())
}

/// Apply per-channel residual scale factors and copy the bounded mono map.
#[allow(clippy::too_many_arguments)]
pub fn apply_color_residual(
    output: &mut [f32],
    output_stride: usize,
    scale: &[f32],
    scale_stride: usize,
    input: &[f32],
    input_stride: usize,
    mono: &[f32],
    mono_stride: usize,
    mono_output: &mut [f32],
    mono_output_stride: usize,
    size: [usize; 2],
    _strength: f32,
) -> Result<()> {
    check_layout(output.len(), size, output_stride, 3)?;
    check_layout(scale.len(), size, scale_stride, 3)?;
    check_layout(input.len(), size, input_stride, 3)?;
    check_layout(mono.len(), size, mono_stride, 1)?;
    check_layout(mono_output.len(), size, mono_output_stride, 1)?;
    if !_strength.is_finite() {
        return Err(Error::InvalidData("invalid SPP residual strength"));
    }
    for y in 0..size[1] {
        for x in 0..size[0] {
            let oi = y * output_stride + x * 3;
            let si = y * scale_stride + x * 3;
            let ii = y * input_stride + x * 3;
            for c in 0..3 {
                let value = lower_bound_zero(f64::from(input[ii + c])) * f64::from(scale[si + c]);
                output[oi + c] = upper_bound_pixel(lower_bound_zero(value));
            }
            let value = mono[y * mono_stride + x];
            mono_output[y * mono_output_stride + x] =
                if value > PIXEL_MAX { PIXEL_MAX } else { value };
        }
    }
    Ok(())
}

/// Apply a mono residual scale to RGB input and copy the bounded mono map.
#[allow(clippy::too_many_arguments)]
pub fn apply_mono_residual(
    output: &mut [f32],
    output_stride: usize,
    scale: &[f32],
    scale_stride: usize,
    input: &[f32],
    input_stride: usize,
    mono: &[f32],
    mono_stride: usize,
    mono_output: &mut [f32],
    mono_output_stride: usize,
    size: [usize; 2],
    _strength: f32,
) -> Result<()> {
    check_layout(output.len(), size, output_stride, 3)?;
    check_layout(scale.len(), size, scale_stride, 1)?;
    check_layout(input.len(), size, input_stride, 3)?;
    check_layout(mono.len(), size, mono_stride, 1)?;
    check_layout(mono_output.len(), size, mono_output_stride, 1)?;
    if !_strength.is_finite() {
        return Err(Error::InvalidData("invalid SPP residual strength"));
    }
    for y in 0..size[1] {
        for x in 0..size[0] {
            let oi = y * output_stride + x * 3;
            let ii = y * input_stride + x * 3;
            let factor = f64::from(scale[y * scale_stride + x]);
            for c in 0..3 {
                let value = lower_bound_zero(f64::from(input[ii + c])) * factor;
                output[oi + c] = upper_bound_pixel(lower_bound_zero(value));
            }
            let value = mono[y * mono_stride + x];
            mono_output[y * mono_output_stride + x] =
                if value > PIXEL_MAX { PIXEL_MAX } else { value };
        }
    }
    Ok(())
}

/// Generate the SPP sharpness luminance map (graineo: the weight is scaled in the log domain,
/// [`power_tone`]).
#[allow(clippy::too_many_arguments)]
pub fn generate_sharp_l(
    source: &mut [f32],
    source_stride: usize,
    output: &mut [f32],
    output_stride: usize,
    rgb: &[f32],
    rgb_stride: usize,
    size: [usize; 2],
    _amount: f64,
    limit: f32,
) -> Result<()> {
    check_layout(source.len(), size, source_stride, 1)?;
    check_layout(output.len(), size, output_stride, 1)?;
    check_layout(rgb.len(), size, rgb_stride, 3)?;
    if !_amount.is_finite() || !limit.is_finite() {
        return Err(Error::InvalidData("invalid SPP sharpness parameters"));
    }
    // graineo fix: `ISO − 100` is clamped to 0…1500, so the
    // weight curve reaches 0 at ISO ≥ 1600 (residual not in the sharpening image). The former
    // `-100` above 1500 multiplied the residual deviation by −100 and made ISO ≥ 1700 images
    // grainy (DP3 Merrill ISO 3200/6400: ΔE00 ≈ 6 against SPP). Below ISO 1600 unchanged.
    let threshold = (limit - 100.0).max(0.0).min(1500.0);
    let curve = 1.0 - threshold / 1500.0;
    for y in 0..size[1] {
        for x in 0..size[0] {
            let weight = power_tone(source[y * source_stride + x], f64::from(curve));
            let ri = y * rgb_stride + x * 3;
            let channels = [
                lower_bound_zero(f64::from(rgb[ri])),
                lower_bound_zero(f64::from(rgb[ri + 1])),
                lower_bound_zero(f64::from(rgb[ri + 2])),
            ];
            let weight = f64::from(weight);
            let sum = channels[0].mul_add(weight, channels[1] * weight);
            let sum = channels[2].mul_add(weight, sum);
            output[y * output_stride + x] = (sum as f32).min(SHARP_L_MAX);
        }
    }
    if size[0] >= 5 && size[1] >= 5 {
        let sharp = output.to_vec();
        for y in 2..size[1] - 2 {
            for x in 2..size[0] - 2 {
                let mut sum = 0.0_f32;
                for yy in y - 1..=y + 1 {
                    for xx in x - 1..=x + 1 {
                        sum += sharp[yy * output_stride + xx];
                    }
                }
                source[y * source_stride + x] = sharp[y * output_stride + x] + sum / -9.0;
            }
        }
    }
    Ok(())
}

/// Apply the Stage2B luminance sharpness plane to RGB pixels.
pub fn sharpen(
    rgb: &mut [f32],
    rgb_stride: usize,
    sharp_l: &[i16],
    sharp_l_stride: usize,
    size: [usize; 2],
    amount: f64,
    control: Control<'_>,
) -> Result<()> {
    check_layout(rgb.len(), size, rgb_stride, 3)?;
    check_layout(sharp_l.len(), size, sharp_l_stride, 1)?;
    if !amount.is_finite() {
        return Err(Error::InvalidData("invalid SPP sharpen amount"));
    }

    let len = size[1]
        .checked_mul(rgb_stride)
        .ok_or(Error::InvalidData("invalid SPP sharpen image layout"))?;
    if rgb.len() < len {
        return Err(Error::InvalidData("invalid SPP sharpen image layout"));
    }
    control.check()?;

    // graineo change: each pixel depends only on itself, so the computation runs
    // in parallel per row and directly in the image (bit-identical to the serial
    // version with an intermediate buffer). On cancellation `rgb` is partially sharpened; the
    // only caller discards the image in that case anyway.
    let row = |y: usize, line: &mut [f32]| {
        for x in 0..size[0] {
            let i = x * 3;
            let channels = [
                lower_bound_zero(f64::from(line[i])),
                lower_bound_zero(f64::from(line[i + 1])),
                lower_bound_zero(f64::from(line[i + 2])),
            ];
            let sum = (channels[0] + channels[1] + channels[2]) as f32;
            let chroma = if sum == 0.0 {
                [0.0; 3]
            } else {
                channels.map(|value| value / f64::from(sum))
            };
            let delta = sharp_l[y * sharp_l_stride + x] as f32 * 0.25;
            let delta = (f64::from(delta) * amount) as f32;
            let delta = if delta > 1000.0 { 1000.0 } else { delta };
            let delta = if delta < -1000.0 { -1000.0 } else { delta };
            let lower = f32::from_bits(0xbeb3_3333) * sum;
            let upper = f32::from_bits(0x3eb3_3333) * sum;
            let delta = if delta > lower { delta } else { lower };
            let delta = if upper < delta { upper } else { delta };
            let luminance = f64::from(sum + delta);
            for channel in 0..3 {
                let value = chroma[channel] * luminance;
                line[i + channel] = (if value > 32767.0 { 32767.0 } else { value }) as f32;
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        rgb[..len]
            .par_chunks_mut(rgb_stride)
            .enumerate()
            .try_for_each(|(y, line)| {
                control.check()?;
                row(y, line);
                Ok::<(), Error>(())
            })?;
    }
    #[cfg(target_arch = "wasm32")]
    for (y, line) in rgb[..len].chunks_mut(rgb_stride).enumerate() {
        control.check()?;
        row(y, line);
    }
    control.check()?;
    Ok(())
}

fn diffuse(
    image: &mut [f32],
    stride: usize,
    size: [usize; 2],
    lambda: f32,
    scale_at: impl Fn(f32) -> f32,
) {
    let [cols, rows] = size;
    if cols < 3 || rows < 3 {
        return;
    }
    let source = image.to_vec();
    for y in 1..rows - 1 {
        for x in 1..cols - 1 {
            let i = y * stride + x;
            let center = source[i];
            let differences = [
                source[i - stride] - center,
                source[i + stride] - center,
                source[i + 1] - center,
                source[i - 1] - center,
            ];
            let scale = scale_at(center);
            let conduction =
                differences.map(|difference| 1.0 / scale.mul_add(difference * difference, 1.0));
            let change = conduction[0].mul_add(differences[0], differences[1] * conduction[1]);
            let change = conduction[2].mul_add(differences[2], change);
            let change = conduction[3].mul_add(differences[3], change);
            image[i] = lambda.mul_add(change, center);
        }
    }
    for y in 0..rows - 2 {
        image[y * stride] = source[(y + 1) * stride];
    }
    image[(rows - 2) * stride + cols - 1] = 0.0;
}

/// Run one native anisotropic diffusion pass with a fixed conduction value.
pub fn anisotropic_diffusion_mono(
    image: &mut [f32],
    stride: usize,
    size: [usize; 2],
    conduction: i32,
    lambda: f32,
) -> Result<()> {
    check_layout(image.len(), size, stride, 1)?;
    let square = conduction.wrapping_mul(conduction);
    if square == 0 || !lambda.is_finite() {
        return Err(Error::InvalidData("invalid SPP diffusion parameters"));
    }
    let scale = 1.0 / square as f32;
    diffuse(image, stride, size, lambda, |_| scale);
    Ok(())
}

/// Run one native anisotropic diffusion pass with a quadratic noise model.
pub fn anisotropic_diffusion_mono_nm(
    image: &mut [f32],
    stride: usize,
    size: [usize; 2],
    noise_model: [f32; 3],
    lambda: f32,
) -> Result<()> {
    check_layout(image.len(), size, stride, 1)?;
    if !lambda.is_finite() || !noise_model.iter().all(|value| value.is_finite()) {
        return Err(Error::InvalidData("invalid SPP diffusion parameters"));
    }
    diffuse(image, stride, size, lambda, |value| {
        let linear = noise_model[1].mul_add(value, noise_model[0]);
        let model = (noise_model[2] * value).mul_add(value, linear);
        let reciprocal = 1.0 / model;
        reciprocal * reciprocal
    });
    Ok(())
}

/// Replace bounded isolated extrema with the nearest 3 by 3 neighbor.
pub fn despeckle3(
    image: &mut [f32],
    stride: usize,
    size: [usize; 2],
    floor: f32,
    level: f32,
) -> Result<()> {
    check_layout(image.len(), size, stride, 1)?;
    if !floor.is_finite() || !level.is_finite() {
        return Err(Error::InvalidData("invalid SPP despeckle parameters"));
    }
    let scaled = (level / 100.0).min(64.0).max(floor);
    // Native despeckle threshold: powf(64 / (4096 / s), 3).
    let threshold = (64.0_f32 / (4096.0_f32 / scaled)).powf(3.0);
    let [cols, rows] = size;
    if cols < 4 || rows < 3 {
        return Ok(());
    }
    for y in 1..rows - 1 {
        for x in 1..cols - 2 {
            let i = y * stride + x;
            let center = image[i];
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            for yy in y - 1..=y + 1 {
                for xx in x - 1..=x + 1 {
                    if yy != y || xx != x {
                        let value = image[yy * stride + xx];
                        low = low.min(value);
                        high = high.max(value);
                    }
                }
            }
            if center > high && center - high > threshold {
                image[i] = high;
            } else if center < low && low - center > threshold {
                image[i] = low;
            }
        }
    }
    Ok(())
}

/// Suppress strict 3 by 3 extrema while preserving the outer border.
pub fn osf_filter(image: &mut [f32], stride: usize, size: [usize; 2]) -> Result<()> {
    check_layout(image.len(), size, stride, 1)?;
    let [cols, rows] = size;
    if cols < 3 || rows < 3 {
        return Ok(());
    }
    let source = image.to_vec();
    for y in 1..rows - 1 {
        for x in 1..cols - 1 {
            let i = y * stride + x;
            let center = source[i];
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            for yy in y - 1..=y + 1 {
                for xx in x - 1..=x + 1 {
                    if yy != y || xx != x {
                        let value = source[yy * stride + xx];
                        low = low.min(value);
                        high = high.max(value);
                    }
                }
            }
            if center > high {
                image[i] = high;
            } else if center < low {
                image[i] = low;
            }
        }
    }
    for y in 0..rows - 2 {
        image[y * stride] = source[(y + 1) * stride];
    }
    for y in 1..rows - 1 {
        image[y * stride + cols - 1] = source[cols - 1];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_f32(values: &[f32]) -> u64 {
        values
            .iter()
            .flat_map(|value| value.to_bits().to_le_bytes())
            .fold(1_469_598_103_934_665_603, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
            })
    }

    fn hash_u16(values: &[u16]) -> u64 {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .fold(1_469_598_103_934_665_603, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
            })
    }

    fn expected(name: &str) -> (usize, u64) {
        let fields: Vec<_> = crate::fixtures::get("spp-residual-native.txt")
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap()
            .split_whitespace()
            .collect();
        (
            fields[1].parse().unwrap(),
            u64::from_str_radix(fields[2], 16).unwrap(),
        )
    }

    fn assert_f32(name: &str, values: &[f32]) {
        let (count, hash) = expected(name);
        assert_eq!(values.len(), count, "{name} count");
        assert_eq!(hash_f32(values), hash, "{name} hash");
    }

    fn assert_u16(name: &str, values: &[u16]) {
        let (count, hash) = expected(name);
        assert_eq!(values.len(), count, "{name} count");
        assert_eq!(hash_u16(values), hash, "{name} hash");
    }

    fn inputs() -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        let mono = (0..54)
            .map(|i| (((i * 17 + 3) % 29) as f32).mul_add(0.071, 0.35))
            .collect();
        let mono2 = (0..54)
            .map(|i| (((i * 11 + 5) % 31) as f32).mul_add(0.063, 0.22))
            .collect();
        let rgb = (0..138)
            .map(|i| (((i * 13 + 7) % 41) as f32).mul_add(0.057, -0.2))
            .collect();
        let rgb2 = (0..138)
            .map(|i| (((i * 19 + 2) % 37) as f32).mul_add(0.049, 0.15))
            .collect();
        (mono, mono2, rgb, rgb2)
    }

    fn battery_pixels(
        cols: usize,
        rows: usize,
        stride: usize,
        channels: usize,
        pattern: usize,
    ) -> Vec<f32> {
        let mut values = vec![f32::from_bits(0xc57a_4000); stride * rows + 11];
        for y in 0..rows {
            for x in 0..cols {
                for c in 0..channels {
                    let i = y * stride + x * channels + c;
                    let mix =
                        ((i as u64 * 2_654_435_761 + pattern as u64 * 2_246_822_519) >> 9) as u32;
                    let mut bits = if pattern == 4 {
                        0x4300_0000
                    } else {
                        0x3f00_0000
                    } | (mix & 0x007f_ffff);
                    if pattern == 0 {
                        bits = 0x3f80_0000;
                    }
                    if pattern == 2 && (x + 2 * y + c) % 3 == 0 {
                        bits |= 0x8000_0000;
                    }
                    if pattern == 3 && x == cols / 2 && y == rows / 2 {
                        bits = if c == 1 { 0x45da_c000 } else { 0xbdcc_cccd };
                    }
                    values[i] = f32::from_bits(bits);
                }
            }
        }
        values
    }

    #[test]
    fn native_focused_residual_battery() {
        if crate::fixtures::missing(&["spp-residual-native.txt"]) {
            return;
        }
        let cases = [
            (5, 5, 2, 0, 0.0, 0.713, 0.013),
            (6, 4, 3, 1, 0.371, 0.823, 0.019),
            (7, 7, 5, 2, 1.0, 1.137, 0.007),
            (8, 6, 1, 3, 0.837, 0.619, 0.023),
        ];
        for (k, &(cols, rows, pad, pattern, mix, tone, epsilon)) in cases.iter().enumerate() {
            let size = [cols, rows];
            let mono_stride = cols + pad;
            let rgb_stride = cols * 3 + pad + 1;
            let mut mixed = battery_pixels(cols, rows, mono_stride, 1, pattern);
            let reference = battery_pixels(cols, rows, rgb_stride, 3, (pattern + 1) % 5);
            let filtered = battery_pixels(cols, rows, rgb_stride, 3, (pattern + 3) % 5);
            mono_residual_mixing(
                &mut mixed,
                mono_stride,
                &reference,
                rgb_stride,
                &filtered,
                rgb_stride,
                size,
                mix,
                tone,
                [0.317, 0.493, 0.191],
                epsilon,
            )
            .unwrap();
            assert_f32(&format!("BAT_MONO_MIX_{k}_OUT"), &mixed);

            let scale_map = battery_pixels(cols, rows, mono_stride, 1, (pattern + 1) % 5);
            let mono_in = battery_pixels(cols, rows, mono_stride, 1, (pattern + 2) % 5);
            let rgb_in = battery_pixels(cols, rows, rgb_stride, 3, pattern);
            let mut rgb_out = battery_pixels(cols, rows, rgb_stride, 3, 0);
            let mut mono_out = battery_pixels(cols, rows, mono_stride, 1, 0);
            apply_mono_residual(
                &mut rgb_out,
                rgb_stride,
                &scale_map,
                mono_stride,
                &rgb_in,
                rgb_stride,
                &mono_in,
                mono_stride,
                &mut mono_out,
                mono_stride,
                size,
                0.137_f32 + k as f32 * 0.11,
            )
            .unwrap();
            assert_f32(&format!("BAT_APPLY_MONO_{k}_RGB"), &rgb_out);
            assert_f32(&format!("BAT_APPLY_MONO_{k}_MONO"), &mono_out);
        }
    }

    #[test]
    fn native_color_and_local_filter_battery() {
        if crate::fixtures::missing(&["spp-residual-native.txt"]) {
            return;
        }
        for k in 0..3 {
            let (cols, rows, pad) = (5 + k, 4 + k, 1 + k);
            let size = [cols, rows];
            let mono_stride = cols + pad;
            let rgb_stride = cols * 3 + pad + 1;
            let mono = battery_pixels(cols, rows, mono_stride, 1, k + 1);
            let reference = battery_pixels(cols, rows, rgb_stride, 3, k + 2);
            let filtered = battery_pixels(cols, rows, rgb_stride, 3, (k + 4) % 5);
            let mut mixed = battery_pixels(cols, rows, rgb_stride, 3, 0);
            color_residual_mixing(
                &mut mixed,
                rgb_stride,
                &mono,
                mono_stride,
                &reference,
                rgb_stride,
                &filtered,
                rgb_stride,
                size,
                if k == 0 {
                    0.0
                } else if k == 1 {
                    0.371
                } else {
                    1.0
                },
                0.613 + 0.071 * k as f64,
                0.817 + 0.113 * k as f64,
                [0.317, 0.493, 0.191],
                0.011 + 0.006 * k as f32,
            )
            .unwrap();
            assert_f32(&format!("BAT_COLOR_MIX_{k}_OUT"), &mixed);

            let mut rgb_out = battery_pixels(cols, rows, rgb_stride, 3, 0);
            let mut mono_out = battery_pixels(cols, rows, mono_stride, 1, 0);
            apply_color_residual(
                &mut rgb_out,
                rgb_stride,
                &filtered,
                rgb_stride,
                &reference,
                rgb_stride,
                &mono,
                mono_stride,
                &mut mono_out,
                mono_stride,
                size,
                0.137 + 0.173 * k as f32,
            )
            .unwrap();
            assert_f32(&format!("BAT_APPLY_COLOR_{k}_RGB"), &rgb_out);
            assert_f32(&format!("BAT_APPLY_COLOR_{k}_MONO"), &mono_out);
        }

        let dimensions = [(4, 4, 2), (5, 5, 1), (8, 7, 3)];
        for (k, &(cols, rows, pad)) in dimensions.iter().enumerate() {
            let size = [cols, rows];
            let mono_stride = cols + pad;
            let rgb_stride = cols * 3 + pad + 1;
            let mut sharp_source = battery_pixels(cols, rows, mono_stride, 1, k + 1);
            let mut sharp_output = battery_pixels(cols, rows, mono_stride, 1, 0);
            let rgb = battery_pixels(cols, rows, rgb_stride, 3, k + 2);
            let mut osf = sharp_source.clone();
            let mut despeckled = sharp_source.clone();
            generate_sharp_l(
                &mut sharp_source,
                mono_stride,
                &mut sharp_output,
                mono_stride,
                &rgb,
                rgb_stride,
                size,
                0.213 + 0.187 * k as f64,
                127.25 + 80.5 * k as f32,
            )
            .unwrap();
            osf_filter(&mut osf, mono_stride, size).unwrap();
            despeckle3(
                &mut despeckled,
                mono_stride,
                size,
                0.011 + 0.007 * k as f32,
                7.3 + 5.125 * k as f32,
            )
            .unwrap();
            assert_f32(&format!("BAT_SHARP_{k}_SOURCE"), &sharp_source);
            assert_f32(&format!("BAT_SHARP_{k}_OUTPUT"), &sharp_output);
            assert_f32(&format!("BAT_OSF_{k}_IMAGE"), &osf);
            assert_f32(&format!("BAT_DESPECKLE_{k}_IMAGE"), &despeckled);
        }
    }

    #[test]
    fn native_residual_and_diffusion_oracle() {
        if crate::fixtures::missing(&["spp-residual-native.txt"]) {
            return;
        }
        let size = [7, 6];
        let (mono, mono2, rgb, rgb2) = inputs();
        let mut rgb_out = vec![-99.0; 138];
        let mut mono_out = vec![-77.0; 54];
        apply_color_residual(
            &mut rgb_out,
            23,
            &rgb2,
            23,
            &rgb,
            23,
            &mono,
            9,
            &mut mono_out,
            9,
            size,
            0.37,
        )
        .unwrap();
        assert_f32("APPLY_COLOR_RGB", &rgb_out);
        assert_f32("APPLY_COLOR_MONO", &mono_out);

        rgb_out.fill(-99.0);
        mono_out.fill(-77.0);
        apply_mono_residual(
            &mut rgb_out,
            23,
            &mono2,
            9,
            &rgb,
            23,
            &mono,
            9,
            &mut mono_out,
            9,
            size,
            -0.4,
        )
        .unwrap();
        assert_f32("APPLY_MONO_RGB", &rgb_out);
        assert_f32("APPLY_MONO_MONO", &mono_out);

        let mut diff = mono.clone();
        anisotropic_diffusion_mono(&mut diff, 9, size, 9, 0.137).unwrap();
        assert_f32("DIFF", &diff);
        let mut diff_nm = mono.clone();
        anisotropic_diffusion_mono_nm(&mut diff_nm, 9, size, [0.73, 0.021, 0.004], 0.119).unwrap();
        assert_f32("DIFF_NM", &diff_nm);
        let mut osf = mono.clone();
        osf_filter(&mut osf, 9, size).unwrap();
        assert_f32("OSF", &osf);
        let mut despeckled = mono.clone();
        despeckle3(&mut despeckled, 9, size, 0.018, 17.3).unwrap();
        assert_f32("DESPECKLE", &despeckled);

        let mut sharp_source = mono.clone();
        let mut sharp_output = vec![-77.0; 54];
        generate_sharp_l(
            &mut sharp_source,
            9,
            &mut sharp_output,
            9,
            &rgb,
            23,
            size,
            0.43,
            287.5,
        )
        .unwrap();
        assert_f32("SHARP_OUTPUT", &sharp_output);
        assert_f32("SHARP_SOURCE", &sharp_source);

        let mut color_mix = vec![-99.0; 138];
        color_residual_mixing(
            &mut color_mix,
            23,
            &mono,
            9,
            &rgb,
            23,
            &rgb2,
            23,
            size,
            0.37,
            0.61,
            0.82,
            [0.3, 0.5, 0.2],
            0.017,
        )
        .unwrap();
        assert_f32("COLOR_MIX", &color_mix);
        let mut mono_mix = mono.clone();
        mono_residual_mixing(
            &mut mono_mix,
            9,
            &rgb,
            23,
            &rgb2,
            23,
            size,
            0.37,
            0.82,
            [0.3, 0.5, 0.2],
            0.017,
        )
        .unwrap();
        assert_f32("MONO_MIX", &mono_mix);
    }

    #[test]
    fn sharpen_matches_native_shapes_padding_and_strengths() {
        if crate::fixtures::missing(&["spp-pipeline-sharpen.txt"]) {
            return;
        }
        let fixture = crate::fixtures::get("spp-pipeline-sharpen.txt");
        for line in fixture.lines().filter(|line| line.starts_with("SHARP ")) {
            let fields: Vec<_> = line.split_whitespace().collect();
            let test: u32 = fields[1].parse().unwrap();
            let width: usize = fields[2].parse().unwrap();
            let rows: usize = fields[3].parse().unwrap();
            let rgb_stride: usize = fields[4].parse().unwrap();
            let amount: f64 = fields[5].parse().unwrap();
            let expected = u64::from_str_radix(fields[6], 16).unwrap();
            let sharp_stride = width + 3;
            let mut rgb = vec![-12345.25_f32; rgb_stride * rows];
            let mut sharp_l = vec![0x5a5a_i16; sharp_stride * rows];
            let mut state = 0x8f3a_21d7_u32.wrapping_add(test);
            for y in 0..rows {
                for x in 0..width {
                    for channel in 0..3 {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        let value = ((state >> 8) % 70_000) as i32 - 12_000;
                        let mut value = value as f32 / 7.0;
                        if x == 0 && y == 0 {
                            value = [-17.25, 0.0, 33.5][channel];
                        }
                        if x + 1 == width && y + 1 == rows {
                            value = if channel == 2 { 40_000.0 } else { 1.25 };
                        }
                        rgb[y * rgb_stride + x * 3 + channel] = value;
                    }
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    sharp_l[y * sharp_stride + x] = (state >> 16) as i16;
                }
            }
            sharp_l[0] = i16::MIN;
            sharp_l[(rows - 1) * sharp_stride + width - 1] = i16::MAX;
            sharpen(
                &mut rgb,
                rgb_stride,
                &sharp_l,
                sharp_stride,
                [width, rows],
                amount,
                Control::none(),
            )
            .unwrap();
            assert_eq!(crate::float_front::fnv1a_f32(&rgb), expected, "case {test}");
            for y in 0..rows {
                assert!(rgb[y * rgb_stride + width * 3..(y + 1) * rgb_stride]
                    .iter()
                    .all(|value| value.to_bits() == (-12345.25_f32).to_bits()));
            }
        }

        let mut rgb = vec![7.25_f32; 24];
        let original = rgb.clone();
        let sharp_l = vec![11_i16; 8];
        assert!(sharpen(&mut rgb, 6, &sharp_l, 2, [3, 4], 1.0, Control::none()).is_err());
        assert_eq!(rgb, original);

        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(matches!(
            sharpen(
                &mut rgb,
                6,
                &sharp_l,
                2,
                [2, 4],
                1.0,
                Control::new(&cancelled),
            ),
            Err(Error::Cancelled)
        ));
        assert_eq!(rgb, original);
    }
}
