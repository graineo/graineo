//! "Sicherung ansehen" in the app (logic: `library::backup_view`): the fixed band at the top while a backup is viewed,
//! starting graineo again into the view or back to the library, and "Bearbeitung von hier übernehmen" in Info.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eframe::egui;
use egui::Color32;
use library::backup_view::{self, Difference, Undo};

use crate::lr;

/// Background of the band: dark amber, clearly not the library (gold stays for main buttons).
const BAND: Color32 = Color32::from_rgb(0x3d, 0x2f, 0x12);
const BAND_TEXT: Color32 = Color32::from_rgb(0xf0, 0xdc, 0xa8);

/// Start graineo again – into the view of `backup` or (`None`) back to the library – and close this window. The new
/// process waits until this one has ended; going back removes the copy at its start.
pub fn restart(ctx: &egui::Context, backup: Option<&Path>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(exe);
    for (k, v) in backup_view::restart_env(backup) {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    c.env("GRAINEO_NO_SPLASH", "1");
    c.spawn().map_err(|e| e.to_string())?;
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    Ok(())
}

/// "2026-11-14" → "14.11.2026".
fn date(d: &str) -> String {
    let p: Vec<&str> = d.split('-').collect();
    if p.len() == 3 { format!("{}.{}.{}", p[2], p[1], p[0]) } else { d.to_string() }
}

/// Open question "restore this backup?" from the band.
static RESTORE_ASK: Mutex<bool> = Mutex::new(false);
/// Photos of the backup the library no longer has (counted once in the background).
static MISSING: Mutex<Option<Option<usize>>> = Mutex::new(None);
static MESSAGE: Mutex<String> = Mutex::new(String::new());

/// The band at the very top while a backup is viewed (nothing otherwise). Also draws the dialogs of the view.
pub fn band(ui: &mut egui::Ui) {
    let Some(v) = backup_view::active() else { return };
    let ctx = ui.ctx().clone();
    {
        let mut m = MISSING.lock().unwrap_or_else(|e| e.into_inner());
        if m.is_none() {
            *m = Some(None);
            let (copy, real) = (v.copy.clone(), library::real_db_path());
            let c2 = ctx.clone();
            std::thread::spawn(move || {
                library::platform::background_priority();
                let n = backup_view::missing_in_library(&copy, &real).map(|s| s.len()).ok();
                *MISSING.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
                c2.request_repaint();
            });
        }
    }
    let missing = MISSING.lock().unwrap_or_else(|e| e.into_inner()).flatten();
    let frame = egui::Frame::new().fill(BAND).inner_margin(egui::Margin::symmetric(12, 0));
    egui::Panel::top("sicherung-band").show_separator_line(false).resizable(false).exact_size(32.0).frame(frame).show(ui, |ui| {
        ui.horizontal_centered(|ui| {
            lr::paint_icon(ui.painter(), egui::Rect::from_center_size(ui.cursor().min + egui::vec2(7.0, ui.available_height() / 2.0), egui::vec2(13.0, 13.0)), lr::Icon::Info, BAND_TEXT);
            ui.add_space(20.0);
            ui.label(egui::RichText::new(crate::trf!("Du siehst die Sicherung vom {d}", d = date(&v.date))).strong().color(BAND_TEXT));
            ui.label(egui::RichText::new("·").color(BAND_TEXT));
            ui.label(egui::RichText::new(crate::tr("Nur zum Ansehen – Änderungen hier werden verworfen")).color(BAND_TEXT));
            if let Some(n) = missing.filter(|n| *n > 0) {
                ui.label(egui::RichText::new("·").color(BAND_TEXT));
                ui.label(egui::RichText::new(crate::trf!("{n} Fotos daraus gibt es in der Bibliothek nicht mehr", n = n)).color(BAND_TEXT));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.button(crate::tr("Diese Sicherung wiederherstellen …"));
                lr::hover_tip(ui, &r, "Bibliothek durch diese Sicherung ersetzen (die jetzige bleibt daneben erhalten)");
                if r.clicked() {
                    *RESTORE_ASK.lock().unwrap_or_else(|e| e.into_inner()) = true;
                }
                let r = ui.button(crate::tr("Zurück zur Bibliothek"));
                lr::hover_tip(ui, &r, "Ansicht schließen – graineo startet neu mit deiner Bibliothek");
                if r.clicked() {
                    if let Err(e) = restart(&ctx, None) {
                        *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = e;
                    }
                }
                let m = MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if !m.is_empty() {
                    ui.label(egui::RichText::new(crate::menu::i18n::tr_msg(&m)).small().color(BAND_TEXT));
                }
            });
        });
    });
    restore_dialog(&ctx, v);
    take_over_dialog(&ctx, v);
}

fn restore_dialog(ctx: &egui::Context, v: &backup_view::View) {
    if !*RESTORE_ASK.lock().unwrap_or_else(|e| e.into_inner()) {
        return;
    }
    let answer = crate::dialog::show(
        ctx,
        "ansicht-wiederherstellen",
        crate::tr("Bibliothek wiederherstellen?"),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::chip(ui, &date(&v.date));
            crate::dialog::text(ui, crate::tr("graineo startet neu und ersetzt die Bibliothek durch diese Sicherung. Änderungen seit der Sicherung (Bewertungen, Stichwörter, Entwicklungen) fehlen danach."));
            crate::dialog::hint(ui, crate::tr("Die jetzige Bibliothek bleibt daneben erhalten (library-before-restore-….sqlite)."));
        },
        crate::dialog::Buttons::confirm(crate::tr("Wiederherstellen und neu starten"), true),
    );
    match answer {
        crate::dialog::Answer::Open => {}
        crate::dialog::Answer::Cancel => *RESTORE_ASK.lock().unwrap_or_else(|e| e.into_inner()) = false,
        crate::dialog::Answer::Primary => {
            *RESTORE_ASK.lock().unwrap_or_else(|e| e.into_inner()) = false;
            // the restore itself happens at the start of the normal library (before anything opens it)
            let r = library::backup::schedule_restore(&library::real_db_path(), &v.backup).and_then(|_| restart(ctx, None));
            if let Err(e) = r {
                *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = crate::trf!("Sicherung nicht verwendbar: {e}", e = e);
            }
        }
    }
}

// ─────────────────────────────────────────────── take over the edit of one photo

/// The photo shown in Info and whether the library still has it (checked once per photo).
static CHECKED: Mutex<Option<(PathBuf, Option<String>, Option<bool>)>> = Mutex::new(None);

struct TakeOver {
    photo: PathBuf,
    copy: Option<String>,
    diff: Option<Result<Difference, String>>,
    with_meta: bool,
}

static TAKE: Mutex<Option<TakeOver>> = Mutex::new(None);
/// The last take-over (for "Rückgängig") and the message about it.
static LAST: Mutex<Option<Undo>> = Mutex::new(None);
static TAKE_MESSAGE: Mutex<String> = Mutex::new(String::new());

/// Section in Info while a backup is viewed: "Bearbeitung von hier übernehmen …" for the shown photo, or the note that
/// the library no longer has it.
pub fn info_section(ui: &mut egui::Ui, photo: &Path, copy: Option<&str>) {
    if backup_view::active().is_none() {
        return;
    }
    let state = {
        let mut c = CHECKED.lock().unwrap_or_else(|e| e.into_inner());
        if c.as_ref().is_none_or(|(p, k, _)| p != photo || k.as_deref() != copy) {
            *c = Some((photo.to_path_buf(), copy.map(str::to_string), None));
            let (p, k, ctx) = (photo.to_path_buf(), copy.map(str::to_string), ui.ctx().clone());
            std::thread::spawn(move || {
                let there = backup_view::in_library(&library::real_db_path(), &p);
                let mut c = CHECKED.lock().unwrap_or_else(|e| e.into_inner());
                if c.as_ref().is_some_and(|(q, kk, _)| *q == p && *kk == k) {
                    *c = Some((p, k, Some(there)));
                }
                ctx.request_repaint();
            });
        }
        c.as_ref().and_then(|x| x.2)
    };
    lr::panel(ui, |ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(crate::tr("AUS DER SICHERUNG")).font(lr::semibold(11.0)).color(lr::TEXT_DIM));
        ui.add_space(2.0);
        match state {
            None => crate::dialog::pending(ui, crate::tr("Wird mit der Bibliothek verglichen …")),
            Some(false) => crate::dialog::warning(ui, crate::tr("Dieses Foto gibt es in der Bibliothek nicht mehr – hier nur zum Ansehen.")),
            Some(true) => {
                // test hook for screenshots (own data folder only): GRAINEO_DIALOG=backup-take-over opens the dialog once
                static HOOK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if crate::env::var_os("GRAINEO_DATA_DIR").is_some() && crate::env::var("GRAINEO_DIALOG").as_deref() == Ok("backup-take-over") && !HOOK.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    open_take_over(ui.ctx(), photo, copy);
                }
                let r = ui.button(crate::tr("Bearbeitung von hier übernehmen …"));
                lr::hover_tip(ui, &r, "Entwicklung dieses Fotos aus der Sicherung in die Bibliothek schreiben");
                if r.clicked() {
                    open_take_over(ui.ctx(), photo, copy);
                }
            }
        }
        let m = TAKE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !m.is_empty() {
            crate::dialog::hint(ui, &m);
            if LAST.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                let r = ui.button(crate::tr("Rückgängig"));
                lr::hover_tip(ui, &r, "Die Bibliothek bekommt den Stand vor dem Übernehmen zurück");
                if r.clicked() {
                    undo_last();
                }
            }
        }
        ui.add_space(4.0);
    });
}

fn open_take_over(ctx: &egui::Context, photo: &Path, copy: Option<&str>) {
    let Some(v) = backup_view::active() else { return };
    *TAKE.lock().unwrap_or_else(|e| e.into_inner()) = Some(TakeOver { photo: photo.to_path_buf(), copy: copy.map(str::to_string), diff: None, with_meta: false });
    let (view, p, k, ctx) = (v.copy.clone(), photo.to_path_buf(), copy.map(str::to_string), ctx.clone());
    std::thread::spawn(move || {
        let d = backup_view::difference(&view, &library::real_db_path(), &p, k.as_deref());
        if let Some(t) = TAKE.lock().unwrap_or_else(|e| e.into_inner()).as_mut().filter(|t| t.photo == p) {
            t.diff = Some(d);
        }
        ctx.request_repaint();
    });
}

fn take_over_dialog(ctx: &egui::Context, v: &backup_view::View) {
    let mut guard = TAKE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(t) = guard.as_mut() else { return };
    let name = t.photo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let name = match &t.copy {
        Some(c) => format!("{name} · {c}"),
        None => name,
    };
    let ready = matches!(&t.diff, Some(Ok(d)) if d.image_in_library);
    let answer = crate::dialog::show(
        ctx,
        "ansicht-uebernehmen",
        crate::tr("Bearbeitung übernehmen?"),
        crate::dialog::WIDTH,
        |ui| {
            crate::dialog::chip(ui, &crate::trf!("{name} · Sicherung vom {d}", name = name, d = date(&v.date)));
            crate::dialog::text(ui, crate::tr("Die Entwicklung dieses Fotos aus der Sicherung ersetzt die jetzige in der Bibliothek. Alle anderen Fotos bleiben, wie sie sind."));
            match &t.diff {
                None => crate::dialog::pending(ui, crate::tr("Wird mit der Bibliothek verglichen …")),
                Some(Err(e)) => crate::dialog::warning(ui, e),
                Some(Ok(d)) if !d.in_library => crate::dialog::warning(ui, crate::tr("Dieses Foto gibt es in der Bibliothek nicht mehr.")),
                Some(Ok(d)) if !d.image_in_library => crate::dialog::warning(ui, crate::tr("Diese virtuelle Kopie gibt es in der Bibliothek nicht mehr.")),
                Some(Ok(d)) => {
                    let same = |b: bool| if b { crate::tr("anders als jetzt") } else { crate::tr("gleich") };
                    crate::dialog::hint(ui, &crate::trf!("Entwicklung: {a} · Bewertung: {b} · Stichwörter: {c}", a = same(d.develop), b = same(d.rating), c = same(d.keywords)));
                    crate::dialog::options(ui, |ui| {
                        crate::dialog::option(ui, &mut t.with_meta, "Auch Sterne, Markierung, Etikett und Stichwörter", "Personen bleiben, wie sie sind.", None);
                    });
                }
            }
            crate::dialog::hint(ui, crate::tr("Lässt sich danach unter Info mit „Rückgängig“ zurücknehmen."));
        },
        crate::dialog::Buttons::confirm(crate::tr("Übernehmen"), ready),
    );
    match answer {
        crate::dialog::Answer::Open => {}
        crate::dialog::Answer::Cancel => *guard = None,
        crate::dialog::Answer::Primary => {
            let t = guard.take().expect("dialog open");
            drop(guard);
            let view = v.copy.clone();
            std::thread::spawn(move || {
                let r = backup_view::take_over(&view, &library::real_db_path(), &t.photo, t.copy.as_deref(), t.with_meta);
                let m = match r {
                    Ok(u) => {
                        *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(u);
                        crate::tr("In die Bibliothek übernommen – sichtbar, sobald du zurück bist.").to_string()
                    }
                    Err(e) => crate::trf!("Nicht übernommen: {e}", e = e),
                };
                *TAKE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = m;
            });
        }
    }
}

fn undo_last() {
    let Some(u) = LAST.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    std::thread::spawn(move || {
        let m = match backup_view::undo_take_over(&library::real_db_path(), &u) {
            Ok(()) => crate::tr("Rückgängig gemacht – die Bibliothek hat wieder den Stand von vorher.").to_string(),
            Err(e) => crate::trf!("Nicht rückgängig gemacht: {e}", e = e),
        };
        *TAKE_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = m;
    });
}
