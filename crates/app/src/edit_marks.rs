//! The mark "edited" for photos whose settings were not read in this session (filmstrip and grid of Develop).
//!
//! An entry of the list reads its settings only when it is needed (opened, selected, computed ahead); until then the
//! mark comes from the library: one background read of all stored own develop settings, checked the same way as for a
//! loaded photo ([`crate::binning_ui::edited`]). The read is repeated when the list is rebuilt, at most every
//! [`EVERY`] (the table also holds the develop data taken over from other programs: no read per rating change). The
//! first read goes through the table in blocks (one row parsed at a time); later reads fetch only the rows changed
//! since the last one and drop the ones that are gone.
//! Photos edited in this session that leave the list are noted here ([`note`]), so they keep their mark when they
//! come back as entries not loaded. Photos outside the library and Sigma photos that only carry the maker's settings
//! in the file show the mark once they are loaded, as before.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `file` → (`copy`, film simulation active) of every edited photo and copy (`""` = the original).
type Marks = HashMap<i64, Vec<(String, bool)>>;

/// Shortest time between two reads of the library.
const EVERY: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Default)]
struct State {
    marks: Arc<Marks>,
    /// state of photos that were loaded in this session when they left the list (`None`: not edited); goes before
    /// `marks`
    noted: HashMap<(i64, String), Option<bool>>,
    running: bool,
    last: Option<std::time::Instant>,
    /// edited photos and copies read from the library: `(file, copy)` → film simulation active
    edited: HashMap<(i64, String), bool>,
    /// newest `settings_time` read (`None`: nothing read yet, the next read is a full one)
    since: Option<i64>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(State::default))
}

/// The mark of one stored settings text: `Some(film)` for edited settings, `None` for neutral or unreadable ones.
fn mark_of(json: &str) -> Option<bool> {
    let s = serde_json::from_str::<develop::Settings>(json).ok()?;
    crate::binning_ui::edited(&s).then(|| s.film.is_active())
}

/// Take a scan of the library into `edited`: changed rows replace their entry, keys that are gone leave it.
fn apply(edited: &mut HashMap<(i64, String), bool>, changed: Vec<((i64, String), Option<bool>)>, keys: &std::collections::HashSet<(i64, String)>) {
    edited.retain(|k, _| keys.contains(k));
    for (k, m) in changed {
        match m {
            Some(film) => {
                edited.insert(k, film);
            }
            None => {
                edited.remove(&k);
            }
        }
    }
}

/// The marks of the edited photos.
fn marks_of(edited: &HashMap<(i64, String), bool>) -> Marks {
    let mut m = Marks::new();
    for ((file, copy), film) in edited {
        m.entry(*file).or_default().push((copy.clone(), *film));
    }
    m
}

/// Read the marks in the background: the first time, then at most every [`EVERY`] (one read at a time).
pub fn refresh(ctx: &egui::Context) {
    let start = with(|s| {
        if s.running || s.last.is_some_and(|t| t.elapsed() < EVERY) {
            return false;
        }
        s.running = true;
        s.last = Some(std::time::Instant::now());
        true
    });
    if !start {
        return;
    }
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new().name("edit-marks".into()).spawn(move || {
        library::platform::background_priority();
        let (mut edited, since) = with(|s| (std::mem::take(&mut s.edited), s.since));
        let mut changed: Vec<((i64, String), Option<bool>)> = Vec::new();
        let scan = library::store::develop_scan(since, |file, copy, json| changed.push(((file, copy.to_string()), json.and_then(mark_of))));
        match scan {
            Some(scan) => {
                apply(&mut edited, changed, &scan.keys);
                let marks = Arc::new(marks_of(&edited));
                with(|s| {
                    s.marks = marks;
                    s.edited = edited;
                    // the overlay of changes not yet written is no database time: it is read again until written
                    s.since = scan.newest.or(since);
                    s.running = false;
                });
            }
            // no library: keep what we had
            None => with(|s| {
                s.edited = edited;
                s.running = false;
            }),
        }
        ctx.request_repaint();
    });
    if spawned.is_err() {
        with(|s| s.running = false);
    }
}

/// A photo loaded in this session leaves the list: its state as of now (`Some(film)`: edited).
pub fn note(file: i64, copy: Option<&str>, edited: Option<bool>) {
    with(|s| s.noted.insert((file, copy.unwrap_or("").to_string()), edited));
}

/// Is the photo (`copy`: a virtual copy) edited according to the library? `Some(film)`: yes, with the film simulation
/// on or off.
pub fn get(file: i64, copy: Option<&str>) -> Option<bool> {
    let marks = match with(|s| if s.noted.is_empty() { Err(s.marks.clone()) } else { s.noted.get(&(file, copy.unwrap_or("").to_string())).copied().ok_or_else(|| s.marks.clone()) }) {
        Ok(noted) => return noted,
        Err(m) => m,
    };
    marks.get(&file)?.iter().find(|(c, _)| c == copy.unwrap_or("")).map(|(_, film)| *film)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stored settings equal to the defaults (a photo reset to neutral) and unreadable ones carry no mark.
    #[test]
    fn only_edited_settings_mark() {
        let edited = serde_json::to_string(&develop::Settings { exposure: 0.5, ..Default::default() }).unwrap();
        let neutral = serde_json::to_string(&develop::Settings::default()).unwrap();
        assert_eq!(mark_of(&edited), Some(false));
        assert_eq!(mark_of(&neutral), None);
        assert_eq!(mark_of("{"), None);
    }

    /// A later read: a new edit adds the mark, a reset (neutral settings or row gone) takes it away.
    #[test]
    fn incremental_reads_add_and_remove_marks() {
        let k = |f: i64| (f, String::new());
        let mut e = HashMap::new();
        apply(&mut e, vec![(k(1), Some(false)), (k(2), Some(true)), ((2, "A".into()), Some(false))], &[k(1), k(2), (2, "A".into())].into_iter().collect());
        assert_eq!(marks_of(&e).len(), 2);
        // 1 reset to neutral (row stays, changed), 2 and its copy unchanged, 3 newly edited
        apply(&mut e, vec![(k(1), None), (k(3), Some(false))], &[k(1), k(2), (2, "A".into()), k(3)].into_iter().collect());
        assert!(!e.contains_key(&k(1)) && e[&k(2)] && e.contains_key(&k(3)));
        // copy "A" gone from the table (reset, row dropped): not in the key list, no changed row
        apply(&mut e, vec![], &[k(2), k(3)].into_iter().collect());
        assert!(!e.contains_key(&(2, "A".to_string())) && e.len() == 2);
    }
}
