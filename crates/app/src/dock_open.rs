//! Drag photos onto the app icon in the Dock ("Open With" in the Finder works too): macOS reports the
//! files to the app delegate via `application:openURLs:`. winit does not know this method, so
//! it is added at runtime to the existing delegate here. The paths land in a queue
//! which the app picks up every frame; they open the import (see `import_ui.rs`).
//!
//! The Info.plist (scripts/bundle-macos.sh) lists the file types with rank "Alternate" so that graineo never
//! becomes the default app for photos.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

/// Fetch the paths reported so far (empties the queue).
pub fn fetch() -> Vec<PathBuf> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// Add the method to the app delegate (once, early – after the window is created).
#[cfg(target_os = "macos")]
pub fn setup(ctx: &eframe::egui::Context) {
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{class, msg_send, sel};
    use std::ffi::{c_char, CStr};
    let _ = CTX.set(ctx.clone());

    unsafe extern "C-unwind" fn open_urls(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, urls: *mut AnyObject) {
        if urls.is_null() {
            return;
        }
        let n: usize = msg_send![urls, count];
        let mut new_paths = Vec::new();
        for i in 0..n {
            let url: *mut AnyObject = msg_send![urls, objectAtIndex: i];
            if url.is_null() {
                continue;
            }
            let ns_path: *mut AnyObject = msg_send![url, path];
            if ns_path.is_null() {
                continue;
            }
            let c: *const c_char = msg_send![ns_path, UTF8String];
            if !c.is_null() {
                new_paths.push(PathBuf::from(CStr::from_ptr(c).to_string_lossy().into_owned()));
            }
        }
        if !new_paths.is_empty() {
            PENDING.lock().unwrap_or_else(|e| e.into_inner()).extend(new_paths);
            if let Some(c) = CTX.get() {
                c.request_repaint();
            }
        }
    }

    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let delegate: *mut AnyObject = msg_send![app, delegate];
        if delegate.is_null() {
            return;
        }
        let cls: *const objc2::runtime::AnyClass = msg_send![delegate, class];
        let imp: unsafe extern "C-unwind" fn() = std::mem::transmute(open_urls as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject));
        // "v@:@@": no return value, self, _cmd, application, list of URLs
        let ok = objc2::ffi::class_addMethod(cls as *mut _, sel!(application:openURLs:), imp, c"v@:@@".as_ptr());
        crate::log(&format!("dock-open: delegate method {}", if ok.as_bool() { crate::tr("nachgerüstet") } else { "already present/rejected" }));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn setup(_ctx: &eframe::egui::Context) {}
