//! Handover of the Lightroom develop data into the sidecars (`lr_handover`) against a mini catalog
//! with the real schema (`catalog-schema.sql` plus the develop settings table).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use library::{db::Db, lightroom, lr_handover, sidecar_import, store, xmp};
use rusqlite::{params, Connection};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("graineo-lr-handover-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const MASTER: &str = "s = { CropAngle = 1.5,\nCropBottom = 0.9,\nCropLeft = 0.1,\nCropRight = 0.95,\nCropTop = 0.05,\nExposure2012 = 0.35,\nHasCrop = true,\nLook = { Name = \"A & <B>\" } }";
const COPY: &str = "s = { ConvertToGrayscale = true,\nExposure2012 = -0.5 }";
// as in real catalogs: the text ends with a line break (the sidecar keeps it trimmed)
const JPEG: &str = "s = { Contrast2012 = 20 }\n";

fn catalog(path: &Path, photos: &Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(include_str!("catalog-schema.sql")).unwrap();
    c.execute_batch("CREATE TABLE Adobe_imageDevelopSettings (id_local INTEGER PRIMARY KEY, image INTEGER, text, hasDevelopAdjustmentsEx);").unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(1,'r1',?1,'Fotos')", [format!("{}/", photos.display())]).unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(2,'r2','/Volumes/GibtEsNicht-graineo/Bilder/','Bilder')", []).unwrap();
    for (id, root, p) in [(11, 1, "2024/"), (13, 2, "")] {
        c.execute("INSERT INTO AgLibraryFolder(id_local,id_global,rootFolder,pathFromRoot) VALUES(?1,?2,?3,?4)", params![id, format!("f{id}"), root, p]).unwrap();
    }
    for (id, folder, base, ext) in [(100, 11, "IMG_1", "CR3"), (101, 11, "IMG_2", "JPG"), (102, 11, "IMG_3", "CR3"), (103, 11, "WEG", "NEF"), (104, 13, "IMG_9", "CR2"), (105, 11, "IMG_4", "JPG"), (106, 11, "IMG_4", "RAF")] {
        c.execute(
            "INSERT INTO AgLibraryFile(id_local,id_global,folder,baseName,extension,idx_filename,originalFilename,sidecarExtensions) VALUES(?1,?2,?3,?4,?5,?6,?6,'')",
            params![id, format!("d{id}"), folder, base, ext, format!("{base}.{ext}")],
        )
        .unwrap();
    }
    // images: id, file, master, copy name, orientation
    for (id, file, master, copy, o) in [(1000, 100, None, None, "BC"), (1001, 100, Some(1000), Some("Kopie 1"), "AB"), (1002, 101, None, None, "AB"), (1003, 102, None, None, "AB"), (1004, 103, None, None, "AB"), (1005, 104, None, None, "AB"), (1006, 105, None, None, "AB"), (1007, 106, None, None, "AB")] {
        c.execute(
            "INSERT INTO Adobe_images(id_local,id_global,rootFile,rating,pick,colorLabels,masterImage,copyName,captureTime,orientation,fileFormat,fileWidth,fileHeight) VALUES(?1,?2,?3,0,0,'',?4,?5,'2024-05-11T14:03:22',?6,'RAW',6000,4000)",
            params![id, format!("i{id}"), file, master, copy, o],
        )
        .unwrap();
    }
    // settings: 1003 only an empty row (photo without edits); 1000 with an extra empty row next to the real one
    for (id, image, text, edited) in [(1, 1000, ""), (2, 1000, MASTER), (3, 1001, COPY), (4, 1002, JPEG), (5, 1003, ""), (6, 1004, MASTER), (7, 1005, MASTER), (8, 1006, JPEG), (9, 1007, MASTER)].map(|(a, b, c)| (a, b, c, (!c.is_empty()) as i64)) {
        c.execute("INSERT INTO Adobe_imageDevelopSettings(id_local,image,text,hasDevelopAdjustmentsEx) VALUES(?1,?2,?3,?4)", params![id, image, text, edited]).unwrap();
    }
}

#[test]
fn handover_writes_sidecars_once_and_keeps_own_settings() {
    let dir = tmp("run");
    let photos = dir.join("Fotos");
    let d = photos.join("2024");
    std::fs::create_dir_all(&d).unwrap();
    for n in ["IMG_1.CR3", "IMG_2.JPG", "IMG_3.CR3", "IMG_4.JPG"] {
        std::fs::write(d.join(n), b"x").unwrap();
    }
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let cat_before = std::fs::read(&cat).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    // a photo removed from the library: its edits are "not found"
    db.conn.execute_batch("DELETE FROM image WHERE file IN (SELECT id FROM file WHERE lr_id=103); DELETE FROM file WHERE lr_id=103;").unwrap();
    // the photos moved (as to the NAS, same relative paths); one without lr_id is matched by its path
    let moved = dir.join("NAS/Fotos");
    std::fs::create_dir_all(moved.join("2024")).unwrap();
    for n in ["IMG_1.CR3", "IMG_2.JPG", "IMG_3.CR3", "IMG_4.JPG"] {
        std::fs::rename(d.join(n), moved.join("2024").join(n)).unwrap();
    }
    db.conn.execute("UPDATE root_folder SET path=?1 WHERE path=?2", params![format!("{}/", moved.display()), format!("{}/", photos.display())]).unwrap();
    db.conn.execute("UPDATE file SET lr_id=NULL WHERE lr_id=101", []).unwrap();
    // an existing sidecar with own settings and a rating
    let x1 = moved.join("2024/IMG_1.xmp");
    xmp::write(&x1, &xmp::Values { stars: 4, rating_time: Some(5), ..Default::default() }).unwrap();
    xmp::write_develop(&x1, Some("{\"exposure\":1.0}")).unwrap();
    let mtime = std::fs::metadata(&x1).unwrap().modified().unwrap();

    let no = AtomicBool::new(false);
    let file = |lr: i64| -> i64 { db.conn.query_row("SELECT id FROM file WHERE lr_id=?1", [lr], |r| r.get(0)).unwrap() };
    let f1 = file(100);
    let f2: i64 = db.conn.query_row("SELECT id FROM file WHERE name='IMG_2.JPG'", [], |r| r.get(0)).unwrap();
    // check run: counts only, writes nothing
    let r = lr_handover::run(&db, &cat, false, &|_, _| {}, &no).unwrap();
    assert_eq!((r.with_data, r.written, r.copies, r.already, r.unreachable, r.not_found, r.errors, r.conflicts), (5, 5, 1, 0, 0, 1, 0, 0), "{:?}", r.log);
    assert!(store::get(&db.conn, f1, None).unwrap().lightroom.is_empty() && !moved.join("2024/IMG_2.xmp").exists());

    // existing sidecars are read once (the background reading after the update)
    sidecar_import::import_unread(&db, 1000, &|| {}).unwrap();
    let r = lr_handover::run(&db, &cat, true, &|_, _| {}, &no).unwrap();
    // into the library – also the photo on the drive that is not mounted (IMG_9) and both IMG_4 (one entry each)
    assert_eq!((r.written, r.copies, r.unreachable, r.not_found, r.errors, r.conflicts), (5, 1, 0, 1, 0, 0), "{:?}", r.log);
    let rec = store::get(&db.conn, f1, None).unwrap();
    assert_eq!((rec.lightroom.develop.as_deref(), rec.lightroom.orientation.as_deref(), rec.lightroom.edited, rec.lightroom.file.as_deref()), (Some(MASTER), Some("BC"), true, Some("IMG_1.CR3")));
    assert_eq!(rec.settings.as_deref(), Some("{\"exposure\":1.0}"), "own settings stay");
    let stars: i64 = db.conn.query_row("SELECT stars FROM image WHERE file=?1 AND copy IS NULL", [f1], |r| r.get(0)).unwrap();
    assert_eq!(stars, 4);
    let k = store::get(&db.conn, f1, Some("Kopie 1")).unwrap().lightroom;
    assert_eq!((k.develop.as_deref(), k.orientation.as_deref()), (Some(COPY), Some("AB")));
    let j = store::get(&db.conn, f2, None).unwrap().lightroom;
    assert_eq!((j.develop.as_deref(), j.edited), (Some(JPEG.trim()), true));
    assert!(store::get(&db.conn, file(102), None).unwrap().lightroom.is_empty(), "no edits, nothing");
    assert_eq!(store::get(&db.conn, file(106), None).unwrap().lightroom.develop.as_deref(), Some(MASTER));
    assert_eq!(store::get(&db.conn, file(105), None).unwrap().lightroom.develop.as_deref(), Some(JPEG.trim()));
    assert_eq!(store::get(&db.conn, file(104), None).unwrap().lightroom.develop.as_deref(), Some(MASTER));
    // nothing written next to the photos (writing sidecars is off)
    assert_eq!(std::fs::metadata(&x1).unwrap().modified().unwrap(), mtime);
    assert!(!moved.join("2024/IMG_2.xmp").exists() && !moved.join("2024/IMG_4.xmp").exists());

    // with writing sidecars on: the same sidecar as the handover wrote before 08.11.2026
    for f in [f1, f2, file(105), file(106)] {
        store::write_sidecar_from_library(&db, f, true).unwrap();
    }
    let lr = xmp::lightroom(&x1);
    assert_eq!((lr.develop.as_deref(), lr.orientation.as_deref(), lr.edited, lr.file.as_deref()), (Some(MASTER), Some("BC"), true, Some("IMG_1.CR3")));
    assert_eq!(xmp::read_develop(&x1).unwrap().as_deref(), Some("{\"exposure\":1.0}"), "own settings stay");
    let xmp::ReadResult::Ok(w) = xmp::read(&x1) else { panic!() };
    assert_eq!((w.stars, w.rating_time), (4, Some(5)));
    let copies = library::sidecar::read_copies(w.copies_json.as_deref());
    assert_eq!(copies.len(), 1);
    assert_eq!((copies[0].name.as_str(), copies[0].lightroom.as_deref(), copies[0].lightroom_orientation.as_deref()), ("Kopie 1", Some(COPY), Some("AB")));
    let j = xmp::lightroom(&moved.join("2024/IMG_2.xmp"));
    assert_eq!((j.develop.as_deref(), j.edited), (Some(JPEG.trim()), true));
    assert!(!moved.join("2024/IMG_3.xmp").exists(), "no edits, no sidecar");
    // JPEG next to a RAW that is not in the folder: the RAW owns the shared sidecar, the JPEG never takes its settings
    let shared = xmp::lightroom(&moved.join("2024/IMG_4.xmp"));
    assert_eq!((shared.file.as_deref(), shared.develop.as_deref()), (Some("IMG_4.RAF"), Some(MASTER)));
    assert!(shared.clone().for_image(&moved.join("2024/IMG_4.JPG")).is_empty());

    // repeated: everything already there
    let r = lr_handover::run(&db, &cat, true, &|_, _| {}, &no).unwrap();
    assert_eq!((r.written, r.already, r.unreachable, r.conflicts), (0, 5, 0, 0), "{:?}", r.log);
    // the catalog stays untouched
    assert_eq!(std::fs::read(&cat).unwrap(), cat_before);
    std::fs::remove_dir_all(&dir).unwrap();
}
