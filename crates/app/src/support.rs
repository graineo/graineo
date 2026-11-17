//! "Support graineo": a quiet link (Help menu, About window) that opens a web page in the browser.
//!
//! The app never sends anything for this: no payment, no account, no tracking, no network request.
//! It only hands an address to the system browser when the user clicks the entry.
//!
//! Built in but off: while [`SUPPORT_URLS`] is empty, neither the menu entry nor the About link is shown.

/// Addresses that can be opened from "Support graineo …", in order of preference (the first non-empty one is used).
///
/// Empty: the feature stays hidden. To switch it on, enable the project's support page below once it links to a
/// provider; a provider page can also be entered directly. Changing the provider is a change of this one line.
const SUPPORT_URLS: &[&str] = &[
    // "https://graineo.github.io/support/",
];

/// Address opened by "Support graineo …", or `None` while no address is set (entry hidden).
///
/// Test aid: with `GRAINEO_DATA_DIR`, `GRAINEO_SUPPORT_TEST_URL` sets an address (screenshots of the menu).
pub fn url() -> Option<String> {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_some() {
        if let Some(u) = crate::env::var_os("GRAINEO_SUPPORT_TEST_URL") {
            return first(&[&u.to_string_lossy()]).map(str::to_string);
        }
    }
    first(SUPPORT_URLS).map(str::to_string)
}

/// Whether the entry is shown.
pub fn enabled() -> bool {
    url().is_some()
}

/// Opens the support address in the browser (no request by the app itself).
pub fn open() {
    if let Some(u) = url() {
        crate::system::open(u);
    }
}

/// First usable address of a list: non-empty and a web address (`https://`).
fn first<'a>(list: &[&'a str]) -> Option<&'a str> {
    list.iter().map(|u| u.trim()).find(|u| u.starts_with("https://") && u.len() > "https://".len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_hides_the_entry() {
        assert_eq!(first(&[]), None);
        assert_eq!(first(&["", "  "]), None);
    }

    #[test]
    fn filled_list_gives_the_first_address() {
        assert_eq!(first(&["https://graineo.github.io/support/"]), Some("https://graineo.github.io/support/"));
        assert_eq!(first(&["", " https://example.org/a ", "https://example.org/b"]), Some("https://example.org/a"));
    }

    #[test]
    fn only_web_addresses() {
        assert_eq!(first(&["file:///etc/passwd", "javascript:alert(1)", "https://"]), None);
        assert_eq!(first(&["http://example.org"]), None);
    }
}
