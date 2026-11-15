//! 16-bit PNG (RGB, ICC profile in iCCP, Sub filter, zlib). Own minimal writer after the PNG
//! specification; no extra dependency.

use std::io::{self, Write};

use rayon::prelude::*;

use crate::Rgb16;

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    out.extend_from_slice(&h.finalize().to_be_bytes());
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::with_capacity(data.len() / 2), flate2::Compression::new(6));
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// Writes `img` as 16-bit RGB PNG; `icc` (name, profile) goes into an iCCP chunk.
pub fn write(img: &Rgb16, path: &std::path::Path, icc: Option<(&str, &[u8])>) -> io::Result<()> {
    let (w, h) = (u32::try_from(img.width).ok().filter(|&n| n > 0), u32::try_from(img.height).ok().filter(|&n| n > 0));
    let (Some(w), Some(h)) = (w, h) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "PNG: ungültige Bildmaße"));
    };
    if img.data.len() != img.width * img.height * 3 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "PNG: Bilddaten passen nicht zu den Maßen"));
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[16, 2, 0, 0, 0]); // 16 bit, truecolour, deflate, adaptive filter, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    if let Some((name, profile)) = icc {
        let mut d = name.as_bytes().iter().copied().filter(|b| (32..127).contains(b)).take(79).collect::<Vec<u8>>();
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&zlib(profile));
        chunk(&mut out, b"iCCP", &d);
    }
    // filtered scanlines: filter 1 (Sub), 6 bytes per pixel, big-endian samples
    let row = img.width * 6;
    let mut raw = vec![0u8; (row + 1) * img.height];
    raw.par_chunks_mut(row + 1).zip(img.data.par_chunks(img.width * 3)).for_each(|(o, src)| {
        o[0] = 1;
        let line = &mut o[1..];
        for (i, v) in src.iter().enumerate() {
            line[2 * i..2 * i + 2].copy_from_slice(&v.to_be_bytes());
        }
        for i in (6..row).rev() {
            line[i] = line[i].wrapping_sub(line[i - 6]);
        }
    });
    // compress in parallel pieces of a single zlib stream would need care; one stream is simple and fast enough
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    let mut f = io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(&out)?;
    f.flush()
}
