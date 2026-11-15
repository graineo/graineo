//! `graineo-dev merrill-new-fit <X3F …>`: measures the parameters of the alternative Merrill parts
//! against the default form on the SPP base of the given files (only outputs are compared):
//! - Highlight Control: start and width of the dcraw-shaped ramp against the default weight.
//!
//! Fill Light is measured against exported reference images instead (`fill-ref` below,
//! fit from a local reference measurement).

use anyhow::Result;
use rayon::prelude::*;
use std::fs::File;

pub fn fit(files: &[String]) -> Result<()> {
    std::env::remove_var(develop::merrill_new::VAR);
    let mut hc: Vec<[f32; 3]> = Vec::new(); // levels
    for f in files {
        let session = develop::Session::open(f)?;
        let s = develop::Settings::default();
        let base = session.ensure_spp(&s).map_err(anyhow::Error::msg)?;
        let c = &base.counts;
        let cal = &session.cal;
        let inv = base.matrix(cal).inverse().unwrap_or(develop::matrix::Mat3::IDENTITY).to_f32();
        let div: [f32; 3] = std::array::from_fn(|k| 1.0 / ((cal.count_scale * cal.wb_gain[k]) as f32 * cal.saturation as f32));
        let mut n = 0;
        for p in c.data.chunks_exact(3).step_by(3) {
            let cam: [f32; 3] = std::array::from_fn(|r| (inv[r * 3] * p[0] + inv[r * 3 + 1] * p[1] + inv[r * 3 + 2] * p[2]) * div[r]);
            if cam.iter().cloned().fold(0.0, f32::max) > 0.35 {
                hc.push(cam);
                n += 1;
            }
        }
        eprintln!("{f}: {n} bright samples");
    }
    // thin out the bright samples to at most 80 000
    let hstep = (hc.len() / 80_000).max(1);
    let hc: Vec<[f32; 3]> = hc.into_iter().step_by(hstep).collect();

    // Highlight Control
    let today: Vec<f32> = hc.iter().map(|l| ((0.03125 * l[0] + 0.03125 * l[1] + 0.9375 * l[2] - 0.62) / 0.24).clamp(0.0, 1.0)).collect();
    let active = today.iter().filter(|&&t| t > 0.0).count();
    for (name, pick) in [("min", 0usize), ("max", 1), ("mean", 2), ("top", 3)] {
        let lvl = |l: &[f32; 3]| match pick {
            0 => l[0].min(l[1]).min(l[2]),
            1 => l[0].max(l[1]).max(l[2]),
            2 => (l[0] + l[1] + l[2]) / 3.0,
            _ => l[2],
        };
        let grid: Vec<(f32, f32)> = (0..71).flat_map(|i| (0..96).map(move |j| (0.30 + 0.01 * i as f32, 0.05 + 0.01 * j as f32))).collect();
        let res: Vec<(f64, f32, f32)> = grid
            .par_iter()
            .map(|&(a, b)| {
                let e: f64 = hc
                    .iter()
                    .zip(&today)
                    .map(|(l, &t)| {
                        let u = ((lvl(l) - a) / b).clamp(0.0, 1.0);
                        let i = 1.0 - (1.0 - u) * (1.0 - u);
                        f64::from(i * i - t).powi(2)
                    })
                    .sum();
                (e / hc.len().max(1) as f64, a, b)
            })
            .collect();
        let best = res.iter().cloned().fold((f64::INFINITY, 0.0, 0.0), |m, r| if r.0 < m.0 { r } else { m });
        println!("highlight control ({name} layer): start {:.2}  width {:.2}  rms {:.4}; {} samples, {active} with today's weight > 0", best.1, best.2, best.0.sqrt(), hc.len());
    }
    Ok(())
}

/// `graineo-dev fill-ref <X3F> <out_dir> [factor]`: data for fitting Fill Light against exported reference images.
/// On the SPP base reduced by `factor` (default 4) it writes little-endian f32 files:
/// - `<stem>_y.f32`: luminance of the counts (0.2126/0.7152/0.0722),
/// - `<stem>_ramp.f32`: output luminance (linear sRGB) after rendering with a global gain 2^(k/4), k = −24 … 24
///   (49 planes; used to translate a reference image into an effective gain per pixel),
/// - `<stem>_heute_<v>.f32`: the default Fill Light log gain (natural log) for v = −2, −1, 1, 2 (full resolution, averaged),
/// - `<stem>.json`: width, height, factor.
pub fn fill_ref(args: &[String]) -> Result<()> {
    use std::io::Write;
    std::env::remove_var(develop::merrill_new::VAR);
    let file = args.first().ok_or_else(|| anyhow::anyhow!("X3F missing"))?;
    let out = std::path::PathBuf::from(args.get(1).ok_or_else(|| anyhow::anyhow!("output folder missing"))?);
    let f: usize = args.get(2).map(|v| v.parse()).transpose()?.unwrap_or(4);
    std::fs::create_dir_all(&out)?;
    let stem = std::path::Path::new(file).file_stem().unwrap().to_string_lossy().to_string();
    let session = develop::Session::open(file)?;
    // as `develop --spp --baseline`: shots in the camera's monochrome mode stay black and white (like SPP's RAW mode)
    let s = develop::sppa::baseline(file).unwrap_or_default();
    let base = session.ensure_spp(&s).map_err(anyhow::Error::msg)?;
    let cal = &session.cal;
    let m = base.matrix(cal);
    let full = &base.counts;
    let mut small = full.downsample(f);
    small.detail = None;
    let (w, h) = (small.width, small.height);
    let write = |name: String, v: &[f32]| -> Result<()> {
        let mut fh = std::io::BufWriter::new(File::create(out.join(name))?);
        for x in v {
            fh.write_all(&x.to_le_bytes())?;
        }
        Ok(())
    };
    write(format!("{stem}_y.f32"), &small.data.chunks_exact(3).map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect::<Vec<_>>())?;
    let lut: Vec<f32> = (0..65536).map(|i| {
        let v = i as f32 / 65535.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }).collect();
    let mut ramp = Vec::with_capacity(49 * w * h);
    let mut s0 = s.clone();
    s0.fill_light = 0.0;
    for k in -24..=24 {
        let g = 2f32.powf(k as f32 / 4.0);
        let mut c = small.clone();
        c.data.iter_mut().for_each(|x| *x *= g);
        let img = develop::render_counts(&c, cal, &s0, &m, None);
        ramp.extend(img.data.chunks_exact(3).map(|p| 0.2126 * lut[p[0] as usize] + 0.7152 * lut[p[1] as usize] + 0.0722 * lut[p[2] as usize]));
    }
    write(format!("{stem}_ramp.f32"), &ramp)?;
    let map = develop::filllight::log_gain_map_counts(full);
    for v in [-2.0f32, -1.0, 1.0, 2.0] {
        let curve = develop::filllight::FillLightCurve::new(v);
        let lg: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let mut sum = 0.0;
                for yy in y * f..y * f + f {
                    for xx in x * f..x * f + f {
                        sum += curve.factor(map[yy * full.width + xx]).ln();
                    }
                }
                sum / (f * f) as f32
            })
            .collect();
        write(format!("{stem}_heute_{v:+.0}.f32"), &lg)?;
    }
    std::fs::write(out.join(format!("{stem}.json")), format!("{{\"w\":{w},\"h\":{h},\"factor\":{f},\"orientation\":{}}}", cal.orientation))?;
    eprintln!("{file}: {w}×{h}");
    Ok(())
}
