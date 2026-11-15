//! Wide-gamut export boundary. Standard sRGB calls the unchanged production renderer.
use graineo_color::{self as color, Space};
use develop::{Rgb16,Settings,look::Geo};
use rayon::prelude::*;
use crate::RawDoc;

pub fn render(doc:&RawDoc,s:&Settings,space:Space)->Result<Rgb16,String>{
 if space==Space::Srgb{return doc.render_full(s)}
 crate::film_profiles::check(s)?;
 let eff=doc.effective_settings(s);let s=&*eff;
 // The film's profile/scan output is an sRGB-defined rendering. Convert its actual colors;
 // do not mislabel its pixels or change spectral/film equations for an export setting.
 if s.film.is_active(){let mut img=doc.render_full(s)?;img.data=color::transcode_srgb16(&img.data,space)?;return Ok(img)}
 let st=doc.full_stage(s)?;let (w,h)=(st.w,st.h);
 let params=crate::render::Params::new(doc,s);let mask=st.mask(doc.lum());
 // dehaze / noise brake as in the sRGB path
 let prepared=crate::render::prepare(doc,&st.px,w,h,mask,s,doc.haze_for(s,w,h,[0,0,w,h]).as_ref(),doc.toneq_for(s,w,h,[0,0,w,h]).as_ref(),crate::render::brake_active(s).then(||st.brake_base().to_vec()));
 let px:&[[f32;3]]=prepared.as_deref().unwrap_or(&st.px);
 let mut data=vec![0f32;w*h*3];
 data.par_chunks_mut(3).enumerate().for_each(|(i,p)|p.copy_from_slice(&params.pixel_extended(px[i],mask[i])));
 let geo=Geo{full_w:doc.full_size[0],full_h:doc.full_size[1],off_x:0,off_y:0,scale:doc.full_size[0] as f32/w as f32,camera:doc.orientation};
 if s.raw.texture!=0.0{texture(&mut data,w,h,s.raw.texture,geo.scale)}
 develop::look::wide::apply(&mut data,w,h,s,geo)?;
 let linear:Vec<f32>=data.par_iter().map(|&v|color::srgb_decode(v)).collect();
 Ok(Rgb16{width:w,height:h,data:color::encode_rgb16(&linear,space)?})
}
fn texture(data:&mut[f32],w:usize,h:usize,amount:f32,scale:f32){
 if w<8||h<8{return}
 let l:Vec<f32>=data.par_chunks(3).map(|p|0.2126*p[0]+0.7152*p[1]+0.0722*p[2]).collect();
 let r=((2.5/scale.max(1.)).round() as usize).max(1);
 let b=crate::render::box_blur(&crate::render::box_blur(&l,w,h,r),w,h,r);
 let k=amount.clamp(-1.,1.)*if amount>0.{1.2}else{0.8};
 data.par_chunks_mut(3).enumerate().for_each(|(i,p)|{if l[i]>1e-5{let d=(l[i]-b[i]).clamp(-0.08,0.08);let f=((l[i]+k*d).max(0.)/l[i]).clamp(0.,3.);for v in p{*v*=f;}}});
}

/// Image for display on a wide-gamut screen: linear RGB values in the space `space` without
/// clipping (colors outside the space remain as values below 0 or above 1), in sensor orientation.
pub struct Wide{pub width:usize,pub height:usize,pub rgb:Vec<f32>}

/// Convert encoded, extended sRGB values (as delivered by the look stage) to linear values of `space`.
fn wide_from_encoded(data:Vec<f32>,w:usize,h:usize,space:Space)->Result<Wide,String>{
 let linear:Vec<f32>=data.par_iter().map(|&v|color::srgb_decode(v)).collect();
 Ok(Wide{width:w,height:h,rgb:color::convert_linear(&linear,Space::Srgb,space)?})
}

/// The preview (like [`RawDoc::render_preview`]) with preserved gamut. `None`: there is no
/// wide path for it (film simulation: it is fixed to sRGB) – then the sRGB display remains.
pub fn render_preview_wide(doc:&RawDoc,s:&Settings,space:Space)->Result<Option<Wide>,String>{
 match render_preview_encoded(doc,s)?{Some(e)=>wide_from_encoded(e.rgb,e.width,e.height,space).map(Some),None=>Ok(None)}
}

/// Like [`render_preview_wide`], but the values stay extended sRGB-encoded (as the look stage delivers them:
/// sRGB transfer, mirrored below 0) – for the display, which shows exactly these values.
pub fn render_preview_encoded(doc:&RawDoc,s:&Settings)->Result<Option<Wide>,String>{
 // the tone equalizer mask is shown on the sRGB path
 if s.film.is_active()||s.tone_eq_mask{return Ok(None)}
 let eff=doc.effective_settings(s);let s=&*eff;
 // noise brake only at full resolution (see RawDoc::render_stage)
 let nb;let s=if crate::render::brake_active(s){nb=Settings{noise_brake:develop::NoiseBrake::Off,..s.clone()};&nb}else{s};
 let st=doc.preview_stage(s);let (w,h)=(st.w,st.h);
 let params=crate::render::Params::new(doc,s);let mask=st.mask(doc.lum());
 // dehaze / noise brake as in the sRGB path
 let prepared=crate::render::prepare(doc,&st.px,w,h,mask,s,doc.haze_for(s,w,h,[0,0,w,h]).as_ref(),doc.toneq_for(s,w,h,[0,0,w,h]).as_ref(),crate::render::brake_active(s).then(||st.brake_base().to_vec()));
 let px:&[[f32;3]]=prepared.as_deref().unwrap_or(&st.px);
 let mut data=vec![0f32;w*h*3];
 data.par_chunks_mut(3).enumerate().for_each(|(i,p)|p.copy_from_slice(&params.pixel_extended(px[i],mask[i])));
 let geo=Geo{full_w:doc.full_size[0],full_h:doc.full_size[1],off_x:0,off_y:0,scale:doc.full_size[0] as f32/w as f32,camera:doc.orientation};
 if s.raw.texture!=0.0{texture(&mut data,w,h,s.raw.texture,geo.scale)}
 develop::look::wide::apply(&mut data,w,h,s,geo)?;
 Ok(Some(Wide{width:w,height:h,rgb:data}))
}

/// Full-resolution crop (like [`RawDoc::render_region`], x, y, w, h in image pixels) with preserved gamut.
/// `None`: no wide path (film, local adjustments).
pub fn render_region_wide(doc:&RawDoc,s:&Settings,region:[usize;4],space:Space)->Result<Option<Wide>,String>{
 match render_region_encoded(doc,s,region)?{Some(e)=>wide_from_encoded(e.rgb,e.width,e.height,space).map(Some),None=>Ok(None)}
}

/// Like [`render_region_wide`], values extended sRGB-encoded (see [`render_preview_encoded`]).
pub fn render_region_encoded(doc:&RawDoc,s:&Settings,region:[usize;4])->Result<Option<Wide>,String>{
 if s.film.is_active()||s.tone_eq_mask||develop::local::active(s){return Ok(None)}
 let st=doc.full_stage(s)?;
 let eff=doc.effective_settings(s);let s=&*eff;
 let [x,y,w,h]=region;
 let (x,y)=(x.min(st.w-1),y.min(st.h-1));
 let (w,h)=(w.min(st.w-x).max(1),h.min(st.h-y).max(1));
 // margin for texture and look
 let m=8;
 let (ex,ey)=(x.saturating_sub(m),y.saturating_sub(m));
 let (ew,eh)=((x+w+m).min(st.w)-ex,(y+h+m).min(st.h)-ey);
 let mask_full=st.mask(doc.lum());
 let params=crate::render::Params::new(doc,s);
 // crop with margin, then dehaze / noise brake as in the sRGB path (cropped dehaze analysis)
 let mut cpx=Vec::with_capacity(ew*eh);let mut cmk=Vec::with_capacity(ew*eh);
 for yy in ey..ey+eh{cpx.extend_from_slice(&st.px[yy*st.w+ex..yy*st.w+ex+ew]);cmk.extend_from_slice(&mask_full[yy*st.w+ex..yy*st.w+ex+ew]);}
 let haze=doc.haze_for(s,st.w,st.h,[ex,ey,ew,eh]);
 let toneq=doc.toneq_for(s,st.w,st.h,[ex,ey,ew,eh]);let prepared=crate::render::prepare(doc,&cpx,ew,eh,&cmk,s,haze.as_ref(),toneq.as_ref(),crate::render::brake_active(s).then(||st.brake_base_crop(ex,ey,ew,eh)));
 let cpx:&[[f32;3]]=prepared.as_deref().unwrap_or(&cpx);
 let mut data=vec![0f32;ew*eh*3];
 data.par_chunks_mut(ew*3).enumerate().for_each(|(yy,row)|{
  for (xx,o) in row.chunks_exact_mut(3).enumerate(){let i=yy*ew+xx;o.copy_from_slice(&params.pixel_extended(cpx[i],cmk[i]))}
 });
 let geo=Geo{full_w:st.w,full_h:st.h,off_x:ex,off_y:ey,scale:1.0,camera:doc.orientation};
 if s.raw.texture!=0.0{texture(&mut data,ew,eh,s.raw.texture,geo.scale)}
 develop::look::wide::apply(&mut data,ew,eh,s,geo)?;
 let mut out_px=Vec::with_capacity(w*h*3);
 for yy in y-ey..y-ey+h{out_px.extend_from_slice(&data[(yy*ew+(x-ex))*3..(yy*ew+(x-ex)+w)*3])}
 Ok(Some(Wide{width:w,height:h,rgb:out_px}))
}

/// The whole image at full resolution with preserved gamut (as [`render`] computes it for a wide-gamut export),
/// values extended sRGB-encoded (see [`render_preview_encoded`]). `None` for the film simulation.
pub fn render_full_encoded(doc:&RawDoc,s:&Settings)->Result<Option<Wide>,String>{
 crate::film_profiles::check(s)?;
 if s.film.is_active(){return Ok(None)}
 let eff=doc.effective_settings(s);let s=&*eff;
 let st=doc.full_stage(s)?;let (w,h)=(st.w,st.h);
 let params=crate::render::Params::new(doc,s);let mask=st.mask(doc.lum());
 let prepared=crate::render::prepare(doc,&st.px,w,h,mask,s,doc.haze_for(s,w,h,[0,0,w,h]).as_ref(),doc.toneq_for(s,w,h,[0,0,w,h]).as_ref(),crate::render::brake_active(s).then(||st.brake_base().to_vec()));
 let px:&[[f32;3]]=prepared.as_deref().unwrap_or(&st.px);
 let mut data=vec![0f32;w*h*3];
 data.par_chunks_mut(3).enumerate().for_each(|(i,p)|p.copy_from_slice(&params.pixel_extended(px[i],mask[i])));
 let geo=Geo{full_w:doc.full_size[0],full_h:doc.full_size[1],off_x:0,off_y:0,scale:doc.full_size[0] as f32/w as f32,camera:doc.orientation};
 if s.raw.texture!=0.0{texture(&mut data,w,h,s.raw.texture,geo.scale)}
 develop::look::wide::apply(&mut data,w,h,s,geo)?;
 Ok(Some(Wide{width:w,height:h,rgb:data}))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
 use super::*;

 /// A black & white camera look (here Fujifilm Monochrom + Ye-Filter) gives R = G = B on the display path
 /// (extended sRGB) as in the export – also for saturated and overexposed colors, which leave the sRGB cube.
 #[test]
 fn monochrome_look_is_gray_on_display_and_export(){
  let dir=std::env::temp_dir().join(format!("graineo-mono-look-{}",std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let path=dir.join("farben.tif");
  // stripes of saturated colors and a gray ramp
  let colors:[[u16;3];8]=[[0,65535,65535],[0,20000,65535],[65535,0,0],[0,65535,0],[65535,0,65535],[65535,65535,0],[5000,40000,52000],[65535,65535,65535]];
  let (w,h)=(64usize,48usize);
  let mut data=Vec::with_capacity(w*h*3);
  for y in 0..h{for x in 0..w{let c=colors[x*8/w];let k=(y as u32*65535/(h as u32-1)) as u16;data.extend(c.map(|v|((v as u32*k as u32)/65535) as u16));}}
  develop::export::write_tiff16(&Rgb16{width:w,height:h,data},&path).unwrap();
  let doc=RawDoc::open(&path,&||true).unwrap().unwrap();
  for (exposure,saturation) in [(0.0,0.0),(2.0,0.0),(1.0,0.8)]{
   let mut s=Settings::default();
   s.raw.look="x100/monochrome-ye".into();
   s.exposure=exposure;s.saturation=saturation;
   let e=render_preview_encoded(&doc,&s).unwrap().expect("display path");
   let spread=e.rgb.chunks_exact(3).map(|p|p[0].max(p[1]).max(p[2])-p[0].min(p[1]).min(p[2])).fold(0f32,f32::max);
   assert!(spread<2e-3,"display: R, G, B differ by up to {spread} (exposure {exposure}, saturation {saturation})");
   let x=doc.render_preview(&s);
   let spread=x.data.chunks_exact(3).map(|p|p[0].max(p[1]).max(p[2])-p[0].min(p[1]).min(p[2])).max().unwrap();
   assert!(spread<=1,"export: R, G, B differ by up to {spread} (exposure {exposure}, saturation {saturation})");
   // the colors really leave the sRGB cube without the look
   if exposure>1.5{s.raw.look="neutral".into();let n=render_preview_encoded(&doc,&s).unwrap().unwrap();assert!(n.rgb.iter().any(|v|*v>1.0||*v<0.0),"test colors stay inside sRGB");}
  }
  let _=std::fs::remove_dir_all(&dir);
 }
}
