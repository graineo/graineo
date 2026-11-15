use graineo_color::{self as color,Space};
use develop::{Rgb16,Settings};
fn scratch(name:&str)->std::path::PathBuf{
 let root=std::env::var_os("GRAINEO_DATA_DIR").expect("isolated tests only");
 let dir=std::path::PathBuf::from(root).join(format!("color-{name}-{}",std::process::id()));std::fs::create_dir_all(&dir).unwrap();dir
}
#[test]
fn input_iccs_are_respected_by_native_loader_and_wide_export(){
 let dir=scratch("icc");
 for space in Space::ALL{
  let data:Vec<u16>=(0..32*24).flat_map(|i|match (i/8)%4{0=>[65535,0,0],1=>[0,65535,0],2=>[0,0,65535],_=>[32768;3]}).collect();
  let path=dir.join(format!("{space:?}.tif"));
  develop::export::color::write_tiff16(&Rgb16{width:32,height:24,data:data.clone()},&path,space).unwrap();
  let (_,_,px,_)=library::imageio::linear_full(&path).unwrap();
  let linear:Vec<f32>=data.iter().map(|&v|space.decode(v as f32/65535.)).collect();
  let expected=color::convert_linear(&linear,space,Space::Srgb).unwrap();
  let error=px.iter().flatten().zip(&expected).map(|(&a,&b)|(a-b).abs()).fold(0f32,f32::max);
  assert!(error<0.003,"{space:?} native ICC max error {error}: first {:?} expected {:?}", &px[..4], &expected[..12]);
  let doc=rawdev::RawDoc::open(&path,&||true).unwrap().unwrap();
  let mut s=Settings::default();s.raw.nr_color=0.;s.raw.nr_luma=0.;s.raw.lens=false;
  let wide=rawdev::color::render(&doc,&s,space).unwrap();
  let max=wide.data.iter().zip(&data).map(|(&a,&b)|a.abs_diff(b)).max().unwrap();
  let flat:Vec<f32>=px.iter().flatten().copied().collect();
  let native_out=color::encode_rgb16(&flat,space).unwrap();
  let pipeline=wide.data.iter().zip(&native_out).map(|(&a,&b)|a.abs_diff(b)).max().unwrap();
  // 1) Our path adds nothing: output = native conversion of the pixels read by ImageIO (rounding error at most 2).
  assert!(pipeline<=2,"{space:?}: own pipeline differs from the native pixels by {pipeline}, first output {:?} native {:?}",&wide.data[..12],&native_out[..12]);
  // 2) ImageIO's own conversion: gray and in-gamut colors are exact (≤ 16/65535); pure primaries at the edge of a
  //    color space larger than sRGB get crosstalk into the other channels from macOS when read (Display P3 ≈ 0.5 %,
  //    Adobe RGB ≈ 2.9 %, ProPhoto ≈ 1.4 % measured). That is a property of the system, not of our path; the limit guards against regressions.
  let gray=data.chunks(3).zip(wide.data.chunks(3)).filter(|(d,_)|d[0]==32768&&d[1]==32768&&d[2]==32768).flat_map(|(d,w)|d.iter().zip(w).map(|(&a,&b)|a.abs_diff(b))).max().unwrap();
  assert!(gray<=16,"{space:?} gray patch error {gray}");
  assert!(max<=2500,"{space:?} gamut-boundary primaries differ by {max} (ImageIO conversion), first output {:?}",&wide.data[..12]);
  if space==Space::DisplayP3 {
   let narrow=doc.render_full(&s).unwrap();
   let rewrapped=color::transcode_srgb16(&narrow.data,space).unwrap();
   assert_ne!(wide.data,rewrapped,"wide image must retain colors lost by the old sRGB output");
  }
 }
}
#[test]
fn srgb_export_is_byte_identical_to_original(){
 let dir=scratch("compat");let image=Rgb16{width:16,height:16,data:vec![23000;16*16*3]};
 let a=dir.join("old.tif");let b=dir.join("new.tif");
 develop::export::write_tiff16(&image,&a).unwrap();develop::export::color::write_tiff16(&image,&b,Space::Srgb).unwrap();assert_eq!(std::fs::read(a).unwrap(),std::fs::read(b).unwrap());
 let a=dir.join("old.jpg");let b=dir.join("new.jpg");
 develop::export::write_jpeg(&image,&a,92).unwrap();develop::export::color::write_jpeg(&image,&b,92,Space::Srgb).unwrap();assert_eq!(std::fs::read(a).unwrap(),std::fs::read(b).unwrap());
}

/// The float path for wide color spaces computes the same sliders as the existing sRGB path: for colors inside sRGB (also after the sliders)
/// both agree up to rounding (Display P3 → back to sRGB against `render_full`).
#[test]
fn wide_path_matches_srgb_path_for_in_gamut_colors(){
 let dir=scratch("parity");
 let data:Vec<u16>=(0..96*64).flat_map(|i|{let (x,y)=(i%96,i/96);[(30000+x*120+y*30) as u16,(34000+y*140) as u16,(41000-x*90) as u16]}).collect();
 let path=dir.join("quelle.tif");
 develop::export::write_tiff16(&Rgb16{width:96,height:64,data},&path).unwrap();
 let doc=rawdev::RawDoc::open(&path,&||true).unwrap().unwrap();
 for (name,f) in [("neutral",(|_:&mut Settings|{}) as fn(&mut Settings)),("saettigung",|s|{s.saturation=0.35;s.vibrance=0.25}),("klarheit",|s|{s.clarity=0.5;s.sharpen=0.6}),("kontrast",|s|{s.contrast=0.3;s.fade=0.2;s.fill_light=0.3})]{
  let mut s=Settings::default();s.raw.nr_color=0.;s.raw.nr_luma=0.;s.raw.lens=false;f(&mut s);
  let narrow=doc.render_full(&s).unwrap();
  let wide=rawdev::color::render(&doc,&s,Space::DisplayP3).unwrap();
  let lin:Vec<f32>=wide.data.iter().map(|&v|Space::DisplayP3.decode(v as f32/65535.)).collect();
  let back=color::convert_linear(&lin,Space::DisplayP3,Space::Srgb).unwrap();
  let back16=color::encode_rgb16(&back,Space::Srgb).unwrap();
  // Near the sRGB boundary the old path clips, the wide one keeps the color (intended): hence the mean and the 99.5 % percentile,
  // not the largest single value.
  let mut dev:Vec<u16>=back16.iter().zip(&narrow.data).map(|(a,b)|a.abs_diff(*b)).collect();
  let mean=dev.iter().map(|&d|d as u64).sum::<u64>() as f64/dev.len() as f64;
  dev.sort_unstable();
  let (p995,max)=(dev[dev.len()*995/1000],*dev.last().unwrap());
  eprintln!("{name}: mean deviation {mean:.1}/65535, 99.5 % below {p995}, largest {max}");
  assert!(mean<2.0&&p995<8,"{name}: wide path differs from the sRGB path (mean {mean:.1}, p99.5 {p995}, max {max})");
 }
}

/// The wide preview and the wide crop (for display) agree with the sRGB path for colors inside sRGB;
/// computed in Display P3 the values stay valid (finite).
#[test]
fn wide_preview_and_region_match_srgb_path_for_in_gamut_colors(){
 let dir=scratch("parity-vorschau");
 let data:Vec<u16>=(0..96*64).flat_map(|i|{let (x,y)=(i%96,i/96);[(30000+x*120+y*30) as u16,(34000+y*140) as u16,(41000-x*90) as u16]}).collect();
 let path=dir.join("quelle.tif");
 develop::export::write_tiff16(&Rgb16{width:96,height:64,data},&path).unwrap();
 let doc=rawdev::RawDoc::open(&path,&||true).unwrap().unwrap();
 let mut s=Settings::default();s.raw.nr_color=0.;s.raw.nr_luma=0.;s.raw.lens=false;s.saturation=0.2;s.contrast=0.2;
 let compare=|name:&str,wide_img:rawdev::color::Wide,narrow_img:&Rgb16|{
  assert_eq!((wide_img.width,wide_img.height),(narrow_img.width,narrow_img.height),"{name}: size");
  let p3=color::convert_linear(&wide_img.rgb,Space::DisplayP3,Space::Srgb).unwrap();
  let back=color::encode_rgb16(&p3,Space::Srgb).unwrap();
  let mut dev:Vec<u16>=back.iter().zip(&narrow_img.data).map(|(a,b)|a.abs_diff(*b)).collect();
  let mean=dev.iter().map(|&d|d as u64).sum::<u64>() as f64/dev.len() as f64;
  dev.sort_unstable();
  let p995=dev[dev.len()*995/1000];
  eprintln!("{name}: mean deviation {mean:.1}/65535, 99.5 % below {p995}");
  assert!(wide_img.rgb.iter().all(|v|v.is_finite()),"{name}: not finite");
  assert!(mean<2.0&&p995<8,"{name}: mean {mean:.1}, p99.5 {p995}");
 };
 let wide_img=rawdev::color::render_preview_wide(&doc,&s,Space::DisplayP3).unwrap().expect("wide path");
 compare("Vorschau",wide_img,&doc.render_preview(&s));
 let region=[10,8,50,40];
 let wide_img=rawdev::color::render_region_wide(&doc,&s,region,Space::DisplayP3).unwrap().expect("wide path");
 compare("Ausschnitt",wide_img,&doc.render_region(&s,region).unwrap());
 // film is fixed to sRGB: no wide path
 let mut f=s.clone();f.film.active=true;
 assert!(rawdev::color::render_preview_wide(&doc,&f,Space::DisplayP3).unwrap().is_none());
}
