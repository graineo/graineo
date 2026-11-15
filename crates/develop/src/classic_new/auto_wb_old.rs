//! Classic chain: the older form of the automatic white balance from the image (used
//! instead of [`super::auto_wb`] when bit 0x1000000 of `SkipStepFlags` is set).
//!
//! Outline:
//! 1. A central window (10 % border on every side) is sampled every fourth row: the mean per layer
//!    (minus the mean dark drift) and the maximum per layer. The maximum is limited to the
//!    saturation level and to twenty times the mean; it sets the scale of a 32³ histogram.
//! 2. Blocks of 4 × 4 pixels (every fourth row and column of the window): when the post polynomial
//!    of the block maximum stays below the saturation level, the post polynomial of the mean of the
//!    four centre pixels, minus the dark drift interpolated for that row, is summed (grey world 1)
//!    and counted in the histogram.
//! 3. From the histogram two more estimates: the bin centres weighted with the bin counts (grey
//!    world 2) and with the square root of the counts ("squashed", so that large uniform areas do
//!    not dominate; with a small correction for the root's offset); the top bins are weighted down.
//! 4. Long exposures: the mean of the three estimates; otherwise the squashed one alone.
//! 5. Constraint in the working space: with aperture and time known the ratios R/G and B/G
//!    (× 10000) are kept inside a polygon that depends on the scene brightness (blended between
//!    ranges); otherwise the estimate is only kept within loose bounds. Red is then scaled by 0.98
//!    and blue by 1.08 and the result taken back to layer space.

use super::auto_wb::{to_max, position, mul_mv, polynomial, tr32, POLY_DARK, POLY_BRIGHT, POLY_MEAN};
use super::calibration::{cam2rimm, inv, Mat};
use super::correction::Drift;

/// Inputs (at this point of the chain).
#[derive(Clone, Debug)]
pub struct Input<'a> {
    /// sensor codes (`w × h × 3`, as 16-bit values) after the input factor, before the row correction
    pub image: &'a [i32],
    pub w: usize,
    pub h: usize,
    /// post polynomial (27 numbers)
    pub post_polynomial: [f32; 27],
    /// saturation level per layer (integer, long-exposure factor applied)
    pub saturation: [i32; 3],
    /// aperture (f-number), exposure time in seconds, exposure compensation
    pub aperture: f64,
    pub time: f64,
    pub correction: f64,
    /// matrix layer codes → XYZ before the neutral-point scaling (the RIMM conversion is done inside)
    pub matrix: Mat,
    /// capture ISO / sensor ISO (held in single precision)
    pub iso_factor: f64,
    /// the dark drift (before flag 64); `top` belongs to the first row, `bottom` to the last
    pub drift: Drift,
    /// exposure not longer than `MaxConstrainTime`
    pub short: bool,
}

/// Intermediate results (for checks).
#[derive(Clone, Copy, Debug, Default)]
pub struct Between {
    pub mean: [f64; 3],
    pub max: [i32; 3],
    pub count: u32,
    pub gray_world1: [f64; 3],
    pub gray_world2: [f64; 3],
    pub squeezed: [f64; 3],
    pub pre_limit: [f64; 3],
    pub bright: f64,
    pub limited: bool,
}

/// Neutral point (largest component 1); non-finite or zero components mean "no estimate".
pub fn neutral_point(e: &Input) -> [f64; 3] {
    compute(e).map_or([f64::NAN; 3], |(n, _)| n)
}

/// Neutral point with the intermediate results.
pub fn compute(e: &Input) -> Option<([f64; 3], Between)> {
    if e.w < 4 || e.h < 4 || e.image.len() < e.w * e.h * 3 || e.w > u32::MAX as usize / 8 || e.h > u32::MAX as usize / 8 {
        return None;
    }
    let mut z = Between::default();
    let m = cam2rimm(&e.matrix, &[-1.0; 3], e.iso_factor, 0.0);
    let (w, h) = (e.w as u32, e.h as u32);
    let px = |x: u32, y: u32, c: usize| -> i32 { i32::from(e.image[(y as usize * e.w + x as usize) * 3 + c] as i16) };
    // window: 10 % border
    let rx = tr32(f64::from(w) * 0.1) as u32;
    let ry = tr32(f64::from(h) * 0.1) as u32;
    let top = e.drift.top.mean;
    let bottom = e.drift.bottom.mean;

    // 1. mean and maximum of every fourth row
    let mut sum = [0.0f64; 3];
    let mut count = 0u32;
    let mut max = [0i32; 3];
    if ry < h.wrapping_sub(ry) && rx < w.wrapping_sub(rx) {
        let mut y = ry;
        while y < h - ry {
            for x in rx..w - rx {
                for c in 0..3 {
                    let v = px(x, y, c);
                    sum[c] += f64::from(v);
                    if v > max[c] {
                        max[c] = v;
                    }
                }
            }
            count = count.wrapping_add(w - 2 * rx);
            y += 4;
        }
    }
    let n = f64::from(count);
    let mean: [f64; 3] = std::array::from_fn(|c| sum[c] / n - (top[c] + bottom[c]) * 0.5);
    for c in 0..3 {
        if e.saturation[c] < max[c] {
            max[c] = e.saturation[c];
        }
        let g = tr32(mean[c] * 20.0);
        if g < max[c] {
            max[c] = g;
        }
    }
    z.mean = mean;
    z.max = max;
    let scale: [i32; 3] = std::array::from_fn(|c| tr32((536870911.0 / f64::from(max[c])).floor()));

    // 2. blocks of 4 × 4: grey world 1 and the histogram
    let sat: [f64; 3] = e.saturation.map(f64::from);
    let mut hist = vec![0u32; 32 * 32 * 32];
    let mut gw1 = [0.0f64; 3];
    let mut counted = 0u32;
    let rows_end = h.wrapping_sub(3).wrapping_sub(ry);
    let columns_end = w.wrapping_sub(3).wrapping_sub(rx);
    let mut y = ry;
    while y < rows_end {
        // dark drift of this row (interpolated between top and bottom, fraction in single precision)
        let t = f64::from((f64::from(y) / f64::from(h - 1)) as f32);
        let dark: [i32; 3] = std::array::from_fn(|c| tr32((bottom[c] - top[c]) * t + top[c]));
        let mut x = rx;
        while x < columns_end {
            let mut mx = [0i32; 3];
            for dy in 0..4 {
                for dx in 0..4 {
                    for c in 0..3 {
                        let v = px(x + dx, y + dy, c);
                        if v > mx[c] {
                            mx[c] = v;
                        }
                    }
                }
            }
            let p = polynomial(&e.post_polynomial, mx.map(f64::from));
            let p: [f64; 3] = p.map(|v| f64::from(tr32(v)));
            if p[0] < sat[0] && p[1] < sat[1] && p[2] < sat[2] {
                let s: [f64; 3] = std::array::from_fn(|c| {
                    let k = px(x + 1, y + 1, c) + px(x + 2, y + 1, c) + px(x + 1, y + 2, c) + px(x + 2, y + 2, c);
                    f64::from(k) * 0.25
                });
                let q = polynomial(&e.post_polynomial, s);
                let d: [i32; 3] = std::array::from_fn(|c| tr32(q[c]).wrapping_sub(dark[c]));
                let mut slot = [0u32; 3];
                for c in 0..3 {
                    gw1[c] += f64::from(d[c]);
                    if d[c] > 0 {
                        slot[c] = (d[c].wrapping_mul(scale[c]).wrapping_add(1) >> 24) as u32;
                    }
                }
                if (slot[0] | slot[1] | slot[2]) < 32 {
                    hist[(slot[2] * 1024 + slot[1] * 32 + slot[0]) as usize] += 1;
                    counted = counted.wrapping_add(1);
                }
            }
            x += 4;
        }
        y += 4;
    }
    z.count = counted;

    // 3. grey world 2 and the squashed estimate from the histogram
    let weight: [f64; 32] = std::array::from_fn(|k| {
        let t = 3 * k as i32;
        if t > 64 {
            f64::from((t - 64) * (t - 64)) * -0.0009765625 + 1.0
        } else {
            1.0
        }
    });
    let l = f64::from(counted / 10000 + 2);
    let w0 = l.sqrt();
    let mut gw2 = [0.0f64; 3];
    let mut sq = [0.0f64; 3];
    for b in 0..32usize {
        let mb = b as f64 + 0.5;
        for g in 0..32usize {
            let mg = g as f64 + 0.5;
            for r in 0..32usize {
                let k = hist[b * 1024 + g * 32 + r];
                if k == 0 {
                    continue;
                }
                let mr = r as f64 + 0.5;
                let k = f64::from(k);
                let wg = weight[r] * weight[g] * weight[b];
                let a = wg * ((l + k).sqrt() - w0);
                let v = wg * k;
                sq[0] += a * mr;
                sq[1] += a * mg;
                sq[2] += a * mb;
                gw2[0] += v * mr;
                gw2[1] += v * mg;
                gw2[2] += v * mb;
            }
        }
    }
    for c in 0..3 {
        sq[c] *= f64::from(max[c]);
        gw2[c] *= f64::from(max[c]);
    }
    // offset of the root for a single count, taken through the inverse matrix
    let mi = inv(&m);
    let x = f64::from((max[1] as u32).wrapping_mul(32));
    let dd = (l + l).sqrt() - w0;
    for i in 0..3 {
        let row = (mi[i * 3] * x + mi[i * 3 + 1] * x) + mi[i * 3 + 2] * x;
        sq[i] += dd * row;
    }
    let gw1 = to_max(gw1);
    let gw2 = to_max(gw2);
    let sq = to_max(sq);
    z.gray_world1 = gw1;
    z.gray_world2 = gw2;
    z.squeezed = sq;

    // 4. long exposures: mean of the three
    let estimate: [f64; 3] = if e.short { sq } else { std::array::from_fn(|c| ((sq[c] + gw1[c]) + gw2[c]) / 3.0) };
    z.pre_limit = estimate;

    // 5. constraint
    let (n, bright, limited) = clamp_limit(e, &m, estimate);
    z.bright = bright;
    z.limited = limited;
    Some((to_max(n), z))
}

/// Constraint of the estimate; returns the layer-space result, the brightness value and whether
/// the polygon constraint was used.
fn clamp_limit(e: &Input, m: &Mat, s: [f64; 3]) -> ([f64; 3], f64, bool) {
    let mut limited = e.short;
    let bv = if 0.5 > e.aperture || 0.0 >= e.time {
        limited = false;
        0.0
    } else {
        let av_tv = ((e.aperture * e.aperture) / e.time).log10() * 3.322 + e.correction;
        ((e.iso_factor * 100.0) / 3.125).log10() / -0.3010299956639812 + av_tv
    };
    let v = mul_mv(m, &s);
    let (x, y, z) = if !limited {
        // loose bounds only
        let a = if v[0] <= 0.1 { 0.1 } else { v[0] };
        let b = if v[1] <= 0.1 { 0.1 } else { v[1] };
        let c = if 0.1 > v[2] { 0.1 } else { v[2] };
        let s = 3.0 / ((b + a) + c);
        let a = s * a;
        let b = s * b;
        let a = if a <= 0.6 { 0.6 } else { a };
        let b = if b <= 0.9 { 0.9 } else { b };
        let sc = s * c;
        let c = if 0.5 > sc { 0.5 } else { sc };
        let f = 1.0 / b;
        (f * a, f * b, f * c)
    } else {
        let p = (tr32((v[0] / v[1]) * 10000.0), tr32((v[2] / v[1]) * 10000.0));
        let q = if 3.2 >= bv {
            im_polygon(&POLY_DARK, p)
        } else if bv > 5.7 {
            if 5.8 >= bv {
                im_polygon(&POLY_MEAN, p)
            } else if 8.3 >= bv {
                mix(im_polygon(&POLY_MEAN, p), im_polygon(&POLY_BRIGHT, p), (bv + -5.8) / 2.500000000000001)
            } else {
                im_polygon(&POLY_BRIGHT, p)
            }
        } else {
            mix(im_polygon(&POLY_DARK, p), im_polygon(&POLY_MEAN, p), (bv + -3.2) / 2.5)
        };
        (f64::from(q.0) / 10000.0, 1.0, f64::from(q.1) / 10000.0)
    };
    let n = mul_mv(&inv(m), &[0.98 * x, y, z * 1.08]);
    (n, bv, limited)
}

/// Blend of two points with the fraction `t` (each part rounded on its own).
fn mix(a: (i32, i32), b: (i32, i32), t: f64) -> (i32, i32) {
    let u = 1.0 - t;
    (
        tr32(f64::from(a.0) * u + 0.5).wrapping_add(tr32(f64::from(b.0) * t + 0.5)),
        tr32(f64::from(a.1) * u + 0.5).wrapping_add(tr32(f64::from(b.1) * t + 0.5)),
    )
}

/// Inside or on the polygon: unchanged; outside: nearest point of the outline.
fn im_polygon(poly: &[(i32, i32)], p: (i32, i32)) -> (i32, i32) {
    if position(poly, p) == b'o' { to_area(poly, p) } else { p }
}

/// Nearest point of the outline: per edge the start point, the rounded projection or the end
/// point, whichever applies; the first strictly nearer one wins.
fn to_area(poly: &[(i32, i32)], p: (i32, i32)) -> (i32, i32) {
    let n = poly.len();
    if n <= 1 {
        return (0, 0);
    }
    let rel: Vec<(i32, i32)> = poly.iter().map(|v| (v.0.wrapping_sub(p.0), v.1.wrapping_sub(p.1))).collect();
    let mut best = 1e20f64;
    let mut res = (0, 0);
    for i in 0..n - 1 {
        let (ax, ay) = rel[i];
        let (bx, by) = rel[i + 1];
        let q0 = (ax.wrapping_add(p.0), ay.wrapping_add(p.1));
        let q1 = (bx.wrapping_add(p.0), by.wrapping_add(p.1));
        let sx = q1.0.wrapping_sub(q0.0);
        let sy = q1.1.wrapping_sub(q0.1);
        let dot = sx.wrapping_mul(ax.wrapping_neg()).wrapping_sub(ay.wrapping_mul(sy));
        let (cand, dx2, dy2) = if dot <= 0 {
            let fx = f64::from(ax.wrapping_neg());
            (q0, fx * fx, ay.wrapping_mul(ay))
        } else {
            let len2 = sx.wrapping_mul(sx).wrapping_add(sy.wrapping_mul(sy));
            if len2 <= dot {
                let fx = f64::from(bx.wrapping_neg());
                (q1, fx * fx, by.wrapping_mul(by))
            } else {
                let r = f64::from(dot) / f64::from(len2);
                let kx = q0.0.wrapping_add(tr32(f64::from(sx) * r + 0.5));
                let ky = q0.1.wrapping_add(tr32(f64::from(sy) * r + 0.5));
                let fx = f64::from(p.0.wrapping_sub(kx));
                let ey = p.1.wrapping_sub(ky);
                ((kx, ky), fx * fx, ey.wrapping_mul(ey))
            }
        };
        let d = (f64::from(dy2) + dx2).sqrt();
        if best > d {
            res = cand;
        }
        if d < best {
            best = d;
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize) -> Vec<i32> {
        let mut image = vec![0i32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = 800 + ((x * 7 + y * 3) % 2000) as i32;
                for c in 0..3 {
                    image[(y * w + x) * 3 + c] = v;
                }
            }
        }
        image
    }

    #[test]
    fn auto_wb_old_grey_image() {
        let (w, h) = (640usize, 480usize);
        let image = gray(w, h);
        let m: Mat = [0.4124, 0.3576, 0.1805, 0.2126, 0.7152, 0.0722, 0.0193, 0.1192, 0.9505];
        for (aperture, time, short) in [(8.0, 0.004, true), (2.8, 0.05, true), (4.0, 0.02, true), (5.6, 0.008, true), (11.0, 0.002, true), (8.0, 2.0, false), (0.0, 0.0, true)] {
            let e = Input { image: &image, w, h, post_polynomial: [0.0; 27], saturation: [16000; 3], aperture, time, correction: 0.0, matrix: m, iso_factor: 1.0, drift: Drift::default(), short };
            let (n, z) = compute(&e).unwrap();
            eprintln!("grey f/{aperture} {time}s kurz={short}: {n:?} bv={:.2} begrenzt={} {z:?}", z.bright, z.limited);
            assert!(n.iter().all(|v| v.is_finite() && *v > 0.0), "{n:?}");
            let mx = n.iter().cloned().fold(0.0, f64::max);
            assert!((mx - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn auto_wb_old_outline() {
        // inside stays, outside goes onto the outline
        assert_eq!(im_polygon(&POLY_BRIGHT, (10000, 10000)), (10000, 10000));
        let q = im_polygon(&POLY_BRIGHT, (20000, 10000));
        assert_ne!(position(&POLY_BRIGHT, q), b'o', "{q:?}");
        let q = im_polygon(&POLY_DARK, (0, 0));
        assert_ne!(position(&POLY_DARK, q), b'o', "{q:?}");
    }

    /// Real file (only when present): plausibility on an SD10 daylight scene.
    #[test]
    fn auto_wb_old_sd10_sample() {
        use super::super::calibration;
        let f = crate::classic_new::check::classic_dir().join("sd10_pixls_RAW_SIGMA_SD10.X3F");
        if !f.exists() {
            eprintln!("skipped: no sample file");
            return;
        }
        let x = x3f::X3fFile::open(&f).unwrap();
        let (camf, props) = (x.camf().unwrap(), x.properties().unwrap());
        let (img, w, h) = crate::classic_new::chain::raw_values(&x).unwrap();
        let d = calibration::Data { camf: &camf };
        let k = calibration::calc(&calibration::Input { camf: &camf, props: &props, wb: "Auto", adobe: false, auto_neutral: None, gray_mean: None }).unwrap();
        let sat = d.triple("SaturationLevel", [16383.0; 3]).map(|v| v as i32);
        let mut pp = [0.0f32; 27];
        if let Some(v) = d.values("PostPolyMatrix").filter(|v| v.len() >= 27) {
            for i in 0..27 {
                pp[i] = v[i] as f32;
            }
        }
        // the image and the dark drift at this point of the chain
        let mut img = img;
        let (pre, _) = super::super::correction::prepare(&mut img, w, h, &d, &k, k.skip).unwrap();
        let drift = pre.drift_raw;
        eprintln!("drift {drift:?}");
        let prop = |n: &str| props.get(n).and_then(|v| v.trim().parse::<f64>().ok());
        let aperture = prop("APERTURE").unwrap_or(8.0);
        let time = prop("EXPTIME").map(|t| t / 1e6).filter(|t| *t > 0.0).unwrap_or(1.0 / 125.0);
        eprintln!("SD10 {w}×{h} sat={sat:?} f/{aperture} {time}s stored neutral={:?}", k.color.neutral);
        for short in [true, false] {
            let e = Input { image: &img, w, h, post_polynomial: pp, saturation: sat, aperture, time, correction: 0.0, matrix: k.color.m_raw, iso_factor: 1.0, drift, short };
            let t = std::time::Instant::now();
            let (n, z) = compute(&e).unwrap();
            eprintln!("SD10 older auto neutral (kurz={short}): {n:?} ({:.1} ms)\n  {z:?}", t.elapsed().as_secs_f64() * 1000.0);
            assert!(n.iter().all(|v| v.is_finite() && (0.7..=1.1).contains(v)), "{n:?}");
        }
    }
}
