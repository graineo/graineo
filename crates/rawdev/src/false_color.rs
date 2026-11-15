//! False color suppression for X-Trans: demosaicing leaves single pixels with a color that does not occur in the scene at very fine,
//! bright edges (backlit hair, twigs, glitter). As in common raw developers ("color smoothing"), only
//! the color of these outliers is matched to their surroundings; luminance and sharpness stay untouched. The reference is the color median of the 5×5 neighborhood; in strongly colorful areas
//! the filter has no effect (real color edges and colored details are preserved).
use develop::Rgb16;
use rayon::prelude::*;

#[inline]
fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Match the color of outliers to their surroundings. `strength` 0 … 1 (0 = off). Computes on the GPU when allowed
/// (setting "Grafikkarte verwenden"), otherwise with [`suppress_cpu`].
pub fn suppress(img: &mut Rgb16, strength: f32) {
    if strength <= 0.0 || img.width < 5 || img.height < 5 {
        return;
    }
    let (w, h) = (img.width, img.height);
    if crate::gpu::false_color::suppress(&mut img.data, w, h, strength).is_some() {
        return;
    }
    let t = std::time::Instant::now();
    suppress_cpu(img, strength);
    crate::timings::lap("CPU Falschfarben", t.elapsed());
}

/// [`suppress`] on the processor (reference of the GPU version, bit-exact test mode).
pub fn suppress_cpu(img: &mut Rgb16, strength: f32) {
    let (w, h) = (img.width, img.height);
    if strength <= 0.0 || w < 5 || h < 5 {
        return;
    }
    let strength = strength.clamp(0.0, 1.0);
    // luminance and color differences in the encoded image (0 … 1)
    let n = w * h;
    let mut y = vec![0f32; n];
    let mut u = vec![0f32; n];
    let mut v = vec![0f32; n];
    y.par_iter_mut().zip(u.par_iter_mut().zip(v.par_iter_mut())).enumerate().for_each(|(i, (yy, (uu, vv)))| {
        let (r, g, b) = (img.data[i * 3] as f32 / 65535.0, img.data[i * 3 + 1] as f32 / 65535.0, img.data[i * 3 + 2] as f32 / 65535.0);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        *yy = l;
        *uu = b - l;
        *vv = r - l;
    });
    let data = &mut img.data;
    data.par_chunks_mut(w * 3).enumerate().for_each(|(row, out)| {
        if row < 2 || row + 2 >= h {
            return;
        }
        for x in 2..w - 2 {
            let i = row * w + x;
            let mut nu = [0f32; 25];
            let mut nv = [0f32; 25];
            let mut k = 0;
            for dy in 0..5 {
                for dx in 0..5 {
                    let j = (row + dy - 2) * w + (x + dx - 2);
                    nu[k] = u[j];
                    nv[k] = v[j];
                    k += 1;
                }
            }
            let (mu, mv) = (median25(nu), median25(nv));
            let dev = ((u[i] - mu).powi(2) + (v[i] - mv).powi(2)).sqrt();
            // Real fine color (neon lettering, red threads) has area: if the neighborhood median is already strongly colorful and the pixel only
            // a bit more so, it stays. False colors are single colorful pixels or thin lines in an almost neutral neighborhood.
            let colorful_surround = (mu * mu + mv * mv).sqrt();
            let t = smooth(0.015, 0.05, dev) * (1.0 - smooth(0.08, 0.2, colorful_surround)) * strength;
            if t <= 0.0 {
                continue;
            }
            let (nu2, nv2) = (u[i] + (mu - u[i]) * t, v[i] + (mv - v[i]) * t);
            let l = y[i];
            let (r, b) = (l + nv2, l + nu2);
            let g = (l - 0.299 * r - 0.114 * b) / 0.587;
            let o = &mut out[x * 3..x * 3 + 3];
            o[0] = (r.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            o[1] = (g.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            o[2] = (b.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        }
    });
}

/// Median of 25 values.
#[inline]
fn median25(mut v: [f32; 25]) -> f32 {
    *v.select_nth_unstable_by(12, |a, b| a.total_cmp(b)).1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize, v: u16) -> Rgb16 {
        Rgb16 { width: w, height: h, data: vec![v; w * h * 3] }
    }

    #[test]
    fn single_colored_outlier_becomes_neutral() {
        let mut img = gray(9, 9, 30000);
        let i = (4 * 9 + 4) * 3;
        img.data[i] = 55000; // pink dots: red high, blue slightly high
        img.data[i + 2] = 45000;
        suppress_cpu(&mut img, 1.0);
        let (r, g, b) = (img.data[i] as i32, img.data[i + 1] as i32, img.data[i + 2] as i32);
        assert!((r - g).abs() < 6000 && (b - g).abs() < 6000, "{r} {g} {b}");
    }

    #[test]
    fn real_color_edge_stays() {
        let mut img = gray(10, 6, 30000);
        for y in 0..6 {
            for x in 5..10 {
                let i = (y * 10 + x) * 3;
                img.data[i] = 60000; // red area on the right
            }
        }
        let before = img.data.clone();
        suppress(&mut img, 1.0);
        assert_eq!(img.data, before);
    }

    /// GPU against processor on a noisy image with single colored outliers and real color areas: equal up to rounding.
    #[test]
    fn gpu_matches_processor() {
        let (w, h) = (203, 157);
        let mut st = 7u32;
        let mut rnd = || {
            st = st.wrapping_mul(1664525).wrapping_add(1013904223);
            st >> 16
        };
        let mut img = gray(w, h, 30000);
        for (i, v) in img.data.iter_mut().enumerate() {
            let (x, c) = ((i / 3) % w, i % 3);
            *v = (if x > 150 && c == 0 { 52000 } else { 28000 } + rnd() % 3000) as u16;
            if rnd() % 97 == 0 {
                *v = 60000;
            }
        }
        let mut cpu = Rgb16 { width: w, height: h, data: img.data.clone() };
        suppress_cpu(&mut cpu, 0.8);
        let mut gpu = Rgb16 { width: w, height: h, data: img.data.clone() };
        if crate::gpu::false_color::suppress(&mut gpu.data, w, h, 0.8).is_none() {
            return; // no GPU here
        }
        let changed = cpu.data.iter().zip(&img.data).filter(|(a, b)| a != b).count();
        assert!(changed > 100, "test image has too few outliers ({changed})");
        let max = cpu.data.iter().zip(&gpu.data).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(max <= 2, "largest difference {max}");
    }

    #[test]
    fn zero_strength_unchanged() {
        let mut img = gray(5, 5, 20000);
        img.data[36] = 60000;
        let v = img.data.clone();
        suppress(&mut img, 0.0);
        assert_eq!(img.data, v);
    }
}
