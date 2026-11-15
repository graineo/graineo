//! Multilingual text model for the semantic search: M-CLIP "XLM-Roberta-Large-Vit-B-32"
//! (Carlsson et al. 2022). It maps text into the space of OpenAI CLIP ViT-B/32, the image
//! vectors of the library therefore stay valid (no re-reading of the photos).
//!
//! - **Tokenizer:** SentencePiece Unigram as described by `tokenizer.json` (XLM-R): precompiled
//!   character map (NFKC-like, applied per grapheme cluster), split at whitespace, "▁" in front of
//!   every word, best segmentation by Viterbi over the piece scores, unknown characters as `<unk>`
//!   (consecutive ones fused), `<s> … </s>`, at most 128 tokens.
//! - **Model:** XLM-RoBERTa large: 24 layers, width 1024, 16 heads, post-LayerNorm, GELU (erf),
//!   positions from 2; then the mean over all tokens and a linear map 1024→512 with bias,
//!   normalized.
//! - **Weights:** at installation the F32 file is converted once into F16 (`convert_f16`):
//!   1.1 GB instead of 2.2 GB. The word table (250 002 × 1024) stays on disk, only the rows of the
//!   tokens are read; the 24 layers (~600 MB in F16) are kept in memory and widened to F32 per
//!   matrix right before the matrix product (BLAS works in F32).
//!
//! Numerically checked against the Python reference (transformers, F32): cosine > 0.999.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use half::f16;
use half::slice::HalfFloatSliceExt;
use unicode_segmentation::UnicodeSegmentation;

use crate::semantic::{erff, matmul_bt, normalize};

// ───────────────────────────────────────────── Safetensors, read piece by piece

/// Read exactly `buf.len()` bytes at `offset` (without moving a shared file position).
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dtype {
    F32,
    F16,
    Other,
}

impl Dtype {
    fn size(self) -> u64 {
        match self {
            Dtype::F32 => 4,
            Dtype::F16 => 2,
            Dtype::Other => 1,
        }
    }
}

struct Entry {
    dtype: Dtype,
    form: Vec<usize>,
    /// absolute byte position in the file
    start: u64,
    bytes: u64,
}

/// A Safetensors file whose tensors are read on demand (the file can be larger than one wants
/// to hold in memory).
struct Safetensors {
    file: File,
    entries: HashMap<String, Entry>,
}

impl Safetensors {
    fn open(p: &Path) -> Result<Safetensors, String> {
        let mut file = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let total = file.metadata().map_err(|e| e.to_string())?.len();
        let mut b8 = [0u8; 8];
        file.read_exact(&mut b8).map_err(|_| "Modellkopf zu kurz")?;
        let n = u64::from_le_bytes(b8);
        if n > 16 * 1024 * 1024 || 8 + n > total {
            return Err("Modellkopf zu groß".into());
        }
        let mut head = vec![0u8; n as usize];
        file.read_exact(&mut head).map_err(|_| "Modellkopf abgeschnitten")?;
        let header: serde_json::Value = serde_json::from_slice(&head).map_err(|e| e.to_string())?;
        let base = 8 + n;
        let whole = |v: &serde_json::Value| -> Result<u64, String> { v.as_u64().ok_or_else(|| "Ungültige Modelldimension oder Datenposition".to_string()) };
        let mut entries = HashMap::new();
        for (name, v) in header.as_object().ok_or("Modellkopf ist kein Objekt")? {
            if name == "__metadata__" {
                continue;
            }
            let dtype = match v["dtype"].as_str() {
                Some("F32") => Dtype::F32,
                Some("F16") => Dtype::F16,
                _ => Dtype::Other,
            };
            let form: Vec<usize> = v["shape"].as_array().ok_or("Modellform fehlt")?.iter().map(|x| whole(x).and_then(|x| usize::try_from(x).map_err(|_| "Modelldimension zu groß".to_string()))).collect::<Result<_, _>>()?;
            let o = v["data_offsets"].as_array().filter(|o| o.len() == 2).ok_or("Modell benötigt zwei Datenpositionen")?;
            let (a, e) = (whole(&o[0])?, whole(&o[1])?);
            if a > e || base.checked_add(e).is_none_or(|end| end > total) {
                return Err("Modelldaten passen nicht zur Dateigröße".into());
            }
            if dtype != Dtype::Other {
                let count = form.iter().try_fold(1u64, |n, &d| n.checked_mul(d as u64)).ok_or("Modellform zu groß")?;
                if count.checked_mul(dtype.size()) != Some(e - a) {
                    return Err(format!("Modelldaten von „{name}“ passen nicht zur Form"));
                }
            }
            entries.insert(name.clone(), Entry { dtype, form, start: base + a, bytes: e - a });
        }
        Ok(Safetensors { file, entries })
    }

    fn entry(&self, name: &str, form: &[usize]) -> Result<&Entry, String> {
        let e = self.entries.get(name).ok_or_else(|| format!("Modell passt nicht: Tensor „{name}“ fehlt"))?;
        if e.form != form {
            return Err(format!("Modell passt nicht: „{name}“ hat die Form {:?}, erwartet {form:?}", e.form));
        }
        if e.dtype == Dtype::Other {
            return Err(format!("Modell passt nicht: „{name}“ ist weder F32 noch F16"));
        }
        Ok(e)
    }

    /// Tensor as F32 (F16 is widened).
    fn f32(&self, name: &str, form: &[usize]) -> Result<Vec<f32>, String> {
        let e = self.entry(name, form)?;
        Ok(match self.weights(e)? {
            Weights::F32(v) => v,
            Weights::F16(v) => {
                let mut out = vec![0f32; v.len()];
                v.convert_to_f32_slice(&mut out);
                out
            }
        })
    }

    /// Matrix in the stored precision.
    fn matrix(&self, name: &str, rows: usize, cols: usize) -> Result<Matrix, String> {
        let e = self.entry(name, &[rows, cols])?;
        Ok(Matrix { rows, cols, w: self.weights(e)? })
    }

    fn weights(&self, e: &Entry) -> Result<Weights, String> {
        let n = (e.bytes / e.dtype.size()) as usize;
        let err = |x: std::io::Error| format!("Modell nicht lesbar: {x}");
        Ok(match e.dtype {
            Dtype::F16 => {
                let mut v = vec![f16::ZERO; n];
                // SAFETY: f16 is a plain 16-bit value; the byte view covers exactly the vector.
                let bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, n * 2) };
                read_at(&self.file, bytes, e.start).map_err(err)?;
                for x in &mut v {
                    *x = f16::from_bits(u16::from_le(x.to_bits()));
                }
                Weights::F16(v)
            }
            _ => {
                let mut v = vec![0f32; n];
                // SAFETY: as above, f32 has no invalid bit patterns.
                let bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, n * 4) };
                read_at(&self.file, bytes, e.start).map_err(err)?;
                for x in &mut v {
                    *x = f32::from_bits(u32::from_le(x.to_bits()));
                }
                Weights::F32(v)
            }
        })
    }
}

enum Weights {
    F32(Vec<f32>),
    F16(Vec<f16>),
}

/// Weight matrix `[rows × cols]` like `nn.Linear` (rows = outputs).
struct Matrix {
    rows: usize,
    cols: usize,
    w: Weights,
}

/// `y = x · Wᵀ + b` for `m` rows of `x`. F16 weights are widened into `scratch` first.
fn linear(x: &[f32], m: usize, w: &Matrix, b: &[f32], scratch: &mut Vec<f32>) -> Vec<f32> {
    let mut y = match &w.w {
        Weights::F32(v) => matmul_bt(x, m, w.cols, v, w.rows),
        Weights::F16(v) => {
            // in blocks of ~2 MB: widened rows stay in the cache for the matrix product, so per query
            // only the F16 weights travel through memory (instead of writing and reading a full F32 copy)
            let block = ((1 << 19) / w.cols).clamp(1, w.rows);
            scratch.resize(block * w.cols, 0.0);
            let mut y = vec![0f32; m * w.rows];
            let mut r0 = 0;
            while r0 < w.rows {
                let nb = block.min(w.rows - r0);
                let part = &mut scratch[..nb * w.cols];
                widen(&v[r0 * w.cols..(r0 + nb) * w.cols], part);
                let t = matmul_bt(x, m, w.cols, part, nb);
                for i in 0..m {
                    y[i * w.rows + r0..i * w.rows + r0 + nb].copy_from_slice(&t[i * nb..(i + 1) * nb]);
                }
                r0 += nb;
            }
            y
        }
    };
    for r in y.chunks_mut(w.rows) {
        for (v, bb) in r.iter_mut().zip(b) {
            *v += bb;
        }
    }
    y
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct VImageBuffer {
    data: *mut std::ffi::c_void,
    height: usize,
    width: usize,
    row_bytes: usize,
}

#[cfg(target_os = "macos")]
#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn vImageConvert_Planar16FtoPlanarF(src: *const VImageBuffer, dest: *const VImageBuffer, flags: u32) -> isize;
}

/// F16 → F32 for a whole matrix. This is the largest single cost per query (≈ 300 million values),
/// so on the Mac the vectorised conversion of Accelerate is used; elsewhere `half`.
fn widen(src: &[f16], dst: &mut [f32]) {
    debug_assert_eq!(src.len(), dst.len());
    #[cfg(target_os = "macos")]
    {
        // in rows of 4096 values (the buffer width is limited only by memory)
        const W: usize = 4096;
        if src.len() % W == 0 || src.len() < W {
            let w = if src.len() < W { src.len() } else { W };
            let h = src.len() / w.max(1);
            let s = VImageBuffer { data: src.as_ptr() as *mut _, height: h, width: w, row_bytes: w * 2 };
            let d = VImageBuffer { data: dst.as_mut_ptr() as *mut _, height: h, width: w, row_bytes: w * 4 };
            // SAFETY: both buffers cover exactly h × w values of their type.
            // kvImageDoNotTile: the blocks are small, no extra threads
            if h > 0 && unsafe { vImageConvert_Planar16FtoPlanarF(&s, &d, 16) } == 0 {
                return;
            }
        }
    }
    src.convert_to_f32_slice(dst);
}

fn layernorm(x: &mut [f32], d: usize, g: &[f32], b: &[f32]) {
    for r in x.chunks_mut(d) {
        let mean = r.iter().sum::<f32>() / d as f32;
        let var = r.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / d as f32;
        let inv = 1.0 / (var + 1e-5).sqrt();
        for (i, v) in r.iter_mut().enumerate() {
            *v = (*v - mean) * inv * g[i] + b[i];
        }
    }
}

// ───────────────────────────────────────────── Conversion F32 → F16 (at installation)

/// Tensors the model does not need (pooler, position index buffer) are left out of the F16 file.
fn needed(name: &str) -> bool {
    name.starts_with("LinearTransformation.") || (name.starts_with("transformer.") && !name.starts_with("transformer.pooler.") && name != "transformer.embeddings.position_ids")
}

/// Write the model as F16: every matrix with at least 2²⁰ values (word table, layer weights) in
/// F16, everything else (LayerNorm, biases, positions, final projection) stays F32. The result is
/// deterministic (names sorted, rounding to nearest even), so its SHA-256 can be pinned.
pub fn convert_f16(src: &Path, dst: &Path) -> Result<(), String> {
    let st = Safetensors::open(src)?;
    let names: BTreeMap<&str, &Entry> = st.entries.iter().filter(|(n, e)| needed(n) && e.dtype != Dtype::Other).map(|(n, e)| (n.as_str(), e)).collect();
    let half_of = |e: &Entry| e.form.len() == 2 && e.form[0] * e.form[1] >= 1 << 20;
    let mut head = String::from("{");
    let mut offset = 0u64;
    for (k, (name, e)) in names.iter().enumerate() {
        let count: u64 = e.form.iter().map(|&d| d as u64).product();
        let bytes = count * if half_of(e) { 2 } else { 4 };
        if k > 0 {
            head.push(',');
        }
        head.push_str(&format!(
            "{}:{{\"dtype\":\"{}\",\"shape\":{},\"data_offsets\":[{offset},{}]}}",
            serde_json::to_string(name).map_err(|e| e.to_string())?,
            if half_of(e) { "F16" } else { "F32" },
            serde_json::to_string(&e.form).map_err(|e| e.to_string())?,
            offset + bytes
        ));
        offset += bytes;
    }
    head.push('}');
    while head.len() % 8 != 0 {
        head.push(' ');
    }
    let out = File::create(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, out);
    let werr = |e: std::io::Error| format!("{}: {e}", dst.display());
    w.write_all(&(head.len() as u64).to_le_bytes()).map_err(werr)?;
    w.write_all(head.as_bytes()).map_err(werr)?;
    const CHUNK: usize = 1 << 20;
    let mut raw = vec![0u8; CHUNK * 4];
    let mut f32s = vec![0f32; CHUNK];
    let mut f16s = vec![f16::ZERO; CHUNK];
    for e in names.values() {
        let in_size = e.dtype.size() as usize;
        let total = (e.bytes / e.dtype.size()) as usize;
        let mut done = 0usize;
        while done < total {
            let n = CHUNK.min(total - done);
            let buf = &mut raw[..n * in_size];
            read_at(&st.file, buf, e.start + (done * in_size) as u64).map_err(|x| format!("Modell nicht lesbar: {x}"))?;
            match e.dtype {
                Dtype::F16 => {
                    for (o, b) in f32s[..n].iter_mut().zip(buf.chunks_exact(2)) {
                        *o = f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32();
                    }
                }
                _ => {
                    for (o, b) in f32s[..n].iter_mut().zip(buf.chunks_exact(4)) {
                        *o = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    }
                }
            }
            if half_of(e) {
                f16s[..n].convert_from_f32_slice(&f32s[..n]);
                for h in &f16s[..n] {
                    w.write_all(&h.to_bits().to_le_bytes()).map_err(werr)?;
                }
            } else {
                for v in &f32s[..n] {
                    w.write_all(&v.to_le_bytes()).map_err(werr)?;
                }
            }
            done += n;
        }
    }
    let out = w.into_inner().map_err(|e| e.to_string())?;
    out.sync_all().map_err(werr)?;
    Ok(())
}

// ───────────────────────────────────────────── Tokenizer (SentencePiece Unigram)

/// Precompiled character map of SentencePiece: a double-array trie over UTF-8 bytes whose
/// leaves point into a pool of null-terminated replacement strings.
struct CharMap {
    trie: Vec<u32>,
    pool: Vec<u8>,
}

impl CharMap {
    fn new(blob: &[u8]) -> Result<CharMap, String> {
        let size = u32::from_le_bytes(blob.get(..4).ok_or("Zeichentabelle zu kurz")?.try_into().map_err(|_| "Zeichentabelle zu kurz")?) as usize;
        let trie_bytes = blob.get(4..4 + size).filter(|t| t.len() % 4 == 0).ok_or("Zeichentabelle beschädigt")?;
        let trie = trie_bytes.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        Ok(CharMap { trie, pool: blob[4 + size..].to_vec() })
    }

    /// Value of the shortest key in the trie that is a prefix of `key` (as the reference does).
    fn first_prefix(&self, key: &[u8]) -> Option<usize> {
        let offset = |u: u32| ((u >> 10) << ((u & (1 << 9)) >> 6)) as usize;
        let mut pos = 0usize;
        let unit = *self.trie.first()?;
        pos ^= offset(unit);
        for &c in key {
            if c == 0 {
                return None;
            }
            pos ^= c as usize;
            let unit = *self.trie.get(pos)?;
            if (unit & ((1 << 31) | 0xFF)) as usize != c as usize {
                return None;
            }
            pos ^= offset(unit);
            if (unit >> 8) & 1 == 1 {
                return Some((*self.trie.get(pos)? & ((1 << 31) - 1)) as usize);
            }
        }
        None
    }

    fn transform(&self, chunk: &str) -> Option<&[u8]> {
        let i = self.first_prefix(chunk.as_bytes())?;
        let rest = self.pool.get(i..)?;
        Some(&rest[..rest.iter().position(|&b| b == 0).unwrap_or(rest.len())])
    }

    /// Replace per grapheme cluster; clusters of 6 bytes or more, and clusters without an entry,
    /// character by character.
    fn normalize(&self, s: &str) -> String {
        let mut out: Vec<u8> = Vec::with_capacity(s.len());
        for g in s.graphemes(true) {
            if g.len() < 6 {
                if let Some(n) = self.transform(g) {
                    out.extend_from_slice(n);
                    continue;
                }
            }
            for (i, c) in g.char_indices() {
                let part = &g[i..i + c.len_utf8()];
                match self.transform(part) {
                    Some(n) => out.extend_from_slice(n),
                    None => out.extend_from_slice(part.as_bytes()),
                }
            }
        }
        String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }
}

/// SentencePiece Unigram tokenizer of XLM-R (`tokenizer.json`).
pub struct Unigram {
    /// piece → (ID, score)
    pieces: HashMap<String, (u32, f64)>,
    /// longest piece in bytes
    max_len: usize,
    unk: u32,
    unk_score: f64,
    bos: u32,
    eos: u32,
    /// special tokens recognised in the raw text: content, ID, whitespace on the left belongs to it
    added: Vec<(String, u32, bool)>,
    charmap: Option<CharMap>,
    size: usize,
}

const META: char = '▁';

impl Unigram {
    pub fn load(tokenizer_json: &Path) -> Result<Unigram, String> {
        let raw = std::fs::read(tokenizer_json).map_err(|e| format!("{}: {e}", tokenizer_json.display()))?;
        Self::from_json(&raw)
    }

    pub fn from_json(raw: &[u8]) -> Result<Unigram, String> {
        let j: serde_json::Value = serde_json::from_slice(raw).map_err(|e| format!("tokenizer.json: {e}"))?;
        let model = &j["model"];
        if model["type"] != "Unigram" {
            return Err("tokenizer.json: kein Unigram-Modell".into());
        }
        let vocab = model["vocab"].as_array().ok_or("tokenizer.json: Vokabular fehlt")?;
        let mut pieces = HashMap::with_capacity(vocab.len());
        let mut min_score = f64::INFINITY;
        let mut max_len = 1;
        for (id, e) in vocab.iter().enumerate() {
            let piece = e[0].as_str().ok_or("tokenizer.json: Vokabular beschädigt")?;
            let score = e[1].as_f64().ok_or("tokenizer.json: Vokabular beschädigt")?;
            min_score = min_score.min(score);
            max_len = max_len.max(piece.len());
            // a duplicate piece keeps the last ID, like the reference
            pieces.insert(piece.to_string(), (id as u32, score));
        }
        let unk = model["unk_id"].as_u64().ok_or("tokenizer.json: unk_id fehlt")? as u32;
        let id = |s: &str| pieces.get(s).map(|p| p.0).ok_or(format!("tokenizer.json: {s} fehlt"));
        let (bos, eos) = (id("<s>")?, id("</s>")?);
        let added = j["added_tokens"]
            .as_array()
            .map(|a| a.iter().filter_map(|t| Some((t["content"].as_str()?.to_string(), t["id"].as_u64()? as u32, t["lstrip"].as_bool().unwrap_or(false)))).filter(|t| !t.0.is_empty()).collect())
            .unwrap_or_default();
        let norm = &j["normalizer"];
        let precompiled = |n: &serde_json::Value| -> Option<String> { (n["type"] == "Precompiled").then(|| n["precompiled_charsmap"].as_str().map(str::to_string)).flatten() };
        let map = precompiled(norm).or_else(|| norm["normalizers"].as_array().and_then(|a| a.iter().find_map(precompiled)));
        let charmap = match map {
            Some(b) => {
                use base64::Engine;
                Some(CharMap::new(&base64::engine::general_purpose::STANDARD.decode(b).map_err(|e| format!("tokenizer.json: {e}"))?)?)
            }
            None => None,
        };
        Ok(Unigram { size: vocab.len(), pieces, max_len, unk, unk_score: min_score - 10.0, bos, eos, added, charmap })
    }

    /// Number of IDs (rows the word table must have at least).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Text → token IDs with `<s>` … `</s>`, at most `max` tokens.
    pub fn encode(&self, text: &str, max: usize) -> Vec<u32> {
        let mut ids = vec![self.bos];
        let mut rest = text;
        while !rest.is_empty() {
            // earliest (then longest) special token in the raw text
            let hit = self.added.iter().filter_map(|(c, id, l)| rest.find(c.as_str()).map(|p| (p, c.len(), *id, *l))).min_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            match hit {
                Some((p, len, id, lstrip)) => {
                    let before = if lstrip { rest[..p].trim_end() } else { &rest[..p] };
                    self.encode_plain(before, &mut ids);
                    ids.push(id);
                    rest = &rest[p + len..];
                }
                None => {
                    self.encode_plain(rest, &mut ids);
                    rest = "";
                }
            }
        }
        ids.truncate(max.max(2) - 1);
        ids.push(self.eos);
        ids
    }

    fn encode_plain(&self, text: &str, ids: &mut Vec<u32>) {
        let normalized = match &self.charmap {
            Some(m) => m.normalize(text),
            None => text.to_string(),
        };
        for word in normalized.split(char::is_whitespace).filter(|w| !w.is_empty()) {
            let word = if word.starts_with(META) { word.to_string() } else { format!("{META}{word}") };
            // split before every "▁" (it belongs to the following part)
            let mut start = 0;
            for (i, _) in word.match_indices(META).skip(1) {
                self.viterbi(&word[start..i], ids);
                start = i;
            }
            self.viterbi(&word[start..], ids);
        }
    }

    /// Best segmentation by the sum of the piece scores.
    fn viterbi(&self, s: &str, ids: &mut Vec<u32>) {
        let n = s.len();
        if n == 0 {
            return;
        }
        // per end position: (score, start, ID); start = usize::MAX: not reached yet
        let mut best: Vec<(f64, usize, u32)> = vec![(0.0, usize::MAX, 0); n + 1];
        let mut pos = 0;
        while pos < n {
            let first = s[pos..].chars().next().map_or(1, char::len_utf8);
            let base = best[pos].0;
            let mut single = false;
            for (off, c) in s[pos..].char_indices() {
                let end = pos + off + c.len_utf8();
                if end - pos > self.max_len {
                    break;
                }
                if let Some(&(id, score)) = self.pieces.get(&s[pos..end]) {
                    let cand = base + score;
                    if best[end].1 == usize::MAX || cand > best[end].0 {
                        best[end] = (cand, pos, id);
                    }
                    if end - pos == first {
                        single = true;
                    }
                }
            }
            if !single {
                let end = pos + first;
                let cand = base + self.unk_score;
                if best[end].1 == usize::MAX || cand > best[end].0 {
                    best[end] = (cand, pos, self.unk);
                }
            }
            pos += first;
        }
        let mut path = Vec::new();
        let mut end = n;
        while end > 0 {
            let (_, start, id) = best[end];
            // consecutive unknown characters become one <unk>
            if !(id == self.unk && path.last() == Some(&self.unk)) {
                path.push(id);
            }
            end = start;
        }
        ids.extend(path.iter().rev());
    }
}

// ───────────────────────────────────────────── Encoder

const D: usize = 1024;
const HEADS: usize = 16;
const FF: usize = 4096;
const LAYERS: usize = 24;
const OUT: usize = 512;
const MAX_TOKENS: usize = 128;

struct Layer {
    q: Matrix,
    k: Matrix,
    v: Matrix,
    o: Matrix,
    qb: Vec<f32>,
    kb: Vec<f32>,
    vb: Vec<f32>,
    ob: Vec<f32>,
    ln1: (Vec<f32>, Vec<f32>),
    up: Matrix,
    upb: Vec<f32>,
    down: Matrix,
    downb: Vec<f32>,
    ln2: (Vec<f32>, Vec<f32>),
}

pub struct MclipEncoder {
    pub tokenizer: Unigram,
    /// word table stays in the file
    words: Safetensors,
    positions: Vec<f32>,
    token_type: Vec<f32>,
    ln: (Vec<f32>, Vec<f32>),
    layers: Vec<Layer>,
    proj: Vec<f32>,
    proj_b: Vec<f32>,
}

impl MclipEncoder {
    /// From a module folder with `tokenizer.json` and `model-f16.safetensors` (or, for checks,
    /// the original `model.safetensors` in F32).
    pub fn load(dir: &Path) -> Result<MclipEncoder, String> {
        let tokenizer = Unigram::load(&dir.join("tokenizer.json"))?;
        let file = [MODEL_F16, MODEL_F32].iter().map(|f| dir.join(f)).find(|p| p.exists()).ok_or("M-CLIP: Modelldatei fehlt")?;
        Self::load_files(tokenizer, &file)
    }

    pub fn load_files(tokenizer: Unigram, model: &Path) -> Result<MclipEncoder, String> {
        let st = Safetensors::open(model)?;
        let we = st.entries.get(WORDS).ok_or_else(|| format!("Modell passt nicht: Tensor „{WORDS}“ fehlt"))?;
        if we.form.len() != 2 || we.form[1] != D || we.form[0] < tokenizer.size() || we.dtype == Dtype::Other {
            return Err(format!("Modell passt nicht: „{WORDS}“ hat die Form {:?}", we.form));
        }
        let e = |s: &str| format!("transformer.embeddings.{s}");
        let positions = st.f32(&e("position_embeddings.weight"), &[514, D])?;
        let token_type = st.f32(&e("token_type_embeddings.weight"), &[1, D])?;
        let ln = (st.f32(&e("LayerNorm.weight"), &[D])?, st.f32(&e("LayerNorm.bias"), &[D])?);
        let mut layers = Vec::with_capacity(LAYERS);
        for l in 0..LAYERS {
            let p = |s: &str| format!("transformer.encoder.layer.{l}.{s}");
            let vec = |s: &str, n: usize| st.f32(&p(s), &[n]);
            layers.push(Layer {
                q: st.matrix(&p("attention.self.query.weight"), D, D)?,
                k: st.matrix(&p("attention.self.key.weight"), D, D)?,
                v: st.matrix(&p("attention.self.value.weight"), D, D)?,
                o: st.matrix(&p("attention.output.dense.weight"), D, D)?,
                qb: vec("attention.self.query.bias", D)?,
                kb: vec("attention.self.key.bias", D)?,
                vb: vec("attention.self.value.bias", D)?,
                ob: vec("attention.output.dense.bias", D)?,
                ln1: (vec("attention.output.LayerNorm.weight", D)?, vec("attention.output.LayerNorm.bias", D)?),
                up: st.matrix(&p("intermediate.dense.weight"), FF, D)?,
                upb: vec("intermediate.dense.bias", FF)?,
                down: st.matrix(&p("output.dense.weight"), D, FF)?,
                downb: vec("output.dense.bias", D)?,
                ln2: (vec("output.LayerNorm.weight", D)?, vec("output.LayerNorm.bias", D)?),
            });
        }
        let proj = st.f32("LinearTransformation.weight", &[OUT, D])?;
        let proj_b = st.f32("LinearTransformation.bias", &[OUT])?;
        Ok(MclipEncoder { tokenizer, words: st, positions, token_type, ln, layers, proj, proj_b })
    }

    /// Row of the word table for one token.
    fn word(&self, id: u32, out: &mut [f32]) -> Result<(), String> {
        let e = &self.words.entries[WORDS];
        let size = e.dtype.size() as usize;
        let mut b = vec![0u8; D * size];
        read_at(&self.words.file, &mut b, e.start + (id as u64) * (D * size) as u64).map_err(|x| format!("Modell nicht lesbar: {x}"))?;
        if size == 2 {
            for (o, c) in out.iter_mut().zip(b.chunks_exact(2)) {
                *o = f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32();
            }
        } else {
            for (o, c) in out.iter_mut().zip(b.chunks_exact(4)) {
                *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
        }
        Ok(())
    }

    /// Sentence → normalized 512-dim vector (CLIP space). On a read error the zero vector.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        self.embed_batch(&[text]).pop().unwrap_or_else(|| vec![0.0; OUT])
    }

    /// Several sentences at once (all tokens go through the matrix products together, the weights
    /// are widened only once per batch).
    pub fn embed_batch(&self, texts: &[&str]) -> Vec<Vec<f32>> {
        let seqs: Vec<Vec<u32>> = texts.iter().map(|t| self.tokenizer.encode(t, MAX_TOKENS)).collect();
        self.embed_ids(&seqs).unwrap_or_else(|_| vec![vec![0.0; OUT]; texts.len()])
    }

    pub fn embed_ids(&self, seqs: &[Vec<u32>]) -> Result<Vec<Vec<f32>>, String> {
        let total: usize = seqs.iter().map(Vec::len).sum();
        if total == 0 {
            return Ok(Vec::new());
        }
        let mut x = vec![0f32; total * D];
        let mut row = 0;
        for s in seqs {
            for (i, &id) in s.iter().enumerate() {
                let r = &mut x[row * D..(row + 1) * D];
                self.word(id, r)?;
                // positions start after the padding index (1): 2, 3, …
                let p = &self.positions[(i + 2).min(513) * D..((i + 2).min(513) + 1) * D];
                for t in 0..D {
                    r[t] += p[t] + self.token_type[t];
                }
                row += 1;
            }
        }
        layernorm(&mut x, D, &self.ln.0, &self.ln.1);
        let mut scratch = Vec::new();
        let hd = D / HEADS;
        let scale = 1.0 / (hd as f32).sqrt();
        for l in &self.layers {
            let q = linear(&x, total, &l.q, &l.qb, &mut scratch);
            let k = linear(&x, total, &l.k, &l.kb, &mut scratch);
            let v = linear(&x, total, &l.v, &l.vb, &mut scratch);
            let mut ctx = vec![0f32; total * D];
            let mut from = 0;
            for s in seqs {
                let n = s.len();
                let mut sc = vec![0f32; n];
                for h in 0..HEADS {
                    for i in 0..n {
                        let qi = &q[(from + i) * D + h * hd..(from + i) * D + (h + 1) * hd];
                        for j in 0..n {
                            sc[j] = qi.iter().zip(&k[(from + j) * D + h * hd..(from + j) * D + (h + 1) * hd]).map(|(a, b)| a * b).sum::<f32>() * scale;
                        }
                        let m = sc.iter().cloned().fold(f32::MIN, f32::max);
                        let mut sum = 0.0;
                        for w in &mut sc {
                            *w = (*w - m).exp();
                            sum += *w;
                        }
                        let c = &mut ctx[(from + i) * D + h * hd..(from + i) * D + (h + 1) * hd];
                        for j in 0..n {
                            let w = sc[j] / sum;
                            for (cv, vv) in c.iter_mut().zip(&v[(from + j) * D + h * hd..(from + j) * D + (h + 1) * hd]) {
                                *cv += w * vv;
                            }
                        }
                    }
                }
                from += n;
            }
            let a = linear(&ctx, total, &l.o, &l.ob, &mut scratch);
            for (xv, av) in x.iter_mut().zip(&a) {
                *xv += av;
            }
            layernorm(&mut x, D, &l.ln1.0, &l.ln1.1);
            let mut hdn = linear(&x, total, &l.up, &l.upb, &mut scratch);
            for v in &mut hdn {
                *v = 0.5 * *v * (1.0 + unsafe { erff(*v / std::f32::consts::SQRT_2) });
            }
            let f = linear(&hdn, total, &l.down, &l.downb, &mut scratch);
            for (xv, fv) in x.iter_mut().zip(&f) {
                *xv += fv;
            }
            layernorm(&mut x, D, &l.ln2.0, &l.ln2.1);
        }
        let mut out = Vec::with_capacity(seqs.len());
        let mut from = 0;
        for s in seqs {
            let n = s.len();
            let mut mean = vec![0f32; D];
            for r in x[from * D..(from + n) * D].chunks(D) {
                for (m, v) in mean.iter_mut().zip(r) {
                    *m += v;
                }
            }
            for m in &mut mean {
                *m /= n.max(1) as f32;
            }
            let mut y = matmul_bt(&mean, 1, D, &self.proj, OUT);
            for (v, b) in y.iter_mut().zip(&self.proj_b) {
                *v += b;
            }
            normalize(&mut y);
            out.push(y);
            from += n;
        }
        Ok(out)
    }
}

const WORDS: &str = "transformer.embeddings.word_embeddings.weight";
/// File names in the module folder.
pub const MODEL_F16: &str = "model-f16.safetensors";
pub const MODEL_F32: &str = "model.safetensors";

#[cfg(test)]
mod tests {
    use super::*;

    /// Small tokenizer in the format of `tokenizer.json` (without character map).
    fn small() -> Unigram {
        let j = serde_json::json!({
            "normalizer": null,
            "added_tokens": [
                {"id": 0, "content": "<s>", "lstrip": false}, {"id": 2, "content": "</s>", "lstrip": false},
                {"id": 9, "content": "<mask>", "lstrip": true}
            ],
            "model": {"type": "Unigram", "unk_id": 3, "vocab": [
                ["<s>", 0.0], ["<pad>", 0.0], ["</s>", 0.0], ["<unk>", 0.0],
                ["▁", -2.0], ["▁Hund", -3.0], ["▁Hu", -2.5], ["nd", -2.5], ["e", -1.0], ["<mask>", 0.0],
                ["▁Ha", -4.0], ["us", -4.0], ["▁Haus", -9.0], ["H", -5.0], ["a", -5.0], ["u", -5.0], ["s", -5.0]
            ]}
        });
        Unigram::from_json(j.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn unigram_takes_best_segmentation() {
        let t = small();
        // "▁Hund" (−3) beats "▁Hu"+"nd" (−5)
        assert_eq!(t.encode("Hund", 128), vec![0, 5, 2]);
        // "▁Ha"+"us" (−8) beats "▁Haus" (−9)
        assert_eq!(t.encode("Haus", 128), vec![0, 10, 11, 2]);
        // every word gets "▁", several spaces count as one
        assert_eq!(t.encode("  Hund   Hunde ", 128), vec![0, 5, 5, 8, 2]);
    }

    #[test]
    fn unknown_characters_are_fused() {
        let t = small();
        // "▁" alone, then "xyz" unknown → one <unk>, then "e"
        assert_eq!(t.encode("xyze", 128), vec![0, 4, 3, 8, 2]);
        assert_eq!(t.encode("€", 128), vec![0, 4, 3, 2]);
    }

    #[test]
    fn special_tokens_and_truncation() {
        let t = small();
        assert_eq!(t.encode("", 128), vec![0, 2]);
        // <mask> takes the whitespace on its left
        assert_eq!(t.encode("Hund <mask>e", 128), vec![0, 5, 9, 4, 8, 2]);
        assert_eq!(t.encode("Hund Hund Hund Hund", 4), vec![0, 5, 5, 2]);
    }

    #[test]
    fn charmap_trie_lookup() {
        // hand-built double array: root → 'a' (leaf, value 0) – pool "b\0"
        // unit layout: label in the low 8 bits, has_leaf bit 8, offset from bit 10
        let root = 1u32 << 10; // offset 1
        let mut trie = vec![0u32; 260];
        trie[0] = root;
        // node position: 0 ^ 1 ^ 'a'
        let pos = 1 ^ (b'a' as usize);
        trie[pos] = (b'a' as u32) | (1 << 8) | (2 << 10); // leaf, offset 2
        trie[pos ^ 2] = 0; // value: pool index 0
        let mut blob = ((trie.len() * 4) as u32).to_le_bytes().to_vec();
        for u in &trie {
            blob.extend(u.to_le_bytes());
        }
        blob.extend(b"b\0");
        let m = CharMap::new(&blob).unwrap();
        assert_eq!(m.normalize("aca"), "bcb");
        assert_eq!(m.normalize("äa"), "äb");
        // damaged blob: error, not a crash
        assert!(CharMap::new(&[1, 0]).is_err());
        assert!(CharMap::new(&[8, 0, 0, 0, 1]).is_err());
    }

    #[test]
    fn f16_conversion_is_lossless_for_small_tensors_and_rounds_matrices() {
        let dir = std::env::temp_dir().join(format!("graineo-xlmr-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let (src, dst) = (dir.join("a.safetensors"), dir.join("b.safetensors"));
        // one big matrix (2^20 values) → F16, a bias → F32, the pooler is dropped
        let big: Vec<f32> = (0..1 << 20).map(|i| (i as f32) * 1e-4 - 3.0).collect();
        let bias = [0.1f32, 0.2, 1.0 / 3.0];
        let head = format!(
            r#"{{"transformer.x.weight":{{"dtype":"F32","shape":[1024,1024],"data_offsets":[0,{a}]}},"transformer.x.bias":{{"dtype":"F32","shape":[3],"data_offsets":[{a},{b}]}},"transformer.pooler.dense.bias":{{"dtype":"F32","shape":[1],"data_offsets":[{b},{c}]}}}}"#,
            a = big.len() * 4,
            b = big.len() * 4 + 12,
            c = big.len() * 4 + 16
        );
        let mut bytes = (head.len() as u64).to_le_bytes().to_vec();
        bytes.extend(head.as_bytes());
        for v in big.iter().chain(&bias).chain(&[7.0f32]) {
            bytes.extend(v.to_le_bytes());
        }
        std::fs::write(&src, bytes).unwrap();
        convert_f16(&src, &dst).unwrap();
        let st = Safetensors::open(&dst).unwrap();
        assert!(!st.entries.contains_key("transformer.pooler.dense.bias"));
        assert_eq!(st.entries["transformer.x.weight"].dtype, Dtype::F16);
        assert_eq!(st.f32("transformer.x.bias", &[3]).unwrap(), bias.to_vec());
        let w = st.f32("transformer.x.weight", &[1024, 1024]).unwrap();
        assert!(w.iter().zip(&big).all(|(a, b)| (a - b).abs() <= b.abs() / 1024.0 + 1e-7));
        // same input, same bytes (the checksum can be pinned)
        let first = std::fs::read(&dst).unwrap();
        convert_f16(&src, &dst).unwrap();
        assert_eq!(first, std::fs::read(&dst).unwrap());
        assert_eq!(u64::from_le_bytes(first[..8].try_into().unwrap()) % 8, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_model_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("graineo-xlmr-bad-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir(&dir).unwrap();
        let p = dir.join("m.safetensors");
        for (shape, offsets) in [("[2]", "[0,4]"), ("[1]", "[4,0]"), ("[1]", "[0,400]")] {
            let head = format!(r#"{{"w":{{"dtype":"F32","shape":{shape},"data_offsets":{offsets}}}}}"#);
            let mut bytes = (head.len() as u64).to_le_bytes().to_vec();
            bytes.extend(head.as_bytes());
            bytes.extend(1.0f32.to_le_bytes());
            std::fs::write(&p, bytes).unwrap();
            assert!(Safetensors::open(&p).is_err(), "{shape} {offsets}");
        }
        std::fs::write(&p, u64::MAX.to_le_bytes()).unwrap();
        assert!(Safetensors::open(&p).is_err());
        // a complete header but not an M-CLIP model
        let head = r#"{"w":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#;
        let mut bytes = (head.len() as u64).to_le_bytes().to_vec();
        bytes.extend(head.as_bytes());
        bytes.extend(1.0f32.to_le_bytes());
        std::fs::write(&p, bytes).unwrap();
        assert!(MclipEncoder::load_files(small(), &p).err().unwrap().contains("fehlt"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Convert `GRAINEO_MCLIP_DIR/model.safetensors` into the F16 file (size and SHA-256 for `module::MCLIP`).
    #[test]
    #[ignore]
    fn convert_model() {
        let dir = std::path::PathBuf::from(std::env::var_os("GRAINEO_MCLIP_DIR").expect("GRAINEO_MCLIP_DIR"));
        let t = std::time::Instant::now();
        convert_f16(&dir.join(MODEL_F32), &dir.join(MODEL_F16)).unwrap();
        let p = dir.join(MODEL_F16);
        eprintln!("{} Bytes, SHA-256 {}, {:?}", std::fs::metadata(&p).unwrap().len(), crate::relocate::sha256(&p).unwrap(), t.elapsed());
    }

    /// Against the Python reference (transformers, F32): token IDs identical, cosine > 0.999.
    /// `GRAINEO_MCLIP_DIR` = folder with tokenizer.json and the model, `GRAINEO_MCLIP_REF` =
    /// JSON of the reference (`{"embed": [{text, ids, vec}], "tokens": [{text, ids}]}`).
    #[test]
    #[ignore]
    fn matches_python_reference() {
        let dir = std::path::PathBuf::from(std::env::var_os("GRAINEO_MCLIP_DIR").expect("GRAINEO_MCLIP_DIR"));
        let r: serde_json::Value = serde_json::from_slice(&std::fs::read(std::env::var_os("GRAINEO_MCLIP_REF").expect("GRAINEO_MCLIP_REF")).unwrap()).unwrap();
        let tok = Unigram::load(&dir.join("tokenizer.json")).unwrap();
        let mut wrong = 0;
        let all = r["tokens"].as_array().unwrap();
        for e in all.iter().chain(r["embed"].as_array().unwrap()) {
            let want: Vec<u32> = e["ids"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect();
            let got = tok.encode(e["text"].as_str().unwrap(), 128);
            if got != want {
                wrong += 1;
                eprintln!("Token weichen ab: {:?}\n  Rust   {got:?}\n  Python {want:?}", e["text"]);
            }
        }
        eprintln!("Tokenizer: {} sentences, {wrong} deviating", all.len() + r["embed"].as_array().unwrap().len());
        let file = std::env::var_os("GRAINEO_MCLIP_FILE").map(std::path::PathBuf::from);
        let t = std::time::Instant::now();
        let enc = match file {
            Some(f) => MclipEncoder::load_files(tok, &dir.join(f)).unwrap(),
            None => MclipEncoder::load(&dir).unwrap(),
        };
        eprintln!("geladen in {:?}", t.elapsed());
        let mut min = 1.0f32;
        let mut sum = 0.0;
        let list = r["embed"].as_array().unwrap();
        let t = std::time::Instant::now();
        for e in list {
            let want: Vec<f32> = e["vec"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
            let got = enc.embed(e["text"].as_str().unwrap());
            let cos: f32 = got.iter().zip(&want).map(|(a, b)| a * b).sum();
            if cos < 0.9995 {
                eprintln!("Kosinus {cos:.6}: {:?}", e["text"]);
            }
            min = min.min(cos);
            sum += cos;
        }
        eprintln!("Cosine over {} sentences: min {min:.6}, mean {:.6}; {:?} per sentence", list.len(), sum / list.len() as f32, t.elapsed() / list.len() as u32);
        assert_eq!(wrong, 0);
        assert!(min > 0.999, "min cosine {min}");
    }
}
