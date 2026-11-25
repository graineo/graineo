//! Assigning a lens profile ("Ändern …" in the lens corrections opens it): the dialog "Objektiv zuweisen"
//! (this photo, the selection, or a default for all photos of a camera without lens name / with this lens name)
//! and the requests that the other lens windows (wizard, window OBJEKTIVE) hand to the main window.
//!
//! The assignment of a photo is `Settings.raw.lens_profile` (`"id@version"`, `""` = no own profile, unset =
//! automatic); a default is a rule in `lens-profiles/assignments.json` (`lensprof::assign`). Nothing is written into
//! the originals.

use std::cell::RefCell;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use eframe::egui;
use lensprof::assign::{self, Rule};
use lensprof::{OwnProfile, ProfileRef};

use crate::dialog::{self, Answer, Buttons};
use crate::lr;

const ID: &str = "objektiv-zuweisen";

/// Camera and lens of a photo as the assignment sees them.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub maker: String,
    pub model: String,
    /// lens name from the file; `None` = none (manual lens, adapter)
    pub lens: Option<String>,
    /// Sigma X3F: the profile is stored, its effect in the Sigma development follows later
    pub foveon: bool,
}

impl Identity {
    pub fn new(maker: String, model: String, lens: Option<String>) -> Self {
        let lens = lens.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        Identity { maker, model, lens, foveon: false }
    }

    pub fn foveon(camera: &str) -> Self {
        let model = camera.trim().to_string();
        let lens = library::exif::fixed_lens(&model).map(str::to_string);
        Identity { maker: "SIGMA".into(), model, lens, foveon: true }
    }

    pub fn camera_label(&self) -> String {
        let (a, b) = (self.maker.trim(), self.model.trim());
        if b.to_ascii_lowercase().starts_with(&a.to_ascii_lowercase()) || a.is_empty() { b.to_string() } else { format!("{a} {b}") }
    }

    fn rule(&self, profile: String) -> Rule {
        match &self.lens {
            None => Rule::CameraWithoutLens { maker: self.maker.clone(), model: self.model.clone(), profile },
            Some(l) => Rule::LensName { maker: self.maker.clone(), model: self.model.clone(), lens: l.clone(), profile },
        }
    }

    /// "SIGMA sd Quattro · ohne Objektivangabe"
    fn title(&self) -> String {
        match &self.lens {
            None => format!("{} · {}", self.camera_label(), crate::tr("ohne Objektivangabe")),
            Some(l) => format!("{} · {l}", self.camera_label()),
        }
    }
}

/// What the other windows ask of the main window.
#[derive(Clone, Debug)]
pub enum Request {
    /// open "Objektiv zuweisen" for the open photo
    Assign,
    /// open the window OBJEKTIVE
    Room,
    /// open the measuring assistant (name and focal length prefilled)
    Wizard,
    /// set an assignment right away (after saving a measurement)
    Apply { profile: String, ident: Identity, rule: bool, photo: bool },
}

thread_local! {
    static REQUESTS: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
}

pub fn request(r: Request) {
    REQUESTS.with(|q| q.borrow_mut().push(r));
}

pub fn take_requests() -> Vec<Request> {
    REQUESTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// no assignment (rule or automatic choice by lens name)
    Automatic,
    /// explicitly no own profile
    None,
    /// profile `"id@version"`
    Profile(String),
}

impl Choice {
    /// Value of `Settings.raw.lens_profile`.
    pub fn pin(&self) -> Option<String> {
        match self {
            Choice::Automatic => None,
            Choice::None => Some(String::new()),
            Choice::Profile(p) => Some(p.clone()),
        }
    }

    fn of_pin(p: Option<&str>) -> Self {
        match p {
            None => Choice::Automatic,
            Some("") => Choice::None,
            Some(r) => Choice::Profile(r.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Photo,
    Selection,
    /// default for all photos of the camera without lens name / with this lens name
    Rule,
}

struct Dialog {
    ident: Identity,
    choice: Choice,
    scope: Scope,
    selected: usize,
    candidates: Vec<Arc<OwnProfile>>,
    matching: Option<usize>,
    rx: Receiver<usize>,
    /// lensfun lenses and Adobe profiles to choose from (filled by a helper thread)
    lists: Option<Lists>,
    lists_rx: Receiver<Lists>,
}

/// Profiles of the databases for the manual choice.
struct Lists {
    /// lensfun: (name, mounts, fits the camera without an adapter)
    lensfun: Vec<(String, String, bool)>,
    /// Adobe: (file name, label)
    lcp: Vec<(String, String)>,
}

fn lists_for(maker: &str, model: &str) -> Lists {
    let db = raw::lensfun::Database::active();
    let cam = db.camera(maker, model);
    let lensfun = db.lens_names().into_iter().map(|(n, m)| (n, m.join(", "), db.mount_fits(cam, &m))).collect();
    let extra = library::settings::value("lcpFolder").as_str().map(std::path::PathBuf::from);
    let mut lcp: Vec<(String, String)> = raw::lcp::index(extra.as_deref())
        .iter()
        .filter_map(|h| {
            let f = h.path.file_name()?.to_str()?.to_string();
            let lens = if h.lens_pretty.is_empty() { h.lens.clone() } else { h.lens_pretty.clone() };
            let lens = if lens.is_empty() { h.model.clone() } else { lens };
            Some((f, format!("{lens} · {} {}", h.make, h.model).trim().to_string()))
        })
        .collect();
    lcp.sort_by_key(|(_, l)| l.to_lowercase());
    lcp.dedup_by(|a, b| a.0 == b.0);
    Lists { lensfun, lcp }
}

/// What the dialog decided; the main window carries it out.
pub struct Applied {
    pub ident: Identity,
    pub choice: Choice,
    pub scope: Scope,
}

/// Open the dialog for the photo `ident`; `current` is its present assignment, `selected` the number of selected photos.
pub fn open(ctx: &egui::Context, ident: Identity, current: Option<&str>, selected: usize) {
    let (tx, rx) = channel();
    let (model, lens) = (ident.model.clone(), ident.lens.clone());
    let c = ctx.clone();
    std::thread::Builder::new()
        .name("objektiv-zaehlen".into())
        .spawn(move || {
            let _ = tx.send(library::lens_use::photos_matching(&model, lens.as_deref()));
            c.request_repaint();
        })
        .ok();
    let (ltx, lists_rx) = channel();
    let (maker, model) = (ident.maker.clone(), ident.model.clone());
    let c = ctx.clone();
    std::thread::Builder::new()
        .name("lens-profile-lists".into())
        .spawn(move || {
            let _ = ltx.send(lists_for(&maker, &model));
            c.request_repaint();
        })
        .ok();
    let candidates = assign::candidates(&ident.maker, &ident.model);
    DIALOG.with(|d| *d.borrow_mut() = Some(Dialog { choice: Choice::of_pin(current), scope: Scope::Photo, selected, candidates, ident, matching: None, rx, lists: None, lists_rx }));
}

fn level_text(p: &OwnProfile) -> String {
    let v = format!("v{}", p.version);
    match &p.quality {
        Some(q) => format!("{} · {v} · {} {} ({})", crate::tr("eigenes"), crate::tr("Güte"), q.score, crate::tr(lensprof::quality::Level::of(q.score).text())),
        None => format!("{} · {v}", crate::tr("eigenes")),
    }
}

/// Name and state of an assignment for display.
pub fn profile_label(r: &str) -> String {
    if let Some(n) = r.strip_prefix(raw::lens::PIN_LENSFUN) {
        return format!("{n} · lensfun");
    }
    if let Some(n) = r.strip_prefix(raw::lens::PIN_LCP) {
        return format!("{n} · Adobe");
    }
    match ProfileRef::parse(r).and_then(|p| lensprof::store::load(&p)) {
        Some(p) => format!("{} · v{}", p.name, p.version),
        None => crate::tr("Profil fehlt").to_string(),
    }
}

/// Draw the dialog; returns the decision once.
pub fn ui(ctx: &egui::Context) -> Option<Applied> {
    DIALOG.with(|cell| {
        let mut guard = cell.borrow_mut();
        let d = guard.as_mut()?;
        if let Ok(n) = d.rx.try_recv() {
            d.matching = Some(n);
        }
        if let Ok(l) = d.lists_rx.try_recv() {
            d.lists = Some(l);
        }
        let mut open_wizard = false;
        let mut open_room = false;
        let has_selection = d.selected > 1;
        let title = crate::tr("Objektiv zuweisen");
        let answer = dialog::show(
            ctx,
            ID,
            title,
            560.0,
            |ui| {
                dialog::chip(ui, &d.ident.title());
                ui.add_space(4.0);
                ui.label(egui::RichText::new(crate::tr("PROFIL")).small().color(lr::TEXT_DIM()));
                let rule = d.scope == Scope::Rule;
                let auto_label = if rule { crate::tr("Vorgabe entfernen (automatisch nach Objektivname)") } else { crate::tr("Automatisch (nach Objektivname)") };
                ui.radio_value(&mut d.choice, Choice::Automatic, auto_label);
                ui.radio_value(&mut d.choice, Choice::None, crate::tr("Kein eigenes Profil"));
                egui::ScrollArea::vertical().max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                    for p in &d.candidates {
                        let r = p.reference().to_string();
                        ui.radio_value(&mut d.choice, Choice::Profile(r), format!("{}    {}", p.name, level_text(p)));
                    }
                });
                if d.candidates.is_empty() {
                    ui.label(egui::RichText::new(crate::tr("Noch kein eigenes Profil für diese Kamera")).small().color(lr::TEXT_DIM()));
                }
                ui.horizontal(|ui| {
                    if ui.button(crate::tr("Neues Objektiv vermessen …")).clicked() {
                        open_wizard = true;
                    }
                    // the window OBJEKTIVE (own profiles, measuring assistant); the panel has no button for it
                    if ui.button(crate::tr("Objektive verwalten …")).clicked() {
                        open_room = true;
                    }
                });
                ui.add_space(4.0);
                database_choice(ui, d);
                ui.add_space(6.0);
                ui.label(egui::RichText::new(crate::tr("GILT FÜR")).small().color(lr::TEXT_DIM()));
                ui.radio_value(&mut d.scope, Scope::Photo, crate::tr("dieses Foto"));
                if has_selection {
                    ui.radio_value(&mut d.scope, Scope::Selection, crate::trf!("die Auswahl ({n} Fotos)", n = d.selected));
                }
                let n = d.matching.map_or("…".to_string(), |n| n.to_string());
                let text = match &d.ident.lens {
                    None => crate::trf!("alle Fotos dieser Kamera ohne Objektivangabe ({n})", n = n),
                    Some(l) => crate::trf!("alle Fotos dieser Kamera mit Objektivangabe „{l}“ ({n})", l = l, n = n),
                };
                ui.radio_value(&mut d.scope, Scope::Rule, text);
                if d.scope == Scope::Rule {
                    dialog::hint(ui, "Gilt auch für künftige Importe. Ein Foto mit eigener Zuweisung behält sie.");
                }
                if d.ident.foveon {
                    dialog::hint(ui, "Sigma-X3F: Die Zuweisung wird gemerkt; die Wirkung im Foveon-Pfad folgt in einer späteren Fassung.");
                }
            },
            Buttons::confirm("Zuweisen", true),
        );
        if open_wizard {
            request(Request::Wizard);
        }
        if open_room {
            request(Request::Room);
        }
        match answer {
            Answer::Primary => {
                let a = Applied { ident: d.ident.clone(), choice: d.choice.clone(), scope: d.scope };
                *guard = None;
                Some(a)
            }
            Answer::Cancel => {
                *guard = None;
                None
            }
            Answer::Open => None,
        }
    })
}

/// The two choices from the databases: a lensfun lens (any mount, also adapted) or an Adobe profile (LCP).
fn database_choice(ui: &mut egui::Ui, d: &mut Dialog) {
    let Some(lists) = &d.lists else {
        ui.label(egui::RichText::new(crate::tr("Profile werden gelesen …")).small().color(lr::TEXT_DIM()));
        return;
    };
    let current = match &d.choice {
        Choice::Profile(p) => p.clone(),
        _ => String::new(),
    };
    let pin_lf = current.strip_prefix(raw::lens::PIN_LENSFUN).map(str::to_string);
    let pin_lcp = current.strip_prefix(raw::lens::PIN_LCP).map(str::to_string);
    let mut picked: Option<String> = None;
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 18.0], egui::Label::new(egui::RichText::new("lensfun").small().color(lr::TEXT_DIM())));
        let shown = pin_lf.clone().unwrap_or_else(|| "–".into());
        egui::ComboBox::from_id_salt("lens-assign-lensfun").selected_text(shown).width(ui.available_width()).height(f32::INFINITY).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show_ui(ui, |ui| {
            lr::list_menu(ui, "lens-assign-lensfun", lists.lensfun.len(), |ui, f| {
                for (n, mounts, fits) in lists.lensfun.iter().filter(|(n, m, _)| f.shows(n) || f.shows(m)) {
                    let text = if *fits { n.clone() } else { format!("{n}    ({mounts})") };
                    if ui.selectable_label(pin_lf.as_deref() == Some(n), text).clicked() || f.take_enter() {
                        picked = Some(format!("{}{n}", raw::lens::PIN_LENSFUN));
                        ui.close();
                    }
                }
            });
        });
    });
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 18.0], egui::Label::new(egui::RichText::new("Adobe").small().color(lr::TEXT_DIM())));
        let shown = pin_lcp.as_ref().map(|f| lists.lcp.iter().find(|(x, _)| x == f).map_or(f.clone(), |(_, l)| l.clone())).unwrap_or_else(|| "–".into());
        ui.add_enabled_ui(!lists.lcp.is_empty(), |ui| {
            egui::ComboBox::from_id_salt("lens-assign-lcp").selected_text(shown).width(ui.available_width()).height(f32::INFINITY).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show_ui(ui, |ui| {
                lr::list_menu(ui, "lens-assign-lcp", lists.lcp.len(), |ui, f| {
                    for (file, label) in lists.lcp.iter().filter(|(_, l)| f.shows(l)) {
                        if ui.selectable_label(pin_lcp.as_deref() == Some(file), label).clicked() || f.take_enter() {
                            picked = Some(format!("{}{file}", raw::lens::PIN_LCP));
                            ui.close();
                        }
                    }
                });
            });
        });
    });
    if let Some(p) = picked {
        d.choice = Choice::Profile(p);
    }
    if lists.lcp.is_empty() {
        ui.label(egui::RichText::new(crate::tr("Keine Adobe-Profile installiert")).small().color(lr::TEXT_DIM()));
    }
    if let Some(n) = &pin_lf {
        if lists.lensfun.iter().any(|(m, _, fits)| m == n && !fits) {
            dialog::hint(ui, "Anderes Bajonett: Das Profil gilt für das Objektiv am Originalbajonett. Mit Adapter stimmen Verzeichnung und Farbquerfehler meist, die Vignettierung nur ohne zusätzliche Abschattung durch den Adapter.");
        }
    }
}

/// Set the default (rule) for the camera and lens of `ident`; `Automatic` removes it.
pub fn set_rule(ident: &Identity, choice: &Choice) -> std::io::Result<()> {
    match choice {
        Choice::Automatic => assign::remove_rule(&ident.rule(String::new())).map(|_| ()),
        Choice::None => assign::set_rule(ident.rule(String::new())),
        Choice::Profile(p) => assign::set_rule(ident.rule(p.clone())),
    }
}

