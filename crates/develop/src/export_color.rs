//! Profiled JPEG/TIFF writers. Samples must already be encoded in the declared space.
//! Standard sRGB delegates to the old writers byte for byte.
use super::*;
use crate::Session;
use graineo_color::Space;
pub fn write_jpeg(img: &Rgb16, path: &Path, quality: u8, space: Space) -> std::io::Result<()> {
    if space==Space::Srgb{return super::write_jpeg(img,path,quality)}
    // JPEG stores dimensions in 16 bits. A cast would truncate a panorama e.g.
    // from 65,537 to 1 pixel; reject before opening the target file.
    let dimension = |n| u16::try_from(n).ok().filter(|&n| n > 0);
    let (Some(width), Some(height)) = (dimension(img.width), dimension(img.height)) else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "JPEG unterstützt Bildmaße von 1 bis 65.535 Pixeln; für größere Bilder TIFF wählen"));
    };
    let to_io = |e: jpeg_encoder::EncodingError| std::io::Error::other(e.to_string());
    let icc = space.icc().map_err(std::io::Error::other)?;
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = jpeg_encoder::Encoder::new(&mut file, quality);
    enc.add_icc_profile(&icc).map_err(to_io)?;
    enc.encode(&img.to_rgb8(), width, height, jpeg_encoder::ColorType::Rgb).map_err(to_io)?;
    super::finish_file(file)
}

/// 16-bit TIFF (sRGB, with embedded ICC profile if available).
pub fn write_tiff16(img: &Rgb16, path: &Path, space: Space) -> std::io::Result<()> {
    if space==Space::Srgb{return super::write_tiff16(img,path)}
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
    { let icc=space.icc().map_err(std::io::Error::other)?;
        tags.push((34675, Undefined(icc)));
    }
    let bytes: Vec<u8> = img.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    tiffw::write(std::io::BufWriter::new(std::fs::File::create(path)?), tags, &bytes)
}


/// Export in an RGB output colour space. sRGB takes the existing path unchanged; Display P3, Adobe RGB
/// and ProPhoto are rendered directly into the target space (extended sRGB values from the render stage,
/// converted linearly, clipped only at the end), so colours outside sRGB are kept. The film simulation
/// and Classic cameras (output defined in sRGB) are converted from the finished sRGB image.
pub fn export(session:&mut Session,s:&Settings,format:Format,engine:Engine,path:&Path,space:Space,opts:&Options)->std::io::Result<bool>{
 if space==Space::Srgb||!format.is_finished_image(){
  if !matches!(format,Format::Dng|Format::DngProfiles)||space==Space::Srgb{return super::export_with_opts(session,s,format,engine,path,opts)}
  return Err(std::io::Error::other("DNG retains its camera profile; RGB output profiles apply only to JPEG/TIFF/PNG"))
 }
 let spp=engine==Engine::Spp&&Session::uses_spp(s);
 let img=match session.render_full_wide(s,spp).map_err(std::io::Error::other)?{
  Some(wide)=>{
   use rayon::prelude::*;
   let linear:Vec<f32>=wide.data.par_iter().map(|&v|if v.is_finite(){graineo_color::srgb_decode(v)}else{0.0}).collect();
   let img=Rgb16{width:wide.width,height:wide.height,data:graineo_color::encode_rgb16(&linear,space).map_err(std::io::Error::other)?};
   // without film the border is neutral (paper/black); the profile does not change neutrals
   super::apply_output_size(crate::film::apply_border(crate::geometry::apply(img,s,session.cal.orientation),&s.film),s.output_size)
  }
  None=>{
   let img=if spp{session.render_full(s)}else{session.render_full_own(s)};
   let mut img=super::apply_output_size(crate::film::apply_border(crate::geometry::apply(img,s,session.cal.orientation),&s.film),s.output_size);
   img.data=graineo_color::transcode_srgb16(&img.data,space).map_err(std::io::Error::other)?;
   img
  }
 };
 super::write_finished(&img,path,format,space,opts)?;
 Ok(spp)
}
