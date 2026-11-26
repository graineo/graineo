//! HEIC/HEIF outside macOS through heifer, a pure-Rust HEIF/HEVC decoder (MIT/Apache-2.0, no unsafe code, no
//! shared libraries to ship). It is the only HEIF path on Windows and Linux: no system codecs, no store extensions.
//! On macOS ImageIO decodes HEIC; there this module is only used by tests.
//!
//! The decoder applies the file's `irot`/`imir`/`clap` (pixels come back upright), drops alpha and ignores auxiliary
//! images (gain maps, depth): the primary SDR image is returned. 10-bit images keep their precision. Its HEVC output is
//! bit-exact (MD5 SEI checked); YCbCr→RGB upsamples chroma bilinearly with co-sited samples like ImageIO (PSNR
//! against ImageIO about 51 dB on an 8-bit 12 MP test). Colour: the embedded ICC profile or nclx Display P3 is
//! applied with the app's colour management, giving extended linear sRGB as on the Mac.

use heifer::heifer_isobmff::{boxes::Property, HeifFile};
use std::path::Path;

/// Largest primary image decoded (pixels): the decoded RGBA16 samples and the app's linear copies need about 40 bytes
/// per pixel, so heifer's own limit (2^28 px) would let a forged file ask for over 10 GB. 200 MP covers every camera.
const MAX_PIXELS: u64 = 200_000_000;
/// Largest embedded thumbnail taken (pixels); a larger `thmb` item is ignored and the whole image is decoded instead.
const MAX_THUMBNAIL_PIXELS: u64 = 16_000_000;

/// heifer is pure Rust but parses untrusted files: a panic in it must not take the calling thread (or the app) down.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err("heifer: decoder panicked".into()))
}

/// File extensions of HEIF images (HEIC from phones, HIF from Canon/Fujifilm/Sony cameras).
pub fn is_heif(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif" | "hif"))
}

/// Whether the decoder returns the image already turned upright: it applies the file's `irot`/`imir`, so the EXIF
/// orientation must not be applied again.
pub fn applies_orientation(path: &Path) -> bool {
    use std::io::Read;
    let mut b = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(4 << 20).read_to_end(&mut b));
    read.is_ok() && crate::imageio_common::heif_colour(&b).transformed
}

/// Upright size of the primary image from its `ispe`, `clap` and `irot`, without decoding pixels.
pub fn size(path: &Path) -> Option<(u32, u32)> {
    let data = std::fs::read(path).ok()?;
    guarded(|| size_of(&data).ok_or_else(String::new)).ok()
}

fn size_of(data: &[u8]) -> Option<(u32, u32)> {
    let file = HeifFile::parse(data).ok()?;
    let (mut w, mut h) = file.image_size(file.primary_id).ok()??;
    for p in file.item_properties(file.primary_id).ok()? {
        match *p {
            Property::CleanAperture { width, height, .. } if width.1 != 0 && height.1 != 0 => {
                w = (f64::from(width.0) / f64::from(width.1)).round() as u32;
                h = (f64::from(height.0) / f64::from(height.1)).round() as u32;
            }
            Property::Rotation(q) if q % 2 == 1 => (w, h) = (h, w),
            _ => {}
        }
    }
    (w > 0 && h > 0).then_some((w, h))
}

/// The file's colour description: embedded ICC profile, else Display P3 for nclx primaries 12, else none (sRGB).
fn profile(data: &[u8]) -> Option<Vec<u8>> {
    let boxes = crate::imageio_common::heif_colour(&data[..data.len().min(4 << 20)]);
    boxes.icc.or_else(|| (boxes.nclx_primaries == Some(12)).then(|| graineo_color::Space::DisplayP3.icc().ok()).flatten())
}

/// heifer's RGBA samples as encoded RGB 0…1 (alpha dropped).
fn encoded(im: &heifer::image::Image) -> Vec<f32> {
    let max = ((1u32 << im.bit_depth) - 1) as f32;
    im.data.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).map(|v| f32::from(v) / max).collect()
}

/// Primary image decoded to encoded RGB 0…1 (upright, alpha dropped) plus the file's colour description. Grid tiles
/// are decoded on all cores.
fn decode_encoded(path: &Path) -> Result<(usize, usize, Vec<f32>, Option<Vec<u8>>), String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    guarded(|| {
        let im = heifer::decode_with_options(&data, &heifer::Options::new().with_max_pixels(MAX_PIXELS)).map_err(|e| format!("heifer: {e}"))?;
        Ok((im.width as usize, im.height as usize, encoded(&im), profile(&data)))
    })
}

fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Encoded RGB with its ICC profile → extended linear sRGB; without a usable profile read as sRGB.
pub(crate) fn to_linear(encoded: &[f32], icc: Option<&[u8]>) -> Vec<[f32; 3]> {
    let flat = icc.and_then(|p| graineo_color::decode_icc(encoded, p).ok()).unwrap_or_else(|| encoded.iter().map(|&c| srgb_decode(c)).collect());
    flat.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect()
}

/// Whole HEIF image as extended linear sRGB, upright (irot/imir applied).
pub fn linear(path: &Path) -> Result<(usize, usize, Vec<[f32; 3]>), String> {
    let (w, h, enc, profile) = decode_encoded(path)?;
    // the profile comes from the file as well
    guarded(|| Ok((w, h, to_linear(&enc, profile.as_deref()))))
}

/// The embedded thumbnail (`thmb` item, e.g. 320×240 in iPhone photos; the smallest when there are several) as
/// extended linear sRGB, upright; `None` when the file has none. Decodes in a few milliseconds instead of a second.
/// The thumbnail is assumed to share the primary image's colour description (it does in files from phones and cameras).
pub fn thumbnail(path: &Path) -> Option<(usize, usize, Vec<[f32; 3]>)> {
    let data = std::fs::read(path).ok()?;
    guarded(|| {
        // heifer decodes the smallest `thmb` item with its default limit: check its declared size first
        let file = HeifFile::parse(&data).map_err(|e| e.to_string())?;
        let smallest = file.referencing_items(file.primary_id, b"thmb").into_iter().filter_map(|id| file.image_size(id).ok().flatten()).map(|(w, h)| u64::from(w) * u64::from(h)).min();
        if smallest.is_none_or(|n| n > MAX_THUMBNAIL_PIXELS) {
            return Err(String::new());
        }
        let im = heifer::decode_thumbnail(&data).map_err(|e| e.to_string())?.ok_or_else(String::new)?;
        if u64::from(im.width) * u64::from(im.height) > MAX_THUMBNAIL_PIXELS {
            return Err(String::new());
        }
        let px = to_linear(&encoded(&im), profile(&data).as_deref());
        Ok((im.width as usize, im.height as usize, px))
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn charts() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/heic")
    }

    /// Test charts of `tests/heic` (made by `make-charts.sh`): `srgb.heic`, `p3.heic`, `p3-10bit.heic` (600×400,
    /// red/green/blue patches in the top row), `rot90.heic` (`srgb.heic` with irot 90°, decoded 400×600).
    #[test]
    fn decodes_heic_charts() {
        let dir = charts();
        for name in ["srgb.heic", "p3.heic", "p3-10bit.heic", "thumb.heic"] {
            let p = dir.join(name);
            let t = std::time::Instant::now();
            let (w, h, px) = linear(&p).unwrap();
            eprintln!("{name}: {w}×{h} in {:?}", t.elapsed());
            assert_eq!((w, h), (600, 400), "{name}");
            assert_eq!(size(&p), Some((600, 400)), "{name}");
            for (x, want) in [(100, [1.0, 0.0, 0.0]), (300, [0.0, 1.0, 0.0]), (500, [0.0, 0.0, 1.0])] {
                let got = px[100 * w + x];
                for c in 0..3 {
                    assert!((got[c] - want[c]).abs() < 0.06, "{name} patch {x}: {got:?}");
                }
            }
        }
        // turned clockwise: the red patch (top left) ends up top right
        let rot = dir.join("rot90.heic");
        let (w, h, px) = linear(&rot).unwrap();
        assert_eq!((w, h), (400, 600));
        assert_eq!(size(&rot), Some((400, 600)));
        let red = px[100 * w + 300];
        assert!(red[0] > 0.9 && red[1] < 0.06, "rot90: {red:?}");
        assert!(applies_orientation(&rot));
        assert!(!applies_orientation(&dir.join("srgb.heic")));
    }

    /// `thumb.heic` (`srgb.heic` with a 120 px `thmb` item) yields its thumbnail with the same colours; the other
    /// charts have none.
    #[test]
    fn embedded_thumbnail() {
        let (w, h, px) = thumbnail(&charts().join("thumb.heic")).expect("thumbnail");
        assert_eq!((w, h), (120, 80));
        for (x, c) in [(20, 0), (60, 1), (100, 2)] {
            let got = px[20 * w + x];
            assert!(got[c] > 0.85 && (0..3).filter(|&k| k != c).all(|k| got[k] < 0.08), "thumb patch {x}: {got:?}");
        }
        assert!(thumbnail(&charts().join("srgb.heic")).is_none());
    }

    /// Mutation fuzzing of every HEIF entry point with the test charts (5000 variants by default, `HEIF_FUZZ_CASES`
    /// for more): no panic escapes, every case ends within 20 s.
    #[test]
    fn fuzz_mutated_charts() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let cases: usize = std::env::var("HEIF_FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(5000);
        let seeds: Vec<Vec<u8>> = ["srgb.heic", "p3-10bit.heic", "rot90.heic", "thumb.heic"].iter().map(|n| std::fs::read(charts().join(n)).unwrap()).collect();
        let dir = std::env::temp_dir().join(format!("graineo-heif-fuzz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        let file = dir.join("case.heic");
        let worker = std::thread::spawn(move || {
            let mut s = 0x2545_F491_4F6C_DD1Du64;
            let mut rnd = move || {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s
            };
            for i in 0..cases {
                let mut f = seeds[i % seeds.len()].clone();
                // mostly the box structure at the start (meta, ispe, clap, iloc), sometimes anywhere
                let head = f.len().min(if rnd() % 4 == 0 { usize::MAX } else { 2048 });
                for _ in 0..1 + rnd() % 6 {
                    let at = rnd() as usize % head.min(f.len());
                    match rnd() % 6 {
                        0 => f[at] ^= 1 << (rnd() % 8),
                        1 => f[at] = rnd() as u8,
                        2 if at + 4 <= f.len() => f[at..at + 4].copy_from_slice(&[[0xFF; 4], [0; 4], [0, 0, 0xFF, 0xFF], [0, 0, 0, 1]][rnd() as usize % 4]),
                        3 => f.truncate(at.max(8)),
                        4 => {
                            let n = (rnd() % 16) as usize;
                            f.drain(at..(at + n).min(f.len()));
                        }
                        _ => {}
                    }
                    if f.len() < 16 {
                        break;
                    }
                }
                let _ = crate::imageio_common::heif_colour(&f);
                std::fs::write(&file, &f).unwrap();
                let _ = size(&file);
                let _ = applies_orientation(&file);
                let _ = thumbnail(&file);
                let _ = linear(&file);
                d.store(i + 1, Ordering::SeqCst);
            }
        });
        let mut last = (0, std::time::Instant::now());
        while !worker.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let n = done.load(Ordering::SeqCst);
            if n != last.0 {
                last = (n, std::time::Instant::now());
            }
            assert!(last.1.elapsed().as_secs() < 20, "case {n} runs longer than 20 s");
        }
        let r = worker.join();
        let _ = std::fs::remove_dir_all(&dir);
        r.expect("no panic");
        assert_eq!(done.load(Ordering::SeqCst), cases);
    }

    /// Timing of a real photo (`GRAINEO_HEIC_BENCH=<file>`): full decode, embedded thumbnail and the app's thumbnail path.
    #[test]
    fn bench_photo() {
        let Some(p) = std::env::var_os("GRAINEO_HEIC_BENCH").map(PathBuf::from) else { return };
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let (w, h, _) = linear(&p).unwrap();
            eprintln!("{w}×{h} linear: {:?}", t.elapsed());
            let t = std::time::Instant::now();
            let r = thumbnail(&p).map(|r| (r.0, r.1));
            eprintln!("embedded thumbnail {r:?}: {:?}", t.elapsed());
            #[cfg(not(target_os = "macos"))]
            {
                let t = std::time::Instant::now();
                let r = crate::imageio::thumb(&p, 256, false).map(|r| (r.0, r.1));
                eprintln!("imageio::thumb 256: {r:?} {:?}", t.elapsed());
            }
        }
    }

    /// The whole path of the app outside macOS (imageio_fallback): size, orientation and pixels upright; the
    /// thumbnail path takes the embedded thumbnail only when allowed (`always == false`).
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn imageio_reads_heic_upright() {
        let dir = charts();
        // red patch: top left of the chart, top right once turned clockwise
        for (name, size, at) in [("rot90.heic", (400, 600), 300), ("p3-10bit.heic", (600, 400), 100)] {
            let p = dir.join(name);
            let e = crate::imageio::properties(&p).unwrap();
            assert_eq!((e.width, e.height, e.orientation.unwrap_or(1)), (Some(size.0), Some(size.1), 1), "{name}");
            let (w, h, px) = crate::imageio::image(&p, None).unwrap();
            assert_eq!((w, h), size, "{name}");
            let i = (100 * w as usize + at) * 3;
            eprintln!("{name}: {:?}", &px[i..i + 3]);
            assert!(px[i] > 230 && px[i + 1] < 40 && px[i + 2] < 40, "{name}: {:?}", &px[i..i + 3]);
        }
        let t = dir.join("thumb.heic");
        assert_eq!(crate::imageio::thumb(&t, 256, false).map(|r| (r.0, r.1)), Some((120, 80)));
        assert_eq!(crate::imageio::thumb(&t, 256, true).map(|r| (r.0, r.1)), Some((256, 171)));
    }
}
