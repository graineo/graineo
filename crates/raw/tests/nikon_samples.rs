//! Nikon NEF/NRW sample files (e.g. the CC0 files of raw.pixls.us). Runs only when `GRAINEO_TEST_NEF_DIR` names a
//! folder with such files; otherwise it is skipped (no sample is kept in the repository: they are 10–90 MB each).
//!
//! Every file either decodes to a Bayer image with a camera matrix, or (Nikon "High Efficiency", which rawler does
//! not decode yet) is reported as not supported with the plain text, never as a damaged file.

use std::path::PathBuf;

#[test]
fn nikon_samples_decode_or_say_why_not() {
    let Some(dir) = std::env::var_os("GRAINEO_TEST_NEF_DIR").map(PathBuf::from) else {
        eprintln!("GRAINEO_TEST_NEF_DIR not set, skipped");
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("GRAINEO_TEST_NEF_DIR not readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("nef") || e.eq_ignore_ascii_case("nrw")))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no NEF/NRW in {}", dir.display());
    let (mut decoded, mut he) = (0, 0);
    for f in &files {
        match raw::RawFile::open(f) {
            Ok(r) => {
                assert_eq!(r.info.format, "NEF", "{}", f.display());
                assert_eq!(r.info.sensor, raw::Sensor::Bayer, "{}", f.display());
                assert!(r.info.matrix.is_some(), "no camera matrix: {}", f.display());
                decoded += 1;
            }
            Err(raw::Error::Unsupported(s)) if s == raw::NIKON_HE => he += 1,
            Err(e) => panic!("{}: {e}", f.display()),
        }
    }
    eprintln!("{decoded} decoded, {he} High Efficiency (not supported)");
}
