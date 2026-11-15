//! Opt-in CPU reference comparison; production render/cache keys are untouched.
use super::*;
pub struct Prepared{pub pixels:Vec<[f32;3]>,pub w:usize,pub h:usize,pub analysis:raw::sharpen_study::Analysis}
pub fn prepare(doc:&RawDoc,s:&Settings)->Result<Prepared,String>{
 if doc.finished||doc.info.sensor!=raw::Sensor::Bayer{return Err("Study requires a Bayer RAW".into())}
 let b=doc.full_base()?;let mut px=b.px.clone();let (w,h)=(b.w,b.h);let key=doc.detail_key(s);
 if let Some(profile)=b.profile.as_ref(){noise::apply(&mut px,w,h,profile,&key.noise())}
 let analysis=raw::sharpen_study::analyze(&px,w,h)?;
 Ok(Prepared{pixels:px,w,h,analysis})
}
pub fn render(doc:&RawDoc,s:&Settings,prepared:&Prepared,method:&str)->Result<Rgb16,String>{
 let mut px=prepared.pixels.clone();let(w,h)=(prepared.w,prepared.h);
 let wb=doc.info.wb.unwrap_or([1.;3]);let clip=0.95*wb[1].min(wb[0]).min(wb[2]);
 raw::sharpen_study::apply(&mut px,w,h,clip,&prepared.analysis,method)?;
 let key=doc.detail_key(s);if key.lens.is_some(){if let(Some(k),_)=doc.correction(key){k.apply_scaled(&mut px,w,h)}}
 let st=Stage::new(px,w,h,None);Ok(doc.render_stage(&st,s))
}
