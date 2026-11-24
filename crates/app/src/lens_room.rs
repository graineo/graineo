//! Window OBJEKTIVE: the lenses of the library (name, camera, number of photos, which profile counts) and the own
//! profiles with their details, history and actions (assign as default, rename, duplicate, export, import, move to
//! the Trash). Only own profiles (graineo measurements) can be changed; originals are never touched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use eframe::egui;
use egui::Color32;
use library::lens_use::LensRow;
use lensprof::assign::{self, Source};
use lensprof::own::RevisionAction;
use lensprof::{exchange, OwnProfile};

use crate::dialog::{self, Answer, Buttons};
use crate::lens_assign::{self, Choice, Identity};
use crate::lr;

const EXPORT: &str = "objektive-export";
const IMPORT: &str = "objektive-import";
const W: f32 = 940.0;
const H: f32 = 500.0;
const LIST_W: f32 = 340.0;

#[derive(Clone, PartialEq)]
enum Sel {
    Nothing,
    Profile(String),
    Lens(usize),
}

enum Ask {
    Trash { id: String, name: String, uses: usize },
}

struct Room {
    sel: Sel,
    filter: String,
    lenses: Option<Vec<LensRow>>,
    lenses_rx: Option<Receiver<Vec<LensRow>>>,
    usage: HashMap<String, usize>,
    usage_rx: Option<(String, Receiver<usize>)>,
    rename: Option<(String, String)>,
    ask: Option<Ask>,
    export_id: Option<String>,
    message: Option<(bool, String)>,
    pick: Choice,
    pick_for: Sel,
}

thread_local! {
    static Z: RefCell<Option<Room>> = const { RefCell::new(None) };
}

/// Open the window.
pub fn open(ctx: &egui::Context) {
    let (tx, rx) = channel();
    let c = ctx.clone();
    std::thread::Builder::new()
        .name("objektive-liste".into())
        .spawn(move || {
            let _ = tx.send(library::lens_use::lenses());
            c.request_repaint();
        })
        .ok();
    Z.with(|z| {
        *z.borrow_mut() = Some(Room {
            sel: Sel::Nothing,
            filter: String::new(),
            lenses: None,
            lenses_rx: Some(rx),
            usage: HashMap::new(),
            usage_rx: None,
            rename: None,
            ask: None,
            export_id: None,
            message: None,
            pick: Choice::Automatic,
            pick_for: Sel::Nothing,
        })
    });
    select_test();
}

/// Test hook: select the first own profile (`GRAINEO_LENSES_SELECT=profile`) or the first lens (`lens`).
pub fn select_test() {
    Z.with(|z| {
        if let (Some(r), Ok(w)) = (z.borrow_mut().as_mut(), crate::env::var("GRAINEO_LENSES_SELECT")) {
            match w.as_str() {
                "profile" => {
                    if let Some(p) = lensprof::store::list().first() {
                        r.sel = Sel::Profile(p.id.clone());
                    }
                }
                _ => r.sel = Sel::Lens(0),
            }
        }
    });
}

fn date(t: i64) -> String {
    use crate::menu::i18n;
    let l = library::platform::local_time(t);
    i18n::format::date(i18n::language(), l.year, l.month as u8, l.tag as u8).unwrap_or_default()
}

fn camera_identity(row: &LensRow) -> Identity {
    // the library often names cameras without the maker ("ILCE-7M3"): the maker stays open (wildcard)
    Identity::new(String::new(), row.camera.clone(), (!row.lens.is_empty()).then(|| row.lens.clone()))
}

fn lens_title(row: &LensRow) -> String {
    if row.lens.is_empty() { crate::tr("ohne Objektivangabe").to_string() } else { crate::cell_info::lens_label(&row.lens) }
}

/// Which profile counts for photos of this lens (without opening photos): rule, else own profile by lens name.
fn lens_state(row: &LensRow) -> (String, bool) {
    let id = camera_identity(row);
    match assign::source(&id.maker, &id.model, id.lens.as_deref(), None) {
        Source::Rule(r) if r.is_empty() => (crate::tr("Kein eigenes Profil").to_string(), true),
        Source::Rule(r) => (lens_assign::profile_label(&r), true),
        _ => match lensprof::store::find_auto(&id.maker, &id.model, id.lens.as_deref()) {
            Some(p) => (format!("{} · {}", p.name, crate::tr("nach Name")), true),
            None => (crate::tr("automatisch").to_string(), false),
        },
    }
}

fn profile_line(p: &OwnProfile) -> String {
    match &p.quality {
        Some(q) => format!("{}  v{} · {}", p.name, p.version, q.score),
        None => format!("{}  v{}", p.name, p.version),
    }
}

/// Two fixed, clipped lines keep long names inside the list column.
fn list_two_lines(ui: &mut egui::Ui, title: &str, detail: &str, active: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
    if active || response.hovered() {
        ui.painter().rect_filled(rect, 0.0, if active { lr::ACTIVE_FILL } else { Color32::from_gray(0x38) });
    }
    let x = rect.left() + 8.0;
    let width = (rect.width() - 16.0).max(0.0);
    let first = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 1.0), egui::vec2(width, 18.0));
    let second = egui::Rect::from_min_size(egui::pos2(x, rect.top() + 19.0), egui::vec2(width, 18.0));
    ui.put(first, egui::Label::new(egui::RichText::new(title).color(lr::TEXT())).truncate().selectable(false));
    ui.put(second, egui::Label::new(egui::RichText::new(detail).small().color(lr::TEXT_DIM())).truncate().selectable(false));
    lr::hover_tip(ui, &response, &format!("{title}\n{detail}"));
    response
}

fn profile_list_text(p: &OwnProfile) -> (String, String) {
    let name = p.name.trim();
    let (name, description) = match name.rsplit_once(" (") {
        Some((name, detail)) if detail.starts_with("graineo-Messung") && detail.ends_with(')') => (name, detail.trim_end_matches(')')),
        _ => (name, ""),
    };
    let mut parts = name.splitn(3, ' ');
    let first = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    let name = if !first.is_empty() && first.eq_ignore_ascii_case(second) {
        name[first.len()..].trim_start()
    } else { name };
    let version = match &p.quality {
        Some(q) => format!("v{} · {}", p.version, q.score),
        None => format!("v{}", p.version),
    };
    (name.to_owned(), if description.is_empty() { version } else { format!("{description} · {version}") })
}

fn action_text(a: &RevisionAction) -> String {
    match a {
        RevisionAction::Migrated => crate::tr("übernommen").to_string(),
        RevisionAction::Measured => crate::tr("gemessen").to_string(),
        RevisionAction::Extended => crate::tr("ergänzt").to_string(),
        RevisionAction::Restored { from } => crate::trf!("zurückgesetzt auf v{a}", a = from),
        RevisionAction::Imported => crate::tr("importiert").to_string(),
        RevisionAction::Duplicated { .. } => crate::tr("dupliziert").to_string(),
        RevisionAction::Renamed => crate::tr("umbenannt").to_string(),
    }
}

fn caption(ui: &mut egui::Ui, t: &str) {
    ui.label(egui::RichText::new(crate::tr(t).to_uppercase()).small().color(lr::TEXT_DIM()));
}

fn dim(ui: &mut egui::Ui, t: impl Into<String>) {
    ui.add(egui::Label::new(egui::RichText::new(t.into()).small().color(lr::TEXT_DIM())).wrap());
}

/// Draw the window every frame.
pub fn ui(ctx: &egui::Context) {
    Z.with(|zz| {
        let mut guard = zz.borrow_mut();
        let Some(r) = guard.as_mut() else { return };
        if let Some(rx) = &r.lenses_rx {
            if let Ok(v) = rx.try_recv() {
                r.lenses = Some(v);
                r.lenses_rx = None;
            }
        }
        if let Some((id, rx)) = &r.usage_rx {
            if let Ok(n) = rx.try_recv() {
                r.usage.insert(id.clone(), n);
                r.usage_rx = None;
            }
        }
        // results of the file dialogs
        if let Some(path) = crate::file_dialog::fetch_one(EXPORT) {
            if let Some(id) = r.export_id.take() {
                r.message = Some(match exchange::export(&id, &path) {
                    Ok(()) => (true, crate::trf!("Exportiert: {a}", a = path.display())),
                    Err(e) => (false, e.to_string()),
                });
            }
        }
        if let Some(path) = crate::file_dialog::fetch_one(IMPORT) {
            r.message = Some(match exchange::import(&path) {
                Ok(p) => {
                    r.sel = Sel::Profile(p.id);
                    (true, crate::tr("Importiert").to_string())
                }
                Err(e) => (false, crate::tr(&e).to_string()),
            });
        }
        // selected profile: how many photos use it (background, once per profile)
        if let Sel::Profile(id) = &r.sel {
            if !r.usage.contains_key(id) && r.usage_rx.is_none() {
                let (tx, rx) = channel();
                let (idc, c) = (id.clone(), ctx.clone());
                r.usage_rx = Some((id.clone(), rx));
                std::thread::Builder::new()
                    .name("objektive-nutzung".into())
                    .spawn(move || {
                        let _ = tx.send(library::lens_use::photos_using(&idc));
                        c.request_repaint();
                    })
                    .ok();
            }
        }
        let mut close = false;
        let mut add = false;
        let mut import = false;
        let frame = egui::Frame::new().fill(Color32::from_gray(0x1e)).stroke(egui::Stroke::new(1.0, Color32::from_gray(0x3a))).corner_radius(0.0).inner_margin(16.0);
        let modal = egui::Modal::new(egui::Id::unique(("dialog", "objektive"))).frame(frame).backdrop_color(Color32::from_black_alpha(150)).show(ctx, |ui| {
            ui.set_width(W);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(crate::tr("OBJEKTIVE")).font(lr::semibold(15.0)).color(Color32::from_gray(0xee))));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(crate::tr("Schließen")).min_size(egui::vec2(90.0, 26.0)).corner_radius(0.0).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, lr::FIELD_STROKE()))).clicked() {
                        close = true;
                    }
                    if lr::main_button_min(ui, crate::tr("Objektiv hinzufügen …"), true, 170.0).clicked() {
                        add = true;
                    }
                    if ui.add(egui::Button::new(crate::tr("Importieren …")).min_size(egui::vec2(100.0, 26.0)).corner_radius(0.0).fill(Color32::TRANSPARENT).stroke(egui::Stroke::new(1.0, lr::FIELD_STROKE()))).clicked() {
                        import = true;
                    }
                });
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                ui.allocate_ui_with_layout(egui::vec2(LIST_W, H), egui::Layout::top_down(egui::Align::Min), |ui| list(ui, r));
                let (line, _) = ui.allocate_exact_size(egui::vec2(1.0, H), egui::Sense::hover());
                ui.painter().rect_filled(line, 0.0, Color32::from_gray(0x3a));
                let w = ui.available_width();
                ui.allocate_ui_with_layout(egui::vec2(w, H), egui::Layout::top_down(egui::Align::Min), |ui| detail(ui, r));
            });
            ui.separator();
            // one fixed line for the result of the last action
            let (ok, text) = r.message.clone().unwrap_or((true, String::new()));
            ui.add_sized([W, 16.0], egui::Label::new(egui::RichText::new(text).small().color(if ok { lr::TEXT_DIM() } else { Color32::from_rgb(0xe0, 0x5c, 0x50) })).truncate());
        });
        let top = modal.is_top_modal;
        if top && (modal.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape))) && r.ask.is_none() && r.rename.is_none() {
            close = true;
        }
        if add {
            crate::lens_wizard::open();
        }
        if import {
            crate::file_dialog::file(ctx, IMPORT, |d| d.set_title(crate::tr("Objektivprofil importieren")).add_filter("graineo lens", &["graineo-lens", "json"]));
        }
        // confirm moving a profile to the Trash
        let mut done: Option<bool> = None;
        if let Some(Ask::Trash { id, name, uses }) = &r.ask {
            let a = dialog::show(
                ctx,
                "objektive-papierkorb",
                "Profil in den Papierkorb legen?",
                480.0,
                |ui| {
                    dialog::chip(ui, name);
                    if *uses > 0 {
                        dialog::text(ui, &crate::trf!("Diese {n} Fotos werden danach ohne dieses Profil gezeigt; es ist im Papierkorb wiederherstellbar.", n = uses));
                    } else {
                        dialog::text(ui, "Kein Foto nutzt dieses Profil zugewiesen. Es ist im Papierkorb wiederherstellbar.");
                    }
                    let _ = id;
                },
                Buttons::destructive("In den Papierkorb", true),
            );
            match a {
                Answer::Primary => done = Some(true),
                Answer::Cancel => done = Some(false),
                Answer::Open => {}
            }
        }
        if let Some(go) = done {
            if let (true, Some(Ask::Trash { id, name, .. })) = (go, r.ask.take()) {
                r.message = Some(match trash_profile(&id) {
                    Ok(()) => {
                        r.sel = Sel::Nothing;
                        (true, crate::trf!("„{a}“ liegt im Papierkorb", a = name))
                    }
                    Err(e) => (false, e),
                });
            } else {
                r.ask = None;
            }
        }
        if close {
            *guard = None;
        }
    });
}

/// Profile file and its snapshots into the Trash (restorable). Rules that point to it stay: the photos then show
/// "Profil fehlt" and nothing is rewritten.
fn trash_profile(id: &str) -> Result<(), String> {
    let path = lensprof::store::path_of(id).ok_or("kein gültiger Profilname")?;
    trash::delete(&path).map_err(|e| e.to_string())?;
    if let Some(h) = lensprof::history::dir(id).filter(|d| d.exists()) {
        let _ = trash::delete(&h);
    }
    lensprof::store::bump_generation();
    Ok(())
}

fn list(ui: &mut egui::Ui, r: &mut Room) {
    lr::edit_field(ui, egui::TextEdit::singleline(&mut r.filter).hint_text(crate::tr("Suchen …")).desired_width(LIST_W));
    let f = r.filter.trim().to_lowercase();
    egui::ScrollArea::vertical().id_salt("objektive-liste").max_height(H - 30.0).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(LIST_W - 8.0);
        caption(ui, "Eigene Profile");
        let profiles = lensprof::store::list();
        let shown: Vec<&Arc<OwnProfile>> = profiles.iter().filter(|p| f.is_empty() || p.name.to_lowercase().contains(&f) || p.model.to_lowercase().contains(&f)).collect();
        if shown.is_empty() {
            dim(ui, crate::tr("Noch keine eigenen Profile. „Objektiv hinzufügen …“ vermisst das erste."));
        }
        for p in shown {
            let on = r.sel == Sel::Profile(p.id.clone());
            let (title, detail) = profile_list_text(p);
            if list_two_lines(ui, &title, &detail, on).clicked() {
                r.sel = Sel::Profile(p.id.clone());
                r.rename = None;
            }
        }
        ui.add_space(8.0);
        caption(ui, "Objektive der Bibliothek");
        match &r.lenses {
            None => dim(ui, crate::tr("wird ermittelt …")),
            Some(rows) if rows.is_empty() => dim(ui, crate::tr("Die Bibliothek enthält keine Fotos mit Kameradaten.")),
            Some(rows) => {
                for (i, row) in rows.iter().enumerate() {
                    let title = lens_title(row);
                    if !f.is_empty() && !title.to_lowercase().contains(&f) && !row.camera.to_lowercase().contains(&f) {
                        continue;
                    }
                    let (state, special) = lens_state(row);
                    let mut detail = format!("{} · {}", row.camera, crate::trf!("{n} Fotos", n = row.photos));
                    if special { detail.push_str(&format!(" · {state}")); }
                    let on = r.sel == Sel::Lens(i);
                    let resp = list_two_lines(ui, &title, &detail, on);
                    if resp.clicked() {
                        r.sel = Sel::Lens(i);
                        r.rename = None;
                    }
                }
            }
        }
    });
}

fn detail(ui: &mut egui::Ui, r: &mut Room) {
    match r.sel.clone() {
        Sel::Nothing => {
            dim(ui, crate::tr("Wähle links ein Profil oder ein Objektiv."));
        }
        Sel::Profile(id) => profile_detail(ui, r, &id),
        Sel::Lens(i) => {
            let Some(row) = r.lenses.as_ref().and_then(|l| l.get(i)).cloned() else { return };
            lens_detail(ui, r, &row);
        }
    }
}

fn lens_detail(ui: &mut egui::Ui, r: &mut Room, row: &LensRow) {
    ui.add(egui::Label::new(egui::RichText::new(lens_title(row)).font(lr::semibold(14.0)).color(Color32::from_gray(0xee))).wrap());
    dim(ui, format!("{} · {}", row.camera, crate::trf!("{n} Fotos", n = row.photos)));
    let (state, _) = lens_state(row);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        caption(ui, "Profil für Fotos dieses Objektivs");
        crate::help::info(ui, "lens-default", "Profil für Fotos dieses Objektivs", &crate::help::LENS_DEFAULT);
    });
    dim(ui, state);
    ui.add_space(6.0);
    let id = camera_identity(row);
    let candidates = assign::candidates(&id.maker, &id.model);
    if r.pick_for != r.sel {
        r.pick = Choice::Automatic;
        r.pick_for = r.sel.clone();
    }
    ui.radio_value(&mut r.pick, Choice::Automatic, crate::tr("Automatisch"));
    ui.radio_value(&mut r.pick, Choice::None, crate::tr("Kein eigenes Profil"));
    caption(ui, "Eigenes Profil wählen");
    egui::ScrollArea::vertical().id_salt("objektive-kandidaten").max_height(120.0).auto_shrink([false, true]).show(ui, |ui| {
        for p in &candidates {
            let choice = Choice::Profile(p.reference().to_string());
            let name = profile_line(p);
            let button = egui::Button::new(&name).selected(r.pick == choice).truncate();
            let response = ui.add_sized([ui.available_width(), 24.0], button);
            lr::hover_tip(ui, &response, &name);
            if response.clicked() { r.pick = choice; }
        }
    });
    if candidates.is_empty() {
        dim(ui, crate::tr("Noch kein eigenes Profil für diese Kamera"));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button(crate::tr("Als Vorgabe setzen")).clicked() {
            r.message = Some(match lens_assign::set_rule(&id, &r.pick) {
                Ok(()) => (true, crate::tr("Vorgabe gespeichert. Gilt für alle Fotos dieses Objektivs ohne eigene Zuweisung.").to_string()),
                Err(e) => (false, e.to_string()),
            });
        }
    });
}

fn profile_detail(ui: &mut egui::Ui, r: &mut Room, id: &str) {
    let Some(p) = lensprof::store::list().into_iter().find(|p| p.id == id) else {
        dim(ui, crate::tr("Dieses Profil gibt es nicht mehr."));
        return;
    };
    ui.add(egui::Label::new(egui::RichText::new(&p.name).font(lr::semibold(14.0)).color(Color32::from_gray(0xee))).wrap());
    let sensor = match p.sensor {
        lensprof::own::SensorKind::Bayer => "Bayer",
        lensprof::own::SensorKind::Xtrans => "X-Trans",
        lensprof::own::SensorKind::Foveon => "Foveon",
        lensprof::own::SensorKind::Linear => "linear",
        lensprof::own::SensorKind::Unknown => "?",
    };
    dim(ui, format!("{} {} · {} · v{} · {sensor}", p.maker, p.model, if p.lens.is_empty() { crate::tr("manuell").to_string() } else { p.lens.clone() }, p.version));
    ui.add_space(4.0);
    if let Some(q) = &p.quality {
        caption(ui, "Güte");
        let parts: Vec<String> = q.parts.iter().map(|(k, v)| format!("{} {v}", crate::tr(match k {
            lensprof::own::QualityPart::Distortion => "Verzeichnung",
            lensprof::own::QualityPart::LateralCa => "Farbsäume",
            lensprof::own::QualityPart::Vignette => "Vignettierung",
            lensprof::own::QualityPart::Cast => "Farbstich",
        }))).collect();
        dim(ui, format!("{} · {} ({})", q.score, crate::tr(lensprof::quality::Level::of(q.score).text()), parts.join(" · ")));
    }
    ui.add_space(4.0);
    caption(ui, "Messungen");
    let focal: Vec<String> = {
        let mut v: Vec<f64> = p.profile.entries.iter().map(|e| e.focal_length).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v.dedup_by(|a, b| (*a - *b).abs() < 0.5);
        v.iter().map(|f| format!("{f:.0}")).collect()
    };
    dim(ui, crate::trf!("Geometrie und Vignette: {a} Einträge · Brennweiten {b} mm", a = p.profile.entries.len(), b = if focal.is_empty() { "–".into() } else { focal.join(", ") }));
    dim(ui, crate::trf!("Farbstichkarten: {a}", a = p.cast.len()));
    ui.add_space(4.0);
    caption(ui, "Versionsverlauf");
    egui::ScrollArea::vertical().id_salt("objektive-verlauf").max_height(110.0).auto_shrink([false, true]).show(ui, |ui| {
        for h in p.history.iter().rev() {
            dim(ui, format!("v{}  {}  {}{}", h.version, if h.time > 0 { date(h.time) } else { "–".into() }, action_text(&h.action), if h.shots.is_empty() { String::new() } else { format!(" · {}", crate::trf!("{n} Aufnahmen", n = h.shots.len())) }));
        }
        if p.history.is_empty() {
            dim(ui, crate::tr("Ältere Datei ohne Verlauf"));
        }
    });
    ui.add_space(4.0);
    let uses = r.usage.get(id).copied();
    dim(ui, match uses {
        Some(n) => crate::trf!("{n} Fotos haben dieses Profil zugewiesen", n = n),
        None => crate::tr("Nutzung wird ermittelt …").to_string(),
    });
    let rules = assign::rules().iter().filter(|x| x.profile().rsplit_once('@').map_or(x.profile(), |(i, _)| i) == id).count();
    if rules > 0 {
        dim(ui, crate::trf!("{n} Vorgaben (Regeln) zeigen darauf", n = rules));
    }
    ui.add_space(8.0);
    if let Some((rid, text)) = &mut r.rename {
        if rid == id {
            let mut commit = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                let e = lr::edit_field(ui, egui::TextEdit::singleline(text).desired_width(260.0));
                if e.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                if ui.button(crate::tr("Umbenennen")).clicked() {
                    commit = true;
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    cancel = true;
                }
            });
            if commit {
                let t = text.clone();
                r.message = Some(match exchange::rename(id, &t) {
                    Ok(()) => (true, crate::tr("Umbenannt").to_string()),
                    Err(e) => (false, e.to_string()),
                });
                r.rename = None;
            } else if cancel {
                r.rename = None;
            }
            return;
        }
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button(crate::tr("Umbenennen")).clicked() {
            r.rename = Some((id.to_string(), p.name.clone()));
        }
        if ui.button(crate::tr("Duplizieren")).clicked() {
            r.message = Some(match exchange::duplicate(id, &format!("{} {}", p.name, crate::tr("Kopie"))) {
                Ok(n) => {
                    r.sel = Sel::Profile(n.id);
                    (true, crate::tr("Dupliziert").to_string())
                }
                Err(e) => (false, e.to_string()),
            });
        }
        if ui.button(crate::tr("Exportieren …")).clicked() {
            r.export_id = Some(id.to_string());
            let name = format!("{}.graineo-lens", p.name.replace(['/', ':'], "-"));
            crate::file_dialog::save(ui.ctx(), EXPORT, move |d| d.set_title(crate::tr("Objektivprofil exportieren")).set_file_name(name).add_filter("graineo lens", &["graineo-lens"]));
        }
        if ui.button(crate::tr("In den Papierkorb")).clicked() {
            r.ask = Some(Ask::Trash { id: id.to_string(), name: p.name.clone(), uses: uses.unwrap_or(0) });
        }
    });
}
