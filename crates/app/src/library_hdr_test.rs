//! Where a new merged photo lands (test 1.5.0-test.4: after "HDR zusammenfügen …" in "Zuletzt importiert" with an
//! expanded series, Develop opened an X3F of another series, the tool "Zusammenfügen" stayed closed, the HDR was not
//! in the view). Every combination of source, filter, series state and stack setting: the merged photo is in the
//! view afterwards and active, Develop is asked for, nothing else is opened.
//!
//! Needs `GRAINEO_DATA_DIR`. A library of its own in the temp folder; empty files stand in for the RAWs (nothing is
//! merged here, only the library side of starting a merge).

use super::*;

struct Lib {
    dir: PathBuf,
    db: Db,
    /// images of the bracket (RW2, dark → bright), of an X3F series and two single JPEGs
    bracket: [i64; 3],
    x3f: [i64; 3],
    singles: [i64; 2],
    /// two RW2 of the same camera half an hour apart (no series)
    far: [i64; 2],
    collection: i64,
    quick: i64,
    keyword: i64,
}

fn library(tag: &str) -> Lib {
    let dir = std::env::temp_dir().join(format!("graineo-hdr-land-{tag}-{}-{}", std::process::id(), library::now()));
    std::fs::create_dir_all(dir.join("fenster")).unwrap();
    std::fs::create_dir_all(dir.join("sigma")).unwrap();
    let db = Db::open(&dir.join("library.sqlite")).unwrap();
    let root = db.create_root(&format!("{}/", dir.display()), "test", "own", None).unwrap();
    let fenster = db.create_folder(root, "fenster/", None).unwrap();
    let sigma = db.create_folder(root, "sigma/", None).unwrap();
    let run = db.new_run("sync", "").unwrap();
    db.mark_run(run, "import").unwrap();
    let add = |folder: i64, sub: &str, name: &str, format: &str, camera: &str, t: f64, second: u32| -> i64 {
        std::fs::write(dir.join(sub).join(name), b"").unwrap();
        db.conn
            .execute(
                &format!(
                    "INSERT INTO file(folder, name, format, camera, capture_time, width, height, time, iso, aperture, size, mtime, exif, text_read, xmp_read)
                     VALUES({folder}, '{name}', '{format}', '{camera}', '2026-11-01T10:00:{second:02}', 4592, 3448, {t}, 200, 8.0, 1, 1, 1, 1, 1)"
                ),
                [],
            )
            .unwrap();
        let file = db.conn.last_insert_rowid();
        db.conn.execute(&format!("INSERT INTO image(file, run) VALUES({file}, {run})"), []).unwrap();
        db.conn.last_insert_rowid()
    };
    let bracket = [
        add(fenster, "fenster", "P1000001.RW2", "RW2", "Panasonic DMC-GX7", 1.0 / 400.0, 10),
        add(fenster, "fenster", "P1000002.RW2", "RW2", "Panasonic DMC-GX7", 1.0 / 100.0, 11),
        add(fenster, "fenster", "P1000003.RW2", "RW2", "Panasonic DMC-GX7", 1.0 / 25.0, 12),
    ];
    let x3f = [
        add(sigma, "sigma", "SDQH1646_1.X3F", "X3F", "SIGMA sd Quattro H", 1.0 / 250.0, 1),
        add(sigma, "sigma", "SDQH1646_2_-2EV.X3F", "X3F", "SIGMA sd Quattro H", 1.0 / 1000.0, 2),
        add(sigma, "sigma", "SDQH1646_3.X3F", "X3F", "SIGMA sd Quattro H", 1.0 / 60.0, 3),
    ];
    std::fs::create_dir_all(dir.join("weit")).unwrap();
    let weit = db.create_folder(root, "weit/", None).unwrap();
    let far = [
        add(weit, "weit", "P1000101.RW2", "RW2", "Panasonic DMC-GX7", 1.0 / 400.0, 0),
        add(weit, "weit", "P1000102.RW2", "RW2", "Panasonic DMC-GX7", 1.0 / 100.0, 0),
    ];
    db.conn.execute(&format!("UPDATE file SET capture_time = '2026-11-01T10:31:00' WHERE id = (SELECT file FROM image WHERE id = {})", far[1]), []).unwrap();
    let singles = [add(fenster, "fenster", "IMG_1.JPG", "JPG", "Panasonic DMC-GX7", 1.0 / 100.0, 40), add(fenster, "fenster", "IMG_2.JPG", "JPG", "Panasonic DMC-GX7", 1.0 / 100.0, 50)];
    library::series::create(&db, &bracket, bracket[0]).unwrap();
    library::series::create(&db, &x3f, x3f[0]).unwrap();
    let collection = db.create_collection("Fenster").unwrap();
    db.add_to_collection(collection, &bracket).unwrap();
    db.add_to_collection(collection, &x3f).unwrap();
    let quick = db.quick_collection().unwrap();
    db.add_to_collection(quick, &bracket).unwrap();
    for b in bracket {
        library::sync::set_keywords(&db, b, &["Fenster".to_string()], library::now(), true).unwrap();
    }
    let keyword = db.keywords().unwrap().into_iter().find(|k| k.name == "Fenster").unwrap().id;
    Lib { dir, db, bracket, x3f, singles, far, collection, quick, keyword }
}

/// The view on a library of its own, rows and series read.
fn view(ctx: &egui::Context, lib: &Lib) -> LibraryView {
    let mut b = LibraryView::new(ctx);
    b.db = Some(Db::open(&lib.db.path).unwrap());
    b.reload_now();
    load_series(&mut b, lib.bracket[0]);
    b
}

/// Read the stacks again and wait until `image` is in one.
fn load_series(b: &mut LibraryView, image: i64) {
    crate::series_job::changed();
    let t = std::time::Instant::now();
    loop {
        b.series_poll();
        if b.series.of(image).is_some() && t.elapsed() > std::time::Duration::from_millis(30) {
            break;
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "series not loaded");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    b.refilter = true;
    b.apply_filter();
}

fn shown(b: &LibraryView) -> Vec<i64> {
    b.visible.iter().map(|&i| b.rows[i].image).collect()
}

#[derive(Clone, Copy, Debug)]
enum Filt {
    None,
    Stars,
    Picked,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fallback {
    /// stays in the source
    No,
    /// folder of the reference
    Folder,
    /// folder of the reference without filters
    Unfiltered,
}

/// One case: source, filter, series expanded, stack setting, selection (indices into the bracket; the right click on
/// the middle one). Returns the merged image.
fn case(source: &str, filt: Filt, expanded: bool, stack: bool, pick: &[usize], expect: Fallback) {
    let lib = library(source);
    let ctx = egui::Context::default();
    lr::apply_theme(&ctx);
    // the sources rated and picked, so star/flag filters keep them in view
    lib.db.conn.execute(&format!("UPDATE image SET stars = 2, flag = 1 WHERE id IN ({}, {}, {})", lib.bracket[0], lib.bracket[1], lib.bracket[2]), []).unwrap();
    let stack_id: i64 = lib.db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [lib.bracket[0]], |r| r.get(0)).unwrap();
    library::series::set_collapsed(&lib.db, stack_id, !expanded).unwrap();
    let mut b = view(&ctx, &lib);
    let src = match source {
        "all" => Source::All,
        "recent" => Source::Recent,
        "folder" => Source::Folder(b.folder.iter().find(|o| o.path == "fenster/").unwrap().root, "fenster/".into()),
        "collection" => Source::Collection(lib.collection),
        "quick" => Source::Quick,
        "keyword" => Source::Keyword(lib.keyword),
        "series" => Source::Series,
        "date" => Source::Date("2026-11-01".into()),
        "duplicates" => Source::Duplicates,
        other => panic!("{other}"),
    };
    b.set_source(src.clone());
    match filt {
        Filt::None => {}
        Filt::Stars => b.filter.stars_min = 1,
        Filt::Picked => b.filter.flags = FLAG_PICKED,
    }
    b.refilter = true;
    b.apply_filter();
    let before = shown(&b);
    let label = format!("{source} {filt:?} expanded={expanded} stack={stack} pick={pick:?}");
    // the user's selection in the grid (duplicates: nothing of the bracket is shown, the photos come from the filmstrip)
    let selected: Vec<i64> = pick.iter().map(|&k| lib.bracket[k]).collect();
    b.selection = selected.iter().copied().collect();
    b.active = Some(lib.bracket[1]);
    b.nothing_selected = false;
    let rows = b.hdr_candidates(Some(lib.bracket[1]));
    let req = match crate::hdr_ui::request(rows) {
        Ok(r) => r,
        Err(why) => panic!("{label}: refused: {why}"),
    };
    let req = crate::hdr_ui::Request { stack, keyword: "HDR".into(), ..req };
    let p = b.prepare_merge(req);
    let (c, _, _, _) = create_merge(&lib.db, &p).unwrap_or_else(|e| panic!("{label}: {e}"));
    // the app: rows reloaded, then the photo is shown (the series follow later, a frame or more after)
    b.reload_now();
    let mut actions = Vec::new();
    b.show_new_merge(c.image, p.stacked(), true, &mut actions);
    let check = |b: &LibraryView, actions: &[Action], when: &str| {
        assert!(shown(b).contains(&c.image), "{label} ({when}): the HDR is not in the view: {:?}", shown(b));
        assert_eq!(b.active, Some(c.image), "{label} ({when}): another photo is active");
        assert!(b.selection.is_empty(), "{label} ({when}): an old selection is left");
        let _ = actions;
    };
    check(&b, &actions, "after the reload");
    assert!(actions.iter().any(|a| matches!(a, Action::Develop)), "{label}: Develop not asked for");
    let note = actions.iter().find_map(|a| if let Action::Status(s) = a { Some(s.clone()) } else { None });
    match expect {
        Fallback::No => {
            assert_eq!(b.source, src, "{label}: the source changed");
            assert!(note.is_none(), "{label}: {note:?}");
        }
        Fallback::Folder | Fallback::Unfiltered => {
            assert!(matches!(&b.source, Source::Folder(_, p) if p == "fenster/"), "{label}: {:?}", b.source);
            let n = note.unwrap_or_else(|| panic!("{label}: no note"));
            assert!(n.contains("fenster"), "{label}: {n}");
            assert_eq!(n.contains("ohne Filter"), expect == Fallback::Unfiltered, "{label}: {n}");
        }
    }
    // the stacks read again (series job): the HDR stays shown and active
    if stack {
        load_series(&mut b, c.image);
        check(&b, &actions, "after the series");
        let k = b.series.of(c.image).expect("in a stack");
        assert_eq!(b.series.stacks[k].collapsed, !expanded, "{label}: the stack of the merge keeps the state of the series");
    }
    // nothing vanishes from the view: the tiles before plus the HDR (an expanded stack or a photo of its own) or the
    // HDR in place of the collapsed series
    if expect == Fallback::No {
        let after = shown(&b);
        // "Serien" keeps every stack as one tile, whatever its state
        let one_tile = stack && (!expanded || source == "series");
        for i in &before {
            let in_stack = lib.bracket.contains(i);
            assert!(after.contains(i) || (in_stack && one_tile), "{label}: {i} vanished: {before:?} → {after:?}");
        }
        let grows = !one_tile;
        assert_eq!(after.len(), before.len() + grows as usize, "{label}: {before:?} → {after:?}");
    }
    let _ = (lib.x3f, lib.singles, lib.quick, lib.far);
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

#[test]
fn the_new_merge_is_always_shown_and_active() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    for source in ["all", "recent", "folder", "collection", "quick", "keyword", "date", "series"] {
        for expanded in [true, false] {
            for stack in [true, false] {
                // "Serien" shows only stacks: a merge without a stack is a photo of its own outside it
                let expect = if source == "series" && !stack { Fallback::Folder } else { Fallback::No };
                case(source, Filt::None, expanded, stack, &[0, 1, 2], expect);
            }
        }
    }
    // two of three, the right click on one of them
    case("recent", Filt::None, true, true, &[1, 2], Fallback::No);
    // filters the merged photo does not pass (no stars, no flag yet): its folder without filters
    for filt in [Filt::Stars, Filt::Picked] {
        for source in ["recent", "folder", "collection"] {
            case(source, filt, true, true, &[0, 1, 2], Fallback::Unfiltered);
        }
    }
    // a view the merged photo can never belong to
    case("duplicates", Filt::None, false, true, &[0, 1, 2], Fallback::Folder);
}

/// The merged photo joins the collection being looked at and carries the keywords of its reference.
/// N3 of the second audit: the library side of a merge is one transaction. When a later part fails (here: the stack,
/// which needs two photos), nothing of the merged photo stays – before, the row was there although the app said
/// "HDR nicht angelegt", without a ⌘Z step and without a job.
#[test]
fn a_merge_that_cannot_be_created_leaves_nothing_behind() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("atomic");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::Collection(lib.collection));
    b.apply_filter();
    b.selection = lib.bracket.iter().copied().collect();
    let req = crate::hdr_ui::request(b.hdr_candidates(Some(lib.bracket[1]))).unwrap();
    let mut p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: "HDR".into(), ..req });
    let n = |sql: &str| lib.db.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    let before = (n("SELECT count(*) FROM file"), n("SELECT count(*) FROM image"), n("SELECT count(*) FROM stack"), n("SELECT count(*) FROM stack_image"), n("SELECT count(*) FROM image_keyword"));
    // a stack that cannot be made: no members besides the merged photo, and the reference in no stack
    p.members.clear();
    p.reference_image = -1;
    assert!(create_merge(&lib.db, &p).is_err());
    assert_eq!(n("SELECT count(*) FROM merge"), 0, "no merged photo stays behind");
    let after = (n("SELECT count(*) FROM file"), n("SELECT count(*) FROM image"), n("SELECT count(*) FROM stack"), n("SELECT count(*) FROM stack_image"), n("SELECT count(*) FROM image_keyword"));
    assert_eq!(before, after);
    // and the library is not left inside a transaction: the next merge works
    let req = crate::hdr_ui::request(b.hdr_candidates(Some(lib.bracket[1]))).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: "HDR".into(), ..req });
    let (c, _, _, _) = create_merge(&lib.db, &p).unwrap();
    assert!(library::merge::is_virtual(&lib.db.conn, c.file));
}

#[test]
fn the_new_merge_joins_collection_and_keywords() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("join");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::Collection(lib.collection));
    b.apply_filter();
    b.selection = lib.bracket.iter().copied().collect();
    let req = crate::hdr_ui::request(b.hdr_candidates(Some(lib.bracket[1]))).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: "HDR".into(), ..req });
    let before: (i64, i64, bool) = lib
        .db
        .conn
        .query_row(&format!("SELECT s.id, 3, s.collapsed FROM stack s JOIN stack_image i ON i.stack = s.id WHERE i.image = {}", lib.bracket[0]), [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    let (c, change, _, _) = create_merge(&lib.db, &p).unwrap();
    assert!(lib.db.images_of_collection(lib.collection).unwrap().contains(&c.image));
    assert!(!lib.db.images_of_collection(lib.quick).unwrap().contains(&c.image), "only the collection looked at");
    let mut words = library::sync::keyword_paths(&lib.db, c.image).unwrap();
    words.sort();
    assert_eq!(words, ["Fenster", "HDR"]);
    // ⌘Z (library_manage.rs, Undo::Merge): the stack as before, the HDR gone from the collection, the sources stay
    library::series::restore(&lib.db, &change.expect("stacked")).unwrap();
    library::merge::remove(&lib.db, c.file).unwrap();
    let left = lib.db.images_of_collection(lib.collection).unwrap();
    assert!(!left.contains(&c.image) && lib.bracket.iter().all(|b| left.contains(b)));
    let state = |db: &Db| -> (i64, i64, bool) {
        db.conn
            .query_row(
                &format!("SELECT s.id, count(*), s.collapsed FROM stack s JOIN stack_image i ON i.stack = s.id WHERE s.id = (SELECT stack FROM stack_image WHERE image = {}) GROUP BY s.id", lib.bracket[0]),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    assert_eq!((state(&lib.db).1, state(&lib.db).2), (3, before.2), "the series is back with its three photos and its state");
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// What the selection offers, and why a merge is refused (the note must name the reason).
#[test]
fn selections_and_refusals() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("pick");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::All);
    b.apply_filter();
    let ask = |b: &mut LibraryView, sel: &[i64], click: i64| {
        b.selection = sel.iter().copied().collect();
        b.active = Some(click);
        b.nothing_selected = false;
        let rows = b.hdr_candidates(Some(click));
        let files: Vec<i64> = rows.iter().map(|r| r.image).collect();
        (files, crate::hdr_ui::request(rows).err())
    };
    // one photo of a series, nothing else selected: its whole bracket
    let (got, why) = ask(&mut b, &[], lib.bracket[1]);
    assert_eq!((got.len(), why), (3, None));
    // a single photo without a bracket
    let (_, why) = ask(&mut b, &[], lib.singles[0]);
    assert!(why.unwrap().contains("Mindestens zwei"));
    // X3F (Foveon) are never sources
    let (_, why) = ask(&mut b, &lib.x3f, lib.x3f[1]);
    assert!(why.unwrap().contains("Mehrfachaufnahmen der Kamera (X3I)"));
    // across two series (RW2 and X3F): refused for the X3F
    let mut both = lib.bracket.to_vec();
    both.extend(lib.x3f);
    let (_, why) = ask(&mut b, &both, lib.bracket[1]);
    assert!(why.unwrap().contains(".X3F"));
    // RAW and JPEG mixed
    let (_, why) = ask(&mut b, &[lib.bracket[0], lib.bracket[1], lib.singles[0]], lib.bracket[1]);
    assert!(why.unwrap().contains("IMG_1.JPG"));
    // the right click outside the selection: that photo's bracket, not the selection
    let (got, _) = ask(&mut b, &[lib.singles[0], lib.singles[1]], lib.bracket[2]);
    assert_eq!(got.len(), 3);
    // a merge result in the selection is left out (never a source)
    let req = crate::hdr_ui::request(b.hdr_candidates(Some(lib.bracket[1]))).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: String::new(), ..req });
    let (c, _, _, _) = create_merge(&lib.db, &p).unwrap();
    b.reload_now();
    load_series(&mut b, c.image);
    let (got, why) = ask(&mut b, &[c.image, lib.bracket[0], lib.bracket[1], lib.bracket[2]], c.image);
    assert_eq!((got.len(), why), (3, None), "the sources, without the HDR");
    // "Alle Reihen zusammenfügen": the bracket with its merge marked, no X3F group
    b.selection = both.iter().copied().collect();
    b.active = Some(lib.bracket[1]);
    let groups = b.hdr_groups();
    assert_eq!(groups.len(), 2, "the Bayer bracket and the X3F series");
    let rw2 = groups.iter().find(|g| g.rows[0].format == "RW2").unwrap();
    assert!(rw2.merged && !rw2.on, "merged already: offered, not ticked");
    let x3f = groups.iter().find(|g| g.rows[0].format == "X3F").unwrap();
    assert!(!x3f.on && x3f.reason.as_deref().is_some_and(|r| r.contains("X3I")), "X3F: not ticked, with the reason");
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// Before the merge: photos that do not look like one bracket are asked about, a series or bracket never.
#[test]
fn doubtful_selections_are_asked_about() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("doubt");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::All);
    b.apply_filter();
    let rows = |b: &mut LibraryView, sel: &[i64]| {
        b.selection = sel.iter().copied().collect();
        b.active = Some(sel[0]);
        b.nothing_selected = false;
        crate::hdr_ui::request(b.hdr_candidates(Some(sel[0]))).unwrap().rows
    };
    // the series: no question
    let r = rows(&mut b, &lib.bracket);
    assert_eq!(b.hdr_doubt(&r), None);
    // two of it: still the series
    let r = rows(&mut b, &lib.bracket[1..]);
    assert_eq!(b.hdr_doubt(&r), None);
    // the same camera 31 minutes apart: asked
    let r = rows(&mut b, &lib.far);
    assert_eq!(b.hdr_doubt(&r), Some(Doubt::Gap(31 * 60)));
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// The rule itself: a series/bracket is never doubted; a pause of more than two minutes or unlike neighbours are.
#[test]
fn bracket_doubt_rule() {
    let row = |capture: Option<i64>| SourceRow {
        file: 1,
        image: 1,
        path: PathBuf::new(),
        name: String::new(),
        format: "RW2".into(),
        camera: None,
        time: Some(0.01),
        iso: None,
        aperture: None,
        focal: None,
        capture,
        width: None,
        height: None,
    };
    let close = [row(Some(0)), row(Some(2)), row(Some(4))];
    assert_eq!(bracket_doubt(&close, false, None), None);
    assert_eq!(bracket_doubt(&close, false, Some(true)), None);
    assert_eq!(bracket_doubt(&close, false, Some(false)), Some(Doubt::Unlike));
    assert_eq!(bracket_doubt(&close, true, Some(false)), None, "a detected series is never doubted");
    let slow = [row(Some(0)), row(Some(120)), row(Some(240))];
    assert_eq!(bracket_doubt(&slow, false, None), None, "two minutes per frame still count");
    let apart = [row(Some(0)), row(Some(3)), row(Some(300))];
    assert_eq!(bracket_doubt(&apart, false, Some(true)), Some(Doubt::Gap(297)));
    assert_eq!(bracket_doubt(&[row(None), row(None)], false, None), None, "no capture times: nothing known");
}

/// QA 1.5.0-test.5 #6/#5: "HDR zusammenfügen" never switches silently to another photo or bracket, and never merges a
/// merged photo or the same frames again unasked.
#[test]
fn the_pick_follows_the_selection_and_existing_merges() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("pick-rule");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::All);
    b.apply_filter();
    let set = |b: &mut LibraryView, sel: &[i64], active: i64| {
        b.selection = sel.iter().copied().collect();
        b.active = Some(active);
        b.nothing_selected = false;
    };
    let images = |rows: &[SourceRow]| {
        let mut v: Vec<i64> = rows.iter().map(|r| r.image).collect();
        v.sort_unstable();
        v
    };
    // the selection with the active photo in it: merged as it is
    set(&mut b, &lib.bracket, lib.bracket[1]);
    assert!(matches!(b.hdr_pick(None), Pick::Merge(ref r) if images(r) == lib.bracket.to_vec()));
    // the active photo outside a selection of more than one: the selection counts, asked about with the active photo named
    set(&mut b, &lib.bracket, lib.singles[0]);
    match b.hdr_pick(None) {
        Pick::Selection(rows, outside) => {
            assert_eq!(images(&rows), lib.bracket.to_vec(), "the selection, not the bracket of the active photo");
            assert_eq!(outside, "IMG_1.JPG");
        }
        other => panic!("{other:?}"),
    }
    // the photo shown in Develop counts like the active one
    set(&mut b, &lib.bracket, lib.bracket[0]);
    assert!(matches!(b.hdr_pick(Some(lib.far[0])), Pick::Selection(..)));
    // one photo: its bracket
    set(&mut b, &[], lib.bracket[2]);
    assert!(matches!(b.hdr_pick(None), Pick::Merge(ref r) if r.len() == 3));
    // nothing exists yet
    let rows = match b.hdr_pick(None) {
        Pick::Merge(r) => r,
        other => panic!("{other:?}"),
    };
    assert!(b.existing_merge(&rows).is_none());
    // merged: the same frames now have an HDR, and the HDR itself opens instead of being merged
    let req = crate::hdr_ui::request(rows.clone()).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: String::new(), ..req });
    let (c, _, _, _) = create_merge(&lib.db, &p).unwrap();
    b.reload_now();
    load_series(&mut b, c.image);
    let (image, name) = b.existing_merge(&rows).expect("the HDR of these frames");
    assert_eq!((image, name.as_str()), (c.image, "P1000002-HDR"));
    assert!(b.existing_merge(&rows[..2]).is_none(), "other frames: no HDR of them");
    set(&mut b, &[c.image], c.image);
    assert_eq!(b.hdr_pick(None), Pick::Open(c.image), "the tile of the stack with the HDR on top");
    set(&mut b, &[], lib.bracket[1]);
    assert_eq!(b.hdr_pick(Some(c.image)), Pick::Open(c.image), "the HDR shown in Develop");
    // an SFD shot (X3I) goes to Develop, which merges its own frames
    lib.db.conn.execute(&format!("UPDATE file SET format = 'X3I' WHERE id = (SELECT file FROM image WHERE id = {})", lib.singles[1]), []).unwrap();
    b.reload_now();
    set(&mut b, &[], lib.singles[1]);
    assert_eq!(b.hdr_pick(None), Pick::Develop(lib.singles[1]));
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// QA 1.5.0-test.5 #13: a detected bracket stays a bracket after the merge – the HDR joins it on top (one stack, every
/// photo once), the grouping leaves it alone from then on, and ⌘Z puts it back exactly.
#[test]
fn the_bracket_stays_a_bracket() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("keep-bracket");
    let stack: i64 = lib.db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [lib.bracket[0]], |r| r.get(0)).unwrap();
    lib.db.conn.execute("UPDATE stack SET kind = 'bracket', origin = 'auto', cover_by = 'auto' WHERE id = ?1", [stack]).unwrap();
    let open_before = library::series::open_count(&lib.db.conn);
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::All);
    b.apply_filter();
    b.selection.clear();
    b.active = Some(lib.bracket[1]);
    let rows = b.hdr_candidates(None);
    let req = crate::hdr_ui::request(rows).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: String::new(), ..req });
    let (c, change, _, _) = create_merge(&lib.db, &p).unwrap();
    let s = library::series::get(&lib.db.conn, stack).unwrap().expect("the same stack");
    assert_eq!(s.kind, library::series::Kind::Bracket, "still a bracket");
    assert_eq!(s.origin, "user", "the grouping leaves it alone");
    assert_eq!(s.cover, Some(c.image));
    assert_eq!(s.images.len(), 4, "the three frames and the HDR, nothing twice: {:?}", s.images);
    assert_eq!(s.images[0], c.image, "the HDR on top");
    assert_eq!(library::series::open_count(&lib.db.conn), open_before, "\"Serien … offen\" keeps counting it");
    let stacks: i64 = lib.db.conn.query_row("SELECT count(*) FROM stack", [], |r| r.get(0)).unwrap();
    assert_eq!(stacks, 2, "no new stack (the bracket and the X3F series)");
    // the view knows it at once the same way (before the series are read again)
    b.reload_now();
    let mut actions = Vec::new();
    b.show_new_merge(c.image, p.stacked(), false, &mut actions);
    let k = b.series.of(c.image).expect("in the stack");
    assert_eq!(b.series.stacks[k].kind, library::series::Kind::Bracket);
    assert_eq!(b.series.stacks[k].images.len(), 4);
    // ⌘Z: the stack exactly as before, the HDR gone
    library::series::restore(&lib.db, &change.expect("stacked")).unwrap();
    library::merge::remove(&lib.db, c.file).unwrap();
    let s = library::series::get(&lib.db.conn, stack).unwrap().expect("the stack");
    assert_eq!((s.kind, s.origin.as_str(), s.cover, s.images.clone()), (library::series::Kind::Bracket, "auto", Some(lib.bracket[0]), lib.bracket.to_vec()));
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// QA 1.5.0-test.5 (repeat): "HDR + 3 RAWs" selected merged the three silently without the HDR. A merged photo in the
/// selection is never a source and never left out unasked: it is named, and offered to open when the other photos are
/// its frames.
#[test]
fn a_merged_photo_in_the_selection_is_never_left_out_silently() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("leaves-out");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::All);
    b.apply_filter();
    // an HDR of the first two frames
    b.selection = lib.bracket[..2].iter().copied().collect();
    b.active = Some(lib.bracket[0]);
    let rows = match b.hdr_pick(None) {
        Pick::Merge(r) => r,
        other => panic!("{other:?}"),
    };
    let req = crate::hdr_ui::request(rows).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: String::new(), ..req });
    let (c, _, _, _) = create_merge(&lib.db, &p).unwrap();
    b.reload_now();
    load_series(&mut b, c.image);
    let set = |b: &mut LibraryView, sel: &[i64], active: i64| {
        b.selection = sel.iter().copied().collect();
        b.active = Some(active);
        b.nothing_selected = false;
    };
    // the HDR and all three frames (one of them not its source): asked, the HDR named, nothing of it to open
    set(&mut b, &[c.image, lib.bracket[0], lib.bracket[1], lib.bracket[2]], c.image);
    match b.hdr_pick(None) {
        Pick::LeavesOut { rows, left, own, outside } => {
            assert_eq!(rows.len(), 3, "the three frames, never the HDR");
            assert_eq!(left, vec![(c.image, "P1000002-HDR".to_string())]);
            assert_eq!(own, None, "the third frame is not one of its sources");
            assert_eq!(outside, None);
        }
        other => panic!("{other:?}"),
    }
    // the HDR and its own frames: offered to open
    set(&mut b, &[c.image, lib.bracket[0], lib.bracket[1]], lib.bracket[0]);
    assert!(matches!(b.hdr_pick(None), Pick::LeavesOut { own: Some(o), .. } if o == c.image));
    // the HDR and one of its frames (nothing new possible): still its own
    set(&mut b, &[c.image, lib.bracket[1]], c.image);
    assert!(matches!(b.hdr_pick(None), Pick::LeavesOut { own: Some(o), ref rows, .. } if o == c.image && rows.len() == 1));
    // without a merged photo nothing changes
    set(&mut b, &lib.bracket, lib.bracket[0]);
    assert!(matches!(b.hdr_pick(None), Pick::Merge(ref r) if r.len() == 3));
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// Carry out the background work of the view (library writes) and read the rows again.
fn settle(b: &mut LibraryView, ctx: &egui::Context) -> Vec<Action> {
    let t = std::time::Instant::now();
    let mut actions = Vec::new();
    b.run_merge_removals();
    loop {
        let done = b.db_writer.fetch(ctx);
        let n = done.len();
        for finished in done {
            finished(b, &mut actions);
        }
        if n == 0 && !b.db_writer.busy() {
            break;
        }
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "library writes not finished");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    b.reload_now();
    actions
}

/// "HDR entfernen (Quellen bleiben)" is one ⌘Z step: the photo comes back with its recipe, development and place in
/// the stack. A photo that carries work of its own (a development by hand, painted areas) is asked about first –
/// values the automatic tone set do not count. ⌘Z of "HDR zusammenfügen" asks for such a photo as well and leaves a
/// step that brings it back.
#[test]
fn removing_a_merge_can_be_taken_back_and_work_is_asked_about() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("remove");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    b.set_source(Source::All);
    b.apply_filter();
    b.selection.clear();
    b.active = Some(lib.bracket[1]);
    let rows = b.hdr_candidates(None);
    let req = crate::hdr_ui::request(rows).unwrap();
    let p = b.prepare_merge(crate::hdr_ui::Request { stack: true, keyword: String::new(), ..req });
    let (c, change, key, _) = create_merge(&lib.db, &p).unwrap();
    b.reload_now();
    let stack: i64 = lib.db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [c.image], |r| r.get(0)).unwrap();
    let there = |db: &Db| library::merge::recipe(&db.conn, c.file).is_some();
    let in_stack = |db: &Db| db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [c.image], |r| r.get::<_, i64>(0)).ok();

    // 1. nothing of its own: removed at once, one ⌘Z step
    assert!(b.merge_work_lines(c.file).is_empty());
    let mut actions = Vec::new();
    let steps = b.manage.undo.len();
    b.remove_merge(c.file, &mut actions);
    assert!(b.manage.ask.is_none(), "no question for a photo that can be made again as it is");
    let said = settle(&mut b, &ctx);
    assert!(!there(&lib.db), "{}", said.iter().map(|a| match a { Action::Status(t) => t.clone(), _ => String::new() }).collect::<Vec<_>>().join(" | "));
    assert_eq!(b.manage.undo.len(), steps + 1);
    assert!(matches!(b.manage.undo.last(), Some(library_manage::Undo::MergeRemoved { files, .. }) if files == &[c.file]));
    assert!(b.manage_undo());
    settle(&mut b, &ctx);
    assert!(there(&lib.db), "⌘Z brings the photo back");
    assert_eq!(in_stack(&lib.db), Some(stack), "on its stack");
    assert_eq!(b.manage.undo.len(), steps);
    assert!(b.rows.iter().any(|z| z.file == c.file && z.status as i64 == library::merge::STATUS_VIRTUAL));

    // 2. the automatic tone alone is no work of the user; a slider moved by hand is
    let tone = crate::Settings { exposure: 0.4, shadow: 12.0, ..Default::default() };
    let applied = crate::hdr_auto::tone_of(&tone);
    let recipe = hdr::Settings { reference: Some(1), auto_tone: Some(hdr::AutoTone { strength: 0.5, applied: Some(applied) }), ..Default::default() };
    library::merge::set_settings(&lib.db, c.file, &serde_json::to_string(&recipe).unwrap()).unwrap();
    let store = |s: &crate::Settings| {
        lib.db.conn.execute("DELETE FROM develop_state WHERE file = ?1", [c.file]).unwrap();
        lib.db.conn.execute("INSERT INTO develop_state(file, copy, settings, settings_time) VALUES(?1, '', ?2, 5)", (c.file, serde_json::to_string(s).unwrap())).unwrap();
    };
    store(&tone);
    assert!(b.merge_work_lines(c.file).is_empty(), "{:?}", b.merge_work_lines(c.file));
    let by_hand = crate::Settings { contrast: 0.3, ..tone.clone() };
    store(&by_hand);
    assert_eq!(b.merge_work_lines(c.file).len(), 1);
    b.remove_merge(c.file, &mut actions);
    assert!(matches!(&b.manage.ask, Some(library_manage::Ask::RemoveMerge { files, undo: false, .. }) if files == &[c.file]));
    settle(&mut b, &ctx);
    assert!(there(&lib.db), "nothing is removed before the answer");
    // "Abbrechen"
    b.manage.ask = None;
    // the answer "HDR entfernen"
    b.remove_merge_now(vec![c.file]);
    settle(&mut b, &ctx);
    assert!(!there(&lib.db));
    assert!(b.manage_undo());
    settle(&mut b, &ctx);
    let stored: String = lib.db.conn.query_row("SELECT settings FROM develop_state WHERE file = ?1", [c.file], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<crate::Settings>(&stored).unwrap(), by_hand, "the development comes back");
    assert_eq!(library::merge::recipe(&lib.db.conn, c.file).unwrap().settings, serde_json::to_string(&recipe).unwrap());

    // 3. ⌘Z of "HDR zusammenfügen" for a photo worked on since: asked first, the step stays
    b.manage.push(library_manage::Undo::Merge { label: "HDR zusammenfügen".into(), file: c.file, key, change });
    let steps = b.manage.undo.len();
    assert!(b.manage_undo());
    assert!(matches!(&b.manage.ask, Some(library_manage::Ask::RemoveMerge { undo: true, .. })));
    settle(&mut b, &ctx);
    assert!(there(&lib.db));
    assert_eq!(b.manage.undo.len(), steps, "the step is still there");
    // confirmed: stack back and photo out in one go; one more ⌘Z brings the photo back
    b.manage.ask = None;
    b.manage.merge_undo_confirmed = vec![c.file];
    assert!(b.manage_undo());
    settle(&mut b, &ctx);
    assert!(!there(&lib.db));
    let s = library::series::get(&lib.db.conn, stack).unwrap().expect("the stack of the frames");
    assert_eq!(s.images, lib.bracket.to_vec(), "the stack as before the merge");
    assert!(matches!(b.manage.undo.last(), Some(library_manage::Undo::MergeRemoved { .. })));
    assert!(b.manage_undo());
    settle(&mut b, &ctx);
    assert!(there(&lib.db), "the photo worked on is not lost");
    let stored: String = lib.db.conn.query_row("SELECT settings FROM develop_state WHERE file = ?1", [c.file], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<crate::Settings>(&stored).unwrap(), by_hand);
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}

/// "Verschieben nach …" leaves merged photos (no file of their own) out and counts them.
#[test]
fn move_leaves_merged_photos_out() {
    assert_eq!(super::super::move_selection(&[(1, false), (2, true), (3, false)]), (vec![1, 3], 1));
    assert_eq!(super::super::move_selection(&[(2, true)]), (Vec::<i64>::new(), 1));
    assert_eq!(super::super::move_selection(&[]), (Vec::<i64>::new(), 0));
}

/// Tournament "Übernehmen" sets ratings and the cover of the series: one ⌘Z takes both back, whichever of the two
/// steps ⌘Z reaches first.
#[test]
fn ratings_and_cover_of_a_tournament_are_one_undo() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let lib = library("adopt");
    let ctx = egui::Context::default();
    let mut b = view(&ctx, &lib);
    let stack: i64 = lib.db.conn.query_row("SELECT stack FROM stack_image WHERE image = ?1", [lib.bracket[0]], |r| r.get(0)).unwrap();
    let cover = |db: &Db| library::series::get(&db.conn, stack).unwrap().unwrap().top();
    let flag = |db: &Db| db.conn.query_row("SELECT flag FROM image WHERE id = ?1", [lib.bracket[2]], |r| r.get::<_, i64>(0)).unwrap();
    let before = cover(&lib.db);
    assert_ne!(before, lib.bracket[2]);
    for from_rating in [false, true] {
        let steps = b.manage.undo.len();
        assert!(!b.rate_each(&[(lib.bracket[2], None, Some(1))]).is_empty());
        let rated = b.rating_stamp();
        b.set_cover_with_rating(stack, lib.bracket[2], rated);
        settle(&mut b, &ctx);
        assert_eq!(cover(&lib.db), lib.bracket[2]);
        assert_eq!(b.manage.undo.len(), steps + 1);
        // one ⌘Z: from the ratings, or from the library step (the newer of the two)
        if from_rating {
            assert!(b.undo_rating_change().is_some());
        } else {
            assert!(b.manage_undo());
        }
        settle(&mut b, &ctx);
        assert_eq!(cover(&lib.db), before, "the cover is back (from the rating step: {from_rating})");
        assert_eq!(b.manage.undo.len(), steps, "no second step left");
        assert_eq!(b.rating_stamp(), None, "the rating step is gone as well");
        let t = std::time::Instant::now();
        while flag(&lib.db) != 0 {
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "the pick flag is not taken back");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    drop(b);
    let _ = std::fs::remove_dir_all(&lib.dir);
}
