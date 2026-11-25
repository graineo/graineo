//! Renew the stored developed previews of photos whose **automatic lens profile** changed: after an update of the
//! matching ([`raw::lens::MATCHING`]) or of the lensfun data (built-in with a new version, or "Objektivprofile
//! aktualisieren" in the settings), [`raw::lens::matching_stamp`] differs from the one recorded.
//!
//! Per non-Sigma RAW the name of the automatically chosen lensfun lens is recorded in `lens-profiles/matching.json`
//! (`{"stamp": …, "choices": {"<file id>": "<lens>"}}`). When the stamp changes, the choice is computed again from the
//! library's camera and lens names (no file is opened); a photo whose choice differs and that has a stored developed
//! preview is developed again with the quiet machinery of [`crate::sigma_refresh`]. Photos with a fixed choice (own
//! assignment or a rule) and photos with the lens correction switched off keep their previews. The first run only
//! records (there is nothing to compare with).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::sigma_refresh::{self, Candidate, Group, Stage};

#[derive(Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    stamp: String,
    #[serde(default)]
    choices: BTreeMap<i64, String>,
}

fn record_path() -> Option<PathBuf> {
    lensprof::store::root().map(|d| d.join("matching.json"))
}

fn read_record() -> Record {
    record_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_record(r: &Record) {
    let Some(p) = record_path() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(t) = serde_json::to_string(r) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

static RUNNING: AtomicBool = AtomicBool::new(false);
/// stamp handled last in this run of the app
static SEEN: Mutex<String> = Mutex::new(String::new());

/// Photos whose choice changed between two records: (file, old, new) for files in both with a different name.
pub fn changed(old: &BTreeMap<i64, String>, now: &BTreeMap<i64, String>) -> HashSet<i64> {
    now.iter().filter(|(f, n)| old.get(f).is_some_and(|o| o != *n)).map(|(f, _)| *f).collect()
}

/// Every frame from the app (after the other renewals have started): checks the stamp once per change.
pub fn maybe_start(lib: &crate::library_ui::LibraryView) {
    if cfg!(test) || RUNNING.load(Ordering::Relaxed) || sigma_refresh::progress().is_some() {
        return;
    }
    // the lensfun data loads in the background; until then nothing to compare
    let Some(stamp) = raw::lens::matching_stamp() else { return };
    if *SEEN.lock().unwrap_or_else(|e| e.into_inner()) == stamp {
        return;
    }
    let Some((cache, cands)) = lib.lens_matching_candidates() else { return };
    *SEEN.lock().unwrap_or_else(|e| e.into_inner()) = stamp.clone();
    RUNNING.store(true, Ordering::Relaxed);
    let started = std::thread::Builder::new().name("lens-matching-refresh".into()).spawn(move || {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                RUNNING.store(false, Ordering::Relaxed);
            }
        }
        let _done = Done;
        library::platform::background_priority();
        let old = read_record();
        if old.stamp == stamp {
            return;
        }
        let lens_of: std::collections::HashMap<i64, Option<String>> = cands.iter().map(|(c, l)| (c.file, l.clone())).collect();
        let now: BTreeMap<i64, String> = cands.iter().map(|(c, lens)| (c.file, raw::lens::auto_lensfun_name(&c.camera, lens.as_deref()))).collect();
        let changed = if old.stamp.is_empty() { HashSet::new() } else { changed(&old.choices, &now) };
        let list: Vec<Candidate> = cands.into_iter().map(|(c, _)| c).filter(|c| changed.contains(&c.file)).collect();
        let mut ok = true;
        if !list.is_empty() {
            pools::set_intent(pools::Intent::Idle);
            let stage = Stage { name: "lens matching", marker: "", revision: 0, done_name: "lens-matching-refresh", selects: |g| g == Group::CameraLook };
            let affected = |c: &Candidate| {
                let (s, _) = crate::sidecar::load(&c.path);
                s.raw.lens && s.raw.lens_profile.is_none() && lensprof::assign::rule_for("", &c.camera, lens_of.get(&c.file).cloned().flatten().as_deref()).is_none()
            };
            let r = sigma_refresh::run_stage(&stage, &cache, &list, &affected, true);
            crate::log(&format!("lens matching previews ({} changed): {r:?}", changed.len()));
            ok = r.unreachable == 0;
            let _ = std::fs::remove_file(library::preview::developed_folder().join(format!("{}-0.txt", stage.done_name)));
        }
        // unreachable photos keep the old record and are tried at the next start
        let choices = if ok { now } else { old.choices.into_iter().filter(|(f, _)| changed.contains(f)).chain(now.into_iter().filter(|(f, _)| !changed.contains(f))).collect() };
        write_record(&Record { stamp: if ok { stamp } else { old.stamp }, choices });
    });
    if started.is_err() {
        RUNNING.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_photos_whose_choice_differs() {
        let old: BTreeMap<i64, String> = [(1, "A".into()), (2, "B".into()), (3, String::new())].into();
        let now: BTreeMap<i64, String> = [(1, "A".into()), (2, "C".into()), (3, "D".into()), (4, "E".into())].into();
        let c = changed(&old, &now);
        assert_eq!(c, [2, 3].into_iter().collect());
    }
}
