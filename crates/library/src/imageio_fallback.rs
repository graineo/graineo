//! Image I/O without macOS (Windows): JPEG, PNG and TIFF via the `image` crate, metadata (EXIF, GPS) from the
//! own header reader for all formats. HEIC/HEIF through the built-in pure-Rust decoder ([`crate::heif_lib`]).
//! Not available: HEIC/AVIF HDR gain maps – those calls then report "nothing" (None, empty list) or an error.

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
    // HEIF: the decoder returns upright pixels (applies irot/imir); size and orientation as the decoded pixels are
    if crate::heif_lib::is_heif(path) && crate::heif_lib::applies_orientation(path) {
        if let Some((w, h)) = crate::heif_lib::size(path) {
            (e.width, e.height) = (Some(w), Some(h));
        }
        e.orientation = Some(1);
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

/// HEIC/HEIF as extended linear sRGB (irot/imir applied, EXIF orientation not). `thumbnail`: the embedded
/// thumbnail when the file has one (callers check its size), else the whole image.
fn heif_linear(path: &Path, thumbnail: bool) -> Option<(usize, usize, Vec<[f32; 3]>)> {
    if thumbnail {
        if let Some(r) = crate::heif_lib::thumbnail(path) {
            return Some(r);
        }
    }
    match crate::heif_lib::linear(path) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("HEIF {}: {e}", path.display());
            None
        }
    }
}

/// Decoded image, not yet EXIF-rotated. HEIC/HEIF colour-managed to sRGB ([`heif_linear`]).
fn open(path: &Path, thumbnail: bool) -> Option<image::DynamicImage> {
    if crate::heif_lib::is_heif(path) {
        let (w, h, px) = heif_linear(path, thumbnail)?;
        let enc = |c: f32| {
            let c = c.clamp(0.0, 1.0);
            let e = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
            (e * 255.0 + 0.5) as u8
        };
        let buf: Vec<u8> = px.iter().flat_map(|p| p.map(enc)).collect();
        return Some(image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w as u32, h as u32, buf)?));
    }
    image::open(path).ok()
}

fn load_upright(path: &Path, thumbnail: bool) -> Option<image::DynamicImage> {
    let img = open(path, thumbnail)?;
    let o = properties(path).and_then(|e| e.orientation).unwrap_or(1);
    // properties() already reports 1 where the decoder turns the image itself (HEIF irot/imir)
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

/// Thumbnail with at most `max` pixels edge length, upright, RGB 8 bit. Without `always` a HEIF's embedded thumbnail
/// may come back smaller than `max` (as ImageIO's embedded thumbnail on the Mac; the preview cache then asks again
/// with `always`).
pub fn thumb(path: &Path, max: u32, always: bool) -> Option<(u32, u32, Vec<u8>)> {
    let img = load_upright(path, !always)?;
    // never enlarge (an embedded thumbnail smaller than `max` stays as it is)
    let img = if img.width().max(img.height()) > max { img.thumbnail(max, max) } else { img };
    let img = over_white(&img);
    Some((img.width(), img.height(), img.into_raw()))
}

/// Whole image (for the large view), optionally limited.
pub fn image(path: &Path, max: Option<u32>) -> Option<(u32, u32, Vec<u8>)> {
    let mut img = load_upright(path, false)?;
    if let Some(m) = max {
        if img.width() > m || img.height() > m {
            img = img.resize(m, m, image::imageops::FilterType::Triangle);
        }
    }
    let img = over_white(&img);
    Some((img.width(), img.height(), img.into_raw()))
}

/// Whole image as linear sRGB (32-bit float), without rotation (orientation in `orientation`).
/// Without color management for JPEG/PNG/TIFF: embedded profiles are read as sRGB. HEIC/HEIF is colour-managed
/// (ICC or nclx, e.g. Display P3 → extended linear sRGB, see [`heif_linear`]).
pub fn linear_full(path: &Path) -> Option<(usize, usize, Vec<[f32; 3]>, u32)> {
    let o = properties(path).and_then(|e| e.orientation).unwrap_or(1);
    if crate::heif_lib::is_heif(path) {
        let (w, h, px) = heif_linear(path, false)?;
        return Some((w, h, px, o));
    }
    let img = image::DynamicImage::ImageRgb8(over_white(&image::open(path).ok()?)).to_rgb32f();
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

/// Gain map of a JPEG read with the own reader (`gainmap`: ISO 21496-1 or Adobe XMP in an MPF container); HEIC/AVIF
/// maps need the system (macOS).
fn own_gain_map(path: &Path) -> Option<gainmap::Map> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg") {
        return None;
    }
    gainmap::read(&std::fs::read(path).ok()?)
}

pub fn hdr_info(path: &Path) -> Option<HdrInfo> {
    let m = own_gain_map(path)?;
    Some(HdrInfo { headroom: m.meta.alternate_headroom.exp2(), iso: m.iso }).filter(|e| e.headroom > 1.01)
}

pub fn gain_map(path: &Path) -> Option<GainMap> {
    let m = own_gain_map(path)?;
    // one value per pixel: the mean of the channels of a colour map
    let log2 = m.log2.chunks(m.channels).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    Some(GainMap { w: m.w, h: m.h, log2, base_headroom: m.meta.base_headroom, alternate_headroom: m.meta.alternate_headroom, base_offset: m.meta.offset_sdr[0], alternate_offset: m.meta.offset_hdr[0] })
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
