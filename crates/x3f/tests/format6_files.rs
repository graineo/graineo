//! Format-6 decoder (SD9/SD10/SD14) against checksums of real files: the decoded layers must stay bit-identical.
//!
//! The files are not in the repository: `GRAINEO_TEST_CLASSIC_DIR` names a folder (or several, separated by
//! ":") holding them; without the variable, or for a missing file, the check is skipped.

use std::path::PathBuf;

/// File name and FNV-1a checksum over the three decoded layers (layer by layer, values little-endian).
const EXPECTED: [(&str, u64); 3] = [
    ("RAW_SIGMA_SD9_SRGB.X3F", 0xa687_9fc1_523d_5705),
    ("RAW_SIGMA_SD10.X3F", 0x69f1_dfd5_dc2f_c5f5),
    ("20120818-SDIM4579.X3F", 0xd992_489f_1180_f0ff),
];

fn fnv(planes: &[Vec<u16>; 3]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in planes {
        for v in p {
            for b in v.to_le_bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    h
}

fn find(name: &str) -> Option<PathBuf> {
    let dirs = std::env::var("GRAINEO_TEST_CLASSIC_DIR").ok()?;
    dirs.split(':').map(|d| PathBuf::from(d).join(name)).find(|p| p.is_file())
}

#[test]
fn format6_files_decode_bit_identically() {
    let mut checked = 0;
    for (name, expected) in EXPECTED {
        let Some(path) = find(name) else {
            eprintln!("skipped (not found, set GRAINEO_TEST_CLASSIC_DIR): {name}");
            continue;
        };
        let file = x3f::X3fFile::open(&path).unwrap();
        // median of five runs (the first one warms up)
        let mut times = Vec::new();
        let mut raw = None;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            raw = Some(file.decode_raw_format6().unwrap());
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        let raw = raw.unwrap();
        eprintln!("{name}: {} × {} decoded in {:.1} ms (median of 5)", raw.columns, raw.rows, times[2]);
        assert_eq!(fnv(&raw.planes), expected, "{name}: decoded layers changed");
        checked += 1;
    }
    eprintln!("{checked} of {} files checked", EXPECTED.len());
}
