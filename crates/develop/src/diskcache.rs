//! Disk cache for Sigma's SPP base.
//!
//! The SPP base (x3fuse-core: preprocessing, noise reduction, colour matrix, ColorDQ)
//! costs ≈ 1 s per image. It depends only on the file, the colour mode and the noise sliders
//! ([`crate::SppKey`]), which makes it ideal for caching. When reopened, the
//! image is SPP-accurate after a fraction of a second.
//!
//! * Location: `~/Library/Caches/graineo/spp/`, one file per image and key.
//! * Content: the f32 values **bit-exact** (byte planes separated, losslessly packed with Deflate,
//!   CRC32 per block), plus Sigma's detail image "L" for the sharpness slider (i16, likewise
//!   packed and checked) and the sharpness base. On reading, key, size and checksums are verified;
//!   on any mismatch the file is discarded and recomputed.
//! * Key: path, file size, modification time, colour mode, noise sliders and the version of the
//!   computing method [`COMPUTE_PATH_VERSION`] (plus the file format [`MAGIC`]). Not the program
//!   itself: an update that leaves the computation unchanged keeps the cache; one that changes it
//!   counts the version up, and the old entries no longer count and are removed ([`sweep_stale`]).
//! * Limit: automatic, at most [`MAX_BYTES`] and never so much that less than
//!   [`MIN_FREE_BYTES`] remain free on the volume. Oldest first (LRU via the
//!   modification time, which is refreshed on every hit).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use rayon::prelude::*;

use crate::{Linear, SppKey};

/// Version of the computing method of the base. Increase on every change that alters a stored base (x3fuse-core,
/// colour mode, noise reduction, Quattro/Classic preparation): entries of another version are never used and are
/// removed. Guard: the reference checksums of the Sigma development (`crates/graineo-dev/reference/sigma.txt`) carry
/// this version and a digest of their checksums in a header line; changed checksums without a new version fail the
/// test `reference_values_carry_the_compute_path_version` and `scripts/check.sh`.
///
/// History: 1.0 and earlier keyed on the program file (empty after every update); 9 = 1.0.1 (Quattro highlights
/// changed, so the bases of 1.0 must not be reused).
pub const COMPUTE_PATH_VERSION: u32 = 9;
/// Upper limit of the cache.
pub const MAX_BYTES: u64 = 20 << 30;
/// At least this much must remain free on the volume.
pub const MIN_FREE_BYTES: u64 = 15 << 30;

const MAGIC: &[u8; 8] = b"X3FSPPC3";
const SUFFIX: &str = "sppc";
/// f32 values per block (4 MB raw)
const CHUNK: usize = 1 << 20;

/// Cache directory (`GRAINEO_CACHE_DIR` overrides it; with `GRAINEO_DATA_DIR` it is `<DATA>/cache/spp`).
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("GRAINEO_CACHE_DIR") {
        return Some(PathBuf::from(d));
    }
    Some(sysdirs::app_cache_dir(sysdirs::APP)?.join("spp"))
}

/// Test variables that change the base without being part of the key also bypass the cache.
const UNKEYED_TEST_VARS: [&str; 2] = ["GRAINEO_NO_DISK_CACHE", "X3F_SPP_TEST"];

fn disabled() -> bool {
    UNKEYED_TEST_VARS.iter().any(|v| std::env::var_os(v).is_some())
}

fn min_free() -> u64 {
    std::env::var("GRAINEO_CACHE_MIN_FREE_GB").ok().and_then(|s| s.parse::<u64>().ok()).map_or(MIN_FREE_BYTES, |g| g << 30)
}

fn mtime_ns(m: &std::fs::Metadata) -> u128 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

/// Line of the key text that names the computing method; an entry without it (older version) never counts.
fn compute_line(version: u32) -> String {
    format!("compute={version}\n")
}

/// Full key text (also stored in the file header and compared on reading).
fn key_text(path: &Path, key: &SppKey, gpu: bool) -> Option<String> {
    key_text_for(path, key, gpu, COMPUTE_PATH_VERSION)
}

/// [`key_text`] for a given version of the computing method (tests simulate other versions).
fn key_text_for(path: &Path, key: &SppKey, gpu: bool, version: u32) -> Option<String> {
    let path = std::fs::canonicalize(path).ok()?;
    let m = std::fs::metadata(&path).ok()?;
    // Append CA correction only if on, so existing entries stay valid.
    let ca = if key.ca.is_off() { String::new() } else { format!("ca={:?}\n", key.ca) };
    // Fixed mark of the computing method (part of the key format), so that an entry written by an older
    // version never counts as this one, even if the program identity were the same
    let ca = ca + "methods=graineo\n";
    // Binning setting other than automatic (Quattro at high ISO, see `quattro::binning_factor`)
    let ca = if key.binning.is_auto() { ca } else { ca + &format!("binning={:?}\n", key.binning) };
    // Test switch for the alternative Merrill parts (only when set, so existing entries stay valid)
    // (only the parts that act in the base; the others act in the output stage)
    let new: Vec<&str> = x3f_core::merrill_new::active_parts().into_iter().filter(|t| ["outliers", "highlights", "ca", "noise"].contains(t)).collect();
    let ca = if new.is_empty() { ca } else { ca + &format!("merrill_new={}\n", new.join(",")) };
    // neural denoising after Sigma's noise reduction (only when on, so existing entries stay valid)
    let ca = if key.ai == 0 { ca } else { ca + &format!("ai={}\n", key.ai) };
    Some(format!(
        "path={}\nsize={}\nmtime={}\nmode={:?}\nnr={:?}\ngpu={}\n{}",
        path.display(),
        m.len(),
        mtime_ns(&m),
        key.mode,
        key.nr,
        gpu,
        compute_line(version),
    ) + &ca)
}

fn hash64(s: &str) -> u64 {
    // FNV-1a: stable across Rust versions
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// File name: image part + key part, so that "how many images" can be counted.
fn file_for(path: &Path, text: &str) -> Option<PathBuf> {
    let canon = std::fs::canonicalize(path).ok()?;
    Some(dir()?.join(format!("{:016x}-{:016x}.{SUFFIX}", hash64(&canon.to_string_lossy()), hash64(text))))
}

/// SPP base from the cache or newly computed (and then stored).
/// Bit-exactly the same result as [`crate::spp::working_key`] with the same arguments.
pub fn working_key(path: &Path, key: &SppKey, gpu: bool, cancel: &AtomicBool) -> Result<Linear, String> {
    LAST_HIT.with(|h| h.set(false));
    let gpu = gpu && pools::gpu_allowed();
    // Grey bases (black and white the Sigma way) are cheap to compute and are not stored
    // a mix of the neural denoising (0 < ai < 100) is cheap from its two stored bases and is not stored itself
    let mix = key.ai > 0 && key.ai < 100;
    let entry = if disabled() || key.mono || mix { None } else { key_text(path, key, gpu).and_then(|t| Some((file_for(path, &t)?, t))) };
    if let Some((file, text)) = &entry {
        if let Some(lin) = load(file, text) {
            log(|| format!("Plattencache: Treffer {}", file.display()));
            LAST_HIT.with(|h| h.set(true));
            return Ok(lin);
        }
    }
    let lin = if key.ai > 0 {
        crate::ai_denoise::working(path, key, cancel, &|k| working_key(path, k, gpu, cancel))?
    } else {
        crate::spp::working_key(path, key, gpu, cancel)?
    };
    if let Some((file, text)) = entry {
        let detail_bytes = lin.detail.as_ref().map_or(0, |d| d.l.len() * 2);
        // also when cancelled after all: the computation finished (a cancel in time ends it with an error), the base is
        // valid under its key – after a quick change of settings and back it is then read instead of computed again
        if fits((lin.data.len() * 4 + detail_bytes) as u64) {
            // Store in the background: the preview should not wait for the disk. Only a photo the user stays with
            // ([`set_current`], [`DWELL`]): a base is ≈ 100 MB on disk, and storing every photo paged past or
            // computed ahead wrote gigabytes (seen: 8.7 GB in 16 minutes).
            let copy = lin.clone();
            let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let generation = PENDING.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            SAVING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::Builder::new()
                .name("spp-cache".into())
                .spawn(move || {
                    let _done = SaveDone;
                    if !worth_storing(&canon, generation) {
                        log(|| format!("disk cache: not stored, photo not looked at ({})", canon.display()));
                        return;
                    }
                    let t = std::time::Instant::now();
                    match store(&file, &text, &copy) {
                        Ok(n) => log(|| format!("disk cache: stored {:.0} MB in {:.0?}", n as f64 / 1e6, t.elapsed())),
                        Err(e) => log(|| format!("disk cache: not stored ({e})")),
                    }
                })
                .ok();
        }
    }
    Ok(lin)
}

thread_local! {
    /// Did the last [`working_key`] on this thread read its base from the cache?
    static LAST_HIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Did the last [`working_key`] on this thread come from the cache? For the app's self-measurement: a hit is no
/// computing time. Ask on the same thread right after the call.
pub fn last_was_hit() -> bool {
    LAST_HIT.with(|h| h.get())
}

/// Stores running in the background right now ([`wait_for_memory`]).
static SAVING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts a background store as finished on every way out of its thread.
struct SaveDone;

impl Drop for SaveDone {
    fn drop(&mut self) {
        SAVING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Measuring tools: wait (at most `max`) until the background stores of computed bases are done, so that a
/// second run finds them.
pub fn wait_for_memory(max: std::time::Duration) {
    let start = std::time::Instant::now();
    while SAVING.load(std::sync::atomic::Ordering::SeqCst) > 0 && start.elapsed() < max {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// How long the user must stay with a photo before its computed base goes into the cache.
pub const DWELL: std::time::Duration = std::time::Duration::from_secs(3);
/// A base computed ahead is given up if its photo does not become the current one within this time.
const AHEAD_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// The photo the user looks at now (canonical path, since when); `None` until the app reports one – then every
/// computed base is stored as before (measuring tools, tests).
static CURRENT: std::sync::Mutex<Option<(PathBuf, std::time::Instant)>> = std::sync::Mutex::new(None);
/// Counter of bases waiting to be stored (the newest one may wait without being current: it was just computed).
static PENDING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The app reports the photo the user looks at (opened in Develop, single view). Bases are stored only for a photo
/// that stays current for [`DWELL`].
pub fn set_current(path: &Path) {
    set_current_in(&CURRENT, path);
}

type Current = std::sync::Mutex<Option<(PathBuf, std::time::Instant)>>;

fn set_current_in(current: &Current, path: &Path) {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut g = current.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|(p, _)| *p != canon) {
        *g = Some((canon, std::time::Instant::now()));
    }
}

/// Wait until it is clear whether the base of `canon` is worth storing: yes when the photo has been current for
/// [`DWELL`] (also a photo computed ahead that the user then goes to); no when the user went on, or when a newer
/// base waits and this photo is not the current one, or after [`AHEAD_LIMIT`].
fn worth_storing(canon: &Path, generation: u64) -> bool {
    worth_storing_in(&CURRENT, &PENDING, canon, generation)
}

/// [`worth_storing`] with explicit state (tests use their own, since other tests compute bases in parallel and
/// count up [`PENDING`]).
fn worth_storing_in(current: &Current, pending: &std::sync::atomic::AtomicU64, canon: &Path, generation: u64) -> bool {
    let start = std::time::Instant::now();
    loop {
        let current = current.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some((p, since)) = current else { return true };
        if p == canon {
            if since.elapsed() >= DWELL {
                return true;
            }
        } else if pending.load(std::sync::atomic::Ordering::SeqCst) != generation || start.elapsed() >= AHEAD_LIMIT {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn log(f: impl FnOnce() -> String) {
    if std::env::var_os("GRAINEO_LOG").is_some() {
        eprintln!("{}", f());
    }
}

// ---------------------------------------------------------------------------------------
// File format
//
//   MAGIC (8) | header length u32 | header (key text, UTF-8) | width u64 | height u64 |
//   block count u32 | per block: raw length u32 (values), packed length u32, CRC32 of the raw bytes u32
//   | block data … | detail image: present u8; if 1: base f32, scale f32, block count u32,
//   block table as above (values i16), block data (two byte planes, Deflate)
//
// Per block the four bytes of each f32 are split into planes. The two lower
// mantissa bytes are practically noise and stay unpacked (fast); the two upper ones
// (sign, exponent, high mantissa) are differenced against the pixel to the left (same channel,
// distance 3) and packed with Deflate (level 1). Lossless.

fn encode_block(c: &[f32]) -> Vec<u8> {
    let n = c.len();
    let sh = shuffle(c);
    let mut hi = sh[2 * n..].to_vec();
    for plane in hi.chunks_mut(n) {
        for i in (3..n).rev() {
            plane[i] = plane[i].wrapping_sub(plane[i - 3]);
        }
    }
    let mut out = sh[..2 * n].to_vec();
    let mut enc = flate2::write::DeflateEncoder::new(out.split_off(2 * n), flate2::Compression::fast());
    enc.write_all(&hi).unwrap();
    out.extend_from_slice(&enc.finish().unwrap());
    out
}

fn decode_block(src: &[u8], out: &mut [f32]) -> bool {
    let n = out.len();
    let Some(lo) = src.get(..2 * n) else { return false };
    let mut all = Vec::with_capacity(4 * n);
    all.extend_from_slice(lo);
    if flate2::read::DeflateDecoder::new(&src[2 * n..]).read_to_end(&mut all).is_err() || all.len() != 4 * n {
        return false;
    }
    for plane in all[2 * n..].chunks_mut(n) {
        for i in 3..n {
            plane[i] = plane[i].wrapping_add(plane[i - 3]);
        }
    }
    unshuffle(&all, out);
    true
}

fn shuffle(vals: &[f32]) -> Vec<u8> {
    let n = vals.len();
    let mut out = vec![0u8; n * 4];
    for (i, v) in vals.iter().enumerate() {
        let b = v.to_bits().to_le_bytes();
        out[i] = b[0];
        out[n + i] = b[1];
        out[2 * n + i] = b[2];
        out[3 * n + i] = b[3];
    }
    out
}

fn unshuffle(bytes: &[u8], out: &mut [f32]) {
    let n = out.len();
    for (i, o) in out.iter_mut().enumerate() {
        *o = f32::from_bits(u32::from_le_bytes([bytes[i], bytes[n + i], bytes[2 * n + i], bytes[3 * n + i]]));
    }
}

fn crc_of(vals: &[f32]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    for v in vals {
        h.update(&v.to_bits().to_le_bytes());
    }
    h.finalize()
}

fn encode_l(c: &[i16]) -> Vec<u8> {
    let n = c.len();
    let mut planes = vec![0u8; 2 * n];
    for (i, v) in c.iter().enumerate() {
        let b = v.to_le_bytes();
        planes[i] = b[0];
        planes[n + i] = b[1];
    }
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&planes).unwrap();
    enc.finish().unwrap()
}

fn decode_l(src: &[u8], out: &mut [i16]) -> bool {
    let n = out.len();
    let mut planes = Vec::with_capacity(2 * n);
    if flate2::read::DeflateDecoder::new(src).read_to_end(&mut planes).is_err() || planes.len() != 2 * n {
        return false;
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = i16::from_le_bytes([planes[i], planes[n + i]]);
    }
    true
}

fn crc_l(vals: &[i16]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    for v in vals {
        h.update(&v.to_le_bytes());
    }
    h.finalize()
}

/// Complete file content (format above) for `lin` under the key text `text`.
fn serialize(text: &str, lin: &Linear) -> Vec<u8> {
    let blocks: Vec<(u32, u32, Vec<u8>)> = lin
        .data
        .par_chunks(CHUNK)
        .map(|c| {
            (c.len() as u32, crc_of(c), encode_block(c))
        })
        .collect();
    let mut buf = Vec::with_capacity(blocks.iter().map(|b| b.2.len()).sum::<usize>() + 4096);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&(text.len() as u32).to_le_bytes());
    buf.extend_from_slice(text.as_bytes());
    buf.extend_from_slice(&(lin.width as u64).to_le_bytes());
    buf.extend_from_slice(&(lin.height as u64).to_le_bytes());
    buf.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    for (n, crc, data) in &blocks {
        buf.extend_from_slice(&n.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
    }
    for (_, _, data) in &blocks {
        buf.extend_from_slice(data);
    }
    match &lin.detail {
        None => buf.push(0),
        Some(d) => {
            let lb: Vec<(u32, u32, Vec<u8>)> = d.l.par_chunks(CHUNK).map(|c| (c.len() as u32, crc_l(c), encode_l(c))).collect();
            buf.push(1);
            buf.extend_from_slice(&d.base.to_bits().to_le_bytes());
            buf.extend_from_slice(&d.scale.to_bits().to_le_bytes());
            buf.extend_from_slice(&(lb.len() as u32).to_le_bytes());
            for (n, crc, data) in &lb {
                buf.extend_from_slice(&n.to_le_bytes());
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(&crc.to_le_bytes());
            }
            for (_, _, data) in &lb {
                buf.extend_from_slice(data);
            }
        }
    }
    buf
}

fn store(file: &Path, text: &str, lin: &Linear) -> std::io::Result<u64> {
    let need_est = (lin.data.len() * 4 + lin.detail.as_ref().map_or(0, |d| d.l.len() * 2)) as u64;
    let dir = file.parent().ok_or_else(|| std::io::Error::other("no directory"))?;
    std::fs::create_dir_all(dir)?;
    // Make room first (incl. reserve for the new file), otherwise do not store at all.
    if !make_room(dir, need_est) {
        return Err(std::io::Error::other("not enough space"));
    }
    let buf = serialize(text, lin);
    // Atomic: write under a temporary name first, then rename
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
    }
    std::fs::rename(&tmp, file)?;
    make_room(dir, 0);
    Ok(buf.len() as u64)
}

/// Size in bytes of the cache file for `lin` (without the key text in the header; writes nothing).
pub fn encoded_size(lin: &Linear) -> u64 {
    serialize("", lin).len() as u64
}

fn load(file: &Path, text: &str) -> Option<Linear> {
    let t = std::time::Instant::now();
    let mut f = std::fs::File::open(file).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).ok()?;
    let t_read = t.elapsed();
    let lin = parse(&buf, text);
    log(|| format!("disk cache: read {t_read:.0?}, unpacked+verified {:.0?}", t.elapsed() - t_read));
    if lin.is_none() {
        // Damaged or outdated: discard
        let _ = std::fs::remove_file(file);
        return None;
    }
    // LRU: record access
    let _ = f.set_modified(std::time::SystemTime::now());
    lin
}

fn parse(buf: &[u8], text: &str) -> Option<Linear> {
    let mut p = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = buf.get(p..p + n)?;
        p += n;
        Some(s)
    };
    if take(8)? != MAGIC {
        return None;
    }
    let hl = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    if take(hl)? != text.as_bytes() {
        return None;
    }
    let width = u64::from_le_bytes(take(8)?.try_into().ok()?) as usize;
    let height = u64::from_le_bytes(take(8)?.try_into().ok()?) as usize;
    let nb = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    let mut table = Vec::with_capacity(nb);
    for _ in 0..nb {
        let e = take(12)?;
        let n = u32::from_le_bytes(e[0..4].try_into().ok()?) as usize;
        let len = u32::from_le_bytes(e[4..8].try_into().ok()?) as usize;
        let crc = u32::from_le_bytes(e[8..12].try_into().ok()?);
        table.push((n, len, crc));
    }
    let total: usize = table.iter().map(|t| t.0).sum();
    if total != width.checked_mul(height)?.checked_mul(3)? || table.iter().any(|t| t.0 > CHUNK) {
        return None;
    }
    // Output and input sections per block
    let mut data = vec![0f32; total];
    let mut jobs = Vec::with_capacity(nb);
    let mut rest: &mut [f32] = &mut data;
    let mut off = p;
    for &(n, len, crc) in &table {
        let (head, tail) = std::mem::take(&mut rest).split_at_mut(n);
        rest = tail;
        jobs.push((head, buf.get(off..off + len)?, crc));
        off += len;
    }
    let ok = jobs.into_par_iter().all(|(out, src, crc)| {
        decode_block(src, out) && crc_of(out) == crc
    });
    if !ok {
        return None;
    }
    let detail = parse_detail(buf.get(off..)?, width * height)?;
    Some(Linear { mono: false, haze: None, toneq: None, width, height, data, detail })
}

/// Detail image section; outer `None` = damaged, inner = not present.
fn parse_detail(buf: &[u8], pixels: usize) -> Option<Option<std::sync::Arc<crate::sharpness::SharpDetail>>> {
    let mut p = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = buf.get(p..p + n)?;
        p += n;
        Some(s)
    };
    match take(1)?[0] {
        0 => return Some(None),
        1 => {}
        _ => return None,
    }
    let word = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
    let base = f32::from_bits(word(take(4)?));
    let scale = f32::from_bits(word(take(4)?));
    let nb = word(take(4)?) as usize;
    let mut table = Vec::with_capacity(nb);
    for _ in 0..nb {
        let e = take(12)?;
        table.push((word(&e[0..4]) as usize, word(&e[4..8]) as usize, word(&e[8..12])));
    }
    if table.iter().map(|t| t.0).sum::<usize>() != pixels || table.iter().any(|t| t.0 > CHUNK) {
        return None;
    }
    let mut l = vec![0i16; pixels];
    let mut jobs = Vec::with_capacity(nb);
    let mut rest: &mut [i16] = &mut l;
    let mut off = p;
    for &(n, len, crc) in &table {
        let (head, tail) = std::mem::take(&mut rest).split_at_mut(n);
        rest = tail;
        jobs.push((head, buf.get(off..off + len)?, crc));
        off += len;
    }
    let ok = jobs.into_par_iter().all(|(out, src, crc)| decode_l(src, out) && crc_l(out) == crc);
    ok.then(|| Some(std::sync::Arc::new(crate::sharpness::SharpDetail { l, base, scale })))
}

// ---------------------------------------------------------------------------------------
// Size, cleanup, statistics

fn entries(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| {
        let e = e.ok()?;
        let p = e.path();
        if p.extension()? != SUFFIX {
            // dispose of leftover temporary files (older than 10 min)
            if p.extension().is_some_and(|x| x.to_string_lossy().starts_with("tmp")) {
                let old = e.metadata().ok()?.modified().ok()?.elapsed().ok()? > std::time::Duration::from_secs(600);
                if old {
                    let _ = std::fs::remove_file(&p);
                }
            }
            return None;
        }
        let m = e.metadata().ok()?;
        Some((p, m.len(), m.modified().ok()?))
    })
    .collect()
}

/// Free space on the volume of `dir` (available to the user).
#[cfg(windows)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let w: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut avail_bytes: u64 = 0;
    // SAFETY: null-terminated UTF-16 path, output into a valid variable
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(w.as_ptr(), &mut avail_bytes, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(avail_bytes)
}

/// Free space on the volume of `dir` (available to the user).
#[cfg(unix)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid, null-terminated path and sufficiently large buffer
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_bsize as u64)
}

/// Allowed total size: at most [`MAX_BYTES`], and after writing
/// [`MIN_FREE_BYTES`] must still be free. `used` = current size of the cache.
fn budget(dir: &Path, used: u64) -> u64 {
    let free = free_bytes(dir).unwrap_or(0);
    MAX_BYTES.min((free + used).saturating_sub(min_free()))
}

/// Delete the oldest entries until `extra` additional bytes fit into the budget.
/// `false` if even an empty cache is not enough.
fn make_room(dir: &Path, extra: u64) -> bool {
    let mut list = entries(dir);
    list.sort_by_key(|e| e.2);
    let mut used: u64 = list.iter().map(|e| e.1).sum();
    let budget = budget(dir, used);
    if extra > budget {
        return false;
    }
    for (p, len, _) in list {
        if used + extra <= budget {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            used -= len;
        }
    }
    true
}

/// Does an entry of this size fit into the budget at all (possibly after cleanup)?
fn fits(extra: u64) -> bool {
    let Some(d) = dir() else { return false };
    let used: u64 = entries(&d).iter().map(|e| e.1).sum();
    // Directory may not exist yet: measure free space at the nearest existing parent folder
    let probe = d.ancestors().find(|p| p.exists()).unwrap_or(&d).to_path_buf();
    budget(&probe, used) >= extra
}

/// Is the cache currently paused because the volume is too full?
pub fn paused() -> bool {
    !fits(200_000_000)
}

/// Cache usage: (bytes, number of images).
pub fn stats() -> (u64, usize) {
    let Some(d) = dir() else { return (0, 0) };
    let list = entries(&d);
    let images: std::collections::HashSet<String> = list
        .iter()
        .filter_map(|e| e.0.file_name()?.to_string_lossy().split('-').next().map(str::to_owned))
        .collect();
    (list.iter().map(|e| e.1).sum(), images.len())
}

/// Remove the entries of another computing method or file format (left by an earlier version after an update) and
/// leftover temporary files. Reads only the header of each file. Returns the number of removed entries.
pub fn sweep_stale() -> usize {
    dir().map_or(0, |d| sweep_stale_in(&d, COMPUTE_PATH_VERSION))
}

fn sweep_stale_in(dir: &Path, version: u32) -> usize {
    let mut removed = 0;
    for (p, _, _) in entries(dir) {
        if !written_by(&p, version) && std::fs::remove_file(&p).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Was this cache file written in the current format by the computing method `version`? Reads only the header.
fn written_by(file: &Path, version: u32) -> bool {
    let Ok(mut f) = std::fs::File::open(file) else { return false };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() || &head[..8] != MAGIC {
        return false;
    }
    let len = u32::from_le_bytes(head[8..12].try_into().unwrap()) as usize;
    if len > 1 << 16 {
        return false;
    }
    let mut text = vec![0u8; len];
    f.read_exact(&mut text).is_ok() && String::from_utf8_lossy(&text).contains(&format!("\n{}", compute_line(version)))
}

/// Clear the cache.
pub fn clear() {
    if let Some(d) = dir() {
        for (p, _, _) in entries(&d) {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_photos_the_user_stays_with_are_stored() {
        // own state: other tests in this crate compute bases in parallel and count up the global PENDING
        static CUR: Current = std::sync::Mutex::new(None);
        static PEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        use std::sync::atomic::Ordering::SeqCst;
        let (a, b, c) = (PathBuf::from("/x/A.X3F"), PathBuf::from("/x/B.X3F"), PathBuf::from("/x/C.X3F"));
        // the current photo, looked at for the dwell time: stored
        set_current_in(&CUR, &a);
        let g = PEN.fetch_add(1, SeqCst) + 1;
        let t = std::time::Instant::now();
        assert!(worth_storing_in(&CUR, &PEN, &a, g));
        assert!(t.elapsed() >= DWELL - std::time::Duration::from_millis(250));
        // computed ahead (B) while the user stays with A, then a newer one (C) waits: B is given up at once
        let gb = PEN.fetch_add(1, SeqCst) + 1;
        let _gc = PEN.fetch_add(1, SeqCst) + 1;
        assert!(!worth_storing_in(&CUR, &PEN, &b, gb));
        // computed ahead and then gone to: stored once the user stays there
        let gc = PEN.load(SeqCst);
        let c2 = c.clone();
        let h = std::thread::spawn(move || worth_storing_in(&CUR, &PEN, &c2, gc));
        std::thread::sleep(std::time::Duration::from_millis(500));
        set_current_in(&CUR, &c);
        assert!(h.join().unwrap());
    }

    /// Small base with detail image for the cache tests.
    fn small_base() -> Linear {
        let (w, h) = (64usize, 8usize);
        let data: Vec<f32> = (0..w * h * 3).map(|i| (i % 977) as f32 * 0.5).collect();
        let detail = crate::sharpness::SharpDetail { l: (0..w * h).map(|i| (i % 300) as i16).collect(), base: 0.5, scale: 1.0 };
        Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: Some(std::sync::Arc::new(detail)) }
    }

    #[test]
    fn cache_survives_a_new_program_but_not_a_new_compute_path() {
        std::env::set_var("GRAINEO_CACHE_MIN_FREE_GB", "0");
        let dir = std::env::temp_dir().join(format!("x3f-cache-version-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("PHOTO.X3F");
        std::fs::write(&photo, b"not a real photo, only its path, size and time count").unwrap();
        let key = SppKey::default();
        let text = key_text(&photo, &key, false).unwrap();
        assert!(text.contains(&compute_line(COMPUTE_PATH_VERSION)), "{text}");
        // nothing of the program itself in the key
        assert!(!text.contains("exe=") && !text.contains("app="), "{text}");
        let file = dir.join(file_for(&photo, &text).unwrap().file_name().unwrap());
        let lin = small_base();
        store(&file, &text, &lin).unwrap();

        // another program (update without a change of the computing method): its file lies elsewhere and has another
        // time – the key stays the same and the entry is used
        let exe = std::env::current_exe().unwrap();
        let other = dir.join("graineo-update");
        std::fs::copy(&exe, &other).unwrap();
        std::fs::File::options().write(true).open(&other).unwrap().set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)).unwrap();
        let output = std::process::Command::new(&other).args(["--exact", "diskcache::tests::print_key_text", "--nocapture"]).env("GRAINEO_KEY_TEXT_OF", &photo).output().unwrap();
        let printed = String::from_utf8_lossy(&output.stdout);
        let other_text = printed.split("KEY<").nth(1).and_then(|r| r.split(">KEY").next()).expect("key text printed by the other program");
        assert_eq!(other_text, text, "another program file must give the same key");
        let back = load(&file, other_text).expect("entry reused by the other program");
        assert!(back.data.iter().zip(&lin.data).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(written_by(&file, COMPUTE_PATH_VERSION));
        assert_eq!(sweep_stale_in(&dir, COMPUTE_PATH_VERSION), 0);
        assert!(file.exists());

        // another computing method: another key (other file name), the old entry does not count …
        let newer = key_text_for(&photo, &key, false, COMPUTE_PATH_VERSION + 1).unwrap();
        assert_ne!(newer, text);
        assert_ne!(file_for(&photo, &newer), file_for(&photo, &text));
        assert!(!written_by(&file, COMPUTE_PATH_VERSION + 1));
        // … and the newer version removes it
        assert_eq!(sweep_stale_in(&dir, COMPUTE_PATH_VERSION + 1), 1);
        assert!(!file.exists());

        // entries of 1.0 (key with the program file, no compute line) and of an older format are removed as well
        let old_key = text.replace(&compute_line(COMPUTE_PATH_VERSION), "app=1.0.0\npipeline=8\nexe=123:456\n");
        let old_file = dir.join("0000000000000001-0000000000000002.sppc");
        store(&old_file, &old_key, &lin).unwrap();
        let mut bytes = std::fs::read(&old_file).unwrap();
        let older_format = dir.join("0000000000000003-0000000000000004.sppc");
        bytes[..8].copy_from_slice(b"X3FSPPC2");
        std::fs::write(&older_format, &bytes).unwrap();
        assert!(load(&old_file, &text).is_none());
        assert_eq!(sweep_stale_in(&dir, COMPUTE_PATH_VERSION), 1, "the 1.0 entry was already discarded by the failed load");
        assert!(!older_format.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Helper for [`cache_survives_a_new_program_but_not_a_new_compute_path`]: run from a copy of the test program, it
    /// prints the key text of the photo named in `GRAINEO_KEY_TEXT_OF`.
    #[test]
    fn print_key_text() {
        if let Some(p) = std::env::var_os("GRAINEO_KEY_TEXT_OF") {
            println!("KEY<{}>KEY", key_text(Path::new(&p), &SppKey::default(), false).unwrap());
        }
    }

    /// Digest of the checksums in the reference file (kind, file, checksum of every check line).
    fn reference_digest(text: &str) -> u64 {
        let lines: String = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" ") + "\n")
            .collect();
        hash64(&lines)
    }

    #[test]
    fn reference_values_carry_the_compute_path_version() {
        // Changed checksums of the Sigma development mean a changed base: the disk cache must not keep the old one.
        let text = include_str!("../../graineo-dev/reference/sigma.txt");
        let digest = reference_digest(text);
        let head = text.lines().find_map(|l| l.strip_prefix("# compute-path-version:")).expect("header line '# compute-path-version: <n> digest: <hex>' in sigma.txt");
        let mut words = head.split_whitespace();
        let version: u32 = words.next().and_then(|v| v.parse().ok()).expect("version number");
        let stored = words.nth(1).and_then(|d| u64::from_str_radix(d, 16).ok()).expect("digest");
        assert_eq!(
            stored, digest,
            "The reference checksums changed: increase COMPUTE_PATH_VERSION in crates/develop/src/diskcache.rs (now {COMPUTE_PATH_VERSION}) and write the header line of sigma.txt as '# compute-path-version: {} digest: {digest:016x}'",
            COMPUTE_PATH_VERSION + 1
        );
        assert_eq!(version, COMPUTE_PATH_VERSION, "header line of sigma.txt and COMPUTE_PATH_VERSION differ");
    }

    #[test]
    fn limits_are_20_gb_and_15_gb() {
        assert_eq!(MAX_BYTES, 20 * (1 << 30));
        assert_eq!(MIN_FREE_BYTES, 15 * (1 << 30));
    }

    #[test]
    fn roundtrip_bitexact() {
        let n = (CHUNK * 2 / 3 + 123) * 3;
        let mut x = 0x1234_5678u32;
        let data: Vec<f32> = (0..n)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                if i % 7 == 0 { f32::from_bits(x) } else { (x % 60000) as f32 * 0.37 }
            })
            .collect();
        let l: Vec<i16> = (0..n / 3).map(|i| (i as i16).wrapping_mul(7)).collect();
        let detail = crate::sharpness::SharpDetail { l, base: 0.8, scale: 1.0 };
        let lin = Linear { mono: false, haze: None, toneq: None, width: n / 3, height: 1, data, detail: Some(std::sync::Arc::new(detail)) };
        let dir = std::env::temp_dir().join(format!("x3f-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a-b.sppc");
        // without space check: encode/decode directly
        let text = "k";
        let blocks: Vec<_> = lin.data.chunks(CHUNK).map(|c| { let s = shuffle(c); let mut o = vec![0f32; c.len()]; unshuffle(&s, &mut o); (c.to_vec(), o) }).collect();
        for (a, b) in blocks {
            assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()));
        }
        std::env::set_var("GRAINEO_CACHE_MIN_FREE_GB", "0");
        store(&file, text, &lin).unwrap();
        let back = load(&file, text).unwrap();
        assert_eq!(back.width, lin.width);
        assert!(back.data.iter().zip(&lin.data).all(|(x, y)| x.to_bits() == y.to_bits()));
        let (d0, d1) = (lin.detail.as_ref().unwrap(), back.detail.as_ref().unwrap());
        assert_eq!((d0.l.as_slice(), d0.base, d0.scale), (d1.l.as_slice(), d1.base, d1.scale));
        assert!(load(&file, "anders").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn encoded_size_matches_stored_file() {
        let (w, h) = (700usize, 20usize);
        let mut x = 0x9e37_79b9u32;
        let data: Vec<f32> = (0..w * h * 3)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x % 4000) as f32 * 0.25
            })
            .collect();
        let l: Vec<i16> = (0..w * h).map(|i| (i % 300) as i16).collect();
        let detail = crate::sharpness::SharpDetail { l, base: 0.5, scale: 1.0 };
        let lin = Linear { mono: false, haze: None, toneq: None, width: w, height: h, data, detail: Some(std::sync::Arc::new(detail)) };
        let dir = std::env::temp_dir().join(format!("x3f-cache-size-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("c-d.sppc");
        let text = "key-for-the-test";
        std::env::set_var("GRAINEO_CACHE_MIN_FREE_GB", "0");
        store(&file, text, &lin).unwrap();
        let on_disk = std::fs::metadata(&file).unwrap().len();
        let est = encoded_size(&lin);
        let diff = on_disk.abs_diff(est);
        assert!(diff <= text.len() as u64 + 64, "file {on_disk}, estimated {est}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
