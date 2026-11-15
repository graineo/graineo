//! Journal of an import (`<data>/import-journal/<id>.jsonl`): one line when the run starts (settings and the planned
//! files), one line per finished or failed file, one line at the end. After a crash or quit during an import the
//! journal has no end line; the app then offers to import the rest ([`unfinished`]).
//!
//! Every finished copy is complete on its own (it appears under its name only after the verified read-back), so the
//! journal only has to say which files are still missing. Lines are appended and flushed; a torn last line (crash
//! while writing) is ignored when reading.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

/// Finished journals kept (the newest ones; older ones are removed).
const KEEP_FINISHED: usize = 20;

pub fn folder() -> PathBuf {
    crate::data_dir().join("import-journal")
}

/// One running import.
pub struct Journal {
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

/// A planned file of the run (enough to import it again later).
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub path: PathBuf,
    pub size: u64,
    pub format: String,
    pub tag: String,
}

impl Journal {
    /// Start a journal in `dir` (normally [`folder`]). `settings`: the options of the run (JSON, given back by
    /// [`unfinished`]). A journal that cannot be written does not stop the import (the copies are safe on their own).
    pub fn start_in(dir: &Path, settings: Value, planned: &[Planned]) -> Journal {
        let _ = std::fs::create_dir_all(dir);
        let id = format!("{}-{}", crate::now(), std::process::id());
        let path = dir.join(format!("{id}.jsonl"));
        let file = std::fs::OpenOptions::new().create_new(true).append(true).open(&path).ok();
        let j = Journal { path, file: Mutex::new(file) };
        let files: Vec<Value> = planned.iter().map(|p| json!([p.path.to_string_lossy(), p.size, p.format, p.tag])).collect();
        j.line(json!({"kind": "start", "time": crate::now(), "settings": settings, "files": files}));
        j
    }

    pub fn start(settings: Value, planned: &[Planned]) -> Journal {
        Self::start_in(&folder(), settings, planned)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn line(&self, v: Value) {
        let mut g = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = g.as_mut() {
            let mut s = v.to_string();
            s.push('\n');
            if f.write_all(s.as_bytes()).and_then(|_| f.flush()).is_err() {
                *g = None;
            }
        }
    }

    /// A source file is copied and verified (`target`: the copy).
    pub fn done(&self, source: &Path, target: &Path) {
        self.line(json!({"kind": "done", "src": source.to_string_lossy(), "dst": target.to_string_lossy()}));
    }

    pub fn failed(&self, source: &Path, why: &str) {
        self.line(json!({"kind": "error", "src": source.to_string_lossy(), "why": why}));
    }

    /// The run is over (finished or cancelled by the user): it is no longer offered for resuming.
    pub fn finish(&self, cancelled: bool) {
        self.line(json!({"kind": "end", "time": crate::now(), "cancelled": cancelled}));
        *self.file.lock().unwrap_or_else(|e| e.into_inner()) = None;
        tidy(self.path.parent().unwrap_or(Path::new(".")));
    }
}

/// An interrupted import (journal without end line).
#[derive(Clone, Debug)]
pub struct Unfinished {
    pub path: PathBuf,
    pub started: i64,
    pub settings: Value,
    pub planned: Vec<Planned>,
    pub done: HashSet<PathBuf>,
}

impl Unfinished {
    /// Planned files that are not finished yet (and still exist).
    pub fn rest(&self) -> Vec<Planned> {
        self.planned.iter().filter(|p| !self.done.contains(&p.path)).cloned().collect()
    }
}

fn read(path: &Path) -> Option<(Unfinished, bool)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok());
    let first = lines.next()?;
    if first["kind"] != "start" {
        return None;
    }
    let planned = first["files"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|e| Some(Planned { path: PathBuf::from(e[0].as_str()?), size: e[1].as_u64()?, format: e[2].as_str()?.to_string(), tag: e[3].as_str()?.to_string() }))
                .collect()
        })
        .unwrap_or_default();
    let mut u = Unfinished { path: path.to_path_buf(), started: first["time"].as_i64().unwrap_or(0), settings: first["settings"].clone(), planned, done: HashSet::new() };
    let mut ended = false;
    for l in lines {
        match l["kind"].as_str() {
            Some("done") => {
                if let Some(s) = l["src"].as_str() {
                    u.done.insert(PathBuf::from(s));
                }
            }
            Some("end") => ended = true,
            _ => {}
        }
    }
    Some((u, ended))
}

/// Interrupted imports in `dir`, newest first (only those with something left to do).
pub fn unfinished_in(dir: &Path) -> Vec<Unfinished> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let own = format!("-{}.jsonl", std::process::id());
    let mut v: Vec<Unfinished> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        // a journal of this process belongs to a run that is still going
        .filter(|p| !p.to_string_lossy().ends_with(&own))
        .filter_map(|p| read(&p))
        .filter(|(u, ended)| !ended && u.planned.len() > u.done.len())
        .map(|(u, _)| u)
        .collect();
    v.sort_by(|a, b| b.started.cmp(&a.started));
    v
}

pub fn unfinished() -> Vec<Unfinished> {
    unfinished_in(&folder())
}

/// Do not offer an interrupted import again (the user declined, or its rest was started as a new run).
pub fn dismiss(u: &Unfinished) {
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&u.path) {
        // a leading line break: the last line may be torn (crash while writing)
        let _ = write!(f, "\n{}\n", json!({"kind": "end", "time": crate::now(), "dismissed": true}));
    }
}

/// Keep only the newest finished journals (own files of the app).
fn tidy(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut finished: Vec<(i64, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| read(&p).filter(|(_, ended)| *ended).map(|(u, _)| (u.started, p)))
        .collect();
    finished.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in finished.into_iter().skip(KEEP_FINISHED) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_run_offers_the_rest() {
        let dir = std::env::temp_dir().join(format!("graineo-import-journal-{}-{}", std::process::id(), crate::now()));
        let planned: Vec<Planned> = ["a.JPG", "b.JPG", "c.JPG"].iter().map(|n| Planned { path: PathBuf::from(format!("/karte/{n}")), size: 3, format: "JPG".into(), tag: "2026-05-02".into() }).collect();
        let j = Journal::start_in(&dir, json!({"mode": "copy"}), &planned);
        j.done(&planned[0].path, Path::new("/ziel/a.JPG"));
        j.failed(&planned[1].path, "Lesefehler");
        // a journal of this process counts as running: rename it as if another (crashed) process had written it
        let crashed = dir.join("1-1.jsonl");
        std::fs::rename(j.path(), &crashed).unwrap();
        // torn last line from a crash
        std::fs::OpenOptions::new().append(true).open(&crashed).unwrap().write_all(b"{\"kind\":\"do").unwrap();
        let u = unfinished_in(&dir);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].settings["mode"], "copy");
        assert_eq!(u[0].rest().iter().map(|p| p.path.clone()).collect::<Vec<_>>(), vec![planned[1].path.clone(), planned[2].path.clone()]);
        dismiss(&u[0]);
        assert!(unfinished_in(&dir).is_empty());
        // a finished run is never offered
        let j = Journal::start_in(&dir, json!({}), &planned);
        j.finish(false);
        assert!(unfinished_in(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
