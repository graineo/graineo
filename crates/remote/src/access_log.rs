//! Access log of the browser access: who did what and when, never contents.
//!
//! One line per event: UTC time, peer address, device id (public, not the token), method, route
//! pattern (ids replaced by `:id`, no query strings) and status. File `remote-access.log` in the
//! log folder, rotated at 2 MB (one older file is kept as `remote-access.1.log`).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub struct AccessLog {
    file: Option<PathBuf>,
    lock: Mutex<()>,
}

/// Route without ids and query: `/api/image/123/thumb?x=1` → `/api/image/:id/thumb`.
pub fn pattern(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let parts: Vec<String> = path
        .split('/')
        .map(|p| {
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
                ":id".to_string()
            } else if p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.' && p.len() > 2 && !p.starts_with('.')) {
                p.to_string()
            } else {
                // anything else (ids with tricks, traversal attempts, encoded text) is not repeated
                "*".to_string()
            }
        })
        .collect();
    let mut s = parts.join("/");
    s.truncate(80);
    // only plain characters in the log (no control characters from a request)
    s.chars().map(|c| if c.is_ascii_graphic() { c } else { '?' }).collect()
}

pub fn utc_text(unix: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(unix) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z",
            t.year(),
            t.month() as u8,
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => unix.to_string(),
    }
}

impl AccessLog {
    pub fn new(file: Option<PathBuf>) -> Self {
        AccessLog { file, lock: Mutex::new(()) }
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.file.as_ref()
    }

    pub fn write(&self, ip: IpAddr, device: Option<&str>, method: &str, url: &str, status: u16, note: &str) {
        let Some(file) = &self.file else { return };
        let line = format!(
            "{} {} device={} {} {} {}{}{}\n",
            utc_text(crate::devices::now_unix()),
            ip,
            device.unwrap_or("-"),
            method.chars().filter(|c| c.is_ascii_uppercase()).take(8).collect::<String>(),
            pattern(url),
            status,
            if note.is_empty() { "" } else { " " },
            note
        );
        let _g = self.lock.lock().unwrap();
        if fs::metadata(file).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
            let _ = fs::rename(file, file.with_file_name("remote-access.1.log"));
        }
        if let Some(d) = file.parent() {
            let _ = fs::create_dir_all(d);
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        if let Ok(mut f) = opts.open(file) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Last `n` lines (for the settings page).
    pub fn tail(&self, n: usize) -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let Some(file) = &self.file else { return Vec::new() };
        let Ok(mut f) = fs::File::open(file) else { return Vec::new() };
        // only the end of the file (the page asks every two seconds; the log grows to 2 MB)
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let from = len.saturating_sub(n as u64 * 512);
        let mut bytes = Vec::new();
        if f.seek(SeekFrom::Start(from)).is_err() || f.take(len - from).read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<&str> = text.lines().collect();
        if from > 0 && !lines.is_empty() {
            // the first line is cut
            lines.remove(0);
        }
        lines[lines.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_hide_ids_and_queries() {
        assert_eq!(pattern("/api/image/123/thumb?token=abc"), "/api/image/:id/thumb");
        assert_eq!(pattern("/api/images?q=secret"), "/api/images");
        assert_eq!(pattern("/x\u{1b}[31m"), "/*");
        assert_eq!(pattern("/api/image/../../etc/passwd"), "/api/image/*/*/etc/passwd");
    }

    #[test]
    fn tail_reads_only_the_end() {
        let dir = std::env::temp_dir().join(format!("graineo-remote-log-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let file = dir.join("a.log");
        let text: String = (0..20_000).map(|i| format!("line {i} {}\n", "x".repeat(40))).collect();
        fs::write(&file, text).unwrap();
        let log = AccessLog::new(Some(file.clone()));
        let t = log.tail(3);
        assert_eq!(t.len(), 3);
        assert!(t[0].starts_with("line 19997 ") && t[2].starts_with("line 19999 "), "{t:?}");
        fs::write(&file, "one\ntwo\n").unwrap();
        assert_eq!(log.tail(5), vec!["one".to_string(), "two".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }
}
