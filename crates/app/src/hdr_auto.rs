//! Automatic tone of a merged photo with linear output ("Stärke" under AUSGABE in the tool "Zusammenfügen").
//!
//! The linear merge keeps the highlights above white; the automatic sets the ordinary develop sliders of the merged
//! photo (exposure, highlights, shadows, whites, blacks; [`rawdev::doc::hdr_auto_values`]) scaled by the strength. The
//! values stand in the edit panel afterwards and can be changed freely.
//!
//! When it tunes again:
//! - the first time the merged photo is shown with its pixels (new merge, batch result opened later);
//! - after a new frame list or another reference, once the new pixels are in place – but only while the five sliders
//!   still hold what the automatic set last ([`hdr::AutoTone::applied`]); a slider moved by hand keeps its value;
//! - when the strength slider is moved: an explicit request, it tunes again even after hand changes (⌘Z brings them
//!   back). This keeps the tool to one slider without a lock or an extra button;
//! - when the output goes back from "Fusion" to "Linear", by the same rule (untouched sliders only).
//!
//! The fusion compresses the range itself: on the change of the output to "Fusion" the values the automatic set are
//! taken back to neutral (again only while untouched), so the two do not add up.
//!
//! The strength and the values set last live in the recipe; they do not change the pixels (not in the cache key), so
//! nothing is merged again.
//!
//! Every change the automatic makes is one step of the develop history. The step names what the recipe held before
//! and after it (strength, output; [`crate::history::Hint::merge_tone`]); after ⌘Z, ⇧⌘Z or a jump in the history the
//! recipe follows the step under the cursor, so sliders, strength and output go back together.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A request to tune again.
struct Want {
    file: i64,
    /// moved strength slider: tune even over hand changes
    force: bool,
    /// the output was switched (linear ↔ fusion)
    output: bool,
    /// cache key of the result shown when asked: wait until another result is in place (new frames, reference)
    wait_for_other_than: Option<String>,
    since: Instant,
}

static WANT: Mutex<Option<Want>> = Mutex::new(None);
/// Recipe settings to store once the mouse button is up (a slider being dragged stores once).
static SAVE: Mutex<Option<(i64, hdr::Settings)>> = Mutex::new(None);
/// Newest automatic tone per merged photo in this session (the open document keeps the recipe it was opened with).
static KNOWN: Mutex<Option<HashMap<i64, Option<hdr::AutoTone>>>> = Mutex::new(None);

/// How long a request waits for the new result before it is dropped.
const WAIT: Duration = Duration::from_secs(120);

/// Ask to tune the merged photo `file` again: `force` for the strength slider; `shown_key` (the result shown now) for
/// a change that computes a new result first.
pub(crate) fn want(file: i64, force: bool, shown_key: Option<String>) {
    *WANT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Want { file, force, output: false, wait_for_other_than: shown_key, since: Instant::now() });
}

/// The output of the merged photo `file` was switched: once the new result is in place (`shown_key`: the one shown
/// now) the automatic values are taken back (fusion) or tuned again (linear), while untouched.
pub(crate) fn want_output(file: i64, shown_key: Option<String>) {
    *WANT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Want { file, force: false, output: true, wait_for_other_than: shown_key, since: Instant::now() });
}

/// ⌘Z, ⇧⌘Z or a jump in the history of the open photo: a merged photo's recipe follows the step (next frame).
/// `from`: the step the cursor stood on before (`None`: history not in memory, nothing to follow).
pub(crate) fn history_moved(from: Option<usize>) {
    *FOLLOW.lock().unwrap_or_else(|e| e.into_inner()) = from;
}

static FOLLOW: Mutex<Option<usize>> = Mutex::new(None);
/// Strength of the automatic tone per merged photo as of the last finished change in this session (what a step of
/// the history names as "before").
static COMMITTED: Mutex<Option<HashMap<i64, f32>>> = Mutex::new(None);
/// Name hint of the step being made while a mouse button is down (a slider held still must not let it expire).
static HOLD: Mutex<Option<(usize, crate::history::Hint)>> = Mutex::new(None);

fn committed(file: i64) -> Option<f32> {
    COMMITTED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&file).copied())
}

fn commit(file: i64, strength: f32) {
    COMMITTED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(file, strength);
}

/// Forget what this session knows about the merged photo `file` (it is new, or gone: a file id may be given again).
pub(crate) fn forget(file: i64) {
    WANT.lock().unwrap_or_else(|e| e.into_inner()).take_if(|w| w.file == file);
    SAVE.lock().unwrap_or_else(|e| e.into_inner()).take_if(|s| s.0 == file);
    if let Some(m) = KNOWN.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(&file);
    }
    if let Some(m) = COMMITTED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(&file);
    }
}

/// The newest automatic tone of `file` known in this session.
pub(crate) fn known(file: i64) -> Option<Option<hdr::AutoTone>> {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&file).copied())
}

pub(crate) fn remember(file: i64, auto: Option<hdr::AutoTone>) {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(file, auto);
}

/// The five sliders the automatic sets: exposure, highlights, shadows, whites, blacks.
pub(crate) fn tone_of(s: &crate::Settings) -> [f32; 5] {
    [s.exposure as f32, s.highlight as f32, s.shadow as f32, s.raw.whites, s.raw.blacks]
}

pub(crate) fn set_tone(s: &mut crate::Settings, v: [f32; 5]) {
    s.exposure = v[0] as f64;
    s.highlight = v[1] as f64;
    s.shadow = v[2] as f64;
    s.raw.whites = v[3];
    s.raw.blacks = v[4];
}

/// A merged photo that is not shown got its first result (a batch computes its photos unseen): its automatic tone is
/// set now, as it would be when the photo is opened – so its tile and an export from the grid show what Develop will
/// show, and opening it later changes nothing. Only for a linear merge with the automatic on and not applied yet,
/// without a development of its own, and not while the photo is being worked on in this session (then the tool
/// tunes it). Returns the develop settings set. Job thread.
pub(crate) fn tune_unseen(file: i64) -> Option<crate::Settings> {
    if crate::hdr_ui::wanted(file).is_some() {
        return None;
    }
    let db = library::db::Db::standard().ok()?;
    let path = library::merge::file_path(&db.conn, file)?;
    let recipe = library::merge::recipe(&db.conn, file)?;
    let settings = rawdev::merged::parse_settings(&recipe.settings).ok()?;
    let auto = settings.auto_tone.filter(|a| a.applied.is_none() && a.strength > 0.0)?;
    if settings.output != hdr::fusion::Output::Linear || library::store::develop_at(&path, None, None).ok().flatten().is_some() {
        return None;
    }
    let doc = rawdev::RawDoc::open(&path, &|| true).ok()??;
    if doc.stands_in() {
        return None;
    }
    let (mut s, _, _) = crate::raw_app::load_settings(&path, None);
    let v = doc.auto_tone_hdr(&s, auto.strength)?;
    set_tone(&mut s, v);
    let s = crate::Settings { view: develop::View::Developed, ..s };
    // the recipe as it is stored now gets the values set (nothing else of it is touched; no pixels change)
    let tx = db.begin_write().ok()?;
    let now = library::merge::recipe(&tx, file)?;
    let mut stored = rawdev::merged::parse_settings(&now.settings).ok()?;
    if stored.auto_tone != Some(auto) || stored.output != hdr::fusion::Output::Linear || crate::hdr_ui::wanted(file).is_some() {
        return None;
    }
    stored.auto_tone = Some(hdr::AutoTone { applied: Some(v), ..auto });
    library::merge::set_settings(&db, file, &serde_json::to_string(&stored).ok()?).ok()?;
    tx.commit().ok()?;
    library::store::set_develop(file, None, Some(serde_json::to_string(&s).ok()?));
    crate::hdr_paint::recipe_changed();
    // (the app may hold the photo's settings in memory already – the list of a folder, the photo at hand)
    crate::hdr_ui::push(crate::hdr_ui::Event::Tuned { file, tone: v });
    crate::log(&format!("hdr: automatic tone {:.0} % → {v:?} (not shown)", auto.strength * 100.0));
    Some(s)
}

/// Do these develop settings of a merged photo hold work of the user? The values the automatic tone set (`applied`)
/// and nobody moved since do not count: they come back when the photo is merged again.
pub(crate) fn edited_by_hand(s: &crate::Settings, applied: Option<[f32; 5]>) -> bool {
    let mut s = s.clone();
    if applied.is_some() && !moved_by_hand(applied, tone_of(&s)) {
        set_tone(&mut s, tone_of(&crate::Settings::default()));
    }
    crate::binning_ui::edited(&s)
}

/// Were the sliders moved by hand since the automatic set `applied`?
pub(crate) fn moved_by_hand(applied: Option<[f32; 5]>, now: [f32; 5]) -> bool {
    applied.is_some_and(|a| a.iter().zip(&now).any(|(x, y)| (x - y).abs() > 1e-3))
}

/// What to do with the sliders now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Decision {
    Nothing,
    /// tune (and remember the values)
    Tune,
}

/// The rule: tune the first time, on request while untouched, always when forced.
pub(crate) fn decide(auto: Option<hdr::AutoTone>, asked: Option<bool>, now: [f32; 5]) -> Decision {
    let Some(a) = auto else { return Decision::Nothing };
    match (a.applied, asked) {
        (_, Some(true)) => Decision::Tune,
        (None, _) => Decision::Tune,
        (Some(_), Some(false)) if !moved_by_hand(a.applied, now) => Decision::Tune,
        _ => Decision::Nothing,
    }
}

/// The switch to "Fusion": are the values the automatic set (`applied`) taken back? Only values it did set and that
/// were not moved by hand since.
pub(crate) fn take_back(auto: Option<hdr::AutoTone>, now: [f32; 5]) -> bool {
    auto.is_some_and(|a| a.applied.is_some() && !moved_by_hand(a.applied, now))
}

impl crate::App {
    /// Each frame: tune the open merged photo when it is due (see the module docs) and store the recipe once the mouse
    /// button is up.
    pub(crate) fn merge_auto_step(&mut self, ctx: &egui::Context) {
        let down = ctx.input(|i| i.pointer.any_down());
        if !ctx.input(|i| i.pointer.primary_down()) {
            if let Some((file, settings)) = SAVE.lock().unwrap_or_else(|e| e.into_inner()).take() {
                if let Some(a) = settings.auto_tone {
                    commit(file, a.strength);
                }
                self.lib.store_merge_auto(file, settings.auto_tone);
            }
        }
        // the name of the step being made stays fresh until the history has recognised it (mouse button up)
        let held = {
            let mut g = HOLD.lock().unwrap_or_else(|e| e.into_inner());
            if down { g.clone() } else { g.take() }
        };
        if let Some((i, h)) = held {
            let pending = down
                || self.entries.get(i).is_some_and(|e| self.committed.as_ref().is_some_and(|(ci, s)| *ci == i && *s != crate::Settings { view: develop::View::Developed, ..e.settings.clone() }));
            if pending {
                crate::history::hint(i, h);
            }
        }
        let follow = FOLLOW.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some((index, doc)) = self.shown_merged() else { return };
        let Some(m) = doc.merged.as_ref() else { return };
        if index >= self.entries.len() {
            return;
        }
        let file = m.source.file;
        if let Some(from) = follow {
            self.merge_follow_history(ctx, index, file, &m.source.settings, from);
            return;
        }
        if m.pending {
            return;
        }
        // the result shown is the one wanted (a new one is being computed: wait for it)
        if crate::hdr_ui::wanted(file).is_some_and(|w| w.key != m.source.key) {
            return;
        }
        // asked: (strength slider, output switched)
        let asked = {
            let mut g = WANT.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_ref() {
                Some(w) if w.file == file => {
                    if w.wait_for_other_than.as_ref().is_some_and(|k| *k == m.source.key) && w.since.elapsed() < WAIT {
                        return;
                    }
                    g.take().map(|w| (w.force, w.output))
                }
                Some(w) if w.since.elapsed() > WAIT => {
                    *g = None;
                    None
                }
                _ => None,
            }
        };
        let mut settings = crate::hdr_paint::settings_of(ctx, file).unwrap_or_else(|| m.source.settings.clone());
        if let Some(a) = known(file) {
            settings.auto_tone = a;
        }
        let fusion = settings.output != hdr::fusion::Output::Linear;
        let switched = asked.is_some_and(|a| a.1);
        let now = tone_of(&self.entries[index].settings);
        let Some(mut auto) = settings.auto_tone else { return };
        let v = if fusion {
            // only the switch to fusion takes the automatic values back (a photo opened with fusion stays as it is)
            if !switched || !take_back(settings.auto_tone, now) {
                return;
            }
            [0.0; 5]
        } else {
            if decide(settings.auto_tone, asked.map(|a| a.0), now) != Decision::Tune {
                return;
            }
            let Some(v) = doc.auto_tone_hdr(&self.entries[index].settings, auto.strength) else { return };
            v
        };
        if v != now {
            let before = committed(file).or(m.source.settings.auto_tone.map(|a| a.strength)).unwrap_or(auto.strength);
            let h = crate::history::Hint::merge_tone([before, auto.strength], [fusion != switched, fusion]);
            crate::history::hint(index, h.clone());
            *HOLD.lock().unwrap_or_else(|e| e.into_inner()) = down.then_some((index, h));
            set_tone(&mut self.entries[index].settings, v);
        }
        auto.applied = Some(v);
        settings.auto_tone = Some(auto);
        remember(file, settings.auto_tone);
        crate::hdr_paint::adopt_auto(ctx, file, settings.auto_tone);
        if fusion {
            crate::log(&format!("hdr: automatic tone taken back for the fusion (was {now:?})"));
        } else {
            crate::log(&format!("hdr: automatic tone {:.0} % → {v:?}", auto.strength * 100.0));
        }
        *SAVE.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, settings));
    }

    /// After ⌘Z, ⇧⌘Z or a jump in the history: strength of the automatic tone and output of the recipe as of the step
    /// under the cursor ([`crate::history::History::merge_recipe_at_cursor`]), when the move from step `from` crossed a
    /// step of the automatic; the sliders of that state count as the values the automatic set. Another output is
    /// merged again (from the cache when it was computed before).
    fn merge_follow_history(&mut self, ctx: &egui::Context, index: usize, file: i64, opened_with: &hdr::Settings, from: usize) {
        let key = crate::history::Key { path: self.entries[index].path.clone(), copy: self.entries[index].copy.clone() };
        let Some((strength, fusion, state)) = self.history.merge_recipe_at_cursor(&key, from) else { return };
        let mut settings = crate::hdr_paint::settings_of(ctx, file).unwrap_or_else(|| opened_with.clone());
        if let Some(a) = known(file) {
            settings.auto_tone = a;
        }
        // recipes made before the automatic existed have none
        if settings.auto_tone.is_none() {
            return;
        }
        let output = if fusion { hdr::fusion::Output::Fusion } else { hdr::fusion::Output::Linear };
        let auto = Some(hdr::AutoTone { strength, applied: Some(tone_of(&state)) });
        if settings.output == output && settings.auto_tone == auto {
            return;
        }
        // a tuning still asked for would change the state the step just brought back
        WANT.lock().unwrap_or_else(|e| e.into_inner()).take_if(|w| w.file == file);
        *HOLD.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let switch = settings.output != output;
        settings.output = output;
        settings.auto_tone = auto;
        remember(file, auto);
        commit(file, strength);
        crate::log(&format!("hdr: history step → strength {:.0} %, output {}", strength * 100.0, if fusion { "fusion" } else { "linear" }));
        if switch {
            SAVE.lock().unwrap_or_else(|e| e.into_inner()).take_if(|(f, _)| *f == file);
            self.merge_restore_recipe(ctx, output, auto);
        } else {
            crate::hdr_paint::adopt_auto(ctx, file, auto);
            *SAVE.lock().unwrap_or_else(|e| e.into_inner()) = Some((file, settings));
        }
    }
}

/// Test: a new session (nothing known, asked or waiting to be stored).
#[cfg(test)]
pub(crate) fn test_new_session() {
    *WANT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SAVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *FOLLOW.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *COMMITTED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *HOLD.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Test: is a tuning asked for or a recipe waiting to be stored?
#[cfg(test)]
pub(crate) fn test_waiting() -> bool {
    SAVE.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(applied: Option<[f32; 5]>) -> Option<hdr::AutoTone> {
        Some(hdr::AutoTone { strength: 0.5, applied })
    }

    /// First time: tuned. Again on a new frame list while untouched; not after a hand change; always for the strength
    /// slider. Old recipes (no automatic) never.
    #[test]
    fn the_rule() {
        let set = [0.2, -0.4, 0.3, -0.1, -0.05];
        let mut moved = set;
        moved[1] = -0.6;
        assert_eq!(decide(auto(None), None, [0.0; 5]), Decision::Tune);
        assert_eq!(decide(auto(Some(set)), None, set), Decision::Nothing, "nothing asked");
        assert_eq!(decide(auto(Some(set)), Some(false), set), Decision::Tune);
        assert_eq!(decide(auto(Some(set)), Some(false), moved), Decision::Nothing, "a hand change stays");
        assert_eq!(decide(auto(Some(set)), Some(true), moved), Decision::Tune, "the strength slider tunes again");
        assert_eq!(decide(None, Some(true), [0.0; 5]), Decision::Nothing, "old recipes: off");
        assert!(!moved_by_hand(None, moved));
        // the switch to fusion: only untouched values the automatic did set go back
        assert!(take_back(auto(Some(set)), set));
        assert!(!take_back(auto(Some(set)), moved), "a hand change stays");
        assert!(!take_back(auto(None), [0.0; 5]) && !take_back(None, set));
        // back to linear after the values were taken back to neutral: tuned again while still neutral
        assert_eq!(decide(auto(Some([0.0; 5])), Some(false), [0.0; 5]), Decision::Tune);
        assert_eq!(decide(auto(Some([0.0; 5])), Some(false), moved), Decision::Nothing);
    }
}
