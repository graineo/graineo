//! Lanczos-3 resampling of a plane (moved from `raw::lens`, 10.11.2026): table with 257 sub-pixel steps × 6 weights,
//! interpolated linearly between the steps (since 12.11.2026), overshoot limited to the four nearest pixels.

pub const LZ_A: usize = 3;
pub const LZ_STEPS: usize = 256;

/// Lanczos-3 weights (`LZ_STEPS + 1` steps × 6, each row normalised to sum 1).
pub fn lanczos_table() -> &'static [[f32; 2 * LZ_A]] {
    static T: std::sync::OnceLock<Vec<[f32; 2 * LZ_A]>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let l = |x: f32| -> f32 {
            if x.abs() < 1e-6 {
                return 1.0;
            }
            if x.abs() >= LZ_A as f32 {
                return 0.0;
            }
            let px = std::f32::consts::PI * x;
            LZ_A as f32 * px.sin() * (px / LZ_A as f32).sin() / (px * px)
        };
        (0..=LZ_STEPS)
            .map(|s| {
                let f = s as f32 / LZ_STEPS as f32;
                let mut w: [f32; 2 * LZ_A] = std::array::from_fn(|j| l(f - (j as f32 - (LZ_A as f32 - 1.0))));
                let sum: f32 = w.iter().sum();
                w.iter_mut().for_each(|v| *v /= sum);
                w
            })
            .collect()
    })
}

/// Lanczos-3 of a plane at position `q`; overshoot is limited to the minimum/maximum of the four
/// nearest pixels (no ringing at hard edges).
#[inline]
pub fn sample(src: &[f32], w: usize, h: usize, q: [f32; 2], tab: &[[f32; 2 * LZ_A]]) -> f32 {
    const T: usize = 2 * LZ_A;
    let (fx, fy) = (q[0].floor(), q[1].floor());
    let (ix, iy) = (fx as isize, fy as isize);
    // weights linearly between the two nearest table steps: the result follows the position continuously (with the
    // nearest step alone, positions differing by 1e-4 px – CPU exact against the interpolated grid of the GPU – could
    // land on neighbouring steps and differ visibly at hard edges)
    let lerp = |f: f32| -> [f32; T] {
        let t = f * LZ_STEPS as f32;
        let i = (t as usize).min(LZ_STEPS - 1);
        let a = t - i as f32;
        let (r0, r1) = (&tab[i], &tab[i + 1]);
        std::array::from_fn(|k| r0[k] + a * (r1[k] - r0[k]))
    };
    let (wx, wy) = (lerp(q[0] - fx), lerp(q[1] - fy));
    let (wi, hi) = (w as isize, h as isize);
    let x0 = ix - (LZ_A as isize - 1);
    let y0 = iy - (LZ_A as isize - 1);
    let inner = x0 >= 0 && y0 >= 0 && x0 + T as isize <= wi && y0 + T as isize <= hi;
    let mut acc = 0f32;
    let mut lo = f32::MAX;
    let mut hi_ = f32::MIN;
    for j in 0..T {
        let yy = (y0 + j as isize).clamp(0, hi - 1) as usize;
        let z: [f32; T] = if inner {
            let s = &src[yy * w + x0 as usize..][..T];
            std::array::from_fn(|i| s[i])
        } else {
            std::array::from_fn(|i| src[yy * w + (x0 + i as isize).clamp(0, wi - 1) as usize])
        };
        let mut s = 0f32;
        for i in 0..T {
            s += wx[i] * z[i];
        }
        acc += wy[j] * s;
        if j == LZ_A - 1 || j == LZ_A {
            lo = lo.min(z[LZ_A - 1]).min(z[LZ_A]);
            hi_ = hi_.max(z[LZ_A - 1]).max(z[LZ_A]);
        }
    }
    acc.clamp(lo, hi_)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanczos_integer_is_identity() {
        let tab = lanczos_table();
        let w = 16;
        let src: Vec<f32> = (0..w * w).map(|i| i as f32).collect();
        let v = sample(&src, w, w, [7.0, 5.0], tab);
        assert!((v - (5 * w + 7) as f32).abs() < 1e-3);
    }
}
