//! Tests of the focus reader with synthetic files: small builders assemble TIFF/Exif/makernote bytes with known
//! values (no real photos).

use super::*;

/// Value of an IFD entry: own bytes, or data already written at an absolute position.
enum V {
    Bytes(Vec<u8>),
    At(usize),
}

struct E {
    tag: u16,
    ty: u16,
    n: u32,
    v: V,
}

/// Writer of TIFF structures; offsets are written relative to `base`.
struct W {
    b: Vec<u8>,
    le: bool,
    base: usize,
}

impl W {
    fn tiff(le: bool) -> Self {
        let mut w = W { b: Vec::new(), le, base: 0 };
        w.b.extend_from_slice(if le { b"II\x2a\0" } else { b"MM\0\x2a" });
        w.b.extend_from_slice(&[0; 4]);
        w
    }
    fn p16(&self, x: u16) -> [u8; 2] {
        if self.le { x.to_le_bytes() } else { x.to_be_bytes() }
    }
    fn p32(&self, x: u32) -> [u8; 4] {
        if self.le { x.to_le_bytes() } else { x.to_be_bytes() }
    }
    fn words(&self, v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|&x| self.p16(x)).collect()
    }
    fn e16(&self, tag: u16, v: &[u16]) -> E {
        E { tag, ty: 3, n: v.len() as u32, v: V::Bytes(self.words(v)) }
    }
    fn e32(&self, tag: u16, v: u32) -> E {
        E { tag, ty: 4, n: 1, v: V::Bytes(self.p32(v).to_vec()) }
    }
    fn text(tag: u16, s: &str) -> E {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        E { tag, ty: 2, n: b.len() as u32, v: V::Bytes(b) }
    }
    fn undef(tag: u16, b: Vec<u8>) -> E {
        E { tag, ty: 7, n: b.len() as u32, v: V::Bytes(b) }
    }
    /// Appends an IFD (and its out-of-line data) at the end; returns its absolute position.
    fn ifd(&mut self, es: &[E]) -> usize {
        if self.b.len() % 2 == 1 {
            self.b.push(0);
        }
        let at = self.b.len();
        let data_start = at + 2 + 12 * es.len() + 4;
        let mut data = Vec::new();
        let mut ents = Vec::new();
        for e in es {
            let val = match &e.v {
                V::Bytes(b) if b.len() <= 4 => {
                    let mut v = b.clone();
                    v.resize(4, 0);
                    v
                }
                V::Bytes(b) => {
                    let off = (data_start + data.len() - self.base) as u32;
                    data.extend_from_slice(b);
                    if data.len() % 2 == 1 {
                        data.push(0);
                    }
                    self.p32(off).to_vec()
                }
                V::At(p) => self.p32((p - self.base) as u32).to_vec(),
            };
            ents.push((e.tag, e.ty, e.n, val));
        }
        let n = self.p16(es.len() as u16);
        self.b.extend_from_slice(&n);
        for (tag, ty, n, val) in ents {
            let (t, y, c) = (self.p16(tag), self.p16(ty), self.p32(n));
            self.b.extend_from_slice(&t);
            self.b.extend_from_slice(&y);
            self.b.extend_from_slice(&c);
            self.b.extend_from_slice(&val);
        }
        self.b.extend_from_slice(&[0; 4]);
        self.b.extend_from_slice(&data);
        at
    }
    /// IFD0 with make, model, orientation and an Exif IFD; sets the IFD0 pointer in the header.
    fn finish(&mut self, make: &str, model: &str, orientation: u16, exif: &[E], ifd0_extra: Vec<E>) {
        let ex = self.ifd(exif);
        let mut es = vec![W::text(0x010f, make), W::text(0x0110, model), self.e16(0x0112, &[orientation]), self.e32(0x8769, (ex - self.base) as u32)];
        es.extend(ifd0_extra);
        es.sort_by_key(|e| e.tag);
        let i0 = self.ifd(&es);
        let p = self.p32((i0 - self.base) as u32);
        self.b[self.base + 4..self.base + 8].copy_from_slice(&p);
    }
}

fn source(d: Vec<u8>, tiff: usize) -> Source {
    Source { d, tiff, cmt3: None, truncated: false }
}

fn temp_file(name: &str, d: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("graineo-focus-test-{}-{name}", std::process::id()));
    std::fs::write(&p, d).unwrap();
    p
}

fn read_bytes(name: &str, d: &[u8], head: usize) -> Option<FocusInfo> {
    let p = temp_file(name, d);
    let r = load(&p, head).and_then(|q| from_source(&q));
    let _ = std::fs::remove_file(&p);
    r
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// Canon `AFInfo2` words: n points with sizes/positions, in-focus and selected bit masks.
fn afinfo2(n: u16, valid: u16, af: (u16, u16), pts: &[(u16, u16, i16, i16)], in_focus: &[u16], selected: &[u16]) -> Vec<u16> {
    let mut v = vec![0, 2, n, valid, af.0, af.1, af.0, af.1];
    let pad = |v: &mut Vec<u16>, f: &dyn Fn(&(u16, u16, i16, i16)) -> u16| {
        for k in 0..n as usize {
            v.push(pts.get(k).map_or(0, f));
        }
    };
    pad(&mut v, &|p| p.0);
    pad(&mut v, &|p| p.1);
    pad(&mut v, &|p| p.2 as u16);
    pad(&mut v, &|p| p.3 as u16);
    v.extend_from_slice(in_focus);
    v.extend_from_slice(selected);
    v.push(0); // primary point
    v[0] = (v.len() * 2) as u16;
    v
}

/// CR2-like TIFF with a Canon makernote (offsets relative to the TIFF header).
fn canon_tiff(le: bool, model: &str, orientation: u16, tag: u16, words: &[u16]) -> Vec<u8> {
    let mut w = W::tiff(le);
    let e = w.e16(tag, words);
    let mn = w.ifd(&[e]);
    let len = w.b.len() - mn;
    w.finish("Canon", model, orientation, &[E { tag: 0x927c, ty: 7, n: len as u32, v: V::At(mn) }], vec![]);
    w.b
}

#[test]
fn canon_afinfo2_rp_single_point() {
    // EOS RP: 143 possible points, one valid: spot 240 px at (34, −103) in 6240 × 4160, Y up
    let v = afinfo2(143, 1, (6240, 4160), &[(240, 240, 34, -103)], &[0; 9], &[0; 9]);
    let d = canon_tiff(true, "Canon EOS RP", 1, 0x0026, &v);
    let info = read_bytes("rp.cr2", &d, HEAD).expect("focus");
    assert_eq!(info.fields.len(), 1);
    let g = info.fields[0];
    assert!(close(g.rect, [0.5 + 34.0 / 6240.0, 0.5 + 103.0 / 4160.0, 240.0 / 6240.0, 240.0 / 4160.0]), "{g:?}");
    assert!(!g.achieved && g.selected, "single used point counts as selected");
    assert_eq!(g.source, FocusSource::Canon);
}

#[test]
fn canon_afinfo2_dslr_nine_points_big_endian() {
    // 9 points, point 4 in focus, point 2 selected; big endian file
    let pts: Vec<(u16, u16, i16, i16)> = (0..9).map(|k| (100, 120, (k as i16 - 4) * 200, (k as i16 % 3 - 1) * 300)).collect();
    let v = afinfo2(9, 9, (5616, 3744), &pts, &[1 << 4], &[1 << 2]);
    let d = canon_tiff(false, "Canon EOS 5D Mark II", 1, 0x0026, &v);
    let info = read_bytes("5d2.cr2", &d, HEAD).expect("focus");
    assert_eq!(info.fields.len(), 9);
    let f4 = info.fields[4];
    assert!(f4.achieved && !f4.selected);
    assert!(close(f4.rect, [0.5, 0.5 - (4 % 3 - 1) as f32 * 300.0 / 3744.0, 100.0 / 5616.0, 120.0 / 3744.0]), "{f4:?}");
    assert!(info.fields[2].selected && !info.fields[2].achieved);
    assert_eq!(info.fields.iter().filter(|g| g.achieved || g.selected).count(), 2);
}

#[test]
fn canon_afinfo3_used_when_afinfo2_missing() {
    let v = afinfo2(1, 1, (8192, 5464), &[(400, 400, -1000, 500)], &[1], &[1]);
    let d = canon_tiff(true, "Canon EOS R5", 6, 0x003c, &v);
    let info = read_bytes("r5.cr2", &d, HEAD).expect("focus");
    assert_eq!(info.orientation, 6);
    assert!(info.fields[0].achieved && info.fields[0].selected);
    assert!(close(info.fields[0].rect, [0.5 - 1000.0 / 8192.0, 0.5 - 500.0 / 5464.0, 400.0 / 8192.0, 400.0 / 5464.0]));
}

#[test]
fn canon_afinfo_old_400d() {
    // AFInfo: 9 points, AF image 3504 × 2336 (differs from the image), shared area size, point 2 in focus
    let xs: [i16; 9] = [-931, -500, 0, 500, 947, -500, 0, 500, 0];
    let ys: [i16; 9] = [0, 300, 504, 300, 0, -300, 0, -300, -496];
    let mut v: Vec<u16> = vec![9, 9, 3888, 2592, 3504, 2336, 120, 110];
    v.extend(xs.iter().map(|&x| x as u16));
    v.extend(ys.iter().map(|&y| y as u16));
    v.push(1 << 2);
    let d = canon_tiff(true, "Canon EOS 400D DIGITAL", 1, 0x0012, &v);
    let info = read_bytes("400d.cr2", &d, HEAD).expect("focus");
    assert_eq!(info.fields.len(), 9);
    let g = info.fields[2];
    assert!(g.achieved);
    assert!(close(g.rect, [0.5, 0.5 - 504.0 / 2336.0, 120.0 / 3504.0, 110.0 / 2336.0]), "{g:?}");
    // top point: above the centre (Y up), left point: left
    assert!(g.rect[1] < 0.5 && info.fields[0].rect[0] < 0.3);
}

#[test]
fn canon_powershot_y_points_down() {
    let v = afinfo2(1, 1, (4000, 3000), &[(300, 300, 0, 600)], &[1], &[1]);
    let d = canon_tiff(true, "Canon PowerShot G7 X", 1, 0x0026, &v);
    let info = read_bytes("g7x.jpg.tif", &d, HEAD).expect("focus");
    assert!(close(info.fields[0].rect, [0.5, 0.7, 0.075, 0.1]), "{:?}", info.fields[0]);
}

#[test]
fn canon_cr3_with_cmt_boxes() {
    // CR3: CMT1 (IFD0) and CMT3 (makernote as its own TIFF) inside an ISO BMFF file
    let mut c1 = W::tiff(true);
    let es = [W::text(0x010f, "Canon"), W::text(0x0110, "Canon EOS RP"), c1.e16(0x0112, &[8])];
    let i0 = c1.ifd(&es);
    let p = c1.p32(i0 as u32);
    c1.b[4..8].copy_from_slice(&p);
    let mut c3 = W::tiff(true);
    let e = c3.e16(0x0026, &afinfo2(143, 1, (6240, 4160), &[(480, 481, 0, 1)], &[1; 9], &[1; 9]));
    let mn = c3.ifd(&[e]);
    let p = c3.p32(mn as u32);
    c3.b[4..8].copy_from_slice(&p);
    let mut d = vec![0, 0, 0, 24];
    d.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
    d.extend_from_slice(b"\0\0\0\0uuid");
    d.extend_from_slice(b"CMT1");
    d.extend_from_slice(&c1.b);
    d.extend_from_slice(b"CMT3");
    d.extend_from_slice(&c3.b);
    let info = read_bytes("rp.cr3", &d, HEAD).expect("focus");
    assert_eq!(info.orientation, 8);
    assert_eq!(info.fields.len(), 1);
    assert!(info.fields[0].achieved);
    assert!(close(info.fields[0].rect, [0.5, 0.5 - 1.0 / 4160.0, 480.0 / 6240.0, 481.0 / 4160.0]));
}

/// Sony makernote ("SONY DSC" header, offsets relative to the TIFF header) inside a TIFF.
fn sony_entries(w: &W, loc: [u16; 4], frame: Option<[u16; 3]>, mode: Option<u8>) -> Vec<E> {
    let mut es = vec![];
    if let Some(m) = mode {
        es.push(E { tag: 0x201b, ty: 1, n: 1, v: V::Bytes(vec![m]) });
    }
    es.push(w.e16(0x2027, &loc));
    if let Some(f) = frame {
        es.push(W::undef(0x2037, w.words(&f)));
    }
    es
}

fn sony_arw(le: bool, loc: [u16; 4], frame: Option<[u16; 3]>, mode: Option<u8>) -> Vec<u8> {
    let mut w = W::tiff(le);
    let mn = w.b.len();
    w.b.extend_from_slice(b"SONY DSC \0\0\0");
    let es = sony_entries(&w, loc, frame, mode);
    w.ifd(&es);
    let len = w.b.len() - mn;
    w.finish("SONY", "ILCE-7M3", 1, &[E { tag: 0x927c, ty: 7, n: len as u32, v: V::At(mn) }], vec![]);
    w.b
}

#[test]
fn sony_arw_location_and_frame() {
    let d = sony_arw(true, [6000, 4000, 1406, 1958], Some([547, 486, 1]), Some(2));
    let info = read_bytes("a7.arw", &d, HEAD).expect("focus");
    assert_eq!(info.fields.len(), 1);
    let g = info.fields[0];
    assert!(close(g.rect, [1406.0 / 6000.0, 1958.0 / 4000.0, 547.0 / 6000.0, 486.0 / 4000.0]), "{g:?}");
    assert!(g.achieved && g.source == FocusSource::Sony);
}

#[test]
fn sony_reference_size_from_tag() {
    // reference 3008 × 2000 (as seen at a 6000 × 4000 image): normalised by the tag's own size
    let d = sony_arw(false, [3008, 2000, 1504, 500], None, None);
    let g = read_bytes("a7b.arw", &d, HEAD).expect("focus").fields[0];
    assert!(close(g.rect, [0.5, 0.25, 0.0, 0.0]) && g.is_point());
}

#[test]
fn sony_invalid_frame_flag_gives_point_and_manual_gives_none() {
    let d = sony_arw(true, [6000, 4000, 3000, 2000], Some([547, 486, 0]), Some(3));
    assert!(read_bytes("a7c.arw", &d, HEAD).unwrap().fields[0].is_point());
    let d = sony_arw(true, [6000, 4000, 3000, 2000], Some([547, 486, 1]), Some(0));
    let info = read_bytes("a7d.arw", &d, HEAD).unwrap();
    assert!(info.manual && info.fields.is_empty());
}

#[test]
fn sony_out_of_range_is_dropped() {
    assert!(read_bytes("a7e.arw", &sony_arw(true, [6000, 4000, 6001, 10], None, None), HEAD).is_none());
    assert!(read_bytes("a7f.arw", &sony_arw(true, [0, 0, 1, 1], None, None), HEAD).is_none());
}

/// Adobe DNG with the Sony makernote in `DNGPrivateData` ("Adobe\0MakN"); `orig`: original offset of the makernote.
fn sony_dng(orig: usize, loc: [u16; 4]) -> Vec<u8> {
    // the makernote as it lay in the original ARW at `orig` (offsets relative to the ARW start)
    let mut m = W { b: vec![0; orig], le: true, base: 0 };
    m.b.extend_from_slice(b"SONY DSC \0\0\0");
    let es = sony_entries(&m, loc, Some([400, 300, 1]), Some(2));
    m.ifd(&es);
    let mn = m.b[orig..].to_vec();
    let mut pd = b"Adobe\0MakN".to_vec();
    pd.extend_from_slice(&((mn.len() + 6) as u32).to_be_bytes());
    pd.extend_from_slice(b"II");
    pd.extend_from_slice(&(orig as u32).to_be_bytes());
    pd.extend_from_slice(&mn);
    let mut w = W::tiff(false);
    let e = [w.e16(0xa002, &[8640])];
    w.finish("SONY", "ILCE-1", 6, &e, vec![E { tag: 0xc634, ty: 1, n: pd.len() as u32, v: V::Bytes(pd) }]);
    w.b
}

#[test]
fn sony_adobe_dng_private_data() {
    for orig in [8usize, 200_000] {
        let info = read_bytes(&format!("sony-{orig}.dng"), &sony_dng(orig, [8640, 5760, 2160, 4320]), HEAD).expect("focus");
        assert_eq!(info.orientation, 6);
        assert!(close(info.fields[0].rect, [0.25, 0.75, 400.0 / 8640.0, 300.0 / 5760.0]), "orig {orig}: {:?}", info.fields[0]);
    }
}

#[test]
fn sparse_read_beyond_head() {
    // the makernote lies far beyond the head; only the needed ranges are read
    let mut w = W::tiff(true);
    w.b.resize(300_000, 0xee);
    let mn = w.b.len();
    w.b.extend_from_slice(b"SONY DSC \0\0\0");
    let es = sony_entries(&w, [6000, 4000, 3000, 1000], None, None);
    w.ifd(&es);
    let len = w.b.len() - mn;
    w.finish("SONY", "ILCE-7M4", 1, &[E { tag: 0x927c, ty: 7, n: len as u32, v: V::At(mn) }], vec![]);
    // image data after everything
    w.b.extend(std::iter::repeat_n(0x55u8, 100_000));
    let info = read_bytes("sparse.arw", &w.b, 64).expect("focus");
    assert!(close(info.fields[0].rect, [0.5, 0.25, 0.0, 0.0]));
}

/// Fujifilm makernote: "FUJIFILM", offset 12, IFD with offsets relative to the makernote start, little endian.
fn fuji_makernote(px: [u16; 2], mode: u16, warning: u16) -> Vec<u8> {
    let mut m = W { b: b"FUJIFILM\x0c\0\0\0".to_vec(), le: true, base: 0 };
    let es = [m.e16(0x1021, &[mode]), m.e16(0x1023, &px), m.e16(0x1301, &[warning])];
    m.ifd(&es);
    m.b
}

/// JPEG with an Exif APP1 segment around `tiff`.
fn jpeg(tiff: &[u8]) -> Vec<u8> {
    let mut j = vec![0xff, 0xd8, 0xff, 0xe1];
    j.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    j.extend_from_slice(b"Exif\0\0");
    j.extend_from_slice(tiff);
    j.extend_from_slice(&[0xff, 0xd9]);
    j
}

fn fuji_raf(px: [u16; 2], jpeg_size: [u16; 2], mode: u16, warning: u16) -> Vec<u8> {
    let mut w = W::tiff(false);
    let e = [W::undef(0x927c, fuji_makernote(px, mode, warning)), w.e16(0xa002, &[jpeg_size[0]]), w.e16(0xa003, &[jpeg_size[1]])];
    w.finish("FUJIFILM", "X100", 1, &e, vec![]);
    let j = jpeg(&w.b);
    let mut d = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
    d.resize(84, 0);
    d.extend_from_slice(&100u32.to_be_bytes());
    d.extend_from_slice(&(j.len() as u32).to_be_bytes());
    d.resize(100, 0);
    d.extend_from_slice(&j);
    d
}

#[test]
fn fuji_raf_focus_pixel() {
    let info = read_bytes("x100.raf", &fuji_raf([1088, 724], [2176, 1448], 0, 0), HEAD).expect("focus");
    let g = info.fields[0];
    assert!(close(g.rect, [0.5, 0.5, 0.0, 0.0]) && g.achieved && g.source == FocusSource::Fujifilm);
    assert!(!info.focus_warning);
}

#[test]
fn fuji_warning_and_manual() {
    let info = read_bytes("x100w.raf", &fuji_raf([2208, 1163], [4416, 2944], 0, 1), HEAD).expect("focus");
    assert!(info.focus_warning && !info.fields[0].achieved && info.fields[0].selected);
    assert!(close(info.fields[0].rect, [0.5, 1163.0 / 2944.0, 0.0, 0.0]));
    let m = read_bytes("x100m.raf", &fuji_raf([2208, 1163], [4416, 2944], 1, 0), HEAD).expect("manual");
    assert!(m.manual && m.fields.is_empty());
}

#[test]
fn fuji_without_reference_size_gives_nothing() {
    let mut w = W::tiff(true);
    w.finish("FUJIFILM", "X-T30", 1, &[W::undef(0x927c, fuji_makernote([100, 100], 0, 0))], vec![]);
    assert!(read_bytes("xt30.jpg", &jpeg(&w.b), HEAD).is_none());
}

/// iPhone-like Exif with SubjectArea in a TIFF.
fn exif_tiff(area: &[u16], px: [u16; 2], orientation: u16) -> Vec<u8> {
    let mut w = W::tiff(false);
    let e = [w.e16(0x9214, area), w.e16(0xa002, &[px[0]]), w.e16(0xa003, &[px[1]])];
    w.finish("Apple", "iPhone 16 Pro", orientation, &e, vec![]);
    w.b
}

#[test]
fn exif_subject_area_forms() {
    let rect = read_bytes("ip.jpg", &jpeg(&exif_tiff(&[2012, 1508, 2217, 1330], [4032, 3024], 6)), HEAD).expect("rect");
    assert_eq!(rect.orientation, 6);
    assert!(close(rect.fields[0].rect, [2012.0 / 4032.0, 1508.0 / 3024.0, 2217.0 / 4032.0, 1330.0 / 3024.0]));
    assert_eq!(rect.fields[0].source, FocusSource::Exif);
    let circle = read_bytes("ip2.jpg", &jpeg(&exif_tiff(&[1000, 1000, 400], [4000, 2000], 1)), HEAD).expect("circle");
    assert!(close(circle.fields[0].rect, [0.25, 0.5, 0.1, 0.2]));
    let point = read_bytes("ip3.jpg", &jpeg(&exif_tiff(&[1000, 500], [4000, 2000], 1)), HEAD).expect("point");
    assert!(point.fields[0].is_point());
    assert!(read_bytes("ip4.jpg", &jpeg(&exif_tiff(&[9000, 500, 10, 10], [4000, 2000], 1)), HEAD).is_none(), "outside the image");
}

/// HEIC: ftyp, meta (iinf with an Exif item, iloc), mdat with the Exif item.
fn heic(tiff: &[u8]) -> Vec<u8> {
    fn bx(ty: &[u8], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(ty);
        b.extend_from_slice(body);
        b
    }
    let mut item = 6u32.to_be_bytes().to_vec();
    item.extend_from_slice(b"Exif\0\0");
    item.extend_from_slice(tiff);
    let meta = |off: u32| {
        let infe_hvc = bx(b"infe", &[&[2, 0, 0, 0][..], &1u16.to_be_bytes(), &[0, 0], b"hvc1", b"\0"].concat());
        let infe = bx(b"infe", &[&[2, 0, 0, 0][..], &2u16.to_be_bytes(), &[0, 0], b"Exif", b"\0"].concat());
        let iinf = bx(b"iinf", &[&[0, 0, 0, 0][..], &2u16.to_be_bytes(), &infe_hvc, &infe].concat());
        let mut il = vec![0, 0, 0, 0, 0x44, 0x00];
        il.extend_from_slice(&2u16.to_be_bytes());
        for (id, o, l) in [(1u16, 0u32, 0u32), (2, off, item.len() as u32)] {
            il.extend_from_slice(&id.to_be_bytes());
            il.extend_from_slice(&0u16.to_be_bytes());
            il.extend_from_slice(&1u16.to_be_bytes());
            il.extend_from_slice(&o.to_be_bytes());
            il.extend_from_slice(&l.to_be_bytes());
        }
        let hdlr = bx(b"hdlr", &[0u8; 24]);
        bx(b"meta", &[&[0, 0, 0, 0][..], &hdlr, &iinf, &bx(b"iloc", &il)].concat())
    };
    let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    let pos = (ftyp.len() + meta(0).len() + 8 + 64) as u32;
    let mut mdat = vec![0x11u8; 64];
    mdat.extend_from_slice(&item);
    [ftyp, meta(pos), bx(b"mdat", &mdat)].concat()
}

#[test]
fn heic_exif_item() {
    let info = read_bytes("ip.heic", &heic(&exif_tiff(&[2178, 1511, 1264, 758], [4032, 3024], 6)), HEAD).expect("heic");
    assert_eq!(info.orientation, 6);
    assert!(close(info.fields[0].rect, [2178.0 / 4032.0, 1511.0 / 3024.0, 1264.0 / 4032.0, 758.0 / 3024.0]));
    // the item beyond the head is read separately
    let info = read_bytes("ip2.heic", &heic(&exif_tiff(&[2178, 1511, 1264, 758], [4032, 3024], 1)), 200).expect("heic, short head");
    assert_eq!(info.fields.len(), 1);
}

#[test]
fn orientation_all_eight() {
    // the top-left quarter point (0.25, 0.125) of a landscape image
    let p = [0.25, 0.125];
    let want = [
        (1, [0.25, 0.125]),
        (2, [0.75, 0.125]),
        (3, [0.75, 0.875]),
        (4, [0.25, 0.875]),
        (5, [0.125, 0.25]),
        (6, [0.875, 0.25]),
        (7, [0.875, 0.75]),
        (8, [0.125, 0.75]),
    ];
    for (o, w) in want {
        let u = upright(p, o);
        assert!((u[0] - w[0]).abs() < 1e-6 && (u[1] - w[1]).abs() < 1e-6, "orientation {o}: {u:?}");
    }
    // invalid values: unchanged
    assert_eq!(upright(p, 0), p);
    assert_eq!(upright(p, 9), p);
    // rotations agree with quarter turns clockwise (the app's way: [1 − y, x] per quarter)
    for o in [1u16, 6, 3, 8] {
        let mut q = p;
        for _ in 0..quarters(o) {
            q = [1.0 - q[1], q[0]];
        }
        assert_eq!(q, upright(p, o), "orientation {o}");
    }
    let g = FocusField { rect: [0.25, 0.125, 0.2, 0.1], achieved: true, selected: false, source: FocusSource::Exif };
    for o in 1..=8u16 {
        let u = g.upright(o);
        let swapped = o >= 5;
        assert_eq!([u.rect[2], u.rect[3]], if swapped { [0.1, 0.2] } else { [0.2, 0.1] }, "orientation {o}");
    }
}

#[test]
fn broken_data_never_panics() {
    // empty, garbage, truncated files and truncated arrays
    assert!(read_bytes("empty", &[], HEAD).is_none());
    assert!(read_bytes("garbage", &[0x42; 5000], HEAD).is_none());
    let d = sony_arw(true, [6000, 4000, 3000, 2000], None, None);
    for cut in [4, 8, 20, 60, d.len() / 2, d.len() - 3] {
        let _ = read_bytes("cut.arw", &d[..cut], HEAD);
        let _ = read_bytes("cut2.arw", &d[..cut], 16);
    }
    let h = heic(&exif_tiff(&[1, 1], [10, 10], 1));
    for cut in [10, 40, 80, h.len() - 5] {
        let _ = read_bytes("cut.heic", &h[..cut], HEAD);
    }
    let r = fuji_raf([1, 1], [10, 10], 0, 0);
    for cut in [50, 90, 120, r.len() - 2] {
        let _ = read_bytes("cut.raf", &r[..cut], HEAD);
    }
    // random bytes behind a valid TIFF header
    let mut x: u32 = 12345;
    for k in 0..200 {
        let mut d = b"II\x2a\0\x08\0\0\0".to_vec();
        for _ in 0..400 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            d.push((x >> 16) as u8);
        }
        let _ = from_source(&source(d.clone(), 0));
        let _ = read_bytes(&format!("rnd{k}"), &d, 64);
    }
    // arrays shorter than declared
    assert!(canon_afinfo2(&[100, 2, 9, 9, 10, 10, 10, 10, 1, 2], true).is_empty());
    assert!(canon_afinfo2(&[0, 2, 0, 0, 10, 10, 10, 10], true).is_empty());
    assert!(canon_afinfo(&[9, 9, 0, 0, 0, 0, 0, 0], true).is_empty());
    assert!(sony_field(&[100, 100, 200, 50], None).is_none());
    assert!(fuji_field(10.0, 10.0, None, false).is_none());
}

// ---------------------------------------------------------------------------------------------
// fuzzing (short run here, long runs: scripts/fuzz.sh)

fn fuzz_seeds() -> Vec<Vec<u8>> {
    let v = afinfo2(9, 9, (5616, 3744), &[(100, 120, 0, 0), (100, 120, 200, 300)], &[1 << 1], &[1]);
    vec![
        canon_tiff(true, "Canon EOS RP", 6, 0x0026, &v),
        canon_tiff(false, "Canon EOS 5D Mark II", 1, 0x003c, &v),
        canon_tiff(true, "Canon EOS 400D DIGITAL", 1, 0x0012, &[9, 9, 3888, 2592, 3504, 2336, 120, 110, 0, 0, 0, 0]),
        sony_arw(true, [6000, 4000, 3000, 2000], Some([400, 300, 1]), Some(2)),
        sony_dng(8, [8640, 5760, 2160, 4320]),
        fuji_raf([100, 80], [6000, 4000], 0, 0),
        heic(&exif_tiff(&[3000, 2000, 400, 300], [6000, 4000], 1)),
        jpeg(&exif_tiff(&[3000, 2000], [6000, 4000], 8)),
    ]
}

/// Focus points and picture style from mutated RAW/JPEG/HEIC headers and makernotes: no panic, no hang.
#[test]
fn fuzz_focus_and_picture_style() {
    fuzzkit::run("raw_makernote", &fuzz_seeds(), 1 << 16, |d| {
        let _ = from_source(&source(d.to_vec(), 0));
        let p = fuzzkit::temp_file("makernote.bin", d);
        let _ = load(&p, HEAD).and_then(|q| from_source(&q));
        let _ = load(&p, 64).and_then(|q| from_source(&q));
        let _ = crate::picture_style::read(&p);
        let _ = crate::picture_style::read_large(&p);
        let _ = crate::picture_style::makernote(&p);
    });
}
