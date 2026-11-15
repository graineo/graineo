//! Open work around people, countable without effort: photos still without a face search, and whether the name
//! suggestions are out of date (somebody confirmed or rejected faces since they were last computed). The title bar
//! shows these ("Gesichter aktualisieren: noch n"); the app starts the work again when something is open.

use rusqlite::Connection;

const KEY: &str = "faces_assign_state";

/// Photos that wait for the face search, as far as a cheap query can tell (originals, no videos, file present).
/// The real list ([`crate::people::open_jobs`]) leaves out a few more (camera JPEGs next to a RAW), so this is an upper bound.
pub fn pending_search_upper_bound(c: &Connection) -> usize {
    c.query_row(
        "SELECT count(*) FROM image b JOIN file d ON d.id = b.file WHERE b.faces_scanned = 0 AND b.copy IS NULL AND d.format <> 'VIDEO' AND d.status = 0",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0) as usize
}

/// Exact number of photos the face search would process now (the same list the run uses).
pub fn pending_search(db: &crate::db::Db) -> usize {
    crate::people::open_jobs(db, usize::MAX).map(|v| v.len()).unwrap_or(0)
}

/// What the suggestions depend on: confirmed faces, rejected names, hidden people.
pub fn assign_signature(c: &Connection) -> String {
    let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    format!(
        "{}|{}|{}",
        n("SELECT count(*) FROM face WHERE status='confirmed' AND person IS NOT NULL AND embedding IS NOT NULL"),
        n("SELECT count(*) FROM face_never"),
        n("SELECT count(*) FROM person_info WHERE hidden=1")
    )
}

pub(crate) fn mark_assigned(c: &Connection, signature: &str) {
    // unchanged: no write (a run without changes writes nothing)
    let stored: Option<String> = c.query_row("SELECT value FROM info WHERE name=?1", [KEY], |r| r.get(0)).ok();
    if stored.as_deref() == Some(signature) {
        return;
    }
    let _ = c.execute("INSERT INTO info(name, value) VALUES(?1, ?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value", rusqlite::params![KEY, signature]);
}

/// Have confirmations, rejections or hidden people changed since the suggestions were computed?
pub fn assign_dirty(c: &Connection) -> bool {
    let stored: String = c.query_row("SELECT value FROM info WHERE name=?1", [KEY], |r| r.get(0)).unwrap_or_else(|_| "0|0|0".into());
    stored != assign_signature(c)
}

/// Why does a face have no person? Reads the same rules as [`crate::people::assign`] and names the one that stops it
/// (for the diagnosis on a copy of a library; changes nothing).
pub fn explain_face(c: &Connection, face: i64) -> Result<String, String> {
    use crate::people::{best_person, load_samples, Thresholds};
    let s = Thresholds::default();
    let all = load_samples(c)?;
    let Some(q) = all.iter().find(|p| p.id == face) else {
        let row: Option<(String, Option<f64>)> = c.query_row("SELECT status, quality FROM face WHERE id=?1", [face], |r| Ok((r.get(0)?, r.get(1)?))).ok();
        return Ok(match row {
            None => "Gesicht gibt es nicht".into(),
            Some((z, _)) => format!("kein Merkmal berechnet (Zustand {z}): das Gesicht wird nie verglichen"),
        });
    };
    if q.person.is_some() && q.status == "confirmed" {
        return Ok("hat schon eine bestätigte Person".into());
    }
    if q.status == "rejected" {
        return Ok("abgelehnt (kein Gesicht): wird nicht verglichen".into());
    }
    if q.quality < 0.3 {
        return Ok(format!("Qualität {:.2} unter 0,30 (zu klein, unscharf oder im Profil): wird nicht verglichen", q.quality));
    }
    let hidden: std::collections::HashSet<i64> = c
        .prepare("SELECT keyword FROM person_info WHERE hidden=1")
        .and_then(|mut st| st.query_map([], |r| r.get::<_, i64>(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    let refs: Vec<&crate::people::Probe> = all.iter().filter(|p| p.status == "confirmed" && p.person.is_some_and(|x| !hidden.contains(&x)) && p.quality >= s.min_quality).collect();
    let mut never: std::collections::HashMap<i64, Vec<i64>> = Default::default();
    for r in c.prepare("SELECT person FROM face_never WHERE face=?1").map_err(|e| e.to_string())?.query_map([face], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())?.flatten() {
        never.entry(face).or_default().push(r);
    }
    for p in all.iter().filter(|p| p.image == q.image && p.status == "confirmed") {
        if let Some(pe) = p.person {
            never.entry(face).or_default().push(pe);
        }
    }
    let Some(m) = best_person(&[q], &refs, s.k, 0, &never).pop().flatten() else {
        return Ok("keine passende Person unter den bestätigten Gesichtern (oder alle auf diesem Foto schon vergeben / abgelehnt)".into());
    };
    let name: String = c.query_row("SELECT name FROM keyword WHERE id=?1", [m.person], |r| r.get(0)).unwrap_or_default();
    Ok(if m.value < s.suggestion {
        format!("beste Person {name}, Ähnlichkeit {:.2} unter der Vorschlagsschwelle {:.2}", m.value, s.suggestion)
    } else if m.value - m.second_item < s.distance * 0.5 {
        format!("beste Person {name} ({:.2}), aber die zweitbeste liegt zu nah ({:.2}, Abstand {:.2} unter {:.2}): kein Vorschlag, damit keine Verwechslung", m.value, m.second_item, m.value - m.second_item, s.distance * 0.5)
    } else {
        format!("Vorschlag für {name} wäre möglich ({:.2}); fehlt nur, wenn ein anderes Gesicht im selben Foto besser passt oder die Berechnung noch nicht lief", m.value)
    })
}

/// Open work for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// photos without face search
    pub photos: usize,
    /// the suggestions need a new computation
    pub suggestions: bool,
}

impl Work {
    pub fn none(&self) -> bool {
        self.photos == 0 && !self.suggestions
    }
}

/// Measure the open work. `known`: (upper bound, exact) of the last measurement – the exact count (all rows of the
/// library) is only repeated when the cheap count changed.
pub fn measure(db: &crate::db::Db, known: &mut Option<(usize, usize)>) -> Work {
    let upper = pending_search_upper_bound(&db.conn);
    let photos = if upper == 0 {
        *known = None;
        0
    } else {
        match known {
            Some((u, exact)) if *u == upper => *exact,
            _ => {
                let exact = pending_search(db);
                *known = Some((upper, exact));
                exact
            }
        }
    };
    Work { photos, suggestions: assign_dirty(&db.conn) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::people::blob;
    use rusqlite::params;

    pub(crate) fn test_db(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("graineo-pstate-{}-{}.sqlite", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        let db = Db::open(&p).unwrap();
        db.conn.execute_batch("INSERT INTO root_folder(id, path, name, origin) VALUES(1,'/x/','x','own'); INSERT INTO folder(id, root, path) VALUES(1,1,'');").unwrap();
        db
    }

    pub(crate) fn add_image(db: &Db, name: &str) -> i64 {
        let id: i64 = db.conn.query_row("SELECT coalesce(max(id),0)+1 FROM file", [], |r| r.get(0)).unwrap();
        db.conn.execute("INSERT INTO file(id, folder, name, format) VALUES(?1, 1, ?2, 'JPG')", params![id, name]).unwrap();
        db.conn.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [id]).unwrap();
        id
    }

    pub(crate) fn add_face(db: &Db, image: i64, person: Option<i64>, state: &str, v: &[f32]) -> i64 {
        db.conn
            .execute(
                "INSERT INTO face(image, x, y, w, h, embedding, model, quality, source, person, status) VALUES(?1,0.1,0.1,0.3,0.3,?2,'test',0.9,'graineo',?3,?4)",
                params![image, blob(v), person, state],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    #[test]
    fn counts_open_work() {
        let db = test_db("work");
        let a = add_image(&db, "a.jpg");
        let _b = add_image(&db, "b.jpg");
        // images in the temp library have no real files: the cheap count sees both, the exact list has no path for them
        assert_eq!(pending_search_upper_bound(&db.conn), 2);
        db.conn.execute("UPDATE image SET faces_scanned = 1 WHERE id = ?1", [a]).unwrap();
        assert_eq!(pending_search_upper_bound(&db.conn), 1);
        let mut known = None;
        let w = measure(&db, &mut known);
        assert!(known.is_some());
        assert_eq!(w.suggestions, false, "nothing confirmed yet");
        // a confirmation makes the suggestions out of date; computing them marks them as current
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(1,'Anna',1)", []).unwrap();
        add_face(&db, a, Some(1), "confirmed", &[1.0, 0.0, 0.0, 0.0]);
        assert!(assign_dirty(&db.conn));
        crate::people::assign(&db.conn, &crate::people::Thresholds::default()).unwrap();
        assert!(!assign_dirty(&db.conn));
        db.conn.execute("INSERT INTO face_never(face, person) VALUES(1, 1)", []).unwrap();
        assert!(assign_dirty(&db.conn), "a rejection counts too");
    }

    /// A photo whose confirmed face is not in the search index at all is found by the plain name (free text).
    #[test]
    fn plain_name_finds_confirmed_faces_without_index() {
        let db = test_db("name");
        let a = add_image(&db, "p1.jpg");
        let b = add_image(&db, "p2.jpg");
        let c = add_image(&db, "p3.jpg");
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(1,'Christa M.',1)", []).unwrap();
        // index built when nobody was confirmed yet
        crate::search::build_index(&db).unwrap();
        add_face(&db, a, Some(1), "confirmed", &[1.0, 0.0, 0.0, 0.0]);
        add_face(&db, b, Some(1), "suggested", &[1.0, 0.0, 0.0, 0.0]);
        let rows = db.rows().unwrap();
        let paths = db.folder_paths().unwrap();
        let k = crate::search::Context { db: &db, rows: &rows, folder_paths: &paths };
        let hits = |q: &str| -> Vec<i64> {
            let r = crate::search::evaluate(&k, &crate::search::parse(q));
            rows.iter().zip(r).filter(|(_, ok)| *ok).map(|(z, _)| z.image).collect()
        };
        for q in ["Christa", "christa m", "\"Christa M.\"", "person:\"Christa M.\""] {
            assert_eq!(hits(q), vec![a], "{q}");
        }
        let _ = c;
        // the small update: the new keyword is in the index without a full rebuild
        crate::sync::set_keywords(&db, b, &["Christa M.".to_string()], 1, true).unwrap();
        assert!(crate::search::word_matches(&db, "\"Christa M.\"", true, false).contains(&b));
        let mut got = hits("Christa");
        got.sort();
        assert_eq!(got, vec![a, b]);
    }

    /// Makes a small demo library for screenshots: `GRAINEO_DEMO_DB=<file> cargo test … make_demo_library -- --ignored`.
    #[test]
    #[ignore]
    fn make_demo_library() {
        let Some(path) = std::env::var_os("GRAINEO_DEMO_DB") else { return };
        let path = std::path::PathBuf::from(path);
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        db.conn.execute_batch("INSERT INTO root_folder(id, path, name, origin) VALUES(1,'/x/','Demo','own'); INSERT INTO folder(id, root, path) VALUES(1,1,'');").unwrap();
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(1,'Anna Beispiel',1),(2,'Ben Muster',1),(3,'Christa M.',1)", []).unwrap();
        for (person, confirmed, suggested) in [(1i64, 12, 0), (2, 7, 3), (3, 20, 5)] {
            for n in 0..confirmed + suggested {
                let i = add_image(&db, &format!("p{person}-{n}.jpg"));
                add_face(&db, i, Some(person), if n < confirmed { "confirmed" } else { "suggested" }, &[1.0, 0.0, 0.0, 0.0]);
            }
        }
        db.conn.execute("UPDATE face SET confidence = 0.8, score_own = 0.5 WHERE status = 'suggested'", []).unwrap();
        db.conn.execute("UPDATE image SET faces_scanned = 1", []).unwrap();
    }

    /// The reason a face has no person is named.
    #[test]
    fn explains_missing_person() {
        let db = test_db("explain");
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(1,'Anna',1)", []).unwrap();
        for n in 0..4 {
            let i = add_image(&db, &format!("r{n}.jpg"));
            add_face(&db, i, Some(1), "confirmed", &[1.0, 0.0, 0.0, 0.0]);
        }
        let i = add_image(&db, "q.jpg");
        let far = add_face(&db, i, None, "unknown", &[0.0, 1.0, 0.0, 0.0]);
        let near = add_face(&db, add_image(&db, "q2.jpg"), None, "unknown", &[1.0, 0.0, 0.0, 0.0]);
        let small = add_face(&db, add_image(&db, "q3.jpg"), None, "unknown", &[1.0, 0.0, 0.0, 0.0]);
        db.conn.execute("UPDATE face SET quality = 0.1 WHERE id = ?1", [small]).unwrap();
        assert!(explain_face(&db.conn, far).unwrap().contains("unter der Vorschlagsschwelle"));
        assert!(explain_face(&db.conn, near).unwrap().contains("Vorschlag für Anna"));
        assert!(explain_face(&db.conn, small).unwrap().contains("Qualität"));
        assert!(explain_face(&db.conn, 9999).unwrap().contains("gibt es nicht"));
    }

    /// Phrases keep all words: "t" occurring earlier in the text does not remove the later one.
    #[test]
    fn phrase_keeps_repeated_words() {
        let db = test_db("phrase");
        let a = add_image(&db, "t.jpg");
        db.conn.execute("INSERT INTO keyword(id, name, person) VALUES(1,'Christa M.',1)", []).unwrap();
        db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(?1, 1)", [a]).unwrap();
        db.conn.execute("UPDATE image SET caption = 't t t' WHERE id = ?1", [a]).unwrap();
        crate::search::build_index(&db).unwrap();
        assert!(crate::search::word_matches(&db, "\"Christa M\"", true, false).contains(&a));
    }
}
