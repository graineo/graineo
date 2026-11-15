//! Measurement helper (analysis only, not part of the app): builds a large synthetic library with faces in the temp folder
//! and measures how long the queries take that the People view runs in the UI thread today.
//! Aufruf: `PM_FACES=110000 cargo test --release -p library --test people_measure -- --ignored --nocapture`

use std::time::Instant;

use library::db::Db;
use library::people;
use rusqlite::params;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    fn n(&mut self, k: usize) -> usize {
        (self.next() % k as u64) as usize
    }
}

fn unit(v: &mut [f32]) {
    let l = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter_mut().for_each(|x| *x /= l);
}

fn ms<T>(name: &str, f: impl FnMut() -> T) -> T {
    let mut f = f;
    let mut times = Vec::new();
    let mut out = None;
    for _ in 0..3 {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.total_cmp(b));
    println!("MESSUNG {name:<58} {:>9.1} ms (min {:.1}, max {:.1})", times[1], times[0], times[2]);
    out.unwrap()
}

fn once<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!("MESSUNG {name:<58} {:>9.1} ms (once)", t.elapsed().as_secs_f64() * 1000.0);
    r
}

/// A grey silhouette (head and shoulders) as a stand-in for a face; the tone follows the person.
fn placeholder(id: i64, who: i64) -> image::RgbImage {
    let side = 192u32;
    let tone = 70 + ((who * 37).rem_euclid(60)) as u8;
    let bg = 40 + ((id * 13).rem_euclid(14)) as u8;
    let (rx, ry) = (34.0 + (who % 5) as f32 * 2.0, 44.0 + (who % 3) as f32 * 3.0);
    let cy = 82.0 + (id % 7) as f32;
    image::RgbImage::from_fn(side, side, |x, y| {
        let (fx, fy) = (x as f32 - 96.0, y as f32);
        let head = (fx / rx).powi(2) + ((fy - cy) / ry).powi(2) <= 1.0;
        let body = fy > cy + ry * 0.9 && (fx / 80.0).powi(2) + ((fy - 230.0) / 90.0).powi(2) <= 1.0;
        if head {
            image::Rgb([tone, tone, tone + 4])
        } else if body {
            image::Rgb([tone - 16, tone - 16, tone - 12])
        } else {
            image::Rgb([bg, bg, bg + 2])
        }
    })
}

#[test]
#[ignore]
fn people_measure() {
    let faces_n: usize = std::env::var("PM_FACES").ok().and_then(|v| v.parse().ok()).unwrap_or(110_000);
    let photos_n: usize = 64_800;
    let people_n: usize = 50;
    let dir = std::env::temp_dir().join(format!("graineo-personen-messung-{faces_n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::env::set_var("GRAINEO_DATA_DIR", dir.join("data"));
    std::env::set_var("GRAINEO_LIBRARY_SIDECARS", "0");
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let c = &db.conn;
    let mut r = Rng(0x9e3779b97f4a7c15);

    // ── Setup
    once("Build synthetic library", || {
        let tx = c.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO root_folder(id, path, name, origin) VALUES(1, ?1, 'Fotos', 'own')", [format!("{}/photos/", dir.display())]).unwrap();
        for o in 0..400 {
            tx.execute("INSERT INTO folder(id, root, path) VALUES(?1, 1, ?2)", params![o + 1, format!("ordner{o}/")]).unwrap();
        }
        for i in 1..=photos_n as i64 {
            tx.execute(
                "INSERT INTO file(id, folder, name, format, size, capture_time, width, height, orientation) VALUES(?1, ?2, ?3, 'JPG', 1000, ?4, 6000, 4000, 1)",
                params![i, 1 + i % 400, format!("IMG_{i:06}.JPG"), format!("20{:02}-{:02}-{:02}T12:00:00", 5 + i % 20, 1 + i % 12, 1 + i % 28)],
            )
            .unwrap();
            tx.execute("INSERT INTO image(id, file) VALUES(?1, ?1)", [i]).unwrap();
        }
        tx.execute("INSERT INTO keyword(id, name) VALUES(1, 'Personen')", []).unwrap();
        let names = ["Anna", "Ben", "Clara", "David", "Emil", "Frieda", "Greta", "Hugo", "Ida", "Jonas", "Karl", "Lena", "Mia", "Noah", "Oskar", "Paula", "Quirin", "Rosa", "Simon", "Theo", "Ulla", "Vera", "Wilma", "Xaver", "Yara", "Zoe"];
        for p in 0..people_n {
            let name = if p < names.len() { names[p].to_string() } else { format!("{} {}", names[p % names.len()], ["Huber", "Maier", "Schmid"][p % 3]) };
            tx.execute("INSERT INTO keyword(id, name, parent, person) VALUES(?1, ?2, 1, 1)", params![100 + p as i64, name]).unwrap();
        }
        // centres: 50 people, 400 strangers
        let centers: Vec<Vec<f32>> = (0..people_n + 400)
            .map(|_| {
                let mut v: Vec<f32> = (0..128).map(|_| r.f() - 0.5).collect();
                unit(&mut v);
                v
            })
            .collect();
        // frequency like in real life: a few people very often
        let weights: Vec<f64> = (0..people_n).map(|p| 1.0 / ((p + 1) as f64).powf(1.1)).collect();
        let total: f64 = weights.iter().sum();
        let mut group_of_stranger: Vec<Option<i64>> = vec![None; 400];
        let mut st = tx
            .prepare("INSERT INTO face(image, x, y, w, h, landmarks, embedding, model, quality, source, person, status, confidence, score_own, cluster, local, apple_id) VALUES(?1,0.4,0.3,0.1,0.15,?2,?3,'sface',?4,?5,?6,?7,?8,?9,?10,0,?11)")
            .unwrap();
        let points = vec![0u8; 40];
        for i in 0..faces_n {
            let image = 1 + r.n(photos_n) as i64;
            let known = r.f() < 0.45;
            let (center, person, state, conf, second, group) = if known {
                let mut x = r.f() as f64 * total;
                let mut p = 0;
                while p + 1 < people_n && x > weights[p] {
                    x -= weights[p];
                    p += 1;
                }
                if r.f() < 0.7 {
                    (p, Some(100 + p as i64), "confirmed", None, None, None)
                } else {
                    (p, Some(100 + p as i64), "suggested", Some(0.6 + 0.3 * r.f() as f64), Some(0.3 + 0.3 * r.f() as f64), None)
                }
            } else {
                let s = r.n(400);
                let grouped = r.f() < 0.5;
                let g = if grouped { Some(*group_of_stranger[s].get_or_insert((i + 1) as i64)) } else { None };
                (people_n + s, None, if r.f() < 0.05 { "abgelehnt" } else { "unknown" }, None, None, g)
            };
            let mut v: Vec<f32> = centers[center].iter().map(|x| x + (r.f() - 0.5) * 0.12).collect();
            unit(&mut v);
            let blob = people::blob(&v);
            let source = ["lightroom", "graineo", "apple"][r.n(3)];
            let apple = (source == "apple").then(|| format!("apple-{i}"));
            st.execute(params![image, points, blob, 0.3 + 0.7 * r.f() as f64, source, person, state, conf, second, group, apple]).unwrap();
        }
        drop(st);
        // people keywords on the photos (confirmed faces)
        tx.execute("INSERT OR IGNORE INTO image_keyword(image, keyword) SELECT DISTINCT image, person FROM face WHERE status='confirmed'", []).unwrap();
        // suspicious names
        tx.execute(
            "UPDATE face SET suspect=1, guessed=100 + ((person - 99) % 50), score_own=0.3, score_guessed=0.5 WHERE id IN (SELECT id FROM face WHERE status='confirmed' AND person<>100 ORDER BY id LIMIT 420)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    });
    let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    println!(
        "STOCK {} photos, {} faces, confirmed {}, suggestions {}, unknown {}, groups {}, suspicious {}, size {:.0} MB",
        photos_n,
        n("SELECT count(*) FROM face"),
        n("SELECT count(*) FROM face WHERE status='confirmed'"),
        n("SELECT count(*) FROM face WHERE status='suggested'"),
        n("SELECT count(*) FROM face WHERE status='unknown'"),
        n("SELECT count(DISTINCT cluster) FROM face WHERE cluster IS NOT NULL"),
        n("SELECT count(*) FROM face WHERE suspect=1"),
        std::fs::metadata(dir.join("library.sqlite")).map(|m| m.len() as f64 / 1e6).unwrap_or(0.0)
    );
    // a cold cache would be more realistic; warm here (lower bound)
    let _ = c.execute_batch("ANALYZE");
    // PM_KEEP=<folder>: copy of the library for a run of the app (GRAINEO_DATA_DIR=<folder>)
    if let Some(keep) = std::env::var_os("PM_KEEP") {
        let target = std::path::Path::new(&keep).join("library.sqlite");
        std::fs::create_dir_all(&keep).unwrap();
        let _ = std::fs::remove_file(&target);
        c.execute("VACUUM INTO ?1", [target.to_string_lossy()]).unwrap();
        println!("COPY {}", target.display());
        // placeholder face images (grey silhouettes, no real faces) for screenshots of the view
        let faces_dir = std::path::Path::new(&keep).join("faces");
        std::fs::create_dir_all(&faces_dir).unwrap();
        let t = Instant::now();
        let mut st = c.prepare("SELECT id, coalesce(person, 0), coalesce(cluster, 0) FROM face").unwrap();
        let rows: Vec<(i64, i64, i64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().flatten().collect();
        for (id, person, group) in &rows {
            placeholder(*id, *person + *group).save(faces_dir.join(format!("{id}.jpg"))).unwrap();
        }
        println!("PLACEHOLDER {} face images in {:.1} s", rows.len(), t.elapsed().as_secs_f64());
        if std::env::var_os("PM_ONLY_KEEP").is_some() {
            return;
        }
    }

    // ── What `load()` does on every reload in the UI thread
    println!("--- load(): counters (every time, every tab)");
    ms("check_count", || people::check_count(c));
    ms("suggestion_count", || people::suggestion_count(c));
    ms("group_count", || people::group_count(c));
    let pl = ms("person_list(false) [for counters]", || people::person_list(c, false));
    ms("person_names", || people::person_names(c));
    let sp = ms("suggestion_people", || people::suggestion_people(c));
    println!("--- load(): list of the tab");
    ms("Check: test_people", || people::test_people(c));
    let cl = ms("Check: check_list(all)", || people::check_list(c, None, usize::MAX, 0));
    let sl = ms("Suggestions: suggestion_list(all)", || people::suggestion_list(c, None, 0.0, usize::MAX, 0));
    let big = pl.first().map(|p| p.id).unwrap();
    let small = pl.last().map(|p| p.id).unwrap();
    ms("Suggestions: suggestion_list(most frequent person)", || people::suggestion_list(c, Some(big), 0.0, usize::MAX, 0));
    let gl = ms("Unnamed: group_list(all)", || people::group_list(c, usize::MAX, 0));
    ms("Unnamed: group_list(first 50)", || people::group_list(c, 50, 0));
    ms("People: person_list", || people::person_list(c, false));
    let pf = ms("People: person_faces(most frequent)", || people::person_faces(c, big));
    ms("People: person_faces(rarest)", || people::person_faces(c, small));
    println!("LISTS check {} · suggestions {} · groups {} · people {} · faces of the most frequent person {} · people with suggestions {}", cl.len(), sl.len(), gl.len(), pl.len(), pf.len(), sp.len());

    // sum of one load() per tab
    let counts = |c: &rusqlite::Connection| {
        let _ = (people::check_count(c), people::suggestion_count(c), people::group_count(c), people::person_list(c, false).len());
        let _ = people::person_names(c);
        let _ = people::suggestion_people(c);
    };
    println!("--- load() total per action (blocks the frame)");
    ms("Open Check tab", || {
        counts(c);
        let _ = people::test_people(c);
        people::check_list(c, None, usize::MAX, 0).len()
    });
    ms("Open Suggestions tab", || {
        counts(c);
        people::suggestion_list(c, None, 0.0, usize::MAX, 0).len()
    });
    ms("Suggestions: choose person in the drop-down", || {
        counts(c);
        people::suggestion_list(c, Some(big), 0.0, usize::MAX, 0).len()
    });
    ms("Open Unnamed tab", || {
        counts(c);
        people::group_list(c, usize::MAX, 0).len()
    });
    ms("Open People tab", || {
        counts(c);
        people::person_list(c, false).len()
    });
    ms("People: double-click person (most frequent)", || {
        counts(c);
        people::person_faces(c, big).len()
    });
    ms("People: ← All people (person_list + load)", || {
        let _ = people::person_list(c, false);
        counts(c);
        people::person_list(c, false).len()
    });

    // ── Proposal: compact face index in memory (without features), filtering/sorting/counting without a database
    println!("--- Proposal: face index in memory");
    #[derive(Clone)]
    #[allow(dead_code)]
    struct F {
        id: i64,
        image: i64,
        person: Option<i64>,
        state: u8,
        conf: f32,
        second: f32,
        group: Option<i64>,
        source: u8,
        suspect: Option<i64>,
        year: u16,
    }
    let index: Vec<F> = ms("Load face index (all faces, without features)", || {
        let mut st = c
            .prepare(
                "SELECT g.id, g.image, g.person, g.status, coalesce(g.confidence,0), coalesce(g.score_own,0), g.cluster, g.source, CASE WHEN g.suspect=1 THEN g.guessed END, coalesce(substr(d.capture_time,1,4),'0')
                 FROM face g JOIN image b ON b.id=g.image JOIN file d ON d.id=b.file",
            )
            .unwrap();
        st.query_map([], |r| {
            let z: String = r.get(3)?;
            let q: String = r.get(7)?;
            Ok(F {
                id: r.get(0)?,
                image: r.get(1)?,
                person: r.get(2)?,
                state: match z.as_str() { "confirmed" => 0, "suggested" => 1, "unknown" => 2, _ => 3 },
                conf: r.get::<_, f64>(4)? as f32,
                second: r.get::<_, f64>(5)? as f32,
                group: r.get(6)?,
                source: match q.as_str() { "lightroom" => 0, "graineo" => 1, "apple" => 2, _ => 3 },
                suspect: r.get(8)?,
                year: r.get::<_, String>(9)?.parse().unwrap_or(0),
            })
        })
        .unwrap()
        .flatten()
        .collect()
    });
    println!("INDEX {} faces, approx. {:.1} MB", index.len(), (index.len() * std::mem::size_of::<F>()) as f64 / 1e6);
    ms("Memory: count facets (person × status × source × year)", || {
        let mut per: std::collections::HashMap<(Option<i64>, u8), usize> = std::collections::HashMap::new();
        let mut src = [0usize; 4];
        let mut years: std::collections::HashMap<u16, usize> = std::collections::HashMap::new();
        for f in &index {
            *per.entry((f.person, f.state)).or_default() += 1;
            src[f.source as usize] += 1;
            *years.entry(f.year).or_default() += 1;
        }
        per.len() + years.len() + src[0]
    });
    ms("Memory: name chosen → suggestions sorted", || {
        let mut v: Vec<&F> = index.iter().filter(|f| f.person == Some(big) && f.state == 1).collect();
        v.sort_by(|a, b| (b.conf - b.second).total_cmp(&(a.conf - a.second)));
        v.len()
    });
    ms("Memory: all suggestions sorted", || {
        let mut v: Vec<&F> = index.iter().filter(|f| f.state == 1).collect();
        v.sort_by(|a, b| (b.conf - b.second).total_cmp(&(a.conf - a.second)));
        v.len()
    });
    ms("Memory: form groups (Ohne Namen)", || {
        let mut g: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
        for f in index.iter().filter(|f| f.person.is_none() && f.state == 2) {
            if let Some(x) = f.group {
                g.entry(x).or_default().push(f.id);
            }
        }
        g.len()
    });
    // remedy for group_list: partial index on gruppe
    c.execute_batch("CREATE INDEX IF NOT EXISTS face_cluster ON face(cluster) WHERE cluster IS NOT NULL").unwrap();
    ms("Unnamed: group_list(all) WITH index gesicht(gruppe)", || people::group_list(c, usize::MAX, 0).len());
    ms("group_count MIT Index", || people::group_count(c));
    {
        let mut img = image::RgbImage::new(1024, 683);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 7 % 255) as u8, (y * 3 % 255) as u8, ((x ^ y) % 255) as u8]);
        }
        img.save(dir.join("vorschau1024.jpg")).unwrap();
        let mut img = image::RgbImage::new(2560, 1707);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 7 % 255) as u8, (y * 3 % 255) as u8, ((x ^ y) % 255) as u8]);
        }
        img.save(dir.join("vorschau2560.jpg")).unwrap();
    }
    for n in ["vorschau1024.jpg", "vorschau2560.jpg"] {
        let p = dir.join(n);
        ms(&format!("Crop from preview cache: decode {n} + 128 px"), || {
            let i = image::open(&p).unwrap().to_rgb8();
            let a = image::imageops::crop_imm(&i, 300, 200, 200, 200).to_image();
            image::imageops::resize(&a, 128, 128, image::imageops::FilterType::Triangle).width()
        });
    }
    c.execute_batch("DROP INDEX IF EXISTS face_cluster").unwrap();

    // ── face images: from the preview cache (camera previews 512 and 2560 px) instead of the original
    {
        let cache = library::cache_folder();
        let ids: Vec<(i64, i64)> = c.prepare("SELECT g.id, g.image FROM face g ORDER BY g.id LIMIT 100").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
        for (_, image) in &ids {
            let (folder, name): (i64, String) = c.query_row("SELECT d.folder, d.name FROM image b JOIN file d ON d.id=b.file WHERE b.id=?1", [image], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            let path = format!("{}/photos/ordner{}/{name}", dir.display(), folder - 1);
            let key = library::preview::cache_key(&path, 1000, 0);
            for (level, w, h) in [(library::preview::small(), 512u32, 341u32), (library::preview::large(), 2560, 1707)] {
                let f = library::preview::file_in(&cache, key, level);
                std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 251) as u8, (y % 241) as u8, 90])).save_with_format(&f, image::ImageFormat::Jpeg).unwrap();
            }
        }
        let t = Instant::now();
        let mut from_cache = 0;
        for (id, _) in &ids {
            if matches!(library::face_index::ensure_thumb(&db, *id), Ok(library::face_index::ThumbSource::PreviewCache)) {
                from_cache += 1;
            }
        }
        println!("MESSUNG {:<58} {:>9.1} ms per face ({from_cache} of {} from the preview cache)", "face image from the preview cache (ensure_thumb)", t.elapsed().as_secs_f64() * 1000.0 / ids.len() as f64, ids.len());
    }
    // ── face index (library module)
    let ix = ms("face_index::load (all faces)", || library::face_index::load(c).unwrap());
    ms("people_model-like: facets and sections", || ix.faces.iter().filter(|f| f.status.is_question()).count());

    // ── Query plans
    for (name, sql) in [
        ("suggestion_count", "SELECT count(*) FROM face g WHERE g.status='suggested' AND g.person IS NOT NULL"),
        ("group_count", "SELECT count(DISTINCT cluster) FROM face WHERE cluster IS NOT NULL AND person IS NULL AND status<>'rejected'"),
        ("group_list Beispiele", "SELECT id FROM face WHERE cluster=5 AND person IS NULL ORDER BY quality DESC, id LIMIT 6"),
        ("person_faces", "SELECT g.id FROM face g JOIN image b ON b.id=g.image JOIN file d ON d.id=b.file WHERE g.person=100 AND g.status='confirmed' ORDER BY d.capture_time DESC"),
    ] {
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = st.query_map([], |r| r.get::<_, String>(3)).unwrap().flatten().collect();
        println!("PLAN {name}: {}", plan.join(" | "));
    }

    // ── Background: counter thread every 10 s, reload library after a name change
    println!("--- Background");
    ms("Counter thread (check_count + suggestion_count)", || (people::check_count(c), people::suggestion_count(c)));
    ms("Library: db.rows() (part of reload after a change)", || db.rows().unwrap().len());
    ms("people_state::assign_dirty", || library::people_state::assign_dirty(c));

    // ── Writing (in the write thread; afterwards the view waits for it and reloads everything)
    println!("--- Writing");
    let some: Vec<i64> = sl.iter().take(20).map(|v| v.face).collect();
    once("answer_suggestion(yes) ×1", || people::answer_suggestion(&db, some[0], true).unwrap());
    once("answer_suggestion(yes) ×10 (multi-selection)", || {
        for f in &some[1..11] {
            people::answer_suggestion(&db, *f, true).unwrap();
        }
    });
    once("answer_suggestion(no) ×1", || people::answer_suggestion(&db, some[11], false).unwrap());
    if let Some(g) = gl.iter().find(|g| g.count >= 20) {
        let cnt = g.count;
        once(&format!("name_group ({cnt} faces)"), || people::name_group(&db, g.id, "Neue Person").unwrap());
    }
    let sure = sl.iter().filter(|v| v.person == big && v.confidence >= 0.7 && v.confidence - v.second_item >= 0.1).count();
    once(&format!("confirm_backups_for(most frequent, {sure} faces)"), || people::confirm_backups_for(&db, Some(big), 0.7, 0.1).unwrap());
    once("rename_person(most frequent → new name)", || people::rename_person(&db, big, "Anna Umbenannt").unwrap());

    // ── Background runs that rewrite the suggestions/check list
    once("clean_up (check names, writing)", || people::clean_up(c, 0.15, 0.45, true).unwrap().len());
    let rep = once("assign (suggestions + groups anew)", || people::assign(c, &people::Thresholds::default()).unwrap());
    println!("ASSIGN {rep:?}");
    ms("load_samples (read all features)", || people::load_samples(c).unwrap().len());

    // ── Face crops: load texture (UI thread, up to 24 per image) and create crop (worker)
    println!("--- Face images");
    let jpg = dir.join("face.jpg");
    {
        let mut img = image::RgbImage::new(224, 224);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 7 % 255) as u8, (y * 3 % 255) as u8, ((x ^ y) % 255) as u8]);
        }
        img.save(&jpg).unwrap();
    }
    ms("Texture: image::open 224-px JPEG ×24 (one image in the UI thread)", || {
        for _ in 0..24 {
            let i = image::open(&jpg).unwrap().to_rgb8();
            std::hint::black_box(i);
        }
    });
    let merrill = std::env::var("GRAINEO_TEST_MERRILL_DIR").unwrap_or_default();
    for name in ["DP2M5412.X3F", "DP2M5412.JPG"] {
        let full = std::path::Path::new(&merrill).join(name);
        let p = full.to_str().unwrap_or_default();
        let path = full.as_path();
        if path.exists() {
            let fmt = if p.ends_with("X3F") { "X3F" } else { "JPG" };
            ms(&format!("Crop source preview::load 1600 ({fmt})"), || library::preview::load(path, fmt, 1600).map(|x| x.0.width()).unwrap_or(0));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
