//! Lightroom's own as-shot white balance as the reference of a changed white balance: read from the develop history
//! (zlib blobs) or the before settings of a mini catalog, attached to the taken-over settings text by the handover
//! and by the on-demand read (`lr_develop::read`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use library::{db::Db, lightroom, lr_develop, lr_handover, store};
use rusqlite::{params, Connection};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("graineo-lr-as-shot-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wb(mode: &str, k: i32, t: i32, profile: &str) -> String {
    format!("s = {{\n\tCameraProfile = \"{profile}\",\n\tExposure2012 = 0.2,\n\tTemperature = {k},\n\tTint = {t},\n\tWhiteBalance = \"{mode}\",\n}}\n")
}

/// History step text as Lightroom stores it: 4-byte big-endian length, then zlib.
fn blob(text: &str) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(text.as_bytes()).unwrap();
    let z = e.finish().unwrap();
    let mut out = (text.len() as u32).to_be_bytes().to_vec();
    out.extend(z);
    out
}

// files: 100 custom with history, 101 custom with before settings only, 102 as shot, 103 custom without any reference,
// 104 custom whose history has two profiles; image 1005 = virtual copy of 1000 (custom, no own history)
fn catalog(path: &Path, photos: &Path) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(include_str!("catalog-schema.sql")).unwrap();
    c.execute_batch(
        "CREATE TABLE Adobe_imageDevelopSettings (id_local INTEGER PRIMARY KEY, image INTEGER, text, hasDevelopAdjustmentsEx);
         CREATE TABLE Adobe_libraryImageDevelopHistoryStep (id_local INTEGER PRIMARY KEY, dateCreated, image INTEGER, name, text);
         CREATE TABLE Adobe_imageDevelopBeforeSettings (id_local INTEGER PRIMARY KEY, beforeText, developSettings INTEGER);",
    )
    .unwrap();
    c.execute("INSERT INTO AgLibraryRootFolder(id_local,id_global,absolutePath,name) VALUES(1,'r1',?1,'Fotos')", [format!("{}/", photos.display())]).unwrap();
    c.execute("INSERT INTO AgLibraryFolder(id_local,id_global,rootFolder,pathFromRoot) VALUES(11,'f11',1,'2024/')", []).unwrap();
    for (id, base) in [(100, "L1"), (101, "L2"), (102, "L3"), (103, "L4"), (104, "L5")] {
        c.execute(
            "INSERT INTO AgLibraryFile(id_local,id_global,folder,baseName,extension,idx_filename,originalFilename,sidecarExtensions) VALUES(?1,?2,11,?3,'DNG',?4,?4,'')",
            params![id, format!("d{id}"), base, format!("{base}.DNG")],
        )
        .unwrap();
    }
    for (id, file, master, copy) in [(1000, 100, None, None), (1001, 101, None, None), (1002, 102, None, None), (1003, 103, None, None), (1004, 104, None, None), (1005, 100, Some(1000), Some("Kopie 1"))] {
        c.execute(
            "INSERT INTO Adobe_images(id_local,id_global,rootFile,rating,pick,colorLabels,masterImage,copyName,captureTime,orientation,fileFormat,fileWidth,fileHeight) VALUES(?1,?2,?3,0,0,'',?4,?5,'2024-05-11T14:03:22','AB','DNG',6000,4000)",
            params![id, format!("i{id}"), file, master, copy],
        )
        .unwrap();
    }
    let settings = [
        (1, 1000, wb("Custom", 9000, 1, "Adobe Standard")),
        (2, 1001, wb("Daylight", 5500, 10, "Adobe Standard")),
        (3, 1002, wb("As Shot", 8000, 1, "Adobe Standard")),
        (4, 1003, wb("Custom", 4000, 0, "Adobe Standard")),
        (5, 1004, wb("Custom", 6000, 5, "Camera Standard")),
        (6, 1005, wb("Custom", 7000, -3, "Adobe Standard")),
    ];
    for (id, image, text) in &settings {
        c.execute("INSERT INTO Adobe_imageDevelopSettings(id_local,image,text,hasDevelopAdjustmentsEx) VALUES(?1,?2,?3,1)", params![id, image, text]).unwrap();
    }
    // history: import step "As Shot" with Lightroom's numbers, then the user's change
    let steps: [(i64, f64, i64, String); 6] = [
        (1, 1.0, 1000, wb("As Shot", 8000, 1, "Adobe Standard")),
        (2, 2.0, 1000, wb("Custom", 9000, 1, "Adobe Standard")),
        (3, 1.0, 1003, wb("Custom", 4000, 0, "Adobe Standard")),
        // 1004: as shot under two profiles, the later one under another profile than the current
        (4, 1.0, 1004, wb("As Shot", 5050, 13, "Camera Standard")),
        (5, 2.0, 1004, wb("As Shot", 5100, 12, "Adobe Standard")),
        (6, 3.0, 1004, wb("Custom", 6000, 5, "Camera Standard")),
    ];
    for (id, date, image, text) in &steps {
        c.execute("INSERT INTO Adobe_libraryImageDevelopHistoryStep(id_local,dateCreated,image,name,text) VALUES(?1,?2,?3,'x',?4)", params![id, date, image, blob(text)]).unwrap();
    }
    c.execute("INSERT INTO Adobe_imageDevelopBeforeSettings(id_local,beforeText,developSettings) VALUES(1,?1,2)", [wb("As Shot", 4950, 21, "Adobe Standard")]).unwrap();
}

#[test]
fn handover_attaches_lightrooms_as_shot_reference() {
    let dir = tmp("run");
    let photos = dir.join("Fotos");
    std::fs::create_dir_all(photos.join("2024")).unwrap();
    for n in ["L1", "L2", "L3", "L4", "L5"] {
        std::fs::write(photos.join("2024").join(format!("{n}.DNG")), b"x").unwrap();
    }
    let cat = dir.join("Test.lrcat");
    catalog(&cat, &photos);
    let cat_before = std::fs::read(&cat).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    let file = |lr: i64| -> i64 { db.conn.query_row("SELECT id FROM file WHERE lr_id=?1", [lr], |r| r.get(0)).unwrap() };
    let no = AtomicBool::new(false);

    // a library from before the reference line: the plain text is there already
    store::put_lightroom(&db.conn, file(100), None, &library::xmp::Lightroom { develop: Some(wb("Custom", 9000, 1, "Adobe Standard").trim().to_string()), orientation: Some("AB".into()), edited: true, file: Some("L1.DNG".into()) }).unwrap();

    let r = lr_handover::run(&db, &cat, true, &|_, _| {}, &no).unwrap();
    assert_eq!(r.errors, 0, "{:?}", r.log);
    let text = |lr: i64, copy: Option<&str>| store::get(&db.conn, file(lr), copy).unwrap().lightroom.develop.unwrap();
    let line = |k: &str, t: &str| format!("\ngraineoAsShot = {{ Temperature = {k}, Tint = {t} }}");
    // history import step; the photo taken over before gets the line on this run (user-started handover)
    assert_eq!(text(100, None), format!("{}{}", wb("Custom", 9000, 1, "Adobe Standard").trim(), line("8000", "1")));
    // before settings only
    assert!(text(101, None).ends_with(&line("4950", "21")), "{}", text(101, None));
    // "As Shot": unchanged text
    assert_eq!(text(102, None), wb("As Shot", 8000, 1, "Adobe Standard").trim());
    // no reference in the catalog: unchanged text (absolute numbers as before)
    assert_eq!(text(103, None), wb("Custom", 4000, 0, "Adobe Standard").trim());
    // the as-shot step under the current profile wins over a later one under another profile
    assert!(text(104, None).ends_with(&line("5050", "13")), "{}", text(104, None));
    // virtual copy without own history: its master's reference
    assert!(text(100, Some("Kopie 1")).ends_with(&line("8000", "1")), "{}", text(100, Some("Kopie 1")));

    // repeated: nothing changes
    let r = lr_handover::run(&db, &cat, true, &|_, _| {}, &no).unwrap();
    assert_eq!(r.written, 0, "{:?}", r.log);

    // the on-demand read (tools, `initial_settings_from_catalog`) attaches the same line
    let e = lr_develop::read(&cat, &photos.join("2024/L1.DNG")).unwrap();
    assert!(e.iter().find(|e| e.copy.is_none()).unwrap().text.ends_with(&line("8000", "1")));
    assert_eq!(std::fs::read(&cat).unwrap(), cat_before, "the catalog stays untouched");
    std::fs::remove_dir_all(&dir).unwrap();
}
