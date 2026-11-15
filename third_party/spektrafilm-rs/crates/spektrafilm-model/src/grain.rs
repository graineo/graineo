// Stochastic grain generation.
// Poisson-binomial particle model with dye cloud blur and lognormal micro-structure.

use spektrafilm_gpu::ComputeBackend;
use spektrafilm_math::gaussian;
use spektrafilm_math::image::ImageBuf;
use spektrafilm_math::precision::{Scalar, ZERO, from_f64};
use rayon::prelude::*;
use std::time::Instant;

fn stage_timings_enabled() -> bool {
    std::env::var_os("SPEKTRAFILM_STAGE_TIMINGS").is_some()
}

fn print_stage_timing(enabled: bool, stage: &str, start: Instant) {
    if enabled {
        eprintln!("stage {stage}: {} ms", start.elapsed().as_millis());
    }
}

/// Apply the Poisson-binomial grain particle model to a single-channel density image.
///
/// Port of Python `layer_particle_model`. Density values flow through in `Scalar`
/// (full f64 precision in `precision-f64` mode); RNG sampling uses f64 directly.
pub fn layer_particle_model(
    density: &[Scalar],
    width: u32,
    height: u32,
    density_max: f64,
    n_particles_per_pixel: f64,
    grain_uniformity: f64,
    seed: u64,
    blur_particle: f32,
) -> Vec<Scalar> {
    // Inputs are f64 to match Python's `layer_particle_model` —
    // density_max/uniformity/agx-area come from JSON profiles at full
    // f64 precision in Python. Passing them as f32 in Rust truncated
    // ~7-decimal noise into `od_particle` and `saturation` which then
    // shifted every Poisson lambda by ~5e-8, producing a different
    // RNG stream and thus visibly different grain patterns.
    let dmax = density_max;
    let od_particle = dmax / n_particles_per_pixel;
    let gu = grain_uniformity;
    let npp = n_particles_per_pixel;

    // Python-bit-exact RNG: numpy's MT19937 + RandomState distribution
    // algorithms (`rk_poisson` / `rk_binomial`). Numpy's seed-to-stream
    // takes a `u32`, so truncate the 64-bit seed — for the CPU export
    // path we never need more than 24 distinct streams (3 channels × N
    // sub-layers). Iteration is single-threaded so the RNG state advances
    // in row-major order exactly like `numpy.random.RandomState(seed)
    // .poisson(lambda_array)` does internally.
    //
    // CRITICAL — match Python's call order: it does
    //   seeds = np.random.poisson(lam_array)   # whole array, then
    //   grain = np.random.binomial(seeds, p)   # whole array.
    // So the RNG must draw ALL Poisson samples first, THEN ALL Binomial
    // samples. Interleaving Poisson/Binomial per pixel (what we used to
    // do) gives a different RNG state at each draw and produces a
    // completely different grain pattern.
    let n = density.len();
    let mut rng = rand_mt::Mt::new(seed as u32);
    let mut p_arr = Vec::with_capacity(n);
    let mut sat_arr = Vec::with_capacity(n);
    for &d in density.iter() {
        let p = ((d as f64) / dmax).clamp(1e-6, 1.0 - 1e-6);
        let saturation = 1.0 - p * gu * (1.0 - 1e-6);
        p_arr.push(p);
        sat_arr.push(saturation);
    }
    let mut seeds = Vec::with_capacity(n);
    for sat in &sat_arr {
        seeds.push(spektrafilm_math::numpy_rng::rk_poisson(&mut rng, npp / sat));
    }
    let mut grain = vec![ZERO; n];
    for (i, slot) in grain.iter_mut().enumerate() {
        let developed = spektrafilm_math::numpy_rng::rk_binomial(&mut rng, seeds[i], p_arr[i]);
        *slot = from_f64((developed as f64) * od_particle * sat_arr[i]);
    }

    if blur_particle > 0.4 {
        // Match Python: `sigma = blur_particle * np.sqrt(od_particle)`
        // — keep the multiplication in f64 and narrow at the end.
        let sigma = (blur_particle as f64 * od_particle.sqrt()) as f32;
        if sigma > 0.1 {
            let mut img = ImageBuf::from_data(
                width,
                height,
                grain.iter().flat_map(|&v| [v, v, v]).collect(),
            );
            img = gaussian::gaussian_blur(&img, sigma);
            grain = img.extract_channel(0);
        }
    }

    grain
}

/// Apply grain to a CMY density image.
///
/// Port of Python `apply_grain_to_density`.
#[allow(clippy::too_many_arguments)]
pub fn apply_grain_to_density(
    density_cmy: &ImageBuf,
    pixel_size_um: f32,
    agx_particle_area_um2: f64,
    agx_particle_scale: [f64; 3],
    density_min: [f64; 3],
    density_max_curves: [f64; 3],
    grain_uniformity: [f64; 3],
    grain_blur: f32,
    n_sub_layers: u32,
    monochrome: bool,
    backend: &dyn ComputeBackend,
) -> ImageBuf {
    let stage_timings = stage_timings_enabled();
    let w = density_cmy.width;
    let h = density_cmy.height;
    let pixel_area = (pixel_size_um as f64) * (pixel_size_um as f64);
    let density_max: [f64; 3] = [
        density_max_curves[0] + density_min[0],
        density_max_curves[1] + density_min[1],
        density_max_curves[2] + density_min[2],
    ];

    let channels: Vec<(usize, Vec<Scalar>)> = (0..3)
        .into_par_iter()
        .map(|ch| {
            let t_ch = Instant::now();
            let particle_area = agx_particle_area_um2 * agx_particle_scale[ch];
            let mut n_particles = pixel_area / particle_area;
            if n_sub_layers > 1 {
                n_particles /= n_sub_layers as f64;
            }

            // Add density_min to input (kept in Scalar precision)
            let dmin_s = from_f64(density_min[ch]);
            let t = Instant::now();
            let density_ch: Vec<Scalar> =
                density_cmy.pixels().map(|px| px[ch] + dmin_s).collect();
            print_stage_timing(stage_timings, "grain.extract_channel", t);

            let mut grain_sum = vec![ZERO; density_ch.len()];

            for sl in 0..n_sub_layers {
                let t = Instant::now();
                // Monochrome (B&W): one shared noise field — every lane draws
                // from channel 0's RNG stream (upstream n_channels==1 has a
                // single emulsion, seed ch=0).
                let seed_ch = if monochrome { 0 } else { ch as u64 };
                let seed = seed_ch + (sl as u64) * 10;
                let g = layer_particle_model(
                    &density_ch,
                    w,
                    h,
                    density_max[ch],
                    n_particles,
                    grain_uniformity[ch],
                    seed,
                    0.0,
                );
                print_stage_timing(stage_timings, "grain.layer_particle_model", t);
                for (s, &v) in grain_sum.iter_mut().zip(g.iter()) {
                    *s += v;
                }
            }

            // Average sub-layers
            let scale = from_f64(1.0 / n_sub_layers as f64);
            for v in &mut grain_sum {
                *v = *v * scale - dmin_s;
            }

            print_stage_timing(stage_timings, "grain.channel_total", t_ch);
            (ch, grain_sum)
        })
        .collect();

    let mut out = ImageBuf::new(w, h);
    for (ch, grain_sum) in channels {
        out.write_channel(ch, &grain_sum);
    }

    // Final blur — typically a few px sigma at 1–6 MP, big enough that the
    // GPU separable kernel wins.
    if grain_blur > 0.4 {
        let t = Instant::now();
        out = backend.gaussian_blur(&out, grain_blur);
        print_stage_timing(stage_timings, "grain.final_blur", t);
    }

    out
}

// ---------------------------------------------------------------------------
// graineo: grain fixed in place (CPU counterpart of `grain.wgsl`).
// ---------------------------------------------------------------------------

#[inline]
fn pcg(state: u32) -> u32 {
    let s = state.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

#[inline]
fn splitmix32(x: u32) -> u32 {
    let mut z = x;
    z = (z ^ (z >> 16)).wrapping_mul(0x85eb_ca6b);
    z = (z ^ (z >> 13)).wrapping_mul(0xc2b2_ae35);
    z ^ (z >> 16)
}

#[inline]
fn standard_normal(state: &mut u32) -> f32 {
    *state = pcg(*state);
    let u1 = *state as f32 * (1.0 / 4_294_967_296.0);
    *state = pcg(*state);
    let u2 = *state as f32 * (1.0 / 4_294_967_296.0);
    let r = (-2.0 * u1.max(1e-7).ln()).sqrt();
    r * (std::f32::consts::TAU * u2).cos()
}

/// Grain like the GPU shader `grain.wgsl` (normal approximation of Poisson and binomial), but the
/// random value per pixel comes from the position in the full image: `origin = [x, y, full_width]`.
/// So crop and full image as well as CPU and GPU show the same grain (up to rounding).
#[allow(clippy::too_many_arguments)]
pub fn apply_grain_hashed(
    density_cmy: &ImageBuf,
    pixel_size_um: f32,
    agx_particle_area_um2: f64,
    agx_particle_scale: [f64; 3],
    density_min: [f64; 3],
    density_max_curves: [f64; 3],
    grain_uniformity: [f64; 3],
    grain_blur: f32,
    n_sub_layers: u32,
    monochrome: bool,
    origin: [u32; 3],
    backend: &dyn ComputeBackend,
) -> ImageBuf {
    use spektrafilm_math::precision::{from_f32, to_f32};
    let w = density_cmy.width;
    let h = density_cmy.height;
    let n_sub = n_sub_layers.max(1);
    let pixel_area = (pixel_size_um as f64) * (pixel_size_um as f64);
    let mut npp = [0f32; 3];
    let mut dmin = [0f32; 3];
    let mut dmax = [0f32; 3];
    let mut gu = [0f32; 3];
    for c in 0..3 {
        npp[c] = ((pixel_area / (agx_particle_area_um2 * agx_particle_scale[c])) / n_sub as f64) as f32;
        dmin[c] = density_min[c] as f32;
        dmax[c] = (density_max_curves[c] + density_min[c]) as f32;
        gu[c] = grain_uniformity[c] as f32;
    }
    let full_w = origin[2].max(w);
    let mut out = ImageBuf::new(w, h);
    out.data
        .par_chunks_mut(w as usize * 3)
        .zip(density_cmy.data.par_chunks(w as usize * 3))
        .enumerate()
        .for_each(|(y, (dst, src))| {
            for x in 0..w as usize {
                let gidx = (origin[1] + y as u32).wrapping_mul(full_w).wrapping_add(origin[0] + x as u32);
                for ch in 0..3 {
                    let od_particle = dmax[ch] / npp[ch];
                    let d_in = to_f32(src[x * 3 + ch]) + dmin[ch];
                    let p = (d_in / dmax[ch]).clamp(1e-6, 1.0 - 1e-6);
                    let saturation = 1.0 - p * gu[ch] * (1.0 - 1e-6);
                    let lambda = npp[ch] / saturation;
                    let mut sum = 0f32;
                    for sl in 0..n_sub {
                        let seed_ch = if monochrome { 0 } else { ch as u32 };
                        let layer_seed = seed_ch + sl * 10;
                        let mut rng = splitmix32(layer_seed) ^ splitmix32(gidx);
                        let z1 = standard_normal(&mut rng);
                        let n_seeds = (lambda + lambda.sqrt() * z1).round().max(0.0);
                        let mean = n_seeds * p;
                        let variance = n_seeds * p * (1.0 - p);
                        let z2 = standard_normal(&mut rng);
                        let developed =
                            if variance > 0.0 { (mean + variance.sqrt() * z2).round().clamp(0.0, n_seeds) } else { mean };
                        sum += developed * od_particle * saturation;
                    }
                    dst[x * 3 + ch] = from_f32(sum / n_sub as f32 - dmin[ch]);
                }
            }
        });
    if grain_blur > 0.4 {
        out = backend.gaussian_blur(&out, grain_blur);
    }
    out
}

/// graineo: polydisperse grain (crystal size classes, see `spektrafilm_gpu::GrainClasses`), fixed in
/// place like [`apply_grain_hashed`] (`origin = [x, y, full_width]`) and computed like
/// `grain_classes.wgsl` (CPU and GPU identical up to rounding).
///
/// Output = blur(density, `mean_sigma`) + Σ_i blur(noise_i, `class_sigma_i`). The mean density
/// is the one of the density curve; each class adds zero-mean noise with the variance of its
/// Poisson-binomial crystal count (normal approximation).
#[allow(clippy::too_many_arguments)]
pub fn apply_grain_classes(
    density_cmy: &ImageBuf,
    pixel_size_um: f32,
    agx_particle_area_um2: f64,
    agx_particle_scale: [f64; 3],
    density_min: [f64; 3],
    density_max_curves: [f64; 3],
    grain_uniformity: [f64; 3],
    grain_blur: f32,
    monochrome: bool,
    origin: [u32; 3],
    classes: &spektrafilm_gpu::GrainClasses,
    backend: &dyn ComputeBackend,
) -> ImageBuf {
    use spektrafilm_math::precision::{from_f32, to_f32};
    let w = density_cmy.width;
    let h = density_cmy.height;
    let pixel_area = (pixel_size_um as f64) * (pixel_size_um as f64);
    let mut npp = [0f32; 3];
    let mut dmin = [0f32; 3];
    let mut dmax = [0f32; 3];
    let mut gu = [0f32; 3];
    for c in 0..3 {
        npp[c] = (pixel_area / (agx_particle_area_um2 * agx_particle_scale[c])) as f32;
        dmin[c] = density_min[c] as f32;
        dmax[c] = (density_max_curves[c] + density_min[c]) as f32;
        gu[c] = grain_uniformity[c] as f32;
    }
    let full_w = origin[2].max(w);
    let mut out = if classes.mean_sigma > 0.05 { backend.gaussian_blur(density_cmy, classes.mean_sigma) } else { density_cmy.clone() };
    for cls in 0..classes.n as usize {
        let mut noise = ImageBuf::new(w, h);
        noise
            .data
            .par_chunks_mut(w as usize * 3)
            .zip(density_cmy.data.par_chunks(w as usize * 3))
            .enumerate()
            .for_each(|(y, (dst, src))| {
                for x in 0..w as usize {
                    let gidx = (origin[1] + y as u32).wrapping_mul(full_w).wrapping_add(origin[0] + x as u32);
                    for ch in 0..3 {
                        let p = ((to_f32(src[x * 3 + ch]) + dmin[ch]) / dmax[ch]).clamp(1e-6, 1.0 - 1e-6);
                        let pi = classes.fraction(ch, p, cls);
                        dst[x * 3 + ch] = from_f32(class_noise(
                            pi,
                            dmax[ch],
                            npp[ch],
                            gu[ch],
                            classes.var_factor[ch][cls],
                            cls,
                            if monochrome { 0 } else { ch as u32 },
                            gidx,
                        ));
                    }
                }
            });
        let sigma = classes.class_sigma[cls];
        if sigma > 0.05 {
            noise = backend.gaussian_blur(&noise, sigma);
        }
        out.data.par_iter_mut().zip(noise.data.par_iter()).for_each(|(o, n)| *o += *n);
    }
    out
}

/// Zero-mean density noise of size class `cls` at one pixel (`pi` = developed fraction of the class).
/// Same arithmetic as `grain_classes.wgsl`.
#[inline]
#[allow(clippy::too_many_arguments)]
fn class_noise(
    pi: f32,
    dmax: f32,
    npp: f32,
    gu: f32,
    var_factor: f32,
    cls: usize,
    seed_ch: u32,
    gidx: u32,
) -> f32 {
    let sat = 1.0 - pi * gu * (1.0 - 1e-6);
    let od = dmax / npp;
    let var = od * od * npp * var_factor * sat * pi;
    let mut rng = splitmix32(seed_ch + 1000 + cls as u32 * 10) ^ splitmix32(gidx);
    var.max(0.0).sqrt() * standard_normal(&mut rng)
}

#[cfg(test)]
mod class_tests {
    use spektrafilm_gpu::GrainClasses;

    #[test]
    fn fractions_keep_mean_density() {
        for reversal in [false, true] {
            let c = GrainClasses::single_hit(4, 6.0, 1.0, reversal, [0.97; 3], 0.65);
            for &p in &[1e-3f32, 0.01, 0.1, 0.3, 0.5, 0.8, 0.95, 0.999] {
                let f: Vec<f32> = (0..4).map(|i| c.fraction(1, p, i)).collect();
                let m: f32 = f.iter().sum::<f32>() / 4.0;
                assert!((m - p).abs() <= 2e-4, "p {p}: mean {m}");
                // negative: large crystals develop first; reversal: they leave dye last
                if reversal {
                    assert!(f[3] <= f[2] && f[2] <= f[1] && f[1] <= f[0], "p {p}: {f:?}");
                } else {
                    assert!(f[3] >= f[2] && f[2] >= f[1] && f[1] >= f[0], "p {p}: {f:?}");
                }
            }
        }
    }

    #[test]
    fn one_class_is_monodisperse() {
        let c = GrainClasses::single_hit(1, 6.0, 1.0, false, [0.97; 3], 0.65);
        assert!((c.fraction(0, 0.37, 0) - 0.37).abs() < 1e-5);
        assert!((c.var_factor[0][0] - 1.0).abs() < 1e-5 && (c.class_sigma[0] - 0.65).abs() < 1e-5);
    }
}

/// graineo: port of upstream's (experimental) sub-layer grain `apply_grain_to_density_layers`
/// together with `interp_density_cmy_layers` and `add_micro_structure` (spektrafilm
/// `model/grain.py`, `model/density_curves.py`): every colour record is split into its sub-layers
/// (fast … slow, `layers[j][l][ch]`, curves of the profile), each with its own density share, crystal
/// area (`particle_scale_layers`) and dye-cloud blur; numpy MT19937 streams `seed = ch + 10·l` as
/// upstream. `curves` = normalised density curves (`density_curves − nanmin`).
///
/// Deviation: the micro-structure (lognormal clumping) uses a position hash instead of numpy's
/// generator; it is only active for pixels smaller than `micro_structure[1]`·0.001/0.05 µm (0.6 µm
/// with the default 30 nm), i.e. practically never.
#[allow(clippy::too_many_arguments)]
pub fn apply_grain_to_density_layers(
    density_cmy: &ImageBuf,
    curves: &[[f64; 3]],
    layers: &[Vec<Vec<f64>>],
    n_layers: usize,
    positive: bool,
    pixel_size_um: f32,
    agx_particle_area_um2: f64,
    agx_particle_scale: [f64; 3],
    particle_scale_layers: &[f64],
    density_min: [f64; 3],
    grain_uniformity: [f64; 3],
    grain_blur: f32,
    blur_dye_clouds_um: f32,
    micro_structure: [f32; 2],
    backend: &dyn ComputeBackend,
) -> ImageBuf {
    use spektrafilm_math::precision::{from_f64, to_f64};
    let w = density_cmy.width;
    let h = density_cmy.height;
    let n_le = curves.len().min(layers.len());
    let val = |v: f64| if v.is_finite() { v } else { 0.0 };
    let pixel_area = (pixel_size_um as f64) * (pixel_size_um as f64);
    let mut out = ImageBuf::new(w, h);
    for ch in 0..3 {
        let mut lmax = vec![f64::NEG_INFINITY; n_layers];
        for j in 0..n_le {
            for l in 0..n_layers {
                let v = layers[j][l][ch];
                if v.is_finite() {
                    lmax[l] = lmax[l].max(v);
                }
            }
        }
        let total: f64 = lmax.iter().sum();
        let xs: Vec<f64> = (0..n_le).map(|j| if positive { -val(curves[j][ch]) } else { val(curves[j][ch]) }).collect();
        let d_in: Vec<f64> = density_cmy.pixels().map(|px| to_f64(px[ch])).collect();
        let mut sum = vec![0f64; d_in.len()];
        for l in 0..n_layers {
            let f = lmax[l] / total;
            let dmin_l = f * density_min[ch];
            let dmax_l = lmax[l] + dmin_l;
            let area = agx_particle_area_um2 * agx_particle_scale[ch] * particle_scale_layers[l];
            let npp = pixel_area * f / area;
            let ys: Vec<f64> = (0..n_le).map(|j| val(layers[j][l][ch])).collect();
            let dens_l: Vec<Scalar> = d_in
                .iter()
                .map(|&d| from_f64(spektrafilm_gpu::np_interp(if positive { -d } else { d }, &xs, &ys) + dmin_l))
                .collect();
            let mut g = layer_particle_model(&dens_l, w, h, dmax_l, npp, grain_uniformity[ch], (ch + l * 10) as u64, 0.0);
            // upstream: `if blur_particle > 0: gaussian(grain, blur_particle·√od_particle)`
            if blur_dye_clouds_um > 0.0 {
                let sigma = (blur_dye_clouds_um as f64 * (dmax_l / npp).sqrt()) as f32;
                g = gaussian::gaussian_blur_channel(&g, w, h, sigma);
            }
            for (s, v) in sum.iter_mut().zip(g.iter()) {
                *s += to_f64(*v);
            }
        }
        out.write_channel(ch, &sum.iter().map(|&v| from_f64(v)).collect::<Vec<_>>());
    }
    // micro-structure: multiplicative lognormal clumping (mean 1, std σ)
    let ms_blur = micro_structure[0] / pixel_size_um;
    let ms_sigma = micro_structure[1] * 0.001 / pixel_size_um;
    if ms_sigma > 0.05 {
        let s_ln = (1.0 + ms_sigma * ms_sigma).ln().sqrt();
        let mu = -0.5 * s_ln * s_ln;
        let mut clump = ImageBuf::new(w, h);
        for (i, v) in clump.data.iter_mut().enumerate() {
            let mut rng = splitmix32(7777) ^ splitmix32(i as u32);
            *v = from_f64((mu + s_ln * standard_normal(&mut rng)).exp() as f64);
        }
        if ms_blur > 0.4 {
            clump = backend.gaussian_blur(&clump, ms_blur);
        }
        for (o, c) in out.data.iter_mut().zip(clump.data.iter()) {
            *o = *o * *c;
        }
    }
    for px in out.data.chunks_exact_mut(3) {
        for ch in 0..3 {
            px[ch] = px[ch] - from_f64(density_min[ch]);
        }
    }
    if grain_blur > 0.0 {
        out = backend.gaussian_blur(&out, grain_blur);
    }
    out
}
