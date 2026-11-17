//! Measuring the depth and sky masks (`segment::depth`, `segment::sky`) on test pictures (PNG/JPEG copies, upright).
//!
//! - `graineo-dev scene-depth <out_folder> <model_folder> <picture>… [--short=518]`: depth per picture as a grey PNG
//!   (`<name>-depth.png`, black = near, white = far, ranks) and the times (load + run).
//! - `graineo-dev scene-depth-compare <model_a> <model_b> <picture>…`: agreement of two depth models per picture
//!   (Spearman rank correlation of the disparities on a 64 × 64 grid, mean absolute rank difference) and times.
//! - `graineo-dev scene-sky <out_folder> <sam_folder> <picture>… [--depth=<model_folder>] [--siglip] [--ref=<folder>]`:
//!   automatic sky selection; writes `<name>-sky.png` (weights) and `<name>-sky-overlay.jpg`; with `--ref` the IoU
//!   against `<folder>/<name>.png` (reference mask, white = sky; a missing file = no sky in the picture). `--siglip`:
//!   confirm with SigLIP 2 Base (installed module or `GRAINEO_HF_CACHE`).
//! - `graineo-dev scene-segments <out_folder> <model_folder> <picture>… [--step=14] [--span=…]`: the segments of the
//!   depth mask (`segment::segments`); writes `<name>-segments.png` (borders over the picture, tinted by depth class:
//!   near orange, middle green, far blue) and prints the number of segments and the times.
//! - `graineo-dev mask-refine <out_folder> <picture> <mask.png> <ref.png> [--amount=1] [--degrade=8]`: "Kante
//!   verfeinern" measured: IoU and boundary F-score (boundaries within 2 px) of the mask against the reference, plain,
//!   with the own edge snap (masks of an image analysis) and with the guided filter (drawn masks); `--degrade=n` first
//!   coarsens the mask as a model of 1/n of the size would (down, bilinear up). Writes crops `<name>-refine-*.png`
//!   around the worst-fitting boundary part.
//! - `graineo-dev mask-ref <picture> <out.png> <rule> [--above=x,y;x,y;…] [--below=…]`: reference mask for the
//!   measurement, built by hand: a polyline (normalised 0..1, left to right) of the skyline and a colour rule inside
//!   (`all`, `bright=<0..255>`, `bluish`, `dark=<0..255>` = excluded dark pixels such as branches).
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use image::{GrayImage, RgbImage};

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().find_map(|a| a.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
}

fn pictures(args: &[String]) -> Vec<PathBuf> {
    args.iter().filter(|a| !a.starts_with("--")).map(PathBuf::from).collect()
}

fn ranks_image(w: u32, h: u32, r: &[u16]) -> GrayImage {
    GrayImage::from_fn(w, h, |x, y| image::Luma([(r[(y * w + x) as usize] >> 8) as u8]))
}

pub fn depth(args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("output folder missing")?);
    let model = PathBuf::from(args.get(1).context("model folder missing")?);
    let short: u32 = opt(args, "--short").map_or(Ok(segment::depth::SHORT_SIDE), str::parse)?;
    std::fs::create_dir_all(&out)?;
    for p in pictures(&args[2..]) {
        let img = image::open(&p).with_context(|| p.display().to_string())?.to_rgb8();
        let t = Instant::now();
        let mut load = 0.0;
        let e = segment::depth::estimate_steps(&model, &img, short, &mut |s| {
            if s == segment::Step::Run {
                load = t.elapsed().as_secs_f64();
            }
            true
        })
        .map_err(anyhow::Error::msg)?;
        let total = t.elapsed().as_secs_f64();
        let r = segment::depth::nearness_ranks(&e.disparity);
        ranks_image(e.width, e.height, &r).save(out.join(format!("{}-depth.png", stem(&p))))?;
        println!("{}\t{}×{}\tload {:.2} s\trun {:.2} s", stem(&p), e.width, e.height, load, total - load);
    }
    Ok(())
}

/// Picture with the segment borders drawn white and each segment tinted by its depth class.
pub fn segments_image(img: &RgbImage, s: &segment::segments::Segments) -> RgbImage {
    let w = s.width as usize;
    RgbImage::from_fn(s.width, s.height, |x, y| {
        let i = y as usize * w + x as usize;
        let l = s.labels[i];
        let border = (x + 1 < s.width && s.labels[i + 1] != l) || ((y + 1) < s.height && s.labels[i + w] != l);
        if border {
            return image::Rgb([255, 255, 255]);
        }
        let tint = match masks::raster::split_label(l).map(|c| c.0) {
            Some(masks::DepthClass::Near) => [255.0, 140.0, 0.0],
            Some(masks::DepthClass::Middle) => [60.0, 200.0, 60.0],
            _ => [40.0, 110.0, 255.0],
        };
        let p = img.get_pixel(x, y);
        image::Rgb([0, 1, 2].map(|c| (p[c] as f32 * 0.6 + tint[c] * 0.4) as u8))
    })
}

pub fn segments(args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("output folder missing")?);
    let model = PathBuf::from(args.get(1).context("model folder missing")?);
    let mut params = segment::segments::Params::default();
    if let Some(v) = opt(args, "--step") {
        params.step = v.parse()?;
    }
    std::fs::create_dir_all(&out)?;
    for p in pictures(&args[2..]) {
        let img = image::open(&p).with_context(|| p.display().to_string())?.to_rgb8();
        let t = Instant::now();
        let e = segment::depth::estimate_steps(&model, &img, segment::depth::SHORT_SIDE, &mut |_| true).map_err(anyhow::Error::msg)?;
        let td = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let ranks = segment::depth::nearness_ranks(&segment::resize(&e.disparity, e.width as usize, e.height as usize, w, h));
        let s = segment::segments::segment(&img, &ranks, &params);
        let ts = t.elapsed().as_secs_f64();
        segments_image(&img, &s).save(out.join(format!("{}-segments.png", stem(&p))))?;
        let per = |c: masks::DepthClass| s.labels.iter().filter(|&&l| masks::raster::split_label(l).map(|x| x.0) == Some(c)).count() as f64 / s.labels.len() as f64 * 100.0;
        println!(
            "{}	{w}×{h}	depth {td:.2} s	segments {ts:.2} s	{} segments	near {:.0} % middle {:.0} % far {:.0} %",
            stem(&p),
            s.count(),
            per(masks::DepthClass::Near),
            per(masks::DepthClass::Middle),
            per(masks::DepthClass::Far)
        );
    }
    Ok(())
}

/// Spearman correlation of two value sets (ranks without tie correction).
fn spearman(a: &[f32], b: &[f32]) -> f64 {
    let rank = |v: &[f32]| -> Vec<f64> {
        let mut i: Vec<usize> = (0..v.len()).collect();
        i.sort_by(|&x, &y| v[x].total_cmp(&v[y]));
        let mut r = vec![0.0; v.len()];
        for (k, &j) in i.iter().enumerate() {
            r[j] = k as f64;
        }
        r
    };
    let (ra, rb) = (rank(a), rank(b));
    let n = a.len() as f64;
    let d2: f64 = ra.iter().zip(&rb).map(|(x, y)| (x - y) * (x - y)).sum();
    1.0 - 6.0 * d2 / (n * (n * n - 1.0))
}

fn grid(e: &segment::depth::Estimate, n: u32) -> Vec<f32> {
    let small = segment::resize(&e.disparity, e.width as usize, e.height as usize, n as usize, n as usize);
    small
}

pub fn depth_compare(args: &[String]) -> Result<()> {
    let a = PathBuf::from(args.first().context("model a missing")?);
    let b = PathBuf::from(args.get(1).context("model b missing")?);
    println!("picture\tspearman\tmean |rank diff| %\ttime a s\ttime b s");
    for p in pictures(&args[2..]) {
        let img = image::open(&p)?.to_rgb8();
        let t = Instant::now();
        let ea = segment::depth::estimate_steps(&a, &img, segment::depth::SHORT_SIDE, &mut |_| true).map_err(anyhow::Error::msg)?;
        let ta = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let eb = segment::depth::estimate_steps(&b, &img, segment::depth::SHORT_SIDE, &mut |_| true).map_err(anyhow::Error::msg)?;
        let tb = t.elapsed().as_secs_f64();
        let (ga, gb) = (grid(&ea, 64), grid(&eb, 64));
        let (ra, rb) = (segment::depth::nearness_ranks(&ga), segment::depth::nearness_ranks(&gb));
        let diff: f64 = ra.iter().zip(&rb).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / ra.len() as f64 / 655.35;
        println!("{}\t{:.3}\t{:.1}\t{:.2}\t{:.2}", stem(&p), spearman(&ga, &gb), diff, ta, tb);
    }
    Ok(())
}

/// Intersection over union of two weight maps (> 0.5 = selected); `None` when both are empty.
pub fn iou(a: &[f32], b: &[f32]) -> Option<f64> {
    let (mut i, mut u) = (0usize, 0usize);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x > 0.5, *y > 0.5);
        i += usize::from(x && y);
        u += usize::from(x || y);
    }
    (u > 0).then(|| i as f64 / u as f64)
}

fn overlay(img: &RgbImage, w: &[f32]) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y);
        let a = w[(y * img.width() + x) as usize] * 0.55;
        image::Rgb([(p[0] as f32 * (1.0 - a) + 255.0 * a) as u8, (p[1] as f32 * (1.0 - a)) as u8, (p[2] as f32 * (1.0 - a)) as u8])
    })
}

pub fn sky(args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("output folder missing")?);
    let sam = PathBuf::from(args.get(1).context("MobileSAM folder missing")?);
    let depth = opt(args, "--depth").map(PathBuf::from);
    let reference = opt(args, "--ref").map(PathBuf::from);
    let siglip = args.iter().any(|a| a == "--siglip");
    std::fs::create_dir_all(&out)?;
    let scorer = if siglip {
        let dir = library::content_keywords::model_folder().context("SigLIP 2 Base not installed (or GRAINEO_HF_CACHE)")?;
        Some(library::sky_check::Scorer::load(&dir).map_err(anyhow::Error::msg)?)
    } else {
        None
    };
    let mut ious = Vec::new();
    let mut false_sky = 0;
    let mut missed = 0;
    for p in pictures(&args[2..]) {
        let img = image::open(&p)?.to_rgb8();
        let t = Instant::now();
        let nearness = match &depth {
            Some(d) => {
                let e = segment::depth::estimate_steps(d, &img, segment::depth::SHORT_SIDE, &mut |_| true).map_err(anyhow::Error::msg)?;
                Some(segment::sky::Depth::from_estimate(&e))
            }
            None => None,
        };
        let td = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let score = scorer.as_ref().map(|s| move |crop: &RgbImage| s.sky(crop));
        let found = segment::sky::find(&sam, &img, nearness.as_ref(), score.as_ref().map(|f| f as &dyn Fn(&RgbImage) -> f32), &mut |_| true).map_err(anyhow::Error::msg)?;
        let ts = t.elapsed().as_secs_f64();
        let (w, h) = (img.width(), img.height());
        let weights = found.weights.clone().unwrap_or_else(|| vec![0.0; (w * h) as usize]);
        GrayImage::from_fn(w, h, |x, y| image::Luma([(weights[(y * w + x) as usize] * 255.0) as u8])).save(out.join(format!("{}-sky.png", stem(&p))))?;
        overlay(&img, &weights).save(out.join(format!("{}-sky-overlay.jpg", stem(&p))))?;
        let mut line = format!("{}\tdepth {td:.2} s\tsky {ts:.2} s\t{}", stem(&p), found.note);
        if let Some(rf) = &reference {
            let rp = rf.join(format!("{}.png", stem(&p)));
            let has_ref = rp.exists();
            let r: Vec<f32> = if has_ref {
                let g = image::open(&rp)?.to_luma8();
                let g = image::imageops::resize(&g, w, h, image::imageops::FilterType::Triangle);
                g.pixels().map(|v| v[0] as f32 / 255.0).collect()
            } else {
                vec![0.0; (w * h) as usize]
            };
            match (has_ref, found.weights.is_some()) {
                (false, true) => false_sky += 1,
                (true, false) => missed += 1,
                _ => {}
            }
            if has_ref {
                let v = iou(&weights, &r).unwrap_or(0.0);
                ious.push(v);
                line.push_str(&format!("\tIoU {v:.3}"));
            } else {
                line.push_str(if found.weights.is_some() { "\tno sky in the reference: WRONG" } else { "\tno sky: right" });
            }
        }
        println!("{line}");
    }
    if !ious.is_empty() {
        let mean = ious.iter().sum::<f64>() / ious.len() as f64;
        println!("mean IoU over {} pictures with sky: {mean:.3}; sky found where there is none: {false_sky}; sky missed: {missed}", ious.len());
    }
    Ok(())
}

/// Boundary pixels of a mask (> 0.5 with a 4-neighbour ≤ 0.5).
fn boundary(m: &[f32], w: usize, h: usize) -> Vec<bool> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let a = m[i] > 0.5;
            let other = |j: usize| (m[j] > 0.5) != a;
            a && ((x > 0 && other(i - 1)) || (x + 1 < w && other(i + 1)) || (y > 0 && other(i - w)) || (y + 1 < h && other(i + w)))
        })
        .collect()
}

/// Boundary F-score: share of each boundary within `d` pixels of the other (precision, recall), harmonic mean.
pub fn boundary_f(a: &[f32], b: &[f32], w: usize, h: usize, d: usize) -> f64 {
    let (ba, bb) = (boundary(a, w, h), boundary(b, w, h));
    let near = |src: &[bool]| -> Vec<bool> {
        let mut out = vec![false; w * h];
        for i in (0..w * h).filter(|&i| src[i]) {
            let (x, y) = (i % w, i / w);
            for yy in y.saturating_sub(d)..=(y + d).min(h - 1) {
                for xx in x.saturating_sub(d)..=(x + d).min(w - 1) {
                    out[yy * w + xx] = true;
                }
            }
        }
        out
    };
    let (na, nb) = (near(&ba), near(&bb));
    let hit = |s: &[bool], n: &[bool]| {
        let c = s.iter().filter(|v| **v).count();
        if c == 0 { 0.0 } else { s.iter().zip(n).filter(|(a, b)| **a && **b).count() as f64 / c as f64 }
    };
    let (p, r) = (hit(&ba, &nb), hit(&bb, &na));
    if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) }
}

fn gray(path: &Path, w: u32, h: u32) -> Result<Vec<f32>> {
    let g = image::open(path).with_context(|| path.display().to_string())?.to_luma8();
    let g = if g.width() != w || g.height() != h { image::imageops::resize(&g, w, h, image::imageops::FilterType::Triangle) } else { g };
    Ok(g.pixels().map(|v| v[0] as f32 / 255.0).collect())
}

pub fn mask_refine(args: &[String]) -> Result<()> {
    let out = PathBuf::from(args.first().context("output folder missing")?);
    let pic = PathBuf::from(args.get(1).context("picture missing")?);
    let mask = PathBuf::from(args.get(2).context("mask missing")?);
    let reference = PathBuf::from(args.get(3).context("reference missing")?);
    let amount: f32 = opt(args, "--amount").map_or(Ok(1.0), str::parse)?;
    let degrade: u32 = opt(args, "--degrade").map_or(Ok(1), str::parse)?;
    std::fs::create_dir_all(&out)?;
    let img = image::open(&pic)?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let (wu, hu) = (w as usize, h as usize);
    let mut m = gray(&mask, w, h)?;
    if degrade > 1 {
        let (sw, sh) = ((wu / degrade as usize).max(1), (hu / degrade as usize).max(1));
        m = segment::resize(&segment::resize(&m, wu, hu, sw, sh), sw, sh, wu, hu);
    }
    let r = gray(&reference, w, h)?;
    let lin: Vec<f32> = img.as_raw().iter().map(|&v| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }).collect();
    let short = wu.min(hu);
    let t = Instant::now();
    let mut snap = m.clone();
    masks::refine::refine(&mut snap, &lin, wu, hu, short, amount, true);
    let ts = t.elapsed().as_secs_f64();
    let mut guided = m.clone();
    masks::refine::refine(&mut guided, &lin, wu, hu, short, amount, false);
    let name = stem(&pic);
    let line = |label: &str, v: &[f32]| format!("{label} IoU {:.4} BF {:.4}", iou(v, &r).unwrap_or(0.0), boundary_f(v, &r, wu, hu, 2));
    println!("{name}	r {} px	{}	{}	{}	snap {ts:.2} s", masks::refine::radius(amount, short), line("plain", &m), line("snap", &snap), line("guided", &guided));
    // crop around the part of the boundary where snap and plain differ most
    let diff: Vec<f32> = m.iter().zip(&snap).map(|(a, b)| (a - b).abs()).collect();
    let k = (0..wu * hu).max_by(|&a, &b| diff[a].total_cmp(&diff[b])).unwrap_or(0);
    let c = 96u32;
    let (cx, cy) = (((k % wu) as u32).saturating_sub(c / 2).min(w - c), ((k / wu) as u32).saturating_sub(c / 2).min(h - c));
    for (label, v) in [("plain", &m), ("snap", &snap), ("guided", &guided), ("ref", &r)] {
        let o = overlay(&img, v);
        let crop = image::imageops::crop_imm(&o, cx, cy, c, c).to_image();
        image::imageops::resize(&crop, c * 4, c * 4, image::imageops::FilterType::Nearest).save(out.join(format!("{name}-refine-{label}.png")))?;
    }
    Ok(())
}

/// Points of a polyline `x,y;x,y;…` (normalised).
fn polyline(s: &str) -> Result<Vec<[f32; 2]>> {
    s.split(';')
        .map(|p| {
            let (x, y) = p.split_once(',').context("point x,y")?;
            Ok([x.trim().parse()?, y.trim().parse()?])
        })
        .collect()
}

fn line_at(l: &[[f32; 2]], x: f32) -> f32 {
    if x <= l[0][0] {
        return l[0][1];
    }
    for s in l.windows(2) {
        if x <= s[1][0] {
            let t = (x - s[0][0]) / (s[1][0] - s[0][0]).max(1e-6);
            return s[0][1] + (s[1][1] - s[0][1]) * t;
        }
    }
    l[l.len() - 1][1]
}

pub fn mask_ref(args: &[String]) -> Result<()> {
    let img = image::open(args.first().context("picture missing")?)?.to_rgb8();
    let out = args.get(1).context("output missing")?;
    let rule = args.get(2).context("rule missing")?;
    let above = opt(args, "--above").map(polyline).transpose()?;
    let below = opt(args, "--below").map(polyline).transpose()?;
    let (w, h) = (img.width(), img.height());
    let num = |r: &str| -> Result<f32> { Ok(r.split_once('=').context("rule=value")?.1.parse()?) };
    let m = GrayImage::from_fn(w, h, |x, y| {
        let (nx, ny) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
        let inside = above.as_ref().is_none_or(|l| ny < line_at(l, nx)) && below.as_ref().is_none_or(|l| ny > line_at(l, nx));
        let p = img.get_pixel(x, y);
        let luma = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
        let ok = match rule.split('=').next().unwrap_or("") {
            "all" => true,
            "bright" => luma >= num(rule).unwrap_or(0.0),
            "dark" => luma > num(rule).unwrap_or(0.0),
            "bluish" => p[2] as i32 >= p[0] as i32 - 4,
            _ => false,
        };
        image::Luma([if inside && ok { 255 } else { 0 }])
    });
    if !["all", "bright", "dark", "bluish"].contains(&rule.split('=').next().unwrap_or("")) {
        bail!("unknown rule {rule}");
    }
    m.save(out)?;
    // overlay for checking by eye (red = reference sky)
    let w8: Vec<f32> = m.pixels().map(|v| v[0] as f32 / 255.0).collect();
    overlay(&img, &w8).save(Path::new(out).with_extension("check.jpg"))?;
    Ok(())
}
