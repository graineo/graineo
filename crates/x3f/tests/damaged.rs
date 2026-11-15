//! Damaged and reduced-resolution X3F files.
//!
//! - `GRAINEO_TEST_DAMAGED_X3F_DIR`: folder with copies of `_SDI2984.X3F` (top plane partly overwritten
//!   with foreign data), `_SDI3314.X3F` (many blocks zeroed) and `_SDI3742.X3F` (intact, SD1 Merrill
//!   in the reduced size LOW). Without it these tests are skipped.
//! - `GRAINEO_TEST_X3F`: an intact Merrill file for the tests with artificial damage (default:
//!   `DP2M5418.X3F` in `GRAINEO_TEST_MERRILL_DIR`, skipped if missing).

use std::path::PathBuf;

use x3f::{Error, Integrity, X3fFile};

fn damaged(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("GRAINEO_TEST_DAMAGED_X3F_DIR")?).join(name);
    p.is_file().then_some(p).or_else(|| {
        eprintln!("{name} missing, skipped");
        None
    })
}

fn intact() -> Option<PathBuf> {
    let p = std::env::var_os("GRAINEO_TEST_X3F")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(|d| PathBuf::from(d).join("DP2M5418.X3F")))?;
    p.is_file().then_some(p).or_else(|| {
        eprintln!("no intact Merrill X3F, skipped");
        None
    })
}

#[test]
fn reduced_resolution_low_is_intact() {
    let Some(p) = damaged("_SDI3742.X3F") else { return };
    let f = X3fFile::open(&p).unwrap();
    assert_eq!(f.properties().unwrap().get("RESOLUTION"), Some("LOW"));
    let raw = f.decode_raw().unwrap();
    assert_eq!((raw.columns, raw.rows), (2464, 1632));
    assert_eq!(f.integrity(), Integrity::Intact);
    assert_eq!(x3f::read_image_size(&p), Some((2336, 1568)));
}

#[test]
fn foreign_data_in_top_plane_is_detected_and_rescued() {
    let Some(p) = damaged("_SDI2984.X3F") else { return };
    let f = X3fFile::open(&p).unwrap();
    match f.decode_raw() {
        Err(Error::Damaged { plane: 0, first_row: Some(row), rows: 3264 }) => assert!((925..=935).contains(&row), "row {row}"),
        other => panic!("expected damage in plane 0, got {:?}", other.map(|_| ())),
    }
    assert!(matches!(f.integrity(), Integrity::Damaged { .. }));
    let (raw, r) = f.decode_raw_rescue().unwrap();
    assert_eq!((raw.columns, raw.rows), (4928, 3264));
    assert!(r.lost[1].is_empty() && r.lost[2].is_empty(), "{r:?}");
    assert!(r.rescued_fraction() > 0.85, "{}", r.rescued_fraction());
    assert_eq!(x3f::read_image_size(&p), Some((4704, 3136)));
}

#[test]
fn zeroed_blocks_are_detected_and_partly_rescued() {
    let Some(p) = damaged("_SDI3314.X3F") else { return };
    let f = X3fFile::open(&p).unwrap();
    assert!(matches!(f.decode_raw(), Err(Error::Damaged { .. })));
    match f.integrity() {
        Integrity::Damaged { zero_blocks, .. } => assert!(zero_blocks > 1000, "{zero_blocks}"),
        other => panic!("{other:?}"),
    }
    let (_, r) = f.decode_raw_rescue().unwrap();
    assert!(r.lost[1].is_empty(), "plane 1 is intact");
    assert!(r.rescued_fraction() > 0.1);
}

/// Overwrites `len` bytes at `frac` of plane `plane` of the TRUE data in a copy of the file.
fn damage(data: &mut [u8], f: &X3fFile, plane: usize, frac: f64, len: usize, mut fill: impl FnMut(usize) -> u8) {
    let (e, _) = f.raw_entry().unwrap();
    let body_start = e.offset + x3f::ImageHeader::SIZE;
    let body = &f.data()[body_start..e.offset + e.length];
    let t = x3f::raw::TrueLayout::parse(body).unwrap();
    let data_start = body_start + (t.data.as_ptr() as usize - body.as_ptr() as usize);
    let mut a = data_start + t.starts[plane] + (t.plane_size[plane] as f64 * frac) as usize;
    a -= a % 4096;
    for (k, b) in data[a..a + len].iter_mut().enumerate() {
        *b = fill(k);
    }
}

#[test]
fn rescued_rows_are_bit_identical() {
    let Some(p) = intact() else { return };
    let original = X3fFile::open(&p).unwrap();
    let truth = original.decode_raw().unwrap();
    let mut bytes = original.data().to_vec();
    // zeros in two planes, pseudo-random foreign data in the top plane
    let mut seed = 0x2545F491u32;
    damage(&mut bytes, &original, 0, 0.3, 368_640, |_| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as u8
    });
    damage(&mut bytes, &original, 2, 0.1, 468_992, |_| 0);
    damage(&mut bytes, &original, 2, 0.6, 337_920, |_| 0);
    let f = X3fFile::from_bytes(bytes).unwrap();
    assert!(matches!(f.decode_raw(), Err(Error::Damaged { .. })));
    let (raw, r) = f.decode_raw_rescue().unwrap();
    assert!(r.is_damaged());
    assert!(r.rescued_fraction() > 0.8, "{}", r.rescued_fraction());
    for c in 0..3 {
        for row in (0..raw.rows).filter(|&row| !r.is_lost(c, row)) {
            let s = row * raw.columns..(row + 1) * raw.columns;
            assert!(raw.planes[c][s.clone()] == truth.planes[c][s], "plane {c} row {row} differs");
        }
    }
}

#[test]
fn intact_file_is_unchanged_by_the_rescue_path() {
    let Some(p) = intact() else { return };
    let f = X3fFile::open(&p).unwrap();
    let a = f.decode_raw().unwrap();
    let (b, r) = f.decode_raw_rescue().unwrap();
    assert!(!r.is_damaged());
    assert!(a.planes == b.planes);
    assert_eq!(f.integrity(), Integrity::Intact);
}

/// Minimal X3F: header, the given sections, directory (`count` may lie).
fn synthetic(sections: &[(&[u8; 4], Vec<u8>)], count: Option<u32>) -> Vec<u8> {
    let mut d = b"FOVb".to_vec();
    d.extend_from_slice(&0x0004_0000u32.to_le_bytes());
    d.resize(64, 0);
    let mut dir = Vec::new();
    for (kind, body) in sections {
        dir.push((d.len() as u32, body.len() as u32, **kind));
        d.extend_from_slice(body);
    }
    let at = d.len() as u32;
    d.extend_from_slice(b"SECd");
    d.extend_from_slice(&0x0002_0000u32.to_le_bytes());
    d.extend_from_slice(&count.unwrap_or(dir.len() as u32).to_le_bytes());
    for (o, l, k) in dir {
        d.extend_from_slice(&o.to_le_bytes());
        d.extend_from_slice(&l.to_le_bytes());
        d.extend_from_slice(&k);
    }
    d.extend_from_slice(&at.to_le_bytes());
    d
}

fn image_section(image_type: u32, format: u32, cols: u32, rows: u32, stride: u32, body: usize) -> Vec<u8> {
    let mut s = b"SECi".to_vec();
    for v in [0x0002_0000u32, image_type, format, cols, rows, stride] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    s.resize(28 + body, 0);
    s
}

#[test]
fn absurd_directory_count_is_an_error_not_an_allocation() {
    let d = synthetic(&[], Some(u32::MAX));
    assert!(matches!(X3fFile::from_bytes(d.clone()), Err(Error::Corrupt(_))));
    let p = std::env::temp_dir().join(format!("graineo-x3f-dir-{}.x3f", std::process::id()));
    std::fs::write(&p, &d).unwrap();
    assert!(x3f::read_embedded_jpeg(&p, true).is_err());
    let _ = std::fs::remove_file(&p);
}

#[test]
fn absurd_image_size_is_refused_quickly() {
    let t = std::time::Instant::now();
    // Merrill RAW 65535 × 65535 in a tiny section: no 3 × 8.6 GB planes, no endless decoding
    let f = X3fFile::from_bytes(synthetic(&[(b"IMA2", image_section(1, 30, 65535, 65535, 0, 64))], None)).unwrap();
    assert!(f.decode_raw().is_err());
    assert!(f.decode_raw_rescue().is_err());
    // 1 column: refused as well (rescue decoder would divide by zero)
    let f = X3fFile::from_bytes(synthetic(&[(b"IMA2", image_section(1, 30, 1, 100, 0, 64))], None)).unwrap();
    assert!(f.decode_raw().is_err());
    // uncompressed preview with rows outside the section
    let f = X3fFile::from_bytes(synthetic(&[(b"IMA2", image_section(2, 3, 100, 100, 300, 64))], None)).unwrap();
    assert_eq!(f.images().count(), 0);
    assert!(t.elapsed().as_secs() < 5);
}

#[test]
fn damaged_camf_is_an_error_not_a_panic() {
    let camf = |kind: u32, decoded: u32| {
        let mut s = b"SECc".to_vec();
        for v in [0x0002_0000u32, kind, decoded, 0, 1, 1] {
            s.extend_from_slice(&v.to_le_bytes());
        }
        s.resize(28 + 40, 0);
        s[28] = 3; // one table pair, then the terminator
        s[29] = 0;
        s
    };
    for (kind, size) in [(4, 0), (4, u32::MAX), (5, u32::MAX)] {
        let f = X3fFile::from_bytes(synthetic(&[(b"CAMF", camf(kind, size))], None)).unwrap();
        assert!(f.camf().is_err(), "type {kind}, size {size}");
    }
    // Huffman table running to the end of the section (type 4): error, no index panic
    let mut s = camf(4, 16);
    s.truncate(28);
    s.push(5);
    let f = X3fFile::from_bytes(synthetic(&[(b"CAMF", s)], None)).unwrap();
    assert!(f.camf().is_err());
}
