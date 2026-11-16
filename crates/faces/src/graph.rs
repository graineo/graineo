//! General executor for small convolutional ONNX graphs with several outputs (face detector YuNet):
//! Conv (regular, depthwise, grouped), Relu, Sigmoid, MaxPool, Resize (nearest), Add, Transpose, Reshape.
//! One image (batch 1), float32, CPU. The regular convolutions run as image columns × weights through
//! [`crate::gemm`], in row strips so that the column buffer stays small even for large images.
//! Intermediate results are freed as soon as no later node needs them.

use std::collections::HashMap;

use crate::onnx::{Model, Node, Tensor};

/// N-dimensional tensor (row-major).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nd {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Nd {
    pub fn new(shape: Vec<usize>, data: Vec<f32>) -> Nd {
        debug_assert_eq!(shape.iter().product::<usize>(), data.len());
        Nd { shape, data }
    }
    fn from_tensor(t: &Tensor) -> Nd {
        Nd { shape: t.dims.clone(), data: t.data.clone() }
    }
    /// (channels, height, width) of an NCHW tensor with batch 1.
    fn chw(&self) -> Result<(usize, usize, usize), String> {
        match self.shape.as_slice() {
            [1, c, h, w] => Ok((*c, *h, *w)),
            s => Err(format!("NCHW mit Stapel 1 erwartet, Form {s:?}")),
        }
    }
}

const SUPPORTED: [&str; 10] = ["Conv", "Relu", "Sigmoid", "MaxPool", "Resize", "Add", "Transpose", "Reshape", "Identity", "Dropout"];

pub struct Graph {
    model: Model,
    /// per node: values whose last use is this node (freed afterwards)
    last_use: Vec<Vec<String>>,
}

impl Graph {
    pub fn new(model: Model) -> Result<Graph, String> {
        if model.graph_inputs.len() != 1 {
            return Err(format!("Netz mit {} Eingängen", model.graph_inputs.len()));
        }
        if model.graph_outputs.is_empty() {
            return Err("Netz ohne Ausgang".into());
        }
        for k in &model.nodes {
            if !SUPPORTED.contains(&k.op.as_str()) {
                return Err(format!("Operator {} wird nicht unterstützt", k.op));
            }
            check_node(&model, k)?;
        }
        // last use of every computed value (graph outputs are kept)
        let mut last: HashMap<&str, usize> = HashMap::new();
        for (i, k) in model.nodes.iter().enumerate() {
            for n in &k.inputs {
                if !model.weights.contains_key(n) {
                    last.insert(n, i);
                }
            }
        }
        let mut last_use = vec![Vec::new(); model.nodes.len()];
        for (n, i) in last {
            if !model.graph_outputs.iter().any(|o| o == n) {
                last_use[i].push(n.to_string());
            }
        }
        Ok(Graph { model, last_use })
    }

    pub fn input_name(&self) -> &str {
        &self.model.graph_inputs[0]
    }

    pub fn output_names(&self) -> &[String] {
        &self.model.graph_outputs
    }

    /// Run the graph; returns all graph outputs by name.
    pub fn run(&self, input: Nd) -> Result<HashMap<String, Nd>, String> {
        let mut values: HashMap<String, Nd> = HashMap::new();
        values.insert(self.model.graph_inputs[0].clone(), input);
        for (i, k) in self.model.nodes.iter().enumerate() {
            let y = self.node(k, &values)?;
            values.insert(k.outputs[0].clone(), y);
            for n in &self.last_use[i] {
                values.remove(n);
            }
        }
        let mut out = HashMap::new();
        for n in &self.model.graph_outputs {
            let v = values.remove(n).ok_or_else(|| format!("Ausgang {n} nicht berechnet"))?;
            out.insert(n.clone(), v);
        }
        Ok(out)
    }

    fn value<'a>(&'a self, values: &'a HashMap<String, Nd>, name: &str, owned: &'a mut Option<Nd>) -> Result<&'a Nd, String> {
        if let Some(v) = values.get(name) {
            return Ok(v);
        }
        let t = self.model.weights.get(name).ok_or_else(|| format!("Wert {name} fehlt"))?;
        *owned = Some(Nd::from_tensor(t));
        Ok(owned.as_ref().unwrap())
    }

    fn weight(&self, name: &str) -> Result<&Tensor, String> {
        self.model.weights.get(name).ok_or_else(|| format!("Gewicht {name} fehlt"))
    }

    fn node(&self, k: &Node, values: &HashMap<String, Nd>) -> Result<Nd, String> {
        let mut tmp = None;
        let x = self.value(values, &k.inputs[0], &mut tmp)?;
        let ctx = |e: String| format!("{} ({}): {e}", k.op, k.name);
        match k.op.as_str() {
            "Conv" => self.conv(k, x).map_err(ctx),
            "Relu" => Ok(Nd { shape: x.shape.clone(), data: x.data.iter().map(|v| v.max(0.0)).collect() }),
            "Sigmoid" => Ok(Nd { shape: x.shape.clone(), data: x.data.iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect() }),
            "Identity" | "Dropout" => Ok(x.clone()),
            "MaxPool" => max_pool(k, x).map_err(ctx),
            "Resize" => self.resize(k, x).map_err(ctx),
            "Add" => {
                let mut tmp2 = None;
                let y = self.value(values, &k.inputs[1], &mut tmp2)?;
                add(x, y).map_err(ctx)
            }
            "Transpose" => transpose(x, k.ints("perm")).map_err(ctx),
            "Reshape" => {
                let s = self.weight(&k.inputs[1])?;
                reshape(x, &s.data).map_err(ctx)
            }
            op => Err(format!("Operator {op} wird nicht unterstützt")),
        }
    }

    fn conv(&self, k: &Node, x: &Nd) -> Result<Nd, String> {
        let w = self.weight(&k.inputs[1])?;
        let bias = k.inputs.get(2).filter(|n| !n.is_empty()).map(|n| self.weight(n)).transpose()?;
        conv2d(x, w, bias.map(|b| b.data.as_slice()), k.int("group").unwrap_or(1) as usize, k.ints("strides").unwrap_or(&[1, 1]), k.ints("pads").unwrap_or(&[0, 0, 0, 0]))
    }

    fn resize(&self, k: &Node, x: &Nd) -> Result<Nd, String> {
        let (c, h, w) = x.chw()?;
        // opset 11: inputs X, roi, scales[, sizes]
        let sizes = k.inputs.get(3).filter(|n| !n.is_empty()).map(|n| self.weight(n)).transpose()?;
        let (oh, ow) = if let Some(s) = sizes.filter(|s| s.data.len() == 4) {
            (s.data[2] as usize, s.data[3] as usize)
        } else {
            let s = k.inputs.get(2).map(|n| self.weight(n)).transpose()?.filter(|s| s.data.len() == 4).ok_or("ohne Maßstab")?;
            if s.data[0] != 1.0 || s.data[1] != 1.0 {
                return Err("Maßstab nur für Höhe und Breite".into());
            }
            ((h as f32 * s.data[2]).floor() as usize, (w as f32 * s.data[3]).floor() as usize)
        };
        let (sy, sx) = (oh as f32 / h as f32, ow as f32 / w as f32);
        let mut d = vec![0f32; c * oh * ow];
        let col: Vec<usize> = (0..ow).map(|ox| ((ox as f32 / sx).floor() as usize).min(w - 1)).collect();
        for ch in 0..c {
            for oy in 0..oh {
                let iy = ((oy as f32 / sy).floor() as usize).min(h - 1);
                let src = &x.data[(ch * h + iy) * w..(ch * h + iy + 1) * w];
                let dst = &mut d[(ch * oh + oy) * ow..(ch * oh + oy + 1) * ow];
                for (o, &ix) in dst.iter_mut().zip(&col) {
                    *o = src[ix];
                }
            }
        }
        Ok(Nd::new(vec![1, c, oh, ow], d))
    }
}

/// Everything the operators index later is checked once when loading, so that a damaged model file ends in an error.
fn check_node(model: &Model, k: &Node) -> Result<(), String> {
    let bad = |what: &str| Err(format!("{} ({}): {what}", k.op, k.name));
    if k.outputs.is_empty() || k.inputs.is_empty() {
        return bad("ohne Ein- oder Ausgang");
    }
    let weight = |i: usize| -> Result<&Tensor, String> {
        let n = k.inputs.get(i).ok_or_else(|| format!("{} ({}): Eingang {i} fehlt", k.op, k.name))?;
        model.weights.get(n).ok_or_else(|| format!("{} ({}): Gewicht {n} fehlt", k.op, k.name))
    };
    match k.op.as_str() {
        "Conv" => {
            let w = weight(1)?;
            if w.dims.len() != 4 || w.dims.iter().any(|d| *d == 0) {
                return bad("Gewicht hat nicht vier Dimensionen");
            }
            let groups = k.int("group").unwrap_or(1);
            if groups < 1 || w.dims[0] % groups as usize != 0 {
                return bad("Gruppen passen nicht zu den Ausgangskanälen");
            }
            if k.ints("strides").is_some_and(|s| s.len() != 2 || s.iter().any(|v| *v < 1)) {
                return bad("ungültige Schrittweiten");
            }
            if k.ints("pads").is_some_and(|p| p.len() != 4 || p.iter().any(|v| *v < 0)) {
                return bad("ungültige Ränder");
            }
            if k.ints("dilations").is_some_and(|d| d.iter().any(|v| *v != 1)) {
                return bad("Dilatation wird nicht unterstützt");
            }
            if k.text("auto_pad").is_some_and(|a| a != "NOTSET") {
                return bad("auto_pad wird nicht unterstützt");
            }
            if let Some(b) = k.inputs.get(2).filter(|n| !n.is_empty()) {
                if model.weights.get(b).map_or(true, |b| b.data.len() != w.dims[0]) {
                    return bad("Bias passt nicht");
                }
            }
        }
        "MaxPool" => {
            if k.ints("kernel_shape").is_none_or(|s| s.len() != 2 || s.iter().any(|v| *v < 1)) {
                return bad("ungültige Fenstergröße");
            }
            if k.ints("strides").is_some_and(|s| s.len() != 2 || s.iter().any(|v| *v < 1)) {
                return bad("ungültige Schrittweiten");
            }
            if k.ints("pads").is_some_and(|p| p.len() != 4 || p.iter().any(|v| *v < 0)) {
                return bad("ungültige Ränder");
            }
            if k.int("ceil_mode").unwrap_or(0) != 0 || k.ints("dilations").is_some_and(|d| d.iter().any(|v| *v != 1)) {
                return bad("ceil_mode/Dilatation werden nicht unterstützt");
            }
        }
        "Resize" => {
            if k.text("mode").unwrap_or("nearest") != "nearest" || k.text("coordinate_transformation_mode").unwrap_or("half_pixel") != "asymmetric" || k.text("nearest_mode").unwrap_or("round_prefer_floor") != "floor" {
                return bad("nur nearest/asymmetric/floor");
            }
            let has = |i: usize| k.inputs.get(i).filter(|n| !n.is_empty()).is_some_and(|n| model.weights.get(n).is_some_and(|t| t.data.len() == 4));
            if !has(2) && !has(3) {
                return bad("Maßstab oder Größe muss eine Konstante sein");
            }
        }
        "Reshape" => {
            weight(1)?;
        }
        "Add" => {
            if k.inputs.len() != 2 {
                return bad("zwei Eingänge erwartet");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Largest number of image-column entries computed at once (bounds the memory of a convolution).
const COLUMN_BUDGET: usize = 1 << 22;

/// 2D convolution (NCHW, batch 1). `pads` = [top, left, bottom, right].
pub fn conv2d(x: &Nd, w: &Tensor, bias: Option<&[f32]>, groups: usize, strides: &[i64], pads: &[i64]) -> Result<Nd, String> {
    let (c, h, wd) = x.chw()?;
    let (oc, icg, kh, kw) = (w.dims[0], w.dims[1], w.dims[2], w.dims[3]);
    if icg * groups != c {
        return Err(format!("Kanäle {c} passen nicht zu Gewicht {icg}×{groups}"));
    }
    let (sy, sx) = (strides[0] as usize, strides[1] as usize);
    let (pt, pl, pb, pr) = (pads[0] as usize, pads[1] as usize, pads[2] as usize, pads[3] as usize);
    if h + pt + pb < kh || wd + pl + pr < kw {
        return Err("Bild kleiner als der Filter".into());
    }
    let oh = (h + pt + pb - kh) / sy + 1;
    let ow = (wd + pl + pr - kw) / sx + 1;
    let mut out = vec![0f32; oc * oh * ow];
    if groups == 1 && kh == 1 && kw == 1 && sy == 1 && sx == 1 && pt + pl + pb + pr == 0 {
        // 1×1: the input itself is the column matrix
        crate::gemm::matmul_into(&w.data, &x.data, &mut out, oc, c, oh * ow);
    } else if groups == 1 {
        let kk = c * kh * kw;
        let rows_per_strip = (COLUMN_BUDGET / (kk * ow).max(1)).clamp(1, oh);
        let mut cols = vec![0f32; kk * rows_per_strip * ow];
        let mut part = vec![0f32; oc * rows_per_strip * ow];
        for y0 in (0..oh).step_by(rows_per_strip) {
            let y1 = (y0 + rows_per_strip).min(oh);
            let n = (y1 - y0) * ow;
            let cols = &mut cols[..kk * n];
            for ch in 0..c {
                for ky in 0..kh {
                    for kx in 0..kw {
                        let row = &mut cols[((ch * kh + ky) * kw + kx) * n..][..n];
                        for oy in y0..y1 {
                            let iy = (oy * sy + ky) as isize - pt as isize;
                            let dst = &mut row[(oy - y0) * ow..(oy - y0 + 1) * ow];
                            if iy < 0 || iy >= h as isize {
                                dst.fill(0.0);
                                continue;
                            }
                            let src = &x.data[(ch * h + iy as usize) * wd..(ch * h + iy as usize + 1) * wd];
                            for (ox, o) in dst.iter_mut().enumerate() {
                                let ix = (ox * sx + kx) as isize - pl as isize;
                                *o = if ix >= 0 && ix < wd as isize { src[ix as usize] } else { 0.0 };
                            }
                        }
                    }
                }
            }
            let part = &mut part[..oc * n];
            crate::gemm::matmul_into(&w.data, cols, part, oc, kk, n);
            for o in 0..oc {
                out[o * oh * ow + y0 * ow..o * oh * ow + y1 * ow].copy_from_slice(&part[o * n..(o + 1) * n]);
            }
        }
    } else {
        let opg = oc / groups;
        for o in 0..oc {
            let g = o / opg;
            let plane = &mut out[o * oh * ow..(o + 1) * oh * ow];
            for ci in 0..icg {
                let ic = g * icg + ci;
                let src = &x.data[ic * h * wd..(ic + 1) * h * wd];
                for ky in 0..kh {
                    for kx in 0..kw {
                        let f = w.data[((o * icg + ci) * kh + ky) * kw + kx];
                        // output columns whose input column lies inside the image
                        let ox_lo = (pl.saturating_sub(kx)).div_ceil(sx);
                        let ox_hi = if wd + pl > kx { ((wd + pl - kx - 1) / sx + 1).min(ow) } else { 0 };
                        if ox_lo >= ox_hi {
                            continue;
                        }
                        for oy in 0..oh {
                            let iy = (oy * sy + ky) as isize - pt as isize;
                            if iy < 0 || iy >= h as isize {
                                continue;
                            }
                            let srow = &src[iy as usize * wd..(iy as usize + 1) * wd];
                            let drow = &mut plane[oy * ow..(oy + 1) * ow];
                            if sx == 1 {
                                let off = ox_lo + kx - pl;
                                for (d, s) in drow[ox_lo..ox_hi].iter_mut().zip(&srow[off..off + (ox_hi - ox_lo)]) {
                                    *d += f * s;
                                }
                            } else {
                                for ox in ox_lo..ox_hi {
                                    drow[ox] += f * srow[ox * sx + kx - pl];
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(b) = bias {
        for o in 0..oc {
            for v in &mut out[o * oh * ow..(o + 1) * oh * ow] {
                *v += b[o];
            }
        }
    }
    Ok(Nd::new(vec![1, oc, oh, ow], out))
}

fn max_pool(k: &Node, x: &Nd) -> Result<Nd, String> {
    let (c, h, w) = x.chw()?;
    let ks = k.ints("kernel_shape").ok_or("ohne Fenstergröße")?;
    let st = k.ints("strides").unwrap_or(&[1, 1]);
    let pads = k.ints("pads").unwrap_or(&[0, 0, 0, 0]);
    let (kh, kw, sy, sx) = (ks[0] as usize, ks[1] as usize, st[0] as usize, st[1] as usize);
    let (pt, pl) = (pads[0] as isize, pads[1] as isize);
    let (ph, pw) = ((pads[0] + pads[2]) as usize, (pads[1] + pads[3]) as usize);
    if h + ph < kh || w + pw < kw {
        return Err("Bild kleiner als das Fenster".into());
    }
    let oh = (h + ph - kh) / sy + 1;
    let ow = (w + pw - kw) / sx + 1;
    let mut d = vec![f32::NEG_INFINITY; c * oh * ow];
    for ch in 0..c {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut m = f32::NEG_INFINITY;
                for ky in 0..kh {
                    let iy = (oy * sy + ky) as isize - pt;
                    if iy < 0 || iy >= h as isize {
                        continue;
                    }
                    for kx in 0..kw {
                        let ix = (ox * sx + kx) as isize - pl;
                        if ix >= 0 && ix < w as isize {
                            m = m.max(x.data[(ch * h + iy as usize) * w + ix as usize]);
                        }
                    }
                }
                d[(ch * oh + oy) * ow + ox] = m;
            }
        }
    }
    Ok(Nd::new(vec![1, c, oh, ow], d))
}

/// Element-wise sum; the second operand may also be a single value or one value per channel (NCHW).
fn add(x: &Nd, y: &Nd) -> Result<Nd, String> {
    if x.data.len() == y.data.len() {
        return Ok(Nd { shape: x.shape.clone(), data: x.data.iter().zip(&y.data).map(|(a, b)| a + b).collect() });
    }
    if y.data.len() == 1 {
        return Ok(Nd { shape: x.shape.clone(), data: x.data.iter().map(|a| a + y.data[0]).collect() });
    }
    if let Ok((c, h, w)) = x.chw() {
        if y.data.len() == c {
            let mut d = x.data.clone();
            for (ch, plane) in d.chunks_exact_mut(h * w).enumerate() {
                plane.iter_mut().for_each(|v| *v += y.data[ch]);
            }
            return Ok(Nd { shape: x.shape.clone(), data: d });
        }
    }
    Err(format!("Formen {:?} und {:?} passen nicht", x.shape, y.shape))
}

pub fn transpose(x: &Nd, perm: Option<&[i64]>) -> Result<Nd, String> {
    let r = x.shape.len();
    let perm: Vec<usize> = match perm {
        Some(p) => p.iter().map(|v| *v as usize).collect(),
        None => (0..r).rev().collect(),
    };
    let mut seen = vec![false; r];
    if perm.len() != r || perm.iter().any(|p| *p >= r || std::mem::replace(&mut seen[*p], true)) {
        return Err(format!("ungültige Permutation {perm:?} für Rang {r}"));
    }
    let shape: Vec<usize> = perm.iter().map(|p| x.shape[*p]).collect();
    // strides of the input, read in the order of the output axes
    let mut in_strides = vec![1usize; r];
    for i in (0..r.saturating_sub(1)).rev() {
        in_strides[i] = in_strides[i + 1] * x.shape[i + 1];
    }
    let s: Vec<usize> = perm.iter().map(|p| in_strides[*p]).collect();
    let mut d = Vec::with_capacity(x.data.len());
    let mut idx = vec![0usize; r];
    for _ in 0..x.data.len() {
        let off: usize = idx.iter().zip(&s).map(|(i, s)| i * s).sum();
        d.push(x.data[off]);
        for a in (0..r).rev() {
            idx[a] += 1;
            if idx[a] < shape[a] {
                break;
            }
            idx[a] = 0;
        }
    }
    Ok(Nd::new(shape, d))
}

/// ONNX Reshape: 0 keeps the input dimension, −1 is derived.
pub fn reshape(x: &Nd, target: &[f32]) -> Result<Nd, String> {
    let mut shape: Vec<isize> = target.iter().map(|v| *v as isize).collect();
    for (i, s) in shape.iter_mut().enumerate() {
        if *s == 0 {
            *s = *x.shape.get(i).ok_or("0 außerhalb der Eingangsform")? as isize;
        }
    }
    let known: isize = shape.iter().filter(|s| **s > 0).product();
    let free = shape.iter().filter(|s| **s == -1).count();
    if free > 1 || shape.iter().any(|s| *s < -1) || known == 0 {
        return Err(format!("ungültige Zielform {target:?}"));
    }
    let n = x.data.len() as isize;
    let shape: Vec<usize> = shape.iter().map(|s| if *s == -1 { (n / known) as usize } else { *s as usize }).collect();
    if shape.iter().product::<usize>() != x.data.len() {
        return Err(format!("Zielform {shape:?} passt nicht zu {} Werten", x.data.len()));
    }
    Ok(Nd::new(shape, x.data.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(dims: Vec<usize>, data: Vec<f32>) -> Tensor {
        Tensor { dims, data }
    }

    /// Reference convolution straight from the definition.
    fn conv_ref(x: &Nd, w: &Tensor, bias: Option<&[f32]>, groups: usize, s: usize, p: usize) -> Nd {
        let (c, h, wd) = x.chw().unwrap();
        let (oc, icg, kh, kw) = (w.dims[0], w.dims[1], w.dims[2], w.dims[3]);
        let oh = (h + 2 * p - kh) / s + 1;
        let ow = (wd + 2 * p - kw) / s + 1;
        let opg = oc / groups;
        let mut d = vec![0f32; oc * oh * ow];
        for o in 0..oc {
            for oy in 0..oh {
                for ox in 0..ow {
                    let mut sum = bias.map_or(0.0, |b| b[o]);
                    for ci in 0..icg {
                        let ic = (o / opg) * icg + ci;
                        for ky in 0..kh {
                            for kx in 0..kw {
                                let (iy, ix) = ((oy * s + ky) as isize - p as isize, (ox * s + kx) as isize - p as isize);
                                if iy >= 0 && iy < h as isize && ix >= 0 && ix < wd as isize {
                                    sum += x.data[(ic * h + iy as usize) * wd + ix as usize] * w.data[((o * icg + ci) * kh + ky) * kw + kx];
                                }
                            }
                        }
                    }
                    d[(o * oh + oy) * ow + ox] = sum;
                }
            }
        }
        let _ = c;
        Nd::new(vec![1, oc, oh, ow], d)
    }

    fn pseudo(n: usize, seed: usize) -> Vec<f32> {
        (0..n).map(|i| (((i * 2654435761 + seed * 40503) % 1000) as f32 - 500.0) / 250.0).collect()
    }

    fn close(a: &Nd, b: &Nd) {
        assert_eq!(a.shape, b.shape);
        for (i, (x, y)) in a.data.iter().zip(&b.data).enumerate() {
            assert!((x - y).abs() < 1e-3 * (1.0 + y.abs()), "index {i}: {x} vs {y}");
        }
    }

    #[test]
    fn conv_variants_match_definition() {
        let x = Nd::new(vec![1, 6, 13, 17], pseudo(6 * 13 * 17, 1));
        // (out channels, groups, kernel, stride, pad)
        for &(oc, groups, k, s, p) in &[(8, 1, 1, 1, 0), (8, 1, 3, 1, 1), (4, 1, 3, 2, 1), (6, 6, 3, 1, 1), (6, 6, 3, 2, 1), (12, 3, 3, 1, 1), (5, 1, 5, 1, 2)] {
            let w = t(vec![oc, 6 / groups, k, k], pseudo(oc * (6 / groups) * k * k, oc + k));
            let b = pseudo(oc, 7);
            let got = conv2d(&x, &w, Some(&b), groups, &[s as i64, s as i64], &[p as i64; 4]).unwrap();
            close(&got, &conv_ref(&x, &w, Some(&b), groups, s, p));
        }
    }

    #[test]
    fn conv_in_strips_matches_whole() {
        // more columns than the budget: computed in several strips
        let x = Nd::new(vec![1, 3, 700, 650], pseudo(3 * 700 * 650, 3));
        let w = t(vec![4, 3, 3, 3], pseudo(4 * 27, 9));
        let got = conv2d(&x, &w, None, 1, &[1, 1], &[1, 1, 1, 1]).unwrap();
        assert!(3 * 9 * 650 * 700 > COLUMN_BUDGET);
        close(&got, &conv_ref(&x, &w, None, 1, 1, 1));
    }

    #[test]
    fn max_pool_resize_transpose_reshape() {
        let mut k = Node { op: "MaxPool".into(), ..Default::default() };
        k.attr.insert("kernel_shape".into(), crate::onnx::Attr::Ints(vec![2, 2]));
        k.attr.insert("strides".into(), crate::onnx::Attr::Ints(vec![2, 2]));
        let x = Nd::new(vec![1, 1, 2, 4], vec![1.0, 5.0, -1.0, 0.0, 3.0, 2.0, -4.0, -2.0]);
        assert_eq!(max_pool(&k, &x).unwrap(), Nd::new(vec![1, 1, 1, 2], vec![5.0, 0.0]));

        let mut m = Model::default();
        m.weights.insert("s".into(), t(vec![4], vec![1.0, 1.0, 2.0, 2.0]));
        m.weights.insert("roi".into(), t(vec![0], vec![]));
        let g = Graph { model: m, last_use: Vec::new() };
        let r = Node { op: "Resize".into(), inputs: vec!["x".into(), "roi".into(), "s".into()], ..Default::default() };
        let x = Nd::new(vec![1, 1, 2, 2], vec![1.0, 2.0, 3.0, 4.0]);
        let y = g.resize(&r, &x).unwrap();
        assert_eq!(y.data, vec![1.0, 1.0, 2.0, 2.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 3.0, 3.0, 4.0, 4.0]);

        // NCHW → NHWC
        let x = Nd::new(vec![1, 2, 2, 3], (0..12).map(|v| v as f32).collect());
        let y = transpose(&x, Some(&[0, 2, 3, 1])).unwrap();
        assert_eq!(y.shape, vec![1, 2, 3, 2]);
        assert_eq!(y.data, vec![0.0, 6.0, 1.0, 7.0, 2.0, 8.0, 3.0, 9.0, 4.0, 10.0, 5.0, 11.0]);
        let z = reshape(&y, &[1.0, -1.0, 2.0]).unwrap();
        assert_eq!(z.shape, vec![1, 6, 2]);
        assert!(reshape(&y, &[5.0, -1.0]).is_err());
    }

    #[test]
    fn add_broadcasts_per_channel() {
        let x = Nd::new(vec![1, 2, 1, 2], vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(add(&x, &Nd::new(vec![2], vec![10.0, 20.0])).unwrap().data, vec![11.0, 12.0, 23.0, 24.0]);
        assert_eq!(add(&x, &x).unwrap().data, vec![2.0, 4.0, 6.0, 8.0]);
        assert!(add(&x, &Nd::new(vec![3], vec![0.0; 3])).is_err());
    }
}
