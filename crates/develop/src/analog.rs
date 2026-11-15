//! Sigma-Analog: black-and-white film exposed directly from the three Foveon layers, and grain instead of
//! sensor noise (Merrill: DP1/DP2/DP3 Merrill, SD1/SD1 Merrill).
//!
//! **Exposure from the layers.** A black-and-white film has one spectral sensitivity `g(λ)` (taking filter
//! included). Its exposure for a surface is predicted from the three white-balanced, normalised layer values
//! `L = (bottom, middle, top)` as `E ≈ w·L`. The weights `w` minimise, over a training set of natural
//! reflectance spectra under daylight (D55), the expected squared relative error of a pixel:
//! the spectral error of the prediction plus, weighted by [`NOISE_WEIGHT`], the sensor noise the weights pass on
//! (`wᵀ C w`, `C` = measured noise covariance of the layers at middle grey for the photo's ISO), with `w·1 = 1`
//! so that neutral grey stays neutral. Closed form, computed per film/filter in microseconds from a few sums
//! ([`data`]: measured layer sensitivities of the SD1 Merrill (NPL), training sums, noise table).
//!
//! The film then receives `(E, E, E)` as its linear input: for a black-and-white film the exposure of a neutral
//! input is proportional to its value, so this is the same as setting the film exposure directly, while the
//! rest of the film chain (curve, grain, halation, print) stays unchanged. The taking filter is part of `g`;
//! the film profile's own filter has no effect on a neutral input (its exposure is normalised to daylight).
//!
//! **Grain instead of noise.** The sensor noise that reaches the film exposure (relative, at middle grey) is
//! handed to the film stage ([`crate::film::FilmSettings::sensor_noise`]); there the grain is reduced so that
//! noise and grain together are as grainy as the film alone, see `film::grain_balanced`. With colour film the colour noise
//! is first smoothed much more strongly (the film's dye clouds take its place); the luminance noise remains and is
//! balanced like with black-and-white film. The colour exposure itself keeps the usual path (matrix → spectrum).

mod data;

use std::sync::Mutex;

use crate::calib::Model;
use crate::layers::LayerMatrix;
use crate::matrix::Mat3;
use crate::{Calibration, Settings};

/// Spectral data of the layers exist for this camera (Merrill generation, measured on the SD1 Merrill).
pub fn camera_supported(cal: &Calibration) -> bool {
    // (non-Sigma RAWs carry a placeholder calibration with `Model::Dp`: check the camera name as well)
    matches!(cal.model, Model::Dp | Model::Sd1) && (cal.camera.contains("Merrill") || cal.camera.starts_with("SIGMA SD1"))
}

/// Why exposing from the layers is not available for this photo and film (`None` = available).
/// German text, used as translation key.
pub fn unavailable_reason(cal: Option<&Calibration>, s: &Settings) -> Option<&'static str> {
    match cal {
        Some(cal) if camera_supported(cal) => {}
        _ => return Some("Nur für Sigma-Merrill-Fotos: Für andere Kameras fehlen gemessene Empfindlichkeiten der Sensorschichten."),
    }
    if !crate::film::stock(&s.film.stock).is_some_and(|f| f.monochrome) {
        return Some("Nur bei Schwarzweißfilm.");
    }
    if s.monochrome {
        return Some("Nicht im Schwarzweiß-Modus der Kamera.");
    }
    None
}

/// Why the grain balance is not available for this photo (`None` = available): Merrill only (noise table), any film.
/// German text, used as translation key.
pub fn grain_balance_reason(cal: Option<&Calibration>, s: &Settings) -> Option<&'static str> {
    match cal {
        Some(cal) if camera_supported(cal) => {}
        _ => return Some("Nur für Sigma-Merrill-Fotos: Für andere Kameras fehlt die Rauschmessung."),
    }
    if s.monochrome {
        return Some("Nicht im Schwarzweiß-Modus der Kamera.");
    }
    None
}

/// Is the film exposed from the layers for these settings?
pub fn applies(cal: &Calibration, s: &Settings) -> bool {
    s.film.is_active() && s.film.layer_exposure && s.view == crate::View::Developed && unavailable_reason(Some(cal), s).is_none()
}

/// Layer weights and the sensor noise they pass on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weights {
    /// Weights for [bottom, middle, top] (sum 1)
    pub w: [f32; 3],
    /// Relative noise (standard deviation / signal) of the film exposure at middle grey, full resolution
    pub noise: f32,
}

/// Weights for these settings (`None` if [`applies`] is false).
pub fn weights(cal: &Calibration, s: &Settings) -> Option<Weights> {
    if !applies(cal, s) {
        return None;
    }
    weights_for(&s.film.stock, &crate::film::effective_filter(&s.film), cal.iso, s.exposure, NOISE_WEIGHT)
}

/// Weight of the noise in the fit. Measured on DP2 Merrill photos (ISO 100 … 3200): after Sigma's processing the noise
/// of the three normalised layers is almost fully correlated (r = 0.999 at ISO 100, 0.92 … 0.98 at ISO 1600/3200), so
/// every weighting with sum 1 passes on nearly the same noise. A noise penalty (1 = expected pixel error) lowers it by
/// at most 10–15 % at ISO 3200 and does so with larger, more exotic weights that triple the tonal error; 0 keeps the
/// weights true to the film at every ISO. The noise is handled by the grain balance instead.
pub const NOISE_WEIGHT: f64 = 0.0;

/// Weights of a film (`stock`) with taking filter `(id, strength)` at `iso`; `ev` = exposure slider (EV),
/// which places middle grey on a lower (+) or higher (−) sensor signal; `beta` = weight of the noise
/// (1 = expected error of a pixel, 0 = only the spectral fit).
pub fn weights_for(stock: &str, filter: &(String, f32), iso: f64, ev: f64, beta: f64) -> Option<Weights> {
    static CACHE: Mutex<Vec<(String, Weights)>> = Mutex::new(Vec::new());
    let key = format!("{stock}|{}|{:.3}|{iso}|{ev:.3}|{beta}", filter.0, filter.1);
    if let Some((_, w)) = CACHE.lock().unwrap().iter().find(|(k, _)| *k == key) {
        return Some(*w);
    }
    let g = film_sensitivity(stock, filter)?;
    let c = noise_cov(iso, ev);
    let w = solve(&g, &c, beta)?;
    let noise = quad(&w, &c).max(0.0).sqrt() as f32;
    let out = Weights { w: w.map(|v| v as f32), noise };
    let mut cache = CACHE.lock().unwrap();
    if cache.len() >= 64 {
        cache.remove(0);
    }
    cache.push((key, out));
    Some(out)
}

/// Film sensitivity (linear, filter included) on the grid 400 … 680 nm of [`data`].
fn film_sensitivity(stock: &str, filter: &(String, f32)) -> Option<[f64; data::N]> {
    let p = crate::film::profile_with_filter(stock, &filter.0, filter.1).ok()?;
    if !p.is_bw() {
        return None;
    }
    let wl = &p.data.wavelengths;
    let lin: Vec<f64> = p.data.log_sensitivity.iter().map(|r| r.first().copied().filter(|v| v.is_finite()).map_or(0.0, |v| 10f64.powf(v))).collect();
    let mut g = [0.0; data::N];
    for (i, gi) in g.iter_mut().enumerate() {
        *gi = interp(wl, &lin, 400.0 + 10.0 * i as f64);
    }
    (g.iter().sum::<f64>() > 0.0).then_some(g)
}

fn interp(xs: &[f64], ys: &[f64], x: f64) -> f64 {
    if xs.is_empty() || x < xs[0] || x > xs[xs.len() - 1] {
        return 0.0;
    }
    let j = xs.partition_point(|&v| v < x);
    if j == 0 {
        return ys[0];
    }
    let (x0, x1) = (xs[j - 1], xs[j]);
    let t = if x1 > x0 { (x - x0) / (x1 - x0) } else { 0.0 };
    ys[j - 1] + t * (ys[j] - ys[j - 1])
}

/// Weights minimising spectral error + `beta` · noise variance with `w·1 = 1` (closed form, Lagrange).
fn solve(g: &[f64; data::N], c: &[[f64; 3]; 3], beta: f64) -> Option<[f64; 3]> {
    let white: f64 = (0..data::N).map(|i| data::D55[i] * g[i]).sum();
    if white <= 0.0 {
        return None;
    }
    let cvec: [f64; 3] = std::array::from_fn(|k| (0..data::N).map(|i| data::K[k][i] * g[i]).sum::<f64>() / white / data::N_OMEGA);
    let h = Mat3(std::array::from_fn(|i| data::A[i / 3][i % 3] / data::N_OMEGA + beta * c[i / 3][i % 3]));
    let hi = h.inverse()?;
    let hc = hi.apply(cvec);
    let h1 = hi.apply([1.0; 3]);
    let nu = (1.0 - hc.iter().sum::<f64>()) / h1.iter().sum::<f64>();
    let w: [f64; 3] = std::array::from_fn(|k| hc[k] + nu * h1[k]);
    w.iter().all(|v| v.is_finite()).then_some(w)
}

fn quad(w: &[f64; 3], c: &[[f64; 3]; 3]) -> f64 {
    (0..3).map(|i| (0..3).map(|j| w[i] * c[i][j] * w[j]).sum::<f64>()).sum()
}

/// Relative noise covariance of the normalised layers at middle grey for `iso` (measured table, interpolated
/// over log ISO in proportion to ISO, scaled in proportion beyond its ends) and exposure slider `ev`
/// (shot noise: variance × 2^ev).
pub fn noise_cov(iso: f64, ev: f64) -> [[f64; 3]; 3] {
    let t = &data::NOISE;
    let iso = iso.max(1.0);
    let scaled = |c: &[[f64; 3]; 3], f: f64| -> [[f64; 3]; 3] { c.map(|r| r.map(|v| v * f)) };
    let j = t.partition_point(|(i, _)| *i < iso);
    let c = if j == 0 {
        scaled(&t[0].1, iso / t[0].0)
    } else if j == t.len() {
        scaled(&t[j - 1].1, iso / t[j - 1].0)
    } else {
        let (a, b) = (&t[j - 1], &t[j]);
        let f = (iso.ln() - a.0.ln()) / (b.0.ln() - a.0.ln());
        let (pa, pb) = (scaled(&a.1, iso / a.0), scaled(&b.1, iso / b.0));
        std::array::from_fn(|r| std::array::from_fn(|k| pa[r][k] + f * (pb[r][k] - pa[r][k])))
    };
    scaled(&c, 2f64.powf(ev.clamp(-4.0, 4.0)))
}

/// Layer matrix of the film input (`m[output][layer]`, all rows equal): the film sees `E = w·L`.
pub fn layer_matrix(cal: &Calibration, s: &Settings) -> Option<LayerMatrix> {
    weights(cal, s).map(|w| [w.w; 3])
}

/// Matrix from white-balanced camera values to the film input: as [`crate::layers::cam_to_rgb`], but without
/// temperature/tint (they would tint the neutral input of a black-and-white film).
pub fn cam_to_input(cal: &Calibration, s: &Settings, l: &LayerMatrix) -> Mat3 {
    let m = cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref());
    let w0 = m.inverse().map(|inv| inv.apply([1.0; 3])).filter(|w| w.iter().all(|v| v.is_finite() && *v > 1e-9)).unwrap_or([1.0; 3]);
    let lm = Mat3(std::array::from_fn(|i| l[i / 3][i % 3] as f64));
    lm.mul(&Mat3::diag(w0.map(|v| 1.0 / v)))
}

/// Relative sensor noise of the luminance on today's path (Sigma's colour matrix, then the film), for comparison
/// and for the grain balance without layer exposure: luminance row of the normalised matrix.
pub fn today_noise(cal: &Calibration, s: &Settings) -> Option<f32> {
    if !camera_supported(cal) {
        return None;
    }
    let m = cal.cam_to_srgb(s.color_mode.as_deref(), s.white_balance.as_deref());
    let w0 = m.inverse()?.apply([1.0; 3]);
    let mn = m.mul(&Mat3::diag(w0));
    let y = [0.2126, 0.7152, 0.0722];
    let row: [f64; 3] = std::array::from_fn(|k| (0..3).map(|r| y[r] * mn.0[r * 3 + k]).sum());
    Some(quad(&row, &noise_cov(cal.iso, s.exposure)).max(0.0).sqrt() as f32)
}

/// Film settings for the film stage of this photo: with the grain balance on (Merrill, black-and-white and colour film), the
/// sensor noise that reaches the film (relative, middle grey) and the long edge of the full image are attached
/// ([`crate::film::FilmSettings::sensor_noise`]).
pub fn film_settings<'a>(cal: &Calibration, s: &'a Settings) -> std::borrow::Cow<'a, crate::film::FilmSettings> {
    let f = &s.film;
    if !f.is_active() || !f.grain_balance || f.grain <= 0.0 || grain_balance_reason(Some(cal), s).is_some() {
        return std::borrow::Cow::Borrowed(f);
    }
    let noise = match weights(cal, s) {
        Some(w) => Some(w.noise),
        None => today_noise(cal, s),
    };
    match noise.filter(|n| *n > 0.0) {
        Some(n) => {
            let [w, h] = cal.full_size();
            std::borrow::Cow::Owned(crate::film::FilmSettings { sensor_noise: Some((n, w.max(h))), ..f.clone() })
        }
        None => std::borrow::Cow::Borrowed(f),
    }
}

/// Film thumbnails ([`crate::film::thumbnails`]) computed as the main image would be with each film: its own layer
/// exposure and grain balance ([`film_settings`]). `scene` renders the small scene-linear image for given settings; it is
/// called once per distinct layer weighting (films without layer exposure share one scene).
pub fn thumbnails(cal: &Calibration, s: &Settings, ids: &[&str], scene: impl Fn(&Settings) -> (crate::Linear, crate::look::Geo)) -> Vec<crate::film::Thumb> {
    let mut scenes: Vec<(Option<[u32; 3]>, (crate::Linear, crate::look::Geo))> = Vec::new();
    crate::film::films()
        .iter()
        .filter(|f| ids.contains(&f.id.as_str()))
        .map(|f| {
            let mut t = s.clone();
            t.film = crate::film::FilmSettings { active: true, stock: f.id.clone(), paper: None, ..s.film.clone() };
            let key = weights(cal, &t).map(|w| w.w.map(f32::to_bits));
            let i = match scenes.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    scenes.push((key, scene(&t)));
                    scenes.len() - 1
                }
            };
            let (lin, geo) = &scenes[i].1;
            crate::film::Thumb { stock: f, image: crate::film::develop(lin, &film_settings(cal, &t), *geo) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bw(stock: &str, iso: f64, beta: f64) -> Weights {
        weights_for(stock, &(String::new(), 1.0), iso, 0.0, beta).expect("weights")
    }

    /// Weights keep neutral grey (sum 1); the noise penalty lowers the noise, more so at high ISO.
    #[test]
    fn weights_sum_and_noise_penalty() {
        if crate::film::data_dir().is_none() {
            return;
        }
        for stock in ["kodak_trix_400", "ilford_hp5_plus", "kodak_tmax_100", "ilford_ortho_plus", "ilford_delta_100"] {
            for iso in [100.0, 3200.0] {
                let w = bw(stock, iso, 1.0);
                let free = bw(stock, iso, 0.0);
                assert!((w.w.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{stock}: {:?}", w.w);
                assert!(w.noise <= free.noise + 1e-6, "{stock} ISO {iso}: {} > {}", w.noise, free.noise);
            }
            let (lo, hi) = (bw(stock, 100.0, 1.0), bw(stock, 3200.0, 1.0));
            let free = bw(stock, 3200.0, 0.0);
            assert!(hi.noise / free.noise <= lo.noise / bw(stock, 100.0, 0.0).noise + 1e-4, "{stock}: penalty acts more at high ISO");
        }
        // orthochromatic film: blind to red, hardly any weight on the bottom layer
        let o = bw("ilford_ortho_plus", 100.0, 1.0);
        assert!(o.w[0] < 0.15, "ortho {:?}", o.w);
    }

    /// A red filter shifts the weights towards the bottom (red) layer.
    #[test]
    fn red_filter_moves_to_bottom_layer() {
        if crate::film::data_dir().is_none() {
            return;
        }
        let plain = bw("kodak_trix_400", 100.0, 1.0);
        let red = weights_for("kodak_trix_400", &("rot25".into(), 1.0), 100.0, 0.0, 1.0).unwrap();
        assert!(red.w[0] > plain.w[0] + 0.3, "{:?} vs {:?}", red.w, plain.w);
        assert!(red.w[2] < plain.w[2]);
    }

    /// Noise table: grows with ISO and with a brightened exposure.
    #[test]
    fn noise_grows_with_iso() {
        let tr = |c: [[f64; 3]; 3]| c[0][0] + c[1][1] + c[2][2];
        assert!(tr(noise_cov(1600.0, 0.0)) > tr(noise_cov(400.0, 0.0)));
        assert!(tr(noise_cov(400.0, 0.0)) > tr(noise_cov(100.0, 0.0)));
        assert!(tr(noise_cov(400.0, 1.0)) > tr(noise_cov(400.0, 0.0)));
        assert!(tr(noise_cov(6400.0, 0.0)) > tr(noise_cov(3200.0, 0.0)));
    }

    /// Film thumbnails: each black-and-white film gets a scene with its own layer weights, colour films share one
    /// without; every thumbnail is computed with the film settings of the main image (grain balance attached).
    #[test]
    fn thumbnails_per_film_scene_and_grain_balance() {
        let Some(path) = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(|d| std::path::PathBuf::from(d).join("DP2M5418.X3F")).filter(|p| p.is_file()) else { return };
        if crate::film::stock("kodak_trix_400").is_none() || crate::film::stock("ilford_hp5_plus").is_none() || crate::film::stock("kodak_portra_400").is_none() {
            return;
        }
        let cal = crate::Session::open(&path).unwrap().cal.clone();
        let mut s = Settings::default();
        s.film.active = true;
        s.film.stock = "kodak_portra_400".into();
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let ids = ["kodak_trix_400", "kodak_portra_400", "ilford_hp5_plus"];
        let thumbs = thumbnails(&cal, &s, &ids, |t| {
            calls.borrow_mut().push(t.film.stock.clone());
            let lin = crate::Linear { mono: false, haze: None, toneq: None, width: 16, height: 16, data: vec![0.184; 16 * 16 * 3], detail: None };
            (lin, crate::look::Geo { full_w: 16 * 300, full_h: 16 * 200, off_x: 0, off_y: 0, scale: 300.0, camera: 0 })
        });
        assert_eq!(thumbs.len(), 3);
        assert!(thumbs.iter().all(|t| t.image.is_ok()));
        let calls = calls.into_inner();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls.contains(&"kodak_trix_400".to_string()) && calls.contains(&"ilford_hp5_plus".to_string()));
        let mut t = s.clone();
        t.film.stock = "kodak_trix_400".into();
        assert!(film_settings(&cal, &t).sensor_noise.is_some());
    }
}

