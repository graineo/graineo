//! Camera picture style from the makernote: Fujifilm film simulation (with dynamic range, highlight/
//! shadow tone, color, grain, Color Chrome), Canon Picture Style, Ricoh image mode ("Image
//! Control"), Sony Creative Style, Nikon Picture Control, Olympus/OM System Picture Mode, Panasonic Photo Style.
//!
//! Own small reader (read-only) based on public documentation of the tag tables (exiftool
//! "FujiFilm Tags", "Canon Tags" → ProcessingInfo, "Sony Tags", "Pentax Tags", "Nikon Tags" → PictureControlData, "Olympus Tags" → CameraSettings,
//! "Panasonic Tags"; exiv2). Nothing is
//! taken from the firmware: only the values the camera itself writes into
//! the file are read. Read from RAW (RAF → embedded JPEG, CR3 → CMT1/CMT3 boxes,
//! DNG → DNGPrivateData or EXIF MakerNote, CR2/ARW/NEF/ORF → EXIF MakerNote, RW2 → EXIF of the embedded JPEG) or
//! camera JPEG.
//! Sigma (fp, fp L, BF; Bayer/DNG): Color Mode as text in makernote tag 0x003d.
//! Ricoh image mode: tag number and values checked against the camera JPEGs of own images.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Picture style read from a shot.
#[derive(Debug, Clone, Default)]
pub struct PictureStyle {
    pub maker: String,
    pub model: String,
    /// key (lowercase, without spaces), e.g. `provia`, `classic-chrome`, `acros-r`,
    /// `standard`, `monochrome` – together with the camera the name of the look
    pub style: String,
    /// further settings (name, value), e.g. dynamic range, highlight/shadow tone, color
    pub details: Vec<(&'static str, String)>,
}

impl PictureStyle {
    /// Short camera name for look names, e.g. `x100`, `x100f`, `eos-rp`, `gr3`, `gr4`.
    pub fn camera_short(&self) -> String {
        camera_short(&self.maker, &self.model)
    }
}

/// Short name of a camera (look names `camera/style`).
pub fn camera_short(maker: &str, model: &str) -> String {
    let h = maker.to_ascii_uppercase();
    let m = model.trim().to_ascii_uppercase();
    let m = m.strip_prefix("FINEPIX ").unwrap_or(&m).to_string();
    let m = m.strip_prefix("CANON ").unwrap_or(&m).to_string();
    let m = m.strip_prefix("RICOH ").unwrap_or(&m).to_string();
    let m = if h.contains("RICOH") || h.contains("PENTAX") {
        m.replace("GR IIIX", "GR3X").replace("GR III", "GR3").replace("GR IV", "GR4").replace("GR II", "GR2")
    } else {
        m
    };
    name_part(&m.to_ascii_lowercase().split_whitespace().collect::<Vec<_>>().join("-"))
}

/// One part of a look name (`camera/style`), also used as a folder or file name: only `A-Z a-z 0-9 . _ -` (anything
/// else becomes `-`), at most 64 characters, never empty and never starting with a dot (no `.`, `..` or hidden
/// files). Camera model and style come from the file (EXIF, makernote text), so a crafted RAW must not be able to
/// name a path outside the looks folder.
pub fn name_part(s: &str) -> String {
    let mut out: String = s.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    let dots = out.len() - out.trim_start_matches('.').len();
    out.replace_range(..dots, &"-".repeat(dots));
    if out.is_empty() {
        out.push_str("unknown");
    }
    out
}

/// Raw makernote entries (for investigation): tag, type, count, first values as number or text.
#[derive(Debug, Clone)]
pub struct Entry {
    pub tag: u16,
    pub type_name: u16,
    pub n: u32,
    pub values: Vec<i64>,
    pub text: Option<String>,
}

/// Read the picture style from a file (RAW or camera JPEG).
pub fn read(path: &Path) -> Option<PictureStyle> {
    // a small head first (a network drive answers 4 MB slowly); taken only when every byte the reader looked at lay
    // inside it, so the result is the same as from the larger reads below
    // the small head, then the usual 4 MB; only when neither was conclusive the old way (whole TIFF-like file)
    if let Some(r) = read_head(path, SMALL_HEAD) {
        return r;
    }
    if let Some(r) = read_head(path, HEAD) {
        return r;
    }
    read_large(path)
}

/// [`read`] without the small head first: the 4 MB head, TIFF-like files completely if needed (measurements compare
/// both ways).
pub fn read_large(path: &Path) -> Option<PictureStyle> {
    let q = Source::load(path, false)?;
    match evaluate(&q) {
        Some(b) if b.style != "unknown" => Some(b),
        r if q.truncated => Source::load(path, true).and_then(|q| evaluate(&q)).or(r),
        r => r,
    }
}

/// Bytes read first (file head; for RAF the head of the embedded JPEG). The makernotes of all cameras seen so far lie
/// within the first 100 KB.
pub const SMALL_HEAD: usize = 256 << 10;
/// The usual head (and the largest part of a RAF's JPEG) read when the small one is not enough.
const HEAD: usize = 4 << 20;
const RAF_JPEG: usize = 1 << 20;

thread_local! {
    /// A read of the TIFF reader ran past the end of its data (set by the accessors, see [`read_head`]).
    static OUT_OF_DATA: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Opening or reading the file failed (see [`read_checked`]).
    static IO_FAILED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A file operation for [`read`]: a failure is noted for [`read_checked`].
fn io<T>(r: std::io::Result<T>) -> Option<T> {
    r.map_err(|_| IO_FAILED.with(|c| c.set(true))).ok()
}

/// [`read`], telling a file that could not be read (`Err`: e.g. a network drive that dropped out; not to be noted
/// as "no picture style") from one without a learnable style (`Ok(None)`).
pub fn read_checked(path: &Path) -> Result<Option<PictureStyle>, ()> {
    IO_FAILED.with(|c| c.set(false));
    let r = read(path);
    if IO_FAILED.with(|c| c.replace(false)) { Err(()) } else { Ok(r) }
}

/// The reader wanted bytes beyond its data.
fn out_of_data() {
    OUT_OF_DATA.with(|c| c.set(true));
}

/// [`read`] from the first `head` bytes alone. Conclusive (`Some`) when every access of the reader lay inside them:
/// a larger read would look at the same bytes and give the same result – also "unknown" or nothing, which the old way
/// found only after reading a TIFF-like file whole (up to 400 MB from a network drive, for a camera without a
/// readable style). `None` when the head is not enough (a structure not in it, or an access past its end): the larger
/// read decides, exactly as before.
fn read_head(path: &Path, head: usize) -> Option<Option<PictureStyle>> {
    let q = Source::load_head(path, head)?;
    OUT_OF_DATA.with(|c| c.set(false));
    let b = evaluate(&q);
    let complete = !OUT_OF_DATA.with(|c| c.replace(false));
    complete.then_some(b)
}

/// Test hook `GRAINEO_SIM_IO=<ms>:<MB/s>` (only together with `GRAINEO_DATA_DIR`): every file read waits like a slow
/// network drive (latency per request plus transfer time), to measure reading without the user's NAS.
pub fn simulate_io(bytes: usize) {
    static SIM: std::sync::OnceLock<Option<(f64, f64)>> = std::sync::OnceLock::new();
    let sim = SIM.get_or_init(|| {
        std::env::var_os("GRAINEO_DATA_DIR")?;
        let v = std::env::var("GRAINEO_SIM_IO").ok()?;
        let (lat, mbps) = v.split_once(':')?;
        Some((lat.parse().ok()?, mbps.parse().ok()?))
    });
    if let Some((lat, mbps)) = *sim {
        // the latency overlaps between readers, the transfer shares one line
        static LINE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        std::thread::sleep(std::time::Duration::from_secs_f64(lat / 1000.0));
        if mbps > 0.0 && bytes > 0 {
            let _g = LINE.lock().unwrap_or_else(|e| e.into_inner());
            std::thread::sleep(std::time::Duration::from_secs_f64(bytes as f64 / (mbps * 1e6)));
        }
    }
}

/// Makernote entries of a file (make, model, entries) – for investigating unknown fields.
pub fn makernote(path: &Path) -> Option<(String, String, Vec<Entry>)> {
    let q = Source::load(path, true)?;
    let e = q.exif()?;
    let mn = e.makernote(&q)?;
    Some((e.make, e.model, mn.all()))
}

// ---------------------------------------------------------------------------------------------
// file → TIFF region(s)

pub(crate) struct Source {
    /// data containing the TIFF structures
    pub(crate) d: Vec<u8>,
    /// start of the TIFF header (EXIF) in `d`
    pub(crate) tiff: usize,
    /// CR3: start of the Canon makernote (CMT3, own TIFF header) in `d`
    pub(crate) cmt3: Option<usize>,
    /// TIFF-like file, only the start read
    pub(crate) truncated: bool,
}

pub(crate) fn read_range(f: &mut std::fs::File, off: u64, len: usize) -> Option<Vec<u8>> {
    let mut v = vec![0u8; len];
    io(f.seek(SeekFrom::Start(off)))?;
    let mut n = 0;
    while n < len {
        match f.read(&mut v[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return io(Err(e)),
        }
    }
    v.truncate(n);
    simulate_io(n);
    Some(v)
}

pub(crate) fn jpeg_exif(d: &[u8]) -> Option<usize> {
    if d.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut p = 2;
    while p + 4 <= d.len() {
        if d[p] != 0xff {
            return None;
        }
        let m = d[p + 1];
        if m == 0xd9 || m == 0xda {
            return None;
        }
        let len = u16::from_be_bytes([d[p + 2], d[p + 3]]) as usize;
        if m == 0xe1 && d.get(p + 4..p + 10) == Some(b"Exif\0\0") {
            return Some(p + 10);
        }
        p += 2 + len;
    }
    None
}

/// Panasonic RW2 ("IIU\0"): position and length of the embedded JPEG (IFD0 tag 0x002e JpgFromRaw).
fn rw2_jpeg(header: &[u8]) -> Option<(u64, usize)> {
    if !header.starts_with(b"IIU\0") {
        return None;
    }
    let t = Tiff { d: header, le: true, basis: 0 };
    let ifd0 = t.ifd(t.u32(4)? as usize, 0)?;
    let e = ifd0.iter().find(|e| e.tag == 0x002e)?;
    let off = t.u32(e.e + 8)? as u64;
    Some((off, e.n as usize))
}

pub(crate) fn find(d: &[u8], needle: &[u8]) -> Option<usize> {
    d.windows(needle.len()).position(|w| w == needle)
}

impl Source {
    /// Like [`Self::load`] with only `head` bytes (and as much of a RAF's JPEG), never the whole file. `None` also when
    /// a structure [`Self::load`] would find in its larger head is not in this one (CR3 boxes, the JPEG's EXIF).
    pub(crate) fn load_head(path: &Path, head: usize) -> Option<Self> {
        let mut f = io(std::fs::File::open(path))?;
        let header = read_range(&mut f, 0, head.min(HEAD))?;
        if header.starts_with(b"FUJIFILMCCD-RAW") {
            let off = u32::from_be_bytes(header.get(84..88)?.try_into().ok()?) as u64;
            let len = u32::from_be_bytes(header.get(88..92)?.try_into().ok()?) as usize;
            let j = read_range(&mut f, off, len.min(head).min(RAF_JPEG))?;
            let tiff = jpeg_exif(&j)?;
            return Some(Self { d: j, tiff, cmt3: None, truncated: false });
        }
        if let Some((off, len)) = rw2_jpeg(&header) {
            let j = read_range(&mut f, off, len.min(head).min(RAF_JPEG))?;
            let tiff = jpeg_exif(&j)?;
            return Some(Self { d: j, tiff, cmt3: None, truncated: false });
        }
        if header.get(4..8) == Some(b"ftyp") {
            // both boxes must be in the head: a CMT3 further on would change the result
            let c1 = find(&header, b"CMT1")? + 4;
            let c3 = find(&header, b"CMT3")? + 4;
            return Some(Self { d: header, tiff: c1, cmt3: Some(c3), truncated: false });
        }
        if let Some(t) = jpeg_exif(&header) {
            return Some(Self { d: header, tiff: t, cmt3: None, truncated: false });
        }
        if header.starts_with(b"II") || header.starts_with(b"MM") {
            return Some(Self { d: header, tiff: 0, cmt3: None, truncated: false });
        }
        None
    }

    pub(crate) fn load(path: &Path, full: bool) -> Option<Self> {
        let mut f = io(std::fs::File::open(path))?;
        let length = io(f.metadata())?.len();
        let header = read_range(&mut f, 0, HEAD)?;
        if header.starts_with(b"FUJIFILMCCD-RAW") {
            // RAF: embedded JPEG (offset/length at bytes 84/88, big endian), EXIF at the start
            let off = u32::from_be_bytes(header.get(84..88)?.try_into().ok()?) as u64;
            let len = u32::from_be_bytes(header.get(88..92)?.try_into().ok()?) as usize;
            let j = read_range(&mut f, off, len.min(RAF_JPEG))?;
            let tiff = jpeg_exif(&j)?;
            return Some(Self { d: j, tiff, cmt3: None, truncated: false });
        }
        if let Some((off, len)) = rw2_jpeg(&header) {
            // RW2: the EXIF data (with the makernote) lie in the embedded JPEG
            let j = read_range(&mut f, off, len.min(RAF_JPEG))?;
            let tiff = jpeg_exif(&j)?;
            return Some(Self { d: j, tiff, cmt3: None, truncated: false });
        }
        if header.get(4..8) == Some(b"ftyp") {
            // CR3 (ISO BMFF): CMT1 = IFD0, CMT3 = Canon makernote, both with their own TIFF header
            let c1 = find(&header, b"CMT1")? + 4;
            let c3 = find(&header, b"CMT3").map(|p| p + 4);
            return Some(Self { d: header, tiff: c1, cmt3: c3, truncated: false });
        }
        if let Some(t) = jpeg_exif(&header) {
            return Some(Self { d: header, tiff: t, cmt3: None, truncated: false });
        }
        if header.starts_with(b"II") || header.starts_with(b"MM") {
            // TIFF-like (DNG, CR2, ARW): the makernote can lie beyond the first 4 MB
            let has_more = length as usize > header.len() && length < (400 << 20);
            let d = if full && has_more { read_range(&mut f, 0, length as usize)? } else { header };
            return Some(Self { d, tiff: 0, cmt3: None, truncated: has_more && !full });
        }
        None
    }

    pub(crate) fn exif(&self) -> Option<Exif> {
        let t = Tiff::new_at(&self.d, self.tiff)?;
        let ifd0 = t.ifd(t.u32(t.basis + 4)? as usize + t.basis, t.basis)?;
        let txt = |tag| ifd0.iter().find(|e| e.tag == tag).and_then(|e| t.text(e));
        let make = txt(0x010f).unwrap_or_default();
        let model = txt(0x0110).unwrap_or_default();
        let mut mn = None;
        // DNGPrivateData (Ricoh/Pentax: "RICOH\0II…", "PENTAX \0…", "AOC\0…")
        if let Some(e) = ifd0.iter().find(|e| e.tag == 0xc634) {
            let p = t.value_pos(e)?;
            mn = Some((p, e.n as usize));
        }
        if let Some(e) = ifd0.iter().find(|e| e.tag == 0x8769) {
            let off = t.u32(t.value_pos(e)?)? as usize + t.basis;
            if let Some(ex) = t.ifd(off, t.basis) {
                if let Some(m) = ex.iter().find(|e| e.tag == 0x927c) {
                    if let Some(p) = t.value_pos(m) {
                        mn = Some((p, m.n as usize));
                    }
                }
            }
        }
        Some(Exif { make, model, le: t.le, tiff: self.tiff, mn })
    }
}

pub(crate) struct Exif {
    pub(crate) make: String,
    pub(crate) model: String,
    pub(crate) le: bool,
    pub(crate) tiff: usize,
    /// Makernote: position in `Source::d`, length
    pub(crate) mn: Option<(usize, usize)>,
}

#[derive(Clone, Copy)]
pub(crate) struct RawEntry {
    pub(crate) tag: u16,
    pub(crate) type_name: u16,
    pub(crate) n: u32,
    /// Position of the entry (12 bytes) in the data
    pub(crate) e: usize,
}

pub(crate) struct Tiff<'a> {
    pub(crate) d: &'a [u8],
    pub(crate) le: bool,
    /// Reference point for offsets
    pub(crate) basis: usize,
}

pub(crate) fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

impl<'a> Tiff<'a> {
    pub(crate) fn new_at(d: &'a [u8], basis: usize) -> Option<Self> {
        let Some(order) = d.get(basis..basis + 2) else {
            out_of_data();
            return None;
        };
        let le = match order {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Self { d, le, basis })
    }
    /// `n` bytes at `o`; notes a read past the end of the data (see [`read_head`]).
    pub(crate) fn get(&self, o: usize, n: usize) -> Option<&'a [u8]> {
        let r = o.checked_add(n).and_then(|end| self.d.get(o..end));
        if r.is_none() {
            out_of_data();
        }
        r
    }
    pub(crate) fn u16(&self, o: usize) -> Option<u16> {
        let b: [u8; 2] = self.get(o, 2)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }
    pub(crate) fn u32(&self, o: usize) -> Option<u32> {
        let b: [u8; 4] = self.get(o, 4)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }
    /// IFD at absolute position `at`; value offsets relative to `basis`.
    pub(crate) fn ifd(&self, at: usize, basis: usize) -> Option<Vec<RawEntry>> {
        let n = self.u16(at)? as usize;
        if n == 0 || n > 1000 {
            return None;
        }
        let mut v = Vec::with_capacity(n);
        for k in 0..n {
            let e = at + 2 + 12 * k;
            v.push(RawEntry { tag: self.u16(e)?, type_name: self.u16(e + 2)?, n: self.u32(e + 4)?, e });
        }
        let _ = basis;
        Some(v)
    }
    /// Position of an entry's values (inline or via offset relative to `self.basis`).
    pub(crate) fn value_pos(&self, e: &RawEntry) -> Option<usize> {
        self.value_pos_base(e, self.basis)
    }
    pub(crate) fn value_pos_base(&self, e: &RawEntry, basis: usize) -> Option<usize> {
        let size = type_size(e.type_name) * e.n as usize;
        let p = if size <= 4 { e.e + 8 } else { basis + self.u32(e.e + 8)? as usize };
        let inside = p + size <= self.d.len();
        if !inside {
            out_of_data();
        }
        inside.then_some(p)
    }
    pub(crate) fn text(&self, e: &RawEntry) -> Option<String> {
        let p = self.value_pos(e)?;
        let b = self.get(p, e.n as usize)?;
        let s: String = b.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        Some(s.trim().to_string())
    }
    /// Numeric values (up to `max`) of an entry.
    pub(crate) fn numbers(&self, e: &RawEntry, basis: usize, max: usize) -> Vec<i64> {
        let Some(p) = self.value_pos_base(e, basis) else { return Vec::new() };
        let g = type_size(e.type_name);
        (0..(e.n as usize).min(max))
            .filter_map(|k| {
                let o = p + k * g;
                Some(match e.type_name {
                    1 | 7 => self.get(o, 1)?[0] as i64,
                    6 => self.get(o, 1)?[0] as i8 as i64,
                    3 => self.u16(o)? as i64,
                    8 => self.u16(o)? as i16 as i64,
                    4 => self.u32(o)? as i64,
                    9 => self.u32(o)? as i32 as i64,
                    _ => return None,
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Makernotes

pub(crate) enum Kind {
    Fuji,
    Canon,
    Sony,
    /// Ricoh from GR III / Pentax layout
    Pentax,
    /// older Ricoh (GR, GR II)
    RicohOld,
    /// Sigma (fp, fp L, BF): "SIGMA\0\0\0" + 2 bytes, then IFD; value offsets relative to the TIFF header
    Sigma,
    /// Nikon type 2: "Nikon\0" + version, own TIFF header at +10, offsets relative to it
    Nikon,
    /// Olympus / OM System: offsets relative to the makernote start (old "OLYMP\0": relative to the TIFF header)
    Olympus,
    /// Panasonic: "Panasonic\0\0\0", offsets relative to the TIFF header
    Panasonic,
    Unknown,
}

pub(crate) struct Makernote<'a> {
    pub(crate) t: Tiff<'a>,
    pub(crate) basis: usize,
    pub(crate) entries: Vec<RawEntry>,
    pub(crate) kind: Kind,
}

impl Exif {
    pub(crate) fn makernote<'a>(&self, q: &'a Source) -> Option<Makernote<'a>> {
        // CR3: the makernote is a separate TIFF block
        if let Some(c3) = q.cmt3 {
            let t = Tiff::new_at(&q.d, c3)?;
            let at = c3 + t.u32(c3 + 4)? as usize;
            let entries = t.ifd(at, c3)?;
            return Some(Makernote { basis: c3, t, entries, kind: Kind::Canon });
        }
        let (p, len) = self.mn?;
        parse_makernote(&q.d, p, len, self.le, self.tiff, &self.make)
    }
}

/// Makernote at `p` (length `len`) in `d`, recognised by its header; `le`/`tiff`: byte order and start of the
/// TIFF header the offsets of headerless makernotes refer to; `make`: camera maker from IFD0.
pub(crate) fn parse_makernote<'a>(d: &'a [u8], p: usize, len: usize, le: bool, tiff: usize, make: &str) -> Option<Makernote<'a>> {
    let h = make.to_ascii_uppercase();
    let Some(header) = d.get(p..p + 16.min(len)) else {
        out_of_data();
        return None;
    };
    if header.starts_with(b"FUJIFILM") {
        // offsets relative to the makernote start, always little endian
        let t = Tiff { d, le: true, basis: p };
        let at = p + t.u32(p + 8)? as usize;
        let entries = t.ifd(at, p)?;
        return Some(Makernote { t, basis: p, entries, kind: Kind::Fuji });
    }
    if header.starts_with(b"SONY DSC ") || header.starts_with(b"SONY CAM ") {
        let t = Tiff { d, le, basis: tiff };
        let entries = t.ifd(p + 12, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::Sony });
    }
    if header.starts_with(b"RICOH\0II") || header.starts_with(b"RICOH\0MM") {
        // Pentax layout: count directly after the 8-byte header, offsets relative to the makernote start
        let t = Tiff { d, le: &header[6..8] == b"II", basis: p };
        let entries = t.ifd(p + 8, p)?;
        return Some(Makernote { t, basis: p, entries, kind: Kind::Pentax });
    }
    if header.starts_with(b"Ricoh\0\0\0") || header.starts_with(b"RICOH\0\0\0") {
        // older Ricoh (GR, GR II): big endian, count after the 8-byte header
        let t = Tiff { d, le: false, basis: tiff };
        let entries = t.ifd(p + 8, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::RicohOld });
    }
    if header.starts_with(b"SIGMA\0\0\0") || header.starts_with(b"FOVEON\0\0") {
        // 8-byte name, 2 bytes version, then count + entries; offsets are relative to the TIFF header
        let t = Tiff { d, le, basis: tiff };
        let entries = t.ifd(p + 10, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::Sigma });
    }
    if header.starts_with(b"Nikon\0") {
        let base = p + 10;
        let t = Tiff::new_at(d, base)?;
        let at = base + t.u32(base + 4)? as usize;
        let entries = t.ifd(at, base)?;
        return Some(Makernote { t, basis: base, entries, kind: Kind::Nikon });
    }
    if header.starts_with(b"OLYMPUS\0") || header.starts_with(b"OM SYSTEM\0") {
        // "OLYMPUS\0II\x03\0" (12 bytes) or "OM SYSTEM\0\0\0II\x04\0" (16 bytes), offsets relative to the makernote start
        let s = if header.starts_with(b"OLYMPUS\0") { 8 } else { 12 };
        let le = header.get(s..s + 2)? == b"II";
        let t = Tiff { d, le, basis: p };
        let entries = t.ifd(p + s + 4, p)?;
        return Some(Makernote { t, basis: p, entries, kind: Kind::Olympus });
    }
    if header.starts_with(b"OLYMP\0") {
        // old Olympus: count after the 8-byte header, offsets relative to the TIFF header
        let t = Tiff { d, le, basis: tiff };
        let entries = t.ifd(p + 8, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::Olympus });
    }
    if header.starts_with(b"Panasonic\0") {
        let t = Tiff { d, le, basis: tiff };
        let entries = t.ifd(p + 12, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::Panasonic });
    }
    if header.starts_with(b"AOC\0") || header.starts_with(b"PENTAX \0") {
        let s = if header.starts_with(b"AOC\0") { 4 } else { 8 };
        // the header holds at most 16 bytes of a possibly short makernote
        let le = header.get(s..s + 2)? == b"II";
        let t = Tiff { d, le, basis: p };
        let entries = t.ifd(p + s + 2, p)?;
        return Some(Makernote { t, basis: p, entries, kind: Kind::Pentax });
    }
    if h.contains("CANON") {
        let t = Tiff { d, le, basis: tiff };
        let entries = t.ifd(p, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::Canon });
    }
    if h.contains("SONY") {
        let t = Tiff { d, le, basis: tiff };
        let entries = t.ifd(p, tiff)?;
        return Some(Makernote { t, basis: tiff, entries, kind: Kind::Sony });
    }
    let t = Tiff { d, le, basis: tiff };
    let entries = t.ifd(p, tiff).unwrap_or_default();
    Some(Makernote { t, basis: tiff, entries, kind: Kind::Unknown })
}

impl Makernote<'_> {
    pub(crate) fn numbers(&self, tag: u16, max: usize) -> Option<Vec<i64>> {
        let e = self.entries.iter().find(|e| e.tag == tag)?;
        let v = self.t.numbers(e, self.basis, max);
        (!v.is_empty()).then_some(v)
    }
    pub(crate) fn number(&self, tag: u16) -> Option<i64> {
        self.numbers(tag, 1).map(|v| v[0])
    }
    fn text(&self, tag: u16) -> Option<String> {
        let e = self.entries.iter().find(|e| e.tag == tag)?;
        if e.type_name != 2 && e.type_name != 7 {
            return None;
        }
        let p = self.t.value_pos_base(e, self.basis)?;
        let b = self.t.get(p, e.n as usize)?;
        Some(b.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect::<String>().trim().to_string())
    }
    fn all(&self) -> Vec<Entry> {
        self.entries
            .iter()
            .map(|e| Entry {
                tag: e.tag,
                type_name: e.type_name,
                n: e.n,
                values: self.t.numbers(e, self.basis, 24),
                text: if e.type_name == 2 { self.text(e.tag) } else { None },
            })
            .collect()
    }
}

fn evaluate(q: &Source) -> Option<PictureStyle> {
    let e = q.exif()?;
    let mut b = PictureStyle { maker: e.make.clone(), model: e.model.clone(), ..Default::default() };
    let Some(mn) = e.makernote(q) else {
        b.style = "unknown".into();
        return Some(b);
    };
    match mn.kind {
        Kind::Fuji => fuji(&mn, &mut b),
        Kind::Canon => canon(&mn, &mut b),
        Kind::Sony => sony(&mn, &mut b),
        Kind::Pentax => ricoh(&mn, &mut b),
        Kind::RicohOld => ricoh_alt(&mn, &mut b),
        Kind::Sigma => sigma(&mn, &mut b),
        Kind::Nikon => nikon(&mn, &mut b),
        Kind::Olympus => olympus(&mn, &mut b),
        Kind::Panasonic => panasonic(&mn, &mut b),
        Kind::Unknown => b.style = "unknown".into(),
    }
    // makernote texts (Sony, Sigma) end up in look names and file names
    b.style = name_part(&b.style);
    Some(b)
}

/// Fujifilm (exiftool "FujiFilm Tags"): 0x1401 FilmMode, 0x1003 Saturation (contains the
/// black & white/Acros/sepia modes), 0x1040/0x1041 shadow/highlight tone, 0x1047 grain,
/// 0x1048 Color Chrome, 0x104e Color Chrome Blue, 0x1402/0x1403 dynamic range.
fn fuji(mn: &Makernote, b: &mut PictureStyle) {
    let film = mn.number(0x1401);
    let sat = mn.number(0x1003);
    let mono = match sat {
        Some(0x300) => Some("monochrome"),
        Some(0x301) => Some("monochrome-r"),
        Some(0x302) => Some("monochrome-ye"),
        Some(0x303) => Some("monochrome-g"),
        Some(0x310) => Some("sepia"),
        Some(0x500) => Some("acros"),
        Some(0x501) => Some("acros-r"),
        Some(0x502) => Some("acros-ye"),
        Some(0x503) => Some("acros-g"),
        _ => None,
    };
    let color = |s: i64| -> Option<&'static str> {
        Some(match s {
            0 => "0",
            0x80 => "+1",
            0x100 => "+2",
            0xc0 => "+3",
            0xe0 => "+4",
            0x180 => "-1",
            0x200 => "-1 (niedrig)",
            0x400 => "-2",
            0x4c0 => "-3",
            0x4e0 => "-4",
            _ => return None,
        })
    };
    b.style = if let Some(m) = mono {
        m.to_string()
    } else {
        match film {
            Some(0x000) | None => "provia",
            Some(0x100) => "studio-portrait",
            Some(0x110) => "studio-portrait-sat",
            Some(0x120) => "astia",
            Some(0x130) => "studio-portrait-sharp",
            Some(0x200) | Some(0x400) => "velvia",
            Some(0x300) => "studio-portrait-ex",
            Some(0x500) => "pro-neg-std",
            Some(0x501) => "pro-neg-hi",
            Some(0x600) => "classic-chrome",
            Some(0x700) => "eterna",
            Some(0x800) => "classic-neg",
            Some(0x900) => "bleach-bypass",
            Some(0xa00) => "nostalgic-neg",
            Some(0xb00) => "reala-ace",
            Some(x) => {
                b.details.push(("FilmMode", format!("{x:#x}")));
                "film-unknown"
            }
        }
        .to_string()
    };
    if mono.is_none() {
        if let Some(s) = sat.and_then(color) {
            b.details.push(("Farbe", s.to_string()));
        }
    }
    let tone = |v: i64| if v == 0 { "0".to_string() } else { format!("{:+}", -(v as f64) / 16.0) };
    if let Some(v) = mn.number(0x1041) {
        b.details.push(("Lichterton", tone(v)));
    }
    if let Some(v) = mn.number(0x1040) {
        b.details.push(("Schattenton", tone(v)));
    }
    let dr = match mn.number(0x1402) {
        Some(0x100) => Some(100),
        Some(0x200) => Some(200),
        Some(0x201) => Some(400),
        _ => mn.number(0x1403).filter(|&v| v >= 100).or_else(|| mn.number(0x140b).filter(|&v| v >= 100)),
    };
    if let Some(d) = dr {
        b.details.push(("DR", d.to_string()));
    }
    // 0x1402 DynamicRangeSetting 0 = automatic: the camera picks DR100/200/400 per scene (adaptive)
    if mn.number(0x1402) == Some(0) {
        b.details.push(("DRAuto", "1".to_string()));
    }
    if let Some(v) = mn.number(0x1047) {
        b.details.push(("Körnung", match v { 0 => "aus", 32 => "schwach", 64 => "stark", _ => "?" }.to_string()));
    }
    if let Some(v) = mn.number(0x1048) {
        b.details.push(("ColorChrome", match v { 0 => "aus", 32 => "schwach", 64 => "stark", _ => "?" }.to_string()));
    }
    if let Some(v) = mn.number(0x104e) {
        b.details.push(("ColorChromeBlau", match v { 0 => "aus", 32 => "schwach", 64 => "stark", _ => "?" }.to_string()));
    }
    if let Some(v) = mn.number(0x1004) {
        b.details.push(("Kontrast", format!("{v:#x}")));
    }
}

/// Canon: ProcessingInfo (0x00a0), element 10 = PictureStyle (exiftool "Canon ProcessingInfo",
/// element 0 = length in bytes);
/// 0x4008 PictureStyleUserDef = base styles of the three user styles.
fn canon(mn: &Makernote, b: &mut PictureStyle) {
    let name = |v: i64| -> &'static str {
        match v {
            0x00 => "none",
            0x01 | 0x81 => "standard",
            0x02 | 0x82 => "portrait",
            0x03 => "high-saturation",
            0x04 => "adobe-rgb",
            0x05 => "low-saturation",
            0x06 | 0x07 => "cm-set",
            0x21..=0x23 => "user",
            0x41..=0x43 => "pc",
            0x83 => "landscape",
            0x84 => "neutral",
            0x85 => "faithful",
            0x86 => "monochrome",
            0x87 => "auto",
            0x88 => "fine-detail",
            _ => "unknown",
        }
    };
    let pi = mn.numbers(0x00a0, 16);
    let ps = pi.as_ref().and_then(|v| v.get(10).copied());
    let mut style = ps.map(name).unwrap_or("unknown").to_string();
    if let (Some(p @ 0x21..=0x23), Some(u)) = (ps, mn.numbers(0x4008, 3)) {
        if let Some(&g) = u.get((p - 0x21) as usize) {
            b.details.push(("Benutzerstil-Basis", name(g).to_string()));
            style = format!("user{}-{}", p - 0x20, name(g));
        }
    }
    if let Some(p) = ps {
        b.details.push(("PictureStyle", format!("{p:#x}")));
    }
    // 0x4018 LightingOpt (element 0 = byte count): element 2 Auto Lighting Optimizer, 0 standard, 1 low, 2 strong, 3 off
    if let Some(alo) = mn.numbers(0x4018, 3).and_then(|v| v.get(2).copied()) {
        b.details.push(("ALO", alo.to_string()));
    }
    // CameraSettings (0x0001): element 13 contrast, 14 saturation, 42? – for information only
    if let Some(cs) = mn.numbers(0x0001, 16) {
        if let (Some(k), Some(s)) = (cs.get(13), cs.get(14)) {
            b.details.push(("Kontrast", k.to_string()));
            b.details.push(("Sättigung", s.to_string()));
        }
    }
    b.style = style;
}

/// Sony: 0xb020 CreativeStyle (text).
fn sony(mn: &Makernote, b: &mut PictureStyle) {
    b.style = mn.text(0xb020).map(|s| s.to_ascii_lowercase().replace(' ', "-")).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
    // 0xb025 DynamicRangeOptimizer: 0 off, otherwise standard/auto/advanced levels (adaptive)
    if let Some(v) = mn.number(0xb025) {
        b.details.push(("DRO", v.to_string()));
    }
}

/// Ricoh GR III/IIIx/IV (Pentax layout of the makernote). Image mode ("Image Control"/effect) in
/// tag 0x004f (Pentax "ImageTone"); values checked against the camera JPEGs of own images.
fn ricoh(mn: &Makernote, b: &mut PictureStyle) {
    let v = mn.number(0x004f);
    b.style = match v {
        Some(x) => ricoh_name(x).to_string(),
        None => "unknown".into(),
    };
    if let Some(x) = v {
        b.details.push(("ImageTone", x.to_string()));
    }
    // Pentax layout: 0x0069 DynamicRangeExpansion (first byte ≠ 0: highlight correction on), 0x0079 ShadowCorrection
    // (first value ≠ 0: on or automatic) – both adapt the camera JPEG to the scene
    if let Some(x) = mn.number(0x0069) {
        b.details.push(("HighlightCorrection", x.to_string()));
    }
    if let Some(x) = mn.number(0x0079) {
        b.details.push(("ShadowCorrection", x.to_string()));
    }
}

fn ricoh_name(v: i64) -> &'static str {
    match v {
        0 => "natural",
        1 => "bright",
        2 => "portrait",
        3 => "landscape",
        4 => "vibrant",
        5 => "monochrome",
        6 => "muted",
        7 => "positive-film",
        8 => "bleach-bypass",
        9 => "radiant",
        10 => "cross-processing",
        11 => "flat",
        // Ricoh GR III/IIIx/IV ("Image Control" in menu order); 256, 258, 260, 261, 262 checked
        // on own images (258/260/261 camera JPEG black & white, 262 color, stronger)
        256 => "standard",
        257 => "vivid",
        258 => "monotone",
        259 => "monotone-soft",
        260 => "monotone-hard",
        261 => "bw-high-contrast",
        262 => "positive-film",
        263 => "bleach-bypass",
        264 => "retro",
        265 => "hdr-tone",
        266 => "cross-processing",
        267 => "negative-film",
        _ => "unknown",
    }
}

/// older Ricoh (GR Digital, GR, GR II; exiftool "Ricoh Tags"): 0x1010 ImageEffects,
/// 0x1015 ToningEffect, 0x1012 contrast, 0x1013 saturation (3 = 0).
fn ricoh_alt(mn: &Makernote, b: &mut PictureStyle) {
    let v = mn.number(0x1010);
    b.style = match v {
        Some(0) => "standard",
        Some(1) => "vivid",
        Some(3) => "bw",
        Some(5) => "bw-toned",
        Some(6) | Some(7) => "user",
        Some(9) => "bw-high-contrast",
        Some(10) => "cross-processing",
        Some(11) => "positive-film",
        Some(12) => "bleach-bypass",
        Some(13) => "retro",
        Some(15) => "miniature",
        Some(17) => "high-key",
        _ => "unknown",
    }
    .to_string();
    for (tag, name) in [(0x1010u16, "ImageEffects"), (0x1015, "Tonung"), (0x1012, "Kontrast"), (0x1013, "Sättigung")] {
        if let Some(x) = mn.number(tag) {
            b.details.push((name, x.to_string()));
        }
    }
}

/// Sigma fp, fp L, BF: tag 0x003d = Color Mode as plain text ("Standard", "Neutral", "Rich", "Calm",
/// "Cinema" …; values checked against the sample files of the three cameras). Only recognition and
/// display; there are no Sigma looks yet.
fn sigma(mn: &Makernote, b: &mut PictureStyle) {
    let mode = mn.text(0x003d).filter(|t| !t.is_empty());
    b.style = match &mode {
        Some(m) => slug(m),
        None => "unknown".into(),
    };
    if let Some(m) = mode {
        b.details.push(("Farbmodus", m));
    }
}

/// Nikon (exiftool "Nikon Tags"): 0x0023 PictureControlData, not encrypted. Version "0100"/"0200": name at byte 4,
/// base at 24; "0300" and later: name at 8, base at 28 (each 20 bytes of text). A custom Picture Control keeps its
/// own name; the base tells which standard control it starts from.
fn nikon(mn: &Makernote, b: &mut PictureStyle) {
    let data = mn.entries.iter().find(|e| e.tag == 0x0023).and_then(|e| {
        let p = mn.t.value_pos_base(e, mn.basis)?;
        mn.t.get(p, e.n as usize)
    });
    let text = |d: &[u8], at: usize| -> Option<String> {
        let s: String = d.get(at..at + 20)?.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        Some(s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let Some(d) = data else {
        b.style = "unknown".into();
        return;
    };
    let v3 = d.get(..2).is_some_and(|v| v != b"01" && v != b"02");
    let (name, base) = if v3 { (text(d, 8), text(d, 28)) } else { (text(d, 4), text(d, 24)) };
    b.style = name.as_deref().map(slug).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
    if let Some(v) = d.get(..4) {
        b.details.push(("PictureControlVersion", v.iter().map(|&c| c as char).collect()));
    }
    if let Some(x) = base.filter(|x| Some(x) != name.as_ref()) {
        b.details.push(("PictureControlBase", x));
    }
    // 0x0022 ActiveD-Lighting: 0 off, otherwise a level or automatic (adapts the camera JPEG to the scene)
    if let Some(x) = mn.number(0x0022) {
        b.details.push(("ActiveDLighting", x.to_string()));
    }
}

/// Olympus / OM System (exiftool "Olympus CameraSettings"): sub-IFD 0x2020, in it 0x0520 PictureMode
/// (first value), 0x0529 ArtFilter. Old makernotes without the sub-IFD stay "unknown".
fn olympus(mn: &Makernote, b: &mut PictureStyle) {
    let cs = mn.entries.iter().find(|e| e.tag == 0x2020).and_then(|e| {
        let off = mn.t.u32(e.e + 8)? as usize;
        let entries = mn.t.ifd(mn.basis + off, mn.basis)?;
        Some(Makernote { t: Tiff { d: mn.t.d, le: mn.t.le, basis: mn.basis }, basis: mn.basis, entries, kind: Kind::Olympus })
    });
    let mode = cs.as_ref().and_then(|c| c.number(0x0520));
    b.style = match mode {
        Some(1) => "vivid",
        Some(2) => "natural",
        Some(3) => "muted",
        Some(4) => "portrait",
        Some(5) => "i-enhance",
        Some(6) => "e-portrait",
        Some(7) => "color-creator",
        Some(9) => "color-profile-1",
        Some(10) => "color-profile-2",
        Some(11) => "color-profile-3",
        Some(12) => "monochrome-profile-1",
        Some(13) => "monochrome-profile-2",
        Some(14) => "monochrome-profile-3",
        Some(17) => "art-mode",
        Some(18) => "monochrome-profile-4",
        Some(256) => "monotone",
        Some(512) => "sepia",
        _ => "unknown",
    }
    .to_string();
    if let Some(x) = mode {
        b.details.push(("PictureMode", x.to_string()));
    }
    if let Some(x) = cs.as_ref().and_then(|c| c.number(0x0529)).filter(|&x| x != 0) {
        b.details.push(("ArtFilter", x.to_string()));
    }
}

/// Panasonic (exiftool "Panasonic Tags"): 0x0089 PhotoStyle.
fn panasonic(mn: &Makernote, b: &mut PictureStyle) {
    let v = mn.number(0x0089);
    b.style = match v {
        Some(0) => "auto",
        Some(1) => "standard",
        Some(2) => "vivid",
        Some(3) => "natural",
        Some(4) => "monochrome",
        Some(5) => "scenery",
        Some(6) => "portrait",
        Some(8) => "cinelike-d",
        Some(9) => "cinelike-v",
        Some(11) => "l-monochrome",
        Some(12) => "like709",
        Some(15) => "l-monochrome-d",
        Some(17) => "v-log",
        Some(18) => "cinelike-d2",
        _ => "unknown",
    }
    .to_string();
    if let Some(x) = v {
        b.details.push(("PhotoStyle", x.to_string()));
    }
}

/// Lowercase key from a style name: "Teal and Orange" → `teal-and-orange`, "709 Look" → `709-look`.
fn slug(t: &str) -> String {
    let t = t.to_ascii_lowercase().replace('&', " and ");
    t.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The small head gives the same picture style as the 4 MB head (and the whole file) – for every RAW in
    /// `GRAINEO_TEST_STYLE_DIR` (skipped without it). Also counts how many files the small head was enough for.
    #[test]
    fn small_head_reads_the_same_as_the_large_one() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_STYLE_DIR") else { return };
        let mut small = 0;
        let mut files = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            files += 1;
            let large = read_large(&p);
            let both = read(&p);
            assert_eq!(format!("{large:?}"), format!("{both:?}"), "{}", p.display());
            if read_head(&p, SMALL_HEAD).is_some() {
                small += 1;
            }
        }
        eprintln!("small head enough for {small} of {files} files");
    }

    /// A head cut short before the makernote: the reader notices it ran out of data and the small read gives up
    /// (the larger read decides), instead of taking a style made from missing fields.
    #[test]
    fn a_short_head_is_noticed() {
        // little-endian TIFF: IFD0 at 8 with one entry, EXIF pointer to 26; EXIF IFD with MakerNote at offset 1000
        let mut d = vec![0u8; 64];
        d[..4].copy_from_slice(b"II*\0");
        d[4..8].copy_from_slice(&8u32.to_le_bytes());
        d[8..10].copy_from_slice(&1u16.to_le_bytes());
        d[10..12].copy_from_slice(&0x8769u16.to_le_bytes());
        d[12..14].copy_from_slice(&4u16.to_le_bytes());
        d[14..18].copy_from_slice(&1u32.to_le_bytes());
        d[18..22].copy_from_slice(&26u32.to_le_bytes());
        d[26..28].copy_from_slice(&1u16.to_le_bytes());
        d[28..30].copy_from_slice(&0x927cu16.to_le_bytes());
        d[30..32].copy_from_slice(&7u16.to_le_bytes());
        d[32..36].copy_from_slice(&64u32.to_le_bytes());
        d[36..40].copy_from_slice(&1000u32.to_le_bytes());
        let q = Source { d, tiff: 0, cmt3: None, truncated: false };
        OUT_OF_DATA.with(|c| c.set(false));
        let _ = evaluate(&q);
        assert!(OUT_OF_DATA.with(|c| c.get()), "the makernote lies beyond the data");
    }

    #[test]
    fn slug_names() {
        assert_eq!(slug("Teal and Orange"), "teal-and-orange");
        assert_eq!(slug("Teal & Orange"), "teal-and-orange");
        assert_eq!(slug("709 Look"), "709-look");
        assert_eq!(slug("FOV Classic Blue"), "fov-classic-blue");
    }

    #[test]
    fn camera_and_style_names_stay_inside_the_looks_folder() {
        assert_eq!(camera_short("FUJIFILM", "X100V"), "x100v");
        assert_eq!(camera_short("Canon", "Canon EOS RP"), "eos-rp");
        assert_eq!(camera_short("RICOH IMAGING COMPANY, LTD.", "RICOH GR IIIx"), "gr3x");
        for model in ["..", ".", "../x", "../../etc", "a/../b", "a\\b", "", " ", ".hidden", "x\u{0}y", "ü"] {
            let c = camera_short("X", model);
            assert!(!c.is_empty() && !c.starts_with('.') && !c.contains(['/', '\\']), "{model:?} → {c:?}");
            assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch)), "{model:?} → {c:?}");
        }
        assert_eq!(name_part(".."), "--");
        assert_eq!(name_part("a..b"), "a..b");
        assert_eq!(name_part(&"x".repeat(500)).len(), 64);
    }

    /// Nikon Picture Control, Olympus Picture Mode, Panasonic Photo Style from free sample files (raw.pixls.us,
    /// first file of each camera folder) in `GRAINEO_TEST_STYLE_SAMPLES`; skipped without it or when a file is missing.
    #[test]
    fn nikon_olympus_panasonic_styles_from_samples() {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_STYLE_SAMPLES") else { return };
        let base = Path::new(&dir);
        for (f, style, detail) in [
            ("nikon-d750.NEF", "standard", "0200"),
            ("nikon-d610.NEF", "landscape", "0100"),
            ("nikon-d7200.NEF", "neutral", "0200"),
            ("nikon-zfc.NEF", "auto", "0300"),
            ("oly-em5.ORF", "natural", "2"),
            ("oly-em1m3.ORF", "muted", "3"),
            ("oly-epl9.ORF", "vivid", "1"),
            ("pana-g80.RW2", "natural", "3"),
            ("pana-lx100.RW2", "standard", "1"),
        ] {
            let p = base.join(f);
            if !p.exists() {
                continue;
            }
            let s = read(&p).expect(f);
            assert_eq!(s.style, style, "{f}");
            assert!(s.details.iter().any(|(_, v)| v == detail), "{f}: {:?}", s.details);
            assert_eq!(format!("{:?}", Some(&s)), format!("{:?}", read_large(&p).as_ref()), "{f}: small head and large read agree");
        }
    }

    /// Sample files (internal, not in the repository): skipped if missing.
    #[test]
    fn sigma_color_mode_from_samples() {
        let base = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testbilder/sigma-neue-kameras"));
        for (f, mode) in [("fp/SDIM4591.DNG", "neutral"), ("fp-L/SDIM0915.DNG", "standard"), ("BF/BF_00531.DNG", "rich")] {
            let p = base.join(f);
            if !p.exists() {
                continue;
            }
            let s = read(&p).expect(f);
            assert_eq!(s.style, mode, "{f}");
        }
    }
}
