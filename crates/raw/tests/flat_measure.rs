//! Measurement workflow with a flat-field pair from any source (`measure_run::measure_with`): synthetic Foveon
//! shots (vignetting, colour cast per layer, lighting gradient mirrored in the second shot, no lens data in the
//! file), hints of the user (name, aperture, pair), saved as own profile with colour-cast map.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use raw::measure_run::{self as run, MeasureHints, Role, ShotImage, ShotSource};

const W: usize = 1500;
const H: usize = 1000;

struct Synthetic;

impl ShotSource for Synthetic {
    fn open(&self, path: &Path) -> Result<ShotImage, String> {
        let tilt = if path.to_string_lossy().contains("B") { -0.06 } else { 0.06 };
        let rc = (W as f64).hypot(H as f64) / 2.0 / W as f64;
        let mut px = vec![[0f32; 3]; W * H];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ (tilt > 0.0) as u64;
        for y in 0..H {
            for x in 0..W {
                let u = (x as f64 + 0.5) / W as f64 * 2.0 - 1.0;
                let r = (x as f64 + 0.5 - W as f64 / 2.0).hypot(y as f64 + 0.5 - H as f64 / 2.0) / W as f64;
                let s = (r / rc).powi(2);
                let base = 0.45 * (1.0 - 0.4 * s) * (1.0 + tilt * u);
                let cast = [1.0 + 0.05 * s, 1.0, 1.0 - 0.04 * s];
                px[y * W + x] = std::array::from_fn(|k| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let noise = ((seed >> 40) as f64 / (1u64 << 24) as f64 - 0.5) * 0.01;
                    (base * cast[k] * (1.0 + noise)) as f32
                });
            }
        }
        Ok(ShotImage { maker: "SIGMA".into(), model: "SIGMA sd Quattro".into(), lens: None, focal_length: None, aperture: None, width: W, height: H, px, saturation: 1.0, sensor: lensprof::own::SensorKind::Foveon })
    }
}

#[test]
fn flat_pair_with_hints_gives_cast_and_saves_it() {
    let tmp = std::env::temp_dir().join(format!("raw-flat-measure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::env::remove_var("GRAINEO_LENS_BACKUP");
    std::env::set_var("GRAINEO_DATA_DIR", &tmp);
    let (a, b) = (PathBuf::from("/synthetisch/A.X3F"), PathBuf::from("/synthetisch/B.X3F"));
    let mut hints = MeasureHints { lens_name: Some("Helios 44-2 58mm".into()), focal_length: Some(58.0), pairs: vec![(a.clone(), b.clone())], ..Default::default() };
    hints.apertures.insert(a.clone(), 4.0);
    hints.apertures.insert(b.clone(), 4.0);
    hints.roles.insert(a.clone(), Role::FlatField);
    hints.roles.insert(b.clone(), Role::FlatField);
    let e = run::measure_with(&[a, b], &Synthetic, &hints, &|_, _, _| {}, &AtomicBool::new(false)).unwrap();
    assert!(e.shots.iter().all(|s| s.role == Role::FlatField), "{:?}", e.shots);
    assert!(e.shots[1].rotated && !e.shots[0].rotated);
    assert_eq!(e.cast.len(), 1);
    let g = &e.cast[0];
    assert_eq!((g.focal_length, g.aperture, g.images.len()), (Some(58.0), Some(4.0), 2));
    let c = g.cast.as_ref().unwrap();
    eprintln!("corner {:?}, quality {:?}", c.quality.corner, c.quality);
    assert!(c.quality.pair_rms.is_some() && c.quality.score >= 80, "{:?}", c.quality);
    assert!((c.quality.corner[0] - 1.05).abs() < 0.01 && (c.quality.corner[2] - 0.96).abs() < 0.01, "{:?}", c.quality.corner);
    let q = e.quality.as_ref().unwrap();
    assert!(q.parts.iter().any(|p| p.0 == lensprof::own::QualityPart::Cast));
    // saved: a manual lens gets its own new profile with the map and the shots
    let path = run::save(&e).unwrap();
    assert!(path.starts_with(&tmp));
    let p = lensprof::store::list().into_iter().next().unwrap();
    assert_eq!((p.name.as_str(), p.lens.as_str(), p.sensor), ("Helios 44-2 58mm", "", lensprof::own::SensorKind::Foveon));
    assert_eq!(p.cast.len(), 1);
    assert!((p.cast[0].av - 4.0).abs() < 1e-9 && p.cast[0].grid.is_valid());
    assert_eq!(p.history[0].shots.iter().filter(|s| s.rotated).count(), 1);
    let _ = std::fs::remove_dir_all(&tmp);
}
