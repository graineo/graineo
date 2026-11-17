//! `graineo-dev content-keywords <output_folder> <labels.tsv> <folder|file>… [--template=T]… [--threshold=P]`
//! (`--template="a|b"`: the vectors of both templates averaged)
//!
//! Measures the keyword suggestions from the picture content (`library::content_keywords`): every photo is read as
//! the search does (preview of 512 px → SigLIP 2 Base image vector), every vocabulary term through one or more
//! prompt templates (`{}` = the term's noun phrase). `labels.tsv`: `file name<TAB>term id, term id, …` with the
//! terms that really apply (term ids = English names of the vocabulary); an empty list means "nothing should be
//! suggested" (synthetic pictures). Output: `report.txt` (ranking quality per template, a threshold sweep, the
//! suggestions per photo at the chosen threshold) and the 512 px previews as JPEG (to label them by eye).
//! The model comes from the installed module or `GRAINEO_HF_CACHE`.
use anyhow::{bail, Context, Result};
use library::content_keywords::{self as ck, TermVectors};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
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

/// Labels per file name (without folder).
fn labels(p: &Path) -> Result<HashMap<String, HashSet<String>>> {
    let ids: HashSet<&str> = ck::terms().iter().map(|t| t.id).collect();
    let mut out = HashMap::new();
    for l in std::fs::read_to_string(p).with_context(|| p.display().to_string())?.lines() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (name, list) = l.split_once('\t').unwrap_or((l, ""));
        let set: HashSet<String> = list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
        for t in &set {
            if !ids.contains(t.as_str()) {
                bail!("{name}: „{t}“ is not a term of the vocabulary");
            }
        }
        out.insert(name.trim().to_string(), set);
    }
    Ok(out)
}

struct Photo {
    name: String,
    vector: Vec<f32>,
    truth: Option<HashSet<String>>,
}

/// Average precision of a ranking against the true set (over the whole vocabulary).
fn average_precision(order: &[usize], truth: &HashSet<String>) -> f64 {
    if truth.is_empty() {
        return f64::NAN;
    }
    let (mut hits, mut sum) = (0usize, 0f64);
    for (rank, &i) in order.iter().enumerate() {
        if truth.contains(ck::terms()[i].id) {
            hits += 1;
            sum += hits as f64 / (rank + 1) as f64;
        }
    }
    sum / truth.len() as f64
}

pub fn run(a: &[String]) -> Result<()> {
    let mut templates: Vec<String> = Vec::new();
    let mut threshold = ck::THRESHOLD;
    let mut pos: Vec<&String> = Vec::new();
    for x in a {
        if let Some(t) = x.strip_prefix("--template=") {
            templates.push(t.to_string());
        } else if let Some(t) = x.strip_prefix("--threshold=") {
            threshold = t.parse().context("--threshold")?;
        } else {
            pos.push(x);
        }
    }
    if pos.len() < 3 {
        bail!("content-keywords <output_folder> <labels.tsv> <folder|file>… [--template=\"a photo of {{}}.\"]… [--threshold=0.02]");
    }
    if templates.is_empty() {
        templates.push(ck::TEMPLATES.join("|"));
    }
    let out = PathBuf::from(pos[0]);
    std::fs::create_dir_all(&out)?;
    let truth = labels(Path::new(pos[1]))?;
    let mut files = Vec::new();
    for p in &pos[2..] {
        collect(Path::new(p), &mut files);
    }
    let dir = ck::model_folder().context("SigLIP 2 Base missing (module or GRAINEO_HF_CACHE)")?;
    // image vectors (cached in the output folder: name<TAB>hex-free f32 in vectors.bin)
    let tower = library::siglip::ImageTower::load(&dir).map_err(anyhow::Error::msg)?;
    let dim = tower.dim();
    let mut photos: Vec<Photo> = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
        let cache = out.join(format!("{name}.vec"));
        let vector = match std::fs::read(&cache).ok().filter(|b| b.len() == dim * 4) {
            Some(b) => b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
            None => {
                let fmt = library::format_from_extension(&name).unwrap_or("JPG");
                let img = match library::preview::load(f, fmt, 512) {
                    Ok((img, _)) => img,
                    Err(e) => {
                        eprintln!("skipped {name}: {e}");
                        continue;
                    }
                };
                img.save(out.join(format!("{name}.jpg")))?;
                let v = tower.embed(&[&img]).remove(0);
                std::fs::write(&cache, v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())?;
                v
            }
        };
        photos.push(Photo { truth: truth.get(&name).cloned(), name, vector });
    }
    drop(tower);
    let mut report = String::new();
    writeln!(report, "content keywords: {} terms, {} photos ({} labelled), threshold {threshold}, at most {}", ck::terms().len(), photos.len(), photos.iter().filter(|p| p.truth.is_some()).count(), ck::MAX_SUGGESTIONS)?;
    let thresholds = [0.001f32, 0.002, 0.005, 0.01, 0.02, 0.03, 0.05, 0.1, 0.2];
    for template in &templates {
        // several templates separated by "|" are averaged (as in the app with more than one template)
        let texts: Vec<Vec<String>> = ck::terms().iter().map(|t| template.split('|').map(|x| x.replace("{}", t.prompt)).collect()).collect();
        let t0 = std::time::Instant::now();
        let tv = TermVectors::compute(&dir, &texts).map_err(anyhow::Error::msg)?;
        writeln!(report, "\n=== template „{template}“ (text vectors {:.1} s, scale {:.2}, bias {:.2})", t0.elapsed().as_secs_f64(), tv.scale, tv.bias)?;
        // ranking quality without threshold
        let (mut ap, mut p1, mut p3, mut p5, mut n) = (0f64, 0f64, 0f64, 0f64, 0usize);
        let probs: Vec<Vec<f32>> = photos.iter().map(|p| tv.probabilities(&p.vector)).collect();
        for (p, pr) in photos.iter().zip(&probs) {
            let Some(t) = p.truth.as_ref().filter(|t| !t.is_empty() && !p.name.to_ascii_lowercase().ends_with(".png")) else { continue };
            let mut order: Vec<usize> = (0..pr.len()).collect();
            order.sort_by(|a, b| pr[*b].total_cmp(&pr[*a]));
            let at = |k: usize| order[..k].iter().filter(|i| t.contains(ck::terms()[**i].id)).count() as f64 / k as f64;
            ap += average_precision(&order, t);
            p1 += at(1);
            p3 += at(3);
            p5 += at(5);
            n += 1;
        }
        let n = n.max(1) as f64;
        writeln!(report, "ranking (labelled camera photos): mAP {:.3}, P@1 {:.2}, P@3 {:.2}, P@5 {:.2}", ap / n, p1 / n, p3 / n, p5 / n)?;
        // threshold sweep with the cap
        // photos = camera files; synthetic pictures = PNG (flat, gradient, noise, text): their wrong suggestions count
        writeln!(report, "threshold | suggested | correct | precision | recall | per photo | wrong per synthetic picture")?;
        for th in thresholds.iter().copied().chain([threshold]) {
            let (mut sug, mut ok, mut lab, mut real, mut synth, mut synth_sug) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
            for (p, pr) in photos.iter().zip(&probs) {
                let Some(t) = &p.truth else { continue };
                let mut v: Vec<(usize, f32)> = pr.iter().copied().enumerate().filter(|(_, x)| *x >= th).collect();
                v.sort_by(|a, b| b.1.total_cmp(&a.1));
                v.truncate(ck::MAX_SUGGESTIONS);
                if p.name.to_ascii_lowercase().ends_with(".png") {
                    synth += 1;
                    synth_sug += v.iter().filter(|(i, _)| !t.contains(ck::terms()[*i].id)).count();
                } else if !t.is_empty() {
                    real += 1;
                    sug += v.len();
                    ok += v.iter().filter(|(i, _)| t.contains(ck::terms()[*i].id)).count();
                    lab += t.len();
                }
            }
            writeln!(
                report,
                "{th:>9} | {sug:>9} | {ok:>7} | {:>9.2} | {:>6.2} | {:>9.1} | {:>27.2}",
                ok as f64 / sug.max(1) as f64,
                ok as f64 / lab.max(1) as f64,
                sug as f64 / real.max(1) as f64,
                synth_sug as f64 / synth.max(1) as f64
            )?;
        }
        // per photo: top 10 and the suggestions at the threshold
        for (p, pr) in photos.iter().zip(&probs) {
            let mut order: Vec<usize> = (0..pr.len()).collect();
            order.sort_by(|a, b| pr[*b].total_cmp(&pr[*a]));
            let mark = |i: usize| match &p.truth {
                Some(t) if t.contains(ck::terms()[i].id) => "+",
                Some(_) => "-",
                None => "?",
            };
            let top: Vec<String> = order[..10].iter().map(|&i| format!("{}{} {:.3}", mark(i), ck::terms()[i].id, pr[i])).collect();
            writeln!(report, "\n{}: {}", p.name, top.join(", "))?;
            let chosen: Vec<usize> = order.iter().copied().filter(|i| pr[*i] >= threshold).take(ck::MAX_SUGGESTIONS).collect();
            if let Some(t) = &p.truth {
                let good: Vec<&str> = chosen.iter().filter(|i| t.contains(ck::terms()[**i].id)).map(|i| ck::terms()[*i].id).collect();
                let bad: Vec<&str> = chosen.iter().filter(|i| !t.contains(ck::terms()[**i].id)).map(|i| ck::terms()[*i].id).collect();
                let missed: Vec<&str> = t.iter().map(String::as_str).filter(|x| !good.contains(x)).collect();
                writeln!(report, "  at {threshold}: correct [{}] wrong [{}] missed [{}]", good.join(", "), bad.join(", "), missed.join(", "))?;
            }
        }
    }
    std::fs::write(out.join("report.txt"), &report)?;
    print!("{report}");
    Ok(())
}
