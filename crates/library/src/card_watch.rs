//! Memory cards that are inserted or taken out while the import window is open.
//!
//! A watcher thread lists the mount points (macOS `/Volumes`, Windows the drive letters, elsewhere `/media` and
//! `/run/media`) – names only, no file access. On the Mac a change of `/Volumes` wakes it at once (kqueue); on every
//! system it looks again every 1.5 s, so a missed event or a slow mount is caught too. Whether a new drive is a card
//! (`DCIM` folder) is asked in a thread of its own: a drive that is still being mounted, or a slow card under load, can
//! take seconds to answer and must never hold up the window. Until the answer is there the drive is listed as "being
//! read"; a drive without `DCIM` is asked again for a while (a card can show its folders a moment after it appears).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the mount points are listed without an event.
const POLL: Duration = Duration::from_millis(1500);
/// A drive without `DCIM` is asked again for this long after it appeared …
const RECHECK_FOR: Duration = Duration::from_secs(30);
/// … at most this often.
const RECHECK_EVERY: Duration = Duration::from_secs(2);
/// A drive whose answer takes longer than this is shown as "being read".
const SLOW: Duration = Duration::from_millis(400);

/// Mount points of drives other than the startup disk (no file access beyond listing the folder of mount points).
pub fn mount_roots() -> Vec<PathBuf> {
    only_test_volumes(system_mount_roots())
}

/// Test aid (only with `GRAINEO_DATA_DIR`): `GRAINEO_TEST_VOLUMES="NAME,…"` – only these drives count, so a real card
/// in the reader stays out of a test.
fn only_test_volumes(v: Vec<PathBuf>) -> Vec<PathBuf> {
    if std::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return v;
    }
    let Ok(only) = std::env::var("GRAINEO_TEST_VOLUMES") else { return v };
    let names: Vec<&str> = only.split(',').map(str::trim).collect();
    v.into_iter().filter(|p| p.file_name().is_some_and(|n| names.contains(&n.to_string_lossy().as_ref()))).collect()
}

fn system_mount_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        // the startup volume appears as a link to "/" and is left out
        let mut v: Vec<PathBuf> = std::fs::read_dir("/Volumes")
            .map(|rd| rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
        // DRIVE_REMOVABLE = 2, DRIVE_FIXED = 3 (many card readers report "fixed"); network and optical drives are left out
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_ascii_uppercase();
        let mask = unsafe { GetLogicalDrives() };
        let mut v = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            if format!("{letter}:") == system {
                continue;
            }
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(Some(0)).collect();
            let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
            if kind == 2 || kind == 3 {
                v.push(PathBuf::from(format!("{letter}:\\")));
            }
        }
        v
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let user = std::env::var("USER").unwrap_or_default();
        let mut v = Vec::new();
        for base in [PathBuf::from("/media").join(&user), PathBuf::from("/run/media").join(&user), PathBuf::from("/media")] {
            if let Ok(rd) = std::fs::read_dir(&base) {
                v.extend(rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).filter(|p| *p != Path::new("/media").join(&user)));
            }
        }
        v.sort();
        v.dedup();
        v
    }
}

/// Is this drive a memory card (a `DCIM` folder at its top)? Touches the drive: not on the UI thread for a drive that
/// has just appeared.
pub fn is_card_root(root: &Path) -> bool {
    root.join("DCIM").is_dir()
}

/// Mounted memory cards (a single look, blocking).
pub fn cards() -> Vec<PathBuf> {
    mount_roots().into_iter().filter(|r| is_card_root(r)).collect()
}

/// Is `p` a card or a folder on one (below a mount point, with `DCIM` above or at it)?
pub fn is_on_card(p: &Path) -> bool {
    mount_roots().iter().any(|r| p.starts_with(r)) && p.ancestors().any(|a| is_card_root(a))
}

/// The mount point `p` lies on (the card's drive).
pub fn root_of(p: &Path) -> Option<PathBuf> {
    mount_roots().into_iter().find(|r| p.starts_with(r))
}

/// What the watcher sees.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volumes {
    /// cards that can be read, sorted
    pub cards: Vec<PathBuf>,
    /// drives that appeared and have not answered yet (name only)
    pub reading: Vec<String>,
}

struct Probe {
    appeared: Instant,
    asked: Instant,
    /// `None` while the drive has not answered
    answer: Arc<Mutex<Option<bool>>>,
}

fn ask(root: &Path) -> Arc<Mutex<Option<bool>>> {
    let answer: Arc<Mutex<Option<bool>>> = Arc::default();
    let (a, r) = (answer.clone(), root.to_path_buf());
    let started = std::thread::Builder::new().name("card-probe".into()).spawn(move || {
        crate::preview::low_priority_thread();
        let card = is_card_root(&r);
        *a.lock().unwrap() = Some(card);
    });
    if started.is_err() {
        *answer.lock().unwrap() = Some(false);
    }
    answer
}

/// Watches the mount points while it exists (the window holds it; dropping it ends the thread within 1.5 s).
pub struct CardWatch {
    state: Arc<Mutex<Volumes>>,
    stop: Arc<AtomicBool>,
}

impl CardWatch {
    /// Start watching; `changed` is called (from the watcher thread) whenever cards or drives being read change. The
    /// first state follows within a few hundredths of a second (never blocking the caller).
    pub fn start(changed: impl Fn() + Send + 'static) -> CardWatch {
        let state: Arc<Mutex<Volumes>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (s, st) = (state.clone(), stop.clone());
        std::thread::Builder::new()
            .name("card-watch".into())
            .spawn(move || watch(&s, &st, &changed))
            .ok();
        CardWatch { state, stop }
    }

    pub fn volumes(&self) -> Volumes {
        self.state.lock().unwrap().clone()
    }
}

impl Drop for CardWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(state: &Mutex<Volumes>, stop: &AtomicBool, changed: &dyn Fn()) {
    let waker = Waker::new();
    let mut known: HashMap<PathBuf, Probe> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let roots = mount_roots();
        known.retain(|r, _| roots.contains(r));
        for r in &roots {
            known.entry(r.clone()).or_insert_with(|| Probe { appeared: now, asked: now, answer: ask(r) });
        }
        // no DCIM yet: ask again for a while (cards under load show their folders late)
        for (r, p) in known.iter_mut() {
            let no = *p.answer.lock().unwrap() == Some(false);
            if no && now.duration_since(p.appeared) < RECHECK_FOR && now.duration_since(p.asked) >= RECHECK_EVERY {
                p.asked = now;
                p.answer = ask(r);
            }
        }
        // a moment for the quick answers, so that a card that is ready does not show as "being read" first
        std::thread::sleep(Duration::from_millis(60));
        let mut v = Volumes::default();
        let mut waiting = false;
        for (r, p) in &known {
            match *p.answer.lock().unwrap() {
                Some(true) => v.cards.push(r.clone()),
                Some(false) => {}
                None => {
                    waiting = true;
                    // a re-ask of a drive that once said "no" stays quiet
                    if p.asked == p.appeared && now.duration_since(p.appeared) >= SLOW {
                        v.reading.push(r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| r.display().to_string()));
                    }
                }
            }
        }
        v.cards.sort();
        v.reading.sort();
        let differs = {
            let mut g = state.lock().unwrap();
            let d = *g != v;
            if d {
                *g = v;
            }
            d
        };
        if differs {
            changed();
        }
        let recheck = known.values().any(|p| *p.answer.lock().unwrap() == Some(false) && now.duration_since(p.appeared) < RECHECK_FOR);
        let wait = if waiting { Duration::from_millis(250) } else if recheck { RECHECK_EVERY.min(POLL) } else { POLL };
        match &waker {
            Some(w) => w.wait(wait),
            None => std::thread::sleep(wait),
        }
    }
}

/// Wakes the watcher when `/Volumes` changes (a drive mounted or unmounted).
#[cfg(target_os = "macos")]
struct Waker {
    kq: i32,
    fd: i32,
}

#[cfg(target_os = "macos")]
impl Waker {
    fn new() -> Option<Waker> {
        unsafe {
            let fd = libc::open(c"/Volumes".as_ptr(), libc::O_EVTONLY);
            if fd < 0 {
                return None;
            }
            let kq = libc::kqueue();
            if kq < 0 {
                libc::close(fd);
                return None;
            }
            let ev = libc::kevent {
                ident: fd as usize,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_WRITE | libc::NOTE_LINK | libc::NOTE_DELETE | libc::NOTE_RENAME,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
                libc::close(kq);
                libc::close(fd);
                return None;
            }
            Some(Waker { kq, fd })
        }
    }

    /// Wait for a change of `/Volumes` or until `d` has passed.
    fn wait(&self, d: Duration) {
        let ts = libc::timespec { tv_sec: d.as_secs() as libc::time_t, tv_nsec: d.subsec_nanos() as libc::c_long };
        unsafe {
            let mut out: libc::kevent = std::mem::zeroed();
            libc::kevent(self.kq, std::ptr::null(), 0, &mut out, 1, &ts);
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Waker {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.kq);
            libc::close(self.fd);
        }
    }
}

/// Elsewhere only the regular look (Windows announces drives to windows, not to threads).
#[cfg(not(target_os = "macos"))]
struct Waker;

#[cfg(not(target_os = "macos"))]
impl Waker {
    fn new() -> Option<Waker> {
        None
    }
    fn wait(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The startup disk and hidden entries are no mount points of their own; a folder without DCIM is not a card.
    #[test]
    fn roots_and_cards() {
        let roots = mount_roots();
        assert!(!roots.iter().any(|r| r.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))));
        let d = std::env::temp_dir().join(format!("graineo-card-watch-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_card_root(&d));
        std::fs::create_dir_all(d.join("DCIM/100")).unwrap();
        assert!(is_card_root(&d));
        // a folder outside the mount points is never "on a card", even with DCIM
        assert!(!is_on_card(&d.join("DCIM/100")));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
