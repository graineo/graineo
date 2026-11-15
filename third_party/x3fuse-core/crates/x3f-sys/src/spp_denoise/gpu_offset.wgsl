// graineo addition: patch fits of the Merrill row offset correction (`cr_offset`) on the GPU.
// One workgroup per patch: polynomial fit through the pseudoinverse, residual, correlation gate
// and the row means of the residual for accepted patches. The accumulation into the row
// accumulators and the final application stay on the CPU (fixed order).
//
// Output per patch (`1 + 4·n` floats): accepted flag, then per patch row y the mean of the
// residual over the valid columns (3 floats) and the number of valid columns.

struct Params {
    stride: u32,   // floats per image row
    n: u32,        // patch size
    terms: u32,
    area: u32,     // n·n
    patches: u32,
    x_limit: f32,
    size_f: f32,
    pad: u32,
}

@group(0) @binding(0) var<storage, read> P: Params;
@group(0) @binding(1) var<storage, read> tabs: array<f32>;   // inverse (terms·area), then basis (area·terms)
@group(1) @binding(0) var<storage, read_write> img: array<f32>;
@group(1) @binding(1) var<storage, read_write> patches_buf: array<u32>; // x0, y0, px per patch
@group(1) @binding(2) var<storage, read_write> outp: array<f32>;

const WG: u32 = 256u;
var<workgroup> red: array<f32, 256>;
var<workgroup> coef: array<f32, 18>;
var<workgroup> stats: array<f32, 12>;

fn reduce(v: f32, lid: u32) -> f32 {
    red[lid] = v;
    workgroupBarrier();
    for (var s = WG / 2u; s > 0u; s = s / 2u) {
        if (lid < s) { red[lid] = red[lid] + red[lid + s]; }
        workgroupBarrier();
    }
    let r = red[0];
    workgroupBarrier();
    return r;
}

fn px(x0: u32, y0: u32, i: u32) -> vec3<f32> {
    let x = i / P.n; let y = i % P.n;
    let b = (y0 + y) * P.stride + 3u * (x0 + x);
    return vec3<f32>(img[b], img[b + 1u], img[b + 2u]);
}

fn fit(i: u32) -> vec3<f32> {
    var f = vec3<f32>(0.0);
    let off = P.terms * P.area;
    for (var k = 0u; k < P.terms; k++) {
        let w = tabs[off + i * P.terms + k];
        f = fma(vec3<f32>(w), vec3<f32>(coef[k * 3u], coef[k * 3u + 1u], coef[k * 3u + 2u]), f);
    }
    return f;
}

@compute @workgroup_size(256, 1, 1)
fn offset_patch(@builtin(local_invocation_index) lid: u32, @builtin(workgroup_id) wg: vec3<u32>) {
    let p = wg.x + wg.y * 65535u;
    if (p >= P.patches) { return; }
    let x0 = patches_buf[3u * p]; let y0 = patches_buf[3u * p + 1u]; let pxi = patches_buf[3u * p + 2u];
    let n = P.n;
    // 1. coefficients
    for (var k = 0u; k < P.terms; k++) {
        var acc = vec3<f32>(0.0);
        for (var i = lid; i < P.area; i += WG) {
            acc = fma(vec3<f32>(tabs[k * P.area + i]), px(x0, y0, i), acc);
        }
        let a = reduce(acc.x, lid); let b = reduce(acc.y, lid); let c = reduce(acc.z, lid);
        if (lid == 0u) { coef[k * 3u] = a; coef[k * 3u + 1u] = b; coef[k * 3u + 2u] = c; }
        workgroupBarrier();
    }
    // 2. sums of the raw values and of the residual
    var raw = vec3<f32>(0.0);
    var res = vec3<f32>(0.0);
    for (var i = lid; i < P.area; i += WG) {
        let d = px(x0, y0, i);
        raw += d;
        res += d - fit(i);
    }
    let r0 = reduce(raw.x, lid); let r1 = reduce(raw.y, lid); let r2 = reduce(raw.z, lid);
    let s0 = reduce(res.x, lid); let s1 = reduce(res.y, lid); let s2 = reduce(res.z, lid);
    let area = P.size_f * P.size_f;
    let mean = vec3<f32>(s0, s1, s2) / area;
    // 3. variances and covariances of the residual
    var v = vec3<f32>(0.0);
    var cv = vec3<f32>(0.0);
    for (var i = lid; i < P.area; i += WG) {
        let d = px(x0, y0, i) - fit(i) - mean;
        v = fma(d, d, v);
        cv = fma(vec3<f32>(d.x, d.x, d.y), vec3<f32>(d.y, d.z, d.z), cv);
    }
    let v0 = reduce(v.x, lid); let v1 = reduce(v.y, lid); let v2 = reduce(v.z, lid);
    let c0 = reduce(cv.x, lid); let c1 = reduce(cv.y, lid); let c2 = reduce(cv.z, lid);
    let rg = abs(c0) / (sqrt(v0) * sqrt(v1));
    let rb = abs(c1) / (sqrt(v0) * sqrt(v2));
    let gb = abs(c2) / (sqrt(v1) * sqrt(v2));
    var corr = max(gb, rb);
    if (rg > corr) { corr = rg; }
    var thr = 0.06;
    if (r0 / area > 600.0 && r1 / area > 600.0 && r2 / area > 600.0) { thr = 0.1; }
    let base = p * (1u + 4u * n);
    // strict comparison: NaN (constant channels) is rejected
    let accepted = corr < thr;
    if (lid == 0u) { outp[base] = select(0.0, 1.0, accepted); }
    if (!accepted) { return; }
    // 4. row means of the residual over the valid columns
    for (var y = lid; y < n; y += WG) {
        var sum = vec3<f32>(0.0);
        var count = 0.0;
        for (var x = 0u; x < n; x++) {
            if (fma(f32(pxi), P.size_f, f32(x)) < P.x_limit) {
                let i = x * n + y;
                sum += px(x0, y0, i) - fit(i);
                count += 1.0;
            }
        }
        let o = base + 1u + 4u * y;
        if (count != 0.0) { sum = sum / count; }
        outp[o] = sum.x; outp[o + 1u] = sum.y; outp[o + 2u] = sum.z; outp[o + 3u] = count;
    }
}
