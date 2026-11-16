//! What the handover of the Lightroom edits does not take over, counted against the small Lightroom Classic 15.1 test
//! catalog (private, outside the repository; skipped if it is not there): AI masks, range masks, subtracted parts,
//! point color, spots of the remove tool – and "bearbeitet" counts the same photos as the check before the import.
//! Path: `GRAINEO_LR_TEST_CATALOG_15`, default `/Users/Shared/graineo-lrtest/katalog-15-1/katalog-15-1.lrcat`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use library::{db::Db, lightroom, lr_handover};

#[test]
fn handover_counts_what_is_not_taken() {
    let cat = PathBuf::from(std::env::var("GRAINEO_LR_TEST_CATALOG_15").unwrap_or_else(|_| "/Users/Shared/graineo-lrtest/katalog-15-1/katalog-15-1.lrcat".into()));
    if !cat.is_file() {
        eprintln!("{} not there, skipped", cat.display());
        return;
    }
    let meta = |p: &std::path::Path| std::fs::metadata(p).map(|m| (m.len(), m.modified().unwrap())).unwrap();
    let before = meta(&cat);
    let dir = std::env::temp_dir().join(format!("graineo-lr-not-taken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let preview = lightroom::preview(&db, &cat).unwrap();
    lightroom::import(&db, &cat, &|_| {}).unwrap();
    // check run: nothing is written, the counts are those of a real run
    let r = lr_handover::run_with(&db, &cat, false, &|_, _| {}, &AtomicBool::new(false), &rawdev::lr_masks::not_taken).unwrap();
    println!("{}", serde_json::to_string_pretty(&r.json()).unwrap());
    assert_eq!(r.edited, preview.edited, "one count for \"bearbeitet\"");
    let n = |k: &str| r.not_taken.get(k).copied().unwrap_or(0);
    assert_eq!(n(rawdev::lr_masks::AI_MASKS), 2);
    assert_eq!(n(rawdev::lr_masks::RANGE_MASKS), 1);
    assert_eq!(n(rawdev::lr_masks::SUBTRACTIONS), 1);
    assert!(n(rawdev::lr_masks::POINT_COLOR) >= 1);
    assert!(n(rawdev::lr_masks::REMOVE_SPOTS) >= 1);
    assert_eq!(n(rawdev::lr_masks::OTHER_MASKS), 0);
    assert_eq!(r.not_taken_photos, 2);
    assert_eq!(before, meta(&cat), "catalog changed");
    let _ = std::fs::remove_dir_all(&dir);
}
