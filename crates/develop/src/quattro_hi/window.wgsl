// Range filter of the Quattro high-ISO chain on the graphics card: one invocation per grid cell, the same
// arithmetic as the processor path (grid_denoise.rs).

struct Job {
    width: u32,
    height: u32,
    radius: u32,
    spare: u32,
}

@group(0) @binding(0) var<storage, read> job: Job;
// the image the window runs over, three values per cell
@group(0) @binding(1) var<storage, read> image: array<f32>;
// per cell: the factors that turn squared layer differences into steps of the weight curve
@group(0) @binding(2) var<storage, read> centre_data: array<f32>;
@group(0) @binding(3) var<storage, read> curve: array<f32>;
@group(0) @binding(4) var<storage, read_write> result: array<f32>;

fn cell(x: i32, y: i32) -> vec3<f32> {
    let cx = u32(clamp(x, 0, i32(job.width) - 1));
    let cy = u32(clamp(y, 0, i32(job.height) - 1));
    let at = 3u * (cy * job.width + cx);
    return vec3<f32>(image[at], image[at + 1u], image[at + 2u]);
}

// Range filter: weighted mean of the window, the weight looked up by the squared distance to the centre in steps
// of the weight curve.
@compute @workgroup_size(16, 16)
fn range_mean(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= job.width || id.y >= job.height) {
        return;
    }
    let here = 3u * (id.y * job.width + id.x);
    let centre = vec3<f32>(image[here], image[here + 1u], image[here + 2u]);
    let factor = vec3<f32>(centre_data[here], centre_data[here + 1u], centre_data[here + 2u]);
    let r = i32(job.radius);
    var total = 0.0;
    var sum = vec3<f32>(0.0);
    for (var dy = -r; dy <= r; dy++) {
        for (var dx = -r; dx <= r; dx++) {
            let n = cell(i32(id.x) + dx, i32(id.y) + dy);
            let d = n - centre;
            let step = ((d.z * d.z * factor.z + d.y * d.y * factor.y) + d.x * d.x * factor.x) + 0.5;
            if (step < f32(arrayLength(&curve))) {
                let weight = curve[u32(step)];
                total += weight;
                sum += weight * n;
            }
        }
    }
    var mean = centre;
    if (total > 0.0) {
        mean = sum / total;
    }
    result[here] = mean.x;
    result[here + 1u] = mean.y;
    result[here + 2u] = mean.z;
}
