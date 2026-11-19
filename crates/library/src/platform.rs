//! Differences between macOS (Unix) and Windows in one place: free space, local drive,
//! same volume, local time, links.

use std::path::Path;

pub mod explorer_menu;
pub mod info;

/// Free space (bytes available to the user) on the volume of the existing path `p`.
#[cfg(unix)]
pub fn free_bytes(p: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid, null-terminated path and sufficiently large buffer
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_bsize as u64)
}

#[cfg(windows)]
pub fn free_bytes(p: &Path) -> Option<u64> {
    let w = wide(p);
    let mut free: u64 = 0;
    // SAFETY: null-terminated UTF-16 path, output into a valid variable
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

/// Is the path on a local drive (not on the network)?
#[cfg(target_os = "macos")]
pub fn local(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else { return false };
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: as above
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return false;
    }
    s.f_flags & (libc::MNT_LOCAL as u32) != 0
}

#[cfg(windows)]
pub fn local(p: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    let w = wide(p);
    let mut root_dir = [0u16; 512];
    // SAFETY: buffer with the given length
    if unsafe { GetVolumePathNameW(w.as_ptr(), root_dir.as_mut_ptr(), root_dir.len() as u32) } == 0 {
        return false;
    }
    // DRIVE_REMOTE = 4, DRIVE_NO_ROOT_DIR = 1, DRIVE_UNKNOWN = 0
    let kind = unsafe { GetDriveTypeW(root_dir.as_ptr()) };
    !matches!(kind, 0 | 1 | 4)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn local(_p: &Path) -> bool {
    true
}

/// Volume identifier (equal = same volume, rename instead of copy possible).
#[cfg(unix)]
pub fn device(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

#[cfg(windows)]
pub fn device(p: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    std::fs::metadata(p).ok()?;
    let w = wide(p);
    let mut root_dir = [0u16; 512];
    // SAFETY: buffer with the given length
    if unsafe { GetVolumePathNameW(w.as_ptr(), root_dir.as_mut_ptr(), root_dir.len() as u32) } == 0 {
        return None;
    }
    let n = root_dir.iter().position(|&c| c == 0).unwrap_or(root_dir.len());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    String::from_utf16_lossy(&root_dir[..n]).to_lowercase().hash(&mut h);
    Some(h.finish())
}

/// Does the file or folder `p` lie on the startup volume (the internal drive of this computer), not on an
/// external drive or a network share? Works for paths that are not reachable right now as well. Never touches a
/// mounted external volume itself, so a hanging network share cannot block the call.
///
/// - macOS: paths under `/Volumes/<name>` are on another volume, unless `<name>` is the link to `/` that macOS
///   puts there for the startup volume. Other paths: the mount point of the nearest existing folder is `/` or the
///   data volume of the system (`/System/Volumes/Data`).
/// - Windows: the drive letter equals the system drive (`SystemDrive`, usually `C:`); network paths (`\\server\…`)
///   are never on it.
/// - other Unix systems: same device as `/`.
pub fn on_startup_volume(p: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut c = p.components();
        if c.next() == Some(std::path::Component::RootDir) {
            if let Some(std::path::Component::Normal(first)) = c.next() {
                if first == "Volumes" {
                    return match c.next() {
                        Some(std::path::Component::Normal(name)) => volumes_entry_is_startup(name),
                        _ => true,
                    };
                }
            }
        }
        match p.ancestors().find_map(mount_point) {
            Some(m) => startup_mount_point(&m),
            None => true,
        }
    }
    #[cfg(windows)]
    {
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        on_system_drive(p, &system)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let root = device(Path::new("/"));
        p.ancestors().find_map(device).map_or(true, |d| Some(d) == root)
    }
}

/// macOS: is this mount point the startup volume (the system `/` or its data volume)?
pub fn startup_mount_point(m: &str) -> bool {
    m == "/" || m == "/System/Volumes/Data"
}

/// Windows rule (testable everywhere): the path starts with the drive letter `system` (e.g. `C:`). Network paths
/// and other drives are not on it; a path without a drive letter counts as on it.
pub fn on_system_drive(p: &Path, system: &str) -> bool {
    let s = p.to_string_lossy().replace('/', "\\");
    if s.starts_with("\\\\") {
        // \\?\C:\… is a local path in long form, \\server\share and \\?\UNC\… are network paths
        return match s.strip_prefix("\\\\?\\") {
            Some(rest) if !rest.to_ascii_uppercase().starts_with("UNC\\") => on_system_drive(Path::new(rest), system),
            _ => false,
        };
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return system.len() >= 2 && s[..2].eq_ignore_ascii_case(&system[..2]);
    }
    true
}

/// macOS: mount point of the volume holding the existing path `p`.
#[cfg(target_os = "macos")]
fn mount_point(p: &Path) -> Option<String> {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid, null-terminated path and sufficiently large buffer
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    // SAFETY: f_mntonname is a null-terminated C string within the buffer
    let name = unsafe { std::ffi::CStr::from_ptr(s.f_mntonname.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

/// macOS: is `/Volumes/<name>` the link to the startup volume? Only the folder `/Volumes` is listed (the kind of an
/// entry comes from the listing itself); a mounted volume is not looked into.
#[cfg(target_os = "macos")]
fn volumes_entry_is_startup(name: &std::ffi::OsStr) -> bool {
    let Ok(rd) = std::fs::read_dir("/Volumes") else { return false };
    for e in rd.flatten() {
        if e.file_name() != name {
            continue;
        }
        return e.file_type().is_ok_and(|t| t.is_symlink()) && std::fs::read_link(e.path()).is_ok_and(|z| z == Path::new("/"));
    }
    false
}

/// Local time for Unix seconds: (year, month 1–12, day).
pub fn local_date(s: i64) -> (i32, u32, u32) {
    let z = local_time(s);
    (z.year, z.month, z.tag)
}

/// Local time including offset to UTC.
#[derive(Clone, Copy, Debug)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub tag: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// seconds east of UTC (CET: 3600)
    pub offset_val: i64,
}

pub fn local_time(s: i64) -> LocalTime {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    let offset_val = {
        // SAFETY: valid input and output
        unsafe { libc::localtime_r(&s, &mut tm) };
        tm.tm_gmtoff as i64
    };
    #[cfg(windows)]
    let offset_val = {
        let mut utc: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: valid input and output
        unsafe {
            libc::localtime_s(&mut tm, &s);
            libc::gmtime_s(&mut utc, &s);
        }
        let secs = |t: &libc::tm| days(t.tm_year + 1900, t.tm_mon + 1, t.tm_mday) * 86400 + (t.tm_hour * 3600 + t.tm_min * 60 + t.tm_sec) as i64;
        secs(&tm) - secs(&utc)
    };
    LocalTime { year: tm.tm_year + 1900, month: (tm.tm_mon + 1) as u32, tag: tm.tm_mday as u32, hour: tm.tm_hour as u32, minute: tm.tm_min as u32, second: tm.tm_sec as u32, offset_val }
}

/// Days since 1970-01-01 (proleptic Gregorian calendar).
#[cfg(windows)]
fn days(j: i32, m: i32, t: i32) -> i64 {
    let (j, m) = if m <= 2 { (j as i64 - 1, m as i64 + 9) } else { (j as i64, m as i64 - 3) };
    let era = j.div_euclid(400);
    let yoe = j - era * 400;
    let doy = (153 * m + 2) / 5 + t as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Create link `link` → `target`. Windows allows symbolic links only with special privileges:
/// there a hard link (same volume), else a copy.
pub fn pointer(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).or_else(|_| std::fs::hard_link(target, link)).or_else(|_| std::fs::copy(target, link).map(|_| ()))
    }
}


/// Move this thread to the very back (macOS: QOS_CLASS_BACKGROUND, efficiency cores; Windows: lowest priority).
pub fn background_priority() {
    set_priority(Priority::Background);
}

/// Priority classes of the app's threads (macOS: quality of service).
/// - `UserInitiated`: what the user is waiting for (opening, rendering, zooming, export on request).
/// - `Utility`: work for the near future that the user may soon need (preloading the neighbour, visible tiles).
/// - `Background`: work nobody waits for (offline copies, large previews, faces, checks). Only this class keeps a
///   thread on the efficiency cores and throttles its disk and network access; `Utility` still runs on the
///   performance cores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    UserInitiated,
    Utility,
    Background,
}

/// Tests only ([`background_as_utility`]): how many tests ask for `Background` to be set as `Utility` (several tests
/// of one process may lift and lower it independently).
static BACKGROUND_AS_UTILITY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Tests only: from now on [`set_priority`] sets `Utility` where `Background` is asked for (`false`: as usual again);
/// threads take it on their next [`set_priority`], the workers of [`background_pool`] at once. Background threads
/// stay on the efficiency cores, and the system's own background work (malware scan of fresh builds, Spotlight)
/// can take those few cores for minutes: the threads then get no processor time at all, although nothing waits
/// for anything (seen on a machine with two efficiency cores: a background thread got 0.02 s of computing in 11 s,
/// a utility thread at the same moment 9.2 s). A lock-up test that measures processor time would take that for a
/// lock-up, and a test with background work does not finish while the computer is busy with builds.
/// Counted: every `true` needs its `false`; the pool stays lifted while any test still asks for it.
pub fn background_as_utility(on: bool) {
    use std::sync::atomic::Ordering::SeqCst;
    if on {
        BACKGROUND_AS_UTILITY.fetch_add(1, SeqCst);
    } else {
        let _ = BACKGROUND_AS_UTILITY.fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1));
    }
    background_pool().broadcast(|_| background_priority());
}

/// Set the priority of the calling thread (it may be raised or lowered again later).
pub fn set_priority(p: Priority) {
    let p = if p == Priority::Background && BACKGROUND_AS_UTILITY.load(std::sync::atomic::Ordering::Relaxed) > 0 { Priority::Utility } else { p };
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, rel: i32) -> i32;
        }
        let qos = match p {
            Priority::UserInitiated => 0x19,
            Priority::Utility => 0x11,
            Priority::Background => 0x09,
        };
        // SAFETY: affects only the own thread
        unsafe {
            pthread_set_qos_class_self_np(qos, 0);
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_LOWEST, THREAD_PRIORITY_NORMAL};
        let level = match p {
            Priority::UserInitiated => THREAD_PRIORITY_NORMAL,
            Priority::Utility => THREAD_PRIORITY_BELOW_NORMAL,
            Priority::Background => THREAD_PRIORITY_LOWEST,
        };
        // SAFETY: pseudo handle of the own thread
        unsafe {
            SetThreadPriority(GetCurrentThread(), level);
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = p;
}

/// Number of efficiency cores (macOS on Apple silicon; 0 where there are none or it is unknown).
pub fn efficiency_cores() -> usize {
    #[cfg(target_os = "macos")]
    {
        let mut n: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        // SAFETY: name is a C string, output buffer has the given length
        let r = unsafe { libc::sysctlbyname(c"hw.perflevel1.logicalcpu".as_ptr(), &mut n as *mut _ as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        if r == 0 && n > 0 {
            return n as usize;
        }
    }
    0
}

/// Threads of the shared background pool: as many as there are efficiency cores (2–4), otherwise 2. Background work
/// runs there with [`Priority::Background`], so more threads would only wait for the same few cores.
pub fn background_threads() -> usize {
    efficiency_cores().clamp(2, 4)
}

/// Threads for the work the user is waiting for (global compute pool of the app): one per physical core. Measured: two
/// fewer (leaving the efficiency cores to the background pool) made paging through RAWs noticeably slower (the next
/// image was preloaded less often in time). The two pools do not compete: background work pauses while the user works
/// (`jobs::wait_for_foreground` in the app) and runs with the lowest priority, which the scheduler gives way at once.
///
/// Hyper-threads do not count: the computing is SIMD-heavy and gains little from a second thread on the same core,
/// but twice the threads keep every core busy and the fan loud (Intel and most Windows computers). On Apple silicon
/// physical and logical cores are the same. Never more than the system lets this process use.
pub fn foreground_threads() -> usize {
    threads_for(info::physical_cores(), std::thread::available_parallelism().map(|n| n.get()).ok())
}

/// [`foreground_threads`] from the physical cores and the logical ones the process may use (pure, for tests).
pub fn threads_for(physical: Option<usize>, logical: Option<usize>) -> usize {
    let n = match (physical, logical) {
        (Some(p), Some(l)) => p.min(l),
        (Some(p), None) => p,
        (None, Some(l)) => l,
        (None, None) => 4,
    };
    n.max(2)
}

/// Processor time (user + system, seconds) this process has used so far; for measurements and lock-up checks.
pub fn cpu_seconds() -> Option<f64> {
    #[cfg(unix)]
    {
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
        let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
        (r == 0).then(|| t(u.ru_utime) + t(u.ru_stime))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) = Default::default();
        // SAFETY: pseudo handle of the own process, four output values
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) } != 0;
        // 100-ns units
        let v = |f: FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 / 1e7;
        ok.then(|| v(kernel) + v(user))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// Bytes this process has written to disk so far (macOS: as the system counts them for its "disk writes" reports);
/// for measurements. `None` elsewhere.
pub fn disk_bytes_written() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::proc_pid_rusage(std::process::id() as libc::c_int, libc::RUSAGE_INFO_V2, &mut info as *mut _ as *mut libc::rusage_info_t) };
        (r == 0).then_some(info.ri_diskio_byteswritten)
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// The one shared pool for work nobody waits for (offline copies, large previews after an edit, face search,
/// previews during a move): [`background_threads`] threads with [`Priority::Background`] – on macOS they stay on
/// the efficiency cores, so they neither slow down the work in front nor spin up the fans.
///
/// Hand work to it with [`background_run`], not with `install` directly.
pub fn background_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(background_threads())
            .thread_name(|i| format!("hintergrund-{i}"))
            .start_handler(|_| {
                pools::mark(pools::Pool::Background);
                background_priority();
            })
            .build()
            .expect("background thread pool")
    })
}

/// Run `f` in the background pool – one piece of work at a time ([`pools::Turn`]).
///
/// Several threads handing work to the small pool at once let its workers, while waiting for the parallel parts of
/// one photo, pick up the next photo on top of it: the photos then ran nested on the two efficiency cores, each one
/// holding its memory, and the first one finished only after all stacked ones (seen with offline copies of Quattro
/// files: 8 copies in 305 s at 58 s of processor time, once not within 10 minutes). Callers from outside the pool
/// (plain threads, workers of other pools) wait here for their turn; a worker of the pool runs `f` at once.
pub fn background_run<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    static TURN: pools::Turn = pools::Turn::new(pools::Pool::Background);
    static LISTED: std::sync::Once = std::sync::Once::new();
    LISTED.call_once(|| pools::register(TURN.gate()));
    TURN.run(background_pool(), f)
}

#[cfg(windows)]
fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests lift the background pool to utility and back; its workers follow at once.
    #[cfg(target_os = "macos")]
    #[test]
    fn background_pool_can_run_as_utility_for_tests() {
        extern "C" {
            fn qos_class_self() -> u32;
        }
        // SAFETY: reads the calling thread's class
        let qos = || background_pool().broadcast(|_| unsafe { qos_class_self() });
        assert!(qos().iter().all(|&q| q == 0x09), "background");
        background_as_utility(true);
        let lifted = qos();
        background_as_utility(false);
        assert!(lifted.iter().all(|&q| q == 0x11), "utility while lifted: {lifted:x?}");
        assert!(qos().iter().all(|&q| q == 0x09), "background again");
    }

    #[test]
    fn threads_follow_physical_cores() {
        // Intel with hyper-threading: 4 cores, 8 logical → 4
        assert_eq!(threads_for(Some(4), Some(8)), 4);
        // Apple silicon: physical = logical
        assert_eq!(threads_for(Some(10), Some(10)), 10);
        // the process may use fewer (container, affinity) → those
        assert_eq!(threads_for(Some(8), Some(3)), 3);
        // unknown physical cores: the logical ones as before
        assert_eq!(threads_for(None, Some(6)), 6);
        assert_eq!(threads_for(None, None), 4);
        assert_eq!(threads_for(Some(1), Some(1)), 2);
        assert!(foreground_threads() >= 2);
    }

    #[test]
    fn startup_volume() {
        // the own temporary folder and the home folder lie on the startup volume
        assert!(on_startup_volume(&std::env::temp_dir()));
        assert!(on_startup_volume(Path::new("/")));
        // a file that does not exist (yet) on the startup volume still counts as on it
        assert!(on_startup_volume(&std::env::temp_dir().join("graineo-not-there/photo.cr3")));
        #[cfg(target_os = "macos")]
        {
            // external drives and network shares, whether connected or not
            assert!(!on_startup_volume(Path::new("/Volumes/graineo-test-not-mounted/Photos/a.cr3")));
            // the link macOS puts into /Volumes for the startup volume
            if let Some(link) = std::fs::read_dir("/Volumes").ok().and_then(|rd| rd.flatten().find(|e| std::fs::read_link(e.path()).is_ok_and(|z| z == Path::new("/")))) {
                assert!(on_startup_volume(&link.path().join("Users")));
            }
            assert!(startup_mount_point("/") && startup_mount_point("/System/Volumes/Data"));
            assert!(!startup_mount_point("/Volumes/media") && !startup_mount_point("/Volumes/Externe SSD"));
        }
    }

    #[test]
    fn system_drive_rule() {
        assert!(on_system_drive(Path::new(r"C:\Users\a\Pictures\x.cr3"), "C:"));
        assert!(on_system_drive(Path::new(r"c:\Fotos"), "C:"));
        assert!(!on_system_drive(Path::new(r"D:\Fotos\x.cr3"), "C:"));
        assert!(!on_system_drive(Path::new(r"\\nas\media\x.cr3"), "C:"));
        assert!(!on_system_drive(Path::new(r"\\?\UNC\nas\media\x.cr3"), "C:"));
        assert!(on_system_drive(Path::new(r"\\?\C:\Fotos\x.cr3"), "C:"));
        assert!(!on_system_drive(Path::new(r"\\?\E:\Fotos\x.cr3"), "C:"));
        assert!(on_system_drive(Path::new(r"D:\Fotos"), "D:"));
    }
}

/// Keeps the computer awake while it lives (no idle sleep of the system or the disks), e.g. during a move over many
/// hours. macOS: `caffeinate` bound to this process (it also ends if the app crashes). Elsewhere: nothing (yet).
pub struct KeepAwake(Option<std::process::Child>);

impl KeepAwake {
    pub fn new() -> KeepAwake {
        #[cfg(target_os = "macos")]
        {
            use std::process::{Command, Stdio};
            let pid = std::process::id().to_string();
            KeepAwake(Command::new("/usr/bin/caffeinate").args(["-i", "-m", "-w", &pid]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok())
        }
        #[cfg(not(target_os = "macos"))]
        {
            KeepAwake(None)
        }
    }

    /// Is the computer kept awake?
    pub fn active(&self) -> bool {
        self.0.is_some()
    }
}

impl Default for KeepAwake {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Memory pressure of the system (macOS: `kern.memorystatus_vm_pressure_level`; other systems: always `Normal`).
/// Test hook `GRAINEO_FAKE_PRESSURE=warn|critical` (only with `GRAINEO_DATA_DIR`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

pub fn memory_pressure() -> Pressure {
    if std::env::var_os("GRAINEO_DATA_DIR").is_some() {
        match std::env::var("GRAINEO_FAKE_PRESSURE").as_deref() {
            Ok("warn") => return Pressure::Warn,
            Ok("critical") => return Pressure::Critical,
            _ => {}
        }
    }
    memory_pressure_system()
}

#[cfg(target_os = "macos")]
fn memory_pressure_system() -> Pressure {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: valid name, output into an int of the stated size
    let rc = unsafe {
        libc::sysctlbyname(c"kern.memorystatus_vm_pressure_level".as_ptr(), (&mut level as *mut libc::c_int).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    if rc != 0 {
        return Pressure::Normal;
    }
    match level {
        2 => Pressure::Warn,
        4 => Pressure::Critical,
        _ => Pressure::Normal,
    }
}

#[cfg(not(target_os = "macos"))]
fn memory_pressure_system() -> Pressure {
    Pressure::Normal
}
