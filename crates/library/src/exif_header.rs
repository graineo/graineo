//! Platform-independent EXIF reader: shooting data, camera, capture time, orientation, size and GPS position from
//! the file header, without the system image reader. On macOS ImageIO stays the main path (this reader is used
//! there only for comparison and tests); on Windows and Linux it is the only one.
//!
//! Only the parts of the file that hold the metadata are read, in blocks of 64 KiB, at most [`READ_LIMIT`] bytes
//! per file (network drives: a large RAW or TIFF costs one or two blocks, not the whole file). The container is
//! recognised by its first bytes, not by the file name:
//!
//! - TIFF and TIFF-based RAWs (DNG, CR2, NEF, ARW, PEF, ORF, RW2, SRW, ERF, 3FR …): IFD0, EXIF and GPS IFD
//! - JPEG: APP1 "Exif"; PNG: `eXIf` chunk; WebP: `EXIF` chunk; PSD: image resource 0x0422
//! - HEIF/HEIC/AVIF: the `Exif` item of the `meta` box, size from `ispe` of the primary item
//! - CR3: the TIFF blocks `CMT1` (IFD0), `CMT2` (EXIF) and `CMT4` (GPS) in the Canon box of `moov`
//! - RAF: EXIF of the embedded JPEG; MRW: the TIFF block `TTW`; RW2: the embedded JPEG fills what IFD0 lacks
//!
//! Field meanings follow what `imageio::properties` returns on the Mac: model and make as text (trimmed), capture
//! time from DateTimeOriginal, else the TIFF DateTime, as 'YYYY-MM-DDThh:mm:ss' (local camera time, no time
//! zone), numbers only when positive, ISO the first value of ISOSpeedRatings.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::imageio_common::{exif_date, Properties};

/// Size of one read from the file.
const BLOCK: u64 = 64 * 1024;
/// Most bytes read from one file (damaged files with offsets all over the place stay cheap).
const READ_LIMIT: u64 = 4 << 20;
/// Most entries of one IFD (real files have well under 100).
const MAX_ENTRIES: usize = 1000;

/// Everything the reader found in one pass.
#[derive(Clone, Debug, Default)]
pub struct Header {
    pub props: Properties,
    /// latitude, longitude in degrees (north and east positive)
    pub gps: Option<(f64, f64)>,
    /// EXIF ExposureBiasValue in EV
    pub exposure_bias: Option<f64>,
    /// EXIF FocalLengthIn35mmFilm
    pub focal_length_35mm: Option<u32>,
    /// EXIF LensMake
    pub lens_make: Option<String>,
}

/// Read the metadata of `path`; `None` when the file cannot be opened or its container is not known.
pub fn read(path: &Path) -> Option<Header> {
    read_from(&Src::open(path)?)
}

fn read_from(src: &Src) -> Option<Header> {
    let mut h = Header::default();
    let head = src.get(0, 32)?;
    if head.len() < 12 {
        return None;
    }
    let found = match () {
        _ if head.starts_with(&[0xFF, 0xD8]) => jpeg(src, 0, &mut h),
        _ if is_tiff(&head) => tiff_file(src, &head, &mut h),
        _ if head.starts_with(b"\x89PNG\r\n\x1a\n") => png(src, &mut h),
        _ if head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" => webp(src, &mut h),
        _ if head.starts_with(b"8BPS") => psd(src, &mut h),
        _ if head.starts_with(b"FUJIFILMCCD-RAW") => raf(src, &mut h),
        _ if head.starts_with(b"\0MRM") => mrw(src, &mut h),
        _ if &head[4..8] == b"ftyp" => {
            let brand = &head[8..12];
            if brand == b"crx " {
                cr3(src, &mut h)
            } else {
                heif(src, &mut h)
            }
        }
        _ => false,
    };
    found.then_some(h)
}

/// Like `imageio::properties`: `None` only when nothing could be read.
pub fn properties(path: &Path) -> Option<Properties> {
    read(path).map(|h| h.props)
}

/// GPS position from the EXIF GPS block (`None` without a usable position, as `imageio::gps_position`).
pub fn gps_position(path: &Path) -> Option<(f64, f64)> {
    read(path)?.gps
}

// ───────────────────────────────────────────── file access

/// The file, read on demand in blocks (each block at most once).
struct Src {
    file: RefCell<std::fs::File>,
    len: u64,
    blocks: RefCell<HashMap<u64, Vec<u8>>>,
    read: RefCell<u64>,
}

impl Src {
    fn open(path: &Path) -> Option<Src> {
        let file = std::fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        Some(Src { file: RefCell::new(file), len, blocks: RefCell::new(HashMap::new()), read: RefCell::new(0) })
    }

    fn block(&self, k: u64) -> Option<()> {
        if self.blocks.borrow().contains_key(&k) {
            return Some(());
        }
        let start = k * BLOCK;
        if start >= self.len {
            return None;
        }
        let n = BLOCK.min(self.len - start);
        {
            let mut r = self.read.borrow_mut();
            if *r + n > READ_LIMIT {
                return None;
            }
            *r += n;
        }
        let mut buf = vec![0u8; n as usize];
        let mut f = self.file.borrow_mut();
        f.seek(SeekFrom::Start(start)).ok()?;
        f.read_exact(&mut buf).ok()?;
        self.blocks.borrow_mut().insert(k, buf);
        Some(())
    }

    /// `n` bytes at `off`; at the end of the file fewer (never more than there are).
    fn get(&self, off: u64, n: usize) -> Option<Vec<u8>> {
        if off >= self.len {
            return None;
        }
        let end = (off + n as u64).min(self.len);
        let mut out = Vec::with_capacity((end - off) as usize);
        let mut pos = off;
        while pos < end {
            let k = pos / BLOCK;
            self.block(k)?;
            let blocks = self.blocks.borrow();
            let b = &blocks[&k];
            let from = (pos - k * BLOCK) as usize;
            let to = ((end - k * BLOCK) as usize).min(b.len());
            out.extend_from_slice(&b[from..to]);
            pos = k * BLOCK + to as u64;
        }
        Some(out)
    }

    /// Exactly `n` bytes at `off`.
    fn exact(&self, off: u64, n: usize) -> Option<Vec<u8>> {
        self.get(off, n).filter(|b| b.len() == n)
    }
}

/// Bytes the TIFF parser reads from: the file itself (TIFF-based RAW) or a block taken out of it (JPEG APP1 …).
enum Bytes<'a> {
    File(&'a Src, u64),
    Mem(&'a [u8]),
}

impl<'a> Bytes<'a> {
    /// The same bytes from `off` on (a block inside the block, e.g. a makernote with its own TIFF header).
    fn sub(&self, off: u64) -> Option<Bytes<'a>> {
        Some(match self {
            Bytes::File(s, base) => Bytes::File(s, base.checked_add(off)?),
            Bytes::Mem(b) => Bytes::Mem(b.get(usize::try_from(off).ok()?..)?),
        })
    }

    fn get(&self, off: u64, n: usize) -> Option<Vec<u8>> {
        match self {
            Bytes::File(s, base) => s.exact(base.checked_add(off)?, n),
            Bytes::Mem(b) => {
                let o = usize::try_from(off).ok()?;
                b.get(o..o.checked_add(n)?).map(<[u8]>::to_vec)
            }
        }
    }
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn be64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

// ───────────────────────────────────────────── TIFF

/// TIFF header or one of the TIFF-based RAW variants (ORF "IIRO"/"IIRS"/"MMOR", RW2 "IIU\0").
fn is_tiff(h: &[u8]) -> bool {
    matches!(&h[..4], b"II*\0" | b"MM\0*" | b"IIRO" | b"IIRS" | b"MMOR" | b"IIU\0")
}

struct Entry {
    tag: u16,
    ty: u16,
    count: u32,
    /// the 4-byte value field (inline value or offset)
    field: [u8; 4],
}

struct Tiff<'a> {
    b: Bytes<'a>,
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(b: Bytes<'a>) -> Option<Tiff<'a>> {
        let h = b.get(0, 4)?;
        let le = match &h[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Tiff { b, le })
    }

    fn u16_of(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.le { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) }
    }

    fn u32_of(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    }

    fn ifd0(&self) -> Option<u32> {
        Some(self.u32_of(&self.b.get(4, 4)?))
    }

    fn entries(&self, ifd: u32) -> Vec<Entry> {
        let Some(n) = self.b.get(ifd as u64, 2).map(|b| self.u16_of(&b) as usize) else { return Vec::new() };
        if n == 0 || n > MAX_ENTRIES {
            return Vec::new();
        }
        let Some(raw) = self.b.get(ifd as u64 + 2, n * 12) else { return Vec::new() };
        raw.chunks_exact(12)
            .map(|e| Entry { tag: self.u16_of(&e[0..2]), ty: self.u16_of(&e[2..4]), count: self.u32_of(&e[4..8]), field: [e[8], e[9], e[10], e[11]] })
            .collect()
    }

    fn size_of(ty: u16) -> Option<usize> {
        Some(match ty {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 => 8,
            _ => return None,
        })
    }

    /// Value bytes of an entry, at most `max` values.
    fn data(&self, e: &Entry, max: u32) -> Option<Vec<u8>> {
        let len = Self::size_of(e.ty)?.checked_mul(e.count.min(max) as usize)?;
        let full = Self::size_of(e.ty)?.checked_mul(e.count as usize)?;
        if full <= 4 {
            Some(e.field[..len].to_vec())
        } else {
            self.b.get(self.u32_of(&e.field) as u64, len)
        }
    }

    fn text(&self, e: &Entry) -> Option<String> {
        if e.ty != 2 && e.ty != 7 && e.ty != 1 {
            return None;
        }
        let b = self.data(e, 4096)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        let s = String::from_utf8_lossy(&b[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// First value as an unsigned integer (BYTE, SHORT, LONG).
    fn uint(&self, e: &Entry) -> Option<u32> {
        let b = self.data(e, 1)?;
        match e.ty {
            1 => b.first().map(|&v| v as u32),
            3 => (b.len() >= 2).then(|| self.u16_of(&b) as u32),
            4 => (b.len() >= 4).then(|| self.u32_of(&b)),
            _ => None,
        }
    }

    /// Value `i` as a number (rationals, integers).
    fn number_at(&self, e: &Entry, i: u32) -> Option<f64> {
        if i >= e.count {
            return None;
        }
        let size = Self::size_of(e.ty)?;
        let all = self.data(e, i + 1)?;
        let b = all.get(i as usize * size..(i as usize + 1) * size)?;
        Some(match e.ty {
            1 => b[0] as f64,
            3 => self.u16_of(b) as f64,
            4 => self.u32_of(b) as f64,
            8 => self.u16_of(b) as i16 as f64,
            9 => self.u32_of(b) as i32 as f64,
            5 | 10 => {
                let (n, d) = (self.u32_of(&b[0..4]), self.u32_of(&b[4..8]));
                if d == 0 {
                    return None;
                }
                if e.ty == 10 { n as i32 as f64 / d as i32 as f64 } else { n as f64 / d as f64 }
            }
            11 => f32::from_bits(self.u32_of(b)) as f64,
            12 => {
                let a: [u8; 8] = b.try_into().ok()?;
                if self.le { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }
            }
            _ => return None,
        })
    }

    fn number(&self, e: &Entry) -> Option<f64> {
        self.number_at(e, 0)
    }

    fn offset(&self, e: &Entry) -> Option<u32> {
        matches!(e.ty, 4 | 13).then(|| self.u32_of(&e.field))
    }
}

/// Positive, finite (what ImageIO's number conversion keeps).
fn positive(v: Option<f64>) -> Option<f64> {
    v.filter(|x| x.is_finite() && *x > 0.0)
}

/// What IFD0 holds besides the data merged into [`Header`].
#[derive(Default)]
struct Ifd0 {
    exif: Option<u32>,
    gps: Option<u32>,
    date_time: Option<String>,
    /// RW2: offset of the embedded JPEG (tag 0x002E)
    rw2_jpeg: Option<u32>,
    size: (Option<u32>, Option<u32>),
    panasonic_iso: Option<u32>,
    /// XMP packet (TIFF tag 700)
    xmp: Option<Vec<u8>>,
    /// NewSubfileType (1 = reduced image: IFD0 of NEF, ARW … is the thumbnail)
    subfile: Option<u32>,
    dng: bool,
}

/// Read IFD0 (`ifd`), EXIF and GPS IFD of `t` into `h`. Fields already set stay. `own_size`: IFD0 describes the
/// image of the file (a TIFF), so its size is the image size; not for EXIF blocks and RAWs (thumbnail or preview there).
fn tiff_into(t: &Tiff, ifd: u32, own_size: bool, h: &mut Header) -> bool {
    let mut z = Ifd0::default();
    let entries = t.entries(ifd);
    if entries.is_empty() {
        return false;
    }
    let p = &mut h.props;
    for e in &entries {
        match e.tag {
            0x0100 => z.size.0 = t.uint(e),
            0x0101 => z.size.1 = t.uint(e),
            0x00FE => z.subfile = t.uint(e),
            0xC612 => z.dng = true,
            0x010F => p.maker = p.maker.take().or_else(|| t.text(e)),
            0x0110 => p.camera = p.camera.take().or_else(|| t.text(e)),
            0x0112 => p.orientation = p.orientation.or(t.uint(e).filter(|o| (1..=8).contains(o))),
            0x0132 => z.date_time = t.text(e),
            0x8769 => z.exif = t.offset(e),
            0x8825 => z.gps = t.offset(e),
            0x002E if e.ty == 7 => z.rw2_jpeg = Some(t.u32_of(&e.field)),
            // Panasonic RW2 / Leica RWL: ISO in IFD0 (tag 0x0017), no EXIF ISO there
            0x0017 if e.ty == 3 => z.panasonic_iso = t.uint(e),
            0x02BC => z.xmp = t.data(e, 1 << 20),
            _ => {}
        }
    }
    if own_size && !z.dng && z.subfile.unwrap_or(0) == 0 {
        if let (Some(w), Some(hh)) = z.size {
            h.props.width = h.props.width.or(Some(w)).filter(|v| *v > 0);
            h.props.height = h.props.height.or(Some(hh)).filter(|v| *v > 0);
        }
    }
    if let Some(x) = z.exif {
        exif_ifd(t, x, h);
    }
    if let Some(g) = z.gps {
        h.gps = h.gps.or_else(|| gps_ifd(t, g));
    }
    if h.props.capture.is_none() {
        h.props.capture = z.date_time.as_deref().and_then(exif_date);
    }
    if h.props.capture.is_none() {
        h.props.capture = z.xmp.as_deref().and_then(xmp_date);
    }
    if let (Some(off), Bytes::File(src, base)) = (z.rw2_jpeg, &t.b) {
        // RW2: IFD0 has camera and ISO only; the embedded JPEG carries the full EXIF
        if h.props.aperture.is_none() || h.props.capture.is_none() {
            jpeg(src, base + off as u64, h);
        }
    }
    if h.props.iso.is_none() {
        h.props.iso = z.panasonic_iso.filter(|v| *v > 0).map(i64::from);
    }
    true
}

/// Capture time from an XMP packet (files without EXIF date, e.g. TIFF written by image editors): EXIF
/// DateTimeOriginal, else photoshop:DateCreated, else xmp:CreateDate; as attribute or element.
fn xmp_date(x: &[u8]) -> Option<String> {
    let x = std::str::from_utf8(x).ok()?;
    ["exif:DateTimeOriginal", "photoshop:DateCreated", "xmp:CreateDate"].iter().find_map(|key| {
        let at = x.find(key)? + key.len();
        let rest = x[at..].trim_start_matches(|c: char| c == '=' || c == '"' || c == '\'' || c == '>' || c.is_whitespace());
        let v = rest.get(..19)?;
        // "2017-01-12T19:32:52" (XMP) → EXIF form for the shared check
        let exif = format!("{}:{}:{} {}", &v[0..4], &v[5..7], &v[8..10], &v[11..19]);
        (v.as_bytes()[4] == b'-' && v.as_bytes()[10] == b'T').then(|| exif_date(&exif)).flatten()
    })
}

fn exif_ifd(t: &Tiff, ifd: u32, h: &mut Header) {
    let p = &mut h.props;
    let mut maker_note = None;
    for e in t.entries(ifd) {
        match e.tag {
            0x829A => p.time = p.time.or(positive(t.number(&e))),
            0x829D => p.aperture = p.aperture.or(positive(t.number(&e))),
            0x8827 => p.iso = p.iso.or(positive(t.number(&e)).map(|v| v.round() as i64)),
            0x9003 => p.capture = p.capture.take().or_else(|| t.text(&e).as_deref().and_then(exif_date)),
            0x9204 => h.exposure_bias = h.exposure_bias.or(t.number(&e).filter(|v| v.is_finite())),
            0x920A => p.focal = p.focal.or(positive(t.number(&e))),
            0xA405 => h.focal_length_35mm = h.focal_length_35mm.or(t.uint(&e).filter(|v| *v > 0)),
            0xA433 => h.lens_make = h.lens_make.take().or_else(|| t.text(&e)),
            0xA434 => p.lens = p.lens.take().or_else(|| t.text(&e)),
            0x927C => maker_note = Some(e),
            _ => {}
        }
    }
    // older Nikon DSLRs (D300 …) write the ISO only into the makernote
    if let Some(e) = maker_note.filter(|_| p.iso.is_none() && p.maker.as_deref().is_some_and(|m| m.to_ascii_uppercase().starts_with("NIKON"))) {
        p.iso = nikon_iso(t, &e);
    }
}

/// ISO from the Nikon makernote (type 3: "Nikon\0", version, own TIFF header at byte 10; tag 0x0002 = [0, ISO]).
fn nikon_iso(t: &Tiff, e: &Entry) -> Option<i64> {
    if e.ty != 7 || e.count < 18 {
        return None;
    }
    let start = t.u32_of(&e.field) as u64;
    if t.b.get(start, 6)? != b"Nikon\0" {
        return None;
    }
    let inner = Tiff::new(t.b.sub(start + 10)?)?;
    let ifd = inner.ifd0()?;
    let entries = inner.entries(ifd);
    let iso = entries.iter().find(|e| e.tag == 0x0002 && e.ty == 3).and_then(|e| positive(inner.number_at(e, 1))).map(|v| v.round() as i64);
    // ISOInfo (0x0025): first byte n, ISO = 100 · 2^((n − 60) / 12), shown as the nominal value (n = 68 → 160)
    iso.or_else(|| {
        let e = entries.iter().find(|e| e.tag == 0x0025 && e.ty == 7)?;
        let n = *inner.data(e, 1)?.first()?;
        (n > 0).then(|| nominal_iso(100.0 * 2f64.powf((n as f64 - 60.0) / 12.0)))
    })
}

/// The nearest value of the usual third-stop ISO row (… 100, 125, 160, 200, 250, 320, 400, 500, 640, 800, 1000 …)
/// when within 3 %, else rounded.
fn nominal_iso(v: f64) -> i64 {
    const ROW: [f64; 10] = [100.0, 125.0, 160.0, 200.0, 250.0, 320.0, 400.0, 500.0, 640.0, 800.0];
    (-1..=4)
        .flat_map(|k| ROW.iter().map(move |r| r * 10f64.powi(k)))
        .find(|n| (v - n).abs() <= 0.03 * n)
        .map_or(v.round() as i64, |n| n.round() as i64)
}

fn gps_ifd(t: &Tiff, ifd: u32) -> Option<(f64, f64)> {
    let (mut lat, mut lon, mut lat_ref, mut lon_ref) = (None, None, None, None);
    for e in t.entries(ifd) {
        match e.tag {
            1 => lat_ref = t.text(&e),
            2 => lat = degrees(t, &e),
            3 => lon_ref = t.text(&e),
            4 => lon = degrees(t, &e),
            _ => {}
        }
    }
    let sign = |v: f64, r: Option<String>, neg: &str| if r.is_some_and(|r| r.trim().eq_ignore_ascii_case(neg)) { -v.abs() } else { v.abs() };
    let (lat, lon) = (sign(lat?, lat_ref, "S"), sign(lon?, lon_ref, "W"));
    // (0, 0) is what cameras write without a fix
    (lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat != 0.0 || lon != 0.0)).then_some((lat, lon))
}

/// Degrees, minutes, seconds → degrees.
fn degrees(t: &Tiff, e: &Entry) -> Option<f64> {
    let d = t.number_at(e, 0)?;
    let m = t.number_at(e, 1).unwrap_or(0.0);
    let s = t.number_at(e, 2).unwrap_or(0.0);
    let v = d + m / 60.0 + s / 3600.0;
    v.is_finite().then_some(v)
}

/// TIFF-based file (TIFF, DNG and most RAWs): the TIFF starts at byte 0.
fn tiff_file(src: &Src, head: &[u8], h: &mut Header) -> bool {
    // ORF and RW2 use their own magic number in place of 42; byte order and IFD0 offset are standard
    let le = head.starts_with(b"II");
    let ifd = if le { le32(head, 4) } else { be32(head, 4) };
    let Some(ifd) = ifd else { return false };
    let t = Tiff { b: Bytes::File(src, 0), le };
    // a plain TIFF: magic 42 and not a CR2 ("CR" behind the header); RAW sizes come from the decoder
    let plain = matches!(&head[..4], b"II*\0" | b"MM\0*") && &head[8..10] != b"CR";
    tiff_into(&t, ifd, plain, h)
}

/// EXIF block (TIFF header first; a leading "Exif\0\0" is skipped).
fn exif_block(b: &[u8], h: &mut Header) -> bool {
    let b = b.strip_prefix(b"Exif\0\0").unwrap_or(b);
    let Some(t) = Tiff::new(Bytes::Mem(b)) else { return false };
    let Some(ifd) = t.ifd0() else { return false };
    tiff_into(&t, ifd, false, h)
}

// ───────────────────────────────────────────── containers

/// JPEG starting at `at`: the first APP1 "Exif" segment; size from the frame header.
fn jpeg(src: &Src, at: u64, h: &mut Header) -> bool {
    let mut pos = at + 2;
    let mut found = false;
    let mut size = None;
    // segments up to the start of the scan (a few dozen at most)
    for _ in 0..256 {
        let Some(m) = src.exact(pos, 4) else { break };
        if m[0] != 0xFF {
            break;
        }
        let marker = m[1];
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            pos += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = be16(&m, 2).unwrap_or(0) as u64;
        if len < 2 {
            break;
        }
        if marker == 0xE1 && !found {
            if let Some(seg) = src.exact(pos + 4, len as usize - 2) {
                if seg.starts_with(b"Exif\0") {
                    found = exif_block(&seg[6.min(seg.len())..], h);
                }
            }
        }
        // SOF0 … SOF15 without DHT (C4), JPG (C8), DAC (CC)
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) && size.is_none() {
            if let Some(f) = src.exact(pos + 4, 5) {
                size = Some((be16(&f, 3).unwrap_or(0) as u32, be16(&f, 1).unwrap_or(0) as u32));
            }
        }
        pos += 2 + len;
    }
    if let Some((w, hh)) = size.filter(|(w, h)| *w > 0 && *h > 0) {
        // only for a JPEG file; an embedded one (RAF, RW2) does not have the size of the RAW
        if at == 0 {
            h.props.width = h.props.width.or(Some(w));
            h.props.height = h.props.height.or(Some(hh));
        }
    }
    found || (at == 0 && size.is_some())
}

/// PNG: size from IHDR, EXIF from `eXIf` (before the image data, where the standard puts it).
fn png(src: &Src, h: &mut Header) -> bool {
    let mut pos = 8u64;
    for _ in 0..10_000 {
        let Some(c) = src.exact(pos, 8) else { break };
        let len = be32(&c, 0).unwrap_or(0) as u64;
        match &c[4..8] {
            b"IHDR" => {
                if let Some(d) = src.exact(pos + 8, 8) {
                    h.props.width = be32(&d, 0).filter(|v| *v > 0);
                    h.props.height = be32(&d, 4).filter(|v| *v > 0);
                }
            }
            b"eXIf" => {
                if let Some(d) = src.exact(pos + 8, len as usize) {
                    exif_block(&d, h);
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    h.props.width.is_some()
}

/// WebP (RIFF): size from VP8X/VP8/VP8L, EXIF from the `EXIF` chunk.
fn webp(src: &Src, h: &mut Header) -> bool {
    let Some(riff) = src.exact(4, 4) else { return false };
    let end = (le32(&riff, 0).unwrap_or(0) as u64 + 8).min(src.len);
    let mut pos = 12u64;
    let mut ok = false;
    while pos + 8 <= end {
        let Some(c) = src.exact(pos, 8) else { break };
        let len = le32(&c, 4).unwrap_or(0) as u64;
        let data = pos + 8;
        match &c[0..4] {
            b"VP8X" => {
                if let Some(d) = src.exact(data, 10) {
                    let w = 1 + (d[4] as u32 | (d[5] as u32) << 8 | (d[6] as u32) << 16);
                    let hh = 1 + (d[7] as u32 | (d[8] as u32) << 8 | (d[9] as u32) << 16);
                    (h.props.width, h.props.height) = (Some(w), Some(hh));
                    ok = true;
                }
            }
            b"VP8 " if h.props.width.is_none() => {
                if let Some(d) = src.exact(data, 10) {
                    h.props.width = Some((u16::from_le_bytes([d[6], d[7]]) & 0x3FFF) as u32);
                    h.props.height = Some((u16::from_le_bytes([d[8], d[9]]) & 0x3FFF) as u32);
                    ok = true;
                }
            }
            b"VP8L" if h.props.width.is_none() => {
                if let Some(d) = src.exact(data, 5) {
                    let bits = le32(&d, 1).unwrap_or(0);
                    h.props.width = Some((bits & 0x3FFF) + 1);
                    h.props.height = Some(((bits >> 14) & 0x3FFF) + 1);
                    ok = true;
                }
            }
            b"EXIF" => {
                if let Some(d) = src.exact(data, len as usize) {
                    ok |= exif_block(&d, h);
                }
            }
            _ => {}
        }
        pos = data + len + (len & 1);
    }
    ok
}

/// Photoshop: image resource 0x0422 (EXIF data 1); size from the file header.
fn psd(src: &Src, h: &mut Header) -> bool {
    let Some(head) = src.exact(0, 26) else { return false };
    h.props.height = be32(&head, 14);
    h.props.width = be32(&head, 18);
    let Some(mode) = src.exact(26, 4).and_then(|b| be32(&b, 0)) else { return true };
    let res = 30 + mode as u64;
    let Some(res_len) = src.exact(res, 4).and_then(|b| be32(&b, 0)) else { return true };
    let (mut pos, end) = (res + 4, res + 4 + res_len as u64);
    while pos + 12 <= end {
        let Some(b) = src.exact(pos, 4 + 2 + 1) else { break };
        if &b[..4] != b"8BIM" {
            break;
        }
        let id = be16(&b, 4).unwrap_or(0);
        let name_len = b[6] as u64;
        let name = (1 + name_len + 1) & !1;
        let Some(size) = src.exact(pos + 6 + name, 4).and_then(|s| be32(&s, 0)) else { break };
        let data = pos + 6 + name + 4;
        if id == 0x0422 {
            if let Some(d) = src.exact(data, size as usize) {
                exif_block(&d, h);
            }
            break;
        }
        pos = data + ((size as u64 + 1) & !1);
    }
    true
}

/// Fujifilm RAF: the header points to the embedded JPEG, whose EXIF has everything.
fn raf(src: &Src, h: &mut Header) -> bool {
    let Some(d) = src.exact(84, 8) else { return false };
    let off = be32(&d, 0).unwrap_or(0) as u64;
    off > 0 && jpeg(src, off, h)
}

/// Minolta MRW: blocks after the header; `\0TTW` holds a TIFF.
fn mrw(src: &Src, h: &mut Header) -> bool {
    let Some(d) = src.exact(4, 4) else { return false };
    let end = 8 + be32(&d, 0).unwrap_or(0) as u64;
    let mut pos = 8u64;
    while pos + 8 <= end {
        let Some(b) = src.exact(pos, 8) else { break };
        let len = be32(&b, 4).unwrap_or(0) as u64;
        if &b[..4] == b"\0TTW" {
            let Some(t) = Tiff::new(Bytes::File(src, pos + 8)) else { return false };
            let Some(ifd) = t.ifd0() else { return false };
            return tiff_into(&t, ifd, false, h);
        }
        pos += 8 + len;
    }
    false
}

/// ISO base media file boxes: (type, start of the payload, end).
fn boxes(src: &Src, mut pos: u64, end: u64) -> Vec<([u8; 4], u64, u64)> {
    let mut out = Vec::new();
    while pos + 8 <= end && out.len() < 4096 {
        let Some(b) = src.exact(pos, 16).or_else(|| src.exact(pos, 8)) else { break };
        let size32 = be32(&b, 0).unwrap_or(0) as u64;
        let kind: [u8; 4] = [b[4], b[5], b[6], b[7]];
        let (head, size) = match size32 {
            1 => (16, be64(&b, 8).unwrap_or(0)),
            0 => (8, end - pos),
            s => (8, s),
        };
        if size < head || pos + size > end {
            break;
        }
        out.push((kind, pos + head, pos + size));
        pos += size;
    }
    out
}

fn find_box(list: &[([u8; 4], u64, u64)], kind: &[u8; 4]) -> Option<(u64, u64)> {
    list.iter().find(|(k, _, _)| k == kind).map(|(_, s, e)| (*s, *e))
}

/// Canon CR3: `moov` → Canon box (uuid 85c0b687…) → CMT1 (IFD0), CMT2 (EXIF IFD), CMT4 (GPS IFD), each a TIFF.
fn cr3(src: &Src, h: &mut Header) -> bool {
    const CANON: [u8; 16] = [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
    let top = boxes(src, 0, src.len);
    let Some((ms, me)) = find_box(&top, b"moov") else { return false };
    for (kind, s, e) in boxes(src, ms, me) {
        if &kind != b"uuid" || src.exact(s, 16).as_deref() != Some(&CANON[..]) {
            continue;
        }
        let mut ok = false;
        for (k, cs, _) in boxes(src, s + 16, e) {
            let Some(t) = Tiff::new(Bytes::File(src, cs)) else { continue };
            let Some(ifd) = t.ifd0() else { continue };
            match &k {
                b"CMT1" => ok |= tiff_into(&t, ifd, false, h),
                b"CMT2" => {
                    exif_ifd(&t, ifd, h);
                    ok = true;
                }
                b"CMT4" => h.gps = h.gps.or_else(|| gps_ifd(&t, ifd)),
                _ => {}
            }
        }
        return ok;
    }
    false
}

/// HEIF/HEIC/AVIF: EXIF item from `meta` (iinf → item ID of type "Exif", iloc → where it is), size from the
/// `ispe` property of the primary item.
fn heif(src: &Src, h: &mut Header) -> bool {
    let top = boxes(src, 0, src.len);
    let Some((ms, me)) = find_box(&top, b"meta") else { return false };
    // full box: version and flags first
    let inner = boxes(src, ms + 4, me);
    let mut primary = None;
    if let Some((s, _)) = find_box(&inner, b"pitm") {
        primary = src.exact(s, 8).and_then(|b| if b[0] == 0 { be16(&b, 4).map(u32::from) } else { be32(&b, 4) });
    }
    // item IDs of the EXIF blocks
    let mut exif_items = Vec::new();
    if let Some((s, e)) = find_box(&inner, b"iinf") {
        let Some(v) = src.exact(s, 8) else { return false };
        let first = if v[0] == 0 { s + 6 } else { s + 8 };
        for (k, is, _) in boxes(src, first, e) {
            if &k != b"infe" {
                continue;
            }
            // version 2: item ID 16 bit, version 3: 32 bit; then protection index (16 bit) and item type
            let Some(b) = src.get(is, 14) else { continue };
            let (id, ty) = match b[0] {
                2 => (be16(&b, 4).map(u32::from), b.get(8..12)),
                3 => (be32(&b, 4), b.get(10..14)),
                _ => continue,
            };
            if ty == Some(b"Exif") {
                exif_items.extend(id);
            }
        }
    }
    // size: ipco lists the properties, ipma says which belong to which item
    if let Some((ps, pe)) = find_box(&inner, b"iprp") {
        let ip = boxes(src, ps, pe);
        let props: Vec<_> = find_box(&ip, b"ipco").map(|(s, e)| boxes(src, s, e)).unwrap_or_default();
        let mut assoc: Vec<u16> = Vec::new();
        if let (Some((s, e)), Some(p)) = (find_box(&ip, b"ipma"), primary) {
            if let Some(b) = src.get(s, (e - s).min(1 << 16) as usize) {
                let (version, flags) = (b[0], b[3]);
                let mut o = 4usize;
                let n = be32(&b, o).unwrap_or(0);
                o += 4;
                for _ in 0..n {
                    let id = if version < 1 { be16(&b, o).map(u32::from) } else { be32(&b, o) };
                    o += if version < 1 { 2 } else { 4 };
                    let Some(&count) = b.get(o) else { break };
                    o += 1;
                    let mut list = Vec::new();
                    for _ in 0..count {
                        let idx = if flags & 1 != 0 {
                            let v = be16(&b, o).unwrap_or(0) & 0x7FFF;
                            o += 2;
                            v
                        } else {
                            let v = (*b.get(o).unwrap_or(&0) & 0x7F) as u16;
                            o += 1;
                            v
                        };
                        list.push(idx);
                    }
                    if id == Some(p) {
                        assoc = list;
                    }
                }
            }
        }
        for idx in assoc {
            if let Some((k, s, _)) = (idx as usize).checked_sub(1).and_then(|i| props.get(i)) {
                if k == b"ispe" {
                    if let Some(b) = src.exact(*s, 12) {
                        h.props.width = be32(&b, 4).filter(|v| *v > 0);
                        h.props.height = be32(&b, 8).filter(|v| *v > 0);
                    }
                }
            }
        }
    }
    let mut ok = h.props.width.is_some();
    if exif_items.is_empty() {
        return ok;
    }
    let Some((ls, le)) = find_box(&inner, b"iloc") else { return ok };
    let Some(b) = src.get(ls, (le - ls).min(1 << 16) as usize) else { return ok };
    let version = b[0];
    let (os, lsz) = ((b[4] >> 4) as usize, (b[4] & 15) as usize);
    let (bs, is) = ((b[5] >> 4) as usize, if version >= 1 { (b[5] & 15) as usize } else { 0 });
    let mut o = 6usize;
    let n = if version < 2 { be16(&b, o).map(u32::from) } else { be32(&b, o) }.unwrap_or(0);
    o += if version < 2 { 2 } else { 4 };
    let uint = |b: &[u8], o: usize, n: usize| -> Option<u64> {
        match n {
            0 => Some(0),
            4 => be32(b, o).map(u64::from),
            8 => be64(b, o),
            _ => None,
        }
    };
    for _ in 0..n {
        let Some(id) = (if version < 2 { be16(&b, o).map(u32::from) } else { be32(&b, o) }) else { break };
        o += if version < 2 { 2 } else { 4 };
        let method = if version >= 1 {
            let m = be16(&b, o).unwrap_or(0) & 15;
            o += 2;
            m
        } else {
            0
        };
        o += 2; // data reference index
        let Some(base) = uint(&b, o, bs) else { break };
        o += bs;
        let Some(extents) = be16(&b, o) else { break };
        o += 2;
        let mut first = None;
        for _ in 0..extents {
            o += is;
            let (Some(off), Some(len)) = (uint(&b, o, os), uint(&b, o + os, lsz)) else { break };
            o += os + lsz;
            first.get_or_insert((off, len));
        }
        if method != 0 || !exif_items.contains(&id) {
            continue;
        }
        let Some((off, len)) = first else { continue };
        let at = base + off;
        let len = if len == 0 { src.len.saturating_sub(at) } else { len };
        // the item starts with the offset of the TIFF header behind these 4 bytes
        if let Some(d) = src.exact(at, len.min(1 << 20) as usize) {
            let skip = be32(&d, 0).unwrap_or(0) as usize;
            if let Some(tiff) = d.get(4 + skip..) {
                ok |= exif_block(tiff, h);
            }
        }
        break;
    }
    ok
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a small big- or little-endian TIFF block with IFD0, EXIF and GPS IFD (test data).
    pub struct TiffBuilder {
        le: bool,
    }

    /// One entry: tag, type, values (raw bytes in the chosen byte order).
    type Field = (u16, u16, u32, Vec<u8>);

    impl TiffBuilder {
        pub fn new(le: bool) -> Self {
            TiffBuilder { le }
        }
        fn u16(&self, v: u16) -> Vec<u8> {
            if self.le { v.to_le_bytes().to_vec() } else { v.to_be_bytes().to_vec() }
        }
        fn u32(&self, v: u32) -> Vec<u8> {
            if self.le { v.to_le_bytes().to_vec() } else { v.to_be_bytes().to_vec() }
        }
        pub fn ascii(&self, tag: u16, s: &str) -> Field {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            (tag, 2, b.len() as u32, b)
        }
        pub fn short(&self, tag: u16, v: u16) -> Field {
            (tag, 3, 1, self.u16(v))
        }
        pub fn rational(&self, tag: u16, vals: &[(u32, u32)]) -> Field {
            let b = vals.iter().flat_map(|(n, d)| [self.u32(*n), self.u32(*d)].concat()).collect();
            (tag, 5, vals.len() as u32, b)
        }
        pub fn srational(&self, tag: u16, n: i32, d: i32) -> Field {
            (tag, 10, 1, [self.u32(n as u32), self.u32(d as u32)].concat())
        }

        /// TIFF with IFD0 (`ifd0`), EXIF IFD (`exif`) and GPS IFD (`gps`, empty = none).
        pub fn build(&self, ifd0: Vec<Field>, exif: Vec<Field>, gps: Vec<Field>) -> Vec<u8> {
            let mut out = if self.le { b"II*\0".to_vec() } else { b"MM\0*".to_vec() };
            out.extend(self.u32(8));
            let mut ifd0 = ifd0;
            // pointer entries get their values while laying out
            ifd0.push((0x8769, 4, 1, self.u32(0)));
            if !gps.is_empty() {
                ifd0.push((0x8825, 4, 1, self.u32(0)));
            }
            ifd0.sort_by_key(|f| f.0);
            let ifds = [ifd0, exif, gps];
            let ifd_len = |f: &Vec<Field>| 2 + f.len() * 12 + 4;
            let mut starts = Vec::new();
            let mut pos = 8usize;
            for (i, f) in ifds.iter().enumerate() {
                starts.push(pos);
                if !(f.is_empty() && i == 2) {
                    pos += ifd_len(f);
                }
            }
            let mut data_pos = pos;
            let mut data = Vec::new();
            for (i, f) in ifds.iter().enumerate() {
                // an empty EXIF IFD is written (IFD0 points to it), an empty GPS IFD is left out (no pointer)
                if f.is_empty() && i == 2 {
                    continue;
                }
                out.extend(self.u16(f.len() as u16));
                for (tag, ty, count, val) in f {
                    out.extend(self.u16(*tag));
                    out.extend(self.u16(*ty));
                    out.extend(self.u32(*count));
                    let v = match tag {
                        0x8769 if i == 0 => self.u32(starts[1] as u32),
                        0x8825 if i == 0 => self.u32(starts[2] as u32),
                        _ => val.clone(),
                    };
                    if v.len() <= 4 {
                        let mut v = v;
                        v.resize(4, 0);
                        out.extend(v);
                    } else {
                        out.extend(self.u32(data_pos as u32));
                        data.extend(&v);
                        data_pos += v.len();
                        if v.len() % 2 == 1 {
                            data.push(0);
                            data_pos += 1;
                        }
                    }
                }
                out.extend(self.u32(0));
            }
            out.extend(data);
            out
        }
    }

    /// The test block: Canon EOS R5, 2024:05:11 14:03:22, 1/250 s, f/5.6, ISO 400, 35 mm, lens, orientation 6,
    /// position 48°8'0"N 11°34'30"W.
    pub fn sample(le: bool) -> Vec<u8> {
        let t = TiffBuilder::new(le);
        t.build(
            vec![t.ascii(0x010F, "Canon"), t.ascii(0x0110, "Canon EOS R5  "), t.short(0x0112, 6), t.ascii(0x0132, "2024:05:12 09:00:00")],
            vec![
                t.rational(0x829A, &[(1, 250)]),
                t.rational(0x829D, &[(56, 10)]),
                t.short(0x8827, 400),
                t.ascii(0x9003, "2024:05:11 14:03:22"),
                t.srational(0x9204, -2, 3),
                t.rational(0x920A, &[(35, 1)]),
                t.ascii(0xA434, "RF24-105mm F4 L IS USM"),
            ],
            vec![t.ascii(1, "N"), t.rational(2, &[(48, 1), (8, 1), (0, 1)]), t.ascii(3, "W"), t.rational(4, &[(11, 1), (34, 1), (30, 1)])],
        )
    }

    /// Smallest valid baseline JPEG (8 × 8 gray), with `exif` as APP1 segment when given.
    pub fn jpeg_with(exif: Option<&[u8]>) -> Vec<u8> {
        let mut plain = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut plain).encode(&[128u8; 64], 8, 8, image::ExtendedColorType::L8).unwrap();
        let Some(exif) = exif else { return plain };
        let mut out = plain[..2].to_vec();
        let len = (2 + 6 + exif.len()) as u16;
        out.extend([0xFF, 0xE1]);
        out.extend(len.to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend(exif);
        out.extend(&plain[2..]);
        out
    }

    pub fn png_with(exif: &[u8]) -> Vec<u8> {
        let mut plain = Vec::new();
        image::codecs::png::PngEncoder::new(&mut plain).write_image(&[128u8; 64], 8, 8, image::ExtendedColorType::L8).unwrap();
        // eXIf right after IHDR (8 signature + 25 IHDR)
        let mut out = plain[..33].to_vec();
        out.extend((exif.len() as u32).to_be_bytes());
        let mut chunk = b"eXIf".to_vec();
        chunk.extend(exif);
        out.extend(&chunk);
        out.extend(crc32(&chunk).to_be_bytes());
        out.extend(&plain[33..]);
        out
    }

    fn crc32(b: &[u8]) -> u32 {
        let mut c = !0u32;
        for &x in b {
            c ^= x as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    use image::ImageEncoder;

    pub fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-exif-header-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn check_sample(h: &Header, what: &str) {
        let p = &h.props;
        assert_eq!(p.maker.as_deref(), Some("Canon"), "{what}");
        assert_eq!(p.camera.as_deref(), Some("Canon EOS R5"), "{what}: trimmed");
        assert_eq!(p.orientation, Some(6), "{what}");
        assert_eq!(p.capture.as_deref(), Some("2024-05-11T14:03:22"), "{what}: DateTimeOriginal wins over DateTime");
        assert_eq!(p.time, Some(1.0 / 250.0), "{what}");
        assert_eq!(p.aperture, Some(5.6), "{what}");
        assert_eq!(p.iso, Some(400), "{what}");
        assert_eq!(p.focal, Some(35.0), "{what}");
        assert_eq!(p.lens.as_deref(), Some("RF24-105mm F4 L IS USM"), "{what}");
        assert!((h.exposure_bias.unwrap() + 2.0 / 3.0).abs() < 1e-9, "{what}");
        let (lat, lon) = h.gps.unwrap();
        assert!((lat - (48.0 + 8.0 / 60.0)).abs() < 1e-9 && (lon + (11.0 + 34.0 / 60.0 + 30.0 / 3600.0)).abs() < 1e-9, "{what}: {lat} {lon}");
    }

    #[test]
    fn tiff_jpeg_png_in_both_byte_orders() {
        let d = scratch("containers");
        for le in [true, false] {
            let exif = sample(le);
            let files = [("t.tif", exif.clone()), ("t.jpg", jpeg_with(Some(&exif))), ("t.png", png_with(&exif))];
            for (name, bytes) in files {
                let p = d.join(name);
                std::fs::write(&p, &bytes).unwrap();
                let h = read(&p).unwrap_or_else(|| panic!("{name}"));
                check_sample(&h, &format!("{name} le={le}"));
                if name != "t.tif" {
                    assert_eq!((h.props.width, h.props.height), (Some(8), Some(8)), "{name}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn heif_cr3_raf_and_webp_containers() {
        let d = scratch("isobmff");
        let exif = sample(false);
        let bx = |kind: &[u8; 4], body: &[u8]| -> Vec<u8> { [((8 + body.len()) as u32).to_be_bytes().to_vec(), kind.to_vec(), body.to_vec()].concat() };
        // HEIF: ftyp, meta (pitm, iinf with one Exif item, iprp with ispe, iloc), mdat with the item
        let mut item = 6u32.to_be_bytes().to_vec();
        item.extend(b"Exif\0\0");
        item.extend(&exif);
        let infe = bx(b"infe", &[&[2u8, 0, 0, 0][..], &1u16.to_be_bytes(), &0u16.to_be_bytes(), b"Exif", b"\0"].concat());
        let iinf = bx(b"iinf", &[&[0u8, 0, 0, 0][..], &1u16.to_be_bytes(), &infe].concat());
        let pitm = bx(b"pitm", &[0u8, 0, 0, 0, 0, 2]);
        let ispe = bx(b"ispe", &[&[0u8; 4][..], &4032u32.to_be_bytes(), &3024u32.to_be_bytes()].concat());
        let ipco = bx(b"ipco", &ispe);
        let ipma = bx(b"ipma", &[&[0u8, 0, 0, 0][..], &1u32.to_be_bytes(), &2u16.to_be_bytes(), &[1u8, 0x81]].concat());
        let iprp = bx(b"iprp", &[ipco, ipma].concat());
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        // iloc version 0: offset and length 32 bit, no base offset; one item with one extent
        let build = |offset: u32| -> Vec<u8> {
            let iloc = bx(b"iloc", &[&[0u8, 0, 0, 0, 0x44, 0x00][..], &1u16.to_be_bytes(), &1u16.to_be_bytes(), &0u16.to_be_bytes(), &1u16.to_be_bytes(), &offset.to_be_bytes(), &(item.len() as u32).to_be_bytes()].concat());
            let meta = bx(b"meta", &[&[0u8; 4][..], &pitm, &iinf, &iprp, &iloc].concat());
            [ftyp.clone(), meta].concat()
        };
        let head_len = build(0).len() as u32;
        let mut heic = build(head_len + 8);
        heic.extend(bx(b"mdat", &item));
        let p = d.join("t.heic");
        std::fs::write(&p, &heic).unwrap();
        let h = read(&p).expect("heic");
        check_sample(&h, "heic");
        assert_eq!((h.props.width, h.props.height), (Some(4032), Some(3024)));

        // CR3: CMT1 = IFD0 (with camera), CMT2 = EXIF IFD as its own TIFF, CMT4 = GPS
        let t = TiffBuilder::new(true);
        let cmt1 = t.build(vec![t.ascii(0x010F, "Canon"), t.ascii(0x0110, "Canon EOS R5"), t.short(0x0112, 6)], vec![], vec![]);
        let cmt2_ifd = t.build(vec![t.rational(0x829A, &[(1, 250)]), t.rational(0x829D, &[(56, 10)]), t.short(0x8827, 400), t.ascii(0x9003, "2024:05:11 14:03:22"), t.srational(0x9204, -2, 3), t.rational(0x920A, &[(35, 1)]), t.ascii(0xA434, "RF24-105mm F4 L IS USM")], vec![], vec![]);
        // the builder always adds an EXIF pointer to IFD0; harmless here (points to an empty IFD)
        let gps = t.build(vec![t.ascii(1, "N"), t.rational(2, &[(48, 1), (8, 1), (0, 1)]), t.ascii(3, "W"), t.rational(4, &[(11, 1), (34, 1), (30, 1)])], vec![], vec![]);
        const CANON: [u8; 16] = [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
        let uuid = bx(b"uuid", &[&CANON[..], &bx(b"CMT1", &cmt1), &bx(b"CMT2", &cmt2_ifd), &bx(b"CMT4", &gps)].concat());
        let cr3 = [bx(b"ftyp", b"crx \0\0\0\x01crx isom"), bx(b"moov", &uuid)].concat();
        let p = d.join("t.cr3");
        std::fs::write(&p, &cr3).unwrap();
        check_sample(&read(&p).expect("cr3"), "cr3");

        // RAF: header with the offset of the embedded JPEG at byte 84
        let jpg = jpeg_with(Some(&exif));
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(100, 0);
        raf[84..88].copy_from_slice(&100u32.to_be_bytes());
        raf[88..92].copy_from_slice(&(jpg.len() as u32).to_be_bytes());
        raf.extend(&jpg);
        let p = d.join("t.raf");
        std::fs::write(&p, &raf).unwrap();
        let h = read(&p).expect("raf");
        check_sample(&h, "raf");
        assert_eq!(h.props.width, None, "the embedded JPEG does not have the size of the RAW");

        // WebP: VP8X with canvas size, EXIF chunk
        let mut vp8x = vec![0x08u8, 0, 0, 0];
        vp8x.extend([(1999u32 & 0xFF) as u8, (1999 >> 8) as u8, 0, (1499u32 & 0xFF) as u8, (1499 >> 8) as u8, 0]);
        let chunk = |k: &[u8; 4], b: &[u8]| -> Vec<u8> {
            let mut c = k.to_vec();
            c.extend((b.len() as u32).to_le_bytes());
            c.extend(b);
            if b.len() % 2 == 1 {
                c.push(0);
            }
            c
        };
        let body = [b"WEBP".to_vec(), chunk(b"VP8X", &vp8x), chunk(b"EXIF", &exif)].concat();
        let webp = [b"RIFF".to_vec(), (body.len() as u32).to_le_bytes().to_vec(), body].concat();
        let p = d.join("t.webp");
        std::fs::write(&p, &webp).unwrap();
        let h = read(&p).expect("webp");
        check_sample(&h, "webp");
        assert_eq!((h.props.width, h.props.height), (Some(2000), Some(1500)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nominal_iso_row() {
        let iso = |n: f64| nominal_iso(100.0 * 2f64.powf((n - 60.0) / 12.0));
        assert_eq!([iso(60.0), iso(64.0), iso(68.0), iso(72.0), iso(84.0), iso(96.0), iso(104.0), iso(108.0), iso(56.0)], [100, 125, 160, 200, 400, 800, 1250, 1600, 80]);
    }

    #[test]
    fn capture_time_from_xmp() {
        assert_eq!(xmp_date(br#"<rdf:Description photoshop:DateCreated="2017-01-12T19:32:52" xmp:CreateDate="2018-01-01T00:00:00">"#).as_deref(), Some("2017-01-12T19:32:52"));
        assert_eq!(xmp_date(b"<exif:DateTimeOriginal>2020-02-03T04:05:06.12+01:00</exif:DateTimeOriginal>").as_deref(), Some("2020-02-03T04:05:06"));
        assert_eq!(xmp_date(br#"xmp:CreateDate="2017""#), None);
    }

    /// Broken and hostile input: no panic, no endless loop, little reading.
    #[test]
    fn damaged_files_are_harmless() {
        let d = scratch("damaged");
        let exif = sample(true);
        let full = jpeg_with(Some(&exif));
        let mut cases: Vec<(&str, Vec<u8>)> = vec![("empty", vec![]), ("short", b"II*\0".to_vec()), ("text", b"hello world, not an image".to_vec())];
        for cut in [3usize, 10, 30, 60, 120, 200] {
            cases.push(("cut", full[..cut.min(full.len())].to_vec()));
        }
        // IFD loop: IFD0 offset points to itself with a huge entry count
        let mut lp = b"II*\0\x08\0\0\0".to_vec();
        lp.extend(0xFFFFu16.to_le_bytes());
        cases.push(("loop", lp));
        // offsets beyond the end
        let mut far = exif.clone();
        far[4..8].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
        cases.push(("far", far));
        for (name, b) in cases {
            let p = d.join(format!("{name}.bin"));
            std::fs::write(&p, &b).unwrap();
            let _ = read(&p);
        }
        // a large file whose metadata lies at the start: only one block is read
        let p = d.join("big.tif");
        let mut big = exif.clone();
        big.resize(20 << 20, 0);
        std::fs::write(&p, &big).unwrap();
        let src = Src::open(&p).unwrap();
        let mut h = Header::default();
        let head = src.get(0, 32).unwrap();
        assert!(tiff_file(&src, &head, &mut h));
        assert_eq!(*src.read.borrow(), BLOCK, "only the first block of a 20 MB file");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Fields of the two readers that differ (numbers with a relative tolerance), as "field: ImageIO | own".
    #[cfg(target_os = "macos")]
    fn differences(a: &Properties, b: &Properties, ga: Option<(f64, f64)>, gb: Option<(f64, f64)>, with_size: bool) -> Vec<String> {
        let mut out = Vec::new();
        let num = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-6 * x.abs().max(y.abs()).max(1e-9),
            (None, None) => true,
            _ => false,
        };
        let mut t = |name: &str, x: &Option<String>, y: &Option<String>| {
            if x != y {
                out.push(format!("{name}: {x:?} | {y:?}"));
            }
        };
        t("maker", &a.maker, &b.maker);
        t("camera", &a.camera, &b.camera);
        t("lens", &a.lens, &b.lens);
        t("capture", &a.capture, &b.capture);
        for (name, x, y) in [("aperture", a.aperture, b.aperture), ("focal", a.focal, b.focal), ("time", a.time, b.time), ("iso", a.iso.map(|v| v as f64), b.iso.map(|v| v as f64))] {
            if !num(x, y) {
                out.push(format!("{name}: {x:?} | {y:?}"));
            }
        }
        if a.orientation.unwrap_or(1) != b.orientation.unwrap_or(1) {
            out.push(format!("orientation: {:?} | {:?}", a.orientation, b.orientation));
        }
        if with_size && (a.width, a.height) != (b.width, b.height) {
            out.push(format!("size: {:?}x{:?} | {:?}x{:?}", a.width, a.height, b.width, b.height));
        }
        if !(num(ga.map(|g| g.0), gb.map(|g| g.0)) && num(ga.map(|g| g.1), gb.map(|g| g.1))) {
            out.push(format!("gps: {ga:?} | {gb:?}"));
        }
        out
    }

    /// Without any outside file: TIFF, JPEG and PNG made here give the same fields with ImageIO and the own reader
    /// (including GPS and orientation; ImageIO is the reference the library uses on the Mac).
    #[cfg(target_os = "macos")]
    #[test]
    fn synthetic_files_agree_with_imageio() {
        let d = scratch("imageio");
        for le in [true, false] {
            let exif = sample(le);
            for (name, bytes) in [("t.tif", exif.clone()), ("t.jpg", jpeg_with(Some(&exif))), ("t.png", png_with(&exif))] {
                let p = d.join(name);
                std::fs::write(&p, &bytes).unwrap();
                let Some(mac) = crate::imageio::properties(&p) else {
                    // ImageIO does not take a TIFF without image data
                    assert_eq!(name, "t.tif");
                    continue;
                };
                let own = read(&p).unwrap();
                let diff = differences(&mac, &own.props, crate::imageio::gps_position(&p), own.gps, name != "t.tif");
                assert!(diff.is_empty(), "{name} le={le}: {diff:#?}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Comparison with ImageIO on real files (`GRAINEO_TEST_EXIF_DIR`, a folder of free sample files; skipped
    /// without it). Prints one line per file (differences, time and bytes read of the own reader); fails only when
    /// the own reader finds nothing where ImageIO does.
    #[cfg(target_os = "macos")]
    #[test]
    fn compare_with_imageio_on_sample_folder() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_EXIF_DIR").map(std::path::PathBuf::from) else {
            eprintln!("GRAINEO_TEST_EXIF_DIR not set, skipped");
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file()).collect();
        files.sort();
        let mut missing = Vec::new();
        for p in files {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let format = crate::format_from_extension(&name).unwrap_or("?");
            if format == "X3F" || format == "X3I" || format == "VIDEO" {
                continue;
            }
            let mac = crate::imageio::properties(&p);
            let t = std::time::Instant::now();
            let src = Src::open(&p).unwrap();
            let own = read_from(&src);
            let us = t.elapsed().as_secs_f64() * 1e6;
            let bytes = *src.read.borrow();
            // what the library stores (exif::read): ImageIO vs. the platform-independent path (with rawler fallback)
            let t = std::time::Instant::now();
            let portable = crate::exif::read_portable(&p, format);
            let us_portable = t.elapsed().as_secs_f64() * 1e6;
            let stored = crate::exif::read(&p, format);
            let mut diff = match (&mac, &own) {
                (Some(m), Some(o)) => differences(m, &o.props, crate::imageio::gps_position(&p), o.gps, !crate::is_raw(format)),
                (Some(_), None) => vec!["own reader: nothing".into()],
                (None, Some(_)) => vec!["ImageIO: nothing".into()],
                (None, None) => vec!["both: nothing".into()],
            };
            if mac.is_some() && own.is_none() {
                missing.push(name.clone());
            }
            let c = |x: &Option<crate::exif::Capture>| x.as_ref().map(|c| (c.camera.clone(), c.lens.clone(), c.capture.clone(), c.aperture, c.focal, c.iso, c.time));
            if c(&stored) != c(&portable) {
                diff.push(format!("exif::read {:?} | portable {:?}", c(&stored), c(&portable)));
            }
            println!("{name}\t{format}\t{us:.0} µs\t{} KiB\tportable {us_portable:.0} µs\t{}", bytes / 1024, if diff.is_empty() { "same".into() } else { diff.join("; ") });
        }
        assert!(missing.is_empty(), "own reader found nothing: {missing:?}");
    }
}
