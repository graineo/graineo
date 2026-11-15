//! Find duplicates:
//! - **identical:** same size and capture time, confirmed via the partial checksum (first and last
//!   64 KB + length); without a reachable file, size and capture time suffice.
//! - **similar:** CLIP image vectors with cosine similarity ≥ [`SIMILAR`], compared only within
//!   the same capture day (series, edited exports next to the original) – that keeps it fast.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::similar::Vectors;
use crate::db::Db;

pub const SIMILAR: f32 = 0.97;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Identical,
    Similar,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub kind: Kind,
    pub files: Vec<i64>,
    /// identical: confirmed by the partial checksum (false: no file reachable, only size and capture time)
    pub verified: bool,
    /// similar: lowest cosine similarity between two members (for the reason shown); identical: 1.0
    pub score: f32,
}

pub fn find(db: &Db, vectors: Option<&Vectors>) -> Result<Vec<Group>, String> {
    let f = |e: rusqlite::Error| e.to_string();
    let mut groups = Vec::new();
    // identical
    let mut st = db
        .conn
        .prepare(
            "SELECT d.id, d.size, d.capture_time, d.status, s.path || o.path || d.name FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root
             WHERE d.size > 0 AND d.capture_time IS NOT NULL AND (d.size, d.capture_time) IN
               (SELECT size, capture_time FROM file WHERE size > 0 AND capture_time IS NOT NULL GROUP BY size, capture_time HAVING count(*) > 1)
             ORDER BY d.size, d.capture_time",
        )
        .map_err(f)?;
    let rows: Vec<(i64, i64, String, i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(f)?.flatten().collect();
    let mut per: HashMap<(i64, String), Vec<(i64, i64, String)>> = HashMap::new();
    for (id, g, a, z, p) in rows {
        per.entry((g, a)).or_default().push((id, z, p));
    }
    let mut equal_ids = std::collections::HashSet::new();
    for (_, v) in per {
        // split by partial checksum (missing files: their own group "unknown")
        let mut by_key: HashMap<String, Vec<i64>> = HashMap::new();
        for (id, z, p) in v {
            let s = if z == 0 { crate::reconcile::partial_sum(&PathBuf::from(&p)).unwrap_or_else(|| "?".into()) } else { "?".into() };
            by_key.entry(s).or_default().push(id);
        }
        for (k, ids) in by_key {
            if ids.len() > 1 {
                equal_ids.extend(ids.iter().copied());
                groups.push(Group { kind: Kind::Identical, files: ids, verified: k != "?", score: 1.0 });
            }
        }
    }
    // similar (per capture day)
    if let Some(v) = vectors {
        let mut st = db.conn.prepare("SELECT id, substr(capture_time, 1, 10) FROM file WHERE capture_time IS NOT NULL ORDER BY capture_time").map_err(f)?;
        let mut days: HashMap<String, Vec<i64>> = HashMap::new();
        for (id, tag) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(f)?.flatten() {
            if v.has(id) && !equal_ids.contains(&id) {
                days.entry(tag).or_default().push(id);
            }
        }
        for (_, ids) in days {
            // simple union: connect pairs above the threshold
            let n = ids.len();
            let mut parent: Vec<usize> = (0..n).collect();
            fn root_dir(e: &mut [usize], i: usize) -> usize {
                let mut i = i;
                while e[i] != i {
                    e[i] = e[e[i]];
                    i = e[i];
                }
                i
            }
            for a in 0..n {
                for b in a + 1..n {
                    if v.similarity(ids[a], ids[b]).is_some_and(|s| s >= SIMILAR) {
                        let (ra, rb) = (root_dir(&mut parent, a), root_dir(&mut parent, b));
                        parent[ra] = rb;
                    }
                }
            }
            let mut g: HashMap<usize, Vec<i64>> = HashMap::new();
            for i in 0..n {
                let r = root_dir(&mut parent, i);
                g.entry(r).or_default().push(ids[i]);
            }
            for (_, d) in g {
                if d.len() > 1 {
                    // lowest similarity of two members (large series: only the first 40 compared)
                    let k = d.len().min(40);
                    let mut score = 1.0f32;
                    for a in 0..k {
                        for b in a + 1..k {
                            if let Some(s) = v.similarity(d[a], d[b]) {
                                score = score.min(s);
                            }
                        }
                    }
                    groups.push(Group { kind: Kind::Similar, files: d, verified: false, score });
                }
            }
        }
    }
    Ok(groups)
}
