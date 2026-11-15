//! `graineo-dev color-chart-raw RAW OUTPUT --patch22 x0,y0,x1,y1[;…] --patch20 x0,y0,x1,y1 [--l20 L] [--no-sync]`
//!
//! Measuring aid for the color fidelity against a ColorChecker in the shot: develops a non-Sigma RAW
//! neutrally as in the app (crates/rawdev, preview path, no camera look, standard starting values),
//! sets the white balance on the neutral patch (`--patch22`, like the pipette: camera neutral of the patch →
//! temperature/tint; several patches separated by `;` are averaged) and the exposure so that the patch `--patch20` reaches L* = `--l20` (D50).
//! Rectangles are fractions (0…1) of the preview, sensor orientation (as `rawdev::color::render_preview_encoded`).
//!
//! Writes three float images (`GRF1`, width, height as u32 LE, then RGB f32 LE):
//! - `AUSGABE.f32`      the app's display path (extended sRGB, encoded, unclipped)
//! - `AUSGABE-lin.f32`  only the camera matrix (DNG ColorMatrix/ForwardMatrix of the file or rawler),
//!                      linear sRGB D65, scaled so that patch 20 reaches the target luminance
//! - `AUSGABE-std.f32`  like `-lin`, plus the built-in neutral correction of the standard development (if any)
//! - `AUSGABE-export.f32` the export path (sRGB encoded, gamut-mapped and clipped, as written to a file)
//! - `AUSGABE-kurve.f32`, `-kurve-h1.f32`, `-kurve-y.f32` decomposition of the rendering (linear sRGB, unclipped):
//!   the app's curve, the curve with full hue preservation, the curve on the luminance only
//!
//! and prints one line `FARBTAFEL {json}` with the settings found and the color data of the file.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use develop::Settings;
use raw::color;

fn rect(v: Option<String>) -> Result<[f32; 4]> {
    let v = v.context("rectangle missing")?;
    let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    anyhow::ensure!(n.len() == 4, "rectangle needs x0,y0,x1,y1");
    Ok([n[0], n[1], n[2], n[3]])
}

fn mean(px: &[[f32; 3]], w: usize, h: usize, r: [f32; 4]) -> [f32; 3] {
    let (x0, x1) = (((r[0] * w as f32) as usize).min(w - 1), ((r[2] * w as f32) as usize).min(w));
    let (y0, y1) = (((r[1] * h as f32) as usize).min(h - 1), ((r[3] * h as f32) as usize).min(h));
    let mut s = [0f64; 3];
    let mut n = 0f64;
    for y in y0..y1.max(y0 + 1) {
        for x in x0..x1.max(x0 + 1) {
            let p = px[y * w + x];
            for c in 0..3 {
                s[c] += p[c] as f64;
            }
            n += 1.0;
        }
    }
    s.map(|v| (v / n) as f32)
}

/// sRGB transfer mirrored below 0 (as the look stage delivers it)
fn decode(v: f32) -> f32 {
    v.signum() * graineo_color::srgb_decode(v.abs())
}

/// linear sRGB D65 → Lab D50 (Bradford)
fn lab(rgb: [f32; 3]) -> [f32; 3] {
    let m = color::mul(&color::bradford(color::xy_to_xyz(color::D65_XY), color::xy_to_xyz(color::D50_XY)), &color::srgb_to_xyz_d65());
    let xyz = color::mulv(&m, rgb);
    let w = color::xy_to_xyz(color::D50_XY);
    let f = |t: f32| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(xyz[0] / w[0]), f(xyz[1] / w[1]), f(xyz[2] / w[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn write_f32(path: &str, w: usize, h: usize, data: &[f32]) -> Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"GRF1")?;
    f.write_all(&(w as u32).to_le_bytes())?;
    f.write_all(&(h as u32).to_le_bytes())?;
    for v in data {
        f.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

pub fn run(file: &str, args: &[String]) -> Result<()> {
    let out = args.first().context("output file missing")?.trim_end_matches(".f32").to_string();
    let value = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    // `--patch22` may name several neutral patches separated by `;` (e.g. 20–22): the white balance then uses
    // the mean chromaticity of all of them (each patch normalised to its green, so all count alike)
    let r22s: Vec<[f32; 4]> = value("--patch22").context("rectangle missing")?.split(';').map(|r| rect(Some(r.to_string()))).collect::<Result<_>>()?;
    let r22 = r22s[0];
    let r20 = rect(value("--patch20"))?;
    let l20: f32 = value("--l20").and_then(|v| v.parse().ok()).unwrap_or(66.8);
    let adjust = !args.iter().any(|a| a == "--no-sync");
    let doc = rawdev::RawDoc::open(Path::new(file), &|| true).map_err(|e| anyhow!(e))?.context("cancelled")?;
    let mut s = Settings::default();
    s.raw.look = "neutral".into();
    let st = doc.preview_stage(&s);
    let (w, h) = (st.w, st.h);
    let wb = doc.info.wb.unwrap_or([1.0; 3]);
    let n_shot = [wb[1] / wb[0], 1.0, wb[1] / wb[2]];
    // pipette on patch 22: camera neutral = white-balanced stage value × as-shot neutral
    let c22 = r22s.iter().fold([0f32; 3], |acc, r| {
        let m = mean(&st.px, w, h, *r);
        [acc[0] + m[0] / m[1] / r22s.len() as f32, 1.0, acc[2] + m[2] / m[1] / r22s.len() as f32]
    });
    let n22 = [c22[0] * n_shot[0] / (c22[1] * n_shot[1]), 1.0, c22[2] * n_shot[2] / (c22[1] * n_shot[1])];
    if adjust {
        let (k, t) = rawdev::wb::temp_from_neutral(&doc.info, n22).context("no temperature for the neutral patch")?;
        s.raw.wb_mode = "custom".into();
        s.raw.temperature = k;
        s.raw.tint = t;
    }
    // exposure: secant method on L* of patch 20
    let measure = |s: &Settings| -> Result<(rawdev::color::Wide, [f32; 3], [f32; 3])> {
        let e = rawdev::color::render_preview_encoded(&doc, s).map_err(|e| anyhow!(e))?.context("no display path")?;
        let px: Vec<[f32; 3]> = e.rgb.chunks_exact(3).map(|p| [decode(p[0]), decode(p[1]), decode(p[2])]).collect();
        let (a, b) = (lab(mean(&px, e.width, e.height, r20)), lab(mean(&px, e.width, e.height, r22)));
        Ok((e, a, b))
    };
    let (mut e, mut l20m, mut l22m) = measure(&s)?;
    if adjust {
        let (mut x0, mut f0) = (s.exposure, l20m[0] - l20);
        let mut x1 = x0 + (-f0 / 30.0) as f64; // ~30 L* per EV in the midtones as first step
        for _ in 0..12 {
            s.exposure = x1;
            (e, l20m, l22m) = measure(&s)?;
            let f1 = l20m[0] - l20;
            if f1.abs() < 0.05 || (f1 - f0).abs() < 1e-6 {
                break;
            }
            let x2 = x1 - f1 as f64 * (x1 - x0) / (f1 - f0) as f64;
            (x0, f0, x1) = (x1, f1, x2.clamp(-4.0, 4.0));
        }
    }
    write_f32(&format!("{out}.f32"), e.width, e.height, &e.rgb)?;
    // matrix only: as rawdev::render::Params (target neutral, scale of the as-shot white balance)
    let n_target = doc.target_neutral(&s).unwrap_or(n_shot);
    let scale_wb = [n_shot[0] / n_target[0], n_shot[1] / n_target[1], n_shot[2] / n_target[2]];
    let ab = doc.info.analog_balance.map(color::diag).unwrap_or(color::diag([1.0; 3]));
    let col = color::determine(n_target, &doc.info.calibration, &ab).context("no color matrix")?;
    let m = color::mul(&color::xyz_d50_to(&color::srgb_to_xyz_d65()), &color::mul(&col.camera_to_xyz_d50, &color::diag(scale_wb)));
    let mut lin: Vec<[f32; 3]> = st.px.iter().map(|p| color::mulv(&m, *p)).collect();
    // target luminance of patch 20 (Y of L* l20)
    let fy = (l20 + 16.0) / 116.0;
    let y_target = if fy > 6.0 / 29.0 { fy * fy * fy } else { (l20 - 16.0) / 116.0 * 3.0 * 36.0 / 841.0 };
    let m20 = mean(&lin, w, h, r20);
    let k = y_target / (0.2126 * m20[0] + 0.7152 * m20[1] + 0.0722 * m20[2]);
    lin.iter_mut().for_each(|p| *p = p.map(|v| v * k));
    write_f32(&format!("{out}-lin.f32"), w, h, &lin.iter().flatten().copied().collect::<Vec<f32>>())?;
    let std_look = raw::development::camera_look(&doc.info.maker, &doc.info.model);
    let std_lut = std_look.as_ref().map(raw::development::LookLut::new);
    if let Some(t) = &std_lut {
        let v: Vec<f32> = lin.iter().flat_map(|p| t.apply(*p)).collect();
        write_f32(&format!("{out}-std.f32"), w, h, &v)?;
    }
    // export path of the app (sRGB, gamut-mapped and clipped), as written to a file
    let ex = doc.render_preview(&s);
    write_f32(&format!("{out}-export.f32"), ex.width, ex.height, &ex.data.iter().map(|v| *v as f32 / 65535.0).collect::<Vec<f32>>())?;
    // decomposition of the rendering (as rawdev::render::Params at default sliders, linear sRGB, unclipped):
    // `-kurve` the app's curve (per channel, hue preservation 0.2), `-kurve-h1` full hue preservation,
    // `-kurve-y` the same curve on the luminance only (all channels scaled alike)
    let ws_xyz = color::rec2020_to_xyz_d65();
    let cam_ws = color::mul(&color::xyz_d50_to(&ws_xyz), &col.camera_to_xyz_d50);
    let gain = (doc.base_ev + s.exposure as f32).exp2();
    let m_app = color::mul(&cam_ws, &color::diag(scale_wb)).map(|r| r.map(|v| v * gain));
    let ws_srgb = color::mul(&color::inv(&color::srgb_to_xyz_d65()).context("sRGB")?, &ws_xyz);
    let lum = ws_xyz[1];
    let base = raw::development::default_tone();
    for (name, hue, luma_only) in [("curve", base.hue, false), ("curve-h1", 1.0, false), ("curve-y", base.hue, true)] {
        let c = raw::tone::ToneCurve::new(raw::tone::Sigmoid { hue, ..base });
        let v: Vec<f32> = st
            .px
            .iter()
            .flat_map(|p| {
                let v = color::mulv(&m_app, *p);
                let t = if luma_only {
                    let y = (lum[0] * v[0] + lum[1] * v[1] + lum[2] * v[2]).max(1e-9);
                    let f = c.pixel([y; 3], lum)[1] / y;
                    v.map(|x| x * f)
                } else {
                    c.pixel(v, lum)
                };
                let s = color::mulv(&ws_srgb, t);
                std_lut.as_ref().map_or(s, |l| l.apply(s))
            })
            .collect();
        write_f32(&format!("{out}-{name}.f32"), w, h, &v)?;
    }
    let cal: Vec<serde_json::Value> = doc
        .info
        .calibration
        .iter()
        .map(|c| serde_json::json!({"kelvin": c.kelvin, "color_matrix": c.color, "forward_matrix": c.forward}))
        .collect();
    let j = serde_json::json!({
        "camera": raw::camera_display(&doc.info.maker, &doc.info.model),
        "format": doc.info.format,
        "preview": [w, h],
        "full": doc.full_size,
        "orientation": doc.orientation,
        "as_shot": {"kelvin": doc.as_shot.0, "tint": doc.as_shot.1, "neutral": n_shot},
        "neutral_patch22": n22,
        "white_balance": {"kelvin": s.raw.temperature, "tint": s.raw.tint, "mode": s.raw.wb_mode},
        "exposure_ev": s.exposure,
        "basis_ev": doc.base_ev,
        "baseline_exposure": doc.info.baseline_exposure,
        "patch20_lab": l20m,
        "patch22_lab": l22m,
        "matrix": {"kelvin": col.kelvin, "weight_illuminant1": col.weight, "forward": col.forward, "camera_to_xyz_d50": col.camera_to_xyz_d50},
        "calibration": cal,
        "analog_balance": doc.info.analog_balance,
        "dng_profile": format!("{:?}", doc.info.dng_profile),
        "standard_correction": std_look.is_some(),
        "lin_factor": k,
    });
    println!("COLOR_CHART\t{j}");
    Ok(())
}
