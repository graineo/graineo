//! The web page, built into the program (no files on disk, nothing from other servers).

/// Body and content type of a static file of the page.
pub fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "/" | "/index.html" => (include_bytes!("../web/index.html"), "text/html; charset=utf-8"),
        "/app.js" => (include_bytes!("../web/app.js"), "text/javascript; charset=utf-8"),
        "/app.css" => (include_bytes!("../web/app.css"), "text/css; charset=utf-8"),
        "/texts.tsv" => (include_bytes!("../web/texts.tsv"), "text/plain; charset=utf-8"),
        "/manifest.webmanifest" => (include_bytes!("../web/manifest.webmanifest"), "application/manifest+json"),
        "/icon.svg" => (include_bytes!("../web/icon.svg"), "image/svg+xml"),
        "/apple-touch-icon.png" => (include_bytes!("../web/apple-touch-icon.png"), "image/png"),
        "/fonts/plex-mono-400.woff2" => (include_bytes!("../web/fonts/plex-mono-400.woff2"), "font/woff2"),
        "/fonts/plex-mono-500.woff2" => (include_bytes!("../web/fonts/plex-mono-500.woff2"), "font/woff2"),
        "/fonts/OFL.txt" => (include_bytes!("../web/fonts/OFL-IBMPlex.txt"), "text/plain; charset=utf-8"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn page_has_no_external_resources() {
        for p in ["/", "/app.js", "/app.css"] {
            let (b, _) = super::asset(p).unwrap();
            let t = std::str::from_utf8(b).unwrap();
            assert!(!t.contains("http://") && !t.contains("//cdn") && !t.contains("https://"), "{p} refers to another server");
            assert!(!t.contains("innerHTML"), "{p}: build elements, do not parse HTML");
        }
        let (b, _) = super::asset("/").unwrap();
        let t = std::str::from_utf8(b).unwrap();
        assert!(!t.contains("<script>") && !t.contains("style="), "no inline script or style (CSP)");
    }

    #[test]
    fn every_text_has_german_and_english() {
        let (b, _) = super::asset("/texts.tsv").unwrap();
        for (i, line) in std::str::from_utf8(b).unwrap().lines().enumerate().skip(1) {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            assert!(cols.len() >= 3 && !cols[1].is_empty() && !cols[2].is_empty(), "line {}: {line}", i + 1);
        }
    }
}
