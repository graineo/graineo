//! Measure camera looks:
//!
//! - `graineo-dev style FILE… | --list L.txt [--raw]`: picture style from the makernote (film simulation,
//!   Picture Style, picture mode, Creative Style); line `STYLE\tpath\tmaker\tmodel\tcamera\tstyle\tdetails`.
//!   `--raw` additionally prints all makernote entries (to investigate unknown fields).
//! - `graineo-dev look-pair --list L.tsv --off FOLDER [--px N]`: for each line "RAW<TAB>camera JPEG|-" write the
//!   neutral development (no look, no sharpening) downscaled as `<name>-own.png` and – for
//!   "-" – the embedded camera preview as `<name>-kamera.png` (both in
//!   sensor orientation, not realigned; matching/alignment is done by `lookfit2.py`).
//!   Line `PAIR\tRAW\treference\tcamera\tstyle\tdetails\torientation\twidth×height preview`.

use std::path::Path;

use anyhow::{Context, Result};
use image::RgbImage;
use raw::RawFile;

fn files(args: &[String]) -> Result<Vec<String>> {
    if let Some(i) = args.iter().position(|a| a == "--list") {
        let f = args.get(i + 1).context("--list FILE")?;
        return Ok(std::fs::read_to_string(f)?.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect());
    }
    Ok(args.iter().filter(|a| !a.starts_with("--")).cloned().collect())
}

pub fn style(args: &[String]) -> Result<()> {
    let raw = args.iter().any(|a| a == "--raw");
    for f in files(args)? {
        let f = f.split('\t').next().unwrap_or("").to_string();
        let p = Path::new(&f);
        match raw::picture_style::read(p) {
            Some(b) => {
                let d: Vec<String> = b.details.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("STYLE\t{f}\t{}\t{}\t{}\t{}\t{}", b.maker, b.model, b.camera_short(), b.style, d.join(" "));
            }
            None => println!("STYLE\t{f}\t-\t-\t-\tERROR\t"),
        }
        if raw {
            if let Some((_, _, e)) = raw::picture_style::makernote(p) {
                for e in e {
                    let w = match &e.text {
                        Some(t) => format!("\"{t}\""),
                        None => format!("{:?}", e.values),
                    };
                    println!("MN\t{f}\t{:#06x}\t{}\t{}\t{w}", e.tag, e.type_name, e.n);
                }
            }
        }
    }
    Ok(())
}

fn downscale(img: &RgbImage, px: u32) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let s = px as f32 / w.max(h) as f32;
    if s >= 1.0 {
        return img.clone();
    }
    let (nw, nh) = (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1));
    // area average (halve coarsely first, then triangle filter) – sufficient for color statistics
    image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle)
}

pub fn look_pair(args: &[String]) -> Result<()> {
    let value = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let out = value("--out").context("--out FOLDER")?;
    let px: u32 = value("--px").and_then(|v| v.parse().ok()).unwrap_or(1024);
    std::fs::create_dir_all(&out)?;
    for row in files(args)? {
        let mut t = row.split('\t');
        let rawp = t.next().unwrap_or("").to_string();
        let refp = t.next().unwrap_or("-").to_string();
        let name = Path::new(&rawp).file_stem().and_then(|s| s.to_str()).unwrap_or("x").to_string();
        // unique name: parent folder + file
        let parent_dir = Path::new(&rawp).parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or("").replace(' ', "_");
        let stem = format!("{out}/{parent_dir}__{name}");
        if Path::new(&format!("{stem}-own.png")).exists() {
            println!("PAIR\t{rawp}\t{refp}\tvorhanden");
            continue;
        }
        let rf = match RawFile::open(&rawp) {
            Ok(r) => r,
            Err(e) => {
                println!("PAIR\t{rawp}\t{refp}\tERROR\t{e}");
                continue;
            }
        };
        let st = raw::picture_style::read(Path::new(&rawp)).or_else(|| if refp != "-" { raw::picture_style::read(Path::new(&refp)) } else { None });
        // noise reduction does not change the color statistics but costs time → off
        let o = raw::development::Options { sharpness: None, camera_look: false, look: None, noise: raw::noise::NoiseReduction::OFF, ..Default::default() };
        let e = match rf.develop(&o) {
            Ok(e) => e,
            Err(e) => {
                println!("PAIR\t{rawp}\t{refp}\tERROR\t{e}");
                continue;
            }
        };
        let mut vs = String::from("-");
        if refp == "-" {
            match rf.preview() {
                Some(v) => {
                    vs = format!("{}x{}", v.width(), v.height());
                    downscale(&v, px).save(format!("{stem}-camera.png"))?;
                }
                None => {
                    println!("PAIR\t{rawp}\t{refp}\tERROR\tno preview");
                    continue;
                }
            }
        }
        downscale(&e.img, px).save(format!("{stem}-own.png"))?;
        let (k, s, d) = match &st {
            Some(b) => (b.camera_short(), b.style.clone(), b.details.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")),
            None => (raw::picture_style::camera_short(&rf.info.maker, &rf.info.model), "unknown".into(), String::new()),
        };
        println!("PAIR\t{rawp}\t{refp}\t{k}\t{s}\t{d}\t{}\t{vs}\t{stem}\t{:.2}", rf.info.orientation, e.ev);
    }
    Ok(())
}

/// `graineo-dev look-image IN.png OUT.png LOOK`: apply a look to a finished sRGB image of the neutral
/// development (check Rust ↔ fitting tool).
pub fn look_image(args: &[String]) -> Result<()> {
    let (input, out, name) = (args.first().context("IN")?, args.get(1).context("OUT")?, args.get(2).context("LOOK")?);
    let l = if name.ends_with(".x3look") { raw::looks::Look::file(Path::new(name)) } else { raw::looks::Look::built_in(name) }.context("look not found")?;
    let mut img = image::open(input)?.to_rgb8();
    l.apply(&mut img);
    img.save(out)?;
    Ok(())
}
