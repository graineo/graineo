//! Runs a small ONNX network (Conv, BatchNorm, PRelu, Gemm …) on the CPU. Compute core of the
//! regular convolutions and the Gemm via [`crate::gemm`] (Apple Accelerate on macOS), the depthwise convolutions
//! (`group = channels`) directly. Exactly the operators of SFace/MobileFaceNet, no more.

use std::collections::HashMap;

use crate::onnx::{Node, Model, Tensor};

use crate::gemm::matmul;

/// Image/feature map: channels × height × width (one image, no batch).
#[derive(Clone, Debug)]
struct FeatureMap {
    c: usize,
    h: usize,
    b: usize,
    d: Vec<f32>,
}

pub struct Net {
    model: Model,
    input: String,
    output: String,
}

impl Net {
    pub fn new(model: Model) -> Result<Net, String> {
        let input = model.graph_inputs.first().cloned().ok_or("Netz ohne Eingang")?;
        let output = model.graph_outputs.first().cloned().ok_or("Netz ohne Ausgang")?;
        for k in &model.nodes {
            if !["Sub", "Mul", "Add", "Conv", "BatchNormalization", "PRelu", "Relu", "Flatten", "Gemm", "Dropout", "GlobalAveragePool"].contains(&k.op.as_str()) {
                return Err(format!("Operator {} wird nicht unterstützt", k.op));
            }
            Self::check_node(&model, k)?;
        }
        Ok(Net { model, input, output })
    }

    /// Everything the operators index without a check later (inputs, weight shapes, attributes) is verified once
    /// here, so that a damaged or foreign model file ends in an error instead of a panic.
    fn check_node(model: &Model, k: &Node) -> Result<(), String> {
        let bad = |what: &str| Err(format!("{} ({}): {what}", k.op, k.name));
        if k.outputs.is_empty() || k.inputs.is_empty() {
            return bad("ohne Ein- oder Ausgang");
        }
        let weight = |i: usize| -> Result<&Tensor, String> {
            let n = k.inputs.get(i).ok_or_else(|| format!("{} ({}): Eingang {i} fehlt", k.op, k.name))?;
            model.weights.get(n).ok_or_else(|| format!("Gewicht {n} fehlt"))
        };
        match k.op.as_str() {
            "Sub" | "Mul" | "Add" => {
                // the second input is either a constant (checked) or another feature map (checked when run)
                if let Some(n) = k.inputs.get(1) {
                    if let Some(t) = model.weights.get(n) {
                        if t.data.is_empty() {
                            return bad("leere Konstante");
                        }
                    }
                } else {
                    return bad("zweiter Eingang fehlt");
                }
            }
            "BatchNormalization" => {
                let (g, b, m, va) = (weight(1)?, weight(2)?, weight(3)?, weight(4)?);
                if g.data.is_empty() || [b.data.len(), m.data.len(), va.data.len()].iter().any(|l| *l != g.data.len()) {
                    return bad("Parameter unterschiedlich lang");
                }
            }
            "PRelu" => {
                if weight(1)?.data.is_empty() {
                    return bad("ohne Steigung");
                }
            }
            "Gemm" => {
                let w = weight(1)?;
                if w.dims.len() != 2 || w.dims.iter().any(|d| *d == 0) {
                    return bad("Gewicht ist keine Matrix");
                }
                let trans_b = k.int("transB").unwrap_or(0) == 1;
                let n = if trans_b { w.dims[0] } else { w.dims[1] };
                if k.inputs.get(2).map(|b| model.weights.get(b).map_or(true, |b| b.data.len() < n)).unwrap_or(false) {
                    return bad("Bias zu kurz");
                }
            }
            "Conv" => {
                let w = weight(1)?;
                if w.dims.len() != 4 || w.dims.iter().any(|d| *d == 0) {
                    return bad("Gewicht hat nicht vier Dimensionen");
                }
                let groups = k.int("group").unwrap_or(1);
                if groups < 1 || w.dims[0] % groups as usize != 0 {
                    return bad("Gruppen passen nicht zu den Ausgangskanälen");
                }
                if k.ints("strides").is_some_and(|s| s.len() < 2 || s.iter().any(|v| *v < 1)) {
                    return bad("ungültige Schrittweiten");
                }
                if k.ints("pads").is_some_and(|p| p.len() < 4 || p.iter().any(|v| *v < 0)) {
                    return bad("ungültige Ränder");
                }
                if k.inputs.get(2).map(|b| model.weights.get(b).map_or(true, |b| b.data.len() < w.dims[0])).unwrap_or(false) {
                    return bad("Bias zu kurz");
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn weight(&self, n: &str) -> Result<&Tensor, String> {
        self.model.weights.get(n).ok_or_else(|| format!("Gewicht {n} fehlt"))
    }

    /// Input: RGB image as `[height][width][3]` in 0…255, side length `side`. Output: the output vector.
    pub fn infer(&self, rgb: &[u8], side: usize) -> Result<Vec<f32>, String> {
        // Networks without their own normalization (no Sub/Mul at the start) expect (x − 127.5) / 127.5
        let (mean, spread) = if self.model.nodes.first().is_some_and(|k| k.op == "Sub") { (0.0, 1.0) } else { (127.5, 127.5) };
        let mut maps: HashMap<&str, FeatureMap> = HashMap::new();
        let mut d = vec![0f32; 3 * side * side];
        for y in 0..side {
            for x in 0..side {
                for c in 0..3 {
                    d[c * side * side + y * side + x] = (rgb[(y * side + x) * 3 + c] as f32 - mean) / spread;
                }
            }
        }
        maps.insert(&self.input, FeatureMap { c: 3, h: side, b: side, d });
        let mut vector: Option<Vec<f32>> = None;
        let mut vec_name: Option<&str> = None;
        for k in &self.model.nodes {
            let input_name = k.inputs.first().map(String::as_str).unwrap_or("");
            // vector phase after Flatten
            if vec_name == Some(input_name) {
                let v = vector.take().unwrap();
                let new = self.vector_op(k, v)?;
                vec_name = Some(&k.outputs[0]);
                vector = Some(new);
                continue;
            }
            let x = maps.get(input_name).ok_or_else(|| format!("Eingang {input_name} fehlt bei {}", k.name))?;
            // sum of two feature maps (residual connection)
            if k.op == "Add" && k.inputs.get(1).is_some_and(|n| maps.contains_key(n.as_str())) {
                let y = &maps[k.inputs[1].as_str()];
                if y.d.len() != x.d.len() {
                    return Err(format!("Add {}: Größen {} und {} passen nicht", k.name, x.d.len(), y.d.len()));
                }
                let d = x.d.iter().zip(&y.d).map(|(a, b)| a + b).collect();
                let sum_map = FeatureMap { c: x.c, h: x.h, b: x.b, d };
                maps.insert(&k.outputs[0], sum_map);
                continue;
            }
            match k.op.as_str() {
                "Flatten" => {
                    vector = Some(x.d.clone());
                    vec_name = Some(&k.outputs[0]);
                }
                "Dropout" | "Identity" => {
                    let y = x.clone();
                    maps.insert(&k.outputs[0], y);
                }
                _ => {
                    let y = self.map_op(k, x)?;
                    maps.insert(&k.outputs[0], y);
                }
            }
        }
        let _ = &self.output;
        vector.ok_or_else(|| "Netz endet nicht in einem Vektor".into())
    }

    fn vector_op(&self, k: &Node, v: Vec<f32>) -> Result<Vec<f32>, String> {
        match k.op.as_str() {
            "Gemm" => {
                let w = self.weight(&k.inputs[1])?;
                let b = k.inputs.get(2).map(|n| self.weight(n)).transpose()?;
                let trans_b = k.int("transB").unwrap_or(0) == 1;
                let (n, kk) = if trans_b { (w.dims[0], w.dims[1]) } else { (w.dims[1], w.dims[0]) };
                if kk != v.len() {
                    return Err(format!("Gemm: {} Eingänge erwartet, {} vorhanden", kk, v.len()));
                }
                let mut out = vec![0f32; n];
                for (j, o) in out.iter_mut().enumerate() {
                    let mut s = b.map_or(0.0, |b| b.data[j]);
                    for (i, x) in v.iter().enumerate() {
                        s += x * if trans_b { w.data[j * kk + i] } else { w.data[i * n + j] };
                    }
                    *o = s;
                }
                Ok(out)
            }
            "BatchNormalization" => {
                let (g, b, m, va) = (self.weight(&k.inputs[1])?, self.weight(&k.inputs[2])?, self.weight(&k.inputs[3])?, self.weight(&k.inputs[4])?);
                let eps = k.float("epsilon").unwrap_or(1e-5);
                Ok(v.iter().enumerate().map(|(i, x)| g.data[i] * (x - m.data[i]) / (va.data[i] + eps).sqrt() + b.data[i]).collect())
            }
            "Dropout" => Ok(v),
            op => Err(format!("{op} im Vektorteil nicht unterstützt")),
        }
    }

    fn map_op(&self, k: &Node, x: &FeatureMap) -> Result<FeatureMap, String> {
        match k.op.as_str() {
            "Sub" | "Mul" | "Add" => {
                let s = self.weight(&k.inputs[1])?;
                let w = s.data.first().copied().ok_or("leere Konstante")?;
                let f: fn(f32, f32) -> f32 = match k.op.as_str() {
                    "Sub" => |a, b| a - b,
                    "Mul" => |a, b| a * b,
                    _ => |a, b| a + b,
                };
                if s.data.len() != 1 {
                    return Err(format!("{}: nur Einzelwert-Konstanten", k.op));
                }
                Ok(FeatureMap { c: x.c, h: x.h, b: x.b, d: x.d.iter().map(|a| f(*a, w)).collect() })
            }
            "BatchNormalization" => {
                let (g, b, m, va) = (self.weight(&k.inputs[1])?, self.weight(&k.inputs[2])?, self.weight(&k.inputs[3])?, self.weight(&k.inputs[4])?);
                let eps = k.float("epsilon").unwrap_or(1e-5);
                let mut d = x.d.clone();
                let plane = x.h * x.b;
                for c in 0..x.c {
                    let s = g.data[c] / (va.data[c] + eps).sqrt();
                    let t = b.data[c] - m.data[c] * s;
                    for v in &mut d[c * plane..(c + 1) * plane] {
                        *v = *v * s + t;
                    }
                }
                Ok(FeatureMap { d, ..x.clone() })
            }
            "PRelu" => {
                let g = self.weight(&k.inputs[1])?;
                let mut d = x.d.clone();
                let plane = x.h * x.b;
                for c in 0..x.c {
                    let a = if g.data.len() == 1 { g.data[0] } else { g.data[c] };
                    for v in &mut d[c * plane..(c + 1) * plane] {
                        if *v < 0.0 {
                            *v *= a;
                        }
                    }
                }
                Ok(FeatureMap { d, ..x.clone() })
            }
            "Relu" => Ok(FeatureMap { d: x.d.iter().map(|v| v.max(0.0)).collect(), ..x.clone() }),
            "GlobalAveragePool" => {
                let plane = x.h * x.b;
                let d = (0..x.c).map(|c| x.d[c * plane..(c + 1) * plane].iter().sum::<f32>() / plane as f32).collect();
                Ok(FeatureMap { c: x.c, h: 1, b: 1, d })
            }
            "Conv" => self.conv(k, x),
            op => Err(format!("{op} nicht unterstützt")),
        }
    }

    fn conv(&self, k: &Node, x: &FeatureMap) -> Result<FeatureMap, String> {
        let w = self.weight(&k.inputs[1])?;
        let bias = k.inputs.get(2).map(|n| self.weight(n)).transpose()?;
        let (oc, icg, kh, kw) = (w.dims[0], w.dims[1], w.dims[2], w.dims[3]);
        let groups = k.int("group").unwrap_or(1) as usize;
        let strides = k.ints("strides").unwrap_or(&[1, 1]);
        let pads = k.ints("pads").unwrap_or(&[0, 0, 0, 0]);
        let (sy, sx) = (strides[0] as usize, strides[1] as usize);
        let (py, px) = (pads[0] as isize, pads[1] as isize);
        let oh = ((x.h as isize + pads[0] as isize + pads[2] as isize - kh as isize) / sy as isize + 1) as usize;
        let ow = ((x.b as isize + pads[1] as isize + pads[3] as isize - kw as isize) / sx as isize + 1) as usize;
        if icg * groups != x.c {
            return Err(format!("Conv {}: Kanäle {} passen nicht zu Gewicht {}×{}", k.name, x.c, icg, groups));
        }
        let mut out = vec![0f32; oc * oh * ow];
        if groups == 1 {
            // im2col: [icg·kh·kw] × [oh·ow], then weight [oc × icg·kh·kw] · columns
            let kk = icg * kh * kw;
            let n = oh * ow;
            let mut cols = vec![0f32; kk * n];
            for c in 0..icg {
                for ky in 0..kh {
                    for kx in 0..kw {
                        let row = (c * kh + ky) * kw + kx;
                        for oy in 0..oh {
                            let iy = (oy * sy) as isize + ky as isize - py;
                            for ox in 0..ow {
                                let ix = (ox * sx) as isize + kx as isize - px;
                                if iy >= 0 && iy < x.h as isize && ix >= 0 && ix < x.b as isize {
                                    cols[row * n + oy * ow + ox] = x.d[c * x.h * x.b + iy as usize * x.b + ix as usize];
                                }
                            }
                        }
                    }
                }
            }
            out = matmul(&w.data, &cols, oc, kk, n);
        } else {
            // grouped/depthwise: directly per output channel
            let opg = oc / groups;
            for o in 0..oc {
                let g = o / opg;
                for oy in 0..oh {
                    for ox in 0..ow {
                        let mut s = 0f32;
                        for c in 0..icg {
                            let ic = g * icg + c;
                            for ky in 0..kh {
                                let iy = (oy * sy) as isize + ky as isize - py;
                                if iy < 0 || iy >= x.h as isize {
                                    continue;
                                }
                                for kx in 0..kw {
                                    let ix = (ox * sx) as isize + kx as isize - px;
                                    if ix < 0 || ix >= x.b as isize {
                                        continue;
                                    }
                                    s += x.d[ic * x.h * x.b + iy as usize * x.b + ix as usize] * w.data[((o * icg + c) * kh + ky) * kw + kx];
                                }
                            }
                        }
                        out[(o * oh + oy) * ow + ox] = s;
                    }
                }
            }
        }
        if let Some(b) = bias {
            for o in 0..oc {
                for v in &mut out[o * oh * ow..(o + 1) * oh * ow] {
                    *v += b.data[o];
                }
            }
        }
        Ok(FeatureMap { c: oc, h: oh, b: ow, d: out })
    }
}
