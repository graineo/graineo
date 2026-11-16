//! Thumbnails via macOS ImageIO (JPEG, HEIC, TIFF, PNG, PSD …).
//!
//! Why ImageIO instead of a shell call (`sips`) or an own HEIC library:
//! - macOS decodes HEIC/HEIF anyway (hardware decoder); a Rust HEIC library would need
//!   libheif + libde265 (C, LGPL) – extra code and license maintenance.
//! - `CGImageSourceCreateThumbnailAtIndex` decodes JPEGs downscaled (DCT scaling) and
//!   applies the EXIF rotation – a 24 MP JPEG thus takes only a few milliseconds.
//! - Color management: the result is drawn into an sRGB bitmap context; Display P3 images
//!   (iPhone) are converted correctly in the process.
//! - No process start per image (`sips` would need ~30 ms just for starting plus an intermediate file).
//!
//! Read-only: `CGImageSourceCreateWithURL` opens the file without write access.

#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::path::Path;

pub use crate::imageio_common::*;

type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: f64,
    h: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    o: CGPoint,
    s: CGSize,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(alloc: CFTypeRef, buf: *const u8, len: isize, is_dir: u8) -> CFTypeRef;
    fn CFDictionaryCreate(alloc: CFTypeRef, keys: *const CFTypeRef, values: *const CFTypeRef, n: isize, kcb: *const c_void, vcb: *const c_void) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberCreate(alloc: CFTypeRef, the_type: isize, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, the_type: isize, value: *mut c_void) -> u8;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut u8, size: isize, enc: u32) -> u8;
    fn CFGetTypeID(p: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFRelease(p: CFTypeRef);
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFArrayGetTypeID() -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFDictionaryGetCount(d: CFTypeRef) -> isize;
    fn CFDictionaryGetKeysAndValues(d: CFTypeRef, keys: *mut CFTypeRef, values: *mut CFTypeRef);
    fn CFStringGetLength(s: CFTypeRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, enc: u32) -> isize;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    static kCFBooleanTrue: CFTypeRef;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageSourceCreateWithURL(url: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateImageAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCopyPropertiesAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceCreateThumbnailFromImageIfAbsent: CFTypeRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImageSourceShouldCache: CFTypeRef;
    static kCGImagePropertyPixelWidth: CFTypeRef;
    static kCGImagePropertyPixelHeight: CFTypeRef;
    static kCGImagePropertyOrientation: CFTypeRef;
    static kCGImagePropertyExifDictionary: CFTypeRef;
    static kCGImagePropertyExifDateTimeOriginal: CFTypeRef;
    static kCGImagePropertyTIFFDictionary: CFTypeRef;
    static kCGImagePropertyTIFFModel: CFTypeRef;
    static kCGImagePropertyTIFFCopyright: CFTypeRef;
    static kCGImagePropertyTIFFMake: CFTypeRef;
    static kCGImagePropertyTIFFDateTime: CFTypeRef;
    static kCGImagePropertyExifFNumber: CFTypeRef;
    static kCGImagePropertyExifFocalLength: CFTypeRef;
    static kCGImagePropertyExifISOSpeedRatings: CFTypeRef;
    static kCGImagePropertyExifExposureTime: CFTypeRef;
    static kCGImagePropertyExifLensModel: CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetWidth(img: CFTypeRef) -> usize;
    fn CGImageGetHeight(img: CFTypeRef) -> usize;
    fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef;
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, cs: CFTypeRef, info: u32) -> CFTypeRef;
    fn CGContextDrawImage(ctx: CFTypeRef, rect: CGRect, img: CFTypeRef);
    fn CGContextSetInterpolationQuality(ctx: CFTypeRef, q: i32);
    static kCGColorSpaceSRGB: CFTypeRef;
    static kCGColorSpaceExtendedLinearSRGB: CFTypeRef;
}

const K_CF_NUMBER_SINT32: isize = 3;
const K_CF_NUMBER_SINT64: isize = 4;
const K_CF_STRING_UTF8: u32 = 0x0800_0100;
/// kCGImageAlphaNoneSkipLast | kCGBitmapByteOrder32Big → R, G, B, X
const BITMAP_RGBX: u32 = 5 | (4 << 12);

/// Holds a CF object and releases it at the end.
struct Cf(CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}


fn source(path: &Path) -> Option<Cf> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let url = Cf(unsafe { CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 0) });
    if url.0.is_null() {
        return None;
    }
    // no caching of the whole image in ImageIO
    let opts = dict(&[(unsafe { kCGImageSourceShouldCache }, false_ref())]);
    let src = Cf(unsafe { CGImageSourceCreateWithURL(url.0, opts.0) });
    (!src.0.is_null()).then_some(src)
}

fn false_ref() -> CFTypeRef {
    extern "C" {
        static kCFBooleanFalse: CFTypeRef;
    }
    unsafe { kCFBooleanFalse }
}

fn dict(pairs: &[(CFTypeRef, CFTypeRef)]) -> Cf {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|p| p.0).collect();
    let vals: Vec<CFTypeRef> = pairs.iter().map(|p| p.1).collect();
    Cf(unsafe {
        CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), vals.as_ptr(), pairs.len() as isize, &kCFTypeDictionaryKeyCallBacks as *const c_void, &kCFTypeDictionaryValueCallBacks as *const c_void)
    })
}

fn number(d: CFTypeRef, key: CFTypeRef) -> Option<i64> {
    let v = unsafe { CFDictionaryGetValue(d, key) };
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out: i64 = 0;
    (unsafe { CFNumberGetValue(v, K_CF_NUMBER_SINT64, &mut out as *mut i64 as *mut c_void) } != 0).then_some(out)
}

fn float_value(v: CFTypeRef) -> Option<f64> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut out: f64 = 0.0;
    (unsafe { CFNumberGetValue(v, 6, &mut out as *mut f64 as *mut c_void) } != 0).then_some(out).filter(|x| x.is_finite() && *x > 0.0)
}

fn float_of(d: CFTypeRef, key: CFTypeRef) -> Option<f64> {
    let v = unsafe { CFDictionaryGetValue(d, key) };
    if !v.is_null() && unsafe { CFGetTypeID(v) == CFArrayGetTypeID() } {
        if unsafe { CFArrayGetCount(v) } > 0 {
            return float_value(unsafe { CFArrayGetValueAtIndex(v, 0) });
        }
        return None;
    }
    float_value(v)
}

fn text(d: CFTypeRef, key: CFTypeRef) -> Option<String> {
    let v = unsafe { CFDictionaryGetValue(d, key) };
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = vec![0u8; 512];
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_UTF8) } == 0 {
        return None;
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..n]).trim().to_string())
}


/// Properties (dimensions, orientation, capture date, camera) without decoding the image.
pub fn properties(path: &Path) -> Option<Properties> {
    let src = source(path)?;
    let props = Cf(unsafe { CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()) });
    if props.0.is_null() {
        return None;
    }
    let p = props.0;
    let mut e = Properties {
        width: number(p, unsafe { kCGImagePropertyPixelWidth }).map(|v| v as u32),
        height: number(p, unsafe { kCGImagePropertyPixelHeight }).map(|v| v as u32),
        orientation: number(p, unsafe { kCGImagePropertyOrientation }).map(|v| v as u32),
        ..Default::default()
    };
    let exif = unsafe { CFDictionaryGetValue(p, kCGImagePropertyExifDictionary) };
    if !exif.is_null() {
        e.capture = text(exif, unsafe { kCGImagePropertyExifDateTimeOriginal }).and_then(|t| exif_date(&t));
        e.aperture = float_of(exif, unsafe { kCGImagePropertyExifFNumber });
        e.focal = float_of(exif, unsafe { kCGImagePropertyExifFocalLength });
        e.iso = float_of(exif, unsafe { kCGImagePropertyExifISOSpeedRatings }).map(|v| v.round() as i64);
        e.time = float_of(exif, unsafe { kCGImagePropertyExifExposureTime });
        e.lens = text(exif, unsafe { kCGImagePropertyExifLensModel }).filter(|t| !t.is_empty());
    }
    let tiff = unsafe { CFDictionaryGetValue(p, kCGImagePropertyTIFFDictionary) };
    if !tiff.is_null() {
        e.camera = text(tiff, unsafe { kCGImagePropertyTIFFModel }).filter(|t| !t.is_empty());
        e.maker = text(tiff, unsafe { kCGImagePropertyTIFFMake }).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        if e.capture.is_none() {
            e.capture = text(tiff, unsafe { kCGImagePropertyTIFFDateTime }).and_then(|t| exif_date(&t));
        }
    }
    Some(e)
}

/// Thumbnail with at most `max` pixels edge length, upright (EXIF rotation applied),
/// in sRGB. `always`: compute from the whole image instead of taking an embedded thumbnail.
/// Returns: width, height, RGB (8 bit).
pub fn thumb(path: &Path, max: u32, always: bool) -> Option<(u32, u32, Vec<u8>)> {
    let src = source(path)?;
    let max_n = max as i32;
    let num = Cf(unsafe { CFNumberCreate(std::ptr::null(), K_CF_NUMBER_SINT32, &max_n as *const i32 as *const c_void) });
    let kind = if always { unsafe { kCGImageSourceCreateThumbnailFromImageAlways } } else { unsafe { kCGImageSourceCreateThumbnailFromImageIfAbsent } };
    let opts = dict(&[(kind, unsafe { kCFBooleanTrue }), (unsafe { kCGImageSourceCreateThumbnailWithTransform }, unsafe { kCFBooleanTrue }), (unsafe { kCGImageSourceThumbnailMaxPixelSize }, num.0)]);
    let img = Cf(unsafe { CGImageSourceCreateThumbnailAtIndex(src.0, 0, opts.0) });
    if img.0.is_null() {
        return None;
    }
    draw(img.0)
}

/// Whole image (for the large view), optionally limited.
pub fn image(path: &Path, max: Option<u32>) -> Option<(u32, u32, Vec<u8>)> {
    match max {
        Some(m) => thumb(path, m, true),
        None => {
            let e = properties(path)?;
            let edge = e.width?.max(e.height?);
            thumb(path, edge, true)
        }
    }
}

/// Largest image this module draws into its own buffers (16 bytes per pixel in the float paths): larger images
/// come from damaged or hostile files and would need tens of gigabytes.
const MAX_PIXELS: usize = 400_000_000;

/// `Some(w·h)` when the size is usable (both ≥ 1, product within [`MAX_PIXELS`]).
fn pixel_count(w: usize, h: usize) -> Option<usize> {
    if w == 0 || h == 0 {
        return None;
    }
    w.checked_mul(h).filter(|n| *n <= MAX_PIXELS)
}

fn draw(img: CFTypeRef) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = unsafe { (CGImageGetWidth(img), CGImageGetHeight(img)) };
    let n = pixel_count(w, h)?;
    // white underneath: an image with transparency (PNG, TIFF, HEIC with alpha) is composited over it. The buffer was
    // black before 0.108 – transparent graphics, logos and screenshots looked too dark or black. White, not "alpha
    // ignored": the colour under fully transparent pixels is arbitrary (often black or garbage), and white is how
    // documents, the web and most viewers show such pictures; photos have no alpha and stay unchanged.
    let mut buf = vec![255u8; n * 4];
    let cs = Cf(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) });
    let ctx = Cf(unsafe { CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, cs.0, BITMAP_RGBX) });
    if ctx.0.is_null() {
        return None;
    }
    unsafe {
        CGContextSetInterpolationQuality(ctx.0, 3);
        CGContextDrawImage(ctx.0, CGRect { o: CGPoint { x: 0.0, y: 0.0 }, s: CGSize { w: w as f64, h: h as f64 } }, img);
    }
    drop(ctx);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in buf.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    Some((w as u32, h as u32, rgb))
}

/// Whole image in full resolution as **linear** RGB (extended linear sRGB, 32-bit
/// float per channel): ImageIO decodes (JPEG, HEIC, TIFF also 16 bit, PNG), CoreGraphics
/// converts the embedded color profile (e.g. Display P3 from the iPhone) – colors outside
/// sRGB are kept as values < 0 or > 1. Without EXIF rotation (orientation in `orientation`).
/// HDR gain maps (iPhone) are not applied: the SDR image is returned.
pub fn linear_full(path: &Path) -> Option<(usize, usize, Vec<[f32; 3]>, u32)> {
    let src = source(path)?;
    let img = Cf(unsafe { CGImageSourceCreateImageAtIndex(src.0, 0, std::ptr::null()) });
    if img.0.is_null() {
        return None;
    }
    let (w, h) = unsafe { (CGImageGetWidth(img.0), CGImageGetHeight(img.0)) };
    let n = pixel_count(w, h)?;
    let orientation = properties(path).and_then(|e| e.orientation).unwrap_or(1);
    // white underneath (linear 1.0), as in [`draw`]: transparent parts over white instead of black
    let mut buf = vec![1f32; n * 4];
    let cs = Cf(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB) });
    // kCGImageAlphaNoneSkipLast | kCGBitmapFloatComponents | kCGBitmapByteOrder32Little
    let info: u32 = 5 | (1 << 8) | (2 << 12);
    let ctx = Cf(unsafe { CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 32, w * 16, cs.0, info) });
    if ctx.0.is_null() {
        return None;
    }
    unsafe {
        CGContextDrawImage(ctx.0, CGRect { o: CGPoint { x: 0.0, y: 0.0 }, s: CGSize { w: w as f64, h: h as f64 } }, img.0);
    }
    drop(ctx);
    let px = buf.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
    Some((w, h, px, orientation))
}

// ─────────────────────────────────────────────────────────── Metadata on export

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageSourceCopyMetadataAtIndex(src: CFTypeRef, index: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceGetType(src: CFTypeRef) -> CFTypeRef;
    fn CGImageMetadataCreateMutable() -> CFTypeRef;
    fn CGImageMetadataCreateMutableCopy(m: CFTypeRef) -> CFTypeRef;
    fn CGImageMetadataSetValueWithPath(m: CFTypeRef, parent: CFTypeRef, path: CFTypeRef, value: CFTypeRef) -> u8;
    fn CGImageMetadataRegisterNamespaceForPrefix(m: CFTypeRef, xmlns: CFTypeRef, prefix: CFTypeRef, err: *mut CFTypeRef) -> u8;
    fn CGImageMetadataSetValueMatchingImageProperty(m: CFTypeRef, dict: CFTypeRef, prop: CFTypeRef, value: CFTypeRef) -> u8;
    fn CGImageMetadataCopyStringValueWithPath(m: CFTypeRef, parent: CFTypeRef, path: CFTypeRef) -> CFTypeRef;
    fn CGImageDestinationCreateWithURL(url: CFTypeRef, type_id: CFTypeRef, count: usize, options: CFTypeRef) -> CFTypeRef;
    fn CGImageDestinationCopyImageSource(dest: CFTypeRef, src: CFTypeRef, options: CFTypeRef, err: *mut CFTypeRef) -> u8;
    static kCGImageDestinationMetadata: CFTypeRef;
    static kCGImageDestinationMergeMetadata: CFTypeRef;
    static kCGImagePropertyExifPixelXDimension: CFTypeRef;
    static kCGImagePropertyExifPixelYDimension: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(alloc: CFTypeRef, bytes: *const u8, n: isize, enc: u32, ext: u8) -> CFTypeRef;
    fn CFArrayCreate(alloc: CFTypeRef, values: *const CFTypeRef, n: isize, cb: *const c_void) -> CFTypeRef;
    static kCFTypeArrayCallBacks: c_void;
}

fn cf_text(s: &str) -> Cf {
    Cf(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, K_CF_STRING_UTF8, 0) })
}

fn cf_number(v: i64) -> Cf {
    Cf(unsafe { CFNumberCreate(std::ptr::null(), K_CF_NUMBER_SINT64, &v as *const i64 as *const c_void) })
}

fn cf_url(p: &Path) -> Cf {
    let b = p.as_os_str().as_encoded_bytes();
    Cf(unsafe { CFURLCreateFromFileSystemRepresentation(std::ptr::null(), b.as_ptr(), b.len() as isize, 0) })
}


#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageMetadataCopyTags(m: CFTypeRef) -> CFTypeRef;
    fn CGImageMetadataTagCopyPrefix(tag: CFTypeRef) -> CFTypeRef;
    fn CGImageMetadataTagCopyName(tag: CFTypeRef) -> CFTypeRef;
    fn CGImageMetadataSetTagWithPath(m: CFTypeRef, parent: CFTypeRef, path: CFTypeRef, tag: CFTypeRef) -> u8;
    fn CGImageMetadataCopyTagWithPath(m: CFTypeRef, parent: CFTypeRef, path: CFTypeRef) -> CFTypeRef;
    static kCGImagePropertyTIFFSoftware: CFTypeRef;
}

/// Shooting data of a file as ImageIO reads them (image properties; RAW, HEIC, JPEG …; not X3F).
pub fn original_shot(path: &Path) -> Option<crate::export_meta::Shot> {
    let src = source(path)?;
    let props = Cf(unsafe { CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()) });
    if props.0.is_null() {
        return None;
    }
    let sub = |name: &str| {
        let v = unsafe { CFDictionaryGetValue(props.0, cf_text(name).0) };
        (!v.is_null() && unsafe { CFGetTypeID(v) == CFDictionaryGetTypeID() }).then_some(v)
    };
    let txt = |d: Option<CFTypeRef>, k: &str| d.and_then(|d| text(d, cf_text(k).0)).filter(|t| !t.is_empty());
    let num = |d: Option<CFTypeRef>, k: &str| d.and_then(|d| float_of(d, cf_text(k).0));
    let signed = |d: Option<CFTypeRef>, k: &str| {
        let v = unsafe { CFDictionaryGetValue(d?, cf_text(k).0) };
        if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
            return None;
        }
        let mut out = 0f64;
        (unsafe { CFNumberGetValue(v, 6, &mut out as *mut f64 as *mut c_void) } != 0).then_some(out).filter(|x| x.is_finite())
    };
    let (tiff, exif, aux) = (sub("{TIFF}"), sub("{Exif}"), sub("{ExifAux}"));
    let shot = crate::export_meta::Shot {
        make: txt(tiff, "Make"),
        model: txt(tiff, "Model"),
        lens_make: txt(exif, "LensMake"),
        lens: txt(exif, "LensModel").or_else(|| txt(aux, "LensModel")),
        date: txt(exif, "DateTimeOriginal").as_deref().and_then(crate::export_meta::normalize_date),
        exposure_time: num(exif, "ExposureTime"),
        f_number: num(exif, "FNumber"),
        iso: num(exif, "ISOSpeedRatings").map(|v| v.round() as u32),
        focal_length: num(exif, "FocalLength"),
        focal_length_35mm: num(exif, "FocalLenIn35mmFilm").map(|v| v.round() as u32),
        exposure_bias: signed(exif, "ExposureBiasValue"),
        exposure_program: num(exif, "ExposureProgram").map(|v| v as u16),
    };
    (!shot.is_empty()).then_some(shot)
}

/// Write metadata into a finished exported JPEG/TIFF/PNG **without recompressing**
/// (`CGImageDestinationCopyImageSource`). From the original (as far as ImageIO reads it – CR2/CR3/RAF/DNG/
/// ARW/NEF/HEIC/JPEG yes, X3F no) only the shooting data allowed by [`crate::export_meta::allowed_tag`]
/// (camera, lens, exposure, date; GPS only with `fields.gps`), never serial numbers or owner names;
/// `fields.shot` fills what is missing. Plus orientation = 1 (the image is already upright), pixel
/// dimensions, Software, copyright and – with `fields.personal` – stars, label, keywords, title,
/// description. The encoder already writes the ICC profile. Returns: short report.
pub fn write_metadata(target: &Path, original: &Path, width: u32, height: u32, fields: &ExportFields) -> Result<String, String> {
    let source_new = source(target).ok_or("export file not readable")?;
    let type_id = unsafe { CGImageSourceGetType(source_new.0) };
    if type_id.is_null() {
        return Err("unknown file type".into());
    }
    let m = Cf(unsafe { CGImageMetadataCreateMutable() });
    if m.0.is_null() {
        return Err("metadata could not be created".into());
    }
    let set_md = |path: &str, v: CFTypeRef| unsafe { CGImageMetadataSetValueWithPath(m.0, std::ptr::null(), cf_text(path).0, v) != 0 };
    // allowed tags of the original
    let mut taken = 0;
    if let Some(src) = source(original) {
        let orig = Cf(unsafe { CGImageSourceCopyMetadataAtIndex(src.0, 0, std::ptr::null()) });
        if !orig.0.is_null() {
            let tags = Cf(unsafe { CGImageMetadataCopyTags(orig.0) });
            if !tags.0.is_null() {
                for i in 0..unsafe { CFArrayGetCount(tags.0) } {
                    let tag = unsafe { CFArrayGetValueAtIndex(tags.0, i) };
                    let prefix = Cf(unsafe { CGImageMetadataTagCopyPrefix(tag) });
                    let name = Cf(unsafe { CGImageMetadataTagCopyName(tag) });
                    let (Some(prefix), Some(name)) = (cf_string(prefix.0), cf_string(name.0)) else { continue };
                    let path = format!("{prefix}:{name}");
                    if crate::export_meta::allowed_tag(&path, fields.gps) && unsafe { CGImageMetadataSetTagWithPath(m.0, std::ptr::null(), cf_text(&path).0, tag) } != 0 {
                        taken += 1;
                    }
                }
            }
        }
    }
    let mut written = vec![if taken > 0 { format!("{taken} fields of the original") } else { "no metadata in the original".to_string() }];
    // shooting data: the original's image properties, the app's own (X3F, RAW decoder) where it says nothing
    let shot = original_shot(original).unwrap_or_default().or(fields.shot.clone());
    if !shot.is_empty() {
        let mut added = 0;
        for (path, value) in shot.xmp_pairs() {
            // dates always as text: a date tag copied from the original does not reach the EXIF block of a TIFF
            let date = path.starts_with("exif:DateTime");
            let present = Cf(unsafe { CGImageMetadataCopyTagWithPath(m.0, std::ptr::null(), cf_text(path).0) });
            if (date || present.0.is_null()) && set_md(path, cf_text(&value).0) {
                added += 1;
            }
        }
        // date, 35 mm focal length and lens reach the EXIF block only this way (copied XMP tags stay XMP in JPEG)
        let exif_dict = cf_text("{Exif}");
        for (key, value) in shot.exif_properties() {
            let key = cf_text(key);
            let v = match value {
                crate::export_meta::Prop::Text(t) => cf_text(&t),
                crate::export_meta::Prop::Number(n) => Cf(unsafe { CFNumberCreate(std::ptr::null(), 13 /* kCFNumberDoubleType */, &n as *const f64 as *const c_void) }),
            };
            if unsafe { CGImageMetadataSetValueMatchingImageProperty(m.0, exif_dict.0, key.0, v.0) } != 0 {
                added += 1;
            }
        }
        if added > 0 {
            written.push(format!("{added} fields from graineo"));
        }
    }
    let one = cf_number(1);
    let software = cf_text(&crate::export_meta::software());
    unsafe {
        CGImageMetadataSetValueMatchingImageProperty(m.0, kCGImagePropertyTIFFDictionary, kCGImagePropertyOrientation, one.0);
        CGImageMetadataSetValueMatchingImageProperty(m.0, kCGImagePropertyTIFFDictionary, kCGImagePropertyTIFFSoftware, software.0);
        let (bw, bh) = (cf_number(width as i64), cf_number(height as i64));
        CGImageMetadataSetValueMatchingImageProperty(m.0, kCGImagePropertyExifDictionary, kCGImagePropertyExifPixelXDimension, bw.0);
        CGImageMetadataSetValueMatchingImageProperty(m.0, kCGImagePropertyExifDictionary, kCGImagePropertyExifPixelYDimension, bh.0);
    }
    set_md("tiff:Orientation", cf_text("1").0);
    set_md("xmp:CreatorTool", software.0);
    if fields.personal {
        set_md("xmp:Rating", cf_number(fields.stars as i64).0);
        written.push(format!("stars {}", fields.stars));
        if !fields.label.is_empty() {
            set_md("xmp:Label", cf_text(&fields.label).0);
            written.push("label".into());
        }
        if !fields.keywords.is_empty() {
            let values: Vec<Cf> = fields.keywords.iter().map(|s| cf_text(s)).collect();
            let raw_refs: Vec<CFTypeRef> = values.iter().map(|c| c.0).collect();
            let arr = Cf(unsafe { CFArrayCreate(std::ptr::null(), raw_refs.as_ptr(), raw_refs.len() as isize, &kCFTypeArrayCallBacks as *const c_void) });
            set_md("dc:subject", arr.0);
            written.push(format!("{} keywords", fields.keywords.len()));
        }
        if !fields.hierarchy.is_empty() {
            // the Lightroom namespace is not known to ImageIO: registered first
            let registered = unsafe { CGImageMetadataRegisterNamespaceForPrefix(m.0, cf_text("http://ns.adobe.com/lightroom/1.0/").0, cf_text("lr").0, std::ptr::null_mut()) != 0 };
            let values: Vec<Cf> = fields.hierarchy.iter().map(|s| cf_text(s)).collect();
            let raw_refs: Vec<CFTypeRef> = values.iter().map(|c| c.0).collect();
            let arr = Cf(unsafe { CFArrayCreate(std::ptr::null(), raw_refs.as_ptr(), raw_refs.len() as isize, &kCFTypeArrayCallBacks as *const c_void) });
            if registered && set_md("lr:hierarchicalSubject", arr.0) {
                written.push("keyword hierarchy".into());
            }
        }
        if let Some(t) = &fields.title {
            set_md("dc:title", cf_text(t).0);
        }
        if let Some(t) = &fields.description {
            set_md("dc:description", cf_text(t).0);
            written.push("description".into());
        }
    }
    if let Some(c) = fields.copyright.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        // EXIF Copyright (TIFF tag 0x8298); ImageIO keeps dc:rights in step with it
        let v = cf_text(c);
        unsafe { CGImageMetadataSetValueMatchingImageProperty(m.0, kCGImagePropertyTIFFDictionary, kCGImagePropertyTIFFCopyright, v.0) };
        written.push("copyright".into());
    }
    let tmp = target.with_file_name(format!(".{}.meta", target.file_name().unwrap_or_default().to_string_lossy()));
    let url = cf_url(&tmp);
    let dest = Cf(unsafe { CGImageDestinationCreateWithURL(url.0, type_id, 1, std::ptr::null()) });
    if dest.0.is_null() {
        return Err("destination could not be created".into());
    }
    let opts = dict(&[(unsafe { kCGImageDestinationMetadata }, m.0), (unsafe { kCGImageDestinationMergeMetadata }, unsafe { kCFBooleanTrue })]);
    let mut err: CFTypeRef = std::ptr::null();
    let ok = unsafe { CGImageDestinationCopyImageSource(dest.0, source_new.0, opts.0, &mut err) } != 0;
    if !err.is_null() {
        unsafe { CFRelease(err) };
    }
    drop(dest);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Err("metadata not written (ImageIO)".into());
    }
    std::fs::rename(&tmp, target).map_err(|e| e.to_string())?;
    Ok(written.join(", "))
}

/// Test helper: read one XMP value of a file (e.g. "xmp:Rating", "exif:FNumber").
pub fn read_metadata(p: &Path, path: &str) -> Option<String> {
    let s = source(p)?;
    let m = Cf(unsafe { CGImageSourceCopyMetadataAtIndex(s.0, 0, std::ptr::null()) });
    if m.0.is_null() {
        return None;
    }
    let v = Cf(unsafe { CGImageMetadataCopyStringValueWithPath(m.0, std::ptr::null(), cf_text(path).0) });
    if v.0.is_null() {
        return None;
    }
    let mut buf = vec![0u8; 512];
    (unsafe { CFStringGetCString(v.0, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_UTF8) } != 0).then(|| {
        let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..n]).into_owned()
    })
}

// ─────────────────────────────────────────────────────────── HDR (gain map, iPhone)

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageSourceCopyAuxiliaryDataInfoAtIndex(src: CFTypeRef, index: usize, type_id: CFTypeRef) -> CFTypeRef;
    static kCGImageAuxiliaryDataInfoMetadata: CFTypeRef;
}

/// Fetch newer ImageIO constants (macOS 15+) at runtime – the toolchain's SDK is older.
fn imageio_constant(name: &std::ffi::CStr) -> Option<CFTypeRef> {
    static H: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let h = *H.get_or_init(|| unsafe { libc::dlopen(c"/System/Library/Frameworks/ImageIO.framework/ImageIO".as_ptr(), libc::RTLD_NOW) } as usize);
    if h == 0 {
        return None;
    }
    let p = unsafe { libc::dlsym(h as *mut c_void, name.as_ptr()) } as *const CFTypeRef;
    (!p.is_null()).then(|| unsafe { *p }).filter(|v| !v.is_null())
}


/// Does the image have an HDR gain map? (header and auxiliary data only, fast)
pub fn hdr_info(path: &Path) -> Option<HdrInfo> {
    let src = source(path)?;
    let value = |info: CFTypeRef, md_path: &str| -> Option<f32> {
        let md = unsafe { CFDictionaryGetValue(info, kCGImageAuxiliaryDataInfoMetadata) };
        if md.is_null() {
            return None;
        }
        let v = Cf(unsafe { CGImageMetadataCopyStringValueWithPath(md, std::ptr::null(), cf_text(md_path).0) });
        if v.0.is_null() {
            return None;
        }
        let mut buf = vec![0u8; 64];
        (unsafe { CFStringGetCString(v.0, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_UTF8) } != 0).then(|| {
            let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..n]).trim().parse::<f32>().ok()
        })?
    };
    let mut outcome = None;
    if let Some(t) = imageio_constant(c"kCGImageAuxiliaryDataTypeISOGainMap") {
        let info = Cf(unsafe { CGImageSourceCopyAuxiliaryDataInfoAtIndex(src.0, 0, t) });
        if !info.0.is_null() {
            let h = value(info.0, "HDRToneMap:AlternateHeadroom").map(|l| l.exp2()).unwrap_or(4.0);
            outcome = Some(HdrInfo { headroom: h, iso: true });
        }
    }
    if let Some(t) = imageio_constant(c"kCGImageAuxiliaryDataTypeHDRGainMap") {
        let info = Cf(unsafe { CGImageSourceCopyAuxiliaryDataInfoAtIndex(src.0, 0, t) });
        if !info.0.is_null() {
            let h = value(info.0, "HDRGainMap:HDRGainMapHeadroom");
            match &mut outcome {
                Some(e) => {
                    if let Some(h) = h {
                        e.headroom = h;
                    }
                }
                None => outcome = Some(HdrInfo { headroom: h.unwrap_or(4.0), iso: false }),
            }
        }
    }
    outcome.filter(|e| e.headroom > 1.01)
}


#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetLength(d: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(d: CFTypeRef) -> *const u8;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    static kCGImageAuxiliaryDataInfoData: CFTypeRef;
    static kCGImageAuxiliaryDataInfoDataDescription: CFTypeRef;
}

/// Read the gain map (ISO preferred, else Apple). None: none.
pub fn gain_map(path: &Path) -> Option<GainMap> {
    let src = source(path)?;
    let md_value = |info: CFTypeRef, md_path: &str| -> Option<f32> {
        let md = unsafe { CFDictionaryGetValue(info, kCGImageAuxiliaryDataInfoMetadata) };
        if md.is_null() {
            return None;
        }
        let v = Cf(unsafe { CGImageMetadataCopyStringValueWithPath(md, std::ptr::null(), cf_text(md_path).0) });
        if v.0.is_null() {
            return None;
        }
        let mut buf = vec![0u8; 64];
        (unsafe { CFStringGetCString(v.0, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_UTF8) } != 0).then(|| {
            let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            String::from_utf8_lossy(&buf[..n]).trim().parse::<f32>().ok()
        })?
    };
    let (info, iso) = match imageio_constant(c"kCGImageAuxiliaryDataTypeISOGainMap").map(|t| Cf(unsafe { CGImageSourceCopyAuxiliaryDataInfoAtIndex(src.0, 0, t) })).filter(|i| !i.0.is_null()) {
        Some(i) => (i, true),
        None => (Cf(unsafe { CGImageSourceCopyAuxiliaryDataInfoAtIndex(src.0, 0, imageio_constant(c"kCGImageAuxiliaryDataTypeHDRGainMap")?) }), false),
    };
    if info.0.is_null() {
        return None;
    }
    let desc = unsafe { CFDictionaryGetValue(info.0, kCGImageAuxiliaryDataInfoDataDescription) };
    let data = unsafe { CFDictionaryGetValue(info.0, kCGImageAuxiliaryDataInfoData) };
    if desc.is_null() || data.is_null() {
        return None;
    }
    // sizes from the file's metadata: checked before any byte is addressed through them
    let w = usize::try_from(number(desc, cf_text("Width").0)?).ok()?;
    let h = usize::try_from(number(desc, cf_text("Height").0)?).ok()?;
    let bpr = usize::try_from(number(desc, cf_text("BytesPerRow").0)?).ok()?;
    let format = number(desc, cf_text("PixelFormat").0).unwrap_or(0) as u32;
    let n = usize::try_from(unsafe { CFDataGetLength(data) }).ok()?;
    let bytes = unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(data), n) };
    pixel_count(w, h)?;
    // 'L008' (8 bit, one plane) or 'L016' (16 bit); other formats: first byte per pixel
    let width_px = if format == u32::from_be_bytes(*b"L016") { 2 } else { (bpr / w).max(1) };
    if bpr.checked_mul(h)? > n || w.checked_mul(width_px)? > bpr {
        return None;
    }
    let g = |x: usize, y: usize| -> f32 {
        let o = y * bpr + x * width_px;
        if format == u32::from_be_bytes(*b"L016") {
            u16::from_ne_bytes([bytes[o], bytes[o + 1]]) as f32 / 65535.0
        } else {
            bytes[o] as f32 / 255.0
        }
    };
    let mut log2 = Vec::with_capacity(w * h);
    if iso {
        let k = |name: &str| md_value(info.0, &format!("HDRToneMap:ChannelMetadata[0].HDRToneMap:{name}"));
        let min = k("GainMapMin").unwrap_or(0.0);
        let max = k("GainMapMax").unwrap_or(md_value(info.0, "HDRToneMap:AlternateHeadroom").unwrap_or(1.0));
        let gamma = k("Gamma").unwrap_or(1.0).max(1e-3);
        for y in 0..h {
            for x in 0..w {
                log2.push(min + (max - min) * g(x, y).powf(1.0 / gamma));
            }
        }
        Some(GainMap {
            w,
            h,
            log2,
            base_headroom: md_value(info.0, "HDRToneMap:BaseHeadroom").unwrap_or(0.0),
            alternate_headroom: md_value(info.0, "HDRToneMap:AlternateHeadroom").unwrap_or(max),
            base_offset: k("BaseOffset").unwrap_or(0.0),
            alternate_offset: k("AlternateOffset").unwrap_or(0.0),
        })
    } else {
        let headroom = md_value(info.0, "HDRGainMap:HDRGainMapHeadroom").unwrap_or(4.0).max(1.0);
        for y in 0..h {
            for x in 0..w {
                log2.push((1.0 + (headroom - 1.0) * g(x, y)).log2());
            }
        }
        Some(GainMap { w, h, log2, base_headroom: 0.0, alternate_headroom: headroom.log2(), base_offset: 0.0, alternate_offset: 0.0 })
    }
}

/// The HDR image (gain map fully applied, ImageIO `kCGImageSourceDecodeToHDR`) as extended
/// linear sRGB in `w`×`h` (without EXIF rotation, like [`linear_full`]). None: HDR not possible.
pub fn hdr_linear(path: &Path, w: usize, h: usize) -> Option<Vec<[f32; 3]>> {
    let k = imageio_constant(c"kCGImageSourceDecodeRequest")?;
    let v = imageio_constant(c"kCGImageSourceDecodeToHDR")?;
    let src = source(path)?;
    let opts = dict(&[(k, v)]);
    let img = Cf(unsafe { CGImageSourceCreateImageAtIndex(src.0, 0, opts.0) });
    if img.0.is_null() {
        return None;
    }
    let n = pixel_count(w, h)?;
    let mut buf = vec![0f32; n * 4];
    let cs = Cf(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB) });
    let info: u32 = 5 | (1 << 8) | (2 << 12);
    let ctx = Cf(unsafe { CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 32, w * 16, cs.0, info) });
    if ctx.0.is_null() {
        return None;
    }
    unsafe {
        CGContextSetInterpolationQuality(ctx.0, 4);
        CGContextDrawImage(ctx.0, CGRect { o: CGPoint { x: 0.0, y: 0.0 }, s: CGSize { w: w as f64, h: h as f64 } }, img.0);
    }
    drop(ctx);
    Some(buf.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect())
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationAddImageFromSource(dest: CFTypeRef, src: CFTypeRef, index: usize, props: CFTypeRef);
    fn CGImageDestinationAddAuxiliaryDataInfo(dest: CFTypeRef, type_id: CFTypeRef, info: CFTypeRef);
    fn CGImageDestinationFinalize(dest: CFTypeRef) -> u8;
    static kCGImageDestinationLossyCompressionQuality: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataCreate(alloc: CFTypeRef, bytes: *const u8, n: isize) -> CFTypeRef;
    fn CFDictionaryCreateMutableCopy(alloc: CFTypeRef, cap: isize, d: CFTypeRef) -> CFTypeRef;
    fn CFDictionarySetValue(d: CFTypeRef, k: CFTypeRef, v: CFTypeRef);
}



/// Attach the original's gain map to an exported JPEG/HEIC (ImageIO; the image is re-encoded with
/// `quality` 0…1 in the process). `mapping`: how the export rotated, cropped or rectified the image
/// relative to the original – the map gets the same geometry (only
/// 8-bit maps); `None`: image unchanged, the map is taken over 1:1.
/// Ok(false): original without gain map or map not convertible – nothing changed.
pub fn append_gain_map(target: &Path, original: &Path, mapping: Option<&Mapping>, quality: f32) -> Result<bool, String> {
    let orig = source(original).ok_or("Original nicht lesbar")?;
    let mut infos: Vec<(CFTypeRef, Cf)> = Vec::new();
    for name in [c"kCGImageAuxiliaryDataTypeHDRGainMap", c"kCGImageAuxiliaryDataTypeISOGainMap"] {
        let Some(t) = imageio_constant(name) else { continue };
        let info = Cf(unsafe { CGImageSourceCopyAuxiliaryDataInfoAtIndex(orig.0, 0, t) });
        if !info.0.is_null() {
            infos.push((t, info));
        }
    }
    if infos.is_empty() {
        return Ok(false);
    }
    // convert (data and description)
    let mut rotated: Vec<(CFTypeRef, Cf)> = Vec::new();
    for (t, info) in &infos {
        let Some(map_px) = mapping else {
            rotated.push((*t, Cf(unsafe { CFDictionaryCreateMutableCopy(std::ptr::null(), 0, info.0) })));
            continue;
        };
        let desc = unsafe { CFDictionaryGetValue(info.0, kCGImageAuxiliaryDataInfoDataDescription) };
        let data = unsafe { CFDictionaryGetValue(info.0, kCGImageAuxiliaryDataInfoData) };
        // both entries must exist and have the right type before anything is read through them
        if desc.is_null() || data.is_null() || unsafe { CFGetTypeID(desc) != CFDictionaryGetTypeID() || CFGetTypeID(data) != CFDataGetTypeID() } {
            return Ok(false);
        }
        let (Some(w), Some(h), Some(bpr)) = (number(desc, cf_text("Width").0), number(desc, cf_text("Height").0), number(desc, cf_text("BytesPerRow").0)) else { return Ok(false) };
        let (Ok(w), Ok(h), Ok(bpr)) = (usize::try_from(w), usize::try_from(h), usize::try_from(bpr)) else { return Ok(false) };
        if number(desc, cf_text("PixelFormat").0) != Some(u32::from_be_bytes(*b"L008") as i64) {
            return Ok(false);
        }
        let n = usize::try_from(unsafe { CFDataGetLength(data) }).unwrap_or(0);
        if pixel_count(w, h).is_none() || bpr < w || bpr.checked_mul(h).is_none_or(|t| t > n) {
            return Ok(false);
        }
        let src = unsafe { std::slice::from_raw_parts(CFDataGetBytePtr(data), n) };
        let (out, nw, nh) = map_gain_map(src, bpr, w, h, map_px);
        let new = Cf(unsafe { CFDictionaryCreateMutableCopy(std::ptr::null(), 0, info.0) });
        let nd = Cf(unsafe { CFDictionaryCreateMutableCopy(std::ptr::null(), 0, desc) });
        let number_cf = |v: usize| Cf(unsafe { CFNumberCreate(std::ptr::null(), 10 /* kCFNumberLongType */, &(v as i64) as *const i64 as *const c_void) });
        unsafe {
            CFDictionarySetValue(nd.0, cf_text("Width").0, number_cf(nw).0);
            CFDictionarySetValue(nd.0, cf_text("Height").0, number_cf(nh).0);
            CFDictionarySetValue(nd.0, cf_text("BytesPerRow").0, number_cf(nw).0);
            let d = Cf(CFDataCreate(std::ptr::null(), out.as_ptr(), out.len() as isize));
            CFDictionarySetValue(new.0, kCGImageAuxiliaryDataInfoData, d.0);
            CFDictionarySetValue(new.0, kCGImageAuxiliaryDataInfoDataDescription, nd.0);
        }
        rotated.push((*t, new));
    }
    // rewrite: image (with its metadata) + gain map(s)
    let src = source(target).ok_or("Exportdatei nicht lesbar")?;
    let type_id = unsafe { CGImageSourceGetType(src.0) };
    if type_id.is_null() {
        return Err("Dateityp unbekannt".into());
    }
    let tmp = target.with_extension(format!("{}.tmp", target.extension().and_then(|e| e.to_str()).unwrap_or("jpg")));
    let url = cf_url(&tmp);
    let dest = Cf(unsafe { CGImageDestinationCreateWithURL(url.0, type_id, 1, std::ptr::null()) });
    if dest.0.is_null() {
        return Err("Ziel nicht anlegbar".into());
    }
    let q = quality.clamp(0.0, 1.0) as f64;
    let qn = Cf(unsafe { CFNumberCreate(std::ptr::null(), 13 /* kCFNumberDoubleType */, &q as *const f64 as *const c_void) });
    let props = dict(&[(unsafe { kCGImageDestinationLossyCompressionQuality }, qn.0)]);
    unsafe {
        CGImageDestinationAddImageFromSource(dest.0, src.0, 0, props.0);
        for (t, info) in &rotated {
            CGImageDestinationAddAuxiliaryDataInfo(dest.0, *t, info.0);
        }
        if CGImageDestinationFinalize(dest.0) == 0 {
            let _ = std::fs::remove_file(&tmp);
            return Err("Schreiben fehlgeschlagen".into());
        }
    }
    drop(src);
    std::fs::rename(&tmp, target).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod gain_map_tests {
    use super::*;

    /// map 8×4 (image 16×8), value grows with x: 0, 32, 64 … 224
    fn card() -> Vec<u8> {
        (0..4).flat_map(|_| (0..8).map(|x| (x * 32) as u8)).collect()
    }

    #[test]
    fn unchanged() {
        let id = |u: [f32; 2]| [u[0] * 16.0, u[1] * 8.0];
        let map_px = Mapping { source: (16, 8), output: (16, 8), to_source: &id };
        let (o, w, h) = map_gain_map(&card(), 8, 8, 4, &map_px);
        assert_eq!((w, h), (8, 4));
        assert_eq!(o, card());
    }

    #[test]
    fn quarter_turn() {
        // Frame::ori_to_src for q = 1: upright (8 × 16) → sensor (16 × 8): (x, y) → (y, 8 − x)
        let f = |u: [f32; 2]| {
            let (ox, oy) = (u[0] * 8.0, u[1] * 16.0);
            [oy, 8.0 - ox]
        };
        let map_px = Mapping { source: (16, 8), output: (8, 16), to_source: &f };
        let (o, w, h) = map_gain_map(&card(), 8, 8, 4, &map_px);
        assert_eq!((w, h), (4, 8));
        // the gradient now runs from top to bottom and is constant per row
        assert!(o[0] < 40 && o[(h - 1) * w] > 190, "{} {}", o[0], o[(h - 1) * w]);
        for y in 0..h {
            assert!(o[y * w..y * w + w].iter().all(|v| *v == o[y * w]));
        }
    }

    #[test]
    fn center_crop() {
        // middle quarter of the width: image pixels 4 … 12 → map pixels 2 … 6
        let f = |u: [f32; 2]| [4.0 + u[0] * 8.0, u[1] * 8.0];
        let map_px = Mapping { source: (16, 8), output: (8, 8), to_source: &f };
        let (o, w, h) = map_gain_map(&card(), 8, 8, 4, &map_px);
        assert_eq!((w, h), (4, 4));
        assert_eq!(o[0], 64);
        assert_eq!(o[3], 160);
    }
}

#[cfg(test)]
mod export_metadata_tests {
    use super::*;

    /// The copyright given at import reaches the exported file as EXIF Copyright and as XMP dc:rights.
    #[test]
    fn copyright_is_written() {
        let dir = std::env::temp_dir().join(format!("graineo-export-copyright-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("a.jpg");
        image::RgbImage::from_pixel(8, 8, image::Rgb([120, 90, 60])).save(&out).unwrap();
        let f = ExportFields { stars: 3, copyright: Some("© 2026 Test".into()), ..Default::default() };
        let written = write_metadata(&out, &out, 8, 8, &f).unwrap();
        assert!(written.contains("copyright"), "{written}");
        assert_eq!(read_metadata(&out, "dc:rights[1]").or_else(|| read_metadata(&out, "dc:rights")).as_deref(), Some("© 2026 Test"));
        let p = properties_dict_copyright(&out);
        assert_eq!(p.as_deref(), Some("© 2026 Test"), "EXIF Copyright");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// "7/5" → 1.4 (ImageIO shortens rationals).
    pub(crate) fn ratio(v: &str) -> f64 {
        match v.split_once('/') {
            Some((n, d)) => n.trim().parse::<f64>().unwrap() / d.trim().parse::<f64>().unwrap(),
            None => v.trim().parse().unwrap(),
        }
    }

    /// A camera-like source: JPEG with shooting data, serial numbers, owner and location (written with
    /// ImageIO directly, as a camera or another program would).
    fn camera_source(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("camera.jpg");
        image::RgbImage::from_pixel(16, 8, image::Rgb([120, 90, 60])).save(&p).unwrap();
        let src = source(&p).unwrap();
        let m = Cf(unsafe { CGImageMetadataCreateMutable() });
        for (path, v) in [
            ("tiff:Make", "FUJIFILM"),
            ("tiff:Model", "X-T5"),
            ("exifEX:LensModel", "XF23mmF1.4 R LM WR"),
            ("exif:FNumber", "14/10"),
            ("exif:ExposureTime", "1/250"),
            ("exif:FocalLength", "23/1"),
            ("exif:FocalLenIn35mmFilm", "35"),
            ("exifEX:PhotographicSensitivity", "200"),
            ("exifEX:BodySerialNumber", "SERIAL-123"),
            ("exifEX:LensSerialNumber", "LENS-456"),
            ("exifEX:CameraOwnerName", "Owner Name"),
            ("aux:SerialNumber", "SERIAL-123"),
            ("exif:GPSLatitude", "52,31.2N"),
            ("exif:GPSLongitude", "13,24.6E"),
            ("xmp:Rating", "5"),
        ] {
            unsafe { CGImageMetadataSetValueWithPath(m.0, std::ptr::null(), cf_text(path).0, cf_text(v).0) };
        }
        let (exif_dict, date) = (cf_text("{Exif}"), cf_text("2025:06:01 10:20:30"));
        unsafe { CGImageMetadataSetValueMatchingImageProperty(m.0, exif_dict.0, cf_text("DateTimeOriginal").0, date.0) };
        let one = cf_text("200");
        let iso = Cf(unsafe { CFArrayCreate(std::ptr::null(), &one.0, 1, &kCFTypeArrayCallBacks as *const c_void) });
        unsafe { CGImageMetadataSetValueWithPath(m.0, std::ptr::null(), cf_text("exif:ISOSpeedRatings").0, iso.0) };
        let kw = cf_text("Familie");
        let kws = Cf(unsafe { CFArrayCreate(std::ptr::null(), &kw.0, 1, &kCFTypeArrayCallBacks as *const c_void) });
        unsafe { CGImageMetadataSetValueWithPath(m.0, std::ptr::null(), cf_text("dc:subject").0, kws.0) };
        let tmp = dir.join("camera-tmp.jpg");
        let dest = Cf(unsafe { CGImageDestinationCreateWithURL(cf_url(&tmp).0, CGImageSourceGetType(src.0), 1, std::ptr::null()) });
        let opts = dict(&[(unsafe { kCGImageDestinationMetadata }, m.0), (unsafe { kCGImageDestinationMergeMetadata }, unsafe { kCFBooleanTrue })]);
        assert!(unsafe { CGImageDestinationCopyImageSource(dest.0, src.0, opts.0, std::ptr::null_mut()) } != 0);
        drop(dest);
        std::fs::rename(&tmp, &p).unwrap();
        assert_eq!(read_metadata(&p, "exifEX:BodySerialNumber").as_deref(), Some("SERIAL-123"), "source has a serial number");
        p
    }

    /// A metadata-free exported image in the format of `name` (JPEG, or converted by ImageIO to TIFF/PNG).
    fn export_of(dir: &Path, name: &str) -> std::path::PathBuf {
        let jpg = dir.join(format!("{name}.plain.jpg"));
        image::RgbImage::from_pixel(16, 8, image::Rgb([10, 20, 30])).save(&jpg).unwrap();
        let out = dir.join(name);
        let uti = match out.extension().and_then(|e| e.to_str()) {
            Some("png") => "public.png",
            Some("tif") => "public.tiff",
            _ => return { std::fs::rename(&jpg, &out).unwrap(); out },
        };
        let src = source(&jpg).unwrap();
        let dest = Cf(unsafe { CGImageDestinationCreateWithURL(cf_url(&out).0, cf_text(uti).0, 1, std::ptr::null()) });
        unsafe {
            CGImageDestinationAddImageFromSource(dest.0, src.0, 0, std::ptr::null());
            assert!(CGImageDestinationFinalize(dest.0) != 0);
        }
        out
    }

    /// Shooting data (default level): camera, lens, exposure, date and copyright; no serial numbers, no owner,
    /// no location, nothing personal of the original (keywords, rating).
    #[test]
    fn shooting_data_without_serial_location_or_personal_fields() {
        let dir = std::env::temp_dir().join(format!("graineo-export-shooting-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = camera_source(&dir);
        let out = export_of(&dir, "a.jpg");
        let f = ExportFields { copyright: Some("© Test".into()), ..Default::default() };
        write_metadata(&out, &src, 16, 8, &f).unwrap();
        let r = |p: &str| read_metadata(&out, p);
        assert_eq!(r("tiff:Make").as_deref(), Some("FUJIFILM"));
        assert_eq!(r("tiff:Model").as_deref(), Some("X-T5"));
        assert_eq!(r("exifEX:LensModel").as_deref(), Some("XF23mmF1.4 R LM WR"));
        assert_eq!(r("exif:FNumber").map(|v| ratio(&v)), Some(1.4));
        assert_eq!(r("exif:ExposureTime").map(|v| ratio(&v)), Some(1.0 / 250.0));
        assert_eq!(exif_num(&out, "FocalLenIn35mmFilm"), Some(35.0));
        assert_eq!(r("exif:ISOSpeedRatings[1]").or_else(|| r("exifEX:PhotographicSensitivity")).as_deref(), Some("200"));
        assert_eq!(properties(&out).and_then(|e| e.capture).as_deref(), Some("2025-06-01T10:20:30"));
        assert!(tiff_text(&out, "Software").is_some_and(|s| s.starts_with("graineo ")), "{:?}", tiff_text(&out, "Software"));
        assert_eq!(properties_dict_copyright(&out).as_deref(), Some("© Test"));
        for p in ["exifEX:BodySerialNumber", "exifEX:LensSerialNumber", "aux:SerialNumber", "exifEX:CameraOwnerName", "exif:GPSLatitude", "exif:GPSLongitude", "xmp:Rating"] {
            assert_eq!(r(p), None, "{p} must not be written");
        }
        let bytes = String::from_utf8_lossy(&std::fs::read(&out).unwrap()).into_owned();
        for t in ["SERIAL-123", "LENS-456", "Owner Name", "Familie"] {
            assert!(!bytes.contains(t), "{t} must not be in the file");
        }
        // with the location switch: GPS of the original, still no serial number
        let out = export_of(&dir, "b.jpg");
        write_metadata(&out, &src, 16, 8, &ExportFields { gps: true, ..Default::default() }).unwrap();
        assert!(read_metadata(&out, "exif:GPSLatitude").is_some());
        assert_eq!(read_metadata(&out, "exifEX:BodySerialNumber"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// "All": stars, label and keywords of the library; originals ImageIO cannot read (X3F) get the
    /// shooting data from `shot`; TIFF and PNG carry the same fields as JPEG.
    #[test]
    fn all_fields_and_shot_in_every_format() {
        let dir = std::env::temp_dir().join(format!("graineo-export-all-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = crate::export_meta::Shot {
            make: Some("SIGMA".into()),
            model: Some("SIGMA DP2 Merrill".into()),
            lens: Some("SIGMA 30mm f/2.8".into()),
            date: Some("2014:07:16 14:02:02".into()),
            exposure_time: Some(1.0 / 320.0),
            f_number: Some(7.1),
            iso: Some(100),
            focal_length: Some(30.0),
            focal_length_35mm: Some(45),
            ..Default::default()
        };
        let missing = dir.join("original.x3f");
        for name in ["a.jpg", "a.tif", "a.png"] {
            let out = export_of(&dir, name);
            let f = ExportFields { personal: true, stars: 4, label: "Rot".into(), keywords: vec!["Meer".into()], shot: Some(shot.clone()), ..Default::default() };
            crate::export_meta::write(&out, &missing, &f).unwrap();
            let r = |p: &str| read_metadata(&out, p);
            assert_eq!(r("tiff:Model").as_deref(), Some("SIGMA DP2 Merrill"), "{name}");
            assert_eq!(r("exifEX:LensModel").as_deref(), Some("SIGMA 30mm f/2.8"), "{name}");
            assert_eq!(r("exif:FNumber").map(|v| ratio(&v)), Some(7.1), "{name}");
            assert_eq!(r("exif:ExposureTime").map(|v| ratio(&v)), Some(1.0 / 320.0), "{name}");
            assert_eq!(exif_num(&out, "FocalLenIn35mmFilm"), Some(45.0), "{name}");
            assert_eq!(properties(&out).and_then(|e| e.capture).as_deref(), Some("2014-07-16T14:02:02"), "{name}");
            assert_eq!(r("xmp:Rating").as_deref(), Some("4"), "{name}");
            assert_eq!(r("xmp:Label").as_deref(), Some("Rot"), "{name}");
            assert!(String::from_utf8_lossy(&std::fs::read(&out).unwrap()).contains(">Meer</rdf:li>"), "{name}: keyword");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A number of the EXIF dictionary of the image properties.
    fn exif_num(p: &Path, key: &str) -> Option<f64> {
        let src = source(p)?;
        let props = Cf(unsafe { CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()) });
        let exif = unsafe { CFDictionaryGetValue(props.0, kCGImagePropertyExifDictionary) };
        (!exif.is_null()).then(|| float_of(exif, cf_text(key).0)).flatten()
    }

    /// A text of the TIFF dictionary of the image properties.
    fn tiff_text(p: &Path, key: &str) -> Option<String> {
        let src = source(p)?;
        let props = Cf(unsafe { CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()) });
        let tiff = unsafe { CFDictionaryGetValue(props.0, kCGImagePropertyTIFFDictionary) };
        (!tiff.is_null()).then(|| text(tiff, cf_text(key).0)).flatten()
    }

    /// EXIF/TIFF Copyright of a file (TIFF dictionary of the image properties).
    fn properties_dict_copyright(p: &Path) -> Option<String> {
        let src = source(p)?;
        let props = Cf(unsafe { CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()) });
        if props.0.is_null() {
            return None;
        }
        let tiff = unsafe { CFDictionaryGetValue(props.0, kCGImagePropertyTIFFDictionary) };
        (!tiff.is_null()).then(|| text(tiff, unsafe { kCGImagePropertyTIFFCopyright })).flatten()
    }
}

#[cfg(test)]
mod date_guard_tests {
    use super::exif_date;
    #[test]
    fn exif_date_checks_before_byte_slicing() {
        assert_eq!(exif_date(" 2024:05:11 14:03:22 ").as_deref(), Some("2024-05-11T14:03:22"));
        assert_eq!(exif_date("2024:05:11 14:03:22+01:00").as_deref(), Some("2024-05-11T14:03:22"));
        for s in ["", "2024", "2024:aä:01 00:00:00", "2024:01:🦀 00:00:00", "2024-01-01 00:00:00", "2024:01:01 0a:00:00"] {
            assert!(exif_date(s).is_none(), "{s}");
        }
    }
}


fn cf_string(v: CFTypeRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let n = unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(v), K_CF_STRING_UTF8) } + 1;
    let mut buf = vec![0u8; n.clamp(2, 1 << 20) as usize];
    if unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_UTF8) } == 0 {
        return None;
    }
    let l = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..l]).trim().to_string()).filter(|t| !t.is_empty())
}

/// String or list of strings → texts.
fn cf_texts(v: CFTypeRef, out: &mut Vec<String>) {
    if v.is_null() {
        return;
    }
    if unsafe { CFGetTypeID(v) == CFArrayGetTypeID() } {
        for i in 0..unsafe { CFArrayGetCount(v) } {
            if let Some(t) = cf_string(unsafe { CFArrayGetValueAtIndex(v, i) }) {
                out.push(t);
            }
        }
    } else if let Some(t) = cf_string(v) {
        out.push(t);
    }
}

fn subword_vocab(p: CFTypeRef, name: &str) -> Option<CFTypeRef> {
    let k = cf_text(name);
    let v = unsafe { CFDictionaryGetValue(p, k.0) };
    (!v.is_null() && unsafe { CFGetTypeID(v) == CFDictionaryGetTypeID() }).then_some(v)
}

fn value_of(d: CFTypeRef, name: &str, out: &mut Vec<String>) {
    let k = cf_text(name);
    cf_texts(unsafe { CFDictionaryGetValue(d, k.0) }, out);
}

/// All strings of a properties dictionary (without date, time and technical fields).
fn all_texts_of(d: CFTypeRef, out: &mut Vec<String>) {
    let n = unsafe { CFDictionaryGetCount(d) };
    let mut keys = vec![std::ptr::null(); n.max(0) as usize];
    let mut vals = vec![std::ptr::null(); n.max(0) as usize];
    unsafe { CFDictionaryGetKeysAndValues(d, keys.as_mut_ptr(), vals.as_mut_ptr()) };
    for (k, v) in keys.iter().zip(vals.iter()) {
        let name = cf_string(*k).unwrap_or_default().to_lowercase();
        if ["date", "time", "version", "digest", "software", "urgency", "rating", "interlace", "gamma", "pixel", "resolution", "orientation", "color", "profile", "dpi", "byline", "credit", "source", "copyright", "writer", "contact", "author", "creator", "rights"].iter().any(|s| name.contains(s)) {
            continue;
        }
        cf_texts(*v, out);
    }
}

/// Text fields from the image properties (IPTC, EXIF comment, TIFF description/artist/copyright, PNG texts).
/// Reads only the file header.
pub fn text_fields(path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Some(src) = source(path) else { return out };
    let props = Cf(unsafe { CGImageSourceCopyPropertiesAtIndex(src.0, 0, std::ptr::null()) });
    if props.0.is_null() {
        return out;
    }
    let p = props.0;
    if let Some(d) = subword_vocab(p, "{IPTC}") {
        all_texts_of(d, &mut out);
    }
    if let Some(d) = subword_vocab(p, "{Exif}") {
        value_of(d, "UserComment", &mut out);
    }
    if let Some(d) = subword_vocab(p, "{TIFF}") {
        for k in ["ImageDescription"] {
            value_of(d, k, &mut out);
        }
    }
    if let Some(d) = subword_vocab(p, "{PNG}") {
        all_texts_of(d, &mut out);
    }
    out
}

#[cfg(test)]
mod alpha_tests {
    /// PNG with transparency: shown over white, not black (before 0.108 a half transparent mid grey came out dark).
    #[test]
    fn transparent_png_over_white() {
        let d = std::env::temp_dir().join(format!("graineo-alpha-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("a.png");
        // left half fully transparent (black under it), right half opaque red, 4×2
        let mut img = image::RgbaImage::new(4, 2);
        for (x, _, px) in img.enumerate_pixels_mut() {
            *px = if x < 2 { image::Rgba([0, 0, 0, 0]) } else { image::Rgba([255, 0, 0, 255]) };
        }
        img.save(&p).unwrap();
        let (w, h, rgb) = super::image(&p, None).unwrap();
        assert_eq!((w, h), (4, 2));
        assert!(rgb[0..3].iter().all(|&c| c >= 250), "transparent → white: {:?}", &rgb[0..3]);
        assert!(rgb[6] >= 250 && rgb[7] <= 5 && rgb[8] <= 5, "opaque red stays: {:?}", &rgb[6..9]);
        let (_, _, lin, _) = super::linear_full(&p).unwrap();
        assert!(lin[0].iter().all(|&c| c > 0.98), "linear: {:?}", lin[0]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
