// Export scaling (Lanczos3, 16 bit) – resize.rs is the reference (processor path).
// cfg: [width, height, new width, new height, vertical taps, horizontal taps, threads per grid row (vertical, horizontal)]
// vtab/htab: per output row/column [first source index as bits, taps weights]

@group(0) @binding(0) var<storage, read> cfg: array<u32>;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read> vtab: array<f32>;
@group(0) @binding(3) var<storage, read> htab: array<f32>;
@group(0) @binding(4) var<storage, read_write> tmp: array<f32>;
@group(0) @binding(5) var<storage, read_write> dst: array<u32>;

fn src_value(el: u32) -> f32 {
    return f32((src[el >> 1u] >> ((el & 1u) * 16u)) & 0xFFFFu);
}

// vertical pass: one thread per pixel of the intermediate image (width × new height), three channels
@compute @workgroup_size(256)
fn vertical(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = cfg[0];
    let h = cfg[1];
    let nh = cfg[3];
    let n = cfg[4];
    let k = g.x + g.y * cfg[6];
    if (k >= w * nh) { return; }
    let x = k % w;
    let oy = k / w;
    let base = oy * (n + 1u);
    let left = bitcast<u32>(vtab[base]);
    var t = vec3<f32>(0.0);
    for (var i = 0u; i < n; i++) {
        let row = left + i;
        if (row >= h) { break; }
        let wt = vtab[base + 1u + i];
        let el = (row * w + x) * 3u;
        t.x += src_value(el) * wt;
        t.y += src_value(el + 1u) * wt;
        t.z += src_value(el + 2u) * wt;
    }
    let o = (oy * w + x) * 3u;
    tmp[o] = t.x;
    tmp[o + 1u] = t.y;
    tmp[o + 2u] = t.z;
}

// horizontal pass: one thread per output word (two 16-bit values), clamped and rounded half up
@compute @workgroup_size(256)
fn horizontal(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = cfg[0];
    let nw = cfg[2];
    let nh = cfg[3];
    let n = cfg[5];
    let total = nw * nh * 3u;
    let k = g.x + g.y * cfg[7];
    if (2u * k >= total) { return; }
    var word = 0u;
    for (var j = 0u; j < 2u; j++) {
        let el = 2u * k + j;
        if (el >= total) { break; }
        let pix = el / 3u;
        let c = el % 3u;
        let y = pix / nw;
        let ox = pix % nw;
        let base = ox * (n + 1u);
        let left = bitcast<u32>(htab[base]);
        var t = 0.0;
        for (var i = 0u; i < n; i++) {
            let col = left + i;
            if (col >= w) { break; }
            t += tmp[(y * w + col) * 3u + c] * htab[base + 1u + i];
        }
        let q = u32(floor(clamp(t, 0.0, 65535.0) + 0.5));
        word = word | (q << (16u * j));
    }
    dst[k] = word;
}
