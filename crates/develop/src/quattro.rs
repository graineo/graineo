//! Quattro cameras (dp0/dp1/dp2/dp3 Quattro, sd Quattro, sd Quattro H) and the older
//! generation ("Classic": SD9/SD10/SD14/SD15, DP1/DP2 incl. s/x), both via the
//! preprocessing of x3fuse-core, with our own calibration ([`Calibration::read_quattro`] or
//! [`Calibration::read_classic`]).
//!
//! Architecture:
//! 1. x3fuse-core (Apache-2.0, third_party/x3fuse-core) decodes and preprocesses the file:
//!    TRUE decoding, CAMF type 5, black levels (top layer separately), defective pixels,
//!    AF pixel grid (sd Quattro/H), noise reduction and upsampling of the two lower
//!    layers to the resolution of the top one (Kalpanika method: top layer as
//!    luminance guide). Result via the public editor interface
//!    `prepare_scene_linear`: linear sRGB plus the camera matrix `camera_to_rgb`.
//! 2. We convert back to the normalised camera space with `camera_to_rgb⁻¹` (x = 0 black,
//!    1 = sensor saturation per layer) and store the image as a synthetic raw image
//!    (u16, `ENC_BLACK + x · ENC_SCALE`). Session, preview, sensor views,
//!    measuring spot and layer export thus work unchanged.
//! 3. The calibration (white balance presets, colour modes, ISO) comes from our own
//!    CAMF parsing, for dp Quattro matched against x3fuse-core's port of the SPP colour modes
//!    (`ColorModeCalibration`): counts = ISO · CC(WB) · CMCM(mode) · diag(WB · n2c) · x.
//!    Our output stage (`render_counts`) runs on that with the Quattro tone curve.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;
use x3f::{PropList, RawImage, X3fFile};

use crate::{Calibration, Linear};

/// Black level of the synthetic raw image (room for slightly negative values after
/// noise reduction).
pub const ENC_BLACK: f64 = 2048.0;
/// Code values per saturation unit (≈ 14-bit resolution of the sensor; headroom up to x ≈ 3.9).
pub const ENC_SCALE: f64 = 16384.0;

/// Preprocessed Quattro file: synthetic raw image, calibration, properties.
pub struct Prepared {
    pub raw: Arc<RawImage>,
    pub cal: Arc<Calibration>,
    pub props: PropList,
    /// Classic: result of Sigma's whole chain ([`crate::classic_new`]) as a linear image in the
    /// normalisation of [`crate::linearize`]; `None` where the chain is not available
    pub classic: Option<Arc<Linear>>,
    /// First noise reduction stage of the colour grid with binning possible (sd Quattro / sd Quattro H from capture
    /// ISO 800): the layers without the noise reduction of the colour grid, for the noise reduction after binning
    /// ([`counts`])
    pub unbinned_noise: Option<Arc<Stage1Source>>,
    /// First noise reduction stage: the denoised colour grid (colour stage on the grid, see [`grid_counts`])
    pub grid: Option<Arc<GridColour>>,
}

/// Layers before the first noise reduction stage of the colour grid and its parameters (binned path).
pub struct Stage1Source {
    pub raw: RawImage,
    pub params: crate::quattro_nr::Params,
    /// normalised layer value → units of the stage (counts above black / (SensorISO/100))
    pub unit: [f32; 3],
}

/// Quattro file? (X3F header version 4.0 or later; reads only the first bytes)
pub fn is_quattro(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && &head[0..4] == b"FOVb"
        && u32::from_le_bytes([head[4], head[5], head[6], head[7]]) >= 0x4_0000
}

/// Classic file (RAW type 3: TRUE format 30 or Huffman 5/6; reads only header and directory)?
pub fn is_classic(path: &Path) -> bool {
    !is_quattro(path) && x3f::read_raw_header(path).is_some_and(|h| h.is_classic_raw())
}

/// Is the file preprocessed via x3fuse-core (Quattro or Classic)?
pub fn is_prepared(path: &Path) -> bool {
    is_quattro(path) || is_classic(path)
}

/// [`is_classic`], remembered per path (asked on the UI thread when a photo opens). Only files that could be read
/// are remembered.
pub fn is_classic_cached(path: &Path) -> bool {
    static KNOWN: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, c)) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, _)| q == path) {
        return c;
    }
    let c = is_classic(path);
    if path.is_file() {
        let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        if k.len() >= 256 {
            k.drain(..128);
        }
        k.push((path.to_path_buf(), c));
    }
    c
}

/// [`is_prepared`], remembered per path (the base key asks for it often, also on the UI thread). Only files that
/// could be read are remembered (an unreachable file is asked again later).
pub fn is_prepared_cached(path: &Path) -> bool {
    static KNOWN: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, p)) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, _)| q == path) {
        return p;
    }
    let p = is_prepared(path);
    if path.is_file() {
        let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
        if k.len() >= 256 {
            k.drain(..128);
        }
        k.push((path.to_path_buf(), p));
    }
    p
}

type Done = Result<Arc<Prepared>, String>;

/// One cached preprocessing: computed once and shared – opening and "SPP base" run in parallel in the app.
type Slot = pools::Shared<Done>;

/// Most recently preprocessed files (opening and "SPP base" run in parallel in the app and
/// should run the preprocessing only once), full or quick ([`prepared_quick`]).
fn cache() -> &'static Mutex<Vec<((PathBuf, bool), Arc<Slot>)>> {
    static CACHE: OnceLock<Mutex<Vec<((PathBuf, bool), Arc<Slot>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Is the preprocessing of a file running right now (full or quick)? For measurements.
pub fn is_preparing(path: &Path) -> bool {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    cache().lock().unwrap().iter().any(|((p, ..), slot)| *p == file && slot.is_running())
}

/// Drop the cached preprocessing of a file (measuring tools: time the preprocessing again).
pub fn forget(path: &Path) {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    cache().lock().unwrap().retain(|((p, _), _)| *p != file);
    scene_cache().lock().unwrap().retain(|(p, _)| *p != file);
}

/// Preprocessed file (≈ 3–6 s on the first call, afterwards from the cache): highlights without reconstruction, they
/// glide to neutral ([`own_highlights`]); the first noise reduction stage on the colour grid ([`crate::quattro_nr`],
/// [`crate::quattro_noise`]).
///
/// Computed once per file and shared ([`pools::Shared`]): while another thread computes the same file, a plain
/// thread or a worker of another pool waits for its result; a worker of the very pool the computation runs in never
/// waits (rule of the app's threads, see `pools`) and computes the file itself. Opening never waits behind the
/// background: if an offline copy (background pool, intent idle) computes the file, a caller in front waits at most
/// [`pools::BACKGROUND_GRACE`] and then computes it itself (the first result to finish is kept). The preprocessing
/// works in parallel in the caller's pool; no lock is held while computing.
pub fn prepared(path: &Path) -> Result<Arc<Prepared>, String> {
    prepared_full(path, false)
}

/// Quick preparation for the first picture: like [`prepared`], but without the first noise reduction stage
/// (the layers as x3fuse-core prepares them for the stage, i.e. without its colour noise reduction where the stage
/// applies). x3fuse-core runs only once for both: the full preparation takes over its result
/// ([`shared_scene`]). Where the stage does not apply, both are the same cache entry.
pub fn prepared_quick(path: &Path) -> Result<Arc<Prepared>, String> {
    prepared_full(path, true)
}

fn prepared_full(path: &Path, quick: bool) -> Result<Arc<Prepared>, String> {
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // where the first stage does not apply (capture ISO, Classic), the quick and the full preparation are the same:
    // one cache entry for both (the quick opening and the Sigma base then share it)
    let quick = quick && stage1_applies(&file);
    let key = (file.clone(), quick);
    let slot = {
        let mut c = cache().lock().unwrap();
        if let Some(i) = c.iter().position(|(p, _)| *p == key) {
            let e = c.remove(i);
            let slot = e.1.clone();
            c.push(e);
            slot
        } else {
            let slot = Arc::new(Slot::default());
            c.push((key.clone(), slot.clone()));
            // Keep at most three images (current, preloaded, previous).
            while c.len() > 3 {
                c.remove(0);
            }
            slot
        }
    };
    let r = slot.get_or_compute(|| prepare(&file, quick).map(Arc::new));
    if r.is_err() {
        cache().lock().unwrap().retain(|(p, _)| *p != key);
    }
    r
}

/// Does the first noise reduction stage of the colour grid run for this file (Quattro below the
/// capture ISO limit [`STAGE1_MAX_ISO`], or from [`STAGE1_RAW_MIN_ISO`] on with the grid from the stored sensor values)?
/// Reads only the camera data; remembered per path.
pub fn stage1_applies(path: &Path) -> bool {
    static KNOWN: Mutex<Vec<(PathBuf, bool)>> = Mutex::new(Vec::new());
    if let Some(&(_, a)) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(q, _)| q == path) {
        return a;
    }
    let applies = (|| {
        let file = X3fFile::open(path).ok()?;
        if file.is_classic() || !is_quattro(path) {
            return Some(false);
        }
        let camf = file.camf().ok()?;
        Some(stage1_enabled(&camf) || mid_luma_applies(&camf))
    })();
    let Some(a) = applies else { return true };
    let mut k = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if k.len() >= 256 {
        k.drain(..128);
    }
    k.push((path.to_path_buf(), a));
    a
}

/// The first stage for a Quattro file with these camera data: below capture ISO [`STAGE1_MAX_ISO`] on the grid of the
/// prepared layers, from [`STAGE1_RAW_MIN_ISO`] on on the grid from the stored sensor values; in between not (x3fuse-core's
/// preparation stays closer to Sigma's output there, measured). `GRAINEO_QUATTRO_STAGE1_HIGH=1`: from ISO 400 on as well
/// (measurements), `GRAINEO_QUATTRO_RAWGRID=0`: the stage stays below ISO 400 only.
fn stage1_enabled(camf: &x3f::Camf) -> bool {
    let capture_iso = crate::calib::quattro_iso_gain(camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0);
    let raw_grid = raw_grid_enabled();
    std::env::var("GRAINEO_QUATTRO_STAGE1").map_or(true, |v| v != "0")
        && (capture_iso < STAGE1_MAX_ISO
            || (raw_grid && capture_iso >= STAGE1_RAW_MIN_ISO)
            || std::env::var("GRAINEO_QUATTRO_STAGE1_HIGH").is_ok_and(|v| v == "1"))
}

type SceneDone = Result<Arc<x3f_core::SceneLinearImage>, String>;

/// x3fuse-core's result for the first noise reduction stage, shared by the quick and the full preparation of a file
/// (the most recent file only; the full preparation removes its entry once it has taken it over).
fn scene_cache() -> &'static Mutex<Vec<(PathBuf, Arc<pools::Shared<SceneDone>>)>> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Arc<pools::Shared<SceneDone>>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// x3fuse-core's preparation of `path`, computed once for the quick and the full preparation ([`prepared_quick`]);
/// `take`: remove the shared entry afterwards (the full preparation).
fn shared_scene(path: &Path, take: bool, compute: impl FnOnce() -> SceneDone) -> SceneDone {
    let key = path.to_path_buf();
    let slot = {
        let mut c = scene_cache().lock().unwrap();
        if let Some((_, s)) = c.iter().find(|(k, _)| *k == key) {
            s.clone()
        } else {
            let s = Arc::new(pools::Shared::default());
            c.push((key.clone(), s.clone()));
            while c.len() > 1 {
                c.remove(0);
            }
            s
        }
    };
    let r = slot.get_or_compute(compute);
    if take || r.is_err() {
        scene_cache().lock().unwrap().retain(|(k, s)| !(*k == key && Arc::ptr_eq(s, &slot)));
    }
    r
}

fn prepare(path: &Path, quick: bool) -> Result<Prepared, String> {
    let t = std::time::Instant::now();
    let file = X3fFile::open(path).map_err(|e| e.to_string())?;
    let camf = file.camf().map_err(|e| e.to_string())?;
    let sd_quattro_h = camf.matrix("CAMERAID").ok().and_then(|m| m.as_f64().first().copied()) == Some(41.0);
    // the first stage below capture ISO 400, and from ISO 6400 on with the grid from the stored sensor values (see
    // `stage1_enabled`); in between x3fuse-core's preparation stays closer to Sigma's output (measured, see FAHRPLAN)
    let stage1 = !file.is_classic() && stage1_enabled(&camf);
    // dp Quattro and sd Quattro (sd Quattro H with the first stage): the base sharpening is shared with the local
    // contrast of the colour stage below ISO 800 (`defer_sharpen`)
    let spp_neutralize = !file.is_classic() && (!sd_quattro_h || stage1) && std::env::var("GRAINEO_QUATTRO_HL").map_or(true, |v| v != "rec");
    let mut opts = x3f_core::SceneLinearOptions {
        // No reconstruction of blown layers here: highlights glide to neutral below (`own_highlights`)
        highlight_recovery: false,
        apply_sgain: std::env::var("GRAINEO_QUATTRO_SGAIN").ok().map(|v| v == "1"),
        denoise_intensity: std::env::var("GRAINEO_QUATTRO_NR").ok().and_then(|v| v.parse().ok()).unwrap_or(10),
        // setting "Grafikkarte verwenden" off: the noise reduction of the preparation on the processor
        denoise_backend: if pools::gpu_allowed() { x3f_core::DenoiseBackend::Auto } else { x3f_core::DenoiseBackend::Cpu },
        ..Default::default()
    };
    // All Quattro: noise strength with the digital factor (sd Quattro H as well since part 6: at ISO 6400 without
    // it the luminance noise was 2.5 × that of the reference; `GRAINEO_QUATTRO_HNR=0` = previous preprocessing).
    if !file.is_classic() && (!sd_quattro_h || std::env::var("GRAINEO_QUATTRO_HNR").map_or(true, |v| v != "0")) {
        opts.denoise_options = quattro_denoise_options(crate::calib::quattro_iso_gain(&camf));
    }
    // the colour noise of the grid is removed by the first stage below ; x3fuse-core
    // keeps only its luminance pass on the full-resolution top layer (from a digital factor > 1)
    if stage1 {
        opts.denoise_options.chroma = false;
        // without x3fuse-core's colour noise reduction the noise of dark regions reaches below the black of its
        // integer buffers at high ISO; a higher black keeps it from being clipped (which would brighten the darks)
        opts.denoise_options.min_intermediate_bias = std::env::var("GRAINEO_QUATTRO_BIAS").ok().and_then(|v| v.parse().ok()).unwrap_or(STAGE1_MIN_BIAS);
        if std::env::var("GRAINEO_QUATTRO_STAGE1_LUMA").is_ok_and(|v| v == "0") {
            opts.denoise_options.luminance = false;
        }
    }
    // brightness smoothing on the preparation (capture ISO 400 to below 6400): after the first picture, like the stage
    let mid_luma = !file.is_classic() && !stage1 && mid_luma_applies(&camf);
    let run = || x3f_core::prepare_scene_linear(path, &opts, &AtomicBool::new(false), |_| {}).map(Arc::new).map_err(|e| e.to_string());
    let scene = if stage1 || mid_luma { shared_scene(path, !quick, run)? } else { run()? };
    // the quick preparation shows the layers before the first stage
    let stage1 = stage1 && !quick;
    let t_scene = t.elapsed();
    let mut scene_data = scene.data.clone();
    let props = file.properties().map_err(|e| e.to_string())?;
    let iso = props.get("ISO").and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0);
    let amount = std::env::var("GRAINEO_QUATTRO_SHARPEN").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| capture_sharpen_amount(iso));
    // dp Quattro and sd Quattro below ISO 800: half the base sharpening here, the plain colour stage sharpens its
    // counts with the other half (`pending_sharpen`)
    let defer_sharpen = spp_neutralize && lce_applies(iso);
    let now = if defer_sharpen { amount * DEFERRED_SHARPEN } else { amount };
    if now > 0.0 {
        capture_sharpen(&mut scene_data, scene.width as usize, scene.height as usize, now as f32);
    }
    let cam_to_rgb = crate::matrix::Mat3(scene.white_balance.camera_to_rgb);
    let inv = cam_to_rgb.inverse().ok_or("Kameramatrix nicht invertierbar")?.to_f32();
    let (w, h) = (scene.width as usize, scene.height as usize);
    let n = w * h;
    // Back to the normalised camera space (planar like RawImage).
    let mut xs: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0f32; n]);
    {
        let [p0, p1, p2] = &mut xs;
        p0.par_chunks_mut(w)
            .zip(p1.par_chunks_mut(w))
            .zip(p2.par_chunks_mut(w))
            .zip(scene_data.par_chunks(w * 3))
            .for_each(|(((r0, r1), r2), src)| {
                for (x, s) in src.chunks_exact(3).enumerate() {
                    let m = &inv;
                    r0[x] = m[0] * s[0] + m[1] * s[1] + m[2] * s[2];
                    r1[x] = m[3] * s[0] + m[4] * s[1] + m[5] * s[2];
                    r2[x] = m[6] * s[0] + m[7] * s[1] + m[8] * s[2];
                }
            });
    }
    let encode = |xs: &[Vec<f32>; 3]| -> [Vec<u16>; 3] {
        let (b, k) = (ENC_BLACK as f32, ENC_SCALE as f32);
        std::array::from_fn(|c| xs[c].par_iter().map(|&v| (b + v * k).round().clamp(0.0, 65535.0) as u16).collect())
    };
    // capture ISO 400 to below 6400 (x3fuse-core's preparation without the first stage): the brightness noise of the
    // top layer smoothed, as a neutral change of all three layers
    if mid_luma && !quick {
        let s1 = stage1_params(&camf, &props, sd_quattro_h);
        let capture_iso = (crate::calib::quattro_iso_gain(&camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0)) as f32;
        let u = s1.unit[2];
        let mut t: Vec<f32> = xs[2].par_iter().map(|v| v * u).collect();
        crate::quattro_nr::luma_smooth(&mut t, w, h, capture_iso);
        let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [w, h]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
        let neutral: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32);
        let [p0, p1, p2] = &mut xs;
        p0.par_iter_mut().zip(p1.par_iter_mut()).zip(p2.par_iter_mut()).zip(t.par_iter()).for_each(|(((b, m), top), &ts)| {
            let d = ts / u - *top;
            *b += d * neutral[0];
            *m += d * neutral[1];
            *top += d * neutral[2];
        });
    }
    let mut unbinned_noise = None;
    let mut grid_colour = None;
    if stage1 {
        let t1 = std::time::Instant::now();
        let s1 = stage1_params(&camf, &props, sd_quattro_h);
        let capture_iso = (crate::calib::quattro_iso_gain(&camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0)) as f32;
        let params = crate::quattro_nr::Params { gains: s1.gains, capture_iso, binned: false, step: crate::quattro_nr::DEFAULT_STEP, white: s1.white };
        let (gw, gh, mut grid) = crate::quattro_nr::grid_from_layers([&xs[0], &xs[1], &xs[2]], w, h);
        let t_grid = t1.elapsed();
        for p in grid.iter_mut() {
            for c in 0..3 {
                p[c] *= s1.unit[c];
            }
        }
        // the grid of the prepared layers: reference for the defect repair and base of the top-layer detail
        let prepared_grid = grid.clone();
        // from capture ISO 400 on: the grid from the stored sensor values (matches Sigma's output)
        let raw = (capture_iso as f64 >= STAGE1_MAX_ISO && raw_grid_enabled())
            .then(|| crate::quattro_grid::read(path, s1.offset, s1.raw_unit))
            .and_then(|r| r.map_err(|e| eprintln!("Quattro grid from the sensor values: {e}")).ok())
            .filter(|r| r.gw == gw && r.gh == gh && r.tw == w && r.th == h);
        let raw = raw.map(|mut r| {
            let top_ref: Vec<f32> = xs[2].par_iter().map(|v| v * s1.unit[2]).collect();
            crate::quattro_grid::repair(&mut r, &prepared_grid, &top_ref);
            r
        });
        // sd Quattro / sd Quattro H: the binned output filters after binning; keep the layers for it
        let sd = camf.matrix("CAMERAID").ok().and_then(|m| m.as_f64().first().copied()).is_some_and(|id| id == 40.0 || id == 41.0);
        if sd && binning_enabled() && binning_for_iso(capture_iso as f64) > 1 {
            let planes = match &raw {
                // bottom/middle repeated over their 2 × 2 block, top at full resolution: binning averages areas
                Some(r) => {
                    let k = ENC_SCALE as f32;
                    let enc = |v: f32, c: usize| (ENC_BLACK as f32 + v / s1.unit[c] * k).round().clamp(0.0, 65535.0) as u16;
                    std::array::from_fn(|c| {
                        (0..w * h)
                            .into_par_iter()
                            .map(|i| {
                                let (x, y) = (i % w, i / w);
                                if c == 2 { enc(r.top[i], 2) } else { enc(r.grid[(y / 2).min(gh - 1) * gw + (x / 2).min(gw - 1)][c], c) }
                            })
                            .collect()
                    })
                }
                None => encode(&xs),
            };
            unbinned_noise = Some((RawImage { columns: w, rows: h, planes }, crate::quattro_nr::Params { binned: true, ..params }, s1.unit));
        }
        if let Some(r) = &raw {
            grid.clone_from(&r.grid);
        }
        drop(raw);
        if std::env::var_os("GRAINEO_QUATTRO_STAGE1_ID").is_none() {
            crate::quattro_nr::stage1(&mut grid, gw, gh, &params);
        }
        let back = |g: &mut Vec<crate::quattro_nr::Px>| {
            for p in g.iter_mut() {
                for c in 0..3 {
                    p[c] /= s1.unit[c];
                }
            }
        };
        // the fine detail of the top layer stays relative to the grid of the prepared layers
        let (mut before, mut after) = (prepared_grid, grid);
        back(&mut before);
        back(&mut after);
        let wbg = Calibration::read_quattro(&file, &camf, &props, &scene, [w, h]).map(|c| c.wb_gain).unwrap_or([1.0; 3]);
        let neutral: [f32; 3] = std::array::from_fn(|c| (wbg[2] / wbg[c]) as f32);
        let t_s1 = t1.elapsed();
        crate::quattro_nr::layers_from_grid(&mut xs, w, h, &before, &after, gw, gh, neutral);
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("Quattro stage 1 timing: grid {:.0} ms, up to layers {:.0} ms, layers back {:.0} ms", t_grid.as_secs_f64() * 1e3, t_s1.as_secs_f64() * 1e3, t1.elapsed().as_secs_f64() * 1e3);
        }
        if std::env::var("GRAINEO_QUATTRO_GRIDCOLOUR").map_or(true, |v| v != "0") {
            grid_colour = Some(Arc::new(GridColour { width: gw, height: gh, data: after }));
        }
        if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
            eprintln!("Quattro stage 1: grid {gw}×{gh}, gains {:?}, unit {:?}, ISO {capture_iso}, {:.2?}", s1.gains, s1.unit, t1.elapsed());
        }
    }
    let mut planes = encode(&xs);
    drop(xs);
    let mut classic = None;
    let mut cal = if file.is_classic() {
        Calibration::read_classic(&file, &camf, &props, &scene, [w, h])?
    } else {
        Calibration::read_quattro(&file, &camf, &props, &scene, [w, h])?
    };
    if cal.is_classic() {
        // Sigma's output stage works on raw values without the dark offset x3fuse-core subtracts
        if let Some((slope, _)) = classic_raw_map(&file, &camf, &planes, w, h, &cal.wb_name) {
            for m in &mut cal.modes {
                if let Some(c) = m.classic.as_mut() {
                    std::sync::Arc::make_mut(c).raw_reference(slope);
                }
            }
        }
        let f = classic_highlights(&file, &camf, &mut planes, w, h, &cal);
        cal.rescale_layers(f);
        classic = classic_chain(&file, &camf, &props, &mut cal, w, h);
    } else {
        own_highlights(&mut planes, w, &cal);
    }
    if defer_sharpen && amount > 0.0 {
        cal.pending_sharpen = (amount * (1.0 - DEFERRED_SHARPEN)) as f32;
    }
    // the binned path gets the same highlight treatment (per pixel, before binning)
    let unbinned_noise = unbinned_noise.map(|(mut raw, params, unit): (RawImage, crate::quattro_nr::Params, [f32; 3])| {
        own_highlights(&mut raw.planes, w, &cal);
        Arc::new(Stage1Source { raw, params, unit })
    });
    let raw = RawImage { columns: w, rows: h, planes };
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!(
            "preprocessing {}: preprocessing {:.2?} (x3fuse-core {:.2?}), {}×{}, orientation {}°",
            path.display(),
            t.elapsed(),
            t_scene,
            w,
            h,
            props.get("ROTATION").unwrap_or("?")
        );
    }
    Ok(Prepared { raw: Arc::new(raw), cal: Arc::new(cal), props, classic, unbinned_noise, grid: grid_colour })
}

/// Grid from the stored sensor values from capture ISO 400 on (`GRAINEO_QUATTRO_RAWGRID=0`: off, measurements).
fn raw_grid_enabled() -> bool {
    std::env::var("GRAINEO_QUATTRO_RAWGRID").map_or(true, |v| v != "0")
}

/// Brightness smoothing for capture ISO 400 to below [`STAGE1_RAW_MIN_ISO`] on x3fuse-core's preparation (measured
/// against Sigma's output: dp Quattro ISO 400–1600 ΔE00 1.7–2.7 → 1.5–2.3, see FAHRPLAN).
fn mid_luma_applies(camf: &x3f::Camf) -> bool {
    let capture_iso = crate::calib::quattro_iso_gain(camf) * camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(100.0);
    (STAGE1_MAX_ISO..STAGE1_RAW_MIN_ISO).contains(&capture_iso) && luma_enabled()
}

/// Brightness smoothing of the top layer ([`crate::quattro_nr::luma_smooth`]; `GRAINEO_QUATTRO_LUMA=0`: off).
fn luma_enabled() -> bool {
    std::env::var("GRAINEO_QUATTRO_LUMA").map_or(true, |v| v != "0")
}

/// Smallest black of x3fuse-core's integer buffers with the first noise reduction stage (intermediate units ≈ sensor
/// counts; leaves the full 14-bit range above it).
const STAGE1_MIN_BIAS: f32 = 2048.0;

/// The first noise reduction stage of the colour grid up to this capture ISO (exclusive).
const STAGE1_MAX_ISO: f64 = 400.0;

/// From this capture ISO on, the first stage runs again, on the grid from the stored sensor values
/// ([`crate::quattro_grid`]). Measured against Sigma's output: ISO 6400 better (dp Quattro ΔE00 3.4–3.8 → 3.0–3.2,
/// block colour 1.9–2.3 → 1.4–1.8), ISO 400–1600 worse than x3fuse-core's preparation; 3200 has no reference.
const STAGE1_RAW_MIN_ISO: f64 = 6400.0;

/// Luminance noise reduction per stop of digital gain (σ_Y = 300 · k · log2 g).
const QUATTRO_LUMA_NR_PER_STOP: f32 = 0.075;

/// Noise reduction of the Quattro preprocessing (x3fuse-core NLM) per digital ISO factor
/// `g = CaptureISO / SensorISO`. x3fuse-core's NLM strength is fixed in the raw value domain;
/// the digital factor (up to ×16 at ISO 6400) amplifies the residual noise afterwards. Hence
/// the strength grows with g (unchanged at g = 1, i.e. ISO 100–400 as before).
/// For experimenting: `GRAINEO_QUATTRO_NRSTR` (colour strength 1–2),
/// `GRAINEO_QUATTRO_NRLK` (luminance per stop of digital factor).
fn quattro_denoise_options(g: f64) -> x3f_core::DenoiseOptions {
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
    let mut o = x3f_core::DenoiseOptions::default();
    let g = g.max(1.0) as f32;
    // Colour: σ grows with √g (at most ×2)
    o.strength = env("GRAINEO_QUATTRO_NRSTR").unwrap_or(g.sqrt()).clamp(1.0, 2.0);
    // Luminance (top layer): only above a digital factor > 1, σ_Y = 300 · k · log2(g)
    let k = env("GRAINEO_QUATTRO_NRLK").unwrap_or(QUATTRO_LUMA_NR_PER_STOP);
    let luma = k * g.log2();
    if luma > 0.0 {
        o.luminance = true;
        o.luma_strength = (luma / o.strength).clamp(0.0, 3.0);
    }
    o
}

/// Saturation of the Classic sensors in raw values per layer: x3fuse-core normalises to
/// `RawSaturationLevel` (or `SaturationLevel`), but the TRUE sensors of the DP1/DP2/SD14/SD15
/// already saturate at ≈ 73–78 % of that (DP1s: ≈ 2320/2240/2230 instead of 2982/3065/3061).
/// Measured with our own TRUE decoder (black ≈ 0 as with x3fuse-core): value of the 20th brightest
/// pixel in the image area (defective pixels excluded); implausibly low (image without
/// blown areas) → [`CLASSIC_CLIP_FALLBACK`] · reference value.
fn classic_clip_fraction(file: &X3fFile, camf: &x3f::Camf) -> [f64; 3] {
    let max_raw: Option<Vec<f64>> = ["RawSaturationLevel", "SaturationLevel"]
        .iter()
        .find_map(|n| camf.matrix(n).ok().map(|m| m.as_f64()).filter(|v| v.len() == 3));
    let (Some(max_raw), Ok(raw)) = (max_raw, file.decode_raw()) else {
        return [1.0; 3];
    };
    let area = camf.matrix("ActiveImageArea").ok().map(|m| m.as_f64()).filter(|v| v.len() == 4);
    let [x0, y0, x1, y1] = area.map_or([0, 0, raw.columns - 1, raw.rows - 1], |v| v.iter().map(|&a| a as usize).collect::<Vec<_>>().try_into().unwrap());
    std::array::from_fn(|c| {
        let mut top: Vec<u16> = Vec::with_capacity((x1 - x0 + 1) * (y1 - y0 + 1));
        for y in y0..=y1.min(raw.rows - 1) {
            top.extend_from_slice(&raw.planes[c][y * raw.columns + x0..=y * raw.columns + x1.min(raw.columns - 1)]);
        }
        let k = top.len().saturating_sub(20);
        let v = *top.select_nth_unstable(k).1 as f64 / max_raw[c];
        if (0.55..=1.0).contains(&v) { v } else { CLASSIC_CLIP_FALLBACK }
    })
}

/// Relation of the preprocessed layers (before layer rescaling) to the raw values per layer:
/// `raw × spatial gain = RawSaturationLevel · layer + offset`, offset as mean over every 4th pixel between about 40
/// raw units and 85 % of `RawSaturationLevel` (the preprocessing removes a dark offset and normalises; Sigma's output
/// stage works on the raw values). `None` without own decoder (Huffman cameras).
fn classic_raw_map(file: &X3fFile, camf: &x3f::Camf, planes: &[Vec<u16>; 3], w: usize, h: usize, wb: &str) -> Option<([f64; 3], [f64; 3])> {
    let sat: Vec<f64> = camf.matrix("RawSaturationLevel").ok().map(|m| m.as_f64()).filter(|v| v.len() == 3)?;
    let raw = file.decode_raw().ok()?;
    let area = camf.matrix("ActiveImageArea").ok().map(|m| m.as_f64()).filter(|v| v.len() == 4)?;
    let (x0, y0) = (area[0] as usize, area[1] as usize);
    if x0 + w > raw.columns || y0 + h > raw.rows {
        return None;
    }
    let sg = classic_spatial_gain(camf, wb);
    let (b, k) = (ENC_BLACK, ENC_SCALE);
    // sums n, Σp, Σr per layer
    let mut acc = [[0f64; 3]; 3];
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let g: [f64; 3] = match &sg {
                Some((rows, cols, v)) => {
                    let fy = y as f64 / h as f64 * (*rows - 1) as f64;
                    let fx = x as f64 / w as f64 * (*cols - 1) as f64;
                    let (ri, ci) = ((fy as usize).min(rows - 2), (fx as usize).min(cols - 2));
                    let (ty, tx) = (fy - ri as f64, fx - ci as f64);
                    let at = |r: usize, c: usize, ch: usize| v[(r * cols + c) * 3 + ch];
                    std::array::from_fn(|ch| (1.0 - ty) * ((1.0 - tx) * at(ri, ci, ch) + tx * at(ri, ci + 1, ch)) + ty * ((1.0 - tx) * at(ri + 1, ci, ch) + tx * at(ri + 1, ci + 1, ch)))
                }
                None => [1.0; 3],
            };
            for c in 0..3 {
                let rv = raw.planes[c][(y0 + y) * raw.columns + x0 + x] as f64;
                if rv > 0.85 * sat[c] {
                    continue;
                }
                let r = rv * g[c];
                let p = (planes[c][y * w + x] as f64 - b) / k;
                // only clearly above black: raw values are cut at 0, which would bias the offset
                if p * sat[c] < 40.0 {
                    continue;
                }
                let a = &mut acc[c];
                a[0] += 1.0;
                a[1] += p;
                a[2] += r;
            }
        }
    }
    // Slope: the preprocessing normalises to `RawSaturationLevel` (checked on DP1, DP2, SD15); only a
    // clear dark offset (high ISO, e.g. DP2x ISO 800: ≈ 68) is taken over, small ones are noise.
    let slope: [f64; 3] = std::array::from_fn(|c| sat[c]);
    let mut off = [0f64; 3];
    for c in 0..3 {
        let [n, sp, sr] = acc[c];
        if n < 1000.0 {
            return None;
        }
        let b = (sr - sat[c] * sp) / n;
        off[c] = if b.abs() >= CLASSIC_DARK_MIN { b } else { 0.0 };
    }
    Some((slope, off))
}

/// Smallest dark offset (raw units) taken over by [`classic_raw_map`].
const CLASSIC_DARK_MIN: f64 = 16.0;

/// Saturation share when the image itself has no blown areas (DP1s measurement).
const CLASSIC_CLIP_FALLBACK: f64 = 0.75;

/// Spatial gain of the Classic cameras (`SpatialGainTables[WB]` or `SpatialGain`, rows ×
/// columns × 3 over the image area, bilinear), as in x3fuse-core `x3f_get_classic_spatial_gain`.
fn classic_spatial_gain(camf: &x3f::Camf, wb: &str) -> Option<(usize, usize, Vec<f64>)> {
    let name = camf.property("SpatialGainTables", wb).unwrap_or("SpatialGain");
    let m = camf.matrix(name).or_else(|_| camf.matrix("SpatialGain")).ok()?;
    let shape = m.shape();
    (shape.len() == 3 && shape[2] == 3 && shape[0] > 1 && shape[1] > 1).then(|| (shape[0], shape[1], m.as_f64()))
}

/// Highlights of the Classic cameras: normalise layers to their real saturation (1 = saturated,
/// so sensor views and raw clipping are correct too) and blend blown areas smoothly to
/// neutral. x3fuse-core's highlight reconstruction does not apply here because it assumes the
/// too-high normalisation; without treatment blown skies would turn magenta.
/// Returns the normalisation factors per layer (for [`Calibration::rescale_layers`]).
fn classic_highlights(file: &X3fFile, camf: &x3f::Camf, planes: &mut [Vec<u16>; 3], w: usize, h: usize, cal: &Calibration) -> [f64; 3] {
    let f = classic_clip_fraction(file, camf);
    let sg = classic_spatial_gain(camf, &cal.wb_name);
    // Gain per layer at the shot white balance (relative)
    let g = cal.wb_gain.map(|v| v / cal.wb_gain.iter().cloned().fold(f64::INFINITY, f64::min));
    let (start, k) = (CLASSIC_HL_START as f32, ENC_SCALE as f32);
    let inv_f = f.map(|v| (1.0 / v) as f32);
    let gf = g.map(|v| v as f32);
    let [p0, p1, p2] = planes;
    p0.par_chunks_mut(w).zip(p1.par_chunks_mut(w)).zip(p2.par_chunks_mut(w)).enumerate().for_each(|(y, ((r0, r1), r2))| {
        // Interpolate the spatial gain row vertically in advance
        let row: Option<Vec<[f32; 3]>> = sg.as_ref().map(|(rows, cols, v)| {
            let fy = y as f64 / h as f64 * (*rows - 1) as f64;
            let (ri, t) = ((fy as usize).min(rows - 2), fy - (fy as usize).min(rows - 2) as f64);
            (0..*cols).map(|c| std::array::from_fn(|ch| ((1.0 - t) * v[(ri * cols + c) * 3 + ch] + t * v[((ri + 1) * cols + c) * 3 + ch]) as f32)).collect()
        });
        for x in 0..w {
            let sgain: [f32; 3] = match (&row, &sg) {
                (Some(r), Some((_, cols, _))) => {
                    let fx = x as f32 / w as f32 * (*cols - 1) as f32;
                    let ci = (fx as usize).min(cols - 2);
                    let t = fx - ci as f32;
                    std::array::from_fn(|ch| (1.0 - t) * r[ci][ch] + t * r[ci + 1][ch])
                }
                _ => [1.0; 3],
            };
            let dec = |u: u16| (u as f32 - ENC_BLACK as f32) / k;
            // x' = x / f: 1 = saturation (without spatial gain), white-balanced v = x' · g
            let xs = [dec(r0[x]) * inv_f[0], dec(r1[x]) * inv_f[1], dec(r2[x]) * inv_f[2]];
            let v = [xs[0] * gf[0], xs[1] * gf[1], xs[2] * gf[2]];
            // Common white point: where the first layer saturates
            let white = (0..3).map(|c| sgain[c] * gf[c]).fold(f32::INFINITY, f32::min);
            let m = v[0].max(v[1]).max(v[2]) / white;
            let out = if m > start {
                let t = ((m - start) / (1.0 - start)).clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t);
                let target = v[0].max(v[1]).max(v[2]).min(white);
                [0, 1, 2].map(|c| ((1.0 - t) * v[c] + t * target).min(white) / gf[c])
            } else {
                xs
            };
            let enc = |x: f32| (ENC_BLACK as f32 + x * k).round().clamp(0.0, 65535.0) as u16;
            r0[x] = enc(out[0]);
            r1[x] = enc(out[1]);
            r2[x] = enc(out[2]);
        }
    });
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Classic: saturation {:.3}/{:.3}/{:.3} of the reference value, spatial gain {}", f[0], f[1], f[2], if sg.is_some() { "yes" } else { "no" });
    }
    f
}

/// From this share of the white point on, Classic highlights glide to neutral.
const CLASSIC_HL_START: f64 = 0.85;

/// Own highlight method for Quattro (without x3fuse-core's reconstruction): from [`CLASSIC_HL_START`] of the common
/// white point (where the first layer saturates, at the shot white balance) the pixel glides to neutral with a
/// smoothstep weight. The weight follows the lowest layer, not the highest: a bright coloured light (one layer near
/// saturation, the others well below) keeps its hue, as in the camera maker's output; only where every layer nears
/// saturation and the colour is no longer measurable does it turn white. Layers above the white point are capped.
/// No donor pixels, no colour models from the neighbourhood.
///
/// Measured against the maker's TIFFs (20 Quattro files, ΔE00): light source in SDQ_0612 7.86 → 0.54 (255/248/240 →
/// 255/231/216, maker 255/230/215); mean over all files 1.243 → 1.242, in highlights 0.935 → 0.917, no file worse.
fn own_highlights(planes: &mut [Vec<u16>; 3], w: usize, cal: &Calibration) {
    let g = cal.wb_gain.map(|v| v / cal.wb_gain.iter().cloned().fold(f64::INFINITY, f64::min));
    let gf = g.map(|v| v as f32);
    let white = gf.iter().cloned().fold(f32::INFINITY, f32::min);
    let (start, k, b) = (CLASSIC_HL_START as f32, ENC_SCALE as f32, ENC_BLACK as f32);
    let [p0, p1, p2] = planes;
    p0.par_chunks_mut(w).zip(p1.par_chunks_mut(w)).zip(p2.par_chunks_mut(w)).for_each(|((r0, r1), r2)| {
        for x in 0..r0.len() {
            let dec = |u: u16| (u as f32 - b) / k;
            let v = [dec(r0[x]) * gf[0], dec(r1[x]) * gf[1], dec(r2[x]) * gf[2]];
            let hi = v[0].max(v[1]).max(v[2]);
            if hi <= start * white {
                continue;
            }
            let lo = v[0].min(v[1]).min(v[2]);
            let t = ((lo / white - start) / (1.0 - start)).clamp(0.0, 1.0);
            let t = t * t * (3.0 - 2.0 * t);
            let target = hi.min(white);
            let enc = |x: f32| (b + x * k).round().clamp(0.0, 65535.0) as u16;
            let out = [0, 1, 2].map(|c| ((1.0 - t) * v[c] + t * target).min(white) / gf[c]);
            r0[x] = enc(out[0]);
            r1[x] = enc(out[1]);
            r2[x] = enc(out[2]);
        }
    });
}

/// Classic: Sigma's whole chain at the shot white balance and colour mode ([`crate::classic_new`]),
/// converted to the normalisation of [`crate::linearize`] through the inverse of the shot mode's
/// conversion matrix ([`crate::classic_new::conversion`]), so that the output step gets back exactly the
/// chain's codes. `None` (and the previous way) where the chain is not available.
fn classic_chain(file: &X3fFile, camf: &x3f::Camf, props: &PropList, cal: &mut Calibration, w: usize, h: usize) -> Option<Arc<Linear>> {
    // output stages that only work with the chain's result are dropped when it is not available
    let drop_chain_only = |cal: &mut Calibration| {
        for m in &mut cal.modes {
            if m.classic.as_ref().is_some_and(|c| c.only_chain) {
                m.classic = None;
            }
        }
    };
    if std::env::var("GRAINEO_CLASSIC_CHAIN").is_ok_and(|v| v == "0") {
        drop_chain_only(cal);
        return None;
    }
    let t = std::time::Instant::now();
    let mode = cal.camera_mode.clone();
    // the chain also hands out the high-pass of its detail step (sharpness slider)
    let run = || crate::classic_new::run(file, camf, props, &cal.wb_name, &mode, file.header_rotation().unwrap_or(0) as u32);
    let (result, high_pass) = crate::classic_new::detail::with_high_pass(run);
    let codes = match result {
        Ok(c) => c,
        Err(e) => {
            if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
                eprintln!("Classic chain not available: {e}");
            }
            drop_chain_only(cal);
            return None;
        }
    };
    // grey-world share of the gains (incandescent light, high ISO): the chain's mean per layer
    if let Some(mean) = crate::classic_new::gray_mean_of_run() {
        for m in &mut cal.modes {
            if let Some(c) = m.classic.as_mut() {
                Arc::make_mut(c).gray_world(mean);
            }
        }
    }
    let [x0, y0, x1, y1] = codes.crop;
    let spp = cal.mode(Some(&mode)).classic.clone();
    let a_inv = spp.as_ref().and_then(|s| crate::matrix::Mat3(std::array::from_fn(|i| f64::from(s.a[i]))).inverse());
    let (Some(spp), Some(a_inv), true) = (spp, a_inv, x1 - x0 == w && y1 - y0 == h) else {
        drop_chain_only(cal);
        return None;
    };
    let a_inv = a_inv.to_f32();
    // Render: x = lin · count gain of the calibration / count gain of the output stage · k_raw
    let k: [f32; 3] = std::array::from_fn(|c| (spp.wb_gain[c] / (spp.k_raw[c] * cal.wb_gain[c])) as f32);
    let mut data = vec![0f32; w * h * 3];
    // new output step: the codes as fractional values that keep the exact index of the detail step
    // (rounded they are the codes; the sharpness slider needs the index, see `output::code_with_position`)
    let high_pass = high_pass.filter(|hp| hp.width == w && hp.height == h && hp.index.len() == w * h * 3);
    let position = high_pass.as_ref().map(|hp| hp.index.as_slice());
    data.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let src = &codes.data[((y0 + y) * codes.width + x0) * 3..((y0 + y) * codes.width + x1) * 3];
        let idx = position.map(|l| &l[y * w * 3..(y + 1) * w * 3]);
        for (x, (o, c)) in row.chunks_exact_mut(3).zip(src.chunks_exact(3)).enumerate() {
            let v = match idx {
                Some(l) => std::array::from_fn(|i| crate::classic_new::output::code_with_position(l[x * 3 + i]) / 4096.0),
                None => [c[0] as f32 / 4096.0, c[1] as f32 / 4096.0, c[2] as f32 / 4096.0],
            };
            for r in 0..3 {
                o[r] = (a_inv[r * 3] * v[0] + a_inv[r * 3 + 1] * v[1] + a_inv[r * 3 + 2] * v[2]) * k[r];
            }
        }
    });
    // the output step's slider-independent part per colour mode from the chain's codes (image mean,
    // mode contrast); used in `render_counts_impl`
    let opt = crate::classic_new::output::Options::for_camera(props);
    for m in &mut cal.modes {
        if m.classic.is_some() {
            m.classic_output = Some(Arc::new(crate::classic_new::output::OutputBasis::new(camf, props, &m.name, &codes, opt)));
        }
    }
    if std::env::var_os("GRAINEO_QUATTRO_LOG").is_some() {
        eprintln!("Classic-Kette: {:.2?}", t.elapsed());
    }
    // the high-pass travels with the counts (crops, disk cache) as a detail image of its own kind
    let detail = high_pass.map(|hp| Arc::new(crate::sharpness::SharpDetail { l: hp.values, base: crate::sharpness::CLASSIC_HIGH_PASS, scale: 1.0 }));
    Some(Arc::new(Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail }))
}

/// Working image for [`crate::render_counts`]: RGB counts of colour mode `mode` at the
/// shot white balance (before the tone curve), like the SPP base for Merrill.
/// Like [`counts`], but from a (e.g. denoised) linear image from
/// [`crate::linearize`] (Quattro: without positional corrections, `x = (u − black)/sat`).
pub fn counts_from_linear(p: &Prepared, lin: &Linear, mode: Option<&str>, binning: crate::Binning) -> Linear {
    let cal = &p.cal;
    let g: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c] * cal.saturation) as f32);
    let src = &lin.data;
    let mut c = binned_counts(cal, mode, lin.width, lin.height, binning_factor(cal, binning), |i| [src[i * 3] * g[0], src[i * 3 + 1] * g[1], src[i * 3 + 2] * g[2]]);
    // Classic: the high-pass of the new output step stays with the counts (same size only)
    if c.width == lin.width && c.height == lin.height {
        c.detail = lin.detail.clone().filter(|d| d.is_classic_high_pass());
    }
    c
}

/// [`to_counts`] after binning `n × n` ([`binning_factor`]); `n = 1` unchanged.
fn binned_counts(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, n: usize, v: impl Fn(usize) -> [f32; 3] + Sync) -> Linear {
    if n <= 1 {
        let data = to_counts(cal, mode, w, h, false, v);
        return Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
    }
    let (bw, bh, b) = bin_values(w, h, n, v);
    let data = to_counts(cal, mode, bw, bh, true, |i| b[i]);
    Linear { mono: false, haze: None, toneq: None, width: bw, height: bh, data, detail: None }
}

/// Working image for [`crate::render_counts`]: RGB counts of colour mode `mode` at the shot white balance (before
/// the tone curve). Quattro: the colour stage ([`crate::quattro_color`]); Classic: the result of Sigma's chain where
/// available; other cameras: the plain colour matrix.
pub fn counts(p: &Prepared, mode: Option<&str>, binning: crate::Binning) -> Linear {
    if let Some(lin) = p.classic.as_ref() {
        return counts_from_linear(p, lin, mode, binning);
    }
    let cal = &p.cal;
    // p = (u − black)/sat, counts = M · diag(count_scale · wb_gain · sat) · p
    let g: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c]) as f32);
    let black = cal.black.map(|b| b as f32);
    let n = binning_factor(cal, binning);
    if let Some(s) = p.unbinned_noise.as_deref().filter(|_| n > 1) {
        // the first noise reduction stage after binning (on the binned grid)
        let (w, h) = (s.raw.columns, s.raw.rows);
        let pl = &s.raw.planes;
        let k = ENC_SCALE as f32;
        let (bw, bh, mut b) = bin_values(w, h, n, |i| std::array::from_fn(|c| (pl[c][i] as f32 - black[c]) / k * s.unit[c]));
        if std::env::var("GRAINEO_QUATTRO_STAGE1_BIN").map_or(true, |v| v != "0") {
            crate::quattro_nr::stage1(&mut b, bw, bh, &s.params);
        }
        let scale: [f32; 3] = std::array::from_fn(|c| k * g[c] / s.unit[c]);
        let data = to_counts(cal, mode, bw, bh, true, |i| std::array::from_fn(|c| b[i][c] * scale[c]));
        return Linear { mono: false, haze: None, toneq: None, width: bw, height: bh, data, detail: None };
    }
    let raw = &p.raw;
    let (w, h) = (raw.columns, raw.rows);
    let pl = &raw.planes;
    let colour_stage = cal.model.is_quattro() && cal.sensor_iso > 0.0 && std::env::var("GRAINEO_RAWBLEND").map_or(true, |v| v != "0");
    if let Some(grid) = p.grid.as_deref().filter(|g| n <= 1 && colour_stage && g.width * 2 <= w && g.height * 2 <= h) {
        let data = grid_counts(cal, mode, grid, w, h, |i| (pl[2][i] as f32 - black[2]) * g[2]);
        return Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: None };
    }
    binned_counts(cal, mode, w, h, n, |i| std::array::from_fn(|c| (pl[c][i] as f32 - black[c]) * g[c]))
}

/// Parameters of the first noise reduction stage from the camera data.
struct Stage1Camera {
    /// daylight white balance gains with temperature factor, normalised to the middle layer
    gains: [f32; 3],
    /// normalised layer value → counts above black / (SensorISO/100) (sd Quattro H: times the digital ISO gain)
    unit: [f32; 3],
    /// black of the stored sensor values (`RawOffset`)
    offset: f32,
    /// stored value above black → units of the stage: digital ISO gain / (SensorISO/100)
    raw_unit: [f32; 3],
    /// white of the raw range above black (14-bit values: 16383 − `RawOffset`)
    white: f32,
}

fn stage1_params(camf: &x3f::Camf, _props: &PropList, sd_quattro_h: bool) -> Stage1Camera {
    let v3 = |name: &str| camf.matrix(name).ok().map(|m| m.as_f64()).filter(|v| v.len() == 3).map(|v| [v[0], v[1], v[2]]);
    let sun = v3("SunlightWBGain").or_else(|| camf.property("WhiteBalanceGains", "Sunlight").and_then(|n| v3(&n))).unwrap_or([1.0; 3]);
    let temp = v3("TempGainFact").filter(|t| t.iter().all(|v| v.is_finite() && (0.5..=1.5).contains(v))).unwrap_or([1.0; 3]);
    let g: [f64; 3] = std::array::from_fn(|c| sun[c] * temp[c]);
    let gains = std::array::from_fn(|c| (g[c] / g[1]) as f32);
    let offset = camf.matrix("RawOffset").ok().and_then(|m| m.as_f64().first().copied()).unwrap_or(2048.0).min(4096.0);
    let sensor = camf.matrix("SensorISO").ok().and_then(|m| m.as_f64().first().copied()).filter(|v| *v > 0.0).unwrap_or(100.0);
    let digital = if sd_quattro_h { v3("DigitalISOGain").unwrap_or([1.0; 3]) } else { [1.0; 3] };
    let unit = std::array::from_fn(|c| ((16383.0 - offset) / (sensor / 100.0) * digital[c]) as f32);
    let raw_unit = std::array::from_fn(|c| (digital[c] / (sensor / 100.0)) as f32);
    Stage1Camera { gains, unit, offset: offset as f32, raw_unit, white: (16383.0 - offset) as f32 }
}

/// White-balanced camera counts `v(i)` → output counts: Quattro the colour stage, other cameras the colour matrix.
/// `binned`: the image was binned (changes the brightness index of the colour stage).
fn to_counts(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, binned: bool, v: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    let m = cal.cam_to_srgb(mode, None);
    let stage = cal.model.is_quattro() && cal.sensor_iso > 0.0 && std::env::var("GRAINEO_RAWBLEND").map_or(true, |v| v != "0");
    if stage {
        return colour_stage(cal, mode, w, h, binned, v);
    }
    let mut data = vec![0f32; w * h * 3];
    {
        let m = m.to_f32();
        data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
            let v = v(i);
            for r in 0..3 {
                px[r] = m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2];
            }
        });
        if cal.pending_sharpen > 0.0 {
            capture_sharpen(&mut data, w, h, cal.pending_sharpen);
        }
    }
    data
}

/// Colour stage ([`crate::quattro_color`]) on white-balanced counts `v`: matrix blend,
/// chroma clamp, colour mode fine correction (sd Quattro); the local contrast follows in [`finish_own`].
fn own_colour(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, binned: bool, v: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    use crate::quattro_color as qf;
    let mix = match (cal.model == crate::calib::Model::SdQuattro, binned) {
        (true, false) => qf::MIX_SD,
        (true, true) => qf::MIX_SD_BINNED,
        _ => qf::MIX_DP,
    };
    let stage = qf::ColorLevel {
        matrix: cal.cam_to_srgb(mode, None).to_f32(),
        iso: cal.iso_gain as f32,
        level: (1.0 / (ENC_SCALE * cal.count_scale * cal.wb_gain[1] * cal.sensor_iso / 100.0)) as f32,
        signal: (100.0 / (cal.iso_gain * cal.sensor_iso)) as f32,
        mix,
    };
    let mut data = vec![0f32; w * h * 3];
    data.par_chunks_mut(3).enumerate().for_each(|(i, px)| px.copy_from_slice(&stage.pixel(v(i))));
    qf::chroma_limit(&mut data, w, h, OWN_CLAMP_RADIUS, OWN_CLAMP_RANK);
    if let Some(a) = cal.mode(mode).axes.as_deref().filter(|_| std::env::var("GRAINEO_MULTIAXIS").map_or(true, |v| v != "0")) {
        data.par_chunks_mut(3).for_each(|px| a.apply(px));
    }
    data
}

/// Colour stage incl. its finishing step.
fn colour_stage(cal: &Calibration, mode: Option<&str>, w: usize, h: usize, binned: bool, v: impl Fn(usize) -> [f32; 3] + Sync) -> Vec<f32> {
    let mut data = own_colour(cal, mode, w, h, binned, v);
    finish_own(cal, &mut data, w, h);
    data
}

/// Neighbourhood radius and rank of the chroma clamp of the own colour stage (3 × 3 minimum/maximum; larger
/// neighbourhoods changed the result by less than 0.05 ΔE00 in the comparison).
const OWN_CLAMP_RADIUS: usize = 1;
const OWN_CLAMP_RANK: usize = 0;

/// After the colour stage: local contrast below capture ISO 800 ([`crate::quattro_color::local_contrast`]).
fn finish_own(cal: &Calibration, data: &mut [f32], w: usize, h: usize) {
    if !lce_applies(cal.iso_gain * cal.sensor_iso) {
        return;
    }
    let white = (ENC_SCALE * cal.count_scale * cal.sensor_iso / 100.0) as f32;
    crate::quattro_color::local_contrast(data, w, h, white, &crate::quattro_color::CONTRAST_GAINS, crate::quattro_color::CONTRAST_SOFT);
}

/// After the first noise reduction stage: the colour is formed on the denoised colour grid (before the
/// detail of the top layer comes back), enlarged 2× and the fine detail of the top
/// layer added to the brightness only (BT.601 Y): ΔY = ΔT · Y/T of the enlarged grid (T floor
/// [`DETAIL_T_FLOOR`]), so the top-layer noise does not become colour noise.
fn grid_counts(cal: &Calibration, mode: Option<&str>, grid: &GridColour, w: usize, h: usize, t_full: impl Fn(usize) -> f32 + Sync) -> Vec<f32> {
    let (gw, gh) = (grid.width, grid.height);
    let g: [f32; 3] = std::array::from_fn(|c| (cal.count_scale * cal.wb_gain[c] * ENC_SCALE) as f32);
    let grid_v = |i: usize| std::array::from_fn(|c| grid.data[i][c] * g[c]);
    let cg = own_colour(cal, mode, gw, gh, false, grid_v);
    // counts → units of the detail floor: counts above black / (SensorISO/100)
    let k = (100.0 / cal.sensor_iso) as f32;
    let up: [Vec<f32>; 3] = std::array::from_fn(|c| crate::quattro_nr::enlarge(&cg.chunks_exact(3).map(|p| p[c]).collect::<Vec<_>>(), gw, gh, w, h));
    let t_up = crate::quattro_nr::enlarge(&grid.data.iter().map(|p| p[2] * g[2]).collect::<Vec<_>>(), gw, gh, w, h);
    let floor = std::env::var("GRAINEO_QUATTRO_TFLOOR").ok().and_then(|v| v.parse().ok()).unwrap_or(DETAIL_T_FLOOR) / k;
    let add = std::env::var("GRAINEO_QUATTRO_DETAILY").is_ok_and(|v| v == "add");
    let mut data = vec![0f32; w * h * 3];
    data.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let o = [up[0][i], up[1][i], up[2][i]];
        let y = 0.299 * o[0] + 0.587 * o[1] + 0.114 * o[2];
        let dy = (t_full(i) - t_up[i]) * y.max(0.0) / t_up[i].max(floor);
        if add {
            for c in 0..3 {
                px[c] = o[c] + dy;
            }
        } else {
            // same ratio on all three channels: brightness changes, hue and saturation stay
            let f = if y > 0.0 { ((y + dy) / y).max(0.0) } else { 1.0 };
            for c in 0..3 {
                px[c] = o[c] * f;
            }
        }
    });
    if cal.pending_sharpen > 0.0 && std::env::var("GRAINEO_QUATTRO_GRIDSHARP").is_ok_and(|v| v == "1") {
        capture_sharpen(&mut data, w, h, cal.pending_sharpen);
    }
    finish_own(cal, &mut data, w, h);
    data
}

/// Smallest top-layer value (grid units) for the brightness slope of the top-layer detail.
const DETAIL_T_FLOOR: f32 = 16.0;

/// Denoised colour grid (normalised layer values, half the size of the layers).
pub struct GridColour {
    pub width: usize,
    pub height: usize,
    pub data: Vec<crate::quattro_nr::Px>,
}

/// Binning factor (neighbouring pixels averaged, processed and output at 1/n of the width and height):
/// `Auto` as in the camera maker's software = sd Quattro / sd Quattro H from capture ISO 800 / 1600 / 6400 → 2 / 3 / 4,
/// dp Quattro never; `On` the same steps for the dp Quattro too (own addition); `Off` and other cameras: 1.
pub fn binning_factor(cal: &Calibration, setting: crate::Binning) -> usize {
    use crate::Binning;
    let camera = match cal.model {
        crate::calib::Model::SdQuattro => setting != Binning::Off,
        crate::calib::Model::DpQuattro => setting == Binning::On,
        _ => false,
    };
    if !camera || !binning_enabled() || std::env::var("GRAINEO_BINNING").is_ok_and(|v| v == "0") {
        return 1;
    }
    binning_for_iso(cal.iso_gain * cal.sensor_iso)
}

/// Binning factor by capture ISO (thresholds 800 / 1600 / 6400 as in the camera maker's software).
pub fn binning_for_iso(capture_iso: f64) -> usize {
    match capture_iso {
        i if i >= 6400.0 => 4,
        i if i >= 1600.0 => 3,
        i if i >= 800.0 => 2,
        _ => 1,
    }
}

static BINNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Allow binning ([`binning_factor`]) in this program. Off by default: the app's geometry (crop, masks, retouching,
/// zoom) still assumes the full image size; the test program and export tools switch it on. `GRAINEO_BINNING=1`
/// switches it on as well.
pub fn enable_binning(on: bool) {
    BINNING.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn binning_enabled() -> bool {
    BINNING.load(std::sync::atomic::Ordering::Relaxed) || std::env::var("GRAINEO_BINNING").is_ok_and(|v| v == "1")
}

/// Binning factor for these settings (see [`binning_factor`]).
pub fn binning_for(cal: &Calibration, s: &crate::Settings) -> usize {
    binning_factor(cal, s.binning)
}

/// Can this image be binned at all (sd Quattro / sd Quattro H, dp Quattro with `On`)? Whether it takes effect
/// also depends on the capture ISO ([`binning_iso_factor`]).
pub fn binning_camera(cal: &Calibration) -> Option<BinningCamera> {
    match cal.model {
        crate::calib::Model::SdQuattro => Some(BinningCamera::Sd),
        crate::calib::Model::DpQuattro => Some(BinningCamera::Dp),
        _ => None,
    }
}

/// Camera class for the binning switch: sd Quattro (H) bin in the camera maker's software, the dp Quattro never.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinningCamera {
    Sd,
    Dp,
}

/// Factor the capture ISO of this image calls for (1 below ISO 800), regardless of the setting.
pub fn binning_iso_factor(cal: &Calibration) -> usize {
    binning_for_iso(cal.iso_gain * cal.sensor_iso)
}

/// Raw image binned `n × n` (block mean, rounded; remainder rows/columns dropped) for the own pipeline and the
/// sensor views of a binned session (Sigma's base bins its counts itself, see [`counts`]).
pub fn bin_raw(raw: &x3f::RawImage, n: usize) -> x3f::RawImage {
    let (bw, bh) = (raw.columns / n, raw.rows / n);
    let w = raw.columns;
    let planes = std::array::from_fn(|c| {
        let src = &raw.planes[c];
        (0..bw * bh)
            .into_par_iter()
            .map(|i| {
                let (bx, by) = (i % bw, i / bw);
                let mut s = 0u32;
                for y in by * n..by * n + n {
                    for x in bx * n..bx * n + n {
                        s += src[y * w + x] as u32;
                    }
                }
                let k = (n * n) as u32;
                ((s + k / 2) / k) as u16
            })
            .collect()
    });
    x3f::RawImage { columns: bw, rows: bh, planes }
}

/// Mean of `n × n` neighbouring pixels of `v` (image `w × h`); remainder rows/columns are dropped.
fn bin_values(w: usize, h: usize, n: usize, v: impl Fn(usize) -> [f32; 3] + Sync) -> (usize, usize, Vec<[f32; 3]>) {
    let (bw, bh) = (w / n, h / n);
    let inv = 1.0 / (n * n) as f32;
    let out = (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % bw, i / bw);
            let mut s = [0f32; 3];
            for y in by * n..by * n + n {
                for x in bx * n..bx * n + n {
                    let p = v(y * w + x);
                    for c in 0..3 {
                        s[c] += p[c];
                    }
                }
            }
            s.map(|a| a * inv)
        })
        .collect();
    (bw, bh, out)
}

/// Share of the base sharpening applied in the preparation where the rest is deferred (measured: sd Quattro
/// SDI0004 edge gradient p99: reference 7.66, with 0.5 → 7.55, without → 5.72).
const DEFERRED_SHARPEN: f64 = 0.5;

/// Sigma's third stage uses the lightness wavelet only below an ISO factor of 8 (capture ISO 800); from there on it
/// uses another (box filter) way that is not rebuilt, so the previous base sharpening stays there.
fn lce_applies(capture_iso: f64) -> bool {
    capture_iso < 800.0 && std::env::var("GRAINEO_QUATTRO_LCE").map_or(true, |v| v != "0")
}

/// Base sharpening strength by ISO: SPP and the camera noticeably sharpen Quattro images even at
/// sharpness 0; less at high ISO so that noise is not emphasised.
fn capture_sharpen_amount(iso: f64) -> f64 {
    let stops = (iso.max(100.0) / 100.0).log2(); // 0 at ISO 100, 6 at ISO 6400
    (1.0 - 0.12 * stops).clamp(0.25, 1.0)
}

/// Base sharpening (unsharp mask, radius σ = 0.8 px) on the luminance of the linear
/// image; all three channels are scaled by the same factor, so no
/// colour fringes appear. Halos are tamed by limiting the factor (0.6 … 1.6).
fn capture_sharpen(rgb: &mut [f32], w: usize, h: usize, amount: f32) {
    let y: Vec<f32> = rgb.par_chunks(3).map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).max(0.0)).collect();
    let sigma = 0.8f32;
    let r = 3usize;
    let k: Vec<f32> = {
        let v: Vec<f32> = (0..=2 * r).map(|i| (-((i as f32 - r as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
        let s: f32 = v.iter().sum();
        v.into_iter().map(|x| x / s).collect()
    };
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
        let src = &y[row * w..(row + 1) * w];
        for x in 0..w {
            let mut acc = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = (x + i).saturating_sub(r).min(w - 1);
                acc += kv * src[xx];
            }
            out[x] = acc;
        }
    });
    let blur: Vec<f32> = (0..h)
        .into_par_iter()
        .flat_map_iter(|row| {
            let (tmp, k) = (&tmp, &k);
            (0..w).map(move |x| {
                let mut acc = 0.0;
                for (i, kv) in k.iter().enumerate() {
                    let yy = (row + i).saturating_sub(r).min(h - 1);
                    acc += kv * tmp[yy * w + x];
                }
                acc
            })
        })
        .collect();
    rgb.par_chunks_mut(3).zip(y.par_iter().zip(blur.par_iter())).for_each(|(p, (&l, &b))| {
        if l > 1e-5 {
            let f = (1.0 + amount * (l - b) / l).clamp(0.6, 1.6);
            p[0] *= f;
            p[1] *= f;
            p[2] *= f;
        }
    });
}

#[cfg(test)]
mod binning_tests {
    use super::*;

    #[test]
    fn factor_by_capture_iso() {
        for (iso, n) in [(100.0, 1), (500.0, 1), (799.0, 1), (800.0, 2), (1250.0, 2), (1600.0, 3), (3200.0, 3), (6400.0, 4), (12800.0, 4)] {
            assert_eq!(binning_for_iso(iso), n, "ISO {iso}");
        }
    }

    #[test]
    fn binning_averages_and_sizes() {
        // 5424 × 3616 (sd Quattro) at ×4 → 1356 × 904 as in the reference output; remainder dropped
        let (w, h) = (10, 7);
        let (bw, bh, v) = bin_values(w, h, 3, |i| [i as f32, 1.0, (i % w) as f32]);
        assert_eq!((bw, bh), (3, 2));
        assert_eq!(v.len(), 6);
        // first block: rows 0..3, columns 0..3 → mean index (0+1+2+10+11+12+20+21+22)/9 = 11
        assert!((v[0][0] - 11.0).abs() < 1e-4 && (v[0][1] - 1.0).abs() < 1e-6 && (v[0][2] - 1.0).abs() < 1e-6);
        assert_eq!((5424 / 4, 3616 / 4), (1356, 904));
    }

    #[test]
    fn raw_binning_rounds_the_block_mean() {
        // 5 × 3, n = 2 → 2 × 1; last column and row dropped
        let p: Vec<u16> = (0..15).collect();
        let raw = x3f::RawImage { columns: 5, rows: 3, planes: [p.clone(), p.clone(), vec![7; 15]] };
        let b = bin_raw(&raw, 2);
        assert_eq!((b.columns, b.rows), (2, 1));
        // (0+1+5+6)/4 = 3, (2+3+7+8)/4 = 5
        assert_eq!(b.planes[0], vec![3, 5]);
        assert_eq!(b.planes[2], vec![7, 7]);
    }
}

