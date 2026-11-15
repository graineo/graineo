//! Monochrome branch of the Merrill denoise: the native monochrome denoise,
//! which native Stage2 calls instead of the tiled denoise when the
//! host's monochrome flag is set.
//!
//! It runs four threaded workers over the whole image, in this order:
//! - RGB-to-intensity mixing → [`hn`]: RGB to one intensity plane
//!   with the `m_ColorMixing` weights.
//! - Mono BCS (column offset matching) → [`bcs`].
//! - Fast bilateral filter → [`hist_bilateral`]: the native histogram
//!   bilateral filter, mono overload.
//! - Despeckle and sharpen → the native despeckle, then the B/W
//!   sharpen, both in [`sharpen_bnw`].
//!
//! Each kernel is checked bit for bit against the native library through the
//! native harness; the whole orchestration is checked against a native
//! monochrome-denoise harness.

pub(crate) mod hist_bilateral;
pub(crate) mod hn;
pub(crate) mod sharpen_bnw;

use crate::{Control, Error, Result};

/// The native noise-parameter fields the monochrome denoise reads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonoNoise {
    /// The ISO used by the despeckle, the B/W sharpen and the BCS threshold.
    pub iso: f32,
    /// `m_fNoiseScaling`; 0 disables the bilateral filter.
    pub noise_scaling: f32,
    /// The gain applied to the result (and despeckle floor, sharpen amount).
    pub iso_gain: f32,
    /// Column noise scale of the BCS tables; 0 disables them.
    pub column_noise_scale: f32,
    /// BCS filter size.
    pub bcs_filter_size: u32,
    /// BCS threshold scale index.
    pub bcs_scale_index: u32,
    /// `m_ColorMixing` as stored by the host.
    pub color_mixing: [f64; 3],
    /// The normalized white balance from the native parameter setup.
    pub white_balance: [f64; 3],
}

/// The scalar arguments of the native monochrome denoise after the noise parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MonoDenoiseArgs {
    /// The native color-float triple: the request gains the white balance is divided by.
    pub gains: [f64; 3],
    /// The two doubles that rescale the noise when `adjust_noise` is set.
    pub noise_divisors: [f64; 2],
    /// Last `bool`: divide the noise scaling by the two divisors and disable
    /// the filter from ISO 800.
    pub adjust_noise: bool,
}

/// Number of worker-pool row bands: from 8 down while `rows / 40 / n < 2`,
/// never below 2 (all in f32, as native).
fn band_count(rows: usize) -> u32 {
    let per = rows as u32 as f32 / 40.0;
    let mut n = 8.0_f32;
    if per * 0.125 < 2.0 {
        loop {
            n += -1.0;
            if per / n >= 2.0 || (per / n).is_nan() {
                break;
            }
        }
    }
    (if n < 2.0 { 2.0 } else { n }) as u32
}

/// Worker-pool row bands: `rows / n` each, inclusive, the last to the end.
fn bands(rows: usize, n: u32) -> Vec<(u32, u32)> {
    let rows = rows as u32;
    let step = rows / n;
    (0..n)
        .map(|k| {
            let first = k * step;
            let end = if k + 1 >= n { rows } else { first + step };
            (first, end.wrapping_sub(1))
        })
        .collect()
}

/// The mixing weights as the native monochrome denoise rewrites them before mixing.
fn mixing_weights(noise: &MonoNoise, gains: [f64; 3]) -> [f64; 3] {
    let mut w = noise.color_mixing.map(|v| v as f32);
    if w.iter().all(|&v| v == 0.0) {
        w = [1.0; 3];
    }
    // Sequential: each divisor uses the weights already normalized.
    w[0] /= w[2] + (w[0] + w[1]);
    w[1] /= w[2] + (w[1] + w[0]);
    w[2] /= w[2] + (w[0] + w[1]);
    if w.iter().all(|&v| v == 0.0) {
        w = [1.0; 3];
    }
    std::array::from_fn(|c| f64::from(w[c] * (noise.white_balance[c] / gains[c]) as f32))
}

/// Port of the native monochrome denoise on whole images.
///
/// `image` (native param 1, RGB, `[w, h]`, `image_stride` floats) is the work
/// and output image: on return every pixel holds the gray result times
/// `iso_gain` in all three channels. `source` (param 3, RGB) is mixed to gray.
/// `sharp_l` (param 2, i16) receives the B/N sharpness plane for Stage2B.
#[allow(clippy::too_many_arguments)]
pub(crate) fn monochrome_denoise(
    image: &mut [f32],
    image_stride: usize,
    source: &[f32],
    source_stride: usize,
    sharp_l: &mut [i16],
    sharp_l_stride: usize,
    size: [usize; 2],
    noise: &MonoNoise,
    args: MonoDenoiseArgs,
    control: Control<'_>,
) -> Result<()> {
    let [cols, rows] = size;
    if cols == 0 || rows == 0 || cols > i32::MAX as usize / 12 || rows > i32::MAX as usize {
        return Err(Error::InvalidData("invalid SPP monochrome size"));
    }
    let mut noise_scaling = noise.noise_scaling;
    if args.adjust_noise && noise_scaling != 0.0 {
        let divisor = (1.0 / (args.noise_divisors[0] * args.noise_divisors[1])) as f32;
        noise_scaling *= divisor;
    }
    let weights = mixing_weights(noise, args.gains);
    let coeffs = (noise_scaling != 0.0).then(|| {
        crate::spp_denoise::fast_bilateral_coeffs(
            f64::from(noise_scaling),
            9.85389175535173,
            0.0428362301201803,
            1.73171583027778e-05,
        )
    });
    let bands = bands(rows, band_count(rows));
    control.check()?;

    let mut mono = vec![0.0_f32; cols * rows];
    let hn = hn::HnParams::monochrome_denoise(weights);
    for &(first, last) in &bands {
        hn::sigma_hn(
            &mut mono,
            cols,
            source,
            source_stride,
            cols,
            first as usize,
            last as usize,
            &hn,
        )?;
    }
    // graineo: no column matching of the grey plane.
    control.check()?;
    let disabled = noise_scaling == 0.0 || (noise.iso >= 800.0 && args.adjust_noise);
    if let (false, Some(coeffs)) = (disabled, &coeffs) {
        for &(first, last) in &bands {
            hist_bilateral::apply_hist_bilateral_filter(
                &mono,
                cols,
                image,
                image_stride,
                size,
                first,
                last,
                coeffs,
            )?;
        }
        for y in 0..rows {
            for x in 0..cols {
                mono[y * cols + x] = image[y * image_stride + 3 * x + 2];
            }
        }
    }
    control.check()?;
    for &(first, last) in &bands {
        sharpen_bnw::despeckle3_mono(
            &mut mono,
            cols,
            image,
            image_stride,
            size,
            [first, last],
            noise.iso_gain,
            noise.iso,
        )?;
        sharpen_bnw::sharpen_bnw(
            &mono,
            cols,
            sharp_l,
            sharp_l_stride,
            size,
            [first, last],
            noise.iso_gain,
            noise.iso,
        )?;
    }
    for y in 0..rows {
        for x in 0..cols {
            let value = noise.iso_gain * mono[y * cols + x];
            image[y * image_stride + 3 * x..][..3].fill(value);
        }
    }
    control.check()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv_bytes(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes
            .into_iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    /// Synthetic input generator of the native harness.
    fn synthetic(x: u32, y: u32, c: u32, seed: u32) -> f32 {
        let mut h = x.wrapping_mul(73_856_093)
            ^ y.wrapping_mul(19_349_663)
            ^ c.wrapping_mul(83_492_791)
            ^ seed.wrapping_mul(2_654_435_761);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        let noise = (h % 2001) as f32 - 1000.0;
        let ramp = 300.0 + 18.0 * x as f32 + 7.0 * y as f32 + 400.0 * c as f32;
        let edge = if ((x / 23 + y / 17) & 1) != 0 {
            1800.0
        } else {
            0.0
        };
        (ramp + edge + 0.35 * noise).max(0.0)
    }

    /// The dp2-s1 NoiseParams blob of spp-image-native.txt.
    fn dp2_noise() -> MonoNoise {
        let line = crate::fixtures::get("spp-image-native.txt")
            .lines()
            .find(|l| l.starts_with("IMAGE dp2-s1 "))
            .unwrap();
        let hex = line.split_whitespace().nth(14).unwrap();
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        assert_eq!(bytes.len(), 0x1c0);
        let u = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let f = |o: usize| f32::from_bits(u(o));
        let d = |o: usize| f64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
        MonoNoise {
            iso: f(0x04),
            noise_scaling: f(0x28),
            iso_gain: f(0x44),
            column_noise_scale: f(0xac),
            bcs_filter_size: u(0xb0),
            bcs_scale_index: u(0xb4),
            color_mixing: [d(0x188), d(0x190), d(0x198)],
            white_balance: [d(0x1a8), d(0x1b0), d(0x1b8)],
        }
    }

    #[test]
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    fn monochrome_denoise_matches_native() {
        if crate::fixtures::missing(&["spp-image-native.txt", "spp-mono-denoise-native.txt"]) {
            return;
        }
        let base = dp2_noise();
        let mut cases = 0;
        for line in crate::fixtures::get("spp-mono-denoise-native.txt").lines().filter(|l| l.starts_with("CASE ")) {
            let p: Vec<&str> = line.split_whitespace().collect();
            let (w, h): (usize, usize) = (p[1].parse().unwrap(), p[2].parse().unwrap());
            let d = |i: usize| f64::from_bits(u64::from_str_radix(p[i], 16).unwrap());
            let noise = MonoNoise {
                white_balance: [d(3), d(4), d(5)],
                color_mixing: [d(6), d(7), d(8)],
                ..base.clone()
            };
            let args = MonoDenoiseArgs {
                gains: [1.711, 1.818, 2.410],
                noise_divisors: [d(10), d(11)],
                adjust_noise: p[9] == "1",
            };
            let source: Vec<f32> = (0..h)
                .flat_map(|y| {
                    (0..w).flat_map(move |x| {
                        (0..3).map(move |c| synthetic(x as u32, y as u32, c, w as u32))
                    })
                })
                .collect();
            let mut image = source.clone();
            let mut sharp = vec![0_i16; w * h];
            monochrome_denoise(
                &mut image,
                w * 3,
                &source,
                w * 3,
                &mut sharp,
                w,
                [w, h],
                &noise,
                args,
                Control::default(),
            )
            .unwrap();
            let out = fnv_bytes(image.iter().flat_map(|v| v.to_bits().to_le_bytes()));
            let sharp = fnv_bytes(sharp.iter().flat_map(|v| v.to_le_bytes()));
            assert_eq!(format!("{out:016x}"), p[12], "{line}: image");
            assert_eq!(format!("{sharp:016x}"), p[13], "{line}: sharpness");
            cases += 1;
        }
        assert_eq!(cases, 6);
    }

    #[test]
    fn band_counts_follow_native() {
        assert_eq!(band_count(64), 2);
        assert_eq!(band_count(77), 2);
        assert_eq!(band_count(3136), 8);
        assert_eq!(bands(10, 3), vec![(0, 2), (3, 5), (6, 9)]);
    }
}
