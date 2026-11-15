//! Point-wise colour/tone response for the chart and a later LUT export.
//! Spatial effects cannot be represented this way and are deliberately left out.
use super::*;
pub fn color_response(fs: &FilmSettings, colors: &[[f32; 3]]) -> Result<Vec<[f32; 3]>, String> {
    if colors.is_empty() || colors.len() > 40_000 {
        return Err("Ungültige Anzahl Farbstützpunkte".into());
    }
    let (film, paper, scan) = resolve(fs)?;
    let corr = correction(fs)?;
    let mut p = runtime_params(fs, Geo::full(colors.len(), 1), colors.len(), 1, scan, corr);
    p.film_render.grain.active = false;
    p.film_render.halation.active = false;
    // A colour LUT assumes a uniform surround for each colour: DIR channel interaction
    // is kept, its spatial diffusion becomes the identity. No mixing of neighbouring
    // cube samples, whose arrangement in the probe image is arbitrary.
    p.film_render.dir_couplers.diffusion_size_um = 0.;
    p.film_render.dir_couplers.diffusion_tail_um = 0.;
    p.film_render.dir_couplers.diffusion_tail_weight = 0.;
    p.camera.lens_blur_um = 0.;
    p.camera.diffusion_filter.active = false;
    p.enlarger.lens_blur = 0.;
    p.enlarger.diffusion_filter.active = false;
    p.scanner.lens_blur = 0.;
    p.scanner.unsharp_mask = [0., 0.];
    p.film_render.glare.blur = 0.;
    p.print_render.glare.blur = 0.;
    let cache_key = serde_json::to_string(fs).map_err(|e| e.to_string())?;
    static CACHE: Mutex<Vec<(String, Arc<Pipeline>)>> = Mutex::new(Vec::new());
    let cached = CACHE
        .lock()
        .unwrap()
        .iter()
        .find(|v| v.0 == cache_key)
        .map(|v| v.1.clone());
    let pipe = if let Some(pipe) = cached {
        pipe
    } else {
        let dir = data_dir().ok_or("Filmdaten fehlen")?;
        // The existing constructor applies paper tone and taking filter.
        let mut pp = load_profile(&paper)?;
        if !scan && fs.paper_tone != 0. && pp.is_bw() {
            let a = 0.2 * fs.paper_tone.clamp(-1., 1.) as f64;
            for (row, w) in pp
                .data
                .channel_density
                .iter_mut()
                .zip(pp.data.wavelengths.iter())
            {
                if let Some(d) = row.first_mut() {
                    *d *= (1. + a * (550. - w) / 150.).max(0.2);
                }
            }
        }
        let fp = profile_with_filter(&film, &effective_filter(fs).0, effective_filter(fs).1)?;
        let pipe = Arc::new(Pipeline::new_with_spectral(fp, pp, p.clone(), dir)?);
        let mut c = CACHE.lock().unwrap();
        if c.len() >= 32 {
            c.remove(0);
        }
        c.push((cache_key, pipe.clone()));
        pipe
    };
    let data = colors
        .iter()
        .flat_map(|c| c.map(|v| from_f32(srgb_decode(v.max(0.)))))
        .collect();
    let input = ImageBuf::from_data(colors.len() as u32, 1, data);
    let out = with_film_gate(move || (*pipe).clone().with_params(p).process(input, &spektrafilm_gpu::cpu_backend::CpuBackend));
    if scan && (fs.scan_black != 0. || fs.scan_white != 1.) {
        let mut data = out
            .data
            .iter()
            .map(|v| (to_f32(*v).clamp(0., 1.) * 65535. + 0.5) as u16)
            .collect::<Vec<_>>();
        super::scan::apply_scan_levels(&mut data, fs, true);
        return Ok(data
            .chunks_exact(3)
            .map(|p| {
                p.try_into()
                    .map(|p: [u16; 3]| p.map(|v| v as f32 / 65535.))
                    .unwrap()
            })
            .collect());
    }
    Ok(out
        .data
        .chunks_exact(3)
        .map(|p| [to_f32(p[0]), to_f32(p[1]), to_f32(p[2])].map(|v| v.clamp(0., 1.)))
        .collect())
}
/// Curves from the datasheet plus the actual neutral overall output (no curve multiplication).
pub fn tone_curves(fs: &FilmSettings) -> Result<Vec<Series>, String> {
    let (_, paper, scan) = resolve(fs)?;
    let p = runtime_params(fs, Geo::full(1, 1), 1, 1, scan, correction(fs)?);
    let profile_curve =
        |id: &str, want: Option<f64>, gamma: f32, label: &str| -> Result<Series, String> {
            let pr = load_profile(id)?;
            let times = &pr.data.development_time;
            let col = want
                .and_then(|v| {
                    times
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| (**a - v).abs().total_cmp(&(**b - v).abs()))
                        .map(|(i, _)| i)
                })
                .unwrap_or(0);
            let mut pts = Vec::new();
            for (i, &x) in pr.data.log_exposure.iter().enumerate().step_by(4) {
                if let Some(row) = pr.data.density_curves.get(i) {
                    let y = if pr.is_bw() {
                        row.get(col).copied().unwrap_or(0.)
                    } else {
                        row.iter().take(3).sum::<f64>() / 3.
                    };
                    pts.push((x as f32, y as f32 * gamma));
                }
            }
            let max = pts.iter().map(|v| v.1).fold(0f32, f32::max).max(0.001);
            for pt in &mut pts {
                pt.1 /= max;
            }
            Ok(Series {
                name: label.into(),
                points: pts,
            })
        };
    let mut curves = vec![profile_curve(
        &fs.stock,
        p.film_render.development_time,
        p.film_render.density_curve_gamma,
        "Film · relative Dichte",
    )?];
    if !scan {
        // Papers with a fitted curve model take the gamma through the print-curve morph (see
        // `runtime_params`): show exactly the morphed curve that is printed with.
        let pr = load_profile(&paper)?;
        let morphed = pr.data.density_curves_model.as_ref().and_then(|model| {
            spektrafilm_core::print_morph::morph_density_curves(
                &pr.log_exposure_f64(),
                model,
                &p.print_render.density_curves_morph,
                pr.is_positive(),
            )
            .ok()
        });
        match morphed {
            Some(rows) => {
                let mut pts: Vec<(f32, f32)> = pr
                    .data
                    .log_exposure
                    .iter()
                    .zip(&rows)
                    .step_by(4)
                    .map(|(&x, r)| (x as f32, ((r[0] + r[1] + r[2]) / 3.) as f32))
                    .collect();
                let max = pts.iter().map(|v| v.1).fold(0f32, f32::max).max(0.001);
                for pt in &mut pts {
                    pt.1 /= max;
                }
                curves.push(Series {
                    name: "Papier · relative Dichte".into(),
                    points: pts,
                });
            }
            None => curves.push(profile_curve(
                &paper,
                p.print_render.development_time,
                p.print_render.density_curve_gamma,
                "Papier · relative Dichte",
            )?),
        }
    }
    let mut input = Vec::new();
    let mut xs = Vec::new();
    for i in 0..65 {
        let x = -2.4 + i as f32 * 4.8 / 64.;
        let linear = 0.184 * 10f32.powf(x);
        let code = if linear <= 0.0031308 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1. / 2.4) - 0.055
        };
        input.push([code; 3]);
        xs.push(x);
    }
    let rgb = color_response(fs, &input)?;
    curves.push(Series {
        name: "Gesamt · Ausgabe".into(),
        points: xs
            .into_iter()
            .zip(
                rgb.iter()
                    .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]),
            )
            .collect(),
    });
    Ok(curves)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tone_curve_tracks_paper_and_exposure() {
        let f = FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            grain: 0.,
            ..Default::default()
        };
        let a = tone_curves(&f).unwrap();
        assert_eq!(a.len(), 3);
        assert!(a
            .iter()
            .flat_map(|r| &r.points)
            .all(|p| p.0.is_finite() && p.1.is_finite()));
        let b = tone_curves(&FilmSettings {
            paper_grade: 5.,
            push: 1.,
            ..f
        })
        .unwrap();
        assert_ne!(a.last().unwrap().points, b.last().unwrap().points);
    }

    /// Paper gamma reaches colour papers (fitted curve model) through the print-curve morph:
    /// both the shown paper curve and the overall output change.
    #[test]
    fn paper_gamma_acts_on_colour_paper() {
        let f = FilmSettings {
            active: true,
            stock: "kodak_portra_400".into(),
            grain: 0.,
            auto_balance: false,
            ..Default::default()
        };
        let a = tone_curves(&f).unwrap();
        let b = tone_curves(&FilmSettings { paper_gamma: 1.3, ..f }).unwrap();
        assert_ne!(a[1].points, b[1].points, "paper curve");
        assert_ne!(a[2].points, b[2].points, "overall output");
    }
}
