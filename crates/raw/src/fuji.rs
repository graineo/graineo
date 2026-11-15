//! Fujifilm RAF: dynamic range (DR100/200/400) from the makernote of the embedded JPEG.
//!
//! rawler 0.8.0 reads the RAF makernote but does not expose it – hence a small reader
//! of our own here (read-only, reads the header and the JPEG part of the file). The layout is publicly
//! documented (exiftool "FujiFilm Tags"): RAF header with JPEG offset/length at bytes 84/88
//! (big endian), makernote starts with "FUJIFILM" + 4-byte offset of the IFD (little endian).
//! Tags: 0x1400 DynamicRange, 0x1402 DynamicRangeSetting (0 = auto, 1 = manual,
//! 0x100 = 100 %, 0x200 = 200 %, 0x201 = 400 %), 0x1403 DevelopmentDynamicRange,
//! 0x140b AutoDynamicRange.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Effective dynamic range in percent (100, 200, 400) or None.
pub fn dynamic_range(path: &Path) -> Option<u16> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 92];
    f.read_exact(&mut head).ok()?;
    if &head[..8] != b"FUJIFILM" {
        return None;
    }
    let off = u32::from_be_bytes(head[84..88].try_into().ok()?) as u64;
    let len = u32::from_be_bytes(head[88..92].try_into().ok()?) as usize;
    if len == 0 || len > 64 << 20 {
        return None;
    }
    // the makernote is in the EXIF part at the start of the JPEG → 256 KB are enough
    let n = len.min(256 << 10);
    let mut j = vec![0u8; n];
    f.seek(SeekFrom::Start(off)).ok()?;
    f.read_exact(&mut j).ok()?;
    let pos = j.windows(12).position(|w| &w[..8] == b"FUJIFILM" && w[8..12] == [12, 0, 0, 0])?;
    let rd16 = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(j.get(o..o + 2)?.try_into().ok()?)) };
    let ifd = pos + 12;
    let cnt = rd16(ifd)? as usize;
    let mut tags = std::collections::HashMap::new();
    for k in 0..cnt {
        let e = ifd + 2 + 12 * k;
        let tag = rd16(e)?;
        let type_name = rd16(e + 2)?;
        if type_name == 3 {
            tags.insert(tag, rd16(e + 8)?);
        }
    }
    let setting = tags.get(&0x1402).copied();
    let dev = tags.get(&0x1403).copied().filter(|&v| v >= 100);
    let auto = tags.get(&0x140b).copied().filter(|&v| v >= 100);
    match setting {
        Some(0x100) => Some(100),
        Some(0x200) => Some(200),
        Some(0x201) => Some(400),
        _ => dev.or(auto),
    }
}
