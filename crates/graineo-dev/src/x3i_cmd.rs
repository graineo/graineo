//! X3I (SFD bracket) merge test bench.
//!
//! `graineo-dev x3i-merge FILE.X3I --out OUT.tif [--no-align] [--denoise N] [--ghost-maps]`: merge the seven exposures
//! with the x3fuse-core merge (noise model, alignment, ghost estimate) and write the scene-linear result as float TIFF
//! (extended linear sRGB, as-shot white balance); prints time and peak memory. `X3F_X3I_*` environment variables
//! tune the merge (see x3fuse-core).
//!
//! `graineo-dev x3i-compare REF.tif CAND.tif[:lin|:srgb]...`: compare candidates against a reference rendering
//! (16-bit sRGB TIFF). Every candidate gets one global linear gain fitted to the reference on mid-tones, then the
//! mid-tone colour difference (CIEDE2000) and the signal-to-noise ratio of flat blocks, binned by brightness, are printed.
//! `:lin` is a linear float TIFF, `:srgb` (default for 16-bit files) an sRGB-encoded one.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn peak_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    if cfg!(target_os = "macos") {
        u.ru_maxrss as f64 / 1048576.0
    } else {
        u.ru_maxrss as f64 / 1024.0
    }
}

pub fn merge(args: &[String]) -> Result<()> {
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut options = x3f_core::SceneLinearOptions { denoise_intensity: 0, ..Default::default() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--no-align" => std::env::set_var("X3F_X3I_NO_ALIGN", "1"),
            "--no-ghost" => std::env::set_var("X3F_X3I_NO_GHOST", "1"),
            "--denoise" => {
                let n: u8 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                options.denoise_intensity = n;
            }
            "--middle" => options.x3i_source = x3f_core::X3iSource::MiddleExposure,
            _ => input = Some(PathBuf::from(a)),
        }
    }
    let input = input.context("X3I missing")?;
    let out = out.context("--out missing")?;
    let cancel = AtomicBool::new(false);
    let t0 = Instant::now();
    let scene = x3f_core::prepare_scene_linear(&input, &options, &cancel, |_| {}).map_err(|e| anyhow::anyhow!("{e}"))?;
    let secs = t0.elapsed().as_secs_f64();
    let max = scene.data.iter().copied().fold(f32::MIN, f32::max);
    eprintln!("x3i-merge: {}x{} orientation {} in {secs:.1} s, peak memory {:.0} MB, max {max:.3}", scene.width, scene.height, scene.orientation, peak_rss_mb());
    let f = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut enc = tiff::encoder::TiffEncoder::new(f)?;
    enc.write_image::<tiff::encoder::colortype::RGB32Float>(scene.width, scene.height, &scene.data)?;
    Ok(())
}

struct Img {
    w: usize,
    h: usize,
    /// linear, three values per pixel
    px: Vec<f32>,
}

fn srgb_to_lin(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn load(spec: &str) -> Result<(String, Img)> {
    let (path, kind) = match spec.rsplit_once(':') {
        Some((p, k)) if k == "lin" || k == "srgb" => (p, Some(k)),
        _ => (spec, None),
    };
    let f = std::io::BufReader::new(std::fs::File::open(path).with_context(|| path.to_string())?);
    let mut dec = tiff::decoder::Decoder::new(f)?.with_limits(tiff::decoder::Limits::unlimited());
    let (w, h) = dec.dimensions()?;
    let px: Vec<f32> = match dec.read_image()? {
        tiff::decoder::DecodingResult::U16(d) => {
            let lut: Vec<f32> = (0..65536).map(|i| srgb_to_lin(i as f32 / 65535.0)).collect();
            if kind == Some("lin") {
                d.iter().map(|v| *v as f32 / 65535.0).collect()
            } else {
                d.iter().map(|v| lut[*v as usize]).collect()
            }
        }
        tiff::decoder::DecodingResult::F32(d) => {
            if kind == Some("srgb") {
                d.iter().map(|v| srgb_to_lin(*v)).collect()
            } else {
                d
            }
        }
        _ => bail!("{path}: unsupported sample type"),
    };
    let name = Path::new(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    Ok((name, Img { w: w as usize, h: h as usize, px }))
}

fn lum(p: &[f32]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

fn lab(rgb: [f32; 3]) -> [f32; 3] {
    let x = 0.4124564 * rgb[0] + 0.3575761 * rgb[1] + 0.1804375 * rgb[2];
    let y = 0.2126729 * rgb[0] + 0.7151522 * rgb[1] + 0.0721750 * rgb[2];
    let z = 0.0193339 * rgb[0] + 0.119192 * rgb[1] + 0.9503041 * rgb[2];
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Least-squares gain `g` with `reference ≈ g · candidate` over the pixels whose reference luminance lies in `lo..hi`.
fn fit_gain(r: &Img, c: &Img, ox: (usize, usize), lo: f32, hi: f32) -> f32 {
    let (mut sxy, mut sxx) = (0f64, 0f64);
    for y in (0..r.h).step_by(5) {
        for x in (0..r.w).step_by(5) {
            let a = &r.px[(y * r.w + x) * 3..][..3];
            let (cx, cy) = (x + ox.0, y + ox.1);
            let b = &c.px[(cy * c.w + cx) * 3..][..3];
            let la = lum(a);
            if la > lo && la < hi {
                let lb = lum(b) as f64;
                sxy += la as f64 * lb;
                sxx += lb * lb;
            }
        }
    }
    (sxy / sxx.max(1e-30)) as f32
}

/// Tone curve fitted per candidate: the median of `reference luminance / candidate luminance` in 96 logarithmic bins of
/// the candidate luminance. Applied to the candidate RGB it removes differences of the tone curve, leaving colour.
fn fit_curve(r: &Img, c: &Img, ox: (usize, usize)) -> Vec<f32> {
    const N: usize = 96;
    let mut v: Vec<Vec<f32>> = vec![Vec::new(); N];
    for y in (0..r.h).step_by(4) {
        for x in (0..r.w).step_by(4) {
            let a = lum(&r.px[(y * r.w + x) * 3..][..3]);
            let b = lum(&c.px[((y + ox.1) * c.w + x + ox.0) * 3..][..3]);
            if b > 1e-5 && a > 1e-5 {
                let i = (((b.log10() + 5.0) / 5.5) * N as f32).clamp(0.0, N as f32 - 1.0) as usize;
                v[i].push(a / b);
            }
        }
    }
    let mut out: Vec<f32> = v
        .iter_mut()
        .map(|b| {
            b.sort_by(|a, c| a.total_cmp(c));
            if b.len() < 20 { f32::NAN } else { b[b.len() / 2] }
        })
        .collect();
    let last = out.iter().copied().find(|x| x.is_finite()).unwrap_or(1.0);
    let mut prev = last;
    for o in out.iter_mut() {
        if o.is_finite() { prev = *o } else { *o = prev }
    }
    out
}

fn curve_gain(curve: &[f32], l: f32) -> f32 {
    let i = (((l.max(1e-5).log10() + 5.0) / 5.5) * curve.len() as f32).clamp(0.0, curve.len() as f32 - 1.0) as usize;
    curve[i]
}

const BLOCK: usize = 32;

/// Noise of a block: standard deviation of the luminance around its least-squares plane, and the mean.
fn block_stats(img: &Img, ox: (usize, usize), bx: usize, by: usize, gain: f32) -> (f32, f32, f32) {
    let n = BLOCK as f64;
    let (mut s, mut sx, mut sy, mut sxx_, mut syy_) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let mut v = [0f32; BLOCK * BLOCK];
    for y in 0..BLOCK {
        for x in 0..BLOCK {
            let (cx, cy) = (bx + x + ox.0, by + y + ox.1);
            let l = lum(&img.px[(cy * img.w + cx) * 3..][..3]) * gain;
            v[y * BLOCK + x] = l;
            let (fx, fy) = (x as f64 - (n - 1.0) / 2.0, y as f64 - (n - 1.0) / 2.0);
            s += l as f64;
            sx += fx * l as f64;
            sy += fy * l as f64;
            sxx_ += fx * fx;
            syy_ += fy * fy;
        }
    }
    let mean = s / (n * n);
    let (ax, ay) = (sx / (sxx_ * n), sy / (syy_ * n));
    let mut res = 0f64;
    for y in 0..BLOCK {
        for x in 0..BLOCK {
            let (fx, fy) = (x as f64 - (n - 1.0) / 2.0, y as f64 - (n - 1.0) / 2.0);
            let d = v[y * BLOCK + x] as f64 - (mean + ax * fx + ay * fy);
            res += d * d;
        }
    }
    let noise = (res / (n * n - 3.0)).sqrt();
    // low-frequency residual: 8x8 cells of the block, plane fit
    let cells = BLOCK / 8;
    let mut cv = vec![0f64; cells * cells];
    for cy in 0..cells {
        for cx in 0..cells {
            let mut a = 0f64;
            for y in 0..8 {
                for x in 0..8 {
                    a += v[(cy * 8 + y) * BLOCK + cx * 8 + x] as f64;
                }
            }
            cv[cy * cells + cx] = a / 64.0;
        }
    }
    let cm = cv.iter().sum::<f64>() / cv.len() as f64;
    let c = (cells as f64 - 1.0) / 2.0;
    let (mut cxs, mut cys, mut q) = (0f64, 0f64, 0f64);
    for cy in 0..cells {
        for cx in 0..cells {
            cxs += (cx as f64 - c) * cv[cy * cells + cx];
            cys += (cy as f64 - c) * cv[cy * cells + cx];
            q += (cx as f64 - c).powi(2);
        }
    }
    let (px_, py_) = (cxs / (q * cells as f64), cys / (q * cells as f64));
    let mut lf = 0f64;
    for cy in 0..cells {
        for cx in 0..cells {
            let d = cv[cy * cells + cx] - (cm + px_ * (cx as f64 - c) + py_ * (cy as f64 - c));
            lf += d * d;
        }
    }
    let lf = (lf / (cells * cells) as f64).sqrt() / cm.max(1e-9);
    (mean as f32, noise as f32, lf as f32)
}

pub fn compare(args: &[String]) -> Result<()> {
    let mut specs = args.iter();
    let (rname, r) = load(specs.next().context("REF missing")?)?;
    let mut cands = Vec::new();
    for s in specs {
        cands.push(load(s)?);
    }
    println!("reference {rname} {}x{}", r.w, r.h);
    let bins = [0.002f32, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.4];
    // offsets: centre crop of larger candidates; the common region is the reference size
    let mut gains = Vec::new();
    let mut offs = Vec::new();
    for (name, c) in &cands {
        if c.w < r.w || c.h < r.h {
            bail!("{name}: {}x{} smaller than the reference", c.w, c.h);
        }
        let o = ((c.w - r.w) / 2, (c.h - r.h) / 2);
        let g = fit_gain(&r, c, o, 0.1, 0.5);
        println!("{name}: {}x{} offset {:?}, mid-tone gain {g:.4}", c.w, c.h, o);
        gains.push(g);
        offs.push(o);
    }
    let curves: Vec<Vec<f32>> = cands.iter().enumerate().map(|(k, (_, c))| fit_curve(&r, c, offs[k])).collect();
    // colour difference on mid-tones (reference luminance 0.1 .. 0.5), every 3rd pixel
    for (k, (name, c)) in cands.iter().enumerate() {
        let mut v: Vec<f32> = Vec::new();
        for y in (0..r.h).step_by(3) {
            for x in (0..r.w).step_by(3) {
                let a = &r.px[(y * r.w + x) * 3..][..3];
                let l = lum(a);
                if l > 0.1 && l < 0.5 {
                    let b = &c.px[((y + offs[k].1) * c.w + x + offs[k].0) * 3..][..3];
                    let g = curve_gain(&curves[k], lum(b));
                    let bb = [b[0] * g, b[1] * g, b[2] * g];
                    v.push(raw::learn_look::de2000(lab([a[0], a[1], a[2]]), lab(bb)));
                }
            }
        }
        v.sort_by(|a, b| a.total_cmp(b));
        let mean = v.iter().sum::<f32>() / v.len().max(1) as f32;
        println!("{name}: dE00 mid-tones mean {mean:.2} median {:.2} p90 {:.2} ({} px)", v[v.len() / 2], v[v.len() * 9 / 10], v.len());
    }
    // flat blocks: flat in the reference and every candidate
    let mut rows: Vec<Vec<(f32, f32)>> = vec![Vec::new(); cands.len() + 1];
    for by in (0..r.h - BLOCK).step_by(BLOCK) {
        for bx in (0..r.w - BLOCK).step_by(BLOCK) {
            let (m, n, lf) = block_stats(&r, (0, 0), bx, by, 1.0);
            if m < bins[0] || m > bins[bins.len() - 1] || lf > 0.03 {
                continue;
            }
            let mut tmp = vec![(m, n)];
            let mut ok = true;
            for (k, (_, c)) in cands.iter().enumerate() {
                let (m2, n2, lf2) = block_stats(c, offs[k], bx, by, gains[k]);
                if lf2 > 0.03 {
                    ok = false;
                    break;
                }
                tmp.push((m2, n2));
            }
            if ok {
                for (i, t) in tmp.into_iter().enumerate() {
                    rows[i].push(t);
                }
            }
        }
    }
    println!("flat blocks ({BLOCK}x{BLOCK}, flat in all images): {}", rows[0].len());
    print!("{:>16}", "mean in range");
    for b in bins.windows(2) {
        print!("{:>11}", format!("{}-{}", b[0], b[1]));
    }
    println!();
    let mut names = vec![rname.clone()];
    names.extend(cands.iter().map(|c| c.0.clone()));
    for (i, name) in names.iter().enumerate() {
        print!("{:>16}", name.chars().take(16).collect::<String>());
        for b in bins.windows(2) {
            // reference-image mean decides the bin; SNR = median over the blocks of 20·log10(mean/noise), mean of this image
            let mut s: Vec<f32> = (0..rows[0].len())
                .filter(|j| rows[0][*j].0 >= b[0] && rows[0][*j].0 < b[1])
                .map(|j| 20.0 * (rows[i][j].0 / rows[i][j].1.max(1e-9)).log10())
                .collect();
            s.sort_by(|a, b| a.total_cmp(b));
            if s.len() < 5 {
                print!("{:>11}", "-");
            } else {
                print!("{:>11}", format!("{:.1}({})", s[s.len() / 2], s.len()));
            }
        }
        println!();
    }
    Ok(())
}

/// `graineo-dev x3i-render FILE.X3I --out OUT.tif`: develop the merge like the app does (Quattro preparation of the
/// merged scene incl. the noise reduction stage, standard settings) and write a 16-bit sRGB TIFF.
pub fn render(args: &[String]) -> Result<()> {
    let input = args.first().context("X3I missing")?;
    let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).context("--out missing")?;
    let t0 = Instant::now();
    let mut s = develop::Session::open(input).map_err(|e| anyhow::anyhow!("{e}"))?;
    let t1 = t0.elapsed().as_secs_f64();
    let img = s.render_full(&develop::Settings::default());
    eprintln!("x3i-render: open {t1:.1} s, render {:.1} s, {}x{}, peak memory {:.0} MB", t0.elapsed().as_secs_f64() - t1, img.width, img.height, peak_rss_mb());
    let f = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut enc = tiff::encoder::TiffEncoder::new(f)?;
    enc.write_image::<tiff::encoder::colortype::RGB16>(img.width as u32, img.height as u32, &img.data)?;
    Ok(())
}
