//! Reference images for comparing against an earlier version.
//!
//! This module only uses interfaces that also exist in version `v0.86.2` and can be dropped in there unchanged:
//! `graineo-dev perf-reference <output> <files …>` writes, per file and case, the output stage
//! (preview 2400 px, 16 bit, raw) and the checksum (SHA-256) of the output stage and of the full resolution.
//! `graineo-dev perf baseline-compare <reference> <output> <files …>` computes the same set with the
//! current version and compares (see `perf.rs`).

use std::path::Path;

use anyhow::{Context, Result};
use develop::film::{self, FilmSettings};
use develop::session::Preview;
use develop::{Rgb16, Session, Settings};
use serde_json::{json, Value};

pub const PREVIEW_EDGE: usize = 2400;

/// Cases as in `graineo-dev perf levels` (without the duplicate crop case).
pub fn cases(sigma: bool) -> Vec<(&'static str, Settings)> {
    let trix = if film::stock("kodak_trix_400").is_some() {
        "kodak_trix_400".to_string()
    } else {
        film::films().iter().find(|f| f.category == film::Category::BlackWhite).map(|f| f.id.clone()).unwrap_or_default()
    };
    let color = film::films().iter().find(|f| f.category == film::Category::ColorNegative).map(|f| f.id.clone()).unwrap_or_default();
    let on = |stock: &str| FilmSettings { active: true, stock: stock.to_string(), grain: 1.0, ..FilmSettings::default() };
    let base = Settings::default();
    let sharp = if sigma { Settings { sigma_sharpness: 1.0, ..base.clone() } } else { Settings { sharpen: 1.0, ..base.clone() } };
    vec![
        ("standard", base.clone()),
        ("film", Settings { film: on(&trix), ..base.clone() }),
        ("film_color", Settings { film: on(&color), ..base.clone() }),
        ("crop", Settings { crop: [0.5, 0.5, 0.4, 0.4], ..base }),
        ("sharpness", sharp),
    ]
}

/// One computed image: output stage (kept raw) and full resolution (checksum only).
pub struct Entry {
    pub file: String,
    pub case: &'static str,
    pub level: Rgb16,
    pub level_sha: String,
    pub full_width: usize,
    pub full_height: usize,
    pub full_sha: String,
}

pub fn name_of(file: &str) -> String {
    Path::new(file).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// Computes all files and cases; `cb` receives each entry immediately (memory stays small).
pub fn render_set(files: &[&str], mut cb: impl FnMut(Entry) -> Result<()>) -> Result<()> {
    for file in files {
        let name = name_of(file);
        let sigma = file.to_ascii_lowercase().ends_with(".x3f");
        if sigma {
            let mut session = Session::open(file).map_err(|e| anyhow::anyhow!("{e}"))?;
            session.ensure_spp(&Settings::default()).map_err(anyhow::Error::msg)?;
            let (base, matrix) = (session.spp_base().context("SPP base")?, session.spp_matrix().context("matrix")?);
            let prev = Preview::from_spp(&base, matrix, PREVIEW_EDGE);
            let cal = session.cal.clone();
            for (case, s) in cases(true) {
                let level = develop::geometry::apply(prev.render(&cal, &s), &s, cal.orientation);
                let full = session.render_full(&s);
                cb(Entry {
                    file: name.clone(), case, level_sha: sha256_u16(&level.data), level,
                    full_width: full.width, full_height: full.height, full_sha: sha256_u16(&full.data),
                })?;
            }
        } else {
            let doc = rawdev::RawDoc::open(Path::new(file), &|| true).map_err(anyhow::Error::msg)?.context("cancelled")?;
            for (case, s) in cases(false) {
                let level = develop::geometry::apply(doc.render_preview(&s), &s, doc.orientation);
                let full = doc.render_full(&s).map_err(anyhow::Error::msg)?;
                cb(Entry {
                    file: name.clone(), case, level_sha: sha256_u16(&level.data), level,
                    full_width: full.width, full_height: full.height, full_sha: sha256_u16(&full.data),
                })?;
            }
        }
    }
    Ok(())
}

/// File name of the output stage in the reference folder.
pub fn level_file(dir: &Path, file: &str, case: &str) -> std::path::PathBuf {
    dir.join(format!("{file}__{case}__2400.rgb16"))
}

/// Raw data: 8-byte header (width, height as u32 little-endian), then u16 little-endian.
pub fn write_level(path: &Path, img: &Rgb16) -> Result<()> {
    let mut buf = Vec::with_capacity(8 + img.data.len() * 2);
    buf.extend_from_slice(&(img.width as u32).to_le_bytes());
    buf.extend_from_slice(&(img.height as u32).to_le_bytes());
    for v in &img.data {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(path, buf)?;
    Ok(())
}

pub fn read_level(path: &Path) -> Result<Rgb16> {
    let buf = std::fs::read(path)?;
    anyhow::ensure!(buf.len() >= 8, "file too short: {}", path.display());
    let width = u32::from_le_bytes(buf[0..4].try_into()?) as usize;
    let height = u32::from_le_bytes(buf[4..8].try_into()?) as usize;
    let data: Vec<u16> = buf[8..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    anyhow::ensure!(data.len() == width * height * 3, "size does not match: {}", path.display());
    Ok(Rgb16 { width, height, data })
}

/// `perf-reference <output> <files …>`
pub fn reference(args: &[String]) -> Result<()> {
    anyhow::ensure!(args.len() >= 2, "usage: graineo-dev perf-reference <output_folder> <files …>");
    let out = Path::new(&args[0]);
    std::fs::create_dir_all(out)?;
    let files: Vec<&str> = args[1..].iter().map(String::as_str).collect();
    let mut rows: Vec<Value> = Vec::new();
    render_set(&files, |e| {
        write_level(&level_file(out, &e.file, e.case), &e.level)?;
        eprintln!("{:<28} {:<11} stage {}  full {}", e.file, e.case, &e.level_sha[..12], &e.full_sha[..12]);
        rows.push(json!({
            "file": e.file, "case": e.case, "level_width": e.level.width, "level_height": e.level.height, "level_sha256": e.level_sha,
            "full_width": e.full_width, "full_height": e.full_height, "full_sha256": e.full_sha,
        }));
        Ok(())
    })?;
    let doc = json!({"command": "perf-reference", "entries": rows.len(), "rows": rows});
    std::fs::write(out.join("reference.json"), serde_json::to_string_pretty(&doc)?)?;
    eprintln!("{} entries → {}", rows.len(), out.join("reference.json").display());
    Ok(())
}

// ---------------------------------------------------------------- SHA-256

pub fn sha256_u16(data: &[u16]) -> String {
    let mut bytes = Vec::with_capacity(data.len() * 2);
    for v in data {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    sha256(&bytes)
}

pub fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
        0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
        0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
        0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
        0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg_tail = data[data.len() / 64 * 64..].to_vec();
    msg_tail.push(0x80);
    while msg_tail.len() % 64 != 56 {
        msg_tail.push(0);
    }
    msg_tail.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    let mut block = |chunk: &[u8]| {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    };
    for chunk in data[..data.len() / 64 * 64].chunks_exact(64) {
        block(chunk);
    }
    for chunk in msg_tail.chunks_exact(64) {
        block(chunk);
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::sha256;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let long = vec![b'a'; 1000];
        assert_eq!(sha256(&long).len(), 64);
    }
}
