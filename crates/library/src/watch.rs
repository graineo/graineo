//! Notice changes on disk without rereading everything:
//! - local drives: macOS FSEvents (per-file events, latency ~1 s); the app then syncs
//!   only the affected root folder.
//! - network drives (SMB/AFP/NFS, where FSEvents does not reliably report changes made by other computers):
//!   a regular cheap sync (only `stat`/`read_dir`) and immediately when the volume
//!   is (re)mounted.

use std::ffi::{c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Is the path on a local drive?
pub fn local(p: &Path) -> bool {
    crate::platform::local(p)
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::c_void;
    pub type Callback = extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);
    #[repr(C)]
    pub struct Context {
        pub version: isize,
        pub info: *mut c_void,
        pub retain: *const c_void,
        pub release: *const c_void,
        pub copy_description: *const c_void,
    }
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        pub fn FSEventStreamCreate(alloc: *const c_void, cb: Callback, ctx: *const Context, paths: *const c_void, since: u64, latency: f64, flags: u32) -> *mut c_void;
        pub fn FSEventStreamSetDispatchQueue(s: *mut c_void, q: *mut c_void);
        pub fn FSEventStreamStart(s: *mut c_void) -> u8;
        pub fn FSEventStreamStop(s: *mut c_void);
        pub fn FSEventStreamInvalidate(s: *mut c_void);
        pub fn FSEventStreamRelease(s: *mut c_void);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFStringCreateWithCString(alloc: *const c_void, s: *const i8, enc: u32) -> *const c_void;
        pub fn CFArrayCreate(alloc: *const c_void, values: *const *const c_void, n: isize, cb: *const c_void) -> *const c_void;
        pub fn CFRelease(p: *const c_void);
        pub static kCFTypeArrayCallBacks: c_void;
    }
    extern "C" {
        pub fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
        pub fn dispatch_release(obj: *mut c_void);
    }
}

type Buffer = Arc<Mutex<Vec<PathBuf>>>;

/// Running FSEvents stream.
pub struct Watcher {
    #[cfg(target_os = "macos")]
    stream: *mut c_void,
    buffer: Buffer,
}

// SAFETY: the stream handle is only used from `Drop` (stop, invalidate, release); the buffer is an `Arc<Mutex>`.
unsafe impl Send for Watcher {}

/// The stream's `info` pointer is a strong `Arc` reference to the buffer (`Arc::into_raw`): FSEvents retains it
/// through [`retain`] and releases it through [`release`], so the buffer lives as long as any callback may run –
/// also after the `Watcher` was dropped while a callback is on the dispatch queue.
#[cfg(target_os = "macos")]
extern "C" fn callback(_s: *const c_void, info: *mut c_void, n: usize, paths: *mut c_void, _flags: *const u32, _ids: *const u64) {
    if info.is_null() || paths.is_null() {
        return;
    }
    // SAFETY: `info` came from `Arc::into_raw` of the buffer and is kept alive by the stream (see above).
    let buffer = unsafe { &*(info as *const Mutex<Vec<PathBuf>>) };
    let paths = paths as *const *const i8;
    // never panic in a C callback (a poisoned lock only means an earlier panic elsewhere)
    let mut v = buffer.lock().unwrap_or_else(|e| e.into_inner());
    for i in 0..n {
        // SAFETY: FSEvents passes `n` valid NUL-terminated C strings.
        let p = unsafe { *paths.add(i) };
        if p.is_null() {
            continue;
        }
        let c = unsafe { std::ffi::CStr::from_ptr(p) };
        v.push(PathBuf::from(c.to_string_lossy().into_owned()));
    }
}

#[cfg(target_os = "macos")]
extern "C" fn retain(info: *const c_void) -> *const c_void {
    if !info.is_null() {
        // SAFETY: `info` is a pointer from `Arc::into_raw` with at least one strong count (the stream's own)
        unsafe { Arc::increment_strong_count(info as *const Mutex<Vec<PathBuf>>) };
    }
    info
}

#[cfg(target_os = "macos")]
extern "C" fn release(info: *const c_void) {
    if !info.is_null() {
        // SAFETY: balances one `into_raw`/`retain`
        unsafe { Arc::decrement_strong_count(info as *const Mutex<Vec<PathBuf>>) };
    }
}

impl Watcher {
    /// Watch folders (local only). `latency` in seconds (batching by macOS).
    #[cfg(target_os = "macos")]
    pub fn start_scan(folder: &[PathBuf], latency: f64) -> Option<Watcher> {
        use ffi::*;
        let local_items: Vec<&PathBuf> = folder.iter().filter(|p| p.is_dir() && local(p)).collect();
        if local_items.is_empty() {
            return None;
        }
        let buffer: Buffer = Arc::new(Mutex::new(Vec::new()));
        // SAFETY: plain CoreFoundation/FSEvents calls with valid arguments; every created object is released here
        // or owned by the stream. `info` is a strong `Arc` reference that the stream retains and releases itself.
        unsafe {
            // a path that is no valid UTF-8 for CoreFoundation yields NULL – it is left out instead of retained
            let strs: Vec<*const c_void> = local_items
                .iter()
                .filter_map(|p| CString::new(p.as_os_str().as_encoded_bytes()).ok())
                .map(|c| CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), 0x0800_0100))
                .filter(|s| !s.is_null())
                .collect();
            if strs.is_empty() {
                return None;
            }
            let arr = CFArrayCreate(std::ptr::null(), strs.as_ptr(), strs.len() as isize, &kCFTypeArrayCallBacks as *const c_void);
            for s in &strs {
                CFRelease(*s);
            }
            if arr.is_null() {
                return None;
            }
            let info = Arc::into_raw(buffer.clone()) as *mut c_void;
            let ctx = Context { version: 0, info, retain: retain as *const c_void, release: release as *const c_void, copy_description: std::ptr::null() };
            // kFSEventStreamCreateFlagNoDefer (0x2) | FileEvents (0x10); since "now"
            let stream = FSEventStreamCreate(std::ptr::null(), callback, &ctx, arr, u64::MAX, latency, 0x2 | 0x10);
            CFRelease(arr);
            // the stream holds its own retained reference now (or none, if it was not created)
            release(info);
            if stream.is_null() {
                return None;
            }
            let q = dispatch_queue_create(c"graineo.fsevents".as_ptr(), std::ptr::null());
            FSEventStreamSetDispatchQueue(stream, q);
            // the stream retains the queue; our reference goes
            dispatch_release(q);
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                return None;
            }
            Some(Watcher { stream, buffer })
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn start_scan(_folder: &[PathBuf], _latency: f64) -> Option<Watcher> {
        None
    }

    /// Changed paths since the last fetch.
    pub fn fetch(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.buffer.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            ffi::FSEventStreamStop(self.stream);
            ffi::FSEventStreamInvalidate(self.stream);
            ffi::FSEventStreamRelease(self.stream);
        }
    }
}

/// Does a change matter to the library? (image, XMP, own sidecar, folder)
pub fn relevant(p: &Path) -> bool {
    let s = p.to_string_lossy();
    if s.contains(".lrdata/") || s.contains(".photoslibrary/") || s.contains("/Lightroom/") || s.ends_with(".tmp") || s.contains(&format!("/{}/.", crate::SIDECAR_DIR)) {
        return false;
    }
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    crate::format_from_extension(&name).is_some() || name.ends_with(".xmp") || p.extension().is_none()
}
