//! SigLIP 2 (Google, Apache-2.0) for the semantic search, reimplemented in Rust like the CLIP
//! encoders in `semantic.rs` (matrix products via Apple Accelerate, model files read directly).
//!
//! - **Image tower:** ViT with 16×16 patches at 256×256 (256 tokens, learned positions, no class token),
//!   pre-LN layers (GELU, tanh form), final LayerNorm, then attention pooling: one learned query
//!   ("probe") attends over all tokens, followed by LayerNorm + MLP with residual. Output = image vector.
//! - **Text tower:** same layer type, 64 positions; text is lower-cased, tokenized with the Gemma
//!   SentencePiece vocabulary (`sentencepiece.rs`), followed by `<eos>` and padded with `<pad>` to 64
//!   tokens (no attention mask, as in training). The vector is the last position after the final
//!   LayerNorm, through a linear head.
//! - Both vectors are normalized; the ranking uses the dot product (= cosine). The sigmoid of the
//!   training (scale and bias) is monotonic and therefore not needed for the ranking.
//! - Preprocessing of photos: whole picture scaled to 256×256 (no crop, so the edges count too),
//!   bilinear with antialiasing, values to −1…1.
//!
//! Memory: only the tower in use is loaded; the 256 000-row word table of the text tower stays in
//! the file and only the rows of the query are read.
//!
//! Checked numerically against the Python reference (cosine > 0.999, see `tests/siglip_measure.rs`).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::sentencepiece::SentencePiece;

#[cfg(target_os = "macos")]
#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn cblas_sgemm(order: i32, ta: i32, tb: i32, m: i32, n: i32, k: i32, alpha: f32, a: *const f32, lda: i32, b: *const f32, ldb: i32, beta: f32, c: *mut f32, ldc: i32);
    // vForce: tanh and exp over whole arrays (vectorized; the scalar functions cost as much as the matrix products)
    fn vvtanhf(y: *mut f32, x: *const f32, n: *const i32);
    fn vvexpf(y: *mut f32, x: *const f32, n: *const i32);
}

/// In place: v = tanh(v) / exp(v) over the whole slice.
fn tanh_all(v: &mut [f32]) {
    #[cfg(target_os = "macos")]
    for c in v.chunks_mut(i32::MAX as usize) {
        let n = c.len() as i32;
        unsafe { vvtanhf(c.as_mut_ptr(), c.as_ptr(), &n) };
    }
    #[cfg(not(target_os = "macos"))]
    v.iter_mut().for_each(|x| *x = x.tanh());
}

fn exp_all(v: &mut [f32]) {
    #[cfg(target_os = "macos")]
    for c in v.chunks_mut(i32::MAX as usize) {
        let n = c.len() as i32;
        unsafe { vvexpf(c.as_mut_ptr(), c.as_ptr(), &n) };
    }
    #[cfg(not(target_os = "macos"))]
    v.iter_mut().for_each(|x| *x = x.exp());
}

/// C[m×n] = op(A)·op(B) with leading dimensions (row-major), like BLAS `sgemm`. The slices are checked
/// against the shapes first: BLAS does not know their lengths.
#[allow(clippy::too_many_arguments)]
fn gemm(ta: bool, tb: bool, m: usize, n: usize, k: usize, a: &[f32], lda: usize, b: &[f32], ldb: usize, c: &mut [f32], ldc: usize) {
    assert!(m > 0 && n > 0 && k > 0);
    assert!([m, n, k, lda, ldb, ldc].iter().all(|&x| x <= i32::MAX as usize));
    // last element that will be touched + 1
    let need = |rows: usize, cols: usize, ld: usize| (rows - 1) * ld + cols;
    let (ar, ac) = if ta { (k, m) } else { (m, k) };
    let (br, bc) = if tb { (n, k) } else { (k, n) };
    assert!(ac <= lda && bc <= ldb && n <= ldc);
    assert!(a.len() >= need(ar, ac, lda) && b.len() >= need(br, bc, ldb) && c.len() >= need(m, n, ldc), "matrix does not match its shape");
    #[cfg(target_os = "macos")]
    unsafe {
        cblas_sgemm(101, if ta { 112 } else { 111 }, if tb { 112 } else { 111 }, m as i32, n as i32, k as i32, 1.0, a.as_ptr(), lda as i32, b.as_ptr(), ldb as i32, 0.0, c.as_mut_ptr(), ldc as i32);
    }
    #[cfg(not(target_os = "macos"))]
    for i in 0..m {
        for j in 0..n {
            let mut s = 0.0;
            for t in 0..k {
                let x = if ta { a[t * lda + i] } else { a[i * lda + t] };
                let y = if tb { b[j * ldb + t] } else { b[t * ldb + j] };
                s += x * y;
            }
            c[i * ldc + j] = s;
        }
    }
}

pub struct Tensor {
    pub form: Vec<usize>,
    pub data: Vec<f32>,
}

/// y[m×out] = x[m×in] · Wᵀ + b
fn linear(x: &[f32], m: usize, w: &Tensor, b: &Tensor) -> Vec<f32> {
    let (out, inn) = (w.form[0], w.form[1]);
    let mut y = vec![0f32; m * out];
    gemm(false, true, m, out, inn, x, inn, &w.data, inn, &mut y, out);
    for r in y.chunks_mut(out) {
        for (v, bb) in r.iter_mut().zip(&b.data) {
            *v += bb;
        }
    }
    y
}

fn layernorm(x: &mut [f32], d: usize, g: &Tensor, b: &Tensor) {
    for r in x.chunks_mut(d) {
        let mean = r.iter().sum::<f32>() / d as f32;
        let var = r.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / d as f32;
        let inv = 1.0 / (var + 1e-6).sqrt();
        for (i, v) in r.iter_mut().enumerate() {
            *v = (*v - mean) * inv * g.data[i] + b.data[i];
        }
    }
}

/// GELU, tanh form (as trained).
fn gelu(v: &mut [f32]) {
    const C: f32 = 0.797_884_6; // sqrt(2/π)
    let mut t: Vec<f32> = v.iter().map(|&x| C * (x + 0.044_715 * x * x * x)).collect();
    tanh_all(&mut t);
    for (x, t) in v.iter_mut().zip(&t) {
        *x = 0.5 * *x * (1.0 + t);
    }
}

/// Multi-head attention of `nq` queries over `nk` keys (rows of width `d`, `heads` heads), without mask.
fn attend(q: &[f32], nq: usize, k: &[f32], v: &[f32], nk: usize, d: usize, heads: usize) -> Vec<f32> {
    let hd = d / heads;
    let scale = 1.0 / (hd as f32).sqrt();
    let mut ctx = vec![0f32; nq * d];
    let mut s = vec![0f32; nq * nk];
    for h in 0..heads {
        let o = h * hd;
        gemm(false, true, nq, nk, hd, &q[o..], d, &k[o..], d, &mut s, nk);
        for row in s.chunks_mut(nk) {
            let m = row.iter().cloned().fold(f32::MIN, f32::max);
            row.iter_mut().for_each(|x| *x = (*x - m) * scale);
        }
        exp_all(&mut s);
        for row in s.chunks_mut(nk) {
            let inv = 1.0 / row.iter().sum::<f32>();
            row.iter_mut().for_each(|x| *x *= inv);
        }
        gemm(false, false, nq, hd, nk, &s, nk, &v[o..], d, &mut ctx[o..], d);
    }
    ctx
}

// ───────────────────────────────────────────── Model file

/// Safetensors read piece by piece (only the requested tensors, F32), without loading the whole
/// file: the large model has 3.5 GB of which one tower needs a part.
struct ModelFile {
    file: Mutex<std::fs::File>,
    base: u64,
    /// name → (shape, start, end) relative to `base`
    index: HashMap<String, (Vec<usize>, u64, u64)>,
    len: u64,
}

impl ModelFile {
    fn open(p: &Path) -> Result<ModelFile, String> {
        let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let len = f.metadata().map_err(|e| e.to_string())?.len();
        let mut h8 = [0u8; 8];
        f.read_exact(&mut h8).map_err(|_| "Modellkopf zu kurz")?;
        let n = u64::from_le_bytes(h8);
        if n > 16 * 1024 * 1024 || n + 8 > len {
            return Err("Modellkopf zu groß oder abgeschnitten".into());
        }
        let mut h = vec![0u8; n as usize];
        f.read_exact(&mut h).map_err(|_| "Modellkopf abgeschnitten")?;
        let header: serde_json::Value = serde_json::from_slice(&h).map_err(|e| e.to_string())?;
        let mut index = HashMap::new();
        for (name, v) in header.as_object().ok_or("Modellkopf ist kein Objekt")? {
            if name == "__metadata__" || v["dtype"] != "F32" {
                continue;
            }
            let form: Vec<usize> = v["shape"].as_array().ok_or("Modellform fehlt")?.iter().map(|x| x.as_u64().and_then(|x| usize::try_from(x).ok()).ok_or("Ungültige Modelldimension")).collect::<Result<_, _>>()?;
            let o = v["data_offsets"].as_array().filter(|o| o.len() == 2).ok_or("Modell benötigt zwei Datenpositionen")?;
            let (a, e) = (o[0].as_u64().ok_or("Datenposition")?, o[1].as_u64().ok_or("Datenposition")?);
            let count = form.iter().try_fold(1u64, |n, &d| n.checked_mul(d as u64)).ok_or("Modellform zu groß")?;
            if e < a || e - a != count * 4 || 8 + n + e > len {
                return Err(format!("Modelldaten von „{name}“ passen nicht zur Form oder Dateigröße"));
            }
            index.insert(name.clone(), (form, a, e));
        }
        Ok(ModelFile { file: Mutex::new(f), base: 8 + n, index, len })
    }

    fn shape(&self, name: &str) -> Option<&[usize]> {
        self.index.get(name).map(|x| x.0.as_slice())
    }

    fn read_range(&self, at: u64, bytes: usize) -> Result<Vec<f32>, String> {
        if at + bytes as u64 > self.len {
            return Err("Modelldatei abgeschnitten".into());
        }
        let mut raw = vec![0u8; bytes];
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        f.read_exact(&mut raw).map_err(|e| e.to_string())?;
        Ok(raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
    }

    /// Whole tensor; `form` must match exactly.
    fn tensor(&self, name: &str, form: &[usize]) -> Result<Tensor, String> {
        let (f, a, e) = self.index.get(name).ok_or_else(|| format!("Modell passt nicht: Tensor „{name}“ fehlt"))?;
        if f != form {
            return Err(format!("Modell passt nicht: „{name}“ hat die Form {f:?}, erwartet {form:?}"));
        }
        Ok(Tensor { form: f.clone(), data: self.read_range(self.base + a, (e - a) as usize)? })
    }

    /// One row of a 2-D tensor (word table).
    fn row(&self, name: &str, r: usize) -> Result<Vec<f32>, String> {
        let (f, a, _) = self.index.get(name).ok_or_else(|| format!("Tensor „{name}“ fehlt"))?;
        if f.len() != 2 || r >= f[0] {
            return Err(format!("Zeile {r} von „{name}“ fehlt"));
        }
        self.read_range(self.base + a + (r * f[1] * 4) as u64, f[1] * 4)
    }
}

// ───────────────────────────────────────────── Shared layer stack

/// Size of a tower; the head count follows the width (64 values per head for Base and Large).
#[derive(Clone, Copy, Debug)]
struct Size {
    d: usize,
    layers: usize,
    mlp: usize,
    heads: usize,
}

struct Layer {
    ln1: (Tensor, Tensor),
    q: (Tensor, Tensor),
    k: (Tensor, Tensor),
    v: (Tensor, Tensor),
    o: (Tensor, Tensor),
    ln2: (Tensor, Tensor),
    fc1: (Tensor, Tensor),
    fc2: (Tensor, Tensor),
}

fn pair(m: &ModelFile, p: &str, w: &[usize], b: usize) -> Result<(Tensor, Tensor), String> {
    Ok((m.tensor(&format!("{p}.weight"), w)?, m.tensor(&format!("{p}.bias"), &[b])?))
}

fn norm(m: &ModelFile, p: &str, d: usize) -> Result<(Tensor, Tensor), String> {
    pair(m, p, &[d], d)
}

/// Determine the size of a tower from the model file (`prefix` = `vision_model` or `text_model`).
fn size_of(m: &ModelFile, prefix: &str) -> Result<Size, String> {
    let d = m.shape(&format!("{prefix}.encoder.layers.0.self_attn.q_proj.weight")).filter(|s| s.len() == 2).ok_or("Modell passt nicht: keine Schichten gefunden")?[0];
    let mlp = m.shape(&format!("{prefix}.encoder.layers.0.mlp.fc1.weight")).ok_or("Modell passt nicht: MLP fehlt")?[0];
    let layers = (0..).take_while(|l| m.shape(&format!("{prefix}.encoder.layers.{l}.layer_norm1.weight")).is_some()).count();
    if d % 64 != 0 || !(1..=64).contains(&layers) {
        return Err(format!("Modell passt nicht: Breite {d}, {layers} Schichten"));
    }
    Ok(Size { d, layers, mlp, heads: d / 64 })
}

fn load_layers(m: &ModelFile, prefix: &str, s: Size) -> Result<Vec<Layer>, String> {
    (0..s.layers)
        .map(|l| {
            let p = |x: &str| format!("{prefix}.encoder.layers.{l}.{x}");
            Ok(Layer {
                ln1: norm(m, &p("layer_norm1"), s.d)?,
                q: pair(m, &p("self_attn.q_proj"), &[s.d, s.d], s.d)?,
                k: pair(m, &p("self_attn.k_proj"), &[s.d, s.d], s.d)?,
                v: pair(m, &p("self_attn.v_proj"), &[s.d, s.d], s.d)?,
                o: pair(m, &p("self_attn.out_proj"), &[s.d, s.d], s.d)?,
                ln2: norm(m, &p("layer_norm2"), s.d)?,
                fc1: pair(m, &p("mlp.fc1"), &[s.mlp, s.d], s.mlp)?,
                fc2: pair(m, &p("mlp.fc2"), &[s.d, s.mlp], s.d)?,
            })
        })
        .collect()
}

/// Run the layer stack over `b` sequences of `n` tokens each (rows of `x`, width `s.d`).
fn run_layers(x: &mut [f32], b: usize, n: usize, s: Size, layers: &[Layer]) {
    let d = s.d;
    let rows = b * n;
    for l in layers {
        let mut h = x.to_vec();
        layernorm(&mut h, d, &l.ln1.0, &l.ln1.1);
        let q = linear(&h, rows, &l.q.0, &l.q.1);
        let k = linear(&h, rows, &l.k.0, &l.k.1);
        let v = linear(&h, rows, &l.v.0, &l.v.1);
        let mut ctx = vec![0f32; rows * d];
        for i in 0..b {
            let r = i * n * d..(i + 1) * n * d;
            let c = attend(&q[r.clone()], n, &k[r.clone()], &v[r.clone()], n, d, s.heads);
            ctx[r].copy_from_slice(&c);
        }
        let a = linear(&ctx, rows, &l.o.0, &l.o.1);
        for (xv, av) in x.iter_mut().zip(&a) {
            *xv += av;
        }
        let mut h = x.to_vec();
        layernorm(&mut h, d, &l.ln2.0, &l.ln2.1);
        let mut m = linear(&h, rows, &l.fc1.0, &l.fc1.1);
        gelu(&mut m);
        let f = linear(&m, rows, &l.fc2.0, &l.fc2.1);
        for (xv, fv) in x.iter_mut().zip(&f) {
            *xv += fv;
        }
    }
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
}

// ───────────────────────────────────────────── Image tower

pub struct ImageTower {
    s: Size,
    patch: usize,
    side: usize,
    patch_w: Tensor,
    patch_b: Tensor,
    pos: Tensor,
    layers: Vec<Layer>,
    post: (Tensor, Tensor),
    probe: Tensor,
    head_in_w: Tensor,
    head_in_b: Tensor,
    head_out: (Tensor, Tensor),
    head_ln: (Tensor, Tensor),
    head_fc1: (Tensor, Tensor),
    head_fc2: (Tensor, Tensor),
}

impl ImageTower {
    /// From the model folder (`model.safetensors`); only the image tower is read.
    pub fn load(dir: &Path) -> Result<ImageTower, String> {
        let m = ModelFile::open(&dir.join("model.safetensors"))?;
        let s = size_of(&m, "vision_model")?;
        let pw = m.shape("vision_model.embeddings.patch_embedding.weight").ok_or("Modell passt nicht: Bildeingang fehlt")?.to_vec();
        if pw.len() != 4 || pw[0] != s.d || pw[1] != 3 || pw[2] != pw[3] {
            return Err(format!("Modell passt nicht: Bildeingang {pw:?}"));
        }
        let patch = pw[2];
        let tokens = m.shape("vision_model.embeddings.position_embedding.weight").ok_or("Modell passt nicht: Positionen fehlen")?[0];
        let grid = (tokens as f64).sqrt() as usize;
        if grid * grid != tokens {
            return Err("Modell passt nicht: Positionen bilden kein Quadrat".into());
        }
        let d = s.d;
        Ok(ImageTower {
            s,
            patch,
            side: grid * patch,
            patch_w: m.tensor("vision_model.embeddings.patch_embedding.weight", &pw)?,
            patch_b: m.tensor("vision_model.embeddings.patch_embedding.bias", &[d])?,
            pos: m.tensor("vision_model.embeddings.position_embedding.weight", &[tokens, d])?,
            layers: load_layers(&m, "vision_model", s)?,
            post: norm(&m, "vision_model.post_layernorm", d)?,
            probe: m.tensor("vision_model.head.probe", &[1, 1, d])?,
            head_in_w: m.tensor("vision_model.head.attention.in_proj_weight", &[3 * d, d])?,
            head_in_b: m.tensor("vision_model.head.attention.in_proj_bias", &[3 * d])?,
            head_out: pair(&m, "vision_model.head.attention.out_proj", &[d, d], d)?,
            head_ln: norm(&m, "vision_model.head.layernorm", d)?,
            head_fc1: pair(&m, "vision_model.head.mlp.fc1", &[s.mlp, d], s.mlp)?,
            head_fc2: pair(&m, "vision_model.head.mlp.fc2", &[d, s.mlp], d)?,
        })
    }

    /// Width of the vectors.
    pub fn dim(&self) -> usize {
        self.s.d
    }

    /// Input side in pixels (256).
    pub fn side(&self) -> usize {
        self.side
    }

    /// Preprocessing: whole picture to side×side (bilinear with antialiasing), channels first, −1…1.
    pub fn prepare(&self, img: &image::RgbImage) -> Vec<f32> {
        let n = self.side as u32;
        let r = image::imageops::resize(img, n, n, image::imageops::FilterType::Triangle);
        let mut out = vec![0f32; 3 * self.side * self.side];
        let plane = self.side * self.side;
        for (i, p) in r.pixels().enumerate() {
            for c in 0..3 {
                out[c * plane + i] = p[c] as f32 / 127.5 - 1.0;
            }
        }
        out
    }

    /// Photos → normalized vectors (computed together, which uses the matrix units better).
    pub fn embed(&self, imgs: &[&image::RgbImage]) -> Vec<Vec<f32>> {
        let px: Vec<Vec<f32>> = imgs.iter().map(|i| self.prepare(i)).collect();
        self.embed_prepared(&px)
    }

    /// Already prepared pixels (channels first, side×side, −1…1) → normalized vectors.
    pub fn embed_prepared(&self, px: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let b = px.len();
        if b == 0 {
            return Vec::new();
        }
        let (d, p, side) = (self.s.d, self.patch, self.side);
        let g = side / p;
        let n = g * g;
        let pl = 3 * p * p;
        let plane = side * side;
        // patches as rows [b·n × 3·p·p] (order channel, y, x like the convolution weights)
        let mut patches = vec![0f32; b * n * pl];
        for (bi, img) in px.iter().enumerate() {
            assert_eq!(img.len(), 3 * plane, "prepared image has the wrong size");
            for gy in 0..g {
                for gx in 0..g {
                    let row = &mut patches[(bi * n + gy * g + gx) * pl..][..pl];
                    for c in 0..3 {
                        for y in 0..p {
                            let src = &img[c * plane + (gy * p + y) * side + gx * p..][..p];
                            row[c * p * p + y * p..][..p].copy_from_slice(src);
                        }
                    }
                }
            }
        }
        let mut x = vec![0f32; b * n * d];
        gemm(false, true, b * n, d, pl, &patches, pl, &self.patch_w.data, pl, &mut x, d);
        for (r, row) in x.chunks_mut(d).enumerate() {
            let pos = &self.pos.data[(r % n) * d..][..d];
            for ((v, bb), pp) in row.iter_mut().zip(&self.patch_b.data).zip(pos) {
                *v += bb + pp;
            }
        }
        run_layers(&mut x, b, n, self.s, &self.layers);
        layernorm(&mut x, d, &self.post.0, &self.post.1);
        // attention pooling: one learned query over all tokens of each image
        let wq = Tensor { form: vec![d, d], data: self.head_in_w.data[..d * d].to_vec() };
        let bq = Tensor { form: vec![d], data: self.head_in_b.data[..d].to_vec() };
        let wk = Tensor { form: vec![d, d], data: self.head_in_w.data[d * d..2 * d * d].to_vec() };
        let bk = Tensor { form: vec![d], data: self.head_in_b.data[d..2 * d].to_vec() };
        let wv = Tensor { form: vec![d, d], data: self.head_in_w.data[2 * d * d..].to_vec() };
        let bv = Tensor { form: vec![d], data: self.head_in_b.data[2 * d..].to_vec() };
        let q = linear(&self.probe.data, 1, &wq, &bq);
        let k = linear(&x, b * n, &wk, &bk);
        let v = linear(&x, b * n, &wv, &bv);
        let mut pooled = vec![0f32; b * d];
        for i in 0..b {
            let r = i * n * d..(i + 1) * n * d;
            pooled[i * d..(i + 1) * d].copy_from_slice(&attend(&q, 1, &k[r.clone()], &v[r], n, d, self.s.heads));
        }
        let mut h = linear(&pooled, b, &self.head_out.0, &self.head_out.1);
        let mut t = h.clone();
        layernorm(&mut t, d, &self.head_ln.0, &self.head_ln.1);
        let mut m = linear(&t, b, &self.head_fc1.0, &self.head_fc1.1);
        gelu(&mut m);
        let f = linear(&m, b, &self.head_fc2.0, &self.head_fc2.1);
        for (hv, fv) in h.iter_mut().zip(&f) {
            *hv += fv;
        }
        h.chunks(d)
            .map(|r| {
                let mut v = r.to_vec();
                normalize(&mut v);
                v
            })
            .collect()
    }
}

// ───────────────────────────────────────────── Text tower

/// Text length the model was trained with.
pub const TEXT_LEN: usize = 64;

pub struct TextTower {
    pub tokenizer: SentencePiece,
    model: ModelFile,
    s: Size,
    pos: Tensor,
    layers: Vec<Layer>,
    fin: (Tensor, Tensor),
    head: (Tensor, Tensor),
    eos: u32,
    pad: u32,
    /// word table rows read so far (pad and eos repeat in every query)
    rows: Mutex<HashMap<u32, Vec<f32>>>,
}

const WORDS: &str = "text_model.embeddings.token_embedding.weight";

impl TextTower {
    /// From the model folder (`model.safetensors`, `tokenizer.model`); only the text tower is read,
    /// the word table stays in the file.
    pub fn load(dir: &Path) -> Result<TextTower, String> {
        let tokenizer = SentencePiece::load(&dir.join("tokenizer.model"))?;
        let m = ModelFile::open(&dir.join("model.safetensors"))?;
        let s = size_of(&m, "text_model")?;
        let d = s.d;
        let words = m.shape(WORDS).ok_or("Modell passt nicht: Worttabelle fehlt")?;
        if words.len() != 2 || words[1] != d || words[0] < tokenizer.size() {
            return Err(format!("Modell passt nicht: Worttabelle {words:?} zu {} Stücken", tokenizer.size()));
        }
        let proj = m.shape("text_model.head.weight").ok_or("Modell passt nicht: Textkopf fehlt")?[0];
        Ok(TextTower {
            eos: tokenizer.control_id("<eos>").ok_or("Tokenizer ohne <eos>")?,
            pad: tokenizer.control_id("<pad>").ok_or("Tokenizer ohne <pad>")?,
            tokenizer,
            pos: m.tensor("text_model.embeddings.position_embedding.weight", &[TEXT_LEN, d])?,
            layers: load_layers(&m, "text_model", s)?,
            fin: norm(&m, "text_model.final_layer_norm", d)?,
            head: pair(&m, "text_model.head", &[proj, d], proj)?,
            s,
            model: m,
            rows: Mutex::new(HashMap::new()),
        })
    }

    pub fn dim(&self) -> usize {
        self.head.0.form[0]
    }

    /// Text → the 64 token ids the model sees: lower case, pieces, `<eos>`, padding.
    pub fn ids(&self, text: &str) -> Vec<u32> {
        let mut ids = self.tokenizer.encode(&text.to_lowercase());
        ids.push(self.eos);
        ids.truncate(TEXT_LEN);
        ids.resize(TEXT_LEN, self.pad);
        ids
    }

    fn word(&self, id: u32) -> Result<Vec<f32>, String> {
        if let Some(r) = self.rows.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
            return Ok(r.clone());
        }
        let r = self.model.row(WORDS, id as usize)?;
        let mut cache = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 50_000 {
            cache.clear();
        }
        cache.insert(id, r.clone());
        Ok(r)
    }

    /// Sentences → normalized vectors (computed together).
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        let b = texts.len();
        if b == 0 {
            return Ok(Vec::new());
        }
        let (d, n) = (self.s.d, TEXT_LEN);
        let mut x = vec![0f32; b * n * d];
        for (i, t) in texts.iter().enumerate() {
            for (j, id) in self.ids(t).into_iter().enumerate() {
                let w = self.word(id)?;
                let row = &mut x[(i * n + j) * d..][..d];
                for ((v, a), p) in row.iter_mut().zip(&w).zip(&self.pos.data[j * d..][..d]) {
                    *v = a + p;
                }
            }
        }
        run_layers(&mut x, b, n, self.s, &self.layers);
        // the last position of each sentence carries the sentence
        let mut last: Vec<f32> = (0..b).flat_map(|i| x[(i * n + n - 1) * d..][..d].to_vec()).collect();
        layernorm(&mut last, d, &self.fin.0, &self.fin.1);
        let y = linear(&last, b, &self.head.0, &self.head.1);
        Ok(y.chunks(self.dim())
            .map(|r| {
                let mut v = r.to_vec();
                normalize(&mut v);
                v
            })
            .collect())
    }

    pub fn embed_one(&self, text: &str) -> Result<Vec<f32>, String> {
        Ok(self.embed(&[text])?.remove(0))
    }
}

/// Model folder of a SigLIP module (installed under modules/<id>), or for tests an existing
/// Hugging Face cache (`GRAINEO_HF_CACHE`, `models--google--<name>`).
pub fn folder(module_id: &str, repo: &str) -> Option<PathBuf> {
    let own = crate::semantic::modules_folder().join(module_id);
    if own.join("model.safetensors").exists() && own.join("tokenizer.model").exists() && own.join("module.json").exists() {
        return Some(own);
    }
    let c = std::env::var_os("GRAINEO_HF_CACHE").map(PathBuf::from)?;
    let snaps = c.join("hub").join(format!("models--{}", repo.replace('/', "--"))).join("snapshots");
    std::fs::read_dir(snaps).ok()?.flatten().map(|e| e.path()).find(|p| p.join("model.safetensors").exists() && p.join("tokenizer.model").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemm_with_strides_and_transposes() {
        // A 2×3, B 3×2 → C 2×2
        let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = [1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let mut c = [0.0; 4];
        gemm(false, false, 2, 2, 3, &a, 3, &b, 2, &mut c, 2);
        assert_eq!(c, [4.0, 5.0, 10.0, 11.0]);
        // sub-matrix via leading dimension: columns 1..3 of A (2×2) times identity
        let id = [1.0, 0.0, 0.0, 1.0];
        let mut c = [0.0; 4];
        gemm(false, true, 2, 2, 2, &a[1..], 3, &id, 2, &mut c, 2);
        assert_eq!(c, [2.0, 3.0, 5.0, 6.0]);
        assert!(std::panic::catch_unwind(|| {
            let mut c = [0.0; 4];
            gemm(false, false, 2, 2, 3, &a[..5], 3, &b, 2, &mut c, 2);
        })
        .is_err());
    }

    #[test]
    fn attention_with_one_key_returns_its_value() {
        let q = [1.0, 2.0, 3.0, 4.0];
        let k = [0.5, 0.5, 0.5, 0.5];
        let v = [9.0, 8.0, 7.0, 6.0];
        assert_eq!(attend(&q, 1, &k, &v, 1, 4, 2), vec![9.0, 8.0, 7.0, 6.0]);
    }

    #[test]
    fn broken_model_files_are_errors() {
        let p = std::env::temp_dir().join(format!("graineo-siglip-audit-{}.safetensors", std::process::id()));
        for bytes in [vec![], u64::MAX.to_le_bytes().to_vec(), 100u64.to_le_bytes().to_vec()] {
            std::fs::write(&p, bytes).unwrap();
            assert!(ModelFile::open(&p).is_err());
        }
        let header = r#"{"w":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}"#;
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend(header.as_bytes());
        bytes.extend(1.5f32.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        assert!(ModelFile::open(&p).is_err(), "data shorter than the shape");
        bytes.extend(2.5f32.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();
        let m = ModelFile::open(&p).unwrap();
        assert_eq!(m.tensor("w", &[2]).unwrap().data, vec![1.5, 2.5]);
        assert!(m.tensor("w", &[3]).is_err());
        assert!(size_of(&m, "vision_model").is_err());
        std::fs::remove_file(p).unwrap();
    }
}
