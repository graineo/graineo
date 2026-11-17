//! JPEG XL output with the pure-Rust encoder `jixel` (BSD-3-Clause OR Apache-2.0).
//!
//! - Lossy: VarDCT in XYB, quality 1–100 on the libjpeg-like scale of the encoder (90 = distance 1.0).
//! - Lossless: modular, the samples exactly as given, 8 or 16 bit.
//! - Colour: sRGB and Display P3 are signalled as JPEG XL colour encodings (every decoder derives the ICC profile
//!   from them); Adobe RGB and ProPhoto RGB have no such encoding and carry the embedded ICC profile of the app.
//! - Metadata (EXIF, XMP) are added afterwards as container boxes by the export path, like for JPEG/TIFF/PNG.

use std::path::Path;

use graineo_color::Space;

use crate::Rgb16;

/// Encoder effort: `Medium` is about twice as slow as `Fast` and gives clearly smaller lossless files
/// (measured 2026-11-17: 26 MP, 16-bit lossless 58 MB instead of 79 MB, 4.6 s instead of 0.9 s).
const SPEED: jixel::Speed = jixel::Speed::Medium;

/// Settings of a JPEG XL file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// 1–100 (lossy only)
    pub quality: u8,
    pub lossless: bool,
    /// 16 instead of 8 bit per sample (lossless only; lossy always gets the 16-bit samples)
    pub bits16: bool,
}

fn srgb_family(space: Space) -> Option<jixel::ColorEncoding> {
    match space {
        Space::Srgb => Some(jixel::ColorEncoding::srgb()),
        Space::DisplayP3 => Some(jixel::ColorEncoding::display_p3()),
        Space::AdobeRgb | Space::ProPhotoRgb => None,
    }
}

/// Rec. 2020 primaries with the sRGB transfer curve: the input encoding for lossy Adobe RGB / ProPhoto RGB
/// (the encoder converts from it into XYB; the file itself declares the ICC profile of the chosen space).
const REC2020_SRGB_CURVE: jixel::ColorEncoding = jixel::ColorEncoding {
    white_point: jixel::WhitePoint::D65,
    primaries: jixel::Primaries::Bt2020,
    transfer: jixel::TransferFunction::Srgb,
    rendering_intent: jixel::RenderingIntent::Relative,
};

/// Samples of `img` (encoded in `space`) re-encoded as Rec. 2020 with the sRGB curve. Adobe RGB lies completely
/// inside Rec. 2020; ProPhoto colours outside it are clipped to its edge.
fn to_rec2020(img: &Rgb16, space: Space) -> std::io::Result<Vec<u16>> {
    use rayon::prelude::*;
    let to_srgb = graineo_color::matrix(space, Space::Srgb).map_err(std::io::Error::other)?;
    let from_srgb = graineo_color::LinearSpace::Rec2020.from_srgb().map_err(std::io::Error::other)?;
    let m: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| from_srgb[i][k] * to_srgb[k][j]).sum()));
    let mut out = vec![0u16; img.data.len()];
    out.par_chunks_mut(3).zip(img.data.par_chunks(3)).for_each(|(o, p)| {
        let lin = [0, 1, 2].map(|c| space.decode(p[c] as f32 / 65535.0));
        let r = graineo_color::pixel(m, lin);
        for c in 0..3 {
            o[c] = (graineo_color::srgb_encode(r[c].clamp(0.0, 1.0)) * 65535.0).round() as u16;
        }
    });
    Ok(out)
}

/// Encodes `img` (samples in `space`) as a JPEG XL file in memory.
pub fn encode(img: &Rgb16, space: Space, p: Params) -> std::io::Result<Vec<u8>> {
    if img.width == 0 || img.height == 0 || img.data.len() != img.width * img.height * 3 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty or inconsistent image"));
    }
    let threads = rayon::current_num_threads().clamp(1, 4);
    let mut cfg = jixel::EncodeConfig::default().with_speed(SPEED).with_num_threads(threads);
    let enum_encoding = srgb_family(space);
    let icc = match enum_encoding {
        Some(_) => None,
        None => Some(space.icc().map_err(std::io::Error::other)?),
    };
    if let Some(icc) = icc {
        cfg = cfg.with_icc_profile(icc);
    }
    let (w, h) = (img.width, img.height);
    // A young encoder: a panic inside it must not take the export (or the app) down.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::io::Result<Vec<u8>> {
        let to_io = |e: jixel::EncodeError| std::io::Error::other(format!("JPEG XL: {e}"));
        if p.lossless {
            // the samples exactly as they are, in the declared space (enum encoding or ICC)
            let cfg = cfg.with_lossless(true).with_color_encoding(enum_encoding.unwrap_or(jixel::ColorEncoding::srgb()));
            if p.bits16 {
                jixel::encode_image_16bit(&img.data, w, h, &cfg).map_err(to_io)
            } else {
                jixel::encode_image(&img.to_rgb8(), w, h, &cfg).map_err(to_io)
            }
        } else {
            let cfg = cfg.with_quality(p.quality.clamp(1, 100) as f32);
            match enum_encoding {
                Some(e) => jixel::encode_image_16bit(&img.data, w, h, &cfg.with_color_encoding(e)).map_err(to_io),
                None => jixel::encode_image_16bit(&to_rec2020(img, space)?, w, h, &cfg.with_color_encoding(REC2020_SRGB_CURVE)).map_err(to_io),
            }
        }
    }));
    result.unwrap_or_else(|_| Err(std::io::Error::other("JPEG XL: encoder failed")))
}

/// Writes `img` (samples in `space`) as a JPEG XL file.
pub fn write(img: &Rgb16, path: &Path, space: Space, p: Params) -> std::io::Result<()> {
    let bytes = encode(img, space, p)?;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    std::io::Write::write_all(&mut file, &bytes)?;
    crate::export::finish_file(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth test image with saturated patches and a fine gradient (16 bit).
    fn test_image(w: usize, h: usize) -> Rgb16 {
        let data = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let g = (x * 65535 / (w - 1)) as u16;
                match (y * 4 / h, x * 4 / w) {
                    (0, 0) => [62000, 4000, 3000],
                    (0, 1) => [3000, 60000, 5000],
                    (0, 2) => [2500, 3500, 61000],
                    (1, _) => [g, g, g],
                    _ => [g, (y * 65535 / (h - 1)) as u16, ((x * 7 + y * 13) % 65536) as u16],
                }
            })
            .collect();
        Rgb16 { width: w, height: h, data }
    }

    struct Decoded {
        width: usize,
        height: usize,
        /// samples 0..1 in the colour space the file declares
        data: Vec<f32>,
        icc: Vec<u8>,
        original_icc: Option<Vec<u8>>,
        bits: u32,
        lossless: bool,
    }

    fn decode(bytes: &[u8]) -> Decoded {
        let mut img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(bytes)).expect("jxl-oxide reads the file");
        // XYB with an ICC profile needs colour management to render into the declared space
        img.set_cms(jxl_oxide::Moxcms);
        let header = img.image_header();
        let bits = header.metadata.bit_depth.bits_per_sample();
        let lossless = !header.metadata.xyb_encoded;
        let icc = img.rendered_icc();
        let original_icc = img.original_icc().map(|b| b.to_vec());
        let r = img.render_frame(0).expect("render");
        let fb = r.image_all_channels();
        assert_eq!(fb.channels(), 3);
        Decoded { width: fb.width(), height: fb.height(), data: fb.buf().to_vec(), icc, original_icc, bits, lossless }
    }

    #[test]
    fn lossless_round_trip_is_exact_in_every_space() {
        let img = test_image(96, 64);
        for space in Space::ALL {
            for bits16 in [true, false] {
                let bytes = encode(&img, space, Params { quality: 92, lossless: true, bits16 }).unwrap();
                let d = decode(&bytes);
                assert_eq!((d.width, d.height, d.lossless), (96, 64, true), "{space:?}");
                assert_eq!(d.bits, if bits16 { 16 } else { 8 }, "{space:?}");
                let max = if bits16 { 65535.0 } else { 255.0 };
                let want: Vec<u16> = if bits16 { img.data.clone() } else { img.to_rgb8().iter().map(|&v| v as u16).collect() };
                let got: Vec<u16> = d.data.iter().map(|v| (v * max).round() as u16).collect();
                assert_eq!(got, want, "{space:?} {bits16}");
                match space {
                    Space::AdobeRgb | Space::ProPhotoRgb => assert_eq!(d.original_icc.as_deref(), Some(&space.icc().unwrap()[..]), "{space:?}"),
                    _ => assert!(d.original_icc.is_none() && !d.icc.is_empty(), "{space:?}: colour encoding instead of ICC"),
                }
            }
        }
    }

    #[test]
    fn lossy_is_close_and_smaller_at_lower_quality() {
        let img = test_image(256, 192);
        let mut sizes = Vec::new();
        for q in [70u8, 92, 98] {
            let bytes = encode(&img, Space::Srgb, Params { quality: q, lossless: false, bits16: true }).unwrap();
            let d = decode(&bytes);
            assert!(!d.lossless);
            let err = img.data.iter().zip(&d.data).map(|(&a, &b)| (a as f32 / 65535.0 - b).abs()).fold(0f32, f32::max);
            let mean = img.data.iter().zip(&d.data).map(|(&a, &b)| (a as f32 / 65535.0 - b).abs() as f64).sum::<f64>() / img.data.len() as f64;
            assert!(mean < if q >= 92 { 0.006 } else { 0.02 }, "q{q}: mean error {mean}, max {err}");
            sizes.push(bytes.len());
        }
        assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2], "{sizes:?}");
    }

    /// Lossy in the wide spaces: the colours arrive in the XYB data unchanged. Compared in linear sRGB without clipping
    /// (jxl-oxide clips to sRGB when it renders into another space; djxl and ImageIO render ICC-tagged XYB as sRGB).
    #[test]
    fn lossy_wide_spaces_keep_their_colours() {
        // moderately saturated colours, inside Rec. 2020 also in ProPhoto RGB
        let (w, h) = (128, 96);
        let data: Vec<u16> = (0..w * h).flat_map(|i| [[50000u16, 16000, 15000], [15000, 46000, 18000], [14000, 17000, 47000], [40000, 40000, 40000]][(i % w) * 4 / w]).collect();
        let img = Rgb16 { width: w, height: h, data };
        for space in [Space::Srgb, Space::DisplayP3, Space::AdobeRgb, Space::ProPhotoRgb] {
            let bytes = encode(&img, space, Params { quality: 95, lossless: false, bits16: true }).unwrap();
            let mut j = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(&bytes)).unwrap();
            assert_eq!(j.original_icc().map(|b| b.to_vec()), srgb_family(space).is_none().then(|| space.icc().unwrap()), "{space:?}");
            j.set_cms(jxl_oxide::Moxcms);
            j.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb_linear(jxl_oxide::RenderingIntent::Relative));
            let r = j.render_frame(0).unwrap();
            let fb = r.image_all_channels();
            let m = graineo_color::matrix(space, Space::Srgb).unwrap();
            let mut sum = 0f64;
            for (p, got) in img.data.chunks(3).zip(fb.buf().chunks(3)) {
                let want = graineo_color::pixel(m, [0, 1, 2].map(|c| space.decode(p[c] as f32 / 65535.0)));
                sum += (0..3).map(|c| (want[c] - got[c]).abs() as f64).sum::<f64>() / 3.0;
            }
            let mean = sum / (w * h) as f64;
            assert!(mean < 0.004, "{space:?}: mean error {mean} (linear)");
        }
    }

    #[test]
    fn rejects_empty_images() {
        for (w, h) in [(0, 4), (4, 0)] {
            let e = encode(&Rgb16 { width: w, height: h, data: Vec::new() }, Space::Srgb, Params { quality: 90, lossless: false, bits16: true }).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}
