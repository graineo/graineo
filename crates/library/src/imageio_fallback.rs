//! Image I/O without macOS (Windows): JPEG, PNG and TIFF via the `image` crate, metadata (EXIF, GPS) from the
//! own header reader for all formats. Not available: decoding HEIC, HDR gain maps – those
//! calls then report "nothing" (None, empty list) or an error.

use std::path::Path;

pub use crate::imageio_common::*;

/// Properties (dimensions, orientation, capture date, camera, shooting data) without decoding the image: the
/// metadata from the own header reader ([`crate::exif_header`], all containers it knows, RAWs included), the
/// size from the decoder's header where it can read the format (as ImageIO reports it), else from the header reader.
pub fn properties(path: &Path) -> Option<Properties> {
    let header = crate::exif_header::properties(path);
    let size = image::image_dimensions(path).ok();
    if header.is_none() && size.is_none() {
        return None;
    }
    let mut e = header.unwrap_or_default();
    if let Some((w, h)) = size {
        (e.width, e.height) = (Some(w), Some(h));
    }
    Some(e)
}

/// GPS position from the EXIF GPS block (north and east positive); `None` without a usable position.
pub fn gps_position(path: &Path) -> Option<(f64, f64)> {
    crate::exif_header::gps_position(path)
}

/// Apply EXIF orientation (1–8).
fn upright(img: image::DynamicImage, o: u32) -> image::DynamicImage {
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

fn load_upright(path: &Path) -> Option<image::DynamicImage> {
    let img = image::open(path).ok()?;
    let o = properties(path).and_then(|e| e.orientation).unwrap_or(1);
    Some(upright(img, o))
}

/// RGB 8 bit with transparent parts over white (as ImageIO on the Mac does in graineo: dropping alpha would show the
/// arbitrary colour under fully transparent pixels).
fn over_white(img: &image::DynamicImage) -> image::RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let a = img.to_rgba8();
    image::RgbImage::from_fn(a.width(), a.height(), |x, y| {
        let p = a.get_pixel(x, y).0;
        let al = p[3] as u32;
        let mix = |c: u8| ((c as u32 * al + 255 * (255 - al) + 127) / 255) as u8;
        image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

/// Thumbnail with at most `max` pixels edge length, upright, RGB 8 bit.
pub fn thumb(path: &Path, max: u32, _always: bool) -> Option<(u32, u32, Vec<u8>)> {
    let img = over_white(&load_upright(path)?.thumbnail(max, max));
    Some((img.width(), img.height(), img.into_raw()))
}

/// Whole image (for the large view), optionally limited.
pub fn image(path: &Path, max: Option<u32>) -> Option<(u32, u32, Vec<u8>)> {
    let mut img = load_upright(path)?;
    if let Some(m) = max {
        if img.width() > m || img.height() > m {
            img = img.resize(m, m, image::imageops::FilterType::Triangle);
        }
    }
    let img = over_white(&img);
    Some((img.width(), img.height(), img.into_raw()))
}

/// Whole image as linear sRGB (32-bit float), without rotation (orientation in `orientation`).
/// Without color management: embedded profiles (e.g. Display P3) are read as sRGB.
pub fn linear_full(path: &Path) -> Option<(usize, usize, Vec<[f32; 3]>, u32)> {
    let img = image::DynamicImage::ImageRgb8(over_white(&image::open(path).ok()?)).to_rgb32f();
    let o = properties(path).and_then(|e| e.orientation).unwrap_or(1);
    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px = img.pixels().map(|p| [lin(p[0]), lin(p[1]), lin(p[2])]).collect();
    Some((w, h, px, o))
}

/// Write metadata into an exported file: without ImageIO with the own writer (shooting data from
/// `fields.shot`; the original itself is not read here, see [`crate::export_meta::write`]).
pub fn write_metadata(target: &Path, _original: &Path, width: u32, height: u32, fields: &ExportFields) -> Result<String, String> {
    crate::export_meta::write_own(target, &crate::export_meta::OwnFields { fields: fields.clone(), width, height })
}

pub fn read_metadata(_p: &Path, _path: &str) -> Option<String> {
    None
}

pub fn hdr_info(_path: &Path) -> Option<HdrInfo> {
    None
}

pub fn gain_map(_path: &Path) -> Option<GainMap> {
    None
}

pub fn hdr_linear(_path: &Path, _w: usize, _h: usize) -> Option<Vec<[f32; 3]>> {
    None
}

pub fn append_gain_map(_target: &Path, _original: &Path, _mapping: Option<&Mapping>, _quality: f32) -> Result<bool, String> {
    Ok(false)
}

pub fn text_fields(_path: &Path) -> Vec<String> {
    Vec::new()
}
