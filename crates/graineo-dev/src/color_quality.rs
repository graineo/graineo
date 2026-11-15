//! Local, reproducible Bayer comparisons and RGB profile export. Never changes defaults.
use std::{path::{Path,PathBuf},time::Instant};
use anyhow::{Result,Context,bail};
use develop::{Rgb16,Settings};
use graineo_color::Space;
use image::{RgbImage,imageops::{self,FilterType}};
use serde_json::{Value,json};
fn output()->Result<PathBuf>{let p=PathBuf::from(std::env::var_os("GRAINEO_DATA_DIR").context("GRAINEO_DATA_DIR must point to scratch")?);std::fs::create_dir_all(&p)?;Ok(p)}
fn rgb(img:&Rgb16)->Result<RgbImage>{RgbImage::from_raw(img.width as u32,img.height as u32,img.to_rgb8()).context("invalid image")}
fn save(img:&Rgb16,path:&Path)->Result<()>{rgb(img)?.save(path)?;Ok(())}
fn regions(img:&Rgb16)->Vec<[usize;4]>{
 let(w,h)=(img.width,img.height);let n=384.min(w/2).min(h/2).max(8);let lum=|x:usize,y:usize|{let p=&img.data[(y*w+x)*3..];(0.2126*p[0] as f64+0.7152*p[1] as f64+0.0722*p[2] as f64)/65535.};
 let mut tiles=Vec::new();let step=n/2;
 for y in (h/10..h*9/10-n).step_by(step){for x in (w/10..w*9/10-n).step_by(step){let(mut score,mut mean,mut count)=(0.,0.,0.);for yy in (y..y+n-1).step_by(4){for xx in (x..x+n-1).step_by(4){let l=lum(xx,yy);mean+=l;score+=(l-lum(xx+1,yy)).abs()+(l-lum(xx,yy+1)).abs();count+=1.;}}if mean/count>0.04&&mean/count<0.96{tiles.push((score/count,[x,y,n,n]));}}}
 tiles.sort_by(|a,b|a.0.total_cmp(&b.0));
 let flat=tiles.first().map(|v|v.1).unwrap_or([w/4,h/4,n,n]);
 let detail=tiles.last().map(|v|v.1).unwrap_or([w/2-n/2,h/2-n/2,n,n]);
 vec![detail,[w/2-n/2,h/2-n/2,n,n],flat]
}
fn crops(img:&Rgb16,regions:&[[usize;4]],dir:&Path,method:&str)->Result<()> {
 let img=rgb(img)?;for (i,&[x,y,w,h]) in regions.iter().enumerate(){let crop=imageops::crop_imm(&img,x as u32,y as u32,w as u32,h as u32).to_image();crop.save(dir.join(format!("{method}-crop-{i}-100.png")))?;imageops::resize(&crop,(w*2) as u32,(h*2) as u32,FilterType::Nearest).save(dir.join(format!("{method}-crop-{i}-200.png")))?;}Ok(())
}
pub fn study(manifest:&str)->Result<()> {
 let cases:Vec<Value>=serde_json::from_slice(&std::fs::read(manifest)?)?;let root=output()?.join("sharpen-real");std::fs::create_dir_all(&root)?;
 let mut results=Vec::new();
 for case in cases{
  let id=case["id"].as_str().context("id")?;if !id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'){bail!("invalid case id")}
  let file=case["path"].as_str().context("path")?;let start=Instant::now();
  let doc=rawdev::RawDoc::open(Path::new(file),&||true).map_err(anyhow::Error::msg)?.context("open cancelled")?;
  let s=Settings::default();let prepared=rawdev::doc::sharpen_study::prepare(&doc,&s).map_err(anyhow::Error::msg)?;
  let prep_ms=start.elapsed().as_secs_f64()*1000.;let dir=root.join(id);std::fs::create_dir_all(&dir)?;
  let mut methods=Vec::new();let mut boxes=None;let mut baseline=None;
  for method in ["current","none","noiseAware","adaptive"]{
   let t=Instant::now();let img=rawdev::doc::sharpen_study::render(&doc,&s,&prepared,method).map_err(anyhow::Error::msg)?;let ms=t.elapsed().as_secs_f64()*1000.;
   let img=develop::geometry::apply(img,&s,doc.orientation);
   let reg=boxes.get_or_insert_with(||regions(&img));
   if method=="current"{baseline=Some(img.data.clone());}
   let reference=baseline.as_ref().context("baseline")?;let mae=img.data.iter().zip(reference).map(|(&a,&b)|a.abs_diff(b) as f64/65535.).sum::<f64>()/img.data.len() as f64;
   save(&img,&dir.join(format!("{method}-full.png")))?;
   let rgb=rgb(&img)?;let thumb=imageops::thumbnail(&rgb,1200,1000);thumb.save(dir.join(format!("{method}-overview.png")))?;
   crops(&img,reg,&dir,method)?;
   methods.push(json!({"id":method,"renderMs":ms,"meanAbsDifferenceFromCurrent":mae}));
   println!("{id} {method} {:.0} ms",ms);
  }
  let value=json!({"id":id,"camera":case["camera"],"iso":doc.info.exif.iso,"width":doc.full_size[0],"height":doc.full_size[1],"preparationMs":prep_ms,"analysis":prepared.analysis,"regions":boxes,"methods":methods});
  std::fs::write(dir.join("metrics.json"),serde_json::to_vec_pretty(&value)?)?;results.push(value);
 }
 std::fs::write(root.join("results.json"),serde_json::to_vec_pretty(&results)?)?;
 report(&root,&results)?;println!("{}",root.join("index.html").display());Ok(())
}
fn report(root:&Path,results:&[Value])->Result<()> {
 let mut html=String::from("<!doctype html><meta charset=utf-8><title>graineo: sharpening comparison</title><style>body{background:#161616;color:#ddd;font:14px system-ui;margin:24px}section{display:flex;gap:12px;overflow:auto}figure{margin:0}figcaption{padding:8px 0}img{max-width:none;image-rendering:pixelated}svg{max-width:100%;height:auto}button{padding:8px}h2{margin-top:50px}.note{max-width:1000px;color:#aaa}</style><h1>graineo – sharpening on real Bayer RAWs</h1><p class=note>Own scratch copies. Same development and noise sliders, only the base sharpening changes. 100 %: one image pixel per file pixel; 200 %: plain pixel doubling. The actual display size depends on browser zoom and screen. No comparison with Iridient, no statement about lost original detail. Photos have no noise-free reference image. The numbers measure differences, not automatically quality.</p>");
 let labels=[("current","Current"),("none","No base sharpening"),("noiseAware","Noise-aware, fixed radius"),("adaptive","Radius selection + noise protection")];
 for r in results{let id=r["id"].as_str().unwrap_or("");let w=r["width"].as_u64().unwrap_or(1);let h=r["height"].as_u64().unwrap_or(1);
 html.push_str(&format!("<h2>{} · ISO {} · {id}</h2><p>Estimated radius {:.2} px; noise threshold {:.4}; usable edges {}.</p><svg viewBox='0 0 {w} {h}' width=1000><image href='{id}/current-overview.png' width='{w}' height='{h}' preserveAspectRatio='none'/>",r["camera"].as_str().unwrap_or("RAW"),r["iso"],r["analysis"]["sigma"].as_f64().unwrap_or(0.7),r["analysis"]["threshold"].as_f64().unwrap_or(0.012),r["analysis"]["edges"]));
 if let Some(boxes)=r["regions"].as_array(){for(i,b)in boxes.iter().enumerate(){html.push_str(&format!("<rect x='{}' y='{}' width='{}' height='{}' fill='none' stroke='#ffc840' stroke-width='14'/><text x='{}' y='{}' fill='#ffc840' font-size='90'>{}</text>",b[0],b[1],b[2],b[3],b[0],b[1].as_u64().unwrap_or(0)+90,i+1));}}html.push_str("</svg>");
 for crop in 0..3{html.push_str(&format!("<h3>Crop {}: {}</h3>",crop+1,["automatically chosen detail area","image center","quiet area (may contain subject structure)"][crop]));for zoom in [100,200]{html.push_str(&format!("<h4>{zoom} %</h4><section>"));for(method,label)in labels{html.push_str(&format!("<figure><img loading=lazy src='{id}/{method}-crop-{crop}-{zoom}.png'><figcaption>{label}</figcaption></figure>"));}html.push_str("</section>");}}
 html.push_str(&format!("<p><a href='{id}/metrics.json'>Measurements</a> · full resolution: "));for(method,label)in labels{html.push_str(&format!("<a href='{id}/{method}-full.png'>{label}</a> · "));}html.push_str("</p>");
 }std::fs::write(root.join("index.html"),html)?;Ok(())
}
pub fn export(file:&str,args:&[String])->Result<()> {
 let out=args.first().context("output path")?;let name=args.iter().position(|a|a=="--space").and_then(|i|args.get(i+1)).map(String::as_str).unwrap_or("srgb");
 let space=match name{"srgb"=>Space::Srgb,"p3"=>Space::DisplayP3,"adobeRgb"=>Space::AdobeRgb,"proPhotoRgb"=>Space::ProPhotoRgb,_=>bail!("space: srgb | p3 | adobeRgb | proPhotoRgb")};
 let doc=rawdev::RawDoc::open(Path::new(file),&||true).map_err(anyhow::Error::msg)?.context("cancelled")?;
 let format=develop::export::Format::from_path(Path::new(out)).context("output must be JPEG/TIFF")?;
 let mut opts=develop::export::Options::default();
 if let Some(v)=args.iter().find_map(|a|a.strip_prefix("--metadata=")){opts.metadata=develop::export::Metadata::parse(v).context("--metadata=none|shooting|all")?;}
 opts.gps=args.iter().any(|a|a=="--gps");
 rawdev::export::export_color(&doc,&Settings::default(),format,Path::new(out),space).map_err(anyhow::Error::msg)?;
 crate::export_metadata(Path::new(out),Path::new(file),format,&opts,Some(rawdev::export::shot(&doc.info)));
 println!("{}: {out}",space.name());Ok(())
}
