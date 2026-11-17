//! Lightroom import across catalog versions: synthetic catalogs (a newer format, missing columns and tables; title,
//! GPS position, synonyms, stacks, labels of a custom set) and the real test catalogs (read-only).
//!
//! The real catalogs are private and live outside the repository:
//! - `import_real_catalogs` (ignored): `GRAINEO_LR_TEST_CATALOGS=/path/a.lrcat:/path/b.lrcat cargo test -p library
//!   --test lr_catalog_versions -- --ignored --nocapture` prints the import report as JSON per catalog.
//! - `test_catalog_15_1` runs if the small Lightroom Classic 15.1 test catalog is there (`GRAINEO_LR_TEST_CATALOG_15`,
//!   default `/Users/Shared/graineo-lrtest/katalog-15-1/katalog-15-1.lrcat`), otherwise it is skipped.
//! Both check that the catalog file stays unchanged.

use std::io::Write;
use std::path::{Path, PathBuf};

use library::{db::Db, lightroom};
use rusqlite::{params, Connection};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("graineo-lr-versions-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fingerprint(p: &Path) -> (u64, std::time::SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    (m.len(), m.modified().unwrap())
}

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// XMP packet as Lightroom stores it in `Adobe_AdditionalMetadata.xmp`: 4-byte length + zlib.
fn packed_xmp(title: &str) -> Vec<u8> {
    let text = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">{title}</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"#
    );
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(text.as_bytes()).unwrap();
    let mut out = (text.len() as u32).to_be_bytes().to_vec();
    out.extend(e.finish().unwrap());
    out
}

/// A catalog with only the core tables, and of `Adobe_images` only some columns (as a future or trimmed format might
/// have them), plus `Adobe_variablesTable` with `version` (listed twice, as in catalogs upgraded over the years).
fn trimmed_catalog(path: &Path, photos: &Path, version: &str) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE AgLibraryRootFolder (id_local INTEGER PRIMARY KEY, absolutePath, name);
         CREATE TABLE AgLibraryFolder (id_local INTEGER PRIMARY KEY, rootFolder INTEGER, pathFromRoot);
         CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, folder INTEGER, baseName, extension);
         CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, rootFile INTEGER, rating, captureTime);
         CREATE TABLE Adobe_variablesTable (id_local INTEGER PRIMARY KEY, name, value);",
    )
    .unwrap();
    c.execute("INSERT INTO Adobe_variablesTable(name, value) VALUES('Adobe_DBVersion', '1500000'), ('Adobe_DBVersion', ?1)", [version]).unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder VALUES(1, ?1, 'Fotos')", [format!("{}/", photos.display())]).unwrap();
    c.execute("INSERT INTO AgLibraryFolder VALUES(10, 1, '')", []).unwrap();
    c.execute("INSERT INTO AgLibraryFile VALUES(100, 10, 'A', 'JPG'), (101, 10, 'B', 'DNG')", []).unwrap();
    c.execute("INSERT INTO Adobe_images VALUES(1000, 100, 4, '2025-01-02T03:04:05'), (1001, 101, NULL, NULL)", []).unwrap();
}

/// A catalog of a newer format with missing columns and tables: checked with a note on the format, imported without
/// "no such column", the missing parts named in the report.
#[test]
fn newer_format_and_missing_columns() {
    let dir = tmp("trimmed");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    let cat = dir.join("Neu.lrcat");
    trimmed_catalog(&cat, &photos, "1600000");
    let db = Db::open(&dir.join("library.sqlite")).unwrap();

    let p = lightroom::preview(&db, &cat).unwrap();
    assert_eq!(p.catalog_version, Some(1_600_000));
    assert!(p.newer_format());
    assert_eq!((p.images, p.new_images, p.virtual_copies, p.keywords, p.edited), (2, 2, 0, 0, 0));
    for gap in ["Adobe_images.masterImage", "AgLibraryCollection", "AgLibraryKeyword", "Adobe_imageDevelopSettings"] {
        assert!(p.catalog_gaps.iter().any(|g| g == gap), "{gap} not in {:?}", p.catalog_gaps);
    }

    let r = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!((r.images, r.files, r.with_stars, r.keywords, r.collections), (2, 2, 1, 0, 0));
    assert!(r.newer_format());
    assert_eq!(r.json()["newer_format"], true);
    for gap in ["Adobe_images.colorLabels", "Adobe_images.masterImage", "AgHarvestedExifMetadata", "AgLibraryIPTC", "AgLibraryKeyword", "Adobe_AdditionalMetadata"] {
        assert!(r.catalog_gaps.iter().any(|g| g == gap), "{gap} not in {:?}", r.catalog_gaps);
    }
    assert_eq!(count(&db, "SELECT count(*) FROM file WHERE capture_time = '2025-01-02T03:04:05'"), 1);

    // the tested format: no note
    let cat2 = dir.join("Getestet.lrcat");
    trimmed_catalog(&cat2, &photos, "1500000");
    let p2 = lightroom::preview(&db, &cat2).unwrap();
    assert_eq!((p2.catalog_version, p2.newer_format()), (Some(1_500_000), false));

    // a core column missing: a clear error instead of "no such column"
    let cat3 = dir.join("Kaputt.lrcat");
    let c = Connection::open(&cat3).unwrap();
    c.execute_batch(
        "CREATE TABLE AgLibraryRootFolder (id_local INTEGER PRIMARY KEY, absolutePath, name);
         CREATE TABLE AgLibraryFolder (id_local INTEGER PRIMARY KEY, rootFolder INTEGER, pathFromRoot);
         CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, folder INTEGER, baseName);
         CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, rating);",
    )
    .unwrap();
    drop(c);
    let e = lightroom::import(&db, &cat3, &|_| {}).unwrap_err();
    assert!(e.contains("Adobe_images.rootFile"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Full catalog schema (`catalog-schema.sql`) plus the tables for title, synonyms and stacks.
fn field_catalog(path: &Path, photos: &Path, title_a: &str) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(include_str!("catalog-schema.sql")).unwrap();
    c.execute_batch(
        "CREATE TABLE Adobe_AdditionalMetadata (id_local INTEGER PRIMARY KEY, image INTEGER, xmp NOT NULL DEFAULT '');
         CREATE TABLE AgLibraryKeywordSynonym (id_local INTEGER PRIMARY KEY, keyword INTEGER NOT NULL DEFAULT 0, lc_name, name);
         CREATE TABLE AgLibraryFolderStackImage (id_local INTEGER PRIMARY KEY, collapsed INTEGER NOT NULL DEFAULT 0, image INTEGER NOT NULL DEFAULT 0, position NOT NULL DEFAULT '', stack INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE Adobe_variablesTable (id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, name, type NOT NULL DEFAULT '', value NOT NULL DEFAULT '');",
    )
    .unwrap();
    c.execute("INSERT INTO Adobe_variablesTable(id_global, name, value) VALUES('v1', 'Adobe_DBVersion', '1500000')", []).unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(1,'r1',?1,'Fotos')", [format!("{}/", photos.display())]).unwrap();
    c.execute("INSERT INTO AgLibraryFolder(id_local,id_global,rootFolder,pathFromRoot) VALUES(10,'f10',1,'')", []).unwrap();
    for (id, base) in [(100, "A"), (101, "B"), (102, "C")] {
        c.execute("INSERT INTO AgLibraryFile(id_local,id_global,folder,baseName,extension,idx_filename) VALUES(?1,?2,10,?3,'JPG',?4)", params![id, format!("d{id}"), base, format!("{base}.JPG")]).unwrap();
    }
    // A: title (packed), GPS, label of a custom set; its virtual copy 1003 (stacked with it: not counted as a stack);
    // B: title as plain text, no GPS (hasGPS 0 with junk values); C: label of the default set; B and C stacked
    for (id, file, label, master) in [(1000, 100, "Winner", None), (1001, 101, "Prüfen", None), (1002, 102, "Rot", None), (1003, 100, "", Some(1000))] {
        c.execute(
            "INSERT INTO Adobe_images(id_local,id_global,rootFile,colorLabels,masterImage,copyName,captureTime) VALUES(?1,?2,?3,?4,?5,?6,'2025-03-01T10:00:00')",
            params![id, format!("i{id}"), file, label, master, master.map(|_| "Kopie 1")],
        )
        .unwrap();
    }
    c.execute("INSERT INTO AgHarvestedExifMetadata(image, hasGPS, gpsLatitude, gpsLongitude) VALUES(1000, 1, 47.5, 10.6666666666667), (1001, 0, 1.0, 2.0)", []).unwrap();
    c.execute("INSERT INTO Adobe_AdditionalMetadata(image, xmp) VALUES(1000, ?1)", [packed_xmp(title_a)]).unwrap();
    c.execute(
        "INSERT INTO Adobe_AdditionalMetadata(image, xmp) VALUES(1001, CAST(?1 AS BLOB))",
        [r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description><dc:title><rdf:Alt><rdf:li xml:lang="x-default">Strand &amp; Meer</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"#],
    )
    .unwrap();
    c.execute("INSERT INTO AgLibraryKeyword(id_local,id_global,name,parent,genealogy) VALUES(1,'k1',NULL,NULL,'/1'), (2,'k2','Katze',1,'/12')", []).unwrap();
    c.execute("INSERT INTO AgLibraryKeywordSynonym(keyword, lc_name, name) VALUES(2, 'mieze', 'Mieze'), (2, 'hauskatze', 'Hauskatze')", []).unwrap();
    c.execute("INSERT INTO AgLibraryFolderStackImage(image, position, stack) VALUES(1000, 1.0, 7), (1003, 2.0, 7), (1001, 1.0, 8), (1002, 2.0, 8)", []).unwrap();
}

/// Title (packed and plain XMP), GPS position, synonyms, stacks and labels of a custom set; a new import keeps a
/// title changed in graineo and takes a title changed in Lightroom otherwise.
#[test]
fn title_position_and_not_taken() {
    let dir = tmp("fields");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    let cat = dir.join("Felder.lrcat");
    field_catalog(&cat, &photos, "Bergsee");
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let r = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert!(r.catalog_gaps.is_empty(), "{:?}", r.catalog_gaps);
    assert_eq!((r.catalog_version, r.newer_format()), (Some(1_500_000), false));
    assert_eq!((r.with_title, r.with_position), (2, 1));
    assert_eq!((r.not_taken.synonyms, r.not_taken.stacks, r.not_taken.stacked_photos), (2, 1, 2));
    assert_eq!(r.unknown_labels.get("Winner"), Some(&1));
    assert_eq!(r.unknown_labels.get("Prüfen"), Some(&1));
    assert_eq!(r.json()["not_taken"]["unknown_labels"], 2);
    let id = |lr: i64| -> i64 { db.conn.query_row("SELECT id FROM image WHERE lr_id = ?1", [lr], |r| r.get(0)).unwrap() };
    assert_eq!(db.title_of(id(1000)).as_deref(), Some("Bergsee"));
    assert_eq!(db.title_of(id(1001)).as_deref(), Some("Strand & Meer"));
    assert_eq!(db.title_of(id(1002)), None);
    let (lat, lon) = db.position_of(id(1000)).unwrap();
    assert!((lat - 47.5).abs() < 1e-9 && (lon - 10.6666666666667).abs() < 1e-9);
    assert_eq!(db.position_of(id(1001)), None);
    // the custom label: no color, but its text is kept and searchable (with the title)
    let (label, label_lr, text): (i64, Option<String>, Option<String>) = db.conn.query_row("SELECT label, label_lr, lr_text FROM image WHERE id = ?1", [id(1000)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!((label, label_lr.as_deref()), (0, Some("Winner")));
    let text = text.unwrap();
    assert!(text.contains("Winner") && text.contains("Bergsee"), "{text}");
    assert_eq!(count(&db, &format!("SELECT label FROM image WHERE id = {}", id(1002))), 1);

    // unchanged catalog: a second import writes the same
    let r2 = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!((r2.with_title, r2.with_position, r2.own_kept), (2, 1, 0));

    // title changed in Lightroom: taken over (graineo kept the old one); changed in graineo as well: the own one stays
    let _ = std::fs::remove_file(&cat);
    field_catalog(&cat, &photos, "Bergsee am Abend");
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(db.title_of(id(1000)).as_deref(), Some("Bergsee am Abend"));
    db.conn.execute("UPDATE image_meta SET title = 'Mein Titel', latitude = 1.5, longitude = 2.5 WHERE image = ?1", [id(1000)]).unwrap();
    let _ = std::fs::remove_file(&cat);
    field_catalog(&cat, &photos, "Bergsee bei Nacht");
    let r3 = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(db.title_of(id(1000)).as_deref(), Some("Mein Titel"));
    assert_eq!(db.position_of(id(1000)), Some((1.5, 2.5)));
    assert!(r3.own_kept >= 2, "{}", r3.own_kept);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The small Lightroom Classic 15.1 test catalog (local, not in the repository): all fields that should
/// come along do, the format is the tested one, nothing is missing; the catalog file stays unchanged.
#[test]
fn test_catalog_15_1() {
    let cat = PathBuf::from(std::env::var("GRAINEO_LR_TEST_CATALOG_15").unwrap_or_else(|_| "/Users/Shared/graineo-lrtest/katalog-15-1/katalog-15-1.lrcat".into()));
    if !cat.is_file() {
        eprintln!("{} not there, skipped", cat.display());
        return;
    }
    let before = fingerprint(&cat);
    let dir = tmp("cat15");
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let p = lightroom::preview(&db, &cat).unwrap();
    assert_eq!((p.images, p.virtual_copies, p.edited), (28, 1, 11));
    assert_eq!((p.catalog_version, p.newer_format()), (Some(1_500_000), false));
    assert!(p.catalog_gaps.is_empty(), "{:?}", p.catalog_gaps);
    let r = lightroom::import(&db, &cat, &|_| {}).unwrap();
    println!("{}", serde_json::to_string_pretty(&r.json()).unwrap());
    assert!(r.catalog_gaps.is_empty(), "{:?}", r.catalog_gaps);
    assert_eq!((r.images, r.with_title, r.with_position), (28, 8, 2));
    // the one stack of two different photos is taken over (the stack of an original with its virtual copy is a
    // version in graineo); top = position 1 in the catalog
    assert_eq!((r.not_taken.synonyms, r.not_taken.stacks, r.stacks), (4, 0, 1));
    assert_eq!(count(&db, "SELECT count(*) FROM stack WHERE origin = 'lightroom' AND kind = 'manual'"), 1);
    assert_eq!(count(&db, "SELECT count(*) FROM stack_image"), 2);
    assert_eq!(count(&db, "SELECT count(*) FROM stack s JOIN image b ON b.id = s.cover WHERE b.lr_id = 200"), 1);
    assert_eq!((r.unknown_labels.get("Winner"), r.unknown_labels.get("Prüfen")), (Some(&3), Some(&1)));
    assert_eq!(count(&db, "SELECT count(*) FROM image WHERE caption IS NOT NULL AND caption <> ''"), 5);
    assert_eq!(before, fingerprint(&cat), "catalog changed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore]
fn import_real_catalogs() {
    let Ok(list) = std::env::var("GRAINEO_LR_TEST_CATALOGS") else {
        eprintln!("GRAINEO_LR_TEST_CATALOGS not set, skipped");
        return;
    };
    for (n, cat) in list.split(':').filter(|s| !s.is_empty()).enumerate() {
        let cat = PathBuf::from(cat);
        let before = fingerprint(&cat);
        let dir = tmp(&format!("c{n}"));
        let db = Db::open(&dir.join("library.sqlite")).unwrap();
        if let Ok(p) = lightroom::preview(&db, &cat) {
            println!("PREVIEW {}\n{}", cat.display(), serde_json::to_string_pretty(&p.json()).unwrap());
        }
        let r = lightroom::import(&db, &cat, &|_| {});
        match &r {
            Ok(b) => {
                println!("CATALOG {}\n{}", cat.display(), serde_json::to_string_pretty(&b.json()).unwrap());
                println!(
                    "library: images={} copies={} stars={} flagged={} rejected={} label={} keywords={} keyword_links={} collections={} captions={} titles={} positions={}",
                    count(&db, "SELECT count(*) FROM image"),
                    count(&db, "SELECT count(*) FROM image WHERE copy IS NOT NULL"),
                    count(&db, "SELECT count(*) FROM image WHERE stars > 0"),
                    count(&db, "SELECT count(*) FROM image WHERE flag = 1"),
                    count(&db, "SELECT count(*) FROM image WHERE flag = -1"),
                    count(&db, "SELECT count(*) FROM image WHERE label > 0"),
                    count(&db, "SELECT count(*) FROM keyword"),
                    count(&db, "SELECT count(*) FROM image_keyword"),
                    count(&db, "SELECT count(*) FROM collection"),
                    count(&db, "SELECT count(*) FROM image WHERE caption IS NOT NULL AND caption <> ''"),
                    count(&db, "SELECT count(*) FROM image_meta WHERE title IS NOT NULL"),
                    count(&db, "SELECT count(*) FROM image_meta WHERE latitude IS NOT NULL"),
                );
            }
            Err(e) => println!("CATALOG {}\nERROR {e}", cat.display()),
        }
        assert!(r.is_ok(), "import failed for {}", cat.display());
        assert_eq!(before, fingerprint(&cat), "catalog changed: {}", cat.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
