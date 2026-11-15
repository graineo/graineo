//! Open files and folders with the system or reveal them in the file manager (macOS Finder, Windows Explorer).

use std::ffi::OsStr;

/// Open a file, folder or address with the responsible application.
pub fn open(p: impl AsRef<OsStr>) {
    let program = if cfg!(windows) { "explorer" } else { "open" };
    let _ = std::process::Command::new(program).arg(p.as_ref()).spawn();
}

/// Reveal a file in the file manager (selected).
pub fn show(p: impl AsRef<OsStr>) {
    if cfg!(windows) {
        let mut a = std::ffi::OsString::from("/select,");
        a.push(p.as_ref());
        let _ = std::process::Command::new("explorer").arg(a).spawn();
    } else {
        let _ = std::process::Command::new("open").arg("-R").arg(p.as_ref()).spawn();
    }
}
