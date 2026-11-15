//! Classic chain, step "speckle removal": single pixels that are brighter (or darker) than all of
//! their neighbours are replaced from the neighbourhood. Works in place on interleaved signed
//! 16-bit codes (stored as `i32`), row by row from the top: the row above has already been
//! processed, the row below and the left/right neighbours in the row are still the original
//! values (read from a copy of the row).
//!
//! There are two ways. The new way judges a pixel by its brightness
//! (red + 2 · green + blue) and then replaces it from the one brightest (or darkest) neighbour;
//! it is used unless the exposure was long (over the CAMF limit) or in bulb mode. The old way
//! works on each layer on its own.

/// Which way to use (see the module description).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// brightness based, one donor neighbour (short exposures)
    New,
    /// per layer, extreme of all eight neighbours (long exposures, bulb mode)
    Old,
}

#[inline]
fn narrow(v: i32) -> i32 {
    v as i16 as i32
}

#[inline]
fn px(row: &[i32], x: usize) -> [i32; 3] {
    [row[x * 3], row[x * 3 + 1], row[x * 3 + 2]]
}

/// Brightness weight of the new way: 1 : 2 : 1.
#[inline]
fn bright(p: [i32; 3]) -> i32 {
    p[0] + p[2] + 2 * p[1]
}

/// Whole image; `w` × `h` pixels, three codes each. Rows 1 … h − 2, columns 1 … w − 2.
pub fn run(img: &mut [i32], w: usize, h: usize, variant: Variant) {
    if w < 3 || h < 3 {
        return;
    }
    let stride = w * 3;
    for y in 1..h - 1 {
        let (top, rest) = img.split_at_mut(y * stride);
        let (center, bottom) = rest.split_at_mut(stride);
        let top = &top[(y - 1) * stride..];
        let bottom = &bottom[..stride];
        let orig = center.to_vec();
        match variant {
            Variant::New => row_new(top, &orig, center, bottom, w),
            Variant::Old => row_old(top, &orig, center, bottom, w),
        }
    }
}

/// Moves the donor `d` towards the centre `c`: a layer in which the donor is on the wrong side
/// of the centre only follows by a quarter of the difference, the other layers are shifted by the
/// same amount. `down` is true for a bright centre (donor layers above the centre are the
/// restricted ones), false for a dark centre.
fn take_over(c: [i32; 3], d: [i32; 3], down: bool) -> [i32; 3] {
    let [r0, g0, b0] = c;
    let [a, mut g, mut b] = d;
    // first layer
    let restricted = if down { a > r0 } else { a < r0 };
    let mut r;
    if restricted {
        let q = (r0 - a) >> 2;
        r = r0 - q;
        g -= q;
        b -= q;
    } else {
        r = a;
    }
    // second layer
    let restricted = if down { g > g0 } else { g < g0 };
    let mut gg = if restricted {
        let q = (2 * g0 - 2 * g) >> 2;
        r -= q;
        b -= q;
        g0 - q
    } else {
        g
    };
    // third layer
    let restricted = if down { b > b0 } else { b < b0 };
    let bb = if restricted {
        let q = (b0 - b) >> 2;
        r -= q;
        gg -= q;
        b0 - q
    } else {
        b
    };
    [narrow(r), narrow(gg), narrow(bb)]
}

fn row_new(top: &[i32], orig: &[i32], center: &mut [i32], bottom: &[i32], w: usize) {
    for x in 1..w - 1 {
        let c = px(orig, x);
        let lc = bright(c);
        let l = px(orig, x - 1);
        let r = px(orig, x + 1);
        let (ll, lr) = (bright(l), bright(r));
        let u = px(top, x);
        let d = px(bottom, x);
        let (lu, ld) = (bright(u), bright(d));
        let diag = [px(top, x - 1), px(top, x + 1), px(bottom, x - 1), px(bottom, x + 1)];
        let ld4 = diag.map(bright);
        // cross neighbours in the order left, right, up, down
        let cross = [(ll, l), (lr, r), (lu, u), (ld, d)];
        if ll < lc && lr < lc {
            // brighter than both horizontal neighbours
            let m4 = ll.max(lr).max(lu).max(ld).max(5);
            let overbright = m4 < lc && {
                let s = (lc >> 1) + 10;
                c.iter().any(|v| *v > s)
            };
            if !(m4 * 2 < lc || overbright) {
                continue;
            }
            let md = ld4.iter().copied().max().unwrap().max(5);
            if !((overbright && md < lc) || md * 2 < lc) {
                continue;
            }
            let donor = if m4 < md {
                let i = ld4.iter().position(|v| *v == md).unwrap_or(3);
                diag[i]
            } else {
                cross.iter().find(|(v, _)| *v == m4).map(|(_, p)| *p).unwrap_or(d)
            };
            let o = take_over(c, donor, true);
            center[x * 3..x * 3 + 3].copy_from_slice(&o);
        } else if lc < ll && lc < lr {
            // darker than both horizontal neighbours: must be darker than all eight
            let min_ud = lu.min(ld);
            if !(lc < min_ud) {
                continue;
            }
            let min_cross = min_ud.min(ll).min(lr);
            let min_diag = ld4.iter().copied().min().unwrap();
            if !(lc < min_diag) {
                continue;
            }
            let donor = if min_diag < min_cross {
                let i = ld4.iter().position(|v| *v == min_diag).unwrap_or(3);
                diag[i]
            } else {
                cross.iter().find(|(v, _)| *v == min_cross).map(|(_, p)| *p).unwrap_or(d)
            };
            let o = take_over(c, donor, false);
            center[x * 3..x * 3 + 3].copy_from_slice(&o);
        }
    }
}

fn row_old(top: &[i32], orig: &[i32], center: &mut [i32], bottom: &[i32], w: usize) {
    for ch in 0..3 {
        for x in 1..w - 1 {
            let v = orig[x * 3 + ch];
            let l = orig[(x - 1) * 3 + ch];
            let r = orig[(x + 1) * 3 + ch];
            let n6 = [top[x * 3 + ch], bottom[x * 3 + ch], top[(x - 1) * 3 + ch], top[(x + 1) * 3 + ch], bottom[(x - 1) * 3 + ch], bottom[(x + 1) * 3 + ch]];
            if v > l && v > r {
                let m = n6.iter().copied().max().unwrap();
                if v > m {
                    center[x * 3 + ch] = m.max(l).max(r);
                }
            } else if v < l && v < r {
                let m = n6.iter().copied().min().unwrap();
                if v < m {
                    center[x * 3 + ch] = m.min(l).min(r);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, v: [i32; 3]) -> Vec<i32> {
        (0..w * h).flat_map(|_| v).collect()
    }

    #[test]
    fn bright_speckle_is_replaced() {
        let (w, h) = (5, 5);
        let mut img = flat(w, h, [100, 100, 100]);
        let i = (2 * w + 2) * 3;
        img[i..i + 3].copy_from_slice(&[400, 400, 400]);
        let mut a = img.clone();
        run(&mut a, w, h, Variant::Old);
        assert_eq!(&a[i..i + 3], &[100, 100, 100]);
        run(&mut img, w, h, Variant::New);
        assert_eq!(&img[i..i + 3], &[100, 100, 100]);
    }

    #[test]
    fn dark_speckle_is_replaced() {
        let (w, h) = (5, 5);
        let mut img = flat(w, h, [100, 100, 100]);
        let i = (2 * w + 2) * 3;
        img[i..i + 3].copy_from_slice(&[10, 10, 10]);
        let mut a = img.clone();
        run(&mut a, w, h, Variant::Old);
        assert_eq!(&a[i..i + 3], &[100, 100, 100]);
        run(&mut img, w, h, Variant::New);
        assert_eq!(&img[i..i + 3], &[100, 100, 100]);
    }

    #[test]
    fn flat_and_small_images_stay() {
        let mut img = flat(6, 6, [7, 8, 9]);
        let before = img.clone();
        run(&mut img, 6, 6, Variant::New);
        run(&mut img, 6, 6, Variant::Old);
        assert_eq!(img, before);
        let mut tiny = flat(2, 2, [1, 2, 3]);
        run(&mut tiny, 2, 2, Variant::New);
    }

    #[test]
    fn edge_is_not_a_speckle() {
        // a step edge is brighter than one side only
        let (w, h) = (6, 4);
        let mut img: Vec<i32> = (0..h).flat_map(|_| (0..w).flat_map(|x| if x < 3 { [50; 3] } else { [500; 3] })).collect();
        let before = img.clone();
        run(&mut img, w, h, Variant::New);
        run(&mut img, w, h, Variant::Old);
        assert_eq!(img, before);
    }
}
