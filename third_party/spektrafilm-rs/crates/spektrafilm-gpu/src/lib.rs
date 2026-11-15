// graineo: silence warnings of the original code (unused helper functions) in the vendored build.
#![allow(unused)]
pub mod cpu_backend;
#[cfg(feature = "wgpu-backend")]
pub mod wgpu_backend;

use spektrafilm_math::image::ImageBuf;

/// Compute backend abstraction. Each method corresponds to a GPU-friendly
/// operation in the film simulation pipeline.
///
/// Default implementations fall back to CPU. GPU backends override the
/// spectral methods for massive speedups.
pub trait ComputeBackend: Send + Sync {
    fn colorspace_convert(&self, img: &ImageBuf, matrix: &[[f32; 3]; 3]) -> ImageBuf;
    fn cctf_encode_srgb(&self, img: &ImageBuf) -> ImageBuf;
    fn cctf_decode_srgb(&self, img: &ImageBuf) -> ImageBuf;
    fn gaussian_blur(&self, img: &ImageBuf, sigma: f32) -> ImageBuf;

    /// Blur `img` with each of `sigmas`, returning one image per sigma.
    /// GPU backends fuse the work into a single command buffer (one upload,
    /// one submit, one readback) — much cheaper than N independent
    /// `gaussian_blur` calls when N > 1. Default implementation calls the
    /// per-sigma kernel in a loop.
    fn gaussian_blur_multi(&self, img: &ImageBuf, sigmas: &[f32]) -> Vec<ImageBuf> {
        sigmas.iter().map(|&s| self.gaussian_blur(img, s)).collect()
    }
    fn table_lookup(&self, img: &ImageBuf, table_x: &[f32], table_y: &[[f32; 3]]) -> ImageBuf;
    fn lut3d_interp(&self, img: &ImageBuf, lut: &Lut3D) -> ImageBuf;

    /// Spectral scanning: density CMY → RGB via spectral integration.
    /// GPU backends override this with a compute shader.
    ///
    /// The `cat` and `xyz_to_rgb` matrices are kept separate so the CPU
    /// path can apply them as two sequential matmuls (matching Python's
    /// `colour.XYZ_to_RGB` step-by-step). Pre-combining loses ~1 ULP per
    /// output channel which compounds to ~5e-6 of drift in the bare
    /// chain. GPU may collapse for performance.
    fn scan_spectral(
        &self,
        density_cmy: &ImageBuf,
        channel_density: &[[f64; 3]],
        base_density: &[f64],
        illuminant: &[f64],
        normalization: f64,
        cat: &[[f64; 3]; 3],
        xyz_to_rgb: &[[f64; 3]; 3],
    ) -> ImageBuf {
        cpu_backend::scan_spectral_cpu(
            density_cmy,
            channel_density,
            base_density,
            illuminant,
            normalization,
            cat,
            xyz_to_rgb,
        )
    }

    /// Spectral printing: film density CMY → print log-exposure via spectral integration.
    /// GPU backends override this with a compute shader.
    fn print_spectral(
        &self,
        density_cmy: &ImageBuf,
        channel_density: &[[f64; 3]],
        base_density: &[f64],
        illuminant: &[f64],
        sensitivity: &[[f64; 3]],
        normalization_factor: f64,
        preflash: [f64; 3],
    ) -> ImageBuf {
        cpu_backend::print_spectral_cpu(
            density_cmy,
            channel_density,
            base_density,
            illuminant,
            sensitivity,
            normalization_factor,
            preflash,
        )
    }

    /// Hanatos2025 RGB → film raw exposure (the per-pixel bicubic LUT lookup).
    ///
    /// CPU implementation does the two-step CAT02 adaptation (RGB → native
    /// XYZ, then CAT02 adapt) per pixel for bit-exact Python parity —
    /// fusing both matrices into a single matmul gives a different ULP than
    /// applying them sequentially. GPU backends override and may collapse
    /// the two into a single matrix per the precision budget for live preview.
    fn hanatos2025_rgb_to_raw(
        &self,
        image: &ImageBuf,
        tc_lut: &spektrafilm_math::spectral::TcLut,
        color_space: &str,
        ref_illuminant: &[f32],
        cat16: bool,
    ) -> ImageBuf {
        spektrafilm_math::spectral::hanatos2025_rgb_to_raw(
            image,
            tc_lut,
            color_space,
            ref_illuminant,
            cat16,
        )
    }

    /// log_raw → density_cmy via per-channel curve interpolation.
    ///
    /// Called twice per render (filming.develop + printing.develop). GPU
    /// backends override with `density_curve_interp.wgsl`; CPU falls back to
    /// the f64 reference (`interpolate_exposure_to_density_f64`).
    fn density_curve_interp(
        &self,
        log_raw: &ImageBuf,
        log_exposure: &[f64],
        density_curves: &[[f64; 3]],
        gamma_factor: f64,
    ) -> ImageBuf {
        // CPU fallback — uses the f64 reference (`fast_interp_image_f64`).
        // Scalar gamma is broadcast to all channels; we just stretch the x-axis once.
        let scaled: Vec<f64> = if (gamma_factor - 1.0).abs() < 1e-12 {
            log_exposure.to_vec()
        } else {
            log_exposure.iter().map(|&v| v / gamma_factor).collect()
        };
        spektrafilm_math::interp::fast_interp_image_f64(log_raw, &scaled, density_curves)
    }

    /// Optional fused fast-path: runs filming + printing + scanning as a single
    /// GPU-resident command buffer (one upload at start, one readback at end).
    /// Returns `None` to fall back to per-stage trait methods. CPU backend
    /// always returns `None`; wgpu implements it.
    fn try_run_film_chain(&self, _params: &FilmChainParams<'_>) -> Option<ImageBuf> {
        None
    }

    /// True when `try_run_film_chain` returns the same post-scan output that
    /// `Pipeline::apply_post_scan` would produce (final clamp and optional
    /// sRGB encoding). Backends default to returning linear RGB and letting
    /// the pipeline apply the final CPU pass.
    fn resident_chain_applies_post_scan(&self) -> bool {
        false
    }

    /// True for f32 GPU backends where effects may trade exactness for
    /// speed (the diffusion filter uses a downsampled sum-of-Gaussians
    /// instead of the exact FFT convolution). CPU keeps the exact path.
    fn is_gpu(&self) -> bool {
        false
    }

    /// graineo: largest image (pixels) this backend can process in one piece. The wgpu backend dispatches
    /// one-dimensional grids of 1024 pixels per work group and binds the whole image as one storage buffer,
    /// so the device limits cap the size (≈ 67 MP on typical GPUs); larger images must use the CPU.
    fn max_pixels(&self) -> usize {
        usize::MAX
    }

    fn name(&self) -> &str;
}

/// All inputs to the GPU-resident film chain. Bundled into a struct so the
/// trait method stays object-safe and adding more optional stages (halation,
/// DIR couplers, …) doesn't churn every callsite.
pub struct FilmChainParams<'a> {
    pub image: &'a ImageBuf,
    /// RGB → film-raw front pass (the configured upsampler).
    pub front: FrontPass<'a>,
    pub film_log_exposure: &'a [f64],
    pub film_density_curves_normalized: &'a [[f64; 3]],
    pub film_gamma: f64,
    pub film_channel_density: &'a [[f64; 3]],
    pub film_base_density: &'a [f64],
    pub print_illuminant: &'a [f64],
    pub print_sensitivity: &'a [[f64; 3]],
    pub print_normalization_factor: f64,
    pub print_log_exposure: &'a [f64],
    pub print_density_curves: &'a [[f64; 3]],
    pub print_gamma: f64,
    pub print_channel_density: &'a [[f64; 3]],
    pub print_base_density: &'a [f64],
    /// Constant enlarger preflash raw 3-vector, added to the print exposure
    /// before the inner log10 (Python `_compute_raw_preflash`). `[0; 3]` off.
    pub preflash: [f64; 3],
    pub viewing_illuminant: &'a [f64],
    pub scan_normalization: f64,
    pub scan_xyz_to_rgb: &'a [[f64; 3]; 3],
    /// B&W/slide scanner luminance remap `(m, q)` applied per-pixel on the
    /// scan XYZ (`clip(m·Y+q, 0, 1)/(Y+1e-10)`). `None` is identity. The
    /// filming/printing exposure halves of the correction are folded by the
    /// caller into `rgb_to_adapted_xyz` / `print_normalization_factor`.
    pub bw_xyz_remap: Option<(f64, f64)>,
    /// Scan the developed film directly instead of printing onto paper.
    /// When true the print_spectral + print density-curve passes are
    /// skipped and scan_spectral runs on the film density buffer using the
    /// film's `film_channel_density` / `film_base_density` (the caller sets
    /// `viewing_illuminant` / `scan_normalization` / `scan_xyz_to_rgb` from
    /// the film's viewing illuminant). Mirrors the CPU `scan_film` path.
    pub scan_film: bool,
    /// Optional halation pass — when `Some`, inserted on the raw film
    /// exposure buffer between hanatos2025 and log10.
    pub halation: Option<HalationGpuParams>,
    /// Optional DIR (development inhibitor release) couplers pass — when
    /// `Some`, inserted on the film density buffer between filming and
    /// printing. Re-interpolates density curves using `density_curves_0`.
    pub dir_couplers: Option<DirCouplersGpuParams<'a>>,
    /// Optional grain pass — Poisson-binomial particle model on the film
    /// density buffer, after DIR couplers and before print spectral.
    /// Uses normal-approximation sampling on the GPU (matches what the
    /// CPU path does for typical λ > 30 / variance > 9 regimes).
    pub grain: Option<GrainGpuParams>,
    /// Optional viewing glare pass — applied after scan spectral on the
    /// final RGB buffer. Lognormal-distributed per-pixel surface noise +
    /// blur + per-channel illuminant offset.
    pub glare: Option<GlareGpuParams>,
    /// Optional output gamut compression pass — applied after glare on
    /// the final RGB buffer, before unsharp (matching the CPU scanning
    /// order). When set, scan_spectral skips its [0,1] clamp so the
    /// compressor sees the out-of-gamut values it needs.
    pub gamut: Option<GamutGpuParams<'a>>,
    /// Optional unsharp mask pass — applied after glare (last step
    /// before readback). Blur σ in pixels + amount scalar; both come
    /// from `scanner.unsharp_mask`.
    pub unsharp: Option<UnsharpGpuParams>,
    /// Optional camera lens Gaussian blur on the raw film exposure buffer,
    /// after camera diffusion and before halation.
    pub camera_lens_blur_px: Option<f32>,
    /// Optional scanner lens Gaussian blur on the final RGB buffer, after
    /// glare/gamut compression and before unsharp.
    pub scanner_lens_blur_px: Option<f32>,
    /// Optional highlight reconstruction on the raw film exposure buffer,
    /// immediately after the front pass and before optical scatter.
    pub highlight_boost: Option<HighlightBoostGpuParams>,
    /// Optional camera lens diffusion filter — applied on the raw film
    /// exposure buffer right after hanatos2025, before halation (matching
    /// the CPU filming order). A downsampled sum-of-Gaussians; see
    /// `DiffusionGpuPlan`.
    pub diffusion: Option<DiffusionGpuPlan>,
    /// Optional enlarger diffusion filter. Applied in the print stage on
    /// linear print raw between print_spectral and print density curves.
    pub enlarger_diffusion: Option<DiffusionGpuPlan>,
    /// The print_exposure × B&W printing correction scalar. Used by CUDA
    /// when `enlarger_diffusion` is active so it can preserve the CPU
    /// print-stage ordering around preflash.
    pub print_exposure_scale: f64,
    /// Whether the final output should be sRGB-encoded after clamping. Mirrors
    /// `RuntimeParams::io.output_cctf_encoding`.
    pub output_cctf_encoding: bool,
}

/// RGB → film-raw front pass of the GPU-resident chain. Both variants are
/// homogeneous in the input RGB, so the caller folds the exposure scale
/// (auto-exposure × EV compensation × B&W filming correction) into the
/// matrix — no separate scale pass.
pub enum FrontPass<'a> {
    /// Hanatos2025 spectral upsampling: per-pixel chromaticity LUT lookup.
    Hanatos2025 {
        tc_lut: &'a spektrafilm_math::spectral::TcLut,
        /// Combined input-RGB → CAT-adapted-XYZ matrix (row-major).
        rgb_to_adapted_xyz: [[f64; 3]; 3],
    },
    /// Mallett2019 reflectance-basis upsampling: one rgb → raw 3×3 matmul
    /// (`core · M_cs`, see `spektrafilm-core/src/mallett.rs`).
    Mallett2019 { matrix: [[f64; 3]; 3] },
}

/// Pre-computed plan for the GPU-resident diffusion filter. The per-channel
/// PSF is decomposed into Gaussian components (3 per PSF sub-component) and
/// the whole scattered field is computed on an image downsampled by `d` so
/// the blur σ stay small. `sigmas`/`coeffs` are parallel: component `i`
/// blurs the downsampled image at `sigmas[i]` (working-resolution px) and
/// adds `coeffs[i]` (per-channel) into the accumulator.
#[derive(Debug, Clone)]
pub struct DiffusionGpuPlan {
    pub d: u32,
    pub small_w: u32,
    pub small_h: u32,
    pub p_s: f32,
    pub sigmas: Vec<f32>,
    pub coeffs: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, Copy)]
pub struct HighlightBoostGpuParams {
    pub boost_ev: f32,
    pub boost_range: f32,
    pub protect_ev: f32,
}

/// DIR couplers parameters for the GPU-resident matmul + diffusion + final
/// density-curve re-interpolation pass. Mirrors the CPU
/// `apply_density_correction`.
#[derive(Debug, Clone, Copy)]
pub struct DirCouplersGpuParams<'a> {
    /// Already-scaled `couplers_matrix * amount`, row-major.
    pub couplers_matrix_scaled: [[f32; 3]; 3],
    /// Per-channel max density from the normalized curves (used only when
    /// `is_positive` is true; ignored otherwise).
    pub density_max: [f32; 3],
    pub is_positive: bool,
    pub diffusion_size_px: f32,
    pub diffusion_tail_px: f32,
    pub diffusion_tail_weight: f32,
    /// "Density curves before DIR" — re-interpolated by the final shader
    /// pass with the corrected log-exposure.
    pub density_curves_0: &'a [[f64; 3]],
    pub log_exposure: &'a [f64],
    pub gamma_factor: f64,
}

/// Grain parameters for the GPU-resident Poisson-binomial particle model.
/// Mirrors `apply_grain_to_density`: per-channel n_particles_per_pixel
/// already divided by `n_sub_layers`, density_max already includes
/// `density_min`, etc.
#[derive(Debug, Clone)]
pub struct GrainGpuParams {
    pub density_min: [f32; 3],
    pub density_max: [f32; 3],
    pub n_particles_per_pixel: [f32; 3],
    pub grain_uniformity: [f32; 3],
    pub n_sub_layers: u32,
    pub base_seed: u32,
    pub grain_blur: f32,
    /// One shared noise field across all channels (B&W single emulsion)
    /// instead of independent per-channel RNG streams.
    pub monochrome: bool,
    /// graineo: `[x, y, full_width]`: position of the buffer in the full image; the random value
    /// per pixel then depends on the position in the full image (`[0, 0, width]` = original).
    pub origin: [u32; 3],
    /// graineo: polydisperse emulsion (crystal size classes with their own area, speed and grain
    /// size). `None` = original single-size model. When set, `n_particles_per_pixel` is per full
    /// pixel (not divided by `n_sub_layers`) and the `grain_classes.wgsl` passes run instead.
    pub classes: Option<GrainClasses>,
}

/// graineo: maximum number of crystal size classes (sub-layers).
pub const GRAIN_MAX_CLASSES: usize = 8;
/// graineo: steps of the table "overall developed fraction p → fraction of each class".
pub const GRAIN_CLASS_STEPS: usize = 1024;
/// graineo: share β of the grain blur variance that does not depend on the crystal size (dye
/// diffusion, optics); the rest scales with the crystal diameter, see [`GrainClasses::from_fractions`].
pub const GRAIN_BLUR_FIXED_SHARE: f32 = 0.25;
/// graineo: reference developed fraction of the calibration, see [`GrainClasses`].
pub const GRAIN_CLASSES_P_REF: f32 = 0.5;

/// graineo: crystal size classes of an emulsion (polydisperse grain).
///
/// Physical picture: a real emulsion holds crystals of different size; a large crystal catches
/// more photons and becomes developable at a lower exposure. At low exposure (thin negative) mostly
/// the large crystals are developed – few, large silver grains or dye clouds (coarse, relatively
/// strong grain); at high exposure the small ones join in (finer). Colour films build this in as
/// fast/medium/slow sub-layers per colour record. Model per channel and class `i`:
/// - density share `w_i` (Σ = 1) and relative crystal area `a_i`,
/// - developed fraction `p_i(p)` for the overall developed fraction `p = D/Dmax`, with
///   Σ w_i·p_i = p (the mean density stays exactly the one of the density curve); tabulated in
///   `lut` (from the single-hit model for B&W, from the sub-layer curves for colour films),
/// - density noise variance `dmax²/npp · w_i·a_i · sat · p_i` (Poisson-binomial count of
///   `npp·w_i/a_i` crystals with optical density `dmax/npp·a_i` each),
/// - spatial size: the class's noise is blurred with `class_sigma_i` px (graineo: σ_i² =
///   σ²·(β + (1−β)·s²·a_i), i.e. partly with the grain diameter), the mean density with
///   `mean_sigma` px (0 = no blur).
///
/// Two ways to build it: [`GrainClasses::upstream_layers`] reproduces upstream spektrafilm's
/// (experimental) sub-layer grain `apply_grain_to_density_layers` for colour films (per sub-layer
/// curve, area and dye-cloud blur; the hashed/GPU counterpart of the exact CPU port
/// `spektrafilm_model::grain::apply_grain_to_density_layers`); [`GrainClasses::single_hit`] /
/// [`GrainClasses::from_fractions`] are graineo's extension (size classes also for B&W, grain size
/// ∝ √area, calibrated as below).
///
/// Calibration: at the reference fraction `p = 0.5` (about density 1.0 above base, where data
/// sheets quote the RMS granularity) the rendered grain (pixel variance after the blur and the
/// variance-weighted mean blur σ²) equals that of the single-size model, so a film's grain
/// calibration and the grain size slider keep their meaning; only the course over density changes.
/// `var_factor` = w_i·a_i × normalisation (per channel), class blur from the green channel.
#[derive(Debug, Clone, PartialEq)]
pub struct GrainClasses {
    pub n: u32,
    /// p_i over p (0 … 1 in `GRAIN_CLASS_STEPS` steps): `lut[(ch·(STEPS+1) + j)·MAX + i]`
    pub lut: Vec<f32>,
    pub var_factor: [[f32; GRAIN_MAX_CLASSES]; 3],
    pub class_sigma: [f32; GRAIN_MAX_CLASSES],
    pub mean_sigma: f32,
}

impl GrainClasses {
    /// B&W emulsion without sub-layer data: `classes` size classes with equal density share,
    /// crystal areas spread geometrically (largest/smallest = `area_spread`), speed ∝ area^κ and the
    /// single-hit model `p_i = 1 − exp(−x·k_i)` (x solved per p). `reversal` (positive film): the dye
    /// is formed from the crystals the first developer left over, `p_i = 1 − f_i(1 − p)`.
    pub fn single_hit(
        classes: u32,
        area_spread: f64,
        speed_exponent: f64,
        reversal: bool,
        uniformity: [f32; 3],
        grain_blur: f32,
    ) -> Self {
        let n = (classes as usize).clamp(1, GRAIN_MAX_CLASSES);
        let spread = area_spread.max(1.0);
        let raw: Vec<f64> = (0..n)
            .map(|i| if n == 1 { 1.0 } else { spread.powf(i as f64 / (n - 1) as f64 - 0.5) })
            .collect();
        let mean: f64 = raw.iter().sum::<f64>() / n as f64;
        let mut w = [0f32; GRAIN_MAX_CLASSES];
        let mut a = [0f32; GRAIN_MAX_CLASSES];
        let mut k = [0f32; GRAIN_MAX_CLASSES];
        for i in 0..n {
            w[i] = 1.0 / n as f32;
            a[i] = (raw[i] / mean) as f32;
            k[i] = (raw[i] / mean).powf(speed_exponent.max(0.0)) as f32;
        }
        let frac = |_ch: usize, p: f32| {
            if reversal {
                let f = single_hit_fractions(n, &w, &k, 1.0 - p);
                let mut o = [0f32; GRAIN_MAX_CLASSES];
                for i in 0..n {
                    o[i] = 1.0 - f[i];
                }
                o
            } else {
                single_hit_fractions(n, &w, &k, p)
            }
        };
        Self::from_fractions(n, [w; 3], [a; 3], frac, uniformity, grain_blur)
    }

    /// Classes from given shares `w`, areas `a` (per channel) and fractions `frac(ch, p)`; the
    /// fractions are rescaled so that Σ w·p_i = p exactly. Builds the table and the calibration.
    pub fn from_fractions(
        n: usize,
        w: [[f32; GRAIN_MAX_CLASSES]; 3],
        a: [[f32; GRAIN_MAX_CLASSES]; 3],
        frac: impl Fn(usize, f32) -> [f32; GRAIN_MAX_CLASSES],
        uniformity: [f32; 3],
        grain_blur: f32,
    ) -> Self {
        let n = n.clamp(1, GRAIN_MAX_CLASSES);
        let m = GRAIN_MAX_CLASSES;
        let mut lut = vec![0f32; 3 * (GRAIN_CLASS_STEPS + 1) * m];
        for ch in 0..3 {
            for j in 0..=GRAIN_CLASS_STEPS {
                let p = (j as f32 / GRAIN_CLASS_STEPS as f32).clamp(1e-6, 1.0 - 1e-6);
                let mut f = frac(ch, p);
                let s: f32 = (0..n).map(|i| w[ch][i] * f[i]).sum();
                for i in 0..n {
                    // no information (e.g. outside the curve): all classes alike
                    f[i] = if s > 0.0 { (f[i] * p / s).clamp(0.0, 1.0) } else { p };
                }
                lut[(ch * (GRAIN_CLASS_STEPS + 1) + j) * m..][..n].copy_from_slice(&f[..n]);
            }
        }
        let blur_on = grain_blur > 0.4;
        let mut c = Self {
            n: n as u32,
            lut,
            var_factor: [[0.0; GRAIN_MAX_CLASSES]; 3],
            class_sigma: [0.0; GRAIN_MAX_CLASSES],
            mean_sigma: if blur_on { grain_blur } else { 0.0 },
        };
        // variance left after a Gaussian blur of white noise (Σ kernel², separable)
        let gain = |sigma: f32| -> f32 {
            if !blur_on || sigma <= 0.05 {
                return 1.0;
            }
            let r = (4.0 * sigma).ceil() as i32;
            let k: Vec<f64> = (-r..=r).map(|x| (-((x * x) as f64) / (2.0 * (sigma as f64).powi(2))).exp()).collect();
            let s: f64 = k.iter().sum();
            let s2: f64 = k.iter().map(|v| (v / s) * (v / s)).sum();
            (s2 * s2) as f32
        };
        // correlation of neighbouring pixels of blurred white noise (separable kernel: 1-D lag-1 ACF)
        let lag1 = |sigma: f32| -> f32 {
            if !blur_on || sigma <= 0.05 {
                return 0.0;
            }
            let r = (4.0 * sigma).ceil() as i32;
            let k: Vec<f64> = (-r..=r).map(|x| (-((x * x) as f64) / (2.0 * (sigma as f64).powi(2))).exp()).collect();
            let a0: f64 = k.iter().map(|v| v * v).sum();
            let a1: f64 = k.windows(2).map(|w| w[0] * w[1]).sum();
            (a1 / a0) as f32
        };
        let class_blur = |s: f32, a: f32| grain_blur * (GRAIN_BLUR_FIXED_SHARE + (1.0 - GRAIN_BLUR_FIXED_SHARE) * s * s * a).sqrt();
        let pr = GRAIN_CLASSES_P_REF;
        for ch in [1usize, 0, 2] {
            let gu = uniformity[ch];
            let pre: Vec<f32> = (0..n)
                .map(|i| {
                    let fi = c.fraction(ch, pr, i);
                    w[ch][i] * a[ch][i] * (1.0 - fi * gu) * fi
                })
                .collect();
            // blur scale s (green channel): σ_i² = grain_blur²·(β + (1−β)·s²·a_i) – a part β of the blur is
            // size-independent (dye diffusion, optics), the rest scales with the grain diameter √a_i – such
            // that the neighbour correlation of
            // the summed noise (lag 1 px, weighted by each class's variance after the blur) equals the one
            // of the single-size grain – the grain then looks equally coarse at the reference
            if ch == 1 {
                let mut s = 1f32;
                if blur_on {
                    let mixed = |s: f32| -> f32 {
                        let (mut u, mut ur) = (0f32, 0f32);
                        for i in 0..n {
                            let sigma = class_blur(s, a[ch][i]);
                            let ui = pre[i] * gain(sigma);
                            u += ui;
                            ur += ui * lag1(sigma);
                        }
                        if u > 0.0 { ur / u } else { 0.0 }
                    };
                    let target = lag1(grain_blur);
                    let (mut lo, mut hi) = (0.05f32, 20f32);
                    for _ in 0..40 {
                        let mid = (lo * hi).sqrt();
                        if mixed(mid) < target { lo = mid } else { hi = mid }
                    }
                    s = (lo * hi).sqrt();
                }
                for i in 0..n {
                    c.class_sigma[i] = if blur_on { class_blur(s, a[ch][i]) } else { 0.0 };
                }
            }
            let post: f32 = (0..n).map(|i| pre[i] * gain(c.class_sigma[i])).sum();
            let v_ref = (1.0 - pr * gu) * pr * gain(grain_blur);
            let norm = if post > 0.0 { v_ref / post } else { 1.0 };
            for i in 0..n {
                c.var_factor[ch][i] = w[ch][i] * a[ch][i] * norm;
            }
        }
        c
    }

    /// Upstream's sub-layer grain (`apply_grain_to_density_layers`) as classes, for the hashed and
    /// GPU paths. `curves` = normalised density curves (n × 3), `layers[j][l][ch]` = sub-layer curves,
    /// `layer_scale` = `particle_scale_layers`, `area_um2·scale[ch]` = mean crystal area.
    /// Per sub-layer: share `f = max L_l / Σ max L`, `dmax_l = max L_l + f·dmin`,
    /// `npp_l = pixel_area·f / (area·scale·layer_scale)`, developed fraction
    /// `p_l = (L_l(D) + f·dmin) / dmax_l`, dye-cloud blur `blur_dye_clouds_um·√(dmax_l/npp_l)` px
    /// (as upstream, green channel), then the common blur `grain_blur` (applied when > 0).
    /// Statistics of the noise as upstream; the mean density is the curve density (upstream's
    /// Σ dmax_l·p_l equals it up to the clipping of p_l).
    #[allow(clippy::too_many_arguments)]
    pub fn upstream_layers(
        curves: &[[f64; 3]],
        layers: &[Vec<Vec<f64>>],
        n_layers: usize,
        layer_scale: &[f64],
        density_min: [f64; 3],
        pixel_size_um: f64,
        area_um2: f64,
        scale: [f64; 3],
        blur_dye_clouds_um: f64,
        grain_blur: f32,
        positive: bool,
    ) -> Self {
        let n = n_layers.clamp(1, GRAIN_MAX_CLASSES);
        let m = GRAIN_MAX_CLASSES;
        let n_le = curves.len().min(layers.len());
        let val = |v: f64| if v.is_finite() { v } else { 0.0 };
        let pixel_area = pixel_size_um * pixel_size_um;
        let mut lut = vec![0f32; 3 * (GRAIN_CLASS_STEPS + 1) * m];
        let mut var_factor = [[0f32; GRAIN_MAX_CLASSES]; 3];
        let mut class_sigma = [0f32; GRAIN_MAX_CLASSES];
        for ch in 0..3 {
            let mut lmax = [0f64; GRAIN_MAX_CLASSES];
            for j in 0..n_le {
                for l in 0..n {
                    lmax[l] = lmax[l].max(val(layers[j][l][ch]));
                }
            }
            let total: f64 = lmax[..n].iter().sum();
            let cmin = curves.iter().map(|r| r[ch]).filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
            let cmax = curves.iter().map(|r| r[ch]).filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
            // the grain stage's dmax: maximum of the normalised curve + dmin
            let dmax = if cmin.is_finite() { cmax - cmin + density_min[ch] } else { density_min[ch].max(1e-6) };
            let npp = pixel_area / (area_um2 * scale[ch]);
            let xs: Vec<f64> = (0..n_le).map(|j| if positive { -val(curves[j][ch]) } else { val(curves[j][ch]) }).collect();
            for l in 0..n {
                let f = if total > 0.0 { lmax[l] / total } else { 1.0 / n as f64 };
                let dmin_l = f * density_min[ch];
                let dmax_l = lmax[l] + dmin_l;
                let npp_l = pixel_area * f / (area_um2 * scale[ch] * layer_scale[l]);
                // Var_l = dmax_l²/npp_l·sat·p_l, expressed as od²·npp·factor with od = dmax/npp
                var_factor[ch][l] = if npp_l > 0.0 { ((dmax_l * dmax_l / npp_l) / (dmax * dmax / npp)) as f32 } else { 0.0 };
                if ch == 1 && npp_l > 0.0 {
                    let s_dye = if blur_dye_clouds_um > 0.0 { blur_dye_clouds_um * (dmax_l / npp_l).sqrt() } else { 0.0 };
                    class_sigma[l] = ((s_dye * s_dye) as f32 + if grain_blur > 0.0 { grain_blur * grain_blur } else { 0.0 }).sqrt();
                }
                let ys: Vec<f64> = (0..n_le).map(|j| val(layers[j][l][ch])).collect();
                for j in 0..=GRAIN_CLASS_STEPS {
                    let p = (j as f64 / GRAIN_CLASS_STEPS as f64).clamp(1e-6, 1.0 - 1e-6);
                    let d = p * dmax - density_min[ch];
                    let x = if positive { -d } else { d };
                    let lay = np_interp(x, &xs, &ys);
                    let pl = if dmax_l > 0.0 { ((lay + dmin_l) / dmax_l).clamp(1e-6, 1.0 - 1e-6) } else { 1e-6 };
                    lut[(ch * (GRAIN_CLASS_STEPS + 1) + j) * m + l] = pl as f32;
                }
            }
        }
        Self { n: n as u32, lut, var_factor, class_sigma, mean_sigma: grain_blur.max(0.0) }
    }

    /// Developed fraction of class `cls` in channel `ch` for the overall fraction `p` (table,
    /// linear interpolation – same as `grain_classes.wgsl`).
    #[inline]
    pub fn fraction(&self, ch: usize, p: f32, cls: usize) -> f32 {
        let t = p.clamp(0.0, 1.0) * GRAIN_CLASS_STEPS as f32;
        let j = (t as usize).min(GRAIN_CLASS_STEPS - 1);
        let fr = t - j as f32;
        let base = (ch * (GRAIN_CLASS_STEPS + 1) + j) * GRAIN_MAX_CLASSES + cls;
        let lo = self.lut[base];
        let hi = self.lut[base + GRAIN_MAX_CLASSES];
        lo + (hi - lo) * fr
    }
}

/// `np.interp` (right-biased for repeated x, clamped at the ends); `xs` ascending.
pub fn np_interp(x: f64, xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len().min(ys.len());
    if n == 0 {
        return 0.0;
    }
    if x <= xs[0] {
        return if x == xs[0] { ys[xs.partition_point(|&v| v <= x) - 1] } else { ys[0] };
    }
    if x >= xs[n - 1] {
        return ys[n - 1];
    }
    // largest k with xs[k] <= x
    let k = xs[..n].partition_point(|&v| v <= x) - 1;
    let (x0, x1) = (xs[k], xs[k + 1]);
    if x1 == x0 {
        return ys[k];
    }
    ys[k] + (ys[k + 1] - ys[k]) * (x - x0) / (x1 - x0)
}

/// Single-hit model: solve Σ w(1 − e^{−x k}) = p (concave, increasing; Newton from the left,
/// start by Jensen x0 = −ln(1−p)/Σwk) and return 1 − e^{−x k_i}.
fn single_hit_fractions(n: usize, w: &[f32; GRAIN_MAX_CLASSES], k: &[f32; GRAIN_MAX_CLASSES], p: f32) -> [f32; GRAIN_MAX_CLASSES] {
    let p = p.clamp(1e-6, 1.0 - 1e-6) as f64;
    let kbar: f64 = (0..n).map(|i| (w[i] * k[i]) as f64).sum();
    let mut x = -(1.0 - p).ln() / kbar.max(1e-12);
    for _ in 0..50 {
        let (mut f, mut fp) = (-p, 0f64);
        for i in 0..n {
            let e = (-x * k[i] as f64).exp();
            f += w[i] as f64 * (1.0 - e);
            fp += (w[i] * k[i]) as f64 * e;
        }
        if fp <= 0.0 || f.abs() < 1e-12 {
            break;
        }
        x -= f / fp;
    }
    let mut out = [0f32; GRAIN_MAX_CLASSES];
    for i in 0..n {
        out[i] = (1.0 - (-x * k[i] as f64).exp()) as f32;
    }
    out
}

/// Output gamut compression parameters for the GPU-resident per-pixel pass.
/// CPU equivalent: `OutputGamutCompress::compress`. The `C_max(L, h)` table
/// is baked once on the CPU (bisection against the output cube) and uploaded
/// as a storage buffer; the shader only does the per-pixel forward/inverse
/// perceptual transform + Reinhard knee.
#[derive(Debug, Clone, Copy)]
pub struct GamutGpuParams<'a> {
    /// 0 = aces_rgc, 1 = oklch, 2 = oklrab, 3 = cam16ucs — must match the
    /// mode dispatch in `gamut_compress.wgsl`.
    pub mode: u32,
    /// Reinhard knee on normalized chroma: (threshold, limit, power).
    pub knee: [f32; 3],
    /// One-sided lightness compression (threshold, limit, power); `None` off.
    pub lightness: Option<[f32; 3]>,
    /// Perceptual lightness of the output white (1.0 OkLab, ~100 CAM16-UCS).
    pub l_white: f32,
    /// Lightness-axis bounds of the `C_max` table grid.
    pub l_min: f32,
    pub l_max: f32,
    /// `C_max(L, h)` table, row-major `[n_l][n_h]`. Empty for aces_rgc.
    pub cmax: &'a [f64],
    pub n_l: u32,
    pub n_h: u32,
}

/// Unsharp mask parameters: blur sigma in pixels and amount.
#[derive(Debug, Clone, Copy)]
pub struct UnsharpGpuParams {
    pub sigma_px: f32,
    pub amount: f32,
}

/// Viewing glare parameters for the GPU-resident lognormal + blur + apply
/// pass. CPU equivalent: `compute_random_glare_amount` followed by
/// `add_glare_with_amount`.
#[derive(Debug, Clone, Copy)]
pub struct GlareGpuParams {
    /// LogNormal μ and σ derived on the CPU from `percent` + `roughness`.
    pub mu: f32,
    pub sigma: f32,
    pub blur_px: f32,
    pub base_seed: u32,
    /// `(XYZ→RGB) * illuminant_xyz`, pre-divided by 100. The shader
    /// just multiplies the lognormal scalar by this per-channel offset.
    pub rgb_offset: [f32; 3],
}

/// Halation parameters for the GPU-resident scatter + multi-bounce pass.
/// Mirrors the field layout of the CPU `apply_halation_um`, but pre-converts
/// the per-channel µm sigmas to a single pixel-space sigma (the GPU path
/// uses the average across channels, matching the existing CPU impl).
#[derive(Debug, Clone, Copy)]
pub struct HalationGpuParams {
    pub scatter_amount: f32,
    /// graineo: per channel, as in the original (the port averaged the three channels).
    pub scatter_core_px: [f32; 3],
    pub scatter_tail_px: [f32; 3],
    /// Per-channel tail weight. Python applies these per channel
    /// (Portra 0.78 / 0.65 / 0.67); averaging is a noticeable
    /// chromatic drift.
    pub scatter_tail_weight: [f32; 3],
    pub halation_amount: f32,
    pub halation_strength_avg: f32,
    /// Per-channel `halation_strength[c] * halation_amount`. Used only
    /// for the renormalize pass (which is per-channel even though the
    /// blur/add use the averaged scalar). When `halation_renormalize`
    /// is false this is ignored.
    pub halation_a_tot: [f32; 3],
    pub halation_first_sigma_px: [f32; 3],
    pub halation_n_bounces: u32,
    pub halation_bounce_decay: f32,
    pub halation_renormalize: bool,
}

pub struct Lut3D {
    pub size: u32,
    pub data: Vec<f32>,
}

/// Select the best available backend at runtime.
///
/// Honors `SPEKTRAFILM_BACKEND=cpu|cuda|wgpu` when the requested backend is compiled in.
/// Useful for benchmarking and for f64 mode where the GPU shaders truncate to f32.
pub fn select_backend() -> Box<dyn ComputeBackend> {
    let requested = std::env::var("SPEKTRAFILM_BACKEND")
        .ok()
        .map(|v| v.to_ascii_lowercase());

    if requested.as_deref() == Some("cpu") {
        tracing::info!("using CPU backend");
        return Box::new(cpu_backend::CpuBackend);
    }

    // graineo: CUDA backend removed.
    if requested.as_deref() == Some("cuda") {
        tracing::warn!("CUDA backend is not included in this build");
    }

    #[cfg(feature = "wgpu-backend")]
    {
        if requested.as_deref().is_none() || requested.as_deref() == Some("wgpu") {
            if let Some(gpu) = wgpu_backend::WgpuBackend::new() {
                tracing::info!("using wgpu GPU backend");
                return Box::new(gpu);
            }
            if requested.as_deref() == Some("wgpu") {
                tracing::warn!("wgpu backend requested but unavailable; falling back");
            }
        }
    }
    tracing::info!("using CPU backend");
    Box::new(cpu_backend::CpuBackend)
}
