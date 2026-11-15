//! System file and folder dialogs without halting the UI.
//!
//! A blocking dialog in the middle of drawing halts the window's event loop. If you then
//! drag e.g. a photo from the Finder onto the dialog, events for our window arrive in the middle of
//! the still running draw and the program aborts. Therefore the dialog runs here on
//! a helper thread (macOS moves the window to the main thread itself), and the result
//! is picked up in the next frame: first `folder`/`file`/… on click, then `fetch` every frame.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use rfd::FileDialog;

#[derive(Default)]
struct Store {
    running: HashSet<String>,
    done: HashMap<String, Vec<PathBuf>>,
    /// chosen write targets refused because they lie in a package of another program (see [`target_folder`])
    refused: Vec<(PathBuf, String)>,
}

fn storage() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(Default::default)
}

#[derive(Clone, Copy)]
enum Kind {
    Folder,
    /// a folder the app writes into (export, move, import target …)
    TargetFolder,
    File,
    /// several files
    Files,
    Save,
}

fn start(ctx: &egui::Context, id: &str, kind: Kind, configure: impl FnOnce(FileDialog) -> FileDialog + Send + 'static) {
    {
        let mut s = storage().lock().unwrap();
        // only one dialog at a time per ID; an old result that was never picked up expires
        if !s.running.insert(id.to_string()) {
            return;
        }
        s.done.remove(id);
    }
    let (ctx, id) = (ctx.clone(), id.to_string());
    std::thread::Builder::new()
        .name("dateidialog".into())
        .spawn(move || {
            let d = configure(FileDialog::new());
            let choice: Vec<PathBuf> = match kind {
                Kind::Folder | Kind::TargetFolder => d.pick_folder().into_iter().collect(),
                Kind::File => d.pick_file().into_iter().collect(),
                Kind::Files => d.pick_files().unwrap_or_default(),
                Kind::Save => d.save_file().into_iter().collect(),
            };
            // write targets: never inside an Apple Photos library or another program's package
            let refused = matches!(kind, Kind::TargetFolder | Kind::Save).then(|| choice.iter().find_map(|p| library::write_target::check(p).err().map(|e| (p.clone(), e)))).flatten();
            let mut s = storage().lock().unwrap();
            s.running.remove(&id);
            if let Some(r) = refused {
                s.refused.push(r);
            } else if !choice.is_empty() {
                s.done.insert(id, choice);
            }
            drop(s);
            ctx.request_repaint();
        })
        .expect("dialog thread");
}

/// Choose a folder. `configure` sets title, start folder etc.
pub fn folder(ctx: &egui::Context, id: &str, configure: impl FnOnce(FileDialog) -> FileDialog + Send + 'static) {
    start(ctx, id, Kind::Folder, configure);
}

/// Choose a folder the app writes into (export, moving, import target …): a folder inside an Apple Photos library
/// or another program's package is refused – the dialog then gives no result and [`take_refused`] the reason.
pub fn target_folder(ctx: &egui::Context, id: &str, configure: impl FnOnce(FileDialog) -> FileDialog + Send + 'static) {
    start(ctx, id, Kind::TargetFolder, configure);
}

/// Choose a file to open.
pub fn file(ctx: &egui::Context, id: &str, configure: impl FnOnce(FileDialog) -> FileDialog + Send + 'static) {
    start(ctx, id, Kind::File, configure);
}

/// Choose several files to open.
pub fn files(ctx: &egui::Context, id: &str, configure: impl FnOnce(FileDialog) -> FileDialog + Send + 'static) {
    start(ctx, id, Kind::Files, configure);
}

/// Choose a save location (refused like [`target_folder`] inside a package).
pub fn save(ctx: &egui::Context, id: &str, configure: impl FnOnce(FileDialog) -> FileDialog + Send + 'static) {
    start(ctx, id, Kind::Save, configure);
}

/// Pick up the finished selection (once). `None`: still open or cancelled.
pub fn fetch(id: &str) -> Option<Vec<PathBuf>> {
    storage().lock().unwrap().done.remove(id)
}

/// Like [`fetch`], for dialogs with exactly one selection.
pub fn fetch_one(id: &str) -> Option<PathBuf> {
    fetch(id).and_then(|v| v.into_iter().next())
}

/// A refused write target (path, reason in German), once – shown by the window as a notice.
pub fn take_refused() -> Option<(PathBuf, String)> {
    let mut s = storage().lock().unwrap();
    (!s.refused.is_empty()).then(|| s.refused.remove(0))
}
