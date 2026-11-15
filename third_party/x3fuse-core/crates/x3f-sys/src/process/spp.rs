//! Merrill SPP export setup and conversion.

use super::*;
use crate::spp_denoise::params::{estimated_iso, NoiseParams, NoiseSliders, ISO_REFERENCE};
use crate::spp_denoise::pipeline::Pipeline;

#[cfg(test)]
mod image_tests;
#[cfg(test)]
mod prefilter_tests;

pub(super) fn validate_mode(
    encoding: x3f_color_encoding_t,
    cineon: bool,
    denoise: i32,
    backend: crate::DenoiseBackend,
    merrill: bool,
    resolution: &str,
) -> crate::Result<()> {
    if !merrill
        || resolution != "HI"
        || cineon
        || !matches!(
            encoding,
            x3f_color_encoding_e_SRGB | x3f_color_encoding_e_ARGB | x3f_color_encoding_e_PPRGB
        )
    {
        return Err(crate::Error::InvalidData(
            "SPP denoise requires a full-resolution Merrill color export",
        ));
    }
    if denoise != 10 {
        return Err(crate::Error::InvalidData(
            "SPP denoise requires full intensity and neutral noise sliders",
        ));
    }
    if backend == crate::DenoiseBackend::Gpu {
        return Err(crate::Error::Gpu(
            "SPP denoise requires the CPU backend".into(),
        ));
    }
    Ok(())
}

unsafe fn typed_scalar(x3f: *mut x3f_t, name: &CStr, kind: u32, default: f64) -> f64 {
    let entry = unsafe { crate::meta::camf_find_by_name(x3f, name.as_ptr()) };
    if entry.is_none_or(|e| e.matrix_type != kind || e.matrix_dim != 1 || e.matrix_elements != 1) {
        return default;
    }
    match kind {
        3 => {
            let mut value = default;
            unsafe {
                x3f_get_camf_float(x3f, name.as_ptr().cast_mut(), &mut value);
            }
            f64::from(value as f32)
        }
        0 => {
            let mut value = default as i32;
            unsafe {
                x3f_get_camf_signed(x3f, name.as_ptr().cast_mut(), &mut value);
            }
            f64::from(value)
        }
        _ => {
            let mut value = default as u32;
            unsafe {
                x3f_get_camf_unsigned(x3f, name.as_ptr().cast_mut(), &mut value);
            }
            f64::from(value)
        }
    }
}

unsafe fn scalar(x3f: *mut x3f_t, name: &CStr, default: i32) -> i32 {
    let Some(entry) = (unsafe { crate::meta::camf_find_by_name(x3f, name.as_ptr()) }) else {
        return default;
    };
    match entry.matrix_type {
        1 | 2 | 5 | 6 => unsafe {
            typed_scalar(x3f, name, entry.matrix_type, f64::from(default)) as u32 as i32
        },
        3 => unsafe { typed_scalar(x3f, name, 3, f64::from(default)) as i32 },
        _ => default,
    }
}

unsafe fn float_rows<const N: usize>(
    x3f: *mut x3f_t,
    name: &CStr,
    maximum: usize,
) -> crate::Result<Vec<[f32; N]>> {
    let entry = unsafe { crate::meta::camf_find_by_name(x3f, name.as_ptr()) };
    if entry.is_none_or(|e| e.matrix_type != 3) {
        return Ok(Vec::new());
    }
    let (mut rows, mut cols, mut data) = (0, 0, ptr::null_mut());
    if unsafe {
        x3f_get_camf_matrix_var(
            x3f,
            name.as_ptr().cast_mut(),
            &mut rows,
            &mut cols,
            ptr::null_mut(),
            matrix_type_t_M_FLOAT,
            &mut data,
        )
    } == 0
        || cols as usize != N
        || rows <= 0
        || rows as usize > maximum
    {
        return Err(crate::Error::InvalidData("invalid SPP calibration table"));
    }
    Ok(
        unsafe { std::slice::from_raw_parts(data.cast::<f64>(), rows as usize * N) }
            .chunks_exact(N)
            .map(|r| std::array::from_fn(|c| r[c] as f32))
            .collect(),
    )
}

unsafe fn estimate(x3f: *mut x3f_t, gain: [f64; 3]) -> crate::Result<f64> {
    let (mut rows, mut cols, mut hist) = (0, 0, ptr::null_mut());
    let entry = unsafe { crate::meta::camf_find_by_name(x3f, c"Histogram".as_ptr()) };
    if entry.is_none_or(|e| e.matrix_type != 5) {
        return Ok(0.);
    }
    if unsafe {
        x3f_get_camf_matrix_var(
            x3f,
            c"Histogram".as_ptr().cast_mut(),
            &mut rows,
            &mut cols,
            ptr::null_mut(),
            matrix_type_t_M_UINT,
            &mut hist,
        )
    } == 0
        || rows < 3
        || !(1..=65536).contains(&cols)
    {
        return Err(crate::Error::InvalidData("invalid SPP ISO histogram"));
    }
    let histogram: Vec<_> =
        unsafe { std::slice::from_raw_parts(hist.cast::<u32>(), cols as usize * 3) }
            .iter()
            .map(|&v| v as u8)
            .collect();
    let (mut length, mut percent) = (0, ptr::null_mut());
    let entry = unsafe { crate::meta::camf_find_by_name(x3f, c"HistPercent".as_ptr()) };
    if entry.is_none_or(|e| e.matrix_type != 3) {
        return Ok(0.);
    }
    if unsafe {
        x3f_get_camf_matrix_var(
            x3f,
            c"HistPercent".as_ptr().cast_mut(),
            &mut length,
            ptr::null_mut(),
            ptr::null_mut(),
            matrix_type_t_M_FLOAT,
            &mut percent,
        )
    } == 0
        || length != cols
    {
        return Err(crate::Error::InvalidData(
            "invalid SPP histogram percentiles",
        ));
    }
    let percentages: Vec<_> =
        unsafe { std::slice::from_raw_parts(percent.cast::<f64>(), length as usize) }
            .iter()
            .map(|&v| v as f32)
            .collect();
    let reference = unsafe { float_rows::<3>(x3f, c"EstimatedISOreference", 15) }?;
    let reference: Vec<_> = reference.iter().map(|r| r.map(f64::from)).collect();
    let reference = if reference.is_empty() {
        &ISO_REFERENCE[..]
    } else {
        &reference
    };
    let outliers = unsafe { scalar(x3f, c"EstimatedISOoutliers", 1) };
    let count = unsafe { scalar(x3f, c"EstimatedISOoutliersNum", 3) };
    if count < 0 {
        return Err(crate::Error::InvalidData("invalid SPP ISO outlier count"));
    }
    estimated_iso(
        &histogram,
        &percentages,
        reference,
        gain,
        unsafe { typed_scalar(x3f, c"HistBitDepth", 6, 0.) } as u32,
        unsafe { scalar(x3f, c"EstimatedISOWBGains", 1) } != 0,
        (outliers != 0).then_some(count as usize),
        unsafe { scalar(x3f, c"EstimatedISOsnap", 0) } != 0,
    )
}

unsafe fn noise_params(
    x3f: *mut x3f_t,
    gain: [f64; 3],
    exif: &crate::jpeg_exif::JpegExif,
    sliders_position: NoiseSliders,
) -> crate::Result<NoiseParams> {
    let float = |name, default| unsafe { typed_scalar(x3f, name, 3, default) as f32 };
    let int = |name, default| {
        let value = unsafe { typed_scalar(x3f, name, 0, f64::from(default)) };
        if !(0. ..=f64::from(u32::MAX)).contains(&value) {
            Err(crate::Error::InvalidData(
                "invalid SPP integer noise setting",
            ))
        } else {
            Ok(value as u32)
        }
    };
    let mut capture = float(c"CaptureISO", 0.);
    if capture == 0. {
        let mut property = ptr::null_mut();
        if unsafe { x3f_get_prop_entry(x3f, c"ISO".as_ptr().cast_mut(), &mut property) } != 0
            && !property.is_null()
        {
            capture = unsafe { CStr::from_ptr(property) }
                .to_str()
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0) as f32;
        }
    }
    let sensor = float(c"SensorISO", 0.);
    let sensor = if sensor == 0. { 100. } else { sensor };
    let iso_range = unsafe { typed_scalar(x3f, c"ISOrange", 6, 0.) } != 0.;
    let adjusted_capture = if iso_range {
        match capture {
            800. => 666.,
            1600. => 1200.,
            3200. => 2133.,
            6400. => 4266.,
            _ => capture,
        }
    } else {
        capture
    };
    let iso_gain = adjusted_capture / sensor;
    if !iso_gain.is_finite() || iso_gain <= 0. {
        return Err(crate::Error::InvalidData("invalid SPP capture ISO"));
    }
    let mut estimated = unsafe { estimate(x3f, gain) }? as f32;
    if estimated == 0. {
        estimated = adjusted_capture;
    }
    let (mut count, mut ambient) = (0, ptr::null_mut());
    let entry = unsafe { crate::meta::camf_find_by_name(x3f, c"AESensorValueAmbient".as_ptr()) };
    let mean = if entry.is_none_or(|e| e.matrix_type != 0) {
        120.
    } else {
        if unsafe {
            x3f_get_camf_matrix_var(
                x3f,
                c"AESensorValueAmbient".as_ptr().cast_mut(),
                &mut count,
                ptr::null_mut(),
                ptr::null_mut(),
                matrix_type_t_M_INT,
                &mut ambient,
            )
        } == 0
            || !(1..=65536).contains(&count)
        {
            return Err(crate::Error::InvalidData(
                "invalid SPP ambient exposure calibration",
            ));
        }
        let sum = unsafe { std::slice::from_raw_parts(ambient.cast::<i32>(), count as usize) }
            .iter()
            .fold(0_i32, |sum, &v| sum.wrapping_add(v));
        f64::from(sum) / f64::from(count)
    };
    let actual_ev = (mean - 48.) * 0.125 + 5.;
    let rational = |r: Option<(u32, u32)>| {
        r.filter(|(n, d)| *n != 0 && *d != 0)
            .map_or(1., |(n, d)| f64::from(n) / f64::from(d))
    };
    let f_number = rational(exif.f_number);
    let shutter = rational(exif.exposure_time);
    let reference_ev =
        ((f_number * f_number / shutter).ln() - f64::from(iso_gain.ln())) / std::f64::consts::LN_2;
    let reference_ev = if reference_ev.is_finite() {
        reference_ev
    } else {
        0.
    };
    let exp_estimate = f64::from(float(c"ExpEstimate", 0.));
    let exp_net = f64::from(float(c"ExpNet", 0.));
    let difference = reference_ev - actual_ev;
    let exposure = if actual_ev < f64::from(float(c"EstimatedISOFlashAmbient", 8.))
        && difference > f64::from(float(c"EstimatedISODiffEV", 3.))
    {
        exp_estimate + exp_net + difference.min(3.)
    } else if reference_ev < 0. {
        exp_estimate - (actual_ev - reference_ev) + exp_net
    } else {
        exp_estimate + exp_net
    };
    let adjusted = adjusted_capture / exposure.exp2() as f32;
    if std::env::var_os("X3F_SPP_PARAMS").is_some() {
        eprintln!(
            "SPP ISO: estimate {estimated} adjusted {adjusted} capture {capture} ref_ev {reference_ev} actual_ev {actual_ev} exposure {exposure}"
        );
    }
    if estimated < adjusted {
        estimated = adjusted;
    }
    let iso_rows = unsafe { float_rows::<13>(x3f, c"ISONoiseSettings", 19) }?;
    let additional = unsafe { float_rows::<13>(x3f, c"AdditionalISONoiseSettings", 19) }?;
    let sliders = unsafe { float_rows::<13>(x3f, c"DenoisingSlidersStep", 5) }?;
    let mut params = NoiseParams {
        iso_gain,
        subsample: int(c"SubSampleKernel", 1)? != 0,
        edge_bias: float(c"EdgeMapLinShift", 10.),
        smooth_edge: int(c"EdgeMapDoSmoothEdge", 1)? != 0,
        accumulate: int(c"EdgeMapAccumulate", 0)? != 0,
        local_mean_iters: int(c"LocalMeanIters", 1)?,
        aniso_iters: int(c"ResidualAnisoDiffIters2", 1)?,
        aniso_k: int(c"ResidualAnisoDiffK2", 2)?,
        lambda: float(c"AnisoLambda", 0.25),
        aniso_noise_scale: float(c"ResidualAnisoDiffScaleNM", 1.),
        epsilon: float(c"ResidualEpsilon", 5.),
        perm_half: unsafe { scalar(x3f, c"PermSizeHalf", 4) } as u32,
        sharpen_limit: float(c"SharpenLimit", 0.25),
        fpn_suppression: unsafe { typed_scalar(x3f, c"FPNSuppression", 6, 0.) } as u32,
        ..NoiseParams::default()
    };
    let color = unsafe { float_rows::<3>(x3f, c"ColorNoiseModel", 3) }?;
    if !color.is_empty() {
        if color.len() != 3 {
            return Err(crate::Error::InvalidData("invalid SPP color noise model"));
        }
        params.color_model = std::array::from_fn(|r| color[r].map(f64::from));
    }
    params.setup(
        estimated,
        capture,
        &iso_rows,
        &additional,
        &sliders,
        sliders_position,
    )?;
    // The native parameter setup scales only the color model, after the intensity model is set.
    for (c, &g) in gain.iter().enumerate() {
        params.color_model[0][c] *= f64::from((g * g) as f32);
        params.color_model[1][c] *= f64::from(g as f32);
    }
    Ok(params)
}

fn compose_color_matrix(
    cc: [f64; 9],
    iso_gain: f32,
    compensation: [f64; 9],
) -> crate::Result<[[f64; 3]; 3]> {
    if !iso_gain.is_finite()
        || iso_gain <= 0.
        || !cc.iter().chain(&compensation).all(|v| v.is_finite())
    {
        return Err(crate::Error::InvalidData("invalid SPP color matrix"));
    }
    let matrix = cc.map(|v| v * f64::from(iso_gain));
    let result = std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            if compensation[0] == 0. {
                matrix[3 * r + c]
            } else {
                matrix[3 * r + 2].mul_add(
                    compensation[6 + c],
                    matrix[3 * r + 1].mul_add(
                        compensation[3 + c],
                        matrix[3 * r].mul_add(compensation[c], 0.),
                    ),
                )
            }
        })
    });
    if !result.iter().flatten().all(|v| v.is_finite()) {
        return Err(crate::Error::InvalidData(
            "invalid SPP composed color matrix",
        ));
    }
    Ok(result)
}

unsafe fn color_matrix(
    x3f: *mut x3f_t,
    wb: *mut libc::c_char,
    iso_gain: f32,
) -> crate::Result<[[f64; 3]; 3]> {
    let mode = merrill_render_color_mode(&unsafe { (*x3f).header.color_mode.map(|b| b as u8) })
        .ok_or(crate::Error::InvalidData(
            "SPP monochrome color modes are unsupported",
        ))?;
    let mut cc = [0.; 9];
    let mut found = unsafe {
        x3f_get_camf_matrix_for_wb(
            x3f,
            c"WhiteBalanceColorCorrections".as_ptr().cast_mut(),
            wb,
            3,
            3,
            cc.as_mut_ptr(),
        )
    };
    if found == 0 && !wb.is_null() && unsafe { CStr::from_ptr(wb) } == c"Sunlight" {
        found = unsafe {
            x3f_get_camf_matrix_for_wb(
                x3f,
                c"WhiteBalanceColorCorrections".as_ptr().cast_mut(),
                c"Daylight".as_ptr().cast_mut(),
                3,
                3,
                cc.as_mut_ptr(),
            )
        };
    }
    if found == 0 {
        return Err(crate::Error::InvalidData(
            "missing SPP white balance color matrix",
        ));
    }
    let mut compensation = [0.; 9];
    let (mut names, mut values, mut count) = (ptr::null_mut(), ptr::null_mut(), 0);
    unsafe {
        x3f_get_camf_property_list(
            x3f,
            c"ColorModeCompensations".as_ptr().cast_mut(),
            &mut names,
            &mut values,
            &mut count,
        );
    }
    unsafe {
        x3f_get_camf_matrix_for_wb(
            x3f,
            c"ColorModeCompensations".as_ptr().cast_mut(),
            mode.as_ptr().cast_mut(),
            3,
            3,
            compensation.as_mut_ptr(),
        );
    }
    if count != 0 && mode == c"FCBlue" && compensation[0] == 0. {
        // Native Stage2 supplies model-specific FCBlue matrices when CAMF has none.
        let bits = if unsafe { merrill_count_scale(x3f) } == Some(2.) {
            [
                0x3ff8d42c3c9eecc0,
                0x3fd46edaa92e6213,
                0xbfebdfce3150dae4,
                0x3fd69d2bf5515055,
                0x3ff6160956c0d6f5,
                0xbfe77aa8a82a5615,
                0x3fc08ad688377186,
                0x3fb73b13d244dbd2,
                0x3fe8f5e84f09528f,
            ]
        } else {
            [
                0x3ff9b97785729b28,
                0x3fd2a44417870542,
                0xbfecc5197a24894c,
                0x3fd8915bdd76683c,
                0x3ff5d78811b1d92b,
                0xbfe7f7b9e060fe48,
                0x3fc2fbe33acd5b68,
                0x3fb4ca0c282c6ef4,
                0x3fe8a7c7c5260f5e,
            ]
        };
        compensation = bits.map(f64::from_bits);
    }
    compose_color_matrix(cc, iso_gain, compensation)
}

unsafe fn selected_color_matrix(
    x3f: *mut x3f_t,
    wb: *mut libc::c_char,
    iso_gain: f32,
    mode: Option<&crate::ColorModeParameters>,
) -> crate::Result<[[f64; 3]; 3]> {
    let Some(mode) = mode else {
        return unsafe { color_matrix(x3f, wb, iso_gain) };
    };
    let (cc, _, _) =
        unsafe { color_mode_base_calibration(x3f, wb, crate::ColorModeSensor::Merrill) }?;
    compose_color_matrix(cc, iso_gain, mode.creative_matrix.unwrap_or([0.0; 9]))
}

/// Inverse of a 3×3 matrix (None if singular).
fn invert3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    let adj = [
        [c(1, 2, 1, 2), -c(0, 2, 1, 2), c(0, 1, 1, 2)],
        [-c(1, 2, 0, 2), c(0, 2, 0, 2), -c(0, 1, 0, 2)],
        [c(1, 2, 0, 1), -c(0, 2, 0, 1), c(0, 1, 0, 1)],
    ];
    Some(adj.map(|r| r.map(|v| v / det)))
}

fn active_in_keep(active: [i32; 4], keep: [i32; 4], size: [usize; 2]) -> crate::Result<[usize; 4]> {
    let invalid = || crate::Error::InvalidData("invalid SPP image rectangles");
    if (0..2).any(|i| i64::from(keep[i + 2]) - i64::from(keep[i]) != size[i] as i64) {
        return Err(invalid());
    }
    let mut result = [0; 4];
    for i in 0..4 {
        result[i] = usize::try_from(i64::from(active[i]) - i64::from(keep[i % 2]))
            .map_err(|_| invalid())?;
    }
    if result[0] >= result[2]
        || result[1] >= result[3]
        || result[2] > size[0]
        || result[3] > size[1]
    {
        return Err(invalid());
    }
    Ok(result)
}

fn zeroed<T: Default + Clone>(length: usize) -> crate::Result<Vec<T>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(length)
        .map_err(|_| crate::Error::Allocation)?;
    result.resize(length, T::default());
    Ok(result)
}

fn sub_size(size: [usize; 2], scale: f32) -> [usize; 2] {
    size.map(|n| ((f64::from(n as f32) * f64::from(scale)).ceil() as usize + 1) & !1)
}

fn prefilter(
    source: &[f32],
    source_stride: usize,
    size: [usize; 2],
    noise: &NoiseParams,
    pipeline: &Pipeline,
    control: Control<'_>,
) -> crate::Result<Vec<f32>> {
    use crate::spp_denoise::pixmap::{
        bilateral_filter_rgb_image, local_mean, BilateralImageParams,
    };
    use crate::spp_denoise::{compute_edge_map, compute_resample_params, resample_image};
    let stride = size[0].checked_mul(3).ok_or(crate::Error::Allocation)?;
    let samples = stride
        .checked_mul(size[1])
        .ok_or(crate::Error::Allocation)?;
    let mut down = zeroed(samples)?;
    let mut filtered = zeroed(samples)?;
    control.check()?;
    resample_image(
        source,
        source_stride,
        &mut down,
        size,
        stride,
        &compute_resample_params(noise.scale),
    )?;
    control.check()?;
    bilateral_filter_rgb_image(
        &down,
        stride,
        &mut filtered,
        stride,
        size,
        &BilateralImageParams {
            filter_size: noise.filter_size,
            noise_model: noise.color_model,
            subsample: noise.subsample,
            exp_lut: pipeline.exp_lut(),
            lut_scale: pipeline.exp_lut_scale(),
            rows: [0, size[1] as u32 - 1],
        },
        control,
    )?;
    // graineo addition: colour slider step 0/1 (tiled path, untiled prepass).
    if noise.alpha != 1. {
        crate::spp_denoise::pipeline::alpha_mix(
            &mut filtered,
            stride,
            &down,
            stride,
            size,
            noise.alpha,
        );
    }
    let mut edge = zeroed(size[0] * size[1])?;
    compute_edge_map(
        &filtered,
        stride,
        &mut edge,
        size,
        size[0],
        &pipeline.edge_params(),
    )?;
    pipeline.remap_edge_map(&mut edge, size[0], size, 0)?;
    control.check()?;
    down.copy_from_slice(&filtered);
    local_mean(
        &down,
        stride,
        &edge,
        size[0],
        size,
        4,
        &mut filtered,
        stride,
    )?;
    control.check()?;
    Ok(filtered)
}

fn denoise_image(
    data: &mut [f32],
    stride: usize,
    size: [usize; 2],
    active: [usize; 4],
    noise: &NoiseParams,
    pipeline: &Pipeline,
    control: Control<'_>,
) -> crate::Result<Vec<i16>> {
    use crate::spp_denoise::pipeline::TileInput;
    use crate::spp_denoise::tiling::{copy_image_to_tile, TileLayout};
    let timing = crate::spp_denoise::prof::enabled();
    let clock = std::time::Instant::now();
    let mut mark = clock;
    let mut step = |name: &str| {
        if timing {
            let now = std::time::Instant::now();
            eprintln!("SPP   {name}: {:.1} ms", (now - mark).as_secs_f64() * 1e3);
            mark = now;
        }
    };
    let layout = TileLayout::new(size, [8, 8], [40, 40])?;
    let reduced_size = sub_size(size, noise.scale);
    let filtered = if f64::from(noise.scale) <= 0.54 {
        Some(prefilter(
            data,
            stride,
            reduced_size,
            noise,
            pipeline,
            control,
        )?)
    } else {
        None
    };
    control.check()?;
    step("prefilter");
    let mut tiles = layout.initialize_parallel(data, stride)?;
    step("set up tiles");
    // Timeline per tile (start/end relative to `clock`) for the utilisation analysis.
    let spans: Vec<std::sync::Mutex<(f64, f64)>> = (0..layout.grid[0] * layout.grid[1])
        .map(|_| std::sync::Mutex::new((0.0, 0.0)))
        .collect();
    let sub_tile = sub_size(layout.tile, noise.scale);
    let halo = layout
        .halo
        .map(|h| (h as f64 * f64::from(noise.scale)) as usize);
    let process_tile = |(index, tile): (usize, &mut [f32])| -> crate::Result<Vec<i16>> {
        control.check()?;
        let begin = clock.elapsed().as_secs_f64();
        let tile_row = index / layout.grid[0];
        let first = if tile_row == 0 { active[1] } else { 0 };
        let last = if tile_row + 1 == layout.grid[1] {
            layout.tile[1]
                .checked_sub(size[1] - active[3])
                .ok_or(crate::Error::InvalidData("invalid SPP active tile rows"))?
        } else {
            layout.tile[1]
        };
        let extracted;
        let input = if let Some(filtered) = &filtered {
            let mut tile = zeroed(sub_tile[0] * sub_tile[1] * 3)?;
            copy_image_to_tile(
                filtered,
                reduced_size[0] * 3,
                reduced_size,
                layout.grid,
                halo,
                index,
                &mut tile,
                sub_tile[0] * 3,
                sub_tile,
            )?;
            extracted = tile;
            TileInput::Prefiltered {
                rgb: &extracted,
                stride: sub_tile[0] * 3,
            }
        } else {
            TileInput::Ordinary
        };
        // graineo: position of the tile in the image (for the CA correction as in SPP)
        let origin = [
            (index % layout.grid[0] * layout.core[0]) as isize - layout.halo[0] as isize,
            (index / layout.grid[0] * layout.core[1]) as isize - layout.halo[1] as isize,
        ];
        let mono = pipeline.process_tile(
            tile,
            layout.tile[0] * 3,
            layout.tile,
            first..last,
            input,
            control,
            origin,
        )?;
        let mut sharp: Vec<i16> = zeroed(layout.core[0] * layout.core[1])?;
        for y in 0..layout.core[1] {
            for x in 0..layout.core[0] {
                let value = mono[(y + layout.halo[1]) * layout.tile[0] + x + layout.halo[0]];
                let value = if value > 8191. { 8191. } else { value };
                sharp[y * layout.core[0] + x] = f64::from(value).mul_add(4., 0.5) as i64 as i16;
            }
        }
        if timing {
            *spans[index].lock().unwrap() = (begin, clock.elapsed().as_secs_f64());
        }
        Ok(sharp)
    };
    #[cfg(not(target_arch = "wasm32"))]
    let sharp_tiles: Vec<_> = {
        use rayon::prelude::*;
        tiles
            .par_chunks_mut(layout.samples)
            .enumerate()
            .map(process_tile)
            .collect::<crate::Result<_>>()?
    };
    #[cfg(target_arch = "wasm32")]
    let sharp_tiles: Vec<_> = tiles
        .chunks_mut(layout.samples)
        .enumerate()
        .map(process_tile)
        .collect::<crate::Result<_>>()?;
    control.check()?;
    step("process tiles");
    if timing {
        let spans: Vec<(f64, f64)> = spans.iter().map(|s| *s.lock().unwrap()).collect();
        let first = spans.iter().map(|s| s.0).fold(f64::MAX, f64::min);
        let last = spans.iter().map(|s| s.1).fold(0.0, f64::max);
        let busy: f64 = spans.iter().map(|s| s.1 - s.0).sum();
        let threads = rayon::current_num_threads();
        let (min, max) = spans
            .iter()
            .map(|s| s.1 - s.0)
            .fold((f64::MAX, 0.0_f64), |a, d| (a.0.min(d), a.1.max(d)));
        eprintln!(
            "SPP   tiles: {} pieces, {:.0}–{:.0} ms each, total {:.0} ms on {threads} threads, span {:.0} ms → utilisation {:.0} %",
            spans.len(), min * 1e3, max * 1e3, busy * 1e3, (last - first) * 1e3,
            100.0 * busy / ((last - first) * threads as f64)
        );
        crate::spp_denoise::prof::report();
    }
    layout.blend(&tiles, data, stride, 4)?;
    step("blend");
    drop(tiles);
    drop(filtered);
    let mut sharp = zeroed(size[0] * size[1])?;
    for (index, tile) in sharp_tiles.into_iter().enumerate() {
        for y in 0..layout.core[1] {
            let from = y * layout.core[0];
            let to = (index / layout.grid[0] * layout.core[1] + y) * size[0]
                + index % layout.grid[0] * layout.core[0];
            sharp[to..to + layout.core[0]].copy_from_slice(&tile[from..from + layout.core[0]]);
        }
    }
    step("assemble sharpening image");
    crate::spp_denoise::residual::sharpen(
        data,
        stride,
        &sharp,
        size[0],
        size,
        f64::from(noise.sharpen_amount),
        control,
    )?;
    step("sharpen");
    // graineo change: the detail image "L" (×4 fixed point) is returned
    // so that the SPP sharpness slider can sharpen later without recomputation.
    control.check()?;
    Ok(sharp)
}

/// graineo addition: was the last SPP noise reduction computed on the GPU?
pub(crate) static LAST_USED_GPU: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// graineo addition: `X3F_SPP_GPU=1` selects the GPU noise reduction.
pub(crate) fn gpu_requested_by_env() -> bool {
    std::env::var("X3F_SPP_GPU").is_ok_and(|v| v != "0" && !v.is_empty())
}

/// graineo addition: noise reduction on the GPU (if requested, compiled in
/// and available) or the CPU. If the GPU fails (no adapter, unsupported
/// parameters, driver error), the CPU computes with unchanged `data`.
#[allow(clippy::too_many_arguments)]
fn denoise_dispatch(
    data: &mut [f32],
    stride: usize,
    size: [usize; 2],
    active: [usize; 4],
    noise: &NoiseParams,
    pipeline: &Pipeline,
    control: Control<'_>,
    gpu: bool,
) -> crate::Result<Vec<i16>> {
    use std::sync::atomic::Ordering;
    LAST_USED_GPU.store(false, Ordering::Relaxed);
    if gpu {
        #[cfg(all(feature = "spp-gpu", not(target_arch = "wasm32")))]
        match crate::spp_denoise::gpu::denoise_image(
            data, stride, size, active, noise, pipeline, control,
        ) {
            Ok(sharp) => {
                LAST_USED_GPU.store(true, Ordering::Relaxed);
                return Ok(sharp);
            }
            Err(crate::Error::Cancelled) => return Err(crate::Error::Cancelled),
            Err(error) => eprintln!("SPP GPU not usable, computing on the CPU: {error}"),
        }
        #[cfg(not(all(feature = "spp-gpu", not(target_arch = "wasm32"))))]
        eprintln!("SPP GPU not compiled in (feature `spp-gpu`), computing on the CPU");
    }
    denoise_image(data, stride, size, active, noise, pipeline, control)
}

#[allow(clippy::too_many_arguments)]
/// graineo addition (not part of x3fuse-core): returns the SPP working image
/// after preprocessing, noise reduction, colour matrix and ColorDQ, i.e. immediately
/// before tone curve and output matrix, as RGB counts (f32), cropped to the active
/// area. Same flow as [`render`] up to and including `denoise_image`.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn working(
    x3f: *mut x3f_t,
    wb: *mut libc::c_char,
    apply_sgain: bool,
    fix_bad: bool,
    control: Control<'_>,
    mode: Option<&crate::ColorModeParameters>,
    gpu: bool,
    sliders: NoiseSliders,
    ca: Option<&crate::spp_denoise::ca::CaCorrection>,
) -> crate::Result<SppWorking> {
    control.check()?;
    let t_setup = std::time::Instant::now();
    let gain = unsafe { merrill_request_gain(x3f, wb, control) }
        .ok_or(crate::Error::InvalidData("unsupported SPP request gains"))?;
    let exif = unsafe { merrill_exif(x3f, control) }?;
    let mut noise = unsafe { noise_params(x3f, gain, &exif, sliders) }?;
    // Only for tests of rarely active paths (CPU and GPU alike):
    // X3F_SPP_TEST=rcps=0.5,rcpsthr=0.2,colormix=0.6,scale=0.5
    if let Ok(spec) = std::env::var("X3F_SPP_TEST") {
        for item in spec.split(',') {
            match item.split_once('=').map(|(k, v)| (k, v.parse::<f32>())) {
                Some(("rcps", Ok(v))) => noise.rcps_blending = v,
                Some(("colormix", Ok(v))) => noise.color_mixing = v,
                Some(("scale", Ok(v))) => noise.scale = v,
                Some(("alpha", Ok(v))) => noise.alpha = v,
                Some(("decay", Ok(v))) => noise.decay = v,
                Some(("aniso", Ok(v))) => noise.aniso_noise_scale = v,
                Some(("mixing", Ok(v))) => noise.mixing = v,
                Some(("strength", Ok(v))) => noise.residual_strength = v,
                Some(("ns", Ok(v))) => noise.noise_scaling = v,
                Some(("intens", Ok(v))) => {
                    noise.intensity_model = noise.intensity_model.map(|x| x * v)
                }
                // Sharpening amount (for checking the sharpness slider)
                Some(("sharpen", Ok(v))) => noise.sharpen_amount = v,
                // Shift ε by `v` ULP (sensitivity test, CPU against CPU).
                Some(("nudge", Ok(v))) => {
                    for _ in 0..v as u32 {
                        noise.epsilon = f32::from_bits(noise.epsilon.to_bits() + 1);
                    }
                }
                Some(("rcpsthr", Ok(v))) => {
                    noise.rcps_threshold = v;
                    noise.rcps_map_threshold = v;
                }
                _ => eprintln!("X3F_SPP_TEST: unknown entry {item}"),
            }
        }
    }
    let matrix = unsafe { selected_color_matrix(x3f, wb, noise.iso_gain, mode) }?;
    let sensor = unsafe { typed_scalar(x3f, c"SensorISO", 3, 0.) } as f32;
    let sensor_scale = if sensor == 0. { 1. } else { sensor / 100. };
    let mut strengths = [0.; 3];
    if unsafe {
        x3f_get_camf_float_vector(
            x3f,
            c"ColorDQCamRGB".as_ptr().cast_mut(),
            strengths.as_mut_ptr(),
        )
    } == 0
    {
        return Err(crate::Error::InvalidData("missing SPP ColorDQ calibration"));
    }
    let mut skip = 0;
    unsafe {
        x3f_get_camf_unsigned(x3f, c"SkipStepFlags".as_ptr().cast_mut(), &mut skip);
    }
    // graineo addition: FPNSuppression only affects the result via
    // `(FPN | 4) == 7` (dark-frame despeckling); other values
    // (e.g. 2 with early DP firmware) compute like 0, matching SPP's output.
    if !denoise_mode_supported(skip, noise.fpn_suppression) {
        return Err(crate::Error::InvalidData(
            "unsupported SPP denoise bypass or FPN mode",
        ));
    }
    let dq_scale = f64::from(noise.color_dq_scale.min(noise.iso_gain));
    let pipeline = Pipeline::new(
        noise.clone(),
        matrix,
        strengths.map(|v| v * dq_scale),
        sensor_scale,
        gain.map(|v| v as f32),
    )?;
    let mut area = x3f_area16_t::default();
    if unsafe { x3f_image_area(x3f, &mut area) } == 0 {
        return Err(crate::Error::InvalidData("missing SPP image"));
    }
    let mut pipeline = pipeline;
    let size = [area.columns as usize, area.rows as usize];
    let (active, keep, _) = unsafe { merrill_rects(x3f) }?;
    // graineo addition: CA correction as in SPP (prepared on ActiveImageArea); the GPU
    // version shifts the same tiles (`ca_shift`).
    if let Some(ca) = ca {
        pipeline.set_ca(crate::spp_denoise::ca::CaTransform::prepare(ca, active));
    }
    let active = active_in_keep(active, keep, size)?;
    let timing = std::env::var_os("X3F_SPP_TIMING").is_some();
    let t0 = std::time::Instant::now();
    if timing {
        eprintln!(
            "SPP setup (noise parameters, matrices): {:.0?}",
            t_setup.elapsed()
        );
    }
    let mut last = t0;
    let mut observe = |stage: &'static str, _: &[f32], _: String| {
        if timing {
            eprintln!("SPP {stage}: {:.0?}", last.elapsed());
            last = std::time::Instant::now();
        }
    };
    // graineo: on the GPU path the patch fits of the offset correction run on the GPU as well
    let mut prepared = crate::cr_offset::with_gpu(gpu, || unsafe {
        crate::process::merrill_prepare_float(
            x3f,
            wb,
            apply_sgain,
            fix_bad,
            Some(noise.despeckle),
            control,
            &mut observe,
        )
    })?;
    if std::env::var_os("X3F_SPP_PARAMS").is_some() {
        eprintln!("SPP noise parameters: {noise:?}");
    }
    let t1 = std::time::Instant::now();
    let sharp = denoise_dispatch(
        &mut prepared.data,
        prepared.row_stride,
        size,
        active,
        &noise,
        &pipeline,
        control,
        gpu,
    )?;
    if timing {
        eprintln!(
            "SPP pre-stages total {:.0?} · noise reduction {:.0?}",
            t1 - t0,
            t1.elapsed()
        );
    }
    let (width, height) = (active[2] - active[0], active[3] - active[1]);
    let mut out = Vec::with_capacity(width * height * 3);
    let mut sharp_l = Vec::with_capacity(width * height);
    for y in 0..height {
        let from = (active[1] + y) * prepared.row_stride + active[0] * 3;
        out.extend_from_slice(&prepared.data[from..from + width * 3]);
        let from = (active[1] + y) * size[0] + active[0];
        sharp_l.extend_from_slice(&sharp[from..from + width]);
    }
    Ok(SppWorking {
        data: out,
        width,
        height,
        sharp_l,
        sharpen_base: noise.sharpen_amount,
    })
}

/// graineo addition: preprocessed image of the SPP pipeline (black level, gains, white balance,
/// defects, highlights, despeckle) before any noise reduction, as camera-space counts (f32 RGB,
/// interleaved) of the active area. Input of the monochrome path, which mixes the layers before
/// filtering (see `develop::mono`).
pub struct SppPrepared {
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
    /// ISO factor CaptureISO/SensorISO of the noise parameters
    pub iso_gain: f32,
    /// Estimated ISO used for the noise parameters
    pub iso: f32,
    /// Noise scaling of the luminance filter (noise parameter, incl. the luminance slider step)
    pub noise_scaling: f32,
}

/// graineo addition: [`working`] up to and including the preprocessing, see [`SppPrepared`].
pub(crate) unsafe fn prepared(
    x3f: *mut x3f_t,
    wb: *mut libc::c_char,
    control: Control<'_>,
    sliders: NoiseSliders,
) -> crate::Result<SppPrepared> {
    control.check()?;
    let gain = unsafe { merrill_request_gain(x3f, wb, control) }
        .ok_or(crate::Error::InvalidData("unsupported SPP request gains"))?;
    let exif = unsafe { merrill_exif(x3f, control) }?;
    let noise = unsafe { noise_params(x3f, gain, &exif, sliders) }?;
    let mut area = x3f_area16_t::default();
    if unsafe { x3f_image_area(x3f, &mut area) } == 0 {
        return Err(crate::Error::InvalidData("missing SPP image"));
    }
    let size = [area.columns as usize, area.rows as usize];
    let (active, keep, _) = unsafe { merrill_rects(x3f) }?;
    let active = active_in_keep(active, keep, size)?;
    let mut observe = |_: &'static str, _: &[f32], _: String| {};
    let prepared = unsafe {
        crate::process::merrill_prepare_float(
            x3f,
            wb,
            true,
            true,
            Some(noise.despeckle),
            control,
            &mut observe,
        )
    }?;
    let (width, height) = (active[2] - active[0], active[3] - active[1]);
    let mut data = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        let from = (active[1] + y) * prepared.row_stride + active[0] * 3;
        data.extend_from_slice(&prepared.data[from..from + width * 3]);
    }
    Ok(SppPrepared {
        data,
        width,
        height,
        iso_gain: noise.iso_gain,
        iso: noise.iso,
        noise_scaling: noise.noise_scaling,
    })
}

/// graineo addition: monochrome working image as in [`render_monochrome`] (Stage2
/// monochrome denoise and Stage2B sharpening at the base amount 1.25, all three channels
/// equal), before the tone curve, cropped to the active area. `mix` is the B/W mixer
/// (`m_ColorMixing`, SPP default 0.33/0.33/0.34); `sliders` scales the noise scaling of the
/// grey image filter by the ratio of the colour path's noise scaling to its neutral value
/// (neutral sliders: exactly as [`render_monochrome`]).
pub(crate) unsafe fn mono_working(
    x3f: *mut x3f_t,
    wb: *mut libc::c_char,
    control: Control<'_>,
    sliders: NoiseSliders,
    mix: [f64; 3],
) -> crate::Result<SppWorking> {
    use crate::spp_denoise::mono::{self, sharpen_bnw};
    control.check()?;
    let gain = unsafe { merrill_request_gain(x3f, wb, control) }
        .ok_or(crate::Error::InvalidData("unsupported SPP request gains"))?;
    let exif = unsafe { merrill_exif(x3f, control) }?;
    let noise = unsafe { noise_params(x3f, gain, &exif, NoiseSliders::NEUTRAL) }?;
    let slider_ratio = if sliders == NoiseSliders::NEUTRAL {
        1.0
    } else {
        let moved = unsafe { noise_params(x3f, gain, &exif, sliders) }?;
        if noise.noise_scaling > 0.0 {
            moved.noise_scaling / noise.noise_scaling
        } else {
            0.0
        }
    };
    let mut skip = 0;
    unsafe {
        x3f_get_camf_unsigned(x3f, c"SkipStepFlags".as_ptr().cast_mut(), &mut skip);
    }
    if skip & (1 << 23) != 0 {
        return Err(crate::Error::InvalidData(
            "unsupported SPP denoise bypass or FPN mode",
        ));
    }
    let parts = unsafe { super::merrill_gain_parts(x3f, wb, control) }
        .ok_or(crate::Error::InvalidData("unsupported SPP request gains"))?;
    let scaling = mono_noise_scaling(parts.divide, noise.iso)?;
    let mono_noise = mono::MonoNoise {
        iso: noise.iso,
        noise_scaling: if slider_ratio == 1.0 { scaling } else { scaling * slider_ratio },
        iso_gain: noise.iso_gain,
        column_noise_scale: noise.column_noise_scale,
        bcs_filter_size: 27,
        bcs_scale_index: 2,
        color_mixing: mix,
        white_balance: mono_white_balance(&parts),
    };
    let mut area = x3f_area16_t::default();
    if unsafe { x3f_image_area(x3f, &mut area) } == 0 {
        return Err(crate::Error::InvalidData("missing SPP image"));
    }
    let size = [area.columns as usize, area.rows as usize];
    let (active, keep, _) = unsafe { merrill_rects(x3f) }?;
    let active = active_in_keep(active, keep, size)?;
    let mut observe = |_: &'static str, _: &[f32], _: String| {};
    let prepared = unsafe {
        crate::process::merrill_prepare_float(
            x3f,
            wb,
            true,
            true,
            Some(noise.despeckle),
            control,
            &mut observe,
        )
    }?;
    let stride = prepared.row_stride;
    let mut work = prepared.data.clone();
    let mut sharp: Vec<i16> = zeroed(size[0] * size[1])?;
    mono::monochrome_denoise(
        &mut work,
        stride,
        &prepared.data,
        stride,
        &mut sharp,
        size[0],
        size,
        &mono_noise,
        mono::MonoDenoiseArgs {
            gains: gain,
            noise_divisors: [1.0, 1.0],
            adjust_noise: false,
        },
        control,
    )?;
    drop(prepared);
    sharpen_bnw::apply_sharp_l_bnw(&mut work, stride, &sharp, size[0], size, 1.25)?;
    control.check()?;
    let (width, height) = (active[2] - active[0], active[3] - active[1]);
    let mut data = Vec::with_capacity(width * height * 3);
    let mut sharp_l = Vec::with_capacity(width * height);
    for y in 0..height {
        let from = (active[1] + y) * stride + active[0] * 3;
        data.extend_from_slice(&work[from..from + width * 3]);
        let from = (active[1] + y) * size[0] + active[0];
        sharp_l.extend_from_slice(&sharp[from..from + width]);
    }
    Ok(SppWorking {
        data,
        width,
        height,
        sharp_l,
        sharpen_base: 1.25,
    })
}

/// graineo addition: result of [`working`].
pub struct SppWorking {
    /// RGB counts (f32, interleaved) of the active area, already sharpened with the
    /// base sharpening (sharpness slider 0).
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
    /// Detail image "L" (high-pass of the luminance, fixed point ×4), same
    /// size as `data`, one value per pixel.
    pub sharp_l: Vec<i16>,
    /// Base of the sharpening amount (noise parameter `SharpenAmount`);
    /// `data` is sharpened with this amount.
    pub sharpen_base: f32,
}

pub(super) unsafe fn render(
    x3f: *mut x3f_t,
    image: *mut x3f_area16_t,
    levels: *mut x3f_image_levels_t,
    encoding: x3f_color_encoding_t,
    crop: bool,
    fix_bad: bool,
    apply_sgain: bool,
    wb: *mut libc::c_char,
    control: Control<'_>,
    mode: Option<&crate::ColorModeParameters>,
) -> crate::Result<()> {
    control.check()?;
    let gain = unsafe { merrill_request_gain(x3f, wb, control) }
        .ok_or(crate::Error::InvalidData("unsupported SPP request gains"))?;
    let exif = unsafe { merrill_exif(x3f, control) }?;
    let noise = unsafe { noise_params(x3f, gain, &exif, NoiseSliders::NEUTRAL) }?;
    if mode.is_none() && unsafe { header_monochrome(x3f) } {
        return unsafe {
            render_monochrome(
                x3f,
                image,
                levels,
                encoding,
                crop,
                fix_bad,
                apply_sgain,
                wb,
                control,
                gain,
                &exif,
                &noise,
            )
        };
    }
    let matrix = unsafe { selected_color_matrix(x3f, wb, noise.iso_gain, mode) }?;
    let sensor = unsafe { typed_scalar(x3f, c"SensorISO", 3, 0.) } as f32;
    let sensor_scale = if sensor == 0. { 1. } else { sensor / 100. };
    let mut strengths = [0.; 3];
    if unsafe {
        x3f_get_camf_float_vector(
            x3f,
            c"ColorDQCamRGB".as_ptr().cast_mut(),
            strengths.as_mut_ptr(),
        )
    } == 0
    {
        return Err(crate::Error::InvalidData("missing SPP ColorDQ calibration"));
    }
    let mut skip = 0;
    unsafe {
        x3f_get_camf_unsigned(x3f, c"SkipStepFlags".as_ptr().cast_mut(), &mut skip);
    }
    // graineo addition: FPNSuppression only affects the result via
    // `(FPN | 4) == 7` (dark-frame despeckling); other values
    // (e.g. 2 with early DP firmware) compute like 0, matching SPP's output.
    if !denoise_mode_supported(skip, noise.fpn_suppression) {
        return Err(crate::Error::InvalidData(
            "unsupported SPP denoise bypass or FPN mode",
        ));
    }
    let dq_scale = f64::from(noise.color_dq_scale.min(noise.iso_gain));
    let pipeline = Pipeline::new(
        noise.clone(),
        matrix,
        strengths.map(|v| v * dq_scale),
        sensor_scale,
        gain.map(|v| v as f32),
    )?;
    let luts = unsafe { make_color_mode_render_luts(x3f, encoding, &exif, true, mode, control) }?;
    let mut area = x3f_area16_t::default();
    if unsafe { x3f_image_area(x3f, &mut area) } == 0 {
        return Err(crate::Error::InvalidData("missing SPP image"));
    }
    let size = [area.columns as usize, area.rows as usize];
    let (active, keep, _) = unsafe { merrill_rects(x3f) }?;
    let active = active_in_keep(active, keep, size)?;
    let trace = std::env::var_os("X3F_FLOAT_FRONT_TRACE").is_some();
    let mut observe = |stage: &'static str, data: &[f32], detail: String| {
        if trace {
            eprintln!(
                "X3F float front: stage={stage} fnv1a={:016x} {detail}",
                crate::float_front::fnv1a_f32(data)
            );
        }
    };
    let mut prepared = unsafe {
        merrill_prepare_float(
            x3f,
            wb,
            apply_sgain,
            fix_bad,
            Some(noise.despeckle),
            control,
            &mut observe,
        )
    }?;
    drop(denoise_dispatch(
        &mut prepared.data,
        prepared.row_stride,
        size,
        active,
        &noise,
        &pipeline,
        control,
        gpu_requested_by_env(),
    )?);
    let rect = if crop {
        active
    } else {
        [0, 0, size[0], size[1]]
    };
    let width = rect[2] - rect[0];
    let height = rect[3] - rect[1];
    let stride = width.checked_mul(3).ok_or(crate::Error::Allocation)?;
    let samples = stride.checked_mul(height).ok_or(crate::Error::Allocation)?;
    let bytes = samples
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or(crate::Error::Allocation)?;
    let buffer = unsafe { libc::malloc(bytes) };
    if buffer.is_null() {
        return Err(crate::Error::Allocation);
    }
    // The controlled caller owns this allocation and frees it on any error.
    unsafe {
        *image = x3f_area16_t {
            data: buffer.cast(),
            buf: buffer,
            rows: height as u32,
            columns: width as u32,
            channels: 3,
            row_stride: stride as u32,
        };
    }
    let output = unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u16>(), samples) };
    for y in 0..height {
        control.check()?;
        let from = (rect[1] + y) * prepared.row_stride + rect[0] * 3;
        for (out, input) in output[y * stride..][..stride]
            .chunks_exact_mut(3)
            .zip(prepared.data[from..][..stride].chunks_exact(3))
        {
            out.copy_from_slice(&luts.render(input.try_into().unwrap()));
        }
    }
    if !levels.is_null() {
        unsafe {
            *levels = x3f_image_levels_t {
                black: [0.; 3],
                white: [65535; 3],
            };
        }
    }
    control.check()
}

/// Whether the header's color mode is "Monochrome" (as shot; Sepia stays
/// unsupported: its toning gains come from SPP and are unknown).
unsafe fn header_monochrome(x3f: *mut x3f_t) -> bool {
    let mode = unsafe { (*x3f).header.color_mode.map(|b| b as u8) };
    let end = mode.iter().position(|&b| b == 0).unwrap_or(mode.len());
    &mode[..end] == b"Monochrome"
}

/// The noise scaling in monochrome: the native noise-parameter setup
/// sets 1.0 at HI resolution, then scales it by the
/// estimated-ISO interpolation of column 2 (the neutral luminance slider,
/// a fixed-point conversion with 0.5 at 2 fractional bits) of a per-process
/// table: one for DP Merrill (process 2.1.x), one for SD1 Merrill (2.0.x),
/// with shared ISO knots.
/// Row 9 is past each 9-row table, as natively (above ISO 12800).
fn mono_noise_scaling(sd1: bool, iso: f32) -> crate::Result<f32> {
    const KNOTS: [u32; 10] = [
        0,
        0x42c8_0000,
        0x4348_0000,
        0x43c8_0000,
        0x4448_0000,
        0x44c8_0000,
        0x4548_0000,
        0x45c8_0000,
        0x4648_0000,
        0x476e_694d,
    ];
    const DP: [u64; 10] = [
        0x3fd3_3333_3333_3333,
        0x3fd3_3333_3333_3333,
        0x3fd3_3333_3333_3333,
        0x3fd3_3333_3333_3333,
        0x3fd3_3333_3333_3333,
        0x3fdc_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3ff0_0000_0000_0000,
        0x3ff8_0000_0000_0000,
        0x3fe4_cccc_cccc_cccd,
    ];
    const SD1: [u64; 10] = [
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3fe4_cccc_cccc_cccd,
        0x3ff0_0000_0000_0000,
        0x44c8_0000_4448_0000,
    ];
    if !(iso > 0.0) {
        // Native indexes row -1 here.
        return Err(crate::Error::InvalidData("invalid SPP monochrome ISO"));
    }
    let bucket = (1..9)
        .find(|&b| iso <= f32::from_bits(KNOTS[b]))
        .unwrap_or(9);
    let k = |i: usize| f32::from_bits(KNOTS[i]);
    let t = |i: usize| f64::from_bits(if sd1 { SD1[i] } else { DP[i] });
    let below = f64::from(iso - k(bucket - 1));
    let above = f64::from(k(bucket) - iso);
    let value =
        t(bucket).mul_add(below, t(bucket - 1) * above) as f32 / (k(bucket) - k(bucket - 1));
    Ok(1.0 * value)
}

/// The white balance parameters from the native parameter setup: the
/// white balance gains over their minimum, times the F-number, temperature
/// and sensor factors (in that order, f64).
fn mono_white_balance(parts: &super::MerrillGainParts) -> [f64; 3] {
    let wb = parts.white_balance;
    let gb = if wb[2] <= wb[1] { wb[2] } else { wb[1] };
    let min = if gb <= wb[0] { gb } else { wb[0] };
    std::array::from_fn(|c| {
        ((parts.fnumber[c] * parts.temperature[c]) * parts.sensor[c]) * (wb[c] / min)
    })
}

/// Monochrome SPP render: native Stage2 monochrome denoise, Stage2B
/// B/W sharpening and the Stage4 monochrome output with the BnW tone curve.
#[allow(clippy::too_many_arguments)]
unsafe fn render_monochrome(
    x3f: *mut x3f_t,
    image: *mut x3f_area16_t,
    levels: *mut x3f_image_levels_t,
    encoding: x3f_color_encoding_t,
    crop: bool,
    fix_bad: bool,
    apply_sgain: bool,
    wb: *mut libc::c_char,
    control: Control<'_>,
    gain: [f64; 3],
    exif: &crate::jpeg_exif::JpegExif,
    noise: &NoiseParams,
) -> crate::Result<()> {
    use crate::spp_denoise::mono::{self, sharpen_bnw};
    let mut skip = 0;
    unsafe {
        x3f_get_camf_unsigned(x3f, c"SkipStepFlags".as_ptr().cast_mut(), &mut skip);
    }
    if skip & (1 << 23) != 0 {
        return Err(crate::Error::InvalidData(
            "unsupported SPP denoise bypass or FPN mode",
        ));
    }
    let parts = unsafe { super::merrill_gain_parts(x3f, wb, control) }
        .ok_or(crate::Error::InvalidData("unsupported SPP request gains"))?;
    let mono_noise = mono::MonoNoise {
        iso: noise.iso,
        noise_scaling: mono_noise_scaling(parts.divide, noise.iso)?,
        iso_gain: noise.iso_gain,
        column_noise_scale: noise.column_noise_scale,
        // The neutral banding slider: filter size 27, threshold index 2.
        bcs_filter_size: 27,
        bcs_scale_index: 2,
        // m_ColorMixing: the B/N mixer SPP passes by default, R 33 %, G 33 %,
        // B 34 % (measured: 82 dB against SPP 6.9 on _SDI0130; zero weights,
        // which the engine turns into 1,1,1, give 26 dB).
        color_mixing: [0.33, 0.33, 0.34],
        white_balance: mono_white_balance(&parts),
    };
    let luts = unsafe { super::make_merrill_mono_render_luts(x3f, encoding, exif, control) }?;
    let mut area = x3f_area16_t::default();
    if unsafe { x3f_image_area(x3f, &mut area) } == 0 {
        return Err(crate::Error::InvalidData("missing SPP image"));
    }
    let size = [area.columns as usize, area.rows as usize];
    let (active, keep, _) = unsafe { merrill_rects(x3f) }?;
    let active = active_in_keep(active, keep, size)?;
    let mut observe = |_: &'static str, _: &[f32], _: String| {};
    let prepared = unsafe {
        merrill_prepare_float(
            x3f,
            wb,
            apply_sgain,
            fix_bad,
            Some(noise.despeckle),
            control,
            &mut observe,
        )
    }?;
    let stride = prepared.row_stride;
    // Stage2 output image: only its outer 9 columns and rows keep this
    // content, and the active area lies further inside.
    let mut work = prepared.data.clone();
    let mut sharp: Vec<i16> = zeroed(size[0] * size[1])?;
    mono::monochrome_denoise(
        &mut work,
        stride,
        &prepared.data,
        stride,
        &mut sharp,
        size[0],
        size,
        &mono_noise,
        mono::MonoDenoiseArgs {
            gains: gain,
            noise_divisors: [1.0, 1.0],
            adjust_noise: false,
        },
        control,
    )?;
    drop(prepared);
    // Stage2B: ComputeSharpenAmount with the fixed monochrome base 1.25.
    sharpen_bnw::apply_sharp_l_bnw(&mut work, stride, &sharp, size[0], size, 1.25)?;
    sharpen_bnw::copy_channel0_to_channel2(&mut work, stride, size)?;
    drop(sharp);
    control.check()?;
    let rect = if crop {
        active
    } else {
        [0, 0, size[0], size[1]]
    };
    let width = rect[2] - rect[0];
    let height = rect[3] - rect[1];
    let out_stride = width.checked_mul(3).ok_or(crate::Error::Allocation)?;
    let samples = out_stride
        .checked_mul(height)
        .ok_or(crate::Error::Allocation)?;
    let bytes = samples
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or(crate::Error::Allocation)?;
    let buffer = unsafe { libc::malloc(bytes) };
    if buffer.is_null() {
        return Err(crate::Error::Allocation);
    }
    // The controlled caller owns this allocation and frees it on any error.
    unsafe {
        *image = x3f_area16_t {
            data: buffer.cast(),
            buf: buffer,
            rows: height as u32,
            columns: width as u32,
            channels: 3,
            row_stride: out_stride as u32,
        };
    }
    let output = unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u16>(), samples) };
    for y in 0..height {
        control.check()?;
        let from = (rect[1] + y) * stride + rect[0] * 3;
        for (out, input) in output[y * out_stride..][..out_stride]
            .chunks_exact_mut(3)
            .zip(work[from..][..out_stride].chunks_exact(3))
        {
            out.copy_from_slice(&luts.render(input.try_into().unwrap()));
        }
    }
    if !levels.is_null() {
        unsafe {
            *levels = x3f_image_levels_t {
                black: [0.; 3],
                white: [65535; 3],
            };
        }
    }
    control.check()
}

/// Whether the ported denoise covers this SkipStepFlags / FPNSuppression pair.
///
/// SkipStepFlags bit 23 bypasses the denoise, which is unported. The native
/// engine reads NoiseParams.FPNSuppression in two places (the tiled
/// denoise entry and the tile processor), and both branch only on
/// `(fpn | 4) == 7`, i.e. values 3 and 7. Every other value, such as the 1
/// written by DP Merrill cameras with FPN Suppression on, takes the ordinary
/// path this port implements. (Preprocess also reads it only for
/// MED/LOW resolution dark shields, which SPP rendering rejects anyway.)
fn denoise_mode_supported(skip_step_flags: u32, fpn_suppression: u32) -> bool {
    skip_step_flags & (1 << 23) == 0 && (fpn_suppression | 4) != 7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_crop_is_relative_to_keep() {
        assert_eq!(
            active_in_keep([64, 44, 4768, 3180], [8, 8, 4936, 3272], [4928, 3264]).unwrap(),
            [56, 36, 4760, 3172]
        );
        assert!(active_in_keep([0, 0, 20, 20], [8, 8, 40, 40], [32, 32]).is_err());
        assert!(active_in_keep([8, 8, 50, 20], [8, 8, 40, 40], [32, 32]).is_err());
        assert!(active_in_keep([8, 8, 20, 20], [8, 8, 40, 40], [31, 32]).is_err());
        assert!(active_in_keep([20, 20, 8, 8], [8, 8, 40, 40], [32, 32]).is_err());
    }

    #[test]
    fn color_matrix_keeps_native_fused_cancellation() {
        let epsilon = 1. / 134_217_728.;
        let cc = [-1., 1. + epsilon, 0., 0., 0., 0., 0., 0., 0.];
        let compensation = [1., 0., 0., 1. - epsilon, 0., 0., 0., 0., 0.];
        let result = compose_color_matrix(cc, 1., compensation).unwrap();
        assert_eq!(result[0][0].to_bits(), 0xbc90_0000_0000_0000);
        assert_eq!(
            compose_color_matrix(cc, 2., [0.; 9]).unwrap()[0],
            [-2., 2. + 2. * epsilon, 0.]
        );
        assert!(compose_color_matrix(cc, f32::NAN, compensation).is_err());
    }

    #[test]
    fn reference_noise_setup_uses_native_iso_and_scale() {
        let root = std::env::var_os("X3F_TEST_FILES")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../x3f_test_files")
            });
        let mut checked = 0;
        for (name, iso, scale) in [
            ("_P2M0358.X3F", 100_f32, 1_f32),
            ("sigma_sd1_merrill_15.x3f", 800_f32, 0.5_f32),
        ] {
            let path = root.join(name);
            if !path.exists() {
                continue;
            }
            let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            unsafe {
                let file = libc::fopen(path.as_ptr(), c"rb".as_ptr());
                assert!(!file.is_null());
                let x3f = crate::x3f_new_from_file(file.cast());
                assert!(!x3f.is_null());
                for section in [crate::x3f_get_camf(x3f), crate::x3f_get_prop(x3f)] {
                    assert!(!section.is_null());
                    assert_eq!(
                        crate::x3f_load_data(x3f, section),
                        crate::x3f_return_e_X3F_OK
                    );
                }
                let gain = merrill_request_gain(x3f, x3f_get_wb(x3f), Control::none()).unwrap();
                let exif = merrill_exif(x3f, Control::none()).unwrap();
                let params = noise_params(x3f, gain, &exif, NoiseSliders::NEUTRAL).unwrap();
                assert_eq!(params.iso.to_bits(), iso.to_bits(), "{name}");
                assert_eq!(params.scale.to_bits(), scale.to_bits(), "{name}");
                assert_eq!(params.filter_size, 21);
                assert_eq!(params.despeckle, 1.);
                assert_eq!(params.fpn_suppression, 0);
                crate::x3f_delete(x3f);
                libc::fclose(file);
                checked += 1;
            }
        }
        if checked == 0 {
            eprintln!("SPP noise setup: skipped, reference corpus is absent");
        }
    }

    #[test]
    fn denoise_mode_follows_native_fpn_gate() {
        for fpn in [0, 1, 2, 4, 5, 6] {
            assert!(denoise_mode_supported(0, fpn), "FPNSuppression {fpn}");
        }
        for fpn in [3, 7] {
            assert!(!denoise_mode_supported(0, fpn), "FPNSuppression {fpn}");
        }
        assert!(!denoise_mode_supported(1 << 23, 0));
        assert!(denoise_mode_supported(!(1 << 23), 1));
    }

    #[test]
    fn rejects_unsupported_export_options() {
        use crate::DenoiseBackend::{Auto, Cpu, Gpu};
        for encoding in [
            x3f_color_encoding_e_SRGB,
            x3f_color_encoding_e_ARGB,
            x3f_color_encoding_e_PPRGB,
        ] {
            for backend in [Auto, Cpu] {
                assert!(validate_mode(encoding, false, 10, backend, true, "HI").is_ok());
            }
        }
        for resolution in ["", "MED", "LOW"] {
            assert!(
                validate_mode(x3f_color_encoding_e_SRGB, false, 10, Cpu, true, resolution).is_err()
            );
        }
        for encoding in [
            x3f_color_encoding_e_NONE,
            x3f_color_encoding_e_UNPROCESSED,
            x3f_color_encoding_e_QTOP,
        ] {
            assert!(validate_mode(encoding, false, 10, Auto, true, "HI").is_err());
        }
        assert!(validate_mode(x3f_color_encoding_e_SRGB, true, 10, Cpu, true, "HI").is_err());
        assert!(validate_mode(x3f_color_encoding_e_SRGB, false, 10, Cpu, false, "HI").is_err());
        assert!(validate_mode(x3f_color_encoding_e_SRGB, false, 0, Cpu, true, "HI").is_err());
        assert!(validate_mode(x3f_color_encoding_e_SRGB, false, 9, Cpu, true, "HI").is_err());
        assert!(matches!(
            validate_mode(x3f_color_encoding_e_SRGB, false, 10, Gpu, true, "HI"),
            Err(crate::Error::Gpu(_))
        ));
    }
}
