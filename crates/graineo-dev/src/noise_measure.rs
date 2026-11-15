//! `graineo-dev noise-measure` – measure noise, colour blotches, banding and shadow tint in developed images.
//!
//! ```text
//! graineo-dev noise-measure <a.tif|jpg|png> [b …] [--crop x,y,w,h] [--mask-own] [--csv out.csv] [--json out.json]
//! ```
//!
//! All images must have the same size. By default the masks (flat / textured areas) are computed once from the
//! FIRST image and reused for all others, so that variants of the same photograph are compared on identical
//! pixels; `--mask-own` computes them per image. The first image is also the reference for ΔE00.
//!
//! Metrics (CIELAB, D65; L 0…100):
//! - `luma_korn`: RMS of L minus its 5×5 mean in flat areas (fine luminance grain)
//! - `chroma_fein`, `chroma_mittel`, `chroma_grob`, `chroma_sehr_grob`: RMS of the chroma (a, b) difference
//!   between successive box means (≈ 1–5, 5–17, 17–65, 65–257 px) in flat areas. `chroma_grob` and
//!   `chroma_sehr_grob` are the colour-blotch indices.
//! - `banding_zeilen`, `banding_spalten`: standard deviation of the per-row / per-column mean of the detrended
//!   luminance in flat areas; `*_chroma` the same for a and b combined. `rauschboden` is the value expected
//!   from uncorrelated grain alone.
//! - `schatten_a`, `schatten_b`, `schatten_chroma_grob`: mean a/b and blotch index in flat shadows (L 4…30).
//! - `detail`: RMS of L minus its 3×3 mean in textured areas (fine detail, includes grain).
//! - `de00_mittel`, `de00_p95`: CIEDE2000 against the first image.

use anyhow::{bail, ensure, Context, Result};
use rayon::prelude::*;

pub struct Img {
    pub w: usize,
    pub h: usize,
    /// Lab per pixel
    pub lab: Vec<[f32; 3]>,
}

fn srgb_to_lin(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn f_lab(t: f32) -> f32 {
    if t > 216.0 / 24389.0 {
        t.cbrt()
    } else {
        (24389.0 / 27.0 * t + 16.0) / 116.0
    }
}

fn rgb_to_lab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let (r, g, b) = (srgb_to_lin(r), srgb_to_lin(g), srgb_to_lin(b));
    let x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = (0.0193339 * r + 0.1191920 * g + 0.9503041 * b) / 1.08883;
    let (fx, fy, fz) = (f_lab(x), f_lab(y), f_lab(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

pub fn load(path: &str, crop: Option<[usize; 4]>) -> Result<Img> {
    let lower = path.to_lowercase();
    let (w, h, rgb): (usize, usize, Vec<f32>) = if lower.ends_with(".tif") || lower.ends_with(".tiff") {
        let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?))?;
        let (w, h) = dec.dimensions()?;
        let ch = match dec.colortype()? {
            tiff::ColorType::RGB(_) => 3,
            tiff::ColorType::RGBA(_) => 4,
            c => bail!("{path}: unsupported colour type {c:?}"),
        };
        let data: Vec<f32> = match dec.read_image()? {
            tiff::decoder::DecodingResult::U16(v) => v.iter().map(|&x| x as f32 / 65535.0).collect(),
            tiff::decoder::DecodingResult::U8(v) => v.iter().map(|&x| x as f32 / 255.0).collect(),
            _ => bail!("{path}: unsupported sample format"),
        };
        let rgb = data.chunks_exact(ch).flat_map(|p| [p[0], p[1], p[2]]).collect();
        (w as usize, h as usize, rgb)
    } else {
        let im = image::open(path).with_context(|| format!("{path}: cannot open"))?.to_rgb8();
        let (w, h) = im.dimensions();
        (w as usize, h as usize, im.into_raw().iter().map(|&x| x as f32 / 255.0).collect())
    };
    let [x0, y0, cw, chh] = crop.unwrap_or([0, 0, w, h]);
    ensure!(x0 + cw <= w && y0 + chh <= h, "{path}: crop outside the image ({w}×{h})");
    let lab = (0..chh)
        .into_par_iter()
        .flat_map_iter(|y| {
            let row = &rgb[((y0 + y) * w + x0) * 3..((y0 + y) * w + x0 + cw) * 3];
            row.chunks_exact(3).map(|p| rgb_to_lab(p[0], p[1], p[2])).collect::<Vec<_>>()
        })
        .collect();
    Ok(Img { w: cw, h: chh, lab })
}

/// Separable masked box mean with radius `r`: Σ(m·v)/Σm over the window (normalised convolution).
/// Pixels with an empty window keep `NaN`.
fn masked_box(v: &[f32], m: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], wt: &[f32], horizontal: bool| -> (Vec<f32>, Vec<f32>) {
        let (n_lines, len) = if horizontal { (h, w) } else { (w, h) };
        let idx = move |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
        let res: Vec<(Vec<f32>, Vec<f32>)> = (0..n_lines)
            .into_par_iter()
            .map(|line| {
                let mut cs = vec![0f64; len + 1];
                let mut cw = vec![0f64; len + 1];
                for i in 0..len {
                    let k = idx(line, i);
                    cs[i + 1] = cs[i] + (src[k] * wt[k]) as f64;
                    cw[i + 1] = cw[i] + wt[k] as f64;
                }
                let mut os = vec![0f32; len];
                let mut ow = vec![0f32; len];
                for i in 0..len {
                    let a = i.saturating_sub(r);
                    let b = (i + r + 1).min(len);
                    os[i] = (cs[b] - cs[a]) as f32;
                    ow[i] = (cw[b] - cw[a]) as f32;
                }
                (os, ow)
            })
            .collect();
        let mut s = vec![0f32; w * h];
        let mut ww = vec![0f32; w * h];
        for (line, (os, ow)) in res.into_iter().enumerate() {
            for i in 0..len {
                let k = idx(line, i);
                // store as mean so the second pass sees values, weights carry the counts
                s[k] = if ow[i] > 0.0 { os[i] / ow[i] } else { 0.0 };
                ww[k] = ow[i];
            }
        }
        (s, ww)
    };
    let (s1, w1) = pass(v, m, true);
    let (s2, w2) = pass(&s1, &w1, false);
    s2.iter().zip(&w2).map(|(&s, &wt)| if wt > 0.0 { s } else { f32::NAN }).collect()
}

fn channel(img: &Img, c: usize) -> Vec<f32> {
    img.lab.iter().map(|p| p[c]).collect()
}

pub struct Masks {
    flat: Vec<f32>,
    textured: Vec<f32>,
    shadow: Vec<f32>,
}

/// Flat = lowest 35 % of local structure (gradient of the 9×9-smoothed L, averaged over 33×33), L 3…97.
/// Textured = highest 15 %. Shadow = flat and L 4…30.
pub fn masks(img: &Img) -> Masks {
    let (w, h) = (img.w, img.h);
    let ones = vec![1f32; w * h];
    let l = channel(img, 0);
    let ls = masked_box(&l, &ones, w, h, 4);
    let mut g = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let k = y * w + x;
            let gx = ls[k + 1] - ls[k - 1];
            let gy = ls[k + w] - ls[k - w];
            g[k] = (gx * gx + gy * gy).sqrt();
        }
    }
    let s = masked_box(&g, &ones, w, h, 16);
    let mut sorted: Vec<f32> = s.iter().copied().filter(|v| v.is_finite()).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let q = |p: f32| sorted[((sorted.len() - 1) as f32 * p) as usize];
    let (lo, hi) = (q(0.35), q(0.85));
    let mut flat = vec![0f32; w * h];
    let mut textured = vec![0f32; w * h];
    let mut shadow = vec![0f32; w * h];
    for k in 0..w * h {
        let lk = l[k];
        if (3.0..97.0).contains(&lk) && s[k] <= lo {
            flat[k] = 1.0;
            if (4.0..30.0).contains(&lk) {
                shadow[k] = 1.0;
            }
        }
        if s[k] >= hi && lk < 97.0 {
            textured[k] = 1.0;
        }
    }
    Masks { flat, textured, shadow }
}

fn rms_masked(v: &[f32], m: &[f32]) -> (f64, f64) {
    let (mut s, mut n) = (0f64, 0f64);
    for (x, &mk) in v.iter().zip(m) {
        if mk > 0.0 && x.is_finite() {
            s += (*x as f64) * (*x as f64);
            n += 1.0;
        }
    }
    (if n > 0.0 { (s / n).sqrt() } else { f64::NAN }, n)
}

fn band(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// Per-line mean of `v` over masked pixels (lines with ≥ min_n pixels), returns std of these means and
/// the mean count per line.
fn line_profile_std(v: &[f32], m: &[f32], w: usize, h: usize, rows: bool, min_n: usize) -> (f64, f64) {
    let (n_lines, len) = if rows { (h, w) } else { (w, h) };
    let mut means = Vec::new();
    let mut counts = 0f64;
    for line in 0..n_lines {
        let (mut s, mut n) = (0f64, 0usize);
        for i in 0..len {
            let k = if rows { line * w + i } else { i * w + line };
            if m[k] > 0.0 && v[k].is_finite() {
                s += v[k] as f64;
                n += 1;
            }
        }
        if n >= min_n {
            means.push(s / n as f64);
            counts += n as f64;
        }
    }
    if means.len() < 8 {
        return (f64::NAN, 0.0);
    }
    let mu = means.iter().sum::<f64>() / means.len() as f64;
    let var = means.iter().map(|x| (x - mu) * (x - mu)).sum::<f64>() / means.len() as f64;
    (var.sqrt(), counts / means.len() as f64)
}

pub fn measure(img: &Img, mk: &Masks, reference: Option<&Img>) -> Vec<(&'static str, f64)> {
    let (w, h) = (img.w, img.h);
    let ones = vec![1f32; w * h];
    let l = channel(img, 0);
    let a = channel(img, 1);
    let b = channel(img, 2);
    let mut out = Vec::new();

    // luminance grain
    let l2 = masked_box(&l, &mk.flat, w, h, 2);
    let (grain, n_flat) = rms_masked(&band(&l, &l2), &mk.flat);
    out.push(("fraction_flat", n_flat / (w * h) as f64));
    let mean_where = |v: &[f32], m: &[f32]| {
        let (mut s, mut n) = (0f64, 0f64);
        for (x, &mk) in v.iter().zip(m) {
            if mk > 0.0 {
                s += *x as f64;
                n += 1.0;
            }
        }
        if n > 0.0 { s / n } else { f64::NAN }
    };
    out.push(("mean_l", l.iter().map(|&x| x as f64).sum::<f64>() / (w * h) as f64));
    out.push(("mean_l_shadows", mean_where(&l, &mk.shadow)));
    out.push(("luma_grain", grain));

    // chroma bands
    let radii = [2usize, 8, 32, 128];
    let pyr = |c: &[f32], m: &[f32]| -> Vec<Vec<f32>> {
        let mut v = vec![c.to_vec()];
        for &r in &radii {
            v.push(masked_box(c, m, w, h, r));
        }
        v
    };
    let pa = pyr(&a, &mk.flat);
    let pb = pyr(&b, &mk.flat);
    let names = ["chroma_fine", "chroma_mean", "chroma_coarse", "chroma_very_coarse"];
    for (i, name) in names.iter().enumerate() {
        let (ra, _) = rms_masked(&band(&pa[i], &pa[i + 1]), &mk.flat);
        let (rb, _) = rms_masked(&band(&pb[i], &pb[i + 1]), &mk.flat);
        out.push((name, (ra * ra + rb * rb).sqrt()));
    }

    // banding: detrend with a 33×33 masked mean, then per-row / per-column means
    let l16 = masked_box(&l, &mk.flat, w, h, 16);
    let a16 = masked_box(&a, &mk.flat, w, h, 16);
    let b16 = masked_box(&b, &mk.flat, w, h, 16);
    let rl = band(&l, &l16);
    let ra = band(&a, &a16);
    let rb = band(&b, &b16);
    for (rows, name, cname) in [(true, "banding_rows", "banding_rows_chroma"), (false, "banding_columns", "banding_columns_chroma")] {
        let (sl, n) = line_profile_std(&rl, &mk.flat, w, h, rows, 64);
        let (sa, _) = line_profile_std(&ra, &mk.flat, w, h, rows, 64);
        let (sb, _) = line_profile_std(&rb, &mk.flat, w, h, rows, 64);
        out.push((name, sl));
        out.push((cname, (sa * sa + sb * sb).sqrt()));
        if rows {
            out.push(("noise_floor_rows", grain / n.max(1.0).sqrt()));
        } else {
            out.push(("noise_floor_columns", grain / n.max(1.0).sqrt()));
        }
    }

    // shadows
    let (mut sa, mut sb, mut sn) = (0f64, 0f64, 0f64);
    for k in 0..w * h {
        if mk.shadow[k] > 0.0 {
            sa += a[k] as f64;
            sb += b[k] as f64;
            sn += 1.0;
        }
    }
    out.push(("fraction_shadows", sn / (w * h) as f64));
    out.push(("shadows_a", if sn > 0.0 { sa / sn } else { f64::NAN }));
    out.push(("shadows_b", if sn > 0.0 { sb / sn } else { f64::NAN }));
    let sa8 = masked_box(&a, &mk.shadow, w, h, 8);
    let sa32 = masked_box(&a, &mk.shadow, w, h, 32);
    let sb8 = masked_box(&b, &mk.shadow, w, h, 8);
    let sb32 = masked_box(&b, &mk.shadow, w, h, 32);
    let (x1, _) = rms_masked(&band(&sa8, &sa32), &mk.shadow);
    let (x2, _) = rms_masked(&band(&sb8, &sb32), &mk.shadow);
    out.push(("shadows_chroma_coarse", (x1 * x1 + x2 * x2).sqrt()));

    // detail in textured areas
    let l1 = masked_box(&l, &ones, w, h, 1);
    let (det, _) = rms_masked(&band(&l, &l1), &mk.textured);
    out.push(("detail", det));

    if let Some(r) = reference {
        let mut d: Vec<f32> = img
            .lab
            .par_iter()
            .zip(&r.lab)
            .map(|(p, q)| crate::raw_compare::de2000(*p, *q))
            .collect();
        let mean = d.iter().map(|&x| x as f64).sum::<f64>() / d.len() as f64;
        d.sort_by(|x, y| x.total_cmp(y));
        out.push(("de00_mean", mean));
        out.push(("de00_p95", d[(d.len() - 1) * 95 / 100] as f64));
        // colour shift without the noise: ΔE00 of the 17×17 means (what a viewer sees at normal size)
        let smooth = |im: &Img| -> Vec<[f32; 3]> {
            let ch: Vec<Vec<f32>> = (0..3).map(|c| masked_box(&channel(im, c), &ones, w, h, 8)).collect();
            (0..w * h).map(|k| [ch[0][k], ch[1][k], ch[2][k]]).collect()
        };
        let (sa, sb) = (smooth(img), smooth(r));
        let mut d: Vec<f32> = sa.par_iter().zip(&sb).map(|(p, q)| crate::raw_compare::de2000(*p, *q)).collect();
        let mean = d.iter().map(|&x| x as f64).sum::<f64>() / d.len() as f64;
        d.sort_by(|x, y| x.total_cmp(y));
        out.push(("de00_coarse_mean", mean));
        out.push(("de00_coarse_p95", d[(d.len() - 1) * 95 / 100] as f64));
        // real colour cast: ΔE00 of 257×257 means (far larger than any blotch) and of the whole-image mean
        let smooth_big = |im: &Img| -> Vec<[f32; 3]> {
            let ch: Vec<Vec<f32>> = (0..3).map(|c| masked_box(&channel(im, c), &ones, w, h, 128)).collect();
            (0..w * h).map(|k| [ch[0][k], ch[1][k], ch[2][k]]).collect()
        };
        let (sa, sb) = (smooth_big(img), smooth_big(r));
        let d: Vec<f32> = sa.par_iter().zip(&sb).map(|(p, q)| crate::raw_compare::de2000(*p, *q)).collect();
        out.push(("de00_flat", d.iter().map(|&x| x as f64).sum::<f64>() / d.len() as f64));
        let mean_lab = |im: &Img| -> [f32; 3] {
            let mut s = [0f64; 3];
            for p in &im.lab {
                for c in 0..3 {
                    s[c] += p[c] as f64;
                }
            }
            s.map(|v| (v / im.lab.len() as f64) as f32)
        };
        out.push(("de00_image_mean", crate::raw_compare::de2000(mean_lab(img), mean_lab(r)) as f64));
    }
    out
}

pub fn run(args: &[String]) -> Result<()> {
    let mut files = Vec::new();
    let mut crop = None;
    let mut own_mask = false;
    let mut best: Option<(usize, usize)> = None;
    let mut csv = None;
    let mut json = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--crop" => {
                let v: Vec<usize> = it.next().context("--crop x,y,w,h")?.split(',').map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
                ensure!(v.len() == 4, "--crop x,y,w,h");
                crop = Some([v[0], v[1], v[2], v[3]]);
            }
            "--mask-own" => own_mask = true,
            "--best-crop" => {
                let v: Vec<usize> = it.next().context("--best-crop WxH")?.split('x').map(|s| s.trim().parse()).collect::<Result<_, _>>()?;
                ensure!(v.len() == 2, "--best-crop WxH");
                best = Some((v[0], v[1]));
            }
            "--csv" => csv = Some(it.next().context("--csv file")?.clone()),
            "--json" => json = Some(it.next().context("--json file")?.clone()),
            _ => files.push(a.clone()),
        }
    }
    ensure!(!files.is_empty(), "usage: graineo-dev noise-measure <image …> [--crop x,y,w,h] [--mask-own] [--csv f] [--json f]");
    let first = load(&files[0], crop)?;
    let base_masks = masks(&first);
    if let Some((bw, bh)) = best {
        // window with the most flat dark-to-mid pixels (shadows count double): where blotches and banding show
        let (w, h) = (first.w, first.h);
        ensure!(bw <= w && bh <= h, "window larger than the image");
        let score: Vec<f64> = (0..w * h)
            .map(|k| {
                let l = first.lab[k][0];
                let f = base_masks.flat[k] as f64 * if (8.0..55.0).contains(&l) { 1.0 } else { 0.0 };
                f + base_masks.shadow[k] as f64
            })
            .collect();
        let mut ii = vec![0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0f64;
            for x in 0..w {
                row += score[y * w + x];
                ii[(y + 1) * (w + 1) + x + 1] = ii[y * (w + 1) + x + 1] + row;
            }
        }
        let mut top = (f64::MIN, 0, 0);
        for y in (0..=h - bh).step_by(32) {
            for x in (0..=w - bw).step_by(32) {
                let s = ii[(y + bh) * (w + 1) + x + bw] - ii[y * (w + 1) + x + bw] - ii[(y + bh) * (w + 1) + x] + ii[y * (w + 1) + x];
                if s > top.0 {
                    top = (s, x, y);
                }
            }
        }
        let (ox, oy) = crop.map(|c| (c[0], c[1])).unwrap_or((0, 0));
        println!("ausschnitt {},{},{},{}", top.1 + ox, top.2 + oy, bw, bh);
        return Ok(());
    }
    let mut rows: Vec<(String, Vec<(&'static str, f64)>)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let img = if i == 0 { None } else { Some(load(f, crop)?) };
        let im = img.as_ref().unwrap_or(&first);
        ensure!((im.w, im.h) == (first.w, first.h), "{f}: size differs from the first image");
        let own;
        let mk = if own_mask && i > 0 {
            own = masks(im);
            &own
        } else {
            &base_masks
        };
        let m = measure(im, mk, if i == 0 { None } else { Some(&first) });
        rows.push((f.clone(), m));
    }
    // table
    let keys: Vec<&str> = rows.iter().flat_map(|r| r.1.iter().map(|(k, _)| *k)).fold(Vec::new(), |mut v, k| {
        if !v.contains(&k) {
            v.push(k);
        }
        v
    });
    let short = |f: &str| std::path::Path::new(f).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    print!("{:<24}", "Kennzahl");
    for (f, _) in &rows {
        print!(" {:>14}", short(f).chars().take(14).collect::<String>());
    }
    println!();
    for k in &keys {
        print!("{k:<24}");
        for (_, m) in &rows {
            match m.iter().find(|(n, _)| n == k) {
                Some((_, v)) => print!(" {:>14.4}", v),
                None => print!(" {:>14}", "–"),
            }
        }
        println!();
    }
    if let Some(p) = csv {
        let mut s = format!("datei,{}\n", keys.join(","));
        for (f, m) in &rows {
            s.push_str(&short(f));
            for k in &keys {
                let v = m.iter().find(|(n, _)| n == k).map(|x| x.1).unwrap_or(f64::NAN);
                s.push_str(&format!(",{v:.5}"));
            }
            s.push('\n');
        }
        std::fs::write(&p, s)?;
    }
    if let Some(p) = json {
        let v: Vec<serde_json::Value> = rows
            .iter()
            .map(|(f, m)| {
                let mut o = serde_json::Map::new();
                o.insert("file".into(), f.clone().into());
                for (k, x) in m {
                    o.insert((*k).into(), if x.is_finite() { (*x).into() } else { serde_json::Value::Null });
                }
                serde_json::Value::Object(o)
            })
            .collect();
        std::fs::write(&p, serde_json::to_string_pretty(&v)?)?;
    }
    Ok(())
}
