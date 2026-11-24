//! Packs the spektrafilm film data (profiles, luts, filters) into one deflate stream that `film.rs` embeds as the
//! last fallback when no data folder is found next to the program. Reproducible: files in sorted order, no
//! timestamps, fixed compression level; no network.
use std::io::Write;
use std::path::{Path, PathBuf};

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(root, &p, out);
        } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.')) {
            out.push(p);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let data = manifest.join("../../third_party/spektrafilm-rs/data");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("film_data.bin");
    let mut files = Vec::new();
    for sub in ["profiles", "luts", "filters"] {
        println!("cargo:rerun-if-changed={}", data.join(sub).display());
        collect(&data, &data.join(sub), &mut files);
    }
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
    // Entry: u32 path length, path (UTF-8, '/'), u64 data length, data
    for f in &files {
        let rel = f.strip_prefix(&data).unwrap().components().map(|c| c.as_os_str().to_str().unwrap()).collect::<Vec<_>>().join("/");
        println!("cargo:rerun-if-changed={}", f.display());
        let bytes = std::fs::read(f).unwrap();
        enc.write_all(&(rel.len() as u32).to_le_bytes()).unwrap();
        enc.write_all(rel.as_bytes()).unwrap();
        enc.write_all(&(bytes.len() as u64).to_le_bytes()).unwrap();
        enc.write_all(&bytes).unwrap();
    }
    std::fs::write(&out, enc.finish().unwrap()).unwrap();
}
