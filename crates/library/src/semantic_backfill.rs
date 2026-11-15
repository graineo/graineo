//! Description vectors of the AI descriptions for the chosen text model, computed in
//! the background. The caller runs this on a low-priority thread; here the work is additionally
//! throttled (pause as long as the computation took, so at most about half of the time computes) and
//! the progress is published for the settings.

use std::sync::Mutex;

use crate::db::Db;
use crate::semantic::{description_model, QueryEncoder, TextModel};

/// Running computation: model, done, total.
static PROGRESS: Mutex<Option<(TextModel, i64, i64)>> = Mutex::new(None);

/// Progress of a running computation (`None`: nothing running).
pub fn progress() -> Option<(TextModel, i64, i64)> {
    *PROGRESS.lock().unwrap_or_else(|e| e.into_inner())
}

fn set(p: Option<(TextModel, i64, i64)>) {
    *PROGRESS.lock().unwrap_or_else(|e| e.into_inner()) = p;
}

/// Compute all missing description vectors of `model` (normally the standard model, see
/// `semantic::description_model`). Stops early when that model changes. Returns: count.
pub fn descriptions(db: &Db, model: TextModel) -> usize {
    let (mut done, total) = crate::similar::description_vector_counts(db, model);
    if done >= total {
        return 0;
    }
    let Ok(enc) = QueryEncoder::load(model) else { return 0 };
    // the large model computes several descriptions together (cheaper per description)
    let batch = if model == TextModel::Standard { 32 } else { 8 };
    let mut n = 0;
    set(Some((model, done, total)));
    loop {
        if description_model() != Some(model) {
            break;
        }
        let list = crate::similar::descriptions_without_vector(db, model, batch);
        if list.is_empty() {
            break;
        }
        let t = std::time::Instant::now();
        let texts: Vec<&str> = list.iter().map(|(_, s)| s.as_str()).collect();
        let vectors = enc.embed_batch(&texts);
        for ((file, _), v) in list.iter().zip(&vectors) {
            let _ = crate::similar::save_text_model(db, *file, model, v);
        }
        n += list.len();
        done += list.len() as i64;
        set(Some((model, done, total.max(done))));
        std::thread::sleep(t.elapsed().max(std::time::Duration::from_millis(20)));
    }
    set(None);
    n
}
