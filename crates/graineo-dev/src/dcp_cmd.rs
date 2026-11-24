//! `graineo-dev dcp FILE OUTDIR [PROFILE.dcp …]`: develops a non-Sigma RAW (preview size) once with the file's own matrices and
//! once per camera profile (DCP; without paths: all installed for the camera), each with and without the LookTable,
//! writes the images as JPEG into OUTDIR and prints the mean/95th-percentile ΔE00 between them. Profiles are only read.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use develop::Settings;

fn lab_image(img: &develop::Rgb16) -> Vec<[f32; 3]> {
    img.data.chunks_exact(3).map(|p| raw::learn_look::srgb_to_lab([p[0] as f32 / 65535.0, p[1] as f32 / 65535.0, p[2] as f32 / 65535.0])).collect()
}

fn de(a: &[[f32; 3]], b: &[[f32; 3]]) -> (f32, f32) {
    let mut v: Vec<f32> = a.iter().zip(b).map(|(x, y)| raw::learn_look::de2000(*x, *y)).collect();
    let mean = v.iter().sum::<f32>() / v.len().max(1) as f32;
    v.sort_by(|a, b| a.total_cmp(b));
    (mean, v.get(v.len() * 95 / 100).copied().unwrap_or(0.0))
}

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("output folder missing")?);
    std::fs::create_dir_all(&out)?;
    let doc = rawdev::RawDoc::open(Path::new(file), &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
    let camera = format!("{} {}", doc.info.maker, doc.info.model);
    let mut profiles: Vec<(String, PathBuf)> = args[1..].iter().filter_map(|p| raw::dcp::Profile::load(Path::new(p)).map(|pr| (pr.name, PathBuf::from(p)))).collect();
    if profiles.is_empty() {
        profiles = raw::dcp::installed_for(&camera);
        if profiles.is_empty() {
            profiles = raw::dcp::installed_for(&doc.info.model);
        }
    }
    println!("CAMERA\t{camera}\t{} profiles", profiles.len());
    let render = |s: &Settings| doc.render_preview(s);
    let save = |img: &develop::Rgb16, name: &str| -> Result<()> {
        let n: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
        develop::export::write_jpeg(img, &out.join(format!("{n}.jpg")), 92)?;
        Ok(())
    };
    let base = render(&Settings::default());
    save(&base, "00-file-matrices")?;
    let base_lab = lab_image(&base);
    let mut with_look: Vec<(String, Vec<[f32; 3]>)> = Vec::new();
    for (name, path) in &profiles {
        let p = raw::dcp::Profile::load(path).context("profile")?;
        let mut s = Settings::default();
        s.raw.camera_profile = Some(path.to_string_lossy().into_owned());
        let a = render(&s);
        // without LookTable (test hook read in rawdev::render::Params::new)
        std::env::set_var("GRAINEO_DCP_NO_LOOK", "1");
        let mut s2 = s.clone();
        s2.exposure += 1e-6; // new settings state
        let b = render(&s2);
        std::env::remove_var("GRAINEO_DCP_NO_LOOK");
        save(&a, name)?;
        let (la, lb) = (lab_image(&a), lab_image(&b));
        let (m1, p1) = de(&la, &lb);
        let (m2, p2) = de(&la, &base_lab);
        println!(
            "PROFILE\t{name}\tHueSatMap {}\tLookTable {}\toffset {:+.2} EV\tΔE00 with/without LookTable mean {m1:.2} p95 {p1:.2}\tvs file matrices mean {m2:.2} p95 {p2:.2}",
            p.hue_sat.first().map_or("-".into(), |t| format!("{}x{}x{}", t.hues, t.sats, t.vals)),
            p.look.as_ref().map_or("-".into(), |t| format!("{}x{}x{}", t.hues, t.sats, t.vals)),
            p.baseline_offset
        );
        with_look.push((name.clone(), la));
    }
    for i in 0..with_look.len() {
        for j in i + 1..with_look.len() {
            let (m, p) = de(&with_look[i].1, &with_look[j].1);
            println!("PAIR\t{}\t{}\tΔE00 mean {m:.2} p95 {p:.2}", with_look[i].0, with_look[j].0);
        }
    }
    Ok(())
}
