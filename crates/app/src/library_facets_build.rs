//! Building the column filters ([`Facets::build`]) from the rows of the library, the keywords and their assignments.
//!
//! The values of the text columns are looked up borrowed (`&str`, no string per row), the small number columns (year,
//! month, day, weekday, time of day, stars …) by their number in a table; the keyword list of a row comes from the
//! chain of each assigned keyword (the term and its parents), computed once per keyword instead of walking the parents
//! per assignment. The result is the same as before (`library_facets_bench.rs` compares it with the earlier
//! algorithm). [`inputs_fingerprint`] tells whether a rebuild would give the same filters at all.

use super::*;
use std::hash::{Hash, Hasher};
use std::time::Instant;

/// Time per phase of a build (ms), for the log.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuildTimes {
    /// value per row and column
    pub rows: f64,
    /// keyword and person lists per row (with the parents)
    pub keywords: f64,
    /// number lists (aperture, focal length, ISO, shutter speed)
    pub numbers: f64,
}

/// Index of a value in a column with few possible values (`slot` = the value as a number): the name is made only
/// for the first row carrying it.
fn small_value(table: &mut [u32], slot: usize, names: &mut Vec<String>, name: impl FnOnce() -> String) -> u32 {
    let i = &mut table[slot];
    if *i == NO_VALUE {
        *i = names.len() as u32;
        names.push(name());
    }
    *i
}

/// Index of a text value (borrowed from the row; the name is copied once per distinct value).
fn text_value<'a>(dict: &mut HashMap<&'a str, u32>, s: &'a str, names: &mut Vec<String>) -> u32 {
    *dict.entry(s).or_insert_with(|| {
        names.push(s.to_string());
        names.len() as u32 - 1
    })
}

impl Facets {
    /// [`Facets::build`] with the time of its phases.
    pub fn build_timed(rows: &[Row], keywords: &[Keyword], keywords_per_image: &HashMap<i64, Vec<i64>>) -> (Self, BuildTimes) {
        let mut times = BuildTimes::default();
        let t = Instant::now();
        let mut names: [Vec<String>; N] = Default::default();
        let (mut cameras, mut lenses, mut formats, mut places) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
        let mut years: HashMap<u16, u32> = HashMap::new();
        let mut months = [NO_VALUE; 13];
        let mut days = [NO_VALUE; 32];
        let mut weekdays = [NO_VALUE; 8];
        let mut times_of_day = [NO_VALUE; 4];
        let mut qualities = [NO_VALUE; 3];
        let mut labels = [NO_VALUE; 6];
        let mut stars = [NO_VALUE; 256];
        let mut values = Vec::with_capacity(rows.len());
        for z in rows {
            let mut w = [NO_VALUE; N];
            if let Some(k) = &z.camera {
                w[0] = text_value(&mut cameras, k, &mut names[0]);
            }
            if let Some(o) = &z.lens {
                w[1] = text_value(&mut lenses, o, &mut names[1]);
            }
            if let Some(o) = &z.place {
                w[PLACE_COL] = text_value(&mut places, o, &mut names[PLACE_COL]);
            }
            if let Some(j) = z.year() {
                let next = names[2].len() as u32;
                w[2] = *years.entry(j).or_insert_with(|| {
                    names[2].push(j.to_string());
                    next
                });
            }
            if let Some(month) = z.month().filter(|m| (1..=12).contains(m)) {
                w[9] = small_value(&mut months, month as usize, &mut names[9], || format!("{month:02}"));
            }
            if let Some(day) = z.day() {
                w[10] = small_value(&mut days, day as usize, &mut names[10], || format!("{day:02}"));
            }
            // Weekday (1 Monday … 7 Sunday) and time of day (0 night … 3 evening)
            if let Some(d) = library::search::weekday_of(z) {
                w[WEEKDAY_COL] = small_value(&mut weekdays, d as usize, &mut names[WEEKDAY_COL], || d.to_string());
            }
            if let Some(h) = library::search::hour_of(z) {
                let part = library::search_fields::time_of_day(h);
                w[TIME_COL] = small_value(&mut times_of_day, part as usize, &mut names[TIME_COL], || part.to_string());
            }
            if let Some(q) = z.ai_quality {
                let q = q as usize % 3;
                w[4] = small_value(&mut qualities, q, &mut names[4], || ["Ausschuss", "mittel", "gut"][q].to_string());
            }
            w[5] = text_value(&mut formats, z.format.as_str(), &mut names[5]);
            let label = z.label as usize % 6;
            w[6] = small_value(&mut labels, label, &mut names[6], || library::label::NAMES[label].to_string());
            let s = z.stars;
            w[7] = small_value(&mut stars, s as usize, &mut names[7], || if s == 0 { "ohne".into() } else { "★".repeat(s as usize) });
            values.push(w);
        }
        times.rows = ms_since(t);

        let t = Instant::now();
        // People are in the "Person" column, not in the keyword column
        let mut keyword_index: HashMap<i64, u32> = HashMap::with_capacity(keywords.len());
        let mut person_index: HashMap<i64, u32> = HashMap::new();
        let mut kw_ids = Vec::new();
        for s in keywords {
            let (index, k) = if s.person { (&mut person_index, PERSON_COL) } else { (&mut keyword_index, KEYWORD_COL) };
            index.insert(s.id, names[k].len() as u32);
            names[k].push(s.name.clone());
            if !s.person {
                kw_ids.push((s.id, s.parent.is_some()));
            }
        }
        // hierarchy: a parent term includes its children (a photo with "Rom" also counts for "Italien" and "Orte", once
        // each); the parents of a person (Lightroom "Personen › Christa") count as keywords as well. The chain of a
        // keyword (itself and its parents, as column values, at most 65 steps against cycles) is computed once.
        let parent_of: HashMap<i64, Option<i64>> = keywords.iter().map(|s| (s.id, s.parent)).collect();
        let mut chains: HashMap<i64, Vec<u32>> = HashMap::with_capacity(parent_of.len());
        for &id in parent_of.keys() {
            let mut chain: Vec<u32> = Vec::new();
            let mut cur = Some(id);
            let mut steps = 0;
            while let Some(k) = cur {
                if let Some(&i) = keyword_index.get(&k) {
                    if !chain.contains(&i) {
                        chain.push(i);
                    }
                }
                cur = parent_of.get(&k).copied().flatten();
                steps += 1;
                if steps > 64 {
                    break;
                }
            }
            chains.insert(id, chain);
        }
        let kw_parent: Vec<Option<u32>> = keywords.iter().filter(|s| !s.person).map(|s| s.parent.and_then(|p| keyword_index.get(&p).copied())).collect();
        let kw_direct: Vec<usize> = keywords.iter().filter(|s| !s.person).map(|s| s.count).collect();
        let kw_ambiguous: HashSet<u32> = {
            let mut seen: HashMap<String, Vec<u32>> = HashMap::new();
            for (i, n) in names[KEYWORD_COL].iter().enumerate() {
                seen.entry(n.trim().to_lowercase()).or_default().push(i as u32);
            }
            seen.into_values().filter(|v| v.len() > 1).flatten().collect()
        };
        let mut kw = Vec::with_capacity(rows.len());
        let mut persons = Vec::with_capacity(rows.len());
        for z in rows {
            let assigned = keywords_per_image.get(&z.image).map(Vec::as_slice).unwrap_or_default();
            let mut list: Vec<u32> = Vec::new();
            let mut people: Vec<u32> = Vec::new();
            for id in assigned {
                if let Some(chain) = chains.get(id) {
                    for &i in chain {
                        if !list.contains(&i) {
                            list.push(i);
                        }
                    }
                }
                if let Some(&p) = person_index.get(id) {
                    people.push(p);
                }
            }
            kw.push(list);
            persons.push(people);
        }
        times.keywords = ms_since(t);

        let t = Instant::now();
        // number lists: only the values or groups that occur, ascending; "(none)" last
        let mut bands: [Vec<Band>; 4] = Default::default();
        let mut band_rows = vec![[NO_VALUE; 4]; rows.len()];
        for k in 0..4 {
            // key per row once; only the distinct keys are sorted
            let row_keys: Vec<Option<i64>> = rows.iter().map(|z| range_value(z, k).filter(|v| v.is_finite() && *v > 0.0).map(|v| band_key(k, v))).collect();
            let mut keys: Vec<i64> = row_keys.iter().flatten().copied().collect();
            keys.sort_unstable();
            keys.dedup();
            keys.sort_by(|a, b| library::search::key_value(FIELDS[k], *a).total_cmp(&library::search::key_value(FIELDS[k], *b)));
            let index: HashMap<i64, u32> = keys.iter().enumerate().map(|(i, key)| (*key, i as u32)).collect();
            bands[k] = keys.iter().map(|key| band_of(k, *key)).collect();
            for (key, b) in row_keys.iter().zip(band_rows.iter_mut()) {
                if let Some(key) = key {
                    b[k] = index[key];
                }
            }
        }
        times.numbers = ms_since(t);
        let facets = Self {
            names,
            values,
            kw,
            persons,
            selection: Default::default(),
            numbers: Default::default(),
            bands,
            band_rows,
            band_selection: Default::default(),
            band_numbers: Default::default(),
            anchor: [None; N + 4],
            search: Default::default(),
            slots: load_slots(),
            kw_ids,
            kw_parent,
            kw_direct,
            kw_ambiguous,
            keyword_request: None,
            fingerprint: 0,
        };
        (facets, times)
    }
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Fingerprint of everything [`Facets::build`] reads: equal fingerprints give equal filters, so a library load that
/// changed nothing of it keeps the finished filters (a few milliseconds instead of a rebuild). Row order counts (the
/// filters address rows by position).
pub fn inputs_fingerprint(rows: &[Row], keywords: &[Keyword], keywords_per_image: &HashMap<i64, Vec<i64>>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rows.len().hash(&mut h);
    for z in rows {
        z.image.hash(&mut h);
        z.camera.hash(&mut h);
        z.lens.hash(&mut h);
        z.place.hash(&mut h);
        z.capture.hash(&mut h);
        z.format.hash(&mut h);
        (z.stars, z.label, z.ai_quality).hash(&mut h);
        for v in [z.aperture, z.focal, z.time] {
            v.map(f64::to_bits).hash(&mut h);
        }
        z.iso.hash(&mut h);
        keywords_per_image.get(&z.image).map(Vec::as_slice).unwrap_or_default().hash(&mut h);
    }
    keywords.len().hash(&mut h);
    for s in keywords {
        (s.id, &s.name, s.parent, s.count, s.person).hash(&mut h);
    }
    h.finish()
}
