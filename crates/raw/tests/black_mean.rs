//! The demosaic keeps the mean of the deepest shadows: on a flat mosaic around black with Gaussian read noise
//! (half of the values negative), every output channel has the mean of its photosites. Clipping the negative noise
//! would lift it by about 0.4 σ.

use raw::development::{demosaic_mosaic, Demosaic};
use raw::Sensor;

/// Small deterministic Gaussian generator (xorshift + Box-Muller).
struct Rng(u64);

impl Rng {
    fn u(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn g(&mut self) -> f64 {
        (-2.0 * self.u().ln()).sqrt() * (std::f64::consts::TAU * self.u()).cos()
    }
}

fn check(sensor: Sensor, pat: [[u8; 6]; 6], d: Demosaic) {
    let (w, h) = (384, 384);
    let sigma = 0.002;
    // means per colour: R slightly above black, G at black, B slightly below (as after an imperfect black level)
    let mean = [0.0005f64, 0.0, -0.0003];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let m: Vec<f32> = (0..w * h).map(|i| (mean[pat[(i / w) % 6][(i % w) % 6] as usize] + sigma * rng.g()) as f32).collect();
    let out = demosaic_mosaic(&m, w, h, pat, d, sensor);
    assert!(out.iter().flatten().all(|v| v.is_finite()));
    let mut s = [0f64; 3];
    let mut n = 0f64;
    for y in 24..h - 24 {
        for x in 24..w - 24 {
            for c in 0..3 {
                s[c] += out[y * w + x][c] as f64;
            }
            n += 1.0;
        }
    }
    for c in 0..3 {
        let bias = s[c] / n - mean[c];
        assert!(bias.abs() < 0.08 * sigma, "{d:?} channel {c}: bias {bias:+.6} = {:+.3} σ", bias / sigma);
    }
}

#[test]
fn rcd_keeps_black_mean() {
    let mut pat = [[0u8; 6]; 6];
    for (y, row) in pat.iter_mut().enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            *v = [[0, 1], [1, 2]][y % 2][x % 2];
        }
    }
    check(Sensor::Bayer, pat, Demosaic::Rcd);
    check(Sensor::Bayer, pat, Demosaic::Bilinear);
}

#[test]
fn markesteijn_keeps_black_mean() {
    // X-Trans pattern (X-T2 and later)
    let pat: [[u8; 6]; 6] = [[1, 1, 0, 1, 1, 2], [1, 1, 2, 1, 1, 0], [2, 0, 1, 0, 2, 1], [1, 1, 2, 1, 1, 0], [1, 1, 0, 1, 1, 2], [0, 2, 1, 2, 0, 1]];
    check(Sensor::XTrans, pat, Demosaic::Markesteijn3);
    check(Sensor::XTrans, pat, Demosaic::Markesteijn1);
}
