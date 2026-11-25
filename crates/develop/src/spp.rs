//! Sigma base: development via x3fuse-core (Apache-2.0, © 2026 Sang Lee;
//! based on Kalpanika x3f, BSD). Source: third_party/x3fuse-core.
//!
//! x3fuse-core contains a Merrill pipeline (preparation, noise reduction, colour). It works
//! at full resolution on the CPU (or the GPU) with neutral sliders.

use std::path::Path;

use x3f_core::{ColorEncoding, ColorMode, ProcessOptions, Reader};

use crate::Rgb16;

fn color_mode(name: Option<&str>) -> Option<ColorMode> {
    Some(match name? {
        "SunsetRed" => ColorMode::SunsetRed,
        "ForestGreen" => ColorMode::ForestGreen,
        "FCYellow" => ColorMode::FCYellow,
        "Cinema" => ColorMode::Cinema,
        "TealAndOrange" => ColorMode::TealAndOrange,
        "Standard" | crate::settings::LINEAR_MODE => ColorMode::Standard,
        "Vivid" => ColorMode::Vivid,
        "Neutral" => ColorMode::Neutral,
        "Portrait" => ColorMode::Portrait,
        "Landscape" => ColorMode::Landscape,
        "FCBlue" => ColorMode::FCBlue,
        _ => return None,
    })
}

fn open(path: &Path) -> Result<Reader, String> {
    let t = std::time::Instant::now();
    let timing = std::env::var_os("X3F_SPP_TIMING").is_some();
    crate::merrill_highlights::install();
    let mut r = Reader::open(path).map_err(|e| e.to_string())?;
    let t_open = t.elapsed();
    // The embedded JPEG provides the EXIF colour space, by which the source matrix is chosen (as in SPP).
    let _ = r.load_thumbnail_jpeg();
    let t_jpeg = t.elapsed();
    let _ = r.load_property_list();
    r.load_camf().map_err(|e| e.to_string())?;
    let t_camf = t.elapsed();
    r.load_raw().map_err(|e| e.to_string())?;
    if timing {
        eprintln!(
            "SPP load + decode file: {:.0?} (open {:.0?}, JPEG {:.0?}, PROP+CAMF {:.0?}, RAW {:.0?})",
            t.elapsed(),
            t_open,
            t_jpeg - t_open,
            t_camf - t_jpeg,
            t.elapsed() - t_camf
        );
    }
    Ok(r)
}

fn options(mode: Option<&str>) -> ProcessOptions {
    ProcessOptions {
        color_encoding: ColorEncoding::Srgb,
        color_mode: Some(color_mode(mode).unwrap_or(ColorMode::AsShot)),
        spp_denoise: true,
        rgb_chroma_lut: Some(false),
        ..Default::default()
    }
}

/// Without a selected colour mode, "as shot" applies. If the camera was set to black and white or sepia (the RAW data is still
/// in colour), Sigma's processing has no mode for it; then "Standard" is used instead of failing entirely.
fn with_fallback_color_mode<T>(mode: Option<&str>, f: impl Fn(ColorMode) -> Result<T, String>) -> Result<T, String> {
    let chosen = color_mode(mode);
    match f(chosen.unwrap_or(ColorMode::AsShot)) {
        Err(e) if chosen.is_none() && e.contains("as-shot monochrome and sepia") => f(ColorMode::Standard),
        r => r,
    }
}

/// Full resolution, sRGB-encoded, 16 bit, with Sigma's pipeline (≈ 4 s).
pub fn render(path: &Path, mode: Option<&str>) -> Result<Rgb16, String> {
    let r = open(path)?;
    let img = with_fallback_color_mode(mode, |m| r.get_image(&ProcessOptions { color_mode: Some(m), ..options(mode) }).map_err(|e| e.to_string()))?;
    let (w, h) = (img.columns as usize, img.rows as usize);
    let mut data = Vec::with_capacity(w * h * 3);
    for row in 0..img.rows {
        data.extend_from_slice(&img.row(row)[..w * 3]);
    }
    Ok(Rgb16 { width: w, height: h, data })
}

/// DNG from x3fuse-core: linear data with embedded camera profiles per Sigma colour mode
/// (selectable in Lightroom), losslessly compressed.
/// `xmp`: Lightroom settings to embed (see [`crate::xmp`]).
pub fn write_dng(path: &Path, out: &Path, mode: Option<&str>, xmp: Option<String>) -> Result<(), String> {
    let r = open(path)?;
    let opts = ProcessOptions {
        color_encoding: ColorEncoding::None,
        color_mode: Some(color_mode(mode).unwrap_or(ColorMode::AsShot)),
        compress: true,
        xmp_packet: xmp,
        ..Default::default()
    };
    match r.dump_dng(out, &opts) {
        Ok(()) => Ok(()),
        // sd Quattro (H): x3fuse-core has no colour modes there → DNG without colour mode profiles
        Err(_) if crate::quattro::is_prepared(path) => {
            let r = open(path)?;
            r.dump_dng(out, &ProcessOptions { color_mode: None, ..opts }).map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// SPP working image (via our addition to x3fuse-core): RGB counts after Sigma's
/// preprocessing, noise reduction, colour matrix of the colour mode and ColorDQ, before
/// tone curve and gamma. Basis for [`crate::render_counts`] (≈ 3 s, full resolution).
pub fn working(path: &Path, mode: Option<&str>) -> Result<crate::Linear, String> {
    working_cancellable(path, mode, &std::sync::atomic::AtomicBool::new(false))
}

/// Like [`working`], cancellable via `cancel`.
pub fn working_cancellable(
    path: &Path,
    mode: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<crate::Linear, String> {
    if crate::quattro::is_prepared(path) {
        return quattro_working(path, mode, cancel);
    }
    let r = open(path)?;
    let (data, width, height) = with_fallback_color_mode(mode, |m| r.spp_working_image(Some(m), cancel).map_err(|e| e.to_string()))?;
    Ok(crate::Linear { mono: false, haze: None, toneq: None, width, height, data, detail: None })
}

/// Like [`working_cancellable`], but the noise reduction runs on the GPU
/// (feature `spp-gpu`); without a GPU automatically on the CPU. The result deviates minimally
/// (f32 rounding) from the bit-exact CPU version. Whether the GPU was used:
/// [`last_used_gpu`].
pub fn working_gpu_cancellable(
    path: &Path,
    mode: Option<&str>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<crate::Linear, String> {
    if crate::quattro::is_prepared(path) {
        return quattro_working(path, mode, cancel);
    }
    let r = open(path)?;
    let (data, width, height) = with_fallback_color_mode(mode, |m| r.spp_working_image_gpu(Some(m), cancel).map_err(|e| e.to_string()))?;
    Ok(crate::Linear { mono: false, haze: None, toneq: None, width, height, data, detail: None })
}

/// SPP working image for a base key (colour mode + noise sliders, see
/// [`crate::Settings::spp_key`]): Sigma's noise reduction with the slider steps as in SPP.
/// `gpu`: noise reduction on the GPU, if compiled in,
/// available and implemented for the parameters (otherwise CPU, bit-exact). With detail image "L" for the
/// sharpness slider ([`crate::Linear::detail`]).
pub fn working_key(
    path: &Path,
    key: &crate::SppKey,
    gpu: bool,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<crate::Linear, String> {
    // setting "Grafikkarte verwenden" off: the processor path (bit-exact)
    let gpu = gpu && pools::gpu_allowed();
    if crate::quattro::is_prepared(path) {
        return quattro_working_nr(path, key, cancel);
    }
    let r = open(path)?;
    let s = key.sliders();
    // Black and white: grey base from the preprocessed layers (see crate::mono)
    if key.mono {
        let sliders = x3f_core::SppNoiseSliders { luma: s.luma, chroma: s.chroma, texture: [1.0, 1.0] };
        let mix = if key.mono_mix == [0; 3] { [0.33, 0.33, 0.34] } else { key.mono_mix.map(|m| f64::from(m) / 1000.0) };
        let w = r.spp_mono_working_image(sliders, mix, cancel).map_err(|e| e.to_string())?;
        return Ok(crate::mono::base(w));
    }
    // highlight sliders of the Merrill rebuild away from the middle: on the kept base, no new development
    // ([`crate::highlight_post`])
    if key.highlight.on && !key.highlight.is_default() && crate::highlight_rebuild::enabled_merrill() {
        let mut k0 = key.clone();
        k0.highlight = crate::HighlightSliders::new(0.5, 0.5);
        // without a known weight the base is developed here (the hook runs and leaves it), not read from the disk cache
        let known = crate::highlight_post::weight(path).is_some();
        let base = crate::highlight_post::base(path, &k0, || if known { crate::diskcache::working_key(path, &k0, gpu, cancel) } else { working_key(path, &k0, gpu, cancel) })?;
        let mut lin = (*base).clone();
        if let Some(Some(w)) = crate::highlight_post::weight(path) {
            crate::highlight_post::apply(&mut lin, &w, key.highlight.colour(), key.highlight.structure());
        }
        return Ok(lin);
    }
    // Lens correction (CA) as in SPP; off or without a profile → `None`, bit-identical as before.
    let ca = crate::lens::correction(path, &key.ca);
    crate::merrill_highlights::clear_capture();
    let w = with_fallback_color_mode(key.mode.as_deref(), |m| {
        let sliders = x3f_core::SppNoiseSliders { luma: s.luma, chroma: s.chroma, texture: key.texture_gains() };
        r.spp_working_image_ca(Some(m), gpu, sliders, ca.as_ref(), cancel).map_err(|e| e.to_string())
    })?;
    if let Some(wt) = crate::merrill_highlights::take_capture(w.width, w.height) {
        crate::highlight_post::remember(path, wt);
    }
    // Detail image "L" for the sharpness slider (Δ relative to sharpness 0, see crate::sharpness)
    let detail = crate::sharpness::SharpDetail { l: w.sharp_l, base: w.sharpen_base, scale: 1.0 };
    let mut data = w.data;
    // Prototype: `GRAINEO_CHROMA_BM3D=strength[,y]`: BM3D only on the colour of the
    // working image; only for graineo-dev comparisons. The disk cache does not know the variable,
    // so only together with `GRAINEO_NO_DISK_CACHE` (otherwise the result would end up under the
    // normal key).
    let proto = std::env::var_os("GRAINEO_NO_DISK_CACHE").and(std::env::var("GRAINEO_CHROMA_BM3D").ok());
    if let Some((strength, guided)) = proto.and_then(|v| {
        let (a, b) = v.split_once(',').map_or((v.as_str(), ""), |(a, b)| (a, b));
        Some((a.parse::<f32>().ok()?, b == "y"))
    }) {
        let t = std::time::Instant::now();
        let control = x3f_core::Control::new(cancel)
            .with_denoise_backend(if gpu { x3f_core::DenoiseBackend::Auto } else { x3f_core::DenoiseBackend::Cpu });
        let info = x3f_core::chroma_bm3d_rgb(&mut data, w.width, w.height, strength, guided, control)
            .map_err(|e| e.to_string())?;
        eprintln!("chroma BM3D strength {strength} {}: σ U/V {:.3}/{:.3}, {} {:.2?}",
            if guided { "(search on Y)" } else { "" }, info.sigma[0], info.sigma[1],
            if info.gpu { "GPU" } else { "CPU" }, t.elapsed());
    }
    Ok(crate::Linear { mono: false, haze: None, toneq: None, width: w.width, height: w.height, data, detail: Some(std::sync::Arc::new(detail)) })
}

/// Quattro: in the middle position the noise reduction of the preparation is kept (first stage on the colour grid,
/// see [`crate::quattro_nr`]); changed sliders act as before via our own noise reduction.
fn quattro_working_nr(path: &Path, key: &crate::SppKey, cancel: &std::sync::atomic::AtomicBool) -> Result<crate::Linear, String> {
    let mut lin = quattro_working_nr_base(path, key, cancel)?;
    // Foveon detail bands after the noise reduction (Quattro and Classic, see crate::foveon_texture)
    if key.texture != crate::TEXTURE_NEUTRAL {
        let (w, h) = (lin.width, lin.height);
        crate::foveon_texture::apply(&mut lin.data, w, h, key.texture_gains());
    }
    Ok(lin)
}

fn quattro_working_nr_base(path: &Path, key: &crate::SppKey, cancel: &std::sync::atomic::AtomicBool) -> Result<crate::Linear, String> {
    if key.neutral_noise() {
        let q = crate::quattro::prepared(path)?;
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("abgebrochen".into());
        }
        return Ok(crate::quattro::counts_hl(&q, key.mode.as_deref(), key.binning, key.highlight));
    }
    let q = crate::quattro::prepared(path)?;
    // high-ISO path: the sliders move from the preparation's result (see `crate::quattro::counts_with_sliders`)
    if let Some(lin) = crate::quattro::counts_with_sliders_hl(&q, key.mode.as_deref(), key.binning, key.nr, key.detail, key.highlight) {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("abgebrochen".into());
        }
        return Ok(lin);
    }
    // Classic: the chain again with the sliders (they act in its range-based reconstruction)
    if q.classic.is_some() {
        let lin = crate::classic_new::sliders::linear(path, &q.cal, q.raw.columns, q.raw.rows, key.nr).ok_or("Classic-Kette mit Reglern nicht verfügbar")?;
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("abgebrochen".into());
        }
        return Ok(crate::quattro::counts_from_linear(&q, &lin, key.mode.as_deref(), key.binning));
    }
    let mut lin = match q.classic.as_ref() {
        Some(l) => (**l).clone(),
        None => crate::linearize(&q.raw, &q.cal),
    };
    crate::denoise::denoise(&mut lin, &q.cal, key.sliders());
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("abgebrochen".into());
    }
    Ok(crate::quattro::counts_from_linear(&q, &lin, key.mode.as_deref(), key.binning))
}

/// Quattro: working image from x3fuse-core's preprocessing (no SPP-accurate base; see
/// [`crate::quattro`]). The preprocessing itself cannot be cancelled, but is cached.
fn quattro_working(path: &Path, mode: Option<&str>, cancel: &std::sync::atomic::AtomicBool) -> Result<crate::Linear, String> {
    let q = crate::quattro::prepared(path)?;
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("abgebrochen".into());
    }
    Ok(crate::quattro::counts(&q, mode, crate::Binning::Off))
}

/// Did the last SPP noise reduction run on the GPU?
pub fn last_used_gpu() -> bool {
    x3f_core::spp_last_used_gpu()
}

/// GPU adapter for the SPP noise reduction (or the reason why none is available).
pub fn gpu_adapter() -> Result<String, String> {
    x3f_core::spp_gpu_adapter()
}

/// Hands a lost GPU device of the Sigma noise reduction (x3fuse-core keeps its own device) to
/// [`pools::gpu_lost`], so every GPU path takes the processor and the app can tell the user. Cheap; called per frame.
pub fn sync_gpu_lost() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if x3f_core::gpu_device_lost() && !DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        pools::gpu_lost("Sigma noise reduction", "device lost");
    }
}

/// Frees the cached GPU buffers of the noise reduction (≈ 0.9 GB).
pub fn gpu_release() {
    x3f_core::spp_gpu_release();
}
