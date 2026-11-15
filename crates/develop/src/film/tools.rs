//! Additional darkroom parameters of the existing spektrafilm core.
//! Ideas: spektrafilm, Andrea Volpato (GPL-3.0); no third-party GUI components.
use super::FilmSettings;
use spektrafilm_core::params::RuntimeParams;
pub(super) fn parameter(f: &FilmSettings, p: &mut RuntimeParams, scale: f32, scan: bool) {
    let h = &mut p.film_render.halation;
    match f.halation_type.as_str() {
        "none" => {
            h.active = false;
        }
        "subtle" => {
            h.halation_amount *= 0.35;
            h.scatter_amount *= 0.5;
        }
        "strong" => {
            h.halation_amount *= 2.;
            h.halation_first_sigma_um = [90., 75., 65.];
        }
        "red" => {
            h.halation_strength = [0.20, 0.008, 0.];
            h.halation_first_sigma_um = [100., 65., 50.];
        }
        _ => {}
    }
    let family = match f.diffusion.as_str() {
        "soft" => Some("black_pro_mist"),
        "mist" => Some("pro_mist"),
        "bloom" => Some("cinebloom"),
        _ => None,
    };
    if let Some(family) = family.filter(|_| f.diffusion_amount > 0.0) {
        if !f.diffusion_enlarger || !scan {
            let d = if f.diffusion_enlarger {
                &mut p.enlarger.diffusion_filter
            } else {
                &mut p.camera.diffusion_filter
            };
            d.active = true;
            d.filter_family = family.into();
            d.strength = f.diffusion_amount.clamp(0., 1.);
        }
    }
    // Presets tuned by eye, not measured profiles of specific lenses.
    match f.optics.as_str() {
        "sharp" => {
            p.scanner.unsharp_mask[1] = 1.4;
        }
        "soft" => {
            p.camera.lens_blur_um = 12.;
            p.enlarger.lens_blur = 0.3 / scale;
            p.scanner.lens_blur = 0.6 / scale;
        }
        "vintage" => {
            p.camera.lens_blur_um = 25.;
            p.enlarger.lens_blur = 0.6 / scale;
            p.scanner.lens_blur = 0.9 / scale;
            p.scanner.unsharp_mask[1] = 0.2;
        }
        _ => {}
    }
    if let Some(v) = f.camera_blur_um {
        p.camera.lens_blur_um = v.clamp(0., 50.);
    }
    if let Some(v) = f.enlarger_blur {
        p.enlarger.lens_blur = v.clamp(0., 3.) / scale;
    }
    if let Some(v) = f.scanner_blur {
        p.scanner.lens_blur = v.clamp(0., 3.) / scale;
    }
    if !scan && f.preflash > 0.0 {
        p.enlarger.preflash_exposure = f.preflash.clamp(0., 1.) * 0.02;
        // Only colour paper has a pre-flash colour.
        if !super::stock(&f.stock).is_some_and(|s| s.monochrome) {
            p.enlarger.preflash_y_filter_shift = -15. * f.preflash_warmth.clamp(-1., 1.);
            p.enlarger.preflash_m_filter_shift = -15. * f.preflash_tint.clamp(-1., 1.);
        }
    }
}
/// Pre-flash is baked in when the pipeline is built, not updated by with_params.
pub(super) fn calibration_key(p: &RuntimeParams) -> String {
    format!(
        "{}|{}|{}",
        p.enlarger.preflash_exposure,
        p.enlarger.preflash_y_filter_shift,
        p.enlarger.preflash_m_filter_shift
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preflash_zero_is_inert_and_old_json_loads() {
        let f: FilmSettings = serde_json::from_str("{}").unwrap();
        let mut p = RuntimeParams::default();
        let before = serde_json::to_value(&p).unwrap();
        parameter(&f, &mut p, 1., false);
        assert_eq!(before, serde_json::to_value(&p).unwrap());
        let f = FilmSettings {
            preflash: 0.5,
            preflash_warmth: 0.3,
            ..f
        };
        assert_eq!(
            f,
            serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap()
        );
    }
    #[test]
    fn preflash_measured_paper_highlights() {
        let scene = crate::Linear { mono: false, haze: None, toneq: None,
            width: 64,
            height: 64,
            data: vec![0.8; 64 * 64 * 3],
            detail: None,
        };
        let mut f = FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            grain: 0.,
            halation: 0.,
            couplers: 0.,
            auto_balance: false,
            ..Default::default()
        };
        let mut values = Vec::new();
        for v in [0., 0.25, 0.5, 1., 0.] {
            f.preflash = v;
            let im = super::super::develop(&scene, &f, crate::look::Geo::full(64, 64)).unwrap();
            values.push(im.data.iter().map(|&p| p as f64).sum::<f64>() / im.data.len() as f64);
        }
        eprintln!("pre-flash 0/.25/.5/1/0, mean sRGB16: {values:?}");
        assert_eq!(values[0], values[4]);
        assert!(values[0] > values[1] && values[1] > values[2] && values[2] > values[3]);
    }
}

#[cfg(test)]
mod optics_tests {
    use super::*;
    #[test]
    fn optics_changes_edge_contrast() {
        let (w, h) = (256, 64);
        let scene = crate::Linear { mono: false, haze: None, toneq: None,
            width: w,
            height: h,
            data: (0..w * h)
                .flat_map(|i| [if i % w < w / 2 { 0.12 } else { 0.28 }; 3])
                .collect(),
            detail: None,
        };
        let base = FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            grain: 0.,
            halation: 0.,
            couplers: 0.,
            auto_balance: false,
            ..Default::default()
        };
        let mut scores = Vec::new();
        for mode in ["", "sharp", "soft", "vintage"] {
            let f = FilmSettings {
                optics: mode.into(),
                ..base.clone()
            };
            let geo = crate::look::Geo {
                full_w: 6000,
                full_h: 4000,
                ..crate::look::Geo::full(w, h)
            };
            let im = super::super::develop(&scene, &f, geo).unwrap();
            let mut sum = 0.;
            for y in 16..h - 16 {
                for x in w / 2 - 1..=w / 2 + 1 {
                    sum += (im.data[(y * w + x) * 3] as f64 - im.data[(y * w + x - 1) * 3] as f64)
                        .abs();
                }
            }
            scores.push(sum / ((h - 32) * 3) as f64);
        }
        eprintln!("edge gradient standard/sharp/soft/vintage: {scores:?}");
        assert!(scores[1] > scores[0]);
        assert!(scores[2] < scores[0]);
        assert!(scores[3] < scores[2]);
    }
}

#[cfg(test)]
mod diffusion_tests {
    use super::*;
    #[test]
    fn diffusion_off_preserves_params_and_point_halo_is_measurable() {
        let (w, h) = (384usize, 256usize);
        let mut scene = crate::Linear { mono: false, haze: None, toneq: None,
            width: w,
            height: h,
            data: vec![0.004; w * h * 3],
            detail: None,
        };
        for y in h / 2 - 2..=h / 2 + 2 {
            for x in w / 2 - 2..=w / 2 + 2 {
                scene.data[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(5.);
            }
        }
        let base = FilmSettings {
            active: true,
            stock: "ilford_hp5_plus".into(),
            grain: 0.,
            halation: 0.,
            couplers: 0.,
            auto_balance: false,
            ..Default::default()
        };
        let mut ring = Vec::new();
        let mut times = Vec::new();
        for family in ["", "soft", "mist", "bloom"] {
            let f = FilmSettings {
                diffusion: family.into(),
                ..base.clone()
            };
            let _warm = super::super::develop(
                &scene,
                &f,
                crate::look::Geo {
                    full_w: 6000,
                    full_h: 4000,
                    ..crate::look::Geo::full(w, h)
                },
            )
            .unwrap();
            let now = std::time::Instant::now();
            let im = super::super::develop(
                &scene,
                &f,
                crate::look::Geo {
                    full_w: 6000,
                    full_h: 4000,
                    ..crate::look::Geo::full(w, h)
                },
            )
            .unwrap();
            times.push(now.elapsed().as_secs_f64() * 1000.);
            let mut sum = 0.;
            let mut n = 0.;
            for y in 0..h {
                for x in 0..w {
                    let d = ((x as f32 - w as f32 / 2.).powi(2)
                        + (y as f32 - h as f32 / 2.).powi(2))
                    .sqrt();
                    if (5.0..15.0).contains(&d) {
                        sum += im.data[(y * w + x) * 3] as f64;
                        n += 1.;
                    }
                }
            }
            ring.push(sum / n - im.data[(16 * w + 16) * 3] as f64);
        }
        eprintln!(
            "diffusion off/soft/mist/bloom: ring sRGB16={ring:?}, time ms={times:?}"
        );
        assert!(ring[1] > ring[0]);
        assert!(ring[2] > ring[0]);
        assert!(ring[3] > ring[0]);
    }
}

#[cfg(test)]
mod halo_tests {
    use super::*;
    #[test]
    fn halo_point_ring_grows() {
        let scene = super::super::light_points(1024, 512);
        let mut scores = Vec::new();
        for kind in ["none", "subtle", "", "strong", "red"] {
            let f = FilmSettings {
                active: true,
                stock: "kodak_portra_400".into(),
                halation_type: kind.into(),
                grain: 0.,
                couplers: 0.,
                auto_balance: false,
                ..Default::default()
            };
            let im = super::super::develop(
                &scene,
                &f,
                crate::look::Geo {
                    full_w: 6000,
                    full_h: 3000,
                    ..crate::look::Geo::full(1024, 512)
                },
            )
            .unwrap();
            let mut sum = 0.;
            let mut n = 0.;
            for y in 512 / 3 - 35..512 / 3 + 35 {
                for x in 921usize - 35..921 + 35 {
                    let d =
                        ((x as f32 - 921.).powi(2) + (y as f32 - (512 / 3) as f32).powi(2)).sqrt();
                    if (10.0..30.0).contains(&d) {
                        sum += im.data[(y * 1024 + x) * 3] as f64;
                        n += 1.;
                    }
                }
            }
            scores.push(sum / n);
        }
        eprintln!("halo ring red, none/subtle/film/strong/red glow: {scores:?}");
        assert!(scores[2] > scores[0]);
        assert!(scores[3] > scores[2]);
        assert!(scores[4] > scores[2]);
    }
}

#[cfg(test)] mod performance {
 use super::*;
 #[test] #[ignore="explicit 2.8/24 MP benchmark in scratch"]
 fn film_tools_preview_and_24mp(){
  assert!(std::env::var_os("GRAINEO_DATA_DIR").is_some());
  let f=FilmSettings{active:true,stock:"ilford_hp5_plus".into(),auto_balance:false,..Default::default()};
  for (w,h) in [(2048usize,1365usize),(6000,4000)]{
   let scene=crate::Linear{ mono: false, haze: None, toneq: None,width:w,height:h,data:(0..w*h).flat_map(|i|[0.05+0.6*(i%w) as f32/w as f32;3]).collect(),detail:None};
   let geo=crate::look::Geo{full_w:6000,full_h:4000,off_x:0,off_y:0,scale:6000.0/w as f32,camera:0};
   for family in ["","soft","mist","bloom"]{let fs=FilmSettings{diffusion:family.into(),..f.clone()};let _=super::super::develop(&scene,&fs,geo).unwrap();let start=std::time::Instant::now();let im=super::super::develop(&scene,&fs,geo).unwrap();eprintln!("FILM-PERF {} {}x{} diffusion={} ms={:.3} sample={}",super::super::backend_name(),w,h,family,start.elapsed().as_secs_f64()*1000.,im.data[im.data.len()/2]);}
  }
 }
}
