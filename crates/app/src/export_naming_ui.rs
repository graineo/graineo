//! The naming part of the export dialog: pattern field with placeholder chips, number, subfolders, existing files,
//! live preview and export templates. The names themselves come from `export_names`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::export_names::{self as names, Collision, Info, JobNames, Naming, Plan, Subfolder};
use crate::export_templates::{self as templates, Template};
use crate::lr;

/// Date formats offered by the date chip (the format, an example is shown).
const DATE_FORMATS: &[&str] = &["YYYY-MM-DD", "YYYYMMDD", "YYYY-MM-DD_hhmmss", "DD.MM.YYYY"];

#[derive(Default)]
pub struct State {
    /// key of the cached plan (photos, rules, format)
    key: u64,
    plan: Plan,
    endings: Vec<String>,
    titles: HashMap<i64, Option<String>>,
    /// capture time and camera read from the file header for photos the library has no row or date for
    headers: HashMap<PathBuf, (Option<String>, Option<String>)>,
    /// files of the plan that exist already (counted in the background: a network drive must not freeze the window)
    existing: Arc<Mutex<Option<(u64, usize)>>>,
    existing_key: u64,
    templates: Option<Vec<(PathBuf, Template)>>,
    chosen: Option<String>,
    /// name being typed for "save as template"
    typing: Option<String>,
    /// a template of this name exists: replace?
    replacing: Option<String>,
    pub(crate) message: String,
    /// export waiting for the folder choice ("choose folder and export")
    pub pick: Option<Vec<usize>>,
    /// export waiting for "overwrite?"
    pub confirm: Option<Vec<usize>>,
    /// "Advanced" of the export dialog opened (read from the settings once)
    pub advanced: Option<bool>,
}

/// File name of photo `k` as the job writes it ("add a number": the job numbers duplicates on its own, the preview
/// shows the name the photo asks for).
fn shown_name(plan: &Plan, endings: &[String], collision: Collision, k: usize) -> Option<String> {
    let p = plan.items.get(k)?;
    let rel = if collision == Collision::Number { &p.base } else { &p.unique };
    Some(format!("{}{}", rel.to_string_lossy(), endings.get(k).map(String::as_str).unwrap_or_default()))
}

/// A row of the export dialog ([`crate::export_ui::row`]).
fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    crate::export_ui::row(ui, label, add);
}

impl crate::App {
    /// The photos of `items` the export writes (the same choice as `export_after_probe`).
    pub(crate) fn export_written(&self, items: &[usize]) -> Vec<usize> {
        items.iter().copied().filter(|&i| (self.entries[i].state == 0 || (!self.export_original && crate::developable_state(self.entries[i].state))) && (self.export_original || crate::image_kind::of(&self.entries[i].path).editable())).collect()
    }

    /// The rules as they apply: originals are never overwritten (verified copies), "overwrite" adds a number there.
    pub(crate) fn export_naming_effective(&self) -> Naming {
        let mut n = self.export_naming.clone();
        if self.export_original && n.collision == Collision::Overwrite {
            n.collision = Collision::Number;
        }
        n
    }

    fn export_info(&mut self, i: usize, want_title: bool, want_header: bool) -> Info {
        let e = &self.entries[i];
        let apple = library::apple_photos::in_photos_library(&e.path);
        let stem = apple
            .then(|| crate::apple_photos_ui::original_name(e.file))
            .flatten()
            .map(|n| std::path::Path::new(&n).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(n))
            .unwrap_or_else(|| e.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        let folder = (!apple).then(|| e.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned())).flatten();
        let (image, mono) = (e.image, e.settings.monochrome && !self.export_original);
        let (mut capture, mut camera) = self.lib.row_of_image(image).map(|r| (r.capture.clone(), r.camera.clone())).unwrap_or_default();
        // a folder only viewed (not in the library): the file header, read once per photo
        if want_header && capture.is_none() && !apple && e.state == 0 {
            let (path, format) = (e.path.clone(), e.format.clone());
            let (c, k) = self.export_names_ui.headers.entry(path.clone()).or_insert_with(|| {
                let format = if format.is_empty() { path.extension().map(|x| x.to_string_lossy().to_uppercase()).unwrap_or_default() } else { format };
                library::exif::read(&path, &format).map(|x| (x.capture, x.camera)).unwrap_or_default()
            });
            capture = c.clone();
            camera = camera.or_else(|| k.clone());
        }
        let title = if want_title && image > 0 {
            if !self.export_names_ui.titles.contains_key(&image) {
                let t = self.lib.title_of(image);
                self.export_names_ui.titles.insert(image, t);
            }
            self.export_names_ui.titles.get(&image).cloned().flatten()
        } else {
            None
        };
        let source_dir = e.path.parent().map(|p| p.to_path_buf());
        Info { stem, mono, capture, camera, title, folder, source_dir }
    }

    /// What follows the stem of photo `i` (".jpg", "_linear.tif", the original's extension).
    fn export_ending(&self, i: usize) -> String {
        if self.export_original {
            self.entries[i].path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()
        } else {
            format!("{}.{}", self.export_format.name_suffix(), self.export_format.extension())
        }
    }

    /// Names of the photos `items` (already filtered with [`Self::export_written`]).
    pub(crate) fn export_plan(&mut self, items: &[usize]) -> (Plan, Vec<String>) {
        let naming = self.export_naming_effective();
        let want_title = naming.uses("title");
        let want_header = naming.uses("date") || naming.uses("camera") || naming.subfolder == Subfolder::Date;
        let infos: Vec<Info> = items.iter().map(|&i| self.export_info(i, want_title, want_header)).collect();
        let endings: Vec<String> = items.iter().map(|&i| self.export_ending(i)).collect();
        (names::plan(&naming, &infos, |k| endings[k].clone()), endings)
    }

    /// The names a job gets for `items` (filtered with [`Self::export_written`]).
    pub(crate) fn export_job_names(&mut self, items: &[usize]) -> JobNames {
        let (plan, _) = self.export_plan(items);
        JobNames::from_plan(&plan, self.export_naming_effective().collision)
    }

    /// The export button was pressed: without a folder the folder choice comes first, "overwrite" asks first.
    pub(crate) fn export_requested(&mut self, ctx: &egui::Context, items: Vec<usize>) -> bool {
        if items.is_empty() || self.exporting {
            return false;
        }
        if self.export_target_dir().is_none() {
            crate::file_dialog::target_folder(ctx, "export-ordner-start", |d| d);
            self.export_names_ui.pick = Some(items);
            return false;
        }
        if self.export_naming_effective().collision == Collision::Overwrite {
            self.export_names_ui.confirm = Some(items);
            return false;
        }
        self.export(items);
        true
    }

    /// Once per frame: folder chosen for a waiting export, the "overwrite?" question.
    pub(crate) fn export_naming_step(&mut self, ctx: &egui::Context) {
        if let Some(d) = crate::file_dialog::fetch_one("export-ordner-start") {
            self.set_export_dir(d);
            if let Some(items) = self.export_names_ui.pick.take() {
                if self.export_requested(ctx, items) {
                    self.show_export = false;
                }
            }
        }
        let Some(items) = self.export_names_ui.confirm.clone() else { return };
        let n = items.len();
        let a = crate::dialog::show(
            ctx,
            "export-overwrite",
            crate::tr("Vorhandene Dateien überschreiben?"),
            crate::dialog::WIDTH,
            |ui| {
                crate::dialog::text(ui, &crate::trf!("Dateien gleichen Namens im Zielordner werden durch den Export ersetzt ({n} Bilder). Die alten kommen in den Papierkorb.", n = n));
                crate::dialog::hint(ui, crate::tr("Das Original eines Fotos wird nie ersetzt."));
            },
            crate::dialog::Buttons::destructive(crate::tr("Überschreiben"), true),
        );
        match a {
            crate::dialog::Answer::Primary => {
                self.export_names_ui.confirm = None;
                self.export(items);
                self.show_export = false;
            }
            crate::dialog::Answer::Cancel => self.export_names_ui.confirm = None,
            crate::dialog::Answer::Open => {}
        }
    }

    pub(crate) fn set_export_dir(&mut self, d: PathBuf) {
        library::settings::set("exportDir", serde_json::Value::String(d.to_string_lossy().into_owned()));
        self.export_dir = Some(d);
    }

    /// The folder an export goes to: the chosen one, or – "next to the original" – a marker folder ("/") that the
    /// absolute planned names replace (`Path::join` with an absolute path). `None`: a folder must be chosen first.
    pub(crate) fn export_target_dir(&self) -> Option<PathBuf> {
        if self.export_naming.beside {
            Some(PathBuf::from(std::path::MAIN_SEPARATOR_STR))
        } else {
            self.export_dir.clone()
        }
    }

    /// The folder shown after an export (reveal, messages): the chosen folder, or the first original's folder.
    pub(crate) fn export_shown_dir(&self) -> Option<PathBuf> {
        if self.export_naming.beside {
            self.export_items.first().and_then(|p| p.parent()).map(PathBuf::from)
        } else {
            self.export_dir.clone()
        }
    }

    /// Main button label: without a folder it says that the folder choice comes first.
    pub(crate) fn export_button_label(&self, n: usize) -> String {
        if self.export_target_dir().is_none() {
            crate::tr("Ordner wählen und exportieren …").to_string()
        } else if n == 1 {
            crate::tr("Exportieren").to_string()
        } else {
            crate::trf!("{a} Bilder exportieren", a = n)
        }
    }

    /// Saves changed naming rules; a chosen template no longer applies then.
    pub(crate) fn export_naming_commit(&mut self, before: &Naming) {
        if self.export_naming != *before {
            if let Ok(v) = serde_json::to_value(&self.export_naming) {
                library::settings::set("exportNaming", v);
            }
            self.export_names_ui.chosen = None;
        }
    }

    /// Rows "Name" (pattern field with the placeholder chips in one row below it) and "Vorhanden".
    pub(crate) fn export_name_rows(&mut self, ui: &mut egui::Ui, items: &[usize]) {
        let before = self.export_naming.clone();
        row(ui, crate::tr("Muster"), |ui| {
            let w = (ui.available_width() - 30.0).max(80.0);
            lr::edit_field(ui, egui::TextEdit::singleline(&mut self.export_naming.pattern).id_salt("export-name-pattern").desired_width(w).font(egui::TextStyle::Monospace));
            let r = ui.add_enabled(self.export_naming.pattern != names::DEFAULT_PATTERN, egui::Button::new("↺").small());
            lr::hover_tip(ui, &r, crate::tr("Zurück zum Originalnamen ({name})"));
            if r.clicked() {
                self.export_naming.pattern = names::DEFAULT_PATTERN.into();
            }
        });
        let digits = names::number_digits(&self.export_naming.pattern);
        let sample = items.first().and_then(|&i| self.lib.row_of_image(self.entries[i].image)).and_then(|r| r.capture.clone()).unwrap_or_else(|| "2026-07-14T09:05:00".into());
        row(ui, crate::tr("Einfügen"), |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.spacing_mut().button_padding.x = 4.0;
            let mut add: Option<String> = None;
            let chip = |ui: &mut egui::Ui, label: &str, tip: &str| {
                let r = ui.add(egui::Button::new(egui::RichText::new(label).small()));
                lr::hover_tip(ui, &r, tip);
                r.clicked()
            };
            if chip(ui, crate::tr("Original"), crate::tr("Originalname {name}")) {
                add = Some("{name}".into());
            }
            if chip(ui, crate::tr("Text"), crate::tr("Eigener Text: im Feld überschreiben")) {
                add = Some(crate::tr("Text").into());
            }
            if chip(ui, crate::tr("Nr."), crate::tr("Laufende Nummer {n:3}")) {
                add = Some(format!("{{n:{}}}", digits.unwrap_or(3)));
            }
            let r = ui.menu_button(egui::RichText::new(format!("{} ▾", crate::tr("Datum"))).small(), |ui| {
                for f in DATE_FORMATS {
                    let example = names::format_date(&sample, f).unwrap_or_default();
                    if ui.button(format!("{example}    {f}")).clicked() {
                        add = Some(if *f == names::DEFAULT_DATE { "{date}".into() } else { format!("{{date:{f}}}") });
                        ui.close();
                    }
                }
            });
            lr::hover_tip(ui, &r.response, crate::tr("Aufnahmedatum"));
            // camera, title and black-and-white: as chips when they fit, otherwise in a "more" menu (never a second line)
            let font = egui::TextStyle::Small.resolve(ui.style());
            let rest = [(crate::tr("Kamera"), "{camera}", "{camera}"), (crate::tr("Titel"), crate::tr("Titel aus der Bibliothek, falls vorhanden"), "{title}"), (crate::tr("S/W"), crate::tr("„_mono“ bei Schwarzweiß"), "{mono}")];
            let need: f32 = rest.iter().map(|(l, _, _)| ui.painter().layout_no_wrap(l.to_string(), font.clone(), lr::TEXT()).size().x + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x).sum();
            if need <= ui.available_width() {
                for (label, tip, value) in rest {
                    if chip(ui, label, tip) {
                        add = Some(value.into());
                    }
                }
            } else {
                ui.menu_button(egui::RichText::new(format!("{} ▾", crate::tr("Mehr"))).small(), |ui| {
                    for (label, tip, value) in rest {
                        let r = ui.button(label);
                        lr::hover_tip(ui, &r, tip);
                        if r.clicked() {
                            add = Some(value.into());
                            ui.close();
                        }
                    }
                });
            }
            if let Some(a) = add {
                let p = &mut self.export_naming.pattern;
                let glue = !p.is_empty() && !p.ends_with(['_', '-', ' ', '.']) && a != "{mono}";
                if glue {
                    p.push('_');
                }
                p.push_str(&a);
            }
        });
        row(ui, crate::tr("Wenn vorhanden"), |ui| {
            let mut options = vec![(Collision::Number, crate::tr("Nummer anhängen"))];
            let mut tips = vec![crate::tr("Name-2.jpg; nichts wird überschrieben")];
            if !self.export_original {
                options.push((Collision::Overwrite, crate::tr("Ersetzen")));
                tips.push(crate::tr("Ersetzt Dateien gleichen Namens (alte in den Papierkorb); wird vorher gefragt"));
            }
            options.push((Collision::Skip, crate::tr("Überspringen")));
            tips.push(crate::tr("Fotos, deren Datei es schon gibt, nicht exportieren"));
            let mut c = self.export_naming_effective().collision;
            if crate::export_ui::choice(ui, "export-collision", &mut c, &options, &tips) {
                self.export_naming.collision = c;
            }
        });
        self.export_naming_commit(&before);
    }

    /// Rows "Unterordner" and "Nummer" (start and digits; greyed out without a number in the pattern).
    pub(crate) fn export_numbering_rows(&mut self, ui: &mut egui::Ui) {
        let before = self.export_naming.clone();
        row(ui, crate::tr("Unterordner"), |ui| {
            crate::export_ui::choice(
                ui,
                "export-subfolder",
                &mut self.export_naming.subfolder,
                &[(Subfolder::None, crate::tr("Keine")), (Subfolder::Date, crate::tr("Nach Datum")), (Subfolder::Folder, crate::tr("Nach Ordner")), (Subfolder::Custom, crate::tr("Eigener"))],
                &["", crate::tr("Jahr/Tag der Aufnahme, z. B. 2026/2026-07-14"), crate::tr("Name des Ordners, in dem das Original liegt"), crate::tr("Eigener Name mit Platzhaltern, z. B. Export/{date:YYYY}")],
            );
        });
        // the typed subfolder: same placeholders as the name, "/" for nested folders (greyed out unless "Eigener")
        let custom = self.export_naming.subfolder == Subfolder::Custom;
        crate::export_ui::row_if(ui, "", custom, |ui| {
            let r = lr::edit_field(ui, egui::TextEdit::singleline(&mut self.export_naming.subfolder_text).hint_text("Export/{date:YYYY}").desired_width(ui.available_width()));
            lr::hover_tip(ui, &r, crate::tr("Platzhalter wie im Dateinamen: {date}, {date:YYYY}, {camera}, {title}, {n}; / trennt Ordner"));
        });
        let digits = names::number_digits(&self.export_naming.pattern);
        crate::export_ui::row_if(ui, crate::tr("Nummer"), digits.is_some(), |ui| {
            ui.label(egui::RichText::new(crate::tr("Start")).color(lr::TEXT_DIM()));
            lr::text_field(ui, egui::DragValue::new(&mut self.export_naming.start).range(0..=999_999).speed(1.0));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::tr("Stellen")).color(lr::TEXT_DIM()));
            let mut d = digits.unwrap_or(3);
            if lr::text_field(ui, egui::DragValue::new(&mut d).range(1..=9).speed(0.1)).changed() {
                self.export_naming.pattern = names::with_number_digits(&self.export_naming.pattern, d);
            }
        });
        self.export_naming_commit(&before);
    }

    /// Live preview: "→ name of the first photo · n files" and the conflicts (same names, existing files, counted on a
    /// helper thread: a network drive must not freeze the window).
    /// The third value: a note says that files are replaced (shown as a warning).
    pub(crate) fn export_preview(&mut self, ctx: &egui::Context, items: &[usize]) -> (String, Vec<String>, bool) {
        let items = self.export_written(items);
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            items.hash(&mut h);
            for &i in &items {
                self.entries[i].path.hash(&mut h);
                self.entries[i].settings.monochrome.hash(&mut h);
            }
            serde_json::to_string(&self.export_naming).unwrap_or_default().hash(&mut h);
            format!("{:?}{}", self.export_format, self.export_original).hash(&mut h);
            h.finish()
        };
        if key != self.export_names_ui.key {
            let (plan, endings) = self.export_plan(&items);
            self.export_names_ui.plan = plan;
            self.export_names_ui.endings = endings;
            self.export_names_ui.key = key;
        }
        let collision = self.export_naming_effective().collision;
        let dir = self.export_target_dir();
        let state = &mut self.export_names_ui;
        let first = shown_name(&state.plan, &state.endings, collision, 0);
        let existing_key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h);
            dir.hash(&mut h);
            h.finish()
        };
        if existing_key != state.existing_key {
            state.existing_key = existing_key;
            *state.existing.lock().unwrap_or_else(|e| e.into_inner()) = None;
            if let Some(dir) = dir {
                let paths: Vec<PathBuf> = (0..state.plan.items.len().min(5000)).filter_map(|k| shown_name(&state.plan, &state.endings, collision, k)).map(|n| dir.join(n)).collect();
                let (out, ctx) = (state.existing.clone(), ctx.clone());
                let _ = std::thread::Builder::new().name("export-names-existing".into()).spawn(move || {
                    let n = paths.iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).count();
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some((existing_key, n));
                    ctx.request_repaint();
                });
            }
        }
        let existing = state.existing.lock().unwrap_or_else(|e| e.into_inner()).filter(|(k, _)| *k == existing_key).map(|(_, n)| n).unwrap_or(0);
        let count = state.plan.items.len();
        let preview = match first {
            Some(f) => crate::trf!("→ {f} · {n}", f = f, n = crate::dialog::count(count, "Datei", "Dateien")),
            None => crate::tr("→ keine Datei").to_string(),
        };
        let mut notes = Vec::new();
        if state.plan.duplicates > 0 {
            notes.push(crate::trf!("{n} gleiche Namen → bekommen eine Nummer", n = state.plan.duplicates));
        }
        if existing > 0 {
            notes.push(match collision {
                Collision::Number => crate::trf!("{n} gibt es schon → bekommen eine Nummer", n = existing),
                Collision::Overwrite => crate::trf!("{n} gibt es schon → werden ersetzt", n = existing),
                Collision::Skip => crate::trf!("{n} gibt es schon → werden übersprungen", n = existing),
            });
        }
        (preview, notes, existing > 0 && collision == Collision::Overwrite)
    }

    /// Template controls (the dialog shows them in its title row): choose, save the current rules and format as a
    /// template, put one into the trash.
    pub(crate) fn export_template_controls(&mut self, ui: &mut egui::Ui) {
        let dir = library::data_dir();
        if self.export_names_ui.templates.is_none() {
            self.export_names_ui.templates = Some(templates::load_all(&dir));
        }
        let list = self.export_names_ui.templates.clone().unwrap_or_default();
        let mut apply: Option<Template> = None;
        let mut save: Option<(String, bool)> = None;
        let mut remove: Option<PathBuf> = None;
        ui.horizontal(|ui| {
            if let Some(name) = self.export_names_ui.replacing.clone() {
                ui.label(egui::RichText::new(crate::trf!("„{a}“ gibt es schon.", a = name)).color(lr::TEXT()));
                if ui.button(crate::tr("Ersetzen")).clicked() {
                    self.export_names_ui.replacing = None;
                    save = Some((name, true));
                }
                if ui.button(crate::tr("Abbrechen")).clicked() {
                    self.export_names_ui.replacing = None;
                }
                return;
            }
            if let Some(mut name) = self.export_names_ui.typing.clone() {
                let r = lr::edit_field(ui, egui::TextEdit::singleline(&mut name).hint_text(crate::tr("Name der Vorlage")).desired_width((ui.available_width() - 150.0).clamp(80.0, 220.0)));
                if self.export_names_ui.typing.as_deref() == Some("") && !r.has_focus() && name.is_empty() {
                    r.request_focus();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ok = ui.add_enabled(!name.trim().is_empty(), egui::Button::new(crate::tr("Sichern"))).clicked() || (enter && !name.trim().is_empty());
                let cancel = ui.button(crate::tr("Abbrechen")).clicked();
                if ok {
                    self.export_names_ui.typing = None;
                    save = Some((name.trim().to_string(), false));
                } else if cancel {
                    self.export_names_ui.typing = None;
                } else {
                    self.export_names_ui.typing = Some(name);
                }
                return;
            }
            let shown = self.export_names_ui.chosen.clone().unwrap_or_else(|| crate::tr("– keine –").into());
            // the list takes the room the two buttons leave
            let font = egui::TextStyle::Button.resolve(ui.style());
            let buttons: f32 = [crate::tr("Sichern …"), crate::tr("Löschen")].iter().map(|t| ui.painter().layout_no_wrap(t.to_string(), font.clone(), lr::TEXT()).size().x + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x).sum();
            let w = (ui.available_width() - buttons - 2.0 * ui.spacing().item_spacing.x + 12.0).clamp(60.0, 400.0);
            egui::ComboBox::from_id_salt("export-template").selected_text(shown).width(w).show_ui(ui, |ui| {
                if list.is_empty() {
                    ui.label(egui::RichText::new(crate::tr("Noch keine Vorlagen")).color(lr::TEXT_DIM()));
                }
                for (_, t) in &list {
                    if ui.selectable_label(self.export_names_ui.chosen.as_deref() == Some(t.name.as_str()), &t.name).clicked() {
                        apply = Some(t.clone());
                    }
                }
            });
            let r = ui.button(crate::tr("Sichern …"));
            lr::hover_tip(ui, &r, crate::tr("Name, Ziel, Format und Größe als Vorlage sichern"));
            if r.clicked() {
                self.export_names_ui.typing = Some(String::new());
            }
            let chosen_path = self.export_names_ui.chosen.as_ref().and_then(|c| list.iter().find(|(_, t)| &t.name == c)).map(|(p, _)| p.clone());
            let r = ui.add_enabled(chosen_path.is_some(), egui::Button::new(crate::tr("Löschen")));
            lr::hover_tip(ui, &r, crate::tr("Vorlage in den Papierkorb legen"));
            if r.clicked() {
                remove = chosen_path;
            }
        });
        if let Some(t) = apply {
            self.export_apply_template(&t);
        }
        if let Some((name, replace)) = save {
            if !replace && templates::existing(&dir, &name).is_some() {
                self.export_names_ui.replacing = Some(name);
            } else {
                let mut t = Template::new(&name, self.export_naming.clone());
                t.original = self.export_original;
                t.file_format = Some(self.export_format);
                t.color_space = Some(self.export_color_space);
                t.limit_size = Some(self.export_limit_size);
                t.size = serde_json::to_value(self.export_size).ok();
                t.resize = Some(self.export_resize.key().to_string());
                t.dir = self.export_dir.as_ref().map(|d| d.to_string_lossy().into_owned());
                match templates::save(&dir, &t) {
                    Ok(_) => {
                        self.export_names_ui.templates = None;
                        self.export_names_ui.chosen = Some(t.name.clone());
                        self.export_names_ui.message.clear();
                    }
                    Err(e) => self.export_names_ui.message = crate::trf!("Vorlage nicht gesichert: {e}", e = e),
                }
            }
        }
        if let Some(p) = remove {
            match trash::delete(&p) {
                Ok(()) => {
                    self.export_names_ui.templates = None;
                    self.export_names_ui.chosen = None;
                    self.export_names_ui.message.clear();
                }
                Err(e) => self.export_names_ui.message = crate::trf!("Vorlage nicht entfernt: {e}", e = e),
            }
        }
    }

    fn export_apply_template(&mut self, t: &Template) {
        // the template's folder: taken when reachable, otherwise the current target stays and the message says so
        // (a template without a folder keeps the current one)
        let mut folder_note = None;
        if let Some(d) = t.dir.as_deref().filter(|d| !d.is_empty()).map(PathBuf::from) {
            match templates::folder_reachable(&d) {
                Ok(()) => self.set_export_dir(d),
                Err(why) => {
                    let current = self.export_dir.as_ref().map(|c| sysdirs::display_path(c)).unwrap_or_else(|| crate::tr("– noch kein Ordner –").into());
                    folder_note = Some(crate::trf!("Ordner der Vorlage {why}: {a} – Ziel bleibt {b}", why = crate::tr(why), a = sysdirs::display_path(&d), b = current));
                }
            }
        }
        self.export_naming = t.naming.clone();
        if let Ok(v) = serde_json::to_value(&self.export_naming) {
            library::settings::set("exportNaming", v);
        }
        self.export_original = t.original;
        if let Some(f) = t.file_format {
            self.export_format = f;
        }
        if let Some(c) = t.color_space {
            self.export_color_space = c;
            if let Ok(v) = serde_json::to_value(c) {
                library::settings::set("exportColorSpace", v);
            }
        }
        if let Some(l) = t.limit_size {
            self.export_limit_size = l;
            library::settings::set("exportLimitSize", serde_json::Value::Bool(l));
        }
        if let Some(s) = t.output_size() {
            self.export_size = s;
            if let Ok(v) = serde_json::to_value(s) {
                library::settings::set("exportSize", v);
            }
        }
        if let Some(f) = t.resize_filter() {
            self.export_resize = f;
            library::settings::set("exportResize", serde_json::Value::String(f.key().into()));
        }
        self.export_names_ui.chosen = Some(t.name.clone());
        self.export_names_ui.message = folder_note.unwrap_or_default();
    }
}
