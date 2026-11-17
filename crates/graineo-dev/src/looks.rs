//! Measure camera looks:
//!
//! - `graineo-dev style FILE… | --list L.txt [--raw]`: picture style from the makernote (film simulation,
//!   Picture Style, picture mode, Creative Style); line `STYLE\tpath\tmaker\tmodel\tcamera\tstyle\tdetails`.
//!   `--raw` additionally prints all makernote entries (to investigate unknown fields).
//! - `graineo-dev style-scan FOLDER [--copies N] [--threads N] [--old]`: time of reading the picture styles of the
//!   RAWs in FOLDER as the window "Kamera-Looks" does (each file N times as a clone `copy-K-NAME` in
//!   `GRAINEO_DATA_DIR/style-scan`, empty cache first, then a second run from the cache); `--old`: one reader, 4 MB
//!   head, no cache (the way before). Simulate a network drive with `GRAINEO_SIM_IO=<ms>:<MB/s>`.
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
    let l = if name.ends_with(".x3look") { raw::looks::Look::file(Path::new(name)) } else { rawdev::looks::find(name).map(|l| (*l).clone()) }.context("look not found")?;
    let mut img = image::open(input)?.to_rgb8();
    l.apply(&mut img);
    img.save(out)?;
    Ok(())
}

pub fn style_scan(args: &[String]) -> Result<()> {
    use rawdev::learn::{self, Candidate};
    let value = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let folder = args.first().context("FOLDER missing")?;
    let copies: usize = value("--copies").and_then(|v| v.parse().ok()).unwrap_or(1);
    let threads: usize = value("--threads").and_then(|v| v.parse().ok()).unwrap_or(6);
    let old = args.iter().any(|a| a == "--old");
    let data = std::env::var_os("GRAINEO_DATA_DIR").context("only with GRAINEO_DATA_DIR (the cache lives there)")?;
    let work = Path::new(&data).join("style-scan");
    std::fs::create_dir_all(&work)?;
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(folder)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| matches!(e.to_ascii_uppercase().to_str(), Some("RAF" | "CR3" | "CR2" | "DNG" | "ARW" | "NEF")))).collect();
    files.sort();
    let mut cands = Vec::new();
    for k in 0..copies {
        for f in &files {
            let name = f.file_name().context("name")?.to_string_lossy().to_string();
            let p = if copies == 1 { f.clone() } else { work.join(format!("copy-{k}-{name}")) };
            if copies > 1 && !p.exists() {
                // a clone on APFS takes no space
                let ok = std::process::Command::new("cp").arg("-c").arg(f).arg(&p).status().map(|s| s.success()).unwrap_or(false);
                if !ok {
                    std::fs::copy(f, &p)?;
                }
            }
            cands.push(Candidate { path: p, day: format!("2026-01-{:02}", 1 + k % 28), folder: work.display().to_string(), camera: String::new(), thumb: None });
        }
    }
    let cache = Path::new(&data).join("cache").join("learn-look-scan.tsv");
    let _ = std::fs::remove_file(&cache);
    let t = std::time::Instant::now();
    let n = if old { learn::scan_all_one_by_one_without_cache(&cands).len() } else { learn::scan_all(&cands, threads, &|| true, &|_, _| {}).map_or(0, |v| v.len()) };
    let first = t.elapsed().as_secs_f64();
    println!("SCAN\t{}\tfiles {}\tstyles {n}\tthreads {}\tfirst {first:.2} s\t{:.0} ms/file", if old { "old" } else { "new" }, cands.len(), if old { 1 } else { threads }, first * 1000.0 / cands.len().max(1) as f64);
    if args.iter().any(|a| a == "--compare") {
        // the same list as the old way (picture style, adaptive setting, camera JPEG, order)
        let line = |v: &[rawdev::learn::Scanned]| v.iter().map(|s| format!("{}|{}|{}|{:?}|{}", s.candidate.path.display(), s.key(), s.camera_name, s.adaptive, s.has_jpeg)).collect::<Vec<_>>();
        let new = learn::scan_all(&cands, threads, &|| true, &|_, _| {}).unwrap_or_default();
        let old = learn::scan_all_one_by_one_without_cache(&cands);
        anyhow::ensure!(line(&new) == line(&old), "the new reading differs from the old one");
        println!("SCAN\tcompare\t{} results the same", new.len());
    }
    if !old {
        let t = std::time::Instant::now();
        let m = learn::scan_all(&cands, threads, &|| true, &|_, _| {}).map_or(0, |v| v.len());
        println!("SCAN\tnew, from the cache\tfiles {}\tstyles {m}\t{:.2} s", cands.len(), t.elapsed().as_secs_f64());
    }
    Ok(())
}
