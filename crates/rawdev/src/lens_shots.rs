//! Shots for the lens measurement from every format graineo develops: X3F through the own Sigma chain
//! (`develop`), everything else through rawler (`raw::measure_run::RawSource`).
//!
//! X3F: the three layers in linear form after black level, spatial gain and colour shading of the camera
//! (`develop::linearize`, the same start as the own chain), top/middle/bottom in the places of R/G/B, sensor
//! position, normalised to the saturation. Camera from the PROP `CAMMODEL`, focal length `FLENGTH`, aperture
//! `APERTURE` (both missing or 0 with a manual lens), lens name as in the library for the fixed lenses of the
//! DP models, otherwise none.

use std::path::Path;

use raw::measure_run::{RawSource, ShotImage, ShotSource};

/// `.x3f` → X3F, otherwise [`RawSource`].
pub struct AnySource;

fn is_x3f(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("x3f"))
}

impl ShotSource for AnySource {
    fn open(&self, path: &Path) -> Result<ShotImage, String> {
        if is_x3f(path) {
            x3f_shot(path)
        } else {
            RawSource.open(path)
        }
    }
}

fn positive(v: Option<&str>) -> Option<f32> {
    v.and_then(|t| t.trim().parse::<f32>().ok()).filter(|f| f.is_finite() && *f > 0.0)
}

/// X3F as measurement shot (see the module description).
pub fn x3f_shot(path: &Path) -> Result<ShotImage, String> {
    let s = develop::Session::open(path).map_err(|e| format!("nicht lesbar: {e}"))?;
    let lin = develop::linearize(s.raw(), &s.cal);
    let px: Vec<[f32; 3]> = lin.data.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    let props = &s.props;
    let model = props.get("CAMMODEL").map(str::trim).filter(|m| !m.is_empty()).unwrap_or("SIGMA").to_string();
    let maker = props.get("CAMMANUF").map(str::trim).filter(|m| !m.is_empty()).unwrap_or("SIGMA").to_string();
    // clipping level after removing the black level (the data are divided by the saturation)
    let black = s.cal.black.iter().fold(0f64, |a, b| a.max(*b));
    let saturation = if s.cal.saturation > 0.0 { (1.0 - black / s.cal.saturation).clamp(0.5, 1.0) as f32 } else { 1.0 };
    Ok(ShotImage {
        maker,
        lens: library::exif::fixed_lens(&model).map(str::to_string),
        model,
        focal_length: positive(props.get("FLENGTH")),
        aperture: positive(props.get("APERTURE")),
        width: lin.width,
        height: lin.height,
        px,
        saturation,
        sensor: lensprof::own::SensorKind::Foveon,
    })
}

/// PROP entries of an X3F that matter for the measurement (camera, lens, exposure), for reports.
pub fn x3f_props(path: &Path) -> Result<Vec<(String, String)>, String> {
    let s = develop::Session::open(path).map_err(|e| format!("nicht lesbar: {e}"))?;
    const KEYS: &[&str] = &["CAMMANUF", "CAMMODEL", "LENSMODEL", "LENSFRANGE", "LENSARANGE", "FLENGTH", "FLEQ35MM", "APERTURE", "SHUTTER", "ISO"];
    Ok(KEYS.iter().map(|k| (k.to_string(), s.props.get(k).unwrap_or("–").to_string())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only with the own sd Quattro file (manual lens), otherwise skipped silently.
    #[test]
    fn x3f_source_reads_layers() {
        let candidates = [
            std::env::var("GRAINEO_TEST_SDQ").unwrap_or_default(),
            "/private/tmp/aobj/fotos/archiv_sdQ_SDQ_0700.X3F".into(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../arbeitsordner/spp-farbtest-2/x3f/archiv_sdQ_SDQ_0700.X3F").into(),
        ];
        let Some(p) = candidates.iter().map(Path::new).find(|p| p.is_file()) else {
            eprintln!("SDQ_0700 not present – skipped");
            return;
        };
        let s = AnySource.open(p).expect("X3F lesbar");
        eprintln!("{} {} · {:?} · {:?} mm f/{:?} · {}×{} · saturation {}", s.maker, s.model, s.lens, s.focal_length, s.aperture, s.width, s.height, s.saturation);
        assert!(s.width > 5000, "{}", s.width);
        assert_eq!(s.px.len(), s.width * s.height);
        assert_eq!(s.maker, "SIGMA");
        assert_eq!(s.sensor, lensprof::own::SensorKind::Foveon);
        let mean: [f64; 3] = std::array::from_fn(|c| s.px.iter().step_by(97).map(|p| p[c] as f64).sum::<f64>() / (s.px.len() / 97) as f64);
        assert!(mean.iter().all(|m| *m > 0.0), "{mean:?}");
    }
}
