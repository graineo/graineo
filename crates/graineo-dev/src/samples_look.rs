//! `graineo-dev samples-look --samples DIR|LIST [--source-name NAME] [--source-url URL] [--license SPDX]
//! [--retrieved YYYY-MM-DD] [--key camera/style] [--include-adaptive] [--out DIR] [--check CANDIDATES.tsv]
//! [--check-key camera/style] [--own FILE.x3look]`:
//! a starting look for a camera from sample files of other photographers (for example the public-domain samples of
//! raw.pixls.us), fitted with the code of "Learn this camera's look" (`raw::learn_look::learn_from_samples`: matrix
//! and curves, no table) on the camera preview embedded in each sample. The look is marked `"origin": "samples"`,
//! `"quality": "unchecked"` and carries the source (name, URL, licence, files).
//!
//! A `--check-key` ending in `*` matches every key that starts with it (all fine settings of a style).
//! Without `--samples`, only the tone curve matched per photo is measured on the `--check` photos of `--check-key`.
//!
//! `--samples`: a folder (every RAW in it) or a list file (one path per line). The samples are only read.
//! `--check CANDIDATES.tsv` (`RAW<TAB>DAY` per line, the user's own photos, only read): measures the look on these
//! photos of the same camera and style (or `--check-key`) against the plain development, the current standard
//! development, the other stages fitted on the samples, an own checked look (`--own`, else the bundled look of the
//! base style if there is one) and a tone curve matched per photo to its own camera rendering
//! (`raw::learn_look::matched_curve`). Decision rule as everywhere: [`raw::learn_look::clearly_better`].
//!
//! Output (stdout, tab-separated):
//! - `SAMPLE key name preview_w preview_h adaptive`, `SKIP name reason`
//! - `PHOTO key name pairs correlation exposure_offset` (samples), `CHECKPHOTO …` (check photos)
//! - `FIT key part photos blocks median p90 p95 mean` (`part`: samples-neutral, samples-look, loo-neutral, loo-look)
//! - `METRIC key variant photos blocks median p90 p95 mean` (check photos)
//! - `DECISION key variant against better|not better (median, p90, photos better)`
//! - `RESULT key look|none reason`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use raw::learn_look::{self as ll, Metrics, Photo};
use rawdev::learn::{self, Candidate};

const RAW_EXT: [&str; 9] = ["raf", "dng", "cr2", "cr3", "arw", "nef", "orf", "rw2", "pef"];

/// One photo with its block pairs and the matched tone curves (plain and current development) for the check.
struct Extracted {
    photo: Photo,
    curve_plain: Option<[f32; ll::MATCH_KNOTS]>,
    curve_current: Option<[f32; ll::MATCH_KNOTS]>,
    /// as the app shows it (no exposure matching, `learn::matched_views`): pairs with `current` = the matched look,
    /// and pairs with `current` = the standard development; `base` = the plain development in both
    app: Option<(Photo, Photo)>,
}

fn name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn extract(path: &Path, day: &str) -> Result<Extracted, String> {
    let r = learn::render_photo(path, false, &|| true)?.ok_or("cancelled")?;
    let pairs = ll::block_pairs(&r.base, Some(&r.current), &r.reference, &r.alignment);
    let warped = ll::warp_reference(r.base.width(), r.base.height(), &r.reference, &r.alignment);
    let photo = Photo {
        name: name(path),
        day: day.to_string(),
        folder: path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
        source: Some(r.source),
        exposure_offset: r.exposure_offset,
        correlation: r.alignment.correlation,
        pairs,
    };
    Ok(Extracted { photo, curve_plain: ll::matched_curve(&r.base, &warped), curve_current: ll::matched_curve(&r.current, &warped), app: None })
}

fn metric_line(key: &str, tag: &str, variant: &str, m: &Metrics) {
    println!("{tag}\t{key}\t{variant}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}", m.photos, m.blocks, m.median, m.p90, m.p95, m.mean);
}

fn decision(key: &str, variant: &str, new: &Metrics, against: &str, old: &Metrics) {
    let n = new.per_photo.len().min(old.per_photo.len());
    let better = new.per_photo.iter().zip(&old.per_photo).filter(|(a, b)| a < b).count();
    let gain = if old.median > 0.0 { 100.0 * (1.0 - new.median / old.median) } else { 0.0 };
    println!(
        "DECISION\t{key}\t{variant}\t{against}\t{}\tmedian {:.2} → {:.2} ({gain:+.1} %), p90 {:.2} → {:.2}, photos better {better}/{n}",
        if ll::clearly_better(new, old) { "better" } else { "not better" },
        old.median,
        new.median,
        old.p90,
        new.p90
    );
}

pub fn run(args: &[String]) -> Result<()> {
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let include_adaptive = args.iter().any(|a| a == "--include-adaptive");
    let only = value("--key");
    let out = value("--out").map(PathBuf::from);
    // without samples: only the tone curve matched per photo is measured on the check photos
    let Some(samples_arg) = value("--samples") else {
        let list = value("--check").context("--samples DIR|LIST or --check CANDIDATES.tsv with --check-key missing")?;
        let key = value("--check-key").context("--check-key camera/style missing")?;
        return check(&key, &list, &key, include_adaptive, None, value("--own").as_deref());
    };
    let samples: Vec<PathBuf> = {
        let p = PathBuf::from(&samples_arg);
        let mut v: Vec<PathBuf> = if p.is_dir() {
            std::fs::read_dir(&p)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|f| f.extension().is_some_and(|e| RAW_EXT.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))).collect()
        } else {
            std::fs::read_to_string(&p)?.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).map(|l| PathBuf::from(l.split('\t').next().unwrap_or(""))).collect()
        };
        v.sort();
        v
    };
    let today = {
        let t = library::platform::local_time(library::now());
        format!("{:04}-{:02}-{:02}", t.year, t.month, t.tag)
    };
    // samples by camera and style
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for p in &samples {
        let c = Candidate { path: p.clone(), day: String::new(), folder: String::new(), camera: String::new(), thumb: None };
        let Some(s) = learn::scan(&c) else {
            println!("SKIP\t{}\tno learnable picture style", name(p));
            continue;
        };
        let (w, h) = learn::reference(p).map(|(i, _)| (i.width(), i.height())).unwrap_or((0, 0));
        println!("SAMPLE\t{}\t{}\t{w}\t{h}\t{}", s.key(), name(p), s.adaptive.unwrap_or("-"));
        if s.adaptive.is_some() && !include_adaptive {
            println!("SKIP\t{}\tadaptive camera setting {}", name(p), s.adaptive.unwrap_or(""));
            continue;
        }
        groups.entry(s.key()).or_default().push(p.clone());
    }
    for (key, paths) in &groups {
        if only.as_ref().is_some_and(|k| k != key) {
            continue;
        }
        let (camera, style) = key.split_once('/').unwrap_or((key, ""));
        let mut photos = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            // every sample counts as its own "day" (they are separate shots of other photographers)
            match extract(p, &format!("sample-{i}")) {
                Ok(e) => {
                    println!("PHOTO\t{key}\t{}\t{}\t{:.3}\t{:+.2}", e.photo.name, e.photo.pairs.len(), e.photo.correlation, e.photo.exposure_offset);
                    photos.push(e.photo);
                }
                Err(e) => println!("SKIP\t{}\t{e}", name(p)),
            }
        }
        let files: Vec<String> = photos.iter().map(|p| p.name.clone()).collect();
        let source = serde_json::json!({
            "name": value("--source-name").unwrap_or_else(|| "raw.pixls.us".into()),
            "url": value("--source-url").unwrap_or_default(),
            "license": value("--license").unwrap_or_else(|| "CC0-1.0".into()),
            "files": files,
            "retrieved": value("--retrieved").unwrap_or_else(|| today.clone()),
        });
        let o = ll::learn_from_samples(&photos, camera, style, &today, source);
        metric_line(key, "FIT", "samples-neutral", &o.fit_neutral);
        metric_line(key, "FIT", "samples-look", &o.fit_look);
        if let Some((n, l)) = &o.leave_one_out {
            metric_line(key, "FIT", "loo-neutral", n);
            metric_line(key, "FIT", "loo-look", l);
        }
        match (&o.look, &o.refused) {
            (Some(look), _) => {
                println!("RESULT\t{key}\tlook\t{} sample photos, stage {}", o.photos, ll::SAMPLES_STAGE.key());
                if let Some(d) = &out {
                    let p = d.join(camera).join(format!("{style}.x3look"));
                    std::fs::create_dir_all(p.parent().unwrap())?;
                    std::fs::write(&p, look.to_bytes().map_err(anyhow::Error::msg)?)?;
                }
            }
            (None, r) => println!("RESULT\t{key}\tnone\t{}", r.clone().unwrap_or_default()),
        }
        if let (Some(list), Some(models)) = (value("--check"), &o.models) {
            check(key, &list, value("--check-key").as_deref().unwrap_or(key), include_adaptive, Some(models), value("--own").as_deref())?;
        }
    }
    Ok(())
}

/// Measure the sample look on the user's own photos (see the module documentation).
fn check(key: &str, list: &str, check_key: &str, include_adaptive: bool, models: Option<&[ll::Model; 3]>, own: Option<&str>) -> Result<()> {
    let mut cands = Vec::new();
    for l in std::fs::read_to_string(list)?.lines() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let t: Vec<&str> = l.split('\t').collect();
        let path = PathBuf::from(t[0]);
        let c = Candidate { path: path.clone(), day: t.get(1).map(|s| s.to_string()).unwrap_or_default(), folder: String::new(), camera: String::new(), thumb: None };
        if let Some(s) = learn::scan(&c) {
            if (s.key() == check_key || check_key.strip_suffix('*').is_some_and(|k| s.key().starts_with(k))) && (include_adaptive || s.adaptive.is_none()) {
                cands.push(c);
            }
        }
    }
    let keys: Vec<(String, String)> = cands.iter().map(|c| (c.day.clone(), name(&c.path))).collect();
    let chosen = ll::select(&keys, ll::MAX_PHOTOS);
    eprintln!("{key}: {} check candidates of {check_key}, {} chosen", cands.len(), chosen.len());
    let mut ex = Vec::new();
    for (n, &i) in chosen.iter().enumerate() {
        eprint!("\rcheck {}/{}   ", n + 1, chosen.len());
        match extract(&cands[i].path, &cands[i].day) {
            Ok(mut e) if e.photo.pairs.len() >= 20 => {
                match learn::matched_views(&cands[i].path, &|| true) {
                    Ok(Some(([plain, matched, standard], reference, a))) => {
                        let photo = |pairs| Photo { name: e.photo.name.clone(), day: e.photo.day.clone(), pairs, ..Default::default() };
                        e.app = Some((photo(ll::block_pairs(&plain, Some(&matched), &reference, &a)), photo(ll::block_pairs(&plain, Some(&standard), &reference, &a))));
                    }
                    Ok(None) => {}
                    Err(err) => println!("APPSKIP\t{}\t{err}", e.photo.name),
                }
                println!("CHECKPHOTO\t{key}\t{}\t{}\t{}\t{:.3}\t{:+.2}", e.photo.name, e.photo.day, e.photo.pairs.len(), e.photo.correlation, e.photo.exposure_offset);
                ex.push(e);
            }
            Ok(e) => println!("SKIP\t{}\tonly {} blocks", e.photo.name, e.photo.pairs.len()),
            Err(e) => println!("SKIP\t{}\t{e}", name(&cands[i].path)),
        }
    }
    eprintln!();
    let photos: Vec<&Photo> = ex.iter().map(|e| &e.photo).collect();
    let mut days: Vec<&str> = photos.iter().map(|p| p.day.as_str()).collect();
    days.sort_unstable();
    days.dedup();
    println!("CHECKSET\t{key}\t{check_key}\t{} photos\t{} days", photos.len(), days.len());
    if photos.is_empty() {
        return Ok(());
    }
    let neutral = ll::measure(&photos, &|q| q.base);
    let current = ll::measure(&photos, &|q| q.current);
    let mut variants: Vec<(String, Metrics)> = vec![("neutral".into(), neutral.clone()), ("current".into(), current.clone())];
    for (stage, m) in ["matrix", "curves", "table"].iter().zip(models.into_iter().flatten()) {
        let look = m.bake(key, r#"{"interpolation":"tetrahedral"}"#);
        variants.push((format!("samples-{stage}"), ll::measure(&photos, &|q| look.pixel(q.base))));
    }
    let own_look = match own {
        Some(f) => raw::looks::Look::file(Path::new(f)),
        None => {
            let (camera, style) = check_key.split_once('/').unwrap_or((check_key, ""));
            raw::looks::Look::built_in(&format!("{camera}/{}", style.split('_').next().unwrap_or("")))
        }
    };
    if let Some(look) = &own_look {
        variants.push(("own".into(), ll::measure(&photos, &|q| look.pixel(q.base))));
    }
    // tone curve matched per photo to its own camera rendering (on the plain and on the current development)
    let per_photo = |current: bool, luminance: bool| {
        ll::metrics_of(
            ex.iter()
                .map(|e| {
                    let c = if current { e.curve_current } else { e.curve_plain };
                    e.photo
                        .pairs
                        .iter()
                        .map(|q| {
                            let x = if current { q.current } else { q.base };
                            let y = match (c, luminance) {
                                (Some(c), false) => ll::apply_matched_curve(&c, x),
                                (Some(c), true) => ll::apply_matched_curve_luminance(&c, x),
                                (None, _) => x,
                            };
                            ll::de2000(ll::srgb_to_lab(y), ll::srgb_to_lab(q.reference))
                        })
                        .collect()
                })
                .collect(),
        )
    };
    variants.push(("matched-curve".into(), per_photo(false, false)));
    variants.push(("matched-curve-luminance".into(), per_photo(false, true)));
    variants.push(("current+matched-curve".into(), per_photo(true, false)));
    variants.push(("current+matched-curve-luminance".into(), per_photo(true, true)));
    println!("CURVES\t{key}\t{} of {} photos with a matched curve", ex.iter().filter(|e| e.curve_plain.is_some()).count(), ex.len());
    // as the app shows it: default settings, no exposure matching (only photos where the matched look was computed)
    let app: Vec<&(Photo, Photo)> = ex.iter().filter_map(|e| e.app.as_ref()).filter(|(m, _)| m.pairs.len() >= 20).collect();
    println!("APPSET\t{key}\t{} of {} photos", app.len(), ex.len());
    if !app.is_empty() {
        let matched: Vec<&Photo> = app.iter().map(|(m, _)| m).collect();
        let standard: Vec<&Photo> = app.iter().map(|(_, c)| c).collect();
        let plain = ll::measure(&matched, &|q| q.base);
        let m = ll::measure(&matched, &|q| q.current);
        let c = ll::measure(&standard, &|q| q.current);
        metric_line(key, "METRIC", "app-plain", &plain);
        metric_line(key, "METRIC", "app-current", &c);
        metric_line(key, "METRIC", "app-matched", &m);
        decision(key, "app-matched", &m, "app-plain", &plain);
        decision(key, "app-matched", &m, "app-current", &c);
    }
    for (v, m) in &variants {
        metric_line(key, "METRIC", v, m);
    }
    for (v, m) in &variants[2..] {
        decision(key, v, m, "neutral", &neutral);
        decision(key, v, m, "current", &current);
    }
    Ok(())
}
