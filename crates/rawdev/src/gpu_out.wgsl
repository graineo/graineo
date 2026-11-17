// Output stage on the GPU (crates/rawdev/src/render.rs: Params::pixel / ausgabe, Stufe::maske in doc.rs).
// Matches the CPU version (reference) step by step: white balance/matrix, exposure, shadows/highlights/
// whites/blacks via the soft luminance map, sigmoid with hue preservation, sRGB, standard look,
// presence, gamut, black & white, encoding, camera look; result 16 bit (two values per u32).
//
// Shared layout as in gpu.wgsl: p = parameters, a…f = buffers; f is atomic u32 here (output).

@group(0) @binding(0) var<storage, read> p: array<f32>;
@group(0) @binding(1) var<storage, read_write> a: array<f32>;
@group(0) @binding(2) var<storage, read_write> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> c: array<f32>;
@group(0) @binding(4) var<storage, read_write> d: array<f32>;
@group(0) @binding(5) var<storage, read_write> e: array<f32>;
@group(0) @binding(6) var<storage, read_write> f: array<atomic<u32>>;

const MITTELGRAU: f32 = 0.1845;
const LOG_MIN: f32 = -16.0;
const LOG_MAX: f32 = 8.0;
const TON_N: f32 = 8192.0;

// ---- Luminance map (Stufe::maske) ----------------------------------------------------
// aus_mask_summe: a = RGB (camera, after detail stage) → b = mean of log2(luminance) per block
//   p[0], p[1] = image width/height; p[2] = block k; p[3], p[4] = grid width/height; p[5..8] = luminance weights
@compute @workgroup_size(16, 16)
fn aus_mask_summe(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let k = i32(p[2]);
    let sw = i32(p[3]);
    let sh = i32(p[4]);
    let bx = i32(g.x);
    let by = i32(g.y);
    if (bx >= sw || by >= sh) { return; }
    let x0 = bx * k;
    let y0 = by * k;
    let x1 = min(x0 + k, w);
    let y1 = min(y0 + k, h);
    var s = 0.0;
    for (var y = y0; y < y1; y++) {
        for (var x = x0; x < x1; x++) {
            let i = u32(y * w + x);
            let l = p[5] * a[3u * i] + p[6] * a[3u * i + 1u] + p[7] * a[3u * i + 2u];
            s += log2(max(l, 1e-7));
        }
    }
    b[u32(by * sw + bx)] = s / f32(max((x1 - x0) * (y1 - y0), 1));
}

// box_blur horizontal (radius 2, edge clamped, fixed window size): a → b; p[0], p[1] = grid size
@compute @workgroup_size(16, 16)
fn aus_box_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    var acc = 0.0;
    for (var t = -2; t <= 2; t++) {
        acc += a[u32(y * w + clamp(x + t, 0, w - 1))];
    }
    b[u32(y * w + x)] = acc / 5.0;
}

// box_blur vertical (radius 2, window shortened at the edge): a → b
@compute @workgroup_size(16, 16)
fn aus_box_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let y0 = max(y - 2, 0);
    let y1 = min(y + 2, h - 1);
    var acc = 0.0;
    for (var yy = y0; yy <= y1; yy++) {
        acc += a[u32(yy * w + x)];
    }
    b[u32(y * w + x)] = acc / f32(y1 - y0 + 1);
}

// ---- Noise brake: base layer (develop/src/lift.rs base_layer) -----------------------------
// Edge-preserving base layer: guided filter of each channel with the luminance as guide, regularised by the
// luminance noise model (estimated on the CPU from the small luminance d). Above 4 MP at half size
// (2×2 mean, radius halved), interpolated back in aus_pixel. Vector buffers hold 4 values per pixel.

// aus_bb_klein: a = RGB (full) → b = (R, G, B, Y), c = (R·Y, G·Y, B·Y, Y·Y), d = Y; p[0], p[1] = small size,
// p[2] = full width, p[3] = factor (1 or 2)
@compute @workgroup_size(16, 16)
fn aus_bb_klein(@builtin(global_invocation_id) g: vec3<u32>) {
    let sw = u32(p[0]);
    let sh = u32(p[1]);
    let w = u32(p[2]);
    if (g.x >= sw || g.y >= sh) { return; }
    var v = vec3<f32>(0.0);
    if (p[3] > 1.5) {
        let i00 = (2u * g.y * w + 2u * g.x) * 3u;
        let i10 = i00 + 3u;
        let i01 = i00 + 3u * w;
        let i11 = i01 + 3u;
        // no writes at a variable index into local vectors (DirectX/FXC)
        v = vec3<f32>(
            0.25 * (a[i00] + a[i10] + a[i01] + a[i11]),
            0.25 * (a[i00 + 1u] + a[i10 + 1u] + a[i01 + 1u] + a[i11 + 1u]),
            0.25 * (a[i00 + 2u] + a[i10 + 2u] + a[i01 + 2u] + a[i11 + 2u]));
    } else {
        let i = (g.y * w + g.x) * 3u;
        v = vec3<f32>(a[i], a[i + 1u], a[i + 2u]);
    }
    let yl = 0.2126 * v.x + 0.7152 * v.y + 0.0722 * v.z;
    let o = g.y * sw + g.x;
    b[4u * o] = v.x;
    b[4u * o + 1u] = v.y;
    b[4u * o + 2u] = v.z;
    b[4u * o + 3u] = yl;
    c[4u * o] = v.x * yl;
    c[4u * o + 1u] = v.y * yl;
    c[4u * o + 2u] = v.z * yl;
    c[4u * o + 3u] = yl * yl;
    d[o] = yl;
}

// box mean horizontal / vertical of a vector buffer (window shortened at the edge): a → b; p[0], p[1] = size,
// p[2] = radius
@compute @workgroup_size(16, 16)
fn aus_bb_box_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let r = i32(p[2]);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let x0 = max(x - r, 0);
    let x1 = min(x + r + 1, w);
    var acc = vec4<f32>(0.0);
    for (var t = x0; t < x1; t++) {
        let k = 4u * u32(y * w + t);
        acc += vec4<f32>(a[k], a[k + 1u], a[k + 2u], a[k + 3u]);
    }
    let o = 4u * u32(y * w + x);
    let m = acc / f32(x1 - x0);
    b[o] = m.x;
    b[o + 1u] = m.y;
    b[o + 2u] = m.z;
    b[o + 3u] = m.w;
}

@compute @workgroup_size(16, 16)
fn aus_bb_box_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let r = i32(p[2]);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let y0 = max(y - r, 0);
    let y1 = min(y + r + 1, h);
    var acc = vec4<f32>(0.0);
    for (var t = y0; t < y1; t++) {
        let k = 4u * u32(t * w + x);
        acc += vec4<f32>(a[k], a[k + 1u], a[k + 2u], a[k + 3u]);
    }
    let o = 4u * u32(y * w + x);
    let m = acc / f32(y1 - y0);
    b[o] = m.x;
    b[o + 1u] = m.y;
    b[o + 2u] = m.z;
    b[o + 3u] = m.w;
}

// aus_bb_ab: coefficients of the guided filter, in place: a = box means (R, G, B, Y) → (aR, aG, aB, 0),
// b = box means (R·Y, G·Y, B·Y, Y·Y) → (bR, bG, bB, 0); p[0], p[1] = size, p[2] = k, p[3], p[4] = noise model a, b
@compute @workgroup_size(16, 16)
fn aus_bb_ab(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = u32(p[0]);
    let h = u32(p[1]);
    if (g.x >= w || g.y >= h) { return; }
    let o = 4u * (g.y * w + g.x);
    let mu = vec4<f32>(a[o], a[o + 1u], a[o + 2u], a[o + 3u]);
    let cy = vec4<f32>(b[o], b[o + 1u], b[o + 2u], b[o + 3u]);
    let var_y = max(cy.w - mu.w * mu.w, 0.0);
    let eps = p[2] * (p[3] * max(mu.w, 0.0) + p[4]);
    let ai = (cy.xyz - mu.xyz * mu.w) / (var_y + eps);
    let bi = mu.xyz - ai * mu.w;
    a[o] = ai.x;
    a[o + 1u] = ai.y;
    a[o + 2u] = ai.z;
    b[o] = bi.x;
    b[o + 1u] = bi.y;
    b[o + 2u] = bi.z;
    a[o + 3u] = 0.0;
    b[o + 3u] = 0.0;
}

// aus_bb_basis: base = mean(a)·Y + mean(b), in place into a; c = Y; p[0], p[1] = size
@compute @workgroup_size(16, 16)
fn aus_bb_basis(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = u32(p[0]);
    let h = u32(p[1]);
    if (g.x >= w || g.y >= h) { return; }
    let i = g.y * w + g.x;
    let o = 4u * i;
    let yl = c[i];
    for (var ch = 0u; ch < 3u; ch++) {
        a[o + ch] = a[o + ch] * yl + b[o + ch];
    }
}

// base layer at full-resolution pixel (x, y) from buffer e (4 values per pixel, size p[59] × p[60], factor p[58])
fn brake_base(x: i32, y: i32, w: i32) -> vec3<f32> {
    let sw = i32(p[59]);
    let sh = i32(p[60]);
    if (p[58] < 1.5) {
        let o = 4u * u32(y * w + x);
        return vec3<f32>(e[o], e[o + 1u], e[o + 2u]);
    }
    let fy = clamp((f32(y) - 0.5) / 2.0, 0.0, f32(sh - 1));
    let y0 = i32(floor(fy));
    let ty = fy - f32(y0);
    let y1 = min(y0 + 1, sh - 1);
    let fx = clamp((f32(x) - 0.5) / 2.0, 0.0, f32(sw - 1));
    let x0 = i32(floor(fx));
    let tx = fx - f32(x0);
    let x1 = min(x0 + 1, sw - 1);
    let k00 = 4u * u32(y0 * sw + x0);
    let k10 = 4u * u32(y0 * sw + x1);
    let k01 = 4u * u32(y1 * sw + x0);
    let k11 = 4u * u32(y1 * sw + x1);
    let top = vec3<f32>(e[k00], e[k00 + 1u], e[k00 + 2u]) * (1.0 - tx) + vec3<f32>(e[k10], e[k10 + 1u], e[k10 + 2u]) * tx;
    let bot = vec3<f32>(e[k01], e[k01 + 1u], e[k01 + 2u]) * (1.0 - tx) + vec3<f32>(e[k11], e[k11 + 1u], e[k11 + 2u]) * tx;
    return top * (1.0 - ty) + bot * ty;
}

// ---- Pixel ------------------------------------------------------------------------------

fn sm(a0: f32, b0: f32, x: f32) -> f32 {
    let t = clamp((x - a0) / (b0 - a0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn ton_eval(x: f32) -> f32 {
    // table in c (8193 values)
    if (x <= 0.0) { return c[0] * min(max(x, 0.0) / exp2(LOG_MIN), 1.0); }
    let l = log2(x);
    if (l <= LOG_MIN) { return c[0] * x / exp2(LOG_MIN); }
    let t = min((l - LOG_MIN) / (LOG_MAX - LOG_MIN) * TON_N, TON_N - 1e-3);
    let i = u32(t);
    let fr = t - f32(i);
    return c[i] * (1.0 - fr) + c[i + 1u] * fr;
}

fn desaturate_negative(v: vec3<f32>) -> vec3<f32> {
    let avg = max((v.x + v.y + v.z) / 3.0, 0.0);
    let mn = min(v.x, min(v.y, v.z));
    var fk = 1.0;
    if (mn < 0.0) { fk = -avg / (mn - avg); }
    return vec3<f32>(avg) + fk * (v - vec3<f32>(avg));
}

// (min, mid, max) as channel indices
fn channel_order(v: vec3<f32>) -> vec3<u32> {
    if (v.x >= v.y) {
        if (v.y > v.z) { return vec3<u32>(2u, 1u, 0u); }
        if (v.z > v.x) { return vec3<u32>(1u, 0u, 2u); }
        if (v.z > v.y) { return vec3<u32>(1u, 2u, 0u); }
        return vec3<u32>(2u, 1u, 0u);
    }
    if (v.x >= v.z) { return vec3<u32>(2u, 0u, 1u); }
    if (v.z > v.y) { return vec3<u32>(0u, 1u, 2u); }
    return vec3<u32>(0u, 2u, 1u);
}

fn preserve_hue_and_energy(pin: vec3<f32>, per: vec3<f32>, o3: vec3<u32>, hp: f32) -> vec3<f32> {
    let mn = o3.x;
    let md = o3.y;
    let mx = o3.z;
    let chroma = pin[mx] - pin[mn];
    var midscale = 0.0;
    if (chroma != 0.0) { midscale = (pin[md] - pin[mn]) / chroma; }
    let full = per[mn] + (per[mx] - per[mn]) * midscale;
    let naive_mid = (1.0 - hp) * per[md] + hp * full;
    let per_energy = per.x + per.y + per.z;
    let naive_energy = per[mn] + naive_mid + per[mx];
    let mm = pin[mn] + pin[md];
    var blend = 0.0;
    if (mm != 0.0) { blend = 2.0 * pin[mn] / mm; }
    let target_e = blend * per_energy + (1.0 - blend) * naive_energy;
    var wmn = 0.0;
    var wmd = 0.0;
    var wmx = 0.0;
    if (naive_mid <= per[md]) {
        let cm = ((1.0 - hp) * per[md] + hp * (midscale * per[mx] + (1.0 - midscale) * (target_e - per[mx]))) / (1.0 + hp * (1.0 - midscale));
        wmn = target_e - per[mx] - cm;
        wmd = cm;
        wmx = per[mx];
    } else {
        let cm = ((1.0 - hp) * per[md] + hp * (per[mn] * (1.0 - midscale) + midscale * (target_e - per[mn]))) / (1.0 + hp * midscale);
        wmn = per[mn];
        wmd = cm;
        wmx = target_e - per[mn] - cm;
    }
    // without writes at a variable index (DirectX/FXC on Windows cannot do that for local vectors)
    return vec3<f32>(kanal_wert(0u, mn, md, wmn, wmd, wmx), kanal_wert(1u, mn, md, wmn, wmd, wmx), kanal_wert(2u, mn, md, wmn, wmd, wmx));
}

fn kanal_wert(k: u32, mn: u32, md: u32, wmn: f32, wmd: f32, wmx: f32) -> f32 {
    return select(select(wmx, wmd, k == md), wmn, k == mn);
}

fn srgb_oetf(v: f32) -> f32 {
    if (v <= 0.0031308) { return 12.92 * v; }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

// 3D table d (standard look, 33³ absolute values, in square-root space) trilinear
fn std_look(cc: vec3<f32>) -> vec3<f32> {
    if (cc.x < 0.0 || cc.y < 0.0 || cc.z < 0.0 || cc.x > 1.0 || cc.y > 1.0 || cc.z > 1.0) { return cc; }
    let n = 33;
    let m = f32(n - 1);
    let pp = sqrt(cc) * m;
    let i = vec3<i32>(min(i32(pp.x), n - 2), min(i32(pp.y), n - 2), min(i32(pp.z), n - 2));
    let fr = pp - vec3<f32>(f32(i.x), f32(i.y), f32(i.z));
    var o = vec3<f32>(0.0);
    for (var ia = 0; ia < 2; ia++) {
        let wa = select(1.0 - fr.x, fr.x, ia == 1);
        for (var ib = 0; ib < 2; ib++) {
            let wb = select(1.0 - fr.y, fr.y, ib == 1);
            for (var id = 0; id < 2; id++) {
                let wd = select(1.0 - fr.z, fr.z, id == 1);
                let k = u32(((i.x + ia) * n + i.y + ib) * n + i.z + id) * 3u;
                let w = wa * wb * wd;
                o += w * vec3<f32>(d[k], d[k + 1u], d[k + 2u]);
            }
        }
    }
    return o;
}

// node of the camera look table at grid position (x, y, z), as a difference
fn kamera_look_node(off: u32, n: i32, x: i32, y: i32, z: i32) -> vec3<f32> {
    let k = u32((x * n + y) * n + z) * 3u;
    return vec3<f32>(d[off + k], d[off + k + 1u], d[off + k + 2u]);
}

// camera look (n³ differences, in d after the standard look from offset p[61]) on encoded sRGB; p[16] = n,
// p[62] = 1: tetrahedral interpolation (learned looks), otherwise trilinear
fn kamera_look(cc: vec3<f32>) -> vec3<f32> {
    let off = u32(p[61]);
    let n = i32(p[16]);
    let m = f32(n - 1);
    let pp = clamp(cc, vec3<f32>(0.0), vec3<f32>(1.0)) * m;
    let i = vec3<i32>(min(i32(pp.x), n - 2), min(i32(pp.y), n - 2), min(i32(pp.z), n - 2));
    let fr = pp - vec3<f32>(f32(i.x), f32(i.y), f32(i.z));
    if (p[62] > 0.5) {
        let c0 = clamp(cc, vec3<f32>(0.0), vec3<f32>(1.0));
        let r = fr.x;
        let g = fr.y;
        let b = fr.z;
        let n000 = kamera_look_node(off, n, i.x, i.y, i.z);
        let n111 = kamera_look_node(off, n, i.x + 1, i.y + 1, i.z + 1);
        var t = vec3<f32>(0.0);
        if (r >= g && g >= b) {
            t = (1.0 - r) * n000 + (r - g) * kamera_look_node(off, n, i.x + 1, i.y, i.z) + (g - b) * kamera_look_node(off, n, i.x + 1, i.y + 1, i.z) + b * n111;
        } else if (r >= b && b > g) {
            t = (1.0 - r) * n000 + (r - b) * kamera_look_node(off, n, i.x + 1, i.y, i.z) + (b - g) * kamera_look_node(off, n, i.x + 1, i.y, i.z + 1) + g * n111;
        } else if (b > r && r >= g) {
            t = (1.0 - b) * n000 + (b - r) * kamera_look_node(off, n, i.x, i.y, i.z + 1) + (r - g) * kamera_look_node(off, n, i.x + 1, i.y, i.z + 1) + g * n111;
        } else if (g > r && r >= b) {
            t = (1.0 - g) * n000 + (g - r) * kamera_look_node(off, n, i.x, i.y + 1, i.z) + (r - b) * kamera_look_node(off, n, i.x + 1, i.y + 1, i.z) + b * n111;
        } else if (g >= b && b > r) {
            t = (1.0 - g) * n000 + (g - b) * kamera_look_node(off, n, i.x, i.y + 1, i.z) + (b - r) * kamera_look_node(off, n, i.x, i.y + 1, i.z + 1) + r * n111;
        } else {
            t = (1.0 - b) * n000 + (b - g) * kamera_look_node(off, n, i.x, i.y, i.z + 1) + (g - r) * kamera_look_node(off, n, i.x, i.y + 1, i.z + 1) + r * n111;
        }
        return clamp(c0 + t, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    var o = cc;
    for (var ia = 0; ia < 2; ia++) {
        let wa = select(1.0 - fr.x, fr.x, ia == 1);
        for (var ib = 0; ib < 2; ib++) {
            let wb = select(1.0 - fr.y, fr.y, ib == 1);
            for (var id = 0; id < 2; id++) {
                let wd = select(1.0 - fr.z, fr.z, id == 1);
                let k = u32(((i.x + ia) * n + i.y + ib) * n + i.z + id) * 3u;
                let w = wa * wb * wd;
                o += w * vec3<f32>(d[off + k], d[off + k + 1u], d[off + k + 2u]);
            }
        }
    }
    return clamp(o, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Parameters (p): 0 w, 1 h, 2 k, 3 sw, 4 sh, 5..7 luminance of the camera map, 8 ev, 9 shadows, 10 highlights,
// 11 whites, 12 blacks, 13 saturation, 14 vibrance, 15 mono, 16 look_n (0 = no look), 17 look_amount,
// 18 has std look, 19 hue preservation, 20 saturation of the curve, 21..23 lum (working space), 24..32 M (row by row),
// 33..41 ws_srgb, 42 unused, 43 dehaze on, 44/45 map width/height, 46..48 airlight,
// 49 ω, 50 gain, 51 removes haze, 52 lower bound of t, 53 offset of the map in c (after the tone table),
// 54 noise brake on, 55/56 exponents of luminance/colour detail, 57 exposure slider (EV), 58 factor of the base
// layer (1 or 2), 59/60 its width/height, 61 offset of the camera look in d.
// a = RGB (camera), b = map (grid), c = tone curve, d = standard look + camera look, e = noise-brake base layer,
// f = output

// exposure correction (EV) of the equalizer (render.rs Params::equalizer): xl = log2 luminance of the pixel relative
// to middle gray, mk = luminance map
fn equalizer(xl: f32, mk: f32) -> f32 {
    var ev = 0.0;
    if (p[9] != 0.0 || p[10] != 0.0) {
        let u = mk + p[8] - log2(MITTELGRAU);
        let ws = sm(-9.0, -5.0, u) * (1.0 - sm(-2.5, 0.5, u));
        let wh = sm(-0.8, 1.6, u);
        ev += 1.6 * p[9] * ws + 1.5 * p[10] * wh;
    }
    if (p[11] != 0.0) { ev += 1.1 * p[11] * sm(0.3, 2.8, xl); }
    if (p[12] != 0.0) { ev += 1.4 * p[12] * (1.0 - sm(-6.5, -2.5, xl)); }
    return ev;
}

fn scene_m(cam: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        p[24] * cam.x + p[25] * cam.y + p[26] * cam.z,
        p[27] * cam.x + p[28] * cam.y + p[29] * cam.z,
        p[30] * cam.x + p[31] * cam.y + p[32] * cam.z);
}
// Scene-linear pixel in the working space (render.rs Params::scene): dehaze, luminance map, noise brake, matrix,
// exposure and equalizer. Shared by the encoded output (aus_pixel) and the scene output for the film (aus_szene).
fn scene_pixel(x: i32, y: i32, w: i32, h: i32) -> vec3<f32> {
    let i = u32(y * w + x);
    let cam0 = vec3<f32>(a[3u * i], a[3u * i + 1u], a[3u * i + 2u]);
    var cam = cam0;
    let brake = p[54] > 0.0;
    var base = vec3<f32>(0.0);
    if (brake) { base = brake_base(x, y, w); }
    // dehaze (develop/src/dehaze.rs Dehaze::pixel): dark-channel map appended to buffer c after the tone table
    if (p[43] > 0.0) {
        let pw = i32(p[44]);
        let ph = i32(p[45]);
        let off = u32(p[53]);
        let mfy = clamp((f32(y) + 0.5) * f32(ph) / f32(h) - 0.5, 0.0, f32(ph - 1));
        let r0 = i32(mfy);
        let tyy = mfy - f32(r0);
        let r1 = min(r0 + 1, ph - 1);
        let mfx = clamp((f32(x) + 0.5) * f32(pw) / f32(w) - 0.5, 0.0, f32(pw - 1));
        let c0 = i32(mfx);
        let txx = mfx - f32(c0);
        let c1 = min(c0 + 1, pw - 1);
        let dd = (c[off + u32(r0 * pw + c0)] * (1.0 - txx) + c[off + u32(r0 * pw + c1)] * txx) * (1.0 - tyy)
            + (c[off + u32(r1 * pw + c0)] * (1.0 - txx) + c[off + u32(r1 * pw + c1)] * txx) * tyy;
        let air = vec3<f32>(p[46], p[47], p[48]);
        let tt = 1.0 - p[49] * dd;
        if (p[51] > 0.5) {
            cam = ((cam - air) / max(tt, p[52]) + air) * p[50];
            base = ((base - air) / max(tt, p[52]) + air) * p[50];
        } else {
            cam = cam * tt + air * (1.0 - tt);
            base = base * tt + air * (1.0 - tt);
        }
    }
    // luminance map: smooth surroundings (bilinear from the grid) + some pixel contribution (of the pixel before
    // dehaze, as the map of the CPU stage)
    let k = f32(i32(p[2]));
    let sw = i32(p[3]);
    let sh = i32(p[4]);
    let lp = log2(max(p[5] * cam0.x + p[6] * cam0.y + p[7] * cam0.z, 1e-7));
    let fy = clamp((f32(y) + 0.5) / k - 0.5, 0.0, f32(sh - 1));
    let y0 = i32(fy);
    let ty = fy - f32(y0);
    let y1 = min(y0 + 1, sh - 1);
    let fx = clamp((f32(x) + 0.5) / k - 0.5, 0.0, f32(sw - 1));
    let x0 = i32(fx);
    let tx = fx - f32(x0);
    let x1 = min(x0 + 1, sw - 1);
    let ga = b[u32(y0 * sw + x0)] * (1.0 - tx) + b[u32(y0 * sw + x1)] * tx;
    let gc = b[u32(y1 * sw + x0)] * (1.0 - tx) + b[u32(y1 * sw + x1)] * tx;
    let glatt = ga * (1.0 - ty) + gc * ty;
    let mk = glatt + 0.35 * clamp(lp - glatt, -2.0, 2.0);
    // noise brake (render.rs prepare, lift.rs Brake::apply): the base layer receives the full brightening of
    // the pixel, luminance detail gain^p[55], colour detail gain^p[56]; pre-shrunk by the gain that follows
    if (brake) {
        let vb = scene_m(cam);
        let yb = max(p[21] * vb.x + p[22] * vb.y + p[23] * vb.z, 1e-9);
        let gb = exp2(max(p[57], 0.0) + max(equalizer(log2(yb / MITTELGRAU), mk), 0.0));
        if (gb > 1.0) {
            let dl = cam - base;
            let dy = 0.2126 * dl.x + 0.7152 * dl.y + 0.0722 * dl.z;
            let gl = select(pow(gb, p[55]), gb, p[55] == 1.0);
            let gc = select(pow(gb, p[56]), 1.0, p[56] == 0.0);
            let q = gb * base + vec3<f32>(gl * dy) + gc * (dl - vec3<f32>(dy));
            cam = q / gb;
        }
    }
    // scene: matrix, exposure, equalizer
    var v = scene_m(cam);
    let yl = max(p[21] * v.x + p[22] * v.y + p[23] * v.z, 1e-9);
    let xl = log2(yl / MITTELGRAU);
    let ev = equalizer(xl, mk);
    if (ev != 0.0) { v = v * exp2(ev); }
    return v;
}

// Film (render.rs scene_linear): linear sRGB of the scene, negative parts cut, as f32 bits in f (three per pixel)
@compute @workgroup_size(16, 16)
fn aus_szene(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let i = u32(y * w + x);
    let v = scene_pixel(x, y, w, h);
    let s = vec3<f32>(
        p[33] * v.x + p[34] * v.y + p[35] * v.z,
        p[36] * v.x + p[37] * v.y + p[38] * v.z,
        p[39] * v.x + p[40] * v.y + p[41] * v.z);
    atomicStore(&f[3u * i], bitcast<u32>(max(s.x, 0.0)));
    atomicStore(&f[3u * i + 1u], bitcast<u32>(max(s.y, 0.0)));
    atomicStore(&f[3u * i + 2u], bitcast<u32>(max(s.z, 0.0)));
}

@compute @workgroup_size(16, 16)
fn aus_pixel(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) { return; }
    let i = u32(y * w + x);
    let v = scene_pixel(x, y, w, h);
    // tone curve per channel with hue preservation
    let pd = desaturate_negative(v);
    let per = vec3<f32>(ton_eval(pd.x), ton_eval(pd.y), ton_eval(pd.z));
    var t = preserve_hue_and_energy(pd, per, channel_order(pd), p[19]);
    if (p[20] != 1.0) {
        let yy = p[21] * t.x + p[22] * t.y + p[23] * t.z;
        t = vec3<f32>(yy) + p[20] * (t - vec3<f32>(yy));
    }
    var s = vec3<f32>(
        p[33] * t.x + p[34] * t.y + p[35] * t.z,
        p[36] * t.x + p[37] * t.y + p[38] * t.z,
        p[39] * t.x + p[40] * t.y + p[41] * t.z);
    if (p[18] > 0.5) { s = std_look(s); }
    if (p[13] != 0.0 || p[14] != 0.0) {
        let y2 = 0.2126 * s.x + 0.7152 * s.y + 0.0722 * s.z;
        let mx = max(s.x, max(s.y, s.z));
        let mn = min(s.x, min(s.y, s.z));
        var cur = 0.0;
        if (mx > 1e-6) { cur = clamp((mx - mn) / mx, 0.0, 1.0); }
        var vv = 0.0;
        if (p[14] > 0.0) { vv = p[14] * (1.0 - cur) * (1.0 - cur); } else { vv = p[14] * cur; }
        let kk = max((1.0 + p[13]) * (1.0 + vv), 0.0);
        s = vec3<f32>(y2) + (s - vec3<f32>(y2)) * kk;
    }
    // gamut: desaturate negative components towards the luminance
    let yg = 0.2126 * s.x + 0.7152 * s.y + 0.0722 * s.z;
    let mnn = min(s.x, min(s.y, s.z));
    if (mnn < 0.0 && yg > 0.0) {
        let fg = yg / (yg - mnn);
        s = vec3<f32>(yg) + fg * (s - vec3<f32>(yg));
    }
    if (p[15] > 0.5) { s = vec3<f32>(max(yg, 0.0)); }
    var enc = vec3<f32>(srgb_oetf(clamp(s.x, 0.0, 1.0)), srgb_oetf(clamp(s.y, 0.0, 1.0)), srgb_oetf(clamp(s.z, 0.0, 1.0)));
    if (p[16] > 0.5) {
        let o = kamera_look(enc);
        enc = enc + p[17] * (o - enc);
    }
    let q = vec3<u32>(u32(clamp(enc.x, 0.0, 1.0) * 65535.0 + 0.5), u32(clamp(enc.y, 0.0, 1.0) * 65535.0 + 0.5), u32(clamp(enc.z, 0.0, 1.0) * 65535.0 + 0.5));
    // 16-bit values, two per word (index 3·i + channel)
    for (var ch = 0u; ch < 3u; ch++) {
        let el = 3u * i + ch;
        atomicOr(&f[el >> 1u], q[ch] << ((el & 1u) * 16u));
    }
}

// ---- False color suppression (X-Trans; false_color.rs is the reference) ------------------------------------
// Works in place on the finished 16-bit image f (two values per word): aus_fc_yuv computes luminance and the two
// color differences into a, b, c; aus_fc_median compares each pixel's color with the color median of its 5×5
// neighborhood and only rewrites the outliers. p[2] = strength 0 … 1.

fn fc_value(el: u32) -> f32 {
    let word = atomicLoad(&f[el >> 1u]);
    return f32((word >> ((el & 1u) * 16u)) & 0xFFFFu) / 65535.0;
}

@compute @workgroup_size(16, 16)
fn aus_fc_yuv(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = u32(p[0]);
    let h = u32(p[1]);
    if (g.x >= w || g.y >= h) { return; }
    let i = g.y * w + g.x;
    let r = fc_value(3u * i);
    let gg = fc_value(3u * i + 1u);
    let bb = fc_value(3u * i + 2u);
    let l = 0.299 * r + 0.587 * gg + 0.114 * bb;
    a[i] = l;
    b[i] = bb - l;
    c[i] = r - l;
}

// 16×16 pixels plus a margin of 2: the color differences of the 5×5 neighborhoods, loaded once per workgroup
var<workgroup> fc_u: array<f32, 400>;
var<workgroup> fc_v: array<f32, 400>;

fn fc_smooth(a0: f32, b0: f32, x: f32) -> f32 {
    let t = clamp((x - a0) / (b0 - a0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn fc_write(el: u32, v: f32) {
    let q = u32(clamp(v, 0.0, 1.0) * 65535.0 + 0.5);
    let sh = (el & 1u) * 16u;
    atomicAnd(&f[el >> 1u], ~(0xFFFFu << sh));
    atomicOr(&f[el >> 1u], q << sh);
}

@compute @workgroup_size(16, 16)
fn aus_fc_median(@builtin(global_invocation_id) g: vec3<u32>, @builtin(local_invocation_id) li: vec3<u32>, @builtin(workgroup_id) wg: vec3<u32>) {
    let w = i32(p[0]);
    let h = i32(p[1]);
    let ox = i32(wg.x) * 16 - 2;
    let oy = i32(wg.y) * 16 - 2;
    for (var k = li.y * 16u + li.x; k < 400u; k += 256u) {
        let gx = clamp(ox + i32(k % 20u), 0, w - 1);
        let gy = clamp(oy + i32(k / 20u), 0, h - 1);
        let j = u32(gy * w + gx);
        fc_u[k] = b[j];
        fc_v[k] = c[j];
    }
    workgroupBarrier();
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 2 || y < 2 || x + 2 >= w || y + 2 >= h) { return; }
    let i = u32(y * w + x);
    // exact median of 25 values without sorting: the value with at most 12 smaller and more than 12 smaller-or-equal
    // neighbors (ties give the same value as sorting)
    var mu = 0.0;
    var mv = 0.0;
    for (var ky = 0u; ky < 5u; ky++) {
        for (var kx = 0u; kx < 5u; kx++) {
            let kc = (li.y + ky) * 20u + li.x + kx;
            let cu = fc_u[kc];
            let cv = fc_v[kc];
            var ltu = 0u;
            var leu = 0u;
            var ltv = 0u;
            var lev = 0u;
            for (var jy = 0u; jy < 5u; jy++) {
                for (var jx = 0u; jx < 5u; jx++) {
                    let kj = (li.y + jy) * 20u + li.x + jx;
                    let ou = fc_u[kj];
                    let ov = fc_v[kj];
                    ltu += select(0u, 1u, ou < cu);
                    leu += select(0u, 1u, ou <= cu);
                    ltv += select(0u, 1u, ov < cv);
                    lev += select(0u, 1u, ov <= cv);
                }
            }
            if (ltu <= 12u && leu > 12u) { mu = cu; }
            if (ltv <= 12u && lev > 12u) { mv = cv; }
        }
    }
    let u = b[i];
    let v = c[i];
    let dev = sqrt((u - mu) * (u - mu) + (v - mv) * (v - mv));
    let colorful = sqrt(mu * mu + mv * mv);
    let t = fc_smooth(0.015, 0.05, dev) * (1.0 - fc_smooth(0.08, 0.2, colorful)) * p[2];
    if (t <= 0.0) { return; }
    let nu = u + (mu - u) * t;
    let nv = v + (mv - v) * t;
    let l = a[i];
    let r = l + nv;
    let bl = l + nu;
    let gr = (l - 0.299 * r - 0.114 * bl) / 0.587;
    fc_write(3u * i, r);
    fc_write(3u * i + 1u, gr);
    fc_write(3u * i + 2u, bl);
}
