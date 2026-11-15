//! `graineo-dev semantic_search <ausgabeordner> <anfragen.tsv> <bildordner|datei>…`
//! Measurement of the semantic search: image vectors of all photos in the folders (cached),
//! per query (German, English translation) relevance values, hit counts at several strictness
//! levels, overlap of the top 20 and a contact sheet of the top hits.
use anyhow::{bail, Context, Result};
use library::semantic::{ImageEncoder, TextEncoder};
use library::similar::{keep, limits, scored, Vectors};
use std::path::{Path, PathBuf};

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let mut e: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).collect();
        e.sort();
        for q in e {
            collect(&q, out);
        }
    } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.') && library::format_from_extension(n).is_some_and(|f| f != "VIDEO")) {
        out.push(p.to_path_buf());
    }
}

pub fn run(a: &[String]) -> Result<()> {
    if a.len() < 3 {
        bail!("semantic-search <output_folder> <queries.tsv> <folder|file>…");
    }
    let out = PathBuf::from(&a[0]);
    std::fs::create_dir_all(&out)?;
    let mut files = Vec::new();
    for p in &a[2..] {
        collect(Path::new(p), &mut files);
    }
    let img_enc = ImageEncoder::load(&library::semantic::image_folder().context("image encoder missing")?).map_err(anyhow::Error::msg)?;
    let txt_enc = TextEncoder::load(&library::semantic::text_folder().context("Text-Encoder fehlt")?).map_err(anyhow::Error::msg)?;
    // vectors with cache (path<TAB>512 f32 as hex-free binary in vec.bin, order in vec.txt)
    let (cache_txt, cache_bin) = (out.join("vec.txt"), out.join("vec.bin"));
    let mut cached: std::collections::HashMap<String, Vec<f32>> = Default::default();
    if let (Ok(t), Ok(b)) = (std::fs::read_to_string(&cache_txt), std::fs::read(&cache_bin)) {
        for (i, l) in t.lines().enumerate() {
            if let Some(c) = b.get(i * 2048..(i + 1) * 2048) {
                cached.insert(l.to_string(), c.chunks_exact(4).map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]])).collect());
            }
        }
    }
    let mut vecs = Vectors::empty();
    let mut names: Vec<(PathBuf, Option<image::RgbImage>)> = Vec::new();
    let (mut txt, mut bin) = (String::new(), Vec::<u8>::new());
    for f in files.iter() {
        let key = f.to_string_lossy().to_string();
        let fmt = library::format_from_extension(&key).unwrap_or("JPG");
        let v = match cached.get(&key) {
            Some(v) => v.clone(),
            None => match library::preview::load(f, fmt, 512) {
                Ok((img, _)) => img_enc.embed(&img),
                Err(e) => {
                    eprintln!("skipped {key}: {e}");
                    continue;
                }
            },
        };
        txt.push_str(&key);
        txt.push('\n');
        bin.extend(v.iter().flat_map(|x| x.to_le_bytes()));
        vecs.insert_into(names.len() as i64, &v);
        names.push((f.clone(), None));
    }
    std::fs::write(&cache_txt, txt)?;
    std::fs::write(&cache_bin, bin)?;
    println!("{} photos with vector", names.len());
    let tsv = std::fs::read_to_string(&a[1])?;
    let levels = [0.0f32, 0.25, 0.5, 0.75, 1.0];
    let top_names = |l: &[library::similar::Scored], n: usize| -> Vec<String> {
        l.iter().take(n).map(|s| names[s.file as usize].0.file_name().unwrap().to_string_lossy().to_string()).collect()
    };
    let mut summary = String::from("Anfrage\tSprache\ttop_z\tz10\tz20\tz50\tmedian_z\t");
    for l in levels {
        summary.push_str(&format!("keep@{l}\t"));
    }
    summary.push_str("Overlap top 20 de/en\n");
    let mut zl = String::new();
    let mut cosl = String::from("de\ten\tcos(de,en)\tTop20 de∩en\tTop20 mix∩en\tTop20 mix∩de\n");
    for (qi, line) in tsv.lines().enumerate() {
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 2 || c[0].starts_with('#') {
            continue;
        }
        let mut lists = Vec::new();
        // how close the multilingual encoder puts the German text to the English one (cosine)
        let (qd, qe) = (txt_enc.embed(c[0]), txt_enc.embed(c[1]));
        let cos: f32 = qd.iter().zip(&qe).map(|(a, b)| a * b).sum();
        // mean of both query vectors: top-20 overlap with the English list
        let mix: Vec<f32> = { let m: Vec<f32> = qd.iter().zip(&qe).map(|(a, b)| a + b).collect(); let n = m.iter().map(|x| x * x).sum::<f32>().sqrt(); m.iter().map(|x| x / n).collect() };
        let lmix = scored(&vecs, None, &mix, 500);
        let len = scored(&vecs, None, &qe, 500);
        let ld = scored(&vecs, None, &qd, 500);
        let top = |l: &[library::similar::Scored]| -> Vec<i64> { l.iter().take(20).map(|s| s.file).collect() };
        let (te, td, tm) = (top(&len), top(&ld), top(&lmix));
        let ov = |a: &[i64], b: &[i64]| a.iter().filter(|x| b.contains(x)).count();
        cosl.push_str(&format!("{}\t{}\t{:.3}\t{}\t{}\t{}\n", c[0], c[1], cos, ov(&td, &te), ov(&tm, &te), ov(&tm, &td)));
        for (lang, text) in [("de", c[0]), ("en", c[1])] {
            let q = txt_enc.embed(text);
            let l = scored(&vecs, None, &q, 500);
            let z = |i: usize| l.get(i).map_or(f32::NAN, |s| s.z);
            summary.push_str(&format!("{}\t{lang}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t", text, z(0), z(9), z(19), z(49), z(l.len() / 2)));
            for s in levels {
                summary.push_str(&format!("{}\t", keep(&l, s, vecs.count())));
            }
            zl.push_str(&format!("{text}\t{lang}\t{}\n", l.iter().take(100).map(|s| format!("{:.2}", s.z)).collect::<Vec<_>>().join(" ")));
            lists.push(l);
            summary.push_str(if lang == "de" { "\t\n" } else { "" });
            if lang == "en" {
                let (d, e) = (top_names(&lists[0], 20), top_names(&lists[1], 20));
                let ov = d.iter().filter(|n| e.contains(n)).count();
                // replace the trailing empty cell of the de line is not needed: overlap is on the en line
                summary.push_str(&format!("{ov}/20\n"));
            }
        }
        // contact sheet: top 12 de (upper half), top 12 en (lower half)
        let cell = 160u32;
        let mut sheet = image::RgbImage::from_pixel(cell * 12, cell * 2, image::Rgb([20, 20, 20]));
        for (row, l) in lists.iter().enumerate() {
            for (k, s) in l.iter().take(12).enumerate() {
                let (p, _) = &names[s.file as usize];
                let fmt = library::format_from_extension(&p.to_string_lossy()).unwrap_or("JPG");
                if let Ok((img, _)) = library::preview::load(p, fmt, cell) {
                    let t = image::imageops::thumbnail(&img, cell, cell);
                    image::imageops::replace(&mut sheet, &t, (k as u32 * cell) as i64, (row as u32 * cell) as i64);
                }
            }
        }
        let fname = out.join(format!("q{qi:02}.png"));
        sheet.save(&fname)?;
        println!("\n[{qi:02}] „{}“ / „{}“  (oben de, unten en) → {}", c[0], c[1], fname.display());
        for (lang, l) in ["de", "en"].iter().zip(&lists) {
            println!("  {lang}: {}", top_names(l, 12).join(" "));
            println!("  {lang} z: {}", l.iter().take(12).map(|s| format!("{:.1}", s.z)).collect::<Vec<_>>().join(" "));
        }
    }
    std::fs::write(out.join("zlists.tsv"), &zl)?;
    println!("\n{cosl}");
    std::fs::write(out.join("language-compare.tsv"), &cosl)?;
    println!("Thresholds per strictness (z lower bound, distance) at N={}: {:?}", vecs.count(), levels.map(|s| limits(s, vecs.count())));
    println!("\n{summary}");
    std::fs::write(out.join("summary.tsv"), &summary)?;
    Ok(())
}

/// `graineo-dev semantic_search-fuellen`: image vectors for all photos of the test library
/// (`GRAINEO_DATA_DIR`) that have none.
pub fn fill(_a: &[String]) -> Result<()> {
    anyhow::ensure!(std::env::var_os("GRAINEO_DATA_DIR").is_some(), "only with GRAINEO_DATA_DIR (test folder)");
    let db = library::db::Db::standard().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let enc = ImageEncoder::load(&library::semantic::image_folder().context("image encoder missing")?).map_err(anyhow::Error::msg)?;
    let mut n = 0;
    loop {
        let list = library::similar::without_vector(&db, 32);
        if list.is_empty() {
            break;
        }
        for (file, path, format) in list {
            let v = library::preview::load(&path, &format, 512).ok().map(|(img, _)| enc.embed(&img)).unwrap_or_else(|| vec![0.0; 512]);
            library::similar::save(&db, file, &v).map_err(anyhow::Error::msg)?;
            n += 1;
        }
    }
    println!("{n} vectors computed");
    Ok(())
}
