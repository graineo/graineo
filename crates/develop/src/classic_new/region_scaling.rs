//! Classic chain, range-based reconstruction (high ISO): reducing the layer image by 2
//! or 4 and enlarging the filtered result again.
//!
//! Reduction: separable, first along each row, then across rows, with a symmetric kernel of 4
//! (factor 2: 1-3-3-1/8) or 8 taps (factor 4); the first output row and column repeat the first
//! input row/column. The last output row(s) can stay empty (zero) when the
//! row loop ends early, and the last output column of the factor-4 variant uses a slightly
//! different pairing of the input columns.
//!
//! Enlargement: linear between neighbouring low-resolution pixels (weights 1/8, 3/8, 5/8, 7/8 or
//! 1/4, 3/4), first along rows, then across rows. The first one (factor 2) or
//! two (factor 4) output rows are a plain copy of the low-resolution buffer read as one long row,
//! and the last output columns and rows that the loops do not reach stay zero.
//!
//! All arithmetic is `f32` in a fixed order (bit-exact results).

/// RGB image with `f32` values, `w × h × 3`.
#[derive(Clone)]
pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub d: Vec<f32>,
}

impl Rgb {
    pub fn new(w: usize, h: usize) -> Rgb {
        Rgb { w, h, d: vec![0.0; w * h * 3] }
    }
    #[inline]
    pub fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.w + x) * 3;
        [self.d[i], self.d[i + 1], self.d[i + 2]]
    }
}

/// Weights of the factor-4 reduction, from the centre outwards.
const C4: [f32; 4] = [0.2188, 0.1563, 0.0938, 0.0313];

#[inline]
fn short(v: i32) -> f32 {
    f32::from(v as i16)
}

/// One input row of the 16-bit layer image as `f32` triples.
fn row16(img: &[i32], w: usize, y: usize) -> Vec<[f32; 3]> {
    (0..w).map(|x| {
        let i = (y * w + x) * 3;
        [short(img[i]), short(img[i + 1]), short(img[i + 2])]
    })
    .collect()
}

/// Horizontal part of the factor-2 reduction of one row.
fn horiz2(x: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let w2 = x.len() & !1;
    let n = x.len() >> 1;
    let mut o = Vec::with_capacity(n);
    if n == 0 {
        return o;
    }
    let f = |a: usize, b: usize, c: usize, d: usize| -> [f32; 3] { std::array::from_fn(|k| (x[a][k] + x[b][k]) * 0.375 + (x[c][k] + x[d][k]) * 0.125) };
    o.push(f(1, 0, 2, 0));
    if w2 - 3 != 1 {
        let mut u = 4usize;
        loop {
            o.push(f(u - 1, u - 2, u, u - 3));
            if !(u - 1 < w2 - 3) {
                break;
            }
            u += 2;
        }
    }
    o.push(f(w2 - 2, w2 - 1, w2 - 3, w2 - 1));
    o.truncate(n);
    o
}

/// Horizontal part of the factor-4 reduction of one row.
fn horiz4(x: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let w4 = x.len() & !3;
    let n = x.len() >> 2;
    let mut o = Vec::with_capacity(n);
    if n == 0 {
        return o;
    }
    let f = |p: [(usize, usize); 4]| -> [f32; 3] {
        std::array::from_fn(|k| (((x[p[0].0][k] + x[p[0].1][k]) * C4[0] + (x[p[1].0][k] + x[p[1].1][k]) * C4[1]) + (x[p[2].0][k] + x[p[2].1][k]) * C4[2]) + (x[p[3].0][k] + x[p[3].1][k]) * C4[3])
    };
    o.push(f([(2, 1), (3, 0), (4, 0), (5, 0)]));
    if w4 - 8 != 0 {
        let mut u = 8usize;
        loop {
            // output centred between u - 3 and u - 2
            o.push(f([(u - 2, u - 3), (u - 1, u - 4), (u, u - 5), (u + 1, u - 6)]));
            if !(u - 2 < w4 - 8) {
                break;
            }
            u += 4;
        }
    }
    // last output column: the outermost columns are paired differently here
    o.push(f([(w4 - 3, w4 - 1), (w4 - 4, w4 - 1), (w4 - 5, w4 - 1), (w4 - 2, w4 - 6)]));
    o.truncate(n);
    o
}

/// Reduces the 16-bit layer image (`w × h × 3`, values as `i16`) by `factor` (1, 2 or 4).
pub fn shrink(img: &[i32], w: usize, h: usize, factor: usize) -> Rgb {
    let (wk, hk) = (w / factor, h / factor);
    let mut out = Rgb::new(wk, hk);
    match factor {
        1 => {
            for (o, v) in out.d.iter_mut().zip(img) {
                *o = short(*v);
            }
        }
        2 => {
            if hk == 0 || wk == 0 {
                return out;
            }
            let rows: Vec<Vec<[f32; 3]>> = (0..h).map(|y| horiz2(&row16(img, w, y))).collect();
            let mut set = |o: usize, a: usize, b: usize, c: usize, d: usize| {
                if o >= hk {
                    return;
                }
                for x in 0..wk {
                    for k in 0..3 {
                        out.d[(o * wk + x) * 3 + k] = (rows[a][x][k] + rows[b][x][k]) * 0.375 + (rows[c][x][k] + rows[d][x][k]) * 0.125;
                    }
                }
            };
            set(0, 1, 0, 2, 0);
            if 3 < h.wrapping_sub(3) {
                let mut o = 1usize;
                let mut next = 3usize;
                loop {
                    next += 2;
                    set(o, 2 * o, 2 * o + 1, 2 * o - 1, 2 * o + 2);
                    next += 2;
                    set(o + 1, 2 * o + 2, 2 * o + 3, 2 * o + 1, 2 * o + 4);
                    o += 2;
                    if !(next < h - 3) {
                        break;
                    }
                }
            }
        }
        _ => {
            if hk == 0 || wk == 0 {
                return out;
            }
            let rows: Vec<Vec<[f32; 3]>> = (0..h).map(|y| horiz4(&row16(img, w, y))).collect();
            let mut set = |o: usize, p: [(usize, usize); 4]| {
                if o >= hk {
                    return;
                }
                for x in 0..wk {
                    for k in 0..3 {
                        let z = |i: usize| rows[p[i].0][x][k] + rows[p[i].1][x][k];
                        out.d[(o * wk + x) * 3 + k] = ((z(0) * C4[0] + z(1) * C4[1]) + z(2) * C4[2]) + z(3) * C4[3];
                    }
                }
            };
            set(0, [(1, 2), (3, 0), (4, 0), (5, 0)]);
            if 6 < h.wrapping_sub(7) {
                let mut o = 1usize;
                let mut next = 6usize;
                loop {
                    for oo in [o, o + 1] {
                        next += 4;
                        let m = 4 * oo;
                        set(oo, [(m + 1, m + 2), (m, m + 3), (m - 1, m + 4), (m - 2, m + 5)]);
                    }
                    o += 2;
                    if !(next < h - 7) {
                        break;
                    }
                }
            }
        }
    }
    out
}

/// Horizontal enlargement of one low-resolution row (`n` pixels) to `factor · n` values; the
/// positions that are not written stay zero.
fn horiz_large(s: &[[f32; 3]], factor: usize) -> Vec<[f32; 3]> {
    let n = s.len();
    let mut o = vec![[0.0f32; 3]; factor * n];
    if n == 0 {
        return o;
    }
    let mix = |a: [f32; 3], b: [f32; 3], wb: f32, wa: f32| -> [f32; 3] { std::array::from_fn(|k| b[k] * wb + a[k] * wa) };
    if factor == 4 {
        o[0] = s[0];
        o[1] = s[0];
        for i in 0..n - 1 {
            let (a, b) = (s[i], s[i + 1]);
            o[4 * i + 2] = mix(a, b, 0.125, 0.875);
            o[4 * i + 3] = mix(a, b, 0.375, 0.625);
            o[4 * i + 4] = mix(a, b, 0.625, 0.375);
            o[4 * i + 5] = mix(a, b, 0.875, 0.125);
        }
    } else {
        o[0] = s[0];
        for i in 0..n - 1 {
            let (a, b) = (s[i], s[i + 1]);
            o[2 * i + 1] = mix(a, b, 0.25, 0.75);
            o[2 * i + 2] = mix(a, b, 0.75, 0.25);
        }
    }
    o
}

/// Enlarges the filtered low-resolution image by `factor` (1, 2 or 4) into a `w × h` image.
pub fn enlarge(small: &Rgb, w: usize, h: usize, factor: usize) -> Rgb {
    let mut out = Rgb::new(w, h);
    let (n, m) = (small.w, small.h);
    if factor == 1 {
        for y in 0..m.min(h) {
            for x in 0..n.min(w) {
                for k in 0..3 {
                    out.d[(y * w + x) * 3 + k] = small.d[(y * n + x) * 3 + k];
                }
            }
        }
        return out;
    }
    let width = factor * n;
    let row = |y: usize| -> Vec<[f32; 3]> { horiz_large(&(0..n).map(|x| small.px(x, y)).collect::<Vec<_>>(), factor) };
    let mut write = |y: usize, v: &[[f32; 3]]| {
        if y >= h {
            return;
        }
        for (x, p) in v.iter().enumerate().take(w) {
            out.d[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(p);
        }
    };
    // first rows: plain copy of the low-resolution buffer read as one long row
    let raw: Vec<[f32; 3]> = (0..width.min(n * m)).map(|i| [small.d[i * 3], small.d[i * 3 + 1], small.d[i * 3 + 2]]).collect();
    let copies = if factor == 4 { 2 } else { 1 };
    for y in 0..copies {
        write(y, &raw);
    }
    if m < 3 || !(1 < m - 2) {
        return out;
    }
    let weights: &[f32] = if factor == 4 { &[0.125, 0.375, 0.625, 0.875] } else { &[0.25, 0.75] };
    let mut before = row(0);
    let mut r = 1usize;
    loop {
        for rr in [r, r + 1] {
            let now = row(rr);
            for (j, wn) in weights.iter().enumerate() {
                let wa = 1.0 - wn;
                let v: Vec<[f32; 3]> = now.iter().zip(&before).map(|(c, p)| std::array::from_fn(|k| c[k] * wn + p[k] * wa)).collect();
                write(factor * rr - factor / 2 + j, &v);
            }
            before = now;
        }
        r += 2;
        if !(r < m - 2) {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_smooth() {
        // a constant image stays constant (apart from rounding) in the written rows
        let (w, h) = (64, 40);
        let img = vec![1000i32; w * h * 3];
        for f in [2usize, 4] {
            let k = shrink(&img, w, h, f);
            for y in 0..k.h - 1 {
                for x in 0..k.w {
                    let p = k.px(x, y);
                    assert!((p[0] - 1000.0).abs() < 1.0, "f={f} ({x},{y}) {p:?}");
                }
            }
        }
    }

    #[test]
    fn enlarge_smooth() {
        let mut k = Rgb::new(16, 10);
        k.d.iter_mut().for_each(|v| *v = 500.0);
        for f in [2usize, 4] {
            let g = enlarge(&k, 16 * f, 10 * f, f);
            let p = g.px(8 * f, 5 * f);
            assert!((p[1] - 500.0).abs() < 1e-3, "{p:?}");
        }
    }
}
