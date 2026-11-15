//! Experimental comparison only. Never selected by production defaults.
//! Adaptive radius/noise-mask idea follows published capture-sharpening principles:
//! RawTherapee rtengine/capturesharpening.cc (GPL-3.0-or-later), darktable demosaic manual.
//! Original Rust implementation, no external code copied. A single estimated Gaussian PSF
//! is an approximation; compare real detail/noise/halos before any product integration.
use crate::sharpness::{self,Sharpness};
use rayon::prelude::*;
use serde::Serialize;
#[derive(Clone,Debug,Serialize)]
pub struct Analysis{pub sigma:f32,pub root_noise:f32,pub threshold:f32,pub edges:usize}
fn lum(p:&[f32;3])->f32{((p[0]+p[1]+p[2])/3.).max(1e-8)}
fn median(v:&mut[f32])->f32{if v.is_empty(){return 0.}let n=v.len()/2;v.select_nth_unstable_by(n,|a,b|a.total_cmp(b));v[n]}
pub fn analyze(px:&[[f32;3]],w:usize,h:usize)->Result<Analysis,String>{
 if w.checked_mul(h)!=Some(px.len())||w<5||h<5||!px.iter().flatten().all(|v|v.is_finite()){return Err("Invalid sharpening study image".into())}
 let root:Vec<f32>=px.par_iter().map(|p|lum(p).sqrt()).collect();
 let step=((w*h/150_000).max(1) as f32).sqrt().ceil() as usize;
 let mut samples=Vec::new();
 for y in (1..h-1).step_by(step*2){for x in (1..w-1).step_by(step*2){let i=y*w+x;let d=(root[i]-root[i+1]-root[i+w]+root[i+w+1])*0.5;samples.push(d.abs());}}
 let noise=median(&mut samples)*1.4826;
 let mut radii=Vec::new();
 let l:Vec<f32>=px.par_iter().map(lum).collect();
 for y in (2..h-2).step_by(step*2){for x in (2..w-2).step_by(step*2){let i=y*w+x;
  for d in [1,w]{let (a,b,c,e)=(l[i-d],l[i],l[i+d],l[i+2*d]);let g=c-b;let n0=b-a;let n1=e-c;
   if g.abs()<0.04 || g.abs()<12.*noise*root[i].max(0.1) || g.abs()<n0.abs() || g.abs()<n1.abs() || n0*g<0. || n1*g<0. {continue}
   let ratio=((n0+n1)/(2.*g)).clamp(0.02,0.95);let sigma=(-0.5/ratio.ln()).sqrt();
   if sigma.is_finite(){radii.push(sigma);}
  }
 }}
 let edges=radii.len();
 let sigma=if edges<32{0.7}else{let n=radii.len()/4;radii.select_nth_unstable_by(n,|a,b|a.total_cmp(b));radii[n].clamp(0.45,0.8)};
 Ok(Analysis{sigma,root_noise:noise,threshold:(noise*3.).max(0.012),edges})
}
pub fn apply(px:&mut[[f32;3]],w:usize,h:usize,clip:f32,analysis:&Analysis,method:&str)->Result<(),String>{
 if w.checked_mul(h)!=Some(px.len())||w<5||h<5||!analysis.sigma.is_finite()||!analysis.threshold.is_finite()||!analysis.root_noise.is_finite()||analysis.root_noise<0. {return Err("Invalid sharpening study parameters".into())}
 match method{
 "none"=>return Ok(()),
 "current"=>{sharpness::apply(px,w,h,&Sharpness::default(),clip);return Ok(())},
 "noiseAware"|"adaptive"=>{},_=>return Err("Unknown sharpening study variant".into())}
 let source=px.to_vec();
 let params=Sharpness{sigma:if method=="adaptive"{analysis.sigma}else{0.7},threshold:analysis.threshold,..Default::default()};
 sharpness::apply(px,w,h,&params,clip);
 // Noise-aware local extrema bound. Preserve color ratios; never create a hard RGB clamp.
 px.par_chunks_mut(w).enumerate().for_each(|(y,row)|{for (x,p) in row.iter_mut().enumerate(){let i=y*w+x;let before=lum(&source[i]);let after=lum(p);
  let (mut lo,mut hi)=(before,before);
  for yy in y.saturating_sub(1)..=(y+1).min(h-1){for xx in x.saturating_sub(1)..=(x+1).min(w-1){let v=lum(&source[yy*w+xx]);lo=lo.min(v);hi=hi.max(v);}}
  let tolerance=analysis.root_noise*before.sqrt()*0.5;
  let bounded=after.clamp((lo-tolerance).max(1e-8),hi+tolerance);
  let k=bounded/after;
  for v in p{*v*=k;}
 }});
 Ok(())
}

#[cfg(test)]mod tests{use super::*;
#[test]fn flat_image_stays_unchanged(){let src=vec![[0.18;3];64*48];let a=analyze(&src,64,48).unwrap();assert!(a.root_noise<1e-7);for method in ["none","current","noiseAware","adaptive"]{let mut p=src.clone();apply(&mut p,64,48,1.,&a,method).unwrap();assert_eq!(p,src);}}
#[test]fn invalid_inputs_return_errors(){assert!(analyze(&[],0,0).is_err());assert!(analyze(&[[f32::NAN;3];25],5,5).is_err());}
}
