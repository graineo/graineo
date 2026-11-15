//! State of the dialog "Import from Lightroom": catalog, last import, last handover of the edits, and which
//! action is needed next (that one becomes the gold main button).

use std::path::PathBuf;

/// Last finished Lightroom import (table `run`, kind `lightroom`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Import {
    /// Unix seconds of the end of the run
    pub time: i64,
    /// photos in the catalog at that run
    pub photos: usize,
}

/// Last handover of the Lightroom edits (protocol `reports/lr-handover-<time>.txt`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Handover {
    pub time: i64,
    /// photos whose Lightroom edits are in the sidecar (written now or already before)
    pub taken: usize,
    pub cancelled: bool,
    /// sidecar folder not reachable (drive not connected)
    pub unreachable: usize,
}

#[derive(Clone, Debug, Default)]
pub struct LrState {
    pub catalog: Option<PathBuf>,
    pub catalog_found: bool,
    pub import: Option<Import>,
    pub handover: Option<Handover>,
}

/// The action the dialog offers as the gold main button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    ChooseCatalog,
    Import,
    Handover,
    /// everything done: no main button
    Done,
}

impl LrState {
    /// Read once when the dialog opens (one small query, newest protocol from the reports folder).
    pub fn read(db: &library::db::Db) -> Self {
        let catalog = library::lightroom_catalog(db);
        let catalog_found = catalog.as_ref().is_some_and(|k| k.exists());
        let import = db
            .conn
            .query_row("SELECT finished, report FROM run WHERE kind='lightroom' AND finished IS NOT NULL ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
            .ok()
            .map(|(time, report)| {
                let photos = report.and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["images"].as_u64()).unwrap_or(0) as usize;
                Import { time, photos }
            });
        LrState { catalog, catalog_found, import, handover: newest_handover(&library::data_dir().join("reports")) }
    }

    /// The edits still need to be taken over: never done, cancelled, or a later import brought new photos.
    pub fn handover_due(&self) -> bool {
        match (&self.import, &self.handover) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(i), Some(h)) => h.cancelled || h.time < i.time,
        }
    }

    pub fn next(&self) -> Next {
        if !self.catalog_found {
            Next::ChooseCatalog
        } else if self.import.is_none() {
            Next::Import
        } else if self.handover_due() {
            Next::Handover
        } else {
            Next::Done
        }
    }
}

/// Newest handover protocol in `dir` (name `lr-handover-<unix seconds>.txt`, JSON at the top).
/// `duration_ms` of the newest sync protocol in `dir` (`sync-<unix seconds>.json`).
pub fn newest_sync_duration(dir: &std::path::Path) -> Option<std::time::Duration> {
    let path = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let t = n.strip_prefix("sync-")?.strip_suffix(".json")?.parse::<i64>().ok()?;
            Some((t, e.path()))
        })
        .max_by_key(|(t, _)| *t)?
        .1;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some(std::time::Duration::from_millis(v["sync"]["duration_ms"].as_u64()?))
}

pub fn newest_handover(dir: &std::path::Path) -> Option<Handover> {
    let (time, path) = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let t = n.strip_prefix("lr-handover-")?.strip_suffix(".txt")?.parse::<i64>().ok()?;
            Some((t, e.path()))
        })
        .max_by_key(|(t, _)| *t)?;
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::Deserializer::from_str(&text).into_iter().next()?.ok()?;
    let n = |k: &str| v[k].as_u64().unwrap_or(0) as usize;
    Some(Handover { time, taken: n("written") + n("already"), cancelled: v["cancelled"] == true, unreachable: n("unreachable") })
}

/// Local date of Unix seconds ("08.11.2026" / "2026-11-08").
pub fn date(time: i64) -> String {
    let t = library::platform::local_time(time);
    crate::menu::i18n::format::date(crate::menu::i18n::language(), t.year, t.month as u8, t.tag as u8).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(found: bool, import: Option<i64>, handover: Option<(i64, bool)>) -> LrState {
        LrState {
            catalog: Some(PathBuf::from("/x.lrcat")),
            catalog_found: found,
            import: import.map(|time| Import { time, photos: 10 }),
            handover: handover.map(|(time, cancelled)| Handover { time, taken: 8, cancelled, unreachable: 0 }),
        }
    }

    /// The gold button always names the action needed now.
    #[test]
    fn next_action_follows_the_state() {
        assert_eq!(state(false, None, None).next(), Next::ChooseCatalog);
        assert_eq!(state(true, None, None).next(), Next::Import);
        assert_eq!(state(true, Some(100), None).next(), Next::Handover);
        assert_eq!(state(true, Some(100), Some((90, false))).next(), Next::Handover, "import after the handover");
        assert_eq!(state(true, Some(100), Some((110, true))).next(), Next::Handover, "cancelled");
        assert_eq!(state(true, Some(100), Some((110, false))).next(), Next::Done);
    }

    #[test]
    fn newest_protocol_is_read() {
        let d = std::env::temp_dir().join(format!("graineo-lrstate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("lr-handover-100.txt"), "{\"written\": 1}\n").unwrap();
        std::fs::write(d.join("lr-handover-200.txt"), "{\n  \"written\": 5,\n  \"already\": 7,\n  \"cancelled\": true\n}\nFehler: x\n").unwrap();
        std::fs::write(d.join("lightroom-300.json"), "{}").unwrap();
        let h = newest_handover(&d).unwrap();
        assert_eq!(h, Handover { time: 200, taken: 12, cancelled: true, unreachable: 0 });
        let _ = std::fs::remove_dir_all(&d);
    }
}
