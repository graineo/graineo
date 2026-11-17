//! Learning the look of a camera from the user's own photos: RAW plus the camera's own rendering of the same shot
//! (camera JPEG next to the RAW or the JPEG preview embedded in it), per camera and picture style.
//!
//! Pipeline (all functions are pure; reading files and developing lives in `rawdev::learn`):
//! 1. [`align`]: the reference is placed on the development by a similarity transform (scale and shift of the common
//!    crop) found by normalised cross-correlation of the luminance; photos below [`MIN_CORRELATION`] are dropped.
//! 2. [`block_pairs`]: block means (about 32 px at 2000 px) in linear light; blocks with texture or edges and blocks
//!    with clipping are dropped, so only colour and tone remain. The exposure difference per photo ([`midtone_ev`]) is
//!    removed before (the development is rendered again), so it is never learned as part of the look.
//! 3. [`learn`]: training and check photos are separated by whole shooting days ([`split_by_day`]). Three stages are
//!    fitted on the training photos: a 3×3 matrix in linear light (rows sum to one, pulled towards the identity),
//!    curves per channel and a 33³ table of corrections in Oklab with tetrahedral interpolation (smoothness, stronger
//!    flatness on lightness, pull towards no change where no data lie and in the shadows). Each stage is baked into
//!    the look table the development uses and measured on the check photos in ΔE00; it is kept only if it beats the
//!    previous best ([`clearly_better`]): median at least 3 % lower, 90th percentile not higher, at least 60 % of the
//!    photos better. The first comparison is the current standard development. Without a gain nothing is offered.
//!
//! The result is a [`Look`] in the `X3LOOK1` format (33³, tetrahedral) that maps the encoded sRGB of the plain
//! development (no camera look, no hue table, no per-camera saturation) to the camera's rendering – the same domain
//! as the bundled camera looks. Only colour values of block means are used; no image is stored.

use crate::development::{oklab_to_srgb_lin, srgb_lin_to_oklab};
use crate::looks::{tetrahedral_weights, Look};
use crate::picture_style::PictureStyle;
use crate::RgbImage;
use graineo_color::{srgb_decode, srgb_encode};

/// Version of the learned-look metadata.
pub const FORMAT_VERSION: u32 = 1;
/// Edge length of the learned table.
pub const LUT_N: usize = 33;
/// Knots of the per-channel curves.
const CURVE_KNOTS: usize = 17;
/// Photos below this luminance correlation after alignment are not used.
pub const MIN_CORRELATION: f32 = 0.9;
/// Fewer training photos than this: no look is learned (a related camera's look may serve as a starting point).
pub const MIN_PHOTOS: usize = 20;
/// Fewer check photos than this: the check would say nothing.
pub const MIN_CHECK_PHOTOS: usize = 6;
/// Photos per shooting day at most (spread over the day).
pub const PER_DAY: usize = 4;
/// Photos used at most per camera and style (training and check together).
pub const MAX_PHOTOS: usize = 80;

/// Where the reference of a photo came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceSource {
    /// camera JPEG next to the RAW (RAW+JPEG); preferred when present
    CameraJpeg,
    /// JPEG preview embedded in the RAW
    Embedded,
}

impl ReferenceSource {
    pub fn key(self) -> &'static str {
        match self {
            Self::CameraJpeg => "jpeg",
            Self::Embedded => "embedded",
        }
    }
}

/// One block: encoded sRGB (0…1) of the plain development, of the camera reference and of the current standard
/// development (what the user sees today; equal to `base` if unknown).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair {
    pub base: [f32; 3],
    pub reference: [f32; 3],
    pub current: [f32; 3],
}

/// The blocks of one photo.
#[derive(Clone, Debug, Default)]
pub struct Photo {
    pub name: String,
    /// shooting day `YYYY-MM-DD` (training and check are split by whole days)
    pub day: String,
    pub folder: String,
    pub source: Option<ReferenceSource>,
    /// exposure difference removed before the blocks were taken (EV, development − reference)
    pub exposure_offset: f32,
    pub correlation: f32,
    pub pairs: Vec<Pair>,
}

// ---------------------------------------------------------------------------------------------
// colour helpers

/// Encoded sRGB → CIELab (D65).
pub fn srgb_to_lab(c: [f32; 3]) -> [f32; 3] {
    let l = c.map(|v| srgb_decode(v.clamp(0.0, 1.0)));
    let x = 0.412_456_4 * l[0] + 0.357_576_1 * l[1] + 0.180_437_5 * l[2];
    let y = 0.212_672_9 * l[0] + 0.715_152_2 * l[1] + 0.072_175 * l[2];
    let z = 0.019_333_9 * l[0] + 0.119_192 * l[1] + 0.950_304_1 * l[2];
    let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 colour difference (Sharma, Wu, Dalal 2005).
pub fn de2000(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (l1, a1, b1) = (a[0] as f64, a[1] as f64, a[2] as f64);
    let (l2, a2, b2) = (b[0] as f64, b[1] as f64, b[2] as f64);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cm = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + 25f64.powi(7))).sqrt());
    let a1p = (1.0 + g) * a1;
    let a2p = (1.0 + g) * a2;
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let hp = |b: f64, a: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let h1p = hp(b1, a1p);
    let h2p = hp(b2, a2p);
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p - h1p > 180.0 {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let dhp_big = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lpm = (l1 + l2) / 2.0;
    let cpm = (c1p + c2p) / 2.0;
    let hpm = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hpm - 30.0).to_radians().cos() + 0.24 * (2.0 * hpm).to_radians().cos() + 0.32 * (3.0 * hpm + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hpm - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hpm - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cpm.powi(7) / (cpm.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lpm - 50.0).powi(2) / (20.0 + (lpm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cpm;
    let sh = 1.0 + 0.015 * cpm * t;
    let rt = -(2.0 * dtheta.to_radians()).sin() * rc;
    let v = (dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhp_big / sh).powi(2) + rt * (dcp / sc) * (dhp_big / sh);
    v.max(0.0).sqrt() as f32
}

fn lin(c: [f32; 3]) -> [f32; 3] {
    c.map(|v| srgb_decode(v.clamp(0.0, 1.0)))
}

fn luminance(l: [f32; 3]) -> f32 {
    0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
}

/// Linear sRGB → encoded sRGB in 0…1; negative components are first desaturated towards the luminance (as the
/// development does before the look).
fn encode_gamut(mut s: [f32; 3]) -> [f32; 3] {
    let y = luminance(s);
    let mn = s[0].min(s[1]).min(s[2]);
    if mn < 0.0 && y > 0.0 {
        let f = y / (y - mn);
        s = s.map(|v| y + f * (v - y));
    }
    s.map(|v| srgb_encode(v.clamp(0.0, 1.0)))
}

// ---------------------------------------------------------------------------------------------
// picture style keys

/// Key of a picture style for learned looks: the style plus the camera's fine settings that change the JPEG
/// (Fujifilm colour, highlight and shadow tone), e.g. `provia` or `provia_h+1_s+1`. Dynamic range modes are not part
/// of it (their exposure is compensated by the development; automatic DR is adaptive, see [`adaptive`]).
pub fn style_key(ps: &PictureStyle) -> String {
    let mut key = ps.style.clone();
    let detail = |name: &str| ps.details.iter().find(|(n, _)| *n == name).map(|(_, v)| v.split_whitespace().next().unwrap_or("").to_string());
    for (name, short) in [("Farbe", "c"), ("Lichterton", "h"), ("Schattenton", "s")] {
        if let Some(v) = detail(name).filter(|v| !v.is_empty() && v != "0" && v != "?") {
            key.push_str(&format!("_{short}{v}"));
        }
    }
    key.chars().map(|c| if c.is_ascii_alphanumeric() || "+-._".contains(c) { c } else { '-' }).collect()
}

/// Adaptive camera setting that makes the camera JPEG depend on the scene (automatic dynamic range, Auto Lighting
/// Optimizer, DRO, highlight/shadow correction, Canon "Auto" style): such photos are left out of learning.
pub fn adaptive(ps: &PictureStyle) -> Option<&'static str> {
    let d = |name: &str| ps.details.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str());
    if d("DRAuto") == Some("1") {
        return Some("DR Auto");
    }
    if d("ALO").is_some_and(|v| v != "3") {
        return Some("Auto Lighting Optimizer");
    }
    if d("DRO").is_some_and(|v| v != "0") {
        return Some("DRO");
    }
    if d("HighlightCorrection").is_some_and(|v| v != "0") {
        return Some("Highlight correction");
    }
    if d("ShadowCorrection").is_some_and(|v| v != "0") {
        return Some("Shadow correction");
    }
    if ps.style == "auto" {
        return Some("Auto");
    }
    None
}

/// Is the style one that can be learned at all? Unknown styles and the user's own styles stay out (their settings
/// are not readable, so photos with different settings would be mixed).
pub fn learnable_style(ps: &PictureStyle) -> bool {
    !matches!(ps.style.as_str(), "unknown" | "film-unknown" | "none" | "user" | "cm-set") && !ps.style.starts_with("user")
}

// ---------------------------------------------------------------------------------------------
// alignment and blocks

/// Similarity transform of the reference onto the development: a point (u, v) of the development's common crop (0…1)
/// lies at `0.5 + (u − 0.5)·scale + dx` (same for v with `dy`) in the reference's common crop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Alignment {
    pub scale: f32,
    pub dx: f32,
    pub dy: f32,
    /// normalised cross-correlation of the luminance after alignment (1 = identical)
    pub correlation: f32,
}

/// Centre crop (x0, y0, w, h) of an image to the aspect ratio `r`.
fn crop(w: u32, h: u32, r: f32) -> (f32, f32, f32, f32) {
    let (wf, hf) = (w as f32, h as f32);
    let (cw, ch) = if wf / hf > r { (hf * r, hf) } else { (wf, wf / r) };
    ((wf - cw) / 2.0, (hf - ch) / 2.0, cw, ch)
}

fn common_ratio(a: &RgbImage, b: &RgbImage) -> f32 {
    let ra = a.width() as f32 / a.height() as f32;
    let rb = b.width() as f32 / b.height() as f32;
    if ra >= 1.0 {
        ra.min(rb)
    } else {
        ra.max(rb)
    }
}

/// Encoded luminance of the crop, area-averaged to `tw`×`th`.
fn luma_grid(img: &RgbImage, c: (f32, f32, f32, f32), tw: usize, th: usize) -> Vec<f32> {
    let (x0, y0, cw, ch) = c;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raw = img.as_raw();
    let mut out = vec![0f32; tw * th];
    for ty in 0..th {
        let ya = y0 + ch * ty as f32 / th as f32;
        let yb = y0 + ch * (ty + 1) as f32 / th as f32;
        for tx in 0..tw {
            let xa = x0 + cw * tx as f32 / tw as f32;
            let xb = x0 + cw * (tx + 1) as f32 / tw as f32;
            let (mut s, mut n) = (0f32, 0f32);
            for y in (ya as usize)..(yb.ceil() as usize).min(h) {
                for x in (xa as usize)..(xb.ceil() as usize).min(w) {
                    let p = &raw[(y * w + x) * 3..(y * w + x) * 3 + 3];
                    s += 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                    n += 1.0;
                }
            }
            out[ty * tw + tx] = s / n.max(1.0) / 255.0;
        }
    }
    out
}

fn bilinear(g: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (ix, iy) = ((x as usize).min(w - 2), (y as usize).min(h - 2));
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let a = g[iy * w + ix] * (1.0 - fx) + g[iy * w + ix + 1] * fx;
    let b = g[(iy + 1) * w + ix] * (1.0 - fx) + g[(iy + 1) * w + ix + 1] * fx;
    a * (1.0 - fy) + b * fy
}

/// Normalised cross-correlation of `a` (grid tw×th) with `b` sampled through the transform, over the inner 80 %.
fn ncc(a: &[f32], b: &[f32], tw: usize, th: usize, s: f32, dx: f32, dy: f32) -> f32 {
    let (mx, my) = (tw / 10, th / 10);
    let (mut sa, mut sb, mut saa, mut sbb, mut sab, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for y in my..th - my {
        let v = (y as f32 + 0.5) / th as f32;
        let yr = (0.5 + (v - 0.5) * s + dy) * th as f32 - 0.5;
        for x in mx..tw - mx {
            let u = (x as f32 + 0.5) / tw as f32;
            let xr = (0.5 + (u - 0.5) * s + dx) * tw as f32 - 0.5;
            let p = a[y * tw + x] as f64;
            let q = bilinear(b, tw, th, xr, yr) as f64;
            sa += p;
            sb += q;
            saa += p * p;
            sbb += q * q;
            sab += p * q;
            n += 1.0;
        }
    }
    let cov = sab / n - sa / n * sb / n;
    let va = saa / n - (sa / n).powi(2);
    let vb = sbb / n - (sb / n).powi(2);
    if va <= 1e-12 || vb <= 1e-12 {
        return 0.0;
    }
    (cov / (va * vb).sqrt()) as f32
}

/// Find scale (0.94…1.06) and shift (±4 % of the crop) of the reference: coarse search on 128 px, refined on 320 px.
pub fn align(base: &RgbImage, reference: &RgbImage) -> Alignment {
    let r = common_ratio(base, reference);
    let (ca, cb) = (crop(base.width(), base.height(), r), crop(reference.width(), reference.height(), r));
    let size = |long: usize| if r >= 1.0 { (long, ((long as f32 / r).round() as usize).max(8)) } else { (((long as f32 * r).round() as usize).max(8), long) };
    let mut best = Alignment { scale: 1.0, dx: 0.0, dy: 0.0, correlation: f32::MIN };
    let (tw, th) = size(128);
    let (ga, gb) = (luma_grid(base, ca, tw, th), luma_grid(reference, cb, tw, th));
    for si in -6..=6 {
        let s = 1.0 + 0.01 * si as f32;
        for yi in -5..=5 {
            for xi in -5..=5 {
                let (dx, dy) = (xi as f32 / tw as f32, yi as f32 / th as f32);
                let c = ncc(&ga, &gb, tw, th, s, dx, dy);
                if c > best.correlation {
                    best = Alignment { scale: s, dx, dy, correlation: c };
                }
            }
        }
    }
    let (tw, th) = size(320);
    let (ga, gb) = (luma_grid(base, ca, tw, th), luma_grid(reference, cb, tw, th));
    let start = best;
    best.correlation = ncc(&ga, &gb, tw, th, start.scale, start.dx, start.dy);
    for si in -4..=4 {
        let s = start.scale + 0.0025 * si as f32;
        for yi in -6..=6 {
            for xi in -6..=6 {
                let (dx, dy) = (start.dx + 0.5 * xi as f32 / tw as f32, start.dy + 0.5 * yi as f32 / th as f32);
                let c = ncc(&ga, &gb, tw, th, s, dx, dy);
                if c > best.correlation {
                    best = Alignment { scale: s, dx, dy, correlation: c };
                }
            }
        }
    }
    best
}

/// Linear mean and encoded-luminance spread of a pixel rectangle; `None` if empty or clipped (a channel ≥ 250).
fn block_stats(img: &RgbImage, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<([f32; 3], f32)> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (xa, ya) = (x0.round().max(0.0) as usize, y0.round().max(0.0) as usize);
    let (xb, yb) = ((x1.round() as usize).min(w), (y1.round() as usize).min(h));
    if xb <= xa || yb <= ya {
        return None;
    }
    let raw = img.as_raw();
    let (mut s, mut sl, mut sll, mut n) = ([0f64; 3], 0f64, 0f64, 0f64);
    for y in ya..yb {
        for x in xa..xb {
            let p = &raw[(y * w + x) * 3..(y * w + x) * 3 + 3];
            if p.iter().any(|&v| v >= 250) {
                return None;
            }
            for c in 0..3 {
                s[c] += srgb_decode(p[c] as f32 / 255.0) as f64;
            }
            let l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) as f64 / 255.0;
            sl += l;
            sll += l * l;
            n += 1.0;
        }
    }
    let spread = ((sll / n - (sl / n).powi(2)).max(0.0)).sqrt() as f32;
    Some((s.map(|v| (v / n) as f32), spread))
}

/// Largest spread of the encoded luminance inside a block that still counts as flat (no texture, no edge).
const MAX_SPREAD: f32 = 0.045;

/// Block pairs of a photo: the development `base` (and the current standard development `current`, same size) and
/// the `reference` placed by `a`. Blocks of about 1/64 of the long edge, an outer margin of 4 % left out (lens
/// corrections differ there), textured and clipped blocks dropped.
pub fn block_pairs(base: &RgbImage, current: Option<&RgbImage>, reference: &RgbImage, a: &Alignment) -> Vec<Pair> {
    let r = common_ratio(base, reference);
    let (ax, ay, aw, ah) = crop(base.width(), base.height(), r);
    let (bx, by, bw, bh) = crop(reference.width(), reference.height(), r);
    let bs = (aw.max(ah) / 64.0).max(4.0);
    let (nx, ny) = ((aw / bs) as usize, (ah / bs) as usize);
    let mut out = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            let (u0, v0) = (i as f32 * bs / aw, j as f32 * bs / ah);
            let (u1, v1) = ((i + 1) as f32 * bs / aw, (j + 1) as f32 * bs / ah);
            if u0 < 0.04 || v0 < 0.04 || u1 > 0.96 || v1 > 0.96 {
                continue;
            }
            let Some((pb, sb)) = block_stats(base, ax + u0 * aw, ay + v0 * ah, ax + u1 * aw, ay + v1 * ah) else { continue };
            if sb > MAX_SPREAD {
                continue;
            }
            let map_u = |u: f32| bx + (0.5 + (u - 0.5) * a.scale + a.dx) * bw;
            let map_v = |v: f32| by + (0.5 + (v - 0.5) * a.scale + a.dy) * bh;
            let Some((pr, sr)) = block_stats(reference, map_u(u0), map_v(v0), map_u(u1), map_v(v1)) else { continue };
            if sr > MAX_SPREAD * 1.5 {
                continue;
            }
            let pc = match current {
                Some(c) => match block_stats(c, ax + u0 * aw, ay + v0 * ah, ax + u1 * aw, ay + v1 * ah) {
                    Some((p, _)) => p,
                    None => continue,
                },
                None => pb,
            };
            out.push(Pair { base: pb.map(srgb_encode), reference: pr.map(srgb_encode), current: pc.map(srgb_encode) });
        }
    }
    out
}

/// Exposure difference (EV, development − reference) on the reference's midtones (L* 30…70): mean log2 luminance
/// ratio. `None` with fewer than 30 midtone blocks.
pub fn midtone_ev(pairs: &[Pair]) -> Option<f32> {
    let v: Vec<f32> = pairs
        .iter()
        .filter_map(|p| {
            let (yb, yr) = (luminance(lin(p.base)), luminance(lin(p.reference)));
            (yr > 0.0662 && yr < 0.4075 && yb > 1e-4).then(|| (yb / yr).log2())
        })
        .collect();
    (v.len() >= 30).then(|| v.iter().sum::<f32>() / v.len() as f32)
}

// ---------------------------------------------------------------------------------------------
// split

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Split photos into training and check by whole shooting days: days in a fixed pseudo-random order (hash of
/// `salt` and day), each day goes to the side that has fewer photos so far (training first). Returns the indices of
/// (training, check). With one day only, everything is training.
pub fn split_by_day(photos: &[Photo], salt: &str) -> (Vec<usize>, Vec<usize>) {
    let mut days: Vec<&str> = photos.iter().map(|p| p.day.as_str()).collect();
    days.sort_unstable();
    days.dedup();
    days.sort_by_key(|d| fnv(&format!("{salt}/{d}")));
    let (mut train, mut check) = (Vec::new(), Vec::new());
    if days.len() < 2 {
        return ((0..photos.len()).collect(), check);
    }
    for d in days {
        let idx: Vec<usize> = photos.iter().enumerate().filter(|(_, p)| p.day == d).map(|(i, _)| i).collect();
        if train.len() <= check.len() {
            train.extend(idx);
        } else {
            check.extend(idx);
        }
    }
    (train, check)
}

/// Choose photos for learning from candidates (index, day, name): at most [`PER_DAY`] per day, spread evenly over the
/// names of the day, days taken round-robin until `max` photos. Returns indices into `cands`.
pub fn select(cands: &[(String, String)], max: usize) -> Vec<usize> {
    let mut by_day: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for (i, (day, _)) in cands.iter().enumerate() {
        by_day.entry(day.as_str()).or_default().push(i);
    }
    let mut lists: Vec<Vec<usize>> = by_day
        .into_values()
        .map(|mut v| {
            v.sort_by(|a, b| cands[*a].1.cmp(&cands[*b].1));
            let k = v.len().min(PER_DAY);
            (0..k).map(|j| v[(j * v.len() + v.len() / (2 * k)) / k]).collect()
        })
        .collect();
    // days in a fixed mixed order, so a cut at `max` does not keep only the oldest days
    lists.sort_by_key(|l| fnv(&cands[l[0]].0));
    let mut out = Vec::new();
    for round in 0..PER_DAY {
        for l in &lists {
            if out.len() >= max {
                return out;
            }
            if let Some(&i) = l.get(round) {
                out.push(i);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// model

/// Learning stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Matrix,
    Curves,
    Table,
}

impl Stage {
    pub fn key(self) -> &'static str {
        match self {
            Self::Matrix => "matrix",
            Self::Curves => "curves",
            Self::Table => "table",
        }
    }
}

/// The fitted mapping: matrix in linear sRGB, then curves per channel (encoded), then corrections in Oklab looked
/// up by the input colour.
#[derive(Clone, Debug)]
pub struct Model {
    pub matrix: [[f32; 3]; 3],
    /// deviation from the identity at the inner knots (per channel)
    pub curves: Option<[[f32; CURVE_KNOTS]; 3]>,
    /// 33³ corrections in Oklab (r slow, b fast)
    pub table: Option<Vec<[f32; 3]>>,
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn curve_eval(d: &[f32; CURVE_KNOTS], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let t = x * (CURVE_KNOTS - 1) as f32;
    let i = (t as usize).min(CURVE_KNOTS - 2);
    let f = t - i as f32;
    x + d[i] * (1.0 - f) + d[i + 1] * f
}

impl Model {
    pub fn identity() -> Self {
        Model { matrix: IDENTITY, curves: None, table: None }
    }

    /// Output of matrix and curves (encoded sRGB).
    fn first_stages(&self, x: [f32; 3]) -> [f32; 3] {
        let l = lin(x);
        let m = &self.matrix;
        let y = [0, 1, 2].map(|r| m[r][0] * l[0] + m[r][1] * l[1] + m[r][2] * l[2]);
        let e = encode_gamut(y);
        match &self.curves {
            Some(c) => [0, 1, 2].map(|k| curve_eval(&c[k], e[k]).clamp(0.0, 1.0)),
            None => e,
        }
    }

    /// Map an encoded sRGB colour of the plain development.
    pub fn apply(&self, x: [f32; 3]) -> [f32; 3] {
        let e = self.first_stages(x);
        let Some(t) = &self.table else { return e };
        let mut lab = srgb_lin_to_oklab(lin(e));
        for (k, w) in tetrahedral_weights(LUT_N, x) {
            for c in 0..3 {
                lab[c] += w * t[k][c];
            }
        }
        encode_gamut(oklab_to_srgb_lin(lab))
    }

    /// The look table the development applies (33³, tetrahedral, differences output − input).
    pub fn bake(&self, name: &str, meta: &str) -> Look {
        let n = LUT_N;
        let d: Vec<[f32; 3]> = (0..n * n * n)
            .map(|k| {
                let x = [k / (n * n), (k / n) % n, k % n].map(|v| v as f32 / (n - 1) as f32);
                let y = self.apply(x);
                [y[0] - x[0], y[1] - x[1], y[2] - x[2]]
            })
            .collect();
        Look::from_table(name, meta, n, d).expect("table size")
    }
}

/// Training samples with weights: every photo counts the same in total (`weight` = blocks / (photos × own blocks)).
struct Sample {
    x: [f32; 3],
    y: [f32; 3],
    w: f32,
}

fn samples(photos: &[&Photo]) -> Vec<Sample> {
    let total: usize = photos.iter().map(|p| p.pairs.len()).sum();
    let np = photos.iter().filter(|p| !p.pairs.is_empty()).count().max(1);
    let mut out = Vec::with_capacity(total);
    for p in photos {
        let w = total as f32 / (np * p.pairs.len().max(1)) as f32;
        out.extend(p.pairs.iter().map(|q| Sample { x: q.base, y: q.reference, w }));
    }
    out
}

/// Solve a small symmetric positive system in place (Gauss with partial pivoting). Returns `None` if singular.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for i in 0..n {
        let p = (i..n).max_by(|&x, &y| a[x][i].abs().total_cmp(&a[y][i].abs()))?;
        if a[p][i].abs() < 1e-12 {
            return None;
        }
        a.swap(i, p);
        b.swap(i, p);
        for r in i + 1..n {
            let f = a[r][i] / a[i][i];
            for c in i..n {
                a[r][c] -= f * a[i][c];
            }
            b[r] -= f * b[i];
        }
    }
    let mut x = vec![0f64; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|c| a[i][c] * x[c]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    Some(x)
}

/// Stage 1: 3×3 matrix in linear light, rows summing to one (neutrals stay neutral), ridge towards the identity.
/// Fitted on colour only: each block is first brought to the reference's luminance, so tone differences (curve,
/// contrast) do not pull on the matrix; errors are relative to the reference luminance.
fn fit_matrix(s: &[Sample]) -> [[f32; 3]; 3] {
    let mut m = IDENTITY;
    let wsum: f64 = s.iter().map(|q| q.w as f64).sum();
    for (r, row) in m.iter_mut().enumerate() {
        let (mut a, mut b) = ([[0f64; 2]; 2], [0f64; 2]);
        for q in s {
            let (x, y) = (lin(q.x), lin(q.y));
            let (yx, yy) = (luminance(x), luminance(y));
            if yx < 1e-3 || yy < 1e-3 {
                continue;
            }
            let x = x.map(|v| v * yy / yx);
            let k = (yy + 0.02) as f64;
            let w = q.w as f64 / (k * k);
            let f = [(x[0] - x[2]) as f64, (x[1] - x[2]) as f64];
            let t = (y[r] - x[r]) as f64;
            for i in 0..2 {
                b[i] += w * f[i] * t;
                for j in 0..2 {
                    a[i][j] += w * f[i] * f[j];
                }
            }
        }
        let ridge = 0.02 * wsum;
        let sol = solve(vec![vec![a[0][0] + ridge, a[0][1]], vec![a[1][0], a[1][1] + ridge]], b.to_vec()).unwrap_or(vec![0.0, 0.0]);
        let (p, q) = (sol[0] as f32, sol[1] as f32);
        row[0] += p;
        row[1] += q;
        row[2] -= p + q;
    }
    m
}

/// Stage 2: curves per channel on the matrix output (encoded), ends fixed (black stays black, white stays white),
/// smoothness on the second differences and a pull towards the identity.
fn fit_curves(s: &[Sample], matrix: [[f32; 3]; 3]) -> [[f32; CURVE_KNOTS]; 3] {
    let pre = Model { matrix, curves: None, table: None };
    let z: Vec<[f32; 3]> = s.iter().map(|q| pre.first_stages(q.x)).collect();
    let wsum: f64 = s.iter().map(|q| q.w as f64).sum();
    let inner = CURVE_KNOTS - 2;
    let mut out = [[0f32; CURVE_KNOTS]; 3];
    for c in 0..3 {
        let mut a = vec![vec![0f64; inner]; inner];
        let mut b = vec![0f64; inner];
        for (q, zz) in s.iter().zip(&z) {
            let x = zz[c].clamp(0.0, 1.0);
            let t = x * (CURVE_KNOTS - 1) as f32;
            let i = (t as usize).min(CURVE_KNOTS - 2);
            let f = (t - i as f32) as f64;
            let r = (q.y[c] - x) as f64;
            let w = q.w as f64;
            let phi = [(i, 1.0 - f), (i + 1, f)];
            for &(ka, wa) in &phi {
                if ka == 0 || ka == CURVE_KNOTS - 1 {
                    continue;
                }
                b[ka - 1] += w * wa * r;
                for &(kb, wb) in &phi {
                    if kb == 0 || kb == CURVE_KNOTS - 1 {
                        continue;
                    }
                    a[ka - 1][kb - 1] += w * wa * wb;
                }
            }
        }
        // second differences over all knots (ends are zero)
        let smooth = 0.05 * wsum;
        for k in 1..CURVE_KNOTS - 1 {
            let idx = [k - 1, k, k + 1];
            let coef = [1.0, -2.0, 1.0];
            for (ia, &ka) in idx.iter().enumerate() {
                if ka == 0 || ka == CURVE_KNOTS - 1 {
                    continue;
                }
                for (ib, &kb) in idx.iter().enumerate() {
                    if kb == 0 || kb == CURVE_KNOTS - 1 {
                        continue;
                    }
                    a[ka - 1][kb - 1] += smooth * coef[ia] * coef[ib];
                }
            }
        }
        for (i, row) in a.iter_mut().enumerate() {
            row[i] += 0.002 * wsum;
        }
        if let Some(d) = solve(a, b) {
            for k in 0..inner {
                out[c][k + 1] = d[k].clamp(-0.25, 0.25) as f32;
            }
        }
    }
    out
}

/// Smoothness of the table per Oklab channel (L, a, b): lightness much flatter, so chroma noise does not turn into
/// lightness blotches.
/// Both in units of the mean data weight per grid node, so they act alike for few and many photos.
const TABLE_SMOOTH: [f64; 3] = [8.0, 2.0, 2.0];
/// Pull of the table towards no change, and its extra share in the shadows.
const TABLE_PULL: f64 = 0.03;
const TABLE_PULL_SHADOWS: f64 = 4.0;

/// Stage 3: corrections in Oklab on the 33³ grid of the input colour (tetrahedral), solved by conjugate gradients per
/// channel: data term + Laplacian smoothness + pull towards zero (stronger in the shadows and near white).
fn fit_table(s: &[Sample], first: &Model, smooth: f64) -> Vec<[f32; 3]> {
    let n = LUT_N;
    let nn = n * n * n;
    let targets: Vec<[f32; 3]> = s
        .iter()
        .map(|q| {
            let a = srgb_lin_to_oklab(lin(first.first_stages(q.x)));
            let b = srgb_lin_to_oklab(lin(q.y));
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]]
        })
        .collect();
    let weights: Vec<[(usize, f32); 4]> = s.iter().map(|q| tetrahedral_weights(n, q.x)).collect();
    let density = s.iter().map(|q| q.w as f64).sum::<f64>() / nn as f64;
    // pull per node: base + shadows (max channel below 0.2) + near white (min channel above 0.92, the camera clips)
    let pull: Vec<f64> = (0..nn)
        .map(|k| {
            let x = [k / (n * n), (k / n) % n, k % n].map(|v| v as f32 / (n - 1) as f32);
            let mx = x[0].max(x[1]).max(x[2]);
            let mn = x[0].min(x[1]).min(x[2]);
            let shadow = ((0.2 - mx) / 0.2).max(0.0) as f64;
            let white = ((mn - 0.92) / 0.08).max(0.0) as f64;
            density * smooth * TABLE_PULL * (1.0 + TABLE_PULL_SHADOWS * shadow * 10.0 + 20.0 * white)
        })
        .collect();
    let neighbours = |k: usize| -> [Option<usize>; 6] {
        let (i, j, l) = (k / (n * n), (k / n) % n, k % n);
        [
            (i > 0).then(|| k - n * n),
            (i + 1 < n).then(|| k + n * n),
            (j > 0).then(|| k - n),
            (j + 1 < n).then(|| k + n),
            (l > 0).then(|| k - 1),
            (l + 1 < n).then(|| k + 1),
        ]
    };
    let mut table = vec![[0f32; 3]; nn];
    for c in 0..3 {
        let lam = density * smooth * TABLE_SMOOTH[c];
        let apply = |t: &[f64], out: &mut [f64]| {
            for k in 0..nn {
                let mut v = pull[k] * t[k];
                for j in neighbours(k).into_iter().flatten() {
                    v += lam * (t[k] - t[j]);
                }
                out[k] = v;
            }
            for (q, wt) in s.iter().zip(&weights) {
                let p: f64 = wt.iter().map(|(k, w)| *w as f64 * t[*k]).sum();
                for (k, w) in wt {
                    out[*k] += q.w as f64 * *w as f64 * p;
                }
            }
        };
        let mut b = vec![0f64; nn];
        let mut diag: Vec<f64> = (0..nn).map(|k| pull[k] + lam * neighbours(k).iter().flatten().count() as f64).collect();
        for ((q, wt), tg) in s.iter().zip(&weights).zip(&targets) {
            for (k, w) in wt {
                b[*k] += q.w as f64 * *w as f64 * tg[c] as f64;
                diag[*k] += q.w as f64 * (*w as f64).powi(2);
            }
        }
        // preconditioned conjugate gradients
        let mut x = vec![0f64; nn];
        let mut r = b.clone();
        let mut z: Vec<f64> = r.iter().zip(&diag).map(|(r, d)| r / d).collect();
        let mut p = z.clone();
        let mut rz: f64 = r.iter().zip(&z).map(|(a, b)| a * b).sum();
        let b_norm: f64 = b.iter().map(|v| v * v).sum::<f64>().sqrt().max(1e-30);
        let mut ap = vec![0f64; nn];
        for _ in 0..400 {
            apply(&p, &mut ap);
            let pap: f64 = p.iter().zip(&ap).map(|(a, b)| a * b).sum();
            if pap <= 0.0 {
                break;
            }
            let alpha = rz / pap;
            for k in 0..nn {
                x[k] += alpha * p[k];
                r[k] -= alpha * ap[k];
            }
            if r.iter().map(|v| v * v).sum::<f64>().sqrt() < 1e-6 * b_norm {
                break;
            }
            for k in 0..nn {
                z[k] = r[k] / diag[k];
            }
            let rz2: f64 = r.iter().zip(&z).map(|(a, b)| a * b).sum();
            let beta = rz2 / rz;
            rz = rz2;
            for k in 0..nn {
                p[k] = z[k] + beta * p[k];
            }
        }
        for k in 0..nn {
            table[k][c] = x[k].clamp(-0.2, 0.2) as f32;
        }
    }
    table
}

// ---------------------------------------------------------------------------------------------
// measuring and the guard

/// ΔE00 statistics of a set of photos.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    pub photos: usize,
    pub blocks: usize,
    pub median: f32,
    pub p90: f32,
    pub p95: f32,
    pub mean: f32,
    /// mean ΔE00 per photo (same order as the photos)
    pub per_photo: Vec<f32>,
}

fn quantile(sorted: &[f32], q: f64) -> f32 {
    if sorted.is_empty() {
        return f32::NAN;
    }
    sorted[(((q * sorted.len() as f64).ceil() as usize).max(1) - 1).min(sorted.len() - 1)]
}

/// ΔE00 of `f(pair)` against the reference over all blocks of the photos.
pub fn measure(photos: &[&Photo], f: &dyn Fn(&Pair) -> [f32; 3]) -> Metrics {
    metrics_of(photos.iter().map(|p| p.pairs.iter().map(|q| de2000(srgb_to_lab(f(q)), srgb_to_lab(q.reference))).collect()).collect())
}

/// [`Metrics`] from the ΔE00 of every block, per photo.
pub fn metrics_of(per_photo_de: Vec<Vec<f32>>) -> Metrics {
    let photos = per_photo_de.len();
    let per_photo = per_photo_de.iter().map(|d| if d.is_empty() { f32::NAN } else { d.iter().sum::<f32>() / d.len() as f32 }).collect();
    let mut all: Vec<f32> = per_photo_de.into_iter().flatten().collect();
    let mean = all.iter().sum::<f32>() / all.len().max(1) as f32;
    all.sort_by(|a, b| a.total_cmp(b));
    Metrics { photos, blocks: all.len(), median: quantile(&all, 0.5), p90: quantile(&all, 0.9), p95: quantile(&all, 0.95), mean, per_photo }
}

/// The decision rule (fixed before any measurement, as in the camera colour project of 16.11.2026): `new` beats `old`
/// on the same photos if the median ΔE00 is at least 3 % lower, the 90th percentile not higher (tolerance 0.02) and at
/// least 60 % of the photos have a lower mean ΔE00.
pub fn clearly_better(new: &Metrics, old: &Metrics) -> bool {
    let n = new.per_photo.len().min(old.per_photo.len());
    let better = new.per_photo.iter().zip(&old.per_photo).filter(|(a, b)| a < b).count();
    n > 0 && new.median <= 0.97 * old.median && new.p90 <= old.p90 + 0.02 && better as f32 >= 0.6 * n as f32
}

/// Result of [`learn`] for one camera and style.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// the stage kept; `None` = no better look found
    pub stage: Option<Stage>,
    pub look: Option<Look>,
    /// (variant, training, check): `neutral` (plain development), `current` (standard development today), then each
    /// fitted stage (`matrix`, `curves`, `table`)
    pub variants: Vec<(String, Metrics, Metrics)>,
    pub train_photos: usize,
    pub check_photos: usize,
    pub train_days: usize,
    pub check_days: usize,
    /// why a stage was kept or not, in order
    pub decisions: Vec<String>,
    /// reason when nothing could be learned at all
    pub refused: Option<String>,
    /// with [`Options::previous`]: the new look was not clearly better than the previous one, which stays
    pub previous_kept: bool,
    /// shooting days of the check photos (stored in the look, so a renewal checks on the same photos)
    pub check_day_list: Vec<String>,
}

impl Outcome {
    /// Metrics (training, check) of a variant.
    pub fn variant(&self, name: &str) -> Option<(&Metrics, &Metrics)> {
        self.variants.iter().find(|v| v.0 == name).map(|v| (&v.1, &v.2))
    }
}

/// Learn the look of `camera`/`style` from the photos (see the module documentation). `meta` gets the numbers of
/// the result; `today` is written into it as the creation date.
pub fn learn(photos: &[Photo], camera: &str, style: &str, today: &str) -> Outcome {
    learn_with(photos, camera, style, today, &Options::default(), &|_| {})
}

/// Choices for [`learn_with`].
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// the learned look in use so far: measured on the same check photos (variant `previous`); a new look is only
    /// offered if it is [`clearly_better`] than this one as well
    pub previous: Option<Look>,
    /// shooting days that form the check set (the days of the previous look's check photos), so a renewed look is
    /// measured on the same photos; days not in the list are training
    pub check_days: Option<Vec<String>>,
}

/// What [`learn_with`] is doing (for a live display).
#[derive(Clone, Copy, Debug)]
pub enum Step<'a> {
    /// fitting this stage on the training photos
    Fitting(Stage),
    /// measuring on the check photos
    Checking,
    /// check result of a variant (`neutral`, `current`, `previous`, `matrix`, `curves`, `table`)
    Measured(&'a str, &'a Metrics),
}

/// Split by the given check days ([`Options::check_days`]): photos of those days are check, all others training.
/// Falls back to [`split_by_day`] if that leaves fewer than [`MIN_CHECK_PHOTOS`] check photos.
pub fn split_fixed(photos: &[Photo], check_days: &[String], salt: &str) -> (Vec<usize>, Vec<usize>) {
    let (check, train): (Vec<usize>, Vec<usize>) = (0..photos.len()).partition(|&i| check_days.iter().any(|d| *d == photos[i].day));
    if check.len() < MIN_CHECK_PHOTOS || train.is_empty() {
        return split_by_day(photos, salt);
    }
    (train, check)
}

/// [`learn`] with options and a live observer.
pub fn learn_with(photos: &[Photo], camera: &str, style: &str, today: &str, opts: &Options, observe: &dyn Fn(Step)) -> Outcome {
    let usable: Vec<Photo> = photos.iter().filter(|p| p.pairs.len() >= 20).cloned().collect();
    let salt = format!("{camera}/{style}");
    let (ti, ci) = match &opts.check_days {
        Some(days) => split_fixed(&usable, days, &salt),
        None => split_by_day(&usable, &salt),
    };
    let train: Vec<&Photo> = ti.iter().map(|&i| &usable[i]).collect();
    let check: Vec<&Photo> = ci.iter().map(|&i| &usable[i]).collect();
    let days = |v: &[&Photo]| {
        let mut d: Vec<&str> = v.iter().map(|p| p.day.as_str()).collect();
        d.sort_unstable();
        d.dedup();
        d.len()
    };
    let mut out = Outcome {
        stage: None,
        look: None,
        variants: Vec::new(),
        train_photos: train.len(),
        check_photos: check.len(),
        train_days: days(&train),
        check_days: days(&check),
        decisions: Vec::new(),
        refused: None,
        previous_kept: false,
        check_day_list: {
            let mut d: Vec<String> = check.iter().map(|p| p.day.clone()).collect();
            d.sort_unstable();
            d.dedup();
            d
        },
    };
    if usable.len() < MIN_PHOTOS {
        out.refused = Some(format!("too few usable photos ({} of at least {MIN_PHOTOS})", usable.len()));
        return out;
    }
    if check.len() < MIN_CHECK_PHOTOS || out.check_days < 1 {
        out.refused = Some(format!("too few shooting days for a separate check ({} days)", out.train_days + out.check_days));
        return out;
    }
    let both = |f: &dyn Fn(&Pair) -> [f32; 3]| (measure(&train, f), measure(&check, f));
    let (nt, nc) = both(&|q| q.base);
    observe(Step::Measured("neutral", &nc));
    out.variants.push(("neutral".into(), nt, nc));
    let (ct, cc) = both(&|q| q.current);
    observe(Step::Measured("current", &cc));
    out.variants.push(("current".into(), ct, cc));
    // the learned look in use so far, on the same check photos (it acts on the plain development, like the new one)
    let previous = opts.previous.as_ref().map(|prev| {
        let (pt, pc) = both(&|q| prev.pixel(q.base));
        observe(Step::Measured("previous", &pc));
        out.variants.push(("previous".into(), pt, pc.clone()));
        pc
    });
    let s = samples(&train);
    observe(Step::Fitting(Stage::Matrix));
    let matrix = fit_matrix(&s);
    let m1 = Model { matrix, curves: None, table: None };
    observe(Step::Fitting(Stage::Curves));
    let curves = fit_curves(&s, matrix);
    let m2 = Model { matrix, curves: Some(curves), table: None };
    observe(Step::Fitting(Stage::Table));
    let smooth = table_smoothness(&train, camera, style);
    out.decisions.push(format!("table smoothness ×{smooth} (chosen on the training days alone)"));
    let table = fit_table(&s, &m2, smooth);
    let m3 = Model { matrix, curves: Some(curves), table: Some(table) };
    observe(Step::Checking);
    let name = format!("{camera}/{style}");
    let mut best_name = "current".to_string();
    let mut best: (Metrics, Metrics) = (out.variants[1].1.clone(), out.variants[1].2.clone());
    let mut kept: Option<(Stage, Look)> = None;
    for (stage, model) in [(Stage::Matrix, &m1), (Stage::Curves, &m2), (Stage::Table, &m3)] {
        let look = model.bake(&name, "{}");
        let (t, c) = both(&|q| look.pixel(q.base));
        observe(Step::Measured(stage.key(), &c));
        let ok = clearly_better(&c, &best.1);
        let photos_better = c.per_photo.iter().zip(&best.1.per_photo).filter(|(a, b)| a < b).count();
        out.decisions.push(format!(
            "{}: check median {:.2} → {:.2}, p90 {:.2} → {:.2}, photos better {}/{} against {best_name}: {}",
            stage.key(),
            best.1.median,
            c.median,
            best.1.p90,
            c.p90,
            photos_better,
            c.per_photo.len(),
            if ok { "kept" } else { "not kept" }
        ));
        out.variants.push((stage.key().into(), t.clone(), c.clone()));
        if ok {
            best = (t, c);
            best_name = stage.key().into();
            kept = Some((stage, look));
        }
    }
    // renewing: the new look must beat the previous learned look on the same check photos as well, else it stays
    if let Some(pc) = &previous {
        match &kept {
            Some(_) if clearly_better(&best.1, pc) => {
                out.decisions.push(format!("against the previous look: check median {:.2} → {:.2}, p90 {:.2} → {:.2}: better", pc.median, best.1.median, pc.p90, best.1.p90));
            }
            _ => {
                out.decisions.push(format!("against the previous look: check median {:.2} → {:.2}, p90 {:.2} → {:.2}: the previous look stays", pc.median, best.1.median, pc.p90, best.1.p90));
                out.previous_kept = true;
                kept = None;
            }
        }
    }
    if let Some((stage, _)) = kept {
        let (t, c) = &best;
        let (nt, nc) = (&out.variants[0].1, &out.variants[0].2);
        let (ct, cc) = (&out.variants[1].1, &out.variants[1].2);
        let meta = serde_json::json!({
            "learned": true,
            "origin": ORIGIN_CHECKED,
            "check_days_list": out.check_day_list,
            "version": FORMAT_VERSION,
            "camera": camera,
            "style": style,
            "stage": stage.key(),
            "interpolation": "tetrahedral",
            "photos": usable.len(),
            "train_photos": train.len(),
            "check_photos": check.len(),
            "train_days": out.train_days,
            "check_days": out.check_days,
            "created": today,
            "fit": {
                "train": {"neutral": [nt.mean, nt.p95], "current": [ct.mean, ct.p95], "learned": [t.mean, t.p95]},
                "check": {"neutral": [nc.mean, nc.p95, nc.median, nc.p90], "current": [cc.mean, cc.p95, cc.median, cc.p90], "learned": [c.mean, c.p95, c.median, c.p90]},
            },
        });
        let model = match stage {
            Stage::Matrix => &m1,
            Stage::Curves => &m2,
            Stage::Table => &m3,
        };
        out.look = Some(model.bake(&name, &meta.to_string()));
        out.stage = Some(stage);
    }
    out
}

/// Smoothness factors of the table tried by [`table_smoothness`].
const SMOOTHNESS: [f64; 4] = [1.0, 3.0, 10.0, 30.0];

/// Choose the smoothness of the table on the training photos alone (the check photos stay untouched): the training
/// days are split once more by day, each half fits all three stages and is measured on the other half; the factor
/// with the lowest mean of the two medians wins. With too few training days the strongest smoothing is used.
fn table_smoothness(train: &[&Photo], camera: &str, style: &str) -> f64 {
    let owned: Vec<Photo> = train.iter().map(|p| (*p).clone()).collect();
    let (a, b) = split_by_day(&owned, &format!("{camera}/{style}/inner"));
    if a.len() < 4 || b.len() < 4 {
        return SMOOTHNESS[SMOOTHNESS.len() - 1];
    }
    let pa: Vec<&Photo> = a.iter().map(|&i| &owned[i]).collect();
    let pb: Vec<&Photo> = b.iter().map(|&i| &owned[i]).collect();
    let prepare = |fit: &[&Photo]| {
        let s = samples(fit);
        let matrix = fit_matrix(&s);
        let curves = fit_curves(&s, matrix);
        (s, Model { matrix, curves: Some(curves), table: None })
    };
    let (sa, ma) = prepare(&pa);
    let (sb, mb) = prepare(&pb);
    let mut best = (f32::MAX, SMOOTHNESS[0]);
    for f in SMOOTHNESS {
        let score: f32 = [(&sa, &ma, &pb), (&sb, &mb, &pa)]
            .iter()
            .map(|(s, m, other)| {
                let model = Model { matrix: m.matrix, curves: m.curves, table: Some(fit_table(s, m, f)) };
                let look = model.bake("inner", "{}");
                measure(other, &|q| look.pixel(q.base)).median
            })
            .sum();
        if score < best.0 {
            best = (score, f);
        }
    }
    best.1
}

// ---------------------------------------------------------------------------------------------
// basis from sample files of other photographers (few photos, no check photos of the user)

/// The three stages fitted on all `photos`: matrix; matrix and curves; matrix, curves and table with the table
/// smoothness `smooth` (see [`SMOOTHNESS`]).
pub fn fit_stages(photos: &[&Photo], smooth: f64) -> [Model; 3] {
    let s = samples(photos);
    let matrix = fit_matrix(&s);
    let curves = fit_curves(&s, matrix);
    let table = fit_table(&s, &Model { matrix, curves: Some(curves), table: None }, smooth);
    [Model { matrix, curves: None, table: None }, Model { matrix, curves: Some(curves), table: None }, Model { matrix, curves: Some(curves), table: Some(table) }]
}

/// Stage of a look fitted on sample files, fixed before any measurement: matrix and curves. One to three scenes per
/// camera cannot carry a 33³ table.
pub const SAMPLES_STAGE: Stage = Stage::Curves;
/// Table smoothness when the table stage is fitted on sample files for comparison (the strongest of [`SMOOTHNESS`]).
pub const SAMPLES_TABLE_SMOOTH: f64 = 30.0;
/// Leave-one-out needs at least this many sample photos.
pub const SAMPLES_LOO_MIN: usize = 3;

/// Result of [`learn_from_samples`].
#[derive(Clone, Debug)]
pub struct SamplesOutcome {
    /// the stages fitted on all usable sample photos (see [`fit_stages`]); `None` without usable photos
    pub models: Option<[Model; 3]>,
    /// the look of [`SAMPLES_STAGE`] with its metadata (origin `samples`, quality `unchecked`)
    pub look: Option<Look>,
    /// usable sample photos (at least 20 blocks)
    pub photos: usize,
    /// on the sample photos themselves (not a check): plain development and the look
    pub fit_neutral: Metrics,
    pub fit_look: Metrics,
    /// leave-one-out over the sample photos (each photo measured with the look fitted on the others), with at least
    /// [`SAMPLES_LOO_MIN`] photos: (plain development, look)
    pub leave_one_out: Option<(Metrics, Metrics)>,
    pub refused: Option<String>,
}

/// A starting look from sample files of other photographers (for example the public-domain samples of raw.pixls.us):
/// matrix and curves ([`SAMPLES_STAGE`]) fitted on the block pairs of all usable photos. There are no check photos of
/// the user's own, so the look is marked `"origin": "samples"`, `"quality": "unchecked"`; `source` (name, URL, licence,
/// files) is stored as given. Leave-one-out over the samples is only a hint (the samples often show one scene).
pub fn learn_from_samples(photos: &[Photo], camera: &str, style: &str, today: &str, source: serde_json::Value) -> SamplesOutcome {
    let usable: Vec<&Photo> = photos.iter().filter(|p| p.pairs.len() >= 20).collect();
    let mut out = SamplesOutcome { models: None, look: None, photos: usable.len(), fit_neutral: Metrics::default(), fit_look: Metrics::default(), leave_one_out: None, refused: None };
    if usable.is_empty() {
        out.refused = Some("no usable sample photo (each needs at least 20 flat blocks)".into());
        return out;
    }
    let models = fit_stages(&usable, SAMPLES_TABLE_SMOOTH);
    let chosen = &models[SAMPLES_STAGE as usize];
    let name = format!("{camera}/{style}");
    let plain = chosen.bake(&name, "{}");
    out.fit_neutral = measure(&usable, &|q| q.base);
    out.fit_look = measure(&usable, &|q| plain.pixel(q.base));
    if usable.len() >= SAMPLES_LOO_MIN {
        let mut de = Vec::new();
        for i in 0..usable.len() {
            let others: Vec<&Photo> = usable.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| *p).collect();
            let look = fit_stages(&others, SAMPLES_TABLE_SMOOTH)[SAMPLES_STAGE as usize].bake(&name, "{}");
            de.push(usable[i].pairs.iter().map(|q| de2000(srgb_to_lab(look.pixel(q.base)), srgb_to_lab(q.reference))).collect());
        }
        out.leave_one_out = Some((measure(&usable, &|q| q.base), metrics_of(de)));
    }
    let m = |x: &Metrics| [x.mean, x.p95, x.median, x.p90];
    let mut meta = serde_json::json!({
        "learned": false,
        "origin": ORIGIN_SAMPLES,
        "quality": QUALITY_UNCHECKED,
        "version": FORMAT_VERSION,
        "camera": camera,
        "style": style,
        "stage": SAMPLES_STAGE.key(),
        "interpolation": "tetrahedral",
        "photos": usable.len(),
        "created": today,
        "fit": {"samples": {"neutral": m(&out.fit_neutral), "look": m(&out.fit_look)}},
        "source": source,
        "method": "graineo learn_look (matrix and curves) on the camera previews embedded in the sample files",
    });
    if let Some((n, l)) = &out.leave_one_out {
        meta["fit"]["leave_one_out"] = serde_json::json!({"neutral": m(n), "look": m(l)});
    }
    out.look = Some(chosen.bake(&name, &meta.to_string()));
    out.models = Some(models);
    out
}

// ---------------------------------------------------------------------------------------------
// tone curve matched to the camera's rendering of one photo

/// Knots of a matched tone curve (equally spaced over 0…1, encoded values).
pub const MATCH_KNOTS: usize = 17;

/// Tone curve that maps the distribution of the encoded luminance (luminance in linear light, then sRGB-encoded) of
/// `base` (the development) onto that of
/// `reference` (the camera's rendering of the same photo, placed on the development and of the same size; black
/// pixels outside the common crop are ignored), by matching the cumulative histograms. 17 knots, ends fixed at 0 and
/// 1, monotone; knots outside the 0.5…99.5 % range of the development's luminance follow the neighbours (no data
/// there). `None` with fewer than 10 000 usable pixels.
///
/// Idea: RawTherapee's "Auto-Matched Curve" (`rtengine/histmatching.cc`, © 2018 Alberto Griggio, GPL-3.0-or-later);
/// own implementation, no code taken over.
pub fn matched_curve(base: &RgbImage, reference: &RgbImage) -> Option<[f32; MATCH_KNOTS]> {
    if base.dimensions() != reference.dimensions() {
        return None;
    }
    let (w, h) = (base.width() as usize, base.height() as usize);
    let (mx, my) = (w / 25, h / 25);
    let (rb, rr) = (base.as_raw(), reference.as_raw());
    let (mut hb, mut hr) = ([0f64; 256], [0f64; 256]);
    // encoded luminance (luminance in linear light, then encoded), the quantity the curve is applied to
    let dec: Vec<f32> = (0..256).map(|v| srgb_decode(v as f32 / 255.0)).collect();
    let luma = |p: &[u8]| ((srgb_encode(luminance([dec[p[0] as usize], dec[p[1] as usize], dec[p[2] as usize]])) * 255.0).round() as usize).min(255);
    for y in my..h - my {
        for x in mx..w - mx {
            let i = (y * w + x) * 3;
            let r = &rr[i..i + 3];
            if r == [0, 0, 0] {
                continue;
            }
            hb[luma(&rb[i..i + 3])] += 1.0;
            hr[luma(r)] += 1.0;
        }
    }
    let n: f64 = hb.iter().sum();
    if n < 10_000.0 {
        return None;
    }
    // cumulative distributions at the bin centres
    let cdf = |hist: &[f64; 256]| {
        let mut c = [0f64; 256];
        let mut s = 0.0;
        for i in 0..256 {
            c[i] = (s + hist[i] / 2.0) / n;
            s += hist[i];
        }
        c
    };
    let (cb, cr) = (cdf(&hb), cdf(&hr));
    // reference level with the same cumulative share (linear between bin centres)
    let inverse = |q: f64| -> f32 {
        if q <= cr[0] {
            return 0.0;
        }
        for i in 1..256 {
            if cr[i] >= q {
                let f = if cr[i] > cr[i - 1] { (q - cr[i - 1]) / (cr[i] - cr[i - 1]) } else { 0.0 };
                return ((i - 1) as f64 + f) as f32 / 255.0;
            }
        }
        1.0
    };
    let mut k: [Option<f32>; MATCH_KNOTS] = [None; MATCH_KNOTS];
    k[0] = Some(0.0);
    k[MATCH_KNOTS - 1] = Some(1.0);
    for (j, kj) in k.iter_mut().enumerate().take(MATCH_KNOTS - 1).skip(1) {
        let v = j as f32 / (MATCH_KNOTS - 1) as f32;
        let fb = v * 255.0;
        let (i, f) = ((fb as usize).min(254), (fb - fb.floor()) as f64);
        let q = cb[i] * (1.0 - f) + cb[i + 1] * f;
        if (0.005..=0.995).contains(&q) {
            *kj = Some(inverse(q));
        }
    }
    // fill knots without data from their neighbours (linear), then light smoothing and monotonicity
    let mut c = [0f32; MATCH_KNOTS];
    for j in 0..MATCH_KNOTS {
        c[j] = match k[j] {
            Some(v) => v,
            None => {
                let a = (0..j).rev().find(|&i| k[i].is_some()).unwrap_or(0);
                let b = (j + 1..MATCH_KNOTS).find(|&i| k[i].is_some()).unwrap_or(MATCH_KNOTS - 1);
                let (va, vb) = (k[a].unwrap_or(0.0), k[b].unwrap_or(1.0));
                va + (vb - va) * (j - a) as f32 / (b - a).max(1) as f32
            }
        };
    }
    let raw = c;
    for j in 1..MATCH_KNOTS - 1 {
        c[j] = 0.25 * raw[j - 1] + 0.5 * raw[j] + 0.25 * raw[j + 1];
    }
    for j in 1..MATCH_KNOTS {
        c[j] = c[j].max(c[j - 1]).min(1.0);
    }
    Some(c)
}

fn curve_at(curve: &[f32; MATCH_KNOTS], v: f32) -> f32 {
    let t = v.clamp(0.0, 1.0) * (MATCH_KNOTS - 1) as f32;
    let i = (t as usize).min(MATCH_KNOTS - 2);
    let f = t - i as f32;
    curve[i] * (1.0 - f) + curve[i + 1] * f
}

/// Apply a matched tone curve ([`matched_curve`]) to each channel of an encoded sRGB colour.
pub fn apply_matched_curve(curve: &[f32; MATCH_KNOTS], x: [f32; 3]) -> [f32; 3] {
    x.map(|v| curve_at(curve, v))
}

/// Apply a matched tone curve to the luminance only: the encoded luminance goes through the curve and the linear
/// colour is scaled by the ratio, so hue and saturation stay as they are.
pub fn apply_matched_curve_luminance(curve: &[f32; MATCH_KNOTS], x: [f32; 3]) -> [f32; 3] {
    let l = lin(x);
    let y = luminance(l);
    if y <= 1e-6 {
        return x;
    }
    let f = srgb_decode(curve_at(curve, srgb_encode(y))) / y;
    encode_gamut(l.map(|v| v * f))
}

// ---------------------------------------------------------------------------------------------
// origin and quality of a look (learned, bundled, shared)

/// Metadata value `origin`: learned and checked on photos that were not used to fit it.
pub const ORIGIN_CHECKED: &str = "checked";
/// Metadata value `origin`: derived from the picture style settings in the camera's makernote, not measured.
pub const ORIGIN_MAKERNOTE: &str = "makernote";
/// Metadata value `origin`: a starting point without measurement (taken over from a related camera, or of unknown
/// origin).
pub const ORIGIN_BASE: &str = "base";
/// Metadata value `origin`: fitted on sample files of other photographers ([`learn_from_samples`]), not checked on
/// the user's own photos.
pub const ORIGIN_SAMPLES: &str = "samples";
/// Metadata value `quality`: not checked on photos that were not used to fit it.
pub const QUALITY_UNCHECKED: &str = "unchecked";

/// Where a look comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// learned (or fitted) and checked on photos that were not used to fit it
    Checked,
    /// from the picture style settings in the makernote
    Makernote,
    /// fitted on sample files of other photographers, not checked
    Samples,
    /// a basis without measurement
    Base,
}

impl Origin {
    pub fn key(self) -> &'static str {
        match self {
            Self::Checked => ORIGIN_CHECKED,
            Self::Makernote => ORIGIN_MAKERNOTE,
            Self::Samples => ORIGIN_SAMPLES,
            Self::Base => ORIGIN_BASE,
        }
    }
}

/// Origin and measured quality of a look, read from its metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub origin: Origin,
    /// ΔE00 against the camera's rendering on photos not used for the fit: median of the check photos for learned
    /// looks, mean of the cross-validation for bundled ones; `None` without a measurement
    pub delta_e: Option<f32>,
    /// the same for the development without this look (standard development of the day it was measured)
    pub before: Option<f32>,
    /// photos the look was fitted on
    pub photos: Option<u64>,
    /// date it was learned (`YYYY-MM-DD`)
    pub created: Option<String>,
    /// look of another camera it was taken over from
    pub derived_from: Option<String>,
    /// loaded from a shared file
    pub imported: bool,
    /// for looks from sample files: where the samples come from (`name, licence, n files`)
    pub source: Option<String>,
}

/// Read [`Provenance`] from look metadata (learned looks, bundled looks, shared files). Looks without an `origin`
/// field: learned ones with check numbers and bundled ones with cross-validation count as checked, the rest as basis.
pub fn provenance(meta: &str) -> Provenance {
    let v: serde_json::Value = serde_json::from_str(meta).unwrap_or_default();
    let f = |x: &serde_json::Value| x.as_f64().map(|v| v as f32).filter(|v| v.is_finite());
    let learned_check = f(&v["fit"]["check"]["learned"][2]);
    let bundled = f(&v["de_look"]);
    let derived_from = v["derived_from"].as_str().map(str::to_string);
    let origin = match v["origin"].as_str() {
        Some(ORIGIN_CHECKED) => Origin::Checked,
        Some(ORIGIN_MAKERNOTE) => Origin::Makernote,
        Some(ORIGIN_SAMPLES) => Origin::Samples,
        Some(_) => Origin::Base,
        None if derived_from.is_some() => Origin::Base,
        None if learned_check.is_some() || bundled.is_some() => Origin::Checked,
        None => Origin::Base,
    };
    let checked = origin == Origin::Checked;
    Provenance {
        origin,
        delta_e: if checked { learned_check.or(bundled) } else { None },
        before: if checked { f(&v["fit"]["check"]["current"][2]).or(f(&v["de_neutral"])) } else { None },
        photos: v["photos"].as_u64().or(v["images"].as_u64()),
        created: v["created"].as_str().map(str::to_string),
        derived_from,
        imported: v["imported"].as_bool().unwrap_or(false),
        source: v["source"]["name"].as_str().map(|n| {
            let files = v["source"]["files"].as_array().map_or(0, |a| a.len());
            let licence = v["source"]["license"].as_str().unwrap_or("?");
            format!("{n}, {licence}, {files}")
        }),
    }
}

/// Quality class of a ΔE00 against the camera's rendering: 0 very close (≤ 1), 1 close (≤ 2), 2 moderate (≤ 3),
/// 3 coarse.
pub fn quality_class(delta_e: f32) -> u8 {
    match delta_e {
        d if d <= 1.0 => 0,
        d if d <= 2.0 => 1,
        d if d <= 3.0 => 2,
        _ => 3,
    }
}

/// Metadata fields that may leave the computer with a shared look: numbers about the fit, never file names, folders,
/// shooting days or anything else about the photos.
const SHAREABLE: [&str; 25] = [
    "learned", "origin", "version", "camera", "camera_name", "style", "stage", "interpolation", "photos", "train_photos", "check_photos", "train_days", "check_days", "created",
    "fit", "derived_from", "images", "de_neutral", "de_look", "better", "mono", "weights", "quality", "source", "method",
];

/// Metadata of a look for sharing (only the [`SHAREABLE`] fields; `extra` fields are added).
pub fn shareable_meta(meta: &str, extra: &[(&str, serde_json::Value)]) -> String {
    let v: serde_json::Value = serde_json::from_str(meta).unwrap_or_default();
    let mut out = serde_json::Map::new();
    if let Some(o) = v.as_object() {
        for (k, x) in o {
            if SHAREABLE.contains(&k.as_str()) {
                out.insert(k.clone(), x.clone());
            }
        }
    }
    for (k, x) in extra {
        out.insert(k.to_string(), x.clone());
    }
    serde_json::Value::Object(out).to_string()
}

/// The reference placed on the development: an image of `w`×`h` (the development's size) sampled from `reference`
/// through the alignment; outside the common crop black. For side-by-side and wipe comparisons.
pub fn warp_reference(w: u32, h: u32, reference: &RgbImage, a: &Alignment) -> RgbImage {
    use rayon::prelude::*;
    let (ra, rb) = (w as f32 / h as f32, reference.width() as f32 / reference.height() as f32);
    let r = if ra >= 1.0 { ra.min(rb) } else { ra.max(rb) };
    let (ax, ay, aw, ah) = crop(w, h, r);
    let (bx, by, bw, bh) = crop(reference.width(), reference.height(), r);
    let (rw, rh) = (reference.width() as usize, reference.height() as usize);
    let src = reference.as_raw();
    let mut out = RgbImage::new(w, h);
    let row_len = w as usize * 3;
    out.as_mut().par_chunks_mut(row_len.max(3)).enumerate().for_each(|(y, row)| {
        let v = (y as f32 + 0.5 - ay) / ah;
        for x in 0..w as usize {
            let u = (x as f32 + 0.5 - ax) / aw;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let sx = (bx + (0.5 + (u - 0.5) * a.scale + a.dx) * bw - 0.5).clamp(0.0, (rw - 1) as f32);
            let sy = (by + (0.5 + (v - 0.5) * a.scale + a.dy) * bh - 0.5).clamp(0.0, (rh - 1) as f32);
            let (ix, iy) = ((sx as usize).min(rw.saturating_sub(2)), (sy as usize).min(rh.saturating_sub(2)));
            let (fx, fy) = (sx - ix as f32, sy - iy as f32);
            for c in 0..3 {
                let p = |xx: usize, yy: usize| src[(yy.min(rh - 1) * rw + xx.min(rw - 1)) * 3 + c] as f32;
                let top = p(ix, iy) * (1.0 - fx) + p(ix + 1, iy) * fx;
                let bottom = p(ix, iy + 1) * (1.0 - fx) + p(ix + 1, iy + 1) * fx;
                row[x * 3 + c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}

/// Colour coverage of the reference blocks: (share of skin-like colours, share of strongly saturated colours).
pub fn coverage(photos: &[Photo]) -> (f32, f32) {
    let (mut n, mut skin, mut sat) = (0usize, 0usize, 0usize);
    for q in photos.iter().flat_map(|p| &p.pairs) {
        let o = srgb_lin_to_oklab(lin(q.reference));
        let c = o[1].hypot(o[2]);
        let h = o[2].atan2(o[1]).to_degrees().rem_euclid(360.0);
        n += 1;
        if (0.45..0.9).contains(&o[0]) && (0.03..0.14).contains(&c) && (25.0..85.0).contains(&h) {
            skin += 1;
        }
        if c > 0.15 {
            sat += 1;
        }
    }
    let n = n.max(1) as f32;
    (skin as f32 / n, sat as f32 / n)
}

/// Brightness and colour coverage of a set of photos (each photo counts the same), for the statistics of the
/// "Kamera-Looks" window: shares of Oklab lightness in 5 bins, of colourful samples per 30° hue sector (12 bins,
/// from red over yellow and green to blue), of skin-like and of strongly saturated samples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub photos: usize,
    pub brightness: [f32; 5],
    pub hues: [f32; 12],
    pub skin: f32,
    pub saturated: f32,
}

impl Coverage {
    /// Add one photo given by its colours (encoded sRGB, 0…1).
    pub fn add(&mut self, colours: impl Iterator<Item = [f32; 3]>) {
        let (mut n, mut b, mut h, mut skin, mut sat) = (0f32, [0f32; 5], [0f32; 12], 0f32, 0f32);
        for c in colours {
            let o = srgb_lin_to_oklab(lin(c));
            let chroma = o[1].hypot(o[2]);
            let hue = o[2].atan2(o[1]).to_degrees().rem_euclid(360.0);
            n += 1.0;
            b[((o[0] * 5.0) as usize).min(4)] += 1.0;
            if chroma > 0.05 {
                h[((hue / 30.0) as usize).min(11)] += 1.0;
            }
            if (0.45..0.9).contains(&o[0]) && (0.03..0.14).contains(&chroma) && (25.0..85.0).contains(&hue) {
                skin += 1.0;
            }
            if chroma > 0.15 {
                sat += 1.0;
            }
        }
        if n == 0.0 {
            return;
        }
        let k = self.photos as f32;
        let mix = |old: f32, new: f32| (old * k + new / n) / (k + 1.0);
        for i in 0..5 {
            self.brightness[i] = mix(self.brightness[i], b[i]);
        }
        for i in 0..12 {
            self.hues[i] = mix(self.hues[i], h[i]);
        }
        self.skin = mix(self.skin, skin);
        self.saturated = mix(self.saturated, sat);
        self.photos += 1;
    }

    /// Coverage of an 8-bit image (sampled on a grid of about 64 × 64).
    pub fn add_image(&mut self, img: &RgbImage) {
        let (w, h) = (img.width().max(1), img.height().max(1));
        let step = (w.max(h) / 64).max(1);
        let raw = img.as_raw();
        let it = (0..h).step_by(step as usize).flat_map(move |y| (0..w).step_by(step as usize).map(move |x| ((y * w + x) * 3) as usize)).map(|i| [raw[i], raw[i + 1], raw[i + 2]].map(|v| v as f32 / 255.0));
        self.add(it);
    }

    /// Coverage of the camera's rendering in the learning blocks.
    pub fn of_photos(photos: &[Photo]) -> Self {
        let mut c = Coverage::default();
        for p in photos {
            c.add(p.pairs.iter().map(|q| q.reference));
        }
        c
    }
}

/// Coverage below which the window warns (shares of all blocks).
pub const THIN_SKIN: f32 = 0.01;
pub const THIN_SATURATED: f32 = 0.01;

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth "camera look" for the tests: a bit more contrast, warmer, slightly more saturated, and a hue twist in
    /// the blues. Applied on encoded sRGB.
    fn camera(x: [f32; 3]) -> [f32; 3] {
        let l = lin(x);
        let m = [[1.08, -0.05, -0.03], [-0.02, 1.04, -0.02], [-0.01, -0.06, 1.07]];
        let y = [0, 1, 2].map(|r| m[r][0] * l[0] + m[r][1] * l[1] + m[r][2] * l[2]);
        let mut e = encode_gamut(y);
        e = e.map(|v| (v + 0.06 * (v * std::f32::consts::PI).sin() * (v - 0.5).signum() * -1.0).clamp(0.0, 1.0));
        let mut o = srgb_lin_to_oklab(lin(e));
        let h = o[2].atan2(o[1]);
        let c = o[1].hypot(o[2]);
        let twist = 0.15 * (-(h - (-1.7)).powi(2) / 0.3).exp();
        o[1] = c * (h + twist).cos();
        o[2] = c * (h + twist).sin();
        encode_gamut(oklab_to_srgb_lin(o))
    }

    fn rng(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Synthetic photos: each a few random colour clusters (like flat areas), with a little noise on the reference.
    fn photos(n: usize, look: &dyn Fn([f32; 3]) -> [f32; 3], seed: u64) -> Vec<Photo> {
        let mut s = seed;
        (0..n)
            .map(|i| {
                let mut pairs = Vec::new();
                for _ in 0..12 {
                    let c = [rng(&mut s), rng(&mut s), rng(&mut s)].map(|v| 0.08 + 0.84 * v);
                    for _ in 0..30 {
                        let x = c.map(|v| (v + 0.05 * (rng(&mut s) - 0.5)).clamp(0.0, 1.0));
                        let y = look(x).map(|v| (v + 0.004 * (rng(&mut s) - 0.5)).clamp(0.0, 1.0));
                        pairs.push(Pair { base: x, reference: y, current: x });
                    }
                }
                Photo { name: format!("p{i}"), day: format!("2026-01-{:02}", i / 2 + 1), folder: "f".into(), pairs, ..Default::default() }
            })
            .collect()
    }

    #[test]
    fn delta_e_reference_values() {
        // Sharma et al. 2005, test pairs 1 and 7
        assert!((de2000([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485]) - 2.0425).abs() < 1e-3);
        assert!((de2000([50.0, 0.0, 0.0], [50.0, -1.0, 2.0]) - 2.3669).abs() < 1e-3);
        let w = srgb_to_lab([1.0, 1.0, 1.0]);
        assert!((w[0] - 100.0).abs() < 0.05 && w[1].abs() < 0.05 && w[2].abs() < 0.05);
    }

    #[test]
    fn identity_data_learns_nothing_offered() {
        let p = photos(30, &|x| x, 7);
        let o = learn(&p, "cam", "std", "2026-11-16");
        assert!(o.refused.is_none(), "{:?}", o.refused);
        assert!(o.stage.is_none(), "{:?}", o.decisions);
        assert!(o.look.is_none());
    }

    #[test]
    fn learns_a_smooth_look_and_checks_it_on_other_days() {
        let p = photos(32, &camera, 3);
        let o = learn(&p, "cam", "std", "2026-11-16");
        assert!(o.refused.is_none(), "{:?}", o.refused);
        let stage = o.stage.expect("a stage is kept");
        let look = o.look.as_ref().unwrap();
        assert!(look.tetrahedral());
        let (_, before) = o.variant("current").unwrap();
        let (_, after) = o.variant(stage.key()).unwrap();
        assert!(after.median < 0.5 * before.median, "{} → {} ({:?})", before.median, after.median, o.decisions);
        // the check photos are of other days than the training photos
        assert!(o.train_days > 0 && o.check_days > 0);
        // gray stays gray, black and white stay put
        for g in [0.0, 0.3, 0.6, 1.0] {
            let y = look.pixel([g, g, g]);
            assert!((y[0] - y[1]).abs() < 0.02 && (y[1] - y[2]).abs() < 0.02, "{g}: {y:?}");
        }
        assert!(look.pixel([1.0; 3]).iter().all(|v| *v > 0.97));
        assert!(look.pixel([0.0; 3]).iter().all(|v| *v < 0.02));
        // the stored metadata says what it is
        let meta: serde_json::Value = serde_json::from_str(&look.meta).unwrap();
        assert_eq!(meta["camera"], "cam");
        assert_eq!(meta["learned"], true);
        assert_eq!(meta["interpolation"], "tetrahedral");
        // the stages were tried in order
        assert_eq!(o.variants.iter().map(|v| v.0.as_str()).collect::<Vec<_>>(), ["neutral", "current", "matrix", "curves", "table"]);
    }

    #[test]
    fn renewal_checks_on_the_same_days_and_keeps_a_previous_look_that_is_not_beaten() {
        let p = photos(32, &camera, 3);
        let first = learn(&p, "cam", "std", "2026-11-16");
        let prev = first.look.clone().expect("first look");
        let meta: serde_json::Value = serde_json::from_str(&prev.meta).unwrap();
        assert_eq!(meta["origin"], ORIGIN_CHECKED);
        let days: Vec<String> = meta["check_days_list"].as_array().unwrap().iter().map(|d| d.as_str().unwrap().to_string()).collect();
        assert_eq!(days, first.check_day_list);
        // more photos of new days: the check photos stay those of the stored days
        let mut more = p.clone();
        more.extend(photos(8, &camera, 11).into_iter().map(|mut q| {
            q.day = format!("2026-02-{}", q.day.rsplit('-').next().unwrap());
            q
        }));
        let steps = std::cell::RefCell::new(Vec::new());
        let o = learn_with(&more, "cam", "std", "2026-11-17", &Options { previous: Some(prev.clone()), check_days: Some(days.clone()) }, &|s| steps.borrow_mut().push(format!("{s:?}")));
        assert_eq!(o.check_day_list, days);
        assert!(o.variant("previous").is_some());
        // learned again from the same photos, the new look is no clear gain over the previous one: it stays
        let same = learn_with(&p, "cam", "std", "2026-11-17", &Options { previous: Some(prev), check_days: Some(days.clone()) }, &|_| {});
        assert!(same.previous_kept && same.look.is_none(), "{:?}", same.decisions);
        let steps = steps.into_inner();
        assert!(steps.iter().any(|s| s.contains("Fitting(Table)")) && steps.iter().any(|s| s.contains("\"previous\"")));
        // against an identity "previous look" the new one is clearly better and offered
        let n = 3;
        let identity = Look::from_table("cam/std", "{}", n, vec![[0.0; 3]; n * n * n]).unwrap();
        let o = learn_with(&more, "cam", "std", "2026-11-17", &Options { previous: Some(identity), check_days: Some(days) }, &|_| {});
        assert!(!o.previous_kept && o.look.is_some(), "{:?}", o.decisions);
    }

    #[test]
    fn provenance_of_learned_bundled_derived_and_shared_looks() {
        let learned = r#"{"learned":true,"photos":40,"created":"2026-11-16","fit":{"check":{"current":[2,3,1.9,3.3],"learned":[1.5,2.5,1.39,3.0]}}}"#;
        let p = provenance(learned);
        assert_eq!((p.origin, p.delta_e, p.before, p.photos), (Origin::Checked, Some(1.39), Some(1.9), Some(40)));
        let bundled = r#"{"camera":"gr","style":"standard","images":40,"de_neutral":3.46,"de_look":2.42,"better":1.0}"#;
        let p = provenance(bundled);
        assert_eq!((p.origin, p.delta_e, p.photos), (Origin::Checked, Some(2.42), Some(40)));
        let derived = r#"{"learned":false,"derived_from":"gr3/standard"}"#;
        assert_eq!(provenance(derived).origin, Origin::Base);
        assert_eq!(provenance(derived).delta_e, None);
        assert_eq!(provenance("{}").origin, Origin::Base);
        assert_eq!(provenance(r#"{"origin":"makernote"}"#).origin, Origin::Makernote);
        assert_eq!([0.8, 1.5, 2.5, 4.0].map(quality_class), [0, 1, 2, 3]);
        // sharing keeps numbers, drops shooting days and anything unknown
        let m = r#"{"learned":true,"camera":"gr","check_days_list":["2026-01-02"],"library_photos":300,"fit":{"x":1}}"#;
        let s: serde_json::Value = serde_json::from_str(&shareable_meta(m, &[("imported", serde_json::json!(true))])).unwrap();
        assert_eq!(s["camera"], "gr");
        assert_eq!(s["fit"]["x"], 1);
        assert_eq!(s["imported"], true);
        assert!(s.get("check_days_list").is_none() && s.get("library_photos").is_none());
    }

    #[test]
    fn warped_reference_lies_on_the_development() {
        let base = scene(200, 133);
        let reference = image::imageops::resize(&scene(300, 200), 300, 200, image::imageops::FilterType::Triangle);
        let a = align(&base, &reference);
        let w = warp_reference(base.width(), base.height(), &reference, &a);
        assert_eq!(w.dimensions(), base.dimensions());
        let d: f64 = base.as_raw().iter().zip(w.as_raw()).map(|(a, b)| (*a as f64 - *b as f64).abs()).sum::<f64>() / base.as_raw().len() as f64;
        assert!(d < 12.0, "mean difference {d}");
    }

    #[test]
    fn too_few_photos_or_one_day_is_refused() {
        let p = photos(10, &camera, 5);
        assert!(learn(&p, "c", "s", "d").refused.is_some());
        let mut p = photos(30, &camera, 5);
        for q in &mut p {
            q.day = "2026-01-01".into();
        }
        let o = learn(&p, "c", "s", "d");
        assert!(o.refused.is_some() && o.look.is_none());
    }

    #[test]
    fn guard_rule() {
        let m = |median: f32, p90: f32, pp: &[f32]| Metrics { median, p90, per_photo: pp.to_vec(), ..Default::default() };
        let old = m(2.0, 4.0, &[2.0, 2.0, 2.0, 2.0, 2.0]);
        assert!(clearly_better(&m(1.9, 4.0, &[1.9, 1.9, 1.9, 2.1, 2.1]), &old));
        assert!(!clearly_better(&m(1.95, 4.0, &[1.9; 5]), &old), "only 2.5 % lower");
        assert!(!clearly_better(&m(1.8, 4.1, &[1.8; 5]), &old), "p90 higher");
        assert!(!clearly_better(&m(1.8, 3.9, &[1.8, 1.8, 2.1, 2.1, 2.1]), &old), "only 40 % of the photos better");
    }

    #[test]
    fn split_keeps_days_together_and_balances() {
        let p: Vec<Photo> = (0..40).map(|i| Photo { day: format!("d{}", i % 9), ..Default::default() }).collect();
        let (t, c) = split_by_day(&p, "x");
        assert_eq!(t.len() + c.len(), 40);
        for i in &t {
            assert!(c.iter().all(|j| p[*j].day != p[*i].day));
        }
        assert!((t.len() as i32 - c.len() as i32).abs() <= 6, "{} {}", t.len(), c.len());
    }

    #[test]
    fn selection_spreads_over_days() {
        let cands: Vec<(String, String)> = (0..100).map(|i| (format!("2020-01-{:02}", i % 5 + 1), format!("IMG{i:03}"))).collect();
        let s = select(&cands, 12);
        assert_eq!(s.len(), 12);
        let mut per_day = std::collections::HashMap::new();
        for i in &s {
            *per_day.entry(&cands[*i].0).or_insert(0) += 1;
        }
        assert!(per_day.values().all(|n| *n <= PER_DAY) && per_day.len() == 5);
        assert!(select(&cands, 1000).len() == 5 * PER_DAY);
    }

    fn scene(w: u32, h: u32) -> RgbImage {
        // flat colour patches with soft borders, a gradient and some texture in one corner
        RgbImage::from_fn(w, h, |x, y| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let k = ((u * 6.0) as u32 + 7 * (v * 4.0) as u32) % 9;
            let base = [[200.0, 60.0, 50.0], [60.0, 170.0, 70.0], [50.0, 80.0, 200.0], [210.0, 190.0, 60.0], [120.0, 120.0, 120.0], [230.0, 180.0, 150.0], [40.0, 40.0, 40.0], [90.0, 160.0, 210.0], [180.0, 90.0, 160.0]][k as usize];
            let tex = if u > 0.8 && v > 0.8 { 40.0 * ((x % 3) as f32 - 1.0) } else { 0.0 };
            image::Rgb(base.map(|c: f32| (c * (0.9 + 0.2 * u) + tex).clamp(0.0, 245.0) as u8))
        })
    }

    #[test]
    fn alignment_finds_scale_and_shift() {
        let big = scene(2400, 1600);
        // base: the full frame at 1200×800; reference: a slightly tighter crop (scale 0.98) shifted right, at 1000 px
        let base = image::imageops::resize(&big, 1200, 800, image::imageops::FilterType::Triangle);
        let (cw, ch) = (2400.0 * 0.98, 1600.0 * 0.98);
        let (x0, y0) = ((2400.0 - cw) / 2.0 + 24.0, (1600.0 - ch) / 2.0);
        let sub = image::imageops::crop_imm(&big, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
        let reference = image::imageops::resize(&sub, 1000, 667, image::imageops::FilterType::Triangle);
        let a = align(&base, &reference);
        assert!(a.correlation > 0.95, "{a:?}");
        // a base point u lies at (u·2400 − x0)/cw in the reference: scale 1/0.98, shift −(x0 − 24 … )/cw
        assert!((a.scale - 1.0 / 0.98).abs() < 0.006, "{a:?}");
        assert!((a.dx + 24.0 / cw).abs() < 0.004, "{a:?}");
        let pairs = block_pairs(&base, None, &reference, &a);
        assert!(pairs.len() > 300, "{}", pairs.len());
        // flat patches: reference and base agree (same image)
        let d: f32 = pairs.iter().map(|p| de2000(srgb_to_lab(p.base), srgb_to_lab(p.reference))).sum::<f32>() / pairs.len() as f32;
        assert!(d < 1.5, "mean ΔE00 {d}");
        assert!(midtone_ev(&pairs).unwrap().abs() < 0.05);
    }

    #[test]
    fn style_keys_and_adaptive_settings() {
        let ps = |style: &str, details: Vec<(&'static str, &str)>| PictureStyle { style: style.into(), details: details.into_iter().map(|(a, b)| (a, b.to_string())).collect(), ..Default::default() };
        assert_eq!(style_key(&ps("provia", vec![("Farbe", "0"), ("Lichterton", "0"), ("Schattenton", "0"), ("DR", "100")])), "provia");
        assert_eq!(style_key(&ps("provia", vec![("Lichterton", "+1"), ("Schattenton", "+1")])), "provia_h+1_s+1");
        assert_eq!(style_key(&ps("velvia", vec![("Farbe", "-1 (niedrig)")])), "velvia_c-1");
        assert_eq!(adaptive(&ps("provia", vec![("DRAuto", "1")])), Some("DR Auto"));
        assert_eq!(adaptive(&ps("standard", vec![("ALO", "3")])), None);
        assert_eq!(adaptive(&ps("standard", vec![("ALO", "0")])), Some("Auto Lighting Optimizer"));
        assert_eq!(adaptive(&ps("standard", vec![("ShadowCorrection", "0"), ("HighlightCorrection", "0")])), None);
        assert!(!learnable_style(&ps("user2-standard", vec![])) && learnable_style(&ps("classic-chrome", vec![])));
    }

    #[test]
    fn look_from_samples_is_marked_unchecked_with_its_source() {
        let p = photos(3, &camera, 11);
        let src = serde_json::json!({"name": "raw.pixls.us", "url": "https://raw.pixls.us/data/X/Y/", "license": "CC0-1.0", "files": ["a.RAF", "b.RAF", "c.RAF"]});
        let o = learn_from_samples(&p, "cam", "std", "2026-11-16", src);
        assert!(o.refused.is_none() && o.photos == 3);
        assert!(o.fit_look.median < 0.7 * o.fit_neutral.median, "{:?} {:?}", o.fit_look.median, o.fit_neutral.median);
        let (n, l) = o.leave_one_out.as_ref().expect("leave-one-out with three photos");
        assert!(l.median < n.median);
        let look = o.look.as_ref().unwrap();
        let v: serde_json::Value = serde_json::from_str(&look.meta).unwrap();
        assert_eq!((v["origin"].as_str(), v["quality"].as_str(), v["stage"].as_str()), (Some(ORIGIN_SAMPLES), Some(QUALITY_UNCHECKED), Some("curves")));
        let pr = provenance(&look.meta);
        assert_eq!((pr.origin, pr.delta_e, pr.photos), (Origin::Samples, None, Some(3)));
        assert_eq!(pr.source.as_deref(), Some("raw.pixls.us, CC0-1.0, 3"));
        // shared: origin, quality and source go along
        let shared: serde_json::Value = serde_json::from_str(&shareable_meta(&look.meta, &[])).unwrap();
        assert_eq!(shared["source"]["license"].as_str(), Some("CC0-1.0"));
        // fewer than three photos: no leave-one-out; none usable: refused
        assert!(learn_from_samples(&p[..1], "cam", "std", "2026-11-16", serde_json::Value::Null).leave_one_out.is_none());
        let empty = vec![Photo::default()];
        assert!(learn_from_samples(&empty, "cam", "std", "2026-11-16", serde_json::Value::Null).look.is_none());
    }

    #[test]
    fn matched_curve_follows_the_reference_tones_and_keeps_colours() {
        // a photo-like picture: continuous tones (gradients in both directions, a little noise), some colour
        let mut seed = 5u64;
        let base = RgbImage::from_fn(600, 400, |x, y| {
            let t = 0.02 + 0.96 * (x as f32 / 600.0 * 0.7 + y as f32 / 400.0 * 0.3);
            let n = 0.01 * (rng(&mut seed) - 0.5);
            image::Rgb([t * 1.05 + n, t + n, t * 0.9 + n].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
        });
        // the same picture as identity: the curve is close to the identity
        let c = matched_curve(&base, &base).unwrap();
        for (j, v) in c.iter().enumerate() {
            assert!((v - j as f32 / (MATCH_KNOTS - 1) as f32).abs() < 0.03, "{j} {v}");
        }
        // reference darker in the midtones (gamma 1.3 on the encoded values): the curve pulls the midtones down
        let darker = RgbImage::from_fn(600, 400, |x, y| image::Rgb(base.get_pixel(x, y).0.map(|v| ((v as f32 / 255.0).powf(1.3) * 255.0).round().max(1.0) as u8)));
        let c = matched_curve(&base, &darker).unwrap();
        assert!(c.windows(2).all(|w| w[1] >= w[0]) && c[0] == 0.0 && c[MATCH_KNOTS - 1] == 1.0);
        let mid = apply_matched_curve_luminance(&c, [0.5, 0.5, 0.5]);
        assert!(mid[0] < 0.47 && (mid[0] - mid[1]).abs() < 1e-4 && (mid[1] - mid[2]).abs() < 1e-4, "{mid:?}");
        // luminance only: the hue of a colour stays
        let red = [0.7, 0.3, 0.2];
        let r = apply_matched_curve_luminance(&c, red);
        let (h0, h1) = (srgb_lin_to_oklab(lin(red)), srgb_lin_to_oklab(lin(r)));
        assert!((h0[2].atan2(h0[1]) - h1[2].atan2(h1[1])).abs() < 0.02);
        // too small or of another size: no curve
        assert!(matched_curve(&scene(60, 40), &scene(60, 40)).is_none() && matched_curve(&base, &scene(300, 200)).is_none());
    }
}
