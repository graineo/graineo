//! Export of a non-Sigma RAW: full resolution, crop/straighten, JPEG, JPEG XL, 16-bit TIFF or PNG
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
    let img = develop::export::finish_output(develop::film::apply_border(img, &s.film), s, graineo_color::Space::Srgb);
    match format {
        Format::Jpeg { quality } => develop::export::write_jpeg(&img, path, quality).map_err(|e| e.to_string()),
        Format::Tiff16 => develop::export::write_tiff16(&img, path).map_err(|e| e.to_string()),
        Format::Png16 | Format::Jxl { .. } => develop::export::write_finished(&img, path, format, graineo_color::Space::Srgb, &Default::default()).map_err(|e| e.to_string()),
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

/// [`export_color`] with export options (TIFF compression). Finished images (JPEG/TIFF/PNG) and, for RAWs, the
/// scene-linear formats (TIFF 32 bit, OpenEXR, linear DNG, see [`export_scene_opts`]).
pub fn export_color_opts(doc:&RawDoc,s:&Settings,format:Format,path:&Path,space:graineo_color::Space,opts:&develop::export::Options)->Result<(),String>{
 if format.is_scene_linear()||format==Format::Dng{return export_scene_opts(doc,s,format,path,opts)}
 if !format.is_finished_image(){return Err("Dieses Exportformat gibt es nur für Sigma-X3F (hier JPEG, TIFF oder PNG)".into())}
 let plain=format==Format::Png16||(format==Format::Tiff16&&opts.tiff_compression!=develop::tiffw::Compression::None);
 if space==graineo_color::Space::Srgb&&!plain{return export(doc,s,format,path)}
 let img=render_color(doc,s,space)?;
 develop::export::write_finished(&img,path,format,space,opts).map_err(|e|e.to_string())
}

/// Is this a format only the scene-linear path ([`export_scene_opts`]) writes?
pub fn is_scene_format(format: Format) -> bool {
    format.is_scene_linear() || format == Format::Dng
}

/// Scene-linear export of a RAW (also a merged photo, whose highlights above the reference white stay):
/// - TIFF 32 bit float and OpenEXR (half/float): the developed image without tone curve or look, crop and rotation
///   applied, unclipped, in the linear working space of `opts` (as for Sigma, see `develop::export::export_scene`);
/// - linear DNG (`Format::Dng`, 32-bit float with `opts.dng_float`): the camera values after the stage (noise
///   reduction, lens correction) without white balance, uncropped in sensor orientation, with the camera matrices and
///   the as-shot neutral, so other programs and graineo itself can develop it again.
pub fn export_scene_opts(doc: &RawDoc, s: &Settings, format: Format, path: &Path, opts: &develop::export::Options) -> Result<(), String> {
    use rayon::prelude::*;
    crate::film_profiles::check(s)?;
    if doc.finished {
        return Err("Gleitkomma- und lineare Formate gibt es nur für RAW-Fotos".into());
    }
    let eff = doc.effective_settings(s);
    let s = &*eff;
    if format == Format::Dng {
        return export_camera_dng(doc, s, path, opts);
    }
    let st = doc.full_stage(s)?;
    let (w, h) = (st.w, st.h);
    let params = crate::render::Params::new(doc, s);
    let mask = st.mask(doc.lum());
    let prepared = crate::render::prepare(doc, &st.px, w, h, mask, s, doc.haze_for(s, w, h, [0, 0, w, h]).as_ref(), doc.toneq_for(s, w, h, [0, 0, w, h]).as_ref(), crate::render::brake_active(s).then(|| st.brake_base().to_vec()));
    let px: &[[f32; 3]] = prepared.as_deref().unwrap_or(&st.px);
    let scene = crate::render::scene_linear(&params, px, w, h, mask);
    let mut scene = develop::geometry::apply_f32(scene, s, doc.orientation);
    scene.data.par_iter_mut().for_each(|v| {
        if !v.is_finite() {
            *v = 0.0
        }
    });
    scene.data = graineo_color::convert_to_linear_space(&scene.data, opts.linear_space)?;
    match format {
        Format::TiffFloat => develop::export::write_tiff_float(&scene, path, opts.linear_space.icc().ok(), opts.tiff_compression).map_err(|e| e.to_string()),
        Format::Exr { half } => {
            let mut attrs = vec![("software", develop::export::SOFTWARE.to_string()), ("comments", format!("{} · scene-linear, {}", doc.info.model, opts.linear_space.name()))];
            if let Some(d) = doc.info.exif.capture_date.as_deref().and_then(library::export_meta::normalize_date) {
                attrs.push(("capDate", d));
            }
            let sample = if half { develop::exr::Sample::Half } else { develop::exr::Sample::Float };
            let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
            develop::exr::write(file, scene.width, scene.height, &scene.data, sample, opts.linear_space.chromaticities(), &attrs).map_err(|e| e.to_string())
        }
        _ => Err("kein Gleitkommaformat".into()),
    }
}

/// EXIF LightSource code of an illuminant temperature.
fn illuminant_code(kelvin: f32) -> u16 {
    const CODES: [(f32, u16); 5] = [(2856.0, 17), (4874.0, 18), (5003.0, 23), (5503.0, 20), (6504.0, 21)];
    CODES.iter().min_by(|a, b| (a.0 - kelvin).abs().total_cmp(&(b.0 - kelvin).abs())).map_or(21, |c| c.1)
}

/// Camera matrices of the decoder as the colour description of a DNG: up to two illuminants (warmest first), else
/// the single matrix of the file.
fn camera_color(info: &raw::Info) -> Option<develop::dng::CameraColor> {
    let flat = |m: &[[f32; 3]; 3]| -> [f64; 9] { std::array::from_fn(|i| m[i / 3][i % 3] as f64) };
    let mut cals: Vec<&raw::color::Calibration> = info.calibration.iter().collect();
    cals.sort_by(|a, b| a.kelvin.total_cmp(&b.kelvin));
    let mut matrices: Vec<([f64; 9], u16)> = Vec::new();
    if let (Some(first), Some(last)) = (cals.first(), cals.last()) {
        matrices.push((flat(&first.color), illuminant_code(first.kelvin)));
        if cals.len() > 1 && (last.kelvin - first.kelvin).abs() > 500.0 {
            matrices.push((flat(&last.color), illuminant_code(last.kelvin)));
        }
    } else if let Some((ill, m)) = &info.matrix {
        let code = match ill.to_ascii_uppercase().as_str() {
            "A" => 17,
            "D50" => 23,
            "D55" => 20,
            _ => 21,
        };
        matrices.push((flat(m), code));
    }
    let wb = info.wb.unwrap_or([1.0; 3]);
    let neutral = [1.0 / wb[0].max(1e-6) as f64, 1.0 / wb[1].max(1e-6) as f64, 1.0 / wb[2].max(1e-6) as f64];
    let g = neutral[1];
    (!matrices.is_empty()).then(|| develop::dng::CameraColor { matrices, neutral: neutral.map(|v| v / g), baseline_exposure: 0.0 })
}

fn export_camera_dng(doc: &RawDoc, s: &Settings, path: &Path, opts: &develop::export::Options) -> Result<(), String> {
    use rayon::prelude::*;
    let color = camera_color(&doc.info).ok_or("Dieses Foto hat keine Kameramatrix für ein DNG")?;
    let st = doc.full_stage(s)?;
    let (w, h) = (st.w, st.h);
    let wb = doc.info.wb.unwrap_or([1.0; 3]);
    // the stage is white-balanced as shot: back to camera values, the balance is `neutral`
    let mut data = vec![0f32; w * h * 3];
    data.par_chunks_mut(3).zip(st.px.par_iter()).for_each(|(o, p)| {
        for c in 0..3 {
            let v = p[c] / wb[c].max(1e-6);
            o[c] = if v.is_finite() { v } else { 0.0 };
        }
    });
    let i = &doc.info;
    let mut shot = develop::dng::ShotInfo {
        model: i.model.clone(),
        maker: Some(i.maker.clone()).filter(|m| !m.trim().is_empty()),
        orientation: Some([1u16, 6, 3, 8][(doc.orientation % 4) as usize]),
        date: i.exif.capture_date.as_deref().and_then(library::export_meta::normalize_date),
        exposure_time: i.exif.exposure_time.map(f64::from).filter(|v| *v > 0.0),
        f_number: i.exif.aperture.map(f64::from).filter(|v| *v > 0.0),
        iso: i.exif.iso.map(|v| v as f64).filter(|v| *v > 0.0),
        focal_length: i.exif.focal_length.map(f64::from).filter(|v| *v > 0.0),
        ..Default::default()
    };
    shot.lens_model = i.exif.lens.clone();
    let options = develop::dng::DngOptions { float: opts.dng_float, embed_original: None };
    let meta = develop::dng::DngMeta { shot: &shot, xmp: None, preview: None, options: &options };
    develop::dng::write_camera_dng(&data, w, h, &color, format!("{} {}", i.maker.trim(), i.model.trim()).trim().to_string(), meta, path).map_err(|e| e.to_string())
}

/// The developed image in `space` (crop, rotation, border and output size of `s` applied) – the rendering of
/// [`export_color_opts`] for RGB spaces, shared with printing.
pub fn render_color(doc:&RawDoc,s:&Settings,space:graineo_color::Space)->Result<develop::Rgb16,String>{
 // Film: print and border are defined in sRGB (paper white can be tinted). First compute the complete sRGB image with border,
 // then convert to the target space; otherwise a tinted border would get wrong colors.
 let film=doc.effective_settings(s).film.is_active();
 let img=if film{
  let img=doc.render_full(s)?;
  let img=develop::geometry::apply(img,s,doc.orientation);
  let mut img=develop::export::finish_output(develop::film::apply_border(img,&s.film),s,graineo_color::Space::Srgb);
  img.data=graineo_color::transcode_srgb16(&img.data,space)?;
  img
 }else{
  let img=crate::color::render(doc,s,space)?;
  let img=develop::geometry::apply(img,s,doc.orientation);
  // Without film the border is neutral (paper/black); the profile does not change neutrals.
  develop::export::finish_output(develop::film::apply_border(img,&s.film),s,space)
 };
 Ok(img)
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

#[cfg(test)]
mod scene_tests {
    use super::*;

    /// A linear float DNG with values above 1 (the headroom of a merged photo) is read back by the RAW decoder with
    /// the same values, the as-shot neutral and the matrix.
    #[test]
    fn camera_dng_round_trip_keeps_headroom() {
        let (w, h) = (16usize, 8usize);
        let data: Vec<f32> = (0..w * h * 3).map(|i| 0.02 + (i % 48) as f32 * 0.07).collect();
        let peak = data.iter().copied().fold(0f32, f32::max);
        assert!(peak > 3.0);
        let color = develop::dng::CameraColor { matrices: vec![([0.9, 0.1, 0.0, 0.1, 0.8, 0.1, 0.0, 0.2, 0.7], 21)], neutral: [0.5, 1.0, 0.7], baseline_exposure: 0.0 };
        let shot = develop::dng::ShotInfo { model: "Test Cam".into(), maker: Some("TestMaker".into()), orientation: Some(6), ..Default::default() };
        let options = develop::dng::DngOptions { float: true, embed_original: None };
        let meta = develop::dng::DngMeta { shot: &shot, xmp: None, preview: None, options: &options };
        let dir = std::env::temp_dir().join(format!("scene-dng-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.dng");
        develop::dng::write_camera_dng(&data, w, h, &color, "TestMaker Test Cam".into(), meta, &path).unwrap();
        let f = raw::RawFile::open(&path).unwrap();
        assert_eq!(f.info.sensor, raw::Sensor::Linear);
        assert_eq!((f.info.width, f.info.height), (w, h));
        let wb = f.info.wb.expect("as-shot white balance");
        assert!((wb[0] - 2.0).abs() < 0.01 && (wb[2] - 1.0 / 0.7).abs() < 0.01, "{wb:?}");
        let lin = f.linear().unwrap();
        let back = &lin.px;
        let worst = data.iter().zip(back.iter().flat_map(|p| p.iter())).map(|(a, b)| ((a - b) / a).abs()).fold(0f32, f32::max);
        assert!(worst < 1e-5, "worst relative deviation {worst}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
