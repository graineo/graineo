//! Fuzz targets for the X3F reader (container, CAMF, PROP, embedded JPEG EXIF, RAW headers).
//! Short runs in every `cargo test`; long runs: `scripts/fuzz.sh`. Seeds are synthetic files.

#[global_allocator]
static ALLOC: fuzzkit::CapAlloc = fuzzkit::CapAlloc;

use x3f::{Camf, PropList, X3fFile};

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// CAMF entries in plain form: one text, one property list, one 2x2 float matrix and one u16 matrix.
fn camf_entries() -> Vec<u8> {
    let mut out = Vec::new();
    // CMbT
    let mut e = Vec::new();
    e.extend_from_slice(b"CMbT");
    e.extend_from_slice(&le32(2));
    e.extend_from_slice(&le32(0)); // size, patched
    e.extend_from_slice(&le32(20)); // name
    e.extend_from_slice(&le32(28)); // value
    e.extend_from_slice(b"Title\0\0\0");
    e.extend_from_slice(&le32(6));
    e.extend_from_slice(b"hello\0\0\0");
    let n = e.len() as u32;
    e[8..12].copy_from_slice(&le32(n));
    out.extend_from_slice(&e);
    // CMbP: 2 properties
    let mut e = Vec::new();
    e.extend_from_slice(b"CMbP");
    e.extend_from_slice(&le32(2));
    e.extend_from_slice(&le32(0));
    e.extend_from_slice(&le32(20));
    e.extend_from_slice(&le32(28));
    e.extend_from_slice(b"Props\0\0\0");
    // value at 28: num, off, then pairs
    e.extend_from_slice(&le32(2));
    e.extend_from_slice(&le32(52));
    for (a, b) in [(0u32, 4u32), (8, 12)] {
        e.extend_from_slice(&le32(a));
        e.extend_from_slice(&le32(b));
    }
    e.extend_from_slice(b"KA\0\0VA\0\0KB\0\0VB\0\0");
    let n = e.len() as u32;
    e[8..12].copy_from_slice(&le32(n));
    out.extend_from_slice(&e);
    // CMbM float 2x2 and u16 1x3
    for (ty, dims, data) in [(3u32, [2u32, 2], [0x3f80_0000u32, 0, 0, 0x3f80_0000].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()), (6, [1, 3], vec![1, 0, 2, 0, 3, 0])] {
        let mut e = Vec::new();
        e.extend_from_slice(b"CMbM");
        e.extend_from_slice(&le32(2));
        e.extend_from_slice(&le32(0));
        e.extend_from_slice(&le32(20));
        e.extend_from_slice(&le32(28));
        e.extend_from_slice(b"Mat\0dim\0");
        e.extend_from_slice(&le32(ty));
        e.extend_from_slice(&le32(2));
        e.extend_from_slice(&le32(64)); // data offset
        for d in dims {
            e.extend_from_slice(&le32(d));
            e.extend_from_slice(&le32(24));
            e.extend_from_slice(&le32(0));
        }
        e.extend_from_slice(&data);
        let n = e.len() as u32;
        e[8..12].copy_from_slice(&le32(n));
        out.extend_from_slice(&e);
    }
    out
}

/// CAMF section of type 2 (XOR stream), as written by older cameras.
fn camf_type2() -> Vec<u8> {
    let mut key = 0x1234u32;
    let plain = camf_entries();
    let mut s = Vec::new();
    s.extend_from_slice(b"SECc");
    s.extend_from_slice(&le32(0x0002_0000));
    s.extend_from_slice(&le32(2));
    s.extend_from_slice(&[0; 12]);
    s.extend_from_slice(&le32(key));
    for b in plain {
        key = key.wrapping_mul(1597).wrapping_add(51749) % 244944;
        let tmp = ((key as i64 * 301_593_171) >> 24) as u32;
        let a = (key << 8).wrapping_sub(tmp);
        s.push(b ^ (((a >> 1).wrapping_add(tmp)) >> 17) as u8);
    }
    s
}

/// CAMF headers of type 4 and 5 with a small Huffman table (the data stream is random; the
/// mutations reach the table and size checks).
fn camf_huffman(kind: u32) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend_from_slice(b"SECc");
    s.extend_from_slice(&le32(0x0002_0000));
    s.extend_from_slice(&le32(kind));
    s.extend_from_slice(&le32(256)); // decoded size
    s.extend_from_slice(&le32(0)); // seed
    s.extend_from_slice(&le32(16)); // block size
    s.extend_from_slice(&le32(4)); // block count
    let mut d = vec![0u8; 32];
    let table = [(2u8, 0x00u8), (2, 0x40), (3, 0x80), (3, 0xa0), (3, 0xc0), (4, 0xe0), (4, 0xf0)];
    for (i, (l, c)) in table.iter().enumerate() {
        d[2 * i] = *l;
        d[2 * i + 1] = *c;
    }
    d[28..32].copy_from_slice(&le32(64));
    s.extend_from_slice(&d);
    s.extend((0..64u32).map(|i| (i.wrapping_mul(97) ^ 0x5a) as u8));
    s
}

fn prop_section() -> Vec<u8> {
    let pairs = [("CAMMODEL", "SIGMA DP2 Merrill"), ("ISO", "100")];
    let mut chars: Vec<u16> = Vec::new();
    let mut table = Vec::new();
    for (k, v) in pairs {
        table.push(chars.len() as u32);
        chars.extend(k.encode_utf16());
        chars.push(0);
        table.push(chars.len() as u32);
        chars.extend(v.encode_utf16());
        chars.push(0);
    }
    let mut s = Vec::new();
    s.extend_from_slice(b"SECp");
    s.extend_from_slice(&le32(0x0002_0000));
    s.extend_from_slice(&le32(pairs.len() as u32));
    s.extend_from_slice(&le32(0));
    s.extend_from_slice(&le32(0));
    s.extend_from_slice(&le32(chars.len() as u32));
    for t in table {
        s.extend_from_slice(&le32(t));
    }
    for c in chars {
        s.extend_from_slice(&c.to_le_bytes());
    }
    s
}

fn tiny_jpeg() -> Vec<u8> {
    // SOI, APP1 Exif (II, IFD0 with make/orientation, Exif IFD with exposure time), SOS, EOI
    let mut t = Vec::new();
    t.extend_from_slice(b"II\x2a\0");
    t.extend_from_slice(&le32(8));
    t.extend_from_slice(&3u16.to_le_bytes());
    let entry = |t: &mut Vec<u8>, tag: u16, ty: u16, n: u32, v: u32| {
        t.extend_from_slice(&tag.to_le_bytes());
        t.extend_from_slice(&ty.to_le_bytes());
        t.extend_from_slice(&le32(n));
        t.extend_from_slice(&le32(v));
    };
    entry(&mut t, 0x010F, 2, 6, 50);
    entry(&mut t, 0x0112, 3, 1, 6);
    entry(&mut t, 0x8769, 4, 1, 58);
    t.extend_from_slice(&le32(0));
    t.extend_from_slice(b"SIGMA\0");
    t.extend_from_slice(&[0; 2]); // pad to 58
    t.extend_from_slice(&1u16.to_le_bytes());
    entry(&mut t, 0x829A, 5, 1, 76);
    t.extend_from_slice(&le32(0));
    t.extend_from_slice(&le32(1));
    t.extend_from_slice(&le32(125));
    let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
    j.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    j.extend_from_slice(b"Exif\0\0");
    j.extend_from_slice(&t);
    j.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
    j
}

fn image_section(image_type: u32, format: u32, cols: u32, rows: u32, stride: u32, body: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend_from_slice(b"SECi");
    s.extend_from_slice(&le32(0x0002_0000));
    for v in [image_type, format, cols, rows, stride] {
        s.extend_from_slice(&le32(v));
    }
    s.extend_from_slice(body);
    s
}

/// A whole X3F with directory: PROP, CAMF, JPEG preview and a tiny Merrill-like RAW section.
fn x3f_file(camf: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"FOVb");
    d.extend_from_slice(&le32(0x0004_0000));
    d.resize(0x100, 0);
    let mut sections: Vec<(u32, u32, [u8; 4])> = Vec::new();
    let raw_body: Vec<u8> = (0..256u32).map(|i| (i * 31) as u8).collect();
    for (kind, body) in [
        (*b"PROP", prop_section()),
        (*b"CAMF", camf.to_vec()),
        (*b"IMA2", image_section(2, 18, 8, 8, 0, &tiny_jpeg())),
        (*b"IMA2", image_section(1, 0x23, 8, 4, 0, &raw_body)),
    ] {
        sections.push((d.len() as u32, body.len() as u32, kind));
        d.extend_from_slice(&body);
    }
    let dir = d.len() as u32;
    d.extend_from_slice(b"SECd");
    d.extend_from_slice(&le32(0x0002_0000));
    d.extend_from_slice(&le32(sections.len() as u32));
    for (o, l, k) in sections {
        d.extend_from_slice(&le32(o));
        d.extend_from_slice(&le32(l));
        d.extend_from_slice(&k);
    }
    d.extend_from_slice(&le32(dir));
    d
}

/// Everything a viewer or the develop path asks of an opened file.
fn exercise(data: &[u8]) {
    let Ok(f) = X3fFile::from_bytes(data.to_vec()) else { return };
    let _ = f.images().count();
    let _ = f.preview_jpeg().map(x3f::exif::parse_jpeg_exif);
    let _ = f.thumbnail_jpeg();
    let _ = f.exif();
    let _ = f.header_rotation();
    if let Ok(c) = f.camf() {
        for name in c.order.iter() {
            if let Ok(m) = c.matrix(name) {
                let _ = (m.as_f64(), m.shape());
            }
        }
    }
    let _ = f.properties();
    let _ = f.integrity();
    let _ = f.decode_raw();
    let _ = f.decode_raw_format6();
    let _ = f.decode_raw_rescue();
}

#[test]
fn fuzz_x3f_container() {
    let seeds = vec![x3f_file(&camf_type2()), x3f_file(&camf_huffman(4)), x3f_file(&camf_huffman(5))];
    fuzzkit::run("x3f_container", &seeds, 1 << 16, exercise);
}

#[test]
fn fuzz_x3f_camf() {
    let seeds = vec![camf_type2(), camf_huffman(4), camf_huffman(5)];
    fuzzkit::run("x3f_camf", &seeds, 1 << 14, |d| {
        let _ = Camf::parse(d);
    });
}

#[test]
fn fuzz_x3f_prop() {
    fuzzkit::run("x3f_prop", &[prop_section()], 1 << 12, |d| {
        let _ = PropList::parse(d);
    });
}

#[test]
fn fuzz_x3f_jpeg_exif() {
    fuzzkit::run("x3f_jpeg_exif", &[tiny_jpeg()], 1 << 12, |d| {
        let _ = x3f::exif::parse_jpeg_exif(d);
    });
}

/// Seeds are valid: the targets reach the deeper code and not only the first check.
#[test]
fn seeds_parse() {
    let f = X3fFile::from_bytes(x3f_file(&camf_type2())).unwrap();
    let c = f.camf().unwrap();
    assert!(matches!(c.entries.get("Title"), Some(x3f::CamfEntry::Text(t)) if t == "hello"), "{:?}", c.order);
    assert_eq!(f.properties().unwrap().get("ISO"), Some("100"));
    let e = x3f::exif::parse_jpeg_exif(&tiny_jpeg()).unwrap();
    assert_eq!(e.orientation, Some(6));
}

/// Regression: a PROP section with a huge entry count must not reserve memory for it.
#[test]
fn prop_huge_count() {
    let mut s = prop_section();
    s[8..12].copy_from_slice(&le32(0xffff_fff0));
    assert!(PropList::parse(&s).is_err());
}

/// Regression: CAMF sections shorter than their header (type 4 and 5) are errors, not panics.
#[test]
fn camf_short_header() {
    for kind in [4u32, 5] {
        let mut s = camf_huffman(kind);
        s.truncate(21);
        assert!(Camf::parse(&s).is_err());
    }
}
