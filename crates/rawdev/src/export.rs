//! Export of a non-Sigma RAW: full resolution, crop/straighten, JPEG or 16-bit TIFF
//! via the writers of [`develop::export`]. There is no DNG here (yet): the original is
//! already a RAW.

use std::path::Path;

use develop::export::Format;
use develop::geometry::Frame;
use develop::Settings;

use crate::RawDoc;

/// Develop and write the image. Returns: error text.
pub fn export(doc: &RawDoc, s: &Settings, format: Format, path: &Path) -> Result<(), String> {
    crate::film_profiles::check(s)?;
    let img = doc.render_full(s)?;
    let img = develop::geometry::apply(img, s, doc.orientation);
    // border "as print": only when set (otherwise unchanged)
    let img = develop::export::apply_output_size(develop::film::apply_border(img, &s.film), s.output_size);
    match format {
        Format::Jpeg { quality } => develop::export::write_jpeg(&img, path, quality).map_err(|e| e.to_string()),
        Format::Tiff16 => develop::export::write_tiff16(&img, path).map_err(|e| e.to_string()),
        Format::Png16 => develop::export::write_finished(&img, path, format, graineo_color::Space::Srgb, &Default::default()).map_err(|e| e.to_string()),
        _ => Err("Dieses Exportformat gibt es nur für Sigma-X3F (hier JPEG, TIFF oder PNG)".into()),
    }
}

/// Image frame with which [`export`] derives the image from the original (rotation, crop,
/// fine angle, perspective) – needed for the gain map.
pub fn export_frame(doc: &RawDoc, s: &Settings) -> Frame {
    Frame::from_settings(s, doc.full_size[0], doc.full_size[1], doc.orientation)
}

/// Attach the original's gain map to the exported JPEG `dest`, in the geometry of the image
/// (`f` from [`export_frame`]). The JPEG is re-encoded with `quality` in the process.
/// Ok(false): original without a gain map or map not convertible – nothing changed.
pub fn append_gain_map(f: &Frame, original: &Path, dest: &Path, quality: u8) -> Result<bool, String> {
    let map_to_src = |u: [f32; 2]| f.to_src(u);
    // unchanged image (no rotation, no crop): map 1:1; otherwise convert into the geometry of the image
    // output size of the finished file (smaller than the crop when exporting with a size limit)
    let output = library::imageio::properties(dest).and_then(|e| Some((e.width? as usize, e.height? as usize))).unwrap_or((f.w.round().max(1.0) as usize, f.h.round().max(1.0) as usize));
    let downscaled = output != (f.w.round().max(1.0) as usize, f.h.round().max(1.0) as usize);
    let map_px = (!f.is_identity() || downscaled).then(|| library::imageio::Mapping { source: (f.sw as usize, f.sh as usize), output: output, to_source: &map_to_src });
    library::imageio::append_gain_map(dest, original, map_px.as_ref(), quality as f32 / 100.0)
}

/// RGB output profile. sRGB remains the original export path; DNG is camera-profiled.
pub fn export_color(doc:&RawDoc,s:&Settings,format:Format,path:&Path,space:graineo_color::Space)->Result<(),String>{
 export_color_opts(doc,s,format,path,space,&Default::default())
}

/// [`export_color`] with export options (TIFF compression). Only finished images (JPEG/TIFF/PNG).
pub fn export_color_opts(doc:&RawDoc,s:&Settings,format:Format,path:&Path,space:graineo_color::Space,opts:&develop::export::Options)->Result<(),String>{
 if !format.is_finished_image(){return Err("Dieses Exportformat gibt es nur für Sigma-X3F (hier JPEG, TIFF oder PNG)".into())}
 let plain=format==Format::Png16||(format==Format::Tiff16&&opts.tiff_compression!=develop::tiffw::Compression::None);
 if space==graineo_color::Space::Srgb&&!plain{return export(doc,s,format,path)}
 // Film: print and border are defined in sRGB (paper white can be tinted). First compute the complete sRGB image with border,
 // then convert to the target space; otherwise a tinted border would get wrong colors.
 let film=doc.effective_settings(s).film.is_active();
 let img=if film{
  let img=doc.render_full(s)?;
  let img=develop::geometry::apply(img,s,doc.orientation);
  let mut img=develop::export::apply_output_size(develop::film::apply_border(img,&s.film),s.output_size);
  img.data=graineo_color::transcode_srgb16(&img.data,space)?;
  img
 }else{
  let img=crate::color::render(doc,s,space)?;
  let img=develop::geometry::apply(img,s,doc.orientation);
  // Without film the border is neutral (paper/black); the profile does not change neutrals.
  develop::export::apply_output_size(develop::film::apply_border(img,&s.film),s.output_size)
 };
 develop::export::write_finished(&img,path,format,space,opts).map_err(|e|e.to_string())
}

/// Shooting data of a non-Sigma RAW as the decoder reads it: written where the original's metadata
/// cannot be read by the system (without ImageIO) and as a fallback for fields ImageIO does not know.
/// Never the serial number.
pub fn shot(info: &raw::Info) -> library::export_meta::Shot {
    let e = &info.exif;
    let text = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    library::export_meta::Shot {
        make: text(&info.maker),
        model: text(&info.model),
        lens_make: e.lens_maker.as_deref().and_then(text),
        lens: e.lens.as_deref().and_then(text).or_else(|| e.lens_db.as_deref().and_then(text)),
        date: e.capture_date.as_deref().and_then(library::export_meta::normalize_date),
        exposure_time: e.exposure_time.map(f64::from).filter(|v| *v > 0.0),
        f_number: e.aperture.map(|v| (v as f64 * 100.0).round() / 100.0).filter(|v| *v > 0.0),
        iso: e.iso.filter(|v| *v > 0),
        focal_length: e.focal_length.map(|v| (v as f64 * 100.0).round() / 100.0).filter(|v| *v > 0.0),
        ..Default::default()
    }
}
