//! Additional, optional scan levels. Negative scans are not normalised automatically by the core.
use super::*;
fn decode(v: u16) -> f32 {
    srgb_decode(v as f32 / 65535.)
}
fn encode(v: f32) -> u16 {
    let x = v.clamp(0., 1.);
    let y = if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1. / 2.4) - 0.055
    };
    (y * 65535. + 0.5) as u16
}
pub fn scan_bounds(im: &Rgb16) -> (f32, f32) {
    let mut histogram = [0usize; 4096];
    for p in im.data.chunks_exact(3) {
        let y = 0.2126 * decode(p[0]) + 0.7152 * decode(p[1]) + 0.0722 * decode(p[2]);
        histogram[(y.clamp(0., 1.) * 4095.).round() as usize] += 1;
    }
    let total = im.width.saturating_mul(im.height);
    if total == 0 {
        return (0., 1.);
    }
    let at = |wanted: usize| {
        let mut sum = 0;
        for (i, n) in histogram.iter().enumerate() {
            sum += n;
            if sum > wanted {
                return i as f32 / 4095.;
            }
        }
        1.
    };
    let tail = (total as f64 * 0.001).floor() as usize;
    let lo = at(tail);
    let hi = at(total - 1 - tail);
    if hi - lo < 0.01 {
        (0., 1.)
    } else {
        (lo, hi)
    }
}
pub(super) fn apply_scan_levels(data: &mut [u16], f: &FilmSettings, scan: bool) {
    if !scan || (f.scan_black == 0. && f.scan_white == 1.) {
        return;
    }
    let black = f.scan_black.clamp(0., 0.95);
    let white = f.scan_white.clamp(black + 0.01, 1.);
    use rayon::prelude::*;
    data.par_iter_mut()
        .for_each(|p| *p = encode((decode(*p) - black) / (white - black)));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_percentiles_ignore_sparse_outliers_and_flat_fields() {
        let im = Rgb16 {
            width: 1000,
            height: 1,
            data: (0..1000)
                .flat_map(|i| {
                    [if i == 0 {
                        0
                    } else if i == 999 {
                        65535
                    } else {
                        20000 + (i as u16 * 20)
                    }; 3]
                })
                .collect(),
        };
        let (a, b) = scan_bounds(&im);
        assert!(a > 0.01 && b < 0.99 && b > a);
        let mut bytes = im.data.clone();
        apply_scan_levels(&mut bytes, &FilmSettings::default(), true);
        assert_eq!(im.data, bytes);
        let f = FilmSettings {
            scan_black: a,
            scan_white: b,
            ..Default::default()
        };
        apply_scan_levels(&mut bytes, &f, true);
        assert_eq!(&bytes[..3], &[0; 3]);
        assert_eq!(&bytes[bytes.len() - 3..], &[65535; 3]);
        let im = Rgb16 {
            width: 10,
            height: 10,
            data: vec![30000; 300],
        };
        assert_eq!(scan_bounds(&im), (0., 1.));
    }
}
