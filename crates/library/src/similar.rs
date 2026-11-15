//! "Similar photos" via the CLIP image vectors of the library (`image_vector`: `clip-ViT-B-32`, 512 × f32
//! little-endian, normalized). The vectors are loaded into memory once (~80 MB for 40,000 images)
//! and compared by dot product (exhaustive search, ~10 ms).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::db::Db;

pub struct Vectors {
    pub dim: usize,
    file: Vec<i64>,
    values: Vec<f32>,
    index: HashMap<i64, usize>,
}

impl Vectors {
    /// Load the library's image vectors (table `image_vector`).
    pub fn load(db: &Db) -> Result<Vectors, String> {
        Self::load_from(db, "image_vector")
    }

    /// Text vectors of the AI descriptions (same space, multilingual text encoder).
    pub fn load_text(db: &Db) -> Result<Vectors, String> {
        Self::load_from(db, "text_vector")
    }

    /// Empty set of the given width (vectors of other search models, `crate::search_model`).
    pub fn with_dim(dim: usize) -> Vectors {
        Vectors { dim, file: Vec::new(), values: Vec::new(), index: HashMap::new() }
    }

    fn load_from(db: &Db, table: &str) -> Result<Vectors, String> {
        let f = |e: rusqlite::Error| e.to_string();
        let mut v = Vectors { dim: 512, file: Vec::new(), values: Vec::new(), index: HashMap::new() };
        let mut st = db.conn.prepare(&format!("SELECT file, vec FROM {table}")).map_err(f)?;
        let mut rows = st.query([]).map_err(f)?;
        while let Some(r) = rows.next().map_err(f)? {
            let (d, blob): (i64, Vec<u8>) = (r.get(0).map_err(f)?, r.get(1).map_err(f)?);
            if blob.len() != v.dim * 4 {
                continue;
            }
            v.index.insert(d, v.file.len());
            v.file.push(d);
            v.values.extend(blob.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        }
        Ok(v)
    }

    /// Test aid (`GRAINEO_SEM_FAKE`): add `n` noisy copies of the existing vectors under file IDs
    /// from 1_000_000_000 on (no photo behind them) to simulate the load of a big library.
    pub fn pad_for_test(&mut self, n: usize) {
        let base = self.values.clone();
        let count = self.file.len();
        if count == 0 {
            return;
        }
        let mut seed = 12345u64;
        for k in 0..n {
            let src = &base[(k % count) * self.dim..(k % count + 1) * self.dim];
            let mut v: Vec<f32> = src.to_vec();
            for x in v.iter_mut() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                *x += ((seed >> 40) as f32 / 16777216.0 - 0.5) * 0.08;
            }
            let nrm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= nrm);
            self.insert_into(1_000_000_000 + k as i64, &v);
        }
    }

    /// Empty set (for measurements and tests; fill with `insert_into`).
    pub fn empty() -> Vectors {
        Vectors { dim: 512, file: Vec::new(), values: Vec::new(), index: HashMap::new() }
    }

    /// Most similar files to a query vector (e.g. text), descending.
    pub fn next_item(&self, q: &[f32], k: usize) -> Vec<(i64, f32)> {
        let mut s: Vec<(i64, f32)> = self.file.iter().enumerate().map(|(j, d)| (*d, self.values[j * self.dim..(j + 1) * self.dim].iter().zip(q).map(|(a, b)| a * b).sum::<f32>())).collect();
        let k = k.min(s.len());
        if k == 0 {
            return s;
        }
        s.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
        s.truncate(k);
        s.sort_by(|a, b| b.1.total_cmp(&a.1));
        s
    }

    /// Similarity of the query to every real vector (zero vectors skipped): (file, cosine).
    pub fn all_similarities(&self, q: &[f32]) -> Vec<(i64, f32)> {
        self.file
            .iter()
            .enumerate()
            .filter_map(|(j, d)| {
                let v = &self.values[j * self.dim..(j + 1) * self.dim];
                v.iter().any(|x| x.abs() > 1e-6).then(|| (*d, v.iter().zip(q).map(|(a, b)| a * b).sum::<f32>()))
            })
            .collect()
    }

    pub fn count(&self) -> usize {
        self.file.len()
    }

    pub fn has(&self, file: i64) -> bool {
        self.index.contains_key(&file)
    }

    /// Does the file have a usable vector? Zero vectors stand for files whose preview
    /// could not be read during computation.
    pub fn has_real(&self, file: i64) -> bool {
        self.index.get(&file).is_some_and(|&i| self.values[i * self.dim..(i + 1) * self.dim].iter().any(|x| x.abs() > 1e-6))
    }

    /// Set or replace a file's vector (after a computation, without reloading).
    pub fn insert_into(&mut self, file: i64, v: &[f32]) {
        if v.len() != self.dim {
            return;
        }
        match self.index.get(&file) {
            Some(&i) => self.values[i * self.dim..(i + 1) * self.dim].copy_from_slice(v),
            None => {
                self.index.insert(file, self.file.len());
                self.file.push(file);
                self.values.extend_from_slice(v);
            }
        }
    }

    /// Cosine similarity of two files (`None` if one has no vector).
    pub fn similarity(&self, a: i64, b: i64) -> Option<f32> {
        let (i, j) = (*self.index.get(&a)?, *self.index.get(&b)?);
        let (x, y) = (&self.values[i * self.dim..(i + 1) * self.dim], &self.values[j * self.dim..(j + 1) * self.dim]);
        Some(x.iter().zip(y).map(|(p, q)| p * q).sum())
    }

    /// The `k` most similar files (excluding the file itself), descending by cosine similarity.
    pub fn similar_items(&self, file: i64, k: usize) -> Vec<(i64, f32)> {
        let Some(&i) = self.index.get(&file) else { return Vec::new() };
        let q = &self.values[i * self.dim..(i + 1) * self.dim];
        let mut s: Vec<(i64, f32)> = self
            .file
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(j, d)| (*d, self.values[j * self.dim..(j + 1) * self.dim].iter().zip(q).map(|(a, b)| a * b).sum::<f32>()))
            .collect();
        let k = k.min(s.len());
        if k == 0 {
            return s;
        }
        s.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
        s.truncate(k);
        s.sort_by(|a, b| b.1.total_cmp(&a.1));
        s
    }
}

/// Save an own image vector.
pub fn save(db: &Db, file: i64, v: &[f32]) -> Result<(), String> {
    let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    db.conn
        .execute("INSERT INTO image_vector(file, model, source, vec) VALUES(?1, 'clip-ViT-B-32', 'own', ?2) ON CONFLICT(file) DO UPDATE SET vec=excluded.vec, source='own'", rusqlite::params![file, blob])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Existing files without an image vector (for the image encoder in the background).
pub fn without_vector(db: &Db, max: usize) -> Vec<(i64, PathBuf, String)> {
    let Ok(mut st) = db.conn.prepare(
        "SELECT d.id, s.path || o.path || d.name, d.format FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root
         WHERE d.status = 0 AND d.format <> 'VIDEO' AND d.id NOT IN (SELECT file FROM image_vector) LIMIT ?1",
    ) else {
        return Vec::new();
    };
    st.query_map([max as i64], |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?), r.get(2)?))).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Path and format of a file (to compute its image vector).
pub fn source_of(db: &Db, file: i64) -> Option<(PathBuf, String)> {
    db.conn
        .query_row(
            "SELECT s.path || o.path || d.name, d.format FROM file d JOIN folder o ON o.id=d.folder JOIN root_folder s ON s.id=o.root WHERE d.id = ?1",
            [file],
            |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get(1)?)),
        )
        .ok()
}

/// Hybrid semantic ranking: image and AI text vectors with 500 candidates each,
/// merged by Reciprocal Rank Fusion (k = 60). Returns: (file, score, image similarity, text similarity).
pub fn hybrid(image: &Vectors, text: Option<&Vectors>, q: &[f32], max: usize) -> Vec<(i64, f32, f32, f32)> {
    const CAND: usize = 500;
    const K: f32 = 60.0;
    let mut m: HashMap<i64, (f32, f32, f32)> = HashMap::new();
    for (r, (d, s)) in image.next_item(q, CAND).into_iter().enumerate() {
        let e = m.entry(d).or_insert((0.0, 0.0, 0.0));
        e.0 += 1.0 / (K + r as f32);
        e.1 = s;
    }
    if let Some(t) = text {
        for (r, (d, s)) in t.next_item(q, CAND).into_iter().enumerate() {
            let e = m.entry(d).or_insert((0.0, 0.0, 0.0));
            e.0 += 1.0 / (K + r as f32);
            e.2 = s;
        }
    }
    let mut v: Vec<(i64, f32, f32, f32)> = m.into_iter().map(|(d, (s, b, t))| (d, s, b, t)).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.truncate(max);
    v
}

/// One semantic hit with a library-size independent relevance value.
#[derive(Clone, Copy, Debug)]
pub struct Scored {
    pub file: i64,
    /// Relevance: standardized similarity (distance to the mean of ALL vectors in units of
    /// their standard deviation); the better of image and description vector.
    pub z: f32,
    /// Raw cosine similarities (image, description); 0 if there is no vector.
    pub image_sim: f32,
    pub text_sim: f32,
}

fn standardize(all: &[(i64, f32)]) -> (f32, f32) {
    let n = all.len().max(1) as f32;
    let mean = all.iter().map(|x| x.1).sum::<f32>() / n;
    let var = all.iter().map(|x| (x.1 - mean).powi(2)).sum::<f32>() / n;
    (mean, var.sqrt().max(1e-4))
}

/// Semantic ranking with relevance values: for image and description vectors the cosine
/// similarity of every photo is standardized against the distribution over the whole library
/// (z value, independent of the library size); a photo counts with the better of both.
/// Result: descending by `z`, at most `max` entries (the cut by relevance is `keep`).
pub fn scored(image: &Vectors, text: Option<&Vectors>, q: &[f32], max: usize) -> Vec<Scored> {
    let mut m: HashMap<i64, Scored> = HashMap::new();
    let mut put = |v: &Vectors, is_text: bool| {
        let all = v.all_similarities(q);
        let (mean, sd) = standardize(&all);
        for (d, s) in all {
            let z = (s - mean) / sd;
            let e = m.entry(d).or_insert(Scored { file: d, z: f32::NEG_INFINITY, image_sim: 0.0, text_sim: 0.0 });
            e.z = e.z.max(z);
            if is_text {
                e.text_sim = s;
            } else {
                e.image_sim = s;
            }
        }
    };
    put(image, false);
    if let Some(t) = text {
        put(t, true);
    }
    let mut v: Vec<Scored> = m.into_values().collect();
    v.sort_by(|a, b| b.z.total_cmp(&a.z).then(a.file.cmp(&b.file)));
    v.truncate(max);
    v
}

/// Default strictness of the semantic search (setting `semanticStrictness`, 0…1).
pub const DEFAULT_STRICTNESS: f32 = 0.3;

/// Which of the `scored` hits stay at the given strictness (0 = lenient … 1 = strict) in a
/// library of `library_size` photos. Rule: `z` at least `floor` (absolute position in the
/// distribution, raised slowly with the library size because chance maxima grow with it) and
/// at most `gap` below the fifth-best hit (a lone outlier on top must not cut the others off).
/// The hits are descending, the result is the number of leading hits to keep (0 = none).
pub fn keep(list: &[Scored], strictness: f32, library_size: usize) -> usize {
    let Some(best) = list.first().map(|s| s.z) else { return 0 };
    let (floor, gap) = limits(strictness, library_size);
    if best < floor {
        return 0;
    }
    let reference = list[(list.len() - 1).min(4)].z;
    list.iter().take_while(|s| s.z >= floor && s.z >= reference - gap).count()
}

/// (absolute lower bound of z, maximum distance to the fifth-best hit) for a strictness 0…1.
pub fn limits(strictness: f32, library_size: usize) -> (f32, f32) {
    let t = strictness.clamp(0.0, 1.0);
    // expected maximum of N chance values grows like sqrt(2 ln N); measured at N = 131 (reference 3.12)
    let growth = (2.0 * (library_size.max(2) as f32).ln()).sqrt() - 3.12;
    (2.8 + 1.6 * t + 0.5 * growth.max(-0.5), 3.0 - 1.5 * t)
}

/// Value to show for a hit: percent of the best hit's z (100 = best hit).
pub fn percent(z: f32, best: f32) -> u32 {
    if best <= 0.0 {
        return 0;
    }
    (100.0 * z / best).clamp(0.0, 100.0).round() as u32
}

/// Description vectors for queries of the given text model: always those of the standard model
/// (`text_vector`; see `semantic::description_model`), completed by the model's own rows in `text_vector_model` for
/// descriptions that have no standard vector.
pub fn load_text_model(db: &Db, m: crate::semantic::TextModel) -> Result<Vectors, String> {
    let mut v = Vectors::load_text(db)?;
    if m == crate::semantic::TextModel::Standard {
        return Ok(v);
    }
    let f = |e: rusqlite::Error| e.to_string();
    let mut st = db.conn.prepare("SELECT file, vec FROM text_vector_model WHERE model = ?1").map_err(f)?;
    let mut rows = st.query([m.model_name()]).map_err(f)?;
    while let Some(r) = rows.next().map_err(f)? {
        let (d, blob): (i64, Vec<u8>) = (r.get(0).map_err(f)?, r.get(1).map_err(f)?);
        if blob.len() == v.dim * 4 && !v.has(d) {
            v.insert_into(d, &blob.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect::<Vec<f32>>());
        }
    }
    Ok(v)
}

/// AI descriptions without a vector of the given model: (file, text to embed), at most `max`.
pub fn descriptions_without_vector(db: &Db, m: crate::semantic::TextModel, max: usize) -> Vec<(i64, String)> {
    let sql = if m == crate::semantic::TextModel::Standard {
        "SELECT k.file, coalesce(k.keywords,'[]'), coalesce(k.scene,''), coalesce(k.reason,'') FROM ai k WHERE ?1 <> '' AND k.file NOT IN (SELECT file FROM text_vector) LIMIT ?2"
    } else {
        "SELECT k.file, coalesce(k.keywords,'[]'), coalesce(k.scene,''), coalesce(k.reason,'') FROM ai k WHERE k.file NOT IN (SELECT file FROM text_vector_model WHERE model = ?1) LIMIT ?2"
    };
    let Ok(mut st) = db.conn.prepare(sql) else { return Vec::new() };
    st.query_map(rusqlite::params![m.model_name(), max as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))
        .map(|r| r.flatten().map(|(d, k, s, b)| (d, crate::semantic::description_text(&k, &s, &b))).collect())
        .unwrap_or_default()
}

/// AI descriptions with a vector of the given model and in total.
pub fn description_vector_counts(db: &Db, m: crate::semantic::TextModel) -> (i64, i64) {
    let n = |sql: &str, p: &[&dyn rusqlite::ToSql]| db.conn.query_row(sql, p, |r| r.get::<_, i64>(0)).unwrap_or(0);
    let total = n("SELECT count(*) FROM ai", &[]);
    let done = if m == crate::semantic::TextModel::Standard {
        n("SELECT count(*) FROM ai WHERE file IN (SELECT file FROM text_vector)", &[])
    } else {
        n("SELECT count(*) FROM ai WHERE file IN (SELECT file FROM text_vector_model WHERE model = ?1)", &[&m.model_name()])
    };
    (done, total)
}

/// Save a description vector of the given model.
pub fn save_text_model(db: &Db, file: i64, m: crate::semantic::TextModel, v: &[f32]) -> Result<(), String> {
    if m == crate::semantic::TextModel::Standard {
        return save_text(db, file, v);
    }
    let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    db.conn
        .execute("INSERT OR REPLACE INTO text_vector_model(file, model, vec) VALUES(?1, ?2, ?3)", rusqlite::params![file, m.model_name(), blob])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Save an own description vector.
pub fn save_text(db: &Db, file: i64, v: &[f32]) -> Result<(), String> {
    let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    db.conn
        .execute("INSERT OR REPLACE INTO text_vector(file, model, source, vec) VALUES(?1, 'clip-ViT-B-32-multilingual-v1', 'own', ?2)", rusqlite::params![file, blob])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; 512];
        v[i] = 1.0;
        v
    }

    #[test]
    fn scored_and_keep() {
        // 200 vectors on axes 0..199 mixed with a little of axis 0, the query is axis 0
        let mut v = Vectors::empty();
        for i in 0..200usize {
            let mut x = unit(1 + i % 300);
            x[0] = if i < 3 { 0.9 } else { 0.01 * (i % 7) as f32 };
            let n = x.iter().map(|a| a * a).sum::<f32>().sqrt();
            x.iter_mut().for_each(|a| *a /= n);
            v.insert_into(i as i64, &x);
        }
        let list = scored(&v, None, &unit(0), 500);
        assert_eq!(list.len(), 200);
        assert!(list.windows(2).all(|w| w[0].z >= w[1].z));
        // the three real hits stay at a lenient setting, nothing else
        assert_eq!(keep(&list, 0.0, v.count()), 3);
        // nothing matches: no forced hits
        let none = scored(&v, None, &unit(400), 500);
        assert_eq!(keep(&none, 0.3, v.count()), 0);
        // stricter never keeps more
        assert!(keep(&list, 1.0, v.count()) <= keep(&list, 0.0, v.count()));
        assert_eq!(percent(2.0, 4.0), 50);
    }
}
