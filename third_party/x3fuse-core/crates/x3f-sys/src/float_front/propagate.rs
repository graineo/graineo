//! graineo addition: highlight reconstruction of our own for the Merrill float frontend.
//!
//! Colour propagation, after the idea of RawTherapee's "Color propagation" highlight
//! reconstruction (E. Martinec, GPL-3.0): the colour of a clipped area is continued from the
//! unclipped surroundings by blurring, not looked up anywhere.
//!
//! 1. A camera-marked clipped sample counts only if a neighbour in the same layer is marked too.
//! 2. Reference is the bottom layer (it clips last). For the middle and top layer the ratio
//!    layer/bottom of unclipped pixels is averaged over growing square windows (8, 32, 128, 512 px),
//!    weighted by the brightness of the pixel; the smallest window with enough weight is used.
//! 3. A clipped sample becomes `max(value, bottom · ratio)`; pixels whose bottom layer is clipped
//!    as well are left to step 4.
//! 4. A soft desaturation towards the bottom layer. Weight: the larger of the pixel brightness
//!    (see [`highlight_brightness`]; smoothstep over 1000 counts above the threshold) and ¾ of a
//!    feathered clip mask (share of clipped pixels in a 9×9 window), so that blown areas end up
//!    neutral.
//!
//! No table indexed by colour ratios, no donor statistics per hue, no test of the reconstructed
//! value against a threshold, no choice between reconstruction and neutralisation (both always run,
//! one after the other).

use rayon::prelude::*;

use super::{HighlightRestoration, HighlightStats};
use crate::{Control, Error, Result};

/// Block size of the ratio grid.
const BLOCK: usize = 8;
/// Window radii in blocks (8, 32, 128, 512 px half-width).
const RADII: [usize; 4] = [1, 4, 16, 64];

impl HighlightRestoration {
    /// Highlight reconstruction (see the module documentation); `sat` is cleaned in place.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn restore_own(
        &self,
        data: &mut [f32],
        width: usize,
        height: usize,
        row_stride: usize,
        sat: &mut [u8],
        valid: [i32; 4],
        control: Control<'_>,
    ) -> Result<HighlightStats> {
        control.check()?;
        if width < 4 || height < 4 {
            return Err(Error::InvalidData("invalid Merrill highlight image size"));
        }
        if sat.len() != width * height * 3 {
            return Err(Error::InvalidData("invalid Merrill highlight map size"));
        }
        if row_stride < width * 3 || data.len() < row_stride * (height - 1) + width * 3 {
            return Err(Error::InvalidData("invalid Merrill highlight image layout"));
        }
        let (left, top, right, bottom) = (valid[0], valid[1], valid[2], valid[3]);
        if left < 0 || top < 0 || right < left || bottom < top || right as usize >= width || bottom as usize >= height {
            return Err(Error::InvalidData("invalid Merrill highlight rectangle"));
        }
        let mut stats = HighlightStats::default();
        let (left, top, right, bottom) = (left as usize, top as usize, right as usize, bottom as usize);

        // 1. Clipped = marked and a marked 8-neighbour in the same layer.
        let clipped = clean_saturation(sat, width, height, &mut stats);
        let threshold = self.threshold();
        let mask = feathered_mask(&clipped, width, height);
        if clipped.iter().any(|&b| b & 0b110 != 0) {
            control.check()?;
            // 2. Ratio grid: per block the brightness-weighted sums of layer/bottom over unclipped pixels.
            let (bw, bh) = (width.div_ceil(BLOCK), height.div_ceil(BLOCK));
            let blocks: Vec<[f64; 3]> = (0..bh)
                .into_par_iter()
                .flat_map_iter(|by| {
                    let data = &*data;
                    let clipped = &clipped;
                    (0..bw).map(move |bx| {
                        let mut s = [0.0_f64; 3]; // weight, weight·r1, weight·r2
                        for y in by * BLOCK..((by + 1) * BLOCK).min(height) {
                            if y < top || y > bottom {
                                continue;
                            }
                            for x in bx * BLOCK..((bx + 1) * BLOCK).min(width) {
                                if x < left || x > right || clipped[y * width + x] != 0 {
                                    continue;
                                }
                                let p = &data[y * row_stride + 3 * x..y * row_stride + 3 * x + 3];
                                let b = f64::from(p[0]);
                                if b <= 1.0 {
                                    continue;
                                }
                                // brighter pixels tell more about the colour of the light
                                let w = (b / threshold).clamp(0.0, 1.0).powi(2);
                                s[0] += w;
                                s[1] += w * f64::from(p[1]) / b;
                                s[2] += w * f64::from(p[2]) / b;
                            }
                        }
                        s
                    })
                })
                .collect();
            // Summed-area table of the grid
            let mut sum = vec![[0.0_f64; 3]; (bw + 1) * (bh + 1)];
            for by in 0..bh {
                let mut row = [0.0_f64; 3];
                for bx in 0..bw {
                    for k in 0..3 {
                        row[k] += blocks[by * bw + bx][k];
                        sum[(by + 1) * (bw + 1) + bx + 1][k] = sum[by * (bw + 1) + bx + 1][k] + row[k];
                    }
                }
            }
            let window = |bx: usize, by: usize, r: usize| -> [f64; 3] {
                let (x0, y0) = (bx.saturating_sub(r), by.saturating_sub(r));
                let (x1, y1) = ((bx + r + 1).min(bw), (by + r + 1).min(bh));
                let at = |x: usize, y: usize| sum[y * (bw + 1) + x];
                let (a, b, c, d) = (at(x1, y1), at(x0, y1), at(x1, y0), at(x0, y0));
                std::array::from_fn(|k| a[k] - b[k] - c[k] + d[k])
            };
            // Ratio per block: smallest window with enough weight (a quarter of its pixels at full weight)
            let ratio: Vec<Option<[f32; 2]>> = (0..bh * bw)
                .into_par_iter()
                .map(|i| {
                    let (bx, by) = (i % bw, i / bw);
                    for r in RADII {
                        let s = window(bx, by, r);
                        // enough weight: 2 % of the window at full weight
                        let need = 0.02 * ((2 * r + 1) * BLOCK).pow(2) as f64;
                        if s[0] > need.max(4.0) {
                            return Some([(s[1] / s[0]) as f32, (s[2] / s[0]) as f32]);
                        }
                    }
                    None
                })
                .collect();
            control.check()?;
            // 3. Fill clipped middle/top samples from the bottom layer and the propagated ratio.
            let filled: usize = data
                .par_chunks_mut(row_stride)
                .enumerate()
                .filter(|(y, _)| (top..=bottom).contains(y))
                .map(|(y, row)| {
                    let mut n = 0;
                    for x in left..=right {
                        let bits = clipped[y * width + x];
                        if bits & 0b110 == 0 || bits & 1 != 0 {
                            continue;
                        }
                        let Some(r) = ratio[(y / BLOCK) * bw + x / BLOCK] else { continue };
                        let p = &mut row[3 * x..3 * x + 3];
                        for c in 1..3 {
                            if bits & (1 << c) != 0 {
                                let estimate = (p[0] * r[c - 1]).min(32767.0);
                                if estimate > p[c] {
                                    p[c] = estimate;
                                    n += 1;
                                }
                            }
                        }
                    }
                    n
                })
                .sum();
            stats.dense_pixels = filled as u32;
            stats.occupied = ratio.iter().filter(|r| r.is_some()).count() as u32;
            if std::env::var_os("X3F_HL_DEBUG").is_some() {
                let mut n = [0usize; 8];
                for &b in &clipped {
                    n[b as usize] += 1;
                }
                eprintln!("highlights own: threshold {threshold} filled {filled} clip patterns {n:?}");
            }
        }
        control.check()?;
        // 4. Desaturation by brightness and the feathered clip mask.
        self.desaturate(data, width, row_stride, &mask, &clipped, [left, top, right, bottom]);
        Ok(stats)
    }

    /// Glide towards the bottom layer: weight = max(smoothstep of [`highlight_brightness`] over
    /// 1000 counts above the threshold, ¾ · feathered clip mask).
    #[allow(clippy::too_many_arguments)]
    fn desaturate(
        &self,
        data: &mut [f32],
        width: usize,
        row_stride: usize,
        mask: &[f32],
        clipped: &[u8],
        rect: [usize; 4],
    ) {
        let [left, top, right, bottom] = rect;
        let threshold = self.threshold();
        data.par_chunks_mut(row_stride).enumerate().filter(|(y, _)| (top..=bottom).contains(y)).for_each(|(y, row)| {
            let row = &mut row[..width * 3];
            for x in left..=right {
                let p = &mut row[3 * x..3 * x + 3];
                let hi = highlight_brightness([p[0], p[1], p[2]], clipped[y * width + x]);
                let t = ((hi - threshold) / 1000.0).clamp(0.0, 1.0);
                let w = (t * t * (3.0 - 2.0 * t)).max(0.75 * f64::from(mask[y * width + x]));
                if w <= 0.0 {
                    continue;
                }
                let anchor = f64::from(p[0]);
                for v in p.iter_mut() {
                    *v = ((1.0 - w) * f64::from(*v) + w * anchor) as f32;
                }
            }
        });
    }
}

/// Weight of the smallest layer in [`highlight_brightness`] (the colour part counts with
/// `1 − GREY_WEIGHT`). Chosen from measurements on bright coloured flowers and lights: values from
/// 0.18 to 0.22 serve equally well; outside that range the worst colour errors grow quickly.
const GREY_WEIGHT: f64 = 0.2;

/// Brightness that drives the highlight desaturation of one pixel (`layers` after filling,
/// `clip_bits` = clipped layers of this pixel, bit c = layer c).
///
/// The pixel is split into a grey part (the smallest layer) and a colour part (largest − smallest),
/// as in HSV/HSL. The brightness is the grey part plus 80 % of the colour part, i.e.
/// `0.8 · largest + 0.2 · smallest`: a pixel is neutralised once it is bright as a whole, not
/// already when a single layer stands high because the colour is strong (bright red petals, coloured
/// lights). Where a layer of this pixel is clipped, its colour is only an estimate; there the
/// cautious measure, the largest layer, is kept.
pub(super) fn highlight_brightness(layers: [f32; 3], clip_bits: u8) -> f64 {
    let [a, b, c] = layers.map(f64::from);
    let largest = a.max(b).max(c);
    if clip_bits != 0 {
        return largest;
    }
    let smallest = a.min(b).min(c);
    largest - GREY_WEIGHT * (largest - smallest)
}

/// Step 1 of both reconstructions: a camera-marked clipped sample counts only if a neighbour in the
/// same layer is marked too. Cleans `sat` in place and returns the clip bits per pixel (bit c = layer c).
pub(super) fn clean_saturation(sat: &mut [u8], width: usize, height: usize, stats: &mut HighlightStats) -> Vec<u8> {
    let clipped: Vec<u8> = (0..height)
        .into_par_iter()
        .flat_map_iter(|y| {
            let sat = &*sat;
            (0..width).map(move |x| {
                let mut bits = 0u8;
                if y == 0 || x == 0 || y + 1 >= height || x + 1 >= width {
                    return bits;
                }
                for c in 0..3 {
                    if sat[3 * (y * width + x) + c] == 0 {
                        continue;
                    }
                    let mut any = false;
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            if dx != 0 || dy != 0 {
                                let n = (y as i32 + dy) as usize * width + (x as i32 + dx) as usize;
                                any |= sat[3 * n + c] != 0;
                            }
                        }
                    }
                    if any {
                        bits |= 1 << c;
                    }
                }
                bits
            })
        })
        .collect();
    for (i, s) in sat.iter_mut().enumerate() {
        let keep = clipped[i / 3] & (1 << (i % 3)) != 0;
        if *s != 0 && !keep {
            stats.removed += 1;
        }
        *s = u8::from(keep);
    }
    clipped
}

/// Share of clipped pixels (any layer) in the 9×9 window around each pixel (separable box sum).
fn feathered_mask(clipped: &[u8], width: usize, height: usize) -> Vec<f32> {
    const R: usize = 4;
    if !clipped.iter().any(|&b| b != 0) {
        return vec![0.0; width * height];
    }
    let mut horiz = vec![0u8; width * height];
    horiz.par_chunks_mut(width).zip(clipped.par_chunks(width)).for_each(|(out, row)| {
        for x in 0..width {
            let (a, b) = (x.saturating_sub(R), (x + R).min(width - 1));
            out[x] = row[a..=b].iter().filter(|&&v| v != 0).count() as u8;
        }
    });
    let mut out = vec![0f32; width * height];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(R), (y + R).min(height - 1));
        for x in 0..width {
            let mut n = 0u32;
            for yy in a..=b {
                n += u32::from(horiz[yy * width + x]);
            }
            row[x] = n as f32 / ((2 * R + 1) * (2 * R + 1)) as f32;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::highlight_brightness;

    #[test]
    fn brightness_without_clip_mixes_largest_and_smallest_layer() {
        let b = highlight_brightness([3864.0, 5827.0, 8831.0], 0);
        assert!((b - (0.8 * 8831.0 + 0.2 * 3864.0)).abs() < 1e-9, "{b}");
        // order of the layers does not matter
        assert_eq!(b, highlight_brightness([8831.0, 3864.0, 5827.0], 0));
    }

    #[test]
    fn brightness_with_clip_bit_is_largest_layer() {
        for bits in [0b001, 0b010, 0b100, 0b111] {
            assert_eq!(highlight_brightness([3864.0, 5827.0, 8831.0], bits), 8831.0);
        }
    }

    #[test]
    fn brightness_edge_cases() {
        assert_eq!(highlight_brightness([5000.0; 3], 0), 5000.0);
        assert_eq!(highlight_brightness([0.0; 3], 0), 0.0);
        assert_eq!(highlight_brightness([0.0; 3], 0b111), 0.0);
    }
}
