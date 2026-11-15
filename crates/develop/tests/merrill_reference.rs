//! Reference fingerprints of the Merrill base (`spp::working`, as `graineo-dev spphash`) for two test images.
//!
//! Runs only when `GRAINEO_TEST_MERRILL_DIR` names a folder with `DP2M5418.X3F` and `DP2M5412.X3F`
//! (otherwise skipped). The same values are checked by `scripts/check.sh` (see `crates/graineo-dev/reference/`).

/// Fingerprints of the Merrill base.
const EXPECTED: [(&str, &str); 2] = [("DP2M5418", "eec84657f928a2db"), ("DP2M5412", "e082cfa9fd4fea0d")];

fn fingerprint(path: &std::path::Path) -> String {
    let c = develop::spp::working(path, None).expect("Merrill base");
    let mut h: u64 = 0xcbf29ce484222325;
    for v in &c.data {
        for b in v.to_bits().to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016x}")
}

#[test]
fn merrill_base_matches_reference() {
    let Some(dir) = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(std::path::PathBuf::from) else {
        eprintln!("GRAINEO_TEST_MERRILL_DIR not set, skipped");
        return;
    };
    for (name, hash) in EXPECTED {
        assert_eq!(fingerprint(&dir.join(format!("{name}.X3F"))), hash, "{name}");
    }
}
