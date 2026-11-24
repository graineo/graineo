//! Foveon detail bands ("fine detail", "structure") for the Quattro and Classic bases.
//!
//! The Merrill base splits the luminance residual inside Sigma's noise reduction (x3fuse-core
//! `residual::texture_bands`). The Quattro and Classic paths have no such residual stage that is
//! reachable from here, so the same band split runs on the finished, noise-reduced base: the
//! luminance is taken to the log domain, split with a separable binomial [1 4 6 4 1]/16 à trous
//! pyramid (steps 1, 2, 4, 8 px) into a fine band (the first two levels, up to about 3 px), a
//! structure band (the next two levels, about 3–30 px) and the broad rest, and recombined as
//! `exp(rest + fine·g(fine) + structure·g(structure))`. Unlike the Merrill residual, the bands of the
//! finished image also carry the edges, so the gain fades back to 1 for large band values
//! (`g(d) = 1 + (gain − 1)·exp(−d²/2τ²)`, τ = 4 × the median absolute value of the band in this image):
//! texture, pores and remaining noise follow the slider, edges, eyelashes and contours stay as they are
//! (no blur at 0, no halos at 200). The RGB values are scaled by the luminance ratio, so colour is kept. The gains act additively on log bands (a power of the band ratio), never as
//! `1 + s·(r − 1)`. `[1, 1]` is never applied (callers skip it, bit-identical).
//!
//! Origin: own code, same construction as the Merrill bands in x3fuse-core (written for graineo).

use rayon::prelude::*;

const STEPS: [usize; 4] = [1, 2, 4, 8];
const FINE_STEPS: usize = 2;
const BINOMIAL5: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];

fn pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, step: usize, horizontal: bool) {
    dst.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let mut sum = 0.0_f32;
            for (k, wt) in BINOMIAL5.iter().enumerate() {
                let d = (k as isize - 2) * step as isize;
                let (sx, sy) = if horizontal {
                    ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                } else {
                    (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                };
                sum += wt * src[sy * w + sx];
            }
            *out = sum;
        }
    });
}

/// Applies the band gains `[fine, structure]` (each 0…2, `[1, 1]` = unchanged) to an interleaved linear
/// RGB image of `w × h` pixels.
pub fn apply(data: &mut [f32], w: usize, h: usize, gains: [f32; 2]) {
    if gains == [1.0, 1.0] || w == 0 || h == 0 || data.len() < w * h * 3 || !gains.iter().all(|g| g.is_finite() && *g >= 0.0) {
        return;
    }
    let lum = |p: &[f32]| 0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2];
    // floor relative to the mean brightness: near black the log would amplify noise without limit
    let mean = data.par_chunks(3).map(|p| f64::from(lum(p).max(0.0))).sum::<f64>() / (w * h) as f64;
    let eps = (mean as f32 * 1e-3).max(1e-6);
    let log: Vec<f32> = data.par_chunks(3).take(w * h).map(|p| (lum(p).max(0.0) + eps).ln()).collect();
    let mut low = log.clone();
    let mut tmp = vec![0.0_f32; w * h];
    let mut fine_low = Vec::new();
    for (i, &step) in STEPS.iter().enumerate() {
        pass(&low, &mut tmp, w, h, step, true);
        pass(&tmp, &mut low, w, h, step, false);
        if i + 1 == FINE_STEPS {
            fine_low = low.clone();
        }
    }
    let [f, s] = gains;
    // scale of the texture per band: median absolute band value over a sparse sample of the image
    let tau = |band: &dyn Fn(usize) -> f32| {
        let mut v: Vec<f32> = (0..w * h).step_by(7).map(|i| band(i).abs()).collect();
        let m = v.len() / 2;
        let med = *v.select_nth_unstable_by(m, |a, b| a.total_cmp(b)).1;
        (4.0 * med).max(1e-4)
    };
    let tf = tau(&|i| log[i] - fine_low[i]);
    let ts = tau(&|i| fine_low[i] - low[i]);
    let gain = |d: f32, g: f32, t: f32| 1.0 + (g - 1.0) * (-0.5 * (d / t) * (d / t)).exp();
    data.par_chunks_mut(3).take(w * h).enumerate().for_each(|(i, p)| {
        let fine = log[i] - fine_low[i];
        let structure = fine_low[i] - low[i];
        let k = (low[i] + gain(fine, f, tf) * fine + gain(structure, s, ts) * structure - log[i]).exp();
        let y = lum(p).max(0.0);
        // scale the luminance plus floor by k, give the floor back: factor on RGB
        let factor = if y > 0.0 { (((y + eps) * k - eps) / y).max(0.0) } else { 1.0 };
        for c in p.iter_mut() {
            *c *= factor;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: usize, h: usize) -> Vec<f32> {
        (0..w * h).flat_map(|i| {
            let v = 0.2 + 0.1 * ((i * 7919 % 13) as f32 / 13.0);
            [v, v * 0.9, v * 1.1]
        }).collect()
    }

    #[test]
    fn unit_gains_change_nothing() {
        let a = image(40, 30);
        let mut b = a.clone();
        apply(&mut b, 40, 30, [1.0, 1.0]);
        assert_eq!(a, b);
    }

    #[test]
    fn zero_fine_smooths_and_keeps_colour_ratio() {
        let a = image(40, 30);
        let mut b = a.clone();
        apply(&mut b, 40, 30, [0.0, 1.0]);
        let var = |d: &[f32]| {
            let g: Vec<f32> = d.chunks(3).map(|p| p[1]).collect();
            let m = g.iter().sum::<f32>() / g.len() as f32;
            g.iter().map(|v| (v - m) * (v - m)).sum::<f32>()
        };
        assert!(var(&b) < 0.5 * var(&a));
        for p in b.chunks(3) {
            assert!((p[0] / p[1] - 1.0 / 0.9).abs() < 1e-3);
        }
    }
}
