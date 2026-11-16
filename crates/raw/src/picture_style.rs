//! Camera picture style from the makernote: Fujifilm film simulation (with dynamic range, highlight/
//! shadow tone, color, grain, Color Chrome), Canon Picture Style, Ricoh image mode ("Image
//! Control"), Sony Creative Style.
//!
//! Own small reader (read-only) based on public documentation of the tag tables (exiftool
//! "FujiFilm Tags", "Canon Tags" → ProcessingInfo, "Sony Tags", "Pentax Tags"; exiv2). Nothing is
//! taken from the firmware: only the values the camera itself writes into
//! the file are read. Read from RAW (RAF → embedded JPEG, CR3 → CMT1/CMT3 boxes,
//! DNG → DNGPrivateData or EXIF MakerNote, CR2/ARW → EXIF MakerNote) or camera JPEG.
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
    m.to_ascii_lowercase().split_whitespace().collect::<Vec<_>>().join("-")
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
    // first only the start of the file; TIFF-like files completely if needed
    let q = Source::load(path, false)?;
    match evaluate(&q) {
        Some(b) if b.style != "unknown" => Some(b),
        r if q.truncated => Source::load(path, true).and_then(|q| evaluate(&q)).or(r),
        r => r,
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
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut n = 0;
    while n < len {
        match f.read(&mut v[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return None,
        }
    }
    v.truncate(n);
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

pub(crate) fn find(d: &[u8], needle: &[u8]) -> Option<usize> {
    d.windows(needle.len()).position(|w| w == needle)
}

impl Source {
    pub(crate) fn load(path: &Path, full: bool) -> Option<Self> {
        let mut f = std::fs::File::open(path).ok()?;
        let length = f.metadata().ok()?.len();
        let header = read_range(&mut f, 0, 4 << 20)?;
        if header.starts_with(b"FUJIFILMCCD-RAW") {
            // RAF: embedded JPEG (offset/length at bytes 84/88, big endian), EXIF at the start
            let off = u32::from_be_bytes(header.get(84..88)?.try_into().ok()?) as u64;
            let len = u32::from_be_bytes(header.get(88..92)?.try_into().ok()?) as usize;
            let j = read_range(&mut f, off, len.min(1 << 20))?;
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
        let le = match d.get(basis..basis + 2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Self { d, le, basis })
    }
    pub(crate) fn u16(&self, o: usize) -> Option<u16> {
        let b: [u8; 2] = self.d.get(o..o + 2)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }
    pub(crate) fn u32(&self, o: usize) -> Option<u32> {
        let b: [u8; 4] = self.d.get(o..o + 4)?.try_into().ok()?;
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
        (p + size <= self.d.len()).then_some(p)
    }
    pub(crate) fn text(&self, e: &RawEntry) -> Option<String> {
        let p = self.value_pos(e)?;
        let b = self.d.get(p..p + e.n as usize)?;
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
                    1 | 7 => *self.d.get(o)? as i64,
                    6 => *self.d.get(o)? as i8 as i64,
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
    let header = d.get(p..p + 16.min(len))?;
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
        let b = self.t.d.get(p..p + e.n as usize)?;
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
        Kind::Unknown => b.style = "unknown".into(),
    }
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

/// Lowercase key from a style name: "Teal and Orange" → `teal-and-orange`, "709 Look" → `709-look`.
fn slug(t: &str) -> String {
    let t = t.to_ascii_lowercase().replace('&', " and ");
    t.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_names() {
        assert_eq!(slug("Teal and Orange"), "teal-and-orange");
        assert_eq!(slug("Teal & Orange"), "teal-and-orange");
        assert_eq!(slug("709 Look"), "709-look");
        assert_eq!(slug("FOV Classic Blue"), "fov-classic-blue");
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
