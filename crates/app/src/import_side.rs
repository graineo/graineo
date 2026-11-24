//! Side columns of the import window: on the left the source (cards, cameras, folders, recent folders), counters and
//! filters; on the right the operation, target, subfolders, second copy and "apply on import" (collection, develop
//! preset, keywords, rating, label, copyright). Both have a fixed order: for "Hinzufügen" the target rows and the file
//! filters keep their place but are not shown (they would do nothing). "Hinzufügen" shows the folders in the middle column instead of the grid, with the photos of the chosen folder below them ([`folders`]).

use super::*;
use library::import_select as sel;

/// What the columns asked for (system dialogs are opened after drawing).
#[derive(Default)]
pub(super) struct Picks {
    pub source: bool,
    pub target: bool,
    pub backup: bool,
    pub card: Option<PathBuf>,
    /// a camera chosen as source (its identifier)
    pub camera: Option<String>,
    /// the chosen camera is to be read again ("Neu einlesen")
    pub camera_reload: Option<String>,
}

fn small(ui: &mut egui::Ui, t: &str, color: Color32) {
    ui.add(egui::Label::new(egui::RichText::new(t).small().color(color)).truncate());
}

/// A number with its label; `None` while it is not known yet ("…").
fn counter(ui: &mut egui::Ui, n: Option<usize>, text: &str) {
    ui.horizontal(|ui| {
        let n = n.map(dialog::number).unwrap_or_else(|| "…".into());
        ui.add_sized([58.0, 16.0], egui::Label::new(egui::RichText::new(n).color(lr::TEXT())).halign(egui::Align::Max));
        ui.label(egui::RichText::new(crate::tr(text)).color(lr::TEXT_DIM()));
    });
}

pub(super) fn left(ui: &mut egui::Ui, imp: &mut Import, pick: &mut Picks) {
    egui::ScrollArea::vertical().id_salt("import-links").auto_shrink([false, false]).show(ui, |ui| {
        heading(ui, "Quelle");
        for k in imp.cards.clone() {
            let n = k.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if lr::list_item(ui, &format!("▣ {n}"), imp.source.as_ref() == Some(&k)).clicked() {
                pick.card = Some(k);
            }
        }
        for name in imp.reading.clone() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(11.0));
                small(ui, &crate::trf!("„{n}“ wird gelesen …", n = name), lr::TEXT_DIM());
            });
        }
        camera::sources(ui, imp, pick);
        ui.add_space(2.0);
        // the folder chosen as source in its own field (label left, button right above it)
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::tr("Ordner")).color(lr::TEXT_DIM()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.button(crate::tr("Wählen …"));
                lr::hover_tip(ui, &r, "Einen Ordner als Quelle wählen");
                if r.clicked() {
                    pick.source = true;
                }
            });
        });
        let folder = imp.source.as_ref().filter(|q| !camera::is_camera(Some(q)) && !is_card(q)).map(|q| q.display().to_string());
        lr::value_field(ui, folder.as_deref(), "Noch nichts gewählt");
        let recent: Vec<PathBuf> = recent_sources().into_iter().filter(|p| Some(p) != imp.source.as_ref()).take(4).collect();
        lr::section_rule(ui);
        heading(ui, "Zuletzt");
        if recent.is_empty() {
            small(ui, crate::tr("–"), lr::TEXT_DIM());
        }
        for p in recent {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
            let r = lr::list_item(ui, &name, false);
            lr::hover_tip(ui, &r, &p.display().to_string());
            if r.clicked() {
                pick.card = Some(p);
            }
        }
        lr::section_rule(ui);
        // the source as it is (photos = shots: RAW+JPEG of one shot is one photo); the filters below only change what
        // is imported (footer and button), never these numbers
        let o = sel::overview(&imp.cand, &imp.cells.iter().map(|c| c.members.clone()).collect::<Vec<_>>());
        // until every file has been compared with the library the split is not known: no numbers, the progress instead
        let checking = imp.scanning && !imp.cand.is_empty();
        if imp.scanning {
            let (g, n) = imp.scan.lock().unwrap().state_key;
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(12.0));
                let n = n.max(imp.cand.len());
                let t = if imp.cand.is_empty() {
                    crate::tr("liest die Fotos …").to_string()
                } else if g >= n && n > 0 {
                    crate::tr("vergleicht mit der Bibliothek …").to_string()
                } else {
                    crate::trf!("prüft, was schon da ist … {g} von {n}", g = dialog::number(g), n = dialog::number(n))
                };
                small(ui, &t, lr::TEXT_DIM());
            });
        }
        let r = ui.scope(|ui| counter(ui, Some(o.photos), "Fotos gefunden")).response;
        lr::hover_tip(ui, &r, "Ein Foto ist eine Aufnahme: RAW und JPG derselben Aufnahme zählen als ein Foto");
        let known = |n: usize| (!checking).then_some(n);
        ui.horizontal(|ui| {
            let n = known(o.new).map(dialog::number).unwrap_or_else(|| "…".into());
            ui.add_sized([58.0, 16.0], egui::Label::new(egui::RichText::new(n).color(lr::TEXT())).halign(egui::Align::Max));
            // no size while nothing is new (the size format shows at least 1 MB)
            let t = if o.new_bytes > 0 && !checking { crate::trf!("neu · {a}", a = dialog::size(o.new_bytes)) } else { crate::tr("neu").to_string() };
            ui.label(egui::RichText::new(t).color(lr::TEXT_DIM()));
        });
        let r = ui.scope(|ui| counter(ui, known(o.there), "schon da")).response;
        lr::hover_tip(ui, &r, "Schon in der Bibliothek (gleicher Inhalt, egal unter welchem Namen und in welchem Ordner); bei RAW+JPG entscheidet das RAW – nicht angehakt");
        let r = ui.scope(|ui| counter(ui, known(o.probable), "vermutlich da")).response;
        lr::hover_tip(ui, &r, "Gleiche Größe und gleicher Name oder Aufnahmezeit wie ein Foto der Bibliothek, dessen Laufwerk nicht verbunden ist – nicht angehakt");
        let r = ui.scope(|ui| counter(ui, known(o.identical), "identisch")).response;
        lr::hover_tip(ui, &r, "Bytegleiche Kopien in dieser Quelle: nur die erste wird importiert");
        lr::section_rule(ui);
        // "Hinzufügen" takes whole folders: the file types and the grid switches would do nothing (their place stays)
        let in_place = imp.mode == Mode::InPlace;
        if in_place {
            ui.add(egui::Label::new(egui::RichText::new(crate::tr("Beim Hinzufügen kommt der ganze Ordner in die Bibliothek.")).small().color(lr::TEXT_DIM())).wrap());
        }
        ui.scope(|ui| {
        if in_place {
            ui.set_invisible();
        }
        heading(ui, "Dateien");
        ui.horizontal_wrapped(|ui| {
            for (f, n) in &o.files {
                let on = !imp.filter.types_off.contains(f);
                let r = ui.add(egui::Button::new(format!("{f} · {}", dialog::number(*n))).selected(on));
                lr::hover_tip(ui, &r, "Dateityp an- oder abwählen: abgewählte Dateien werden weder gezeigt noch importiert");
                if r.clicked() {
                    if on {
                        imp.filter.types_off.insert(f.clone());
                    } else {
                        imp.filter.types_off.remove(f);
                    }
                }
            }
        });
        let mut raw_only = imp.filter.raw_only;
        let r = ui.add_enabled(o.pairs > 0 || raw_only, egui::Checkbox::new(&mut raw_only, crate::tr("RAW+JPG: nur RAW")));
        lr::hover_tip(ui, &r, "Bei RAW und JPG derselben Aufnahme nur das RAW importieren; JPGs ohne RAW bleiben dabei");
        if raw_only != imp.filter.raw_only {
            imp.filter.raw_only = raw_only;
            library::settings::set("importRawOnly", serde_json::json!(raw_only));
        }
        ui.add_space(6.0);
        ui.add_enabled(o.there > 0 || imp.show_existing, egui::Checkbox::new(&mut imp.show_existing, crate::tr("Schon importierte zeigen")));
        ui.add_enabled(o.identical > 0 || imp.show_identical, egui::Checkbox::new(&mut imp.show_identical, crate::tr("Identische zeigen")));
        });
    });
}

/// One sentence under the mode switch: what happens to the photos.
pub(super) fn mode_sentence(mode: Mode, from_camera: bool) -> &'static str {
    if from_camera {
        return "Von der Kamera wird immer geprüft kopiert. Auf der Kamera ändert sich nichts.";
    }
    match mode {
        Mode::InPlace => "Die Fotos bleiben an ihrem Ort und werden nur eingetragen.",
        Mode::CopyFiles => "Die Fotos werden geprüft kopiert; die Originale bleiben.",
        Mode::MoveFiles => "Nach der geprüften Kopie kommen die Originale in den Papierkorb.",
    }
}

/// Free space at the target, read in the background (a sleeping NAS must not stop the window).
fn free_space(imp: &Import, target: &Path) -> Option<u64> {
    let mut g = imp.free.lock().unwrap();
    match &*g {
        Some((p, v)) if p == target => *v,
        _ => {
            *g = Some((target.to_path_buf(), None));
            let (free, p) = (imp.free.clone(), target.to_path_buf());
            std::thread::spawn(move || {
                let v = library::free(&p);
                let mut g = free.lock().unwrap();
                if g.as_ref().is_some_and(|(q, _)| *q == p) {
                    *g = Some((p, v));
                }
            });
            None
        }
    }
}

pub(super) fn right(ui: &mut egui::Ui, imp: &mut Import, collections: &[(i64, String)], target_gone: bool, pick: &mut Picks) {
    egui::ScrollArea::vertical().id_salt("import-rechts").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            heading(ui, "Was passiert");
            crate::help::info(ui, "import", "Fotos importieren", &crate::help::IMPORT);
        });
        // from a camera only copying: nothing is ever deleted on it, and it has no folders to add
        let from_camera = camera::is_camera(imp.source.as_deref());
        if from_camera {
            imp.mode = Mode::CopyFiles;
        }
        ui.add_enabled_ui(!from_camera, |ui| {
            if lr::segmented_blue(ui, &mut imp.mode, &[(Mode::InPlace, crate::tr("Hinzufügen")), (Mode::CopyFiles, crate::tr("Kopieren")), (Mode::MoveFiles, crate::tr("Verschieben"))]) {
                imp.mode_automatic = false;
            }
        });
        // what happens, always visible under the switch (two lines reserved: nothing moves when the mode changes)
        let what = mode_sentence(imp.mode, from_camera);
        ui.add_space(4.0);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 30.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(crate::tr(what)).small().color(lr::TEXT_DIM())).wrap());
        });
        let copies = imp.mode != Mode::InPlace;
        lr::section_rule(ui);
        // "Hinzufügen": the target rows would do nothing – their place stays, empty
        ui.scope(|ui| {
            if !copies {
                ui.set_invisible();
            }
            ui.horizontal(|ui| {
                heading(ui, "Ablegen in");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::tr("Ändern …")).clicked() {
                        pick.target = true;
                    }
                });
            });
            {
                let t = imp.target.as_ref().map(|p| p.display().to_string());
                lr::value_field(ui, t.as_deref(), "Wähle zuerst, wohin die Fotos kommen.");
                // a fixed line below the path: problem, or free space and what is needed
                let need = imp.chosen_tally().bytes;
                let line = if let (None, Some(p)) = (&imp.target, &imp.target_missing) {
                    (crate::trf!("„{p}“ ist nicht erreichbar", p = p.display()), Color32::from_rgb(0xe0, 0xb0, 0x40))
                } else if target_gone {
                    (crate::tr("Zielordner gerade nicht erreichbar").to_string(), Color32::from_rgb(0xe0, 0xb0, 0x40))
                } else if let Some(z) = imp.target.clone() {
                    match free_space(imp, &z) {
                        // nothing chosen yet: no "needs 1 MB" (the size format shows at least 1 MB)
                        Some(f) if need == 0 => (crate::trf!("{a} frei", a = dialog::size(f)), lr::TEXT_DIM()),
                        Some(f) if f < need => (crate::trf!("{a} frei · braucht {b} – zu wenig Platz", a = dialog::size(f), b = dialog::size(need)), Color32::from_rgb(0xe0, 0x5c, 0x50)),
                        Some(f) => (crate::trf!("{a} frei · braucht {b}", a = dialog::size(f), b = dialog::size(need)), lr::TEXT_DIM()),
                        None => (crate::trf!("braucht {b}", b = dialog::size(need)), lr::TEXT_DIM()),
                    }
                } else {
                    (String::new(), lr::TEXT_DIM())
                };
                small(ui, &line.0, line.1);
            }
            lr::section_rule(ui);
            heading(ui, "Unterordner");
            ui.checkbox(&mut imp.by_date, crate::tr("nach Aufnahmedatum (JJJJ/JJJJ-MM-TT)"));
            let example = imp.chosen().next().map(|ci| {
                let k = &imp.cand[imp.cells[ci].members[0]];
                let name = k.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if imp.by_date && k.tag.len() >= 4 {
                    format!("→ {}/{}/{name}", &k.tag[..4], k.tag)
                } else {
                    format!("→ {name}")
                }
            });
            small(ui, &example.unwrap_or_default(), lr::TEXT_DIM());
            lr::section_rule(ui);
            ui.horizontal(|ui| {
                heading(ui, "Zweite Kopie");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(imp.backup.is_some(), egui::Button::new(crate::tr("Keine"))).clicked() {
                        imp.backup = None;
                    }
                    if ui.button(crate::tr("Wählen …")).clicked() {
                        pick.backup = true;
                    }
                });
            });
            let t = imp.backup.as_ref().map(|p| p.display().to_string());
            lr::value_field(ui, t.as_deref(), "keine");
        });
        lr::section_rule(ui);
        apply_on_import(ui, imp, collections);
    });
}

/// "Beim Import anwenden": collection, develop preset (only looks and film recipes; RAW, JPEG on request),
/// keywords, rating, color label and copyright.
fn apply_on_import(ui: &mut egui::Ui, imp: &mut Import, collections: &[(i64, String)]) {
    heading(ui, "Beim Import anwenden");
    let label_w = 86.0;
    let field_w = ui.available_width() - label_w - 10.0;
    let row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal(|ui| {
            ui.add_sized([label_w, 20.0], egui::Label::new(egui::RichText::new(crate::tr(label)).color(lr::TEXT_DIM())).halign(egui::Align::Min));
            add(ui);
        });
    };
    row(ui, "Sammlung", &mut |ui| {
        let name = match imp.collection {
            Choice::Nothing => crate::tr("Keine").to_string(),
            Choice::New => crate::tr("Neue Sammlung …").to_string(),
            Choice::Existing(id) => collections.iter().find(|s| s.0 == id).map(|s| s.1.clone()).unwrap_or_default(),
        };
        egui::ComboBox::from_id_salt("imp-sammlung").selected_text(name).width(field_w).show_ui(ui, |ui| {
            ui.selectable_value(&mut imp.collection, Choice::Nothing, crate::tr("Keine"));
            ui.selectable_value(&mut imp.collection, Choice::New, crate::tr("Neue Sammlung …"));
            for (id, n) in collections {
                ui.selectable_value(&mut imp.collection, Choice::Existing(*id), n);
            }
        });
    });
    // the name field and the set always stay in place (dimmed unless a new collection is chosen)
    let new = imp.collection == Choice::New;
    if new && !imp.name_prefilled && imp.new_collection.is_empty() {
        imp.new_collection = crate::trf!("Import {a}", a = ci::today());
        imp.name_prefilled = true;
    }
    row(ui, "", &mut |ui| {
        ui.add_enabled_ui(new, |ui| {
            crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut imp.new_collection).hint_text(crate::tr("Name der Sammlung")).desired_width(field_w));
        });
    });
    row(ui, "", &mut |ui| {
        ui.add_enabled(new, egui::Checkbox::new(&mut imp.under_imports, crate::tr("im Satz „Importe“")));
    });
    row(ui, "Vorgabe", &mut |ui| {
        let r = apply::combo(ui, &mut imp.preset, field_w);
        lr::hover_tip(ui, &r, "Look oder Film-Rezept für die importierten RAWs");
    });
    row(ui, "", &mut |ui| {
        ui.add_enabled(!imp.preset.is_empty(), egui::Checkbox::new(&mut imp.preset_jpeg, crate::tr("auch für JPEG und HEIC")));
    });
    row(ui, "Stichwörter", &mut |ui| {
        crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut imp.keywords).hint_text(crate::tr("durch Komma getrennt")).desired_width(field_w));
    });
    row(ui, "Bewertung", &mut |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for s in 1..=5u8 {
            let on = imp.stars >= s;
            let r = ui.add(egui::Button::new(egui::RichText::new("★").color(if on { Color32::from_gray(0xe8) } else { Color32::from_gray(0x55) })).frame(false));
            lr::hover_tip(ui, &r, &crate::trf!("{n} Sterne", n = s));
            if r.clicked() {
                imp.stars = if imp.stars == s { 0 } else { s };
            }
        }
    });
    row(ui, "Etikett", &mut |ui| {
        // the same rectangular cells as in the library; a click on the chosen one takes it off again
        let names = ["Etikett Rot", "Etikett Gelb", "Etikett Grün", "Etikett Blau", "Etikett Lila"];
        let cells: Vec<_> = (1..=5u8).map(|e| {
            let c = library::label::COLORS[e as usize];
            (lr::Cell::Color(Color32::from_rgb(c[0], c[1], c[2])), imp.label == e, names[e as usize - 1])
        }).collect();
        if let Some(i) = lr::cell_group_sized(ui, "imp-etikett", &cells, 26.0, 22.0).0 {
            let e = i as u8 + 1;
            imp.label = if imp.label == e { 0 } else { e };
        }
    });
    row(ui, "Copyright", &mut |ui| {
        crate::lr::edit_field(ui, egui::TextEdit::singleline(&mut imp.copyright).hint_text(crate::tr("z. B. © 2026 Name")).desired_width(field_w));
    });
}

/// Height of a folder row in "Hinzufügen".
const FOLDER_ROW_H: f32 = 22.0;
/// Edge of a preview cell below the folder list.
const PEEK_CELL: f32 = 104.0;
/// Fixed lines at the bottom of the column (where the folders go).
const FOOTER_H: f32 = 44.0;

/// The folder after (`down`) or before `current` in the list, staying at the ends; without a current one the first.
pub(super) fn step_folder(folders: &[PathBuf], current: Option<&Path>, down: bool) -> Option<PathBuf> {
    let i = current.and_then(|c| folders.iter().position(|p| p == c));
    let j = match i {
        None => 0,
        Some(i) if down => (i + 1).min(folders.len().saturating_sub(1)),
        Some(i) => i.saturating_sub(1),
    };
    folders.get(j).cloned()
}

/// Cells (photos; RAW+JPEG = one) that lie directly in `folder`, in the order of the grid (day by day, by path).
pub(super) fn cells_in_folder(imp: &Import, folder: &Path) -> Vec<usize> {
    imp.days.iter().flat_map(|d| d.cells.iter().copied()).filter(|&ci| imp.cand[imp.cells[ci].members[0]].path.parent() == Some(folder)).collect()
}

/// "Hinzufügen": the folders that are added (checked per folder; a folder whose photos are all in the library already
/// starts unchecked) and where they go (inside a root folder, or the source becomes a new one). A click on a row (or
/// ↑/↓) shows that folder's photos as thumbnails in the fixed area below the list – view only, nothing changes.
pub(super) fn folders(ui: &mut egui::Ui, imp: &mut Import) {
    heading(ui, "Hinzugefügt wird");
    let source = imp.source.clone().unwrap_or_default();
    let width = ui.available_width();
    // fixed split: list on top (about a third), photos of the chosen folder below, two lines at the bottom
    let total = ui.available_height() - FOOTER_H;
    let list_h = (total * 0.34).clamp(4.0 * FOLDER_ROW_H + 12.0, 9.0 * FOLDER_ROW_H + 12.0);
    let peek_h = (total - list_h - 30.0).max(PEEK_CELL);
    let names: Vec<PathBuf> = imp.folders.iter().map(|(p, _, _)| p.clone()).collect();
    // ↑/↓ move between the folders (not while a text field has the keys)
    let ctx = ui.ctx().clone();
    let mut moved = false;
    if !names.is_empty() && ctx.memory(|m| m.focused().is_none()) {
        for (key, down) in [(egui::Key::ArrowDown, true), (egui::Key::ArrowUp, false)] {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                imp.folder_focus = step_folder(&names, imp.folder_focus.as_deref(), down);
                moved = true;
            }
        }
    }
    let rel_name = |p: &Path| p.strip_prefix(&source).ok().map(|r| r.display().to_string()).filter(|r| !r.is_empty()).unwrap_or_else(|| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(4, 6)).show(ui, |ui| {
        ui.set_min_size(egui::vec2(width - 8.0, list_h - 12.0));
        ui.set_max_width(width - 8.0);
        egui::ScrollArea::vertical().id_salt("import-ordner").auto_shrink([false, false]).max_height(list_h - 12.0).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if imp.folders.is_empty() {
                small(ui, if imp.scanning { crate::tr("liest die Fotos …") } else { crate::tr("Keine Ordner mit Fotos.") }, lr::TEXT_DIM());
            }
            for (p, n, all_there) in imp.folders.clone() {
                let focused = imp.folder_focus.as_ref() == Some(&p);
                let on = imp.folders_on.get(&p).copied().unwrap_or(false);
                let (rect, row) = ui.allocate_exact_size(egui::vec2(ui.available_width(), FOLDER_ROW_H), egui::Sense::click());
                let check = egui::Rect::from_min_size(rect.left_center() + egui::vec2(6.0, -7.5), egui::vec2(15.0, 15.0));
                let check_r = ui.interact(check.expand(3.0), ui.scope_id().with(("imp-ordner-haken", &p)), egui::Sense::click());
                let painter = ui.painter();
                if focused {
                    painter.rect_filled(rect, 0.0, Color32::from_gray(0x3a));
                } else if row.hovered() {
                    painter.rect_filled(rect, 0.0, Color32::from_gray(0x26));
                }
                // check (light, not gold: gold is the main button only)
                painter.rect_stroke(check, 0.0, egui::Stroke::new(1.0, Color32::from_gray(if check_r.hovered() { 0xe0 } else { 0xb0 })), egui::StrokeKind::Inside);
                if on {
                    painter.rect_filled(check.shrink(3.5), 0.0, Color32::from_gray(0xe8));
                }
                let font = egui::TextStyle::Body.resolve(ui.style());
                let count = painter.layout_no_wrap(dialog::count(n, crate::tr("Foto"), crate::tr("Fotos")), font.clone(), lr::TEXT_DIM());
                let mut right = rect.right() - 8.0 - count.size().x;
                painter.galley(egui::pos2(right, rect.center().y - count.size().y / 2.0), count, lr::TEXT_DIM());
                if all_there {
                    let g = painter.layout_no_wrap(crate::tr("schon in der Bibliothek").to_string(), font.clone(), Color32::from_rgb(0x8f, 0xc0, 0x8a));
                    right -= g.size().x + 10.0;
                    painter.galley(egui::pos2(right, rect.center().y - g.size().y / 2.0), g, Color32::from_rgb(0x8f, 0xc0, 0x8a));
                }
                let color = if focused || row.hovered() { Color32::WHITE } else { lr::TEXT() };
                let name = painter.layout_no_wrap(rel_name(&p), font, color);
                let clip = egui::Rect::from_min_max(egui::pos2(check.right() + 8.0, rect.top()), egui::pos2(right - 10.0, rect.bottom()));
                painter.with_clip_rect(clip).galley(egui::pos2(clip.left(), rect.center().y - name.size().y / 2.0), name, color);
                if check_r.clicked() {
                    imp.folders_on.insert(p.clone(), !on);
                } else if row.clicked() {
                    imp.folder_focus = Some(p.clone());
                }
                if focused && moved {
                    ui.scroll_to_rect(rect, None);
                }
            }
        });
    });
    ui.add_space(4.0);
    peek(ui, imp, width, peek_h, &rel_name);
    ui.add_space(4.0);
    if imp.source_in_library {
        small(ui, crate::tr("liegt in einem Ordner der Bibliothek"), lr::TEXT_DIM());
    } else if imp.source.is_some() {
        small(ui, crate::tr("Neuer Ordner in der Bibliothek:"), lr::TEXT_DIM());
        small(ui, &source.display().to_string(), lr::TEXT());
    }
}

/// Photos of the folder chosen in the list, as thumbnails (only the cells in view are read; the import window's
/// thumbnails, so the import later takes them over). A fixed area: its place and size never change.
fn peek(ui: &mut egui::Ui, imp: &mut Import, width: f32, height: f32, rel_name: &dyn Fn(&Path) -> String) {
    let folder = imp.folder_focus.clone();
    let cells = folder.as_deref().map(|f| cells_in_folder(imp, f)).unwrap_or_default();
    // head line: which folder, how many photos (one fixed line)
    ui.allocate_ui_with_layout(egui::vec2(width, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let t = match &folder {
            Some(f) => crate::trf!("Inhalt von „{a}“ · {b}", a = rel_name(f), b = dialog::count(cells.len(), crate::tr("Foto"), crate::tr("Fotos"))),
            None => String::new(),
        };
        ui.add(egui::Label::new(egui::RichText::new(t).small().color(lr::TEXT_DIM())).truncate());
    });
    let mut wanted: Vec<(PathBuf, String)> = Vec::new();
    egui::Frame::new().fill(lr::PANEL_DARK).inner_margin(egui::Margin::symmetric(4, 4)).show(ui, |ui| {
        let (w, h) = (width - 8.0, height - 8.0);
        ui.set_min_size(egui::vec2(w, h));
        ui.set_max_width(w);
        if cells.is_empty() {
            ui.centered_and_justified(|ui| {
                let t = if imp.scanning { crate::tr("liest die Fotos …") } else if imp.folders.is_empty() { "" } else { crate::tr("Klicke auf einen Ordner, um seine Fotos zu sehen.") };
                ui.label(egui::RichText::new(t).color(lr::TEXT_DIM()));
            });
            return;
        }
        let columns = ((w - 10.0) / PEEK_CELL).floor().max(1.0) as usize;
        let cell_w = ((w - 10.0) / columns as f32).floor();
        let cell_h = (cell_w * 0.9).round();
        let rows = cells.len().div_ceil(columns);
        let on = folder.as_ref().is_some_and(|f| imp.folders_on.get(f).copied().unwrap_or(false));
        // a new folder starts at the top
        let id = ui.scope_id().with(("imp-ordner-inhalt", folder.as_ref()));
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).max_height(h).show_viewport(ui, |ui, viewport| {
            ui.set_height(rows as f32 * cell_h + 4.0);
            let origin = ui.min_rect().min;
            let first = ((viewport.min.y / cell_h).floor().max(0.0)) as usize;
            let last = ((viewport.max.y / cell_h).ceil() as usize).min(rows);
            for r in first..last {
                for c in 0..columns {
                    let Some(&ci) = cells.get(r * columns + c) else { break };
                    let rect = egui::Rect::from_min_size(origin + egui::vec2(c as f32 * cell_w, r as f32 * cell_h), egui::vec2(cell_w, cell_h));
                    wanted.push(peek_cell(ui, imp, ci, rect, on));
                }
            }
        });
    });
    // the helpers take the newest request first: requested from the bottom up, the top left cell comes first
    for (p, f) in wanted.iter().rev() {
        imp.thumbs.request(p, f);
    }
}

/// One preview cell: thumbnail and name; photos already in the library (or of an unchecked folder) dimmed.
fn peek_cell(ui: &egui::Ui, imp: &Import, ci: usize, rect: egui::Rect, folder_on: bool) -> (PathBuf, String) {
    let c = &imp.cells[ci];
    let first = grid::picture_of(imp, c);
    let (path, format) = (imp.cand[first].path.clone(), imp.cand[first].format.clone());
    let there = imp.cell_duplicate(c);
    let p = ui.painter();
    let inner = rect.shrink(3.0);
    p.rect_filled(inner, 0.0, Color32::from_gray(0x1a));
    let image = egui::Rect::from_min_size(inner.min + egui::vec2(4.0, 4.0), egui::vec2(inner.width() - 8.0, inner.height() - 22.0));
    match imp.thumbs.tex.get(&path) {
        Some(t) => {
            let sz = t.size_vec2();
            let f = (image.width() / sz.x).min(image.height() / sz.y);
            let tint = if there || !folder_on { Color32::from_gray(120) } else { Color32::WHITE };
            p.image(t.id(), egui::Rect::from_center_size(image.center(), sz * f), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
        }
        None => {
            p.text(image.center(), egui::Align2::CENTER_CENTER, &format, egui::FontId::proportional(10.0), Color32::from_gray(0x82));
        }
    }
    let color = if there { lr::TEXT_DIM() } else { lr::TEXT() };
    let name = p.layout_no_wrap(grid::cell_name(imp, c), egui::FontId::proportional(10.0), color);
    let clip = egui::Rect::from_min_max(egui::pos2(inner.left() + 4.0, inner.bottom() - 17.0), egui::pos2(inner.right() - 4.0, inner.bottom()));
    p.with_clip_rect(clip).galley(egui::pos2(inner.left() + 5.0, inner.bottom() - 15.0), name, color);
    (path, format)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ↑/↓ walk the folder list and stop at its ends; without a chosen folder the first one is taken.
    #[test]
    fn step_folder_walks_and_stops_at_the_ends() {
        let f: Vec<PathBuf> = ["/a", "/a/b", "/c"].iter().map(PathBuf::from).collect();
        assert_eq!(step_folder(&f, None, true), Some(PathBuf::from("/a")));
        assert_eq!(step_folder(&f, Some(Path::new("/a")), true), Some(PathBuf::from("/a/b")));
        assert_eq!(step_folder(&f, Some(Path::new("/c")), true), Some(PathBuf::from("/c")));
        assert_eq!(step_folder(&f, Some(Path::new("/a/b")), false), Some(PathBuf::from("/a")));
        assert_eq!(step_folder(&f, Some(Path::new("/a")), false), Some(PathBuf::from("/a")));
        assert_eq!(step_folder(&f, Some(Path::new("/gone")), false), Some(PathBuf::from("/a")));
        assert_eq!(step_folder(&[], None, true), None);
    }
}
