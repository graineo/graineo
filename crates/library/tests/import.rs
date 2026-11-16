//! Lightroom import and sync against a mini catalog with the real schema
//! (`catalog-schema.sql`, read from a Lightroom Classic catalog).

use std::path::{Path, PathBuf};

use library::{reconcile, db::Db, lightroom};
use rusqlite::{params, Connection};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("x3f-bibliothek-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn catalog(path: &Path, photos: &Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(include_str!("catalog-schema.sql")).unwrap();
    let root_dir = format!("{}/", photos.display());
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(1,'r1',?1,'Fotos')", [&root_dir]).unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(2,'r2','/Volumes/GibtEsNicht-x3f/Bilder/','Bilder')", []).unwrap();
    for (id, root, p) in [(10, 1, ""), (11, 1, "2024/"), (12, 1, "2024/Urlaub ä/"), (13, 2, "")] {
        c.execute("INSERT INTO AgLibraryFolder(id_local,id_global,rootFolder,pathFromRoot) VALUES(?1,?2,?3,?4)", params![id, format!("f{id}"), root, p]).unwrap();
    }
    // files: CR3 (with sidecar JPG), JPG, HEIC, missing DNG, CR2 on a missing volume, missing RAF with its JPEG
    for (id, folder, base, ext) in [(100, 11, "IMG_1", "CR3"), (101, 11, "IMG_2", "JPG"), (102, 12, "IMG_3", "HEIC"), (103, 12, "R0001", "DNG"), (104, 13, "IMG_9", "CR2"), (105, 12, "DSCF1", "RAF"), (106, 12, "DSCF1", "jpg")] {
        c.execute(
            "INSERT INTO AgLibraryFile(id_local,id_global,folder,baseName,extension,idx_filename,originalFilename,sidecarExtensions) VALUES(?1,?2,?3,?4,?5,?6,?6,?7)",
            params![id, format!("d{id}"), folder, base, ext, format!("{base}.{ext}"), if id == 100 { "JPG" } else { "" }],
        )
        .unwrap();
    }
    c.execute("INSERT INTO AgInternedExifCameraModel(id_local,value) VALUES(1,'Canon EOS RP')", []).unwrap();
    // images: id, file, rating, pick, label, master, copyName, captureTime, orientation, fileFormat
    let images: [(i64, i64, Option<f64>, f64, &str, Option<i64>, Option<&str>, &str, &str, &str); 9] = [
        (1000, 100, Some(5.0), 1.0, "Grün", None, None, "2024-05-11T14:03:22.12", "AB", "RAW"),
        (1001, 100, Some(3.0), 0.0, "", Some(1000), Some("Kopie 1"), "2024-05-11T14:03:22", "AB", "RAW"),
        (1002, 101, None, -1.0, "Good to Use", None, None, "2024-05-12T10:00:00", "BC", "JPG"),
        (1003, 102, Some(2.0), 0.0, "Pink", None, None, "2024-06-01T09:00:00", "AB", "HEIC"),
        (1004, 103, Some(1.0), 0.0, "Rot", None, None, "2024-06-02T09:00:00", "AB", "DNG"),
        (1005, 104, Some(4.0), 0.0, "Blue", None, None, "2010-01-01T00:00:00", "AB", "RAW"),
        (1006, 105, None, 0.0, "", None, None, "2024-06-03T09:00:00", "AB", "RAW"),
        (1007, 106, None, 0.0, "Eigenes Etikett", None, None, "2024-06-03T09:00:00", "AB", "JPG"),
        (1008, 101, Some(1.0), 0.0, "", Some(1002), Some("Copy 2"), "2024-05-12T10:00:00", "BC", "JPG"),
    ];
    for (id, file, rating, pick, label, master, copy, t, o, fmt) in images {
        c.execute(
            "INSERT INTO Adobe_images(id_local,id_global,rootFile,rating,pick,colorLabels,masterImage,copyName,captureTime,orientation,fileFormat,fileWidth,fileHeight) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,6000,4000)",
            params![id, format!("i{id}"), file, rating, pick, label, master, copy, t, o, fmt],
        )
        .unwrap();
        c.execute("INSERT INTO AgHarvestedExifMetadata(image,cameraModelRef) VALUES(?1,1)", [id]).unwrap();
    }
    // keywords: invisible root 1 → Personen → Anna; Orte → Italien → Rom
    for (id, name, parent, type_id) in [(1, None, None, None), (2, Some("Personen"), Some(1), None), (3, Some("Anna"), Some(2), Some("person")), (4, Some("Orte"), Some(1), None), (5, Some("Italien"), Some(4), None), (6, Some("Rom"), Some(5), None)] {
        c.execute("INSERT INTO AgLibraryKeyword(id_local,id_global,name,parent,genealogy,keywordType) VALUES(?1,?2,?3,?4,?5,?6)", params![id, format!("k{id}"), name, parent, format!("/{id}"), type_id]).unwrap();
    }
    // "Orte" not included on export (Lightroom: the mark of a branch)
    c.execute("UPDATE AgLibraryKeyword SET includeOnExport=0 WHERE id_local=4", []).unwrap();
    for (img, tag) in [(1000, 3), (1000, 6), (1002, 6), (1003, 5)] {
        c.execute("INSERT INTO AgLibraryKeywordImage(image,tag) VALUES(?1,?2)", [img, tag]).unwrap();
    }
    // collections: set "Reisen" with collection "Rom 2024", smart collection, quick collection
    for (id, name, parent, cid, sys) in [
        (50, "Reisen", None, "com.adobe.ag.library.group", "0.0"),
        (51, "Rom 2024", Some(50), "com.adobe.ag.library.collection", "0.0"),
        (52, "Fünf Sterne", None, "com.adobe.ag.library.smart_collection", "0.0"),
        (53, "quick collection", None, "com.adobe.ag.library.collection", "1.0"),
        (54, "Druck", None, "com.adobe.ag.print", "0.0"),
    ] {
        c.execute("INSERT INTO AgLibraryCollection(id_local,name,parent,creationId,genealogy,systemOnly) VALUES(?1,?2,?3,?4,?5,?6)", params![id, name, parent, cid, format!("/{id}"), sys]).unwrap();
    }
    for (coll, img, pos) in [(51, 1000, 2.0), (51, 1002, 1.0), (53, 1003, 1.0)] {
        c.execute("INSERT INTO AgLibraryCollectionImage(collection,image,positionInCollection) VALUES(?1,?2,?3)", params![coll, img, pos]).unwrap();
    }
    c.execute("INSERT INTO AgLibraryCollectionContent(collection,owningModule,content) VALUES(52,'ag.library.smart_collection','s = { { criteria = \"rating\", operation = \">=\", value = 5 } }')", []).unwrap();
}

fn disk(photos: &Path) {
    let d = photos.join("2024");
    let u = d.join("Urlaub ä");
    std::fs::create_dir_all(&u).unwrap();
    std::fs::write(d.join("IMG_1.CR3"), b"cr3").unwrap();
    std::fs::write(d.join("IMG_2.JPG"), b"jpg").unwrap();
    // sidecar JPEG for the CR3 (Lightroom: sidecarExtensions "JPG") – not an image of its own
    std::fs::write(d.join("IMG_1.jpg"), b"jpg").unwrap();
    std::fs::write(u.join("IMG_3.HEIC"), b"heic").unwrap();
    // R0001.DNG and DSCF1.RAF missing (only the JPEG of the RAF is there)
    std::fs::write(u.join("DSCF1.jpg"), b"jpg").unwrap();
    // new file in a known folder (not in the catalog) + X3F with own sidecar and foreign XMP
    std::fs::write(d.join("NEU_1.jpg"), b"jpg").unwrap();
    std::fs::write(d.join("SDIM0001.X3F"), b"x3f").unwrap();
    // own sidecar (namespace graineo:): read as our own
    std::fs::write(
        d.join("SDIM0001.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:graineo="https://graineo.github.io/ns/1.0/" xmp:CreatorTool="graineo" xmp:Rating="4" graineo:Flag="1"><graineo:DevelopSettings>{&quot;exposure&quot;:0.3}</graineo:DevelopSettings></rdf:Description></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();
    // unknown folder with images (only reported); a package is skipped
    std::fs::create_dir_all(photos.join("Unbekannt")).unwrap();
    std::fs::write(photos.join("Unbekannt/a.jpg"), b"jpg").unwrap();
    std::fs::create_dir_all(photos.join("Photos Library.photoslibrary/originals")).unwrap();
    std::fs::write(photos.join("Photos Library.photoslibrary/originals/b.jpg"), b"jpg").unwrap();
}

fn count_rows(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn import_reconcile_repeatable() {
    let dir = tmp("import");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let cat_before = std::fs::read(&cat).unwrap();
    let db = Db::open(&dir.join("bibliothek.sqlite")).unwrap();

    let b = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!((b.roots, b.folder, b.files, b.images, b.virtual_copies), (2, 4, 7, 9, 2));
    assert_eq!(b.new_images, 9);
    assert_eq!((b.keywords, b.keyword_assignments), (5, 4));
    // includeOnExport = 0 becomes the mark "never export" (the whole branch: Rom is not exported either)
    let orte = count_rows(&db, "SELECT id FROM keyword WHERE name='Orte'");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM keyword_option WHERE export=0"), 1);
    assert!(library::keywords::export_marked(&db.conn, orte));
    let b1000 = count_rows(&db, "SELECT id FROM image WHERE lr_id=1000");
    assert!(library::keywords::export_lists(&db.conn, b1000, true).unwrap().0.is_empty(), "Anna is a person, Rom below Orte");
    assert_eq!((b.collections, b.collection_sets, b.smart_collections, b.collection_assignments), (2, 1, 1, 3));
    assert_eq!(b.unknown_labels.get("Eigenes Etikett"), Some(&1));
    // catalog unchanged (only read, no -wal/-shm created)
    assert_eq!(std::fs::read(&cat).unwrap(), cat_before);
    assert!(!dir.join("Test.lrcat-wal").exists() && !dir.join("Test.lrcat-shm").exists());

    // values
    let (s, m, e, lr): (i64, i64, i64, String) = db.conn.query_row("SELECT stars, flag, label, label_lr FROM image WHERE lr_id=1000", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!((s, m, e, lr.as_str()), (5, 1, 3, "Grün"));
    assert_eq!(count_rows(&db, "SELECT label FROM image WHERE lr_id=1002"), 3); // Good to Use → green
    assert_eq!(count_rows(&db, "SELECT flag FROM image WHERE lr_id=1002"), -1);
    assert_eq!(count_rows(&db, "SELECT label FROM image WHERE lr_id=1003"), 5); // Pink → purple
    let copy: String = db.conn.query_row("SELECT copy FROM image WHERE lr_id=1001", [], |r| r.get(0)).unwrap();
    assert_eq!(copy, "Kopie 1");
    assert_eq!(count_rows(&db, "SELECT count(DISTINCT file) FROM image WHERE lr_id IN (1000,1001)"), 1);
    let (fmt, cam, capture, orient): (String, String, String, i64) =
        db.conn.query_row("SELECT format, camera, capture_time, orientation FROM file WHERE lr_id=101", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!((fmt.as_str(), cam.as_str(), capture.as_str(), orient), ("JPG", "Canon EOS RP", "2024-05-12T10:00:00", 6));
    assert_eq!(count_rows(&db, "SELECT count(*) FROM file WHERE sidecars='JPG'"), 1);
    // keyword hierarchy and collections
    let rome = count_rows(&db, "SELECT id FROM keyword WHERE name='Rom'");
    let places = count_rows(&db, "SELECT id FROM keyword WHERE name='Orte'");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM keyword WHERE parent IS NULL"), 2);
    assert_eq!(db.images_with_keyword(places).unwrap().len(), 3);
    assert_eq!(db.images_with_keyword(rome).unwrap().len(), 2);
    let b1000 = count_rows(&db, "SELECT id FROM image WHERE lr_id=1000");
    assert_eq!(db.keywords_of(b1000).unwrap(), vec!["Orte › Italien › Rom".to_string(), "Personen › Anna".to_string()]);
    assert_eq!(count_rows(&db, "SELECT person FROM keyword WHERE name='Anna'"), 1);
    let rom24 = count_rows(&db, "SELECT id FROM collection WHERE name='Rom 2024'");
    assert_eq!(count_rows(&db, "SELECT kind FROM collection WHERE id=(SELECT parent FROM collection WHERE name='Rom 2024')"), 1);
    let order_list = db.images_of_collection(rom24).unwrap();
    assert_eq!(order_list, vec![count_rows(&db, "SELECT id FROM image WHERE lr_id=1002"), b1000]);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection WHERE name='Schnellsammlung'"), 1);
    assert!(db.conn.query_row("SELECT rules FROM collection WHERE kind=2", [], |r| r.get::<_, String>(0)).unwrap().contains("rating"));
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection WHERE name='Druck'"), 0);
    // search
    assert_eq!(db.search("rom").unwrap().len(), 2);
    assert_eq!(db.search("img_3").unwrap().len(), 1);

    // ── Sync ──
    let a = reconcile::reconcile(&db, None, &|_| {}).unwrap();
    assert_eq!(a.offline.len(), 1, "{:?}", a.offline);
    assert_eq!(a.offline[0].1, 1);
    assert_eq!(a.missing, 2, "{:?}", a.missing_list); // R0001.DNG, DSCF1.RAF
    assert!(a.missing_list.iter().any(|p| p.ends_with("R0001.DNG")) && a.missing_list.iter().any(|p| p.ends_with("DSCF1.RAF")));
    assert_eq!(a.newly_added, 2); // NEU_1.jpg, SDIM0001.X3F
    assert_eq!(a.sidecars, 1); // IMG_1.jpg
    assert_eq!(a.unknown_folders.len(), 1);
    assert!(a.unknown_folders.keys().next().unwrap().ends_with("Unbekannt/"));
    // "Nicht im Katalog" lists the same folder
    let nic = library::not_in_catalog::search(&db, &|_| {}).unwrap();
    assert_eq!(nic.len(), 1, "{nic:?}");
    assert!(nic[0].rel.ends_with("Unbekannt/") && nic[0].count >= 1, "{nic:?}");
    assert_eq!(a.conflicts.len(), 0, "{:?}", a.conflicts); // X3F: only the own sidecar counts
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE lr_id=103"), 1);
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE lr_id=104"), 2);
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE lr_id=105"), 1);
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE lr_id=106"), 0);
    assert_eq!(count_rows(&db, "SELECT size FROM file WHERE lr_id=100"), 3);
    let (s, m): (i64, i64) = db.conn.query_row("SELECT b.stars, b.flag FROM image b JOIN file d ON d.id=b.file WHERE d.name='SDIM0001.X3F'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((s, m), (4, 1));
    // umlaut folder found (NFC/NFD)
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE lr_id=102"), 0);

    // ── Repeat: no duplicates ──
    let n_image = count_rows(&db, "SELECT count(*) FROM image");
    let n_file = count_rows(&db, "SELECT count(*) FROM file");
    let b2 = lightroom::import(&db, &cat, &|_| {}).unwrap();
    let a2 = reconcile::reconcile(&db, None, &|_| {}).unwrap();
    assert_eq!(b2.new_images, 0);
    assert_eq!(a2.newly_added, 0, "{}", a2.json());
    assert_eq!(a2.missing, 2, "{}", a2.json());
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image"), n_image);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM file"), n_file);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image_keyword"), 4);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection_image"), 3);

    // ── Conflict rules ──
    // own change (IMG_2: 0 → 2 stars), Lightroom unchanged → own value stays
    let b1002 = count_rows(&db, "SELECT id FROM image WHERE lr_id=1002");
    db.set_rating(&[b1002], Some(2), None, None).unwrap();
    // only Lightroom changed (IMG_3: 2 → 3 stars) → Lightroom wins
    // both changed (IMG_1: LR 5 → 4, own 5 → 1) → own value stays, conflict
    db.set_rating(&[b1000], Some(1), None, None).unwrap();
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("UPDATE Adobe_images SET rating=3 WHERE id_local=1003", []).unwrap();
        c.execute("UPDATE Adobe_images SET rating=4 WHERE id_local=1000", []).unwrap();
    }
    let b3 = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1002"), 2);
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1003"), 3);
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1000"), 1);
    assert_eq!(b3.conflicts.len(), 1, "{:?}", b3.conflicts);
    assert!(b3.conflicts[0].contains("IMG_1.CR3"));

    // quick collection: after the import it belongs to the app; a new import leaves it untouched
    let sq = db.quick_collection().unwrap();
    let b1003 = count_rows(&db, "SELECT id FROM image WHERE lr_id=1003");
    assert_eq!(db.images_of_collection(sq).unwrap(), vec![b1003], "taken over from Lightroom");
    db.quick_set(&[b1003], false).unwrap();
    db.quick_set(&[b1000], true).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(count_rows(&db, &format!("SELECT kind FROM collection WHERE id={sq}")), 3);
    assert_eq!(db.images_of_collection(sq).unwrap(), vec![b1000], "own content stays");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection WHERE kind=3"), 1);

    // Lightroom label mapping: all values of a real catalog are known
    // the Lightroom takeover is no import from the import window: "Zuletzt importiert" stays empty
    assert!(db.last_import().is_none());

    // keywords removed in Lightroom: kept as own keywords while graineo uses them (own assignments, faces)
    db.conn.execute("UPDATE image SET keywords_changed = 1 WHERE id = ?1", [b1002]).unwrap();
    let anna = count_rows(&db, "SELECT id FROM keyword WHERE lr_id=3");
    db.conn.execute("INSERT INTO face(image, x, y, w, h, source, status, person) VALUES(?1, 0.1, 0.1, 0.1, 0.1, 'graineo', 'confirmed', ?2)", [b1000, anna]).unwrap();
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("DELETE FROM AgLibraryKeywordImage WHERE tag IN (3, 6)", []).unwrap();
        c.execute("DELETE FROM AgLibraryKeyword WHERE id_local IN (3, 6)", []).unwrap();
    }
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(count_rows(&db, "SELECT count(*) FROM keyword WHERE name='Rom' AND lr_id IS NULL"), 1, "own assignment keeps it");
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM image_keyword bs JOIN keyword s ON s.id = bs.keyword WHERE s.name='Rom' AND bs.image={b1002}")), 1);
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM keyword WHERE id={anna} AND lr_id IS NULL")), 1, "the face keeps its name");
    assert_eq!(count_rows(&db, &format!("SELECT person FROM face WHERE image={b1000}")), anna);

    // own smart collection: lives in the library; the file in the root folder is only an optional copy
    db.save_smart("Schimmel", "schimmel sterne:>=1").unwrap();
    let file = library::collection_file::write(&db).unwrap();
    assert_eq!(file, photos.join(".graineo/collections.json"));
    assert!(std::fs::read_to_string(&file).unwrap().contains("\"smartCollections\""));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A new Lightroom import keeps what was added or changed in graineo: own assignments to
/// Lightroom collections, assignments taken out in graineo, collections deleted in Lightroom, ratings and names
/// changed in graineo, collections deleted in graineo. What changed only in Lightroom comes over.
#[test]
fn reimport_keeps_own_changes() {
    use library::manage;
    let dir = tmp("eigenes");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    // one more collection "Alt" with IMG_2 (deleted in Lightroom later)
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("INSERT INTO AgLibraryCollection(id_local,name,parent,creationId,genealogy,systemOnly) VALUES(55,'Alt',NULL,'com.adobe.ag.library.collection','/55','0.0')", []).unwrap();
        c.execute("INSERT INTO AgLibraryCollectionImage(id_local,collection,image,positionInCollection) VALUES(2001,55,1002,1.0)", []).unwrap();
    }
    let cat_before = std::fs::read(&cat).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(std::fs::read(&cat).unwrap(), cat_before, "catalog only read");
    let id = |sql: &str| count_rows(&db, sql);
    let (b1000, b1002, b1003, b1006) = (id("SELECT id FROM image WHERE lr_id=1000"), id("SELECT id FROM image WHERE lr_id=1002"), id("SELECT id FROM image WHERE lr_id=1003"), id("SELECT id FROM image WHERE lr_id=1006"));
    let (rom, alt, travel) = (id("SELECT id FROM collection WHERE lr_id=51"), id("SELECT id FROM collection WHERE lr_id=55"), id("SELECT id FROM collection WHERE lr_id=50"));
    let mut in_rom = db.images_of_collection(rom).unwrap();
    in_rom.sort();
    assert_eq!(in_rom, vec![b1000, b1002]);

    // ── changes in graineo ──
    manage::add_to_collection(&db, rom, &[b1003]).unwrap(); // own assignment
    db.conn.execute("DELETE FROM collection_image WHERE collection=?1 AND image=?2", [rom, b1000]).unwrap(); // taken out in graineo
    db.set_rating(&[b1002], Some(4), None, None).unwrap(); // IMG_2: 0 → 4 stars
    manage::rename_collection(&db, travel, "Reisen alt").unwrap();
    manage::delete_collection(&db, id("SELECT id FROM collection WHERE lr_id=52")).unwrap(); // smart collection "Fünf Sterne"
    let italy = id("SELECT id FROM keyword WHERE lr_id=5");
    manage::rename_keyword(&db, italy, "Italia").unwrap();

    // ── changes in Lightroom (new ids above everything known: one counter for all tables) ──
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("DELETE FROM AgLibraryCollectionImage WHERE collection=55", []).unwrap();
        c.execute("DELETE FROM AgLibraryCollection WHERE id_local=55", []).unwrap(); // "Alt" deleted in Lightroom
        c.execute("UPDATE Adobe_images SET rating=5 WHERE id_local=1003", []).unwrap(); // only Lightroom changed
        c.execute("UPDATE Adobe_images SET rating=3, colorLabels='Rot' WHERE id_local=1002", []).unwrap(); // stars on both sides, label only here
        c.execute("UPDATE AgLibraryCollection SET name='Reisen LR' WHERE id_local=50", []).unwrap();
        c.execute("UPDATE AgLibraryCollection SET name='Rom 24' WHERE id_local=51", []).unwrap();
        c.execute("UPDATE AgLibraryKeyword SET name='Italy' WHERE id_local=5", []).unwrap();
        c.execute("DELETE FROM AgLibraryCollectionImage WHERE collection=51 AND image=1002", []).unwrap(); // removed in Lightroom
        c.execute("INSERT INTO AgLibraryCollectionImage(id_local,collection,image,positionInCollection) VALUES(3001,51,1006,5.0)", []).unwrap(); // new in Lightroom
        c.execute("INSERT INTO AgLibraryCollection(id_local,name,parent,creationId,genealogy,systemOnly) VALUES(3002,'Neu',NULL,'com.adobe.ag.library.collection','/3002','0.0')", []).unwrap();
        c.execute("INSERT INTO AgLibraryCollectionImage(id_local,collection,image,positionInCollection) VALUES(3003,3002,1000,1.0)", []).unwrap();
    }
    let r = lightroom::import(&db, &cat, &|_| {}).unwrap();

    // own assignment stays, the one taken out in graineo stays out, removed in Lightroom goes, new in Lightroom comes
    let mut in_rom = db.images_of_collection(rom).unwrap();
    in_rom.sort();
    assert_eq!(in_rom, vec![b1003, b1006], "{}", r.json());
    // collection deleted in Lightroom stays, with its photo, as an own collection
    assert_eq!(db.images_of_collection(alt).unwrap(), vec![b1002]);
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM collection WHERE id={alt} AND lr_id IS NULL AND name='Alt'")), 1);
    // collection deleted in graineo does not come back
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection WHERE name='Fünf Sterne'"), 0);
    // new collection from Lightroom comes in
    let new = id("SELECT id FROM collection WHERE lr_id=3002");
    assert_eq!(db.images_of_collection(new).unwrap(), vec![b1000]);
    // names: changed in graineo → stays; changed only in Lightroom → comes over
    assert_eq!(db.conn.query_row(&format!("SELECT name FROM collection WHERE id={travel}"), [], |r| r.get::<_, String>(0)).unwrap(), "Reisen alt");
    assert_eq!(db.conn.query_row(&format!("SELECT name FROM collection WHERE id={rom}"), [], |r| r.get::<_, String>(0)).unwrap(), "Rom 24");
    assert_eq!(db.conn.query_row(&format!("SELECT name FROM keyword WHERE id={italy}"), [], |r| r.get::<_, String>(0)).unwrap(), "Italia");
    // ratings: changed in graineo → stays (conflict named); label changed only in Lightroom → comes; only Lightroom changed → comes
    assert_eq!(count_rows(&db, &format!("SELECT stars FROM image WHERE id={b1002}")), 4);
    assert_eq!(count_rows(&db, &format!("SELECT label FROM image WHERE id={b1002}")), 1);
    assert_eq!(count_rows(&db, &format!("SELECT stars FROM image WHERE id={b1003}")), 5);
    assert_eq!(r.conflicts.len(), 1, "{:?}", r.conflicts);
    assert!(r.conflicts[0].contains("IMG_2.JPG") && r.conflicts[0].contains("Sterne"), "{:?}", r.conflicts);
    assert!(r.own_kept >= 4, "{}", r.json());

    // a third import changes nothing
    let snapshot = |db: &Db| -> Vec<(i64, i64)> {
        let mut st = db.conn.prepare("SELECT collection, image FROM collection_image ORDER BY 1, 2").unwrap();
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
        v
    };
    let before = (snapshot(&db), count_rows(&db, "SELECT count(*) FROM collection"), count_rows(&db, "SELECT sum(stars * 10 + label) FROM image"));
    let r3 = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!((snapshot(&db), count_rows(&db, "SELECT count(*) FROM collection"), count_rows(&db, "SELECT sum(stars * 10 + label) FROM image")), before);
    assert_eq!(r3.new_images, 0);
    // Lightroom changes again where graineo kept its own stars: Lightroom's new value does not win either
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("UPDATE Adobe_images SET rating=2 WHERE id_local=1002", []).unwrap();
    }
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(count_rows(&db, &format!("SELECT stars FROM image WHERE id={b1002}")), 4);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn own_root() {
    let dir = tmp("stamm");
    let f = dir.join("SIGMA");
    std::fs::create_dir_all(f.join("102SIGMA")).unwrap();
    std::fs::write(f.join("102SIGMA/A.X3F"), b"x").unwrap();
    std::fs::write(f.join("102SIGMA/A.JPG"), b"x").unwrap();
    std::fs::write(f.join("notiz.txt"), b"x").unwrap();
    let db = Db::open(&dir.join("b.sqlite")).unwrap();
    let (id, b) = reconcile::add_root(&db, &f, &|_| {}).unwrap();
    assert_eq!(b.newly_added, 2);
    // adding a subfolder → the same root, nothing duplicated
    let (id2, b2) = reconcile::add_root(&db, &f.join("102SIGMA"), &|_| {}).unwrap();
    assert_eq!((id, b2.newly_added), (id2, 0));
    // parent folder of a root → rejected
    assert!(reconcile::add_root(&db, &dir, &|_| {}).is_err());
    let p = db.file_path(count_rows(&db, "SELECT id FROM file WHERE name='A.X3F'")).unwrap();
    assert_eq!(p, f.join("102SIGMA/A.X3F"));
    assert!(db.image_to_path(&p).is_some());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// AI descriptions (matched via Lightroom file ID and path), full-text index and search.
#[test]
fn ai_index_search() {
    use library::{ai, search};
    let dir = tmp("ki");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let db = Db::open(&dir.join("b.sqlite")).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    reconcile::reconcile(&db, None, &|_| {}).unwrap();
    // AI descriptions as the library keeps them (the file of Lightroom id 100 and a new photo)
    let neu = photos.join("2024/NEU_1.jpg");
    db.conn
        .execute(
            "INSERT INTO ai(file, source, model, keywords, scene, quality, technical, discard, reason, nonsense, created)
             SELECT id, ?1, 'm', '[\"Schimmel\", \"Wand\"]', 'Schimmelbefall an der Wand im Bad', 2, 0.7, 0.2, 'scharf', 0, '2025-07-27' FROM file WHERE lr_id = 100",
            [ai::SOURCE],
        )
        .unwrap();
    let neu_file = db.image_to_path(&neu).and_then(|b| db.file_of(b)).unwrap();
    db.conn
        .execute(
            "INSERT INTO ai(file, source, model, keywords, scene, quality, technical, discard, reason, nonsense, created) VALUES(?1, ?2, 'm', '[\"Hund\"]', 'Ein Hund vor feuchten Wänden', 1, 0.5, 0.4, 'unscharf', 0, '2025-07-27')",
            rusqlite::params![neu_file, ai::SOURCE],
        )
        .unwrap();
    assert_eq!(count_rows(&db, "SELECT count(*) FROM ai"), 2);
    let (n, _) = search::build_index(&db).unwrap();
    assert!(n >= 9);
    let rows = db.rows().unwrap();
    let paths = db.folder_paths().unwrap();
    let k = search::Context { db: &db, rows: &rows, folder_paths: &paths };
    let matches = |q: &str| -> Vec<String> {
        let v = search::evaluate(&k, &search::parse(q));
        let mut t: Vec<String> = rows.iter().zip(v).filter(|(_, ok)| *ok).map(|(z, _)| format!("{}{}", z.name, z.copy.as_deref().map(|k| format!("/{k}")).unwrap_or_default())).collect();
        t.sort();
        t
    };
    assert_eq!(matches("schimmel"), vec!["IMG_1.CR3", "IMG_1.CR3/Kopie 1"]);
    assert_eq!(matches("wand schimmel"), vec!["IMG_1.CR3", "IMG_1.CR3/Kopie 1"]);
    assert_eq!(matches("wände"), vec!["IMG_1.CR3", "IMG_1.CR3/Kopie 1", "NEU_1.jpg"]);
    assert_eq!(matches("schimel"), vec!["IMG_1.CR3", "IMG_1.CR3/Kopie 1"]); // typo
    assert_eq!(matches("hund | schimmel").len(), 3);
    assert_eq!(matches("wände -hund").len(), 2);
    assert_eq!(matches("rom"), vec!["IMG_1.CR3", "IMG_2.JPG"]); // keyword including parent term
    assert_eq!(matches("orte").len(), 3); // parent term "Orte"
    assert_eq!(matches("stichwort:anna"), vec!["IMG_1.CR3"]);
    assert_eq!(matches("kamera:\"eos rp\" sterne:>=3"), vec!["IMG_1.CR3", "IMG_1.CR3/Kopie 1", "IMG_9.CR2"]);
    assert_eq!(matches("ki:mittel"), vec!["NEU_1.jpg"]);
    assert_eq!(matches("format:heic").len(), 1);
    assert_eq!(matches("ohne:stichwort").len(), rows.len() - 3); // IMG_1, IMG_2, IMG_3 have keywords
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Sidecars in both directions, rebuild (only in temp folders),
/// keywords, virtual copies, moving/renaming including sidecars with ⌘Z,
/// refinding files moved in the Finder, card import, backup.
#[test]
fn m3_move_sidecars_import() {
    use library::{sidecar, card_import, backup, sync, relocate, xmp, store, sidecar_import};
    // the library is the main store; the sidecars next to the photos are what the background writer would write with
    // "Begleitdateien neben die Fotos schreiben" on – here it is called directly
    let write_all = |db: &Db| {
        let ids: Vec<i64> = { let mut st = db.conn.prepare("SELECT id FROM file").unwrap(); let v = st.query_map([], |r| r.get(0)).unwrap().flatten().collect(); v };
        for id in ids {
            store::enqueue(&db.conn, id).unwrap();
        }
        store::write_queued(db, 100_000, true, &|| {}).unwrap()
    };
    let dir = tmp("m3");
    std::env::set_var("GRAINEO_LIBRARY_JOURNAL", dir.join("journal"));
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let u = photos.join("2024/Urlaub ä");
    // foreign XMP with Camera Raw settings next to the HEIC
    std::fs::write(u.join("IMG_3.xmp"), "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" crs:Exposure2012=\"+0.35\"/></rdf:RDF></x:xmpmeta>").unwrap();
    let db_path = dir.join("library.sqlite");
    let db = Db::open(&db_path).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    reconcile::reconcile(&db, None, &|_| {}).unwrap();
    // nothing is written next to the photos while writing sidecars is off
    assert!(!photos.join("2024/IMG_1.xmp").exists() && !photos.join("2024/IMG_2.xmp").exists());
    // existing sidecars are read once (background reading), then written from the library
    sidecar_import::import_unread(&db, 10_000, &|| {}).unwrap();
    assert!(sidecar_import::unread_count(&db.conn) <= 1); // a photo of the catalog on a drive that is not there stays for later
    let r = write_all(&db);
    assert!(r.written >= 3, "{r:?}"); // IMG_1.CR3, IMG_2.JPG, IMG_3.HEIC …
    let cr3 = photos.join("2024/IMG_1.CR3");
    let Xmp(w) = read_xmp(&photos.join("2024/IMG_1.xmp")) else { panic!() };
    assert_eq!((w.stars, w.flag, w.label), (5, 1, 3));
    assert!(w.keywords.contains(&"Orte|Italien|Rom".to_string()) && w.keywords.contains(&"Personen|Anna".to_string()));
    assert_eq!(sidecar::read_copies(w.copies_json.as_deref()).len(), 1);
    // foreign fields stay
    let heic_xmp = std::fs::read_to_string(u.join("IMG_3.xmp")).unwrap();
    assert!(heic_xmp.contains("crs:Exposure2012=\"+0.35\"") && heic_xmp.contains("xmp:Rating=\"2\""));
    // X3F: its own sidecar was read into the library and written back
    let x3f_id = count_rows(&db, "SELECT id FROM file WHERE format='X3F'");
    let x3f_dev = store::get(&db.conn, x3f_id, None).unwrap().settings;
    assert!(photos.join("2024/SDIM0001.xmp").exists());
    assert_eq!(xmp::read_develop(&photos.join("2024/SDIM0001.xmp")).unwrap().is_some(), x3f_dev.is_some());
    assert!(x3f_dev.is_some_and(|j| j.contains("0.3")));

    // change in the app → sidecar immediately
    let b1 = count_rows(&db, "SELECT id FROM image WHERE lr_id=1000");
    db.set_rating(&[b1], Some(2), None, Some(4)).unwrap();
    sync::write_sidecar(&db, count_rows(&db, "SELECT id FROM file WHERE lr_id=100")).unwrap();
    write_all(&db);
    let Xmp(w) = read_xmp(&photos.join("2024/IMG_1.xmp")) else { panic!() };
    assert_eq!((w.stars, w.label), (2, 4));
    // edit keywords (also several images), create a copy
    let b2 = count_rows(&db, "SELECT id FROM image WHERE lr_id=1002");
    sync::change_keywords(&db, &[b1, b2], &["Orte > Frankreich > Paris".into()], &["Anna".into()]).unwrap();
    let k = sync::create_copy(&db, b1, None).unwrap();
    write_all(&db);
    let Xmp(w) = read_xmp(&photos.join("2024/IMG_1.xmp")) else { panic!() };
    assert!(w.keywords.contains(&"Orte|Frankreich|Paris".to_string()) && !w.keywords.iter().any(|s| s.ends_with("Anna")));
    let copies = sidecar::read_copies(w.copies_json.as_deref());
    assert_eq!(copies.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), vec!["Kopie 1", "Kopie 2"]);
    assert!(k > 0);
    // the Lightroom import does not undo own keyword changes
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert!(sync::keyword_paths(&db, b1).unwrap().contains(&"Orte|Frankreich|Paris".to_string()));

    // XMP deleted → rewritten from the library; broken → backed up + rewritten
    std::fs::remove_file(photos.join("2024/IMG_2.xmp")).unwrap();
    std::fs::write(photos.join("2024/IMG_1.xmp"), "kaputt").unwrap();
    reconcile::reconcile(&db, None, &|_| {}).unwrap();
    write_all(&db);
    assert!(photos.join("2024/IMG_2.xmp").exists());
    assert!(matches!(read_xmp(&photos.join("2024/IMG_1.xmp")), Xmp(_)));
    assert!(std::fs::read_dir(photos.join("2024")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".IMG_1.xmp.defekt-")));
    // conflict: XMP changed outside (newer) → the library takes it over
    let mut w = match read_xmp(&photos.join("2024/IMG_2.xmp")) { Xmp(w) => w, _ => panic!() };
    w.stars = 4;
    w.rating_time = Some(library::now() + 100);
    xmp::write(&photos.join("2024/IMG_2.xmp"), &w).unwrap();
    // … but only on request ("Begleitdateien einlesen …"), not with every sync
    let before = count_rows(&db, "SELECT stars FROM image WHERE lr_id=1002");
    reconcile::reconcile(&db, None, &|_| {}).unwrap();
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1002"), before);
    let r = sidecar_import::run(&db, &|_, _| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert!(r.log.iter().any(|l| l.contains("IMG_2.JPG: Bewertung")), "{:?}", r.log);
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1002"), 4);
    // a copy deleted in the library (sidecar still lists it) does not come back; a NAS clock ahead does not win
    let d1 = count_rows(&db, "SELECT id FROM file WHERE lr_id=100");
    db.conn.execute("DELETE FROM image WHERE file=?1 AND copy='Kopie 2'", [d1]).unwrap();
    let mut w = match read_xmp(&photos.join("2024/IMG_2.xmp")) { Xmp(w) => w, _ => panic!() };
    w.stars = 1;
    w.rating_time = None;
    xmp::write(&photos.join("2024/IMG_2.xmp"), &w).unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(86_400);
    std::fs::File::options().write(true).open(photos.join("2024/IMG_2.xmp")).unwrap().set_modified(future).unwrap();
    db.conn.execute("UPDATE image SET stars=5, rating_time=?1 WHERE lr_id=1002", [library::now() + 10]).unwrap();
    sidecar_import::run(&db, &|_, _| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM image WHERE file={d1} AND copy='Kopie 2'")), 0, "deleted copy stays deleted");
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1002"), 5, "file time in the future capped");

    // database lost → rebuild from sidecars + Lightroom + roots.json
    let new = dir.join("rebuild.sqlite");
    let r = backup::rebuild(&new, &backup::roots_file(&db_path), Some(cat.clone()), &|_| {}).unwrap();
    let db2 = Db::open(&new).unwrap();
    assert_eq!(count_rows(&db2, "SELECT stars FROM image WHERE lr_id=1000"), 2, "{r}");
    assert_eq!(count_rows(&db2, "SELECT count(*) FROM image b JOIN file d ON d.id=b.file WHERE d.lr_id=100"), 3); // original + 2 copies (first reading)
    let b1n = count_rows(&db2, "SELECT id FROM image WHERE lr_id=1000");
    assert!(sync::keyword_paths(&db2, b1n).unwrap().contains(&"Orte|Frankreich|Paris".to_string()));
    drop(db2);

    // backup
    let (s, n) = backup::create_backup(&db).unwrap();
    assert!(s.exists() && n > 0);
    assert_eq!(backup::list(&backup::target(&db)).len(), 1);

    // move including sidecars (XMP, camera JPEG) + ⌘Z
    let target = photos.join("2024/Neu");
    let d100 = count_rows(&db, "SELECT id FROM file WHERE lr_id=100");
    let mut v = relocate::plan_files(&db, &[d100], &target, None).unwrap();
    relocate::execute_step(&db, &mut v).unwrap();
    assert!(target.join("IMG_1.CR3").exists() && target.join("IMG_1.xmp").exists() && target.join("IMG_1.jpg").exists() && !cr3.exists());
    assert_eq!(db.file_path(d100).unwrap(), target.join("IMG_1.CR3"));
    relocate::undo(&db, &mut v).unwrap();
    assert!(cr3.exists() && photos.join("2024/IMG_1.xmp").exists() && !target.join("IMG_1.CR3").exists());
    assert_eq!(db.file_path(d100).unwrap(), cr3);
    // rename via "other drive" (copy → verify → delete)
    std::env::set_var("GRAINEO_MOVE_COPY", "1");
    let d101 = count_rows(&db, "SELECT id FROM file WHERE lr_id=101");
    let mut v = relocate::plan_files(&db, &[d101], &photos.join("2024"), Some("Strand")).unwrap();
    relocate::execute_step(&db, &mut v).unwrap();
    std::env::remove_var("GRAINEO_MOVE_COPY");
    assert!(photos.join("2024/Strand.JPG").exists() && photos.join("2024/Strand.xmp").exists() && !photos.join("2024/IMG_2.JPG").exists());
    // never overwrite
    assert!(relocate::plan_files(&db, &[d101], &photos.join("2024"), Some("NEU_1")).is_err());
    // rename folder
    let o = count_rows(&db, "SELECT id FROM folder WHERE path='2024/Urlaub ä/'");
    let mut v = relocate::plan_folder(&db, o, "Urlaub Rom").unwrap();
    relocate::execute_step(&db, &mut v).unwrap();
    assert!(photos.join("2024/Urlaub Rom/IMG_3.HEIC").exists());
    assert_eq!(count_rows(&db, "SELECT count(*) FROM folder WHERE path='2024/Urlaub Rom/'"), 1);
    assert_eq!(relocate::journals().len(), 3); // ⌘Z marks the operation as undone

    // moved in the Finder → relinked (rating stays)
    std::fs::rename(photos.join("2024/Strand.JPG"), photos.join("Strand.JPG")).unwrap();
    let a = reconcile::reconcile(&db, None, &|_| {}).unwrap();
    assert_eq!(a.relinked, 1, "{:?} {:?}", a.linked_list, a.missing_list);
    assert_eq!(db.file_path(d101).unwrap(), photos.join("Strand.JPG"));
    assert_eq!(count_rows(&db, "SELECT stars FROM image WHERE lr_id=1002"), 5);

    // card import into date folders, without duplicates, RAW+JPEG together, backup copy
    let card = dir.join("Karte/DCIM/100CANON");
    std::fs::create_dir_all(&card).unwrap();
    std::fs::write(card.join("A_0001.CR3"), b"raw-a").unwrap();
    std::fs::write(card.join("A_0001.JPG"), b"jpg-a").unwrap();
    std::fs::write(card.join("A_0002.JPG"), b"jpg-b").unwrap();
    let k = card_import::scan(&db, &dir.join("Karte"));
    assert_eq!(k.len(), 3);
    let backup_dir = dir.join("Sicherung");
    let b = card_import::import(&k, &photos, Some(&backup_dir), &std::sync::atomic::AtomicBool::new(false), &|_| {});
    assert_eq!((b.copied, b.backed_up, b.duplicate), (3, 3, 0), "{:?}", b.error);
    let tag = &k[0].tag;
    let dst_dir = photos.join(&tag[..4]).join(tag);
    assert!(dst_dir.join("A_0001.CR3").exists() && dst_dir.join("A_0001.JPG").exists());
    assert!(backup_dir.join(&tag[..4]).join(tag).join("A_0002.JPG").exists());
    let a = card_import::register(&db, &photos, &b).unwrap();
    assert_eq!(a.newly_added, 3);
    let k2 = card_import::scan(&db, &dir.join("Karte"));
    assert!(k2.iter().all(|x| x.duplicate), "second import recognizes everything as present");
    std::env::remove_var("GRAINEO_LIBRARY_JOURNAL");
    std::fs::remove_dir_all(&dir).unwrap();
}

use library::xmp::ReadResult::Ok as Xmp;
fn read_xmp(p: &Path) -> library::xmp::ReadResult {
    library::xmp::read(p)
}

/// Newer library: opens if it only adds (`schema_kompatibel`), otherwise a clear message.
#[test]
fn newer_version() {
    let dir = tmp("version");
    let p = dir.join("l.sqlite");
    {
        let db = Db::open(&p).unwrap();
        db.conn.pragma_update(None, "user_version", 99).unwrap();
    }
    let db = Db::open(&p).unwrap();
    let v: i32 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, 99, "version stamp is never lowered");
    db.set_info("schema_compatible", "50").unwrap();
    drop(db);
    let e = Db::open(&p).err().unwrap().to_string();
    assert!(e.contains("neueren Version (Schema 99)") && !e.contains("Invalid parameter"), "{e}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn collection_file_is_never_read_and_only_written_when_enabled() {
    let dir = tmp("sammel-optional");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(photos.join(".graineo")).unwrap();
    std::fs::write(photos.join("a.jpg"), b"jpg").unwrap();
    // a file from another program or an older version: the library does not take it over
    std::fs::write(photos.join(".graineo/collections.json"), r#"{"smartCollections":[{"name":"Fremd","rule":"sterne:5"}]}"#).unwrap();
    let db = Db::open(&dir.join("t.sqlite")).unwrap();
    reconcile::add_root(&db, &photos, &|_| {}).unwrap();
    reconcile::reconcile(&db, None, &|_| {}).unwrap();
    let n: i64 = db.conn.query_row("SELECT count(*) FROM collection WHERE name='Fremd'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "collections.json is never read");
    // option off: nothing is written, the existing file stays untouched
    db.save_smart("Eigen", "sterne:>=1").unwrap();
    std::fs::remove_file(photos.join(".graineo/collections.json")).unwrap();
    assert_eq!(library::collection_file::write_if(&db, false).unwrap(), None);
    assert!(!photos.join(".graineo/collections.json").exists());
    // option on: written
    let file = library::collection_file::write_if(&db, true).unwrap().unwrap();
    assert_eq!(file, photos.join(".graineo/collections.json"));
    assert!(std::fs::read_to_string(&file).unwrap().contains("\"Eigen\""));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// "Nicht im Katalog": find folders, add, ignore (only in temp folders).
#[test]
fn not_in_catalog_find_add_ignore() {
    use library::not_in_catalog as nic;
    let dir = tmp("nik");
    let root_dir = dir.join("Fotos");
    for (o, d) in [("A", "1.jpg"), ("B/C", "2.jpg"), ("B/C", "3.CR3"), (".versteckt", "4.jpg")] {
        std::fs::create_dir_all(root_dir.join(o)).unwrap();
        std::fs::write(root_dir.join(o).join(d), b"x").unwrap();
    }
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    // root folder from Lightroom (new folders are not added there by themselves)
    db.conn.execute("INSERT INTO root_folder(path, name, origin) VALUES(?1, 'Fotos', 'lightroom')", [format!("{}/", root_dir.display())]).unwrap();
    let root = db.conn.last_insert_rowid();
    db.create_folder(root, "", None).unwrap();

    let all = nic::search(&db, &|_| {}).unwrap();
    assert_eq!(all.iter().map(|e| (e.rel.as_str(), e.count)).collect::<Vec<_>>(), vec![("A/", 1), ("B/C/", 2)], "{all:?}");
    assert_eq!(all[1].formats, vec![("CR3".to_string(), 1), ("JPG".to_string(), 1)]);

    // add: A/ is in the catalog afterwards, the file itself stays untouched
    let new = nic::add(&db, &[(root, "A/".to_string())], &|_| {}).unwrap();
    assert_eq!(new, 1);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM file WHERE name='1.jpg'"), 1);
    assert_eq!(std::fs::read(root_dir.join("A/1.jpg")).unwrap(), b"x");
    let rest = nic::search(&db, &|_| {}).unwrap();
    assert_eq!(rest.len(), 1);

    // ignore: disappears from the list, comes back with "reset"
    nic::ignore(&db, &[rest[0].cache_key()]).unwrap();
    assert!(nic::search(&db, &|_| {}).unwrap().is_empty());
    nic::clear_ignored(&db).unwrap();
    assert_eq!(nic::search(&db, &|_| {}).unwrap().len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// One import for everything: copy (with collection, keyword, XMP), move, in place.
#[test]
fn import_modes_copy_move_in_place() {
    use library::card_import::{self as ai, Mode, Options, CollectionTarget};
    use std::sync::atomic::AtomicBool;
    let dir = tmp("modi");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    let db_path = dir.join("library.sqlite");
    let db = Db::open(&db_path).unwrap();
    reconcile::add_root(&db, &photos, &|_| {}).unwrap();
    let source = dir.join("Quelle");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("A_1.JPG"), b"jpg-eins").unwrap();
    std::fs::write(source.join("A_1.xmp"), b"<xmp/>").unwrap();
    std::fs::write(source.join("A_2.JPG"), b"jpg-zwei!").unwrap();
    let no_trash = |_: &[PathBuf]| -> Result<(), String> { panic!("copying must not dispose of anything") };
    let never = AtomicBool::new(false);
    let open = || Db::open(&db_path).map_err(|e| e.to_string());

    // copy into date folders, with a new collection and keyword
    let k = ai::scan(&db, &source);
    assert_eq!(k.len(), 2);
    let opt = Options { mode: Mode::CopyFiles, source: Some(source.clone()), target: Some(photos.clone()), by_date: true, backup: None, collection: CollectionTarget::New("Import 1".into()), parent_name: Some("Importe".into()), keywords: vec!["Test|Eins".into()], copyright: "© 2026 Test".into(), ..Options::default() };
    let e = ai::run_import(&open, &k, &opt, &no_trash, &never, &|_| {});
    assert_eq!((e.copy_file.copied, e.added, e.keyword_images), (2, 2, 2), "{e:?}");
    assert_eq!(e.collection, Some(("Import 1".to_string(), 2)));
    assert!(e.catalog_error.is_none() && e.notes.is_empty(), "{e:?}");
    let tag = &k[0].tag;
    assert!(photos.join(&tag[..4]).join(tag).join("A_1.xmp").exists(), "XMP comes along");
    assert!(source.join("A_1.JPG").exists(), "source stays");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection_image WHERE collection=(SELECT id FROM collection WHERE name='Import 1')"), 2);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image_keyword"), 2);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image WHERE copyright = '© 2026 Test'"), 2, "copyright given at import");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection c JOIN collection e ON e.id=c.parent WHERE c.name='Import 1' AND e.name='Importe' AND e.kind=1 AND e.parent IS NULL"), 1, "new collection is in the set \"Importe\"");

    // move: originals are disposed of only after the verified copy (here: deleted instead of Trash)
    std::fs::write(source.join("B_1.JPG"), b"jpg-drei").unwrap();
    std::fs::write(source.join("B_1.xmp"), b"<xmp/>").unwrap();
    let k = ai::scan(&db, &source);
    let new: Vec<_> = k.iter().filter(|x| !x.duplicate).cloned().collect();
    assert_eq!(new.len(), 1, "A_1 and A_2 are already in the catalog");
    let disposed = std::sync::Mutex::new(Vec::<PathBuf>::new());
    let remove_fn = |d: &[PathBuf]| -> Result<(), String> {
        for p in d {
            std::fs::remove_file(p).map_err(|e| e.to_string())?;
            disposed.lock().unwrap().push(p.clone());
        }
        Ok(())
    };
    let opt = Options { mode: Mode::MoveFiles, collection: CollectionTarget::Nothing, keywords: vec![], copyright: String::new(), ..opt };
    let e = ai::run_import(&open, &new, &opt, &remove_fn, &never, &|_| {});
    assert_eq!((e.copy_file.copied, e.copy_file.moved, e.added), (1, 1, 1), "{e:?}");
    assert!(!source.join("B_1.JPG").exists() && !source.join("B_1.xmp").exists());
    assert_eq!(disposed.lock().unwrap().len(), 2, "photo and XMP");

    // in place: a folder outside all root folders becomes a root folder, nothing is copied
    let external_dir = dir.join("Extern/Reise");
    std::fs::create_dir_all(&external_dir).unwrap();
    std::fs::write(external_dir.join("R_1.JPG"), b"jpg-reise").unwrap();
    let k = ai::scan(&db, &external_dir);
    let opt = Options { mode: Mode::InPlace, source: Some(external_dir.clone()), target: None, collection: CollectionTarget::Existing(count_rows(&db, "SELECT id FROM collection WHERE name='Import 1'")), ..opt };
    let e = ai::run_import(&open, &k, &opt, &no_trash, &never, &|_| {});
    assert_eq!((e.copy_file.copied, e.added), (0, 1), "{e:?}");
    assert_eq!(e.collection, Some(("Import 1".to_string(), 1)));
    assert_eq!(std::fs::read(external_dir.join("R_1.JPG")).unwrap(), b"jpg-reise");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM file WHERE name='R_1.JPG'"), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The folder sync holds no write lock while reading the disk: at every progress step
/// (run over the disk, sidecars) another part of the app can write
/// immediately. Whatever another operation has entered in the meantime (e.g. an import) is not created
/// twice and does not make the sync fail.
#[test]
fn reconcile_does_not_lock_library_while_reading() {
    let dir = tmp("sperre");
    let f = dir.join("FOTOS");
    std::fs::create_dir_all(f.join("A")).unwrap();
    std::fs::write(f.join("A/1.JPG"), b"x").unwrap();
    std::fs::write(f.join("A/2.JPG"), b"x").unwrap();
    let p = dir.join("b.sqlite");
    let db = Db::open(&p).unwrap();
    reconcile::add_root(&db, &f, &|_| {}).unwrap();
    // something to do: a new file, a new folder, a vanished file
    std::fs::write(f.join("A/3.JPG"), b"x").unwrap();
    std::fs::write(f.join("A/4.JPG"), b"x").unwrap();
    std::fs::create_dir_all(f.join("B")).unwrap();
    std::fs::write(f.join("B/5.JPG"), b"x").unwrap();
    std::fs::remove_file(f.join("A/2.JPG")).unwrap();
    // the "UI": never waits
    let other = Connection::open(&p).unwrap();
    other.busy_timeout(std::time::Duration::ZERO).unwrap();
    let folder_a: i64 = other.query_row("SELECT o.id FROM folder o WHERE o.path='A/'", [], |r| r.get(0)).unwrap();
    let error = std::cell::RefCell::new(Vec::<String>::new());
    let steps = std::cell::Cell::new(0);
    let b = reconcile::reconcile(&db, None, &|step| {
        steps.set(steps.get() + 1);
        if let Err(e) = other.execute_batch("BEGIN IMMEDIATE; UPDATE info SET value=value WHERE name='schema'; COMMIT;") {
            error.borrow_mut().push(format!("{step}: {e}"));
        }
        // an import registers 4.JPG while the sync reads the disk
        if step.starts_with("Abgleich") {
            other.execute("INSERT INTO file(folder, name, format, status) VALUES(?1, '4.JPG', 'JPG', 0)", params![folder_a]).unwrap();
            let d = other.last_insert_rowid();
            other.execute("INSERT INTO image(file) VALUES(?1)", [d]).unwrap();
        }
    })
    .unwrap();
    assert!(steps.get() >= 4, "too few steps: {}", steps.get());
    assert!(error.borrow().is_empty(), "library locked: {:?}", error.borrow());
    // 3.JPG and B/5.JPG new; the import already had 4.JPG (not duplicated)
    assert_eq!(b.newly_added, 2);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM file WHERE name='4.JPG'"), 1);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM file WHERE name='5.JPG' AND status=0"), 1);
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE name='2.JPG'"), 1);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image b JOIN file d ON d.id=b.file WHERE d.name IN ('3.JPG','4.JPG','5.JPG')"), 3);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Import into a main folder outside all root folders (e.g. on the NAS): the target becomes a root folder; if it already
/// contains a root folder (chosen a level too high), the year folder of the import becomes the root folder instead. The
/// sources are reported next to the copies (previews right after the import come from the card).
#[test]
fn import_into_a_new_main_folder() {
    use library::card_import::{self as ai, CollectionTarget, Mode, Options};
    use std::sync::atomic::AtomicBool;
    let dir = tmp("hauptordner");
    let db_path = dir.join("library.sqlite");
    let db = Db::open(&db_path).unwrap();
    let card = dir.join("Karte/DCIM/100CANON");
    std::fs::create_dir_all(&card).unwrap();
    std::fs::write(card.join("IMG_1.JPG"), b"jpg-eins").unwrap();
    std::fs::write(card.join("IMG_1.xmp"), b"<xmp/>").unwrap();
    let never = AtomicBool::new(false);
    let no_trash = |_: &[PathBuf]| -> Result<(), String> { panic!("copying must not dispose of anything") };
    let open = || Db::open(&db_path).map_err(|e| e.to_string());

    // 1. target outside all root folders → it becomes a root folder
    let nas = dir.join("nas/Fotos");
    let k = ai::scan(&db, &dir.join("Karte"));
    let opt = Options { mode: Mode::CopyFiles, source: Some(dir.join("Karte")), target: Some(nas.clone()), by_date: true, backup: None, collection: CollectionTarget::Nothing, parent_name: None, keywords: vec![] , ..Options::default() };
    let e = ai::run_import(&open, &k, &opt, &no_trash, &never, &|_| {});
    assert_eq!((e.copy_file.copied, e.added), (1, 1), "{e:?}");
    assert!(e.catalog_error.is_none(), "{e:?}");
    assert_eq!(e.copy_file.source_files, vec![card.join("IMG_1.JPG")]);
    assert_eq!(e.copy_file.target_files.len(), 1);
    let tag = &k[0].tag;
    assert_eq!(e.copy_file.target_files[0], nas.join(&tag[..4]).join(tag).join("IMG_1.JPG"));
    assert!(nas.join(&tag[..4]).join(tag).join("IMG_1.xmp").exists(), "XMP next to the copy");
    let root = format!("{}/", nas.display());
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM root_folder WHERE path='{root}'")), 1);

    // 2. a second import into the same main folder: no new root folder, the photo belongs to it
    std::fs::write(card.join("IMG_2.JPG"), b"jpg-zwei!").unwrap();
    let k: Vec<_> = ai::scan(&db, &dir.join("Karte")).into_iter().filter(|x| !x.duplicate).collect();
    assert_eq!(k.len(), 1);
    let e = ai::run_import(&open, &k, &opt, &no_trash, &never, &|_| {});
    assert_eq!((e.copy_file.copied, e.added), (1, 1), "{e:?}");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM root_folder"), 1);

    // 3. target above a root folder: the year folder of the import becomes the root folder
    let above = dir.join("platte");
    let old = above.join("Alt");
    std::fs::create_dir_all(&old).unwrap();
    reconcile::add_root(&db, &old, &|_| {}).unwrap();
    std::fs::write(card.join("IMG_3.JPG"), b"jpg-drei!!").unwrap();
    let k: Vec<_> = ai::scan(&db, &dir.join("Karte")).into_iter().filter(|x| !x.duplicate).collect();
    let opt = Options { target: Some(above.clone()), ..opt };
    let e = ai::run_import(&open, &k, &opt, &no_trash, &never, &|_| {});
    assert!(e.catalog_error.is_none(), "{e:?}");
    assert_eq!((e.copy_file.copied, e.added), (1, 1), "{e:?}");
    let year = format!("{}/{}/", above.display(), &k[0].tag[..4]);
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM root_folder WHERE path='{year}'")), 1, "year folder becomes the root folder");
    assert_eq!(count_rows(&db, &format!("SELECT count(*) FROM root_folder WHERE path='{}/'", above.display())), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Schema 16 removes the earlier offloading from a library of schema 15 (on the way to the current schema): a RAW adopted
/// from the NAS (row in `adopted_offloaded`) keeps its folder (the row moves to `moved_files`), a JPEG that stood in for
/// a RAW (without an image of its own) becomes an ordinary photo, an offloaded file counts as missing; a backup is made
/// first.
#[test]
fn migration_16_removes_offloading_and_keeps_adopted_raw_in_its_folder() {
    let dir = tmp("adopted");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("library.sqlite");
    legacy_names::sql::create_schema_15_offloading(&rusqlite::Connection::open(&path).unwrap()).unwrap();
    let db = Db::open(&path).unwrap();
    assert!(dir.join(format!("backups/library-before-schema-{}.sqlite", library::db::VERSION)).is_file(), "backup before the migration");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM sqlite_master WHERE name = 'adopted_offloaded'"), 0);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM moved_files WHERE file = 1 AND old_path = '/old/A.RAF'"), 1);
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image"), 3, "the proxy JPEG is an ordinary photo now");
    assert!(db.rows().unwrap().iter().any(|z| z.file == 2 && z.copy.is_none()));
    assert_eq!(count_rows(&db, "SELECT status FROM file WHERE id = 3"), 1, "offloaded counts as missing");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// "Prüfen" in the Lightroom dialog: what an import would bring, read without writing (library and catalog unchanged).
#[test]
fn preview_counts_without_writing() {
    let dir = tmp("preview");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let cat_before = std::fs::read(&cat).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let images_before = count_rows(&db, "SELECT count(*) FROM image");

    let p = lightroom::preview(&db, &cat).unwrap();
    assert_eq!((p.images, p.new_images, p.virtual_copies), (9, 9, 2));
    // the quick collection is not counted as a collection (it becomes graineo's quick collection)
    assert_eq!((p.collections, p.collection_sets, p.smart_collections, p.keywords), (1, 1, 1, 5));
    assert_eq!(p.roots, 2);
    assert_eq!(p.roots_unreachable, vec!["/Volumes/GibtEsNicht-x3f/Bilder/".to_string()]);
    // nothing written: neither the library nor the catalog (no -wal/-shm)
    assert_eq!(count_rows(&db, "SELECT count(*) FROM image"), images_before);
    assert_eq!(std::fs::read(&cat).unwrap(), cat_before);
    assert!(!dir.join("Test.lrcat-wal").exists() && !dir.join("Test.lrcat-shm").exists());

    // after the import nothing is new any more
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    let p2 = lightroom::preview(&db, &cat).unwrap();
    assert_eq!((p2.images, p2.new_images), (9, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Unreadable file: the preview says so (the dialog shows it) instead of failing silently.
#[test]
fn preview_of_a_file_that_is_no_catalog() {
    let dir = tmp("preview-broken");
    let cat = dir.join("Kaputt.lrcat");
    std::fs::write(&cat, b"this is not a database at all, just some text that is long enough").unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let e = lightroom::preview(&db, &cat).unwrap_err();
    assert!(e.starts_with("Katalog nicht lesbar"), "{e}");
    let missing = lightroom::preview(&db, &dir.join("Fehlt.lrcat")).unwrap_err();
    assert!(missing.starts_with("Katalog nicht lesbar"), "{missing}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Collections at the top level of the catalog go into the chosen set; the quick collection stays at the top level,
/// a set in the catalog keeps its own collections; a second import neither moves nor counts them as "own changes".
#[test]
fn import_into_a_collection_set() {
    let dir = tmp("import-set");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let opts = lightroom::Options { collection_set: Some("Test".into()) };
    lightroom::import_with(&db, &cat, &opts, &|_| {}).unwrap();
    let set: i64 = db.conn.query_row("SELECT id FROM collection WHERE name='Test' AND kind=1 AND parent IS NULL", [], |r| r.get(0)).unwrap();
    let parent = |name: &str| -> Option<i64> { db.conn.query_row("SELECT parent FROM collection WHERE name=?1", [name], |r| r.get(0)).unwrap() };
    assert_eq!(parent("Reisen"), Some(set));
    assert_eq!(parent("Fünf Sterne"), Some(set));
    assert_ne!(parent("Rom 2024"), Some(set), "inside its own set");
    assert_eq!(parent("Schnellsammlung"), None, "the quick collection stays at the top level");
    let r = lightroom::import_with(&db, &cat, &opts, &|_| {}).unwrap();
    assert_eq!(r.own_kept, 0, "{r:?}");
    assert_eq!(parent("Reisen"), Some(set));
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection WHERE name='Test'"), 1);
    // an own order in graineo: a new import never writes positions and keeps them
    let id = |name: &str| -> i64 { db.conn.query_row("SELECT id FROM collection WHERE name=?1", [name], |r| r.get(0)).unwrap() };
    let first = library::collection_order::Tree::build(&db.collections().unwrap());
    let shown = first.children(Some(set)).len();
    assert!(shown >= 2, "{shown}");
    library::collection_order::place(&db, id("Fünf Sterne"), Some(set), Some(id("Reisen"))).unwrap();
    let order = || -> Vec<(i64, Option<i64>, i64)> {
        db.conn.prepare("SELECT collection, parent, position FROM collection_order ORDER BY 1").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    let ordered = order();
    lightroom::import_with(&db, &cat, &opts, &|_| {}).unwrap();
    assert_eq!(order(), ordered, "the import leaves the own order alone");
    // the set moved into another set and renamed in graineo: the next import uses it, no second set
    let other = db.create_collection_in("Archiv", None).unwrap();
    db.conn.execute("UPDATE collection SET kind=1 WHERE id=?1", [other]).unwrap();
    library::collection_order::place(&db, set, Some(other), None).unwrap();
    library::manage::rename_collection(&db, set, "Lightroom").unwrap();
    let r = lightroom::import_with(&db, &cat, &opts, &|_| {}).unwrap();
    assert_eq!(r.own_kept, 0, "{r:?}");
    assert_eq!(count_rows(&db, "SELECT count(*) FROM collection WHERE name='Test'"), 0, "no second set");
    assert_eq!(parent("Reisen"), Some(set));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Own moves in graineo and opposite moves in Lightroom (A under B here, B under A there) must not give a loop: the
/// parent change from Lightroom that would close it is left out and counted.
#[test]
fn reimport_never_creates_a_parent_cycle() {
    let dir = tmp("zyklus");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    disk(&photos);
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("INSERT INTO AgLibraryCollection(id_local,name,parent,creationId,genealogy,systemOnly) VALUES(56,'Archiv',NULL,'com.adobe.ag.library.group','/56','0.0')", []).unwrap();
    }
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    let id = |sql: &str| count_rows(&db, sql);
    let (people, places) = (id("SELECT id FROM keyword WHERE lr_id=2"), id("SELECT id FROM keyword WHERE lr_id=4"));
    let (travel, archive) = (id("SELECT id FROM collection WHERE lr_id=50"), id("SELECT id FROM collection WHERE lr_id=56"));
    // in graineo: "Personen" under "Orte", set "Reisen" into set "Archiv"
    db.conn.execute("UPDATE keyword SET parent=?1 WHERE id=?2", [places, people]).unwrap();
    db.conn.execute("UPDATE collection SET parent=?1 WHERE id=?2", [archive, travel]).unwrap();
    // in Lightroom the other way round
    {
        let c = Connection::open(&cat).unwrap();
        c.execute("UPDATE AgLibraryKeyword SET parent=2, genealogy='/2/4' WHERE id_local=4", []).unwrap();
        c.execute("UPDATE AgLibraryCollection SET parent=50, genealogy='/50/56' WHERE id_local=56", []).unwrap();
    }
    let r = lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(r.cycles_avoided, 2, "{}", r.json());
    // the own places stay, no loop
    assert_eq!(id(&format!("SELECT parent FROM keyword WHERE id={people}")), places);
    assert_ne!(id(&format!("SELECT coalesce(parent, 0) FROM keyword WHERE id={places}")), people);
    assert_eq!(id(&format!("SELECT parent FROM collection WHERE id={travel}")), archive);
    assert_ne!(id(&format!("SELECT coalesce(parent, 0) FROM collection WHERE id={archive}")), travel);
    // searches over the hierarchy still answer
    assert!(db.images_with_keyword(places).is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}
