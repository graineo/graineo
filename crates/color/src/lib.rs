//! RGB/ICC boundary for graineo. All working samples remain float and may be negative.
//! Built-in ICCs are generated from mathematical primaries/TRCs by moxcms (BSD/Apache);
//! no Adobe profile files are read, copied or bundled. ICC PCS matrices use D50.
pub mod cube;
use moxcms::{ColorProfile, DataColorSpace, ParsingOptions, ToneReprCurve};
use rayon::prelude::*;
use serde::{Deserialize,Serialize};

pub type Result<T> = std::result::Result<T,String>;
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum Space { #[default] Srgb, DisplayP3, AdobeRgb, ProPhotoRgb }
impl Space {
    pub const ALL:[Self;4]=[Self::Srgb,Self::DisplayP3,Self::AdobeRgb,Self::ProPhotoRgb];
    pub fn name(self)->&'static str {match self{Self::Srgb=>"sRGB",Self::DisplayP3=>"Display P3",Self::AdobeRgb=>"Adobe RGB (1998)",Self::ProPhotoRgb=>"ProPhoto RGB"}}
    pub fn profile(self)->ColorProfile {let mut p=match self{Self::Srgb=>ColorProfile::new_srgb(),Self::DisplayP3=>ColorProfile::new_display_p3(),Self::AdobeRgb=>ColorProfile::new_adobe_rgb(),Self::ProPhotoRgb=>ColorProfile::new_pro_photo_rgb()};p.cicp=None;p}
    /// Profile bytes. The generator stamps the current time into the header; we set a fixed date so that identical
    /// inputs produce identical files (and comparisons do not depend on the second hand).
    pub fn icc(self)->Result<Vec<u8>> {let mut b=self.profile().encode().map_err(|e|format!("ICC: {e}"))?;if b.len()>=36{for (i,v) in [2026u16,10,30,0,0,0].iter().enumerate(){b[24+2*i..26+2*i].copy_from_slice(&v.to_be_bytes())}}Ok(pad_icc(b))}
    pub fn encode(self,x:f32)->f32 {let x=x.clamp(0.0,1.0);match self{Self::Srgb|Self::DisplayP3=>srgb_encode(x),Self::AdobeRgb=>x.powf(1.0/2.19921875),Self::ProPhotoRgb=>x.powf(1.0/1.8)}}
    pub fn decode(self,x:f32)->f32 {match self{Self::Srgb|Self::DisplayP3=>srgb_decode(x),Self::AdobeRgb=>x.signum()*x.abs().powf(2.19921875),Self::ProPhotoRgb=>x.signum()*x.abs().powf(1.8)}}
}
/// ICC profiles must be a multiple of 4 bytes long (ICC.1 7.1; PNG readers reject other lengths):
/// pad with zeros and correct the size in the header.
fn pad_icc(mut b:Vec<u8>)->Vec<u8>{if b.len()>=4&&b.len()%4!=0{b.resize(b.len().div_ceil(4)*4,0);let n=b.len() as u32;b[..4].copy_from_slice(&n.to_be_bytes());}b}
/// Extended sRGB transfer: the linear branch retains negative out-of-gamut components.
pub fn srgb_decode(x:f32)->f32 {if x<=0.04045{x/12.92}else{((x+0.055)/1.055).powf(2.4)}}
pub fn srgb_encode(x:f32)->f32 {if x<=0.0031308{x*12.92}else{1.055*x.powf(1.0/2.4)-0.055}}
pub type Matrix=[[f64;3];3];
fn pcs(p:&ColorProfile)->Matrix {let (r,g,b)=(p.red_colorant,p.green_colorant,p.blue_colorant);[[r.x,g.x,b.x],[r.y,g.y,b.y],[r.z,g.z,b.z]]}
fn inv(m:Matrix)->Result<Matrix>{let [[a,b,c],[d,e,f],[g,h,i]]=m;let det=a*(e*i-f*h)-b*(d*i-f*g)+c*(d*h-e*g);if !det.is_finite()||det.abs()<1e-12{return Err("Singular RGB profile matrix".into())}Ok([[e*i-f*h,c*h-b*i,b*f-c*e],[f*g-d*i,a*i-c*g,c*d-a*f],[d*h-e*g,b*g-a*h,a*e-b*d]].map(|r|r.map(|x|x/det)))}
fn mul(a:Matrix,b:Matrix)->Matrix{std::array::from_fn(|i|std::array::from_fn(|j|(0..3).map(|k|a[i][k]*b[k][j]).sum()))}
pub fn matrix(from:Space,to:Space)->Result<Matrix>{if from==to{return Ok([[1.,0.,0.],[0.,1.,0.],[0.,0.,1.]])}Ok(mul(inv(pcs(&to.profile()))?,pcs(&from.profile())))}
pub fn pixel(m:Matrix,p:[f32;3])->[f32;3]{m.map(|r|(r[0]*p[0] as f64+r[1]*p[1] as f64+r[2]*p[2] as f64) as f32)}
fn check(data:&[f32])->Result<()>{if !data.len().is_multiple_of(3)||!data.iter().all(|x|x.is_finite()){return Err("Invalid RGB samples".into())}Ok(())}
/// Convert linear RGB without clipping, including negative and above-white samples.
pub fn convert_linear(data:&[f32],from:Space,to:Space)->Result<Vec<f32>>{check(data)?;let m=matrix(from,to)?;let mut out=vec![0.;data.len()];out.par_chunks_mut(3).zip(data.par_chunks(3)).for_each(|(o,p)|o.copy_from_slice(&pixel(m,[p[0],p[1],p[2]])));Ok(out)}
/// The only quantization/gamut clipping boundary for a new export path.
pub fn encode_rgb16(linear_srgb:&[f32],to:Space)->Result<Vec<u16>>{let linear=convert_linear(linear_srgb,Space::Srgb,to)?;Ok(linear.par_iter().map(|&x|(to.encode(x)*65535.0).round() as u16).collect())}
/// Convert an existing encoded sRGB image. Does not claim to recover already-clipped colors.
pub fn transcode_srgb16(data:&[u16],to:Space)->Result<Vec<u16>>{if to==Space::Srgb{return Ok(data.to_vec())}let linear:Vec<f32>=data.par_iter().map(|&v|srgb_decode(v as f32/65535.0)).collect();encode_rgb16(&linear,to)}
/// Decode an embedded RGB ICC profile to extended linear sRGB. Missing ICC is a separate caller policy.
pub fn decode_icc(data:&[f32],icc:&[u8])->Result<Vec<f32>>{
    check(data)?;
    let options=ParsingOptions{max_profile_size:16*1024*1024,max_allowed_clut_size:16*1024*1024,max_allowed_trc_size:1_000_000};
    if icc.len()>options.max_profile_size{return Err("ICC profile exceeds 16 MiB".into())}
    let src=ColorProfile::new_from_slice_with_options(icc,options).map_err(|e|format!("ICC: {e}"))?;
    if src.color_space!=DataColorSpace::Rgb{return Err("Only RGB input profiles are supported".into())}
    if src.lut_a_to_b_perceptual.is_some() || src.lut_a_to_b_colorimetric.is_some() || src.lut_a_to_b_saturation.is_some() { return Err("LUT input profiles use the native ImageIO color-management path".into()); }
    let curves=[src.red_trc.as_ref(),src.green_trc.as_ref(),src.blue_trc.as_ref()];
    if curves.iter().any(|c|c.is_none()) {return Err("RGB profile has no transfer curves".into())}
    let matrix=mul(inv(pcs(&Space::Srgb.profile()))?,pcs(&src));
    let mut out=Vec::with_capacity(data.len());
    for p in data.chunks_exact(3){let mut l=[0.;3];for c in 0..3{l[c]=eval_curve(curves[c].ok_or("Missing transfer curve")?,p[c])?;}out.extend_from_slice(&pixel(matrix,l));}
    check(&out)?;Ok(out)

}

/// Scene-linear working spaces for floating-point exports (TIFF 32 bit, OpenEXR). Values stay
/// unclipped; ICC profiles have linear curves, EXR files carry the primaries as `chromaticities`.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum LinearSpace { #[default] Rec2020, AcesCg, ProPhoto }
impl LinearSpace {
    pub const ALL:[Self;3]=[Self::Rec2020,Self::AcesCg,Self::ProPhoto];
    pub fn name(self)->&'static str {match self{Self::Rec2020=>"Rec. 2020 linear",Self::AcesCg=>"ACEScg",Self::ProPhoto=>"ProPhoto linear"}}
    pub fn profile(self)->ColorProfile {
        let mut p=match self{Self::Rec2020=>ColorProfile::new_bt2020(),Self::AcesCg=>ColorProfile::new_aces_cg_linear(),Self::ProPhoto=>ColorProfile::new_pro_photo_rgb()};
        let lin=ToneReprCurve::Lut(Vec::new());
        p.red_trc=Some(lin.clone());p.green_trc=Some(lin.clone());p.blue_trc=Some(lin);p.cicp=None;p
    }
    /// ICC profile with linear curves (fixed date, see [`Space::icc`]).
    pub fn icc(self)->Result<Vec<u8>> {let mut b=self.profile().encode().map_err(|e|format!("ICC: {e}"))?;if b.len()>=36{for (i,v) in [2026u16,11,10,0,0,0].iter().enumerate(){b[24+2*i..26+2*i].copy_from_slice(&v.to_be_bytes())}}Ok(pad_icc(b))}
    /// Primaries and white point (CIE xy): red, green, blue, white.
    pub fn chromaticities(self)->[[f32;2];4] {match self{
        Self::Rec2020=>[[0.708,0.292],[0.170,0.797],[0.131,0.046],[0.3127,0.3290]],
        Self::AcesCg=>[[0.713,0.293],[0.165,0.830],[0.128,0.044],[0.32168,0.33767]],
        Self::ProPhoto=>[[0.7347,0.2653],[0.1596,0.8404],[0.0366,0.0001],[0.3457,0.3585]]}}
    /// Linear sRGB → this space (white to white, via the D50 colorants of the ICC profiles).
    pub fn from_srgb(self)->Result<Matrix> {Ok(mul(inv(pcs(&self.profile()))?,pcs(&Space::Srgb.profile())))}
}
/// Convert linear sRGB samples into a linear working space, without clipping.
pub fn convert_to_linear_space(data:&[f32],to:LinearSpace)->Result<Vec<f32>>{check(data)?;let m=to.from_srgb()?;let mut out=vec![0.;data.len()];out.par_chunks_mut(3).zip(data.par_chunks(3)).for_each(|(o,p)|o.copy_from_slice(&pixel(m,[p[0],p[1],p[2]])));Ok(out)}

// ICC.1 parametricCurveType / curveType. Decode before the matrix, never clip its result.
fn eval_curve(curve:&ToneReprCurve,x:f32)->Result<f32>{
 if !(0.0..=1.0).contains(&x){return Err("ICC encoded RGB input must be in 0..1".into())}
 let y=match curve{
  ToneReprCurve::Lut(v)=>match v.len(){0=>x,1=>x.powf(v[0] as f32/256.0),n=>{let t=x*(n-1) as f32;let a=(t as usize).min(n-2);let f=t-a as f32;(v[a] as f32*(1.-f)+v[a+1] as f32*f)/65535.}},
  ToneReprCurve::Parametric(p)=>{let v=match p.as_slice(){[g]=>x.powf(*g),[g,a,b]=>if x>=-b/a{(a*x+b).max(0.).powf(*g)}else{0.},[g,a,b,c]=>if x>=-b/a{(a*x+b).max(0.).powf(*g)+c}else{*c},[g,a,b,c,d]=>if x>=*d{(a*x+b).max(0.).powf(*g)}else{c*x},[g,a,b,c,d,e,f]=>if x>=*d{(a*x+b).max(0.).powf(*g)+e}else{c*x+f},_=>return Err("Unsupported ICC transfer curve".into())};v}
 };if y.is_finite(){Ok(y)}else{Err("Invalid ICC transfer curve result".into())}
}

#[cfg(test)]mod tests{
 use super::*;
 #[test]fn profiles_are_generated_and_parseable(){for s in Space::ALL{let p=ColorProfile::new_from_slice(&s.icc().unwrap()).unwrap();assert_eq!(p.color_space,DataColorSpace::Rgb);}}
 #[test]fn linear_roundtrips_keep_extended_gamut(){let p=[-0.1,0.6,1.4,0.,0.,0.,1.,1.,1.];for s in Space::ALL{let a=convert_linear(&p,Space::Srgb,s).unwrap();let b=convert_linear(&a,s,Space::Srgb).unwrap();for (x,y) in p.iter().zip(b){assert!((x-y).abs()<2e-6,"{s:?}: {x} {y}")}}}
 #[test]fn p3_red_is_preserved_until_output(){let s=convert_linear(&[1.,0.,0.],Space::DisplayP3,Space::Srgb).unwrap();assert!(s[0]>1.&&s[1]<0.);let back=encode_rgb16(&s,Space::DisplayP3).unwrap();assert_eq!(back,[65535,0,0]);let clipped=s.iter().map(|x|x.clamp(0.,1.)).collect::<Vec<_>>();assert_ne!(encode_rgb16(&clipped,Space::DisplayP3).unwrap(),back);}
 #[test]fn encoded_icc_matches_builtin_matrix(){for s in Space::ALL{let e=[0.2,0.5,0.8,1.,0.,0.,0.5,0.5,0.5];let a=decode_icc(&e,&s.icc().unwrap()).unwrap();let l=e.map(|v|s.decode(v));let b=convert_linear(&l,s,Space::Srgb).unwrap();for (x,y) in a.iter().zip(b){assert!((x-y).abs()<0.002,"{s:?}: {x} {y}")}}}
 #[test]fn neutral_axis_and_srgb_identity(){let src=[0,1,128,32768,65534,65535];assert_eq!(transcode_srgb16(&src,Space::Srgb).unwrap(),src);for s in Space::ALL{let p=convert_linear(&[0.18;3],Space::Srgb,s).unwrap();assert!((p[0]-p[1]).abs()<0.0002&&(p[1]-p[2]).abs()<0.0002)}}
 #[test]fn icc_lengths_are_padded(){for s in Space::ALL{let b=s.icc().unwrap();assert_eq!(b.len()%4,0);assert_eq!(u32::from_be_bytes(b[..4].try_into().unwrap()) as usize,b.len());}for l in LinearSpace::ALL{assert_eq!(l.icc().unwrap().len()%4,0)}}
 #[test]fn linear_spaces_keep_white_and_parse(){for l in LinearSpace::ALL{let p=ColorProfile::new_from_slice(&l.icc().unwrap()).unwrap();assert_eq!(p.color_space,DataColorSpace::Rgb);let w=convert_to_linear_space(&[1.,1.,1.,0.18,0.18,0.18],l).unwrap();for v in &w[..3]{assert!((v-1.).abs()<1e-3,"{l:?} {w:?}")}}let red=convert_to_linear_space(&[1.,0.,0.],LinearSpace::Rec2020).unwrap();assert!(red[0]>0.6&&red[0]<0.65,"{red:?}");}
 #[test]fn malformed_inputs_are_errors(){assert!(decode_icc(&[0.;3],b"invalid").is_err());assert!(convert_linear(&[0.;4],Space::Srgb,Space::DisplayP3).is_err());assert!(convert_linear(&[f32::NAN;3],Space::Srgb,Space::DisplayP3).is_err());}
}
