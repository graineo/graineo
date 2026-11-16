//! Focus areas reported by the camera, read from the file's metadata (read-only).
//!
//! First stage: Canon (`AFInfo` 0x0012, `AFInfo2` 0x0026, `AFInfo3` 0x003c; CR2, CR3, JPEG, DNG), Sony
//! (`FocusLocation` 0x2027 with `FocusFrameSize` 0x2037; ARW, JPEG, Adobe DNG), Fujifilm (`FocusPixel` 0x1023 with
//! `FocusWarning` 0x1301; RAF, JPEG) and the Exif standard (`SubjectArea` 0x9214, `SubjectLocation` 0xa214; iPhone
//! HEIC/JPEG and others). Tag layouts follow the public tag tables (exiftool "Canon Tags", "Sony Tags", "FujiFilm
//! Tags", Exif 2.3); no code is taken from other programs.
//!
//! All areas are normalised to 0 … 1 in the *unrotated* image as stored (origin top left), using the reference size
//! the camera writes next to the values (not the size of the decoded image). [`upright`] applies the Exif orientation.
//! Missing or implausible data simply yields no area; nothing here panics on broken files.
//!
//! Assumptions (see the internal concept): Sony `FocusFrameSize` uses the same reference size as `FocusLocation`;
//! Fujifilm `FocusPixel` refers to the size of the in-camera JPEG (Exif `PixelXDimension`/`PixelYDimension` of the
//! embedded JPEG); Canon mirrorless bodies that report a single valid AF point used that point.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::picture_style::{find, jpeg_exif, parse_makernote, read_range, type_size, Exif, Kind, Makernote, Source, Tiff};

/// Where the area comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusSource {
    Canon,
    Sony,
    Fujifilm,
    /// Exif `SubjectArea` / `SubjectLocation`
    Exif,
}

/// One focus area: centre and size, normalised (0 … 1) in the unrotated image, origin top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusField {
    /// centre x, centre y, width, height; width = height = 0: only a point is known
    pub rect: [f32; 4],
    /// the camera reports focus at this area (Canon "in focus", Sony focus location, Fujifilm without focus warning,
    /// Exif subject area)
    pub achieved: bool,
    /// the area was selected (Canon "selected", Fujifilm point with focus warning)
    pub selected: bool,
    pub source: FocusSource,
}

impl FocusField {
    /// Only a point, no size?
    pub fn is_point(&self) -> bool {
        self.rect[2] <= 0.0 || self.rect[3] <= 0.0
    }

    /// The area in the upright image (Exif orientation 1 … 8).
    pub fn upright(&self, orientation: u16) -> FocusField {
        let [cx, cy, w, h] = self.rect;
        let c = upright([cx, cy], orientation);
        let (w, h) = if (5..=8).contains(&orientation) { (h, w) } else { (w, h) };
        FocusField { rect: [c[0], c[1], w, h], ..*self }
    }
}

/// Focus data of a file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FocusInfo {
    /// Exif orientation (1 … 8) of the file; 1 if missing
    pub orientation: u16,
    pub fields: Vec<FocusField>,
    /// the camera reports manual focus: no area
    pub manual: bool,
    /// Fujifilm: the camera warned that focus was not achieved
    pub focus_warning: bool,
}

/// Point (0 … 1) in the stored image → point in the upright image for Exif orientation `o` (all eight values).
pub fn upright(p: [f32; 2], o: u16) -> [f32; 2] {
    let [x, y] = p;
    match o {
        2 => [1.0 - x, y],
        3 => [1.0 - x, 1.0 - y],
        4 => [x, 1.0 - y],
        5 => [y, x],
        6 => [1.0 - y, x],
        7 => [1.0 - y, 1.0 - x],
        8 => [y, 1.0 - x],
        _ => [x, y],
    }
}

/// Quarter turns clockwise of an Exif orientation, mirroring dropped (as the app displays images).
pub fn quarters(o: u16) -> u8 {
    match o {
        6 | 5 => 1,
        3 | 4 => 2,
        8 | 7 => 3,
        _ => 0,
    }
}

/// Focus areas of a file (RAW, camera JPEG, HEIC). `None`: no focus data (or an unreadable file).
pub fn read(path: &Path) -> Option<FocusInfo> {
    let q = load(path, HEAD)?;
    from_source(&q)
}

/// Bytes read from the start of a file in one go.
const HEAD: usize = 4 << 20;

// ---------------------------------------------------------------------------------------------
// file → TIFF data

fn load(path: &Path, head: usize) -> Option<Source> {
    let mut f = std::fs::File::open(path).ok()?;
    let length = f.metadata().ok()?.len() as usize;
    let header = read_range(&mut f, 0, head.min(length))?;
    if header.starts_with(b"FUJIFILMCCD-RAW") {
        // RAF: embedded JPEG (offset/length at bytes 84/88, big endian), EXIF at its start
        let off = u32::from_be_bytes(header.get(84..88)?.try_into().ok()?) as u64;
        let len = u32::from_be_bytes(header.get(88..92)?.try_into().ok()?) as usize;
        let j = read_range(&mut f, off, len.min(1 << 20))?;
        let tiff = jpeg_exif(&j)?;
        return Some(Source { d: j, tiff, cmt3: None, truncated: false });
    }
    if header.get(4..8) == Some(b"ftyp") {
        if let Some(c1) = find(&header, b"CMT1") {
            // CR3: CMT1 = IFD0, CMT3 = Canon makernote, both with their own TIFF header
            let c3 = find(&header, b"CMT3").map(|p| p + 4);
            return Some(Source { d: header, tiff: c1 + 4, cmt3: c3, truncated: false });
        }
        return heif(&mut f, header);
    }
    if let Some(t) = jpeg_exif(&header) {
        return Some(Source { d: header, tiff: t, cmt3: None, truncated: false });
    }
    if header.starts_with(b"II") || header.starts_with(b"MM") {
        let d = if length > header.len() { tiff_sparse(&mut f, header, length)? } else { header };
        return Some(Source { d, tiff: 0, cmt3: None, truncated: false });
    }
    None
}

/// Largest buffer [`tiff_sparse`] lays out: metadata of camera files sits far below it; in a larger file (scans,
/// panoramas) what lies beyond is not read (no focus fields), the buffer never grows with the file.
const SPARSE_MAX: usize = 512 << 20;

/// A zero-filled buffer of `len` bytes, `None` instead of ending the process if the memory is not there. The zeros come
/// from the allocator (pages untouched until written, also on Windows where a large block is reserved, not filled).
fn zeroed(len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    let layout = std::alloc::Layout::array::<u8>(len).ok()?;
    // SAFETY: `layout` has a non-zero size; a non-null result is `len` zeroed bytes from the global allocator with
    // alignment 1, which is exactly what `Vec<u8>` with capacity `len` owns and frees.
    unsafe {
        let p = std::alloc::alloc_zeroed(layout);
        (!p.is_null()).then(|| Vec::from_raw_parts(p, len, len))
    }
}

/// TIFF-like file (CR2, ARW, DNG) larger than the head: a zero-filled buffer of the file's size (at most
/// [`SPARSE_MAX`]; the memory is only committed where written) into which IFD0, the Exif IFD, their values and the
/// makernote / DNG private data are read – never the image data.
fn tiff_sparse(f: &mut std::fs::File, header: Vec<u8>, length: usize) -> Option<Vec<u8>> {
    let length = length.min(SPARSE_MAX).max(header.len());
    let mut s = Sparse { have: header.len(), d: zeroed(length)?, f };
    s.d[..header.len()].copy_from_slice(&header);
    let le = header.starts_with(b"II");
    let ifd0 = s.u32(4, le)? as usize;
    let exif = s.walk(ifd0, le, &[0x8769, 0xc634]);
    if let Some(e) = exif {
        s.walk(e, le, &[0x927c]);
    }
    Some(s.d)
}

struct Sparse<'f> {
    d: Vec<u8>,
    /// bytes from the start already read
    have: usize,
    f: &'f mut std::fs::File,
}

impl Sparse<'_> {
    fn fetch(&mut self, off: usize, len: usize) {
        let end = off.saturating_add(len).min(self.d.len());
        if end <= self.have || off >= end {
            return;
        }
        let off = off.max(self.have.min(off));
        if self.f.seek(SeekFrom::Start(off as u64)).is_err() {
            return;
        }
        let mut n = off;
        while n < end {
            match self.f.read(&mut self.d[n..end]) {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
        }
    }
    fn u16(&mut self, o: usize, le: bool) -> Option<u16> {
        self.fetch(o, 2);
        let b: [u8; 2] = self.d.get(o..o + 2)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }
    fn u32(&mut self, o: usize, le: bool) -> Option<u32> {
        self.fetch(o, 4);
        let b: [u8; 4] = self.d.get(o..o + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }
    /// Reads an IFD and the values of its entries (up to 64 KB each, `big` tags up to 32 MB); returns the offset of
    /// the Exif IFD if the IFD points to one.
    fn walk(&mut self, at: usize, le: bool, big: &[u16]) -> Option<usize> {
        let n = self.u16(at, le)? as usize;
        if n == 0 || n > 1000 {
            return None;
        }
        self.fetch(at, 2 + 12 * n + 4);
        let mut exif = None;
        for k in 0..n {
            let e = at + 2 + 12 * k;
            let (Some(tag), Some(ty), Some(count)) = (self.u16(e, le), self.u16(e + 2, le), self.u32(e + 4, le)) else { continue };
            let size = type_size(ty).saturating_mul(count as usize);
            if tag == 0x8769 {
                exif = self.u32(e + 8, le).map(|v| v as usize);
            }
            let limit = if big.contains(&tag) { 32 << 20 } else { 64 << 10 };
            if size > 4 && size <= limit {
                if let Some(off) = self.u32(e + 8, le) {
                    self.fetch(off as usize, size);
                }
            }
        }
        exif
    }
}

/// HEIF/HEIC: the Exif item (meta → iinf/iloc); data starts with the offset of the TIFF header.
fn heif(f: &mut std::fs::File, header: Vec<u8>) -> Option<Source> {
    let item = heif_exif_item(&header).and_then(|(off, len)| {
        let len = len.min(4 << 20);
        match header.get(off..off.checked_add(len)?) {
            Some(b) => Some(b.to_vec()),
            None => read_range(f, off as u64, len),
        }
    });
    if let Some(item) = item {
        let skip = item.get(0..4).map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize + 4);
        let tiff = skip.filter(|&t| matches!(item.get(t..t + 2), Some(b"II") | Some(b"MM"))).or_else(|| find(&item, b"Exif\0\0").map(|p| p + 6));
        if let Some(tiff) = tiff {
            return Some(Source { d: item, tiff, cmt3: None, truncated: false });
        }
    }
    // fallback: an Exif block within the head
    let p = find(&header, b"Exif\0\0MM\0\x2a").or_else(|| find(&header, b"Exif\0\0II\x2a\0"))?;
    Some(Source { d: header, tiff: p + 6, cmt3: None, truncated: false })
}

fn be(d: &[u8], p: usize, n: usize) -> Option<u64> {
    let b = d.get(p..p.checked_add(n)?)?;
    Some(b.iter().fold(0u64, |a, &x| (a << 8) | x as u64))
}

/// Child boxes (type, content start, end) of an ISO BMFF range.
fn boxes(d: &[u8], mut p: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut v = Vec::new();
    let end = end.min(d.len());
    while p + 8 <= end {
        let Some(size) = be(d, p, 4) else { break };
        let ty: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (hdr, size) = match size {
            1 => match be(d, p + 8, 8) {
                Some(s) => (16, s as usize),
                None => break,
            },
            0 => (8, end - p),
            s => (8, s as usize),
        };
        if size < hdr || p.saturating_add(size) > end {
            // a box reaching beyond the read range (mdat): its start is still listed
            v.push((ty, p + hdr, end));
            break;
        }
        v.push((ty, p + hdr, p + size));
        p += size;
    }
    v
}

/// File offset and length of the Exif item of a HEIF file.
fn heif_exif_item(d: &[u8]) -> Option<(usize, usize)> {
    let (_, mc, me) = boxes(d, 0, d.len()).into_iter().find(|b| &b.0 == b"meta")?;
    let inner = boxes(d, mc + 4, me);
    // item ID of the Exif item
    let (_, ic, ie) = *inner.iter().find(|b| &b.0 == b"iinf")?;
    let v = *d.get(ic)?;
    let first = if v == 0 { ic + 6 } else { ic + 8 };
    let mut id = None;
    for (ty, c, _) in boxes(d, first, ie) {
        if &ty != b"infe" {
            continue;
        }
        let iv = *d.get(c)?;
        let (item, kind) = match iv {
            2 => (be(d, c + 4, 2)?, d.get(c + 8..c + 12)?),
            3 => (be(d, c + 4, 4)?, d.get(c + 10..c + 14)?),
            _ => continue,
        };
        if kind == b"Exif" {
            id = Some(item);
            break;
        }
    }
    let id = id?;
    let (_, lc, _) = *inner.iter().find(|b| &b.0 == b"iloc")?;
    let v = *d.get(lc)?;
    let mut p = lc + 4;
    let sizes = *d.get(p)?;
    let (off_size, len_size) = ((sizes >> 4) as usize, (sizes & 15) as usize);
    let sizes2 = *d.get(p + 1)?;
    let base_size = (sizes2 >> 4) as usize;
    let index_size = if v == 1 || v == 2 { (sizes2 & 15) as usize } else { 0 };
    p += 2;
    let id_size = if v < 2 { 2 } else { 4 };
    let count = be(d, p, id_size)?;
    p += id_size;
    let num = |p: &mut usize, n: usize| -> Option<u64> {
        let x = if n == 0 { 0 } else { be(d, *p, n)? };
        *p += n;
        Some(x)
    };
    for _ in 0..count.min(10_000) {
        let item = num(&mut p, id_size)?;
        let method = if v == 1 || v == 2 { num(&mut p, 2)? & 15 } else { 0 };
        num(&mut p, 2)?; // data reference index
        let base = num(&mut p, base_size)?;
        let extents = num(&mut p, 2)?;
        let mut first = None;
        for _ in 0..extents.min(10_000) {
            num(&mut p, index_size)?;
            let off = num(&mut p, off_size)?;
            let len = num(&mut p, len_size)?;
            first.get_or_insert((off, len));
        }
        if item == id {
            let (off, len) = first?;
            return (method == 0).then_some(((base + off) as usize, len as usize));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// tags → areas

/// Values of IFD0 and the Exif IFD needed here.
#[derive(Default)]
struct Tags {
    orientation: u16,
    /// Exif PixelXDimension × PixelYDimension
    pixel: Option<(f32, f32)>,
    subject_area: Vec<i64>,
    subject_location: Vec<i64>,
}

fn exif_tags(q: &Source) -> Tags {
    let mut g = Tags { orientation: 1, ..Default::default() };
    let Some(t) = Tiff::new_at(&q.d, q.tiff) else { return g };
    let Some(ifd0) = t.u32(t.basis + 4).and_then(|o| t.ifd(o as usize + t.basis, t.basis)) else { return g };
    let first = |es: &[crate::picture_style::RawEntry], tag: u16, max: usize| es.iter().find(|e| e.tag == tag).map(|e| t.numbers(e, t.basis, max)).unwrap_or_default();
    if let Some(&o) = first(&ifd0, 0x0112, 1).first() {
        if (1..=8).contains(&o) {
            g.orientation = o as u16;
        }
    }
    let exif = ifd0.iter().find(|e| e.tag == 0x8769).and_then(|e| t.value_pos(e)).and_then(|p| t.u32(p)).and_then(|o| t.ifd(o as usize + t.basis, t.basis));
    if let Some(ex) = exif {
        let (w, h) = (first(&ex, 0xa002, 1), first(&ex, 0xa003, 1));
        if let (Some(&w), Some(&h)) = (w.first(), h.first()) {
            if w > 0 && h > 0 {
                g.pixel = Some((w as f32, h as f32));
            }
        }
        g.subject_area = first(&ex, 0x9214, 4);
        g.subject_location = first(&ex, 0xa214, 2);
    }
    g
}

fn from_source(q: &Source) -> Option<FocusInfo> {
    let e = q.exif()?;
    let tags = exif_tags(q);
    let mut info = FocusInfo { orientation: tags.orientation, ..Default::default() };
    with_makernote(&e, q, |mn| {
        let Some(mn) = mn else { return };
        match mn.kind {
            Kind::Canon => canon(mn, &e.model, &mut info),
            Kind::Sony => sony(mn, &mut info),
            Kind::Fuji => fuji(mn, tags.pixel, &mut info),
            _ => {}
        }
    });
    if info.fields.is_empty() && !info.manual {
        exif_subject(&tags, &mut info);
    }
    info.fields.retain(plausible);
    (!info.fields.is_empty() || info.manual).then_some(info)
}

/// The makernote, also from an Adobe DNG (`DNGPrivateData` = "Adobe\0MakN", count, byte order, original offset,
/// makernote): offsets inside it refer to the original file, so the reference point is shifted.
fn with_makernote(e: &Exif, q: &Source, f: impl FnOnce(Option<&Makernote>)) {
    if let Some((p, _)) = e.mn.filter(|_| q.cmt3.is_none()) {
        if q.d.get(p..p + 10) == Some(b"Adobe\0MakN") {
            let parts = (|| {
                let count = be(&q.d, p + 10, 4)? as usize;
                let le = match q.d.get(p + 14..p + 16)? {
                    b"II" => true,
                    b"MM" => false,
                    _ => return None,
                };
                let orig = be(&q.d, p + 16, 4)? as usize;
                let len = count.checked_sub(6)?;
                let data = p + 20;
                q.d.get(data..data.checked_add(len)?)?;
                Some((le, orig, data, len))
            })();
            let Some((le, orig, data, len)) = parts else { return f(None) };
            if data >= orig {
                return f(parse_makernote(&q.d, data, len, le, data - orig, &e.make).as_ref());
            }
            // the makernote must lie at its original offset (or later): place a copy there
            if orig > (256 << 20) {
                return f(None);
            }
            let mut buf = vec![0u8; orig + len];
            buf[orig..].copy_from_slice(&q.d[data..data + len]);
            return f(parse_makernote(&buf, orig, len, le, 0, &e.make).as_ref());
        }
    }
    f(e.makernote(q).as_ref())
}

/// Values of a tag as 16-bit words in the makernote's byte order, whatever its declared type (Sony stores some as
/// "undefined" bytes).
fn words(mn: &Makernote, tag: u16, max: usize) -> Option<Vec<u16>> {
    let e = mn.entries.iter().find(|e| e.tag == tag)?;
    let p = mn.t.value_pos_base(e, mn.basis)?;
    let n = (type_size(e.type_name) * e.n as usize / 2).min(max);
    (0..n).map(|k| mn.t.u16(p + 2 * k)).collect()
}

fn plausible(g: &FocusField) -> bool {
    let [cx, cy, w, h] = g.rect;
    [cx, cy, w, h].iter().all(|v| v.is_finite()) && (0.0..=1.0).contains(&cx) && (0.0..=1.0).contains(&cy) && (0.0..=1.0).contains(&w) && (0.0..=1.0).contains(&h)
}

/// Canon: `AFInfo2`/`AFInfo3` (newer bodies, CR3) or `AFInfo` (EOS until about 2007). Positions are offsets of the
/// area centres from the image centre in the AF image size; Y points up on EOS bodies, down on PowerShot.
fn canon(mn: &Makernote, model: &str, info: &mut FocusInfo) {
    let eos = model.to_ascii_uppercase().contains("EOS");
    let fields = match mn.numbers(0x0026, 4096).or_else(|| mn.numbers(0x003c, 4096)) {
        Some(v) => canon_afinfo2(&v, eos),
        None => mn.numbers(0x0012, 4096).map(|v| canon_afinfo(&v, eos)).unwrap_or_default(),
    };
    info.fields.extend(fields);
}

fn bit(words: &[u16], k: usize) -> bool {
    words.get(k / 16).is_some_and(|w| (w >> (k % 16)) & 1 == 1)
}

fn canon_field(x: i16, y: i16, w: u16, h: u16, aw: f32, ah: f32, eos: bool) -> [f32; 4] {
    let cx = 0.5 + x as f32 / aw;
    let cy = if eos { 0.5 - y as f32 / ah } else { 0.5 + y as f32 / ah };
    [cx, cy, w as f32 / aw, h as f32 / ah]
}

/// `AFInfo2` (int16): size in bytes, area mode, number of points, valid points, Canon image size, AF image size, then
/// widths[n], heights[n], x[n], y[n], in-focus bits[⌈n/16⌉], selected bits[⌈n/16⌉] (if present), …
pub(crate) fn canon_afinfo2(v: &[i64], eos: bool) -> Vec<FocusField> {
    let words: Vec<u16> = v.iter().map(|&x| x as u16).collect();
    // the declared size (bytes) limits the array
    let size = words.first().map_or(0, |&s| s as usize / 2);
    let words = if size >= 8 && size < words.len() { &words[..size] } else { &words[..] };
    let u = |i: usize| words.get(i).copied();
    let (Some(n), Some(valid)) = (u(2), u(3)) else { return Vec::new() };
    let (n, valid) = (n as usize, valid as usize);
    if n == 0 || n > 1024 {
        return Vec::new();
    }
    let (mut aw, mut ah) = (u(6).unwrap_or(0), u(7).unwrap_or(0));
    if aw == 0 || ah == 0 {
        (aw, ah) = (u(4).unwrap_or(0), u(5).unwrap_or(0));
    }
    let m = n.div_ceil(16);
    if aw == 0 || ah == 0 || words.len() < 8 + 4 * n + m {
        return Vec::new();
    }
    let in_focus = &words[8 + 4 * n..8 + 4 * n + m];
    let selected = words.get(8 + 4 * n + m..8 + 4 * n + 2 * m).unwrap_or(&[]);
    let valid = valid.min(n);
    let mut out = Vec::new();
    for k in 0..valid {
        let r = canon_field(words[8 + 2 * n + k] as i16, words[8 + 3 * n + k] as i16, words[8 + k], words[8 + n + k], aw as f32, ah as f32, eos);
        let achieved = bit(in_focus, k);
        // mirrorless bodies list only the point they used (assumption: shown as selected)
        let sel = bit(selected, k) || (valid == 1 && !achieved);
        out.push(FocusField { rect: r, achieved, selected: sel, source: FocusSource::Canon });
    }
    out
}

/// `AFInfo` (int16): number of points, valid points, Canon image size, AF image size, area width/height (shared),
/// x[n], y[n], in-focus bits[⌈n/16⌉].
pub(crate) fn canon_afinfo(v: &[i64], eos: bool) -> Vec<FocusField> {
    let words: Vec<u16> = v.iter().map(|&x| x as u16).collect();
    let u = |i: usize| words.get(i).copied();
    let (Some(n), Some(valid)) = (u(0), u(1)) else { return Vec::new() };
    let (n, valid) = (n as usize, (valid as usize).min(n as usize));
    let (aw, ah, w, h) = (u(4).unwrap_or(0), u(5).unwrap_or(0), u(6).unwrap_or(0), u(7).unwrap_or(0));
    let m = n.div_ceil(16);
    if n == 0 || n > 1024 || aw == 0 || ah == 0 || words.len() < 8 + 2 * n + m {
        return Vec::new();
    }
    let in_focus = &words[8 + 2 * n..8 + 2 * n + m];
    (0..valid)
        .map(|k| {
            let r = canon_field(words[8 + k] as i16, words[8 + n + k] as i16, w, h, aw as f32, ah as f32, eos);
            FocusField { rect: r, achieved: bit(in_focus, k), selected: false, source: FocusSource::Canon }
        })
        .collect()
}

/// Sony: `FocusLocation` [reference width, reference height, x, y] (unrotated, origin top left), frame size from
/// `FocusFrameSize` [width, height, valid]. `FocusMode` 0x201b = 0 means manual focus.
fn sony(mn: &Makernote, info: &mut FocusInfo) {
    if mn.number(0x201b) == Some(0) {
        info.manual = true;
        return;
    }
    let Some(loc) = words(mn, 0x2027, 4).filter(|v| v.len() == 4) else { return };
    let frame = words(mn, 0x2037, 3).filter(|v| v.len() == 3);
    if let Some(g) = sony_field(&loc, frame.as_deref()) {
        info.fields.push(g);
    }
}

pub(crate) fn sony_field(loc: &[u16], frame: Option<&[u16]>) -> Option<FocusField> {
    let (w, h, x, y) = (loc[0] as f32, loc[1] as f32, loc[2] as f32, loc[3] as f32);
    if w <= 0.0 || h <= 0.0 || x > w || y > h || (x == 0.0 && y == 0.0) {
        return None;
    }
    let (fw, fh) = match frame {
        Some(&[fw, fh, valid]) if valid != 0 && fw > 0 && fh > 0 => (fw as f32 / w, fh as f32 / h),
        _ => (0.0, 0.0),
    };
    // a frame larger than most of the image is no focus frame
    let (fw, fh) = if fw > 0.8 || fh > 0.8 { (0.0, 0.0) } else { (fw, fh) };
    Some(FocusField { rect: [x / w, y / h, fw, fh], achieved: true, selected: true, source: FocusSource::Sony })
}

/// Fujifilm: `FocusPixel` [x, y] in the size of the in-camera JPEG; `FocusMode` 0x1021 = 1 manual; `FocusWarning`
/// 0x1301 = 1: the camera reports "out of focus".
fn fuji(mn: &Makernote, pixel: Option<(f32, f32)>, info: &mut FocusInfo) {
    if mn.number(0x1021) == Some(1) {
        info.manual = true;
        return;
    }
    let Some(v) = mn.numbers(0x1023, 2).filter(|v| v.len() == 2) else { return };
    let warning = mn.number(0x1301) == Some(1);
    if let Some(g) = fuji_field(v[0] as f32, v[1] as f32, pixel, warning) {
        info.focus_warning = warning;
        info.fields.push(g);
    }
}

pub(crate) fn fuji_field(x: f32, y: f32, pixel: Option<(f32, f32)>, warning: bool) -> Option<FocusField> {
    let (w, h) = pixel?;
    if x > w || y > h || (x == 0.0 && y == 0.0) {
        return None;
    }
    Some(FocusField { rect: [x / w, y / h, 0.0, 0.0], achieved: !warning, selected: true, source: FocusSource::Fujifilm })
}

/// Exif `SubjectArea` (2 values: point, 3: circle centre + diameter, 4: centre + width/height) or `SubjectLocation`,
/// in pixels of the unrotated main image (`PixelXDimension` × `PixelYDimension`).
fn exif_subject(g: &Tags, info: &mut FocusInfo) {
    let Some((w, h)) = g.pixel else { return };
    let v = if g.subject_area.len() >= 2 { &g.subject_area } else { &g.subject_location };
    let f = |i: usize| v.get(i).map(|&x| x as f32);
    let rect = match v.len() {
        2 => [f(0).unwrap() / w, f(1).unwrap() / h, 0.0, 0.0],
        3 => [f(0).unwrap() / w, f(1).unwrap() / h, f(2).unwrap() / w, f(2).unwrap() / h],
        4 => [f(0).unwrap() / w, f(1).unwrap() / h, f(2).unwrap() / w, f(3).unwrap() / h],
        _ => return,
    };
    info.fields.push(FocusField { rect, achieved: true, selected: true, source: FocusSource::Exif });
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
