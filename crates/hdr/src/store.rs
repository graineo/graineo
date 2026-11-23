//! Storing a merged result: half floats (relative error ≤ 0.05 %, range up to ±65 504, negative values kept, larger
//! values clamped to that range), planar with the bytes of each value split into two planes (better compression),
//! deflate. Header: magic, version, size, noise model.

use crate::Merged;
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use half::f16;
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"GRNHDR01";
/// Largest image the reader accepts (pixels): more than any merge, small enough that a damaged header cannot ask for
/// gigabytes.
pub const MAX_PIXELS: usize = 1 << 28;

/// Largest finite value a stored sample can hold.
pub const MAX_VALUE: f32 = 65504.0;

/// The stored form of one sample: never infinite and never NaN. Values beyond the half-float range (a very wide
/// bracket with the brightest frame as reference) are clamped to [`MAX_VALUE`] instead of turning into `inf`, which
/// would poison every filter that touches the pixel; NaN becomes black.
#[inline]
fn to_half(v: f32) -> f16 {
    if v.is_nan() {
        f16::ZERO
    } else {
        f16::from_f32(v.clamp(-MAX_VALUE, MAX_VALUE))
    }
}

/// Round the samples of `m` to what a stored result holds (half floats, see [`to_half`]). A base that goes on being
/// used in memory after it was computed is rounded with this, so that everything made from it is the same whether the
/// base comes from memory or is read back from the cache.
pub fn quantize(m: &mut Merged) {
    use rayon::prelude::*;
    m.px.par_iter_mut().for_each(|p| *p = p.map(|v| to_half(v).to_f32()));
}

/// Write `m` to `path` atomically (temporary file of this writer alone, then rename). Nothing is left behind when
/// writing fails.
pub fn write(path: &Path, m: &Merged) -> std::io::Result<()> {
    let n = m.w * m.h;
    let mut planes = vec![0u8; n * 6];
    for c in 0..3 {
        for (i, p) in m.px.iter().enumerate() {
            // signed: values below black (noise around zero) are kept, so the mean of deep shadows stays where the
            // merge put it – cut off at zero the shadows of a cached merge would come out brighter than computed
            let b = to_half(p[c]).to_bits();
            planes[(2 * c) * n + i] = (b >> 8) as u8;
            planes[(2 * c + 1) * n + i] = b as u8;
        }
    }
    let mut head = Vec::with_capacity(64);
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&(m.w as u32).to_le_bytes());
    head.extend_from_slice(&(m.h as u32).to_le_bytes());
    for v in m.noise.a.iter().chain(m.noise.b.iter()) {
        head.extend_from_slice(&v.to_le_bytes());
    }
    let tmp = tmp_path(path);
    let done = (|| {
        let f = std::fs::File::create(&tmp)?;
        let mut f = std::io::BufWriter::new(f);
        f.write_all(&head)?;
        let mut z = ZlibEncoder::new(f, Compression::fast());
        z.write_all(&planes)?;
        z.finish()?.flush()?;
        std::fs::rename(&tmp, path)
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Extension of the temporary files of [`write`].
pub const TMP_EXTENSION: &str = "tmp";

/// Temporary file next to `path`, unique per writer (process and running number): two writers of the same result
/// never share one, so neither renames the half-written data of the other into place.
fn tmp_path(path: &Path) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_stem().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{n}.{TMP_EXTENSION}", std::process::id()));
    path.with_file_name(name)
}

/// Age above which a temporary file counts as left behind by a writer that was interrupted (the app quit or crashed
/// while writing). A write takes seconds and touches its file all the time, so no running writer is that old.
pub const STALE_TMP_AGE: std::time::Duration = std::time::Duration::from_secs(600);

/// Remove the temporary files of [`write`] in `dir` that were last written more than `older_than` ago. Returns the
/// bytes freed.
pub fn remove_stale_tmp(dir: &Path, older_than: std::time::Duration) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut freed = 0;
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != TMP_EXTENSION) {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        // a time that cannot be read, or one in the future, never counts as old
        let stale = m.is_file() && m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > older_than);
        if stale && std::fs::remove_file(&path).is_ok() {
            freed += m.len();
        }
    }
    freed
}

/// Read a result written by [`write`].
pub fn read(path: &Path) -> std::io::Result<Merged> {
    let bad = |s: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, s.to_string());
    let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut head = [0u8; 8 + 8 + 24];
    f.read_exact(&mut head)?;
    if &head[..8] != MAGIC {
        return Err(bad("not a merge result"));
    }
    let u = |o: usize| u32::from_le_bytes(head[o..o + 4].try_into().unwrap()) as usize;
    let fl = |o: usize| f32::from_le_bytes(head[o..o + 4].try_into().unwrap());
    let (w, h) = (u(8), u(12));
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err(bad("size"));
    }
    let noise = raw::noise::NoiseModel { a: [fl(16), fl(20), fl(24)], b: [fl(28), fl(32), fl(36)] };
    let n = w * h;
    // the memory follows the data really there (a damaged header reserves nothing), and the stream ends at the size
    // the header names (one byte more shows a stream that is too long)
    let mut planes = Vec::new();
    ZlibDecoder::new(f).take(n as u64 * 6 + 1).read_to_end(&mut planes)?;
    if planes.len() != n * 6 {
        return Err(bad("truncated"));
    }
    let px = (0..n)
        .map(|i| std::array::from_fn(|c| f16::from_bits(((planes[(2 * c) * n + i] as u16) << 8) | planes[(2 * c + 1) * n + i] as u16).to_f32()))
        .collect();
    Ok(Merged { px, w, h, noise })
}
