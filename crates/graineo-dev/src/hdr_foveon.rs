//! `graineo-dev hdr-foveon FRAME.X3F... [--reference N] [--fusion] [--threads N] [--merged OUT.tif] [--render OUT.tif]`:
//! merge a Foveon exposure bracket (e.g. the seven X3F of an SFD X3I, cut out with `x3i-split`) in the linear layer
//! domain: every frame goes through the Foveon development up to the linearisation (`develop::linearize`, nothing of
//! the single-photo result changes), the three layers are the colour, and the merge core of the `hdr` crate does the
//! rest (exposure factors, alignment, noise-optimal weights per pixel, saturation of any layer excludes the pixel,
//! ghost handling). `--merged` writes the merged layers (float TIFF, counts / saturation, reference exposure = 1);
//! `--render` develops them with the standard Foveon rendering of the reference (16-bit sRGB TIFF).

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn info_of(path: &std::path::Path, w: usize, h: usize, wb: [f32; 3]) -> raw::Info {
    let e = [true, false].into_iter().find_map(|large| x3f::exif::parse_jpeg_exif(&x3f::read_embedded_jpeg_up_to(path, large, 128 * 1024).ok()?));
    let (t, iso, n, model) = e.map(|e| (e.exposure_time, e.iso, e.f_number, e.model)).unwrap_or((None, None, None, None));
    raw::Info {
        format: "X3F".into(),
        maker: "SIGMA".into(),
        model: model.unwrap_or_else(|| "Foveon".into()),
        mode: String::new(),
        width: w,
        height: h,
        channels: 3,
        bits: 14,
        sensor: raw::Sensor::Linear,
        cfa: String::new(),
        cfa_size: (1, 1),
        black: Vec::new(),
        black_pattern: (1, 1, 1),
        white: vec![1.0],
        // the merge applies the white balance of the camera setting (the fusion needs balanced channels); undone before rendering
        wb: Some(wb),
        matrix: None,
        matrix_illuminants: Vec::new(),
        orientation: 1,
        active: None,
        crop: None,
        fuji_rotated: false,
        baseline_exposure: 0.0,
        dng_profile: Vec::new(),
        exif: raw::Exif { iso, exposure_time: t.map(|v| v as f32), aperture: n.map(|v| v as f32), ..Default::default() },
        corrections: Vec::new(),
        calibration: Vec::new(),
        analog_balance: None,
        fuji_dr: None,
    }
}

/// Value of a fully saturated pixel of every layer after the linearisation (spatial gain and colour shading make it
/// different from pixel to pixel, so a plain per-layer clip level does not exist): the linearisation of a raw image
/// that holds the saturation count everywhere. Interleaved, `width × height × 3`.
fn clip_map(s: &develop::Session) -> Vec<f32> {
    let raw = s.raw();
    let planes: [Vec<u16>; 3] = std::array::from_fn(|c| vec![(s.cal.black[c] + s.cal.saturation).round().clamp(0.0, 65535.0) as u16; raw.columns * raw.rows]);
    let full = x3f::RawImage { columns: raw.columns, rows: raw.rows, planes };
    develop::linearize(&full, &s.cal).data
}

pub fn run(args: &[String]) -> Result<()> {
    let mut frames: Vec<PathBuf> = Vec::new();
    let mut settings = hdr::Settings::default();
    let (mut merged_out, mut render_out): (Option<PathBuf>, Option<PathBuf>) = (None, None);
    let mut threads = 4usize;
    let mut single: Option<usize> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--reference" => settings.reference = it.next().and_then(|v| v.parse().ok()),
            "--fusion" => settings.output = hdr::fusion::Output::Fusion,
            "--threads" => threads = it.next().and_then(|v| v.parse().ok()).unwrap_or(4),
            "--merged" => merged_out = it.next().map(PathBuf::from),
            "--render" => render_out = it.next().map(PathBuf::from),
            // develop only this frame (0-based, in the order given) with the same rendering: the single-frame comparison
            "--single" => single = it.next().and_then(|v| v.parse().ok()),
            _ => frames.push(PathBuf::from(a)),
        }
    }
    if frames.len() < 2 {
        bail!("at least two X3F frames needed");
    }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    pool.install(|| -> Result<()> {
        let t0 = Instant::now();
        if let Some(k) = single {
            let p = frames.get(k).context("--single: no such frame")?;
            let s = develop::Session::open(p).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
            let lin = develop::linearize(s.raw(), &s.cal);
            let img = develop::render(&lin, &s.cal, &develop::Settings::default(), None);
            let out = render_out.as_ref().context("--render needed with --single")?;
            let f = std::io::BufWriter::new(std::fs::File::create(out)?);
            let mut enc = tiff::encoder::TiffEncoder::new(f)?;
            enc.write_image::<tiff::encoder::colortype::RGB16>(img.width as u32, img.height as u32, &img.data)?;
            return Ok(());
        }
        let mut layers: Vec<(PathBuf, usize, usize, Vec<[f32; 3]>)> = Vec::new();
        let mut cals = Vec::new();
        for p in &frames {
            let s = develop::Session::open(p).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
            let lin = develop::linearize(s.raw(), &s.cal);
            let (w, h) = (lin.width, lin.height);
            // every layer relative to its own saturation value: clip = 1 everywhere
            let cm = clip_map(&s);
            let px: Vec<[f32; 3]> = lin.data.chunks_exact(3).zip(cm.chunks_exact(3)).map(|(c, m)| [c[0] / m[0].max(1e-6), c[1] / m[1].max(1e-6), c[2] / m[2].max(1e-6)]).collect();
            drop((lin, cm));
            layers.push((p.clone(), w, h, px));
            cals.push(s.cal.clone());
            eprintln!("loaded {} {w}x{h}", p.display());
        }
        // the layers saturate at their own level, the same in every frame
        let g = cals[0].wb_gain;
        let wb: [f32; 3] = [(g[0] / g[1]) as f32, 1.0, (g[2] / g[1]) as f32];
        eprintln!("white balance {wb:?}");
        let clip = hdr::frames::planar_clip(&layers.iter().map(|l| l.3.as_slice()).collect::<Vec<_>>());
        eprintln!("layer clip {clip:?}");
        let loaded: Vec<hdr::frames::Frame> = layers.into_iter().map(|(p, w, h, px)| hdr::frames::from_planar(p.clone(), info_of(&p, w, h, wb), px, w, h, clip)).collect();
        let t_load = t0.elapsed().as_secs_f64();
        let cancel = AtomicBool::new(false);
        let progress = |_: f32| {};
        let ctl = hdr::Control { cancel: &cancel, progress: &progress };
        let mut plan = hdr::plan_from_frames(loaded, &settings, &ctl).map_err(|e| anyhow::anyhow!("{e}"))?;
        // test of the noise model: `GRAINEO_FOVEON_NOISE=a,b` replaces the estimated model of every frame
        if let Some((na, nb)) = std::env::var("GRAINEO_FOVEON_NOISE").ok().and_then(|v| v.split_once(',').map(|(a, b)| (a.parse::<f32>().unwrap_or(0.0), b.parse::<f32>().unwrap_or(0.0)))) {
            plan.noise.iter_mut().for_each(|n| *n = (na, nb));
        }
        let s = plan.summary();
        let reference = plan.reference;
        // diagnostics: `GRAINEO_FOVEON_PATCH=x,y` prints every frame's mean layer values (divided by its exposure factor) of a 21×21 patch
        if let Some((px, py)) = std::env::var("GRAINEO_FOVEON_PATCH").ok().and_then(|v| v.split_once(',').map(|(a, b)| (a.parse::<usize>().unwrap_or(0), b.parse::<usize>().unwrap_or(0)))) {
            for (i, f) in plan.frames.iter().enumerate() {
                let pl = f.planar.as_ref().context("planar")?;
                let mut acc = [0f64; 3];
                for y in py - 10..=py + 10 {
                    for x in px - 10..=px + 10 {
                        for c in 0..3 {
                            acc[c] += pl[y * f.w + x][c] as f64;
                        }
                    }
                }
                let n = 441.0 * plan.k[i];
                eprintln!("patch frame {i} k {:.3}: {:.4} {:.4} {:.4}  (raw {:.4} {:.4} {:.4})", plan.k[i], acc[0] / n, acc[1] / n, acc[2] / n, acc[0] / 441.0, acc[1] / 441.0, acc[2] / 441.0);
            }
        }
        if std::env::var_os("GRAINEO_FOVEON_STATS").is_some() {
            for (i, f) in plan.frames.iter().enumerate() {
                let pl = f.planar.as_ref().context("planar")?;
                for c in 0..3 {
                    let mut v: Vec<f32> = pl.iter().step_by(7).map(|p| p[c]).collect();
                    v.sort_by(|a, b| a.total_cmp(b));
                    let q = |x: f64| v[((v.len() - 1) as f64 * x) as usize];
                    // histogram peak above 0.3 (width 0.005)
                    let mut hist = vec![0usize; 220];
                    for x in &v {
                        if *x > 0.3 && *x < 1.1 {
                            hist[((*x - 0.3) / 0.005) as usize] += 1;
                        }
                    }
                    let (pk, cnt) = hist.iter().enumerate().max_by_key(|(_, n)| **n).map(|(i, n)| (0.3 + 0.005 * i as f32, *n)).unwrap_or((0.0, 0));
                    eprintln!("stats frame {i} layer {c}: p99 {:.3} p99.9 {:.3} p99.99 {:.3} max {:.3} clip {:.3} hist-peak>0.3 at {pk:.3} ({cnt} of {})", q(0.99), q(0.999), q(0.9999), q(1.0), f.clip[c], v.len());
                }
            }
        }
        let t1 = Instant::now();
        let mut m = hdr::merge::full(&mut plan, &ctl).map_err(|e| anyhow::anyhow!("{e}"))?;
        // back to developed counts (reference units): the layers' own saturation values of the reference
        // (the merge delivers white-balanced values: back to counts for the rendering, which applies the balance itself)
        for p in m.px.iter_mut() {
            *p = [p[0] / wb[0], p[1], p[2] / wb[2]];
        }
        if settings.output == hdr::fusion::Output::Linear {
            let rs = develop::Session::open(&frames[reference]).map_err(|e| anyhow::anyhow!("{e}"))?;
            let cm = clip_map(&rs);
            anyhow::ensure!(cm.len() == m.px.len() * 3, "crop differs from the frame size");
            for (p, c) in m.px.iter_mut().zip(cm.chunks_exact(3)) {
                *p = [p[0] * c[0], p[1] * c[1], p[2] * c[2]];
            }
        }
        let t_merge = t1.elapsed().as_secs_f64();
        drop(plan);
        eprintln!("hdr-foveon: {}x{} ref {} EV {:?} shift {:?} warnings {:?}, load {t_load:.1} s, merge {t_merge:.1} s, noise a {:?} b {:?}", m.w, m.h, s.reference, s.ev, s.shift, s.warnings, m.noise.a, m.noise.b);
        if let Some(out) = &merged_out {
            let f = std::io::BufWriter::new(std::fs::File::create(out)?);
            let mut enc = tiff::encoder::TiffEncoder::new(f)?;
            let flat: Vec<f32> = m.px.iter().flatten().copied().collect();
            enc.write_image::<tiff::encoder::colortype::RGB32Float>(m.w as u32, m.h as u32, &flat)?;
        }
        if let Some(out) = &render_out {
            let lin = develop::Linear { mono: false, haze: None, toneq: None, width: m.w, height: m.h, data: m.px.iter().flatten().copied().collect(), detail: None };
            let img = develop::render(&lin, &cals[reference], &develop::Settings::default(), None);
            let f = std::io::BufWriter::new(std::fs::File::create(out).with_context(|| out.display().to_string())?);
            let mut enc = tiff::encoder::TiffEncoder::new(f)?;
            enc.write_image::<tiff::encoder::colortype::RGB16>(img.width as u32, img.height as u32, &img.data)?;
        }
        Ok(())
    })
}
