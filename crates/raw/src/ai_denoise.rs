//! Neural denoising of linear RGB with RawNIND's UtNet2 ("linear" variant: B. Brummer, UCLouvain, GPL-3.0; weights
//! trained on the Raw Natural Image Noise Dataset, CC BY 4.0 / CC0). The network runs after the classic demosaic on
//! scene-linear Rec.2020 (white-balanced, sensor white ≈ 1) – never on the mosaic, so denoising and demosaicing stay
//! two separate steps.
//!
//! Own executor for exactly the operators of this U-Net (3×3/1×1 Conv, LeakyRelu, MaxPool 2×2, ConvTranspose 2×2
//! stride 2, Concat): the convolutions are matrix products (Apple Accelerate on macOS, `matrixmultiply` elsewhere),
//! so it builds and runs on every system without a native inference library. A foreign or damaged model file ends
//! in an error, never in a panic ([`Net::new`] checks every node once).
//!
//! Tiling as in the reference implementation: tiles of 512 px, 32 px overlap per side, mirrored at the image border,
//! the overlaps blended with complementary linear ramps (no seams), one brightness gain per tile and channel (mean of
//! the input over the mean of the output, see [`gain_match`]).

use std::collections::HashMap;

use rayon::prelude::*;

use crate::color::M3;

/// Tile edge (pixels) the network was published for; any multiple of 16 would run.
pub const TILE: usize = 512;
/// Overlap per side between neighbouring tiles (pixels).
pub const OVERLAP: usize = 32;

/// Feature map: `c` planes of `h × w` (planar, one image).
#[derive(Clone)]
struct Map {
    c: usize,
    h: usize,
    w: usize,
    d: Vec<f32>,
}

enum Op {
    /// k×k convolution, stride 1, padding k/2, optional fused LeakyRelu; `wt` is `cout × (cin·k·k)` row-major
    Conv { input: usize, output: usize, cin: usize, cout: usize, k: usize, wt: Vec<f32>, bias: Vec<f32>, leaky: Option<f32> },
    MaxPool { input: usize, output: usize },
    /// 2×2 transposed convolution, stride 2; `wt` is `(cout·4) × cin` (row = output channel and sub-position)
    Up { input: usize, output: usize, cin: usize, cout: usize, wt: Vec<f32>, bias: Vec<f32> },
    Concat { inputs: Vec<usize>, output: usize },
}

/// A loaded UtNet2 (or any network of the same operators): ready to run tiles.
pub struct Net {
    ops: Vec<Op>,
    slots: usize,
    input: usize,
    output: usize,
    /// per slot: index of the last op that reads it (to free memory early)
    last_use: Vec<usize>,
    in_channels: usize,
    out_channels: usize,
}

impl Net {
    /// Load an ONNX file of this operator set.
    pub fn load(path: &std::path::Path) -> Result<Net, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Net::new(&faces::onnx::parse(&bytes)?)
    }

    pub fn new(m: &faces::onnx::Model) -> Result<Net, String> {
        let mut slot: HashMap<String, usize> = HashMap::new();
        let id = |name: &str, slot: &mut HashMap<String, usize>| -> usize {
            let n = slot.len();
            *slot.entry(name.to_string()).or_insert(n)
        };
        let input_name = m.graph_inputs.first().ok_or("network without input")?.clone();
        let output_name = m.graph_outputs.first().ok_or("network without output")?.clone();
        let input = id(&input_name, &mut slot);
        // consumers per tensor name (fusing Conv + LeakyRelu needs a Conv output read only by the activation)
        let mut readers: HashMap<&str, usize> = HashMap::new();
        for n in &m.nodes {
            for i in &n.inputs {
                *readers.entry(i.as_str()).or_default() += 1;
            }
        }
        let weight = |n: &faces::onnx::Node, i: usize| -> Result<&faces::onnx::Tensor, String> {
            let name = n.inputs.get(i).ok_or_else(|| format!("{} ({}): input {i} missing", n.op, n.name))?;
            m.weights.get(name).ok_or_else(|| format!("{} ({}): weight {name} missing", n.op, n.name))
        };
        let mut ops: Vec<Op> = Vec::new();
        let mut channels: HashMap<usize, usize> = HashMap::new();
        channels.insert(input, 3);
        let mut skip_next: Option<String> = None;
        for (i, n) in m.nodes.iter().enumerate() {
            if skip_next.as_deref() == Some(n.name.as_str()) {
                skip_next = None;
                continue;
            }
            let bad = |what: &str| Err(format!("{} ({}): {what}", n.op, n.name));
            if n.outputs.len() != 1 || n.inputs.is_empty() {
                return bad("unexpected inputs or outputs");
            }
            match n.op.as_str() {
                "Conv" => {
                    let w = weight(n, 1)?;
                    let b = weight(n, 2)?;
                    let [cout, cin, kh, kw] = w.dims[..] else { return bad("weight shape") };
                    let k = kh;
                    if kh != kw || !(k == 1 || k == 3) || b.data.len() != cout {
                        return bad("only 1×1 and 3×3 with bias");
                    }
                    if n.int("group").unwrap_or(1) != 1 || n.ints("strides").is_some_and(|s| s.iter().any(|v| *v != 1)) || n.ints("dilations").is_some_and(|s| s.iter().any(|v| *v != 1)) {
                        return bad("only stride 1, no groups");
                    }
                    let pad = (k / 2) as i64;
                    if n.ints("pads").is_some_and(|p| p.iter().any(|v| *v != pad)) {
                        return bad("padding does not fit the kernel size");
                    }
                    let src = id(&n.inputs[0], &mut slot);
                    if channels.get(&src) != Some(&cin) {
                        return bad("input channel count");
                    }
                    // fuse a following LeakyRelu that is the only reader of this output
                    let mut out_name = n.outputs[0].clone();
                    let mut leaky = None;
                    if let Some(next) = m.nodes.get(i + 1) {
                        if next.op == "LeakyRelu" && next.inputs.first() == Some(&out_name) && readers.get(out_name.as_str()) == Some(&1) && next.outputs.len() == 1 {
                            leaky = Some(next.float("alpha").unwrap_or(0.01));
                            out_name = next.outputs[0].clone();
                            skip_next = Some(next.name.clone());
                        }
                    }
                    let output = id(&out_name, &mut slot);
                    channels.insert(output, cout);
                    ops.push(Op::Conv { input: src, output, cin, cout, k, wt: w.data.clone(), bias: b.data.clone(), leaky });
                }
                "LeakyRelu" => return bad("activation without a preceding convolution"),
                "MaxPool" => {
                    if n.ints("kernel_shape") != Some(&[2, 2][..]) || n.ints("strides") != Some(&[2, 2][..]) || n.ints("pads").is_some_and(|p| p.iter().any(|v| *v != 0)) || n.int("ceil_mode").unwrap_or(0) != 0 {
                        return bad("only 2×2, stride 2");
                    }
                    let src = id(&n.inputs[0], &mut slot);
                    let c = *channels.get(&src).ok_or("MaxPool: unknown input")?;
                    let output = id(&n.outputs[0], &mut slot);
                    channels.insert(output, c);
                    ops.push(Op::MaxPool { input: src, output });
                }
                "ConvTranspose" => {
                    let w = weight(n, 1)?;
                    let b = weight(n, 2)?;
                    let [cin, cout, 2, 2] = w.dims[..] else { return bad("only 2×2") };
                    if n.ints("strides") != Some(&[2, 2][..]) || n.ints("pads").is_some_and(|p| p.iter().any(|v| *v != 0)) || n.int("group").unwrap_or(1) != 1 || b.data.len() != cout || n.ints("output_padding").is_some_and(|p| p.iter().any(|v| *v != 0)) {
                        return bad("only stride 2 without padding");
                    }
                    let src = id(&n.inputs[0], &mut slot);
                    if channels.get(&src) != Some(&cin) {
                        return bad("input channel count");
                    }
                    // [cin][cout][dy][dx] → rows (co, dy, dx), columns ci
                    let mut wt = vec![0f32; cout * 4 * cin];
                    for ci in 0..cin {
                        for co in 0..cout {
                            for s in 0..4 {
                                wt[(co * 4 + s) * cin + ci] = w.data[(ci * cout + co) * 4 + s];
                            }
                        }
                    }
                    let output = id(&n.outputs[0], &mut slot);
                    channels.insert(output, cout);
                    ops.push(Op::Up { input: src, output, cin, cout, wt, bias: b.data.clone() });
                }
                "Concat" => {
                    if n.int("axis") != Some(1) {
                        return bad("only along the channels");
                    }
                    let inputs: Vec<usize> = n.inputs.iter().map(|x| id(x, &mut slot)).collect();
                    let mut c = 0;
                    for s in &inputs {
                        c += *channels.get(s).ok_or("Concat: unknown input")?;
                    }
                    let output = id(&n.outputs[0], &mut slot);
                    channels.insert(output, c);
                    ops.push(Op::Concat { inputs, output });
                }
                other => return Err(format!("operator {other} not supported")),
            }
        }
        let output = *slot.get(&output_name).ok_or("output is never computed")?;
        let out_channels = *channels.get(&output).ok_or("output is never computed")?;
        let mut last_use = vec![usize::MAX; slot.len()];
        for (k, op) in ops.iter().enumerate() {
            let reads: Vec<usize> = match op {
                Op::Conv { input, .. } | Op::MaxPool { input, .. } | Op::Up { input, .. } => vec![*input],
                Op::Concat { inputs, .. } => inputs.clone(),
            };
            for r in reads {
                last_use[r] = k;
            }
        }
        if out_channels != 3 {
            return Err(format!("output with {out_channels} instead of 3 channels"));
        }
        Ok(Net { ops, slots: slot.len(), input, output, last_use, in_channels: 3, out_channels })
    }

    /// Run one tile: `x` is planar RGB (`3 × h × w`, `h` and `w` multiples of 16). Returns planar RGB of the same size.
    pub fn run(&self, x: &[f32], h: usize, w: usize) -> Result<Vec<f32>, String> {
        if h % 16 != 0 || w % 16 != 0 || x.len() != self.in_channels * h * w {
            return Err("tile size".into());
        }
        let mut v: Vec<Option<Map>> = vec![None; self.slots];
        v[self.input] = Some(Map { c: self.in_channels, h, w, d: x.to_vec() });
        for (k, op) in self.ops.iter().enumerate() {
            let (out_slot, out) = match op {
                Op::Conv { input, output, cin, cout, k: ks, wt, bias, leaky } => {
                    let a = v[*input].as_ref().ok_or("input missing")?;
                    (*output, conv(a, *cin, *cout, *ks, wt, bias, *leaky))
                }
                Op::MaxPool { input, output } => (*output, maxpool(v[*input].as_ref().ok_or("input missing")?)),
                Op::Up { input, output, cin, cout, wt, bias } => (*output, up(v[*input].as_ref().ok_or("input missing")?, *cin, *cout, wt, bias)),
                Op::Concat { inputs, output } => {
                    let parts: Vec<&Map> = inputs.iter().map(|i| v[*i].as_ref()).collect::<Option<_>>().ok_or("input missing")?;
                    let (h, w) = (parts[0].h, parts[0].w);
                    if parts.iter().any(|p| p.h != h || p.w != w) {
                        return Err("Concat: sizes differ".into());
                    }
                    let mut d = Vec::with_capacity(parts.iter().map(|p| p.d.len()).sum());
                    for p in &parts {
                        d.extend_from_slice(&p.d);
                    }
                    (*output, Map { c: parts.iter().map(|p| p.c).sum(), h, w, d })
                }
            };
            v[out_slot] = Some(out);
            // free what no later op reads
            for (s, last) in self.last_use.iter().enumerate() {
                if *last == k && s != self.output {
                    v[s] = None;
                }
            }
        }
        let o = v[self.output].take().ok_or("output missing")?;
        if o.c != self.out_channels || o.h != h || o.w != w {
            return Err("output has another size".into());
        }
        Ok(o.d)
    }
}

#[cfg(target_os = "macos")]
#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn cblas_sgemm(order: i32, ta: i32, tb: i32, m: i32, n: i32, k: i32, alpha: f32, a: *const f32, lda: i32, b: *const f32, ldb: i32, beta: f32, c: *mut f32, ldc: i32);
}

/// C (`m × n`, row stride `ldc`) = A (`m × k`) · B (`k × n`), A and B row-major and contiguous.
///
/// # Safety
/// `a` holds `m·k`, `b` holds `k·n` values; `c` is writable for rows `0..m` of `n` values at stride `ldc ≥ n`.
unsafe fn gemm(m: usize, n: usize, k: usize, a: *const f32, b: *const f32, c: *mut f32, ldc: usize) {
    #[cfg(target_os = "macos")]
    {
        cblas_sgemm(101, 111, 111, m as i32, n as i32, k as i32, 1.0, a, k as i32, b, n as i32, 0.0, c, ldc as i32);
    }
    #[cfg(not(target_os = "macos"))]
    {
        matrixmultiply::sgemm(m, k, n, 1.0, a, k as isize, 1, b, n as isize, 1, 0.0, c, ldc as isize, 1);
    }
}

/// Raw pointer that may cross threads: the bands below write disjoint rows.
#[derive(Clone, Copy)]
struct Out(*mut f32);
unsafe impl Send for Out {}
unsafe impl Sync for Out {}

/// Rows per band of the im2col buffer (bounded to ≈ 16 MB per band).
fn band_rows(k_len: usize, w: usize, h: usize) -> usize {
    (4_000_000 / (k_len * w).max(1)).clamp(1, h)
}

fn conv(a: &Map, cin: usize, cout: usize, k: usize, wt: &[f32], bias: &[f32], leaky: Option<f32>) -> Map {
    let (h, w) = (a.h, a.w);
    let plane = h * w;
    let mut out = vec![0f32; cout * plane];
    let finish = |o: &mut [f32], co: usize| {
        let b = bias[co];
        match leaky {
            Some(s) => o.iter_mut().for_each(|v| {
                let t = *v + b;
                *v = if t < 0.0 { t * s } else { t };
            }),
            None => o.iter_mut().for_each(|v| *v += b),
        }
    };
    if k == 1 {
        // SAFETY: wt is cout × cin, a.d is cin × plane, out is cout × plane
        unsafe { gemm(cout, plane, cin, wt.as_ptr(), a.d.as_ptr(), out.as_mut_ptr(), plane) };
        out.par_chunks_mut(plane).enumerate().for_each(|(co, o)| finish(o, co));
        return Map { c: cout, h, w, d: out };
    }
    let kl = cin * 9;
    let rows = band_rows(kl, w, h);
    let ptr = Out(out.as_mut_ptr());
    let bands: Vec<usize> = (0..h).step_by(rows).collect();
    bands.par_iter().for_each(|&y0| {
        let y1 = (y0 + rows).min(h);
        let bh = y1 - y0;
        let n = bh * w;
        let mut col = vec![0f32; kl * n];
        for ci in 0..cin {
            let src = &a.d[ci * plane..(ci + 1) * plane];
            for ky in 0..3 {
                for kx in 0..3 {
                    let r = &mut col[(ci * 9 + ky * 3 + kx) * n..(ci * 9 + ky * 3 + kx + 1) * n];
                    for yy in 0..bh {
                        let sy = (y0 + yy) as isize + ky as isize - 1;
                        let dst = &mut r[yy * w..(yy + 1) * w];
                        if sy < 0 || sy >= h as isize {
                            dst.fill(0.0);
                            continue;
                        }
                        let s = &src[sy as usize * w..(sy as usize + 1) * w];
                        match kx {
                            0 => {
                                dst[0] = 0.0;
                                dst[1..].copy_from_slice(&s[..w - 1]);
                            }
                            1 => dst.copy_from_slice(s),
                            _ => {
                                dst[..w - 1].copy_from_slice(&s[1..]);
                                dst[w - 1] = 0.0;
                            }
                        }
                    }
                }
            }
        }
        let p = ptr;
        // SAFETY: rows y0..y1 of every output plane belong to this band only
        unsafe {
            gemm(cout, n, kl, wt.as_ptr(), col.as_ptr(), p.0.add(y0 * w), plane);
            for co in 0..cout {
                finish(std::slice::from_raw_parts_mut(p.0.add(co * plane + y0 * w), n), co);
            }
        }
    });
    Map { c: cout, h, w, d: out }
}

fn maxpool(a: &Map) -> Map {
    let (h, w) = (a.h / 2, a.w / 2);
    let mut d = vec![0f32; a.c * h * w];
    d.par_chunks_mut(h * w).enumerate().for_each(|(c, o)| {
        let s = &a.d[c * a.h * a.w..(c + 1) * a.h * a.w];
        for y in 0..h {
            let (r0, r1) = (&s[2 * y * a.w..(2 * y + 1) * a.w], &s[(2 * y + 1) * a.w..(2 * y + 2) * a.w]);
            for x in 0..w {
                o[y * w + x] = r0[2 * x].max(r0[2 * x + 1]).max(r1[2 * x]).max(r1[2 * x + 1]);
            }
        }
    });
    Map { c: a.c, h, w, d }
}

fn up(a: &Map, cin: usize, cout: usize, wt: &[f32], bias: &[f32]) -> Map {
    let plane = a.h * a.w;
    let mut tmp = vec![0f32; cout * 4 * plane];
    // SAFETY: wt is (cout·4) × cin, a.d is cin × plane
    unsafe { gemm(cout * 4, plane, cin, wt.as_ptr(), a.d.as_ptr(), tmp.as_mut_ptr(), plane) };
    let (h, w) = (a.h * 2, a.w * 2);
    let mut d = vec![0f32; cout * h * w];
    d.par_chunks_mut(h * w).enumerate().for_each(|(co, o)| {
        let b = bias[co];
        for s in 0..4 {
            let (dy, dx) = (s / 2, s % 2);
            let t = &tmp[(co * 4 + s) * plane..(co * 4 + s + 1) * plane];
            for y in 0..a.h {
                let row = &mut o[(2 * y + dy) * w..(2 * y + dy + 1) * w];
                for x in 0..a.w {
                    row[2 * x + dx] = t[y * a.w + x] + b;
                }
            }
        }
    });
    Map { c: cout, h, w, d }
}

/// Mirror an index into `0..n` (reflection without repeating the edge pixel).
fn mirror(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let p = 2 * (n as isize - 1);
    let m = i.rem_euclid(p);
    (if m < n as isize { m } else { p - m }) as usize
}

/// Seam weight along one axis for position `p` of a tile whose own (unshared) span is `base..end`: 1 inside, a linear
/// ramp over `2·OVERLAP` around each edge shared with a neighbour, so neighbouring weights add up to 1.
fn seam(p: usize, base: usize, end: usize, before: bool, after: bool) -> f32 {
    let o = OVERLAP as f32;
    if before && p + OVERLAP >= base && p < base + OVERLAP {
        return ((p + OVERLAP - base) as f32 + 0.5) / (2.0 * o);
    }
    if after && p + OVERLAP >= end && p < end + OVERLAP {
        return 1.0 - ((p + OVERLAP - end) as f32 + 0.5) / (2.0 * o);
    }
    1.0
}

/// Brightness of a tile: the network's output scale is arbitrary (it was trained with a gain match), so the output is
/// scaled to the input's mean – one gain per channel: the noise is zero-mean in linear light, so the noisy tile's
/// channel means are those of the clean scene. One gain for all channels (the reference implementation;
/// `GRAINEO_AI_GAIN=joint` for measurements) leaves a slight green cast (a* −1.5 … −2 in the mid tones, measured).
fn gain_match(input: &[f32], out: &mut [f32], plane: usize) {
    static PER_CHANNEL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let per = *PER_CHANNEL.get_or_init(|| std::env::var("GRAINEO_AI_GAIN").as_deref() != Ok("joint"));
    let parts: Vec<(usize, usize)> = if per { (0..3).map(|c| (c * plane, (c + 1) * plane)).collect() } else { vec![(0, 3 * plane)] };
    for (a, b) in parts {
        let si: f64 = input[a..b].par_iter().map(|v| *v as f64).sum();
        let so: f64 = out[a..b].par_iter().map(|v| *v as f64).sum();
        let g = if so.abs() > 1e-30 && si.is_finite() && so.is_finite() { (si / so) as f32 } else { 1.0 };
        out[a..b].par_iter_mut().for_each(|v| *v *= g);
    }
}

/// Progress of a running computation: tiles done, tiles in all.
pub type Progress<'a> = &'a (dyn Fn(usize, usize) + Sync);

/// Denoise a whole image. `px` is the stage (white-balanced camera RGB, interleaved), `to_model` converts it to the
/// network's linear Rec.2020, `from_model` back. Returns the denoised stage at full strength (same space as `px`).
/// `None` when `keep_going` turned false (checked before every tile).
pub fn denoise(net: &Net, px: &[[f32; 3]], w: usize, h: usize, to_model: &M3, from_model: &M3, progress: Progress, keep_going: &dyn Fn() -> bool) -> Result<Option<Vec<[f32; 3]>>, String> {
    if px.len() != w * h || w == 0 || h == 0 {
        return Err("image size".into());
    }
    let t = TILE;
    let step = t - 2 * OVERLAP;
    let (cols, rows) = (w.div_ceil(step), h.div_ceil(step));
    let total = cols * rows;
    let mut out = vec![[0f32; 3]; w * h];
    let tp = t * t;
    let mut done = 0;
    progress(0, total);
    for ty in 0..rows {
        for tx in 0..cols {
            if !keep_going() {
                return Ok(None);
            }
            let (yb, xb) = (ty * step, tx * step);
            let (ye, xe) = ((yb + step).min(h), (xb + step).min(w));
            // tile with mirrored border, in the network's space, planar
            let mut tile = vec![0f32; 3 * tp];
            let (r_plane, rest) = tile.split_at_mut(tp);
            let (g_plane, b_plane) = rest.split_at_mut(tp);
            r_plane.par_chunks_mut(t).zip(g_plane.par_chunks_mut(t)).zip(b_plane.par_chunks_mut(t)).enumerate().for_each(|(dy, ((r, g), b))| {
                let sy = mirror(yb as isize - OVERLAP as isize + dy as isize, h);
                for dx in 0..t {
                    let sx = mirror(xb as isize - OVERLAP as isize + dx as isize, w);
                    let c = crate::color::mulv(to_model, px[sy * w + sx]);
                    r[dx] = c[0];
                    g[dx] = c[1];
                    b[dx] = c[2];
                }
            });
            let mut res = net.run(&tile, t, t)?;
            gain_match(&tile, &mut res, tp);
            // blend into the output: the tile's span plus the overlap into its neighbours
            let (has_top, has_bot, has_left, has_right) = (ty > 0, ty + 1 < rows, tx > 0, tx + 1 < cols);
            let y0 = if has_top { yb - OVERLAP } else { yb };
            let y1 = if has_bot { (ye + OVERLAP).min(h) } else { ye };
            let x0 = if has_left { xb - OVERLAP } else { xb };
            let x1 = if has_right { (xe + OVERLAP).min(w) } else { xe };
            let res = &res;
            out[y0 * w..y1 * w].par_chunks_mut(w).enumerate().for_each(|(i, row)| {
                let y = y0 + i;
                let wy = seam(y, yb, ye, has_top, has_bot);
                let my = y + OVERLAP - yb;
                for x in x0..x1 {
                    let wgt = wy * seam(x, xb, xe, has_left, has_right);
                    let mx = x + OVERLAP - xb;
                    let k = my * t + mx;
                    let c = crate::color::mulv(from_model, [res[k], res[tp + k], res[2 * tp + k]]);
                    let o = &mut row[x];
                    o[0] += wgt * c[0];
                    o[1] += wgt * c[1];
                    o[2] += wgt * c[2];
                }
            });
            done += 1;
            progress(done, total);
        }
    }
    Ok(Some(out))
}

/// Separable box mean of radius `r` (clamped at the borders).
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut tmp = vec![0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = &src[y * w..(y + 1) * w];
        let mut acc = 0f32;
        let mut n = 0f32;
        for x in 0..=r.min(w - 1) {
            acc += s[x];
            n += 1.0;
        }
        for (x, o) in row.iter_mut().enumerate() {
            *o = acc / n;
            if x + r + 1 < w {
                acc += s[x + r + 1];
                n += 1.0;
            }
            if x >= r {
                acc -= s[x - r];
                n -= 1.0;
            }
        }
    });
    let mut out = vec![0f32; w * h];
    let cols: Vec<usize> = (0..w).collect();
    let o = Out(out.as_mut_ptr());
    cols.par_chunks(64).for_each(|xs| {
        let o = o;
        for &x in xs {
            let mut acc = 0f32;
            let mut n = 0f32;
            for y in 0..=r.min(h - 1) {
                acc += tmp[y * w + x];
                n += 1.0;
            }
            for y in 0..h {
                // SAFETY: each column x is written by one task only
                unsafe { *o.0.add(y * w + x) = acc / n };
                if y + r + 1 < h {
                    acc += tmp[(y + r + 1) * w + x];
                    n += 1.0;
                }
                if y >= r {
                    acc -= tmp[(y - r) * w + x];
                    n -= 1.0;
                }
            }
        }
    });
    out
}

/// Highlight protection: the network rings around bright point lights (stars, specular highlights: dark rings with
/// a coloured fringe) and is unreliable near clipping, where it saw nothing like it in training. There the result
/// falls back to the input: wherever the input (in the network's space, white = 1) is near clipping or a local peak
/// far above its surroundings, plus a margin that covers the rings, feathered so no edge shows.
pub fn protect_highlights(orig: &[[f32; 3]], den: &mut [[f32; 3]], w: usize, h: usize, to_model: &M3) {
    if w < 8 || h < 8 || orig.len() != w * h || den.len() != w * h {
        return;
    }
    let y: Vec<f32> = orig.par_iter().map(|p| {
        let c = crate::color::mulv(to_model, *p);
        (c[0] + c[1] + c[2]) / 3.0
    }).collect();
    let near = box_mean(&y, w, h, 1);
    let close = box_mean(&y, w, h, SPOT_RADIUS);
    let around = box_mean(&y, w, h, PEAK_SURROUND);
    // a peak stands out of the image's own noise: [`PEAK_NOISE`] × the robust spread of (near − around), sampled
    let mut spread: Vec<f32> = near.iter().zip(&around).step_by(7).map(|(n, a)| (n - a).abs()).collect();
    let mid = spread.len() / 2;
    // measurement switch: GRAINEO_AI_PEAK=ratio,noise,spot
    let tune: Vec<f32> = std::env::var("GRAINEO_AI_PEAK").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
    let (ratio, noise, spot) = if tune.len() == 3 { (tune[0], tune[1], tune[2]) } else { (PEAK_RATIO, PEAK_NOISE, SPOT_SHARE) };
    let above = if spread.is_empty() { PEAK_ABOVE } else { (noise * 1.4826 * *spread.select_nth_unstable_by(mid, f32::total_cmp).1).max(PEAK_ABOVE) };
    // seeds: 2 = near clipping or a strong peak (its ripples reach far), 1 = a faint peak
    let seed: Vec<u8> = near.par_iter().zip(around.par_iter()).zip(close.par_iter()).zip(y.par_iter()).map(|(((&n, &a), &c), &p)| {
        // a point light, not an edge or a letter: it also stands out of its close surroundings; a single bright pixel
        // (faint star, hot pixel) counts when the pixel itself stands out twice as far (the 3 × 3 mean dilutes it)
        let peak = (n - a > above && n > ratio * a.max(0.0) && n - c > spot * above) || (p - a > 2.0 * above && p > ratio * a.max(0.0) && p - c > 2.0 * spot * above);
        let n = n.max(p * 0.5);
        if n > CLIP_LEVEL || (peak && n - a > STRONG_ABOVE) { 2 } else if peak { 1 } else { 0 }
    }).collect();
    if !seed.iter().any(|v| *v > 0) {
        return;
    }
    // grow by the ring radius of each kind (any seed within the window), then feather
    let grow = |kind: u8, r: usize| -> Vec<f32> {
        let m: Vec<f32> = seed.par_iter().map(|v| if *v >= kind { 1.0 } else { 0.0 }).collect();
        box_mean(&m, w, h, r).into_par_iter().map(|v| if v > 0.0 { 1.0 } else { 0.0 }).collect()
    };
    let (strong, faint) = (grow(2, STRONG_RADIUS), grow(1, RING_RADIUS));
    let grown: Vec<f32> = strong.par_iter().zip(faint.par_iter()).map(|(a, b)| a.max(*b)).collect();
    let weight = box_mean(&grown, w, h, FEATHER);
    if std::env::var_os("GRAINEO_LOG").is_some() {
        let n = |k: u8| seed.iter().filter(|v| **v == k).count();
        eprintln!("ai denoise protection: noise threshold {above:.4}, seeds clip/strong {} faint {}, area {:.1} %", n(2), n(1), 100.0 * weight.iter().sum::<f32>() / (w * h) as f32);
    }
    den.par_iter_mut().zip(orig.par_iter()).zip(weight.par_iter()).for_each(|((d, o), &k)| {
        if k > 0.0 {
            for c in 0..3 {
                d[c] += k * (o[c] - d[c]);
            }
        }
    });
}

/// Level (network space, white = 1) above which the input is kept.
const CLIP_LEVEL: f32 = 0.9;
/// Close surroundings of a point light (pixels): a spot stands out of them, a broad edge does not.
const SPOT_RADIUS: usize = 4;
/// A local peak: brighter than its surroundings (radius [`PEAK_SURROUND`]) by at least this much, by [`PEAK_NOISE`]
/// times the noise spread, and by this factor (faint stars on a bright night sky stand only a little above it).
const PEAK_ABOVE: f32 = 0.0005;
const PEAK_NOISE: f32 = 8.0;
const PEAK_RATIO: f32 = 2.0;
/// Share of the threshold a point light stands above its close surroundings ([`SPOT_RADIUS`]).
const SPOT_SHARE: f32 = 1.0;
const PEAK_SURROUND: usize = 12;
/// A strong peak (its rings ripple further out): this much above its surroundings.
const STRONG_ABOVE: f32 = 0.05;
/// Margin around a protected spot that covers the rings (faint peaks; strong peaks and clipping), and the width of the
/// soft edge (pixels). Measured on a night sky (sd Quattro, 15 s) and window highlights (DP2 Merrill).
const RING_RADIUS: usize = 12;
const STRONG_RADIUS: usize = 20;
const FEATHER: usize = 6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seams_add_up_to_one() {
        let (w, step) = (2000usize, TILE - 2 * OVERLAP);
        let cols = w.div_ceil(step);
        for x in 0..w {
            let mut s = 0.0;
            for tx in 0..cols {
                let (xb, xe) = (tx * step, ((tx + 1) * step).min(w));
                let (l, r) = (tx > 0, tx + 1 < cols);
                let x0 = if l { xb - OVERLAP } else { xb };
                let x1 = if r { (xe + OVERLAP).min(w) } else { xe };
                if (x0..x1).contains(&x) {
                    s += seam(x, xb, xe, l, r);
                }
            }
            assert!((s - 1.0).abs() < 1e-5, "x {x}: {s}");
        }
    }

    #[test]
    fn highlights_keep_the_input() {
        let (w, h) = (80, 80);
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut orig = vec![[0.01f32; 3]; w * h];
        orig[20 * w + 20] = [0.9; 3];
        let mut den = vec![[0.0f32; 3]; w * h];
        protect_highlights(&orig, &mut den, w, h, &id);
        // the star and its ring zone keep the input, far away the network's result stays
        assert_eq!(den[20 * w + 20], [0.9; 3]);
        assert!((den[20 * w + 24][0] - 0.01).abs() < 1e-6);
        assert!((den[20 * w + 32][0] - 0.01).abs() < 1e-6);
        assert_eq!(den[79 * w + 79], [0.0; 3]);
        // nothing bright: unchanged
        let flat = vec![[0.2f32; 3]; w * h];
        let mut d2 = vec![[0.1f32; 3]; w * h];
        protect_highlights(&flat, &mut d2, w, h, &id);
        assert_eq!(d2[0], [0.1; 3]);
    }

    #[test]
    fn mirror_reflects() {
        assert_eq!(mirror(-1, 5), 1);
        assert_eq!(mirror(5, 5), 3);
        assert_eq!(mirror(-7, 5), 1);
        assert_eq!(mirror(3, 1), 0);
    }

    /// The operators against plain loops on a tiny random network.
    #[test]
    fn operators_match_reference() {
        let mut r = 12345u64;
        let mut rnd = || {
            r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((r >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        let (cin, cout, h, w) = (3, 5, 16, 32);
        let a = Map { c: cin, h, w, d: (0..cin * h * w).map(|_| rnd()).collect() };
        let wt: Vec<f32> = (0..cout * cin * 9).map(|_| rnd()).collect();
        let bias: Vec<f32> = (0..cout).map(|_| rnd()).collect();
        let o = conv(&a, cin, cout, 3, &wt, &bias, Some(0.2));
        for co in 0..cout {
            for y in 0..h {
                for x in 0..w {
                    let mut s = bias[co];
                    for ci in 0..cin {
                        for ky in 0..3 {
                            for kx in 0..3 {
                                let (sy, sx) = (y as isize + ky as isize - 1, x as isize + kx as isize - 1);
                                if sy >= 0 && sy < h as isize && sx >= 0 && sx < w as isize {
                                    s += wt[co * cin * 9 + ci * 9 + ky * 3 + kx] * a.d[ci * h * w + sy as usize * w + sx as usize];
                                }
                            }
                        }
                    }
                    let s = if s < 0.0 { 0.2 * s } else { s };
                    assert!((o.d[co * h * w + y * w + x] - s).abs() < 1e-4);
                }
            }
        }
        // transposed 2×2 convolution
        let wt4: Vec<f32> = (0..cin * cout * 4).map(|_| rnd()).collect();
        let mut re = vec![0f32; cout * 4 * cin];
        for ci in 0..cin {
            for co in 0..cout {
                for s in 0..4 {
                    re[(co * 4 + s) * cin + ci] = wt4[(ci * cout + co) * 4 + s];
                }
            }
        }
        let u = up(&a, cin, cout, &re, &bias);
        for co in 0..cout {
            for y in 0..2 * h {
                for x in 0..2 * w {
                    let mut s = bias[co];
                    for ci in 0..cin {
                        s += wt4[(ci * cout + co) * 4 + (y % 2) * 2 + x % 2] * a.d[ci * h * w + (y / 2) * w + x / 2];
                    }
                    assert!((u.d[co * 4 * h * w + y * 2 * w + x] - s).abs() < 1e-4);
                }
            }
        }
        let p = maxpool(&a);
        assert_eq!(p.d[0], a.d[0].max(a.d[1]).max(a.d[w]).max(a.d[w + 1]));
    }
}
