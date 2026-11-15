//! Comparison of our own development with a reference (reference preview in Adobe RGB or
//! camera JPEG in sRGB): ΔE2000 by brightness zones of the reference, exposure difference in EV,
//! color difference (ΔL, Δa, Δb, ΔC) and PSNR. Both images are center-cropped to the common
//! aspect ratio and area-averaged down to a 400 px long edge
//! (small offsets and sharpness differences hardly matter that way).

use anyhow::Result;
use image::RgbImage;
use raw::color;

pub struct Comparison {
    pub de: f32,
    /// shadows (L* < 30), midtones (30…70), highlights (> 70) – by L* of the reference
    pub de_zone: [f32; 3],
    pub fraction: [f32; 3],
    /// log2(Y ours / Y reference), averaged over the midtone pixels
    pub ev_mid: f32,
    /// log2 of the mean luminances over the whole image
    pub ev: f32,
    pub dl: f32,
    pub da: f32,
    pub db: f32,
    /// chroma ours − reference (positive = more saturated)
    pub dc: f32,
    pub psnr: f32,
    /// mean L* difference (ours − reference) in shadows, midtones, highlights (by L* of the reference)
    pub dl_zone: [f32; 3],
}

const LONG_EDGE: usize = 400;

fn srgb_eotf(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// 8-bit image → XYZ (D65) per pixel.
fn to_xyz(img: &RgbImage, adobe: bool) -> Vec<[f32; 3]> {
    let m = if adobe { color::adobe_rgb_to_xyz_d65() } else { color::srgb_to_xyz_d65() };
    let lut: Vec<f32> = (0..256)
        .map(|v| {
            let x = v as f32 / 255.0;
            if adobe {
                x.powf(563.0 / 256.0)
            } else {
                srgb_eotf(x)
            }
        })
        .collect();
    img.pixels().map(|p| color::mulv(&m, [lut[p.0[0] as usize], lut[p.0[1] as usize], lut[p.0[2] as usize]])).collect()
}

/// Center-crop to aspect ratio `r` and downscale to (tw, th) by area averaging.
fn downscale(px: &[[f32; 3]], w: usize, h: usize, r: f32, tw: usize, th: usize) -> Vec<[f32; 3]> {
    let (wf, hf) = (w as f32, h as f32);
    let (cw, ch) = if wf / hf > r { (hf * r, hf) } else { (wf, wf / r) };
    let (x0, y0) = ((wf - cw) / 2.0, (hf - ch) / 2.0);
    let mut out = vec![[0f32; 3]; tw * th];
    for ty in 0..th {
        let ya = y0 + ch * ty as f32 / th as f32;
        let yb = y0 + ch * (ty + 1) as f32 / th as f32;
        for tx in 0..tw {
            let xa = x0 + cw * tx as f32 / tw as f32;
            let xb = x0 + cw * (tx + 1) as f32 / tw as f32;
            let mut s = [0f32; 3];
            let mut n = 0f32;
            for y in (ya as usize)..(yb.ceil() as usize).min(h) {
                for x in (xa as usize)..(xb.ceil() as usize).min(w) {
                    let p = px[y * w + x];
                    s[0] += p[0];
                    s[1] += p[1];
                    s[2] += p[2];
                    n += 1.0;
                }
            }
            out[ty * tw + tx] = [s[0] / n.max(1.0), s[1] / n.max(1.0), s[2] / n.max(1.0)];
        }
    }
    out
}

fn lab(xyz: [f32; 3]) -> [f32; 3] {
    let w = color::xy_to_xyz(color::D65_XY);
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(xyz[0] / w[0]), f(xyz[1] / w[1]), f(xyz[2] / w[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 (Sharma, Wu, Dalal 2005).
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
    let hp = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            let h = b.atan2(a).to_degrees();
            if h < 0.0 {
                h + 360.0
            } else {
                h
            }
        }
    };
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

fn size(r: f32) -> (usize, usize) {
    if r >= 1.0 {
        (LONG_EDGE, (LONG_EDGE as f32 / r).round() as usize)
    } else {
        ((LONG_EDGE as f32 * r).round() as usize, LONG_EDGE)
    }
}

/// Rotate a reference image given in sensor orientation (e.g. the embedded camera preview, which the
/// camera does not rotate) by `quarters` quarter turns clockwise, like our output (`RawDoc::orientation`).
pub fn rotate_quarters(reference: &RgbImage, quarters: u8) -> RgbImage {
    use image::imageops::{rotate180, rotate270, rotate90};
    match quarters % 4 {
        1 => rotate90(reference),
        2 => rotate180(reference),
        3 => rotate270(reference),
        _ => reference.clone(),
    }
}

/// Decide whether an external reference still has to be turned: with 90°/270° the format of a reference in sensor
/// orientation (landscape) differs from that of our output (portrait). With 180° the format does not tell,
/// there `--ref-sensor` decides.
pub fn reference_needs_rotation(own: (u32, u32), reference: (u32, u32), quarters: u8) -> bool {
    quarters % 2 == 1 && (own.0 >= own.1) != (reference.0 >= reference.1)
}

/// Reference in the orientation of our output: `sensor` = given in sensor orientation (always turn);
/// otherwise only when the format shows that it was not turned yet. Returns the image and whether it was turned.
pub fn reference_oriented(reference: RgbImage, own: (u32, u32), quarters: u8, sensor: bool) -> (RgbImage, bool) {
    if quarters % 4 != 0 && (sensor || reference_needs_rotation(own, (reference.width(), reference.height()), quarters)) {
        (rotate_quarters(&reference, quarters), true)
    } else {
        (reference, false)
    }
}

pub fn compare_images(own: &RgbImage, reference: &RgbImage, ref_adobe: bool) -> Comparison {
    let ra = own.width() as f32 / own.height() as f32;
    let rb = reference.width() as f32 / reference.height() as f32;
    let r = if ra >= 1.0 { ra.min(rb) } else { ra.max(rb) };
    let (tw, th) = size(r);
    let a = downscale(&to_xyz(own, false), own.width() as usize, own.height() as usize, r, tw, th);
    let b = downscale(&to_xyz(reference, ref_adobe), reference.width() as usize, reference.height() as usize, r, tw, th);
    let to_srgb8 = {
        let m = color::inv(&color::srgb_to_xyz_d65()).expect("sRGB");
        move |x: [f32; 3]| -> [f32; 3] { color::mulv(&m, x).map(|v| raw::srgb_oetf(v.clamp(0.0, 1.0)) * 255.0) }
    };
    let n = a.len() as f64;
    let mut de = 0f64;
    let mut dz = [0f64; 3];
    let mut dlz = [0f64; 3];
    let mut nz = [0f64; 3];
    let (mut dl, mut da, mut db, mut dc, mut se) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let (mut ya, mut yb) = (0f64, 0f64);
    let (mut evm, mut nm) = (0f64, 0f64);
    for (pa, pb) in a.iter().zip(&b) {
        let la = lab(*pa);
        let lb = lab(*pb);
        let d = de2000(la, lb) as f64;
        de += d;
        let z = if lb[0] < 30.0 {
            0
        } else if lb[0] <= 70.0 {
            1
        } else {
            2
        };
        dz[z] += d;
        dlz[z] += (la[0] - lb[0]) as f64;
        nz[z] += 1.0;
        dl += (la[0] - lb[0]) as f64;
        da += (la[1] - lb[1]) as f64;
        db += (la[2] - lb[2]) as f64;
        dc += ((la[1].hypot(la[2])) - (lb[1].hypot(lb[2]))) as f64;
        ya += pa[1] as f64;
        yb += pb[1] as f64;
        if z == 1 && pa[1] > 1e-5 && pb[1] > 1e-5 {
            evm += (pa[1] / pb[1]).log2() as f64;
            nm += 1.0;
        }
        let (sa, sb) = (to_srgb8(*pa), to_srgb8(*pb));
        for c in 0..3 {
            let e = (sa[c] - sb[c]) as f64;
            se += e * e;
        }
    }
    let mse = se / (n * 3.0);
    Comparison {
        de: (de / n) as f32,
        de_zone: std::array::from_fn(|z| if nz[z] > 0.0 { (dz[z] / nz[z]) as f32 } else { f32::NAN }),
        fraction: std::array::from_fn(|z| (nz[z] / n) as f32),
        dl_zone: std::array::from_fn(|z| if nz[z] > 0.0 { (dlz[z] / nz[z]) as f32 } else { f32::NAN }),
        ev_mid: if nm > 0.0 { (evm / nm) as f32 } else { f32::NAN },
        ev: (ya / yb.max(1e-9)).log2() as f32,
        dl: (dl / n) as f32,
        da: (da / n) as f32,
        db: (db / n) as f32,
        dc: (dc / n) as f32,
        psnr: (10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()) as f32,
    }
}

/// Both images (reference converted to sRGB) side by side as JPEG, 800 px per image.
pub fn side_by_side(own: &RgbImage, reference: &RgbImage, ref_adobe: bool, path: &str) -> Result<()> {
    let ra = own.width() as f32 / own.height() as f32;
    let rb = reference.width() as f32 / reference.height() as f32;
    let r = if ra >= 1.0 { ra.min(rb) } else { ra.max(rb) };
    let (tw, th) = if r >= 1.0 { (800, (800.0 / r).round() as usize) } else { ((800.0 * r).round() as usize, 800) };
    let a = downscale(&to_xyz(own, false), own.width() as usize, own.height() as usize, r, tw, th);
    let b = downscale(&to_xyz(reference, ref_adobe), reference.width() as usize, reference.height() as usize, r, tw, th);
    let m = color::inv(&color::srgb_to_xyz_d65()).expect("sRGB");
    let mut out = RgbImage::new((2 * tw + 8) as u32, th as u32);
    for (k, src) in [a, b].iter().enumerate() {
        for y in 0..th {
            for x in 0..tw {
                let s = color::mulv(&m, src[y * tw + x]).map(|v| (raw::srgb_oetf(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8);
                out.put_pixel((x + k * (tw + 8)) as u32, y as u32, image::Rgb(s));
            }
        }
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut f, 88).encode_image(&out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::de2000;
    #[test]
    fn sharma_test_values() {
        // Sharma et al., table 1, pairs 1 and 7
        assert!((de2000([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485]) - 2.0425).abs() < 1e-3);
        assert!((de2000([50.0, 0.0, 0.0], [50.0, -1.0, 2.0]) - 2.3669).abs() < 1e-3);
    }
}

#[cfg(test)] mod film_lut_tests{
 use super::*;
 #[test] fn lut_gray_midpoint_delta_e00_matches_flat_pipeline(){
  let f=develop::film::FilmSettings{active:true,stock:"ilford_hp5_plus".into(),grain:0.,halation:0.,couplers:1.,auto_balance:false,..Default::default()};
  let cube=develop::film::cube_lut(&f,"test").unwrap();let rows:Vec<[f32;3]>=cube.lines().filter_map(|s|s.split_whitespace().map(str::parse).collect::<std::result::Result<Vec<f32>,_>>().ok()).filter_map(|p|p.try_into().ok()).collect();
  let dec=|v:f32|if v<=0.04045{v/12.92}else{((v+0.055)/1.055).powf(2.4)};
  let center=rows[16*33*33+16*33+16];let linear=dec(0.5);
  let scene=develop::Linear{ mono: false, haze: None, toneq: None,width:64,height:64,data:vec![linear;64*64*3],detail:None};let im=develop::film::develop(&scene,&f,develop::look::Geo::full(64,64)).unwrap();let at=(32*64+32)*3;let reference=[im.data[at],im.data[at+1],im.data[at+2]].map(|v|v as f32/65535.);
  let to_lab=|p:[f32;3]|lab(color::mulv(&color::srgb_to_xyz_d65(),p.map(dec)));
  let de=de2000(to_lab(center),to_lab(reference));eprintln!("LUT mid gray ΔE00={de:.6}");assert!(de<1.0);
 }
}

/// Lab pairs (ours, reference) on a coarse grid of the common crop (basis for tuning tone and color).
pub fn samples(own: &RgbImage, reference: &RgbImage, ref_adobe: bool) -> Vec<[f32; 6]> {
    let ra = own.width() as f32 / own.height() as f32;
    let rb = reference.width() as f32 / reference.height() as f32;
    let r = if ra >= 1.0 { ra.min(rb) } else { ra.max(rb) };
    let (tw, th) = if r >= 1.0 { (72, (72.0 / r).round() as usize) } else { ((72.0 * r).round() as usize, 72) };
    let a = downscale(&to_xyz(own, false), own.width() as usize, own.height() as usize, r, tw, th);
    let b = downscale(&to_xyz(reference, ref_adobe), reference.width() as usize, reference.height() as usize, r, tw, th);
    a.iter()
        .zip(&b)
        .map(|(pa, pb)| {
            let (la, lb) = (lab(*pa), lab(*pb));
            [la[0], la[1], la[2], lb[0], lb[1], lb[2]]
        })
        .collect()
}

#[cfg(test)]
mod orientation_tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, 90]))
    }

    #[test]
    fn rotated_reference_matches_output() {
        // our output (portrait, orientation 6 = 90° clockwise) vs reference in sensor orientation (landscape)
        let sensor = gradient(120, 80);
        let own = rotate_quarters(&sensor, 1);
        assert_eq!((own.width(), own.height()), (80, 120));
        // unturned reference: format differs and comparison is useless
        let bad = compare_images(&own, &sensor, false);
        let (r, turned) = reference_oriented(sensor.clone(), (own.width(), own.height()), 1, false);
        assert!(turned);
        let good = compare_images(&own, &r, false);
        assert!(good.de < 0.01, "{}", good.de);
        assert!(bad.de > good.de + 3.0, "{} vs {}", bad.de, good.de);
        // already turned reference (e.g. Lightroom export) stays
        let (r2, turned2) = reference_oriented(own.clone(), (80, 120), 1, false);
        assert!(!turned2 && r2.dimensions() == (80, 120));
    }

    #[test]
    fn rotation_180_needs_flag() {
        let sensor = gradient(120, 80);
        let own = rotate_quarters(&sensor, 2);
        assert!(!reference_oriented(sensor.clone(), (120, 80), 2, false).1);
        let (r, turned) = reference_oriented(sensor, (120, 80), 2, true);
        assert!(turned && compare_images(&own, &r, false).de < 0.01);
    }
}
