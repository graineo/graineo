//! graineo addition: second own highlight reconstruction for the Merrill float frontend, switched on
//! with `GRAINEO_MERRILL_NEW=highlights` (see [`crate::merrill_new`]).
//!
//! Two independent steps, one after the other, without any weight that chooses or blends between
//! "reconstruct" and "neutralise":
//!
//! 1. **Reconstruction "inpaint opposed"** after darktable's highlight reconstruction mode of that
//!    name (Hanno Schwalm, `src/iop/hlreconstruct/opposed.c`, GPL-3.0), adapted to three layers per
//!    pixel: for every layer the mean of the two *opposed* layers over the 3×3 neighbourhood is
//!    formed in cube-root space; one chroma offset per layer (image-wide mean of `value − opposed`
//!    over unclipped pixels that border clipped areas of that layer) is added to it, and a clipped
//!    sample becomes `max(value, opposed + offset)`.
//! 2. **Soft saturation limit by brightness**, a continuous function applied to every pixel alike:
//!    brightness `Y` = the highest level of the three layers relative to its own clip level (median
//!    of its clipped samples; a layer without clipped samples is measured against the exposure
//!    threshold of the stage), taken before step 1; allowed saturation `L(Y) = (1 − w)/w` with `w = smoothstep(0.5, 1, Y)`;
//!    saturation `S = (max − min)/max` becomes `S / (1 + (S/L)⁴)^¼` (unchanged far below the limit,
//!    approaches the limit above it), keeping the brightest layer. Pixels with a layer at its clip level
//!    end up neutral, at the brightness the reconstruction gave them.
//!
//! No density or clip map enters step 2, and no reconstructed value is tested against a threshold.

use rayon::prelude::*;

use super::propagate::clean_saturation;
use super::{HighlightRestoration, HighlightStats};
use crate::{Control, Error, Result};

/// Radius of the border zone around clipped samples that feeds the chroma offset.
const BORDER: usize = 3;

impl HighlightRestoration {
    /// Highlight reconstruction "opposed" + soft saturation limit (see the module documentation).
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn restore_opposed(
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
        if left < 0
            || top < 0
            || right < left
            || bottom < top
            || right as usize >= width
            || bottom as usize >= height
        {
            return Err(Error::InvalidData("invalid Merrill highlight rectangle"));
        }
        let mut stats = HighlightStats::default();
        let (left, top, right, bottom) =
            (left as usize, top as usize, right as usize, bottom as usize);
        let clipped = clean_saturation(sat, width, height, &mut stats);
        let threshold = self.threshold() as f32;

        // Clip level per layer: median of the clipped samples of that layer
        let clip: [Option<f32>; 3] = std::array::from_fn(|c| {
            let mut v: Vec<f32> = (0..width * height)
                .into_par_iter()
                .filter(|&i| clipped[i] & (1 << c) != 0)
                .map(|i| data[(i / width) * row_stride + (i % width) * 3 + c])
                .collect();
            if v.is_empty() {
                return None;
            }
            let mid = v.len() / 2;
            Some(*v.select_nth_unstable_by(mid, f32::total_cmp).1)
        });

        // Opposed mean of layer `c` at (x, y): 3×3 means per layer, cube roots, mean of the other two, cubed.
        let opposed = |data: &[f32], x: usize, y: usize, c: usize| -> f32 {
            let mut m = [0.0f32; 3];
            for yy in y - 1..=y + 1 {
                for xx in x - 1..=x + 1 {
                    let p = &data[yy * row_stride + xx * 3..yy * row_stride + xx * 3 + 3];
                    for k in 0..3 {
                        m[k] += p[k];
                    }
                }
            }
            let r = m.map(|v| (v / 9.0).max(0.0).cbrt());
            let (a, b) = ((c + 1) % 3, (c + 2) % 3);
            (0.5 * (r[a] + r[b])).powi(3)
        };

        // brightness for step 2, from the values before the reconstruction: highest level relative to
        // the clip level of its layer (a layer without clipped samples against the exposure threshold)
        let reference: [f32; 3] = clip.map(|c| c.unwrap_or(threshold).max(1.0));
        let level: Vec<f32> = (0..height)
            .into_par_iter()
            .flat_map_iter(|y| {
                let data = &*data;
                (0..width).map(move |x| {
                    let p = &data[y * row_stride + 3 * x..y * row_stride + 3 * x + 3];
                    (0..3).map(|c| p[c] / reference[c]).fold(0.0f32, f32::max)
                })
            })
            .collect();
        let mut filled = 0usize;
        if clipped.iter().any(|&b| b != 0) {
            control.check()?;
            // Border zone per layer: within BORDER pixels of a clipped sample of that layer
            let near = dilate(&clipped, width, height, BORDER);
            let (x0, x1) = (left.max(1), right.min(width - 2));
            let (y0, y1) = (top.max(1), bottom.min(height - 2));
            // 1. chroma offset per layer from unclipped border pixels
            let sums: [(f64, usize); 3] = {
                let data = &*data;
                let rows: Vec<[(f64, usize); 3]> = (y0..=y1)
                    .into_par_iter()
                    .map(|y| {
                        let mut s = [(0.0f64, 0usize); 3];
                        for x in x0..=x1 {
                            let i = y * width + x;
                            if clipped[i] != 0 || near[i] == 0 {
                                continue;
                            }
                            for c in 0..3 {
                                let Some(cl) = clip[c] else { continue };
                                let v = data[y * row_stride + x * 3 + c];
                                if near[i] & (1 << c) == 0 || v < 0.2 * cl || v >= cl {
                                    continue;
                                }
                                s[c].0 += f64::from(v - opposed(data, x, y, c));
                                s[c].1 += 1;
                            }
                        }
                        s
                    })
                    .collect();
                let mut t = [(0.0f64, 0usize); 3];
                for r in rows {
                    for c in 0..3 {
                        t[c].0 += r[c].0;
                        t[c].1 += r[c].1;
                    }
                }
                t
            };
            let offset: [f32; 3] =
                sums.map(|(s, n)| if n > 0 { (s / n as f64) as f32 } else { 0.0 });
            // 2. fill clipped samples (read from a copy: the opposed means use the unfilled values)
            let source = data[..].to_vec();
            filled = data
                .par_chunks_mut(row_stride)
                .enumerate()
                .filter(|(y, _)| (y0..=y1).contains(y))
                .map(|(y, row)| {
                    let mut n = 0;
                    for x in x0..=x1 {
                        let bits = clipped[y * width + x];
                        if bits == 0 {
                            continue;
                        }
                        for c in 0..3 {
                            if bits & (1 << c) == 0 {
                                continue;
                            }
                            let estimate = (opposed(&source, x, y, c) + offset[c]).min(32767.0);
                            if estimate > row[3 * x + c] {
                                row[3 * x + c] = estimate;
                                n += 1;
                            }
                        }
                    }
                    n
                })
                .sum();
            if std::env::var_os("X3F_HL_DEBUG").is_some() {
                eprintln!("highlights opposed: threshold {threshold} clip {clip:?} offset {offset:?} border {:?} filled {filled}", sums.map(|s| s.1));
            }
        }
        stats.dense_pixels = filled as u32;
        control.check()?;
        // 3. soft saturation limit by brightness (relative to the clip levels)
        const Y_LO: f32 = 0.5;
        data.par_chunks_mut(row_stride)
            .enumerate()
            .filter(|(y, _)| (top..=bottom).contains(y))
            .for_each(|(y, row)| {
                for x in left..=right {
                    let p = &mut row[3 * x..3 * x + 3];
                    let t = ((level[y * width + x] - Y_LO) / (1.0 - Y_LO)).clamp(0.0, 1.0);
                    let w = t * t * (3.0 - 2.0 * t);
                    if w <= 0.0 {
                        continue;
                    }
                    let hi = p[0].max(p[1]).max(p[2]);
                    let lo = p[0].min(p[1]).min(p[2]);
                    if hi <= 0.0 || hi <= lo {
                        continue;
                    }
                    let s = (hi - lo) / hi;
                    let factor = if w >= 1.0 {
                        0.0
                    } else {
                        let limit = (1.0 - w) / w;
                        let q = s / limit;
                        1.0 / (1.0 + q * q * q * q).powf(0.25)
                    };
                    for v in p.iter_mut() {
                        *v = hi - (hi - *v) * factor;
                    }
                }
            });
        Ok(stats)
    }
}

/// Bits of all clipped layers within `r` pixels (square window), per pixel (separable maximum).
fn dilate(bits: &[u8], width: usize, height: usize, r: usize) -> Vec<u8> {
    let mut horiz = vec![0u8; width * height];
    horiz
        .par_chunks_mut(width)
        .zip(bits.par_chunks(width))
        .for_each(|(out, row)| {
            for x in 0..width {
                let (a, b) = (x.saturating_sub(r), (x + r).min(width - 1));
                out[x] = row[a..=b].iter().fold(0, |m, &v| m | v);
            }
        });
    let mut out = vec![0u8; width * height];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(r), (y + r).min(height - 1));
        for x in 0..width {
            let mut m = 0u8;
            for yy in a..=b {
                m |= horiz[yy * width + x];
            }
            row[x] = m;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_clipped_layer_from_opposed_layers() {
        let (w, h) = (24, 24);
        // reddish light: layer 0 = 3000, layer 1 = 2000, layer 2 = 2400; layer 1 clipped at 2000 in the centre
        let mut data: Vec<f32> = (0..w * h).flat_map(|_| [3000.0, 2000.0, 2400.0]).collect();
        let mut sat = vec![0u8; w * h * 3];
        for y in 8..16 {
            for x in 8..16 {
                let i = y * w + x;
                data[i * 3] = 6000.0;
                data[i * 3 + 2] = 4800.0;
                sat[i * 3 + 1] = 1;
            }
        }
        let r = HighlightRestoration::new(0.0, 0.0);
        let control = Control::none();
        r.restore_opposed(
            &mut data,
            w,
            h,
            w * 3,
            &mut sat,
            [1, 1, w as i32 - 2, h as i32 - 2],
            control,
        )
        .unwrap();
        let c = (12 * w + 12) * 3;
        // the clipped layer rises above its clip level
        assert!(data[c + 1] > 2000.0, "{:?}", &data[c..c + 3]);
    }
}
