//! Keyword suggestions from the picture content (zero-shot with SigLIP 2 Base): every term of a fixed vocabulary
//! (`data/content_terms.tsv`, several hundred common photographic terms in five languages) is turned into a text
//! vector once; a photo's image vector, already computed for the semantic search, is compared with all of them.
//! The model's own sigmoid (scale and bias from training) gives a probability per term; only terms above a
//! measured threshold are offered, at most [`MAX_SUGGESTIONS`] per photo.
//!
//! Suggestions are never assigned by themselves: the interface shows them as chips, a click assigns the keyword
//! (one ⌘Z step), "reject" remembers the term for this photo (table `content_term_rejected`). Nothing is computed
//! in bulk: suggestions are worked out when a photo is shown (a few hundred dot products), the text vectors once
//! per vocabulary and model (file `content-term-vectors.bin` in the data folder). Without the model or without an
//! image vector of the photo nothing is suggested and nothing is downloaded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::types::Value;
use rusqlite::{params, OptionalExtension};

use crate::db::Db;
use crate::manage::Snapshot;
use crate::module::Definition;

type R<T> = Result<T, String>;
fn f(e: rusqlite::Error) -> String {
    e.to_string()
}

/// The model the suggestions use (its image vectors come from the search stage "Beste Treffer").
pub const MODEL: &Definition = &crate::module::SIGLIP_BASE;

/// Texts given to the model per term (`{}` = the term's noun phrase); with several, their vectors are averaged.
/// Measured on the test photos (`graineo-dev content-keywords`): see [`THRESHOLD`] for the numbers.
pub const TEMPLATES: &[&str] = &["a photo of {}."];

/// Smallest probability (model sigmoid) of a suggested term, one value for all terms. Measured with
/// `graineo-dev content-keywords` on eleven test photos labelled by hand (6.6 true terms each) and nine synthetic
/// pictures (flat, gradients, noise, checkerboard, text), 16.11.2026:
///
/// | threshold | per photo | precision | recall | wrong per synthetic picture |
/// |---|---|---|---|---|
/// | 0.01 | 3.6 | 0.72 | 0.39 | 0.44 |
/// | 0.02 | 2.9 | 0.84 | 0.36 | 0.33 |
/// | **0.03** | 2.5 | 0.93 | 0.33 | 0.22 ("texture" on noise) |
/// | 0.05 | 1.8 | 1.00 | 0.27 | 0.22 |
///
/// Ranking without threshold: P@1 1.00, P@3 0.82, mAP 0.71. "This is a photo of {}." and averaged templates ranked
/// alike within the noise of eleven photos but suggested more on synthetic pictures. Thresholds per term would need
/// labelled photos per term; the probabilities of the model are calibrated alike across terms by its training
/// (sigmoid per pair), so one value is used.
pub const THRESHOLD: f32 = 0.03;

/// At most this many suggestions per photo.
pub const MAX_SUGGESTIONS: usize = 5;

/// One term of the vocabulary.
#[derive(Debug, Clone)]
pub struct Term {
    /// stable id (English name), stored for rejections
    pub id: &'static str,
    /// English noun phrase for the text model
    pub prompt: &'static str,
    pub de: &'static str,
    pub es: &'static str,
    pub fr: &'static str,
    pub ja: &'static str,
}

impl Term {
    /// Name in a language (BCP 47 code `de`, `en`, `es`, `fr`, `ja`; anything else: German).
    pub fn name(&self, language: &str) -> &'static str {
        match language {
            "en" => self.id,
            "es" => self.es,
            "fr" => self.fr,
            "ja" => self.ja,
            _ => self.de,
        }
    }

    /// All names (for "the photo already carries it").
    pub fn names(&self) -> [&'static str; 5] {
        [self.de, self.id, self.es, self.fr, self.ja]
    }

    /// Texts the model reads (one per template; their vectors are averaged).
    pub fn texts(&self) -> Vec<String> {
        TEMPLATES.iter().map(|t| t.replace("{}", self.prompt)).collect()
    }
}

const TERMS_TSV: &str = include_str!("../data/content_terms.tsv");

fn parse(tsv: &'static str) -> Vec<Term> {
    tsv.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let c: Vec<&'static str> = l.split('\t').map(str::trim).collect();
            (c.len() == 6 && c.iter().all(|x| !x.is_empty())).then(|| Term { id: c[0], prompt: c[1], de: c[2], es: c[3], fr: c[4], ja: c[5] })
        })
        .collect()
}

/// The vocabulary.
pub fn terms() -> &'static [Term] {
    static T: std::sync::OnceLock<Vec<Term>> = std::sync::OnceLock::new();
    T.get_or_init(|| parse(TERMS_TSV))
}

// ───────────────────────────────────────────── Text vectors

/// Text vectors of all terms with the model's sigmoid.
pub struct TermVectors {
    pub dim: usize,
    /// row per term, normalized, in the order of [`terms`]
    pub vectors: Vec<f32>,
    pub scale: f32,
    pub bias: f32,
}

impl TermVectors {
    /// Probability per term (order of [`terms`]) for a normalized image vector.
    pub fn probabilities(&self, image: &[f32]) -> Vec<f32> {
        if image.len() != self.dim {
            return Vec::new();
        }
        self.vectors
            .chunks_exact(self.dim)
            .map(|t| {
                let cos: f32 = t.iter().zip(image).map(|(a, b)| a * b).sum();
                1.0 / (1.0 + (-(self.scale * cos + self.bias)).exp())
            })
            .collect()
    }

    /// Compute them with the text tower of the model in `dir` (several seconds; in a background thread). Each term
    /// has one or more texts (prompt templates); their vectors are averaged and normalized again.
    pub fn compute(dir: &Path, texts: &[Vec<String>]) -> R<TermVectors> {
        let (scale, bias) = crate::siglip::logit_params(dir)?;
        let tower = crate::siglip::TextTower::load(dir)?;
        let dim = tower.dim();
        let flat: Vec<&str> = texts.iter().flatten().map(String::as_str).collect();
        let mut all: Vec<Vec<f32>> = Vec::with_capacity(flat.len());
        for chunk in flat.chunks(32) {
            all.extend(tower.embed(chunk)?);
        }
        let mut vectors = Vec::with_capacity(texts.len() * dim);
        let mut next = all.into_iter();
        for t in texts {
            let mut sum = vec![0f32; dim];
            for v in next.by_ref().take(t.len()) {
                sum.iter_mut().zip(&v).for_each(|(a, b)| *a += b);
            }
            let n = sum.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            vectors.extend(sum.into_iter().map(|x| x / n));
        }
        Ok(TermVectors { dim, vectors, scale, bias })
    }

    fn to_bytes(&self, key: u64) -> Vec<u8> {
        let mut b = Vec::with_capacity(32 + self.vectors.len() * 4);
        b.extend(MAGIC);
        b.extend(key.to_le_bytes());
        b.extend((self.dim as u32).to_le_bytes());
        b.extend(((self.vectors.len() / self.dim.max(1)) as u32).to_le_bytes());
        b.extend(self.scale.to_le_bytes());
        b.extend(self.bias.to_le_bytes());
        for v in &self.vectors {
            b.extend(v.to_le_bytes());
        }
        b
    }

    fn from_bytes(b: &[u8], key: u64, count: usize) -> Option<TermVectors> {
        let rest = b.strip_prefix(MAGIC)?;
        let u32_at = |o: usize| rest.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
        let f32_at = |o: usize| rest.get(o..o + 4).map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]]));
        if u64::from_le_bytes(rest.get(..8)?.try_into().ok()?) != key {
            return None;
        }
        let (dim, n) = (u32_at(8)? as usize, u32_at(12)? as usize);
        let (scale, bias) = (f32_at(16)?, f32_at(20)?);
        let data = rest.get(24..)?;
        if n != count || dim == 0 || dim > 4096 || data.len() != n * dim * 4 {
            return None;
        }
        let vectors = data.chunks_exact(4).map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]])).collect();
        Some(TermVectors { dim, vectors, scale, bias })
    }
}

const MAGIC: &[u8] = b"GRAINEO-CTV1";

/// File of the cached text vectors (data folder of the app).
pub fn cache_file() -> PathBuf {
    crate::data_dir().join("content-term-vectors.bin")
}

/// Identity of model, template and vocabulary: a cache file of another state is computed anew.
fn cache_key(texts: &[Vec<String>]) -> u64 {
    // FNV-1a, stable across builds (the std hasher is not)
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [MODEL.repo, MODEL.revision].into_iter().map(str::as_bytes).chain(texts.iter().flatten().map(String::as_bytes)) {
        for b in part.iter().chain(&[0u8]) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Folder of the installed model (or, for tests, a Hugging Face cache via `GRAINEO_HF_CACHE`).
pub fn model_folder() -> Option<PathBuf> {
    crate::siglip::folder(MODEL.id, MODEL.repo)
}

/// Text vectors from the cache file, or computed and written to it.
pub fn load_or_compute(dir: &Path) -> R<TermVectors> {
    let texts: Vec<Vec<String>> = terms().iter().map(Term::texts).collect();
    let key = cache_key(&texts);
    let file = cache_file();
    if let Some(v) = std::fs::read(&file).ok().and_then(|b| TermVectors::from_bytes(&b, key, texts.len())) {
        return Ok(v);
    }
    let v = TermVectors::compute(dir, &texts)?;
    // the cache only saves time: a failed write is no error
    let tmp = file.with_extension("bin.tmp");
    if std::fs::write(&tmp, v.to_bytes(key)).and_then(|_| std::fs::rename(&tmp, &file)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(v)
}

static READY: Mutex<Option<Arc<TermVectors>>> = Mutex::new(None);
static PREPARING: AtomicBool = AtomicBool::new(false);
static FAILED: Mutex<Option<String>> = Mutex::new(None);

/// State of the text vectors for the interface.
#[derive(Clone)]
pub enum Vectors {
    Ready(Arc<TermVectors>),
    /// being computed (first use, a few seconds)
    Preparing,
    /// the model is not installed
    NoModel,
    Failed(String),
}

/// Text vectors when ready; otherwise starts preparing them once in a thread of its own (utility priority) and calls
/// `done` when finished. Never downloads anything.
pub fn vectors(done: impl Fn() + Send + 'static) -> Vectors {
    if let Some(v) = READY.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Vectors::Ready(v);
    }
    if let Some(e) = FAILED.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Vectors::Failed(e);
    }
    let Some(dir) = model_folder() else { return Vectors::NoModel };
    if !PREPARING.swap(true, Ordering::SeqCst) {
        std::thread::spawn(move || {
            // the user looks at the photo and waits for the row: once a few seconds on the performance cores
            // (background priority on the efficiency cores took minutes while the search read photos in)
            crate::platform::set_priority(crate::platform::Priority::Utility);
            let t = std::time::Instant::now();
            match load_or_compute(&dir) {
                Ok(v) => {
                    eprintln!("content keywords: term vectors ready in {:.1} s", t.elapsed().as_secs_f64());
                    *READY.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(v));
                }
                Err(e) => {
                    eprintln!("content keywords: {e}");
                    *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                }
            }
            PREPARING.store(false, Ordering::SeqCst);
            done();
        });
    }
    Vectors::Preparing
}

/// The model was installed or removed: forget loaded vectors and a failure.
pub fn model_changed() {
    *READY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

// ───────────────────────────────────────────── Suggestions

/// Image vector of a photo in the model's space; `None` if the photo has none yet (or a zero vector: unreadable).
pub fn image_vector(db: &Db, image: i64) -> R<Option<Vec<f32>>> {
    let b: Option<Vec<u8>> = db
        .conn
        .query_row("SELECT v.vec FROM image i JOIN image_vector_model v ON v.file = i.file AND v.model = ?2 WHERE i.id = ?1", params![image, MODEL.repo], |r| r.get(0))
        .optional()
        .map_err(f)?;
    Ok(b.map(|b| b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect::<Vec<f32>>()).filter(|v| v.iter().any(|x| *x != 0.0)))
}

/// Terms rejected for a photo (ids).
pub fn rejected(db: &Db, image: i64) -> R<HashSet<String>> {
    let mut st = db.conn.prepare_cached("SELECT term FROM content_term_rejected WHERE image = ?1").map_err(f)?;
    let v = st.query_map([image], |r| r.get::<_, String>(0)).map_err(f)?.collect::<rusqlite::Result<HashSet<String>>>().map_err(f)?;
    Ok(v)
}

/// A suggested term.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub term: &'static Term,
    pub probability: f32,
}

/// The best terms above the threshold, at most [`MAX_SUGGESTIONS`], skipping those in `skip` (term ids).
pub fn pick(probabilities: &[f32], skip: &dyn Fn(&Term) -> bool) -> Vec<Suggestion> {
    let mut v: Vec<Suggestion> = terms()
        .iter()
        .zip(probabilities)
        .filter(|(t, p)| **p >= THRESHOLD && !skip(t))
        .map(|(term, p)| Suggestion { term, probability: *p })
        .collect();
    v.sort_by(|a, b| b.probability.total_cmp(&a.probability));
    v.truncate(MAX_SUGGESTIONS);
    v
}

/// Suggestions for a photo: `None` without image vector. Terms the photo already carries as a keyword (by name in
/// any of the languages, ignoring case and accents) and rejected terms are left out.
pub fn suggestions(db: &Db, image: i64, vectors: &TermVectors) -> R<Option<Vec<Suggestion>>> {
    let Some(v) = image_vector(db, image)? else { return Ok(None) };
    let gone = rejected(db, image)?;
    let have: HashSet<String> = crate::keywords::shown_of(&db.conn, image)?
        .into_iter()
        .map(|(_, p)| crate::search::fold_text(p.rsplit(crate::keywords::SHOWN_SEPARATOR).next().unwrap_or(&p)))
        .collect();
    let skip = |t: &Term| gone.contains(t.id) || t.names().iter().any(|n| have.contains(&crate::search::fold_text(n)));
    Ok(Some(pick(&vectors.probabilities(&v), &skip)))
}

/// The keyword a suggestion means: an existing keyword of that name (one that exists exactly once in the tree, or the
/// one at the top level), otherwise a new keyword at the top level. People are never taken.
fn keyword_for(db: &Db, name: &str) -> R<(i64, bool)> {
    let person = |id: i64| db.conn.query_row("SELECT person <> 0 FROM keyword WHERE id = ?1", [id], |r| r.get::<_, bool>(0)).unwrap_or(false);
    if let crate::keywords::Resolved::Existing(id) = crate::keywords::resolve(db, name, false)? {
        if !person(id) {
            return Ok((id, false));
        }
    }
    if let Some(id) = crate::keywords::sibling_named(&db.conn, None, name, None)? {
        if !person(id) {
            return Ok((id, false));
        }
    }
    db.conn.execute("INSERT INTO keyword(name, parent) VALUES(?1, NULL)", [name.trim()]).map_err(f)?;
    Ok((db.conn.last_insert_rowid(), true))
}

/// Accept a suggestion: assign the keyword `name` (the term in the interface language) to the photo. Returns the
/// number of sidecars written and the snapshot for ⌘Z (also removes a keyword the step created).
pub fn accept(db: &Db, image: i64, name: &str) -> R<(usize, Snapshot)> {
    let (id, created) = keyword_for(db, name)?;
    let mut snap = Snapshot::default();
    if created {
        snap.add_created_keywords(&[id]);
    }
    let (n, s) = crate::keywords::change(db, &[image], &[id], &[])?;
    snap.merge(s);
    Ok((n, snap))
}

/// Reject a suggestion for this photo (remembered; ⌘Z forgets it again).
pub fn reject(db: &Db, image: i64, term: &str) -> R<Snapshot> {
    let n = db.conn.execute("INSERT OR IGNORE INTO content_term_rejected(image, term, time) VALUES(?1, ?2, ?3)", params![image, term, crate::now()]).map_err(f)?;
    let mut snap = Snapshot::default();
    if n > 0 {
        snap.add_rows("content_term_rejected", &["image", "term"], vec![vec![Value::Integer(image), Value::Text(term.to_string())]], Vec::new());
    }
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_is_complete_and_unique_per_language() {
        let t = terms();
        assert!(t.len() >= 300, "{} terms", t.len());
        let lines = TERMS_TSV.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).count();
        assert_eq!(lines, t.len(), "every line has six columns");
        for lang in ["de", "en", "es", "fr", "ja"] {
            let mut seen = HashSet::new();
            for x in t {
                assert!(seen.insert(x.name(lang).to_lowercase()), "{lang}: „{}“ twice", x.name(lang));
            }
        }
        assert!(t.iter().all(|x| x.texts().iter().all(|s| !s.contains("{}") && s.contains(x.prompt))));
    }

    #[test]
    fn probabilities_threshold_and_cache_round_trip() {
        let n = terms().len();
        // a term vector equal to the image gives cosine 1, all others 0
        let dim = 4;
        let mut vectors = vec![0f32; n * dim];
        vectors[0] = 1.0; // term 0 along axis 0
        vectors[dim + 1] = 1.0; // term 1 along axis 1
        let tv = TermVectors { dim, vectors, scale: 100.0, bias: -10.0 };
        let p = tv.probabilities(&[1.0, 0.0, 0.0, 0.0]);
        assert_eq!(p.len(), n);
        assert!(p[0] > 0.99 && p[1] < 1e-4);
        let s = pick(&p, &|_| false);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].term.id, terms()[0].id);
        assert!(pick(&p, &|t| t.id == terms()[0].id).is_empty(), "skipped terms are left out");
        assert!(tv.probabilities(&[1.0]).is_empty(), "wrong width");
        let b = tv.to_bytes(7);
        let back = TermVectors::from_bytes(&b, 7, n).unwrap();
        assert_eq!((back.dim, back.scale, back.bias, back.vectors.len()), (4, 100.0, -10.0, n * 4));
        assert!(TermVectors::from_bytes(&b, 8, n).is_none(), "other vocabulary or model");
        assert!(TermVectors::from_bytes(&b[..b.len() - 1], 7, n).is_none());
    }

    #[test]
    fn accept_and_reject_with_undo() {
        let d = std::env::temp_dir().join(format!("graineo-content-kw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let db = Db::open(&d.join("library.sqlite")).unwrap();
        db.conn
            .execute_batch(
                "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/f/', 'f', 'own'); INSERT INTO folder(id, root, path) VALUES(1, 1, '');
                 INSERT INTO file(id, folder, name, format) VALUES(1, 1, 'a.jpg', 'JPG'); INSERT INTO image(id, file) VALUES(1, 1);
                 INSERT INTO keyword(id, name) VALUES(10, 'Tiere'); INSERT INTO keyword(id, name, parent) VALUES(11, 'Hund', 10);
                 INSERT INTO keyword(id, name, person) VALUES(12, 'Strand', 1);",
            )
            .unwrap();
        // an existing keyword somewhere in the tree is used, not duplicated
        let (_, snap) = accept(&db, 1, "Hund").unwrap();
        assert_eq!(crate::keywords::ids_of(&db.conn, 1).unwrap(), vec![11]);
        crate::manage::undo(&db, &snap).unwrap();
        assert!(crate::keywords::ids_of(&db.conn, 1).unwrap().is_empty());
        assert_eq!(db.conn.query_row("SELECT count(*) FROM keyword", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        // a new keyword at the top level (a person of the same name is never taken); ⌘Z removes it again
        let (_, snap) = accept(&db, 1, "Strand").unwrap();
        let id = crate::keywords::ids_of(&db.conn, 1).unwrap()[0];
        assert_ne!(id, 12);
        assert_eq!(db.conn.query_row("SELECT parent FROM keyword WHERE id = ?1", [id], |r| r.get::<_, Option<i64>>(0)).unwrap(), None);
        crate::manage::undo(&db, &snap).unwrap();
        assert_eq!(db.conn.query_row("SELECT count(*) FROM keyword WHERE id = ?1", [id], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // the new keyword got further photos on a way without its own ⌘Z step (Lightroom import, reading sidecars):
        // ⌘Z takes back only this step's assignment, the keyword and the later assignments stay
        db.conn.execute_batch("INSERT INTO file(id, folder, name, format) VALUES(2, 1, 'b.jpg', 'JPG'); INSERT INTO image(id, file) VALUES(2, 2);").unwrap();
        let (_, snap) = accept(&db, 1, "Strand").unwrap();
        let id = crate::keywords::ids_of(&db.conn, 1).unwrap()[0];
        db.conn.execute("INSERT INTO image_keyword(image, keyword) VALUES(2, ?1)", [id]).unwrap();
        crate::manage::undo(&db, &snap).unwrap();
        assert!(crate::keywords::ids_of(&db.conn, 1).unwrap().is_empty());
        assert_eq!(crate::keywords::ids_of(&db.conn, 2).unwrap(), vec![id], "later assignment kept");
        // the same with a keyword below it
        db.conn.execute("DELETE FROM image_keyword WHERE image = 2", []).unwrap();
        db.conn.execute("DELETE FROM keyword WHERE id = ?1", [id]).unwrap();
        let (_, snap) = accept(&db, 1, "Strand").unwrap();
        let id = crate::keywords::ids_of(&db.conn, 1).unwrap()[0];
        db.conn.execute("INSERT INTO keyword(name, parent) VALUES('Ostsee', ?1)", [id]).unwrap();
        crate::manage::undo(&db, &snap).unwrap();
        assert_eq!(db.conn.query_row("SELECT count(*) FROM keyword WHERE id = ?1 OR parent = ?1", [id], |r| r.get::<_, i64>(0)).unwrap(), 2, "kept with its child");
        // rejecting is remembered per photo and undone
        let snap = reject(&db, 1, "beach").unwrap();
        assert!(rejected(&db, 1).unwrap().contains("beach"));
        assert!(reject(&db, 1, "beach").unwrap().is_empty(), "twice: nothing new");
        crate::manage::undo(&db, &snap).unwrap();
        assert!(rejected(&db, 1).unwrap().is_empty());
        // no image vector: no suggestions
        let tv = TermVectors { dim: 2, vectors: vec![0.0; terms().len() * 2], scale: 1.0, bias: 0.0 };
        assert!(suggestions(&db, 1, &tv).unwrap().is_none());
        drop(db);
        std::fs::remove_dir_all(d).unwrap();
    }
}
