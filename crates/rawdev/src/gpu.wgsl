// Compute kernels for full resolution (noise reduction, base sharpening, lens correction).
// Each function matches one step of the CPU version in crates/raw (rauschen.rs, schaerfe.rs,
// objektiv.rs) – same order, same border handling, so that the results agree.
//
// Shared layout: p = parameters (p[0] = width, p[1] = height, more per kernel), a…f = buffers.

@group(0) @binding(0) var<storage, read> p: array<f32>;
@group(0) @binding(1) var<storage, read_write> a: array<f32>;
@group(0) @binding(2) var<storage, read_write> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> c: array<f32>;
@group(0) @binding(4) var<storage, read_write> d: array<f32>;
@group(0) @binding(5) var<storage, read_write> e: array<f32>;
@group(0) @binding(6) var<storage, read_write> f: array<f32>;

fn breite() -> i32 { return i32(p[0]); }
fn hoehe() -> i32 { return i32(p[1]); }

// rauschen.rs: spiegeln()
fn spiegeln(i0: i32, n: i32) -> i32 {
    var i = i0;
    if (i < 0) { i = -i; }
    if (i >= n) { i = 2 * (n - 1) - i; }
    return clamp(i, 0, n - 1);
}

// --- Noise reduction ---------------------------------------------------------------------
// p[3] a, p[4] s2, p[5] gauss (0 = no), p[6] factor 1/√b

fn ans_vor(v: f32) -> f32 {
    if (p[5] > 0.5) { return v * p[6]; }
    let x = v / p[3];
    return 2.0 * sqrt(max(x + 0.375 + p[4], 0.0));
}

fn ans_rueck(z: f32) -> f32 {
    if (p[5] > 0.5) { return z / p[6]; }
    var x: f32;
    if (z > 1.2) {
        let s = 1.2247449;
        let zi = 1.0 / z;
        x = 0.25 * z * z + 0.25 * s * zi - 1.375 * zi * zi + 0.625 * s * zi * zi * zi - 0.125 - p[4];
    } else {
        x = 0.25 * z * z - 0.375 - p[4];
    }
    return x * p[3];
}

// a = RGB (interleaved) → b, c, d = Y, U, V
@compute @workgroup_size(16, 16)
fn yuv_vor(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    if (i32(g.x) >= w || i32(g.y) >= hoehe()) { return; }
    let i = u32(i32(g.y) * w + i32(g.x));
    let r = ans_vor(a[3u * i]);
    let gg = ans_vor(a[3u * i + 1u]);
    let bb = ans_vor(a[3u * i + 2u]);
    b[i] = (r + gg + bb) / 3.0;
    c[i] = 0.5 * (r - bb);
    d[i] = 0.5 * (r + bb) - gg;
}

// b, c, d = Y, U, V → a = RGB
@compute @workgroup_size(16, 16)
fn yuv_rueck(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    if (i32(g.x) >= w || i32(g.y) >= hoehe()) { return; }
    let i = u32(i32(g.y) * w + i32(g.x));
    let y = b[i];
    let u = c[i];
    let v = d[i];
    let gg = y - v * (2.0 / 3.0);
    let rb = y + v / 3.0;
    a[3u * i] = ans_rueck(rb + u);
    a[3u * i + 1u] = ans_rueck(gg);
    a[3u * i + 2u] = ans_rueck(rb - u);
}

// 5-tap filter with step p[2] and weights p[8..13], border mirrored (rauschen.rs: filter5)
@compute @workgroup_size(16, 16)
fn filt_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= hoehe()) { return; }
    let s = i32(p[2]);
    let z = y * w;
    var acc = 0.0;
    for (var t = 0; t < 5; t++) {
        acc += p[8 + t] * a[u32(z + spiegeln(x + (t - 2) * s, w))];
    }
    b[u32(z + x)] = acc;
}

@compute @workgroup_size(16, 16)
fn filt_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let h = hoehe();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let s = i32(p[2]);
    var acc = 0.0;
    for (var t = 0; t < 5; t++) {
        acc += p[8 + t] * a[u32(spiegeln(y + (t - 2) * s, h) * w + x)];
    }
    b[u32(y * w + x)] = acc;
}

// combined: b (current level) → c = vertically filtered a (smoothed), b = detail
// (b − c); p[3] > 0: d = detail² (energy for the threshold), otherwise e += detail
@compute @workgroup_size(16, 16)
fn nr_v_detail(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let h = hoehe();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let s = i32(p[2]);
    var acc = 0.0;
    for (var t = 0; t < 5; t++) {
        acc += p[8 + t] * a[u32(spiegeln(y + (t - 2) * s, h) * w + x)];
    }
    let i = u32(y * w + x);
    let dd = b[i] - acc;
    c[i] = acc;
    b[i] = dd;
    if (p[3] > 0.0) {
        d[i] = dd * dd;
    } else {
        e[i] = e[i] + dd;
    }
}

// combined: average the energy vertically (a = horizontally averaged), shrink detail b
// (threshold² p[3]), c += detail
@compute @workgroup_size(16, 16)
fn nr_v_schrumpfen(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let h = hoehe();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let s = i32(p[2]);
    var en = 0.0;
    for (var t = 0; t < 5; t++) {
        en += p[8 + t] * a[u32(spiegeln(y + (t - 2) * s, h) * w + x)];
    }
    let i = u32(y * w + x);
    let t2 = p[3];
    var k = 0.0;
    if (en > t2) { k = (en - t2) / en; }
    c[i] = c[i] + b[i] * k;
}

fn index(g: vec3<u32>) -> i32 {
    let w = breite();
    if (i32(g.x) >= w || i32(g.y) >= hoehe()) { return -1; }
    return i32(g.y) * w + i32(g.x);
}

// a = a − b (detail = current level − smoothed)
@compute @workgroup_size(16, 16)
fn minus(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    a[u32(i)] = a[u32(i)] - b[u32(i)];
}

// b = a²
@compute @workgroup_size(16, 16)
fn quadrat(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    let v = a[u32(i)];
    b[u32(i)] = v * v;
}

// shrink a (detail) with energy b and threshold² p[3]; c += a
@compute @workgroup_size(16, 16)
fn schrumpfen(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = u32(index(g));
    if (index(g) < 0) { return; }
    let en = b[i];
    let t2 = p[3];
    var k = 0.0;
    if (en > t2) { k = (en - t2) / en; }
    let dd = a[i] * k;
    a[i] = dd;
    c[i] = c[i] + dd;
}

// c += a
@compute @workgroup_size(16, 16)
fn addieren(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    c[u32(i)] = c[u32(i)] + a[u32(i)];
}

// b = a, a = 0 (the level becomes the sum of the details)
@compute @workgroup_size(16, 16)
fn umlegen(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    b[u32(i)] = a[u32(i)];
    a[u32(i)] = 0.0;
}

// --- Base sharpening (Richardson-Lucy, schaerfe.rs) -------------------------------------------
// Gaussian with p[4] taps (odd), weights from p[16], border clamped

@compute @workgroup_size(16, 16)
fn gauss_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= hoehe()) { return; }
    let n = i32(p[4]);
    let r = n / 2;
    let z = y * w;
    var acc = 0.0;
    for (var t = 0; t < n; t++) {
        acc += p[16 + t] * a[u32(z + clamp(x + t - r, 0, w - 1))];
    }
    b[u32(z + x)] = acc;
}

@compute @workgroup_size(16, 16)
fn gauss_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let h = hoehe();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let n = i32(p[4]);
    let r = n / 2;
    var acc = 0.0;
    for (var t = 0; t < n; t++) {
        acc += p[16 + t] * a[u32(clamp(y + t - r, 0, h - 1) * w + x)];
    }
    b[u32(y * w + x)] = acc;
}

// a = RGB → b = luminance (≥ 1e-7), c = estimate (copy)
@compute @workgroup_size(16, 16)
fn luminanz(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    let k = u32(i);
    let y = max((a[3u * k] + a[3u * k + 1u] + a[3u * k + 2u]) / 3.0, 1e-7);
    b[k] = y;
    c[k] = y;
}

// b = a (luminance) / max(b, 1e-7)
@compute @workgroup_size(16, 16)
fn teilen(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    let k = u32(i);
    b[k] = a[k] / max(b[k], 1e-7);
}

// a (estimate) *= clamp(b, 0.5, 2)
@compute @workgroup_size(16, 16)
fn mal_begrenzt(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = index(g);
    if (i < 0) { return; }
    let k = u32(i);
    a[k] = a[k] * clamp(b[k], 0.5, 2.0);
}

// a = RGB (gets sharpened), b = luminance, c = estimate; p[5] strength, p[6] threshold, p[7] clip
@compute @workgroup_size(16, 16)
fn schaerfen(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let h = hoehe();
    let xx = i32(g.x);
    let yy = i32(g.y);
    if (xx >= w || yy >= h) { return; }
    let i = u32(yy * w + xx);
    let clip = p[7];
    if (a[3u * i] >= clip || a[3u * i + 1u] >= clip || a[3u * i + 2u] >= clip) { return; }
    let x0 = max(xx - 1, 0);
    let x1 = min(xx + 1, w - 1);
    let y0 = max(yy - 1, 0);
    let y1 = min(yy + 1, h - 1);
    let gx = sqrt(b[u32(yy * w + x1)]) - sqrt(b[u32(yy * w + x0)]);
    let gy = sqrt(b[u32(y1 * w + xx)]) - sqrt(b[u32(y0 * w + xx)]);
    let gr = sqrt(gx * gx + gy * gy);
    let thr = p[6];
    let m = clamp((gr - thr) / thr, 0.0, 1.0);
    if (m <= 0.0) { return; }
    let fk = clamp(1.0 + p[5] * m * (c[i] / b[i] - 1.0), 0.6, 1.8);
    a[3u * i] = a[3u * i] * fk;
    a[3u * i + 1u] = a[3u * i + 1u] * fk;
    a[3u * i + 2u] = a[3u * i + 2u] * fk;
}

// --- Lens correction (objektiv.rs) ------------------------------------------------------
// grid: p[3] step, p[4] grid width, p[5] grid height

// Grid split at the centre of the correction (raw::lens::Grid::split): p[6]/p[8] = last pixel left of/above the centre,
// p[7]/p[9] = number of grid lines up to it; on each side the lines lie every p[3] pixels counted from the centre, so
// no cell spans the centre lines (a radial model may jump there).
fn gitter_achse(v: i32, last_left: f32, nl: i32, n: i32) -> vec2<f32> {
    let s = p[3];
    if (f32(v) <= last_left) {
        let f = f32(nl - 1) - (last_left - f32(v)) / s;
        let i = clamp(i32(floor(f)), 0, max(nl - 2, 0));
        return vec2<f32>(f32(i), f - f32(i));
    }
    let f = f32(nl) + (f32(v) - last_left - 1.0) / s;
    let i = clamp(i32(floor(f)), nl, max(n - 2, nl));
    return vec2<f32>(f32(i), f - f32(i));
}

fn gitter_pos(x: i32, y: i32) -> vec4<f32> {
    let gx = gitter_achse(x, p[6], i32(p[7]), i32(p[4]));
    let gy = gitter_achse(y, p[8], i32(p[9]), i32(p[5]));
    return vec4<f32>(gx.x, gy.x, gx.y, gy.y);
}

// a = RGB *= factor (b = factor per grid point)
@compute @workgroup_size(16, 16)
fn vignette(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= hoehe()) { return; }
    let gw = i32(p[4]);
    let q = gitter_pos(x, y);
    let ix = i32(q.x);
    let iy = i32(q.y);
    let k00 = b[u32(iy * gw + ix)];
    let k10 = b[u32(iy * gw + ix + 1)];
    let k01 = b[u32((iy + 1) * gw + ix)];
    let k11 = b[u32((iy + 1) * gw + ix + 1)];
    let k = mix(mix(k00, k10, q.z), mix(k01, k11, q.z), q.w);
    let i = u32(y * w + x);
    a[3u * i] = a[3u * i] * k;
    a[3u * i + 1u] = a[3u * i + 1u] * k;
    a[3u * i + 2u] = a[3u * i + 2u] * k;
}

// Lanczos-3 from channel ch of a at position q; weights in d (257 × 6), limited to the four neighbors
fn abtasten(ch: u32, q: vec2<f32>) -> f32 {
    let w = breite();
    let h = hoehe();
    let fx = floor(q.x);
    let fy = floor(q.y);
    let ix = i32(fx);
    let iy = i32(fy);
    // weights linearly between the two nearest of the 257 steps (as lensprof::resample::sample)
    let tx = (q.x - fx) * 256.0;
    let ty = (q.y - fy) * 256.0;
    let sx = min(u32(tx), 255u);
    let sy = min(u32(ty), 255u);
    let ax = tx - f32(sx);
    let ay = ty - f32(sy);
    let x0 = ix - 2;
    let y0 = iy - 2;
    var acc = 0.0;
    var lo = 3.4e38;
    var hi = -3.4e38;
    for (var j = 0; j < 6; j++) {
        let yy = clamp(y0 + j, 0, h - 1);
        var s = 0.0;
        for (var i = 0; i < 6; i++) {
            let xx = clamp(x0 + i, 0, w - 1);
            let z = a[3u * u32(yy * w + xx) + ch];
            let wx = d[sx * 6u + u32(i)] + ax * (d[(sx + 1u) * 6u + u32(i)] - d[sx * 6u + u32(i)]);
            s += wx * z;
            if ((j == 2 || j == 3) && (i == 2 || i == 3)) {
                lo = min(lo, z);
                hi = max(hi, z);
            }
        }
        acc += (d[sy * 6u + u32(j)] + ay * (d[(sy + 1u) * 6u + u32(j)] - d[sy * 6u + u32(j)])) * s;
    }
    return clamp(acc, lo, hi);
}

// c = RGB from a via the source points in grid b (6 values per point), weights in d
@compute @workgroup_size(16, 16)
fn geometrie(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= hoehe()) { return; }
    let gw = i32(p[4]);
    let q = gitter_pos(x, y);
    let ix = i32(q.x);
    let iy = i32(q.y);
    let i = u32(y * w + x);
    for (var ch = 0u; ch < 3u; ch++) {
        let o = ch * 2u;
        let p00 = vec2<f32>(b[u32(iy * gw + ix) * 6u + o], b[u32(iy * gw + ix) * 6u + o + 1u]);
        let p10 = vec2<f32>(b[u32(iy * gw + ix + 1) * 6u + o], b[u32(iy * gw + ix + 1) * 6u + o + 1u]);
        let p01 = vec2<f32>(b[u32((iy + 1) * gw + ix) * 6u + o], b[u32((iy + 1) * gw + ix) * 6u + o + 1u]);
        let p11 = vec2<f32>(b[u32((iy + 1) * gw + ix + 1) * 6u + o], b[u32((iy + 1) * gw + ix + 1) * 6u + o + 1u]);
        let src = mix(mix(p00, p10, q.z), mix(p01, p11, q.z), q.w);
        c[3u * i + ch] = abtasten(ch, src);
    }
}

// crop: b = region of a (RGB interleaved); p[3] = x0, p[4] = y0, p[5] = width of a; output size p[0], p[1]
@compute @workgroup_size(16, 16)
fn zuschneiden(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = breite();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= hoehe()) { return; }
    let src = u32((y + i32(p[4])) * i32(p[5]) + x + i32(p[3]));
    let dst = u32(y * w + x);
    b[3u * dst] = a[3u * src];
    b[3u * dst + 1u] = a[3u * src + 1u];
    b[3u * dst + 2u] = a[3u * src + 2u];
}
