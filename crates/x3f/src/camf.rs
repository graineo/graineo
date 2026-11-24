//! CAMF ("camera file"): calibration and capture data of every X3F file.
//!
//! For Merrill cameras (CAMF type 4) the block is compressed with the same TRUE method
//! as the image data: prediction differences are Huffman-coded
//! and yield 12-bit values, which are packed in pairs into three bytes each.
//!
//! The unpacked block is a sequence of entries with a 20-byte header:
//! ```text
//! u32 id ("CMbT" text, "CMbP" properties, "CMbM" matrix)
//! u32 version, u32 entry_size, u32 name_offset, u32 value_offset
//! ```

use std::collections::BTreeMap;

use crate::container::u32_at;
use crate::error::{corrupt, Error, Result};
use crate::huffman::{decode_true_stream, TrueHuffman};

#[derive(Debug, Clone)]
pub enum MatrixData {
    Int(Vec<i32>),
    UInt(Vec<u32>),
    Float(Vec<f64>),
}

#[derive(Debug, Clone)]
pub struct CamfMatrix {
    /// Dimensions from outer to inner, e.g. `[("R", 3), ("C", 3)]`.
    pub dims: Vec<(String, usize)>,
    pub element_type: u32,
    pub data: MatrixData,
}

impl CamfMatrix {
    pub fn len(&self) -> usize {
        match &self.data {
            MatrixData::Int(v) => v.len(),
            MatrixData::UInt(v) => v.len(),
            MatrixData::Float(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_f64(&self) -> Vec<f64> {
        match &self.data {
            MatrixData::Int(v) => v.iter().map(|&x| x as f64).collect(),
            MatrixData::UInt(v) => v.iter().map(|&x| x as f64).collect(),
            MatrixData::Float(v) => v.clone(),
        }
    }

    pub fn shape(&self) -> Vec<usize> {
        self.dims.iter().map(|d| d.1).collect()
    }
}

#[derive(Debug, Clone)]
pub enum CamfEntry {
    Text(String),
    Properties(Vec<(String, String)>),
    Matrix(CamfMatrix),
}

#[derive(Debug, Default)]
pub struct Camf {
    /// Entries in file order.
    pub order: Vec<String>,
    pub entries: BTreeMap<String, CamfEntry>,
}

fn cstr(d: &[u8], off: usize) -> String {
    let s = d.get(off..).unwrap_or(&[]);
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).into_owned()
}

/// Upper bound for the decoded CAMF block (real files: well under 1 MB); larger values only occur in damaged files.
const MAX_DECODED: usize = 64 << 20;

impl Camf {
    const HEADER_SIZE: usize = 28;

    pub fn parse(section: &[u8]) -> Result<Self> {
        if section.get(0..4) != Some(b"SECc") {
            return corrupt("CAMF-Sektion ohne SECc-Kennung");
        }
        let kind = u32_at(section, 8)?;
        if kind == 5 {
            return Self::parse_type5(section);
        }
        if kind == 2 {
            return Self::parse_type2(section);
        }
        if kind != 4 {
            return Err(Error::Unsupported(format!("CAMF-Typ {kind} (nur Typ 2/Classic, 4/Merrill und 5/Quattro)")));
        }
        let decoded_size = u32_at(section, 12)? as usize;
        if decoded_size == 0 || decoded_size > MAX_DECODED {
            return corrupt(format!("CAMF mit unplausibler Größe {decoded_size}"));
        }
        let seed = u32_at(section, 16)? as i32;
        let block_size = u32_at(section, 20)? as usize;
        let block_count = u32_at(section, 24)? as usize;
        let data = section.get(Self::HEADER_SIZE..).ok_or_else(|| Error::Corrupt("CAMF zu kurz".into()))?;

        // Huffman table at the start of the data block, data stream from byte 32.
        let mut pairs = Vec::new();
        let mut p = 0;
        while let Some(&len) = data.get(p) {
            if len == 0 {
                break;
            }
            let Some(&code) = data.get(p + 1) else { return corrupt("CAMF-Huffman-Tabelle abgeschnitten") };
            pairs.push((len, code));
            p += 2;
        }
        let huff = TrueHuffman::new(&pairs)?;
        let stream = data.get(32..).ok_or_else(|| Error::Corrupt("CAMF zu kurz".into()))?;

        // Pack 12-bit values in pairs into three bytes.
        let mut out = vec![0u8; decoded_size];
        let mut pos = 0usize;
        let mut odd = false;
        decode_true_stream(&huff, stream, seed, block_count, block_size, |_, _, v| {
            if !odd {
                out[pos] = (v >> 4) as u8;
                pos += 1;
                if pos >= decoded_size {
                    return false;
                }
                out[pos] = ((v << 4) & 0xf0) as u8;
            } else {
                out[pos] |= ((v >> 8) & 0x0f) as u8;
                pos += 1;
                if pos >= decoded_size {
                    return false;
                }
                out[pos] = v as u8;
                pos += 1;
                if pos >= decoded_size {
                    return false;
                }
            }
            odd = !odd;
            true
        })?;

        Self::parse_entries(&out)
    }

    /// CAMF type 2 (older cameras, SD9 to DP2x): encrypted byte by byte with a pseudo-random stream
    /// (XOR), key in the header at offset 24 (as in Kalpanika/x3fuse-core
    /// `decode_camf`; the multiplication overflows in 32 bits before the modulo).
    fn parse_type2(section: &[u8]) -> Result<Self> {
        let mut key = u32_at(section, 24)?;
        let data = section.get(Self::HEADER_SIZE..).ok_or_else(|| Error::Corrupt("CAMF zu kurz".into()))?;
        let out: Vec<u8> = data
            .iter()
            .map(|&b| {
                key = key.wrapping_mul(1597).wrapping_add(51749) % 244944;
                let tmp = ((key as i64 * 301_593_171) >> 24) as u32;
                let a = (key << 8).wrapping_sub(tmp);
                b ^ (((a >> 1).wrapping_add(tmp)) >> 17) as u8
            })
            .collect();
        Self::parse_entries(&out)
    }

    /// CAMF type 5 (Quattro): the same Huffman table as type 4, but a simple
    /// 1-D predictor over bytes (value = previous value + difference, one byte each).
    /// Header: offset 12 decoded size, 16 start value; data: table (≤ 14 pairs),
    /// offset 28 length of the bit stream, bit stream from byte 32.
    fn parse_type5(section: &[u8]) -> Result<Self> {
        let decoded_size = u32_at(section, 12)? as usize;
        let seed = u32_at(section, 16)? as i32;
        let data = section.get(Self::HEADER_SIZE..).ok_or_else(|| Error::Corrupt("CAMF zu kurz".into()))?;
        if data.len() < 32 || decoded_size == 0 {
            return corrupt("CAMF zu kurz");
        }
        if decoded_size > MAX_DECODED {
            return corrupt(format!("CAMF mit unplausibler Größe {decoded_size}"));
        }
        let mut pairs = Vec::new();
        for pair in data[..28].chunks_exact(2) {
            if pair[0] == 0 {
                break;
            }
            pairs.push((pair[0], pair[1]));
        }
        let huff = TrueHuffman::new(&pairs)?;
        let len = (u32_at(data, 28)? as usize).min(data.len() - 32);
        let mut br = crate::bits::BitReader::new(&data[32..32 + len]);
        let mut out = vec![0u8; decoded_size];
        let mut acc = seed;
        for b in out.iter_mut() {
            acc = acc.wrapping_add(huff.decode_diff(&mut br));
            *b = acc as u8;
        }
        if br.overrun() {
            return corrupt("CAMF-Datenstrom endet vorzeitig");
        }
        Self::parse_entries(&out)
    }

    fn parse_entries(d: &[u8]) -> Result<Self> {
        let mut camf = Camf::default();
        let mut p = 0usize;
        while p + 20 <= d.len() {
            let id = &d[p..p + 4];
            if id != b"CMbT" && id != b"CMbP" && id != b"CMbM" {
                break;
            }
            let e = &d[p..];
            let size = u32_at(e, 8)? as usize;
            let name_off = u32_at(e, 12)? as usize;
            let value_off = u32_at(e, 16)? as usize;
            if size < 20 || p + size > d.len() {
                return corrupt("CAMF-Eintrag mit ungültiger Größe");
            }
            let e = &d[p..p + size];
            let name = cstr(e, name_off);
            let v = value_off;
            let entry = match id {
                b"CMbT" => {
                    let n = u32_at(e, v)? as usize;
                    let bytes = e.get(v + 4..v + 4 + n).unwrap_or(&[]);
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                    CamfEntry::Text(String::from_utf8_lossy(&bytes[..end]).into_owned())
                }
                b"CMbP" => {
                    let num = u32_at(e, v)? as usize;
                    let off = u32_at(e, v + 4)? as usize;
                    // every property needs 8 bytes in the entry
                    if num > e.len() / 8 {
                        return corrupt("CAMF-Eigenschaften mit ungültiger Anzahl");
                    }
                    let mut props = Vec::with_capacity(num);
                    for i in 0..num {
                        let n = off + u32_at(e, v + 8 + 8 * i)? as usize;
                        let val = off + u32_at(e, v + 12 + 8 * i)? as usize;
                        props.push((cstr(e, n), cstr(e, val)));
                    }
                    CamfEntry::Properties(props)
                }
                _ => CamfEntry::Matrix(Self::parse_matrix(e, v)?),
            };
            camf.order.push(name.clone());
            camf.entries.insert(name, entry);
            p += size;
        }
        Ok(camf)
    }

    fn parse_matrix(e: &[u8], v: usize) -> Result<CamfMatrix> {
        let element_type = u32_at(e, v)?;
        let ndim = u32_at(e, v + 4)? as usize;
        let data_off = u32_at(e, v + 8)? as usize;
        // every dimension needs 12 bytes in the entry
        if ndim > e.len() / 12 {
            return corrupt("CAMF-Matrix mit ungültiger Dimensionszahl");
        }
        let mut dims = Vec::with_capacity(ndim);
        let mut total = 1usize;
        for i in 0..ndim {
            let size = u32_at(e, v + 12 + 12 * i)? as usize;
            let name_off = u32_at(e, v + 16 + 12 * i)? as usize;
            dims.push((cstr(e, name_off), size));
            total = total.checked_mul(size).ok_or_else(|| Error::Corrupt("CAMF-Matrix zu groß".into()))?;
        }
        let raw = e.get(data_off..).unwrap_or(&[]);
        let need = |sz: usize| -> Result<&[u8]> {
            total.checked_mul(sz).and_then(|n| raw.get(..n))
                .ok_or_else(|| Error::Corrupt("CAMF-Matrix abgeschnitten".into()))
        };
        let data = match element_type {
            0 => MatrixData::Int(
                need(2)?.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as i32).collect(),
            ),
            1 | 2 => MatrixData::UInt(
                need(4)?.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect(),
            ),
            3 => MatrixData::Float(
                need(4)?
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes(c.try_into().unwrap()) as f64)
                    .collect(),
            ),
            5 => MatrixData::UInt(need(1)?.iter().map(|&b| b as u32).collect()),
            6 => MatrixData::UInt(
                need(2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect(),
            ),
            t => return Err(Error::Unsupported(format!("CAMF-Matrixtyp {t}"))),
        };
        Ok(CamfMatrix { dims, element_type, data })
    }

    pub fn matrix(&self, name: &str) -> Result<&CamfMatrix> {
        match self.entries.get(name) {
            Some(CamfEntry::Matrix(m)) => Ok(m),
            _ => Err(Error::MissingCamf(name.into())),
        }
    }

    pub fn properties(&self, name: &str) -> Result<&[(String, String)]> {
        match self.entries.get(name) {
            Some(CamfEntry::Properties(p)) => Ok(p),
            _ => Err(Error::MissingCamf(name.into())),
        }
    }

    pub fn property(&self, list: &str, key: &str) -> Option<&str> {
        self.properties(list)
            .ok()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}
