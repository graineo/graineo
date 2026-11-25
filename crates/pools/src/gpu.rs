//! Whether the app may compute on the graphics card (setting "Grafikkarte verwenden", default on).
//!
//! One switch for every GPU path that has a processor path beside it: SPP noise reduction (Sigma), the noise
//! reduction of the Quattro preparation, film simulation, local corrections (masks) and the full-resolution stages of
//! other RAW formats. Off, all of them take their processor path (slower, the fan starts sooner; the Sigma path is the
//! bit-exact one). `GRAINEO_GPU=0` switches it off for test runs and tools; the setting changes it while running.
//! Takes effect with the next computation; images already computed stay as they are.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

/// 0 = not read yet, 1 = allowed, 2 = processor only
static STATE: AtomicU8 = AtomicU8::new(0);

/// A GPU device was lost (driver reset, sleep, Windows timeout detection): every GPU path stays on the processor
/// until the app restarts.
static LOST: AtomicBool = AtomicBool::new(false);
/// GPU errors caught so far (validation, out of memory) – a job that saw one discards its result.
static ERRORS: AtomicU64 = AtomicU64::new(0);

/// May the GPU be used?
pub fn gpu_allowed() -> bool {
    if LOST.load(Ordering::Relaxed) {
        return false;
    }
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var("GRAINEO_GPU").as_deref() != Ok("0");
            let _ = STATE.compare_exchange(0, if on { 1 } else { 2 }, Ordering::Relaxed, Ordering::Relaxed);
            STATE.load(Ordering::Relaxed) == 1
        }
    }
}

/// Allow or forbid the GPU (setting). `GRAINEO_GPU=0` always wins, so a test run stays on the processor.
pub fn set_gpu_allowed(on: bool) {
    let on = on && std::env::var("GRAINEO_GPU").as_deref() != Ok("0");
    STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
}

/// Error handler of a GPU device (`on_uncaptured_error`): count and log instead of wgpu's default, which ends the
/// thread with a panic. Jobs compare [`gpu_errors`] before and after and fall back to the processor.
pub fn gpu_error(source: &str, message: &str) {
    let n = ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 20 || n % 1000 == 0 {
        eprintln!("GPU error #{n} ({source}), the processor computes: {message}");
    }
}

/// The app has shown the lost device to the user (see [`take_gpu_lost_notice`]).
static LOST_SHOWN: AtomicBool = AtomicBool::new(false);

/// `true` exactly once after a GPU device was lost, so the app can tell the user that it now computes on the
/// processor (slower, same results).
pub fn take_gpu_lost_notice() -> bool {
    LOST.load(Ordering::Relaxed) && !LOST_SHOWN.swap(true, Ordering::Relaxed)
}

/// Number of GPU errors caught so far (see [`gpu_error`]).
pub fn gpu_errors() -> u64 {
    ERRORS.load(Ordering::Relaxed)
}

/// Device-lost callback of a GPU device: from now on everything computes on the processor.
pub fn gpu_lost(source: &str, message: &str) {
    if !LOST.swap(true, Ordering::Relaxed) {
        eprintln!("GPU device lost ({source}), computing on the processor from now on: {message}");
    }
    ERRORS.fetch_add(1, Ordering::Relaxed);
}

/// Share of the physical memory one GPU job may take at most. GPU buffers count towards the process on Apple
/// Silicon (unified memory): a 100-MP export needed 10 GB on the GPU path against 6 GB on the processor path, and a
/// 16-GB Mac started swapping. Above this share the processor computes (slower, but leaner).
pub const GPU_MEMORY_SHARE: f64 = 0.25;

/// Estimated GPU memory per image pixel of a full-resolution job (several whole f32 RGB buffers at once; measured:
/// GFX 100 S II, 102 MP, GPU path 9.9 GB peak against 6.2 GB on the processor path, ≈ 37–40 bytes per pixel more;
/// with 10 % margin). With it a 102-MP photo computes on the processor on a 16-GB Mac, a 45-MP one stays on the GPU
/// even with 8 GB.
pub const GPU_BYTES_PER_PIXEL: u64 = 44;

/// Physical memory of the machine in bytes (`None` if unknown).
pub fn physical_memory() -> Option<u64> {
    static MEM: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *MEM.get_or_init(read_physical_memory)
}

#[cfg(target_os = "macos")]
fn read_physical_memory() -> Option<u64> {
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    // SAFETY: sysctlbyname writes at most `len` bytes into `v`
    let r = unsafe { libc::sysctlbyname(name.as_ptr(), (&mut v as *mut u64).cast(), &mut len, std::ptr::null_mut(), 0) };
    (r == 0 && v > 0).then_some(v)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_physical_memory() -> Option<u64> {
    // SAFETY: plain queries without pointers
    let (pages, size) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
    (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
}

#[cfg(windows)]
fn read_physical_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: the structure is zeroed and carries its own length, as the call requires
    let mut s: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    s.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ok = unsafe { GlobalMemoryStatusEx(&mut s) } != 0;
    (ok && s.ullTotalPhys > 0).then_some(s.ullTotalPhys)
}

#[cfg(not(any(unix, windows)))]
fn read_physical_memory() -> Option<u64> {
    None
}

/// May a GPU job on an image of `pixels` pixels run, memory-wise? Its estimated need ([`GPU_BYTES_PER_PIXEL`])
/// must stay below [`GPU_MEMORY_SHARE`] of the physical memory; otherwise the processor computes. Unknown memory: yes.
pub fn gpu_fits_memory(pixels: usize) -> bool {
    fits(pixels, physical_memory())
}

fn fits(pixels: usize, memory: Option<u64>) -> bool {
    memory.is_none_or(|m| (pixels as u64).saturating_mul(GPU_BYTES_PER_PIXEL) as f64 <= m as f64 * GPU_MEMORY_SHARE)
}

#[cfg(test)]
mod tests {
    #[test]
    fn switch_follows_the_setting() {
        super::set_gpu_allowed(false);
        assert!(!super::gpu_allowed());
        super::set_gpu_allowed(true);
        assert_eq!(super::gpu_allowed(), std::env::var("GRAINEO_GPU").as_deref() != Ok("0"));
    }

    /// A caught GPU error is counted (jobs compare before/after and fall back to the processor) and does not
    /// switch the GPU off; only a lost device does (not tested here: it would switch it off for the whole test run).
    #[test]
    fn caught_errors_are_counted() {
        let before = super::gpu_errors();
        super::gpu_error("test", "validation error");
        assert!(super::gpu_errors() > before);
        assert!(!super::LOST.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// A 102-MP image (GFX 100) needs ≈ 4,5 GB on the GPU: on a 16-GB Mac the processor computes, on 32 GB the GPU;
    /// photos up to 45 MP stay on the GPU even with 8 GB.
    #[test]
    fn gpu_only_when_the_job_fits_a_quarter_of_memory() {
        const GB: u64 = 1 << 30;
        assert!(!super::fits(11_648 * 8_736, Some(16 * GB)));
        assert!(super::fits(11_648 * 8_736, Some(32 * GB)));
        assert!(super::fits(8_256 * 5_504, Some(8 * GB)));
        assert!(super::fits(usize::MAX, None));
        assert!(super::physical_memory().is_some_and(|m| m > GB));
    }
}
