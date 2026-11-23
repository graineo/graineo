//! Downscaling for the export (selectable filter, default Lanczos3, 16 bit): first vertically, then horizontally, with the same weights, the same
//! order of summation and the same rounding as `image::imageops::resize` with `FilterType::Lanczos3`, which the export
//! used before. The processor path is bit-identical to it but runs in parallel (rows); the GPU path (setting „Grafikkarte
//! verwenden“) computes the same sums in f32 on the graphics card and deviates at most by rounding (±1 of 65535).

use rayon::prelude::*;

use crate::Rgb16;

/// Filter taps of one output row/column: first source index and normalized weights.
pub(crate) struct Taps {
    pub left: Vec<u32>,
    /// `n` weights per output position (zero-padded to the longest)
    pub w: Vec<f32>,
    pub n: usize,
}

fn sinc(t: f32) -> f32 {
    let a = t * std::f32::consts::PI;
    if t == 0.0 {
        1.0
    } else {
        a.sin() / a
    }
}

/// Resampling filter of the export (separable, applied to the gamma-encoded finished image, like the previous Lanczos3
/// path: no linearisation, so every filter behaves identically in that respect). Weights are normalised to a sum of 1
/// per output position and the support grows with the reduction ratio (filter scaling), so shrinking never aliases more
/// than the kernel itself allows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeFilter {
    /// Lanczos, 3 lobes (support 3): very sharp, slight ringing at hard edges. The previous behaviour.
    #[default]
    Sharp,
    /// Catmull-Rom cubic (B 0, C 1/2, support 2): nearly as sharp, shorter, less ringing.
    Balanced,
    /// Hann-windowed sinc (support 3, the "Hanning" filter of XnView/ImageMagick): softer, hardly any ringing.
    Soft,
    /// Mitchell-Netravali cubic (B = C = 1/3, support 2): smooth, no visible ringing, suppresses moire and noise.
    Smooth,
}

impl ResizeFilter {
    pub const ALL: [ResizeFilter; 4] = [ResizeFilter::Sharp, ResizeFilter::Balanced, ResizeFilter::Soft, ResizeFilter::Smooth];

    /// Half width of the kernel in output pixels.
    pub fn support(self) -> f32 {
        match self {
            ResizeFilter::Sharp | ResizeFilter::Soft => 3.0,
            ResizeFilter::Balanced | ResizeFilter::Smooth => 2.0,
        }
    }

    /// Kernel value at distance `x` (symmetric, zero outside the support).
    pub fn kernel(self, x: f32) -> f32 {
        let x = x.abs();
        if x >= self.support() {
            return 0.0;
        }
        match self {
            ResizeFilter::Sharp => sinc(x) * sinc(x / 3.0),
            ResizeFilter::Soft => sinc(x) * (0.5 + 0.5 * (std::f32::consts::PI * x / 3.0).cos()),
            ResizeFilter::Balanced => {
                if x < 1.0 {
                    (1.5 * x - 2.5) * x * x + 1.0
                } else {
                    ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0
                }
            }
            ResizeFilter::Smooth => {
                if x < 1.0 {
                    (7.0 * x * x * x - 12.0 * x * x + 16.0 / 3.0) / 6.0
                } else {
                    (-7.0 / 3.0 * x * x * x + 12.0 * x * x - 20.0 * x + 32.0 / 3.0) / 6.0
                }
            }
        }
    }

    /// Stable key for settings and export presets.
    pub fn key(self) -> &'static str {
        match self {
            ResizeFilter::Sharp => "sharp",
            ResizeFilter::Balanced => "balanced",
            ResizeFilter::Soft => "soft",
            ResizeFilter::Smooth => "smooth",
        }
    }

    pub fn from_key(k: &str) -> Option<ResizeFilter> {
        ResizeFilter::ALL.into_iter().find(|f| f.key() == k)
    }
}

/// Weights for `src` → `dst` samples (as the image crate: support 3 × ratio when shrinking, normalized to 1).
pub(crate) fn taps(src: usize, dst: usize, filter: ResizeFilter) -> Taps {
    let ratio = src as f32 / dst as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let support = filter.support() * sratio;
    let mut rows: Vec<(u32, Vec<f32>)> = Vec::with_capacity(dst);
    for out in 0..dst {
        let input = (out as f32 + 0.5) * ratio;
        let left = ((input - support).floor() as i64).clamp(0, src as i64 - 1);
        let right = ((input + support).ceil() as i64).clamp(left + 1, src as i64);
        let input = input - 0.5;
        let mut ws: Vec<f32> = (left..right).map(|i| filter.kernel((i as f32 - input) / sratio)).collect();
        let sum: f32 = ws.iter().fold(0.0, |a, b| a + b);
        for w in ws.iter_mut() {
            *w /= sum;
        }
        rows.push((left as u32, ws));
    }
    let n = rows.iter().map(|r| r.1.len()).max().unwrap_or(1);
    let mut left = Vec::with_capacity(dst);
    let mut w = vec![0f32; dst * n];
    for (k, (l, ws)) in rows.into_iter().enumerate() {
        left.push(l);
        w[k * n..k * n + ws.len()].copy_from_slice(&ws);
    }
    Taps { left, w, n }
}

/// Number of non-zero-padded taps of position `k` (the padding must not be read: it may lie outside the image).
fn len(t: &Taps, k: usize, src: usize) -> usize {
    t.n.min(src - t.left[k] as usize)
}

/// Lanczos3 on the processor (print layout, tests): [`resize_cpu`] with [`ResizeFilter::Sharp`].
pub fn lanczos3_cpu(img: &Rgb16, nw: usize, nh: usize) -> Rgb16 {
    resize_cpu(img, nw, nh, ResizeFilter::Sharp)
}

/// `filter` to `nw`×`nh` on the processor (parallel over rows); with `Sharp` bit-identical to the image crate's Lanczos3.
pub fn resize_cpu(img: &Rgb16, nw: usize, nh: usize, filter: ResizeFilter) -> Rgb16 {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || nw == 0 || nh == 0 {
        return Rgb16 { width: nw, height: nh, data: vec![0; nw * nh * 3] };
    }
    let vt = taps(h, nh, filter);
    let ht = taps(w, nw, filter);
    // vertical: f32 intermediate image w × nh (the sums run over the taps in ascending order, as in the image crate)
    let mut tmp = vec![0f32; w * nh * 3];
    tmp.par_chunks_mut(w * 3).enumerate().for_each(|(oy, row)| {
        let left = vt.left[oy] as usize;
        for i in 0..len(&vt, oy, h) {
            let wt = vt.w[oy * vt.n + i];
            let src = &img.data[(left + i) * w * 3..(left + i + 1) * w * 3];
            for (t, &s) in row.iter_mut().zip(src) {
                *t += s as f32 * wt;
            }
        }
    });
    // horizontal: clamped to 0 … 65535, rounded half away from zero
    let mut data = vec![0u16; nw * nh * 3];
    data.par_chunks_mut(nw * 3).enumerate().for_each(|(y, out)| {
        let src = &tmp[y * w * 3..(y + 1) * w * 3];
        for ox in 0..nw {
            let left = ht.left[ox] as usize;
            let mut t = [0f32; 3];
            for i in 0..len(&ht, ox, w) {
                let wt = ht.w[ox * ht.n + i];
                let p = &src[(left + i) * 3..(left + i) * 3 + 3];
                t[0] += p[0] * wt;
                t[1] += p[1] * wt;
                t[2] += p[2] * wt;
            }
            for c in 0..3 {
                out[ox * 3 + c] = t[c].clamp(0.0, 65535.0).round() as u16;
            }
        }
    });
    Rgb16 { width: nw, height: nh, data }
}

/// Export scaling to `nw`×`nh` with the chosen filter. Measured 12.11.2026 (M1 Pro, 24–50 MP → 2048 px): processor path in parallel
/// 20–42 ms and bit-identical to the previous export, GPU path 47–129 ms (uploading the 16-bit image costs more than the
/// computation) – so the processor computes; the GPU path only with `GRAINEO_RESIZE_GPU=1` (measurements).
pub fn resize(img: &Rgb16, nw: usize, nh: usize, filter: ResizeFilter) -> Rgb16 {
    if std::env::var_os("GRAINEO_RESIZE_GPU").is_some() {
        if let Some(out) = crate::resize_gpu::resize(img, nw, nh, filter) {
            return out;
        }
    }
    resize_cpu(img, nw, nh, filter)
}

/// The previous export scaling (image crate, one thread) – reference for tests and measurements.
pub fn lanczos3_image_crate(img: Rgb16, size: Option<crate::export::OutputSize>) -> Rgb16 {
    let Some(g) = size else { return img };
    let (w, h) = g.target_size(img.width, img.height);
    if (w, h) == (img.width, img.height) {
        return img;
    }
    let Some(buf) = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_raw(img.width as u32, img.height as u32, img.data) else {
        return Rgb16 { width: 0, height: 0, data: Vec::new() };
    };
    let small = image::imageops::resize(&buf, w as u32, h as u32, image::imageops::FilterType::Lanczos3);
    Rgb16 { width: w, height: h, data: small.into_raw() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image(w: usize, h: usize) -> Rgb16 {
        let mut s = 12345u32;
        let data = (0..w * h * 3)
            .map(|i| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                // gradient plus noise plus hard edges (overshoot of the filter is clamped)
                let x = (i / 3) % w;
                let base = if x % 37 < 3 { 65535 } else { (x * 65535 / w) as u32 };
                (base / 2 + (s >> 17)).min(65535) as u16
            })
            .collect();
        Rgb16 { width: w, height: h, data }
    }

    #[test]
    fn processor_path_is_bit_identical_to_the_image_crate() {
        for (w, h, nw, nh) in [(301, 203, 100, 67), (640, 480, 213, 160), (97, 300, 31, 96), (50, 40, 49, 39), (64, 48, 128, 96)] {
            let img = test_image(w, h);
            let buf = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_raw(w as u32, h as u32, img.data.clone()).unwrap();
            let reference = image::imageops::resize(&buf, nw as u32, nh as u32, image::imageops::FilterType::Lanczos3).into_raw();
            let own = lanczos3_cpu(&img, nw, nh);
            assert_eq!(own.data, reference, "{w}×{h} → {nw}×{nh}");
        }
    }

    #[test]
    fn gpu_path_matches_the_processor_path() {
        let img = test_image(1201, 803);
        for f in ResizeFilter::ALL {
            let cpu = resize_cpu(&img, 400, 267, f);
            let Some(gpu) = crate::resize_gpu::resize(&img, 400, 267, f) else { return }; // no GPU here
            let max = cpu.data.iter().zip(&gpu.data).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
            eprintln!("{f:?}: largest GPU/CPU difference {max} of 65535");
            assert!(max <= 2, "{f:?}: largest difference {max}");
        }
    }

    #[test]
    fn kernels_are_symmetric_normalised_and_vanish_outside() {
        for f in ResizeFilter::ALL {
            // Mitchell (B = C = 1/3) is not interpolating: its centre value is (6 - 2B) / 6 = 8/9
            let centre = if matches!(f, ResizeFilter::Smooth) { 8.0 / 9.0 } else { 1.0 };
            assert!((f.kernel(0.0) - centre).abs() < 1e-6, "{f:?} at 0");
            for k in 1..40 {
                let x = k as f32 * 0.07;
                assert_eq!(f.kernel(x), f.kernel(-x));
            }
            assert_eq!(f.kernel(f.support()), 0.0);
            assert_eq!(f.kernel(f.support() + 0.5), 0.0);
            // interpolating kernels are 0 at the other integers; Mitchell and Hann are not required to
            if matches!(f, ResizeFilter::Sharp | ResizeFilter::Soft | ResizeFilter::Balanced) {
                for n in 1..f.support() as i32 {
                    assert!(f.kernel(n as f32).abs() < 1e-6, "{f:?} at {n}");
                }
            }
            // partition of unity: integer-spaced samples of the kernel sum to 1 at every phase (no DC change)
            for ph in 0..10 {
                let d = ph as f32 / 10.0;
                let sum: f32 = (-4..=4).map(|n| f.kernel(n as f32 + d)).sum();
                // windowed sincs are only nearly a partition of unity (the taps are renormalised per position)
                let tol = match f { ResizeFilter::Soft => 0.06, ResizeFilter::Sharp => 0.01, _ => 1e-3 };
                assert!((sum - 1.0).abs() < tol, "{f:?} phase {d}: {sum}");
            }
        }
    }

    #[test]
    fn weights_sum_to_one_and_flat_images_stay_flat() {
        for f in ResizeFilter::ALL {
            for (src, dst) in [(1000, 333), (1000, 500), (97, 31), (50, 49)] {
                let t = taps(src, dst, f);
                for k in 0..dst {
                    let sum: f32 = t.w[k * t.n..(k + 1) * t.n].iter().sum();
                    assert!((sum - 1.0).abs() < 1e-5, "{f:?} {src}->{dst} at {k}: {sum}");
                }
            }
            let img = Rgb16 { width: 300, height: 200, data: vec![40000; 300 * 200 * 3] };
            let out = resize_cpu(&img, 77, 51, f);
            assert!(out.data.iter().all(|&v| v == 40000), "{f:?} flat image changed");
        }
    }

    #[test]
    fn filter_keys_roundtrip() {
        for f in ResizeFilter::ALL {
            assert_eq!(ResizeFilter::from_key(f.key()), Some(f));
            assert_eq!(serde_json::from_value::<ResizeFilter>(serde_json::to_value(f).unwrap()).unwrap(), f);
        }
        assert_eq!(ResizeFilter::default(), ResizeFilter::Sharp);
    }
}
