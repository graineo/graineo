//! Classic chain: the coring table ("tanh table") shared by the chroma and colour steps.
//!
//! For each of the three layers the table maps a signed code difference `x` to a limited value
//! `a · tanh(k · x / a)` (amplitude `a`, slope `k`), faded out towards the end of the table by a
//! raised cosine so that large differences are not touched at all. The table is odd-symmetric
//! (the entry at −n/2 repeats the one at −n/2 + 1), stored in 16-bit integers with +0.5
//! rounding, and indexed by the low bits of the signed code, so codes beyond ±n/2 wrap around.
//! Its size (512 … 8192 entries) grows with the largest amplitude-to-slope ratio.

/// The tables of the three layers.
pub struct TanhTable {
    /// number of entries per layer (a power of two)
    pub n: usize,
    t: [Vec<i16>; 3],
}

impl TanhTable {
    /// Entry for layer `c` at code `v` (wraps with the table size).
    #[inline]
    pub fn get(&self, c: usize, v: i32) -> i32 {
        i32::from(self.t[c][(v as usize) & (self.n - 1)])
    }
}

/// Builds the tables for amplitudes `amp` and slopes `steep` (all positive).
pub fn build(amp: [f64; 3], steep: [f64; 3]) -> TanhTable {
    let ratio = (0..3).map(|c| amp[c] / steep[c]).fold(f64::MIN, f64::max);
    let factor = (0.5 / ratio) * 0.5;
    let s = (6.2853 / factor) as i32;
    let n: usize = if s <= 0x200 {
        0x200
    } else if s <= 0x400 {
        0x400
    } else if s <= 0x800 {
        0x800
    } else if s <= 0x1000 {
        0x1000
    } else {
        0x2000
    };
    let half = n / 2;
    let mask = n - 1;
    let mut t = [vec![0i16; n], vec![0i16; n], vec![0i16; n]];
    for x in 0..half {
        let xf = x as f64;
        let outp = if factor * xf < 3.1416 { ((factor * xf).cos() + 1.0) * 0.5 } else { 0.0 };
        for c in 0..3 {
            let v = (steep[c] * xf / amp[c]).tanh() * amp[c] * outp + 0.5;
            t[c][x & mask] = v as i32 as i16;
        }
    }
    for c in 0..3 {
        for x in 1..half {
            t[c][(0usize.wrapping_sub(x)) & mask] = t[c][x & mask].wrapping_neg();
        }
        t[c][half] = t[c][half + 1];
    }
    TanhTable { n, t }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_and_bounded() {
        let t = build([60.0, 40.0, 80.0], [1.0; 3]);
        assert!(t.n.is_power_of_two() && t.n >= 512);
        for c in 0..3 {
            assert_eq!(t.get(c, 0), 0);
            for x in 1..t.n as i32 / 2 - 1 {
                assert_eq!(t.get(c, -x), -t.get(c, x));
            }
            // small differences pass nearly unchanged
            assert_eq!(t.get(c, 3), 3);
        }
        // never above the amplitude (plus rounding)
        assert!(t.get(2, 50) <= 80);
    }
}
