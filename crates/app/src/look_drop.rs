//! Looks dropped on the window: first the app checks in the background what the drop really holds (valid LUTs, look
//! profiles; never the sidecars of photos), then asks ("3 Looks hinzufügen?" with the list), then says what happened
//! in the same dialog ("3 Looks hinzugefügt", "Looks ansehen" / "Fertig"). A drop without looks gets a short hint at
//! the bottom of the window instead; the settings never open by themselves. Submodule of `film_profiles_ui`.

use super::{work, SERIAL};
use crate::{dialog, lr};
use eframe::egui;
use raw::film_profile::{self, FoundLooks, Report, Storage};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// How long the short hint stays (seconds).
const HINT_SECS: f64 = 3.0;
/// Rows of the look list before it scrolls.
const LIST_H: f32 = 240.0;

/// What the dialog shows once the looks have been added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Added {
    pub added: usize,
    pub existing: usize,
    pub skipped: usize,
    pub messages: Vec<String>,
}

enum Stage {
    /// the drop is being looked at (no dialog yet: it takes a moment at most)
    Reading,
    Ask(FoundLooks),
    Adding(FoundLooks),
    Done(Added),
    /// short hint at the bottom of the window: title, detail, shown until (egui time; `None`: from the next frame)
    Hint(String, String, Option<f64>),
}

static STAGE: Mutex<Option<Stage>> = Mutex::new(None);

fn set(s: Option<Stage>) {
    *STAGE.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

/// Could this be a look at all (a `.xmp`/`.cube` file, or a folder)? The real check runs in the background.
fn maybe_look(p: &Path) -> bool {
    p.is_dir() || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp") || e.eq_ignore_ascii_case("cube"))
}

/// Files and folders dropped on the window that were neither photos nor taken by anyone else.
pub fn dropped(ctx: &egui::Context, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    // a dialog of an earlier drop is still open: it stays (one at a time)
    if STAGE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| !matches!(s, Stage::Hint(..))) {
        return;
    }
    if !paths.iter().any(|p| maybe_look(p)) {
        set(Some(Stage::Hint(crate::tr("Keine Fotos oder Looks erkannt").to_string(), String::new(), None)));
        ctx.request_repaint();
        return;
    }
    set(Some(Stage::Reading));
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("look-drop".into())
        .spawn(move || {
            // whatever happens while reading (also a panic): the state never stays "Reading", or later drops would be
            // ignored
            let found = std::panic::catch_unwind(|| film_profile::detect_looks(&paths));
            set(Some(match found {
                Ok(found) => {
                    crate::log(&format!("look drop: {} looks, {} sidecars, {} rejected in {} items", found.looks.len(), found.sidecars, found.rejected.len(), paths.len()));
                    if found.looks.is_empty() {
                        Stage::Hint(crate::tr("Keine Looks erkannt").to_string(), String::new(), None)
                    } else {
                        Stage::Ask(found)
                    }
                }
                Err(_) => {
                    crate::log("look drop: reading the dropped files failed");
                    Stage::Hint(crate::tr("Keine Looks erkannt").to_string(), String::new(), None)
                }
            }));
            ctx.request_repaint();
        });
    if spawned.is_err() {
        set(None);
    }
}

/// Add the references of the drop and count what came in (by identifier, against the looks known before).
pub(super) fn add(storage: &Storage, found: &FoundLooks) -> Added {
    let known = |v: &[film_profile::FilmProfile]| -> HashSet<String> { v.iter().filter(|p| p.available).map(|p| p.uuid.clone()).collect() };
    let before = known(&storage.search());
    let report: Report = storage.add_folder(&found.references);
    let after = known(&storage.search());
    let mut a = Added { messages: report.messages, ..Default::default() };
    for l in &found.looks {
        if before.contains(&l.profile.uuid) {
            a.existing += 1;
        } else if after.contains(&l.profile.uuid) {
            a.added += 1;
        } else {
            a.skipped += 1;
        }
    }
    a.skipped += found.rejected.len();
    a
}

fn ask_body(ui: &mut egui::Ui, found: &FoundLooks) {
    egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().id_salt("look-drop-list").max_height(LIST_H).auto_shrink([false, true]).show(ui, |ui| {
            for l in &found.looks {
                let r = ui.add(egui::Label::new(egui::RichText::new(&l.profile.name).color(lr::TEXT)).truncate());
                lr::hover_tip(ui, &r, &l.profile.path.display().to_string());
            }
        });
    });
}

/// Once per frame (from [`super::frame`]): the dialog of a drop, or the short hint.
pub(super) fn frame(ctx: &egui::Context) {
    let mut g = STAGE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stage) = g.as_mut() else { return };
    match stage {
        Stage::Reading | Stage::Adding(_) => {
            // a moment at most; nothing is shown meanwhile except while adding (the dialog keeps its place)
            if let Stage::Adding(found) = stage {
                let n = found.looks.len();
                let _ = dialog::show(ctx, "look-drop", &crate::trf!("{a} hinzufügen?", a = dialog::count(n, crate::tr("Look"), crate::tr("Looks"))), 420.0, |ui| dialog::pending(ui, crate::tr("Wird hinzugefügt …")), dialog::Buttons::confirm("Hinzufügen", false));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        Stage::Hint(title, detail, until) => {
            let now = ctx.input(|i| i.time);
            let end = *until.get_or_insert(now + HINT_SECS);
            if now > end {
                *g = None;
                return;
            }
            hint(ctx, title, detail);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((end - now).max(0.05)));
        }
        Stage::Ask(found) => {
            let n = found.looks.len();
            let title = crate::trf!("{a} hinzufügen?", a = dialog::count(n, crate::tr("Look"), crate::tr("Looks")));
            let answer = dialog::show(ctx, "look-drop", &title, 420.0, |ui| ask_body(ui, found), dialog::Buttons::confirm("Hinzufügen", true));
            let answer = test_answer(ctx, answer, "ask");
            match answer {
                dialog::Answer::Primary => {
                    let found = found.clone();
                    *g = Some(Stage::Adding(found.clone()));
                    let ctx = ctx.clone();
                    std::thread::Builder::new()
                        .name("look-add".into())
                        .spawn(move || {
                            let added = {
                                let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
                                let a = add(&Storage::standard(), &found);
                                for m in &a.messages {
                                    crate::log(&format!("look drop: {m}"));
                                }
                                film_profile::reread();
                                a
                            };
                            set(Some(Stage::Done(added)));
                            // the look list and the settings page read the sources again
                            work(&ctx, |_| Report::default());
                            ctx.request_repaint();
                        })
                        .ok();
                }
                dialog::Answer::Cancel => *g = None,
                dialog::Answer::Open => {}
            }
        }
        Stage::Done(a) => {
            let title = if a.added > 0 { crate::trf!("{a} hinzugefügt", a = dialog::count(a.added, crate::tr("Look"), crate::tr("Looks"))) } else { crate::tr("Keine neuen Looks").to_string() };
            let (answer, show) = dialog::show_extra(ctx, "look-drop", &title, 420.0, |_| {}, dialog::Buttons { cancel: "", primary: Some(("Fertig", dialog::Style::Normal, true)) }, Some("Looks ansehen"));
            let answer = test_answer(ctx, answer, "done");
            if show || test_show(ctx) {
                *g = None;
                drop(g);
                super::manage();
            } else if answer != dialog::Answer::Open {
                *g = None;
            }
        }
    }
}

/// The short hint: two lines at the bottom centre of the window, above everything, without a click target.
fn hint(ctx: &egui::Context, title: &str, detail: &str) {
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::unique("look-drop-hint"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(screen.center().x, screen.bottom() - 60.0))
        .show(ctx, |ui| {
            egui::Frame::new().fill(egui::Color32::from_gray(0x24)).stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(0x44))).inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
                ui.set_max_width(460.0);
                ui.label(egui::RichText::new(title).strong().color(lr::TEXT));
                if !detail.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(detail).small().color(lr::TEXT_DIM)).wrap());
                }
            });
        });
}

/// Test hook (only with GRAINEO_DATA_DIR): GRAINEO_LOOK_DROP_YES=ask|done|both confirms the question and/or the
/// result after one second (screenshots of each step).
fn test_answer(ctx: &egui::Context, answer: dialog::Answer, step: &str) -> dialog::Answer {
    if answer != dialog::Answer::Open || crate::env::var_os("GRAINEO_DATA_DIR").is_none() {
        return answer;
    }
    let Ok(v) = crate::env::var("GRAINEO_LOOK_DROP_YES") else { return answer };
    if v != step && v != "both" {
        return answer;
    }
    let id = egui::Id::unique(("look-drop-test", step));
    let now = ctx.input(|i| i.time);
    let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
    if now - start > 1.0 { dialog::Answer::Primary } else { answer }
}

/// Test hook: GRAINEO_LOOK_DROP_SHOW=1 presses "Looks ansehen" in the result after one second.
fn test_show(ctx: &egui::Context) -> bool {
    if crate::env::var_os("GRAINEO_DATA_DIR").is_none() || crate::env::var("GRAINEO_LOOK_DROP_SHOW").as_deref() != Ok("1") {
        return false;
    }
    let id = egui::Id::unique("look-drop-test-show");
    let now = ctx.input(|i| i.time);
    let start = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
    now - start > 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("graineo-look-drop-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cube(n: usize) -> String {
        let mut t = format!("LUT_3D_SIZE {n}\n");
        for i in 0..n * n * n {
            let v = (i % n) as f32 / (n - 1) as f32;
            t += &format!("{v} {v} {v}\n");
        }
        t
    }

    /// The counts of the result: new looks, looks already known, files that could not be used.
    #[test]
    fn adding_counts_new_existing_and_skipped() {
        let root = scratch("add");
        let storage = Storage { data: root.join("data"), cache: root.join("cache"), automatic: Vec::new() };
        let looks = root.join("looks");
        std::fs::create_dir_all(&looks).unwrap();
        std::fs::write(looks.join("A.cube"), cube(17)).unwrap();
        std::fs::write(looks.join("B.cube"), cube(17).replacen("LUT_3D_SIZE 17", "TITLE \"b\"\nLUT_3D_SIZE 17", 1)).unwrap();
        std::fs::write(looks.join("Broken.cube"), "LUT_3D_SIZE 17\n0 0 0\n").unwrap();
        let found = film_profile::detect_looks(std::slice::from_ref(&looks));
        assert_eq!(found.looks.len(), 2);
        assert_eq!(found.rejected.len(), 1);
        let a = add(&storage, &found);
        assert_eq!((a.added, a.existing, a.skipped), (2, 0, 1));
        // the same drop again: both known already
        let a = add(&storage, &found);
        assert_eq!((a.added, a.existing, a.skipped), (0, 2, 1));
        // a single file in another folder is referenced by itself
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("C.cube"), cube(17).replacen("LUT_3D_SIZE 17", "# c\nLUT_3D_SIZE 17", 1)).unwrap();
        std::fs::write(other.join("D.cube"), cube(17).replacen("LUT_3D_SIZE 17", "# d\nLUT_3D_SIZE 17", 1)).unwrap();
        let found = film_profile::detect_looks(&[other.join("C.cube")]);
        let a = add(&storage, &found);
        assert_eq!((a.added, a.existing, a.skipped), (1, 0, 0));
        assert!(!storage.search().iter().any(|p| p.name == "D"), "only the dropped file, not its folder");
    }

    /// A drop of photo sidecars only is no look: the hint, never the dialog or the settings.
    #[test]
    fn sidecars_only_are_no_looks() {
        let d = scratch("sidecars");
        std::fs::write(d.join("IMG_1.CR3"), b"raw").unwrap();
        std::fs::write(d.join("IMG_1.xmp"), "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF/></x:xmpmeta>").unwrap();
        let found = film_profile::detect_looks(&[d.join("IMG_1.xmp"), d.clone()]);
        assert!(found.looks.is_empty());
        assert_eq!(found.sidecars, 2);
        assert!(maybe_look(&d) && maybe_look(&d.join("IMG_1.xmp")) && !maybe_look(&d.join("IMG_1.CR3")));
    }
}
