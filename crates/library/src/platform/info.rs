//! Facts about the computer for the performance report (Help → Create Performance Report …) and the timing log:
//! operating system, processor, memory, power, thermal state, the kind of drive a folder lies on, and counters of the
//! own process (memory, threads, processor time) and of the whole system (processor busy).
//!
//! Only facts about the hardware and its state: never user, computer or volume names, serial numbers or paths.
//! macOS and Windows; other systems get placeholders.

use std::path::Path;

/// The computer and its state at one moment.
#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    /// "macOS 15.5 (24F74)", "Windows 11 Pro 23H2 (build 22631.4460)"
    pub os: String,
    /// model class ("MacBookPro11,5", "Surface Laptop 4"); never a serial number
    pub model: Option<String>,
    /// processor name ("Apple M1 Pro", "Intel(R) Core(TM) i7-4870HQ CPU @ 2.50GHz")
    pub cpu: String,
    pub physical_cores: Option<usize>,
    pub logical_cores: usize,
    /// (performance, efficiency) cores where the processor has two kinds
    pub core_kinds: Option<(usize, usize)>,
    pub memory_total: Option<u64>,
    /// memory the system can hand out without swapping (free plus reclaimable)
    pub memory_available: Option<u64>,
    pub power: Power,
    /// macOS Low Power Mode, Windows battery saver
    pub low_power_mode: Option<bool>,
    /// macOS: "nominal", "fair", "serious", "critical"; `None` where the system does not tell
    pub thermal: Option<String>,
    /// 1, 5 and 15 minute load average (Unix)
    pub load_average: Option<[f64; 3]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PowerSource {
    #[default]
    Unknown,
    Ac,
    Battery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Power {
    pub source: PowerSource,
    /// charge in percent, `None` without battery or when unknown
    pub battery_percent: Option<u8>,
}

/// Kind of drive a file or folder lies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Network,
    Ssd,
    Hdd,
    Unknown,
}

impl DriveKind {
    pub fn name(self) -> &'static str {
        match self {
            DriveKind::Network => "network drive",
            DriveKind::Ssd => "SSD",
            DriveKind::Hdd => "hard disk",
            DriveKind::Unknown => "unknown drive",
        }
    }
}

/// Drive of `p` (an existing file or folder): network share, SSD or hard disk, and whether it is built in
/// (`Some(true)`), external (`Some(false)`) or unknown. Never reports the name of the drive.
pub fn drive(p: &Path) -> (DriveKind, Option<bool>) {
    if !super::local(p) {
        return (DriveKind::Network, Some(false));
    }
    drive_local(p)
}

#[cfg(target_os = "macos")]
fn drive_local(p: &Path) -> (DriveKind, Option<bool>) {
    let Some(mount) = p.ancestors().find_map(super::mount_point) else { return (DriveKind::Unknown, None) };
    let Ok(o) = std::process::Command::new("/usr/sbin/diskutil").args(["info", &mount]).output() else { return (DriveKind::Unknown, None) };
    let text = String::from_utf8_lossy(&o.stdout);
    let field = |name: &str| text.lines().find_map(|l| l.trim().strip_prefix(name).map(|v| v.trim_start_matches(':').trim().to_string()));
    let kind = match field("Solid State").as_deref() {
        Some("Yes") => DriveKind::Ssd,
        Some("No") => DriveKind::Hdd,
        _ => DriveKind::Unknown,
    };
    let internal = field("Device Location").map(|v| v == "Internal");
    (kind, internal)
}

#[cfg(windows)]
fn drive_local(p: &Path) -> (DriveKind, Option<bool>) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, GetDriveTypeW, GetVolumePathNameW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Ioctl::{PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let w = super::wide(p);
    let mut root = [0u16; 512];
    // SAFETY: buffer with the given length
    if unsafe { GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return (DriveKind::Unknown, None);
    }
    // DRIVE_REMOVABLE = 2, DRIVE_FIXED = 3
    let removable = unsafe { GetDriveTypeW(root.as_ptr()) } == 2;
    let n = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    let letter = String::from_utf16_lossy(&root[..n]);
    let letter = letter.trim_end_matches('\\');
    if letter.len() != 2 || !letter.ends_with(':') {
        return (DriveKind::Unknown, removable.then_some(false));
    }
    let device: Vec<u16> = format!("\\\\.\\{letter}").encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: null-terminated device name; access 0 only queries properties (no administrator rights needed)
    let h = unsafe { CreateFileW(device.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return (DriveKind::Unknown, removable.then_some(false));
    }
    let mut q: STORAGE_PROPERTY_QUERY = unsafe { std::mem::zeroed() };
    q.PropertyId = StorageDeviceSeekPenaltyProperty;
    q.QueryType = PropertyStandardQuery;
    let mut d: DEVICE_SEEK_PENALTY_DESCRIPTOR = Default::default();
    let mut got = 0u32;
    // SAFETY: input and output buffers with their sizes, handle opened above
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&q as *const STORAGE_PROPERTY_QUERY).cast(),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            (&mut d as *mut DEVICE_SEEK_PENALTY_DESCRIPTOR).cast(),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &mut got,
            std::ptr::null_mut(),
        )
    } != 0;
    unsafe { CloseHandle(h) };
    let kind = if !ok || (got as usize) < std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() {
        DriveKind::Unknown
    } else if d.IncursSeekPenalty {
        DriveKind::Hdd
    } else {
        DriveKind::Ssd
    };
    (kind, removable.then_some(false))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn drive_local(_p: &Path) -> (DriveKind, Option<bool>) {
    (DriveKind::Unknown, None)
}

/// Collect the facts (runs system tools on macOS: call it off the UI thread).
pub fn system_info() -> SystemInfo {
    let logical_cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut s = SystemInfo { logical_cores, ..Default::default() };
    fill(&mut s);
    s
}

#[cfg(target_os = "macos")]
fn sysctl_text(name: &std::ffi::CStr) -> Option<String> {
    let mut len = 0usize;
    // SAFETY: first call asks for the length, second fills a buffer of that length
    if unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) } != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    if unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[..len]).trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut len = buf.len();
    // SAFETY: output buffer of `len` bytes; the value is a 32- or 64-bit integer
    if unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    match len {
        4 => Some(u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64),
        8 => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn fill(s: &mut SystemInfo) {
    let version = sysctl_text(c"kern.osproductversion").unwrap_or_default();
    let build = sysctl_text(c"kern.osversion").map(|b| format!(" ({b})")).unwrap_or_default();
    s.os = format!("macOS {version}{build}");
    s.model = sysctl_text(c"hw.model");
    s.cpu = sysctl_text(c"machdep.cpu.brand_string").unwrap_or_else(|| std::env::consts::ARCH.to_string());
    s.physical_cores = sysctl_u64(c"hw.physicalcpu").map(|n| n as usize);
    if let Some(n) = sysctl_u64(c"hw.logicalcpu") {
        s.logical_cores = n as usize;
    }
    if sysctl_u64(c"hw.nperflevels").is_some_and(|n| n >= 2) {
        if let (Some(p), Some(e)) = (sysctl_u64(c"hw.perflevel0.physicalcpu"), sysctl_u64(c"hw.perflevel1.physicalcpu")) {
            s.core_kinds = Some((p as usize, e as usize));
        }
    }
    s.memory_total = sysctl_u64(c"hw.memsize");
    s.memory_available = available_memory();
    s.power = power_macos();
    let info = objc2_foundation::NSProcessInfo::processInfo();
    s.low_power_mode = Some(info.isLowPowerModeEnabled());
    s.thermal = Some(
        match info.thermalState().0 {
            0 => "nominal",
            1 => "fair",
            2 => "serious",
            3 => "critical",
            _ => "unknown",
        }
        .to_string(),
    );
    let mut l = [0f64; 3];
    // SAFETY: array of three values
    if unsafe { libc::getloadavg(l.as_mut_ptr(), 3) } == 3 {
        s.load_average = Some(l);
    }
}

/// Free plus inactive and purgeable pages (what the system hands out without swapping).
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn available_memory() -> Option<u64> {
    let mut v: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: statistics structure with its size in natural_t units, as the call requires
    let r = unsafe { libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, (&mut v as *mut libc::vm_statistics64).cast(), &mut count) };
    if r != 0 {
        return None;
    }
    // SAFETY: plain query
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (page > 0).then(|| (v.free_count as u64 + v.inactive_count as u64 + v.purgeable_count as u64) * page as u64)
}

/// `pmset -g batt`: "Now drawing from 'AC Power'" and, with a battery, "… 87%; charging; …".
#[cfg(target_os = "macos")]
fn power_macos() -> Power {
    let Ok(o) = std::process::Command::new("/usr/bin/pmset").args(["-g", "batt"]).output() else { return Power::default() };
    parse_pmset(&String::from_utf8_lossy(&o.stdout))
}

/// Parse the output of `pmset -g batt` (testable everywhere).
pub fn parse_pmset(text: &str) -> Power {
    let source = if text.contains("'AC Power'") {
        PowerSource::Ac
    } else if text.contains("'Battery Power'") {
        PowerSource::Battery
    } else {
        PowerSource::Unknown
    };
    let battery_percent = text.lines().filter(|l| l.contains("InternalBattery")).find_map(|l| {
        let i = l.find('%')?;
        let digits: String = l[..i].chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
        digits.parse::<u8>().ok()
    });
    Power { source, battery_percent }
}

#[cfg(windows)]
fn registry_text(key: &str, value: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let k: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
    let v: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: null-terminated key and value names, output buffer with its size in bytes
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    if r != 0 {
        return None;
    }
    let n = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(windows)]
fn registry_u32(key: &str, value: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    let k: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
    let v: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut out = 0u32;
    let mut len = 4u32;
    // SAFETY: as above, a DWORD output
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, k.as_ptr(), v.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut out as *mut u32).cast(), &mut len) };
    (r == 0).then_some(out)
}

#[cfg(windows)]
fn fill(s: &mut SystemInfo) {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = registry_text(NT, "CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
    s.os = windows_name(registry_text(NT, "ProductName").as_deref(), registry_text(NT, "DisplayVersion").as_deref(), build, registry_u32(NT, "UBR"));
    s.model = registry_text(r"HARDWARE\DESCRIPTION\System\BIOS", "SystemProductName").filter(|m| !m.eq_ignore_ascii_case("System Product Name") && !m.eq_ignore_ascii_case("To be filled by O.E.M."));
    s.cpu = registry_text(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString").or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok()).unwrap_or_default();
    let (cores, kinds) = windows_cores();
    s.physical_cores = cores;
    s.core_kinds = kinds;
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: zeroed structure carrying its own length
        if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 {
            s.memory_total = Some(m.ullTotalPhys);
            s.memory_available = Some(m.ullAvailPhys);
        }
    }
    {
        use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut p: SYSTEM_POWER_STATUS = Default::default();
        // SAFETY: output structure
        if unsafe { GetSystemPowerStatus(&mut p) } != 0 {
            s.power = windows_power(p.ACLineStatus, p.BatteryFlag, p.BatteryLifePercent);
            // SystemStatusFlag 1: battery saver is on (Windows 10 and later)
            s.low_power_mode = Some(p.SystemStatusFlag == 1);
        }
    }
}

/// "Windows 11 Pro 23H2 (build 22631.4460)": Windows 11 still names itself "Windows 10" in `ProductName`; the build
/// number tells (22000 and later = Windows 11).
pub fn windows_name(product: Option<&str>, display: Option<&str>, build: u32, ubr: Option<u32>) -> String {
    let mut name = product.unwrap_or("Windows").to_string();
    if build >= 22000 {
        name = name.replace("Windows 10", "Windows 11");
    }
    let display = display.map(|d| format!(" {d}")).unwrap_or_default();
    let ubr = ubr.map(|u| format!(".{u}")).unwrap_or_default();
    if build > 0 {
        format!("{name}{display} (build {build}{ubr})")
    } else {
        format!("{name}{display}")
    }
}

/// Power state from `GetSystemPowerStatus` (testable everywhere): AC line 0 = battery, 1 = mains, 255 = unknown;
/// battery flag 128 = no battery; percent 255 = unknown.
pub fn windows_power(ac_line: u8, battery_flag: u8, percent: u8) -> Power {
    let source = match ac_line {
        0 => PowerSource::Battery,
        1 => PowerSource::Ac,
        _ => PowerSource::Unknown,
    };
    let battery_percent = (battery_flag != 128 && battery_flag != 255 && percent <= 100).then_some(percent);
    Power { source, battery_percent }
}

/// Physical cores and, on hybrid processors, (performance, efficiency) cores.
#[cfg(windows)]
fn windows_cores() -> (Option<usize>, Option<(usize, usize)>) {
    use windows_sys::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX};
    let mut len = 0u32;
    // SAFETY: first call only asks for the length
    unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut len) };
    if len == 0 {
        return (None, None);
    }
    // one whole record more than asked for: a field read of the last (shorter) record never leaves the buffer
    let record = std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
    let mut buf = vec![0u64; (len as usize + record).div_ceil(8)];
    // SAFETY: buffer of at least `len` bytes, 8-byte aligned
    if unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, buf.as_mut_ptr().cast(), &mut len) } == 0 {
        return (None, None);
    }
    let len = len as usize;
    let bytes = buf.as_ptr() as *const u8;
    let mut classes = Vec::new();
    let mut off = 0usize;
    while off + std::mem::size_of::<u32>() * 2 <= len {
        let rec = bytes.wrapping_add(off) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
        // SAFETY: `off < len` and the buffer reaches `record` bytes beyond `len`, so every field of a record starting
        // at `off` lies inside it; fields are read by value through raw pointers (no reference to a record that may be
        // shorter than the type), unaligned reads to be safe
        let (relationship, size) = unsafe { (std::ptr::addr_of!((*rec).Relationship).read_unaligned(), std::ptr::addr_of!((*rec).Size).read_unaligned() as usize) };
        if size == 0 || off + size > len {
            break;
        }
        if relationship == RelationProcessorCore {
            // SAFETY: as above; the union holds a processor record for this relationship
            classes.push(unsafe { std::ptr::addr_of!((*rec).Anonymous.Processor.EfficiencyClass).read_unaligned() });
        }
        off += size;
    }
    if classes.is_empty() {
        return (None, None);
    }
    let top = *classes.iter().max().unwrap_or(&0);
    let kinds = (classes.iter().any(|&c| c != top)).then(|| {
        let p = classes.iter().filter(|&&c| c == top).count();
        (p, classes.len() - p)
    });
    (Some(classes.len()), kinds)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn fill(s: &mut SystemInfo) {
    // placeholder: only what the standard library knows
    s.os = std::env::consts::OS.to_string();
    s.cpu = std::env::consts::ARCH.to_string();
}

/// Memory the own process occupies now (macOS: physical footprint as in Activity Monitor; Windows: working set).
pub fn process_memory() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        // SAFETY: own process, structure of the requested version
        let r = unsafe { libc::proc_pid_rusage(std::process::id() as libc::c_int, libc::RUSAGE_INFO_V2, &mut info as *mut _ as *mut libc::rusage_info_t) };
        (r == 0).then_some(info.ri_phys_footprint)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut c: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: pseudo handle of the own process, structure with its size
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0;
        ok.then_some(c.WorkingSetSize as u64)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    None
}

/// Threads of the own process now.
pub fn process_threads() -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        let mut t: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: own process, buffer of the given size
        let r = unsafe { libc::proc_pidinfo(std::process::id() as libc::c_int, libc::PROC_PIDTASKINFO, 0, (&mut t as *mut libc::proc_taskinfo).cast(), size) };
        (r == size).then_some(t.pti_threadnum as usize)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32};
        // SAFETY: snapshot of all threads, walked with a correctly sized entry, closed afterwards
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let pid = std::process::id();
            let mut e: THREADENTRY32 = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut n = 0usize;
            let mut more = Thread32First(snap, &mut e) != 0;
            while more {
                if e.th32OwnerProcessID == pid {
                    n += 1;
                }
                more = Thread32Next(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            Some(n)
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    None
}

/// Processor time of the whole system so far: (busy, total) in system ticks, summed over all cores. The share busy
/// between two readings is `Δbusy / Δtotal`.
pub fn system_cpu_ticks() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    #[allow(deprecated)]
    {
        let mut info: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        // SAFETY: structure with its size in natural_t units
        let r = unsafe { libc::host_statistics(libc::mach_host_self(), libc::HOST_CPU_LOAD_INFO, (&mut info as *mut libc::host_cpu_load_info).cast(), &mut count) };
        if r != 0 {
            return None;
        }
        let t = info.cpu_ticks.map(|v| v as u64);
        let idle = t[libc::CPU_STATE_IDLE as usize];
        let total: u64 = t.iter().sum();
        Some((total - idle, total))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::GetSystemTimes;
        let (mut idle, mut kernel, mut user): (FILETIME, FILETIME, FILETIME) = Default::default();
        // SAFETY: three output values
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let v = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        // kernel time includes the idle time
        let total = v(kernel) + v(user);
        Some((total.saturating_sub(v(idle)), total))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    None
}

/// Share (0–1) of the whole processor that was busy between two [`system_cpu_ticks`] readings.
pub fn busy_share(before: (u64, u64), after: (u64, u64)) -> Option<f64> {
    let total = after.1.checked_sub(before.1)?;
    let busy = after.0.checked_sub(before.0)?;
    (total > 0).then(|| (busy as f64 / total as f64).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmset_output() {
        let ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=23396451)\t87%; charging; 0:40 remaining present: true\n";
        assert_eq!(parse_pmset(ac), Power { source: PowerSource::Ac, battery_percent: Some(87) });
        let bat = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t100%; discharging; 5:12 remaining present: true\n";
        assert_eq!(parse_pmset(bat), Power { source: PowerSource::Battery, battery_percent: Some(100) });
        // desktop Mac without battery
        assert_eq!(parse_pmset("Now drawing from 'AC Power'\n"), Power { source: PowerSource::Ac, battery_percent: None });
        assert_eq!(parse_pmset(""), Power::default());
    }

    #[test]
    fn windows_names_and_power() {
        assert_eq!(windows_name(Some("Windows 10 Pro"), Some("23H2"), 22631, Some(4460)), "Windows 11 Pro 23H2 (build 22631.4460)");
        assert_eq!(windows_name(Some("Windows 10 Home"), Some("22H2"), 19045, None), "Windows 10 Home 22H2 (build 19045)");
        assert_eq!(windows_name(None, None, 0, None), "Windows");
        assert_eq!(windows_power(1, 128, 255), Power { source: PowerSource::Ac, battery_percent: None });
        assert_eq!(windows_power(0, 1, 64), Power { source: PowerSource::Battery, battery_percent: Some(64) });
        assert_eq!(windows_power(255, 255, 255), Power::default());
    }

    #[test]
    fn busy_share_between_readings() {
        assert_eq!(busy_share((10, 100), (60, 200)), Some(0.5));
        assert_eq!(busy_share((10, 100), (10, 100)), None);
        assert_eq!(busy_share((10, 100), (5, 50)), None);
    }

    #[test]
    fn this_computer_is_described() {
        let s = system_info();
        assert!(!s.os.is_empty() && s.logical_cores >= 1);
        if cfg!(any(target_os = "macos", windows)) {
            assert!(s.memory_total.is_some_and(|m| m > 0));
            assert!(process_memory().is_some_and(|m| m > 0));
            assert!(process_threads().is_some_and(|n| n >= 1));
            assert!(system_cpu_ticks().is_some());
        }
    }
}
