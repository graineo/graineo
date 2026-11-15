//! Videos in the library: duration (for the "▶ 0:42" badge in the grid) and a preview image.
//!
//! Durations are stored in `video-durations.json` in the data folder (path → seconds): videos from Apple Photos
//! bring them along from the library (even if the video is only in iCloud); for own videos
//! generating the preview image measures them. On the Mac, the preview image of own videos comes from Quick Look
//! (`qlmanage`), Apple's built-in file preview.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::RgbImage;

static DURATIONS: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);

fn file() -> PathBuf {
    crate::data_dir().join("video-durations.json")
}

fn with_durations<R>(f: impl FnOnce(&mut HashMap<String, f64>) -> R) -> R {
    let mut g = DURATIONS.lock().unwrap();
    let m = g.get_or_insert_with(|| std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
    f(m)
}

/// Duration of a video in seconds, if known.
pub fn duration(path: &Path) -> Option<f64> {
    with_durations(|m| m.get(&*path.to_string_lossy()).copied())
}

/// Remember durations (and save if something changed).
pub fn set_durations(new: impl IntoIterator<Item = (String, f64)>) {
    let changed = with_durations(|m| {
        let mut g = false;
        for (k, v) in new {
            if m.get(&k) != Some(&v) {
                m.insert(k, v);
                g = true;
            }
        }
        g.then(|| serde_json::to_vec(m).unwrap_or_default())
    });
    if let Some(b) = changed {
        let _ = crate::files::write_atomic(&file(), &b);
    }
}

/// "0:42", "12:05", "1:02:03"
pub fn duration_text(s: f64) -> String {
    let t = s.round().max(0.0) as u64;
    let (h, m, secs) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{secs:02}")
    } else {
        format!("{m}:{secs:02}")
    }
}

/// Preview image of an own video (Mac only); measures the duration along the way.
#[cfg(target_os = "macos")]
pub fn preview(path: &Path, max: u32) -> Result<RgbImage, String> {
    if duration(path).is_none() {
        if let Some(s) = measure_length(path) {
            set_durations([(path.to_string_lossy().into_owned(), s)]);
        }
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!("{TMP_PREFIX}{}-{}", std::process::id(), COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let r = run_quick_look(&tmp, path, max);
    let image = std::fs::read_dir(&tmp).ok().and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "png")));
    let result = match (r, image) {
        (Ok(_), Some(b)) => image::open(&b).map(|i| i.to_rgb8()).map_err(|e| e.to_string()),
        _ => Err("Video – kein Vorschaubild".into()),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Temporary output folders of Quick Look start with this (`graineo-video-<pid>-<n>`); it also marks our own
/// `qlmanage` processes for [`cleanup_orphans`].
#[cfg(target_os = "macos")]
const TMP_PREFIX: &str = "graineo-video-";

/// Longest wait for one Quick Look preview; a hanging `qlmanage` is ended after that.
#[cfg(target_os = "macos")]
const QUICK_LOOK_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// One `qlmanage` at a time, with a time limit: Quick Look hangs indefinitely on some videos (seen: two processes
/// for 7 days, one for 2 hours on a .MOV). `-x` lets the system's Quick Look service compute the preview, so the
/// process we wait for stays small and ends when killed. It runs in its own process group, which is ended as a whole.
#[cfg(target_os = "macos")]
fn run_quick_look(out: &Path, path: &Path, max: u32) -> std::io::Result<std::process::ExitStatus> {
    use std::os::unix::process::CommandExt;
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut child = std::process::Command::new("/usr/bin/qlmanage")
        .arg("-x")
        .arg("-t")
        .arg("-s")
        .arg(max.max(64).to_string())
        .arg("-o")
        .arg(out)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    let end = std::time::Instant::now() + QUICK_LOOK_LIMIT;
    loop {
        if let Some(st) = child.try_wait()? {
            return Ok(st);
        }
        if std::time::Instant::now() > end {
            // the whole group (qlmanage and anything it started), then the process itself
            unsafe {
                libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "qlmanage hängt"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// At start: end `qlmanage` processes left over from an earlier run of the app (it quit or crashed while Quick Look
/// was still working; such processes were seen hanging for days) and remove their output folders. Only processes
/// whose output folder is one of ours ([`TMP_PREFIX`]) and whose app process no longer runs. Returns how many ended.
#[cfg(target_os = "macos")]
pub fn cleanup_orphans() -> usize {
    let mut ended = 0;
    let Ok(out) = std::process::Command::new("/bin/ps").args(["-axww", "-o", "pid=,command="]).output() else {
        return 0;
    };
    let own = std::process::id();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim_start();
        let Some((pid, cmd)) = line.split_once(' ') else { continue };
        let Ok(pid) = pid.parse::<i32>() else { continue };
        if let Some(app) = orphaned_app_pid(cmd) {
            if app != own && !process_alive(app) {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                ended += 1;
            }
        }
    }
    // output folders of app processes that no longer run
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(app) = name.strip_prefix(TMP_PREFIX).and_then(|r| r.split('-').next()).and_then(|p| p.parse::<u32>().ok()) else { continue };
            if app != own && !process_alive(app) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    ended
}

#[cfg(not(target_os = "macos"))]
pub fn cleanup_orphans() -> usize {
    0
}

/// App process id from a `qlmanage` command line with one of our output folders (`…/graineo-video-<pid>-<n>`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn orphaned_app_pid(cmd: &str) -> Option<u32> {
    if !cmd.starts_with("/usr/bin/qlmanage") {
        return None;
    }
    let rest = &cmd[cmd.find("graineo-video-")? + "graineo-video-".len()..];
    rest.split('-').next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn process_alive(pid: u32) -> bool {
    // signal 0: only checks; EPERM means it exists but belongs to someone else
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(not(target_os = "macos"))]
pub fn preview(_path: &Path, _max: u32) -> Result<RgbImage, String> {
    Err("Video – keine Vorschau".into())
}

/// Duration via Spotlight metadata (fast; missing on non-indexed drives).
#[cfg(target_os = "macos")]
fn measure_length(path: &Path) -> Option<f64> {
    let out = std::process::Command::new("/usr/bin/mdls").arg("-raw").arg("-name").arg("kMDItemDurationSeconds").arg(path).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok().filter(|s| *s > 0.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn duration_as_text() {
        assert_eq!(super::duration_text(42.4), "0:42");
        assert_eq!(super::duration_text(725.0), "12:05");
        assert_eq!(super::duration_text(3723.0), "1:02:03");
    }

    #[test]
    fn leftover_quick_look_processes_are_recognised() {
        let cmd = "/usr/bin/qlmanage -x -t -s 2048 -o /var/folders/x/T/graineo-video-4711-3 /Volumes/a/b.MOV";
        assert_eq!(super::orphaned_app_pid(cmd), Some(4711));
        assert_eq!(super::orphaned_app_pid("/usr/bin/qlmanage -t -s 64 -o /tmp/other /a.MOV"), None);
        assert_eq!(super::orphaned_app_pid("/bin/zsh graineo-video-12-1"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quick_look_time_limit_is_short() {
        // the limit is short enough that a hanging preview never blocks the preview generator for long
        assert!(super::QUICK_LOOK_LIMIT <= std::time::Duration::from_secs(20));
    }
}
