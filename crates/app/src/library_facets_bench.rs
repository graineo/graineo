//! Tests and measurements of the column filter build ([`Facets::build`]): synthetic libraries of realistic size, the
//! previous algorithm as a reference (the results must stay identical), and timings per phase.
//!
//! Measurements (release, output on stderr):
//! `cargo test --release -p graineo facets_bench -- --ignored --nocapture`
//! With `GRAINEO_FACETS_BENCH_DB=<copy of a library.sqlite>` the measurement also reads a real library (read-only).

use super::*;
use std::time::Instant;

/// Small deterministic generator (no extra dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    /// Skewed choice: small indices are much more frequent (like real cameras and keywords).
    fn skewed(&mut self, n: u64) -> u64 {
        let a = self.below(n);
        let b = self.below(n);
        a.min(b).min(self.below(n))
    }
}

pub(crate) struct Synthetic {
    pub rows: Vec<Row>,
    pub keywords: Vec<Keyword>,
    pub assignments: HashMap<i64, Vec<i64>>,
}

/// A library like the user's: `n` photos, ~40 cameras, ~200 lenses, 2005–2026, 11 000 keywords (177 with a parent,
/// 164 people, some names twice under different parents), `n * 5.2` keyword assignments.
pub(crate) fn synthetic(n: usize, seed: u64) -> Synthetic {
    let mut g = Lcg(seed);
    let cameras: Vec<String> = (0..40).map(|i| format!("Camera Model {i}")).collect();
    let lenses: Vec<String> = (0..200).map(|i| format!("Lens {}-{} mm F{}", 10 + i, 20 + i * 2, 1.4 + (i % 5) as f64)).collect();
    let formats = ["CR2", "CR3", "RAF", "DNG", "ARW", "JPG", "HEIC", "TIFF", "PNG", "X3F"];
    let apertures = [1.4, 1.8, 2.0, 2.8, 4.0, 5.6, 8.0, 11.0, 16.0, 2.2, 3.5, 6.3];
    let focals = [14.0, 24.0, 28.0, 35.0, 50.0, 85.0, 135.0, 200.0, 41.0, 30.0];
    let times = [1.0 / 4000.0, 1.0 / 1000.0, 1.0 / 250.0, 1.0 / 125.0, 1.0 / 60.0, 1.0 / 30.0, 0.5, 2.0, 30.0];
    let rows: Vec<Row> = (0..n)
        .map(|i| {
            let mut z = Row { image: i as i64 * 3 + 7, file: i as i64, folder: 1, name: format!("IMG_{i:05}.CR2"), ..Default::default() };
            z.format = formats[g.skewed(formats.len() as u64) as usize].to_string();
            if g.below(50) != 0 {
                z.camera = Some(cameras[g.skewed(40) as usize].clone());
            }
            if g.below(10) != 0 {
                z.lens = Some(lenses[g.skewed(200) as usize].clone());
            }
            if g.below(40) != 0 {
                let (y, m, d, h, mi, s) = (2005 + g.below(22), 1 + g.below(12), 1 + g.below(28), g.below(24), g.below(60), g.below(60));
                z.capture = Some(format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}"));
            }
            z.stars = g.below(6) as u8 * (g.below(3) == 0) as u8;
            z.label = if g.below(5) == 0 { g.below(6) as u8 } else { 0 };
            z.flag = g.below(3) as i8 - 1;
            z.ai_quality = (g.below(3) == 0).then(|| g.below(3) as u8);
            z.aperture = (g.below(8) != 0).then(|| apertures[g.below(apertures.len() as u64) as usize]);
            z.focal = (g.below(8) != 0).then(|| focals[g.below(focals.len() as u64) as usize] + (g.below(20) == 0) as u8 as f64 * 0.5);
            z.iso = (g.below(8) != 0).then(|| [100, 200, 400, 800, 1600, 3200, 6400, 125, 160][g.below(9) as usize]);
            z.time = (g.below(8) != 0).then(|| times[g.below(times.len() as u64) as usize]);
            z
        })
        .collect();
    // keywords: 200 roots/places first (parents), 177 children, 164 people (half below "Personen"), the rest flat
    let mut keywords: Vec<Keyword> = Vec::new();
    let mut id = 1i64;
    let mut push = |name: String, parent: Option<i64>, person: bool, keywords: &mut Vec<Keyword>| -> i64 {
        keywords.push(Keyword { id, name, parent, count: 0, person });
        id += 1;
        id - 1
    };
    let places = push("Orte".into(), None, false, &mut keywords);
    let people_root = push("Personen".into(), None, false, &mut keywords);
    let mut parents = vec![places];
    for i in 0..20 {
        parents.push(push(format!("Land {i}"), Some(places), false, &mut keywords));
    }
    // 177 with a parent in total (the 20 countries above + 157 below them, some repeated names "Paris")
    for i in 0..157 {
        let p = parents[1 + (i % 20)];
        let name = if i % 25 == 0 { "Paris".to_string() } else { format!("Stadt {i}") };
        push(name, Some(p), false, &mut keywords);
    }
    for i in 0..164 {
        // people: half without parent, half below "Personen" (counted in "Personen" via the person's parent)
        push(format!("Person {i}"), None, true, &mut keywords);
    }
    let _ = people_root;
    while keywords.len() < 11_000 {
        let i = keywords.len();
        let name = if i % 997 == 0 { "paris ".to_string() } else { format!("Stichwort {i}") };
        push(name, None, false, &mut keywords);
    }
    // the parent of a person (Lightroom "Personen › Christa"): some people get one
    for k in keywords.iter_mut().filter(|k| k.person).step_by(2) {
        k.parent = Some(people_root);
    }
    let n_kw = keywords.len() as u64;
    let mut assignments: HashMap<i64, Vec<i64>> = HashMap::new();
    let total = n * 52 / 10;
    for _ in 0..total {
        let image = rows[g.below(n as u64) as usize].image;
        let k = keywords[g.skewed(n_kw) as usize].id;
        let list = assignments.entry(image).or_default();
        if !list.contains(&k) {
            list.push(k);
        }
    }
    // an unknown keyword ID (deleted meanwhile) on one photo
    assignments.entry(rows[0].image).or_default().push(999_999);
    for k in keywords.iter_mut() {
        k.count = (k.id as usize * 7) % 50;
    }
    Synthetic { rows, keywords, assignments }
}

/// The algorithm before 16.11.2026 (reference): the new build must give exactly this. Returns the facets and the
/// times of its phases (ms): rows, keyword index, keyword lists per row (parents), people, number lists.
pub(crate) fn build_reference(rows: &[Row], keywords: &[Keyword], keywords_per_image: &HashMap<i64, Vec<i64>>) -> (Facets, [f64; 5]) {
    let mut times = [0.0; 5];
    let mut t = Instant::now();
    let mut lap = |i: usize, t: &mut Instant| {
        times[i] = t.elapsed().as_secs_f64() * 1000.0;
        *t = Instant::now();
    };
    let mut names: [Vec<String>; N] = Default::default();
    let mut dict: [HashMap<String, u32>; N] = Default::default();
    let mut wid = |k: usize, s: String, names: &mut [Vec<String>; N]| -> u32 {
        if let Some(&i) = dict[k].get(&s) {
            return i;
        }
        let i = names[k].len() as u32;
        names[k].push(s.clone());
        dict[k].insert(s, i);
        i
    };
    let mut values = Vec::with_capacity(rows.len());
    for z in rows {
        let mut w = [NO_VALUE; N];
        if let Some(k) = &z.camera {
            w[0] = wid(0, k.clone(), &mut names);
        }
        if let Some(o) = &z.lens {
            w[1] = wid(1, o.clone(), &mut names);
        }
        if let Some(j) = z.year() {
            w[2] = wid(2, j.to_string(), &mut names);
        }
        if let Some(month) = z.month().filter(|m| (1..=12).contains(m)) {
            w[9] = wid(9, format!("{month:02}"), &mut names);
        }
        if let Some(day) = z.day() {
            w[10] = wid(10, format!("{day:02}"), &mut names);
        }
        if let Some(d) = library::search::weekday_of(z) {
            w[WEEKDAY_COL] = wid(WEEKDAY_COL, d.to_string(), &mut names);
        }
        if let Some(h) = library::search::hour_of(z) {
            w[TIME_COL] = wid(TIME_COL, library::search_fields::time_of_day(h).to_string(), &mut names);
        }
        if let Some(q) = z.ai_quality {
            w[4] = wid(4, ["Ausschuss", "mittel", "gut"][q as usize % 3].to_string(), &mut names);
        }
        w[5] = wid(5, z.format.as_str().to_string(), &mut names);
        w[6] = wid(6, library::label::NAMES[z.label as usize % 6].to_string(), &mut names);
        w[7] = wid(7, if z.stars == 0 { "ohne".into() } else { "★".repeat(z.stars as usize) }, &mut names);
        values.push(w);
    }
    lap(0, &mut t);
    let mut keyword_index: HashMap<i64, u32> = HashMap::new();
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
    let row_list = |index: &HashMap<i64, u32>, image: i64| -> Vec<u32> { keywords_per_image.get(&image).map(|v| v.iter().filter_map(|s| index.get(s).copied()).collect()).unwrap_or_default() };
    let parent_of: HashMap<i64, Option<i64>> = keywords.iter().map(|s| (s.id, s.parent)).collect();
    let kw_parent: Vec<Option<u32>> = keywords.iter().filter(|s| !s.person).map(|s| s.parent.and_then(|p| keyword_index.get(&p).copied())).collect();
    let kw_direct: Vec<usize> = keywords.iter().filter(|s| !s.person).map(|s| s.count).collect();
    let kw_ambiguous: HashSet<u32> = {
        let mut seen: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, n) in names[KEYWORD_COL].iter().enumerate() {
            seen.entry(n.trim().to_lowercase()).or_default().push(i as u32);
        }
        seen.into_values().filter(|v| v.len() > 1).flatten().collect()
    };
    lap(1, &mut t);
    let kw = rows
        .iter()
        .map(|z| {
            let mut list: Vec<u32> = Vec::new();
            for &id in keywords_per_image.get(&z.image).map(Vec::as_slice).unwrap_or_default() {
                let mut cur = Some(id);
                let mut steps = 0;
                while let Some(k) = cur {
                    if let Some(&i) = keyword_index.get(&k) {
                        if !list.contains(&i) {
                            list.push(i);
                        }
                    }
                    cur = parent_of.get(&k).copied().flatten();
                    steps += 1;
                    if steps > 64 {
                        break;
                    }
                }
            }
            list
        })
        .collect();
    lap(2, &mut t);
    let persons = rows.iter().map(|z| row_list(&person_index, z.image)).collect();
    lap(3, &mut t);
    let mut bands: [Vec<Band>; 4] = Default::default();
    let mut band_rows = vec![[NO_VALUE; 4]; rows.len()];
    for k in 0..4 {
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
    lap(4, &mut t);
    let f = Facets {
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
        slots: Vec::new(),
        kw_ids,
        kw_parent,
        kw_direct,
        kw_ambiguous,
        keyword_request: None,
        fingerprint: 0,
    };
    (f, times)
}

/// Everything a build computes must be equal (the selection state is not part of a build).
fn assert_same_build(a: &Facets, b: &Facets) {
    assert_eq!(a.names, b.names, "value names");
    assert_eq!(a.values, b.values, "value per row");
    assert_eq!(a.kw, b.kw, "keywords per row (with parents)");
    assert_eq!(a.persons, b.persons, "people per row");
    assert_eq!(a.bands, b.bands, "number lists");
    assert_eq!(a.band_rows, b.band_rows, "number list row per photo");
    assert_eq!(a.kw_ids, b.kw_ids);
    assert_eq!(a.kw_parent, b.kw_parent);
    assert_eq!(a.kw_direct, b.kw_direct);
    assert_eq!(a.kw_ambiguous, b.kw_ambiguous);
}

/// Apply a set of selections on both and compare result rows and all counts.
fn assert_same_filtering(a: &mut Facets, b: &mut Facets, rows: &[Row]) {
    let all: Vec<usize> = (0..rows.len()).collect();
    let some: Vec<usize> = (0..rows.len()).step_by(1387).collect();
    let selections: [&dyn Fn(&mut Facets); 6] = [
        &|_f| {},
        &|f| {
            f.selection[0].insert(0);
            f.selection[0].insert(3);
        },
        &|f| {
            f.selection[KEYWORD_COL].insert(0);
            f.selection[2].insert(NO_VALUE);
        },
        &|f| {
            f.selection[PERSON_COL].insert(1);
            if f.bands[0].len() > 2 {
                f.band_selection[0].insert(2);
            }
        },
        &|f| {
            f.selection[KEYWORD_COL].insert(5);
            f.selection[KEYWORD_COL].insert(250);
            f.selection[7].insert(0);
        },
        &|f| {
            f.selection[WEEKDAY_COL].insert(0);
            f.selection[TIME_COL].insert(1);
            f.band_selection[3].insert(NO_VALUE);
        },
    ];
    for (n, choose) in selections.iter().enumerate() {
        for basis in [&all, &some] {
            a.reset();
            b.reset();
            choose(a);
            choose(b);
            assert_eq!(a.apply(rows, basis), b.apply(rows, basis), "result of selection {n}");
            assert_eq!(a.numbers, b.numbers, "counts of selection {n}");
            assert_eq!(a.band_numbers, b.band_numbers, "number list counts of selection {n}");
            assert_eq!(a.remember(), b.remember());
        }
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The new build gives exactly the old result: value lists, rows, keyword lists with parents, people, duplicate
/// names below different parents, empty values, number lists; and the same filtering and counts.
#[test]
fn build_equals_reference_on_synthetic_library() {
    for (n, seed) in [(3000, 1), (20_000, 7)] {
        let s = synthetic(n, seed);
        let (mut old, _) = build_reference(&s.rows, &s.keywords, &s.assignments);
        let mut new = Facets::build(&s.rows, &s.keywords, &s.assignments);
        assert_same_build(&new, &old);
        assert!(!old.kw_ambiguous.is_empty(), "test data has duplicate names");
        assert!(old.values.iter().any(|w| w[0] == NO_VALUE && w[2] == NO_VALUE), "test data has empty values");
        assert!(old.persons.iter().any(|p| !p.is_empty()));
        assert_same_filtering(&mut new, &mut old, &s.rows);
    }
}

/// Edge cases: cycles in the hierarchy, a keyword assigned with its parent, a person with a parent, no rows.
#[test]
fn build_equals_reference_on_edge_cases() {
    let kw = |id: i64, name: &str, parent: Option<i64>, person: bool| Keyword { id, name: name.into(), parent, count: 1, person };
    let keywords = vec![kw(1, "A", Some(3), false), kw(2, "B", Some(1), false), kw(3, "C", Some(2), false), kw(4, "Rom", Some(5), false), kw(5, "Italien", None, false), kw(6, "Personen", None, false), kw(7, "Eva", Some(6), true), kw(8, "eva", None, false), kw(9, "Selbst", Some(9), false)];
    let mut rows = vec![Row::default(); 6];
    for (i, z) in rows.iter_mut().enumerate() {
        z.image = i as i64 + 1;
        z.format = "JPG".into();
    }
    rows[1].capture = Some("2020-13-40T99:00:00".into());
    rows[2].capture = Some("bad".into());
    rows[3].capture = Some("0000-01-01T00:00:00".into());
    rows[4].stars = 9;
    rows[4].label = 17;
    rows[4].ai_quality = Some(7);
    rows[5].aperture = Some(f64::NAN);
    rows[5].focal = Some(-3.0);
    let per_image: HashMap<i64, Vec<i64>> = HashMap::from([(1, vec![2, 1]), (2, vec![4, 5, 4]), (3, vec![7, 8]), (4, vec![9, 42]), (6, vec![7, 7])]);
    let (mut old, _) = build_reference(&rows, &keywords, &per_image);
    let mut new = Facets::build(&rows, &keywords, &per_image);
    assert_same_build(&new, &old);
    assert_same_filtering(&mut new, &mut old, &rows);
    let (old, _) = build_reference(&[], &keywords, &per_image);
    assert_same_build(&Facets::build(&[], &keywords, &per_image), &old);
}

/// Measurement (not a check): old and new build on 65 000 synthetic photos and optionally on a copy of a real
/// library, cold and warm, per phase; `apply` on the whole library and on a collection of 47 photos.
#[test]
#[ignore]
fn facets_bench() {
    let s = synthetic(65_000, 42);
    eprintln!("synthetic: {} rows, {} keywords, {} assignments", s.rows.len(), s.keywords.len(), s.assignments.values().map(Vec::len).sum::<usize>());
    measure("synthetic 65 000", &s.rows, &s.keywords, &s.assignments);
    if let Ok(path) = std::env::var("GRAINEO_FACETS_BENCH_DB") {
        let path = std::path::PathBuf::from(path);
        for round in ["cold", "warm"] {
            let t = Instant::now();
            let db = library::db::Db::read_only(&path).expect("library copy");
            let open = ms(t);
            let t = Instant::now();
            let rows = db.rows().unwrap();
            let rows_ms = ms(t);
            let t = Instant::now();
            let keywords = db.keywords().unwrap();
            let kw_ms = ms(t);
            let t = Instant::now();
            let assignments = read_assignments(&db);
            let read_ms = ms(t);
            eprintln!("backup copy ({round}): open {open:.1} ms, Db::rows {rows_ms:.0} ms ({} rows), Db::keywords {kw_ms:.0} ms ({}), read_assignments {read_ms:.0} ms ({} images)", rows.len(), keywords.len(), assignments.len());
            if round == "warm" {
                // the earlier keyword query (correlated count per keyword) for comparison: same counts
                let t = Instant::now();
                let mut st = db.conn.prepare("SELECT s.id, (SELECT count(*) FROM image_keyword bs JOIN image b ON b.id = bs.image JOIN file d ON d.id = b.file WHERE bs.keyword = s.id AND d.status <> 4) FROM keyword s ORDER BY s.name COLLATE NOCASE").unwrap();
                let old: Vec<(i64, usize)> = st.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize))).unwrap().map(|x| x.unwrap()).collect();
                eprintln!("backup copy: earlier keyword query {:.0} ms", ms(t));
                assert_eq!(keywords.iter().map(|k| (k.id, k.count)).collect::<Vec<_>>(), old, "keyword counts");
                measure("backup copy", &rows, &keywords, &assignments);
            }
        }
    }
}

fn measure(label: &str, rows: &[Row], keywords: &[Keyword], assignments: &HashMap<i64, Vec<i64>>) {
    for round in ["cold", "warm"] {
        let (mut old, p) = build_reference(rows, keywords, assignments);
        eprintln!("{label} old build ({round}): rows {:.1} ms, keyword index {:.1} ms, keyword lists {:.1} ms, people {:.1} ms, number lists {:.1} ms = {:.1} ms", p[0], p[1], p[2], p[3], p[4], p.iter().sum::<f64>());
        let t = Instant::now();
        let (mut new, p) = Facets::build_timed(rows, keywords, assignments);
        let total = ms(t);
        let t = Instant::now();
        let fingerprint = inputs_fingerprint(rows, keywords, assignments);
        eprintln!("{label} new build ({round}): rows {:.1} ms, keywords+people {:.1} ms, number lists {:.1} ms = {total:.1} ms; fingerprint {:.1} ms ({fingerprint:x})", p.rows, p.keywords, p.numbers, ms(t));
        let all: Vec<usize> = (0..rows.len()).collect();
        let t = Instant::now();
        old.apply(rows, &all);
        let a_all = ms(t);
        let small: Vec<usize> = (0..rows.len()).step_by((rows.len() / 47).max(1)).take(47).collect();
        let t = Instant::now();
        new.apply(rows, &small);
        let a_small = ms(t);
        let t = Instant::now();
        new.apply(rows, &all);
        eprintln!("{label} apply ({round}): all {a_all:.1} ms (old) / {:.1} ms (new), 47 photos {a_small:.2} ms", ms(t));
    }
}

/// Same query as `library_facets_job::read_assignments` (that module is private to the library view).
fn read_assignments(db: &library::db::Db) -> HashMap<i64, Vec<i64>> {
    let mut keywords: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut st = db.conn.prepare("SELECT image, keyword FROM image_keyword").unwrap();
    for (b, k) in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap().flatten() {
        keywords.entry(b).or_default().push(k);
    }
    keywords
}

/// Measurement (not a check): the same build while all cores are busy with work of normal priority (previews,
/// Apple Photos sync), at QoS UTILITY and USER_INITIATED: is the time spent computing or waiting?
#[test]
#[ignore]
fn facets_bench_under_load() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let s = synthetic(65_000, 42);
    let stop = Arc::new(AtomicBool::new(false));
    let cores = std::thread::available_parallelism().map_or(8, |n| n.get());
    let load: Vec<_> = (0..cores * 2)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut x = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                }
                x
            })
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(200));
    for (name, p) in [("utility", library::platform::Priority::Utility), ("user-initiated", library::platform::Priority::UserInitiated), ("background", library::platform::Priority::Background)] {
        let (rows, keywords, assignments) = (&s.rows, &s.keywords, &s.assignments);
        let took = std::thread::scope(|sc| {
            sc.spawn(move || {
                library::platform::set_priority(p);
                let t = Instant::now();
                let f = Facets::build(rows, keywords, assignments);
                let _ = inputs_fingerprint(rows, keywords, assignments);
                (ms(t), f.rows_len())
            })
            .join()
            .unwrap()
        });
        eprintln!("under load ({} busy threads on {cores} cores), {name}: build + check {:.0} ms", cores * 2, took.0);
    }
    stop.store(true, Ordering::Relaxed);
    for t in load {
        let _ = t.join();
    }
}
