//! Semantic search without Python: a small reimplementation of both encoders in Rust,
//! compute kernel via Apple Accelerate (`cblas_sgemm`, uses the AMX units of the M1).
//!
//! - **Text:** `sentence-transformers/clip-ViT-B-32-multilingual-v1` (Apache-2.0) = DistilBERT
//!   (multilingual, cased, 6 layers) → mean over all tokens → dense 768→512 (without bias)
//!   → normalized. Lives in the same space as the CLIP image vectors.
//! - **Image:** `clip-ViT-B-32` (MIT, OpenAI) vision part: 224×224, patches 32×32, 12 layers
//!   (pre-LN, quick_gelu), projection 768→512, normalized.
//! - Tokenizer: BERT WordPiece like `tokenizers` (BertNormalizer without lower-casing,
//!   BertPreTokenizer, `##` continuations), vocabulary from `vocab.txt`.
//!
//! Why not candle + tokenizers: together they cost well over 1 GB of intermediate build
//! files and a lot of time; both networks are small and fixed, the reimplementation is ~400 lines
//! and numerically checked against the Python reference (cosine > 0.999).
//! Model files are Safetensors (f32), read without an extra library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn cblas_sgemm(order: i32, ta: i32, tb: i32, m: i32, n: i32, k: i32, alpha: f32, a: *const f32, lda: i32, b: *const f32, ldb: i32, beta: f32, c: *mut f32, ldc: i32);
}
extern "C" {
    pub(crate) fn erff(x: f32) -> f32;
}

const ROW_MAJOR: i32 = 101;
const NO_TRANS: i32 = 111;
const TRANS: i32 = 112;

/// C[m×n] = A[m×k] · B (B stored as [n×k], i.e. Bᵀ) – like `nn.Linear`.
pub(crate) fn matmul_bt(a: &[f32], m: usize, k: usize, b: &[f32], n: usize) -> Vec<f32> {
    // This limit must never be exceeded unchecked, even with damaged model
    // weights: BLAS does not know the lengths of the Rust slices.
    assert!(m <= i32::MAX as usize && n <= i32::MAX as usize && k <= i32::MAX as usize);
    assert!(k > 0 && n > 0);
    assert_eq!(Some(a.len()), m.checked_mul(k), "matrix A does not match the shape");
    assert_eq!(Some(b.len()), n.checked_mul(k), "matrix B does not match the shape");
    let length = m.checked_mul(n).expect("matrix result is too large");
    let mut c = vec![0f32; length];
    #[cfg(target_os = "macos")]
    unsafe {
        cblas_sgemm(ROW_MAJOR, NO_TRANS, TRANS, m as i32, n as i32, k as i32, 1.0, a.as_ptr(), k as i32, b.as_ptr(), k as i32, 0.0, c.as_mut_ptr(), n as i32);
    }
    #[cfg(not(target_os = "macos"))]
    for i in 0..m {
        for j in 0..n {
            c[i * n + j] = (0..k).map(|t| a[i * k + t] * b[j * k + t]).sum();
        }
    }
    c
}

fn linear(x: &[f32], m: usize, w: &Tensor, b: Option<&Tensor>) -> Vec<f32> {
    let (out, inn) = (w.form[0], w.form[1]);
    let mut y = matmul_bt(x, m, inn, &w.data, out);
    if let Some(b) = b {
        for r in y.chunks_mut(out) {
            for (v, bb) in r.iter_mut().zip(&b.data) {
                *v += bb;
            }
        }
    }
    y
}

fn layernorm(x: &mut [f32], d: usize, g: &Tensor, b: &Tensor, eps: f32) {
    for r in x.chunks_mut(d) {
        let mean = r.iter().sum::<f32>() / d as f32;
        let var = r.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / d as f32;
        let inv = 1.0 / (var + eps).sqrt();
        for (i, v) in r.iter_mut().enumerate() {
            *v = (*v - mean) * inv * g.data[i] + b.data[i];
        }
    }
}

/// Multi-head self-attention without mask (one sequence of length `n`).
fn attention(x: &[f32], n: usize, d: usize, heads: usize, q: (&Tensor, &Tensor), k: (&Tensor, &Tensor), v: (&Tensor, &Tensor), o: (&Tensor, &Tensor)) -> Vec<f32> {
    let qq = linear(x, n, q.0, Some(q.1));
    let kk = linear(x, n, k.0, Some(k.1));
    let vv = linear(x, n, v.0, Some(v.1));
    let hd = d / heads;
    let scale = 1.0 / (hd as f32).sqrt();
    let mut ctx = vec![0f32; n * d];
    for h in 0..heads {
        for i in 0..n {
            let qi = &qq[i * d + h * hd..i * d + (h + 1) * hd];
            let mut s: Vec<f32> = (0..n).map(|j| qi.iter().zip(&kk[j * d + h * hd..j * d + (h + 1) * hd]).map(|(a, b)| a * b).sum::<f32>() * scale).collect();
            let m = s.iter().cloned().fold(f32::MIN, f32::max);
            let mut sum = 0.0;
            for v in &mut s {
                *v = (*v - m).exp();
                sum += *v;
            }
            for j in 0..n {
                let w = s[j] / sum;
                let vj = &vv[j * d + h * hd..j * d + (h + 1) * hd];
                for t in 0..hd {
                    ctx[i * d + h * hd + t] += w * vj[t];
                }
            }
        }
    }
    linear(&ctx, n, o.0, Some(o.1))
}

pub struct Tensor {
    pub form: Vec<usize>,
    pub data: Vec<f32>,
}

/// Read Safetensors (F32 only); `filter`: only names for which it returns `true`.
pub fn safetensors(p: &Path, filter: &dyn Fn(&str) -> bool) -> Result<HashMap<String, Tensor>, String> {
    let raw_refs = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let n = usize::try_from(u64::from_le_bytes(raw_refs.get(..8).ok_or("Modellkopf zu kurz")?.try_into().map_err(|_| "Modellkopf zu kurz")?)).map_err(|_| "Modellkopf zu groß")?;
    // Our two encoders only need small JSON headers; do not pass arbitrarily
    // large/destructively nested headers to the JSON parser.
    if n > 16 * 1024 * 1024 { return Err("Modellkopf zu groß".into()); }
    let base = 8usize.checked_add(n).ok_or("Modellkopf zu groß")?;
    let header: serde_json::Value = serde_json::from_slice(raw_refs.get(8..base).ok_or("Modellkopf abgeschnitten")?).map_err(|e| e.to_string())?;
    let payload = raw_refs.get(base..).ok_or("Modelldaten fehlen")?;
    let whole = |v: &serde_json::Value| -> Result<usize, String> {
        usize::try_from(v.as_u64().ok_or("Ungültige Modelldimension oder Datenposition")?).map_err(|_| "Modelldimension zu groß".into())
    };
    let mut out = HashMap::new();
    for (name, v) in header.as_object().ok_or("Modellkopf ist kein Objekt")? {
        if name == "__metadata__" || !filter(name) || v["dtype"] != "F32" {
            continue;
        }
        let form: Vec<usize> = v["shape"].as_array().ok_or("Modellform fehlt")?.iter().map(whole).collect::<Result<_, _>>()?;
        let count = form.iter().try_fold(1usize, |n, &d| n.checked_mul(d)).ok_or("Modellform zu groß")?;
        let bytes = count.checked_mul(4).ok_or("Modelldaten zu groß")?;
        let o = v["data_offsets"].as_array().filter(|o| o.len() == 2).ok_or("Modell benötigt zwei Datenpositionen")?;
        let (a, e) = (whole(&o[0])?, whole(&o[1])?);
        let slice = payload.get(a..e).filter(|b| b.len() == bytes).ok_or("Modelldaten passen nicht zur Form oder Dateigröße")?;
        let data: Vec<f32> = slice.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        out.insert(name.clone(), Tensor { form, data });
    }
    Ok(out)
}

/// Architecture check on load: every tensor the computation needs must be present and have the
/// expected shape. A wrong or damaged model thus produces an error message instead of a
/// crash in the middle of the computation. `min_rows`: `(name, rows, columns)` – more rows are allowed.
fn check_shapes(t: &HashMap<String, Tensor>, exact: &[(String, Vec<usize>)], min_rows: &[(&str, usize, usize)]) -> Result<(), String> {
    for (name, form) in exact {
        let x = t.get(name).ok_or_else(|| format!("Modell passt nicht: Tensor „{name}“ fehlt"))?;
        if &x.form != form {
            return Err(format!("Modell passt nicht: „{name}“ hat die Form {:?}, erwartet {form:?}", x.form));
        }
    }
    for &(name, rows, columns) in min_rows {
        let x = t.get(name).ok_or_else(|| format!("Modell passt nicht: Tensor „{name}“ fehlt"))?;
        if x.form.len() != 2 || x.form[1] != columns || x.form[0] < rows {
            return Err(format!("Modell passt nicht: „{name}“ hat die Form {:?}, erwartet mindestens [{rows}, {columns}]", x.form));
        }
    }
    Ok(())
}

fn tensor_form(name: impl Into<String>, form: &[usize]) -> (String, Vec<usize>) {
    (name.into(), form.to_vec())
}

/// Expected tensors of the text encoder (DistilBERT, 6 layers, width 768) including the projection to 512.
fn text_shapes(dense: &Tensor) -> Vec<(String, Vec<usize>)> {
    let mut v = vec![tensor_form("embeddings.LayerNorm.weight", &[768]), tensor_form("embeddings.LayerNorm.bias", &[768])];
    for l in 0..6 {
        let p = |s: &str| format!("transformer.layer.{l}.{s}");
        for a in ["q_lin", "k_lin", "v_lin", "out_lin"] {
            v.push(tensor_form(p(&format!("attention.{a}.weight")), &[768, 768]));
            v.push(tensor_form(p(&format!("attention.{a}.bias")), &[768]));
        }
        for n in ["sa_layer_norm", "output_layer_norm"] {
            v.push(tensor_form(p(&format!("{n}.weight")), &[768]));
            v.push(tensor_form(p(&format!("{n}.bias")), &[768]));
        }
        v.push(tensor_form(p("ffn.lin1.weight"), &[3072, 768]));
        v.push(tensor_form(p("ffn.lin1.bias"), &[3072]));
        v.push(tensor_form(p("ffn.lin2.weight"), &[768, 3072]));
        v.push(tensor_form(p("ffn.lin2.bias"), &[768]));
    }
    let _ = dense;
    v
}

/// Expected tensors of the image encoder (CLIP ViT-B/32, 12 layers, width 768, 50 positions).
fn image_shapes() -> Vec<(String, Vec<usize>)> {
    let mut v = vec![
        tensor_form("vision_model.embeddings.patch_embedding.weight", &[768, 3, 32, 32]),
        tensor_form("vision_model.embeddings.class_embedding", &[768]),
        tensor_form("vision_model.embeddings.position_embedding.weight", &[50, 768]),
        tensor_form("vision_model.pre_layrnorm.weight", &[768]),
        tensor_form("vision_model.pre_layrnorm.bias", &[768]),
        tensor_form("vision_model.post_layernorm.weight", &[768]),
        tensor_form("vision_model.post_layernorm.bias", &[768]),
        tensor_form("visual_projection.weight", &[512, 768]),
    ];
    for l in 0..12 {
        let p = |s: &str| format!("vision_model.encoder.layers.{l}.{s}");
        for a in ["q_proj", "k_proj", "v_proj", "out_proj"] {
            v.push(tensor_form(p(&format!("self_attn.{a}.weight")), &[768, 768]));
            v.push(tensor_form(p(&format!("self_attn.{a}.bias")), &[768]));
        }
        for n in ["layer_norm1", "layer_norm2"] {
            v.push(tensor_form(p(&format!("{n}.weight")), &[768]));
            v.push(tensor_form(p(&format!("{n}.bias")), &[768]));
        }
        v.push(tensor_form(p("mlp.fc1.weight"), &[3072, 768]));
        v.push(tensor_form(p("mlp.fc1.bias"), &[3072]));
        v.push(tensor_form(p("mlp.fc2.weight"), &[768, 3072]));
        v.push(tensor_form(p("mlp.fc2.bias"), &[768]));
    }
    v
}

pub(crate) fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    for x in v {
        *x /= n;
    }
}

// ───────────────────────────────────────────── Tokenizer (BERT WordPiece)

pub struct WordPiece {
    vocab: HashMap<String, u32>,
    unk: u32,
    cls: u32,
    sep: u32,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x2A700..=0x2B73F | 0x2B740..=0x2B81F | 0x2B820..=0x2CEAF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F)
}

fn is_punctuation(c: char) -> bool {
    let u = c as u32;
    (33..=47).contains(&u) || (58..=64).contains(&u) || (91..=96).contains(&u) || (123..=126).contains(&u) || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_control() && u > 127 && !is_symbol(c))
}

/// Rough Unicode symbol detection (BERT splits only punctuation, not symbols like €, ©, emoji).
fn is_symbol(c: char) -> bool {
    matches!(c as u32, 0x20A0..=0x20CF | 0x2100..=0x214F | 0x2190..=0x23FF | 0x2500..=0x27BF | 0x1F000..=0x1FAFF | 0xA2..=0xA9 | 0xAC..=0xB1 | 0xB4 | 0xB8 | 0xD7 | 0xF7)
}

impl WordPiece {
    pub fn load(vocab_txt: &Path) -> Result<WordPiece, String> {
        let t = std::fs::read_to_string(vocab_txt).map_err(|e| format!("{}: {e}", vocab_txt.display()))?;
        let vocab: HashMap<String, u32> = t.lines().enumerate().map(|(i, l)| (l.to_string(), i as u32)).collect();
        let id = |s: &str| vocab.get(s).copied().ok_or(format!("{s} fehlt im Vokabular"));
        Ok(WordPiece { unk: id("[UNK]")?, cls: id("[CLS]")?, sep: id("[SEP]")?, vocab })
    }

    /// Number of entries the model must have at least as rows of the word embedding.
    pub fn size(&self) -> usize {
        self.vocab.values().max().map_or(0, |m| *m as usize + 1)
    }

    /// Text → token IDs incl. [CLS] … [SEP], at most `max` tokens.
    pub fn encode(&self, text: &str, max: usize) -> Vec<u32> {
        // BertNormalizer: remove control characters, normalize whitespace, isolate CJK (no lower-casing)
        let mut clean = String::with_capacity(text.len());
        for c in text.chars() {
            let u = c as u32;
            if u == 0 || u == 0xFFFD || (c.is_control() && !matches!(c, '\t' | '\n' | '\r')) {
                continue;
            }
            if c.is_whitespace() {
                clean.push(' ');
            } else if is_cjk(c) {
                clean.push(' ');
                clean.push(c);
                clean.push(' ');
            } else {
                clean.push(c);
            }
        }
        // BertPreTokenizer: split at whitespace and punctuation
        let mut words: Vec<String> = Vec::new();
        for w in clean.split(' ').filter(|w| !w.is_empty()) {
            let mut cur = String::new();
            for c in w.chars() {
                if is_punctuation(c) {
                    if !cur.is_empty() {
                        words.push(std::mem::take(&mut cur));
                    }
                    words.push(c.to_string());
                } else {
                    cur.push(c);
                }
            }
            if !cur.is_empty() {
                words.push(cur);
            }
        }
        let mut ids = vec![self.cls];
        for w in words {
            let chars_of: Vec<char> = w.chars().collect();
            if chars_of.len() > 100 {
                ids.push(self.unk);
                continue;
            }
            let mut start = 0;
            let mut parts = Vec::new();
            let mut ok = true;
            while start < chars_of.len() {
                let mut end = chars_of.len();
                let mut found_id = None;
                while start < end {
                    let s: String = chars_of[start..end].iter().collect();
                    let s = if start > 0 { format!("##{s}") } else { s };
                    if let Some(&id) = self.vocab.get(&s) {
                        found_id = Some(id);
                        break;
                    }
                    end -= 1;
                }
                match found_id {
                    Some(id) => {
                        parts.push(id);
                        start = end;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                ids.extend(parts);
            } else {
                ids.push(self.unk);
            }
        }
        ids.truncate(max - 1);
        ids.push(self.sep);
        ids
    }
}

// ───────────────────────────────────────────── Text encoder

pub struct TextEncoder {
    pub tokenizer: WordPiece,
    t: HashMap<String, Tensor>,
    dense: Tensor,
}

impl TextEncoder {
    /// From a model folder (as on Hugging Face: `model.safetensors`, `vocab.txt`, `2_Dense/model.safetensors`).
    pub fn load(dir: &Path) -> Result<TextEncoder, String> {
        let tokenizer = WordPiece::load(&dir.join("vocab.txt"))?;
        let t = safetensors(&dir.join("model.safetensors"), &|_| true)?;
        let mut d = safetensors(&dir.join("2_Dense/model.safetensors"), &|_| true)?;
        let dense = d.remove("linear.weight").ok_or("2_Dense/linear.weight fehlt")?;
        if dense.form != [512, 768] {
            return Err(format!("Modell passt nicht: „linear.weight“ hat die Form {:?}, erwartet [512, 768]", dense.form));
        }
        check_shapes(&t, &text_shapes(&dense), &[("embeddings.word_embeddings.weight", tokenizer.size(), 768), ("embeddings.position_embeddings.weight", 128, 768)])?;
        Ok(TextEncoder { tokenizer, t, dense })
    }

    fn g(&self, n: &str) -> &Tensor {
        &self.t[n]
    }

    /// Sentence → normalized 512-dim vector (CLIP space).
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let ids = self.tokenizer.encode(text, 128);
        let n = ids.len();
        let d = 768;
        let we = self.g("embeddings.word_embeddings.weight");
        let pe = self.g("embeddings.position_embeddings.weight");
        let mut x = vec![0f32; n * d];
        for (i, id) in ids.iter().enumerate() {
            for t in 0..d {
                x[i * d + t] = we.data[*id as usize * d + t] + pe.data[i * d + t];
            }
        }
        layernorm(&mut x, d, self.g("embeddings.LayerNorm.weight"), self.g("embeddings.LayerNorm.bias"), 1e-12);
        for l in 0..6 {
            let p = |s: &str| format!("transformer.layer.{l}.{s}");
            let w = |s: &str| (self.g(&p(&format!("{s}.weight"))), self.g(&p(&format!("{s}.bias"))));
            let a = attention(&x, n, d, 12, w("attention.q_lin"), w("attention.k_lin"), w("attention.v_lin"), w("attention.out_lin"));
            for (xv, av) in x.iter_mut().zip(&a) {
                *xv += av;
            }
            layernorm(&mut x, d, self.g(&p("sa_layer_norm.weight")), self.g(&p("sa_layer_norm.bias")), 1e-12);
            let (w1, b1) = w("ffn.lin1");
            let (w2, b2) = w("ffn.lin2");
            let mut h = linear(&x, n, w1, Some(b1));
            for v in &mut h {
                *v = 0.5 * *v * (1.0 + unsafe { erff(*v / std::f32::consts::SQRT_2) });
            }
            let f = linear(&h, n, w2, Some(b2));
            for (xv, fv) in x.iter_mut().zip(&f) {
                *xv += fv;
            }
            layernorm(&mut x, d, self.g(&p("output_layer_norm.weight")), self.g(&p("output_layer_norm.bias")), 1e-12);
        }
        let mut average = vec![0f32; d];
        for r in x.chunks(d) {
            for (m, v) in average.iter_mut().zip(r) {
                *m += v / n as f32;
            }
        }
        let mut y = linear(&average, 1, &self.dense, None);
        normalize(&mut y);
        y
    }
}

// ───────────────────────────────────────────── Image encoder

pub struct ImageEncoder {
    t: HashMap<String, Tensor>,
}

const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const STD_DEV: [f32; 3] = [0.268_629_54, 0.261_302_58, 0.275_777_1];

impl ImageEncoder {
    /// From `0_CLIPModel/model.safetensors` (only the image part is loaded).
    pub fn load(dir: &Path) -> Result<ImageEncoder, String> {
        let t = safetensors(&dir.join("model.safetensors"), &|n| n.starts_with("vision_model.") || n == "visual_projection.weight")?;
        check_shapes(&t, &image_shapes(), &[])?;
        Ok(ImageEncoder { t })
    }

    fn g(&self, n: &str) -> &Tensor {
        &self.t[n]
    }

    /// Preprocessing like CLIP: shorter side to 224 (bicubic), center 224×224, normalized.
    pub fn prepare(img: &image::RgbImage) -> Vec<f32> {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let k = 224.0 / w.min(h);
        let (nw, nh) = (((w * k).round() as u32).max(224), ((h * k).round() as u32).max(224));
        let r = image::imageops::resize(img, nw, nh, image::imageops::FilterType::CatmullRom);
        let (x0, y0) = ((nw - 224) / 2, (nh - 224) / 2);
        let mut out = vec![0f32; 3 * 224 * 224];
        for y in 0..224 {
            for x in 0..224 {
                let p = r.get_pixel(x0 + x, y0 + y);
                for c in 0..3 {
                    out[c * 224 * 224 + (y * 224 + x) as usize] = (p[c] as f32 / 255.0 - MEAN[c]) / STD_DEV[c];
                }
            }
        }
        out
    }

    /// Image → normalized 512-dim vector.
    pub fn embed(&self, img: &image::RgbImage) -> Vec<f32> {
        let px = Self::prepare(img);
        let d = 768;
        let n = 50;
        // patches 32×32 as matrix [49 × 3072] · weight [768 × 3072]ᵀ
        let mut patches = vec![0f32; 49 * 3072];
        for py in 0..7 {
            for pxx in 0..7 {
                let i = py * 7 + pxx;
                for c in 0..3 {
                    for y in 0..32 {
                        for x in 0..32 {
                            patches[i * 3072 + c * 1024 + y * 32 + x] = px[c * 224 * 224 + (py * 32 + y) * 224 + pxx * 32 + x];
                        }
                    }
                }
            }
        }
        let pw = self.g("vision_model.embeddings.patch_embedding.weight");
        let emb = matmul_bt(&patches, 49, 3072, &pw.data, d);
        let mut x = vec![0f32; n * d];
        x[..d].copy_from_slice(&self.g("vision_model.embeddings.class_embedding").data);
        x[d..].copy_from_slice(&emb);
        let pos = self.g("vision_model.embeddings.position_embedding.weight");
        for (v, p) in x.iter_mut().zip(&pos.data) {
            *v += p;
        }
        layernorm(&mut x, d, self.g("vision_model.pre_layrnorm.weight"), self.g("vision_model.pre_layrnorm.bias"), 1e-5);
        for l in 0..12 {
            let p = |s: &str| format!("vision_model.encoder.layers.{l}.{s}");
            let w = |s: &str| (self.g(&p(&format!("{s}.weight"))), self.g(&p(&format!("{s}.bias"))));
            let mut h = x.clone();
            layernorm(&mut h, d, self.g(&p("layer_norm1.weight")), self.g(&p("layer_norm1.bias")), 1e-5);
            let a = attention(&h, n, d, 12, w("self_attn.q_proj"), w("self_attn.k_proj"), w("self_attn.v_proj"), w("self_attn.out_proj"));
            for (xv, av) in x.iter_mut().zip(&a) {
                *xv += av;
            }
            let mut h = x.clone();
            layernorm(&mut h, d, self.g(&p("layer_norm2.weight")), self.g(&p("layer_norm2.bias")), 1e-5);
            let (w1, b1) = w("mlp.fc1");
            let (w2, b2) = w("mlp.fc2");
            let mut m = linear(&h, n, w1, Some(b1));
            for v in &mut m {
                *v *= 1.0 / (1.0 + (-1.702 * *v).exp()); // quick_gelu
            }
            let f = linear(&m, n, w2, Some(b2));
            for (xv, fv) in x.iter_mut().zip(&f) {
                *xv += fv;
            }
        }
        let mut cls = x[..d].to_vec();
        layernorm(&mut cls, d, self.g("vision_model.post_layernorm.weight"), self.g("vision_model.post_layernorm.bias"), 1e-5);
        let mut y = linear(&cls, 1, self.g("visual_projection.weight"), None);
        normalize(&mut y);
        y
    }
}

// ───────────────────────────────────────────── Model locations

/// Folder of the modules: `~/Library/Application Support/<APP>/modules/`.
pub fn modules_folder() -> PathBuf {
    crate::data_dir().join("modules")
}

pub const TEXT_MODEL: &str = "sentence-transformers/clip-ViT-B-32-multilingual-v1";
pub const IMAGE_MODEL: &str = "sentence-transformers/clip-ViT-B-32";

/// Model folder of the text encoder: first the installed module, else (for testing only) an
/// existing Hugging Face cache (`GRAINEO_HF_CACHE`).
pub fn text_folder() -> Option<PathBuf> {
    let own = modules_folder().join("text-encoder");
    if own.join("model.safetensors").exists() && own.join("2_Dense/model.safetensors").exists() {
        return Some(own);
    }
    hf_snapshot("models--sentence-transformers--clip-ViT-B-32-multilingual-v1", "model.safetensors")
}

pub fn image_folder() -> Option<PathBuf> {
    let own = modules_folder().join("image-encoder");
    if own.join("model.safetensors").exists() {
        return Some(own);
    }
    hf_snapshot("models--sentence-transformers--clip-ViT-B-32", "0_CLIPModel/model.safetensors").map(|p| p.join("0_CLIPModel"))
}

// ───────────────────────────────────────────── Choice of the text model

/// Text model for search queries (and AI description vectors). Both live in the space of the
/// CLIP ViT-B/32 image vectors, so switching never requires re-reading the photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextModel {
    /// `clip-ViT-B-32-multilingual-v1` (DistilBERT, small, fast)
    Standard,
    /// M-CLIP `XLM-Roberta-Large-Vit-B-32` (larger, clearly better in German, French, Spanish, Japanese)
    Multilingual,
}

impl TextModel {
    /// Value in the settings (`textModel`).
    pub fn key(self) -> &'static str {
        match self {
            TextModel::Standard => "standard",
            TextModel::Multilingual => "mclip",
        }
    }

    /// Model name stored with description vectors.
    pub fn model_name(self) -> &'static str {
        match self {
            TextModel::Standard => "clip-ViT-B-32-multilingual-v1",
            TextModel::Multilingual => MCLIP_MODEL,
        }
    }

    /// Model folder, if installed.
    pub fn folder(self) -> Option<PathBuf> {
        match self {
            TextModel::Standard => text_folder(),
            TextModel::Multilingual => mclip_folder(),
        }
    }
}

pub const MCLIP_MODEL: &str = "M-CLIP/XLM-Roberta-Large-Vit-B-32";

/// Folder of the M-CLIP module (complete: tokenizer and F16 model).
pub fn mclip_folder() -> Option<PathBuf> {
    let own = modules_folder().join(crate::module::MCLIP.id);
    (own.join("tokenizer.json").exists() && own.join(crate::xlmr::MODEL_F16).exists() && own.join("module.json").exists()).then_some(own)
}

/// Chosen text model: the setting `textModel`; without a setting the large model as soon as it is
/// installed (recommended), otherwise the standard model. A chosen but missing model falls back to
/// the standard model.
pub fn selected_text_model() -> TextModel {
    let installed = mclip_folder().is_some();
    match crate::settings::value("textModel").as_str() {
        Some("standard") => TextModel::Standard,
        Some("mclip") if installed => TextModel::Multilingual,
        None if installed => TextModel::Multilingual,
        _ => TextModel::Standard,
    }
}

static TEXT_MODEL_ROUND: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Save the choice; loaded encoders notice it via `text_model_round`.
pub fn set_text_model(m: TextModel) {
    crate::settings::set("textModel", serde_json::json!(m.key()));
    text_model_changed();
}

/// Counter that changes whenever the choice (or the installation) of a text model changes.
pub fn text_model_round() -> u64 {
    TEXT_MODEL_ROUND.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn text_model_changed() {
    TEXT_MODEL_ROUND.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Model in which the AI descriptions are embedded: the standard model whenever it is installed,
/// whatever model the queries use. Measured (XTD10, German query against English texts, R@1):
/// M-CLIP query → standard texts 97.8 %, M-CLIP → M-CLIP 98.3 %, standard → standard 78.1 %. Both
/// models imitate the same CLIP text space, so recomputing tens of thousands of descriptions with
/// the large model (hours of background work) would gain almost nothing. Only without the standard
/// model the large model embeds them itself.
pub fn description_model() -> Option<TextModel> {
    if text_folder().is_some() {
        Some(TextModel::Standard)
    } else if mclip_folder().is_some() {
        Some(TextModel::Multilingual)
    } else {
        None
    }
}

/// Encoder for search queries in the chosen text model.
pub enum QueryEncoder {
    Standard(TextEncoder),
    Multilingual(crate::xlmr::MclipEncoder),
}

impl QueryEncoder {
    pub fn load(m: TextModel) -> Result<QueryEncoder, String> {
        let dir = m.folder().ok_or("Text-Modell fehlt")?;
        Ok(match m {
            TextModel::Standard => QueryEncoder::Standard(TextEncoder::load(&dir)?),
            TextModel::Multilingual => QueryEncoder::Multilingual(crate::xlmr::MclipEncoder::load(&dir)?),
        })
    }

    pub fn model(&self) -> TextModel {
        match self {
            QueryEncoder::Standard(_) => TextModel::Standard,
            QueryEncoder::Multilingual(_) => TextModel::Multilingual,
        }
    }

    /// Sentence → normalized 512-dim vector (CLIP space).
    pub fn embed(&self, text: &str) -> Vec<f32> {
        match self {
            QueryEncoder::Standard(e) => e.embed(text),
            QueryEncoder::Multilingual(e) => e.embed(text),
        }
    }

    /// Several sentences (the large model computes them together, which is much cheaper per sentence).
    pub fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        match self {
            QueryEncoder::Standard(e) => texts.iter().map(|t| e.embed(t)).collect(),
            QueryEncoder::Multilingual(e) => e.embed_batch(texts),
        }
    }
}

/// Text of an AI description as it is embedded (keywords
/// sorted, scene, reasoning).
pub fn description_text(keywords_json: &str, scene: &str, reason: &str) -> String {
    let mut k: Vec<String> = serde_json::from_str(keywords_json).unwrap_or_default();
    k.sort();
    let parts: Vec<String> = [k.join(", "), scene.to_string(), reason.to_string()].into_iter().filter(|t| !t.trim().is_empty()).collect();
    parts.join(". ")
}

fn hf_snapshot(name: &str, file: &str) -> Option<PathBuf> {
    let c = std::env::var_os("GRAINEO_HF_CACHE").map(PathBuf::from)?;
    let snaps = c.join("hub").join(name).join("snapshots");
    std::fs::read_dir(snaps).ok()?.flatten().map(|e| e.path()).find(|p| p.join(file).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matmul() {
        // [2×3]·[2×3]ᵀ
        let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = [1.0, 0.0, 1.0, 0.0, 1.0, 0.0];
        assert_eq!(matmul_bt(&a, 2, 3, &b, 2), vec![4.0, 2.0, 10.0, 5.0]);
    }

    #[test]
    fn broken_models_are_rejected_before_decoding() {
        let p = std::env::temp_dir().join(format!("graineo-model-audit-{}.safetensors", std::process::id()));
        for bytes in [vec![], u64::MAX.to_le_bytes().to_vec(), 100u64.to_le_bytes().to_vec()] {
            std::fs::write(&p, bytes).unwrap();
            assert!(safetensors(&p, &|_| true).is_err());
        }
        for (shape, offsets) in [
            ("[2]", "[]"), ("[2]", "[0]"), ("[2]", "[4,0]"),
            ("[2]", "[0,100]"), ("[2]", "[0,4]"), ("[-1]", "[0,4]"),
            ("[18446744073709551615,2]", "[0,4]"),
        ] {
            let header = format!(r#"{{"w":{{"dtype":"F32","shape":{shape},"data_offsets":{offsets}}}}}"#);
            let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
            bytes.extend(header.as_bytes()); bytes.extend(1.25f32.to_le_bytes());
            std::fs::write(&p, bytes).unwrap();
            assert!(safetensors(&p, &|_| true).is_err(), "shape={shape}, offsets={offsets}");
        }
        let header = r#"{"w":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#;
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend(header.as_bytes()); bytes.extend(1.25f32.to_le_bytes());
        std::fs::write(&p, bytes).unwrap();
        let ts = safetensors(&p, &|_| true).unwrap();
        assert_eq!(ts["w"].form, vec![1]); assert_eq!(ts["w"].data, vec![1.25]);
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn wrong_model_architecture_is_an_error_not_a_crash() {
        let empty = |form: &[usize]| Tensor { form: form.to_vec(), data: Vec::new() };
        let dense = empty(&[512, 768]);
        let expected = text_shapes(&dense);
        // complete and correctly shaped: fine
        let mut t: HashMap<String, Tensor> = expected.iter().map(|(n, f)| (n.clone(), empty(f))).collect();
        t.insert("embeddings.word_embeddings.weight".into(), empty(&[30522, 768]));
        t.insert("embeddings.position_embeddings.weight".into(), empty(&[512, 768]));
        let minimum = [("embeddings.word_embeddings.weight", 30000, 768), ("embeddings.position_embeddings.weight", 128, 768)];
        check_shapes(&t, &expected, &minimum).unwrap();
        // word embedding too small (token ID could reach past the end)
        t.insert("embeddings.word_embeddings.weight".into(), empty(&[100, 768]));
        assert!(check_shapes(&t, &expected, &minimum).unwrap_err().contains("word_embeddings"));
        t.insert("embeddings.word_embeddings.weight".into(), empty(&[30522, 768]));
        // missing tensor
        let remove_fn = t.remove("transformer.layer.3.ffn.lin1.weight").unwrap();
        assert!(check_shapes(&t, &expected, &minimum).unwrap_err().contains("fehlt"));
        // wrong shape
        t.insert("transformer.layer.3.ffn.lin1.weight".into(), empty(&[768, 3072]));
        assert!(check_shapes(&t, &expected, &minimum).unwrap_err().contains("Form"));
        t.insert("transformer.layer.3.ffn.lin1.weight".into(), remove_fn);
        check_shapes(&t, &expected, &minimum).unwrap();
        // image encoder: empty model
        assert!(check_shapes(&HashMap::new(), &image_shapes(), &[]).is_err());
        assert_eq!(image_shapes().len(), 8 + 12 * 16);
    }

    #[test]
    fn mismatched_dimensions_never_reach_blas() {
        assert!(std::panic::catch_unwind(|| matmul_bt(&[1.0], 2, 2, &[1.0; 4], 2)).is_err());
        assert!(std::panic::catch_unwind(|| matmul_bt(&[1.0; 4], 2, 2, &[1.0], 2)).is_err());
        assert!(std::panic::catch_unwind(|| matmul_bt(&[], usize::MAX, 2, &[], 2)).is_err());
    }
}
