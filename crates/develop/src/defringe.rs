//! Colour fringe removal (defringing) as in common photo apps (lens corrections → manual):
//! one amount and one hue range each for purple/magenta and for green fringes.
//!
//! Foveon images often show purple or green fringes at hard contrast edges (branches against
//! the sky, backlight, highlights) (axial chromatic aberration, "purple fringing"). The lateral
//! chromatic aberration is already corrected in the SPP base; this handles the rest.
//!
//! Method (per pixel, on the finished sRGB image before the look stage):
//! 1. **Hue candidate:** hue within the selected range (soft edges) and enough chroma.
//! 2. **Edge proximity:** brightness span (brightest − darkest value) within a radius of r pixels;
//!    only near strong edges is anything corrected.
//! 3. **Object protection:** a fringe lies *between* two areas that are not fringe-coloured
//!    themselves (white sky and dark branch). If the brightest or the darkest point of the
//!    neighbourhood is itself purple or green (a bright blade of grass against dark soil, a flower),
//!    it is a real object and stays untouched. In addition, the share of
//!    fringe-coloured pixels in a larger window (3 r) counts: fringes are narrow bands, large
//!    purple or green areas are preserved even at their edges.
//! 4. **Correction:** the colour (chroma at equal luminance) is pulled towards the colour of the
//!    surroundings: between the colour of the brightest and the darkest point, depending on the
//!    pixel's brightness. Between white sky and black branch the fringe becomes grey, between red
//!    and white pink; no grey outlines around coloured areas.
//!
//! Why on output-referred values (after the Sigma tone curve, before the look stage): the hue
//! picked on the slider is the one you see; gradients in the gamma-encoded image are
//! perceptually uniform (an edge in the shadows counts as much as one in the highlights); both
//! pipelines (SPP base and our own) deliver the same format here; tone curve, colour mixer
//! and color grading do not change the detection.
//!
//! Resolution-independent like clarity/sharpening ([`crate::look`]): radius in full image pixels,
//! converted to output pixels via [`Geo::scale`]. Crops (loupe, 1:1) compute at the edge
//! with extended border pixels; invisible for a radius of a few pixels.

use rayon::prelude::*;

use crate::look::Geo;
use crate::{Rgb16, Settings, View};

/// Hue range of purple fringes: slider value 0 … 100 → degrees (blue-violet … red).
pub fn purple_hue_deg(v: f32) -> f32 {
    220.0 + 1.4 * v.clamp(0.0, 100.0)
}

/// Hue range of green fringes: slider value 0 … 100 → degrees (orange-yellow … cyan).
pub fn green_hue_deg(v: f32) -> f32 {
    30.0 + 1.5 * v.clamp(0.0, 100.0)
}

/// Minimum distance between the two hue sliders.
pub const MIN_SPAN: f32 = 10.0;

/// Soft margin outside the hue range (degrees).
const FEATHER: f32 = 10.0;

/// Is fringe removal active?
pub fn active(s: &Settings) -> bool {
    s.view == View::Developed && !s.monochrome && (s.defringe_purple_amount > 0.0 || s.defringe_green_amount > 0.0)
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A colour range with an amount.
#[derive(Clone, Copy)]
struct Band {
    lo: f32,
    hi: f32,
    /// Desaturation strength 0 … 1
    k: f32,
}

impl Band {
    fn new(amount: f32, hue: [f32; 2], map: fn(f32) -> f32) -> Option<Self> {
        let a = amount.clamp(0.0, 1.0);
        (a > 0.0).then(|| {
            let (lo, hi) = (hue[0].min(hue[1]), hue[0].max(hue[1]).max(hue[0].min(hue[1]) + MIN_SPAN));
            Self { lo: map(lo), hi: map(hi.min(100.0)), k: (a * 2.0).min(1.0) }
        })
    }

    /// Membership of a hue (degrees) 0 … 1.
    #[inline]
    fn weight(&self, h: f32) -> f32 {
        // Hues just above 0° (red) belong to the upper end of the purple range
        let h = if h < 90.0 && self.hi > 300.0 { h + 360.0 } else { h };
        if h < self.lo {
            (1.0 - (self.lo - h) / FEATHER).max(0.0)
        } else if h > self.hi {
            (1.0 - (h - self.hi) / FEATHER).max(0.0)
        } else {
            1.0
        }
    }
}

/// Hue (degrees) and chroma (max − min) of a pixel in encoded sRGB (0 … 1).
#[inline]
fn hue_chroma(c: [f32; 3]) -> (f32, f32) {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    if d <= 1e-6 {
        return (0.0, 0.0);
    }
    let h = if mx == c[0] {
        60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        60.0 * ((c[2] - c[0]) / d + 2.0)
    } else {
        60.0 * ((c[0] - c[1]) / d + 4.0)
    };
    (h, d)
}

/// Membership (hue × chroma) and strength of the matching range.
#[inline]
fn membership(c: [f32; 3], bands: &[Band]) -> (f32, f32) {
    let (h, chroma) = hue_chroma(c);
    let cw = smoothstep(0.03, 0.10, chroma);
    if cw <= 0.0 {
        return (0.0, 0.0);
    }
    let mut best = (0.0f32, 0.0f32);
    for b in bands {
        let w = b.weight(h) * cw;
        if w > best.0 {
            best = (w, b.k);
        }
    }
    best
}

#[inline]
fn px(p: &[u16]) -> [f32; 3] {
    [p[0] as f32 / 65535.0, p[1] as f32 / 65535.0, p[2] as f32 / 65535.0]
}

/// Largest amount `amount` (0 … 1) over both ranges.
fn max_amount(s: &Settings) -> f32 {
    s.defringe_purple_amount.max(s.defringe_green_amount).clamp(0.0, 1.0)
}

/// Running maximum or minimum over ±`r` (van Herk/Gil-Werman, independent of `r`;
/// at the image border the window reaches at most one block further).
fn running(src: &[u32], r: usize, out: &mut [u32], g: &mut [u32], h: &mut [u32], op: impl Fn(u32, u32) -> u32) {
    let (n, k) = (src.len(), 2 * r + 1);
    for i in 0..n {
        g[i] = if i % k == 0 { src[i] } else { op(g[i - 1], src[i]) };
    }
    for i in (0..n).rev() {
        h[i] = if i % k == k - 1 || i == n - 1 { src[i] } else { op(h[i + 1], src[i]) };
    }
    for x in 0..n {
        out[x] = op(h[x.saturating_sub(r)], g[(x + r).min(n - 1)]);
    }
}

/// Colour differences B − Y and R − Y (−1 … 1) as unsigned bytes.
#[inline]
fn chroma_bytes(v: [f32; 3], l: f32) -> u32 {
    let q = |d: f32| ((d * 127.0).round().clamp(-127.0, 127.0) + 128.0) as u32;
    q(v[2] - l) << 8 | q(v[0] - l)
}

#[inline]
fn chroma_of(packed: u32) -> [f32; 2] {
    [((packed >> 8 & 0xff) as f32 - 128.0) / 127.0, ((packed & 0xff) as f32 - 128.0) / 127.0]
}

/// Rows per strip (each strip computes its own neighbourhood, so memory use stays
/// small and the result is independent of the partitioning).
const STRIP: usize = 64;

/// Remove colour fringes in image `img` (sRGB, position `geo`). Without an amount: nothing (bit-identical).
pub fn apply(img: &mut Rgb16, s: &Settings, geo: Geo) {
    if !active(s) {
        return;
    }
    let (w, h) = (img.width, img.height);
    if w < 3 || h < 3 {
        return;
    }
    let bands: Vec<Band> = [
        Band::new(s.defringe_purple_amount, s.defringe_purple_hue, purple_hue_deg),
        Band::new(s.defringe_green_amount, s.defringe_green_hue, green_hue_deg),
    ]
    .into_iter()
    .flatten()
    .collect();
    let a = max_amount(s);
    // Radius in full image pixels grows with the amount (as in common photo apps, a higher
    // amount also acts wider), converted to output pixels
    let r_full = 1.5 + 4.5 * a;
    let r = (r_full / geo.scale.max(1.0)).round().max(1.0) as usize;
    let r2 = 3 * r;
    let norm = 1.0 / ((2 * r2 + 1) as f32 * (2 * r2 + 1) as f32 * 255.0);
    // Edge threshold: brightness span of the encoded luminance in the neighbourhood
    let (t0, t1) = (0.16 - 0.06 * a, 0.36 - 0.1 * a);

    // 1. Per pixel: luminance (bits 24–31), fringe membership × 255 (16–23), colour differences
    //    (0–15). The running maximum/minimum thus yields membership and colour of the
    //    brightest or darkest point at the same time.
    let mut packed = vec![0u32; w * h];
    let row_has: Vec<bool> = packed
        .par_chunks_mut(w)
        .zip(img.data.par_chunks(w * 3))
        .map(|(o, src)| {
            let mut any = false;
            for x in 0..w {
                let p = &src[x * 3..x * 3 + 3];
                let v = px(p);
                let l = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                // fast pre-test for chroma
                let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
                let c = if mx - mn > 1966 { (membership(v, &bands).0 * 255.0 + 0.5) as u32 } else { 0 };
                any |= c > 0;
                o[x] = ((l * 255.0 + 0.5) as u32).min(255) << 24 | c << 16 | chroma_bytes(v, l);
            }
            any
        })
        .collect();
    if !row_has.iter().any(|&b| b) {
        return;
    }

    // 2. Strip by strip: horizontal running maximum/minimum over ±r (with margin), then per
    //    candidate vertically, edge strength, object protection and correction
    img.data.par_chunks_mut(w * 3 * STRIP).enumerate().for_each(|(si, strip)| {
        let (y0, y1) = (si * STRIP, (si * STRIP + STRIP).min(h));
        if !row_has[y0..y1].iter().any(|&b| b) {
            return;
        }
        let (ha, hb) = (y0.saturating_sub(r), (y1 + r).min(h));
        let mut hmax = vec![0u32; (hb - ha) * w];
        let mut hmin = vec![0u32; (hb - ha) * w];
        let (mut g, mut hh) = (vec![0u32; w], vec![0u32; w]);
        for yy in ha..hb {
            let src = &packed[yy * w..yy * w + w];
            let o = (yy - ha) * w;
            running(src, r, &mut hmax[o..o + w], &mut g, &mut hh, u32::max);
            running(src, r, &mut hmin[o..o + w], &mut g, &mut hh, u32::min);
        }
        // Horizontal sums of membership over ±r2 (running, border extended)
        let (sa, sb) = (y0.saturating_sub(r2), (y1 + r2).min(h));
        let mut hsum = vec![0u16; (sb - sa) * w];
        for yy in sa..sb {
            let c = |x: isize| packed[yy * w + x.clamp(0, w as isize - 1) as usize] >> 16 & 0xff;
            let o = &mut hsum[(yy - sa) * w..(yy - sa + 1) * w];
            let mut acc: u32 = (-(r2 as isize)..=r2 as isize).map(c).sum();
            for x in 0..w {
                o[x] = acc as u16;
                acc = acc + c(x as isize + r2 as isize + 1) - c(x as isize - r2 as isize);
            }
        }
        for y in y0..y1 {
            if !row_has[y] {
                continue;
            }
            let row = &packed[y * w..y * w + w];
            let line = &mut strip[(y - y0) * w * 3..(y - y0 + 1) * w * 3];
            let (ya, yb) = (y.saturating_sub(r) - ha, (y + r).min(h - 1) - ha);
            for x in 0..w {
                if row[x] >> 16 & 0xff == 0 {
                    continue;
                }
                let (mut mx, mut mn) = (0u32, u32::MAX);
                for yy in ya..=yb {
                    mx = mx.max(hmax[yy * w + x]);
                    mn = mn.min(hmin[yy * w + x]);
                }
                let (lmax, lmin) = ((mx >> 24) as f32 / 255.0, (mn >> 24) as f32 / 255.0);
                let edge = smoothstep(t0, t1, lmax - lmin);
                if edge <= 0.0 {
                    continue;
                }
                // If the brightest or darkest point is itself fringe-coloured, it is an object
                let ext = (mx >> 16 & 0xff).max(mn >> 16 & 0xff) as f32 / 255.0;
                let mut protect = 1.0 - smoothstep(0.05, 0.3, ext);
                if protect <= 0.0 {
                    continue;
                }
                let mut sum = 0u32;
                for dy in -(r2 as isize)..=r2 as isize {
                    let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    sum += hsum[(yy - sa) * w + x] as u32;
                }
                protect *= 1.0 - smoothstep(0.3, 0.55, sum as f32 * norm);
                if protect <= 0.0 {
                    continue;
                }
                let p = &mut line[x * 3..x * 3 + 3];
                let v = px(p);
                let (m, k) = membership(v, &bands);
                let t = (k * m * edge * protect).min(1.0);
                if t <= 0.0 {
                    continue;
                }
                // Target colour: between darkest and brightest point according to the pixel's own brightness
                let l = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                let u = ((l - lmin) / (lmax - lmin).max(1e-3)).clamp(0.0, 1.0);
                let (cmax, cmin) = (chroma_of(mx), chroma_of(mn));
                let target = [cmin[0] + u * (cmax[0] - cmin[0]), cmin[1] + u * (cmax[1] - cmin[1])];
                let (cb, cr) = (v[2] - l, v[0] - l);
                let (cb, cr) = (cb + t * (target[0] - cb), cr + t * (target[1] - cr));
                let (rr, bb) = (l + cr, l + cb);
                let gg = (l - 0.2126 * rr - 0.0722 * bb) / 0.7152;
                for (o, c) in p.iter_mut().zip([rr, gg, bb]) {
                    *o = (c.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test image: bright on the left, dark on the right, a purple fringe in between; at the bottom a large
    /// purple area (flower) with an edge.
    fn test_image() -> Rgb16 {
        let (w, h) = (120, 120);
        let mut data = vec![0u16; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let c: [u16; 3] = if y < 60 {
                    if x < 50 {
                        [60000, 60000, 60000]
                    } else if x < 54 {
                        [40000, 12000, 48000] // fringe
                    } else {
                        [6000, 6000, 6000]
                    }
                } else if x < 40 {
                    [55000, 55000, 55000]
                } else {
                    [38000, 10000, 46000] // purple area
                };
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        Rgb16 { width: w, height: h, data }
    }

    fn chroma(img: &Rgb16, x: usize, y: usize) -> u16 {
        let p = &img.data[(y * img.width + x) * 3..(y * img.width + x) * 3 + 3];
        p.iter().max().unwrap() - p.iter().min().unwrap()
    }

    #[test]
    fn default_bit_identical() {
        let a = test_image();
        let mut b = test_image();
        apply(&mut b, &Settings::default(), Geo::full(120, 120));
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn fringe_gone_area_stays() {
        let mut img = test_image();
        let s = Settings { defringe_purple_amount: 0.5, ..Settings::default() };
        let before_fringe = chroma(&img, 52, 30);
        let before_flower = chroma(&img, 90, 90);
        apply(&mut img, &s, Geo::full(120, 120));
        assert!(chroma(&img, 52, 30) < before_fringe / 4, "fringe not removed: {}", chroma(&img, 52, 30));
        // Interior of the area unchanged, its edge (edge at x = 40) largely protected
        assert_eq!(chroma(&img, 90, 90), before_flower);
        // in u32: the chroma of the flower is above 21 845, ×3 overflows u16
        assert!(chroma(&img, 41, 90) as u32 > before_flower as u32 * 3 / 4, "flower edge desaturated: {}", chroma(&img, 41, 90));
    }

    #[test]
    fn bright_stem_stays() {
        // bright green blade (3 px) against dark soil: real object, not a fringe
        let (w, h) = (60, 60);
        let mut data = vec![4000u16; w * h * 3];
        for y in 0..h {
            for x in 28..31 {
                data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[30000, 52000, 20000]);
            }
        }
        let a = Rgb16 { width: w, height: h, data: data.clone() };
        let mut b = Rgb16 { width: w, height: h, data };
        let s = Settings { defringe_green_amount: 1.0, defringe_green_hue: [0.0, 100.0], ..Settings::default() };
        apply(&mut b, &s, Geo::full(w, h));
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn green_does_not_affect_violet() {
        let a = test_image();
        let mut b = test_image();
        let s = Settings { defringe_green_amount: 1.0, ..Settings::default() };
        apply(&mut b, &s, Geo::full(120, 120));
        assert_eq!(a.data, b.data);
    }
}

#[path = "defringe_wide.rs"]
pub(crate) mod wide;
