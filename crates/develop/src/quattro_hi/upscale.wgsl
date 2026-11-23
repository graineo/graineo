// Step 3 of the Quattro high-ISO chain on the graphics card (the arithmetic of upscale.rs): smoothing of the top
// layer and the joint filter from the colour grid to the full resolution.
//
// `numbers` starts with 16 settings, then the tables:
//   [0] grid width  [1] grid height  [2] weights per class  [3] 1 = block means (constant share)
//   [4] brightness → share index  [5] constant share  [6] first row of the band the joint filter computes
//   [16 ..] reach per class (1024), weights (1024 × span), share by brightness (16384), spatial weights (4 × 25)

@group(0) @binding(0) var<storage, read> numbers: array<f32>;
@group(0) @binding(1) var<storage, read> denoised: array<f32>;
@group(0) @binding(2) var<storage, read> plain: array<f32>;
@group(0) @binding(3) var<storage, read> colour: array<f32>;
// top layer with its spikes clamped
@group(0) @binding(4) var<storage, read> top: array<f32>;
@group(0) @binding(5) var<storage, read_write> smooth_top: array<f32>;
// colour and brightness ratio per pixel of the band
@group(0) @binding(6) var<storage, read_write> joined: array<vec4<f32>>;

const SETTINGS: u32 = 16u;
const CLASSES: u32 = 1024u;
const SHARES: u32 = 16384u;
const RADIUS: u32 = 5u;

fn denoised_at(i: u32) -> vec3<f32> {
    return vec3<f32>(denoised[3u * i], denoised[3u * i + 1u], denoised[3u * i + 2u]);
}

fn plain_at(i: u32) -> vec3<f32> {
    return vec3<f32>(plain[3u * i], plain[3u * i + 1u], plain[3u * i + 2u]);
}

fn colour_at(i: u32) -> vec3<f32> {
    return vec3<f32>(colour[3u * i], colour[3u * i + 1u], colour[3u * i + 2u]);
}

fn grid_size() -> vec2<u32> {
    return vec2<u32>(u32(numbers[0]), u32(numbers[1]));
}

fn class_of(v: f32) -> u32 {
    return u32(clamp(v, 0.0, 16383.0)) >> 4u;
}

@compute @workgroup_size(16, 16)
fn calm_top(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = grid_size() * 2u;
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let here = id.y * size.x + id.x;
    let centre = top[here];
    var value = centre;
    if (id.x >= RADIUS && id.y >= RADIUS && id.x + RADIUS < size.x && id.y + RADIUS < size.y) {
        let span = u32(numbers[2]);
        let k = class_of(centre);
        let reach = u32(numbers[SETTINGS + k]);
        let weights = SETTINGS + CLASSES + k * span;
        var total = 0.0;
        var weight = 0.0;
        for (var y = id.y - RADIUS; y <= id.y + RADIUS; y++) {
            for (var x = id.x - RADIUS; x <= id.x + RADIUS; x++) {
                let n = top[y * size.x + x];
                let j = class_of(n);
                let apart = max(j, k) - min(j, k);
                if (apart <= reach) {
                    let wt = numbers[weights + apart];
                    total += wt * n;
                    weight += wt;
                }
            }
        }
        value = total / weight;
    }
    if (numbers[3] != 0.0) {
        let share = numbers[5];
        value = share * value + (1.0 - share) * centre;
    } else {
        let at = u32(clamp(round(numbers[4] * value), 0.0, f32(SHARES - 1u)));
        let share = clamp(numbers[SETTINGS + CLASSES + CLASSES * u32(numbers[2]) + at], 0.0, 1.0);
        value = value * share + centre * (1.0 - share);
    }
    smooth_top[here] = value;
}

// Taps of the 2× cubic enlargement: first source cell and the four weights for output position `i`.
fn first_tap(i: u32) -> i32 {
    return i32(i / 2u) - 2 + i32(i % 2u);
}

fn tap_weights(i: u32) -> vec4<f32> {
    if (i % 2u == 0u) {
        return vec4<f32>(-0.0234375, 0.2265625, 0.8671875, -0.0703125);
    }
    return vec4<f32>(-0.0703125, 0.8671875, 0.2265625, -0.0234375);
}

@compute @workgroup_size(16, 16)
fn join(@builtin(global_invocation_id) band: vec3<u32>) {
    let grid = grid_size();
    let size = grid * 2u;
    let id = vec2<u32>(band.x, band.y + u32(numbers[6]));
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let here = id.y * size.x + id.x;
    let last = vec2<i32>(grid) - vec2<i32>(1);
    // guide: the unfiltered grid enlarged (rows first), times the ratio of the top layer to its enlarged value
    let column_weights = tap_weights(id.x);
    let row_weights = tap_weights(id.y);
    let first = vec2<i32>(first_tap(id.x), first_tap(id.y));
    var enlarged = vec3<f32>(0.0);
    for (var t = 0; t < 4; t++) {
        let row = u32(clamp(first.y + t, 0, last.y)) * grid.x;
        var along = vec3<f32>(0.0);
        for (var s = 0; s < 4; s++) {
            along += column_weights[s] * plain_at(row + u32(clamp(first.x + s, 0, last.x)));
        }
        enlarged += row_weights[t] * along;
    }
    enlarged = trunc(clamp(enlarged, vec3<f32>(0.0), vec3<f32>(32767.0)));
    let lift = (max(top[here], 0.0) + 5.0) / (enlarged.z + 5.0);
    // 16-bit signed values that wrap, as stored
    let wrapped = ((vec3<i32>(enlarged * lift + 0.5) + 32768) & vec3<i32>(65535)) - 32768;
    let guide = vec3<f32>(wrapped);
    let peak = max(guide.x, max(guide.y, guide.z));
    var spread = 25.0;
    if (peak > 1000.0) {
        spread = peak * 0.025;
    }
    let own = vec2<i32>(id / 2u);
    let near = SETTINGS + CLASSES + CLASSES * u32(numbers[2]) + SHARES + 25u * (2u * (id.y % 2u) + id.x % 2u);
    var weight = 0.0;
    var top_sum = 0.0;
    var sum = vec3<f32>(0.0);
    for (var gy = max(own.y - 2, 0); gy <= min(own.y + 2, last.y); gy++) {
        for (var gx = max(own.x - 2, 0); gx <= min(own.x + 2, last.x); gx++) {
            let at = u32(gy) * grid.x + u32(gx);
            let cell = denoised_at(at);
            let d = abs(cell - guide);
            let apart = d.x + d.y + d.z;
            let wt = numbers[near + u32(gy - own.y + 2) * 5u + u32(gx - own.x + 2)] * (1.0 / (apart / spread + 1.0));
            top_sum += cell.z * wt;
            sum += wt * colour_at(at);
            weight += wt;
        }
    }
    let grid_top = top_sum / weight;
    var ratio = 1.0;
    if (grid_top > 0.0) {
        let t = smooth_top[here];
        ratio = (max(t, 0.0) + 5.0) / (grid_top + 5.0);
        if (t >= 11000.0 && ratio < 1.0) {
            let fade = clamp((t - 11000.0) / 3000.0, 0.0, 1.0);
            ratio = (ratio - 1.0) * (1.0 - fade) + 1.0;
        }
    }
    let home = denoised_at(u32(own.y) * grid.x + u32(own.x));
    let excess = ((home.x + home.y) - 2.0 * home.z) / max(home.z, 1e-4);
    let gate = 1.0 - 0.3 / (exp(excess * -4.0 + 7.0) + 1.0);
    ratio = clamp(pow(ratio, gate), 0.001, 1000.0);
    let rgb = trunc(min(clamp(sum / weight, vec3<f32>(0.0), vec3<f32>(65535.0)) + 0.5, vec3<f32>(65535.0)));
    joined[band.y * size.x + band.x] = vec4<f32>(rgb, ratio);
}
