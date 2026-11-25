//! Unknown camera vs. damaged file. Runs only when `GRAINEO_TEST_UNKNOWN_DIR` names a folder with
//! `unknown-camera.*` (a supported RAW whose EXIF model was changed to one the decoder does not know) and
//! `truncated.*` (a supported RAW cut short); otherwise it is skipped (no sample is kept in the repository).

use std::path::PathBuf;

fn find(dir: &std::path::Path, stem: &str) -> PathBuf {
    std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).find(|p| p.file_stem().is_some_and(|s| s == stem)).unwrap_or_else(|| panic!("{stem}.* missing in {}", dir.display()))
}

#[test]
fn unknown_camera_and_damaged_file_are_told_apart() {
    let Some(dir) = std::env::var_os("GRAINEO_TEST_UNKNOWN_DIR").map(PathBuf::from) else {
        eprintln!("GRAINEO_TEST_UNKNOWN_DIR not set, skipped");
        return;
    };
    let unknown = raw::RawFile::open(find(&dir, "unknown-camera")).err().expect("unknown camera must not open");
    eprintln!("unknown camera: {unknown}");
    let check = raw::check_decodable(&find(&dir, "unknown-camera")).err().expect("unknown camera must not be decodable");
    eprintln!("check_decodable: {check}");
    let damaged = raw::RawFile::open(find(&dir, "truncated")).err().expect("truncated file must not open");
    eprintln!("truncated: {damaged}");
    assert!(raw::unknown_camera(&unknown.to_string()).is_some(), "{unknown}");
    assert!(raw::unknown_camera(&check.to_string()).is_some(), "{check}");
    assert!(raw::unknown_camera(&damaged.to_string()).is_none(), "{damaged}");
    assert!(damaged.to_string().contains(raw::DAMAGED), "{damaged}");
}
