//! State of the dialog "Import from Lightroom": catalog, last import, last handover of the edits, and which
//! step is needed next (that one becomes the gold main button, [`LrState::step`]).

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
    /// photos Lightroom marks as edited whose edits are in the library (written now or already before); older reports
    /// lack that count: there all photos with settings (written + already)
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
    Some(Handover { time, taken: if v["edited"].is_u64() { n("edited") } else { n("written") + n("already") }, cancelled: v["cancelled"] == true, unreachable: n("unreachable") })
}

/// Local date of Unix seconds ("08.11.2026" / "2026-11-08").
pub fn date(time: i64) -> String {
    let t = library::platform::local_time(time);
    crate::menu::i18n::format::date(crate::menu::i18n::language(), t.year, t.month as u8, t.tag as u8).unwrap_or_default()
}

/// What the read-only check ("Prüfen") found for a catalog, or why it could not read it.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// running in the background
    Running,
    Done(library::lightroom::Preview),
    Failed(String),
}

/// The main button of the dialog in the one-step flow: choose a catalog → check it (reads only) → import (writes;
/// the edits come along). `Blocked`: the button is shown but disabled, and the dialog says why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// no catalog: choose one (or take the suggestion: then "Prüfen" with it)
    Choose,
    /// a catalog found in the Pictures folder can be checked right away
    CheckSuggestion,
    Check,
    Import,
    /// the edits of an earlier run are still due (cancelled): take them over again
    Handover,
    /// the catalog cannot be read: the import button stays, disabled, with the reason
    Blocked,
    /// a run or the check is going on
    Wait,
    Done,
}

impl LrState {
    /// Next step for the main button. `check`: result of the check of the current catalog in this session;
    /// `failed`: the last run (or check) could not read the catalog; `suggestion`: a catalog found in the Pictures
    /// folder while none is chosen.
    pub fn step(&self, check: Option<&Check>, failed: bool, suggestion: bool, running: bool) -> Step {
        if running || matches!(check, Some(Check::Running)) {
            return Step::Wait;
        }
        if !self.catalog_found {
            return if self.catalog.is_none() && suggestion { Step::CheckSuggestion } else { Step::Choose };
        }
        if failed || matches!(check, Some(Check::Failed(_))) {
            return Step::Blocked;
        }
        match check {
            Some(Check::Done(_)) => Step::Import,
            _ if self.import.is_none() => Step::Check,
            _ if self.handover_due() => Step::Handover,
            _ => Step::Done,
        }
    }
}

/// Plain sentence for a catalog that cannot be read (the technical text goes into the details).
pub fn failure_text(error: &str, catalog: &std::path::Path) -> String {
    let e = error.to_lowercase();
    let name = catalog.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !catalog.exists() {
        if catalog.starts_with("/Volumes") {
            return crate::trf!("„{a}“ ist nicht da. Ist das Laufwerk verbunden?", a = name);
        }
        return crate::trf!("„{a}“ ist nicht da. Wurde der Katalog verschoben oder umbenannt?", a = name);
    }
    if error.contains(library::lightroom::OTHER_CATALOG) {
        return crate::tr("Die Bibliothek wurde aus einem anderen Lightroom-Katalog übernommen. Ein zweiter Katalog würde Fotos vertauschen, deshalb bleibt alles, wie es ist.").to_string();
    }
    if e.contains("not a database") || e.contains("malformed") || e.contains("no such table") || e.contains("file is encrypted") {
        return crate::tr("Diese Datei ist kein Lightroom-Katalog oder ist beschädigt.").to_string();
    }
    if e.contains("unable to open") || e.contains("permission") || e.contains("not authorized") {
        return crate::tr("Der Katalog lässt sich nicht öffnen. Hat graineo Zugriff auf den Ordner?").to_string();
    }
    if e.contains("locked") || e.contains("busy") {
        return crate::tr("Lightroom schreibt gerade in den Katalog. Beende Lightroom und prüfe noch einmal.").to_string();
    }
    crate::tr("Der Katalog lässt sich nicht lesen.").to_string()
}

/// Name of the collection set the catalog's collections go into: the catalog's name without the extension.
pub fn default_set_name(catalog: &std::path::Path) -> String {
    catalog.file_stem().map(|n| n.to_string_lossy().trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| "Lightroom".to_string())
}

/// Folder in which Lightroom keeps its catalogs by default (`~/Pictures/Lightroom`); for test runs
/// `GRAINEO_LR_SEARCH_DIR` (only with `GRAINEO_DATA_DIR`).
pub fn search_dir() -> Option<PathBuf> {
    if let Some(d) = crate::env::var_os("GRAINEO_DATA_DIR").and_then(|_| crate::env::var_os("GRAINEO_LR_SEARCH_DIR")) {
        return Some(PathBuf::from(d));
    }
    Some(sysdirs::home()?.join("Pictures").join("Lightroom"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn state(found: bool, import: Option<i64>, handover: Option<(i64, bool)>) -> LrState {
        LrState {
            catalog: Some(PathBuf::from("/x.lrcat")),
            catalog_found: found,
            import: import.map(|time| Import { time, photos: 10 }),
            handover: handover.map(|(time, cancelled)| Handover { time, taken: 8, cancelled, unreachable: 0 }),
        }
    }

    /// One step after the other: choose (or check the suggestion) → check → import; a catalog that cannot be read
    /// blocks the import button instead of offering it.
    #[test]
    fn main_button_follows_the_check() {
        let none = LrState::default();
        assert_eq!(none.step(None, false, false, false), Step::Choose);
        assert_eq!(none.step(None, false, true, false), Step::CheckSuggestion);
        let missing = state(false, None, None);
        assert_eq!(missing.step(None, false, true, false), Step::Choose, "a chosen catalog that is gone: choose again");
        let fresh = state(true, None, None);
        assert_eq!(fresh.step(None, false, false, false), Step::Check);
        assert_eq!(fresh.step(Some(&Check::Running), false, false, false), Step::Wait);
        assert_eq!(fresh.step(Some(&Check::Done(Default::default())), false, false, false), Step::Import);
        assert_eq!(fresh.step(Some(&Check::Failed("file is not a database".into())), false, false, false), Step::Blocked);
        assert_eq!(fresh.step(None, true, false, false), Step::Blocked, "the last run could not read it");
        assert_eq!(fresh.step(None, false, false, true), Step::Wait);
        assert_eq!(state(true, Some(100), Some((110, false))).step(None, false, false, false), Step::Done);
        assert_eq!(state(true, Some(100), Some((110, true))).step(None, false, false, false), Step::Handover);
        assert_eq!(state(true, Some(100), Some((110, false))).step(Some(&Check::Done(Default::default())), false, false, false), Step::Import, "checked again: import again");
    }

    #[test]
    fn failure_in_plain_words() {
        let _l = crate::menu::i18n::locale::TestLanguage::set(crate::menu::i18n::Language::De);
        let d = std::env::temp_dir().join(format!("graineo-lrfail-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let k = d.join("Kaputt.lrcat");
        std::fs::write(&k, b"x").unwrap();
        assert_eq!(failure_text("Katalog nicht lesbar: file is not a database", &k), "Diese Datei ist kein Lightroom-Katalog oder ist beschädigt.");
        assert!(failure_text(&format!("{}: …", library::lightroom::OTHER_CATALOG), &k).starts_with("Die Bibliothek wurde aus einem anderen Lightroom-Katalog übernommen."));
        assert_eq!(failure_text("x", Path::new("/Volumes/NAS/LR/K.lrcat")), "„K.lrcat“ ist nicht da. Ist das Laufwerk verbunden?");
        assert!(failure_text("x", &d.join("weg.lrcat")).starts_with("„weg.lrcat“ ist nicht da."));
        assert_eq!(default_set_name(Path::new("/a/Lightroom Catalog-v13.lrcat")), "Lightroom Catalog-v13");
        let _ = std::fs::remove_dir_all(&d);
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
