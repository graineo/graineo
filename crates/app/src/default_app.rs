//! Default app per RAW format (settings → General): shows which program opens X3F (and on request CR3, RAF,
//! DNG, ARW) in the Finder and makes graineo the default with one click (Launch Services,
//! `LSSetDefaultRoleHandlerForContentType`). Only reads and sets the system's assignment, nothing else.
//! Other systems: no function (the row is not shown).

/// Formats the settings offer: X3F always, the others behind "More formats".
pub const FORMATS: [&str; 5] = ["x3f", "cr3", "raf", "dng", "arw"];

/// Default handler of a file extension: (bundle identifier, app name). `None`: none known.
pub fn handler(ext: &str) -> Option<(String, String)> {
    imp::handler(ext)
}

/// Bundle identifier of the running app. `None`: started without a bundle (development build), setting is impossible.
pub fn own_id() -> Option<String> {
    imp::own_id()
}

/// Make graineo the default for this extension. Error text for the user.
pub fn set_default(ext: &str) -> Result<(), String> {
    imp::set_default(ext)
}

/// Same app? Launch Services answers bundle identifiers in lower case in some versions.
pub fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_void, CStr};
    use std::os::raw::c_char;

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const ROLES_ALL: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, len: isize, encoding: u32, external: u8) -> CFStringRef;
        fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(cf: *const c_void);
    }
    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(class: CFStringRef, tag: CFStringRef, conforming: CFStringRef) -> CFStringRef;
        fn LSCopyDefaultRoleHandlerForContentType(uti: CFStringRef, role: u32) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(uti: CFStringRef, role: u32, bundle: CFStringRef) -> i32;
    }

    struct Cf(CFStringRef);
    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }
    fn cf(s: &str) -> Cf {
        Cf(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0) })
    }
    fn text(s: CFStringRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        let mut buf = vec![0 as c_char; 1024];
        let ok = unsafe { CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, UTF8) };
        (ok != 0).then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
    }

    /// Content types of an extension: the system's preferred one, for X3F also the one graineo declares itself.
    fn types(ext: &str) -> Vec<String> {
        let tag = cf(ext);
        let preferred = Cf(unsafe { UTTypeCreatePreferredIdentifierForTag(kUTTagClassFilenameExtension, tag.0, std::ptr::null()) });
        let mut v: Vec<String> = text(preferred.0).into_iter().collect();
        if ext.eq_ignore_ascii_case("x3f") && !v.iter().any(|t| t == "com.sigmaphoto.photopro-x3f") {
            v.push("com.sigmaphoto.photopro-x3f".into());
        }
        v
    }

    pub fn handler(ext: &str) -> Option<(String, String)> {
        let uti = types(ext).into_iter().next()?;
        let u = cf(&uti);
        let id = Cf(unsafe { LSCopyDefaultRoleHandlerForContentType(u.0, ROLES_ALL) });
        let id = text(id.0)?;
        let name = app_name(&id).unwrap_or_else(|| id.clone());
        Some((id, name))
    }

    fn app_name(id: &str) -> Option<String> {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2_foundation::NSString;
        unsafe {
            let ws: *mut AnyObject = msg_send![AnyClass::get(c"NSWorkspace")?, sharedWorkspace];
            if ws.is_null() {
                return None;
            }
            let ns = NSString::from_str(id);
            let url: *mut AnyObject = msg_send![ws, URLForApplicationWithBundleIdentifier: &*ns];
            if url.is_null() {
                return None;
            }
            let path: *mut NSString = msg_send![url, path];
            let p = path.as_ref()?.to_string();
            std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().into_owned())
        }
    }

    pub fn own_id() -> Option<String> {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2_foundation::NSString;
        unsafe {
            let bundle: *mut AnyObject = msg_send![AnyClass::get(c"NSBundle")?, mainBundle];
            if bundle.is_null() {
                return None;
            }
            let id: *mut NSString = msg_send![bundle, bundleIdentifier];
            id.as_ref().map(|s| s.to_string())
        }
    }

    pub fn set_default(ext: &str) -> Result<(), String> {
        let Some(own) = own_id() else {
            return Err(crate::tr("Nur in der installierten App möglich.").to_string());
        };
        let b = cf(&own);
        let mut error = None;
        for uti in types(ext) {
            let u = cf(&uti);
            let status = unsafe { LSSetDefaultRoleHandlerForContentType(u.0, ROLES_ALL, b.0) };
            if status != 0 {
                error = Some(crate::trf!("macOS hat abgelehnt (Fehler {a}).", a = status));
            }
        }
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn handler(_ext: &str) -> Option<(String, String)> {
        None
    }
    pub fn own_id() -> Option<String> {
        None
    }
    pub fn set_default(_ext: &str) -> Result<(), String> {
        Err(String::new())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_ignores_case() {
        assert!(super::same("io.github.graineo", "io.github.Graineo"));
        assert!(!super::same("io.github.graineo", "com.apple.Preview"));
    }

    /// Reads only (never sets): the system answers for a common extension without crashing.
    #[cfg(target_os = "macos")]
    #[test]
    fn reading_the_handler_does_not_crash() {
        let _ = super::handler("x3f");
        let jpg = super::handler("jpg");
        assert!(jpg.is_some(), "JPEG always has a default app on macOS");
    }
}
