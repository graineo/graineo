//! Output as JPEG, JPEG XL, 16-bit TIFF, PNG or linear DNG.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{dng, srgb_icc, tiffw, Rgb16, Session, Settings};

/// Program name in TIFF/EXIF/DNG (`Software` tag); the reference checks compare image data only.
pub const SOFTWARE: &str = concat!("graineo ", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Jpeg { quality: u8 },
    Tiff16,
    /// Own linear DNG (our noise reduction, without profiles)
    Dng,
    /// DNG from x3fuse-core with embedded Sigma colour profiles, compressed
    DngProfiles,
    /// 16-bit PNG (finished image, like TIFF 16 bit)
    Png16,
    /// 32-bit floating-point TIFF, scene-linear and unclipped (see [`Options::linear_space`])
    TiffFloat,
    /// OpenEXR, scene-linear and unclipped, half or 32-bit float, ZIP compression
    Exr { half: bool },
    /// The unprocessed sensor layers as 16-bit TIFF (X3F only)
    RawLayers,
    /// JPEG XL: lossy with `quality` (1–100) or lossless with 8 or 16 bit (`bits16` applies to lossless only)
    Jxl { quality: u8, lossless: bool, bits16: bool },
}

/// Additional export options (defaults = previous behaviour).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    /// Own linear DNG with 32-bit floating-point samples (DNG 1.4)
    pub dng_float: bool,
    /// Embed the original X3F into the own linear DNG (OriginalRawFileData)
    pub dng_embed_original: bool,
    /// Lossless compression of TIFF files (16 bit, 32 bit, sensor layers)
    pub tiff_compression: tiffw::Compression,
    /// Working space of the scene-linear formats (TIFF 32 bit, OpenEXR)
    pub linear_space: graineo_color::LinearSpace,
    /// Which metadata go into exported JPEG/TIFF/PNG files
    pub metadata: Metadata,
    /// Take over the location (GPS) of the original, if it has one
    pub gps: bool,
}

/// How much metadata an exported JPEG/TIFF/PNG carries. Never: serial numbers, names of people.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Metadata {
    /// Pixels and colour profile only
    None,
    /// Camera, lens, exposure, focal length, capture date, software, copyright from the import
    #[default]
    Shooting,
    /// Shooting data plus keywords, description, stars and label from the library
    All,
}

impl Metadata {
    /// Name for command lines and settings: "none", "shooting", "all".
    pub fn parse(s: &str) -> Option<Metadata> {
        match s {
            "none" => Some(Metadata::None),
            "shooting" => Some(Metadata::Shooting),
            "all" => Some(Metadata::All),
            _ => None,
        }
    }
}

/// Which pipeline renders JPEG/TIFF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    /// Own pipeline: all sliders, monochrome, Fill Light
    Own,
    /// Sigma's exact base (preprocessing + noise reduction via x3fuse-core) with
    /// our output stage: all sliders, colour and monochrome; with changed noise sliders
    /// our own pipeline
    Spp,
}

impl Format {
    pub fn extension(&self) -> &'static str {
        match self {
            Format::Jpeg { .. } => "jpg",
            Format::Tiff16 | Format::TiffFloat | Format::RawLayers => "tif",
            Format::Dng | Format::DngProfiles => "dng",
            Format::Png16 => "png",
            Format::Exr { .. } => "exr",
            Format::Jxl { .. } => "jxl",
        }
    }

    /// Addition to the file name that keeps formats with the same extension apart.
    pub fn name_suffix(&self) -> &'static str {
        match self {
            Format::TiffFloat => "_linear",
            Format::RawLayers => "_layers",
            _ => "",
        }
    }

    /// Finished (display-referred) image in an RGB output colour space: JPEG, JPEG XL, TIFF 16 bit, PNG.
    pub fn is_finished_image(&self) -> bool {
        matches!(self, Format::Jpeg { .. } | Format::Jxl { .. } | Format::Tiff16 | Format::Png16)
    }

    /// JPEG XL with the defaults of the export dialog (lossy, quality 92).
    pub const JXL_DEFAULT: Format = Format::Jxl { quality: 92, lossless: false, bits16: true };

    /// Scene-linear floating-point image (TIFF 32 bit, OpenEXR).
    pub fn is_scene_linear(&self) -> bool {
        matches!(self, Format::TiffFloat | Format::Exr { .. })
    }

    /// Format from the file extension (JPEG with quality 92).
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Format::Jpeg { quality: 92 }),
            "tif" | "tiff" => Some(Format::Tiff16),
            "dng" => Some(Format::Dng),
            "png" => Some(Format::Png16),
            "exr" => Some(Format::Exr { half: true }),
            "jxl" => Some(Format::JXL_DEFAULT),
            _ => None,
        }
    }
}

pub fn write_jpeg(img: &Rgb16, path: &Path, quality: u8) -> std::io::Result<()> {
    // JPEG stores dimensions in 16 bits. A cast would truncate a panorama e.g.
    // from 65,537 to 1 pixel; reject before opening the target file.
    let dimension = |n| u16::try_from(n).ok().filter(|&n| n > 0);
    let (Some(width), Some(height)) = (dimension(img.width), dimension(img.height)) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "JPEG unterstützt Bildmaße von 1 bis 65.535 Pixeln; für größere Bilder TIFF wählen"));
    };
    let to_io = |e: jpeg_encoder::EncodingError| std::io::Error::other(e.to_string());
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = jpeg_encoder::Encoder::new(&mut file, quality);
    if let Some(icc) = srgb_icc() {
        enc.add_icc_profile(&icc).map_err(to_io)?;
    }
    enc.encode(&img.to_rgb8(), width, height, jpeg_encoder::ColorType::Rgb).map_err(to_io)?;
    finish_file(file)
}

/// Write out the buffer and report errors (full disk, network drive gone): dropping a `BufWriter` swallows the
/// error of its last write, and the export would report a truncated JPEG as saved.
pub(crate) fn finish_file(mut w: std::io::BufWriter<std::fs::File>) -> std::io::Result<()> {
    use std::io::Write;
    w.flush()?;
    w.into_inner().map_err(|e| e.into_error())?.sync_all()
}

/// 16-bit TIFF (sRGB, with embedded ICC profile if available).
pub fn write_tiff16(img: &Rgb16, path: &Path) -> std::io::Result<()> {
    use tiffw::Value::*;
    let mut tags = vec![
        (256, Long(vec![img.width as u32])),
        (257, Long(vec![img.height as u32])),
        (258, Short(vec![16, 16, 16])),
        (259, Short(vec![1])),
        (262, Short(vec![2])),
        (274, Short(vec![1])),
        (277, Short(vec![3])),
        (278, Long(vec![img.height as u32])),
        (284, Short(vec![1])),
        (305, Ascii(SOFTWARE.into())),
    ];
    if let Some(icc) = srgb_icc() {
        tags.push((34675, Undefined(icc)));
    }
    let bytes: Vec<u8> = img.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    tiffw::write(std::io::BufWriter::new(std::fs::File::create(path)?), tags, &bytes)
}

/// 16-bit TIFF with ICC profile and optional lossless compression (none = [`write_tiff16`] layout).
pub fn write_tiff16_with(img: &Rgb16, path: &Path, icc: Option<Vec<u8>>, c: tiffw::Compression) -> std::io::Result<()> {
    use tiffw::Value::*;
    let bytes: Vec<u8> = img.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (strips, mut tags) = tiffw::strips(&bytes, img.width, 3, tiffw::Samples::U16, c);
    tags.extend([
        (256, Long(vec![img.width as u32])),
        (257, Long(vec![img.height as u32])),
        (258, Short(vec![16, 16, 16])),
        (262, Short(vec![2])),
        (274, Short(vec![1])),
        (277, Short(vec![3])),
        (284, Short(vec![1])),
        (305, Ascii(SOFTWARE.into())),
    ]);
    if let Some(icc) = icc {
        tags.push((34675, Undefined(icc)));
    }
    tiffw::write_ifd(std::io::BufWriter::new(std::fs::File::create(path)?), tiffw::Ifd { tags, strips, ..Default::default() })
}

/// 32-bit floating-point TIFF (RGB, linear ICC profile, optional compression with floating-point predictor).
pub fn write_tiff_float(img: &crate::Linear, path: &Path, icc: Option<Vec<u8>>, c: tiffw::Compression) -> std::io::Result<()> {
    use rayon::prelude::*;
    use tiffw::Value::*;
    let bytes: Vec<u8> = img.data.par_iter().flat_map_iter(|v| v.to_le_bytes()).collect();
    let (strips, mut tags) = tiffw::strips(&bytes, img.width, 3, tiffw::Samples::F32, c);
    tags.extend([
        (256, Long(vec![img.width as u32])),
        (257, Long(vec![img.height as u32])),
        (258, Short(vec![32, 32, 32])),
        (262, Short(vec![2])),
        (274, Short(vec![1])),
        (277, Short(vec![3])),
        (284, Short(vec![1])),
        (305, Ascii(SOFTWARE.into())),
        (339, Short(vec![3, 3, 3])),
    ]);
    if let Some(icc) = icc {
        tags.push((34675, Undefined(icc)));
    }
    tiffw::write_ifd(std::io::BufWriter::new(std::fs::File::create(path)?), tiffw::Ifd { tags, strips, ..Default::default() })
}

/// Writes a finished 16-bit image in the encoding of `space` (TIFF with compression, PNG, JPEG).
pub fn write_finished(img: &Rgb16, path: &Path, format: Format, space: graineo_color::Space, opts: &Options) -> std::io::Result<()> {
    let icc = || if space == graineo_color::Space::Srgb { srgb_icc() } else { space.icc().ok() };
    match format {
        Format::Jpeg { quality } => color::write_jpeg(img, path, quality, space),
        Format::Tiff16 if opts.tiff_compression == tiffw::Compression::None => color::write_tiff16(img, path, space),
        Format::Tiff16 => write_tiff16_with(img, path, icc(), opts.tiff_compression),
        Format::Png16 => crate::png16::write(img, path, icc().as_deref().map(|p| (space.name(), p))),
        Format::Jxl { quality, lossless, bits16 } => crate::jxl::write(img, path, space, crate::jxl::Params { quality, lossless, bits16 }),
        _ => Err(std::io::Error::other("kein fertiges Bildformat")),
    }
}

/// Scene-linear floating-point export (TIFF 32 bit, OpenEXR): crop/straighten applied, no size
/// limit, no look stage, unclipped, in the linear working space of `opts`.
pub fn export_scene(session: &mut Session, s: &Settings, format: Format, spp: bool, path: &Path, opts: &Options) -> std::io::Result<()> {
    use rayon::prelude::*;
    let scene = session.render_full_scene(s, spp).map_err(std::io::Error::other)?;
    let mut scene = crate::geometry::apply_f32(scene, s, session.cal.orientation);
    scene.data.par_iter_mut().for_each(|v| if !v.is_finite() { *v = 0.0 });
    scene.data = graineo_color::convert_to_linear_space(&scene.data, opts.linear_space).map_err(std::io::Error::other)?;
    match format {
        Format::TiffFloat => write_tiff_float(&scene, path, opts.linear_space.icc().ok(), opts.tiff_compression),
        Format::Exr { half } => {
            let shot = dng::ShotInfo::from_props(&session.props, &session.cal.camera);
            let mut attrs = vec![("software", SOFTWARE.to_string()), ("comments", format!("{} · scene-linear, {}", shot.model, opts.linear_space.name()))];
            if let Some(d) = &shot.date {
                attrs.push(("capDate", d.clone()));
            }
            let sample = if half { crate::exr::Sample::Half } else { crate::exr::Sample::Float };
            let w = std::io::BufWriter::new(std::fs::File::create(path)?);
            crate::exr::write(w, scene.width, scene.height, &scene.data, sample, opts.linear_space.chromaticities(), &attrs.iter().map(|(k, v)| (*k, v.clone())).collect::<Vec<_>>())
        }
        _ => Err(std::io::Error::other("kein Gleitkommaformat")),
    }
}

/// Maximum output size on export. Never upscales.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum OutputSize {
    /// longer side at most this many pixels
    LongEdge(u32),
    /// shorter side at most this many pixels
    ShortEdge(u32),
    /// fit into a frame width × height (portrait and landscape: the frame rotates along)
    BoundingBox {
        width: u32,
        height: u32,
    },
    /// at most this many megapixels
    Megapixel(f32),
}

impl OutputSize {
    /// Read a stored value; the old variant names of graineo 1.0 are translated.
    pub fn from_json(mut v: serde_json::Value) -> Result<Self, serde_json::Error> {
        legacy_names::settings_keys::upgrade_develop(&mut v);
        serde_json::from_value(v)
    }

    /// Target size for an image `w` × `h` (aspect ratio stays, never larger than the image).
    pub fn target_size(&self, w: usize, h: usize) -> (usize, usize) {
        if w == 0 || h == 0 {
            return (w, h);
        }
        let (long, short) = (w.max(h) as f64, w.min(h) as f64);
        let k = match *self {
            OutputSize::LongEdge(n) => n as f64 / long,
            OutputSize::ShortEdge(n) => n as f64 / short,
            OutputSize::BoundingBox { width, height } => {
                // the frame adapts to the orientation of the image (long side of the frame = long side of the image)
                let (rl, rk) = (width.max(height) as f64, width.min(height) as f64);
                (rl / long).min(rk / short)
            }
            OutputSize::Megapixel(mp) => ((mp as f64 * 1e6) / (w as f64 * h as f64)).sqrt(),
        };
        if !(k > 0.0) || k >= 1.0 {
            return (w, h);
        }
        (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1))
    }
}

/// Downscale the image to the output size (Lanczos3, 16 bit, on the GPU when allowed – see [`crate::resize`]);
/// unchanged without a limit or if already small enough.
pub fn apply_output_size(img: Rgb16, g: Option<OutputSize>) -> Rgb16 {
    let Some(g) = g else { return img };
    let (w, h) = g.target_size(img.width, img.height);
    if (w, h) == (img.width, img.height) {
        return img;
    }
    if img.data.len() != img.width * img.height * 3 {
        return Rgb16 { width: 0, height: 0, data: Vec::new() };
    }
    crate::resize::lanczos3(&img, w, h)
}

/// Exports the image of the session. DNG contains the linear data (noise reduction
/// according to the settings, without tone curve); JPEG/TIFF the fully developed image.
/// Returns whether Sigma's exact base was used.
pub fn export_with(session: &mut Session, s: &Settings, format: Format, engine: Engine, path: &Path) -> std::io::Result<bool> {
    export_with_opts(session, s, format, engine, path, &Options::default())
}

/// [`export_with`] with additional options.
pub fn export_with_opts(session: &mut Session, s: &Settings, format: Format, engine: Engine, path: &Path, opts: &Options) -> std::io::Result<bool> {
    let spp = engine == Engine::Spp && Session::uses_spp(s);
    match format {
        Format::Tiff16 | Format::Png16 | Format::Jxl { .. } | Format::Jpeg { .. } if !spp => {
            // TIFF/PNG: true 16-bit gradations (tone curve interpolated, exact gamma)
            let img = if matches!(format, Format::Jpeg { .. }) { session.render_full_own(s) } else { session.render_full_own_fine(s) };
            let img = crate::geometry::apply(img, s, session.cal.orientation);
            let img = apply_output_size(crate::film::apply_border(img, &s.film), s.output_size);
            match format {
                Format::Tiff16 if opts.tiff_compression == tiffw::Compression::None => write_tiff16(&img, path)?,
                Format::Jpeg { quality } => write_jpeg(&img, path, quality)?,
                _ => write_finished(&img, path, format, graineo_color::Space::Srgb, opts)?,
            }
            Ok(false)
        }
        Format::TiffFloat | Format::Exr { .. } => {
            export_scene(session, s, format, spp, path, opts)?;
            Ok(spp)
        }
        Format::RawLayers => {
            crate::rawlayers::write(&session.path, &session.cal, path, opts.tiff_compression)?;
            Ok(false)
        }
        _ => {
            export_opts(session, s, format, path, opts)?;
            Ok(spp || (format == Format::DngProfiles && !session.cal.is_classic()))
        }
    }
}

pub fn export(session: &mut Session, s: &Settings, format: Format, path: &Path) -> std::io::Result<()> {
    export_opts(session, s, format, path, &Options::default())
}

/// Shot data and preview for the own DNG.
fn dng_parts(session: &mut Session, s: &Settings) -> (dng::ShotInfo, Option<tiffw::Ifd>) {
    let shot = dng::ShotInfo::from_props(&session.props, &session.cal.camera);
    let (small, _) = session.preview_linear(1024);
    let mut t = s.effective().into_owned();
    t.fill_light = 0.0;
    t.view = crate::View::Developed;
    let img = crate::render(&small, &session.cal, &t, None);
    (shot, dng::preview_ifd(&img))
}

pub fn export_opts(session: &mut Session, s: &Settings, format: Format, path: &Path, opts: &Options) -> std::io::Result<()> {
    let dng_options = dng::DngOptions { float: opts.dng_float, embed_original: opts.dng_embed_original.then(|| session.path.clone()) };
    match format {
        // Black and white: true Foveon monochrome DNG with baked-in layer mix
        Format::DngProfiles | Format::Dng if s.monochrome => {
            let xmp = crate::xmp::lightroom_xmp(s, &session.cal, None, true);
            let (shot, preview) = dng_parts(session, s);
            let cal = session.cal.clone();
            let lin = session.linear(s);
            let meta = dng::DngMeta { shot: &shot, xmp: Some(&xmp), preview, options: &dng_options };
            dng::write_mono_dng(lin, &cal, s.normalized_mix(), meta, path)
        }
        // Classic: x3fuse-core's DNG normalises the highlights too high (magenta skies) →
        // linear DNG from our preprocessing
        Format::DngProfiles if session.cal.is_classic() => export_opts(session, s, Format::Dng, path, opts),
        Format::DngProfiles => {
            let profile = s.color_mode.clone().unwrap_or_else(|| session.cal.camera_mode.clone());
            let xmp = crate::xmp::lightroom_xmp(s, &session.cal, Some(&profile), false);
            crate::spp::write_dng(&session.path, path, s.color_mode.as_deref(), Some(xmp)).map_err(std::io::Error::other)
        }
        Format::Dng => {
            let xmp = crate::xmp::lightroom_xmp(s, &session.cal, None, false);
            let (shot, preview) = dng_parts(session, s);
            let cal = session.cal.clone();
            let lin = session.linear(s);
            let meta = dng::DngMeta { shot: &shot, xmp: Some(&xmp), preview, options: &dng_options };
            dng::write_linear_dng(lin, &cal, s.color_mode.as_deref(), meta, path)
        }
        // Crop/straighten only after development (DNGs stay uncropped)
        Format::Png16 | Format::Jxl { .. } | Format::Tiff16 if format != Format::Tiff16 || opts.tiff_compression != tiffw::Compression::None => {
            let img = apply_output_size(crate::film::apply_border(crate::geometry::apply(session.render_full(s), s, session.cal.orientation), &s.film), s.output_size);
            write_finished(&img, path, format, graineo_color::Space::Srgb, opts)
        }
        Format::TiffFloat | Format::Exr { .. } => export_scene(session, s, format, Session::uses_spp(s), path, opts),
        Format::RawLayers => crate::rawlayers::write(&session.path, &session.cal, path, opts.tiff_compression).map(|_| ()),
        Format::Png16 | Format::Jxl { .. } | Format::Tiff16 => write_tiff16(&apply_output_size(crate::film::apply_border(crate::geometry::apply(session.render_full(s), s, session.cal.orientation), &s.film), s.output_size), path),
        Format::Jpeg { quality } => write_jpeg(&apply_output_size(crate::film::apply_border(crate::geometry::apply(session.render_full(s), s, session.cal.orientation), &s.film), s.output_size), path, quality),
    }
}

#[cfg(test)]
mod output_size_tests {
    use super::*;
    #[test]
    fn size_keeps_aspect_and_never_upscales() {
        assert_eq!(OutputSize::LongEdge(2048).target_size(6000, 4000), (2048, 1365));
        assert_eq!(OutputSize::LongEdge(2048).target_size(4000, 6000), (1365, 2048), "Hochformat");
        assert_eq!(OutputSize::LongEdge(8000).target_size(6000, 4000), (6000, 4000), "never upscale");
        assert_eq!(OutputSize::ShortEdge(1080).target_size(6000, 4000), (1620, 1080));
        // Frame 1920 × 1080 rotates along for portrait
        assert_eq!(OutputSize::BoundingBox { width: 1920, height: 1080 }.target_size(6000, 4000), (1620, 1080));
        assert_eq!(OutputSize::BoundingBox { width: 1920, height: 1080 }.target_size(4000, 6000), (1080, 1620));
        let (w, h) = OutputSize::Megapixel(6.0).target_size(6000, 4000);
        assert!((w * h) as f64 <= 6.0e6 * 1.01 && (w * h) as f64 >= 5.9e6, "{w}×{h}");
    }
    #[test]
    fn downscale_yields_target_size() {
        let img = Rgb16 { width: 400, height: 200, data: vec![30000; 400 * 200 * 3] };
        let k = apply_output_size(img, Some(OutputSize::LongEdge(100)));
        assert_eq!((k.width, k.height, k.data.len()), (100, 50, 100 * 50 * 3));
        assert!(k.data.iter().all(|&v| (29000..=31000).contains(&v)));
        let unchanged = apply_output_size(Rgb16 { width: 40, height: 20, data: vec![1; 2400] }, Some(OutputSize::LongEdge(100)));
        assert_eq!((unchanged.width, unchanged.height), (40, 20));
    }
}

#[cfg(test)]
mod jpeg_size_tests {
    use super::*;
    #[test]
    fn jpeg_rejects_overflow_and_empty_images_before_creating_file() {
        let dir = std::env::temp_dir().join(format!("graineo-jpeg-audit-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let p = dir.join("keep.jpg"); std::fs::write(&p, b"existing-file").unwrap();
        for (width, height) in [(65536, 1), (65537, 1), (1, 65537), (0, 1), (1, 0)] {
            let img = Rgb16 { width, height, data: Vec::new() };
            assert_eq!(write_jpeg(&img, &p, 92).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
            assert_eq!(std::fs::read(&p).unwrap(), b"existing-file");
            let missing = dir.join("missing.jpg");
            assert!(write_jpeg(&img, &missing, 92).is_err());
            assert!(!missing.exists());
        }
        let img = Rgb16 { width: 2, height: 2, data: vec![20000; 12] };
        write_jpeg(&img, &dir.join("valid.jpg"), 92).unwrap();
        assert!(std::fs::read(dir.join("valid.jpg")).unwrap().starts_with(&[0xff, 0xd8]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The file on disk is the complete JPEG (written out and checked, not left to the `BufWriter` drop),
    /// byte for byte the same as an encoding in memory; also for the wide-gamut writer.
    #[test]
    fn jpeg_file_is_complete_and_unchanged() {
        let dir = std::env::temp_dir().join(format!("graineo-jpeg-flush-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = Rgb16 { width: 300, height: 200, data: (0..300 * 200 * 3).map(|i| (i * 37 % 65536) as u16).collect() };
        let p = dir.join("a.jpg");
        write_jpeg(&img, &p, 92).unwrap();
        let mut mem = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut mem, 92);
        if let Some(icc) = srgb_icc() {
            enc.add_icc_profile(&icc).unwrap();
        }
        enc.encode(&img.to_rgb8(), 300, 200, jpeg_encoder::ColorType::Rgb).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), mem);
        let q = dir.join("p3.jpg");
        color::write_jpeg(&img, &q, 92, graineo_color::Space::DisplayP3).unwrap();
        assert!(std::fs::read(&q).unwrap().ends_with(&[0xff, 0xd9]));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[path = "export_color.rs"]
pub mod color;
