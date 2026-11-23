//! File container: header, section directory and section headers.

use std::path::Path;

use crate::camf::Camf;
use crate::error::{corrupt, Error, Result};
use crate::prop::PropList;
use crate::raw::RawImage;

pub(crate) fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| Error::Corrupt(format!("Lesen hinter Dateiende bei {o}")))
}

pub(crate) fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| Error::Corrupt(format!("Lesen hinter Dateiende bei {o}")))
}

/// Entry in the section directory ("SECd").
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub offset: usize,
    pub length: usize,
    /// `IMA2`, `IMAG`, `CAMF` or `PROP`.
    pub kind: [u8; 4],
}

/// Result of [`X3fFile::integrity`].
#[derive(Debug, Clone, PartialEq)]
pub enum Integrity {
    /// all RAW planes decode consistently
    Intact,
    /// the compressed RAW data contradicts itself from row `first_row` (of `rows`) on, e.g. because
    /// parts of the file were overwritten with zeros (`zero_blocks`: blocks of 4 KiB that are all zero)
    Damaged { first_row: usize, rows: usize, zero_blocks: usize },
    /// format without a check (Quattro, older compressions)
    NotChecked,
}

/// Number of 4 KiB blocks (aligned to the start of `data`) that consist of zeros only. Compressed
/// image data never contains such long zero runs; on damaged memory cards whole clusters read as zeros.
pub fn zero_blocks(data: &[u8]) -> usize {
    data.chunks_exact(4096).filter(|b| b.iter().all(|&v| v == 0)).count()
}

/// Header of an image section ("SECi").
#[derive(Debug, Clone, Copy)]
pub struct ImageHeader {
    pub image_type: u32,
    pub format: u32,
    pub columns: u32,
    pub rows: u32,
    pub row_stride: u32,
}

/// Upper bounds against damaged files (real X3F: at most a few dozen sections, sensors up to ~30 MP per layer).
pub const MAX_SECTIONS: usize = 1000;
pub const MAX_PIXELS: u64 = 80_000_000;

impl ImageHeader {
    pub const SIZE: usize = 28;

    /// Plausible geometry for a section of `section_len` bytes (header included): at most [`MAX_PIXELS`], RAW
    /// data at least 2 × 2 pixels, rows of uncompressed data inside the section. Damaged headers otherwise lead to
    /// huge allocations and endless decoding.
    pub fn check(&self, section_len: usize) -> Result<()> {
        let pixels = self.columns as u64 * self.rows as u64;
        if pixels > MAX_PIXELS {
            return corrupt(format!("unplausible Bildmaße {} × {}", self.columns, self.rows));
        }
        if (self.is_merrill_raw() || self.is_quattro_raw() || self.is_classic_raw()) && (self.columns < 2 || self.rows < 2) {
            return corrupt(format!("unplausible Bildmaße {} × {}", self.columns, self.rows));
        }
        // uncompressed preview (type 2, format 3): the rows must fit into the section
        if self.image_type == 2 && self.format == 3 && self.row_stride as u64 * self.rows as u64 > section_len.saturating_sub(Self::SIZE) as u64 {
            return corrupt("Bildzeilen ragen über die Sektion hinaus");
        }
        Ok(())
    }

    /// RAW data of the Merrill generation (TRUE compression, three full planes).
    pub fn is_merrill_raw(&self) -> bool {
        self.image_type == 1 && self.format == 30
    }

    /// RAW data of the Quattro generation (format 35 dp Quattro, 37 sd Quattro, 39 sd Quattro H):
    /// top layer at full, lower two at half resolution (1:1:4).
    pub fn is_quattro_raw(&self) -> bool {
        self.image_type == 1 && matches!(self.format, 35 | 37 | 39)
    }

    /// RAW data of the older Foveon generation ("Classic": SD9/SD10/SD14/SD15, DP1/DP2 incl.
    /// s/x): type 3, format 30 (TRUE, SD14 and newer) or 5/6 (Huffman, SD9/SD10).
    pub fn is_classic_raw(&self) -> bool {
        self.image_type == 3 && matches!(self.format, 5 | 6 | 30)
    }

    /// Embedded JPEG (format 18; Quattro thumbnails 1440 × 960 have format 25).
    pub fn is_jpeg(&self) -> bool {
        self.image_type == 2 && matches!(self.format, 18 | 25)
    }
}

/// Opens an X3F for the header readers below: returns the file, the offset where the X3F starts and its length.
/// A plain X3F starts at 0; an X3I (SFD bracket) is read as its first frame, the base exposure, as with
/// [`X3fFile::open`].
fn open_range(path: &Path) -> Result<(std::fs::File, u64, u64)> {
    let f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if crate::x3i::is_x3i(path) {
        let x = crate::x3i::X3iFile::open(path)?;
        let &(s, e) = x.frames.first().ok_or_else(|| Error::Corrupt("X3I: no frame".into()))?;
        return Ok((f, s, e - s));
    }
    Ok((f, 0, len))
}

/// Reads only the embedded JPEG with the smallest (`largest = false`) or largest
/// area, without loading the whole file (for thumbnails in the file browser).
pub fn read_embedded_jpeg(path: impl AsRef<Path>, largest: bool) -> Result<Vec<u8>> {
    read_embedded_jpeg_up_to(path, largest, usize::MAX)
}

/// Like [`read_embedded_jpeg`], but reads at most the first `max` bytes of the JPEG. The EXIF data is at the start; so on a
/// network drive one small read suffices instead of several megabytes per file.
pub fn read_embedded_jpeg_up_to(path: impl AsRef<Path>, largest: bool, max: usize) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let (mut f, base, size) = open_range(path.as_ref())?;
    let mut read_at = |off: u64, len: usize| -> Result<Vec<u8>> {
        if off.checked_add(len as u64).is_none_or(|end| end > size) {
            return corrupt("Sektion ragt über das Dateiende");
        }
        f.seek(SeekFrom::Start(base + off))?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b)?;
        Ok(b)
    };
    let head = read_at(0, 4)?;
    if head != b"FOVb" {
        return Err(Error::NotX3f(head.try_into().unwrap_or([0; 4])));
    }
    let dir = u32_at(&read_at(size.saturating_sub(4), 4)?, 0)? as u64;
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return corrupt("Sektionsverzeichnis nicht gefunden");
    }
    let count = (u32_at(&dir_head, 8)? as usize).min(MAX_SECTIONS);
    let table = read_at(dir + 12, count * 12)?;
    let mut best: Option<(u64, usize, u32)> = None;
    for i in 0..count {
        let (offset, length) = (u32_at(&table, i * 12)? as u64, u32_at(&table, i * 12 + 4)? as usize);
        if &table[i * 12 + 8..i * 12 + 12] != b"IMA2" || length < ImageHeader::SIZE {
            continue;
        }
        let h = read_at(offset, ImageHeader::SIZE)?;
        let (ty, fmt, area) = (u32_at(&h, 8)?, u32_at(&h, 12)?, u32_at(&h, 16)? * u32_at(&h, 20)?);
        if ty != 2 || !matches!(fmt, 18 | 25) {
            continue;
        }
        let better = best.is_none_or(|b| if largest { area > b.2 } else { area < b.2 });
        if better {
            best = Some((offset, length, area));
        }
    }
    let (offset, length, _) = best.ok_or_else(|| Error::Unsupported("kein eingebettetes JPEG".into()))?;
    read_at(offset + ImageHeader::SIZE as u64, (length - ImageHeader::SIZE).min(max))
}

/// Uncompressed preview (type 2, format 3; SD9/SD10/SD14 sometimes have no JPEG) as RGB8,
/// smallest or largest area, without loading the whole file. Rows of `row_stride` bytes,
/// 3 bytes per pixel.
pub fn read_embedded_plain(path: impl AsRef<Path>, largest: bool) -> Option<(u32, u32, Vec<u8>)> {
    use std::io::{Read, Seek, SeekFrom};
    let (mut f, base, size) = open_range(path.as_ref()).ok()?;
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size {
            return None;
        }
        f.seek(SeekFrom::Start(base + off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    if read_at(0, 4)? != b"FOVb" {
        return None;
    }
    let dir = u32_at(&read_at(size.checked_sub(4)?, 4)?, 0).ok()? as u64;
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return None;
    }
    let count = (u32_at(&dir_head, 8).ok()? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    let mut best: Option<(u64, ImageHeader)> = None;
    for i in 0..count {
        let offset = u32_at(&table, i * 12).ok()? as u64;
        let length = u32_at(&table, i * 12 + 4).ok()? as u64;
        let kind = &table[i * 12 + 8..i * 12 + 12];
        if (kind != b"IMA2" && kind != b"IMAG") || length < ImageHeader::SIZE as u64 {
            continue;
        }
        let h = read_at(offset, ImageHeader::SIZE)?;
        let hd = ImageHeader {
            image_type: u32_at(&h, 8).ok()?,
            format: u32_at(&h, 12).ok()?,
            columns: u32_at(&h, 16).ok()?,
            rows: u32_at(&h, 20).ok()?,
            row_stride: u32_at(&h, 24).ok()?,
        };
        let fits = hd.row_stride as u64 >= hd.columns as u64 * 3 && ImageHeader::SIZE as u64 + hd.row_stride as u64 * hd.rows as u64 <= length;
        if hd.image_type != 2 || hd.format != 3 || !fits || hd.columns == 0 || hd.rows == 0 {
            continue;
        }
        let area = hd.columns * hd.rows;
        if best.as_ref().is_none_or(|b| if largest { area > b.1.columns * b.1.rows } else { area < b.1.columns * b.1.rows }) {
            best = Some((offset, hd));
        }
    }
    let (offset, h) = best?;
    let data = read_at(offset + ImageHeader::SIZE as u64, h.row_stride as usize * h.rows as usize)?;
    let mut rgb = Vec::with_capacity(h.columns as usize * h.rows as usize * 3);
    for row in data.chunks_exact(h.row_stride as usize) {
        rgb.extend_from_slice(&row[..h.columns as usize * 3]);
    }
    Some((h.columns, h.rows, rgb))
}

/// Header of the RAW image section, without loading the whole file (type/format determine the decoder).
pub fn read_raw_header(path: impl AsRef<Path>) -> Option<ImageHeader> {
    use std::io::{Read, Seek, SeekFrom};
    let (mut f, base, size) = open_range(path.as_ref()).ok()?;
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size {
            return None;
        }
        f.seek(SeekFrom::Start(base + off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    if read_at(0, 4)? != b"FOVb" {
        return None;
    }
    let dir = u32_at(&read_at(size.checked_sub(4)?, 4)?, 0).ok()? as u64;
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return None;
    }
    let count = (u32_at(&dir_head, 8).ok()? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    for i in 0..count {
        let offset = u32_at(&table, i * 12).ok()? as u64;
        let kind = &table[i * 12 + 8..i * 12 + 12];
        if kind != b"IMA2" && kind != b"IMAG" {
            continue;
        }
        let h = read_at(offset, ImageHeader::SIZE)?;
        let header = ImageHeader {
            image_type: u32_at(&h, 8).ok()?,
            format: u32_at(&h, 12).ok()?,
            columns: u32_at(&h, 16).ok()?,
            rows: u32_at(&h, 20).ok()?,
            row_stride: u32_at(&h, 24).ok()?,
        };
        if header.image_type == 1 || header.image_type == 3 {
            return header.check(usize::MAX).is_ok().then_some(header);
        }
    }
    None
}

/// Size of the finished image (width, height, unrotated as stored), without loading the whole file:
/// the largest embedded preview (JPEG or uncompressed) has the camera's output size
/// (Merrill HI 4704 × 3136, LOW 2336 × 1568). Without a preview: `None`.
pub fn read_image_size(path: impl AsRef<Path>) -> Option<(u32, u32)> {
    use std::io::{Read, Seek, SeekFrom};
    let (mut f, base, size) = open_range(path.as_ref()).ok()?;
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size {
            return None;
        }
        f.seek(SeekFrom::Start(base + off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    if read_at(0, 4)? != b"FOVb" {
        return None;
    }
    let dir = u32_at(&read_at(size.checked_sub(4)?, 4)?, 0).ok()? as u64;
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return None;
    }
    let count = (u32_at(&dir_head, 8).ok()? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    let mut best: Option<(u32, u32)> = None;
    for i in 0..count {
        let offset = u32_at(&table, i * 12).ok()? as u64;
        let kind = &table[i * 12 + 8..i * 12 + 12];
        if kind != b"IMA2" && kind != b"IMAG" {
            continue;
        }
        let h = read_at(offset, ImageHeader::SIZE)?;
        let (ty, fmt, w, hh) = (u32_at(&h, 8).ok()?, u32_at(&h, 12).ok()?, u32_at(&h, 16).ok()?, u32_at(&h, 20).ok()?);
        if ty == 2 && matches!(fmt, 3 | 18 | 25) && best.is_none_or(|(bw, bh)| w as u64 * hh as u64 > bw as u64 * bh as u64) {
            best = Some((w, hh));
        }
    }
    best.filter(|&(w, h)| w > 0 && h > 0)
}

/// Camera orientation from the file header in degrees (clockwise), without loading the whole file.
/// Header up to version 3.x: offset 36; from 4.0 (Quattro): offset 48.
pub fn read_header_rotation(path: impl AsRef<Path>) -> Option<u32> {
    use std::io::Read;
    let mut head = [0u8; 52];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).ok()?;
    header_rotation(&head)
}

fn header_rotation(head: &[u8]) -> Option<u32> {
    if head.get(0..4)? != b"FOVb" {
        return None;
    }
    let version = u32_at(head, 4).ok()?;
    u32_at(head, if version >= 0x4_0000 { 48 } else { 36 }).ok()
}

pub struct X3fFile {
    data: Vec<u8>,
    pub version: u32,
    pub entries: Vec<DirEntry>,
}

impl X3fFile {
    /// Open an X3F. An X3I (SFD bracket in one file) opens as its first frame (the base exposure); a virtual path
    /// `NAME.X3I#n` opens frame `n` (0-based, file order) of the X3I, see [`crate::x3i::split_virtual`]. The X3I is
    /// only read, never changed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some((file, n)) = crate::x3i::split_virtual(path) {
            return crate::x3i::X3iFile::open(file)?.frame(n);
        }
        if crate::x3i::is_x3i(path) {
            return crate::x3i::X3iFile::open(path)?.frame(0);
        }
        Self::from_bytes(std::fs::read(path)?)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        if data.len() < 8 {
            return corrupt("Datei zu kurz");
        }
        let magic: [u8; 4] = data[0..4].try_into().unwrap();
        if &magic != b"FOVb" {
            return Err(Error::NotX3f(magic));
        }
        let version = u32_at(&data, 4)?;
        let dir = u32_at(&data, data.len() - 4)? as usize;
        if data.get(dir..dir + 4) != Some(b"SECd") {
            return corrupt("Sektionsverzeichnis nicht gefunden");
        }
        let count = u32_at(&data, dir + 8)? as usize;
        if count > MAX_SECTIONS || count * 12 > data.len().saturating_sub(dir + 12) {
            return corrupt(format!("Sektionsverzeichnis mit unplausibler Zahl {count}"));
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let base = dir + 12 + i * 12;
            let offset = u32_at(&data, base)? as usize;
            let length = u32_at(&data, base + 4)? as usize;
            let kind: [u8; 4] = data
                .get(base + 8..base + 12)
                .ok_or_else(|| Error::Corrupt("Verzeichnis abgeschnitten".into()))?
                .try_into()
                .unwrap();
            if offset + length > data.len() {
                return corrupt(format!("Sektion {:?} ragt über das Dateiende", kind));
            }
            entries.push(DirEntry { offset, length, kind });
        }
        Ok(Self { data, version, entries })
    }

    pub fn section(&self, e: &DirEntry) -> &[u8] {
        &self.data[e.offset..e.offset + e.length]
    }

    /// The whole file (e.g. for the file header, see [`crate::sppa`]).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn image_header(&self, e: &DirEntry) -> Result<ImageHeader> {
        let s = self.section(e);
        if s.get(0..4) != Some(b"SECi") {
            return corrupt("Bildsektion ohne SECi-Kennung");
        }
        let h = ImageHeader {
            image_type: u32_at(s, 8)?,
            format: u32_at(s, 12)?,
            columns: u32_at(s, 16)?,
            rows: u32_at(s, 20)?,
            row_stride: u32_at(s, 24)?,
        };
        h.check(s.len())?;
        Ok(h)
    }

    pub fn images(&self) -> impl Iterator<Item = (&DirEntry, ImageHeader)> {
        self.entries
            .iter()
            .filter(|e| &e.kind == b"IMA2" || &e.kind == b"IMAG")
            .filter_map(|e| self.image_header(e).ok().map(|h| (e, h)))
    }

    /// Embedded preview JPEG at full camera resolution (if present).
    pub fn preview_jpeg(&self) -> Option<&[u8]> {
        self.images()
            .filter(|(_, h)| h.is_jpeg())
            .max_by_key(|(_, h)| h.columns * h.rows)
            .map(|(e, _)| &self.section(e)[ImageHeader::SIZE..])
    }

    /// Smallest embedded JPEG (thumbnail), if present.
    pub fn thumbnail_jpeg(&self) -> Option<&[u8]> {
        self.images()
            .filter(|(_, h)| h.is_jpeg())
            .min_by_key(|(_, h)| h.columns * h.rows)
            .map(|(e, _)| &self.section(e)[ImageHeader::SIZE..])
    }

    pub fn raw_entry(&self) -> Result<(&DirEntry, ImageHeader)> {
        self.images()
            .find(|(_, h)| h.image_type == 1 || h.image_type == 3)
            .ok_or_else(|| Error::Unsupported("keine RAW-Bildsektion".into()))
    }

    /// Decode RAW data (three Foveon planes, 12 bit, unprocessed).
    pub fn decode_raw(&self) -> Result<RawImage> {
        let (e, h) = self.raw_entry()?;
        // Classic TRUE (type 3, format 30) has the same structure as Merrill TRUE.
        if !h.is_merrill_raw() && !(h.is_classic_raw() && h.format == 30) {
            return Err(Error::Unsupported(format!(
                "RAW-Format type={} format={} (nur Merrill wird unterstützt)",
                h.image_type, h.format
            )));
        }
        RawImage::decode_merrill(&self.section(e)[ImageHeader::SIZE..], h)
    }

    /// Decodes the RAW data of the SD9/SD10 (type 3, format 6: Huffman with difference table, see
    /// [`crate::huffman06`]); values are signed 16-bit sums, stored as their bit pattern.
    pub fn decode_raw_format6(&self) -> Result<RawImage> {
        let (e, h) = self.raw_entry()?;
        if !(h.image_type == 3 && h.format == 6) {
            return Err(Error::Unsupported(format!("RAW-Format type={} format={} ist nicht Format 6", h.image_type, h.format)));
        }
        crate::huffman06::decode(&self.section(e)[ImageHeader::SIZE..], h.columns as usize, h.rows as usize)
    }

    /// Like [`Self::decode_raw`], but rescues as much as possible from damaged RAW data (see
    /// [`crate::rescue`]). For intact files the result is identical to `decode_raw` and the
    /// report is empty.
    pub fn decode_raw_rescue(&self) -> Result<(RawImage, crate::Rescue)> {
        let (e, h) = self.raw_entry()?;
        if !h.is_merrill_raw() && !(h.is_classic_raw() && h.format == 30) {
            return self.decode_raw().map(|r| (r, crate::Rescue { rows: h.rows as usize, ..Default::default() }));
        }
        match RawImage::decode_merrill(&self.section(e)[ImageHeader::SIZE..], h) {
            Err(Error::Damaged { .. }) => {}
            other => return other.map(|r| (r, crate::Rescue { rows: h.rows as usize, ..Default::default() })),
        }
        // dark columns (light shield, the wider of the two ranges) for the start values behind a damage
        let dark = self.camf().ok().and_then(|c| c.matrix("DarkShieldColRange").ok().map(|m| m.as_f64())).filter(|v| v.len() == 4).map(|v| {
            let (a, b) = ((v[0] as usize, v[1] as usize), (v[2] as usize, v[3] as usize));
            if b.1.saturating_sub(b.0) >= a.1.saturating_sub(a.0) { b } else { a }
        });
        crate::rescue::decode_rescue(&self.section(e)[ImageHeader::SIZE..], h, dark)
    }

    pub fn camf(&self) -> Result<Camf> {
        let e = self
            .entries
            .iter()
            .find(|e| &e.kind == b"CAMF")
            .ok_or_else(|| Error::Unsupported("keine CAMF-Sektion".into()))?;
        Camf::parse(self.section(e))
    }

    /// Property list. Quattro files have no PROP section; then a list with
    /// the same keys is generated from the EXIF of the embedded JPEG and the CAMF
    /// ([`PropList::synthesize`]).
    pub fn properties(&self) -> Result<PropList> {
        match self.entries.iter().find(|e| &e.kind == b"PROP") {
            Some(e) => PropList::parse(self.section(e)),
            None => Ok(PropList::synthesize(self.exif().unwrap_or_default(), self.camf().ok().as_ref(), self.header_rotation())),
        }
    }

    /// Does the file have its own PROP section (Merrill) or was it synthesised (Quattro)?
    pub fn has_prop_section(&self) -> bool {
        self.entries.iter().any(|e| &e.kind == b"PROP")
    }

    /// Uncompressed preview (type 2, format 3; SD9/SD10/SD14 sometimes have no JPEG) as RGB8,
/// smallest or largest area, without loading the whole file. Rows of `row_stride` bytes,
/// 3 bytes per pixel.
pub fn read_embedded_plain(path: impl AsRef<Path>, largest: bool) -> Option<(u32, u32, Vec<u8>)> {
    use std::io::{Read, Seek, SeekFrom};
    let (mut f, base, size) = open_range(path.as_ref()).ok()?;
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size {
            return None;
        }
        f.seek(SeekFrom::Start(base + off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    if read_at(0, 4)? != b"FOVb" {
        return None;
    }
    let dir = u32_at(&read_at(size.checked_sub(4)?, 4)?, 0).ok()? as u64;
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return None;
    }
    let count = (u32_at(&dir_head, 8).ok()? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    let mut best: Option<(u64, ImageHeader)> = None;
    for i in 0..count {
        let offset = u32_at(&table, i * 12).ok()? as u64;
        let length = u32_at(&table, i * 12 + 4).ok()? as u64;
        let kind = &table[i * 12 + 8..i * 12 + 12];
        if (kind != b"IMA2" && kind != b"IMAG") || length < ImageHeader::SIZE as u64 {
            continue;
        }
        let h = read_at(offset, ImageHeader::SIZE)?;
        let hd = ImageHeader {
            image_type: u32_at(&h, 8).ok()?,
            format: u32_at(&h, 12).ok()?,
            columns: u32_at(&h, 16).ok()?,
            rows: u32_at(&h, 20).ok()?,
            row_stride: u32_at(&h, 24).ok()?,
        };
        let fits = hd.row_stride as u64 >= hd.columns as u64 * 3 && ImageHeader::SIZE as u64 + hd.row_stride as u64 * hd.rows as u64 <= length;
        if hd.image_type != 2 || hd.format != 3 || !fits || hd.columns == 0 || hd.rows == 0 {
            continue;
        }
        let area = hd.columns * hd.rows;
        if best.as_ref().is_none_or(|b| if largest { area > b.1.columns * b.1.rows } else { area < b.1.columns * b.1.rows }) {
            best = Some((offset, hd));
        }
    }
    let (offset, h) = best?;
    let data = read_at(offset + ImageHeader::SIZE as u64, h.row_stride as usize * h.rows as usize)?;
    let mut rgb = Vec::with_capacity(h.columns as usize * h.rows as usize * 3);
    for row in data.chunks_exact(h.row_stride as usize) {
        rgb.extend_from_slice(&row[..h.columns as usize * 3]);
    }
    Some((h.columns, h.rows, rgb))
}

/// Header of the RAW image section, without loading the whole file (type/format determine the decoder).
pub fn read_raw_header(path: impl AsRef<Path>) -> Option<ImageHeader> {
    use std::io::{Read, Seek, SeekFrom};
    let (mut f, base, size) = open_range(path.as_ref()).ok()?;
    let mut read_at = |off: u64, len: usize| -> Option<Vec<u8>> {
        if off.checked_add(len as u64)? > size {
            return None;
        }
        f.seek(SeekFrom::Start(base + off)).ok()?;
        let mut b = vec![0u8; len];
        f.read_exact(&mut b).ok()?;
        Some(b)
    };
    if read_at(0, 4)? != b"FOVb" {
        return None;
    }
    let dir = u32_at(&read_at(size.checked_sub(4)?, 4)?, 0).ok()? as u64;
    let dir_head = read_at(dir, 12)?;
    if &dir_head[0..4] != b"SECd" {
        return None;
    }
    let count = (u32_at(&dir_head, 8).ok()? as usize).min(1000);
    let table = read_at(dir + 12, count * 12)?;
    for i in 0..count {
        let offset = u32_at(&table, i * 12).ok()? as u64;
        let kind = &table[i * 12 + 8..i * 12 + 12];
        if kind != b"IMA2" && kind != b"IMAG" {
            continue;
        }
        let h = read_at(offset, ImageHeader::SIZE)?;
        let header = ImageHeader {
            image_type: u32_at(&h, 8).ok()?,
            format: u32_at(&h, 12).ok()?,
            columns: u32_at(&h, 16).ok()?,
            rows: u32_at(&h, 20).ok()?,
            row_stride: u32_at(&h, 24).ok()?,
        };
        if header.image_type == 1 || header.image_type == 3 {
            return header.check(usize::MAX).is_ok().then_some(header);
        }
    }
    None
}

/// Camera orientation from the file header in degrees (see [`read_header_rotation`]).
    pub fn header_rotation(&self) -> Option<u32> {
        header_rotation(self.data.get(..52)?)
    }

    /// EXIF of the largest embedded JPEG (otherwise of the thumbnail).
    pub fn exif(&self) -> Option<crate::exif::JpegExif> {
        self.preview_jpeg().and_then(crate::exif::parse_jpeg_exif).or_else(|| self.thumbnail_jpeg().and_then(crate::exif::parse_jpeg_exif))
    }

    /// Is the RAW data intact? Checks the TRUE planes of Merrill (and Classic format 30) files
    /// completely (≈ 0.1 s for a 15 MP file); other formats are not checked.
    pub fn integrity(&self) -> Integrity {
        let Ok((e, h)) = self.raw_entry() else { return Integrity::NotChecked };
        if !h.is_merrill_raw() && !(h.is_classic_raw() && h.format == 30) {
            return Integrity::NotChecked;
        }
        let body = &self.section(e)[ImageHeader::SIZE..];
        let zero_blocks = zero_blocks(body);
        match RawImage::check_merrill(body, h) {
            Err(_) => Integrity::Damaged { first_row: 0, rows: h.rows as usize, zero_blocks },
            Ok(checks) if checks.iter().all(|c| c.is_clean()) => Integrity::Intact,
            Ok(checks) => Integrity::Damaged {
                first_row: checks.iter().filter_map(|c| c.first_bad_row()).min().unwrap_or(0),
                rows: h.rows as usize,
                zero_blocks,
            },
        }
    }

    /// Older Foveon camera ("Classic", RAW type 3)?
    pub fn is_classic(&self) -> bool {
        self.raw_entry().is_ok_and(|(_, h)| h.is_classic_raw())
    }

    /// Quattro file (RAW format 35/37/39)?
    pub fn is_quattro(&self) -> bool {
        self.raw_entry().is_ok_and(|(_, h)| h.is_quattro_raw())
    }
}
