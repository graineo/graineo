//! Headless egui runs check geometry/events without a GPU renderer.
//! Generated texture commands are explicitly discarded; egui 0.37 requires
//! this acknowledgement even when a test only looks at shapes.
use eframe::egui;
pub fn run(
    ctx: &egui::Context,
    input: egui::RawInput,
    f: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut output = ctx.run_ui(input, f);
    output.textures_delta.clear();
    output
}

/// Tests that build a whole `App` share process-wide state (worker pools and their priority gates,
/// settings, the data folder of the test run). Run in parallel they wait for each other's background
/// work and miss their time limits; each takes this lock while its app lives, so they run one at a time.
pub fn app_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Upper bound for waiting on background work in the window tests (loading, developing, previews). It only
/// guards against a hang: with debug builds and the other tests (or other builds) busy on the same machine,
/// loading a small photo took over 10 s; the checks after the wait are unchanged.
pub const WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Every run of the app's tests gets a fresh data folder: `GRAINEO_DATA_DIR` (the folder the caller chose, required by
/// the window tests) becomes its subfolder `test-run-<process>`, set before `main` and before any test reads it. So
/// leftovers of earlier runs (looks saved by a test, settings, library, previews) never meet a later run – e.g.
/// `darkroom_recipes::drawer_recognizes_applies_and_closes` counts the looks it saves. Without the variable nothing
/// changes (the libraries then use their own temp folder per test program anyway).
extern "C" fn fresh_data_dir() {
    let Some(base) = std::env::var_os("GRAINEO_DATA_DIR").filter(|v| !v.is_empty()) else { return };
    let dir = std::path::PathBuf::from(base).join(format!("test-run-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_ok() {
        std::env::set_var("GRAINEO_DATA_DIR", &dir);
    }
}

/// Runs [`fresh_data_dir`] when the test program loads (a static initialiser, as the `ctor` crate does it), before
/// the test threads start.
#[used]
#[cfg_attr(target_vendor = "apple", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(all(unix, not(target_vendor = "apple")), link_section = ".init_array")]
#[cfg_attr(windows, link_section = ".CRT$XCU")]
static FRESH_DATA_DIR: extern "C" fn() = fresh_data_dir;

#[test]
fn test_run_has_its_own_data_folder() {
    if let Some(d) = std::env::var_os("GRAINEO_DATA_DIR") {
        let d = std::path::PathBuf::from(d);
        assert_eq!(d.file_name().and_then(|n| n.to_str()), Some(format!("test-run-{}", std::process::id()).as_str()));
        assert_eq!(library::data_dir(), d);
    }
}
