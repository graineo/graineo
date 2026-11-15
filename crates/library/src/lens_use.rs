//! Which lenses the library holds and where an own lens profile is used (read only, for the window OBJEKTIVE
//! and the assignment). Runs on the calling thread's own read connection; call it from a background thread.

use rusqlite::params;

/// One lens (as the files name it) of one camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LensRow {
    /// camera as stored in the library ("Canon EOS RP", "SIGMA sd Quattro")
    pub camera: String,
    /// lens name from the file; empty = none (manual lens, adapter)
    pub lens: String,
    pub photos: usize,
}

/// All combinations of camera and lens of photos that are present, most photos first.
pub fn lenses() -> Vec<LensRow> {
    crate::store::with_conn(|c| {
        let mut st = c
            .prepare(
                "SELECT coalesce(camera, ''), coalesce(lens, ''), count(*) FROM file
                 WHERE status IN (0, 2) AND format <> 'VIDEO' AND camera IS NOT NULL AND camera <> ''
                 GROUP BY 1, 2 ORDER BY 3 DESC, 1, 2",
            )
            .ok()?;
        let rows = st.query_map([], |r| Ok(LensRow { camera: r.get(0)?, lens: r.get(1)?, photos: r.get::<_, i64>(2)? as usize })).ok()?;
        Some(rows.flatten().collect())
    })
    .unwrap_or_default()
}

/// Photos of a camera (`model` as the library or the file names it; the library may add the maker in front) whose
/// file names no lens (`lens = None`) or this lens name.
pub fn photos_matching(model: &str, lens: Option<&str>) -> usize {
    crate::store::with_conn(|c| {
        let n: i64 = match lens {
            None => c.query_row("SELECT count(*) FROM file WHERE status IN (0, 2) AND lower(camera) LIKE '%' || lower(?1) AND (lens IS NULL OR lens = '')", params![model], |r| r.get(0)).ok()?,
            Some(l) => c.query_row("SELECT count(*) FROM file WHERE status IN (0, 2) AND lower(camera) LIKE '%' || lower(?1) AND lens = ?2", params![model, l], |r| r.get(0)).ok()?,
        };
        Some(n as usize)
    })
    .unwrap_or(0)
}

/// Photos (files) that carry the profile `id` in their own develop settings.
pub fn photos_using(profile_id: &str) -> usize {
    if !profile_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ')) {
        return 0;
    }
    let needle = format!("\"lens_profile\":\"{profile_id}@");
    crate::store::with_conn(|c| {
        let n: i64 = c.query_row("SELECT count(DISTINCT file) FROM develop_state WHERE instr(settings, ?1) > 0", params![needle], |r| r.get(0)).ok()?;
        Some(n as usize)
    })
    .unwrap_or(0)
}
