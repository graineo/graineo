//! SFD bracket (sd Quattro H, seven exposures) merged on the Quattro sensor grid ([`develop::sfd`]) and developed with
//! the Quattro chain; comparison against a reference rendering without any fitted gain.
//!
//! `graineo-dev sfd-merge FRAMEDIR --out OUT.tif [--base middle|shortest|longest] [--dump PREFIX] [--frames N]`
//! reads all `.X3F` frames of `FRAMEDIR`, merges them (longest exposure first), hands the merge to the Quattro
//! preparation of the base frame (calibration, white balance and colour of that frame) and writes a 16-bit sRGB TIFF
//! with standard settings. `--dump`: the merged grid and top layer as float TIFFs.
//!
//! `graineo-dev sfd-compare REF.tif CAND.tif [--crops OUT.png x,y;x,y;...]`: absolute comparison of two 16-bit sRGB
//! renderings of the same size: CIEDE2000 overall and per reference lightness, code differences per channel,
//! the gain a least-squares fit would apply (for information only), noise of flat blocks per brightness;
//! `--crops`: 256 × 256 crops of both side by side.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

struct FrameData {
    path: PathBuf,
    time: f32,
    /// clip level per grid channel (top: of the 2 × 2 mean) and white balance gains
    clip: [f32; 3],
    gains: [f32; 3],
    grid: develop::quattro_grid::RawGrid,
}

fn camf_scalar(camf: &x3f::Camf, name: &str) -> Option<f64> {
    camf.matrix(name).ok()?.as_f64().first().copied()
}

fn read_frame(path: &Path) -> Result<FrameData> {
    let file = x3f::X3fFile::open(path)?;
    let camf = file.camf()?;
    let offset = camf_scalar(&camf, "RawOffset").unwrap_or(2048.0) as f32;
    let sensor = camf_scalar(&camf, "SensorISO").filter(|v| *v > 0.0).unwrap_or(100.0);
    let digital = camf.matrix("DigitalISOGain").ok().map(|m| m.as_f64()).filter(|v| v.len() == 3).unwrap_or(vec![1.0; 3]);
    let unit: [f32; 3] = std::array::from_fn(|c| (digital[c] / (sensor / 100.0)) as f32);
    let time = camf_scalar(&camf, "CaptureShutter").context("CaptureShutter missing")? as f32;
    let fix = std::env::var("GRAINEO_SFD_FIXBAD").map_or(true, |v| v != "0");
    let grid = develop::quattro_grid::read_with(path, offset, unit, fix).map_err(|e| anyhow::anyhow!("{e}"))?;
    let white = 16383.0 - offset;
    let clip: [f32; 3] = std::array::from_fn(|c| 0.97 * white * unit[c]);
    let g = camf.matrix("AutoWBGain").ok().map(|m| m.as_f64()).filter(|v| v.len() == 3).unwrap_or(vec![1.0; 3]);
    // the units of the grid include the digital gain of the layer: the white balance gains apply as they are
    let gains: [f32; 3] = std::array::from_fn(|c| g[c] as f32);
    Ok(FrameData { path: path.to_path_buf(), time, clip, gains, grid })
}

fn write_f32(path: &Path, w: usize, h: usize, rgb: &[f32]) -> Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = tiff::encoder::TiffEncoder::new(f)?;
    if rgb.len() == w * h * 3 {
        enc.write_image::<tiff::encoder::colortype::RGB32Float>(w as u32, h as u32, rgb)?;
    } else {
        enc.write_image::<tiff::encoder::colortype::Gray32Float>(w as u32, h as u32, rgb)?;
    }
    Ok(())
}

pub fn merge(args: &[String]) -> Result<()> {
    let mut dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut dump: Option<String> = None;
    let mut base = "middle".to_string();
    let mut sets: Vec<(String, f64)> = Vec::new();
    let mut fit_ref: Option<String> = None;
    let mut drx = false;
    let mut bracket = false;
    let mut scene_out: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--set" => {
                for kv in it.next().map(|s| s.as_str()).unwrap_or("").split(',') {
                    if let Some((k, v)) = kv.split_once('=') {
                        sets.push((k.to_string(), v.parse().unwrap_or(0.0)));
                    }
                }
            }
            "--fit" => fit_ref = it.next().cloned(),
            "--auto" => drx = true,
            "--scene" => scene_out = it.next().map(PathBuf::from),
            "--bracket-tone" => bracket = true,
            "--out" => out = it.next().map(PathBuf::from),
            "--dump" => dump = it.next().cloned(),
            "--base" => base = it.next().cloned().unwrap_or(base),
            _ => dir = Some(PathBuf::from(a)),
        }
    }
    let dir = dir.context("FRAMEDIR missing")?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3f")))
        .collect();
    paths.sort();
    if paths.is_empty() {
        bail!("no X3F in {}", dir.display());
    }
    let t0 = Instant::now();
    let mut frames: Vec<FrameData> = paths.iter().map(|p| read_frame(p)).collect::<Result<_>>()?;
    // longest exposure first
    frames.sort_by(|a, b| b.time.total_cmp(&a.time));
    let (gw, gh) = (frames[0].grid.gw, frames[0].grid.gh);
    for f in &frames {
        if f.grid.gw != gw || f.grid.gh != gh || f.grid.tw < 2 * gw || f.grid.th < 2 * gh {
            bail!("{}: other size", f.path.display());
        }
        eprintln!("frame {} time {} µs", f.path.file_name().unwrap().to_string_lossy(), f.time);
    }
    // focus pixel lattice (sd Quattro H): top layer one column in 16 and two rows in 32, the grid one column in 8 and
    // one row in 16; phase measured on the longest exposure, repaired from the horizontal neighbours in every frame
    if std::env::var("GRAINEO_SFD_DEFECTS").map_or(true, |v| v != "0") {
        let (tw, th) = (frames[0].grid.tw, frames[0].grid.th);
        let (gw, gh) = (frames[0].grid.gw, frames[0].grid.gh);
        let (px, py, ratio) = develop::sfd::focus_lattice(&frames[0].grid.top, tw, th);
        eprintln!("focus pixel lattice: column phase {px}, row phase {py}, median ratio {ratio:.3}");
        if ratio < 0.97 {
            for f in frames.iter_mut() {
                let g = &mut f.grid;
                develop::sfd::repair_lattice(&mut g.top, tw, th, px, 16, (py + 31) % 32, 32, 3);
                for c in 0..2 {
                    let mut p: Vec<f32> = g.grid.iter().map(|q| q[c]).collect();
                    develop::sfd::repair_lattice(&mut p, gw, gh, px / 2, 8, py / 2, 16, 1);
                    for (q, v) in g.grid.iter_mut().zip(p) {
                        q[c] = v;
                    }
                }
                for y in 0..g.gh {
                    for x in 0..g.gw {
                        let i = 2 * y * g.tw + 2 * x;
                        g.grid[y * g.gw + x][2] = 0.25 * (g.top[i] + g.top[i + 1] + g.top[i + g.tw] + g.top[i + g.tw + 1]);
                    }
                }
            }
        }
    }
    // clipped highlights of every exposure to grey (`GRAINEO_SFD_HIGHLIGHTS=0`: off)
    if std::env::var("GRAINEO_SFD_HIGHLIGHTS").map_or(true, |v| v != "0") {
        for f in frames.iter_mut() {
            let (clip, gains, gw, tw) = (f.clip, f.gains, f.grid.gw, f.grid.tw);
            let n = develop::sfd::neutralize_clipped(&mut f.grid.grid, &mut f.grid.top, gw, tw, clip, gains);
            eprintln!("{}: {n} clipped grid pixels set to grey", f.path.file_name().unwrap().to_string_lossy());
        }
    }
    let t_read = t0.elapsed().as_secs_f64();
    let views: Vec<develop::sfd::Frame> = frames.iter().map(|f| develop::sfd::Frame { grid: &f.grid.grid, top: &f.grid.top, tw: f.grid.tw, time: f.time }).collect();
    let p = develop::sfd::Params::default();
    let valid = [0, 0, gw.saturating_sub(1), gh.saturating_sub(1)];
    let m = develop::sfd::merge(&views, gw, gh, valid, &p);
    let t_merge = t0.elapsed().as_secs_f64() - t_read;
    let sat = m.saturated.iter().filter(|s| **s).count();
    eprintln!(
        "sfd-merge: grid {gw}x{gh}, max {:.2} counts (shortest-exposure units), scale {:.5}, saturated {sat}, factors {:?}, read {t_read:.1} s, merge {t_merge:.1} s",
        m.max,
        m.scale,
        m.factors.iter().map(|f| format!("{f:.4}")).collect::<Vec<_>>()
    );
    // percentiles of the merged grid (largest channel)
    let mut v: Vec<f32> = m.grid.iter().step_by(7).map(|p| p[0].max(p[1]).max(p[2])).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |f: f64| v[((v.len() as f64 - 1.0) * f) as usize];
    eprintln!("merged grid, largest channel: p1 {:.1} p10 {:.1} p50 {:.1} p90 {:.1} p99 {:.1} p99.99 {:.1}", q(0.01), q(0.1), q(0.5), q(0.9), q(0.99), q(0.9999));
    if let Some(prefix) = &dump {
        let flat: Vec<f32> = m.grid.iter().flat_map(|p| p.iter().copied()).collect();
        write_f32(Path::new(&format!("{prefix}-grid.tif")), gw, gh, &flat)?;
        write_f32(Path::new(&format!("{prefix}-top.tif")), 2 * gw, 2 * gh, &m.top)?;
        write_f32(Path::new(&format!("{prefix}-noise.tif")), gw, gh, &m.grid_noise)?;
    }
    let Some(out) = out else { return Ok(()) };
    // base frame: calibration and colour of this exposure
    let n = frames.len();
    let bi = match base.as_str() {
        "shortest" => n - 1,
        "longest" => 0,
        _ => n / 2,
    };
    let base_path = frames[bi].path.clone();
    let shortest = frames.iter().map(|f| f.time).fold(f32::INFINITY, f32::min);
    let base_gain = develop::sfd::base_gain(frames[bi].time, shortest, m.scale);
    let (tw, th) = (frames[bi].grid.tw, frames[bi].grid.th);
    drop(views);
    drop(frames);
    // top layer in the frame's size (rows/columns beyond 2 × grid repeated from the edge)
    let mut m = m;
    // brightness noise reduction of the merged top layer with the noise of the merge (`GRAINEO_SFD_LUMA=scatter,mix`)
    if let Some((sc, mix)) = std::env::var("GRAINEO_SFD_LUMA").ok().or(Some(format!("{},{}", develop::sfd::LUMA_SCATTER, develop::sfd::LUMA_MIX))).and_then(|v| {
        let (a, b) = v.split_once(',')?;
        Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?))
    }) {
        let scale = m.scale;
        // `GRAINEO_SFD_LUMA_MEAN=1`: weighted mean of the frame variances (variance of one exposure) instead of the
        // variance of the mean
        let noise: Vec<f32> = if std::env::var("GRAINEO_SFD_LUMA_MEAN").map_or(true, |v| v != "0") {
            m.top_noise.iter().zip(&m.top_frames).map(|(n, f)| n * f).collect()
        } else {
            m.top_noise.clone()
        };
        develop::sfd::smooth_top(&mut m.top, &noise, 2 * gw, 2 * gh, scale, sc, mix);
        eprintln!("top layer smoothed: scatter {sc}, mix {mix}");
    }
    let mut top = vec![0f32; tw * th];
    for y in 0..th {
        for x in 0..tw {
            top[y * tw + x] = m.top[y.min(2 * gh - 1) * 2 * gw + x.min(2 * gw - 1)];
        }
    }
    develop::sfd::set_replacement(
        &base_path,
        develop::sfd::Replacement { grid: m.grid, gw, gh, top, tw, th, grid_noise: Some(m.grid_noise), base_gain },
    );
    eprintln!("base frame {}", base_path.display());
    // the replaced values are not part of the cache key
    std::env::set_var("GRAINEO_NO_DISK_CACHE", "1");
    let t1 = Instant::now();
    let mut s = develop::Session::open(&base_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut st = develop::Settings::default();
    if bracket {
        // tone of the merged bracket: level from the scene-linear rendering of the standard curve
        let lin = s.render_full_scene(&st, true).map_err(|e| anyhow::anyhow!("{e}"))?;
        let k = develop::film::scene_gain(&s.cal, &st) as f64;
        let white = std::env::var("GRAINEO_SFD_WHITE").ok().and_then(|v| v.parse().ok()).unwrap_or(develop::sfd::BRACKET_WHITE);
        let t = develop::sfd::BracketTone::analyse(&lin.data, lin.width, lin.height, 1.0, white, k);
        eprintln!("bracket tone: {t:?}");
        develop::sfd::set_bracket_tone(Some(t));
    }
    if let Some(p) = &scene_out {
        let lin = s.render_full_scene(&st, true).map_err(|e| anyhow::anyhow!("{e}"))?;
        write_f32(p, lin.width, lin.height, &lin.data)?;
    }
    if drx {
        let lin = s.render_full_scene(&st, true).map_err(|e| anyhow::anyhow!("{e}"))?;
        let v = auto_tone(&lin.data, lin.width, lin.height);
        eprintln!("automatic tone values: exposure {:.3} contrast {:.3} shadow {:.3} highlight {:.3}", v[0], v[1], v[2], v[3]);
        for (k, x) in ["exposure", "contrast", "shadow", "highlight"].iter().zip(v) {
            set_slider(&mut st, k, x);
        }
    }
    apply_sets(&mut st, &sets);
    if let Some(reference) = &fit_ref {
        st = fit_sliders(&mut s, st, &load16(reference)?)?;
    }
    let img = s.render_full(&st);
    eprintln!("develop {:.1} s, {}x{}", t1.elapsed().as_secs_f64(), img.width, img.height);
    let f = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut enc = tiff::encoder::TiffEncoder::new(f)?;
    enc.write_image::<tiff::encoder::colortype::RGB16>(img.width as u32, img.height as u32, &img.data)?;
    Ok(())
}

/// Automatic tone values of the merged bracket from the brightness distribution of the linear rendering (scale-free):
/// quantiles `q(k/80)` of the channel mean, `top = 0.25·q(1) + 0.75·q(79/80)`, `P = q/top`, image mean `m`:
/// exposure `−½·(log2((P(79/80) + P(1))/2) + log2(m / (0.145·q(1))))`, contrast `log2(7·q(16/80)/q(64/80))`,
/// shadow `−log2(19.61·P(4/80)) − exposure`, highlight `−log2(P(76/80)/0.91) − exposure`.
fn auto_tone(rgb: &[f32], w: usize, h: usize) -> [f64; 4] {
    let mut v: Vec<f32> = Vec::with_capacity(w * h);
    let (mut s0, mut n) = (0f64, 0usize);
    // the analysis skips a one-pixel border
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let p = &rgb[(y * w + x) * 3..][..3];
            let m = (p[0].max(0.0) + p[1].max(0.0) + p[2].max(0.0)) / 3.0;
            v.push(m);
            s0 += m as f64;
            n += 1;
        }
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |k: usize| v[((v.len() as f64 * (k as f64 + 1.0) * 0.0125) as usize).min(v.len() - 1)].max(1e-9) as f64;
    let top = 0.25 * q(79) + 0.75 * q(78);
    let p = |k: usize| q(k) / top;
    let mean = s0 / n.max(1) as f64;
    let exposure = -0.5 * (((p(78) + p(79)) * 0.5).log2() + (mean / (0.145 * q(79))).log2());
    let contrast = (7.0 * q(15) / q(63)).log2();
    let shadow = -(19.61 * p(3)).log2() - exposure;
    let highlight = -(p(75) / 0.91).log2() - exposure;
    [exposure, contrast, shadow, highlight]
}

const SLIDERS: [&str; 5] =["exposure", "contrast", "shadow", "highlight", "fill"];

fn get_slider(s: &develop::Settings, k: &str) -> f64 {
    match k {
        "exposure" => s.exposure,
        "contrast" => s.contrast,
        "shadow" => s.shadow,
        "highlight" => s.highlight,
        "fill" => s.fill_light as f64,
        "saturation" => s.saturation as f64,
        _ => 0.0,
    }
}

fn set_slider(s: &mut develop::Settings, k: &str, v: f64) {
    match k {
        "exposure" => s.exposure = v,
        "contrast" => s.contrast = v,
        "shadow" => s.shadow = v,
        "highlight" => s.highlight = v,
        "fill" => s.fill_light = v as f32,
        "saturation" => s.saturation = v as f32,
        _ => eprintln!("unknown slider {k}"),
    }
}

fn apply_sets(s: &mut develop::Settings, sets: &[(String, f64)]) {
    for (k, v) in sets {
        set_slider(s, k, *v);
    }
}

/// Mean CIEDE2000 of a rendering against the reference (every 6th pixel).
fn mean_de(lut: &[f32], r: &Img16, c: &develop::Rgb16) -> f64 {
    let mut sum = 0f64;
    let mut n = 0usize;
    for y in (0..r.h.min(c.height)).step_by(6) {
        for x in (0..r.w.min(c.width)).step_by(6) {
            let i = (y * r.w + x) * 3;
            let j = (y * c.width + x) * 3;
            sum += raw::learn_look::de2000(lab_of(lut, &r.px[i..i + 3]), lab_of(lut, &c.data[j..j + 3])) as f64;
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

/// Coordinate search of the tone sliders that bring the rendering closest to the reference (mean CIEDE2000).
fn fit_sliders(s: &mut develop::Session, mut st: develop::Settings, r: &Img16) -> Result<develop::Settings> {
    let lut: Vec<f32> = (0..65536).map(|i| srgb_lin(i as f32 / 65535.0)).collect();
    let mut best = mean_de(&lut, r, &s.render_full(&st));
    eprintln!("fit start: dE00 {best:.3}");
    let mut step = 0.5f64;
    while step >= 0.05 {
        let mut improved = true;
        while improved {
            improved = false;
            for k in SLIDERS {
                for dir in [1.0, -1.0] {
                    let mut t = st.clone();
                    set_slider(&mut t, k, get_slider(&st, k) + dir * step);
                    let e = mean_de(&lut, r, &s.render_full(&t));
                    if e + 1e-4 < best {
                        best = e;
                        st = t;
                        improved = true;
                        eprintln!("  {k} {:+.2}: dE00 {e:.3}", get_slider(&st, k));
                    }
                }
            }
        }
        step *= 0.5;
    }
    eprintln!("fit: {} dE00 {best:.3}", SLIDERS.iter().map(|k| format!("{k}={:.2}", get_slider(&st, k))).collect::<Vec<_>>().join(" "));
    Ok(st)
}

struct Img16 {
    w: usize,
    h: usize,
    px: Vec<u16>,
}

fn load16(path: &str) -> Result<Img16> {
    let f = std::io::BufReader::new(std::fs::File::open(path).with_context(|| path.to_string())?);
    let mut dec = tiff::decoder::Decoder::new(f)?.with_limits(tiff::decoder::Limits::unlimited());
    let (w, h) = dec.dimensions()?;
    match dec.read_image()? {
        tiff::decoder::DecodingResult::U16(d) if d.len() == (w * h * 3) as usize => Ok(Img16 { w: w as usize, h: h as usize, px: d }),
        _ => bail!("{path}: 16-bit RGB expected"),
    }
}

fn srgb_lin(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn lab_of(lut: &[f32], p: &[u16]) -> [f32; 3] {
    let (r, g, b) = (lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]);
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.119192 * g + 0.9503041 * b;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn pct(v: &mut [f32], f: f64) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    let i = ((v.len() as f64 - 1.0) * f) as usize;
    *v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1
}

/// `sfd-scene FILE OUT.tif`: scene-linear float rendering of a file with standard settings (e.g. the app's X3I path).
pub fn scene(args: &[String]) -> Result<()> {
    let input = args.first().context("FILE missing")?;
    let out = PathBuf::from(args.get(1).context("OUT missing")?);
    let mut s = develop::Session::open(input).map_err(|e| anyhow::anyhow!("{e}"))?;
    let lin = s.render_full_scene(&develop::Settings::default(), true).map_err(|e| anyhow::anyhow!("{e}"))?;
    write_f32(&out, lin.width, lin.height, &lin.data)
}

/// `sfd-curve REF.tif SCENE.tif`: the reference's tone relation over the scene-linear luminance of a candidate
/// (float TIFF): per logarithmic luminance class the median and the 10/90 % quantiles of the reference luminance
/// (sRGB-encoded 0…1). A narrow spread means a global curve, a wide one a local (neighbourhood-dependent) operation.
pub fn curve(args: &[String]) -> Result<()> {
    let r = load16(args.first().context("REF missing")?)?;
    let path = args.get(1).context("SCENE missing")?;
    let f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut dec = tiff::decoder::Decoder::new(f)?.with_limits(tiff::decoder::Limits::unlimited());
    let (w, h) = dec.dimensions()?;
    let tiff::decoder::DecodingResult::F32(sc) = dec.read_image()? else { bail!("float TIFF expected") };
    if (w as usize, h as usize) != (r.w, r.h) {
        bail!("sizes differ");
    }
    let lut: Vec<f32> = (0..65536).map(|i| srgb_lin(i as f32 / 65535.0)).collect();
    const N: usize = 40;
    let mut bins: Vec<Vec<f32>> = vec![Vec::new(); N];
    for y in (0..r.h).step_by(3) {
        for x in (0..r.w).step_by(3) {
            let i = (y * r.w + x) * 3;
            let ys = 0.2126 * sc[i] + 0.7152 * sc[i + 1] + 0.0722 * sc[i + 2];
            if ys <= 1e-6 {
                continue;
            }
            let k = ((ys.log10() + 5.0) / 6.0 * N as f32).clamp(0.0, N as f32 - 1.0) as usize;
            let yr = 0.2126 * lut[r.px[i] as usize] + 0.7152 * lut[r.px[i + 1] as usize] + 0.0722 * lut[r.px[i + 2] as usize];
            let enc = if yr <= 0.0031308 { 12.92 * yr } else { 1.055 * yr.powf(1.0 / 2.4) - 0.055 };
            bins[k].push(enc);
        }
    }
    println!("{:>12} {:>8} {:>8} {:>8} {:>8}", "Y_scene", "n", "p10", "median", "p90");
    let mut knots: Vec<(f32, f32)> = Vec::new();
    for (k, b) in bins.iter_mut().enumerate() {
        if b.len() < 50 {
            continue;
        }
        let ys = 10f32.powf(k as f32 / N as f32 * 6.0 - 5.0);
        let med = pct(b, 0.5);
        knots.push(((ys.log10() + 5.0) / 6.0 * N as f32 + 0.5, med));
        println!("{ys:>12.6} {:>8} {:>8.4} {:>8.4} {:>8.4}", b.len(), pct(b, 0.1), med, pct(b, 0.9));
    }
    // `--apply OUT.tif`: the candidate's scene-linear image through the reference's measured tone relation (luminance
    // ratio, colour kept), as 16-bit sRGB: equal tone curves for a fair noise and colour comparison
    if let Some(i) = args.iter().position(|a| a == "--apply") {
        let out = args.get(i + 1).context("--apply OUT missing")?;
        knots.dedup_by(|a, b| a.1 <= b.1);
        let tone = |ys: f32| -> f32 {
            let t = (ys.max(1e-7).log10() + 5.0) / 6.0 * N as f32;
            let k = knots.partition_point(|p| p.0 < t);
            let enc = if k == 0 {
                knots[0].1
            } else if k >= knots.len() {
                knots[knots.len() - 1].1
            } else {
                let (a, b) = (knots[k - 1], knots[k]);
                a.1 + (b.1 - a.1) * (t - a.0) / (b.0 - a.0)
            };
            srgb_lin(enc.clamp(0.0, 1.0))
        };
        let enc = |v: f32| -> u16 {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (e * 65535.0 + 0.5) as u16
        };
        let data: Vec<u16> = sc
            .chunks_exact(3)
            .flat_map(|p| {
                let ys = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                let g = if ys > 1e-7 { tone(ys) / ys } else { 0.0 };
                [enc(p[0] * g), enc(p[1] * g), enc(p[2] * g)]
            })
            .collect();
        let f = std::io::BufWriter::new(std::fs::File::create(out)?);
        let mut e = tiff::encoder::TiffEncoder::new(f)?;
        e.write_image::<tiff::encoder::colortype::RGB16>(w, h, &data)?;
        println!("tone-matched candidate: {out}");
    }
    Ok(())
}

pub fn compare(args: &[String]) -> Result<()> {
    let r = load16(args.first().context("REF missing")?)?;
    let c = load16(args.get(1).context("CAND missing")?)?;
    if r.w != c.w || r.h != c.h {
        bail!("sizes differ: {}x{} / {}x{}", r.w, r.h, c.w, c.h);
    }
    let lut: Vec<f32> = (0..65536).map(|i| srgb_lin(i as f32 / 65535.0)).collect();
    let bins = [0.0f32, 10.0, 25.0, 50.0, 75.0, 101.0];
    let mut de: Vec<Vec<f32>> = vec![Vec::new(); bins.len() - 1];
    let mut dl: Vec<Vec<f32>> = vec![Vec::new(); bins.len() - 1];
    let mut all = Vec::new();
    let mut dcode: [Vec<f32>; 3] = Default::default();
    let (mut sy_ref, mut sy_c) = (0f64, 0f64);
    for y in (0..r.h).step_by(2) {
        for x in (0..r.w).step_by(2) {
            let i = (y * r.w + x) * 3;
            let (a, b) = (&r.px[i..i + 3], &c.px[i..i + 3]);
            let (la, lb) = (lab_of(&lut, a), lab_of(&lut, b));
            let d = raw::learn_look::de2000(la, lb);
            all.push(d);
            let k = bins.windows(2).position(|w| la[0] >= w[0] && la[0] < w[1]).unwrap_or(0);
            de[k].push(d);
            dl[k].push(lb[0] - la[0]);
            for ch in 0..3 {
                dcode[ch].push(b[ch] as f32 - a[ch] as f32);
            }
            let yr = 0.2126 * lut[a[0] as usize] + 0.7152 * lut[a[1] as usize] + 0.0722 * lut[a[2] as usize];
            let yc = 0.2126 * lut[b[0] as usize] + 0.7152 * lut[b[1] as usize] + 0.0722 * lut[b[2] as usize];
            if yr > 0.02 && yr < 0.6 {
                sy_ref += (yr * yc) as f64;
                sy_c += (yc * yc) as f64;
            }
        }
    }
    let n = all.len();
    let mean = all.iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    println!(
        "dE00 all: mean {mean:.2} median {:.2} p95 {:.2} max {:.1} ({n} px); gain a fit would apply {:.4}",
        pct(&mut all, 0.5),
        pct(&mut all, 0.95),
        pct(&mut all, 1.0),
        sy_ref / sy_c.max(1e-30)
    );
    for (k, w) in bins.windows(2).enumerate() {
        let m = de[k].iter().map(|v| *v as f64).sum::<f64>() / de[k].len().max(1) as f64;
        let ml = dl[k].iter().map(|v| *v as f64).sum::<f64>() / dl[k].len().max(1) as f64;
        println!(
            "  L* {:>3}-{:<3}: share {:5.1} %  dE00 mean {m:5.2} median {:5.2} p95 {:5.2}  dL* mean {ml:+.2}",
            w[0],
            w[1].min(100.0),
            100.0 * de[k].len() as f64 / n as f64,
            pct(&mut de[k], 0.5),
            pct(&mut de[k], 0.95)
        );
    }
    // brightness relation: median of reference / candidate luminance (linear) per reference luminance class
    {
        let edges = [0.0f32, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.4, 0.7, 1.01];
        let mut q: Vec<Vec<f32>> = vec![Vec::new(); edges.len() - 1];
        for y in (0..r.h).step_by(3) {
            for x in (0..r.w).step_by(3) {
                let i = (y * r.w + x) * 3;
                let yl = |p: &[u16]| 0.2126 * lut[p[0] as usize] + 0.7152 * lut[p[1] as usize] + 0.0722 * lut[p[2] as usize];
                let (yr, yc) = (yl(&r.px[i..i + 3]), yl(&c.px[i..i + 3]));
                if yc > 1e-5 {
                    if let Some(k) = edges.windows(2).position(|w| yr >= w[0] && yr < w[1]) {
                        q[k].push(yr / yc);
                    }
                }
            }
        }
        print!("  Y_ref/Y_cand by Y_ref:");
        for (k, w) in edges.windows(2).enumerate() {
            if q[k].len() > 100 {
                print!(" {}-{}: {:.3}", w[0], w[1], pct(&mut q[k], 0.5));
            }
        }
        println!();
    }
    for (ch, name) in ["R", "G", "B"].iter().enumerate() {
        let m = dcode[ch].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
        let mut a: Vec<f32> = dcode[ch].iter().map(|v| v.abs()).collect();
        println!("  code diff {name} (16 bit): mean {m:+.0} |d| median {:.0} p95 {:.0} max {:.0}", pct(&mut a, 0.5), pct(&mut a, 0.95), pct(&mut a, 1.0));
    }
    if let Some(i) = args.iter().position(|a| a == "--crops") {
        let out = args.get(i + 1).context("crops OUT missing")?;
        let spec = args.get(i + 2).context("crop list missing")?;
        let pts: Vec<(usize, usize)> = spec
            .split(';')
            .filter_map(|s| {
                let (a, b) = s.split_once(',')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            })
            .collect();
        const S: usize = 256;
        let (ow, oh) = (2 * S + 8, pts.len() * (S + 8));
        let mut img = image::RgbImage::new(ow as u32, oh as u32);
        for (k, &(x0, y0)) in pts.iter().enumerate() {
            for (j, src) in [&r, &c].iter().enumerate() {
                for y in 0..S {
                    for x in 0..S {
                        let (sx, sy) = ((x0 + x).min(src.w - 1), (y0 + y).min(src.h - 1));
                        let p = &src.px[(sy * src.w + sx) * 3..][..3];
                        img.put_pixel((j * (S + 8) + x) as u32, (k * (S + 8) + y) as u32, image::Rgb([(p[0] >> 8) as u8, (p[1] >> 8) as u8, (p[2] >> 8) as u8]));
                    }
                }
            }
        }
        img.save(out)?;
        println!("crops (reference left, candidate right): {out}");
    }
    Ok(())
}

/// Tone model of a merged bracket (test bench), per channel on scene-linear values `x`:
/// encoded `e = (x/x_max)^(1/2.4)`, levelled `u = e/top·ρ` (`top` = upper quantile of the encoded channel mean),
/// `t = u·2^(exposure/1.5)`, curve end `t_max = P·ρ·2^(exposure/1.5)` (`P` = maximum/top);
/// sigmoid `S(t) = 1/(1 + e^(−k·(a + t·(b − a))))`, `k = 2^clamp(contrast, ±2)`, `a = −1.17·2^(clamp(−shadow, ±2)/8)`,
/// `b = 2.85·2^(clamp(highlight, ±2)/8)`; output (encoded) `floor + (1 − floor)·(S(min(t, t_max)) − S(0))/(S(t_max) − S(0))`.
#[derive(Clone, Copy, Debug)]
struct ToneModel {
    top: f64,
    pmax: f64,
    rho: f64,
    exposure: f64,
    contrast: f64,
    shadow: f64,
    highlight: f64,
    floor: f64,
    lift: f64,
}

impl ToneModel {
    /// output for the encoded value `e` (0…1 relative to the scene maximum)
    fn eval_enc(&self, e: f64) -> f64 {
        let k = self.contrast.clamp(-2.0, 2.0).exp2();
        let a = -1.17 * ((-self.shadow).clamp(-2.0, 2.0) / 8.0).exp2();
        let b = 2.85 * (self.highlight.clamp(-2.0, 2.0) / 8.0).exp2();
        let s = |t: f64| 1.0 / (1.0 + (-k * (a + t * (b - a))).exp());
        let g = (self.exposure / 1.5).exp2() * self.rho;
        let tmax = self.pmax * g;
        // `lift`: gamma on the encoded input (shadow lift), 1 = none
        let t = (e.max(0.0).powf(self.lift) / self.top * g).min(tmax);
        let v = ((s(t) - s(0.0)) / (s(tmax) - s(0.0))).clamp(0.0, 1.0);
        // `floor` < 0: the curve value is read as sRGB-encoded and written with the pure power 1/2.4
        if self.floor < 0.0 {
            let lin = if v < 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            return lin.powf(1.0 / 2.4);
        }
        (self.floor + (1.0 - self.floor) * v).clamp(0.0, 1.0)
    }
}

fn load_f32(path: &str) -> Result<(usize, usize, Vec<f32>)> {
    let f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut dec = tiff::decoder::Decoder::new(f)?.with_limits(tiff::decoder::Limits::unlimited());
    let (w, h) = dec.dimensions()?;
    let tiff::decoder::DecodingResult::F32(d) = dec.read_image()? else { bail!("float TIFF expected") };
    Ok((w as usize, h as usize, d))
}

/// `sfd-tone REF.tif SCENE.tif [--set k=v,…] [--fit names] [--out OUT.tif]`: renders the scene-linear image through
/// [`ToneModel`] (start values: automatic tone values of the encoded image) and reports the absolute CIEDE2000.
pub fn tone(args: &[String]) -> Result<()> {
    let r = load16(args.first().context("REF missing")?)?;
    let (w, h, sc) = load_f32(args.get(1).context("SCENE missing")?)?;
    if (w, h) != (r.w, r.h) {
        bail!("sizes differ");
    }
    let mx = sc.iter().copied().fold(0f32, f32::max).max(1e-6) as f64;
    // `--clip c`: channel values clipped at c (scene-linear units) before the analysis and the curve
    let clip = args.iter().position(|a| a == "--clip").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::MAX);
    let mx = mx.min(clip);
    let enc: Vec<f32> = sc.iter().map(|v| (v.max(0.0) as f64 / mx).min(1.0).powf(1.0 / 2.4) as f32).collect();
    let a = auto_tone(&enc, w, h);
    // quantiles of the encoded channel mean
    let mut v: Vec<f32> = enc.chunks_exact(3).map(|p| (p[0] + p[1] + p[2]) / 3.0).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    let q = |k: usize| v[((v.len() as f64 * (k as f64 + 1.0) * 0.0125) as usize).min(v.len() - 1)] as f64;
    let top = 0.25 * q(79) + 0.75 * q(78);
    println!("automatic values (encoded): exposure {:.3} contrast {:.3} shadow {:.3} highlight {:.3}; top {top:.4}, max/top {:.4}", a[0], a[1], a[2], a[3], q(79) / top);
    let mut m = ToneModel { top, pmax: q(79) / top, rho: 1.0, exposure: a[0], contrast: a[1], shadow: a[2], highlight: a[3], floor: 0.0, lift: 1.0 };
    let names = ["rho", "exposure", "contrast", "shadow", "highlight", "floor", "lift"];
    let get = |m: &ToneModel, k: &str| match k {
        "rho" => m.rho,
        "exposure" => m.exposure,
        "contrast" => m.contrast,
        "shadow" => m.shadow,
        "highlight" => m.highlight,
        "floor" => m.floor,
        "lift" => m.lift,
        _ => f64::NAN,
    };
    let set = |m: &mut ToneModel, k: &str, v: f64| match k {
        "rho" => m.rho = v,
        "exposure" => m.exposure = v,
        "contrast" => m.contrast = v,
        "shadow" => m.shadow = v,
        "highlight" => m.highlight = v,
        "floor" => m.floor = v,
        "lift" => m.lift = v,
        _ => eprintln!("unknown {k}"),
    };
    let mut fit: Vec<String> = Vec::new();
    let mut it = args.iter().skip(2);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--fit" => fit = it.next().map(|s| s.split(',').map(String::from).collect()).unwrap_or_default(),
            "--set" => {
                for kv in it.next().map(|s| s.as_str()).unwrap_or("").split(',') {
                    if let Some((k, v)) = kv.split_once('=') {
                        set(&mut m, k, v.parse().unwrap_or(0.0));
                    }
                }
            }
            _ => {}
        }
    }
    {
        let mut v: Vec<f32> = sc.chunks_exact(3).step_by(97).map(|p| (p[0] + p[1] + p[2]) / 3.0).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        println!("scene maximum {mx:.4}, channel mean p50 {} p90 {}", v[v.len() / 2], v[v.len() * 9 / 10]);
    }
    let lut: Vec<f32> = (0..65536).map(|i| srgb_lin(i as f32 / 65535.0)).collect();
    let idx: Vec<usize> = (0..w * h).step_by(29).collect();
    let lab_ref: Vec<[f32; 3]> = idx.iter().map(|&i| lab_of(&lut, &r.px[i * 3..i * 3 + 3])).collect();
    // encoded input quantised to 4096 steps for a table per model
    let codes: Vec<[u16; 3]> = idx.iter().map(|&i| std::array::from_fn(|c| (enc[i * 3 + c].clamp(0.0, 1.0) * 4095.0).round() as u16)).collect();
    let table = |m: &ToneModel| -> Vec<u16> { (0..4096).map(|i| (m.eval_enc(i as f64 / 4095.0) * 65535.0 + 0.5) as u16).collect() };
    let score = |m: &ToneModel| -> f64 {
        let t = table(m);
        let s: f64 = codes.iter().zip(&lab_ref).map(|(c, l)| raw::learn_look::de2000(*l, lab_of(&lut, &c.map(|k| t[k as usize]))) as f64).sum();
        s / codes.len() as f64
    };
    println!("start: dE00 {:.3} ({})", score(&m), names.iter().map(|k| format!("{k}={:.3}", get(&m, k))).collect::<Vec<_>>().join(" "));
    if !fit.is_empty() {
        let n = fit.len();
        let mk = |x: &[f64]| {
            let mut t = m;
            for (k, v) in fit.iter().zip(x) {
                set(&mut t, k, *v);
            }
            t
        };
        let base: Vec<f64> = fit.iter().map(|k| get(&m, k)).collect();
        let mut sp: Vec<(Vec<f64>, f64)> = vec![(base.clone(), score(&mk(&base)))];
        for j in 0..n {
            let mut x = base.clone();
            x[j] += if x[j].abs() > 0.05 { 0.25 * x[j].abs() } else { 0.05 };
            let s = score(&mk(&x));
            sp.push((x, s));
        }
        for _ in 0..60 * n {
            sp.sort_by(|a, b| a.1.total_cmp(&b.1));
            let c: Vec<f64> = (0..n).map(|j| sp[..n].iter().map(|s| s.0[j]).sum::<f64>() / n as f64).collect();
            let worst = sp[n].clone();
            let pt = |f: f64| -> Vec<f64> { (0..n).map(|j| c[j] + f * (worst.0[j] - c[j])).collect() };
            let xr = pt(-1.0);
            let sr = score(&mk(&xr));
            if sr < sp[0].1 {
                let xe = pt(-2.0);
                let se = score(&mk(&xe));
                sp[n] = if se < sr { (xe, se) } else { (xr, sr) };
            } else if sr < sp[n - 1].1 {
                sp[n] = (xr, sr);
            } else {
                let xc = pt(0.5);
                let scc = score(&mk(&xc));
                if scc < worst.1 {
                    sp[n] = (xc, scc);
                } else {
                    let b0 = sp[0].0.clone();
                    for s in sp.iter_mut().skip(1) {
                        s.0 = (0..n).map(|j| b0[j] + 0.5 * (s.0[j] - b0[j])).collect();
                        s.1 = score(&mk(&s.0));
                    }
                }
            }
        }
        sp.sort_by(|a, b| a.1.total_cmp(&b.1));
        m = mk(&sp[0].0);
        println!("fit: dE00 {:.3} ({})", sp[0].1, names.iter().map(|k| format!("{k}={:.3}", get(&m, k))).collect::<Vec<_>>().join(" "));
    }
    if let Some(i) = args.iter().position(|a| a == "--out") {
        let data: Vec<u16> = enc.iter().map(|e| (m.eval_enc(*e as f64) * 65535.0 + 0.5) as u16).collect();
        let f = std::io::BufWriter::new(std::fs::File::create(args.get(i + 1).context("OUT")?)?);
        tiff::encoder::TiffEncoder::new(f)?.write_image::<tiff::encoder::colortype::RGB16>(w as u32, h as u32, &data)?;
    }
    Ok(())
}
