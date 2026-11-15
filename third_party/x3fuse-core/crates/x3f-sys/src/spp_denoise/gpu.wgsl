// graineo addition: SPP tile noise reduction (tiled path) on the GPU.
//
// Each kernel reproduces one stage of `Pipeline::process_tile` (or tile setup,
// blending and sharpening) for a batch of tiles. The tiles lie
// one after another in memory; `gid.z` is the tile within the batch. All border and
// counter special cases of the CPU version are reproduced; deviations arise only
// from f32 instead of f64 and from different rounding of individual operations.
//
// Bindings: group 0 = parameters + tables, group 1 = six freely assigned
// work buffers b0…b5, whose meaning per kernel is given in the comment.

struct Params {
    cols: u32, rows: u32, fcols: u32, frows: u32,
    core_w: u32, core_h: u32, halo_x: u32, halo_y: u32,
    grid_x: u32, grid_y: u32, img_w: u32, img_h: u32,
    img_stride: u32, tile_base: u32, batch: u32, bil_start: u32,
    kstep: u32, kcols: u32, krows: u32, subsample: u32,
    filter_half: u32, scaled: u32, smooth_edge: u32, accumulate: u32,
    match_first0: u32, match_last_cut: u32, db_radius: u32, db_end: u32,
    corr_radius: u32, color_mode: u32, perm_half: u32, perm_len: u32,
    dq_mask: u32, blend_border: u32, rcps_on: u32, power: u32,
    d_taps: u32, d_phases: u32, d_tail: i32, d_half: u32,
    u_taps: u32, u_phases: u32, u_tail: i32, u_half: u32,
    off_exp: u32, off_edge: u32, off_dq: u32, off_perm: u32,
    off_dtab: u32, off_dhcol: u32, off_dhph: u32, off_dvrow: u32,
    off_utab: u32, off_uhcol: u32, off_uhph: u32, off_uvrow: u32,
    iso: f32, lut_scale: f32, edge_bias: f32, edge_max: f32,
    edge_factor: f32, db_weight: f32, w0: f32, w1: f32,
    w2: f32, epsilon: f32, nm0: f32, nm1: f32,
    nm2: f32, lambda: f32, kscale: f32, rcps_thr: f32,
    rcps_map_thr: f32, rcps_blend: f32, mixing: f32, color_mixing: f32,
    strength: f32, sharp_limit: f32, sharpen_amount: f32, alpha: f32,
    model: array<f32, 9>,
    edge_model: array<f32, 6>,
    matrix: array<f32, 9>,
    alpha_rest: f32,
    edge_border: u32, lm_border: u32,
    pre_w: u32, pre_h: u32, pre_core_x: u32, pre_core_y: u32, pre_halo_x: u32, pre_halo_y: u32,
    chroma_off: u32, ca_on: u32,
    ca_v: array<f32, 8>,
    ca_flags: array<f32, 6>,
    ca_c: array<f32, 12>,
}

@group(0) @binding(0) var<storage, read> P: Params;
@group(0) @binding(1) var<storage, read> tabs: array<f32>;
@group(1) @binding(0) var<storage, read_write> b0: array<f32>;
@group(1) @binding(1) var<storage, read_write> b1: array<f32>;
@group(1) @binding(2) var<storage, read_write> b2: array<f32>;
@group(1) @binding(3) var<storage, read_write> b3: array<f32>;
@group(1) @binding(4) var<storage, read_write> b4: array<f32>;
@group(1) @binding(5) var<storage, read_write> b5: array<f32>;

const PIXEL_MAX: f32 = 32766.0;
const SHARP_L_MAX: f32 = 32767.0;

fn tpix() -> u32 { return P.cols * P.rows; }
// Index (single channel) in the full tile grid or in the (possibly downscaled) filter grid.
fn ix(t: u32, x: u32, y: u32) -> u32 { return t * tpix() + y * P.cols + x; }
fn fx(t: u32, x: u32, y: u32) -> u32 { return t * tpix() + y * P.fcols + x; }

fn ld3_0(i: u32) -> vec3<f32> { return vec3<f32>(b0[3u * i], b0[3u * i + 1u], b0[3u * i + 2u]); }
fn ld3_1(i: u32) -> vec3<f32> { return vec3<f32>(b1[3u * i], b1[3u * i + 1u], b1[3u * i + 2u]); }
fn ld3_2(i: u32) -> vec3<f32> { return vec3<f32>(b2[3u * i], b2[3u * i + 1u], b2[3u * i + 2u]); }
fn ld3_3(i: u32) -> vec3<f32> { return vec3<f32>(b3[3u * i], b3[3u * i + 1u], b3[3u * i + 2u]); }
fn st3_0(i: u32, v: vec3<f32>) { b0[3u * i] = v.x; b0[3u * i + 1u] = v.y; b0[3u * i + 2u] = v.z; }
fn st3_1(i: u32, v: vec3<f32>) { b1[3u * i] = v.x; b1[3u * i + 1u] = v.y; b1[3u * i + 2u] = v.z; }
fn st3_2(i: u32, v: vec3<f32>) { b2[3u * i] = v.x; b2[3u * i + 1u] = v.y; b2[3u * i + 2u] = v.z; }
fn st3_4(i: u32, v: vec3<f32>) { b4[3u * i] = v.x; b4[3u * i + 1u] = v.y; b4[3u * i + 2u] = v.z; }
fn tab_u(i: u32) -> u32 { return u32(tabs[i]); }
fn tab_i(i: u32) -> i32 { return i32(tabs[i]); }

// ---------------------------------------------------------------------------
// Tile setup: b0 = image (RGB, row stride img_stride), b1 = tiles (batch).
// Reproduces `TileLayout::source_of`.

fn source_of(tile: u32, x: u32, y: u32) -> vec2<i32> {
    let w = P.core_w; let h = P.core_h; let hx = P.halo_x; let hy = P.halo_y;
    let tx = tile % P.grid_x; let ty = tile / P.grid_x;
    let last_x = tx + 1u == P.grid_x; let last_y = ty + 1u == P.grid_y;
    var rx = 2u; if (x < hx) { rx = 0u; } else if (x < hx + w) { rx = 1u; }
    var ry = 2u; if (y < hy) { ry = 0u; } else if (y < hy + h) { ry = 1u; }
    var col: u32;
    if (rx == 0u) { if (tx == 0u) { col = 1u; } else { col = tx * w + x - hx; } }
    else if (rx == 1u) { col = tx * w + x - hx; }
    else { if (last_x) { col = tx * w + w - 1u; } else { col = (tx + 1u) * w + x - hx - w; } }
    if (rx == 1u && ry == 1u) { return vec2<i32>(i32(tx * w + x - hx), i32(ty * h + y - hy)); }
    if (rx == 1u && ry == 0u) {
        var r = ty * h - hy + y; if (ty == 0u) { r = 1u; }
        return vec2<i32>(i32(col), i32(r));
    }
    if (rx == 1u && ry == 2u) {
        var r = (ty + 1u) * h + y - hy - h; if (last_y) { r = ty * h + h - 1u; }
        return vec2<i32>(i32(col), i32(r));
    }
    if (ry == 1u) { return vec2<i32>(i32(col), i32(ty * h + y - hy)); }
    if (rx == 0u && ry == 0u) {
        if (tx > 0u && ty > 0u) { return vec2<i32>(i32(tx * w - hx + x), i32(ty * h - hy + y)); }
        return vec2<i32>(i32(col), i32(ty * h + 1u));
    }
    if (rx == 2u && ry == 0u) {
        if (ty > 0u && !last_x) { return vec2<i32>(i32((tx + 1u) * w + x - hx - w), i32(ty * h - hy + y)); }
        return vec2<i32>(i32(col), i32(ty * h + 1u));
    }
    if (rx == 2u && ry == 2u) {
        if (!last_y && !last_x) { return vec2<i32>(i32((tx + 1u) * w + x - hx - w), i32((ty + 1u) * h + y - hy - h)); }
        return vec2<i32>(-1, -1);
    }
    if (tx > 0u && !last_y) { return vec2<i32>(i32(tx * w - hx + x), i32((ty + 1u) * h + y - hy - h)); }
    return vec2<i32>(i32(col), i32(ty * h + h - 1u));
}

@compute @workgroup_size(16, 16, 1)
fn init_tiles(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let s = source_of(P.tile_base + g.z, g.x, g.y);
    var v = vec3<f32>(0.0);
    if (s.x >= 0) {
        let i = u32(s.y) * P.img_stride + 3u * u32(s.x);
        v = vec3<f32>(b0[i], b0[i + 1u], b0[i + 2u]);
    }
    st3_1(ix(g.z, g.x, g.y), v);
}

// ---------------------------------------------------------------------------
// resample_image (polyphase, separable): b0 = source, b1 = target.
// `down`: source full grid → target filter grid; `up`: the other way round.

fn resample_h(up: bool, src_stride: u32, base: u32, r: i32, j: u32) -> vec3<f32> {
    var taps = P.d_taps; var phases = P.d_phases; var tail = P.d_tail; var half = P.d_half;
    var otab = P.off_dtab; var ocol = P.off_dhcol; var oph = P.off_dhph; var dw = P.fcols;
    if (up) {
        taps = P.u_taps; phases = P.u_phases; tail = P.u_tail; half = P.u_half;
        otab = P.off_utab; ocol = P.off_uhcol; oph = P.off_uhph; dw = P.cols;
    }
    var kind = 0u;
    var row = r;
    if (r - tail < i32(half)) { kind = u32(r - tail) + 1u; row = max(r, 0); }
    let col = tab_i(ocol + kind * dw + j);
    let ph = tab_u(oph + kind * dw + j);
    var acc = vec3<f32>(0.0);
    for (var k = 0u; k < taps; k++) {
        let x = u32(max(col + i32(k), 0));
        let i = base + u32(row) * src_stride + x;
        let wgt = tabs[otab + ph * taps + k];
        acc = acc + vec3<f32>(b0[3u * i], b0[3u * i + 1u], b0[3u * i + 2u]) * wgt;
    }
    return acc;
}

fn resample_px(up: bool, x: u32, y: u32, t: u32) {
    var taps = P.d_taps; var phases = P.d_phases; var otab = P.off_dtab; var orow = P.off_dvrow;
    var dw = P.fcols; var dh = P.frows; var sstride = P.cols;
    if (up) { taps = P.u_taps; phases = P.u_phases; otab = P.off_utab; orow = P.off_uvrow;
              dw = P.cols; dh = P.rows; sstride = P.fcols; }
    if (x >= dw || y >= dh) { return; }
    let o = t * tpix() + y * dw + x;
    if (y + 4u >= dh) { st3_1(o, vec3<f32>(0.0)); return; }
    let start = tab_i(orow + y);
    let ph = y % phases;
    var acc = vec3<f32>(0.0);
    for (var k = 0u; k < taps; k++) {
        acc = acc + resample_h(up, sstride, t * tpix(), start + i32(k), x) * tabs[otab + ph * taps + k];
    }
    st3_1(o, acc);
}

@compute @workgroup_size(16, 16, 1)
fn resample_down(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.z >= P.batch) { return; }
    resample_px(false, g.x, g.y, g.z);
}

@compute @workgroup_size(16, 16, 1)
fn resample_up(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.z >= P.batch) { return; }
    resample_px(true, g.x, g.y, g.z);
}

// ---------------------------------------------------------------------------
// generate_kernel_map: b0 = RGB in the filter grid, b1 = map (first mean, then radius
// in the second half, length per half batch·kcols·krows).

@compute @workgroup_size(64, 1, 1)
fn kmap_mean(@builtin(global_invocation_id) g: vec3<u32>) {
    let n = P.kcols * P.krows;
    if (g.x >= n || g.z >= P.batch) { return; }
    let bx = g.x % P.kcols; let by = g.x / P.kcols; let s = P.kstep;
    // The CPU counters assign pixel (x, y) to block ((x+1)/s, (y+1)/s).
    let y0 = max(i32(by * s) - 1, 0); let y1 = min((by + 1u) * s - 1u, P.frows);
    let x0 = max(i32(bx * s) - 1, 0); let x1 = min((bx + 1u) * s - 1u, P.fcols);
    var sum = 0.0;
    for (var y = u32(y0); y < y1; y++) {
        for (var x = u32(x0); x < x1; x++) {
            let p = ld3_0(fx(g.z, x, y));
            var low = p.z; if (p.y < p.z) { low = p.y; }
            var v = low; if (p.x < low) { v = p.x; }
            sum = sum + v;
        }
    }
    var tw = s; if (bx == P.kcols - 1u) { tw = P.fcols - (P.kcols - 1u) * s; }
    var th = s; if (by == P.krows - 1u) { th = P.frows - (P.krows - 1u) * s; }
    b1[g.z * n + g.x] = sum / f32(tw * th);
}

fn min3(a: f32, b: f32, c: f32) -> f32 {
    var low = b; if (a < b) { low = a; }
    if (c < low) { return c; }
    return low;
}

@compute @workgroup_size(64, 1, 1)
fn kmap_radius(@builtin(global_invocation_id) g: vec3<u32>) {
    let n = P.kcols * P.krows;
    if (g.x >= n || g.z >= P.batch) { return; }
    let col = g.x % P.kcols; let row = g.x / P.kcols; let base = g.z * n;
    let up = select(row - 1u, 0u, row == 0u) * P.kcols;
    let mid = row * P.kcols;
    let down = min(row + 1u, P.krows - 1u) * P.kcols;
    let left = select(col - 1u, 0u, col == 0u);
    let right = min(col + 1u, P.kcols - 1u);
    let above = min3(b1[base + up + col], b1[base + up + right], b1[base + up + left]);
    let center = min3(b1[base + mid + col], b1[base + mid + right], b1[base + mid + left]);
    let below = min3(b1[base + down + col], b1[base + down + right], b1[base + down + left]);
    var low = below; if (center < below) { low = center; }
    var value = low; if (above < low) { value = above; }
    var r0 = 5.0; var r1 = 4.0; var r2 = 3.0; var r3 = 2.0;
    if (P.iso > 400.0) { r0 = 7.0; r1 = 6.0; r2 = 5.0; r3 = 3.0; }
    var radius = r3;
    if (value < 150.0) { radius = 9.0; } else if (value < 300.0) { radius = r0; }
    else if (value < 500.0) { radius = r1; } else if (value < 700.0) { radius = r2; }
    b1[P.batch * n + base + g.x] = radius;
}

// ---------------------------------------------------------------------------
// bilateral_filter_rgb (tile variant): b0 = source, b1 = target (both filter grid),
// b2 = kernel map.

fn lut_index(e: f32) -> i32 {
    // As in the CPU version: rounding via 1.5·2^52 after adding ≈1.5e-8 → halves round up.
    if (e < 2047.5) { return i32(floor(e + 0.5)); }
    return -1;
}

// Tile in workgroup memory: 16×16 output pixels + border 9 (largest radius).
const BT: u32 = 16u;
const BH: u32 = 9u;
const BW: u32 = 34u; // BT + 2·BH
var<workgroup> bil_r: array<f32, 1156>; // BW·BW, per channel
var<workgroup> bil_g: array<f32, 1156>;
var<workgroup> bil_b: array<f32, 1156>;

@compute @workgroup_size(16, 16, 1)
fn bilateral(@builtin(global_invocation_id) g: vec3<u32>,
             @builtin(local_invocation_index) li: u32,
             @builtin(workgroup_id) wg: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    // Shared load (all threads, before any branch).
    let ox = i32(wg.x * BT) - i32(BH);
    let oy = i32(wg.y * BT) - i32(BH);
    for (var k = li; k < BW * BW; k += BT * BT) {
        let sx = ox + i32(k % BW);
        let sy = oy + i32(k / BW);
        var v = vec3<f32>(0.0);
        if (sx >= 0 && sy >= 0 && sx < i32(P.fcols) && sy < i32(P.frows) && t < P.batch) {
            v = ld3_0(fx(t, u32(sx), u32(sy)));
        }
        bil_r[k] = v.x; bil_g[k] = v.y; bil_b[k] = v.z;
    }
    workgroupBarrier();
    if (x >= P.fcols || y >= P.frows || t >= P.batch) { return; }
    let o = fx(t, x, y);
    let start = P.bil_start;
    if (x < start || x + start + 1u >= P.fcols || y < start || y + start >= P.frows) {
        // Border: output 0, blended with the source for alpha ≠ 1 (graineo). The CPU
        // blends after the border copy that the bilateral filter performs in the source
        // (like `border_copy`, here on the filtered grid).
        var edge_out = vec3<f32>(0.0);
        if (P.alpha != 1.0) {
            var sx = x; var sy = y;
            let hf = P.filter_half;
            if (hf != 0u) {
                if (y < hf) { sy = hf; } else if (y >= P.frows - hf) { sy = P.frows - hf - 1u; }
                if (x < hf) { sx = hf; } else if (x >= P.fcols - hf) { sx = P.fcols - hf - 1u; }
            }
            edge_out = ld3_0(fx(t, sx, sy)) * P.alpha_rest;
        }
        st3_1(o, edge_out);
        return;
    }
    // Radius: map counters start at `start`; the radius is not reset at the start of a row
    // (it takes over the last block of the previous row), except in the first
    // row and in rows where the block row changes.
    let s = P.kstep;
    let trow = (y + 1u) / s;
    var tcol = (x + 1u) / s;
    if (tcol == 0u && y != start && (y + 1u) % s != 0u) {
        tcol = (P.fcols - start - 1u) / s;
    }
    let n = P.kcols * P.krows;
    let r = min(u32(b2[P.batch * n + t * n + trow * P.kcols + tcol]), BH);
    var skip = 0u; if (r > 3u && P.subsample != 0u) { skip = 1u; }
    let lx = x - wg.x * BT + BH;
    let ly = y - wg.y * BT + BH;
    let pi = ly * BW + lx;
    let p = vec3<f32>(bil_r[pi], bil_g[pi], bil_b[pi]);
    let m0 = vec3<f32>(P.model[0], P.model[1], P.model[2]);
    let m1 = vec3<f32>(P.model[3], P.model[4], P.model[5]);
    let m2 = vec3<f32>(P.model[6], P.model[7], P.model[8]);
    let inv = P.lut_scale / ((abs(p) * m1 + m0) + (m2 * p) * p);
    // Window rows y−r … min(y+r, frows−1); the width alternates r, r−skip, …
    let last = min(y + r, P.frows - 1u) - y + ly;
    let span = 2u * r - skip;
    var half = r;
    var sum = 0.0;
    var acc = vec3<f32>(0.0);
    for (var yy = ly - r; yy <= last; yy++) {
        var xx = lx - half;
        let row = yy * BW;
        loop {
            if (xx > lx + half) { break; }
            let qi = row + xx;
            let q = vec3<f32>(bil_r[qi], bil_g[qi], bil_b[qi]);
            let d = q - p;
            let e = dot(d * d, inv);
            let k = lut_index(e);
            if (k >= 0) {
                let wgt = tabs[P.off_exp + u32(k)];
                sum = sum + wgt;
                acc = fma(q, vec3<f32>(wgt), acc);
            }
            xx = xx + 1u + skip;
        }
        half = span - half;
    }
    var out = p;
    if (sum != 0.0) { out = acc / sum; }
    // graineo: colour slider step 0/1: A = alpha·A + (1 − alpha)·T .
    if (P.alpha != 1.0) { out = out * P.alpha + p * P.alpha_rest; }
    st3_1(o, out);
}

// Border copy after the bilateral filter (only at scale 1): b0 = source (tile).
@compute @workgroup_size(16, 16, 1)
fn border_copy(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y;
    if (x >= P.cols || y >= P.rows || g.z >= P.batch) { return; }
    let hf = P.filter_half;
    if (hf == 0u) { return; }
    var cy = y; if (y < hf) { cy = hf; } else if (y >= P.rows - hf) { cy = P.rows - hf - 1u; }
    var cx = x; if (x < hf) { cx = hf; } else if (x >= P.cols - hf) { cx = P.cols - hf - 1u; }
    if (cx == x && cy == y) { return; }
    st3_0(ix(g.z, x, y), ld3_0(ix(g.z, cx, cy)));
}

// ---------------------------------------------------------------------------
// compute_edge_map (border 0) in three steps: median → energy → smoothing + EdgeLUT.

fn edge_median5(a0: f32, a1: f32, s2: f32, a3: f32, a4: f32) -> f32 {
    var s0 = a0; var s1 = a1; var s3 = a3; var s4 = a4;
    var s5 = select(s0, s1, s0 > s1);
    s0 = select(s1, s0, s0 > s1);
    var gt = s3 > s4;
    s1 = select(s3, s4, gt);
    s3 = select(s4, s3, gt);
    gt = s5 > s1;
    s4 = select(s0, s3, gt);
    s1 = select(s1, s5, gt);
    s0 = select(s3, s0, gt);
    s3 = select(s1, s4, s4 < s1);
    s5 = select(s0, s2, s0 > s2);
    s3 = select(s5, s3, !(s1 < s2));
    s0 = select(s0, s4, s4 < s0);
    s5 = select(s1, s2, s1 > s2);
    s0 = select(s5, s0, !(s4 < s1));
    return select(s0, s3, !(s4 < s2));
}

// Shifted channel value B(y, x) of the filter grid (0 outside the working columns).
fn edge_b(t: u32, x: u32, y: i32, c: u32) -> f32 {
    if (y < 0 || y >= i32(P.frows) || x < 1u || x + 1u >= P.fcols) { return 0.0; }
    return b0[3u * fx(t, x, u32(y)) + c] + P.edge_bias;
}

// b0 = RGB (filter grid), b1 = median RGB.
@compute @workgroup_size(16, 16, 1)
fn edge_median(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.fcols || y >= P.frows || t >= P.batch) { return; }
    var m = vec3<f32>(0.0);
    if (x >= 1u && x + 1u < P.fcols && y + 1u < P.frows) {
        let yi = i32(y);
        for (var c = 0u; c < 3u; c++) {
            m[c] = edge_median5(edge_b(t, x - 1u, yi, c), edge_b(t, x, yi, c), edge_b(t, x + 1u, yi, c),
                                edge_b(t, x, yi - 1, c), edge_b(t, x, yi + 1, c));
        }
    }
    st3_1(fx(t, x, y), m);
}

// b0 = median RGB, b1 = edge energy E.
@compute @workgroup_size(16, 16, 1)
fn edge_energy(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.fcols || y >= P.frows || t >= P.batch) { return; }
    var e = 0.0;
    if (x >= 1u && x + 1u < P.fcols && y >= 1u && y + 3u <= P.frows) {
        let c0 = ld3_0(fx(t, x, y));
        let l = ld3_0(fx(t, x - 1u, y));
        let r = ld3_0(fx(t, x + 1u, y));
        let u = ld3_0(fx(t, x, y - 1u));
        let d = ld3_0(fx(t, x, y + 1u));
        let div = vec3<f32>(P.edge_model[0], P.edge_model[1], P.edge_model[2])
                + vec3<f32>(P.edge_model[3], P.edge_model[4], P.edge_model[5]) * c0;
        let inv = vec3<f32>(1.0) / div;
        // Median column 0 and fcols−1 are always 0 in the CPU version.
        var lv = l; if (x == 1u) { lv = vec3<f32>(0.0); }
        var rv = r; if (x + 2u == P.fcols) { rv = vec3<f32>(0.0); }
        let dx = (lv - rv) * inv;
        let dy = (u - d) * inv;
        let hh = dx * dx; let vv = dy * dy;
        var eh: f32; var ev: f32;
        if (P.accumulate != 0u) {
            eh = (hh.x + hh.y) + hh.z; ev = (vv.x + vv.y) + vv.z;
        } else {
            let th = select(hh.z, hh.y, hh.y > hh.z); eh = select(th, hh.x, hh.x > th);
            let tv = select(vv.z, vv.y, vv.y > vv.z); ev = select(tv, vv.x, vv.x > tv);
        }
        e = sqrt((eh + ev) * 0.5);
    }
    b1[fx(t, x, y)] = e;
}

// b0 = E, b1 = edge map (recursion weights).
@compute @workgroup_size(16, 16, 1)
fn edge_final(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.fcols || y >= P.frows || t >= P.batch) { return; }
    var v = 0.0;
    if (x >= 1u && x + 1u < P.fcols && y >= 3u && y + 3u < P.frows) {
        let e = b0[fx(t, x, y)];
        if (P.smooth_edge != 0u) {
            var el = 0.0; if (x > 1u) { el = b0[fx(t, x - 1u, y)]; }
            var er = 0.0; if (x + 2u < P.fcols) { er = b0[fx(t, x + 1u, y)]; }
            let sum = b0[fx(t, x, y + 1u)] + (b0[fx(t, x, y - 1u)] + (er + (e + el)));
            v = sum * 0.2;
        } else {
            v = e;
        }
    }
    // Remap with the edge LUT (half-open bounds; border 8 per tile, 0 for the whole-image prefilter,
    // where the rows without an edge value also pass through the LUT).
    let eb = P.edge_border;
    if (y >= eb && y + eb < P.frows && x >= eb + 1u && x + eb + 1u < P.fcols) {
        var m = P.edge_max; if (v < P.edge_max) { m = v; }
        v = tabs[P.off_edge + u32(m * P.edge_factor)];
    }
    b1[fx(t, x, y)] = v;
}

// ---------------------------------------------------------------------------
// local_mean = FIR per row (transposed) + FIR per column, border `lm_border` (8 per tile, 4 for the prefilter).
// iir_rows: b0 = source RGB, b1 = weights, b2 = transposed RGB, b3 = auxiliary gain.
// iir_cols: b0 = transposed RGB, b1 = weights, b2 = target RGB, b3 = auxiliary gain.

@compute @workgroup_size(64, 1, 1)
fn iir_rows(@builtin(global_invocation_id) g: vec3<u32>) {
    let y = g.x + P.lm_border; let t = g.z;
    let w = P.fcols; let h = P.frows;
    if (y + P.lm_border >= h || t >= P.batch) { return; }
    let lo = P.lm_border; let hi = w - P.lm_border;
    let base = t * tpix();
    // Forward: store F and NF (transposed: row = x, column = y).
    var f = ld3_0(fx(t, lo, y));
    var nf = 1.0;
    st3_2(base + lo * h + y, f);
    b3[base + lo * h + y] = nf;
    for (var p = lo + 1u; p < hi; p++) {
        let wgt = b1[fx(t, p, y)];
        f = wgt * f + ld3_0(fx(t, p, y));
        nf = wgt * nf + 1.0;
        st3_2(base + p * h + y, f);
        b3[base + p * h + y] = nf;
    }
    // Backward and merge.
    var bk = ld3_0(fx(t, hi - 1u, y));
    var nb = 1.0;
    st3_2(base + (hi - 1u) * h + y, f / nf);
    for (var q = i32(hi) - 2; q >= i32(lo); q--) {
        let qq = u32(q);
        let src = ld3_0(fx(t, qq, y));
        let wgt = b1[fx(t, qq, y)];
        bk = wgt * bk + src;
        nb = wgt * nb + 1.0;
        let o = base + qq * h + y;
        if (qq == lo) {
            st3_2(o, bk / nb);
        } else {
            st3_2(o, ((ld3_2(o) + bk) - src) / ((b3[o] + nb) + -1.0));
        }
    }
}

@compute @workgroup_size(64, 1, 1)
fn iir_cols(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x + P.lm_border; let t = g.z;
    let w = P.fcols; let h = P.frows;
    if (x + P.lm_border >= w || t >= P.batch) { return; }
    let lo = P.lm_border; let hi = h - P.lm_border;
    let base = t * tpix();
    var f = ld3_0(base + x * h + lo);
    var nf = 1.0;
    st3_2(fx(t, x, lo), f);
    b3[fx(t, x, lo)] = nf;
    for (var p = lo + 1u; p < hi; p++) {
        let wgt = b1[fx(t, x, p)];
        f = wgt * f + ld3_0(base + x * h + p);
        nf = wgt * nf + 1.0;
        st3_2(fx(t, x, p), f);
        b3[fx(t, x, p)] = nf;
    }
    var bk = ld3_0(base + x * h + hi - 1u);
    var nb = 1.0;
    st3_2(fx(t, x, hi - 1u), f / nf);
    for (var q = i32(hi) - 2; q >= i32(lo); q--) {
        let qq = u32(q);
        let src = ld3_0(base + x * h + qq);
        let wgt = b1[fx(t, x, qq)];
        bk = wgt * bk + src;
        nb = wgt * nb + 1.0;
        let o = fx(t, x, qq);
        if (qq == lo) {
            st3_2(o, bk / nb);
        } else {
            st3_2(o, ((ld3_2(o) + bk) - src) / ((b3[o] + nb) + -1.0));
        }
    }
}

// ---------------------------------------------------------------------------
// Whole-image prefilter at low internal scale (`scale <= 0.54`): `bilateral_image`
// (constant radius, reads the buffer directly because the radius can exceed the workgroup
// cache of `bilateral`), the other stages reuse `resample_down`, `edge_*` and `iir_*` with a
// parameter block for the reduced image (`fcols`/`frows` = reduced size, one batch).
// `copy_prefiltered` then extracts one tile of the result per batch position.

// bilateral_image: b0 = reduced source, b1 = target (reduced size).
@compute @workgroup_size(16, 16, 1)
fn bilateral_image(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y;
    if (x >= P.fcols || y >= P.frows) { return; }
    let o = fx(0u, x, y);
    let r = P.bil_start;
    if (x < r || x + r + 1u >= P.fcols || y < r || y + 4u >= P.frows) {
        // graineo: border 0, blended with the source for alpha ≠ 1 (as `alpha_mix` after
        // the whole-image filter on the CPU).
        var edge_out = vec3<f32>(0.0);
        if (P.alpha != 1.0) { edge_out = ld3_0(fx(0u, x, y)) * P.alpha_rest; }
        st3_1(o, edge_out);
        return;
    }
    let skip = P.subsample;
    let p = ld3_0(fx(0u, x, y));
    let m0 = vec3<f32>(P.model[0], P.model[1], P.model[2]);
    let m1 = vec3<f32>(P.model[3], P.model[4], P.model[5]);
    let m2 = vec3<f32>(P.model[6], P.model[7], P.model[8]);
    let inv = P.lut_scale / ((abs(p) * m1 + m0) + (m2 * p) * p);
    let last = min(y + r, P.frows - 1u);
    let span = 2u * r - skip;
    var half = r;
    var sum = 0.0;
    var acc = vec3<f32>(0.0);
    for (var yy = y - r; yy <= last; yy++) {
        var xx = x - half;
        loop {
            if (xx > x + half) { break; }
            let q = ld3_0(fx(0u, xx, yy));
            let d = q - p;
            let e = dot(d * d, inv);
            let k = lut_index(e);
            if (k >= 0) {
                let wgt = tabs[P.off_exp + u32(k)];
                sum = sum + wgt;
                acc = fma(q, vec3<f32>(wgt), acc);
            }
            xx = xx + 1u + skip;
        }
        half = span - half;
    }
    var out = p;
    if (sum != 0.0) { out = acc / sum; }
    // graineo: colour slider step 0/1: A = alpha·A + (1 − alpha)·T .
    if (P.alpha != 1.0) { out = out * P.alpha + p * P.alpha_rest; }
    st3_1(o, out);
}

// copy_prefiltered: b0 = prefiltered image (`pre_w` × `pre_h`, RGB), b1 = filter-grid tiles.
// Block copy of core + halos, then replication of the outer halos, as in `copy_image_to_tile`.
@compute @workgroup_size(16, 16, 1)
fn copy_prefiltered(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.fcols || y >= P.frows || t >= P.batch) { return; }
    let tile = P.tile_base + t;
    let tx = tile % P.grid_x; let ty = tile / P.grid_x;
    let cw = P.pre_core_x; let ch = P.pre_core_y;
    let hx = P.pre_halo_x; let hy = P.pre_halo_y;
    let first_x = tx == 0u; let first_y = ty == 0u;
    let last_x = tx + 1u == P.grid_x; let last_y = ty + 1u == P.grid_y;
    var mx = x;
    if (first_x && x < hx) { mx = hx; } else if (last_x && x >= P.fcols - hx) { mx = P.fcols - hx - 1u; }
    var my = y;
    if (first_y && y < hy) { my = hy; } else if (last_y && y >= P.frows - hy) { my = P.frows - hy - 1u; }
    let to_x = select(0u, hx, first_x); let to_y = select(0u, hy, first_y);
    let count_x = cw + select(hx, 0u, first_x) + select(hx, 0u, last_x);
    let count_y = ch + select(hy, 0u, first_y) + select(hy, 0u, last_y);
    var v = vec3<f32>(0.0);
    if (mx >= to_x && mx < to_x + count_x && my >= to_y && my < to_y + count_y) {
        let from_x = select(tx * cw - hx, 0u, first_x);
        let from_y = select(ty * ch - hy, 0u, first_y);
        let i = (from_y + my - to_y) * P.pre_w * 3u + (from_x + mx - to_x) * 3u;
        v = vec3<f32>(b0[i], b0[i + 1u], b0[i + 2u]);
    }
    st3_1(fx(t, x, y), v);
}

// ---------------------------------------------------------------------------
// Luminance, diffusion, OSF, residual quotient.

// b0 = tile RGB, b1 = luminance.
@compute @workgroup_size(16, 16, 1)
fn luma(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let i = ix(g.z, g.x, g.y);
    let p = ld3_0(i);
    b1[i] = fma(p.z, P.w2, fma(p.x, P.w0, p.y * P.w1));
}

fn diffuse_px(t: u32, x: u32, y: u32, nm: bool) -> f32 {
    let c = b0[ix(t, x, y)];
    if (x == 0u && y + 2u < P.rows) { return b0[ix(t, 0u, y + 1u)]; }
    if (x + 1u == P.cols && y + 2u == P.rows) { return 0.0; }
    if (x == 0u || y == 0u || x + 1u >= P.cols || y + 1u >= P.rows) { return c; }
    let d = vec4<f32>(b0[ix(t, x, y - 1u)] - c, b0[ix(t, x, y + 1u)] - c,
                      b0[ix(t, x + 1u, y)] - c, b0[ix(t, x - 1u, y)] - c);
    var scale = P.kscale;
    if (nm) {
        let model = fma(P.nm2 * c, c, fma(P.nm1, c, P.nm0));
        let rec = 1.0 / model;
        scale = rec * rec;
    }
    let k = vec4<f32>(1.0) / fma(vec4<f32>(scale), d * d, vec4<f32>(1.0));
    var change = fma(k.x, d.x, d.y * k.y);
    change = fma(k.z, d.z, change);
    change = fma(k.w, d.w, change);
    return fma(P.lambda, change, c);
}

// b0 = source, b1 = target.
@compute @workgroup_size(16, 16, 1)
fn diffuse_nm(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    b1[ix(g.z, g.x, g.y)] = diffuse_px(g.z, g.x, g.y, true);
}

@compute @workgroup_size(16, 16, 1)
fn diffuse_k(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    b1[ix(g.z, g.x, g.y)] = diffuse_px(g.z, g.x, g.y, false);
}

// b0 = luminance after diffusion, b1 = residual quotient, b2 = smooth RGB.
@compute @workgroup_size(16, 16, 1)
fn osf_ratio(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.cols || y >= P.rows || t >= P.batch) { return; }
    var v = b0[ix(t, x, y)];
    if (x == 0u && y + 2u < P.rows) {
        v = b0[ix(t, 0u, y + 1u)];
    } else if (x + 1u == P.cols && y >= 1u && y + 1u < P.rows) {
        v = b0[ix(t, P.cols - 1u, 0u)];
    } else if (x > 0u && y > 0u && x + 1u < P.cols && y + 1u < P.rows) {
        var low = 3.4e38; var high = -3.4e38;
        for (var yy = y - 1u; yy <= y + 1u; yy++) {
            for (var xx = x - 1u; xx <= x + 1u; xx++) {
                if (yy != y || xx != x) {
                    let n = b0[ix(t, xx, yy)];
                    low = min(low, n); high = max(high, n);
                }
            }
        }
        if (v > high) { v = high; } else if (v < low) { v = low; }
    }
    let s = ld3_2(ix(t, x, y));
    let l = s.x * P.w0 + s.y * P.w1 + s.z * P.w2;
    b1[ix(t, x, y)] = (v + P.epsilon) / (max(l, 0.0) + P.epsilon);
}

// ---------------------------------------------------------------------------
// Residual mix, colour matrix + ColorDQ, sharpening image, application.

// graineo: log-domain scaling of a ratio, as `power_tone` on the CPU (residual.rs).
fn power_tone(v: f32, a: f32) -> f32 {
    if (!(v > 0.0)) { return v; }
    return exp(a * log(v));
}

fn tone(v: f32) -> f32 {
    return power_tone(v, P.strength);
}

// mix_mono: b0 = residual R, b1 = scaling (output), b2 = smooth RGB, b3 = tile.
@compute @workgroup_size(16, 16, 1)
fn mix_mono(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.cols || y >= P.rows || t >= P.batch) { return; }
    let i = ix(t, x, y);
    let r = b0[i];
    if (y >= (P.rows & ~1u)) { b1[i] = r; return; }
    if (P.mixing < 1.0) {
        let s = ld3_2(i) * vec3<f32>(P.w0, P.w1, P.w2);
        let k = max(ld3_3(i) * vec3<f32>(P.w0, P.w1, P.w2), vec3<f32>(0.0));
        let den = (s.x + s.y) + s.z;
        let num = (k.x + k.y) + k.z;
        let ratio = (num + P.epsilon) / (max(den, 0.0) + P.epsilon);
        let mixed = fma(P.mixing, r, (1.0 - P.mixing) * ratio);
        b1[i] = tone(mixed);
    } else {
        b1[i] = tone(r);
    }
}

// mix_color: b0 = residual R, b1 = factors RGB (output), b2 = smooth RGB, b3 = tile.
@compute @workgroup_size(16, 16, 1)
fn mix_color(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y; let t = g.z;
    if (x >= P.cols || y >= P.rows || t >= P.batch) { return; }
    let i = ix(t, x, y);
    let wv = vec3<f32>(P.w0, P.w1, P.w2);
    let src = ld3_2(i) * wv;
    let smo = max(ld3_3(i) * wv, vec3<f32>(0.0));
    var base = b0[i];
    if (P.mixing < 1.0) {
        let num = ((smo.x + smo.y) + smo.z) + P.epsilon;
        let den = max((src.x + src.y) + src.z, 0.0) + P.epsilon;
        base = fma(P.mixing, b0[i], (1.0 - P.mixing) * (num / den));
    }
    var out: vec3<f32>;
    for (var c = 0u; c < 3u; c++) {
        let ratio = (smo[c] * 3.0 + P.epsilon) / (src[c] * 3.0 + P.epsilon);
        let v = (1.0 - P.color_mixing) * base + P.color_mixing * ratio;
        out[c] = tone(v);
    }
    st3_1(i, out);
}

fn dq(c: u32, i: i32) -> i32 {
    return tab_i(P.off_dq + c * (P.dq_mask + 1u) + (u32(i) & P.dq_mask));
}

// matrix_dq: b0 = smooth RGB (in place).
@compute @workgroup_size(16, 16, 1)
fn matrix_dq(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let i = ix(g.z, g.x, g.y);
    let p = ld3_0(i);
    var q: vec3<f32>;
    for (var r = 0u; r < 3u; r++) {
        let v = P.matrix[3u * r + 1u] * p.y;
        q[r] = fma(P.matrix[3u * r + 2u], p.z, fma(P.matrix[3u * r], p.x, v));
    }
    var res: vec3<i32>;
    for (var c = 0u; c < 3u; c++) {
        let k = i32(floor(q[c] + 0.5));
        res[c] = k - dq(c, k);
    }
    let mean = (res.x + (res.y << 1u) + res.z) >> 1u >> 1u;
    var out: vec3<f32>;
    for (var c = 0u; c < 3u; c++) { out[c] = q[c] - f32(dq(c, res[c] - mean)); }
    st3_0(i, out);
}

// sharp_l: b0 = residual R (before mixing), b1 = L (output), b2 = smooth RGB.
@compute @workgroup_size(16, 16, 1)
fn sharp_l(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let i = ix(g.z, g.x, g.y);
    // Clamp of ISO - 100 to 0..1500 (graineo fix, see residual.rs generate_sharp_l_with).
    let threshold = min(max(P.sharp_limit - 100.0, 0.0), 1500.0);
    let curve = 1.0 - threshold / 1500.0;
    let l = b0[i];
    let wgt = power_tone(l, curve);
    let c = max(ld3_2(i), vec3<f32>(0.0));
    let sum = fma(c.z, wgt, fma(c.x, wgt, c.y * wgt));
    b1[i] = min(sum, SHARP_L_MAX);
}

// apply_mono / apply_color:
// b0 = L, b1 = residual R, b2 = scaling (mono) or factors RGB (colour),
// b3 = smooth RGB, b4 = tile (output), b5 = sharpening image (whole image, i32 bits).
fn apply_common(t: u32, x: u32, y: u32, scale: vec3<f32>) {
    let i = ix(t, x, y);
    var hp = b1[i];
    if (x >= 2u && y >= 2u && x + 2u < P.cols && y + 2u < P.rows) {
        var sum = 0.0;
        for (var yy = y - 1u; yy <= y + 1u; yy++) {
            for (var xx = x - 1u; xx <= x + 1u; xx++) { sum = sum + b0[ix(t, xx, yy)]; }
        }
        hp = b0[i] + sum / -9.0;
    }
    let s = ld3_3(i);
    st3_4(i, clamp(max(s, vec3<f32>(0.0)) * scale, vec3<f32>(0.0), vec3<f32>(PIXEL_MAX)));
    if (x >= P.halo_x && x < P.halo_x + P.core_w && y >= P.halo_y && y < P.halo_y + P.core_h) {
        var v = hp; if (v > PIXEL_MAX) { v = PIXEL_MAX; }
        if (v > 8191.0) { v = 8191.0; }
        let q = i32(trunc(v * 4.0 + 0.5));
        let wrapped = (q << 16u) >> 16u;
        let tile = P.tile_base + t;
        let gx = (tile % P.grid_x) * P.core_w + x - P.halo_x;
        let gy = (tile / P.grid_x) * P.core_h + y - P.halo_y;
        b5[gy * P.img_w + gx] = bitcast<f32>(wrapped);
    }
}

@compute @workgroup_size(16, 16, 1)
fn apply_mono(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    apply_common(g.z, g.x, g.y, vec3<f32>(b2[ix(g.z, g.x, g.y)]));
}

@compute @workgroup_size(16, 16, 1)
fn apply_color(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    apply_common(g.z, g.x, g.y, ld3_2(ix(g.z, g.x, g.y)));
}

// ---------------------------------------------------------------------------
// Blending (reproduces `TileLayout::blend_pixel`) and sharpening.
// compose: b0 = image (in place), b1 = all tiles, b2 = sharpening image.

fn tile_px(index: u32, sx: u32, sy: u32) -> vec3<f32> {
    let i = (index * tpix() + sy * P.cols + sx) * 3u;
    return vec3<f32>(b1[i], b1[i + 1u], b1[i + 2u]);
}

fn mixv(a: vec3<f32>, b: vec3<f32>, w: f32) -> vec3<f32> {
    return a * w + b * (1.0 - w);
}

@compute @workgroup_size(16, 16, 1)
fn compose(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = g.x; let y = g.y;
    if (x >= P.img_w || y >= P.img_h) { return; }
    let w = P.core_w; let h = P.core_h; let hx = P.halo_x; let hy = P.halo_y;
    let border = P.blend_border;
    let step = 1.0 / f32(2u * border);
    let io = y * P.img_stride + 3u * x;
    var value = vec3<f32>(b0[io], b0[io + 1u], b0[io + 2u]);
    let cx = x / w; let cy = y / h;
    let ty_hi = min(cy + 2u, P.grid_y); let tx_hi = min(cx + 2u, P.grid_x);
    let ty_lo = select(cy - 1u, 0u, cy == 0u); let tx_lo = select(cx - 1u, 0u, cx == 0u);
    for (var ty = i32(ty_hi) - 1; ty >= i32(ty_lo); ty--) {
        for (var tx = i32(tx_hi) - 1; tx >= i32(tx_lo); tx--) {
            let txu = u32(tx); let tyu = u32(ty);
            let index = tyu * P.grid_x + txu;
            let last_col = txu + 1u == P.grid_x;
            let last_row = tyu + 1u == P.grid_y;
            var left = border; if (txu == 0u) { left = 0u; }
            var top = border; if (tyu == 0u) { top = 0u; }
            let sx0 = hx - left;
            var sx1 = w + hx - border - 1u; if (last_col) { sx1 = w + hx - 1u; }
            let dx0 = txu * w - left;
            if (!last_row && border != 0u && sx0 < sx1) {
                let base = (tyu + 1u) * h + border - 1u;
                if (y <= base && base - y <= 2u * border && x >= dx0 && x < dx0 + (sx1 - sx0)) {
                    let k = base - y;
                    let sx = sx0 + x - dx0;
                    value = mixv(tile_px(index, sx, h + hy + border - 1u - k), value, f32(k) * step);
                }
            }
            var sy1 = h + hy - border - 1u; if (last_row) { sy1 = h + hy - 1u; }
            var dy1 = (tyu + 1u) * h - border - 1u; if (last_row) { dy1 = (tyu + 1u) * h - 1u; }
            if (y + top >= tyu * h && y <= dy1) {
                let sy = sy1 - (dy1 - y);
                if (x >= dx0 && x <= dx0 + (sx1 - sx0)) {
                    value = tile_px(index, sx0 + x - dx0, sy);
                } else if (!last_col && border != 0u && x + border >= (txu + 1u) * w && x <= (txu + 1u) * w + border) {
                    let k = x + border - (txu + 1u) * w;
                    value = mixv(tile_px(index, sx1 + 1u + k, sy), value, 1.0 - f32(k) * step);
                }
            }
        }
    }
    // Sharpening (residual::sharpen).
    let c = max(value, vec3<f32>(0.0));
    let sum = (c.x + c.y) + c.z;
    var chroma = vec3<f32>(0.0);
    if (sum != 0.0) { chroma = c / sum; }
    var delta = f32(bitcast<i32>(b2[y * P.img_w + x])) * 0.25 * P.sharpen_amount;
    delta = clamp(delta, -1000.0, 1000.0);
    let lower = -0.35 * sum;
    let upper = 0.35 * sum;
    if (!(delta > lower)) { delta = lower; }
    if (upper < delta) { delta = upper; }
    let lum = sum + delta;
    let out = min(chroma * lum, vec3<f32>(32767.0));
    b0[io] = out.x; b0[io + 1u] = out.y; b0[io + 2u] = out.z;
}

// ---------------------------------------------------------------------------
// graineo additions: CA correction and colour slider "Off".

// copy_rgb: b0 → b1 (tile RGB).
@compute @workgroup_size(16, 16, 1)
fn copy_rgb(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let i = ix(g.z, g.x, g.y);
    st3_1(i, ld3_0(i));
}

// Bilinear sample of channel `ch` of tile `t` in b0 (reads past the last pixel are clamped).
fn ca_sample(t: u32, ch: u32, sx: f32, sy: f32, fxf: f32, fyf: f32) -> f32 {
    let x0 = min(u32(fxf), P.cols - 1u); let x1 = min(u32(fxf) + 1u, P.cols - 1u);
    let y0 = min(u32(fyf), P.rows - 1u); let y1 = min(u32(fyf) + 1u, P.rows - 1u);
    let tx = sx - fxf; let ty = sy - fyf;
    let p00 = b0[3u * ix(t, x0, y0) + ch]; let p10 = b0[3u * ix(t, x1, y0) + ch];
    let p01 = b0[3u * ix(t, x0, y1) + ch]; let p11 = b0[3u * ix(t, x1, y1) + ch];
    return (p11 * tx + (1.0 - tx) * p01) * ty + (1.0 - ty) * (p10 * tx + p00 * (1.0 - tx));
}

// ca_shift: b0 = tile RGB, b1 = shifted tile (radial shift of red and blue, `CaTransform::apply`).
@compute @workgroup_size(16, 16, 1)
fn ca_shift(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let t = g.z;
    let i = ix(t, g.x, g.y);
    var out = ld3_0(i);
    let tile = P.tile_base + t;
    let ox = max(i32((tile % P.grid_x) * P.core_w) - i32(P.halo_x), 0);
    let oy = max(i32((tile / P.grid_x) * P.core_h) - i32(P.halo_y), 0);
    let w = P.ca_v[3]; let h = P.ca_v[4];
    let x = f32(ox + i32(g.x)) * (2.0 / (w - 1.0)) - 1.0;
    let yy = f32(oy + i32(g.y)) * (2.0 / (h - 1.0)) - 1.0;
    let r = sqrt(x * x + yy * yy);
    if (r != 0.0) {
        let c = P.ca_c;
        let rc = r * r * r; let r4 = r * rc;
        let pr = ((c[3] * r * r + c[0] * r) + c[6] * rc) + c[9] * r4;
        let pb = ((c[5] * r * r + c[2] * r) + c[8] * rc) + c[11] * r4;
        let sr = P.ca_v[1]; let sb = P.ca_v[2];
        var rr: f32; var rb: f32;
        if (P.ca_v[0] != 0.0) {
            rr = abs(r - pr) * sr + pr;
            rb = abs(r - pb) * sr + pb;
        } else {
            let mid = P.ca_v[6]; let slope = P.ca_v[7]; let stp = P.ca_v[5];
            let d_r = 1.0 / (exp((pr + mid) * slope) + 1.0) * sr * stp;
            let d_b = 1.0 / (exp((pb + mid) * slope) + 1.0) * sb * stp;
            let f = P.ca_flags;
            rr = r * f[0] + (pr - d_r) * f[2] + (pr + d_r) * f[1];
            rb = (pb - d_b) * f[5] + (pb + d_b) * f[4] + r * f[3];
        }
        let hw_m = w * 0.5 - 0.5; let hw_p = w * 0.5 + 0.5;
        let hh_m = h * 0.5 - 0.5; let hh_p = h * 0.5 + 0.5;
        let tw = f32(P.cols); let th = f32(P.rows);
        let fox = f32(ox); let foy = f32(oy);
        let f_r = rr / r; let f_b = rb / r;
        // red
        var sx = ((x * f_r * hw_m + hw_p) - fox) - 1.0;
        var fx0 = floor(sx);
        if (!(tw < fx0 || fx0 < 0.0)) {
            let sy = ((f_r * yy * hh_m + hh_p) - foy) - 1.0;
            let fy0 = floor(sy);
            if (!(th < fy0 || fy0 < 0.0)) { out.x = ca_sample(t, 0u, sx, sy, fx0, fy0); }
        }
        // blue
        sx = ((x * f_b * hw_m + hw_p) - fox) - 1.0;
        fx0 = floor(sx);
        if (!(tw < fx0 || fx0 < 0.0)) {
            let sy = ((f_b * yy * hh_m + hh_p) - foy) - 1.0;
            let fy0 = floor(sy);
            if (!(th < fy0 || fy0 < 0.0)) { out.z = ca_sample(t, 2u, sx, sy, fx0, fy0); }
        }
    }
    st3_1(i, out);
}

// restore_chroma: b0 = result tile (in place), b1 = B after matrix, ColorDQ and CA, b2 = base A.
// `out_c = g·(Y_A/3 + B_c − Y_B/3)` with g = Y_out/Y_A (`Pipeline::restore_chroma`).
@compute @workgroup_size(16, 16, 1)
fn restore_chroma(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= P.cols || g.y >= P.rows || g.z >= P.batch) { return; }
    let i = ix(g.z, g.x, g.y);
    let a = max(ld3_2(i), vec3<f32>(0.0));
    let ya = a.x + a.y + a.z;
    if (ya <= 0.0) { return; }
    let o = ld3_0(i);
    let gain = (o.x + o.y + o.z) / ya;
    let bb = ld3_1(i);
    let yb = (bb.x + bb.y + bb.z) / 3.0;
    st3_0(i, clamp(gain * (vec3<f32>(ya / 3.0) + bb - vec3<f32>(yb)), vec3<f32>(0.0), vec3<f32>(PIXEL_MAX)));
}
