//! Minimal TIFF writer (little endian) for TIFF and DNG: one IFD with one strip ([`write`]) or
//! several IFDs with compressed strips ([`write_ifd`]).

use std::io::{self, Write};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub enum Value {
    Byte(Vec<u8>),
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<(u32, u32)>),
    SRational(Vec<(i32, i32)>),
    Undefined(Vec<u8>),
    Float(Vec<f32>),
}

impl Value {
    fn type_and_count(&self) -> (u16, u32) {
        match self {
            Value::Byte(v) => (1, v.len() as u32),
            Value::Ascii(s) => (2, s.len() as u32 + 1),
            Value::Short(v) => (3, v.len() as u32),
            Value::Long(v) => (4, v.len() as u32),
            Value::Rational(v) => (5, v.len() as u32),
            Value::SRational(v) => (10, v.len() as u32),
            Value::Undefined(v) => (7, v.len() as u32),
            Value::Float(v) => (11, v.len() as u32),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            Value::Byte(v) | Value::Undefined(v) => b.extend_from_slice(v),
            Value::Ascii(s) => {
                b.extend_from_slice(s.as_bytes());
                b.push(0);
            }
            Value::Short(v) => v.iter().for_each(|x| b.extend_from_slice(&x.to_le_bytes())),
            Value::Long(v) => v.iter().for_each(|x| b.extend_from_slice(&x.to_le_bytes())),
            Value::Float(v) => v.iter().for_each(|x| b.extend_from_slice(&x.to_le_bytes())),
            Value::Rational(v) => v.iter().for_each(|(n, d)| {
                b.extend_from_slice(&n.to_le_bytes());
                b.extend_from_slice(&d.to_le_bytes());
            }),
            Value::SRational(v) => v.iter().for_each(|(n, d)| {
                b.extend_from_slice(&n.to_le_bytes());
                b.extend_from_slice(&d.to_le_bytes());
            }),
        }
        b
    }
}

/// Floating-point number as a signed fraction with denominator 10000.
pub fn srat(x: f64) -> (i32, i32) {
    ((x * 10000.0).round() as i32, 10000)
}

pub fn rat(x: f64) -> (u32, u32) {
    ((x.max(0.0) * 10000.0).round() as u32, 10000)
}

pub const STRIP_OFFSETS: u16 = 273;
pub const STRIP_BYTE_COUNTS: u16 = 279;

/// Writes a TIFF file. `tags` must not contain StripOffsets/StripByteCounts.
pub fn write(mut w: impl Write, mut tags: Vec<(u16, Value)>, image: &[u8]) -> io::Result<()> {
    tags.push((STRIP_OFFSETS, Value::Long(vec![0])));
    tags.push((STRIP_BYTE_COUNTS, Value::Long(vec![image.len() as u32])));
    tags.sort_by_key(|t| t.0);

    let ifd_offset = 8u32;
    let ifd_size = 2 + tags.len() as u32 * 12 + 4;
    let mut extra: Vec<u8> = Vec::new();
    let extra_start = ifd_offset + ifd_size;
    // Put values that do not fit into 4 bytes after the IFD (keeping word alignment).
    let mut entries: Vec<(u16, u16, u32, [u8; 4])> = Vec::new();
    let mut strip_entry = None;
    for (i, (tag, value)) in tags.iter().enumerate() {
        let (ty, count) = value.type_and_count();
        let data = value.bytes();
        let mut field = [0u8; 4];
        if data.len() <= 4 {
            field[..data.len()].copy_from_slice(&data);
        } else {
            let off = extra_start + extra.len() as u32;
            field = off.to_le_bytes();
            extra.extend_from_slice(&data);
            if extra.len() % 2 == 1 {
                extra.push(0);
            }
        }
        if *tag == STRIP_OFFSETS {
            strip_entry = Some(i);
        }
        entries.push((*tag, ty, count, field));
    }
    let mut image_offset = extra_start + extra.len() as u32;
    image_offset += (4 - image_offset % 4) % 4;
    if let Some(i) = strip_entry {
        entries[i].3 = image_offset.to_le_bytes();
    }

    let mut out = Vec::with_capacity(image_offset as usize);
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&ifd_offset.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, ty, count, field) in &entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(field);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&extra);
    out.resize(image_offset as usize, 0);
    w.write_all(&out)?;
    w.write_all(image)?;
    w.flush()
}

// ---- Several IFDs, several strips, compression -------------------------------------------

/// One image file directory with its image strips, sub-IFDs (tag 330, e.g. DNG preview) and
/// EXIF IFD (tag 34665). StripOffsets/StripByteCounts/SubIFDs/ExifIFD are added by [`write_ifd`].
#[derive(Default)]
pub struct Ifd {
    pub tags: Vec<(u16, Value)>,
    pub strips: Vec<Vec<u8>>,
    pub sub_ifds: Vec<Ifd>,
    pub exif: Option<Box<Ifd>>,
}

const SUB_IFDS: u16 = 330;
const EXIF_IFD: u16 = 34665;

/// Flattened IFD with the placeholder tags in place.
struct Flat {
    tags: Vec<(u16, Value)>,
    strips: Vec<Vec<u8>>,
    /// indices of the sub-IFDs / EXIF IFD in the flat list
    subs: Vec<usize>,
    exif: Option<usize>,
}

fn flatten(ifd: Ifd, out: &mut Vec<Flat>) -> usize {
    let me = out.len();
    out.push(Flat { tags: ifd.tags, strips: ifd.strips, subs: Vec::new(), exif: None });
    if let Some(e) = ifd.exif {
        let i = flatten(*e, out);
        out[me].exif = Some(i);
    }
    for s in ifd.sub_ifds {
        let i = flatten(s, out);
        out[me].subs.push(i);
    }
    me
}

fn entries_size(tags: &[(u16, Value)]) -> (u32, u32) {
    let mut extra = 0u32;
    for (_, v) in tags {
        let n = v.bytes().len() as u32;
        if n > 4 {
            extra += n + (n & 1);
        }
    }
    (2 + tags.len() as u32 * 12 + 4, extra)
}

/// Writes a TIFF with IFD0 = `root` (and its sub-IFDs/EXIF IFD). All image strips follow the
/// directories. Fails above 4 GiB (classic TIFF).
pub fn write_ifd(mut w: impl Write, root: Ifd) -> io::Result<()> {
    let mut flat = Vec::new();
    flatten(root, &mut flat);
    // placeholders
    for f in flat.iter_mut() {
        if !f.strips.is_empty() {
            let n = f.strips.len();
            f.tags.retain(|t| t.0 != STRIP_OFFSETS && t.0 != STRIP_BYTE_COUNTS);
            f.tags.push((STRIP_OFFSETS, Value::Long(vec![0; n])));
            f.tags.push((STRIP_BYTE_COUNTS, Value::Long(f.strips.iter().map(|s| s.len() as u32).collect())));
        }
        if !f.subs.is_empty() {
            f.tags.push((SUB_IFDS, Value::Long(vec![0; f.subs.len()])));
        }
        if f.exif.is_some() {
            f.tags.push((EXIF_IFD, Value::Long(vec![0])));
        }
        f.tags.sort_by_key(|t| t.0);
    }
    // layout: directories (each followed by its out-of-line values), then the strips
    let mut ifd_off = Vec::with_capacity(flat.len());
    let mut pos = 8u64;
    for f in &flat {
        pos += pos & 1;
        ifd_off.push(pos);
        let (dir, extra) = entries_size(&f.tags);
        pos += (dir + extra) as u64;
    }
    pos += (4 - pos % 4) % 4;
    let mut strip_off: Vec<Vec<u64>> = Vec::with_capacity(flat.len());
    for f in &flat {
        let mut v = Vec::with_capacity(f.strips.len());
        for s in &f.strips {
            v.push(pos);
            pos += s.len() as u64;
            pos += pos & 1;
        }
        strip_off.push(v);
    }
    if pos > u32::MAX as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Datei größer als 4 GB (TIFF-Grenze)"));
    }
    // fill the placeholders
    let fills: Vec<(Vec<u32>, Vec<u32>, Option<u32>)> = flat
        .iter()
        .enumerate()
        .map(|(i, f)| (strip_off[i].iter().map(|&o| o as u32).collect(), f.subs.iter().map(|&s| ifd_off[s] as u32).collect(), f.exif.map(|e| ifd_off[e] as u32)))
        .collect();
    for (f, (so, subs, exif)) in flat.iter_mut().zip(fills) {
        for (tag, v) in f.tags.iter_mut() {
            match *tag {
                STRIP_OFFSETS if !f.strips.is_empty() => *v = Value::Long(so.clone()),
                SUB_IFDS if !f.subs.is_empty() => *v = Value::Long(subs.clone()),
                EXIF_IFD if exif.is_some() => *v = Value::Long(vec![exif.unwrap_or(0)]),
                _ => {}
            }
        }
    }
    // serialise the directories
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&(ifd_off[0] as u32).to_le_bytes());
    for (i, f) in flat.iter().enumerate() {
        out.resize(ifd_off[i] as usize, 0);
        let (dir, _) = entries_size(&f.tags);
        let mut extra_pos = ifd_off[i] as u32 + dir;
        let mut extra = Vec::new();
        out.extend_from_slice(&(f.tags.len() as u16).to_le_bytes());
        for (tag, value) in &f.tags {
            let (ty, count) = value.type_and_count();
            let data = value.bytes();
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&ty.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            if data.len() <= 4 {
                let mut field = [0u8; 4];
                field[..data.len()].copy_from_slice(&data);
                out.extend_from_slice(&field);
            } else {
                out.extend_from_slice(&extra_pos.to_le_bytes());
                extra.extend_from_slice(&data);
                if data.len() % 2 == 1 {
                    extra.push(0);
                }
                extra_pos = ifd_off[i] as u32 + dir + extra.len() as u32;
            }
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // next IFD: none (sub-IFDs are referenced via tag 330)
        out.extend_from_slice(&extra);
    }
    w.write_all(&out)?;
    let mut written = out.len() as u64;
    for (i, f) in flat.iter().enumerate() {
        for (s, &o) in f.strips.iter().zip(&strip_off[i]) {
            if written < o {
                w.write_all(&vec![0u8; (o - written) as usize])?;
                written = o;
            }
            w.write_all(s)?;
            written += s.len() as u64;
        }
    }
    w.flush()
}

/// Lossless compression of TIFF/DNG strips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compression {
    #[default]
    None,
    /// Deflate/ZIP (8) with predictor
    Deflate,
    /// LZW (5) with predictor
    Lzw,
}

impl Compression {
    pub fn tag(self) -> u16 {
        match self {
            Compression::None => 1,
            Compression::Deflate => 8,
            Compression::Lzw => 5,
        }
    }
}

/// Kind of the samples, for the predictor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Samples {
    U16,
    F16,
    F32,
}

/// Splits `data` (little-endian rows of `width` × `spp` samples) into strips of `rows` rows,
/// compressed with `c` (Deflate/LZW with predictor 2 for integers, 3 for floating point).
/// Returns the strips and the tags Compression, Predictor, RowsPerStrip.
pub fn strips(data: &[u8], width: usize, spp: usize, samples: Samples, c: Compression) -> (Vec<Vec<u8>>, Vec<(u16, Value)>) {
    let bps = match samples {
        Samples::U16 | Samples::F16 => 2,
        Samples::F32 => 4,
    };
    let row_bytes = width * spp * bps;
    let height = if row_bytes == 0 { 0 } else { data.len() / row_bytes };
    // ≈ 256 KiB per strip
    let rows = if c == Compression::None { height.max(1) } else { (256 * 1024 / row_bytes.max(1)).clamp(1, height.max(1)) };
    let mut tags = vec![(259, Value::Short(vec![c.tag()])), (278, Value::Long(vec![rows as u32]))];
    if c == Compression::None {
        return (vec![data.to_vec()], tags);
    }
    tags.push((317, Value::Short(vec![if samples == Samples::U16 { 2 } else { 3 }]))); // Predictor
    let out = data
        .par_chunks(rows * row_bytes)
        .map(|chunk| {
            let mut buf = chunk.to_vec();
            for row in buf.chunks_mut(row_bytes) {
                match samples {
                    Samples::U16 => predict_u16(row, spp),
                    Samples::F16 => predict_float(row, spp, 2),
                    Samples::F32 => predict_float(row, spp, 4),
                }
            }
            match c {
                Compression::Deflate => {
                    let mut e = flate2::write::ZlibEncoder::new(Vec::with_capacity(buf.len() / 2), flate2::Compression::new(6));
                    let _ = e.write_all(&buf);
                    e.finish().unwrap_or_default()
                }
                Compression::Lzw => lzw(&buf),
                Compression::None => buf,
            }
        })
        .collect();
    (out, tags)
}

/// Horizontal differencing (predictor 2) on little-endian 16-bit samples.
fn predict_u16(row: &mut [u8], spp: usize) {
    let n = row.len() / 2;
    for i in (spp..n).rev() {
        let a = u16::from_le_bytes([row[2 * i], row[2 * i + 1]]);
        let b = u16::from_le_bytes([row[2 * (i - spp)], row[2 * (i - spp) + 1]]);
        row[2 * i..2 * i + 2].copy_from_slice(&a.wrapping_sub(b).to_le_bytes());
    }
}

/// Floating-point predictor (3, Adobe TIFF Technical Note 3): bytes of the row regrouped by
/// significance (most significant first), then byte differences with stride `spp`.
fn predict_float(row: &mut [u8], spp: usize, bytes: usize) {
    let wc = row.len() / bytes;
    let tmp = row.to_vec();
    for i in 0..wc {
        for b in 0..bytes {
            row[(bytes - 1 - b) * wc + i] = tmp[bytes * i + b];
        }
    }
    for i in (spp..row.len()).rev() {
        row[i] = row[i].wrapping_sub(row[i - spp]);
    }
}

/// TIFF LZW (MSB-first codes, early change, Clear 256 / EOI 257).
fn lzw(data: &[u8]) -> Vec<u8> {
    use std::collections::HashMap;
    let mut out = Vec::with_capacity(data.len() / 2);
    let (mut acc, mut nbits) = (0u32, 0u32);
    let mut put = |code: u32, width: u32, out: &mut Vec<u8>| {
        acc = (acc << width) | code;
        nbits += width;
        while nbits >= 8 {
            out.push((acc >> (nbits - 8)) as u8);
            nbits -= 8;
            acc &= (1 << nbits) - 1;
        }
    };
    let mut dict: HashMap<(u32, u8), u32> = HashMap::new();
    let mut next = 258u32;
    let mut width = 9u32;
    put(256, width, &mut out);
    let mut cur: Option<u32> = None;
    for &b in data {
        match cur {
            None => cur = Some(b as u32),
            Some(c) => {
                if let Some(&code) = dict.get(&(c, b)) {
                    cur = Some(code);
                } else {
                    put(c, width, &mut out);
                    dict.insert((c, b), next);
                    next += 1;
                    // early change: the reader widens one code earlier
                    if next + 1 > (1 << width) && width < 12 {
                        width += 1;
                    }
                    if next == 4094 {
                        put(256, width, &mut out);
                        dict.clear();
                        next = 258;
                        width = 9;
                    }
                    cur = Some(b as u32);
                }
            }
        }
    }
    if let Some(c) = cur {
        put(c, width, &mut out);
        next += 1;
        if next + 1 > (1 << width) && width < 12 {
            width += 1;
        }
    }
    put(257, width, &mut out);
    if nbits > 0 {
        out.push((acc << (8 - nbits)) as u8);
    }
    out
}
