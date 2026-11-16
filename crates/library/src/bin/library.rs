//! Command line for the library (checking and measuring without a UI).
//!
//! ```text
//! library import [KATALOG]        import a Lightroom catalog (read-only) and sync
//! library abgleich                library ↔ disk
//! library hinzufuegen ORDNER      add a folder as an own root folder
//! library vorschau ANZAHL [THREADS]  create thumbnails for the first ANZAHL images (measurement)
//! library status                  numbers of the library and the cache
//! ```
//! Database: `GRAINEO_LIBRARY_DB` or `~/Library/Application Support/graineo/library.sqlite`.

use std::sync::Arc;
use std::time::Instant;

use library::{reconcile, db::Db, lightroom, preview};

fn main() {
    // shorten output: paths relative to the user's Pictures folder
    let images = sysdirs::home().map(|h| format!("{}/Pictures/", h.display())).unwrap_or_default();
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("status");
    if std::env::var_os("GRAINEO_LIBRARY_DB").is_none() {
        for m in library::migrate() {
            eprintln!("renamed: {m}");
        }
    }
    let t_open = Instant::now();
    let db = match Db::standard() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot open library: {e}");
            std::process::exit(1);
        }
    };
    let report_fn = |s: &str| eprintln!("  {s}");
    match cmd {
        "import" => {
            let k = args.get(2).map(std::path::PathBuf::from).or_else(|| library::lightroom_catalog(&db)).unwrap_or_else(|| error("KATALOG missing"));
            let t = Instant::now();
            let b = lightroom::import(&db, &k, &report_fn).unwrap_or_else(|e| error(&e));
            let ti = t.elapsed();
            let t2 = Instant::now();
            let a = reconcile::reconcile(&db, None, &report_fn).unwrap_or_else(|e| error(&e));
            let ta = t2.elapsed();
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({"import": b.json(), "sync": a.json(), "import_duration_ms": ti.as_millis(), "sync_duration_ms": ta.as_millis()})).unwrap());
        }
        // card import measurement: library karte-messen <source> <target (empty, will be created)>
        "karte-messen" => {
            let q = std::path::PathBuf::from(args.get(2).unwrap_or_else(|| error("source missing")));
            let z = std::path::PathBuf::from(args.get(3).unwrap_or_else(|| error("target missing")));
            let t = Instant::now();
            let upfront = std::cell::Cell::new(0u128);
            let k = library::card_import::scan_with(&db, &q, &|v| upfront.set(t.elapsed().as_millis().max(1) + 0 * v.len() as u128), &|_, _| {});
            let ts = t.elapsed().as_millis();
            let t2 = Instant::now();
            let b = library::card_import::import(&k, &z, None, &std::sync::atomic::AtomicBool::new(false), &|_| {});
            let tk = t2.elapsed().as_millis();
            println!("{} photos ({} MB): list visible after {} ms, capture dates done after {ts} ms; copied {} in {tk} ms ({:.0} MB/s), errors {}", k.len(), b.bytes / 1_000_000, upfront.get(), b.copied, b.bytes as f64 / 1e6 / (tk as f64 / 1000.0), b.error.len());
        }
        "exif" => {
            let t = Instant::now();
            let n = library::exif::backfill(&db, 100_000, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_else(|e| error(&e));
            println!("shooting data: {n} files in {:?}", t.elapsed());
        }
        "text" => {
            let t = Instant::now();
            let n = library::textmeta::backfill(&db, 1_000_000, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_else(|e| error(&e));
            println!("texts in the files: {n} files in {:?}", t.elapsed());
        }
        "gesichter" => {
            // gesichter [ANZAHL] [THREADS]: search for faces (Vision) and compute features (SFace)
            let max = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
            let threads = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
            library::people::allow_download();
            let m = library::people::load_model().unwrap_or_else(|e| error(&e));
            let b = library::people::search(&db, &m, max, threads, &std::sync::atomic::AtomicBool::new(false), &|f, g| eprintln!("  {f}/{g}")).unwrap_or_else(|e| error(&e));
            println!("{b:?}\n{}", library::people::numbers(&db.conn).unwrap());
        }
        "gesichter-messen" => {
            // gesichter-messen [K] [MAX]: leave-one-photo-out measurement of the matching
            let k = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
            let max = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5000);
            let except = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1u8);
            println!("{}", library::people::score(&db.conn, k, max, except).unwrap_or_else(|e| error(&e)));
        }
        "gesichter-fehler" => {
            let n = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
            let out = args.get(3).cloned().unwrap_or_else(|| "/tmp/graineo-gesicht-fehler.png".into());
            println!("{}", library::people::error_images(&db, n, std::path::Path::new(&out)).unwrap_or_else(|e| error(&e)));
        }
        "gesichter-bereinigen" => {
            // gesichter-bereinigen [ABSTAND] [MINDEST] [schreiben]
            let distance = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.15f32);
            let minimum = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.45f32);
            let write = args.get(4).is_some_and(|s| s == "schreiben");
            println!("{}", library::people::clean_up(&db.conn, distance, minimum, write).unwrap_or_else(|e| error(&e)));
        }
        "personenbogen" => {
            let name = args.get(2).cloned().unwrap_or_else(|| "Erika M".into());
            let out = args.get(3).cloned().unwrap_or_else(|| "/tmp/graineo-gesicht-person.png".into());
            println!("{}", library::people::person_sheet(&db, &name, std::path::Path::new(&out)).unwrap_or_else(|e| error(&e)));
        }
        "gesichter-zuordnen" => {
            let b = library::people::assign(&db.conn, &library::people::Thresholds::default()).unwrap_or_else(|e| error(&e));
            println!("{b:?}");
        }
        "gesichter-gruppen-test" => {
            println!("{}", library::people::groups_test(&db.conn).unwrap_or_else(|e| error(&e)));
        }
        "gesichter-erkennung" => {
            // gesichter-erkennung [FOTOS] [RASTER] [ANTEIL]
            let n = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
            let r = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2usize);
            let a = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0.6f32);
            print!("{}", library::people::detection_test(&db, n, r, a).unwrap_or_else(|e| error(&e)));
        }
        "exif-nachtragen" => {
            // fill in missing shooting data (like the background run)
            let n = library::exif::backfill(&db, 100_000, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_else(|e| error(&e));
            println!("{n} files processed");
        }
        "exif-lesen" => {
            // exif-lesen DATEI FORMAT: read shooting data like the background run
            let (path, format) = (args.get(2).cloned().unwrap_or_default(), args.get(3).cloned().unwrap_or_default());
            println!("{:?}", library::exif::read(std::path::Path::new(&path), &format));
        }
        "gesicht-pruefen" => {
            // gesicht-pruefen BILD_ID: scan one photo right away (like the single view) and print the faces with suggestions
            let image: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let t = Instant::now();
            library::people::allow_download();
            let n = library::people::check_image(&db, image).unwrap_or_else(|e| error(&e));
            println!("{n} faces in {:?}", t.elapsed());
            for g in library::people::faces_to_image(&db.conn, image) {
                println!("  face {} ({:.3},{:.3},{:.3},{:.3}) {} {} {:.2}", g.id, g.x, g.y, g.b, g.h, g.status, g.name, g.confidence);
            }
        }
        "gesicht-hand" => {
            // gesicht-hand BILD_ID X Y B H: create a box by hand, compare with the image's existing faces, discard again
            let z: Vec<f32> = args.iter().skip(3).filter_map(|s| s.parse().ok()).collect();
            let image: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            if z.len() < 4 {
                error("gesicht-hand BILD_ID X Y B H");
            }
            library::people::allow_download();
            let id = library::people::manual_face(&db, image, z[0], z[1], z[2], z[3]).unwrap_or_else(|e| error(&e));
            let hole = |g: i64| -> Option<Vec<f32>> { db.conn.query_row("SELECT embedding FROM face WHERE id=?1", [g], |r| r.get::<_, Option<Vec<u8>>>(0)).ok().flatten().map(|b| library::people::read_blob(&b)) };
            let new = hole(id).unwrap_or_default();
            let mut st = db.conn.prepare("SELECT id, x, y, w, h FROM face WHERE image=?1 AND id<>?2").unwrap_or_else(|e| error(&e.to_string()));
            let other: Vec<(i64, f64, f64, f64, f64)> = st.query_map(rusqlite::params![image, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().flatten().collect();
            println!("new face {id}, {} values", new.len());
            for (g, x, y, b, h) in other {
                if let Some(m) = hole(g) {
                    let (sp, n1, n2) = m.iter().zip(&new).fold((0f32, 0f32, 0f32), |a, (p, q)| (a.0 + p * q, a.1 + p * p, a.2 + q * q));
                    println!("  vs face {g} ({x:.3},{y:.3},{b:.3},{h:.3}): similarity {:.3}", sp / (n1.sqrt() * n2.sqrt()).max(1e-9));
                }
            }
            library::people::discard_manual_face(&db, id).unwrap_or_else(|e| error(&e));
        }
        "aehnlich" => {
            let t = Instant::now();
            let v = library::similar::Vectors::load(&db).unwrap_or_else(|e| error(&e));
            let tl = t.elapsed();
            let name = args.get(2).cloned().unwrap_or_default();
            let d: i64 = db.conn.query_row("SELECT id FROM file WHERE name=?1", [&name], |r| r.get(0)).unwrap_or(0);
            let t = Instant::now();
            let a = v.similar_items(d, 10);
            println!("{} vectors loaded in {tl:?}; search {:?}", v.count(), t.elapsed());
            for (d, s) in a {
                println!("  {s:.3} {}", db.file_path(d).map(|p| p.display().to_string()).unwrap_or_default());
            }
        }
        "duplikate" => {
            let t = Instant::now();
            let v = library::similar::Vectors::load(&db).ok();
            let tv = t.elapsed();
            let t = Instant::now();
            let g = library::duplicates::find(&db, v.as_ref()).unwrap_or_else(|e| error(&e));
            let equal: Vec<_> = g.iter().filter(|x| x.kind == library::duplicates::Kind::Identical).collect();
            let similar: Vec<_> = g.iter().filter(|x| x.kind == library::duplicates::Kind::Similar).collect();
            println!("identical: {} groups ({} files), similar: {} groups ({} files); vectors {tv:?}, search {:?}", equal.len(), equal.iter().map(|x| x.files.len()).sum::<usize>(), similar.len(), similar.iter().map(|x| x.files.len()).sum::<usize>(), t.elapsed());
            for x in equal.iter().take(3).chain(similar.iter().take(3)) {
                println!("  {:?}: {}", x.kind, x.files.iter().filter_map(|d| db.file_path(*d)).map(|p| p.display().to_string().replace(&images, "")).collect::<Vec<_>>().join("  |  "));
            }
        }
        "beobachten" => {
            // measure latency: create a file in the folder and wait for the event
            let dir = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| error("folder missing")));
            let b = library::watch::Watcher::start_scan(std::slice::from_ref(&dir), 0.2).unwrap_or_else(|| error("FSEvents not available"));
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut values = Vec::new();
            for i in 0..5 {
                let p = dir.join(format!("probe-{i}.jpg"));
                let t = Instant::now();
                std::fs::write(&p, b"x").unwrap();
                loop {
                    if b.fetch().iter().any(|x| x.file_name() == p.file_name()) {
                        break;
                    }
                    if t.elapsed().as_secs() > 10 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                values.push(t.elapsed().as_secs_f64() * 1000.0);
                std::fs::remove_file(&p).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(400));
                let _ = b.fetch();
            }
            println!("FSEvents latency (ms): {values:.0?}; local: {}", library::watch::local(&dir));
        }
        "wertung" | "kopie" | "stichwort" | "verschieben" | "umbenennen" => {
            // test helpers (use only with an own test database)
            let name = args.get(2).cloned().unwrap_or_else(|| error("file name missing"));
            let d: i64 = db.conn.query_row("SELECT id FROM file WHERE name=?1", [&name], |r| r.get(0)).unwrap_or_else(|_| error("unknown file"));
            let b: i64 = db.conn.query_row("SELECT id FROM image WHERE file=?1 AND copy IS NULL", [d], |r| r.get(0)).unwrap();
            let arg = args.get(3).cloned().unwrap_or_default();
            let t = Instant::now();
            library::location::note_before_change(&db, &[d]);
            let r = match cmd {
                "wertung" => db.set_rating(&[b], arg.parse().ok(), None, args.get(4).and_then(|e| e.parse().ok())).map_err(|e| e.to_string()).and_then(|_| library::sync::write_sidecar(&db, d)),
                "kopie" => library::sync::create_copy(&db, b, None).map(|_| ()),
                "stichwort" => library::sync::change_keywords(&db, &[b], &[arg.clone()], &[]).map(|_| ()),
                "verschieben" => library::relocate::plan_files(&db, &[d], std::path::Path::new(&arg), None).and_then(|mut v| library::relocate::execute_step(&db, &mut v)),
                _ => {
                    let dir = db.file_path(d).unwrap().parent().unwrap().to_path_buf();
                    library::relocate::plan_files(&db, &[d], &dir, Some(&arg)).and_then(|mut v| library::relocate::execute_step(&db, &mut v))
                }
            };
            println!("{cmd}: {r:?} in {:?}", t.elapsed());
        }
        "rueckgaengig" => {
            let mut v = library::relocate::journals().into_iter().find(|v| v.status == "fertig").unwrap_or_else(|| error("nothing"));
            println!("{}: {:?}", v.title.clone(), library::relocate::undo(&db, &mut v));
        }
        "sichern" => {
            let t = Instant::now();
            println!("{:?} in {:?}", library::backup::create_backup(&db), t.elapsed());
        }
        "neuaufbau" => {
            let target = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| error("target missing")));
            let t = Instant::now();
            let r = library::backup::rebuild(&target, &library::backup::roots_file(&db.path), db.info("lightroom_catalog").map(std::path::PathBuf::from), &report_fn);
            println!("{} in {:?}", r.map(|v| v.to_string().chars().take(400).collect::<String>()).unwrap_or_else(|e| e), t.elapsed());
        }
        "einbetten" => {
            // measure the text encoder: load time, tokens and vector per sentence (JSON for comparison with Python)
            let dir = library::semantic::text_folder().unwrap_or_else(|| error("text encoder missing"));
            let t = Instant::now();
            let enc = library::semantic::TextEncoder::load(&dir).unwrap_or_else(|e| error(&e));
            eprintln!("loaded in {:?}", t.elapsed());
            let mut out = Vec::new();
            for s in &args[2..] {
                let t = Instant::now();
                let v = enc.embed(s);
                eprintln!("{s}: {:?} Tokens, {:?}", enc.tokenizer.encode(s, 128), t.elapsed());
                out.push(serde_json::json!({"text": s, "vec": v}));
            }
            println!("{}", serde_json::Value::Array(out));
        }
        "image-vector" => {
            // check the image encoder against the vectors stored in the library
            let dir = library::semantic::image_folder().unwrap_or_else(|| error("image encoder missing"));
            let t = Instant::now();
            let enc = library::semantic::ImageEncoder::load(&dir).unwrap_or_else(|e| error(&e));
            eprintln!("loaded in {:?}", t.elapsed());
            for p in &args[2..] {
                let fm = library::format_from_extension(p).unwrap_or("JPG");
                let (img, _) = library::preview::load(std::path::Path::new(p), fm, 1024).unwrap_or_else(|e| error(&e));
                let t = Instant::now();
                let v = enc.embed(&img);
                let ms = t.elapsed();
                let file = db.image_to_path(std::path::Path::new(p)).and_then(|b| db.file_of(b));
                let old: Option<Vec<u8>> = file.and_then(|d| db.conn.query_row("SELECT vec FROM image_vector WHERE file=?1", [d], |r| r.get(0)).ok());
                let cos = old.map(|b| b.chunks_exact(4).map(|x| f32::from_le_bytes([x[0], x[1], x[2], x[3]])).zip(&v).map(|(a, b)| a * b).sum::<f32>());
                println!("{p}: cosine vs stored vector {cos:?}, {ms:?}");
            }
        }
        "sem" => {
            // semantic search: rank of the known "Schimmel" images among the first 30
            let t0 = Instant::now();
            let v = library::similar::Vectors::load(&db).unwrap_or_else(|e| error(&e));
            let tv = library::similar::Vectors::load_text(&db).ok();
            let enc = library::semantic::TextEncoder::load(&library::semantic::text_folder().unwrap_or_else(|| error("text encoder missing"))).unwrap_or_else(|e| error(&e));
            eprintln!("{} vectors + encoder in {:?}", v.count(), t0.elapsed());
            for q in &args[2..] {
                let t = Instant::now();
                let qv = enc.embed(q);
                let te = t.elapsed();
                if std::env::var_os("RANKS").is_some() {
                    for (name, vv) in [("Bild", Some(&v)), ("Text", tv.as_ref())] {
                        let Some(vv) = vv else { continue };
                        let all = vv.next_item(&qv, 40000);
                        let r: Vec<String> = all.iter().enumerate().filter_map(|(i, (d, s))| {
                            let p = db.file_path(*d)?.display().to_string();
                            ["_DSF1332", "_DSF1333", "R3008537", "R3008538", "R3010457", "R3010459"].iter().any(|z| p.contains(z)).then(|| format!("{}:{s:.3}", i + 1))
                        }).collect();
                        println!("  {name}: ranks {r:?}, best {:.3}", all.first().map_or(0.0, |x| x.1));
                    }
                }
                let r: Vec<(i64, f32)> = library::similar::hybrid(&v, tv.as_ref(), &qv, std::env::var("N").ok().and_then(|n| n.parse().ok()).unwrap_or(30)).into_iter().map(|(d, s, _, _)| (d, s)).collect();
                let ts = t.elapsed();
                println!("\"{q}\": text {te:?}, total {ts:?}");
                for (i, (d, s)) in r.iter().enumerate() {
                    let p = db.file_path(*d).map(|p| p.display().to_string().replace(&images, "")).unwrap_or_default();
                    let target = ["_DSF1332", "_DSF1333", "R3008537", "R3008538", "R3010457", "R3010459"].iter().any(|z| p.contains(z));
                    if target || i < 8 {
                        println!("  {:2}. {s:.3} {p}{}", i + 1, if target { "  ◀" } else { "" });
                    }
                }
            }
        }
        "modul" => {
            // modul installieren|entfernen|status <id>; probe: small real download with checksum
            const PROBE: library::module::Definition = library::module::Definition {
                id: "probe",
                name: "Probe",
                repo: "sentence-transformers/clip-ViT-B-32-multilingual-v1",
                revision: "58edf8cada9e398793dca955574a48cbb7f18be2",
                files: &[
                    library::module::ModuleFile { path: "2_Dense/model.safetensors", target: "2_Dense/model.safetensors", size: 1572984, sha256: "d12568dc7300970a4d3dbb49068ad16cd89b99840b74b026f8e48071e9414f74" },
                    library::module::ModuleFile { path: "vocab.txt", target: "vocab.txt", size: 995526, sha256: "fe0fda7c425b48c516fc8f160d594c8022a0808447475c1a7c6d6479763f310c" },
                    library::module::ModuleFile { path: "1_Pooling/config.json", target: "1_Pooling/config.json", size: 190, sha256: "a37f83ada23e7887be6b88f4998927dbeac0038af301553c7cd5461413bf1a56" },
                ],
                license: "Apache-2.0",
                purpose: "Test",
                derived: None,
                info: None,
            };
            let was = args.get(2).map(String::as_str).unwrap_or("status");
            let id = args.get(3).map(String::as_str).unwrap_or("text-encoder");
            let d = if id == "probe" { &PROBE } else { *library::module::ALL.iter().chain(library::module::SEARCH.iter()).find(|d| d.id == id).unwrap_or_else(|| error("unknown module")) };
            let t = Instant::now();
            match was {
                "installieren" => {
                    let r = library::module::install(d, &|a, b, f| eprint!("\r  {:5.1} % {f}            ", a as f64 * 100.0 / b.max(1) as f64), &std::sync::atomic::AtomicBool::new(false));
                    eprintln!();
                    println!("{r:?} in {:?}", t.elapsed());
                }
                "entfernen" => println!("{:?}", library::module::remove(d)),
                _ => {}
            }
            let (z, g) = library::module::status(d);
            println!("{}: {z:?}, {:.1} MB, {}", d.id, g as f64 / 1e6, library::module::folder(d).display());
        }
        "suchmodell" => {
            // suchmodell einlesen|waehlen|stand <clip-b32|siglip2-base|siglip2-large>: read the photos in for a
            // search model stage (measurement: GRAINEO_REINDEX_FULL=1 without throttling) or choose it
            use library::search_model as sm;
            let stage = args.get(3).and_then(|k| sm::Stage::from_key(k)).unwrap_or_else(|| error("stage missing"));
            match args.get(2).map(String::as_str).unwrap_or("stand") {
                "einlesen" => {
                    let t = Instant::now();
                    let last = std::sync::Mutex::new(Instant::now());
                    let r = sm::run(&db, stage, &std::sync::atomic::AtomicBool::new(false), &|p| {
                        let mut l = last.lock().unwrap();
                        if l.elapsed().as_secs() >= 5 || p.done == p.total {
                            *l = Instant::now();
                            let s = p.since.elapsed().as_secs_f64();
                            eprintln!("  {} {}/{} · {:.1} pro Minute · Rest {:?}", p.part, p.done, p.total, p.run_done as f64 * 60.0 / s.max(1e-3), p.remaining());
                        }
                    });
                    println!("{r:?} in {:.1} s", t.elapsed().as_secs_f64());
                }
                "waehlen" => println!("sofort aktiv: {}", sm::choose(&db, stage)),
                "anfrage" => {
                    // load the query encoder and time queries (memory: /usr/bin/time -l)
                    let t = Instant::now();
                    let enc = sm::QueryEncoder::load(stage).unwrap_or_else(|e| error(&e));
                    let load = t.elapsed();
                    let text = args.get(4).map(String::as_str).unwrap_or("ein Hund am Strand bei Sonnenuntergang");
                    enc.embed(text);
                    let t = Instant::now();
                    for _ in 0..20 {
                        enc.embed(text);
                    }
                    println!("laden {:.0} ms · Anfrage {:.1} ms", load.as_secs_f64() * 1000.0, t.elapsed().as_secs_f64() * 50.0);
                }
                _ => {}
            }
            println!("{}: {:?}, aktiv {:?}, Ziel {:?}", stage.key(), sm::progress_of(&db, stage), sm::active(), sm::target());
        }
        "index" => {
            let (n, ms) = library::search::build_index(&db).unwrap_or_else(|e| error(&e));
            println!("index: {n} images in {ms} ms");
        }
        "suche" => {
            // measure search: hits, duration, first files
            let rows = db.rows().unwrap_or_default();
            let paths = db.folder_paths().unwrap_or_default();
            for q in &args[2..] {
                let t = Instant::now();
                let a = library::search::parse(q);
                let k = library::search::Context { db: &db, rows: &rows, folder_paths: &paths };
                let v = library::search::evaluate(&k, &a);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let matches: Vec<String> = rows.iter().zip(&v).filter(|(_, ok)| **ok).map(|(z, _)| format!("{}{}", paths.get(&z.folder).cloned().unwrap_or_default().replace(&images, ""), z.name)).collect();
                println!("\"{q}\": {} hits in {ms:.1} ms", matches.len());
                for t in matches.iter().take(std::env::var("N").ok().and_then(|n| n.parse().ok()).unwrap_or(12)) {
                    println!("    {t}");
                }
            }
        }
        // opening migrates an older library (schema 15: backup first); prints the time it took
        "migrieren" => {
            let v: i32 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
            let n: i64 = db.conn.query_row("SELECT count(*) FROM file", [], |r| r.get(0)).unwrap_or(0);
            println!("schema {v}, {n} files, open/migration {} ms, unread sidecars {}", t_open.elapsed().as_millis(), library::sidecar_import::unread_count(&db.conn));
        }
        // read the sidecars not read yet (as the app does in the background after the update); nothing is written to disk
        "begleit-einlesen" => {
            let max: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(usize::MAX);
            let t = Instant::now();
            let (mut files, mut sidecars, mut taken, mut develop, mut lr, mut errors) = (0, 0, 0, 0, 0, 0);
            while files < max {
                let r = library::sidecar_import::import_unread(&db, 1000.min(max - files), &|| {}).unwrap_or_else(|e| error(&e));
                if r.files == 0 {
                    break;
                }
                (files, sidecars, taken, develop, lr, errors) = (files + r.files, sidecars + r.sidecars, taken + r.taken, develop + r.develop, lr + r.lightroom, errors + r.errors);
                eprintln!("  {files} files, {sidecars} sidecars, {} s", t.elapsed().as_secs());
            }
            println!("{files} files, {sidecars} sidecars, taken over from {taken} (develop {develop}, Lightroom {lr}), errors {errors}, {} ms, left {}", t.elapsed().as_millis(), library::sidecar_import::unread_count(&db.conn));
        }
        // Lightroom handover into the library: library uebergabe KATALOG [pruefen]
        "uebergabe" => {
            let k = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| error("KATALOG missing")));
            let write = args.get(3).map(String::as_str) != Some("pruefen");
            let r = library::lr_handover::run(&db, &k, write, &|_, _| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_else(|e| error(&e));
            println!("{}", serde_json::to_string_pretty(&r.json()).unwrap());
        }
        // time for saving one develop change: library speichern-messen N (test library only – old way writes XMP files)
        "speichern-messen" => {
            let n: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(50);
            let files: Vec<i64> = { let mut st = db.conn.prepare("SELECT id FROM file WHERE format <> 'VIDEO' AND status = 0 ORDER BY id LIMIT ?1").unwrap(); let v = st.query_map([n as i64], |r| r.get(0)).unwrap().flatten().collect(); v };
            let json = r#"{"exposure":0.37,"contrast":12.0}"#;
            let t = Instant::now();
            for &f in &files {
                let p = db.file_path(f).unwrap();
                library::xmp::write_develop(&library::xmp::path_to(&p), Some(json)).unwrap_or_else(|e| error(&e));
            }
            let old = t.elapsed().as_secs_f64() * 1000.0 / files.len().max(1) as f64;
            let t = Instant::now();
            for &f in &files {
                let tx = db.conn.unchecked_transaction().unwrap();
                library::store::put_settings(&db.conn, f, None, Some(json), library::now()).unwrap();
                db.conn.execute("UPDATE image SET changed = ?2 WHERE file = ?1 AND copy IS NULL", rusqlite::params![f, library::now()]).unwrap();
                tx.commit().unwrap();
            }
            let new = t.elapsed().as_secs_f64() * 1000.0 / files.len().max(1) as f64;
            println!("{} files: sidecar (XMP, fsync) {old:.2} ms per change, library {new:.3} ms per change", files.len());
        }
        "abgleich" => {
            let a = reconcile::reconcile(&db, None, &report_fn).unwrap_or_else(|e| error(&e));
            println!("{}", serde_json::to_string_pretty(&a.json()).unwrap());
        }
        "hinzufuegen" => {
            let o = args.get(2).map(std::path::PathBuf::from).unwrap_or_else(|| error("folder missing"));
            let (id, a) = reconcile::add_root(&db, &o, &report_fn).unwrap_or_else(|e| error(&e));
            println!("root folder {id}\n{}", serde_json::to_string_pretty(&a.json()).unwrap());
        }
        "vorschau" => {
            let n: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200);
            let threads: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
            let cache = Arc::new(preview::Cache::standard());
            let rows = db.rows().unwrap_or_default();
            let paths = db.folder_paths().unwrap_or_default();
            // spread evenly over the library (all formats, all years), only existing files
            let mut jobs = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let candidates: Vec<_> = rows.iter().filter(|z| z.status == 0 && z.format != "VIDEO").collect();
            let step = (candidates.len() / n.max(1)).max(1);
            for z in candidates.iter().step_by(step).take(n) {
                if !seen.insert(z.file) {
                    continue;
                }
                let path = format!("{}{}", paths.get(&z.folder).cloned().unwrap_or_default(), z.name);
                let key = preview::cache_key_with_orientation(preview::cache_key(&path, z.size.unwrap_or(0), z.mtime.unwrap_or(0)), z.orientation);
                jobs.push(preview::Job { file: z.file, path: path.into(), format: z.format.clone(), key, level: preview::small(), orientation: z.orientation });
            }
            let total = jobs.len();
            let (before, _) = cache.occupied();
            let e = preview::Generator::new(cache.clone(), threads, Box::new(|_| {}));
            let t = Instant::now();
            e.background(jobs);
            e.background_on(true);
            let mut per_format: std::collections::BTreeMap<String, (usize, f64)> = Default::default();
            let mut finished = 0;
            let mut errors = Vec::new();
            let format_of: std::collections::HashMap<i64, String> = rows.iter().map(|z| (z.file, z.format.clone())).collect();
            while finished < total {
                std::thread::sleep(std::time::Duration::from_millis(50));
                for r in e.fetch() {
                    finished += 1;
                    let f = format_of.get(&r.file).cloned().unwrap_or_default();
                    let entry = per_format.entry(f).or_default();
                    entry.0 += 1;
                    if let Some(m) = r.error {
                        errors.push(format!("{}: {m}", r.file));
                    }
                    if let Some(m) = r.meta {
                        let _ = db.complete_file(r.file, m.width, m.height, m.capture.as_deref(), m.camera.as_deref());
                    }
                }
                if e.open_set() == 0 && finished >= total {
                    break;
                }
                if t.elapsed().as_secs() > 1800 {
                    break;
                }
            }
            let duration = t.elapsed();
            let (n_ok, n_errors, ms) = e.counter();
            let (after, files) = cache.occupied();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "jobs": total, "created": n_ok, "errors": n_errors, "error_list": errors.iter().take(20).collect::<Vec<_>>(),
                    "threads": threads, "duration_s": duration.as_secs_f64(), "ms_per_image_wall": duration.as_secs_f64() * 1000.0 / total.max(1) as f64,
                    "ms_per_image_compute": ms, "per_format": per_format.iter().map(|(k, v)| (k.clone(), v.0)).collect::<std::collections::BTreeMap<_, _>>(),
                    "cache_bytes_before": before, "cache_bytes_after": after, "cache_files": files,
                    "bytes_per_image": (after.saturating_sub(before)) as f64 / n_ok.max(1) as f64,
                }))
                .unwrap()
            );
        }
        "vorschau-messen" => {
            // import preview (192 px) per file: time as in the import dialog, files as arguments
            for p in &args[2..] {
                let p = std::path::Path::new(p);
                let f = p.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
                let f = if f == "JPEG" { "JPG".to_string() } else { f };
                let t = Instant::now();
                let r = preview::load(p, &f, 192);
                println!("{f} {:.0} ms {:?}", t.elapsed().as_secs_f64() * 1000.0, r.map(|(i, m)| (i.width(), i.height(), m.capture, m.camera)).map_err(|e| e));
            }
        }
        "bench" => {
            // a few files per format: time with our own path and (for comparison) with ImageIO
            let rows = db.rows().unwrap_or_default();
            let paths = db.folder_paths().unwrap_or_default();
            for f in ["JPG", "HEIC", "DNG", "CR3", "CR2", "RAF", "ARW", "PNG", "TIFF"] {
                let selection: Vec<_> = rows.iter().filter(|z| z.format == f && z.status == 0).step_by(97).take(8).collect();
                let (mut t_own, mut t_io, mut n) = (0.0, 0.0, 0);
                let mut sizes = Vec::new();
                for z in selection {
                    let p = std::path::PathBuf::from(format!("{}{}", paths.get(&z.folder).cloned().unwrap_or_default(), z.name));
                    let t = Instant::now();
                    let r = preview::load(&p, f, std::env::var("LEVEL").ok().and_then(|s| s.parse().ok()).unwrap_or(preview::large()));
                    t_own += t.elapsed().as_secs_f64() * 1000.0;
                    let t = Instant::now();
                    let io = library::imageio::thumb(&p, preview::large(), false);
                    t_io += t.elapsed().as_secs_f64() * 1000.0;
                    sizes.push(format!("{:?}/{:?}", r.ok().map(|(i, _)| (i.width(), i.height())), io.map(|(w, h, _)| (w, h))));
                    n += 1;
                }
                if n > 0 {
                    println!("{f}: own {:.0} ms, ImageIO (embedded) {:.0} ms per image; {}", t_own / n as f64, t_io / n as f64, sizes.iter().take(3).cloned().collect::<Vec<_>>().join(" "));
                }
            }
        }
        "messen" => {
            // library startup time in the app: open + all rows + load trees
            let t = Instant::now();
            let db2 = Db::standard().unwrap();
            let z = db2.rows().unwrap();
            let t_z = t.elapsed();
            let o = db2.folder().unwrap();
            let s = db2.collections().unwrap();
            let k = db2.keywords().unwrap();
            let t_all = t.elapsed();
            let t2 = Instant::now();
            let c = preview::Cache::standard();
            let t_c = t2.elapsed();
            println!("rows {} in {:?}; folders {} collections {} keywords {} – total {:?}; open cache {:?} ({:?})", z.len(), t_z, o.len(), s.len(), k.len(), t_all, t_c, c.occupied());
        }
        _ => {
            let n = db.image_count();
            let c = preview::Cache::standard();
            let (bytes, files) = c.occupied();
            println!("library {}: {n} images; preview cache {}: {:.1} MB in {files} files", db.path.display(), c.folder().display(), bytes as f64 / 1e6);
            for l in db.runs().unwrap_or_default().iter().take(5) {
                println!("  run {} {} start {} new {}", l.id, l.kind, l.begin, l.new);
            }
        }
    }
}

fn error(m: &str) -> ! {
    eprintln!("error: {m}");
    std::process::exit(1)
}
