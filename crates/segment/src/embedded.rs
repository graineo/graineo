//! Depth stored in the photo itself (macOS ImageIO): HEIC/JPEG files of phones with a depth or disparity map as
//! auxiliary image (portrait mode, iPhone). Measured by the camera, not estimated; read-only. The map has the
//! orientation of the stored pixels (like the mask space of a photo that is not a RAW) and a lower resolution.
//! Other systems and files without such a map: `None`.
use std::path::Path;

/// Disparity (larger = nearer) of the depth map stored in `path`, as an [`crate::depth::Estimate`]; `None` if the
/// file has none (or on other systems).
pub fn read(path: &Path) -> Option<crate::depth::Estimate> {
    #[cfg(target_os = "macos")]
    {
        mac::read(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// Turn a depth map (metres, larger = farther) into disparity; invalid values (0, NaN) become the farthest.
pub fn disparity_from_depth(depth: &[f32]) -> Vec<f32> {
    depth.iter().map(|d| if d.is_finite() && *d > 0.0 { 1.0 / d } else { 0.0 }).collect()
}

/// Decode a half float (IEEE 754 binary16).
pub fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((bits >> 10) & 0x1f) as i32;
    let frac = (bits & 0x3ff) as f32;
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => {
            if frac == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Pixel formats of the auxiliary images (Core Video four-character codes).
pub const DISPARITY_F16: u32 = u32::from_be_bytes(*b"hdis");
pub const DISPARITY_F32: u32 = u32::from_be_bytes(*b"fdis");
pub const DEPTH_F16: u32 = u32::from_be_bytes(*b"hdep");
pub const DEPTH_F32: u32 = u32::from_be_bytes(*b"fdep");

/// Values of a map in `format` from rows of `bpr` bytes; disparity in any case. `None`: unknown format or too short.
pub fn decode(format: u32, width: usize, height: usize, bpr: usize, bytes: &[u8]) -> Option<Vec<f32>> {
    let (size, is_depth) = match format {
        DISPARITY_F16 => (2, false),
        DISPARITY_F32 => (4, false),
        DEPTH_F16 => (2, true),
        DEPTH_F32 => (4, true),
        _ => return None,
    };
    if width == 0 || height == 0 || bpr < width * size || bytes.len() < bpr * (height - 1) + width * size {
        return None;
    }
    let mut v = Vec::with_capacity(width * height);
    for y in 0..height {
        let row = &bytes[y * bpr..y * bpr + width * size];
        for c in row.chunks_exact(size) {
            v.push(if size == 2 { half(u16::from_le_bytes([c[0], c[1]])) } else { f32::from_le_bytes([c[0], c[1], c[2], c[3]]) });
        }
    }
    if is_depth {
        v = disparity_from_depth(&v);
    }
    // NaN (no measurement) → farthest
    Some(v.into_iter().map(|d| if d.is_finite() { d } else { 0.0 }).collect())
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::path::Path;

    type CFTypeRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(alloc: CFTypeRef, buf: *const u8, len: isize, is_dir: u8) -> CFTypeRef;
        fn CFDictionaryGetValue(d: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(n: CFTypeRef, the_type: isize, value: *mut c_void) -> u8;
        fn CFDataGetLength(d: CFTypeRef) -> isize;
        fn CFDataGetBytePtr(d: CFTypeRef) -> *const u8;
        fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const u8, enc: u32) -> CFTypeRef;
        fn CFRelease(p: CFTypeRef);
    }

    #[link(name = "ImageIO", kind = "framework")]
    extern "C" {
        fn CGImageSourceCreateWithURL(url: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
        fn CGImageSourceCopyAuxiliaryDataInfoAtIndex(src: CFTypeRef, index: usize, kind: CFTypeRef) -> CFTypeRef;
        static kCGImageAuxiliaryDataTypeDisparity: CFTypeRef;
        static kCGImageAuxiliaryDataTypeDepth: CFTypeRef;
        static kCGImageAuxiliaryDataInfoData: CFTypeRef;
        static kCGImageAuxiliaryDataInfoDataDescription: CFTypeRef;
    }

    struct Cf(CFTypeRef);
    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn key(s: &str) -> Cf {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        Cf(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr() as *const u8, 0x0800_0100) })
    }

    fn number(d: CFTypeRef, name: &str) -> Option<i64> {
        let k = key(name);
        let v = unsafe { CFDictionaryGetValue(d, k.0) };
        if v.is_null() {
            return None;
        }
        let mut out: i64 = 0;
        // kCFNumberSInt64Type = 4
        (unsafe { CFNumberGetValue(v, 4, &mut out as *mut i64 as *mut c_void) } != 0).then_some(out)
    }

    pub fn read(path: &Path) -> Option<crate::depth::Estimate> {
        let bytes = path.as_os_str().as_encoded_bytes();
        let url = Cf(unsafe { CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 0) });
        if url.0.is_null() {
            return None;
        }
        let src = Cf(unsafe { CGImageSourceCreateWithURL(url.0, std::ptr::null()) });
        if src.0.is_null() {
            return None;
        }
        // disparity first (what the phones store), depth otherwise
        for kind in unsafe { [kCGImageAuxiliaryDataTypeDisparity, kCGImageAuxiliaryDataTypeDepth] } {
            let info = Cf(unsafe { CGImageSourceCopyAuxiliaryDataInfoAtIndex(src.0, 0, kind) });
            if info.0.is_null() {
                continue;
            }
            let data = unsafe { CFDictionaryGetValue(info.0, kCGImageAuxiliaryDataInfoData) };
            let desc = unsafe { CFDictionaryGetValue(info.0, kCGImageAuxiliaryDataInfoDataDescription) };
            if data.is_null() || desc.is_null() {
                continue;
            }
            let (w, h, bpr, format) = (number(desc, "Width")?, number(desc, "Height")?, number(desc, "BytesPerRow")?, number(desc, "PixelFormat")?);
            let n = unsafe { CFDataGetLength(data) } as usize;
            let p = unsafe { CFDataGetBytePtr(data) };
            if p.is_null() || w <= 0 || h <= 0 || bpr <= 0 {
                continue;
            }
            let raw = unsafe { std::slice::from_raw_parts(p, n) };
            let disparity = super::decode(format as u32, w as usize, h as usize, bpr as usize, raw)?;
            return Some(crate::depth::Estimate { width: w as u32, height: h as u32, disparity });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0x4000), 2.0);
        assert_eq!(half(0x3800), 0.5);
        assert_eq!(half(0xc000), -2.0);
        assert!(half(0x7e00).is_nan());
    }

    #[test]
    fn maps_of_all_formats_become_disparity() {
        // 2 × 1, rows padded to 8 bytes
        let f16 = [0x00, 0x3c, 0x00, 0x40, 0, 0, 0, 0]; // 1.0, 2.0
        assert_eq!(decode(DISPARITY_F16, 2, 1, 8, &f16), Some(vec![1.0, 2.0]));
        assert_eq!(decode(DEPTH_F16, 2, 1, 8, &f16), Some(vec![1.0, 0.5]));
        let f32s: Vec<u8> = [4.0f32, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(decode(DEPTH_F32, 2, 1, 8, &f32s), Some(vec![0.25, 0.0]), "no measurement = farthest");
        assert_eq!(decode(DISPARITY_F32, 2, 1, 8, &f32s), Some(vec![4.0, 0.0]));
        assert!(decode(u32::from_be_bytes(*b"L008"), 2, 1, 8, &f16).is_none());
        assert!(decode(DISPARITY_F16, 4, 1, 8, &f16[..6]).is_none());
    }

    #[test]
    fn a_photo_without_depth_has_none() {
        let dir = std::env::temp_dir().join(format!("graineo-embedded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plain.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3])).save(&p).unwrap();
        assert!(read(&p).is_none());
        assert!(read(&dir.join("missing.heic")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A HEIC with a disparity map written by the system (macOS only): the map comes back as written.
    #[test]
    #[cfg(target_os = "macos")]
    fn disparity_written_by_the_system_is_read_back() {
        let dir = std::env::temp_dir().join(format!("graineo-embedded-heic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("portrait.heic");
        // disparity map 64 × 48: near (2.0) on the left half, far (0.5) on the right
        let (w, h) = (64usize, 48usize);
        let values: Vec<f32> = (0..w * h).map(|i| if i % w < w / 2 { 2.0 } else { 0.5 }).collect();
        if !write_heic_with_disparity(&p, 256, 192, w, h, &values) {
            eprintln!("HEIC writing not available here; skipped");
            return;
        }
        let e = read(&p).expect("disparity map");
        assert_eq!((e.width as usize, e.height as usize), (w, h));
        assert!((e.disparity[0] - 2.0).abs() < 1e-3 && (e.disparity[w - 1] - 0.5).abs() < 1e-3, "{:?}", &e.disparity[..4]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writes a grey HEIC of `iw × ih` with a 32-bit disparity map (test helper, system encoder).
    #[cfg(target_os = "macos")]
    fn write_heic_with_disparity(path: &Path, iw: usize, ih: usize, w: usize, h: usize, values: &[f32]) -> bool {
        use std::ffi::c_void;
        type R = *const c_void;
        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFURLCreateFromFileSystemRepresentation(a: R, b: *const u8, l: isize, d: u8) -> R;
            fn CFStringCreateWithCString(a: R, s: *const u8, e: u32) -> R;
            fn CFDataCreate(a: R, b: *const u8, l: isize) -> R;
            fn CFNumberCreate(a: R, t: isize, v: *const c_void) -> R;
            fn CFDictionaryCreate(a: R, k: *const R, v: *const R, n: isize, kc: *const c_void, vc: *const c_void) -> R;
            fn CFRelease(p: R);
            static kCFTypeDictionaryKeyCallBacks: c_void;
            static kCFTypeDictionaryValueCallBacks: c_void;
        }
        #[link(name = "ImageIO", kind = "framework")]
        extern "C" {
            fn CGImageDestinationCreateWithURL(url: R, kind: R, count: usize, opts: R) -> R;
            fn CGImageDestinationAddImage(d: R, img: R, props: R);
            fn CGImageDestinationAddAuxiliaryDataInfo(d: R, kind: R, info: R);
            fn CGImageDestinationFinalize(d: R) -> bool;
            static kCGImageAuxiliaryDataTypeDisparity: R;
            static kCGImageAuxiliaryDataInfoData: R;
            static kCGImageAuxiliaryDataInfoDataDescription: R;
        }
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGColorSpaceCreateDeviceRGB() -> R;
            fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, cs: R, info: u32) -> R;
            fn CGBitmapContextCreateImage(ctx: R) -> R;
        }
        unsafe {
            let s = |t: &str| {
                let c = std::ffi::CString::new(t).unwrap();
                CFStringCreateWithCString(std::ptr::null(), c.as_ptr() as *const u8, 0x0800_0100)
            };
            let num = |v: i64| CFNumberCreate(std::ptr::null(), 4, &v as *const i64 as *const c_void);
            let dict = |k: &[R], v: &[R]| CFDictionaryCreate(std::ptr::null(), k.as_ptr(), v.as_ptr(), k.len() as isize, &kCFTypeDictionaryKeyCallBacks as *const c_void, &kCFTypeDictionaryValueCallBacks as *const c_void);
            let mut px = vec![128u8; iw * ih * 4];
            let cs = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(px.as_mut_ptr() as *mut c_void, iw, ih, 8, iw * 4, cs, 5 | (4 << 12));
            let img = CGBitmapContextCreateImage(ctx);
            let b = path.as_os_str().as_encoded_bytes();
            let url = CFURLCreateFromFileSystemRepresentation(std::ptr::null(), b.as_ptr(), b.len() as isize, 0);
            let heic = s("public.heic");
            let dest = CGImageDestinationCreateWithURL(url, heic, 1, std::ptr::null());
            if dest.is_null() || img.is_null() {
                return false;
            }
            CGImageDestinationAddImage(dest, img, std::ptr::null());
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let data = CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize);
            let desc = dict(&[s("Width"), s("Height"), s("BytesPerRow"), s("PixelFormat")], &[num(w as i64), num(h as i64), num((w * 4) as i64), num(DISPARITY_F32 as i64)]);
            let info = dict(&[kCGImageAuxiliaryDataInfoData, kCGImageAuxiliaryDataInfoDataDescription], &[data, desc]);
            CGImageDestinationAddAuxiliaryDataInfo(dest, kCGImageAuxiliaryDataTypeDisparity, info);
            let ok = CGImageDestinationFinalize(dest);
            for o in [info, desc, data, dest, heic, url, img, ctx, cs] {
                CFRelease(o);
            }
            ok && path.exists()
        }
    }
}
