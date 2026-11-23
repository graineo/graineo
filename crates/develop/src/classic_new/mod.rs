//! Classic chain: the processing of the older Foveon cameras (SD9/SD10, SD14/SD15, DP1…DP2x)
//! from the raw values to the linear working codes, step by step. Each step lives in its own file; [`kette`] runs them in order
//! ([`run`] is the entry point). The output stage (tone curve, colour adjustment, output matrix)
//! is not part of this module, see [`kette`].

pub mod auto_wb;
pub mod output;
pub mod auto_wb_old;
pub mod region;
pub mod sliders;
pub mod region_filter;
pub mod region_scaling;
pub mod dcraw;
pub mod despeckle;
pub mod detail;
pub mod color;
pub mod calibration;
pub mod edge;
pub mod chain;
pub mod correction;
pub mod meta;
pub mod tanh_table;
pub mod conversion;

pub use chain::run;
pub use meta::{Codes, Meta};

thread_local! {
    /// Mean of the layers from the new sensor correction of the current image (only kept for
    /// incandescent white balance at high ISO, where the later steps need it as well).
    static GRAY_MEAN: std::cell::Cell<Option<[f64; 3]>> = const { std::cell::Cell::new(None) };
    /// Neutral point of the automatic white balance from the image, estimated by the sensor step of
    /// the chain run on this thread and used by all later steps of the same run.
    static AUTO_NEUTRAL: std::cell::Cell<Option<[f64; 3]>> = const { std::cell::Cell::new(None) };
    /// The calibration of the run on this thread has a monochrome matrix (set by the sensor step).
    static MONOCHROME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mean of the layers kept by the sensor step of the last run on this thread (incandescent white
/// balance at high ISO only; `None` otherwise).
pub fn gray_mean_of_run() -> Option<[f64; 3]> {
    GRAY_MEAN.with(|g| g.get())
}

/// The grey mean for the calibration of the later steps: `Ok(None)` when not needed, `Err` when it
/// is needed but the new sensor correction did not run for this image.
fn gray_mean(k: &calibration::Calibration) -> Result<Option<[f64; 3]>, String> {
    if !(k.iso.high && k.wb.code == 5) {
        return Ok(None);
    }
    GRAY_MEAN.with(|g| g.get()).map(Some).ok_or_else(|| "Kunstlicht ab ISO 400: Bildmittel der Sensorkorrektur fehlt".to_string())
}

/// Calibration for the later steps of a run (with the image's automatic white balance, if any).
fn cal(m: &Meta, wb: &str) -> Result<calibration::Calibration, String> {
    calibration::calc(&calibration::Input { camf: m.camf, props: m.props, wb, adobe: m.adobe, auto_neutral: AUTO_NEUTRAL.get(), gray_mean: None })
}

/// The automatic white balance from the image runs (white balance "Auto" without the cached
/// calibration).
fn auto_from_image(k: &calibration::Calibration) -> bool {
    k.wb.code == 1 && !k.color.cached
}

/// Inputs of the automatic white balance: the codes after the input factor, the drift before flag
/// 64, and the calibration with the stored neutral point.
fn auto_wb_input<'a>(img: &'a [i32], w: usize, h: usize, d: &calibration::Data, m: &Meta, k: &calibration::Calibration, drift: &correction::Drift) -> auto_wb::Input<'a> {
    // saturation level (long exposure: raised with the number of frames, at most × 1.2)
    let mut sat = d.triple("SaturationLevel", [0.0; 3]).map(|v| v as i32);
    if k.iso.bulb {
        let f = (f64::from(k.iso.bulb_images.wrapping_sub(1) as i32)).sqrt() / 2.5;
        let f = if f <= 1.2 { f } else { 1.2 };
        sat = sat.map(|v| (f64::from(v) * f) as i32);
    }
    let mut pp = [0.0f32; 27];
    if let Some(v) = d.values("PostPolyMatrix").filter(|v| v.len() >= 27) {
        for i in 0..27 {
            pp[i] = v[i] as f32;
        }
    }
    if let Some(q) = k.bulb_squares {
        pp[0] = q[0];
        pp[10] = q[1];
        pp[20] = q[2];
    }
    let time = exposure_s(d, m, k);
    // brightness value: aperture and time from the header (else the camera data), plus the exposure
    // compensation, minus the speed value of the ISO factor
    let prop = |n: &str| m.props.get(n).and_then(|v| v.trim().parse::<f64>().ok());
    let aperture = prop("APERTURE").filter(|a| *a != 0.0).unwrap_or_else(|| f64::from(d.number("CaptureAperture")));
    let iso_factor = f64::from(k.iso.factor);
    let bv = ((aperture * aperture) / time).log10() * 3.322 + prop("EXPCOMP").unwrap_or(0.0) - ((iso_factor * 100.0) / 3.125).log10() / 2.0f64.log10();
    let flash = m.props.get("FLASH").is_some_and(|v| !v.trim().is_empty() && !v.trim().eq_ignore_ascii_case("OFF"));
    let mut limit = f64::from(d.number("MaxConstrainTime"));
    if limit == 0.0 {
        limit = 0.5;
    }
    let scatter = d.triple("FlareFactor", [0.0; 3]);
    let mean = correction::subsampling(img, w, h, 4);
    let offset: [f64; 3] = std::array::from_fn(|c| scatter[c] * mean[c] + (drift.top.mean[c] + drift.bottom.mean[c]) / 2.0);
    auto_wb::Input { image: img, w, h, saturation: sat, post_polynomial: pp, bv, flash, matrix: k.color.m_raw, iso_factor, offset, short: limit >= time }
}

/// Exposure time in seconds for the brightness value: the camera data (the SD9/SD10 store seconds,
/// the others microseconds), else the shutter time of the file's properties (`SHUTTER`, seconds; on
/// the SD10 sample equal to the camera data, while `EXPTIME` is a different number), else 1 s as in
/// the maker's software. The early SD9 files carry neither, so their brightness value is that of
/// 1 s (measured against the SD9 reference export: mean ΔE00 1.18 with `EXPTIME` → 0.52).
fn exposure_s(d: &calibration::Data, m: &Meta, k: &calibration::Calibration) -> f64 {
    let mut time = f64::from(d.number("CaptureShutter"));
    if !matches!(k.model.class, calibration::Camera::Sd9 | calibration::Camera::Sd10) {
        time /= 1_000_000.0;
    }
    if time == 0.0 {
        time = m.props.get("SHUTTER").and_then(|v| v.trim().parse::<f64>().ok()).filter(|t| t.is_finite()).unwrap_or(0.0);
    }
    if time == 0.0 {
        time = 1.0;
    }
    time
}

pub(crate) fn correction_new(img: &mut [i32], w: usize, h: usize, m: &Meta, wb: &str) -> Result<(), String> {
    GRAY_MEAN.with(|g| g.set(None));
    let d = calibration::Data { camf: m.camf };
    let e = |gray| calibration::Input { camf: m.camf, props: m.props, wb, adobe: m.adobe, auto_neutral: AUTO_NEUTRAL.get(), gray_mean: gray };
    AUTO_NEUTRAL.set(None);
    MONOCHROME.set(false);
    let k0 = calibration::calc(&e(None))?;
    MONOCHROME.set(k0.color.degenerate);
    let skip = k0.skip;
    let (pre, inp) = correction::prepare(img, w, h, &d, &k0, skip)?;
    let k0 = if auto_from_image(&k0) {
        // automatic white balance from the image; without a usable estimate the stored neutral point stays
        let new_e = auto_wb_input(img, w, h, &d, m, &k0, &pre.drift_raw);
        let x = if skip & 0x1000000 != 0 {
            // older form of the estimate
            let prop = |n: &str| m.props.get(n).and_then(|v| v.trim().parse::<f64>().ok());
            auto_wb_old::neutral_point(&auto_wb_old::Input {
                image: img,
                w,
                h,
                post_polynomial: new_e.post_polynomial,
                saturation: new_e.saturation,
                aperture: prop("APERTURE").filter(|a| *a != 0.0).unwrap_or_else(|| f64::from(d.number("CaptureAperture"))),
                time: exposure_s(&d, m, &k0),
                correction: prop("EXPCOMP").unwrap_or(0.0),
                matrix: new_e.matrix,
                iso_factor: new_e.iso_factor,
                drift: pre.drift_raw,
                short: new_e.short,
            })
        } else {
            auto_wb::neutral_point(&new_e)
        };
        if std::env::var_os("GRAINEO_CLASSIC_DUMP").is_some() {
            eprintln!("Image white balance: {x:?} (stored {:?}), brightness value {:.3}", k0.color.neutral, new_e.bv);
        }
        if x.iter().all(|v| v.is_finite() && *v != 0.0) {
            AUTO_NEUTRAL.set(Some(x));
            calibration::calc(&e(None))?
        } else {
            k0
        }
    } else {
        k0
    };
    let k = if k0.iso.high && k0.wb.code == 5 {
        GRAY_MEAN.with(|g| g.set(Some(inp.mean)));
        calibration::calc(&e(Some(inp.mean)))?
    } else {
        k0
    };
    // sensor correction after dcraw (see `dcraw`)
    dcraw::sensor(img, w, h, &d, &k, &pre.drift)
}

pub(crate) fn highlights_new(img: &mut [i32], w: usize, h: usize, m: &Meta, wb: &str) -> Result<(), String> {
    let d = calibration::Data { camf: m.camf };
    let k0 = cal(m, wb)?;
    // incandescent at high ISO: the gains are mixed with the grey mean of the sensor correction
    let k = match gray_mean(&k0)? {
        Some(g) => calibration::calc(&calibration::Input { camf: m.camf, props: m.props, wb, adobe: m.adobe, auto_neutral: AUTO_NEUTRAL.get(), gray_mean: Some(g) })?,
        None => k0,
    };
    // white level in codes = saturation level times the layer gains; nothing for a monochrome matrix
    if k.color.degenerate {
        return Ok(());
    }
    if std::env::var_os("GRAINEO_CLASSIC_DUMP").is_some() {
        let sat = d.triple("SaturationLevel", [0.0; 3]);
        let g = k.amplification.permanent;
        eprintln!("White level: {:?}", std::array::from_fn::<f64, 3, _>(|c| sat[c] * g[c]));
        eprintln!("Kappgrenzen: {:?}", dcraw::clip_limits(img));
    }
    // own fade into grey (see `dcraw::highlights_gray`)
    dcraw::highlights_gray(img, w, h, dcraw::LIGHT_GRAY);
    Ok(())
}

/// Defective pixels from the camera's list and, for the cameras whose data list it
/// (`RedSharpen`), the first-layer sharpening, both after dcraw.
pub(crate) fn repair(img: &mut [i32], w: usize, h: usize, m: &Meta) {
    dcraw::bad_pixel(img, w, h, m);
    if !MONOCHROME.get() && (calibration::Data { camf: m.camf }).number("RedSharpen") as i32 != 0 {
        dcraw::rot(img, w, h);
    }
}

/// Speckle removal; `lang` is true for a long exposure or bulb mode (old way per layer). Flag
/// 0x4000 of `SkipStepFlags` switches the step off.
pub(crate) fn despeckle(img: &mut [i32], w: usize, h: usize, m: &Meta, lang: bool) {
    let d = calibration::Data { camf: m.camf };
    if d.integer("SkipStepFlags") & 0x4000 != 0 {
        return;
    }
    // old way for long exposure mode or an exposure over the camera's limit (default 0.15 s)
    let lang = match calibration::model(m.props.get("CAMMODEL").unwrap_or("")) {
        Some(md) => {
            let mut limit = d.integer("DespeckleExpTimeThresh");
            if limit == 0 {
                limit = 150_000;
            }
            (d.integer("BulbFrameCount") as i32) > 1 || i64::from(calibration::exposure_us(&d, m.props, md.class)) > i64::from(limit as i32)
        }
        None => lang,
    };
    despeckle::run(img, w, h, if lang { despeckle::Variant::Old } else { despeckle::Variant::New });
}

#[cfg(test)]
pub(crate) mod check {
    use std::path::PathBuf;

    /// Folder of the Classic sample files: GRAINEO_CLASSIC_TEST_DATA, otherwise the local measurement
    /// folder relative to the project (not in the repository; tests skip when it is missing).
    pub fn classic_dir() -> PathBuf {
        std::env::var_os("GRAINEO_CLASSIC_TEST_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../arbeitsordner/spp-farbtest-2/x3f")))
    }

    /// Classic sample files of the measurement folder (empty when the folder is not there).
    pub fn classic_files() -> Vec<PathBuf> {
        let dir = classic_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
                ["dp1_", "dp1s", "dp2_", "dp2x", "dp2s", "sd15", "sd14", "sd10", "sd9_", "pb_sigma_dp", "pb_sigma_sd15", "ir_dp1", "archiv_dp1s", "pixls_raw_sigma_dp1"].iter().any(|k| n.contains(k))
                    && !n.contains("dp0q")
                    && !n.contains("dp1q")
                    && !n.contains("dp2q")
                    && !n.contains("dp3q")
            })
            .collect();
        v.sort();
        v
    }
}
