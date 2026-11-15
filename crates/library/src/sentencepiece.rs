//! SentencePiece tokenizer (BPE model with byte fallback) read directly from `tokenizer.model`
//! (protobuf), without an extra library. Used for the Gemma vocabulary of SigLIP 2 (256 000 pieces).
//!
//! Encoding follows the published SentencePiece BPE algorithm:
//! 1. normalize (only what the model's normalizer spec asks for: escape spaces as `▁`, optional
//!    dummy prefix, optional removal of extra spaces; a precompiled character map is not supported
//!    and rejected on load),
//! 2. split into characters, user-defined pieces (e.g. `<mask>`, runs of `▁`) taken whole
//!    (longest match),
//! 3. repeatedly merge the adjacent pair with the highest piece score whose concatenation is a
//!    piece (ties: leftmost),
//! 4. symbols without a piece become their UTF-8 bytes (`<0xNN>`), or `<unk>` without byte fallback.
//!
//! Checked against the reference implementation on all XTD10 captions (five languages) and special
//! cases (see `tests/siglip_measure.rs`).

use std::collections::HashMap;
use std::path::Path;

/// Piece types as in the model file.
const NORMAL: u32 = 1;
const UNKNOWN: u32 = 2;
const CONTROL: u32 = 3;
const USER_DEFINED: u32 = 4;
const BYTE: u32 = 6;
const UNUSED: u32 = 5;

pub struct SentencePiece {
    /// piece → (id, score) for pieces BPE may produce (normal, user-defined, unused)
    pieces: HashMap<String, (u32, f32)>,
    /// user-defined pieces, longest first (matched whole before BPE)
    user: Vec<String>,
    /// id of `<0xNN>` per byte (if the model has byte fallback)
    bytes: Option<Box<[u32; 256]>>,
    unk: u32,
    add_dummy_prefix: bool,
    remove_extra_whitespaces: bool,
    escape_whitespaces: bool,
    /// number of pieces (rows the embedding table must have at least)
    size: usize,
    /// ids of control pieces by name (`<eos>`, `<pad>` …)
    control: HashMap<String, u32>,
}

/// Minimal protobuf reader (only varint, 32-bit and length-delimited fields are needed).
struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

enum Field<'a> {
    Varint(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

impl<'a> Pb<'a> {
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for s in 0..10 {
            let c = *self.b.get(self.i).ok_or("Tokenizer-Datei abgeschnitten")?;
            self.i += 1;
            v |= u64::from(c & 0x7f) << (7 * s);
            if c & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("Tokenizer-Datei beschädigt (Zahl zu lang)".into())
    }

    fn next(&mut self) -> Result<Option<(u32, Field<'a>)>, String> {
        if self.i >= self.b.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let (no, wire) = ((key >> 3) as u32, key & 7);
        let f = match wire {
            0 => Field::Varint(self.varint()?),
            1 => {
                self.i = self.i.checked_add(8).filter(|&e| e <= self.b.len()).ok_or("Tokenizer-Datei abgeschnitten")?;
                Field::Varint(0)
            }
            2 => {
                let n = usize::try_from(self.varint()?).map_err(|_| "Tokenizer-Datei beschädigt")?;
                let e = self.i.checked_add(n).filter(|&e| e <= self.b.len()).ok_or("Tokenizer-Datei abgeschnitten")?;
                let s = &self.b[self.i..e];
                self.i = e;
                Field::Bytes(s)
            }
            5 => {
                let s = self.b.get(self.i..self.i + 4).ok_or("Tokenizer-Datei abgeschnitten")?;
                self.i += 4;
                Field::Fixed32(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            }
            _ => return Err(format!("Tokenizer-Datei: unbekannter Feldtyp {wire}")),
        };
        Ok(Some((no, f)))
    }
}

impl SentencePiece {
    pub fn load(path: &Path) -> Result<SentencePiece, String> {
        let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes(&b)
    }

    pub fn from_bytes(b: &[u8]) -> Result<SentencePiece, String> {
        let mut list: Vec<(String, f32, u32)> = Vec::new();
        let mut model_type = 1u64; // UNIGRAM by default in the format; we need BPE (2)
        let mut byte_fallback = false;
        let (mut add_dummy_prefix, mut remove_extra_whitespaces, mut escape_whitespaces) = (true, true, true);
        let mut pb = Pb { b, i: 0 };
        while let Some((no, f)) = pb.next()? {
            match (no, f) {
                (1, Field::Bytes(p)) => {
                    let (mut piece, mut score, mut kind) = (String::new(), 0f32, NORMAL);
                    let mut q = Pb { b: p, i: 0 };
                    while let Some((n, g)) = q.next()? {
                        match (n, g) {
                            (1, Field::Bytes(s)) => piece = String::from_utf8(s.to_vec()).map_err(|_| "Tokenizer: Stück ist kein UTF-8")?,
                            (2, Field::Fixed32(x)) => score = f32::from_bits(x),
                            (3, Field::Varint(x)) => kind = x as u32,
                            _ => {}
                        }
                    }
                    list.push((piece, score, kind));
                }
                (2, Field::Bytes(t)) => {
                    let mut q = Pb { b: t, i: 0 };
                    while let Some((n, g)) = q.next()? {
                        match (n, g) {
                            (3, Field::Varint(x)) => model_type = x,
                            (35, Field::Varint(x)) => byte_fallback = x != 0,
                            _ => {}
                        }
                    }
                }
                (3, Field::Bytes(t)) => {
                    let mut q = Pb { b: t, i: 0 };
                    while let Some((n, g)) = q.next()? {
                        match (n, g) {
                            (2, Field::Bytes(m)) if !m.is_empty() => return Err("Tokenizer mit eigener Zeichentabelle wird nicht unterstützt".into()),
                            (3, Field::Varint(x)) => add_dummy_prefix = x != 0,
                            (4, Field::Varint(x)) => remove_extra_whitespaces = x != 0,
                            (5, Field::Varint(x)) => escape_whitespaces = x != 0,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        if model_type != 2 {
            return Err(format!("Tokenizer-Typ {model_type} wird nicht unterstützt (nur BPE)"));
        }
        if list.is_empty() {
            return Err("Tokenizer ohne Vokabular".into());
        }
        let mut pieces = HashMap::with_capacity(list.len());
        let mut user = Vec::new();
        let mut control = HashMap::new();
        let mut bytes = byte_fallback.then(|| Box::new([u32::MAX; 256]));
        let mut unk = None;
        for (id, (p, score, kind)) in list.iter().enumerate() {
            let id = id as u32;
            match *kind {
                NORMAL | UNUSED => {
                    pieces.insert(p.clone(), (id, *score));
                }
                USER_DEFINED => {
                    pieces.insert(p.clone(), (id, *score));
                    user.push(p.clone());
                }
                UNKNOWN => unk = Some(id),
                CONTROL => {
                    control.insert(p.clone(), id);
                }
                BYTE => {
                    if let (Some(t), Some(h)) = (bytes.as_mut(), p.strip_prefix("<0x").and_then(|s| s.strip_suffix('>'))) {
                        if let Ok(v) = u8::from_str_radix(h, 16) {
                            t[v as usize] = id;
                        }
                    }
                }
                _ => {}
            }
        }
        if bytes.as_ref().is_some_and(|t| t.contains(&u32::MAX)) {
            return Err("Tokenizer: Byte-Stücke unvollständig".into());
        }
        user.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Ok(SentencePiece { pieces, user, bytes, unk: unk.ok_or("Tokenizer ohne <unk>")?, add_dummy_prefix, remove_extra_whitespaces, escape_whitespaces, size: list.len(), control })
    }

    /// Number of pieces (vocabulary size).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Id of a control piece such as `<eos>` or `<pad>`.
    pub fn control_id(&self, name: &str) -> Option<u32> {
        self.control.get(name).copied()
    }

    fn normalize(&self, text: &str) -> String {
        let mut s: String = if self.remove_extra_whitespaces { text.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ") } else { text.to_string() };
        if self.add_dummy_prefix && !s.is_empty() {
            s.insert(0, ' ');
        }
        if self.escape_whitespaces {
            s = s.replace(' ', "\u{2581}");
        }
        s
    }

    /// Text → piece ids (without special tokens).
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let s = self.normalize(text);
        // 1. initial symbols: user-defined pieces whole, otherwise single characters
        let mut sym: Vec<&str> = Vec::new();
        let mut fixed: Vec<bool> = Vec::new();
        let mut i = 0;
        while i < s.len() {
            let rest = &s[i..];
            if let Some(u) = self.user.iter().find(|u| rest.starts_with(u.as_str())) {
                sym.push(&s[i..i + u.len()]);
                fixed.push(true);
                i += u.len();
                continue;
            }
            let n = rest.chars().next().map_or(1, char::len_utf8);
            sym.push(&s[i..i + n]);
            fixed.push(false);
            i += n;
        }
        // 2. BPE merges: best scoring pair first, leftmost on ties. Symbols are contiguous slices of `s`,
        //    so a merge is just a wider slice.
        let mut buf = String::new();
        let score_of = |a: &str, b: &str, buf: &mut String| -> Option<f32> {
            buf.clear();
            buf.push_str(a);
            buf.push_str(b);
            self.pieces.get(buf.as_str()).map(|p| p.1)
        };
        let mut scores: Vec<Option<f32>> = (0..sym.len().saturating_sub(1)).map(|k| if fixed[k] || fixed[k + 1] { None } else { score_of(sym[k], sym[k + 1], &mut buf) }).collect();
        loop {
            let mut best: Option<(usize, f32)> = None;
            for (k, sc) in scores.iter().enumerate() {
                if let Some(v) = sc {
                    if best.is_none_or(|b| *v > b.1) {
                        best = Some((k, *v));
                    }
                }
            }
            let Some((k, _)) = best else { break };
            let start = sym[k].as_ptr() as usize - s.as_ptr() as usize;
            let end = sym[k + 1].as_ptr() as usize - s.as_ptr() as usize + sym[k + 1].len();
            sym[k] = &s[start..end];
            sym.remove(k + 1);
            fixed.remove(k + 1);
            scores.remove(k);
            if k > 0 {
                scores[k - 1] = if fixed[k - 1] { None } else { score_of(sym[k - 1], sym[k], &mut buf) };
            }
            if k < scores.len() {
                scores[k] = if fixed[k + 1] { None } else { score_of(sym[k], sym[k + 1], &mut buf) };
            }
        }
        // 3. ids, byte fallback for unknown symbols
        let mut ids = Vec::with_capacity(sym.len());
        for w in sym {
            match self.pieces.get(w) {
                Some(&(id, _)) => ids.push(id),
                None => match &self.bytes {
                    Some(t) => ids.extend(w.bytes().map(|b| t[b as usize])),
                    None => ids.push(self.unk),
                },
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a tiny model file: pieces (piece, score, type), BPE, byte fallback, Gemma normalizer.
    fn model(pieces: &[(&str, f32, u32)]) -> Vec<u8> {
        fn varint(o: &mut Vec<u8>, mut v: u64) {
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    o.push(b);
                    return;
                }
                o.push(b | 0x80);
            }
        }
        fn bytes(o: &mut Vec<u8>, no: u32, b: &[u8]) {
            varint(o, u64::from(no << 3 | 2));
            varint(o, b.len() as u64);
            o.extend_from_slice(b);
        }
        let mut o = Vec::new();
        let mut all: Vec<(String, f32, u32)> = pieces.iter().map(|(p, s, t)| (p.to_string(), *s, *t)).collect();
        all.extend((0..256).map(|b| (format!("<0x{b:02X}>"), 0.0, BYTE)));
        for (p, s, t) in &all {
            let mut m = Vec::new();
            bytes(&mut m, 1, p.as_bytes());
            varint(&mut m, 2 << 3 | 5);
            m.extend(s.to_bits().to_le_bytes());
            varint(&mut m, 3 << 3);
            varint(&mut m, u64::from(*t));
            bytes(&mut o, 1, &m);
        }
        let mut t = Vec::new();
        varint(&mut t, 3 << 3);
        varint(&mut t, 2);
        varint(&mut t, 35 << 3);
        varint(&mut t, 1);
        bytes(&mut o, 2, &t);
        let mut n = Vec::new();
        for (no, v) in [(3u64, 0u64), (4, 0), (5, 1)] {
            varint(&mut n, no << 3);
            varint(&mut n, v);
        }
        bytes(&mut o, 3, &n);
        o
    }

    #[test]
    fn bpe_merges_by_score_and_falls_back_to_bytes() {
        let m = model(&[("<pad>", 0.0, CONTROL), ("<eos>", 0.0, CONTROL), ("<unk>", 0.0, UNKNOWN), ("▁▁", 0.0, USER_DEFINED), ("a", -1.0, NORMAL), ("b", -1.0, NORMAL), ("▁", -1.0, NORMAL), ("ab", -2.0, NORMAL), ("▁ab", -3.0, NORMAL), ("bb", -5.0, NORMAL)]);
        let sp = SentencePiece::from_bytes(&m).unwrap();
        assert_eq!(sp.control_id("<eos>"), Some(1));
        // "ab" merges first (better score, leftmost first), then "▁ab"
        assert_eq!(sp.encode("ab ab"), vec![7, 8]);
        // "abb": ab (-2) beats bb (-5)
        assert_eq!(sp.encode("abb"), vec![7, 5]);
        // double space is a user-defined piece; "ä" is not a piece → its two UTF-8 bytes
        let ids = sp.encode("a  ä");
        assert_eq!(ids[..2], [4, 3]);
        assert_eq!(ids.len(), 4);
        assert_eq!(sp.encode(""), Vec::<u32>::new());
    }

    #[test]
    fn broken_files_are_errors() {
        assert!(SentencePiece::from_bytes(&[]).is_err());
        assert!(SentencePiece::from_bytes(&[0x0a, 0xff]).is_err());
        assert!(SentencePiece::from_bytes(&[0x0a, 0x05, 0x01]).is_err());
    }
}
