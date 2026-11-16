//! Data safety of the Lightroom import (review before 1.1.0):
//! - a second, different catalog must not be imported over the first one: Lightroom numbers every catalog from the
//!   start, so its ids collide with those of the first catalog and the import would relink root folders, files and
//!   ratings of the first catalog to the photos of the second one;
//! - a table or column the catalog lacks (another Lightroom version, a trimmed catalog) is "not known", not "removed
//!   in Lightroom": a new import must not wipe the ratings, captions, titles, keyword and collection assignments that
//!   came from Lightroom before.

use std::path::{Path, PathBuf};

use library::{db::Db, lightroom, lr_handover};
use rusqlite::{params, Connection};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("graineo-lr-safety-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A catalog with the full table skeleton: one root folder `photos`, files `names` (ids 100…), one image per file
/// (ids 1000…, stars `stars`), a keyword "Katze" on every image, a collection "Reise" with every image, a caption.
fn catalog(path: &Path, photos: &Path, names: &[&str], stars: i64, guid: &str) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(include_str!("catalog-schema.sql")).unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(1,?2,?1,'Fotos')", params![format!("{}/", photos.display()), format!("{guid}-r1")]).unwrap();
    c.execute("INSERT INTO AgLibraryFolder(id_local,id_global,rootFolder,pathFromRoot) VALUES(10,?1,1,'')", [format!("{guid}-f10")]).unwrap();
    c.execute("INSERT INTO AgLibraryKeyword(id_local,id_global,name,parent,genealogy) VALUES(1,?1,NULL,NULL,'/1'), (2,?2,'Katze',1,'/12')", params![format!("{guid}-k1"), format!("{guid}-k2")]).unwrap();
    c.execute("INSERT INTO AgLibraryCollection(id_local,name,parent,creationId,genealogy) VALUES(5,'Reise',NULL,'com.adobe.ag.library.collection','/15')", []).unwrap();
    for (n, name) in names.iter().enumerate() {
        let (file, image) = (100 + n as i64, 1000 + n as i64);
        let (base, ext) = name.rsplit_once('.').unwrap();
        c.execute("INSERT INTO AgLibraryFile(id_local,id_global,folder,baseName,extension,idx_filename) VALUES(?1,?2,10,?3,?4,?5)", params![file, format!("{guid}-d{file}"), base, ext, name]).unwrap();
        c.execute("INSERT INTO Adobe_images(id_local,id_global,rootFile,rating,captureTime) VALUES(?1,?2,?3,?4,'2025-03-01T10:00:00')", params![image, format!("{guid}-i{image}"), file, stars]).unwrap();
        c.execute("INSERT INTO AgLibraryKeywordImage(image, tag) VALUES(?1, 2)", [image]).unwrap();
        c.execute("INSERT INTO AgLibraryCollectionImage(collection, image, positionInCollection) VALUES(5, ?1, ?2)", params![image, n as f64]).unwrap();
        c.execute("INSERT INTO AgLibraryIPTC(image, caption) VALUES(?1, 'Am See')", [image]).unwrap();
    }
}

/// Snapshot of what the library holds for the photos: (path, stars, caption, keywords, in collection).
fn snapshot(db: &Db) -> Vec<(String, i64, Option<String>, i64, i64)> {
    let mut st = db
        .conn
        .prepare(
            "SELECT s.path || o.path || d.name, i.stars, i.caption,
                    (SELECT count(*) FROM image_keyword k WHERE k.image = i.id),
                    (SELECT count(*) FROM collection_image c WHERE c.image = i.id)
             FROM image i JOIN file d ON d.id = i.file JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root ORDER BY 1",
        )
        .unwrap();
    let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    v
}

/// Two different catalogs: the second one is refused (nothing written), the photos of the first stay as they were.
#[test]
fn second_catalog_does_not_mix_up_the_first() {
    let dir = tmp("two");
    let (pa, pb) = (dir.join("A"), dir.join("B"));
    std::fs::create_dir_all(&pa).unwrap();
    std::fs::create_dir_all(&pb).unwrap();
    let (ka, kb) = (dir.join("A.lrcat"), dir.join("B.lrcat"));
    catalog(&ka, &pa, &["IMG_0001.JPG", "IMG_0002.JPG", "IMG_0003.JPG"], 5, "a");
    catalog(&kb, &pb, &["DSC_7001.JPG", "DSC_7002.JPG", "DSC_7003.JPG"], 1, "b");
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    lightroom::import(&db, &ka, &|_| {}).unwrap();
    let before = snapshot(&db);
    assert_eq!(before.len(), 3);
    assert!(before.iter().all(|z| z.0.starts_with(&format!("{}/", pa.display())) && z.1 == 5), "{before:?}");

    // the check (reads only) and the import both refuse the other catalog
    let e = lightroom::preview(&db, &kb).unwrap_err();
    assert!(e.contains(lightroom::OTHER_CATALOG), "{e}");
    let e = lightroom::import(&db, &kb, &|_| {}).unwrap_err();
    assert!(e.contains(lightroom::OTHER_CATALOG), "{e}");
    assert_eq!(snapshot(&db), before, "the other catalog changed the library");
    let e = lr_handover::run(&db, &kb, true, &|_, _| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap_err();
    assert!(e.contains(lightroom::OTHER_CATALOG), "{e}");

    // the same catalog again (also under another name, as after an upgrade, and with a photo renamed in Lightroom)
    // goes on as before
    let ka2 = dir.join("A-v14.lrcat");
    catalog(&ka2, &pa, &["IMG_0001.JPG", "IMG_0002-bearbeitet.JPG", "IMG_0003.JPG"], 4, "a");
    lightroom::preview(&db, &ka2).unwrap();
    lightroom::import(&db, &ka2, &|_| {}).unwrap();
    assert!(snapshot(&db).iter().all(|z| z.1 == 4));
    lr_handover::run(&db, &ka2, false, &|_, _| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A catalog that lacks the keyword, collection, caption and rating parts (a newer or trimmed format): the import
/// still runs, but what came from Lightroom before stays.
#[test]
fn missing_parts_do_not_wipe_earlier_values() {
    let dir = tmp("gaps");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(&photos).unwrap();
    let cat = dir.join("K.lrcat");
    catalog(&cat, &photos, &["IMG_0001.JPG", "IMG_0002.JPG"], 3, "k");
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    let before = snapshot(&db);
    assert!(before.iter().all(|z| z.1 == 3 && z.2.as_deref() == Some("Am See") && z.3 == 1 && z.4 == 1), "{before:?}");
    let keywords_before = count(&db, "SELECT count(*) FROM keyword");

    // the same catalog without those parts
    let c = Connection::open(&cat).unwrap();
    c.execute_batch(
        "DROP TABLE AgLibraryKeywordImage; DROP TABLE AgLibraryCollectionImage; DROP TABLE AgLibraryIPTC;
         ALTER TABLE Adobe_images DROP COLUMN rating;",
    )
    .unwrap();
    drop(c);
    let r = lightroom::import(&db, &cat, &|_| {}).unwrap();
    for gap in ["AgLibraryKeywordImage", "AgLibraryCollectionImage", "AgLibraryIPTC", "Adobe_images.rating"] {
        assert!(r.catalog_gaps.iter().any(|g| g == gap), "{gap} not in {:?}", r.catalog_gaps);
    }
    assert_eq!(snapshot(&db), before, "a missing part of the catalog removed values");
    assert_eq!(count(&db, "SELECT count(*) FROM keyword"), keywords_before);

    // without the keyword table itself: the keywords and their assignments stay as well
    let c = Connection::open(&cat).unwrap();
    c.execute_batch("DROP TABLE AgLibraryKeyword; DROP TABLE AgLibraryCollection;").unwrap();
    drop(c);
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    assert_eq!(snapshot(&db), before, "a missing keyword or collection table removed values");
    assert_eq!(count(&db, "SELECT count(*) FROM keyword WHERE lr_id IS NOT NULL"), keywords_before);
    let _ = std::fs::remove_dir_all(&dir);
}
