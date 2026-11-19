//! Windows: "Mit graineo ansehen" in the context menu of the Explorer, for folders, the background of an open folder,
//! drives and photo files. graineo opens the folder like "View folder" (a file: its folder, with the file active; the
//! command line already does that). Written only under HKEY_CURRENT_USER\Software\Classes (no administrator rights, no
//! installer), only in graineo's own keys (`…\shell\graineo`); switching it off deletes exactly these keys again.
//! Photo files use `SystemFileAssociations\.<ext>`: it works whichever program is the default for the extension and
//! never touches that assignment.
//!
//! Windows 11 shows such entries under "Show more options" (the classic menu); the new short menu takes only packaged
//! apps. Other systems: no function ([`AVAILABLE`] is false, the setting is not shown).

use std::path::Path;

/// Can the menu be offered on this system?
pub const AVAILABLE: bool = cfg!(windows);

/// Name of graineo's verb key under each `shell`.
pub const VERB: &str = "graineo";

/// One place in the registry: the key of the verb (below `HKCU\Software\Classes`) and the placeholder Explorer fills in.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub key: String,
    pub arg: &'static str,
}

/// Extensions of the photo files that get the entry: every photo format graineo shows, not videos.
pub fn photo_extensions() -> Vec<&'static str> {
    crate::EXTENSIONS.iter().filter(|(_, f)| *f != "VIDEO").map(|(e, _)| *e).collect()
}

/// All places of the entry: folders (`%1`), the background of an open folder (`%V`, the folder itself), drives and
/// photo files (`%1`).
pub fn places() -> Vec<Place> {
    let mut v = vec![
        Place { key: format!(r"Directory\shell\{VERB}"), arg: "%1" },
        Place { key: format!(r"Directory\Background\shell\{VERB}"), arg: "%V" },
        Place { key: format!(r"Drive\shell\{VERB}"), arg: "%1" },
    ];
    v.extend(photo_extensions().into_iter().map(|e| Place { key: format!(r"SystemFileAssociations\.{e}\shell\{VERB}"), arg: "%1" }));
    v
}

/// The command Explorer runs: program and argument each in quotes (paths with spaces; Windows paths never contain
/// a quote character, so no escaping is needed). Unicode stays as it is (written as UTF-16).
pub fn command_line(exe: &Path, arg: &str) -> String {
    format!("\"{}\" \"{arg}\"", exe.display())
}

/// Icon of the entry: the program's own icon (first icon resource of the exe).
pub fn icon_value(exe: &Path) -> String {
    exe.display().to_string()
}

/// Is the entry there, and does it start this program? (`Some(false)`: there, but for another place of graineo.exe, for
/// example after moving the unzipped folder.) `None`: not there.
pub fn state() -> Option<bool> {
    let exe = std::env::current_exe().ok()?;
    let first = places().into_iter().next()?;
    let found = imp::read_default(&format!(r"{}\command", first.key))?;
    Some(found == command_line(&exe, first.arg))
}

/// Write the entry for all places (`label`: the menu text, already translated), or remove it (`label` = `None`).
pub fn set(label: Option<&str>) -> Result<(), String> {
    match label {
        Some(text) => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            for p in places() {
                imp::write_default(&p.key, text)?;
                imp::write_value(&p.key, "Icon", &icon_value(&exe))?;
                imp::write_default(&format!(r"{}\command", p.key), &command_line(&exe, p.arg))?;
            }
            Ok(())
        }
        None => {
            for p in places() {
                imp::delete_tree(&p.key)?;
            }
            Ok(())
        }
    }
}

/// At startup: an entry that starts another graineo.exe (the folder was moved) is written again for this one.
pub fn renew(label: &str) {
    if state() == Some(false) {
        let _ = set(Some(label));
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ};

    const BASE: &str = r"Software\Classes\";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let k = wide(&format!("{BASE}{key}"));
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: null-terminated key name, output handle; no class, default security
        let r = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, std::ptr::null(), &mut h, std::ptr::null_mut()) };
        if r != ERROR_SUCCESS {
            return Err(format!("registry {key}: error {r}"));
        }
        let n = name.map(wide);
        let v = wide(value);
        // SAFETY: open handle, null-terminated value name (or null for the default value), UTF-16 data with its size
        // in bytes including the terminating zero
        let r = unsafe { RegSetValueExW(h, n.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()), 0, REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32) };
        // SAFETY: handle opened above
        unsafe { RegCloseKey(h) };
        if r != ERROR_SUCCESS {
            return Err(format!("registry {key}: error {r}"));
        }
        Ok(())
    }

    pub fn write_default(key: &str, value: &str) -> Result<(), String> {
        set(key, None, value)
    }

    pub fn write_value(key: &str, name: &str, value: &str) -> Result<(), String> {
        set(key, Some(name), value)
    }

    pub fn read_default(key: &str) -> Option<String> {
        let k = wide(&format!("{BASE}{key}"));
        let mut buf = vec![0u16; 2048];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: null-terminated key name, null value name (default value), output buffer with its size in bytes
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), std::ptr::null(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != ERROR_SUCCESS {
            return None;
        }
        let n = (len as usize / 2).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]).trim_end_matches('\0').to_string())
    }

    pub fn delete_tree(key: &str) -> Result<(), String> {
        let k = wide(&format!("{BASE}{key}"));
        // SAFETY: null-terminated key name below HKCU; deletes only this key and its subkeys
        let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr()) };
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return Err(format!("registry {key}: error {r}"));
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn write_default(_: &str, _: &str) -> Result<(), String> {
        Err("only on Windows".into())
    }
    pub fn write_value(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("only on Windows".into())
    }
    pub fn read_default(_: &str) -> Option<String> {
        None
    }
    pub fn delete_tree(_: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn command_line_quotes_program_and_argument() {
        let exe = PathBuf::from(r"C:\Users\Jörg Müller\Programme\graineo 1.4\graineo.exe");
        assert_eq!(command_line(&exe, "%1"), r#""C:\Users\Jörg Müller\Programme\graineo 1.4\graineo.exe" "%1""#);
        assert_eq!(command_line(&exe, "%V"), r#""C:\Users\Jörg Müller\Programme\graineo 1.4\graineo.exe" "%V""#);
        assert_eq!(icon_value(&exe), r"C:\Users\Jörg Müller\Programme\graineo 1.4\graineo.exe");
        // UTF-16 round trip of what is written (umlauts, CJK)
        let s = command_line(&PathBuf::from(r"D:\写真\graineo.exe"), "%1");
        assert_eq!(String::from_utf16(&s.encode_utf16().collect::<Vec<_>>()).unwrap(), s);
    }

    #[test]
    fn places_are_only_graineos_own_keys() {
        let p = places();
        assert_eq!(p[0], Place { key: r"Directory\shell\graineo".into(), arg: "%1" });
        assert_eq!(p[1], Place { key: r"Directory\Background\shell\graineo".into(), arg: "%V" });
        assert_eq!(p[2], Place { key: r"Drive\shell\graineo".into(), arg: "%1" });
        assert!(p.iter().all(|x| x.key.ends_with(r"\shell\graineo")), "nothing outside our own verb keys");
        for e in ["jpg", "jpeg", "x3f", "cr3", "raf", "orf", "dng", "heic", "tif", "png"] {
            assert!(p.iter().any(|x| x.key == format!(r"SystemFileAssociations\.{e}\shell\graineo")), "{e}");
        }
        assert!(!p.iter().any(|x| x.key.contains(".mp4") || x.key.contains(".mov")), "no videos");
        // every listed extension is one graineo reads
        for e in photo_extensions() {
            assert!(crate::format_from_extension(&format!("a.{e}")).is_some(), "{e}");
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn nothing_on_other_systems() {
        assert!(!AVAILABLE);
        assert_eq!(state(), None);
        assert!(set(Some("x")).is_err());
        assert!(set(None).is_ok());
    }
}
