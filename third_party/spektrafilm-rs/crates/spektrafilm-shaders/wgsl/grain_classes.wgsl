// graineo: polydisperse film grain (crystal size classes).
//
// CPU equivalent: `spektrafilm_model::grain::apply_grain_classes`; model in
// `spektrafilm_gpu::GrainClasses`. One pipeline, three modes (per dispatch its own uniform):
//   mode 0: dens = density_cmy                  (copy of the noise-free density)
//   mode 1: noise = zero-mean density noise of class `cls`, from dens
//   mode 2: density_cmy += noise                (after the class blur)
// The host blurs density_cmy (grain_blur) after mode 0 and `noise` (grain_blur·blur_factor) after each
// mode 1, so the result is blur(density) + Σ blur_i(noise_i).
// (blur_i = grain_blur · blur_factor_i)

struct Params {
    n_pixels: u32,
    mode: u32,
    cls: u32,
    monochrome: u32,
    origin_x: u32,
    origin_y: u32,
    full_width: u32,
    width: u32,
    n_classes: u32,
    base_seed: u32,
    lut_steps: u32,
    lut_stride: u32,                   // = GRAIN_MAX_CLASSES
    density_min: vec4<f32>,            // .xyz used
    density_max: vec4<f32>,            // .xyz used (already includes density_min)
    n_particles_per_pixel: vec4<f32>,  // .xyz used (per full pixel)
    grain_uniformity: vec4<f32>,       // .xyz used
    var_factor: array<vec4<f32>, 6>,   // [ch][class]: 2 × vec4 per channel
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> density_cmy: array<f32>;
@group(0) @binding(2) var<storage, read_write> dens: array<f32>;
@group(0) @binding(3) var<storage, read_write> noise: array<f32>;
// developed fraction per class over p: lut[(ch·(steps+1) + j)·stride + class]
@group(0) @binding(4) var<storage, read> lut: array<f32>;

fn pcg(state: u32) -> u32 {
    var s: u32 = state * 747796405u + 2891336453u;
    let word: u32 = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (word >> 22u) ^ word;
}

fn unit_f32(x: u32) -> f32 {
    return f32(x) * (1.0 / 4294967296.0);
}

fn splitmix32(x: u32) -> u32 {
    var z: u32 = x;
    z = (z ^ (z >> 16u)) * 0x85ebca6bu;
    z = (z ^ (z >> 13u)) * 0xc2b2ae35u;
    return z ^ (z >> 16u);
}

fn next_u32(state: ptr<function, u32>) -> u32 {
    let s = pcg(*state);
    *state = s;
    return s;
}

fn standard_normal(state: ptr<function, u32>) -> f32 {
    let u1 = unit_f32(next_u32(state));
    let u2 = unit_f32(next_u32(state));
    let r = sqrt(-2.0 * log(max(u1, 1e-7)));
    return r * cos(6.28318530717958647 * u2);
}

fn cv(ch: u32, i: u32) -> f32 { return params.var_factor[ch * 2u + i / 4u][i % 4u]; }

// Developed fraction of class `cls` in channel `ch` (table, linear interpolation).
fn class_fraction(ch: u32, p: f32, cls: u32) -> f32 {
    let t = clamp(p, 0.0, 1.0) * f32(params.lut_steps);
    let j = min(u32(t), params.lut_steps - 1u);
    let fr = t - f32(j);
    let base = (ch * (params.lut_steps + 1u) + j) * params.lut_stride + cls;
    let lo = lut[base];
    let hi = lut[base + params.lut_stride];
    return lo + (hi - lo) * fr;
}

@compute @workgroup_size(1024)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.n_pixels { return; }
    let base = idx * 3u;
    if params.mode == 0u {
        for (var ch: u32 = 0u; ch < 3u; ch++) {
            dens[base + ch] = density_cmy[base + ch];
        }
        return;
    }
    if params.mode == 2u {
        for (var ch: u32 = 0u; ch < 3u; ch++) {
            density_cmy[base + ch] = density_cmy[base + ch] + noise[base + ch];
        }
        return;
    }
    let gidx = (params.origin_y + idx / params.width) * params.full_width
        + params.origin_x + idx % params.width;
    let cls = params.cls;
    for (var ch: u32 = 0u; ch < 3u; ch++) {
        let dmax = params.density_max[ch];
        let d = dens[base + ch] + params.density_min[ch];
        let p = clamp(d / dmax, 1e-6, 1.0 - 1e-6);
        let pi = class_fraction(ch, p, cls);
        let sat = 1.0 - pi * params.grain_uniformity[ch] * (1.0 - 1e-6);
        let od = dmax / params.n_particles_per_pixel[ch];
        let variance = od * od * params.n_particles_per_pixel[ch] * cv(ch, cls) * sat * pi;
        var seed_ch = ch;
        if params.monochrome != 0u {
            seed_ch = 0u;
        }
        var rng = splitmix32(seed_ch + 1000u + cls * 10u + params.base_seed) ^ splitmix32(gidx);
        noise[base + ch] = sqrt(max(variance, 0.0)) * standard_normal(&rng);
    }
}
