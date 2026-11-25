//! Safe start after a crash of the whole computer while graineo computed on the graphics card, and quiet GPU warm-ups.
//!
//! Forum report (Windows, integrated Intel GPU): a blue screen without opening a photo, only menus clicked; another
//! Foveon program did the same on that computer. Without a photo, the GPU work of graineo is the window itself and
//! the warm-ups at the start (devices of the Sigma noise reduction and of the other RAW formats, their shaders
//! compiled ahead).
//!
//! **Start marker:** every start leaves `session-running.json` in the data folder (start time, whether the graphics
//! card may compute); a clean quit removes it. Is it still there at the next start, the computer has started again
//! since then (boot time later than the marker; on Windows also no orderly shutdown recorded since), and that session
//! computed on the graphics card, graineo starts with "Grafikkarte verwenden" switched off (saved like the setting)
//! and says so once, with a button to switch it back on.
//!
//! No endless fallback: a session without the graphics card never triggers it, and a program that only ended (crash,
//! force quit, update) neither – only a restart of the whole computer does. The window always draws with the graphics
//! card; this guard covers the computing paths and the warm-ups.
//! Only macOS and Windows: there an orderly shutdown either ends graineo cleanly (macOS asks every program to quit) or
//! is recorded by the system (Windows `ShutdownTime`); on Linux a shutdown simply ends the process.
//!
//! **Warm-ups:** one after another on one thread at background priority, a few seconds after the start and only when
//! the graphics card may compute; skipped on an integrated Intel GPU under Windows (the device is then created by the
//! first photo that needs it, 1–2 s once per session).

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use eframe::egui_wgpu::wgpu;
use serde::{Deserialize, Serialize};

const MARKER: &str = "session-running.json";

/// The guard is active on these systems (see the module text).
const ACTIVE: bool = cfg!(any(windows, target_os = "macos"));

/// Seconds of tolerance between the boot time and the marker (Windows derives the boot time from the uptime).
const SLACK: u64 = 2;

/// The notice is due (set at the start, cleared by the dialog).
static NOTICE: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    /// start of the session, seconds since 1970
    pub started: u64,
    /// the graphics card was allowed to compute in this session (at the start or later)
    pub gpu: bool,
}

/// Did the computer go down while the session of `marker` computed on the graphics card? `boot`: start of the system
/// now running; `clean_shutdown`: last orderly shutdown or restart the system recorded (Windows), if known.
pub fn system_crashed_with_gpu(marker: Option<Marker>, boot: Option<u64>, clean_shutdown: Option<u64>) -> bool {
    let (Some(m), Some(boot)) = (marker, boot) else { return false };
    m.gpu && boot > m.started + SLACK && !clean_shutdown.is_some_and(|c| c >= m.started)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn marker_path() -> Option<std::path::PathBuf> {
    crate::app_file(MARKER)
}

fn write_marker(m: Marker) {
    if let (Some(p), Ok(json)) = (marker_path(), serde_json::to_vec(&m)) {
        let _ = std::fs::write(p, json);
    }
}

/// May the graphics card compute (setting and `GRAINEO_GPU=0`)?
fn gpu_on() -> bool {
    crate::prefs::gpu_setting() && crate::env::var("GRAINEO_GPU").as_deref() != Ok("0")
}

/// Test hook (only with `GRAINEO_DATA_DIR`): `GRAINEO_TEST_BOOT_TIME=<seconds since 1970>|now` pretends the system
/// started then (`now`: just now, i.e. after every earlier marker).
fn test_boot_time() -> Option<u64> {
    crate::env::var_os("GRAINEO_DATA_DIR")?;
    match crate::env::var("GRAINEO_TEST_BOOT_TIME").ok()?.trim() {
        "now" => Some(now()),
        v => v.parse().ok(),
    }
}

/// At the start, before the window: read the marker of the previous session, fall back to the processor after a
/// crash of the computer, and leave the marker of this session.
pub fn at_start() {
    if !ACTIVE {
        return;
    }
    let previous = marker_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice::<Marker>(&b).ok());
    let boot = test_boot_time().or_else(library::platform::boot_time);
    if system_crashed_with_gpu(previous, boot, library::platform::last_clean_shutdown()) {
        crate::log(&format!("gpu guard: the computer started again (boot {boot:?}) after the session of {:?} that computed on the graphics card; this session computes on the processor", previous.map(|m| m.started)));
        library::settings::set("useGpu", serde_json::json!(false));
        NOTICE.store(true, Ordering::Relaxed);
    }
    write_marker(Marker { started: now(), gpu: gpu_on() });
}

/// Clean quit: the session ended in order.
pub fn at_quit() {
    if let Some(p) = marker_path().filter(|_| ACTIVE) {
        let _ = std::fs::remove_file(p);
    }
}

/// The setting "Grafikkarte verwenden" changed while running: save, apply, and note in the marker that this session
/// may compute on the graphics card (a later crash of the computer then counts).
pub fn set_gpu(on: bool) {
    library::settings::set("useGpu", serde_json::json!(on));
    crate::prefs::apply_gpu_setting();
    if ACTIVE && gpu_on() {
        let started = marker_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice::<Marker>(&b).ok()).map_or_else(now, |m| m.started);
        write_marker(Marker { started, gpu: true });
    }
}

/// The notice after the fallback, once (dialog with "Schließen" and "Grafikkarte wieder verwenden").
pub fn show_notice(ctx: &egui::Context) {
    if !NOTICE.load(Ordering::Relaxed) {
        return;
    }
    let (answer, again) = crate::dialog::show_extra(
        ctx,
        "gpu-guard",
        crate::tr("graineo rechnet ohne Grafikkarte"),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::text(ui, crate::tr("Der Rechner wurde neu gestartet, während graineo lief (z. B. nach einem Bluescreen). Zur Sicherheit rechnet graineo jetzt nur auf dem Prozessor."));
            crate::dialog::hint(ui, crate::tr("Einstellungen → Entwickeln → Leistung → Grafikkarte verwenden"));
        },
        crate::dialog::Buttons::close(),
        Some(crate::tr("Grafikkarte wieder verwenden")),
    );
    if again {
        set_gpu(true);
        crate::log("gpu guard: graphics card switched on again from the notice");
    }
    if again || answer != crate::dialog::Answer::Open {
        NOTICE.store(false, Ordering::Relaxed);
    }
}

/// Name and kind of the window's graphics adapter (from `photo_display::init`).
static WINDOW_ADAPTER: std::sync::OnceLock<(u32, bool)> = std::sync::OnceLock::new();

/// Remember the window's adapter: vendor id and whether it is an integrated GPU.
pub fn note_window_adapter(info: &wgpu::AdapterInfo) {
    let _ = WINDOW_ADAPTER.set((info.vendor, info.device_type == wgpu::DeviceType::IntegratedGpu));
}

/// Skip the warm-ups: integrated Intel GPU (PCI vendor 0x8086) under Windows. Pure for the test.
fn skip_warm_up(windows: bool, vendor: u32, integrated: bool) -> bool {
    windows && integrated && vendor == 0x8086
}

/// Warm the GPU paths up (devices, shaders), so that the first photo is refined quickly: one after another on one
/// thread at background priority, a few seconds after the start, only when the graphics card may compute.
pub fn warm_up_later() {
    let _ = std::thread::Builder::new().name("gpu-warm-up".into()).spawn(|| {
        library::platform::background_priority();
        std::thread::sleep(std::time::Duration::from_secs(4));
        crate::jobs::wait_for_foreground(std::time::Duration::from_secs(20));
        if let Some(&(vendor, integrated)) = WINDOW_ADAPTER.get() {
            if skip_warm_up(cfg!(windows), vendor, integrated) {
                crate::log("gpu warm-up: skipped (integrated Intel GPU under Windows; devices are created by the first photo)");
                return;
            }
        }
        if !pools::gpu_allowed() {
            crate::log("gpu warm-up: skipped (graphics card switched off)");
            return;
        }
        let t = std::time::Instant::now();
        let r = develop::spp::gpu_adapter();
        crate::log(&format!("SPP-GPU: {r:?}"));
        if pools::gpu_allowed() {
            rawdev::gpu::warm_up();
        }
        crate::log(&format!("gpu warm-up: done in {:.1} s", t.elapsed().as_secs_f64()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(started: u64, gpu: bool) -> Option<Marker> {
        Some(Marker { started, gpu })
    }

    /// Only a restart of the whole computer after a session that computed on the graphics card counts.
    #[test]
    fn fallback_only_after_a_system_restart_with_gpu() {
        // clean quit (no marker), unknown boot time
        assert!(!system_crashed_with_gpu(None, Some(2_000), None));
        assert!(!system_crashed_with_gpu(m(1_000, true), None, None));
        // the computer kept running: the program only ended (crash, force quit, update)
        assert!(!system_crashed_with_gpu(m(1_000, true), Some(500), None));
        assert!(!system_crashed_with_gpu(m(1_000, true), Some(1_001), None), "within the tolerance of the boot time");
        // the computer started again after the session: blue screen, power cut, hard reset
        assert!(system_crashed_with_gpu(m(1_000, true), Some(1_300), None));
        // ... but the session computed on the processor only: no fallback (no endless fallback either)
        assert!(!system_crashed_with_gpu(m(1_000, false), Some(1_300), None));
        // Windows recorded an orderly shutdown or restart after the start: not a crash
        assert!(!system_crashed_with_gpu(m(1_000, true), Some(1_300), Some(1_200)));
        // the last orderly shutdown was before the session: crash
        assert!(system_crashed_with_gpu(m(1_000, true), Some(1_300), Some(900)));
    }

    #[test]
    fn marker_round_trip() {
        let a = Marker { started: 1_795_564_800, gpu: true };
        let b: Marker = serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
        assert_eq!(a, b);
        assert!(serde_json::from_slice::<Marker>(b"{").is_err(), "a damaged marker counts as none");
    }

    #[test]
    fn warm_up_skipped_only_on_integrated_intel_under_windows() {
        assert!(skip_warm_up(true, 0x8086, true));
        assert!(!skip_warm_up(true, 0x8086, false), "Intel Arc (discrete)");
        assert!(!skip_warm_up(true, 0x10de, false), "NVIDIA");
        assert!(!skip_warm_up(true, 0x1002, true), "AMD integrated");
        assert!(!skip_warm_up(false, 0x8086, true), "Intel Mac");
    }
}
