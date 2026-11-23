//! Reader for Sigma X3I files (Super Fine Detail bracket of the sd Quattro H: seven exposures in one file).
//!
//! Layout as observed: a header `FOVi` of [`HEADER_LEN`] bytes, then complete X3F files (`FOVb`), one behind the other,
//! each followed by [`PADDING`] zero bytes; at the end a directory block `SECs`. The X3F files are byte-identical to the
//! single files the camera software extracts. This reader cuts them out at the `FOVb` borders; every frame then opens
//! like any other X3F ([`crate::X3fFile`]).
//!
//! The order in the file is the base exposure first, then −3 … +3 EV; [`X3iFile::frames_by_exposure`] sorts them by
//! the exposure time of the embedded EXIF.

use crate::{Error, Result, X3fFile};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Magic of an X3I file.
pub const MAGIC: &[u8; 4] = b"FOVi";
/// Length of the header in front of the first X3F.
pub const HEADER_LEN: u64 = 1440;
/// Zero bytes behind each X3F.
pub const PADDING: u64 = 12;
/// Most frames a file may hold (the camera writes seven).
pub const MAX_FRAMES: usize = 64;

/// Is this file an X3I (by its first four bytes)?
pub fn is_x3i(path: impl AsRef<Path>) -> bool {
    let mut b = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == MAGIC
}

/// Split a virtual frame path `NAME.X3I#n` into the file and the 0-based frame number (`None` for any other path).
/// A file whose name really ends in `#n` (it exists) is not virtual.
pub fn split_virtual(path: &Path) -> Option<(PathBuf, usize)> {
    let s = path.to_str()?;
    let (file, n) = s.rsplit_once('#')?;
    let n: usize = n.parse().ok()?;
    let p = PathBuf::from(file);
    (!path.exists() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x3i"))).then_some((p, n))
}

/// An opened X3I: where each X3F lies, nothing is read into memory yet.
pub struct X3iFile {
    path: PathBuf,
    /// byte ranges `(start, end)` of the X3F files, in file order
    pub frames: Vec<(u64, u64)>,
}

fn corrupt<T>(why: &str) -> Result<T> {
    Err(Error::Corrupt(format!("X3I: {why}")))
}

/// Positions of the X3F files in `data` (`len` bytes of the file, read through `read_at`).
fn locate(len: u64, read_at: &mut dyn FnMut(u64, usize) -> std::io::Result<Vec<u8>>) -> Result<Vec<(u64, u64)>> {
    if len < HEADER_LEN + 64 || read_at(0, 4)? != MAGIC {
        return corrupt("not an X3I file");
    }
    // scan for `FOVb` in blocks; a start counts at the end of the header or behind PADDING zero bytes (compressed
    // image data could contain the four letters by chance, but not after a run of zeros)
    let mut starts: Vec<u64> = vec![];
    const BLOCK: usize = 8 << 20;
    let mut pos = HEADER_LEN;
    while pos < len {
        let n = (BLOCK as u64).min(len - pos) as usize;
        let want = (n + 3).min((len - pos) as usize);
        let buf = read_at(pos, want)?;
        let mut i = 0;
        while i + 4 <= buf.len() {
            if &buf[i..i + 4] == b"FOVb" && i < n {
                let at = pos + i as u64;
                let ok = if at == HEADER_LEN {
                    true
                } else {
                    at >= HEADER_LEN + PADDING && read_at(at - PADDING, PADDING as usize)?.iter().all(|b| *b == 0)
                };
                // a plausible X3F version follows (major 2 … 4)
                // (`read_at` shortens at the end of the file: a `FOVb` in the last bytes has no version behind it)
                let ver = read_at(at + 4, 4)?;
                let major = if ver.len() == 4 { u16::from_le_bytes([ver[2], ver[3]]) } else { 0 };
                if ok && (2..=4).contains(&major) {
                    if starts.len() == MAX_FRAMES {
                        return corrupt("too many frames");
                    }
                    starts.push(at);
                }
                i += 4;
            } else {
                i += 1;
            }
        }
        pos += n as u64;
    }
    if starts.is_empty() {
        return corrupt("no X3F inside");
    }
    // the directory block `SECs` at the end closes the last X3F
    let tail_len = 16384.min(len) as usize;
    let tail = read_at(len - tail_len as u64, tail_len)?;
    // (the padding is there behind the other frames; behind the last one the directory block may follow at once: the last
    // four bytes of an X3F are its directory pointer, never zero, so zeros before `SECs` are padding)
    let last_end = match tail.windows(4).rposition(|w| w == b"SECs") {
        Some(i) => {
            let at = len - tail_len as u64 + i as u64;
            if at >= PADDING && read_at(at - PADDING, PADDING as usize)?.iter().all(|b| *b == 0) {
                at - PADDING
            } else {
                at
            }
        }
        None => len,
    };
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let e = match starts.get(k + 1) {
            Some(n) => n - PADDING,
            None => last_end,
        };
        if e <= s + 8 {
            return corrupt("empty frame");
        }
        out.push((s, e));
    }
    Ok(out)
}

/// Frame ranges from the directory block at the end of the file (`SECf`, entries `FRAM`: offset, length; the last four
/// bytes of the file point to it). `None` if the block is missing or does not describe X3F files.
fn from_directory(len: u64, read_at: &mut dyn FnMut(u64, usize) -> std::io::Result<Vec<u8>>) -> Option<Vec<(u64, u64)>> {
    if len < HEADER_LEN + 64 || read_at(0, 4).ok()? != MAGIC {
        return None;
    }
    let tail = read_at(len - 4, 4).ok()?;
    let dir = u32::from_le_bytes(tail.try_into().ok()?) as u64;
    if dir < HEADER_LEN || dir + 12 > len {
        return None;
    }
    let head = read_at(dir, 12).ok()?;
    if &head[0..4] != b"SECf" {
        return None;
    }
    let count = u32::from_le_bytes(head[8..12].try_into().ok()?) as usize;
    if count == 0 || count > MAX_FRAMES || dir + 12 + count as u64 * 12 > len {
        return None;
    }
    let table = read_at(dir + 12, count * 12).ok()?;
    let mut out = Vec::new();
    for e in table.chunks_exact(12) {
        if &e[8..12] != b"FRAM" {
            continue;
        }
        let off = u32::from_le_bytes(e[0..4].try_into().ok()?) as u64;
        let size = u32::from_le_bytes(e[4..8].try_into().ok()?) as u64;
        if size <= 8 || off < HEADER_LEN || off + size > len || read_at(off, 4).ok()? != b"FOVb" {
            return None;
        }
        out.push((off, off + size));
    }
    (!out.is_empty()).then_some(out)
}

impl X3iFile {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut f = std::fs::File::open(&path)?;
        let len = f.metadata()?.len();
        let mut read_at = |off: u64, n: usize| -> std::io::Result<Vec<u8>> {
            f.seek(SeekFrom::Start(off))?;
            let mut b = vec![0u8; n.min(len.saturating_sub(off) as usize)];
            f.read_exact(&mut b)?;
            Ok(b)
        };
        // the directory at the end of the file lists the frames (a few bytes read instead of the whole file); files
        // without a usable directory are scanned
        let frames = match from_directory(len, &mut read_at) {
            Some(f) => f,
            None => locate(len, &mut read_at)?,
        };
        Ok(X3iFile { path, frames })
    }

    /// The bytes of frame `i` (an X3F file).
    pub fn frame_bytes(&self, i: usize) -> Result<Vec<u8>> {
        let &(s, e) = self.frames.get(i).ok_or_else(|| Error::Corrupt(format!("X3I: no frame {i}")))?;
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(s))?;
        let mut b = vec![0u8; (e - s) as usize];
        f.read_exact(&mut b)?;
        Ok(b)
    }

    /// Frame `i` as an X3F file.
    pub fn frame(&self, i: usize) -> Result<X3fFile> {
        X3fFile::from_bytes(self.frame_bytes(i)?)
    }

    /// Write frame `i` as an ordinary X3F file.
    pub fn extract(&self, i: usize, out: impl AsRef<Path>) -> Result<()> {
        std::fs::write(out, self.frame_bytes(i)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid X3F (no sections): `FOVb`, version 4.0, padding, `SECd`, count 0, directory pointer.
    fn tiny_x3f(tag: u8) -> Vec<u8> {
        let mut v = b"FOVb".to_vec();
        v.extend_from_slice(&0x0004_0000u32.to_le_bytes());
        v.extend_from_slice(&[tag; 8]);
        v.extend_from_slice(b"SECd");
        v.extend_from_slice(&0x0004_0000u32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&16u32.to_le_bytes());
        v
    }

    fn tiny_x3i(n: usize) -> Vec<u8> {
        tiny_x3i_padded(n, true)
    }

    fn tiny_x3i_padded(n: usize, last_padding: bool) -> Vec<u8> {
        let mut v = MAGIC.to_vec();
        v.resize(HEADER_LEN as usize, 7);
        for k in 0..n {
            v.extend(tiny_x3f(k as u8 + 1));
            if k + 1 < n || last_padding {
                v.extend([0u8; PADDING as usize]);
            }
        }
        v.extend_from_slice(b"SECs");
        v.extend_from_slice(&[9u8; 60]);
        v
    }

    #[test]
    fn cuts_frames_at_the_borders() {
        let data = tiny_x3i(7);
        let mut read = |off: u64, n: usize| -> std::io::Result<Vec<u8>> { Ok(data[off as usize..(off as usize + n).min(data.len())].to_vec()) };
        let frames = locate(data.len() as u64, &mut read).unwrap();
        assert_eq!(frames.len(), 7);
        for (k, &(s, e)) in frames.iter().enumerate() {
            assert_eq!(&data[s as usize..e as usize], &tiny_x3f(k as u8 + 1)[..]);
            assert!(X3fFile::from_bytes(data[s as usize..e as usize].to_vec()).is_ok());
        }
    }

    /// the real files: no padding between the last X3F and the directory block
    #[test]
    fn last_frame_without_padding() {
        let data = tiny_x3i_padded(3, false);
        let mut read = |off: u64, n: usize| -> std::io::Result<Vec<u8>> { Ok(data[off as usize..(off as usize + n).min(data.len())].to_vec()) };
        let frames = locate(data.len() as u64, &mut read).unwrap();
        let &(s, e) = frames.last().unwrap();
        assert_eq!(&data[s as usize..e as usize], &tiny_x3f(3)[..]);
    }

    /// A file cut off right behind a `FOVb` (no version bytes left) is refused, not a panic.
    #[test]
    fn truncated_behind_the_magic() {
        for cut in 0..=4usize {
            let mut data = MAGIC.to_vec();
            data.resize(HEADER_LEN as usize + 100, 7);
            data.extend([0u8; PADDING as usize]);
            data.extend_from_slice(b"FOVb");
            data.extend_from_slice(&0x0004_0000u32.to_le_bytes()[..cut]);
            let mut read = |off: u64, n: usize| -> std::io::Result<Vec<u8>> { Ok(data[(off as usize).min(data.len())..(off as usize + n).min(data.len())].to_vec()) };
            let r = locate(data.len() as u64, &mut read);
            if cut < 4 {
                assert!(r.is_err(), "cut {cut}");
            }
        }
        // and through the file reader
        let mut data = tiny_x3i(2);
        data.extend_from_slice(b"FOVb");
        let p = std::env::temp_dir().join(format!("x3i-truncated-{}.X3I", std::process::id()));
        std::fs::write(&p, &data).unwrap();
        let r = X3iFile::open(&p);
        std::fs::remove_file(&p).ok();
        assert_eq!(r.map(|f| f.frames.len()).ok(), Some(2));
    }

    /// The scan stops at an absurd number of frames.
    #[test]
    fn scan_limits_the_frame_count() {
        let data = tiny_x3i(MAX_FRAMES + 1);
        let mut read = |off: u64, n: usize| -> std::io::Result<Vec<u8>> { Ok(data[off as usize..(off as usize + n).min(data.len())].to_vec()) };
        assert!(locate(data.len() as u64, &mut read).is_err());
        let data = tiny_x3i(MAX_FRAMES);
        let mut read = |off: u64, n: usize| -> std::io::Result<Vec<u8>> { Ok(data[off as usize..(off as usize + n).min(data.len())].to_vec()) };
        assert_eq!(locate(data.len() as u64, &mut read).unwrap().len(), MAX_FRAMES);
    }

    #[test]
    fn virtual_paths() {
        let p = Path::new("/nowhere/SDQH1644.X3I#3");
        assert_eq!(split_virtual(p), Some((PathBuf::from("/nowhere/SDQH1644.X3I"), 3)));
        assert_eq!(split_virtual(Path::new("/nowhere/a.X3F#3")), None);
        assert_eq!(split_virtual(Path::new("/nowhere/a.X3I")), None);
    }

    #[test]
    fn refuses_other_files() {
        let data = tiny_x3f(1);
        let mut read = |off: u64, n: usize| -> std::io::Result<Vec<u8>> { Ok(data[off as usize..(off as usize + n).min(data.len())].to_vec()) };
        assert!(locate(data.len() as u64, &mut read).is_err());
    }
}
