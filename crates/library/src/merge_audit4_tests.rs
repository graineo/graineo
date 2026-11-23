//! Fourth audit of the merged photos (before 1.5.0): one test per finding, stating the behaviour that is expected.

use super::*;

/// F3: the merged photos a rebuilt library takes over ([`copy_from`]) get their ids from the database, and the ids
/// are not noted as "a merged photo had this id". An ordinary photo that comes into the library after such a merged
/// photo was removed takes its file and image id – ⌘Z cannot bring the merged photo back ("place taken").
#[test]
fn ids_of_merged_photos_taken_over_by_a_rebuild_are_never_given_again() {
    let dir = std::env::temp_dir().join(format!("merge-audit4-{}-{}", std::process::id(), crate::now()));
    std::fs::create_dir_all(&dir).unwrap();
    let rows = "INSERT INTO root_folder(id, path, name, origin) VALUES(1, '/fotos/', 'fotos', 'own');
                INSERT INTO folder(id, root, path) VALUES(1, 1, 'reihe/');
                INSERT INTO file(id, folder, name, format, size, mtime) VALUES(1, 1, 'IMG_1.CR2', 'CR2', 100, 10), (2, 1, 'IMG_2.CR2', 'CR2', 101, 11), (3, 1, 'IMG_3.CR2', 'CR2', 102, 12);
                INSERT INTO image(id, file) VALUES(11, 1), (12, 2), (13, 3);";
    let old = Db::open(&dir.join("old.sqlite")).unwrap();
    old.conn.execute_batch(rows).unwrap();
    create(&old, "hdr", &[1, 2, 3], 1, "{}", 3).unwrap();
    let new = Db::open(&dir.join("new.sqlite")).unwrap();
    new.conn.execute_batch(rows).unwrap();
    assert_eq!(copy_from(&old.conn, &new).unwrap(), Copied { copied: 1, lost: 0 });
    let (file, image): (i64, i64) = new.conn.query_row("SELECT m.file, b.id FROM merge m JOIN image b ON b.file = m.file", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    // "HDR entfernen", then the sync finds a new photo
    let snap = take_out(&new, &[file]).unwrap();
    new.conn.execute(&format!("INSERT INTO file(id, folder, name, format) VALUES({NEW_FILE_ID}, 1, 'IMG_9.CR2', 'CR2')"), []).unwrap();
    let photo = new.conn.last_insert_rowid();
    new.conn.execute(&format!("INSERT INTO image(id, file) VALUES({NEW_IMAGE_ID}, ?1)"), [photo]).unwrap();
    let photo_image = new.conn.last_insert_rowid();
    let back = bring_back(&new, &snap);
    drop((old, new));
    let _ = std::fs::remove_dir_all(dir);
    assert!(photo != file && photo_image != image, "the new photo got file {photo} and image {photo_image}; the merged photo taken out had file {file} and image {image}");
    assert_eq!(back, Ok(()), "⌘Z brings the merged photo back");
}
