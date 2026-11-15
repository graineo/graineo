//! Minimal OpenEXR writer: single part, scanlines, RGB as half or float, ZIP compression
//! (16 lines per block), `chromaticities` attribute. Written after the public OpenEXR file
//! layout description (openexr.com, "OpenEXR File Layout"); no OpenEXR code is used.

use std::io::{self, Write};

use rayon::prelude::*;

/// Sample type of the channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    Half,
    Float,
}

/// Primaries and white point as CIE xy: red, green, blue, white.
pub type Chromaticities = [[f32; 2]; 4];

/// Writes `rgb` (interleaved, `width` × `height`, linear, unclipped) as OpenEXR.
/// `attributes`: additional string attributes (e.g. `owner`, `capDate`, `comments`).
pub fn write(mut w: impl Write, width: usize, height: usize, rgb: &[f32], sample: Sample, chroma: Chromaticities, attributes: &[(&str, String)]) -> io::Result<()> {
    if width == 0 || height == 0 || rgb.len() != width * height * 3 || width > i32::MAX as usize || height > i32::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "EXR: ungültige Bildmaße"));
    }
    let mut h = Vec::new();
    h.extend_from_slice(&20000630u32.to_le_bytes());
    h.extend_from_slice(&2u32.to_le_bytes()); // version 2, single part scanline
    let ptype: i32 = match sample {
        Sample::Half => 1,
        Sample::Float => 2,
    };
    let mut ch = Vec::new();
    // channel list sorted by name
    for name in ["B", "G", "R"] {
        ch.extend_from_slice(name.as_bytes());
        ch.push(0);
        ch.extend_from_slice(&ptype.to_le_bytes());
        ch.extend_from_slice(&[0, 0, 0, 0]); // pLinear + reserved
        ch.extend_from_slice(&1i32.to_le_bytes());
        ch.extend_from_slice(&1i32.to_le_bytes());
    }
    ch.push(0);
    attr(&mut h, "channels", "chlist", &ch);
    let mut c = Vec::new();
    for v in chroma.iter().flatten() {
        c.extend_from_slice(&v.to_le_bytes());
    }
    attr(&mut h, "chromaticities", "chromaticities", &c);
    attr(&mut h, "compression", "compression", &[3]); // ZIP, 16 lines
    let mut b = Vec::new();
    for v in [0i32, 0, width as i32 - 1, height as i32 - 1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    attr(&mut h, "dataWindow", "box2i", &b);
    attr(&mut h, "displayWindow", "box2i", &b);
    attr(&mut h, "lineOrder", "lineOrder", &[0]);
    attr(&mut h, "pixelAspectRatio", "float", &1f32.to_le_bytes());
    let mut v2 = Vec::new();
    v2.extend_from_slice(&0f32.to_le_bytes());
    v2.extend_from_slice(&0f32.to_le_bytes());
    attr(&mut h, "screenWindowCenter", "v2f", &v2);
    attr(&mut h, "screenWindowWidth", "float", &1f32.to_le_bytes());
    for (name, value) in attributes {
        if !value.is_empty() {
            attr(&mut h, name, "string", value.as_bytes());
        }
    }
    h.push(0); // end of header

    const LINES: usize = 16;
    let blocks = height.div_ceil(LINES);
    let bytes = match sample {
        Sample::Half => 2,
        Sample::Float => 4,
    };
    let chunks: Vec<Vec<u8>> = (0..blocks)
        .into_par_iter()
        .map(|bi| {
            let y0 = bi * LINES;
            let y1 = (y0 + LINES).min(height);
            let mut raw = Vec::with_capacity((y1 - y0) * width * 3 * bytes);
            for y in y0..y1 {
                let row = &rgb[y * width * 3..(y + 1) * width * 3];
                for c in [2usize, 1, 0] {
                    for x in 0..width {
                        let v = row[x * 3 + c];
                        match sample {
                            Sample::Half => raw.extend_from_slice(&f32_to_half(v).to_le_bytes()),
                            Sample::Float => raw.extend_from_slice(&v.to_le_bytes()),
                        }
                    }
                }
            }
            let packed = zip_block(&raw);
            let data = if packed.len() < raw.len() { packed } else { raw };
            let mut chunk = Vec::with_capacity(data.len() + 8);
            chunk.extend_from_slice(&(y0 as i32).to_le_bytes());
            chunk.extend_from_slice(&(data.len() as i32).to_le_bytes());
            chunk.extend_from_slice(&data);
            chunk
        })
        .collect();
    let mut offset = (h.len() + blocks * 8) as u64;
    for c in &chunks {
        h.extend_from_slice(&offset.to_le_bytes());
        offset += c.len() as u64;
    }
    w.write_all(&h)?;
    for c in &chunks {
        w.write_all(c)?;
    }
    w.flush()
}

fn attr(h: &mut Vec<u8>, name: &str, ty: &str, value: &[u8]) {
    h.extend_from_slice(name.as_bytes());
    h.push(0);
    h.extend_from_slice(ty.as_bytes());
    h.push(0);
    h.extend_from_slice(&(value.len() as i32).to_le_bytes());
    h.extend_from_slice(value);
}

/// ZIP block of OpenEXR: bytes split into even/odd halves, difference predictor, zlib.
fn zip_block(raw: &[u8]) -> Vec<u8> {
    let n = raw.len();
    let mut t = vec![0u8; n];
    let half = n.div_ceil(2);
    for (i, &b) in raw.iter().enumerate() {
        if i % 2 == 0 {
            t[i / 2] = b;
        } else {
            t[half + i / 2] = b;
        }
    }
    let mut p = t.first().copied().unwrap_or(0) as i32;
    for v in t.iter_mut().skip(1) {
        let d = *v as i32 - p + (128 + 256);
        p = *v as i32;
        *v = d as u8;
    }
    let mut e = flate2::write::ZlibEncoder::new(Vec::with_capacity(n / 2), flate2::Compression::new(6));
    let _ = e.write_all(&t);
    e.finish().unwrap_or_default()
}

/// IEEE 754 binary16 with round-to-nearest-even; beyond the half range ±inf, NaN stays NaN.
pub fn f32_to_half(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let man = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let mut h = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        if rem > halfway || (rem == halfway && (h & 1) == 1) {
            h += 1;
        }
        return sign | h as u16;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && (h & 1) == 1) {
        h += 1; // may carry into the exponent (correct, up to inf)
    }
    sign | h as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn half_conversion() {
        assert_eq!(f32_to_half(0.0), 0);
        assert_eq!(f32_to_half(1.0), 0x3c00);
        assert_eq!(f32_to_half(-2.0), 0xc000);
        assert_eq!(f32_to_half(65504.0), 0x7bff);
        assert_eq!(f32_to_half(1e6), 0x7c00);
        assert_eq!(f32_to_half(0.18), 0x31c3);
        assert_eq!(f32_to_half(5.960_464_5e-8), 1); // smallest subnormal
    }
    /// Reads back what [`write`] wrote (offset table, zlib, predictor, byte halves).
    fn read_back(out: &[u8], w: usize, h: usize, bytes: usize) -> Vec<Vec<u8>> {
        use std::io::Read;
        let end = out.windows(2).position(|p| p == b"\0\0").map(|_| ()).is_some();
        assert!(end);
        let blocks = h.div_ceil(16);
        // the offset table directly precedes the first chunk: find it from the first offset
        let first_chunk = (0..out.len() - 8).find(|&i| u64::from_le_bytes(out[i..i + 8].try_into().unwrap()) as usize == i + blocks * 8).unwrap();
        let mut rows = Vec::new();
        for bi in 0..blocks {
            let off = u64::from_le_bytes(out[first_chunk + 8 * bi..first_chunk + 8 * bi + 8].try_into().unwrap()) as usize;
            assert_eq!(i32::from_le_bytes(out[off..off + 4].try_into().unwrap()) as usize, bi * 16);
            let n = i32::from_le_bytes(out[off + 4..off + 8].try_into().unwrap()) as usize;
            let lines = (h - bi * 16).min(16);
            let raw_len = lines * w * 3 * bytes;
            let data = &out[off + 8..off + 8 + n];
            let raw = if n == raw_len {
                data.to_vec()
            } else {
                let mut t = Vec::new();
                flate2::read::ZlibDecoder::new(data).read_to_end(&mut t).unwrap();
                for i in 1..t.len() {
                    t[i] = (t[i - 1] as i32 + t[i] as i32 - 128) as u8;
                }
                let half = t.len().div_ceil(2);
                (0..t.len()).map(|i| if i % 2 == 0 { t[i / 2] } else { t[half + i / 2] }).collect()
            };
            for l in 0..lines {
                rows.push(raw[l * w * 3 * bytes..(l + 1) * w * 3 * bytes].to_vec());
            }
        }
        rows
    }

    #[test]
    fn roundtrip_half_and_float() {
        let (w, h) = (7, 37);
        let rgb: Vec<f32> = (0..w * h * 3).map(|i| (i as f32 * 0.731).sin() * 3.0).collect();
        for (sample, bytes) in [(Sample::Half, 2), (Sample::Float, 4)] {
            let mut out = Vec::new();
            write(&mut out, w, h, &rgb, sample, [[0.708, 0.292], [0.17, 0.797], [0.131, 0.046], [0.3127, 0.329]], &[("owner", "x".into())]).unwrap();
            assert_eq!(&out[..4], &20000630u32.to_le_bytes());
            let rows = read_back(&out, w, h, bytes);
            assert_eq!(rows.len(), h);
            for (y, row) in rows.iter().enumerate() {
                for (ci, c) in [2usize, 1, 0].iter().enumerate() {
                    for x in 0..w {
                        let at = (ci * w + x) * bytes;
                        let v = rgb[(y * w + x) * 3 + c];
                        match sample {
                            Sample::Half => assert_eq!(u16::from_le_bytes([row[at], row[at + 1]]), f32_to_half(v)),
                            Sample::Float => assert_eq!(f32::from_le_bytes(row[at..at + 4].try_into().unwrap()), v),
                        }
                    }
                }
            }
        }
    }
}
