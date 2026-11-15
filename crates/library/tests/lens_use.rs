//! Camera/lens listing and counts of `library::lens_use` on a temporary library.

use library::db::Db;

#[test]
fn lenses_and_usage_from_library() {
    let dir = std::env::temp_dir().join(format!("lens-use-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::env::set_var("GRAINEO_DATA_DIR", dir.join("data"));
    std::env::set_var("GRAINEO_LIBRARY_SIDECARS", "0");
    let db = Db::open(&dir.join("data/library.sqlite")).unwrap();
    db.conn
        .execute_batch(
            "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/t/', 'T', 'own');
             INSERT INTO folder(id, root, path) VALUES(1, 1, '');
             INSERT INTO file(id, folder, name, format, camera, lens, status) VALUES
               (1, 1, 'a.x3f', 'X3F', 'SIGMA sd Quattro', NULL, 0),
               (2, 1, 'b.x3f', 'X3F', 'SIGMA sd Quattro', '', 0),
               (3, 1, 'c.arw', 'ARW', 'SONY ILCE-7M3', 'FE 50mm', 0),
               (4, 1, 'd.arw', 'ARW', 'SONY ILCE-7M3', '----', 0),
               (5, 1, 'e.arw', 'ARW', 'SONY ILCE-7M3', 'FE 50mm', 1);
             INSERT INTO develop_state(file, copy, settings) VALUES (1, '', '{\"raw\":{\"lens_profile\":\"abc123@2\"}}'), (3, '', '{}');",
        )
        .unwrap();
    let rows = library::lens_use::lenses();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0].photos, 2);
    assert_eq!(library::lens_use::photos_matching("sd Quattro", None), 2);
    assert_eq!(library::lens_use::photos_matching("ILCE-7M3", Some("----")), 1);
    assert_eq!(library::lens_use::photos_matching("ILCE-7M3", Some("FE 50mm")), 1, "missing file does not count");
    assert_eq!(library::lens_use::photos_using("abc123"), 1);
    assert_eq!(library::lens_use::photos_using("abc"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
