//! The shown list (`visible`) must never point past `rows`: a viewed folder that refreshes to nothing or is closed,
//! a reload that comes back empty (crash report 1.4.2-test.1: "index out of bounds: the len is 0 but the index is 0"
//! in the grid after a memory warning), and any sequence of such replacements mixed with drawing, selecting, stepping
//! and filtering.
//!
//! Needs `GRAINEO_DATA_DIR`. Synthetic photos.

use super::*;
use std::sync::Arc;

const SCREEN: (f32, f32) = (620.0, 900.0);

fn library() -> (egui::Context, LibraryView) {
    let ctx = egui::Context::default();
    crate::lr::apply_theme(&ctx);
    let mut b = LibraryView::new(&ctx);
    b.centre_width = 600.0;
    b.cell_size = 150.0;
    (ctx, b)
}

fn draw(ctx: &egui::Context, b: &mut LibraryView, t: &mut f64) {
    *t += 0.05;
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN.0, SCREEN.1));
    let _ = crate::ui_test::run(ctx, egui::RawInput { screen_rect: Some(screen), time: Some(*t), ..Default::default() }, |root| {
        egui::CentralPanel::default().show(root, |ui| {
            let mut actions = Vec::new();
            b.raster(ui, &mut actions);
        });
    });
}

fn synthetic(ids: impl IntoIterator<Item = i64>, folder: i64) -> Vec<Row> {
    ids.into_iter().map(|k| Row { image: k, file: k, folder, name: format!("p{k}.png"), format: "PNG".into(), capture: Some(format!("2026-11-01T10:00:{:02}", k % 60)), ..Row::default() }).collect()
}

fn invariant(b: &LibraryView, what: &str) {
    assert!(b.visible.iter().all(|&i| i < b.rows.len()), "{what}: visible {:?} past {} rows", b.visible, b.rows.len());
    assert!(b.sem_from.is_none_or(|n| n <= b.visible.len()), "{what}: sem_from past visible");
    assert!(b.index.iter().all(|(&image, &i)| b.rows.get(i).is_some_and(|z| z.image == image)), "{what}: index out of step with rows");
}

/// The crash report: a viewed folder with photos, then its rows go away (refresh to an empty listing, closing) while the
/// grid still has the old list; the next frame must not index past the end.
#[test]
fn viewed_folder_rows_vanish_before_the_next_filter_run() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some(), "UI tests need their own data folder");
    let _app_lock = crate::ui_test::app_lock();
    let root = std::env::temp_dir().join(format!("graineo-rows-vanish-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for k in 0..6u8 {
        image::RgbImage::from_pixel(24, 16, image::Rgb([60 + k, 100, 80])).save(root.join(format!("p{k}.png"))).unwrap();
    }
    let (ctx, mut b) = library();
    let mut t = 0.0;
    b.browse_folder(&root, None);
    draw(&ctx, &mut b, &mut t);
    b.apply_filter();
    assert_eq!(b.visible.len(), 6);
    draw(&ctx, &mut b, &mut t);
    invariant(&b, "start");

    // refresh that finds an empty folder: rows replaced, filter not yet run (it may wait for the column filters)
    b.insert_viewed(&root, Vec::new());
    invariant(&b, "after an empty refresh");
    assert!(b.visible.is_empty());
    draw(&ctx, &mut b, &mut t);
    b.apply_filter();
    draw(&ctx, &mut b, &mut t);

    // photos back, then the folder is closed
    b.insert_viewed(&root, synthetic(100..105, VIEW_FOLDER));
    b.apply_filter();
    draw(&ctx, &mut b, &mut t);
    assert_eq!(b.visible.len(), 5);
    b.close_view_folder();
    invariant(&b, "after closing the viewed folder");
    draw(&ctx, &mut b, &mut t);
    let _ = std::fs::remove_dir_all(&root);
}

/// A reload that comes back empty keeps the shown list in step (the load is taken over like in the app).
#[test]
fn empty_reload_leaves_no_stale_indices() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let _app_lock = crate::ui_test::app_lock();
    let (ctx, mut b) = library();
    let mut t = 0.0;
    b.source = Source::All;
    b.filter = Default::default();
    b.rows = Arc::new(synthetic(1..=9, 1));
    b.index = b.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
    b.refilter = true;
    b.apply_filter();
    assert_eq!(b.visible.len(), 9);
    draw(&ctx, &mut b, &mut t);
    b.edit_rows(|r| r.clear());
    invariant(&b, "rows emptied");
    draw(&ctx, &mut b, &mut t);
    // the safety net: a path that forgot to re-point the list is repaired in the next frame, not a crash
    b.rows = Arc::new(synthetic(1..=9, 1));
    b.index = b.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
    b.visible = (0..9).collect();
    b.sem_from = Some(9);
    b.rows = Arc::new(Vec::new());
    b.index.clear();
    draw(&ctx, &mut b, &mut t);
    invariant(&b, "healed");
    assert!(b.visible.is_empty() && b.refilter);
}

/// Any order of "replace rows (empty, smaller, larger, other photos)" and "draw, select, step, filter" survives.
#[test]
fn random_sequences_of_replacing_rows_and_using_the_grid() {
    assert!(crate::env::var_os("GRAINEO_DATA_DIR").is_some());
    let _app_lock = crate::ui_test::app_lock();
    let (ctx, mut b) = library();
    let mut t = 0.0;
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    b.source = Source::All;
    b.filter = Default::default();
    b.rows = Arc::new(synthetic(1..=12, 1));
    b.index = b.rows.iter().enumerate().map(|(i, z)| (z.image, i)).collect();
    b.refilter = true;
    b.apply_filter();
    for step in 0..400 {
        let op = next(10);
        match op {
            0 => {
                let (from, n) = (next(40) as i64, next(30) as i64);
                b.edit_rows(|r| *r = synthetic(from..from + n, 1));
            }
            1 => b.edit_rows(|r| r.clear()),
            2 => b.edit_rows(|r| r.truncate(next(8) as usize)),
            3 => {
                let n = next(15) as i64;
                b.edit_rows(|r| r.extend(synthetic(50..50 + n, 1)));
            }
            4 => b.apply_filter(),
            5 => b.select_all(),
            6 => b.step(if next(2) == 0 { 1 } else { -1 }),
            7 => b.extend_external(if next(2) == 0 { 1 } else { -1 }),
            8 => b.refilter = true,
            _ => {
                let a = b.visible.first().map(|&i| b.rows[i].image);
                b.active = a;
            }
        }
        invariant(&b, &format!("step {step} op {op}"));
        if next(3) == 0 {
            draw(&ctx, &mut b, &mut t);
        }
    }
}
