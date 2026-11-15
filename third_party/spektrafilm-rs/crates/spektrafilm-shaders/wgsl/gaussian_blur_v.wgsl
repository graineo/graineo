// Separable Gaussian blur — vertical pass.
// Pairs with `gaussian_blur_h.wgsl`. Reads a column, writes one pixel.

struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> input: array<f32>;
@group(0) @binding(2) var<storage, read> kernel_buf: array<f32>;
@group(0) @binding(3) var<storage, read_write> output: array<f32>;

// graineo: scipy.ndimage mode='reflect' (d c b a | a b c d | d c b a) as in the original's
// fast_gaussian_filter; the port clamped to the edge pixel.
fn reflect_index(i: i32, n: i32) -> u32 {
    let period = 2i * n;
    var j = i % period;
    if j < 0i { j += period; }
    if j >= n { j = period - 1i - j; }
    return u32(j);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if x >= params.width || y >= params.height {
        return;
    }

    var sum = vec3<f32>(0.0, 0.0, 0.0);
    let kernel_size = 2u * params.radius + 1u;
    let h_i32 = i32(params.height);
    for (var k = 0u; k < kernel_size; k++) {
        let dy = i32(k) - i32(params.radius);
        let sy_signed = i32(y) + dy;
        let sy = reflect_index(sy_signed, h_i32);
        let idx = (sy * params.width + x) * 3u;
        let w = kernel_buf[k];
        sum.x += w * input[idx];
        sum.y += w * input[idx + 1u];
        sum.z += w * input[idx + 2u];
    }

    let out_idx = (y * params.width + x) * 3u;
    output[out_idx] = sum.x;
    output[out_idx + 1u] = sum.y;
    output[out_idx + 2u] = sum.z;
}
