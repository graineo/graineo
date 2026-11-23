// Markesteijn demosaicing (3 passes) for X-Trans on the GPU – step by step like
// crates/raw/src/markesteijn.rs (CPU, reference; origin from darktable/dcraw is noted there). Instead of tiles
// each kernel processes the whole strip image; the borders (computation regions) match the insets of the
// CPU version. The strip has PAD pixels of margin all around (mirrored at the image edge as on the CPU).
//
// Buffers: a = planes (8 × N × 3: directions 0–3 = first set, 4–7 = second), b = mosaic (w·h),
// c = green limits (min: [0, N), max: [N, 2N)), d = derivatives/intermediate values (8 × N), e = homogeneity
// (8 × N), f = output RGB (w·h·3).
//
// Parameters p: 0 Wb, 1 Hb (strip buffer), 2 w, 3 h (image), 4 global row of buffer row 0, 5 PAD,
// 6 sgrow, 7 sgcol, 8 N = Wb·Hb, 9 plane offset (0 or 4), 10/11 first/last+1 output row (global),
// 12..47 pattern 6×6 (color), 48..191 hexagon offsets (72 × (dx, dy)), 192..263 anchors (36 × (drow, dcol)).

@group(0) @binding(0) var<storage, read> p: array<f32>;
@group(0) @binding(1) var<storage, read_write> a: array<f32>;
@group(0) @binding(2) var<storage, read_write> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> c: array<f32>;
@group(0) @binding(4) var<storage, read_write> d: array<f32>;
@group(0) @binding(5) var<storage, read_write> e: array<f32>;
@group(0) @binding(6) var<storage, read_write> f: array<f32>;

fn wb() -> i32 { return i32(p[0]); }
fn hb() -> i32 { return i32(p[1]); }
fn nn() -> u32 { return u32(p[8]); }
fn gy_of(y: i32) -> i32 { return i32(p[4]) + y; }
fn gx_of(x: i32) -> i32 { return x - i32(p[5]); }
fn pm(v: i32, m: i32) -> i32 { return ((v % m) + m) % m; }
fn fcol(row: i32, col: i32) -> i32 { return i32(p[12u + u32(pm(row, 6) * 6 + pm(col, 6))]); }
fn sgrow() -> i32 { return i32(p[6]); }
fn sgcol() -> i32 { return i32(p[7]); }

// hexagon offset k (0…7) of the phase of (row, col)
fn hexv(row: i32, col: i32, k: i32) -> vec2<i32> {
    let idx = u32((pm(row, 3) * 3 + pm(col, 3)) * 8 + k);
    return vec2<i32>(i32(p[48u + 2u * idx]), i32(p[49u + 2u * idx]));
}
fn hexi(row: i32, col: i32, k: i32) -> i32 {
    let v = hexv(row, col, k);
    return v.x + v.y * wb();
}

// planes stored channel by channel ([plane][channel][pixel]) – consecutive threads read/write contiguously
fn pg(pl: i32, i: i32, ch: i32) -> f32 { return a[(u32(pl) * 3u + u32(ch)) * nn() + u32(i)]; }
fn ps(pl: i32, i: i32, ch: i32, v: f32) { a[(u32(pl) * 3u + u32(ch)) * nn() + u32(i)] = v; }

fn spiegel(n: i32, size: i32) -> i32 {
    if (n >= size) { return 2 * size - n - 2; }
    return abs(n);
}

// ---- 0. Fill: mosaic value at its own color position, mirrored at the image edge; planes 0–3 equal ----
@compute @workgroup_size(32, 8)
fn xt_fuellen(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= wb() || y >= hb()) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    let w = i32(p[2]);
    let h = i32(p[3]);
    let fk = fcol(gy, gx);
    var v = 0.0;
    if (gx >= 0 && gy >= 0 && gx < w && gy < h) {
        v = max(b[u32(gy * w + gx)], 0.0);
    } else {
        let cy = spiegel(gy, h);
        let cx = spiegel(gx, w);
        if (fk == fcol(cy, cx)) {
            v = max(b[u32(cy * w + cx)], 0.0);
        } else {
            var sum = 0.0;
            var cnt = 0.0;
            for (var yy = gy - 1; yy <= gy + 1; yy++) {
                for (var xx = gx - 1; xx <= gx + 1; xx++) {
                    let ty = spiegel(yy, h);
                    let tx = spiegel(xx, w);
                    if (fcol(ty, tx) == fk) {
                        sum += max(b[u32(ty * w + tx)], 0.0);
                        cnt += 1.0;
                    }
                }
            }
            if (cnt > 0.0) { v = sum / cnt; }
        }
    }
    let i = y * wb() + x;
    // the other channels were filled with 0 beforehand (clear_buffer)
    for (var pl = 0; pl < 4; pl++) {
        ps(pl, i, fk, v);
    }
}

// ---- 1. Green minimum/maximum of the six neighbors per R/B pixel (border 3; anchors see gpu_anker) ----
@compute @workgroup_size(32, 8)
fn xt_gminmax(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 3 || y < 3 || x >= wb() - 3 || y >= hb() - 3) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    if (fcol(gy, gx) == 1) { return; }
    let ai = u32(pm(gy, 6) * 6 + pm(gx, 6));
    let ay = i32(p[192u + 2u * ai]);
    let ax = i32(p[193u + 2u * ai]);
    let ianc = (y + ay) * wb() + (x + ax);
    var mn = 3.4e38;
    var mx = 0.0;
    for (var k = 0; k < 6; k++) {
        let v = pg(0, ianc + hexi(gy + ay, gx + ax, k), 1);
        mn = min(mn, v);
        mx = max(mx, v);
    }
    let i = u32(y * wb() + x);
    c[i] = mn;
    c[nn() + i] = mx;
}

// ---- 2. Interpolate green in four directions (border 3) ----
@compute @workgroup_size(32, 8)
fn xt_gruen(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 3 || y < 3 || x >= wb() - 3 || y >= hb() - 3) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    let fk = fcol(gy, gx);
    if (fk == 1) { return; }
    let i = y * wb() + x;
    var h: array<i32, 8>;
    for (var k = 0; k < 8; k++) { h[k] = hexi(gy, gx, k); }
    let p0f = pg(0, i, fk);
    var col: array<f32, 4>;
    col[0] = 0.6796875 * (pg(0, i + h[1], 1) + pg(0, i + h[0], 1)) - 0.1796875 * (pg(0, i + 2 * h[1], 1) + pg(0, i + 2 * h[0], 1));
    col[1] = 0.87109375 * pg(0, i + h[3], 1) + pg(0, i + h[2], 1) * 0.13 + 0.359375 * (p0f - pg(0, i - h[2], fk));
    for (var cc = 0; cc < 2; cc++) {
        let hh = h[4 + cc];
        col[2 + cc] = 0.640625 * pg(0, i + hh, 1) + 0.359375 * pg(0, i - 2 * hh, 1)
            + 0.12890625 * (2.0 * p0f - pg(0, i + 3 * hh, fk) - pg(0, i - 3 * hh, fk));
    }
    var sw = 0;
    if (pm(gy - sgrow(), 3) == 0) { sw = 1; }
    let lo = c[u32(i)];
    let hi = c[nn() + u32(i)];
    for (var cc = 0; cc < 4; cc++) {
        ps(cc ^ sw, i, 1, min(max(col[cc], lo), hi));
    }
}

// ---- 3a/3b. Recompute green (from pass 2 on; border 6): first buffer in d, then write ----
@compute @workgroup_size(32, 8)
fn xt_gruen2_a(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 6 || y < 6 || x >= wb() - 6 || y >= hb() - 6) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    let fk = fcol(gy, gx);
    if (fk == 1) { return; }
    let i = y * wb() + x;
    var sw = 0;
    if (pm(gy - sgrow(), 3) == 0) { sw = 1; }
    let roff = i32(p[9]);
    for (var dd = 3; dd < 6; dd++) {
        let hx = hexi(gy, gx, dd);
        let pl = roff + ((dd - 2) ^ sw);
        let val = pg(pl, i - 2 * hx, 1) + 2.0 * pg(pl, i + hx, 1) - pg(pl, i - 2 * hx, fk) - 2.0 * pg(pl, i + hx, fk) + 3.0 * pg(pl, i, fk);
        d[u32(dd - 3) * nn() + u32(i)] = min(max(val / 3.0, c[u32(i)]), c[nn() + u32(i)]);
    }
}

@compute @workgroup_size(32, 8)
fn xt_gruen2_b(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 6 || y < 6 || x >= wb() - 6 || y >= hb() - 6) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    if (fcol(gy, gx) == 1) { return; }
    let i = y * wb() + x;
    var sw = 0;
    if (pm(gy - sgrow(), 3) == 0) { sw = 1; }
    let roff = i32(p[9]);
    for (var dd = 3; dd < 6; dd++) {
        ps(roff + ((dd - 2) ^ sw), i, 1, d[u32(dd - 3) * nn() + u32(i)]);
    }
}

// ---- 4. R and B at isolated green pixels (border 5) ----
@compute @workgroup_size(32, 8)
fn xt_rb1(@builtin(global_invocation_id) g: vec3<u32>) {
    // only the grid points of the isolated green pixels (every 3 pixels): start point in p[264], p[265]
    let x = i32(p[264]) + 3 * i32(g.x);
    let y = i32(p[265]) + 3 * i32(g.y);
    if (x < 5 || y < 5 || x >= wb() - 5 || y >= hb() - 5) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    if (pm(gy - sgrow(), 3) != 0 || pm(gx - sgcol(), 3) != 0) { return; }
    let i = y * wb() + x;
    let roff = i32(p[9]);
    var hh = fcol(gy, gx + 1);
    var diff: array<f32, 6>;
    var col0: array<f32, 6>;
    var col1: array<f32, 6>;
    for (var k = 0; k < 6; k++) { diff[k] = 0.0; col0[k] = 0.0; col1[k] = 0.0; }
    var vert = false;
    var pl = 0;
    for (var dd = 0; dd < 6; dd++) {
        for (var cc = 0; cc < 2; cc++) {
            var step = 1;
            if (vert) { step = wb(); }
            let ic = step * (1 + cc);
            let pe = roff + pl;
            let gg = 2.0 * pg(pe, i, 1) - pg(pe, i + ic, 1) - pg(pe, i - ic, 1);
            let colv = gg + pg(pe, i + ic, hh) + pg(pe, i - ic, hh);
            if (hh != 0) { col1[dd] = colv; } else { col0[dd] = colv; }
            if (dd > 1) {
                let t = pg(pe, i + ic, 1) - pg(pe, i - ic, 1) - pg(pe, i + ic, hh) + pg(pe, i - ic, hh);
                diff[dd] += t * t + gg * gg;
            }
            hh = hh ^ 2;
        }
        if (dd < 2 || (dd & 1) == 1) {
            var dout = dd;
            if (dd > 1 && diff[dd - 1] + 1e-12 < diff[dd]) { dout = dd - 1; }
            ps(roff + pl, i, 0, col0[dout] / 2.0);
            ps(roff + pl, i, 2, col1[dout] / 2.0);
            pl += 1;
        }
        vert = !vert;
        hh = hh ^ 2;
    }
}

// ---- 5. R at B pixels and B at R pixels (border 5) ----
@compute @workgroup_size(32, 8)
fn xt_rb2(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 5 || y < 5 || x >= wb() - 5 || y >= hb() - 5) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    let fk = 2 - fcol(gy, gx);
    if (fk == 1) { return; }
    let i = y * wb() + x;
    let roff = i32(p[9]);
    var vert = false;
    if (pm(gy - sgrow(), 3) != 0) { vert = true; }
    var cst = 1;
    var hst = 3 * wb();
    var cpar = 1;
    if (vert) { cst = wb(); hst = 3; cpar = 0; }
    for (var dd = 0; dd < 4; dd++) {
        let pe = roff + dd;
        let g0 = pg(pe, i, 1);
        var use_c = dd > 1 || (((dd & 1) ^ cpar) != 0);
        if (!use_c) {
            use_c = (abs(g0 - pg(pe, i + cst, 1)) + abs(g0 - pg(pe, i - cst, 1))) < 2.0 * (abs(g0 - pg(pe, i + hst, 1)) + abs(g0 - pg(pe, i - hst, 1)));
        }
        var s = hst;
        if (use_c) { s = cst; }
        let v = (pg(pe, i + s, fk) + pg(pe, i - s, fk) + 2.0 * g0 - pg(pe, i + s, 1) - pg(pe, i - s, 1)) / 2.0;
        ps(pe, i, fk, v);
    }
}

// ---- 6. R and B in 2×2 green blocks (border 4) ----
@compute @workgroup_size(32, 8)
fn xt_g22(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 4 || y < 4 || x >= wb() - 4 || y >= hb() - 4) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    if (pm(gy - sgrow(), 3) == 0 || pm(gx - sgcol(), 3) == 0) { return; }
    let i = y * wb() + x;
    let roff = i32(p[9]);
    for (var dd = 0; dd < 8; dd += 2) {
        let h0 = hexv(gy, gx, dd);
        let h1 = hexv(gy, gx, dd + 1);
        let o0 = h0.x + h0.y * wb();
        let o1 = h1.x + h1.y * wb();
        let pe = roff + dd / 2;
        var v0 = 0.0;
        var v2 = 0.0;
        if (h0.x + h1.x != 0 || h0.y + h1.y != 0) {
            let gg = 3.0 * pg(pe, i, 1) - 2.0 * pg(pe, i + o0, 1) - pg(pe, i + o1, 1);
            v0 = (gg + 2.0 * pg(pe, i + o0, 0) + pg(pe, i + o1, 0)) / 3.0;
            v2 = (gg + 2.0 * pg(pe, i + o0, 2) + pg(pe, i + o1, 2)) / 3.0;
        } else {
            let gg = 2.0 * pg(pe, i, 1) - pg(pe, i + o0, 1) - pg(pe, i + o1, 1);
            v0 = (gg + pg(pe, i + o0, 0) + pg(pe, i + o1, 0)) / 2.0;
            v2 = (gg + pg(pe, i + o0, 2) + pg(pe, i + o1, 2)) / 2.0;
        }
        ps(pe, i, 0, v0);
        ps(pe, i, 2, v2);
    }
}

// ---- 6b. Copy planes 0–3 → 4–7 (start of the second pass): 12 channel planes ----
@compute @workgroup_size(32, 8)
fn xt_kopie(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= wb() || y >= hb()) { return; }
    let i = u32(y * wb() + x);
    for (var k = 0u; k < 12u; k++) {
        a[(12u + k) * nn() + i] = a[k * nn() + i];
    }
}

// ---- 7. Derivative per direction (border 14; YPbPr is computed along the way); z = direction ----
fn ypp(pl: i32, i: i32) -> vec3<f32> {
    let r = pg(pl, i, 0);
    let gg = pg(pl, i, 1);
    let bb = pg(pl, i, 2);
    let yy = 0.2627 * r + 0.6780 * gg + 0.0593 * bb;
    return vec3<f32>(yy, (bb - yy) * 0.56433, (r - yy) * 0.67815);
}

@compute @workgroup_size(32, 8, 1)
fn xt_drv(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    let dd = i32(g.z);
    if (x < 14 || y < 14 || x >= wb() - 14 || y >= hb() - 14) { return; }
    var off = 1;
    switch (dd & 3) {
        case 0: { off = 1; }
        case 1: { off = wb(); }
        case 2: { off = wb() + 1; }
        default: { off = wb() - 1; }
    }
    let i = y * wb() + x;
    let y0 = ypp(dd, i);
    let y1 = ypp(dd, i + off);
    let y2 = ypp(dd, i - off);
    let t = 2.0 * y0 - y1 - y2;
    d[u32(dd) * nn() + u32(i)] = t.x * t.x + t.y * t.y + t.z * t.z;
}

// ---- 8. Homogeneity (border 15) ----
@compute @workgroup_size(32, 8)
fn xt_homo(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x < 15 || y < 15 || x >= wb() - 15 || y >= hb() - 15) { return; }
    let i = y * wb() + x;
    var tr = 3.4e38;
    for (var k = 0; k < 8; k++) { tr = min(tr, d[u32(k) * nn() + u32(i)]); }
    tr = max(8.0 * tr, 1e-10);
    for (var k = 0; k < 8; k++) {
        var s = 0.0;
        for (var v = -1; v <= 1; v++) {
            for (var hh = -1; hh <= 1; hh++) {
                if (d[u32(k) * nn() + u32(i + v * wb() + hh)] <= tr) { s += 1.0; }
            }
        }
        e[u32(k) * nn() + u32(i)] = s;
    }
}

// ---- 9. 5×5 sum of the homogeneity: horizontal d ← e, vertical e ← d; z = direction ----
@compute @workgroup_size(32, 8, 1)
fn xt_h5(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    let k = u32(g.z);
    if (x < 2 || y < 2 || x >= wb() - 2 || y >= hb() - 2) { return; }
    let i = u32(y * wb() + x);
    var s = 0.0;
    for (var t = -2; t <= 2; t++) { s += e[k * nn() + u32(i32(i) + t)]; }
    d[k * nn() + i] = s;
}

@compute @workgroup_size(32, 8, 1)
fn xt_v5(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    let k = u32(g.z);
    if (x < 2 || y < 2 || x >= wb() - 2 || y >= hb() - 2) { return; }
    let i = y * wb() + x;
    var s = 0.0;
    for (var t = -2; t <= 2; t++) { s += d[k * nn() + u32(i + t * wb())]; }
    e[k * nn() + u32(i)] = s;
}

// part of the mosaic (buffer b) below 0 at (gx, gy), mean-demosaiced: own colour exactly, other colours the mean of
// the same colour in 3×3 (5×5 if the pattern has none there), only samples inside the image
fn xt_negative(gx: i32, gy: i32, w: i32, h: i32) -> vec3<f32> {
    var r = vec3<f32>(0.0);
    let own = fcol(gy, gx);
    let inner = gx >= 2 && gy >= 2 && gx + 2 < w && gy + 2 < h;
    for (var c = 0; c < 3; c++) {
        if (c == own) {
            r[c] = min(b[u32(gy * w + gx)], 0.0);
            continue;
        }
        var rad = 1;
        var found = false;
        for (var dy = -1; dy <= 1; dy++) {
            for (var dx = -1; dx <= 1; dx++) {
                if (fcol(gy + dy, gx + dx) == c) { found = true; }
            }
        }
        if (!found) { rad = 2; }
        var s = 0.0;
        var n = 0.0;
        for (var dy = -rad; dy <= rad; dy++) {
            for (var dx = -rad; dx <= rad; dx++) {
                if (fcol(gy + dy, gx + dx) != c) { continue; }
                let xx = gx + dx;
                let yy = gy + dy;
                if (!inner && (xx < 0 || yy < 0 || xx >= w || yy >= h)) { continue; }
                s += min(b[u32(yy * w + xx)], 0.0);
                n += 1.0;
            }
        }
        if (s < 0.0) { r[c] = s / n; }
    }
    return r;
}

// ---- 10. Average the most homogeneous directions → output (only pixels of the strip, inside the image) ----
@compute @workgroup_size(32, 8)
fn xt_fertig(@builtin(global_invocation_id) g: vec3<u32>) {
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= wb() || y >= hb()) { return; }
    let gy = gy_of(y);
    let gx = gx_of(x);
    let w = i32(p[2]);
    if (gx < 0 || gx >= w || gy < i32(p[10]) || gy >= i32(p[11])) { return; }
    let i = y * wb() + x;
    var hm: array<u32, 8>;
    var maxval = 0u;
    for (var k = 0; k < 8; k++) {
        hm[k] = u32(e[u32(k) * nn() + u32(i)]);
        maxval = max(maxval, hm[k]);
    }
    maxval = maxval - (maxval >> 3u);
    for (var k = 0; k < 4; k++) {
        if (hm[k] < hm[k + 4]) {
            hm[k] = 0u;
        } else if (hm[k] > hm[k + 4]) {
            hm[k + 4] = 0u;
        }
    }
    var avg = vec3<f32>(0.0);
    var cnt = 0.0;
    for (var k = 0; k < 8; k++) {
        if (hm[k] >= maxval) {
            avg += vec3<f32>(pg(k, i, 0), pg(k, i, 1), pg(k, i, 2));
            cnt += 1.0;
        }
    }
    // the interpolation saw the mosaic clipped at 0: add the negative part back linearly (raw::add_negative_part,
    // same neighbours and order), no clipping at 0
    let o = avg / cnt + xt_negative(gx, gy, w, i32(p[3]));
    let oi = u32(gy * w + gx) * 3u;
    f[oi] = o.x;
    f[oi + 1u] = o.y;
    f[oi + 2u] = o.z;
}
